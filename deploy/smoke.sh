#!/usr/bin/env bash
# Acceptance smoke against a running platformd, through the `platform` CLI
# and curl. Prints a PASS/FAIL/SKIP table; exits 1 if anything failed.
#
# Usage: deploy/smoke.sh <base-url> <owner-token-file> [--receiver-url URL] [--platform PATH]
#   <base-url>          e.g. http://100.x.y.z:7340 or http://127.0.0.1:7340
#   <owner-token-file>  file holding the owner token (read, never printed)
#   --receiver-url URL  a mock receiver the server can reach (deploy/mock-receiver.py);
#                       without it the receiver scenario is SKIP
#   --platform PATH     platform CLI binary (default: `platform` on PATH)
#
# Every run registers new agents named smoke-<ulid> (v1 has no delete), so
# runs never collide. The `all` scenario fans out to every agent joined to
# `all`, including real ones. At the end the smoke agents leave `all` and
# drop their receiver so they stop collecting messages and notifications.
set -euo pipefail

# shellcheck source=deploy/lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

# How long the receiver scenario waits for the 202 to be recorded.
readonly RECEIVER_WAIT_SECS=30
readonly PAGE_LIMIT=2
readonly ULID_ALPHABET=0123456789abcdefghjkmnpqrstvwxyz

base_url=""
owner_token_file=""
receiver_url=""
platform_bin=platform
workdir=""

# Results, in order (bash 3.2: no associative arrays).
result_names=()
result_states=()
result_notes=()

# Filled in by the scenarios.
agent_a="" agent_b="" agent_r=""
all_message_id="" dm_message_id="" dm_room=""
snapshot_head=""

usage() {
  sed -n '5,10p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2
  exit 2
}

parse_args() {
  local positional=()
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --receiver-url)
        [[ $# -ge 2 ]] || usage
        receiver_url="$2"
        shift
        ;;
      --platform)
        [[ $# -ge 2 ]] || usage
        platform_bin="$2"
        shift
        ;;
      -h | --help | -*) usage ;;
      *) positional+=("$1") ;;
    esac
    shift
  done
  [[ ${#positional[@]} -eq 2 ]] || usage
  base_url="${positional[0]%/}"
  owner_token_file="${positional[1]}"
  [[ "$base_url" =~ ^https?://[^[:space:]]+$ ]] || die "base URL must be http(s)://...: $base_url"
  [[ -r "$owner_token_file" ]] || die "cannot read owner token file: $owner_token_file"
}

# ---- helpers ---------------------------------------------------------------

# A ULID (Crockford base32, lowercase): 48-bit millisecond time (second
# precision here; bash 3.2 has no sub-second clock) and 80 random bits.
new_ulid() {
  local time_ms=$(($(date +%s) * 1000))
  local ulid="" index
  for index in 9 8 7 6 5 4 3 2 1 0; do
    ulid="$ulid${ULID_ALPHABET:$(((time_ms >> (index * 5)) & 31)):1}"
  done
  local byte
  for byte in $(od -An -N16 -tu1 /dev/urandom); do
    ulid="$ulid${ULID_ALPHABET:$((byte & 31)):1}"
  done
  printf '%s\n' "$ulid"
}

token_file() {
  printf '%s/%s.token\n' "$workdir" "$1"
}

save_token() {
  printf '%s\n' "$2" >"$(token_file "$1")"
}

# platform --json <args...> with the token read from a file. stderr (the
# CLI's one-line diagnostics) is kept for failure notes: see cli_error.
cli() {
  local token="$1"
  shift
  "$platform_bin" --url "$base_url" --token-file "$token" --json "$@" 2>"$workdir/cli.stderr"
}

cli_error() {
  tail -n 1 "$workdir/cli.stderr" 2>/dev/null || true
}

# api METHOD PATH TOKEN_FILE [JSON_BODY] -> sets http_status and http_body.
# The token goes in a header file so it never appears in the process list.
http_status="" http_body=""
api() {
  local method="$1" path="$2" token="$3" body="${4:-}"
  local headers="$workdir/headers" out="$workdir/body"
  printf 'Authorization: Bearer %s\n' "$(tr -d '[:space:]' <"$token")" >"$headers"
  local args=(-sS -X "$method" -o "$out" -w '%{http_code}' -H "@$headers")
  if [[ -n "$body" ]]; then
    args+=(-H 'Content-Type: application/json' --data-binary "$body")
  fi
  http_status="$(curl "${args[@]}" "$base_url$path")" || http_status="curl-error"
  http_body="$(cat "$out" 2>/dev/null || true)"
}

# Fails the current scenario with a note unless `jq -e FILTER` holds.
note=""
expect_json() {
  local json="$1" filter="$2" what="$3"
  if jq -e "$filter" >/dev/null 2>&1 <<<"$json"; then
    return 0
  fi
  note="$what"
  return 1
}

expect_status() {
  local expected="$1" what="$2"
  [[ "$http_status" == "$expected" ]] && return 0
  note="$what: HTTP $http_status, expected $expected"
  return 1
}

send_as() {
  local sender="$1" to="$2" body="$3" key="$4"
  api POST /v1/messages "$(token_file "$sender")" \
    "$(jq -nc --arg to "$to" --arg body "$body" --arg key "$key" '{to: $to, body: $body, client_key: $key}')"
}

record() {
  result_names+=("$1")
  result_states+=("$2")
  result_notes+=("$3")
  printf '%-4s %s%s\n' "$2" "$1" "${3:+  ($3)}" >&2
}

# scenario NAME FUNCTION: runs FUNCTION and records PASS or FAIL with its note.
scenario() {
  local name="$1" fn="$2"
  note=""
  if "$fn"; then
    record "$name" PASS "$note"
  else
    record "$name" FAIL "${note:-failed}"
  fi
}

# ---- scenarios -------------------------------------------------------------

check_health() {
  local json
  json="$(cli "$owner_token_file" health)" || {
    note="platform health failed: $(cli_error)"
    return 1
  }
  expect_json "$json" '.status == "ok" and (.schema_version | type == "number")' "healthz body: $json"
}

register() {
  local agent="$1" receiver="${2:-}"
  local args=(agent register "$agent")
  [[ -z "$receiver" ]] || args+=(--receiver-url "$receiver")
  local json
  json="$(cli "$owner_token_file" "${args[@]}")" || {
    note="platform agent register $agent failed: $(cli_error)"
    return 1
  }
  expect_json "$json" '(.agent_api_key | startswith("pak_")) and (.receiver_secret | startswith("prs_"))' \
    "register $agent: missing one-time credentials" || return 1
  save_token "$agent" "$(jq -r .agent_api_key <<<"$json")"
  printf '%s\n' "$(jq -r .agent_api_key <<<"$json")" >"$workdir/$agent.first-key"
  printf '%s\n' "$(jq -r .receiver_secret <<<"$json")" >"$workdir/$agent.secret"
}

check_register() {
  register "$agent_a" && register "$agent_b"
}

check_directory_hides_credentials() {
  local json
  json="$(cli "$owner_token_file" agent list)" || {
    note="platform agent list failed: $(cli_error)"
    return 1
  }
  expect_json "$json" "[.. | strings] | (index(\"$agent_a\") != null and index(\"$agent_b\") != null)" \
    "agent list lacks the smoke agents" || return 1
  local agent secret
  for agent in "$agent_a" "$agent_b"; do
    for secret in "$(cat "$workdir/$agent.first-key")" "$(cat "$workdir/$agent.secret")"; do
      if grep -qF -- "$secret" <<<"$json"; then
        note="agent list exposes a credential of $agent"
        return 1
      fi
    done
  done
}

inbox_has() {
  local agent="$1" message_id="$2" json
  json="$(cli "$(token_file "$agent")" inbox --all-pages)" || return 1
  jq -e --arg id "$message_id" 'any(.items[]; .message_id == $id)' >/dev/null <<<"$json"
}

check_all_fanout() {
  send_as "$agent_a" all "smoke all" smoke-all
  expect_status 201 "send to all" || return 1
  all_message_id="$(jq -r .message_id <<<"$http_body")"
  inbox_has "$agent_b" "$all_message_id" || {
    note="recipient inbox lacks the all message"
    return 1
  }
  inbox_has "$agent_a" "$all_message_id" || {
    note="sender inbox lacks its own all message"
    return 1
  }
}

check_dm() {
  send_as "$agent_a" "$agent_b" "smoke dm" smoke-dm
  expect_status 201 "send DM" || return 1
  dm_message_id="$(jq -r .message_id <<<"$http_body")"
  dm_room="$(jq -r .room_id <<<"$http_body")"
  inbox_has "$agent_b" "$dm_message_id" || {
    note="recipient inbox lacks the DM"
    return 1
  }
  if inbox_has "$agent_a" "$dm_message_id"; then
    note="the DM landed in the sender's inbox"
    return 1
  fi
  local json
  json="$(cli "$(token_file "$agent_a")" history --room "$dm_room")" || {
    note="sender history of $dm_room failed: $(cli_error)"
    return 1
  }
  expect_json "$json" "any(.items[]; .message_id == \"$dm_message_id\")" "sender history lacks the DM"
}

check_dm_errors() {
  send_as "$agent_a" "smoke-unknown-$(new_ulid)" "x" smoke-unknown
  expect_status 404 "unknown recipient" || return 1
  expect_json "$http_body" '.error.code == "unknown_recipient"' "unknown recipient code: $http_body" || return 1
  send_as "$agent_a" "$agent_a" "x" smoke-self
  expect_status 422 "self DM" || return 1
  expect_json "$http_body" '.error.code == "self_dm_not_supported"' "self DM code: $http_body"
}

check_idempotency() {
  send_as "$agent_a" "$agent_b" "smoke dm" smoke-dm
  expect_status 200 "same key, same payload" || return 1
  expect_json "$http_body" ".message_id == \"$dm_message_id\" and .deduplicated == true" \
    "replay did not return the original message" || return 1
  send_as "$agent_a" "$agent_b" "smoke dm, changed" smoke-dm
  expect_status 409 "same key, other payload" || return 1
  expect_json "$http_body" '.error.code == "idempotency_conflict"' "conflict code: $http_body"
}

# Pages through B's inbox from 0 with a fixed snapshot while a new message
# arrives after the first page; expects every seq up to the snapshot head
# exactly once and nothing above it.
check_inbox_paging() {
  local index
  for index in 1 2 3; do
    send_as "$agent_a" "$agent_b" "smoke page $index" "smoke-page-$index"
    expect_status 201 "page message $index" || return 1
  done
  local token
  token="$(token_file "$agent_b")"
  api GET "/v1/inbox?after_seq=0&limit=$PAGE_LIMIT" "$token"
  expect_status 200 "first page" || return 1
  snapshot_head="$(jq -r .snapshot_head_seq <<<"$http_body")"
  local seqs after has_more
  seqs="$(jq -r '.items[].inbox_seq' <<<"$http_body")"
  after="$(jq -r .next_after_seq <<<"$http_body")"
  has_more="$(jq -r .has_more <<<"$http_body")"

  send_as "$agent_a" "$agent_b" "smoke after snapshot" smoke-after-snapshot
  expect_status 201 "message after the snapshot" || return 1

  while [[ "$has_more" == true ]]; do
    api GET "/v1/inbox?after_seq=$after&until_seq=$snapshot_head&limit=$PAGE_LIMIT" "$token"
    expect_status 200 "next page" || return 1
    expect_json "$http_body" ".snapshot_head_seq == \"$snapshot_head\"" "snapshot head moved" || return 1
    seqs="$seqs"$'\n'"$(jq -r '.items[].inbox_seq' <<<"$http_body")"
    after="$(jq -r .next_after_seq <<<"$http_body")"
    has_more="$(jq -r .has_more <<<"$http_body")"
  done
  local expected
  expected="$(seq 1 "$snapshot_head")"
  [[ "$(grep -v '^$' <<<"$seqs")" == "$expected" ]] || {
    note="pages returned $(tr '\n' ' ' <<<"$seqs")for snapshot head $snapshot_head"
    return 1
  }
  note="$snapshot_head rows in pages of $PAGE_LIMIT"
}

read_marker_of() {
  local json
  json="$(cli "$(token_file "$1")" inbox --limit 1)" || return 1
  jq -r .read_marker <<<"$json"
}

check_read_marker() {
  local json
  json="$(cli "$(token_file "$agent_b")" inbox read --through-seq "$snapshot_head")" || {
    note="platform inbox read failed: $(cli_error)"
    return 1
  }
  expect_json "$json" ".read_marker == \"$snapshot_head\"" "marker not at $snapshot_head: $json" || return 1
  api PUT /v1/inbox/read-marker "$(token_file "$agent_b")" "{\"through_seq\":\"$((snapshot_head + 100))\"}"
  expect_status 409 "marker beyond head" || return 1
  expect_json "$http_body" '.error.code == "read_marker_ahead"' "ahead code: $http_body" || return 1
  api PUT /v1/inbox/read-marker "$(token_file "$agent_b")" '{"through_seq":"1"}'
  expect_status 200 "lower marker" || return 1
  expect_json "$http_body" ".read_marker == \"$snapshot_head\"" "lower marker moved it back: $http_body"
}

# B has one unread row (sent after the snapshot). Owner reads must not ack it.
check_owner_no_ack() {
  cli "$owner_token_file" agent inbox "$agent_b" >/dev/null || {
    note="owner agent inbox failed: $(cli_error)"
    return 1
  }
  cli "$owner_token_file" history --room all >/dev/null || {
    note="owner history failed: $(cli_error)"
    return 1
  }
  cli "$owner_token_file" history --room "$dm_room" >/dev/null || {
    note="owner DM history failed: $(cli_error)"
    return 1
  }
  local marker
  marker="$(read_marker_of "$agent_b")" || {
    note="cannot read the marker"
    return 1
  }
  [[ "$marker" == "$snapshot_head" ]] || {
    note="owner reads moved the marker to $marker"
    return 1
  }
}

check_delivery_no_receiver() {
  local json
  json="$(cli "$owner_token_file" delivery "$agent_b")" || {
    note="platform delivery failed: $(cli_error)"
    return 1
  }
  expect_json "$json" '.state == "no_receiver" and .receiver_url_present == false' "delivery: $json"
}

check_rotate() {
  local old_key="$workdir/$agent_a.first-key" json
  json="$(cli "$owner_token_file" agent tokens rotate "$agent_a")" || {
    note="platform agent tokens rotate failed: $(cli_error)"
    return 1
  }
  expect_json "$json" '(.agent_api_key | startswith("pak_")) and (.receiver_secret | startswith("prs_"))' \
    "rotate: missing new credentials" || return 1
  save_token "$agent_a" "$(jq -r .agent_api_key <<<"$json")"
  api GET /v1/inbox "$old_key"
  expect_status 401 "old key after rotate" || return 1
  expect_json "$http_body" '.error.code == "unauthorized"' "old key code: $http_body" || return 1
  api GET /v1/inbox "$(token_file "$agent_a")"
  expect_status 200 "new key after rotate"
}

check_receiver() {
  register "$agent_r" "$receiver_url" || return 1
  send_as "$agent_a" "$agent_r" "smoke wake" smoke-wake
  expect_status 201 "DM to the receiver agent" || return 1
  local seq json deadline=$((SECONDS + RECEIVER_WAIT_SECS))
  seq="$(jq -r .inbox_head_seq <<<"$(cli "$owner_token_file" delivery "$agent_r")")"
  while :; do
    json="$(cli "$owner_token_file" delivery "$agent_r")" || {
      note="platform delivery failed: $(cli_error)"
      return 1
    }
    if jq -e --arg seq "$seq" '.last_accepted_seq == $seq' >/dev/null <<<"$json"; then
      break
    fi
    if ((SECONDS >= deadline)); then
      note="no 202 recorded within ${RECEIVER_WAIT_SECS}s: $json"
      return 1
    fi
    sleep 1
  done
  expect_json "$json" '.read_marker == "0" and .state == "pending"' "202 must not move the marker: $json" || return 1
  json="$(cli "$(token_file "$agent_r")" inbox read --through-seq "$seq")" || {
    note="receiver agent inbox read failed: $(cli_error)"
    return 1
  }
  json="$(cli "$owner_token_file" delivery "$agent_r")" || return 1
  expect_json "$json" '.state == "idle"' "marker at head should be idle: $json" || return 1
  note="last_accepted_seq $seq, then idle at marker=head"
}

# Leaves `all` and drops receivers so smoke agents stop accumulating.
check_cleanup() {
  local agent failed=""
  for agent in "$agent_a" "$agent_b" ${agent_r:+"$agent_r"}; do
    api PUT "/v1/agents/$agent/membership/all" "$owner_token_file" '{"joined":false}'
    [[ "$http_status" == 200 ]] || failed="$failed $agent:membership=$http_status"
  done
  if [[ -n "$agent_r" ]]; then
    cli "$owner_token_file" receiver unset "$agent_r" >/dev/null || failed="$failed $agent_r:receiver"
  fi
  [[ -z "$failed" ]] || {
    note="cleanup failed:$failed"
    return 1
  }
  note="smoke agents left all"
}

# ---- main ------------------------------------------------------------------

print_table() {
  local index failures=0
  printf '\n%-36s %-5s %s\n' SCENARIO RESULT NOTE
  for ((index = 0; index < ${#result_names[@]}; index++)); do
    printf '%-36s %-5s %s\n' "${result_names[$index]}" "${result_states[$index]}" "${result_notes[$index]}"
    [[ "${result_states[$index]}" != FAIL ]] || failures=$((failures + 1))
  done
  printf '\nagents: %s %s%s\n' "$agent_a" "$agent_b" "${agent_r:+ $agent_r}"
  if [[ "$failures" -gt 0 ]]; then
    printf 'smoke: %d FAIL\n' "$failures"
    return 1
  fi
  printf 'smoke: all PASS\n'
}

main() {
  parse_args "$@"
  require_cmd curl jq od seq "$platform_bin"
  workdir="$(mktemp -d)"
  chmod 0700 "$workdir"
  # shellcheck disable=SC2064 # expand now
  trap "rm -rf '$workdir'" EXIT

  agent_a="smoke-$(new_ulid)"
  agent_b="smoke-$(new_ulid)"
  log "smoke against $base_url as $agent_a, $agent_b"

  scenario "healthz / platform health" check_health
  scenario "register (one-time credentials)" check_register
  if [[ "${result_states[1]}" != PASS ]]; then
    print_table
    die "registration failed; later scenarios need the agents"
  fi
  scenario "directory hides credentials" check_directory_hides_credentials
  scenario "all fanout (1)" check_all_fanout
  scenario "DM recipient-only (2)" check_dm
  scenario "unknown 404 / self DM 422 (2)" check_dm_errors
  scenario "idempotency 200 / 409 (4)" check_idempotency
  scenario "inbox paging snapshot (10)" check_inbox_paging
  scenario "read marker ahead/no-op (10)" check_read_marker
  scenario "owner reads do not ack (12)" check_owner_no_ack
  scenario "delivery no_receiver" check_delivery_no_receiver
  scenario "rotate: old key 401 (12)" check_rotate
  if [[ -n "$receiver_url" ]]; then
    agent_r="smoke-$(new_ulid)"
    scenario "receiver 202 keeps marker (5)" check_receiver
  else
    record "receiver 202 keeps marker (5)" SKIP "no --receiver-url"
  fi
  scenario "cleanup (leave all, unset receiver)" check_cleanup
  print_table
}

main "$@"
