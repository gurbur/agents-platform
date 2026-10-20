#!/usr/bin/env bash
# Acceptance smoke against a running platformd, through the `platform` CLI
# and curl. Prints a PASS/FAIL/SKIP table; exits 1 if anything failed.
#
# Usage: deploy/smoke.sh <base-url> <owner-token-file> [--receiver-url URL] [--receiver-log FILE] [--identity-aud AUD] [--post-as AGENT_ID] [--platform PATH]
#   <base-url>          e.g. http://100.x.y.z:7340 or http://127.0.0.1:7340
#   <owner-token-file>  file holding the owner token (read, never printed)
#   --receiver-url URL  a mock receiver the server can reach (deploy/mock-receiver.py);
#                       without it the receiver scenarios (5, 19-21) are SKIP
#   --receiver-log FILE that mock receiver's stdout, to check the wake body keys (21)
#   --identity-aud AUD  audience for the identity token scenario (default: garage-docs);
#                       a hub without [identity] makes that scenario SKIP
#   --post-as AGENT_ID  a registered agent in the hub's [owner] post_as (e.g. human);
#                       without it the owner barge-in parts (14, 18) and the
#                       protected-id part of the deletion scenario are skipped
#   --platform PATH     platform CLI binary (default: `platform` on PATH)
#
# Every run registers new agents named smoke-<ulid>, so runs never collide.
# The `all` scenario fans out to every agent joined to `all`, including real
# ones. At the end the smoke agents are deleted (their messages stay in the
# history, like any deleted agent's). The smoke topic rooms room:smoke-<ulid>
# stay, archived: rooms are never deleted (S13).
set -euo pipefail

# shellcheck source=deploy/lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

# How long the receiver scenario waits for the 202 to be recorded.
readonly RECEIVER_WAIT_SECS=30
# How long the presence scenario waits for last_seen_at, which the hub
# writes off the request path.
readonly LAST_SEEN_WAIT_SECS=10
readonly PAGE_LIMIT=2
readonly ULID_ALPHABET=0123456789abcdefghjkmnpqrstvwxyz
readonly DEFAULT_IDENTITY_AUD=garage-docs
# A valid 1x1 PNG for the avatar scenario, and the hub's avatar limit
# (platform-wire AVATAR_MAX_BYTES).
readonly AVATAR_PNG_HEX=89504e470d0a1a0a0000000d49484452000000010000000108060000001f15c4890000000d4944415478da6364f8cf500f00038601805a347d6b0000000049454e44ae426082
readonly AVATAR_MAX_BYTES=524288

base_url=""
owner_token_file=""
receiver_url=""
receiver_log=""
identity_aud="$DEFAULT_IDENTITY_AUD"
post_as=""
platform_bin=platform
workdir=""

# Results, in order (bash 3.2: no associative arrays).
result_names=()
result_states=()
result_notes=()

# Filled in by the scenarios.
agent_a="" agent_b="" agent_c="" agent_p="" agent_r="" agent_z=""
all_message_id="" dm_message_id="" dm_room="" topic_room="" wake_room=""
snapshot_head=""

usage() {
  sed -n '5,16p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2
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
      --receiver-log)
        [[ $# -ge 2 ]] || usage
        receiver_log="$2"
        shift
        ;;
      --identity-aud)
        [[ $# -ge 2 ]] || usage
        identity_aud="$2"
        shift
        ;;
      --post-as)
        [[ $# -ge 2 ]] || usage
        post_as="$2"
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

# api_bytes METHOD PATH TOKEN_FILE CONTENT_TYPE FILE [HEADER] -> sets
# http_status and http_body like api, sending FILE as the raw body (FILE
# may be empty for no body) with one optional extra header.
api_bytes() {
  local method="$1" path="$2" token="$3" content_type="$4" file="$5" extra="${6:-}"
  local headers="$workdir/headers" out="$workdir/body"
  printf 'Authorization: Bearer %s\n' "$(tr -d '[:space:]' <"$token")" >"$headers"
  [[ -z "$content_type" ]] || printf 'Content-Type: %s\n' "$content_type" >>"$headers"
  [[ -z "$extra" ]] || printf '%s\n' "$extra" >>"$headers"
  local args=(-sS -X "$method" -o "$out" -w '%{http_code}' -H "@$headers")
  [[ -z "$file" ]] || args+=(--data-binary "@$file")
  http_status="$(curl "${args[@]}" "$base_url$path")" || http_status="curl-error"
  http_body="$(cat "$out" 2>/dev/null || true)"
}

# Writes the hex string $1 as bytes to file $2.
write_hex() {
  local hex="$1" escaped=""
  while [[ -n "$hex" ]]; do
    escaped="$escaped\\x${hex:0:2}"
    hex="${hex:2}"
  done
  printf '%b' "$escaped" >"$2"
}

# Decodes one unpadded base64url JWT segment (jq's @base64d wants the
# standard alphabet with padding).
b64url_decode() {
  local segment
  segment="$(tr '_-' '/+' <<<"$1")"
  while ((${#segment} % 4)); do
    segment="$segment="
  done
  jq -nr --arg segment "$segment" '$segment | @base64d'
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

# send_as SENDER DESTINATION BODY KEY: a room id (`all`, `dm:…`, `room:…`)
# goes out as `room_id`, an agent id as the DM alias `to` (S13: `to` never
# takes `all`).
send_as() {
  local sender="$1" to="$2" body="$3" key="$4" field=to
  [[ "$to" != all && "$to" != *:* ]] || field=room_id
  api POST /v1/messages "$(token_file "$sender")" \
    "$(jq -nc --arg field "$field" --arg to "$to" --arg body "$body" --arg key "$key" \
      '{($field): $to, body: $body, client_key: $key}')"
}

record() {
  result_names+=("$1")
  result_states+=("$2")
  result_notes+=("$3")
  printf '%-4s %s%s\n' "$2" "$1" "${3:+  ($3)}" >&2
}

# scenario NAME FUNCTION: runs FUNCTION and records PASS or FAIL with its
# note. A FUNCTION that succeeds after setting skip_note records SKIP.
skip_note=""
scenario() {
  local name="$1" fn="$2"
  note=""
  skip_note=""
  if "$fn"; then
    if [[ -n "$skip_note" ]]; then
      record "$name" SKIP "$skip_note"
    else
      record "$name" PASS "$note"
    fi
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

# Display name of a smoke agent: "smoke <ulid>". "smoke $agent" would be
# 38 characters, over the 32-character name limit.
smoke_name() {
  printf 'smoke %s\n' "${1#smoke-}"
}

register() {
  local agent="$1" receiver="${2:-}"
  local args=(agent register "$agent" --name "$(smoke_name "$agent")")
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

# Scenario 13: A declares its profile, B reads it from the directory, A
# cannot rename itself, the owner can, null clears a field and an unknown
# enum spelling is 422. Names and intros are never put in the notes. The
# smoke agents keep their profile (there is no delete).
check_directory_profile() {
  local key_a key_b json rc=0
  key_a="$(token_file "$agent_a")"
  key_b="$(token_file "$agent_b")"
  json="$(printf 'smoke intro\nsecond line\n' |
    cli "$key_a" profile set "$agent_a" --gender-form none --role "smoke role" --intro-file -)" || {
    note="profile set failed: $(cli_error)"
    return 1
  }
  json="$(cli "$key_b" agent list)" || {
    note="agent list as $agent_b failed: $(cli_error)"
    return 1
  }
  expect_json "$json" "any(.items[]; .agent_id == \"$agent_a\" and .name == \"$(smoke_name "$agent_a")\"
      and .profile.gender_form == \"none\" and .profile.age_band == null and .profile.role == \"smoke role\"
      and .profile.intro == \"smoke intro\\nsecond line\" and (.profile.updated_at | type == \"string\"))" \
    "$agent_b does not see the declared name and profile of $agent_a" || return 1

  cli "$key_a" profile set-name "$agent_a" "smoke self-rename" >/dev/null || rc=$?
  if [[ "$rc" != 1 ]] || ! grep -q 'forbidden' "$workdir/cli.stderr"; then
    note="agent set-name: exit $rc, expected 1 forbidden"
    return 1
  fi
  json="$(cli "$owner_token_file" profile set-name "$agent_a" "smoke renamed")" || {
    note="owner set-name failed: $(cli_error)"
    return 1
  }
  expect_json "$json" '.name == "smoke renamed"' "owner set-name did not rename" || return 1

  json="$(cli "$key_a" profile set "$agent_a" --clear role)" || {
    note="profile set --clear role failed: $(cli_error)"
    return 1
  }
  expect_json "$json" '.profile.role == null and .profile.gender_form == "none"' \
    "--clear role did not clear only role" || return 1

  api PUT "/v1/agents/$agent_a/profile" "$key_a" '{"gender_form":"Female"}'
  expect_status 422 "unknown enum spelling" || return 1
  expect_json "$http_body" '.error.code == "invalid_field" and .error.details.field == "gender_form"' \
    "unknown enum: not invalid_field on gender_form" || return 1
  note="B sees A, agent set-name 403, owner rename, clear, enum 422"
}

# Scenario 15: A uploads a PNG avatar, B sees its sha256 in the directory
# and downloads the same bytes, the ETag revalidates to 304, B cannot write
# A's avatar, oversize is 413 and a wrong type 415, and after A clears it
# the download is 404 and the directory shows null.
check_directory_avatar() {
  local key_a key_b png="$workdir/avatar.png" got="$workdir/avatar-got.png" json sha rc=0
  key_a="$(token_file "$agent_a")"
  key_b="$(token_file "$agent_b")"
  write_hex "$AVATAR_PNG_HEX" "$png"
  json="$(cli "$key_a" profile avatar set "$agent_a" "$png")" || {
    note="profile avatar set failed: $(cli_error)"
    return 1
  }
  expect_json "$json" '.avatar.content_type == "image/png" and (.avatar.sha256 | test("^[0-9a-f]{64}$"))' \
    "avatar set response: $json" || return 1
  sha="$(jq -r .avatar.sha256 <<<"$json")"

  json="$(cli "$key_b" agent list)" || {
    note="agent list as $agent_b failed: $(cli_error)"
    return 1
  }
  expect_json "$json" "any(.items[]; .agent_id == \"$agent_a\" and .avatar.sha256 == \"$sha\")" \
    "$agent_b does not see the avatar of $agent_a" || return 1
  json="$(cli "$key_b" profile avatar get "$agent_a" --out "$got")" || {
    note="profile avatar get failed: $(cli_error)"
    return 1
  }
  cmp -s "$png" "$got" || {
    note="downloaded avatar differs from the upload"
    return 1
  }
  api_bytes GET "/v1/agents/$agent_a/avatar" "$key_b" "" "" "If-None-Match: \"$sha\""
  expect_status 304 "If-None-Match with the ETag" || return 1

  cli "$key_b" profile avatar set "$agent_a" "$png" >/dev/null || rc=$?
  if [[ "$rc" != 1 ]] || ! grep -q 'forbidden' "$workdir/cli.stderr"; then
    note="$agent_b writing $agent_a's avatar: exit $rc, expected 1 forbidden"
    return 1
  fi
  local big="$workdir/avatar-big.png"
  head -c $((AVATAR_MAX_BYTES + 1)) /dev/zero >"$big"
  api_bytes PUT "/v1/agents/$agent_a/avatar" "$key_a" image/png "$big"
  expect_status 413 "oversized avatar" || return 1
  expect_json "$http_body" '.error.code == "avatar_too_large"' "oversize code: $http_body" || return 1
  api_bytes PUT "/v1/agents/$agent_a/avatar" "$key_a" image/gif "$png"
  expect_status 415 "image/gif avatar" || return 1
  expect_json "$http_body" '.error.code == "unsupported_media_type"' "type code: $http_body" || return 1

  cli "$key_a" profile avatar clear "$agent_a" >/dev/null || {
    note="profile avatar clear failed: $(cli_error)"
    return 1
  }
  api_bytes GET "/v1/agents/$agent_a/avatar" "$key_b" "" ""
  expect_status 404 "avatar after clear" || return 1
  expect_json "$http_body" '.error.code == "avatar_not_found"' "cleared code: $http_body" || return 1
  json="$(cli "$key_b" agent list)" || return 1
  expect_json "$json" "any(.items[]; .agent_id == \"$agent_a\" and .avatar == null)" \
    "directory still lists the cleared avatar" || return 1
  note="sha256 listed, bytes equal, 304, B 403, 413, 415, cleared 404"
}

inbox_has() {
  local agent="$1" message_id="$2" json
  json="$(cli "$(token_file "$agent")" inbox --all-pages)" || return 1
  jq -e --arg id "$message_id" 'any(.items[]; .message_id == $id)' >/dev/null <<<"$json"
}

check_all_fanout() {
  ack_head "$agent_a" || return 1
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
  not_woken "$agent_a" || {
    note="the sender's own row woke it: $(cli "$owner_token_file" delivery "$agent_a")"
    return 1
  }
  note="sender has its own row and owes no wake"
}

delivery_field() {
  cli "$owner_token_file" delivery "$1" | jq -c ".$2"
}

head_of() {
  cli "$owner_token_file" delivery "$1" | jq -r .inbox_head_seq
}

# Whether AGENT's inbox item for MESSAGE_ID has `woke` WOKE (true|false):
# the sender's rule for that recipient, never its own wake_policy (S13).
item_woke_is() {
  local agent="$1" message_id="$2" woke="$3" json
  json="$(cli "$(token_file "$agent")" inbox --all-pages)" || return 1
  jq -e --arg id "$message_id" --argjson woke "$woke" \
    'any(.items[]; .message_id == $id and .woke == $woke)' >/dev/null <<<"$json" || {
    note="$agent's item for $message_id does not have woke=$woke"
    return 1
  }
}

# Moves AGENT's marker to its head, so it owes no wake.
ack_head() {
  local head
  head="$(head_of "$1")" || return 1
  cli "$(token_file "$1")" inbox read --through-seq "$head" >/dev/null
}

# Whether AGENT owes no wake: `pending_seq` is null (its own rows and quiet
# rows never set it, ADR-0011). Callers move the marker to the head first
# (ack_head) so only the event under test counts.
not_woken() {
  local json
  json="$(cli "$owner_token_file" delivery "$1")" || return 1
  jq -e '.pending_seq == null' >/dev/null <<<"$json"
}

check_dm() {
  ack_head "$agent_a" || return 1
  send_as "$agent_a" "$agent_b" "smoke dm" smoke-dm
  expect_status 201 "send DM" || return 1
  dm_message_id="$(jq -r .message_id <<<"$http_body")"
  dm_room="$(jq -r .room_id <<<"$http_body")"
  inbox_has "$agent_b" "$dm_message_id" || {
    note="recipient inbox lacks the DM"
    return 1
  }
  inbox_has "$agent_a" "$dm_message_id" || {
    note="the sender's inbox lacks its own DM (S13: every member gets the event)"
    return 1
  }
  not_woken "$agent_a" || {
    note="the sender's own DM woke it"
    return 1
  }
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

# The owner is refused before the configuration is consulted; an agent gets
# a three-part EdDSA JWT with a kid, or 503 when [identity] is not set. The
# token stays in the private workdir and is never printed.
check_identity_token() {
  local request
  request="$(jq -nc --arg aud "$identity_aud" '{aud: $aud}')"
  api POST /v1/identity/tokens "$owner_token_file" "$request"
  expect_status 403 "owner token" || return 1
  expect_json "$http_body" '.error.code == "owner_not_allowed"' "owner code: $http_body" || return 1

  api POST /v1/identity/tokens "$(token_file "$agent_b")" "$request"
  if [[ "$http_status" == 503 ]]; then
    expect_json "$http_body" '.error.code == "identity_disabled"' "503 code: $http_body" || return 1
    skip_note="identity disabled"
    return 0
  fi
  expect_status 201 "agent token (aud $identity_aud)" || {
    note="$note: $(jq -c .error <<<"$http_body" 2>/dev/null)"
    return 1
  }
  expect_json "$http_body" "(keys == [\"aud\", \"sub\", \"token\"]) and .sub == \"$agent_b\" and .aud == \"$identity_aud\"" \
    "response is not exactly {token, sub, aud} for the caller" || return 1
  local token header claims kid
  token="$(jq -r .token <<<"$http_body")"
  [[ "$token" =~ ^[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+$ ]] || {
    note="token is not three base64url parts"
    return 1
  }
  header="$(b64url_decode "${token%%.*}")"
  expect_json "$header" '.alg == "EdDSA" and .typ == "JWT" and (.kid | test("^[0-9a-f]{8}$"))' \
    "header: $header" || return 1
  claims="$(b64url_decode "$(cut -d. -f2 <<<"$token")")"
  expect_json "$claims" ".sub == \"$agent_b\" and (.exp - .iat) > 0" "claims sub/exp" || return 1
  kid="$(jq -r .kid <<<"$header")"
  note="kid $kid, exp-iat $(jq '.exp - .iat' <<<"$claims")s, owner 403"
}

# Scenario 14, list half: the owner sees the smoke DM room with its two
# members and `all`; an agent lists its own rooms (S13), not others' DMs.
check_rooms_list() {
  local json
  json="$(cli "$owner_token_file" room list)" || {
    note="platform room list failed: $(cli_error)"
    return 1
  }
  expect_json "$json" "any(.rooms[]; .room_id == \"$dm_room\" and .kind == \"dm\"
      and .members == (\"$dm_room\" | ltrimstr(\"dm:\") | split(\":\"))
      and (.last_sender_id | type == \"string\") and .message_count >= 1)" \
    "rooms lacks $dm_room with its members" || return 1
  expect_json "$json" 'any(.rooms[]; .room_id == "all" and .kind == "all")' "rooms lacks all" || return 1
  local mine
  mine="$(cli "$(token_file "$agent_b")" room list)" || {
    note="room list as $agent_b failed: $(cli_error)"
    return 1
  }
  expect_json "$mine" "any(.rooms[]; .room_id == \"$dm_room\") and any(.rooms[]; .room_id == \"all\")
      and all(.rooms[]; .kind != \"dm\" or (.members | index(\"$agent_b\")) != null)" \
    "$agent_b's room list is not its own rooms" || return 1
  note="$(jq '.rooms | length' <<<"$json") rooms (owner), $(jq '.rooms | length' <<<"$mine") for $agent_b"
}

# Scenario 14, post half: an agent key sending sender_id is 422 and a
# sender outside post_as 403; with --post-as the owner barges into the
# smoke DM as that agent (S13: POST /v1/messages with sender_id), both smoke
# agents get the row, the room history shows it, a replay is 200 and a DM
# room with an unregistered member is 404. Bodies stay out of notes.
check_owner_room_post() {
  local request
  request="$(jq -nc --arg sender "$agent_a" --arg room "$dm_room" \
    '{sender_id: $sender, room_id: $room, body: "smoke barge-in", client_key: "smoke-barge"}')"
  api POST /v1/messages "$(token_file "$agent_a")" "$request"
  expect_status 422 "agent key with sender_id" || return 1
  expect_json "$http_body" '.error.code == "unexpected_field" and .error.details.field == "sender_id"' \
    "agent sender_id code: $http_body" || return 1
  api POST /v1/messages "$owner_token_file" "$request"
  expect_status 403 "sender outside post_as" || return 1
  expect_json "$http_body" '.error.code == "forbidden" and .error.details.field == "sender_id"' \
    "allowlist code: $http_body" || return 1
  if [[ -z "$post_as" ]]; then
    skip_note="422/403 pass; barge-in needs --post-as"
    return 0
  fi

  local key json message_id
  key="smoke-barge-$(new_ulid)"
  json="$(cli "$owner_token_file" send --as "$post_as" --room "$dm_room" --body "smoke barge-in" \
    --client-key "$key")" || {
    note="platform send --as $post_as failed: $(cli_error)"
    return 1
  }
  expect_json "$json" ".room_id == \"$dm_room\" and .sender_id == \"$post_as\"" \
    "owner post response does not match" || return 1
  message_id="$(jq -r .message_id <<<"$json")"
  local agent
  for agent in "$agent_a" "$agent_b"; do
    inbox_has "$agent" "$message_id" || {
      note="$agent inbox lacks the barge-in"
      return 1
    }
  done
  json="$(cli "$(token_file "$agent_a")" history --room "$dm_room")" || {
    note="history of $dm_room failed: $(cli_error)"
    return 1
  }
  expect_json "$json" "any(.items[]; .message_id == \"$message_id\" and .sender_id == \"$post_as\")" \
    "room history lacks the barge-in" || return 1

  request="$(jq -nc --arg sender "$post_as" --arg key "$key" --arg room "$dm_room" \
    '{sender_id: $sender, room_id: $room, body: "smoke barge-in", client_key: $key}')"
  api POST /v1/messages "$owner_token_file" "$request"
  expect_status 200 "replayed owner post" || return 1
  expect_json "$http_body" ".message_id == \"$message_id\" and .deduplicated == true" \
    "replay did not return the original" || return 1

  api POST /v1/messages "$owner_token_file" \
    "$(jq -nc --arg sender "$post_as" --arg room "$(dm_room_of "$agent_a" "smoke-unknown-$(new_ulid)")" \
      '{sender_id: $sender, room_id: $room, body: "x", client_key: "smoke-unknown-room"}')"
  expect_status 404 "room with an unregistered member" || return 1
  expect_json "$http_body" '.error.code == "unknown_room"' "unknown room code: $http_body" || return 1
  note="as $post_as: both members got it, replay 200, unknown_room 404, 422/403"
}

# The canonical DM room id of two agent ids.
dm_room_of() {
  if [[ "$1" < "$2" ]]; then
    printf 'dm:%s:%s\n' "$1" "$2"
  else
    printf 'dm:%s:%s\n' "$2" "$1"
  fi
}

# Scenario 16: Z sends a DM to A and is deleted; Z's key is 401, its inbox
# and directory entry are gone, the DM stays in A's history and the room
# list, a DM to Z is unknown_recipient and a second delete 404. Z registers
# again with a fresh inbox whose first row is inbox_seq 1. An agent key may
# not delete (403) and, with --post-as, that id is protected (409).
check_agent_deletion() {
  local key_a key_z json message_id room
  key_a="$(token_file "$agent_a")"
  register "$agent_z" || return 1
  key_z="$(token_file "$agent_z")"
  send_as "$agent_z" "$agent_a" "smoke dm from the deleted agent" smoke-deleted-dm
  expect_status 201 "DM from $agent_z" || return 1
  message_id="$(jq -r .message_id <<<"$http_body")"
  room="$(jq -r .room_id <<<"$http_body")"
  cp "$key_z" "$workdir/$agent_z.old.token"

  json="$(cli "$owner_token_file" agent delete "$agent_z" --yes)" || {
    note="platform agent delete failed: $(cli_error)"
    return 1
  }
  expect_json "$json" ".agent_id == \"$agent_z\" and .deleted == true" "delete output: $json" || return 1
  api GET /v1/inbox "$workdir/$agent_z.old.token"
  expect_status 401 "deleted agent's key" || return 1
  api GET "/v1/agents/$agent_z/inbox" "$owner_token_file"
  expect_status 404 "owner inbox of the deleted agent" || return 1
  expect_json "$http_body" '.error.code == "agent_not_found"' "inbox code: $http_body" || return 1
  json="$(cli "$owner_token_file" agent list)" || return 1
  expect_json "$json" "all(.items[]; .agent_id != \"$agent_z\")" "directory still lists $agent_z" || return 1
  json="$(cli "$key_a" history --room "$room")" || {
    note="history of $room failed: $(cli_error)"
    return 1
  }
  expect_json "$json" "any(.items[]; .message_id == \"$message_id\" and .sender_id == \"$agent_z\")" \
    "the deleted agent's DM left the history" || return 1
  json="$(cli "$owner_token_file" room list)" || return 1
  expect_json "$json" "any(.rooms[]; .room_id == \"$room\")" "room list no longer lists $room" || return 1
  send_as "$agent_a" "$agent_z" "anyone there?" "smoke-to-deleted-$(new_ulid)"
  expect_status 404 "DM to the deleted agent" || return 1
  expect_json "$http_body" '.error.code == "unknown_recipient"' "DM code: $http_body" || return 1
  api DELETE "/v1/agents/$agent_z" "$owner_token_file"
  expect_status 404 "second delete" || return 1

  register "$agent_z" || return 1
  json="$(cli "$key_z" inbox)" || return 1
  expect_json "$json" '.items == [] and .snapshot_head_seq == "0"' "re-registered inbox is not empty" || return 1
  send_as "$agent_a" "$agent_z" "welcome back" smoke-welcome-back
  expect_status 201 "DM to the re-registered agent" || return 1
  json="$(cli "$key_z" inbox)" || return 1
  expect_json "$json" '.items | length == 1 and .[0].inbox_seq == "1"' "first new row is not inbox_seq 1" || return 1

  api DELETE "/v1/agents/$agent_z" "$key_a"
  expect_status 403 "agent key on delete" || return 1
  local protected="skipped (no --post-as)"
  if [[ -n "$post_as" ]]; then
    api DELETE "/v1/agents/$post_as" "$owner_token_file"
    expect_status 409 "delete of post_as $post_as" || return 1
    expect_json "$http_body" '.error.code == "agent_protected"' "protected code: $http_body" || return 1
    protected="$post_as 409"
  fi
  api DELETE "/v1/agents/$agent_z" "$owner_token_file"
  expect_status 204 "delete of the re-registered agent" || return 1
  note="401, 404s, history and room kept, re-register seq 1, agent 403, protected: $protected"
}

# ---- S14: presence ------------------------------------------------------------

# The `.items[]` entry of AGENT in a directory JSON, compacted.
directory_item() {
  jq -c --arg agent "$2" '.items[] | select(.agent_id == $agent)' <<<"$1"
}

# Scenarios 22-24: a new agent P has never been seen and is offline; any
# request with P's key sets last_seen_at (written off the request path, so
# it is polled); P's busy declaration with --ttl 1 reaches A's directory
# with its note and expiry and then reads offline; clear withdraws at once;
# A's key may not declare for P (403); a ttl above the contract ceiling
# (86400) is ttl_too_long whatever the hub's configuration, and
# state "offline" is invalid_field. The note never goes into the notes.
check_presence() {
  local key_p key_a json item
  register "$agent_p" || return 1
  key_p="$(token_file "$agent_p")"
  key_a="$(token_file "$agent_a")"
  json="$(cli "$owner_token_file" agent list)" || {
    note="agent list failed: $(cli_error)"
    return 1
  }
  item="$(directory_item "$json" "$agent_p")"
  expect_json "$item" '.last_seen_at == null and .presence == {state: "offline", note: null, updated_at: null, expires_at: null}' \
    "new agent is not offline and never seen: $item" || return 1

  cli "$key_p" inbox >/dev/null || {
    note="inbox with the new key failed: $(cli_error)"
    return 1
  }
  local deadline=$((SECONDS + LAST_SEEN_WAIT_SECS))
  while :; do
    json="$(cli "$owner_token_file" agent list)" || return 1
    item="$(directory_item "$json" "$agent_p")"
    jq -e '.last_seen_at | type == "string"' >/dev/null <<<"$item" && break
    if ((SECONDS >= deadline)); then
      note="last_seen_at not set ${LAST_SEEN_WAIT_SECS}s after a request with the key"
      return 1
    fi
    sleep 1
  done

  json="$(cli "$key_p" presence set "$agent_p" busy --note "smoke note" --ttl 1)" || {
    note="presence set failed: $(cli_error)"
    return 1
  }
  expect_json "$json" '.presence.state == "busy" and .presence.note == "smoke note" and (.presence.expires_at | type == "string")' \
    "presence set answered without the busy declaration" || return 1
  json="$(cli "$key_a" agent list)" || return 1
  item="$(directory_item "$json" "$agent_p")"
  expect_json "$item" '.presence.state == "busy" and .presence.note == "smoke note" and (.presence.expires_at | type == "string")' \
    "another agent's directory lacks the busy declaration" || return 1
  sleep 2
  json="$(cli "$key_a" agent list)" || return 1
  item="$(directory_item "$json" "$agent_p")"
  expect_json "$item" '.presence == {state: "offline", note: null, updated_at: null, expires_at: null}' \
    "the declaration did not expire after its ttl" || return 1

  cli "$key_p" presence set "$agent_p" online >/dev/null || {
    note="presence set online failed: $(cli_error)"
    return 1
  }
  json="$(cli "$key_p" presence clear "$agent_p")" || {
    note="presence clear failed: $(cli_error)"
    return 1
  }
  json="$(cli "$owner_token_file" agent list)" || return 1
  item="$(directory_item "$json" "$agent_p")"
  expect_json "$item" '.presence.state == "offline"' "clear did not withdraw at once: $item" || return 1

  expect_cli_error forbidden "another agent declares for P" "$key_a" presence set "$agent_p" online || return 1
  expect_cli_error ttl_too_long "ttl above the ceiling" "$key_p" presence set "$agent_p" online --ttl 86401 || return 1
  api PUT "/v1/agents/$agent_p/presence" "$key_p" '{"state":"offline"}'
  expect_status 422 "state offline" || return 1
  expect_json "$http_body" '.error.code == "invalid_field" and .error.details.field == "state"' "state offline code: $http_body" || return 1
  note="never seen -> seen, busy -> expired, clear, 403, ttl_too_long, offline 422"
}

# ---- S13: topic rooms, reply_to routing, quiet sends, mentions, wake_policy ----

# Runs `platform` and expects exit 1 with CODE on stderr.
expect_cli_error() {
  local code="$1" what="$2" rc=0
  shift 2
  cli "$@" >/dev/null || rc=$?
  if [[ "$rc" == 1 ]] && grep -q "platform: $code:" "$workdir/cli.stderr"; then
    return 0
  fi
  note="$what: exit $rc, expected 1 $code ($(cli_error))"
  return 1
}

history_status() {
  api GET "/v1/messages?room_id=$2" "$(token_file "$1")"
}

# Scenario 17: the owner creates a topic room and invites A and B; A's
# message reaches B and A's own inbox without waking A; uninvited C can
# neither read nor write, reads the room from the start once invited, and
# loses it when removed; an agent key cannot create rooms; room lists are
# per agent; an archived room is read-only. The room stays archived.
check_topic_room() {
  topic_room="room:smoke-$(new_ulid)"
  register "$agent_c" || return 1
  local json message_id key_a key_c
  key_a="$(token_file "$agent_a")"
  key_c="$(token_file "$agent_c")"
  json="$(cli "$owner_token_file" room create "$topic_room" --title "smoke room")" || {
    note="room create failed: $(cli_error)"
    return 1
  }
  expect_json "$json" ".kind == \"topic\" and .members == [] and .created_by == \"owner\"" \
    "room create output: $json" || return 1
  local agent
  for agent in "$agent_a" "$agent_b"; do
    cli "$owner_token_file" room invite "$topic_room" "$agent" >/dev/null || {
      note="room invite $agent failed: $(cli_error)"
      return 1
    }
  done
  ack_head "$agent_a" || return 1
  json="$(cli "$key_a" send --room "$topic_room" --body "smoke topic")" || {
    note="send to $topic_room failed: $(cli_error)"
    return 1
  }
  message_id="$(jq -r .message_id <<<"$json")"
  inbox_has "$agent_b" "$message_id" || {
    note="$agent_b inbox lacks the topic message"
    return 1
  }
  inbox_has "$agent_a" "$message_id" || {
    note="$agent_a inbox lacks its own topic message"
    return 1
  }
  not_woken "$agent_a" || {
    note="the sender was woken by its own topic message"
    return 1
  }

  history_status "$agent_c" "$topic_room"
  expect_status 403 "uninvited history" || return 1
  expect_cli_error not_room_member "uninvited send" "$key_c" send --room "$topic_room" --body x || return 1
  cli "$owner_token_file" room invite "$topic_room" "$agent_c" >/dev/null || return 1
  json="$(cli "$key_c" history --room "$topic_room")" || {
    note="invited history failed: $(cli_error)"
    return 1
  }
  expect_json "$json" "any(.items[]; .message_id == \"$message_id\")" \
    "the invited agent does not read the earlier message" || return 1
  cli "$owner_token_file" room remove "$topic_room" "$agent_c" >/dev/null || return 1
  history_status "$agent_c" "$topic_room"
  expect_status 403 "history after removal" || return 1

  expect_cli_error forbidden "agent room create" "$key_a" room create "room:smoke-$(new_ulid)" || return 1
  json="$(cli "$key_a" room list)" || return 1
  expect_json "$json" "any(.rooms[]; .room_id == \"$topic_room\")" "$agent_a does not list $topic_room" || return 1
  json="$(cli "$key_c" room list)" || return 1
  expect_json "$json" "all(.rooms[]; .room_id != \"$topic_room\")" "$agent_c still lists $topic_room" || return 1

  json="$(cli "$owner_token_file" room archive "$topic_room")" || {
    note="room archive failed: $(cli_error)"
    return 1
  }
  expect_json "$json" '.archived_at | type == "string"' "archive output: $json" || return 1
  expect_cli_error room_archived "send to the archived room" "$key_a" send --room "$topic_room" --body x || return 1
  history_status "$agent_a" "$topic_room"
  expect_status 200 "history of the archived room" || return 1
  note="$topic_room: B and A rows, C 403/invited/removed, agent create 403, archived 409/200"
}

# Scenario 18: B answers A's DM with --reply-to alone and lands in the DM;
# --reply-to with --room is a CLI usage error and the raw request 422
# invalid_destination. With --post-as: the owner barges in as that agent,
# A answers with --reply-to and --mention, and the answer reaches that
# agent's inbox (read through the owner, which acks nothing).
check_reply_to_routing() {
  local json key_a key_b rc=0
  key_a="$(token_file "$agent_a")"
  key_b="$(token_file "$agent_b")"
  json="$(cli "$key_b" send --reply-to "$dm_message_id" --body "smoke answer")" || {
    note="send --reply-to failed: $(cli_error)"
    return 1
  }
  expect_json "$json" ".room_id == \"$dm_room\" and .reply_to == \"$dm_message_id\"" \
    "the answer is not in $dm_room: $json" || return 1
  cli "$key_b" send --reply-to "$dm_message_id" --room all --body x >/dev/null || rc=$?
  [[ "$rc" == 2 ]] || {
    note="--reply-to with --room: exit $rc, expected 2"
    return 1
  }
  api POST /v1/messages "$key_b" \
    "$(jq -nc --arg reply "$dm_message_id" '{room_id: "all", reply_to: $reply, body: "x", client_key: "smoke-both"}')"
  expect_status 422 "room_id with reply_to" || return 1
  expect_json "$http_body" '.error.code == "invalid_destination"' "destination code: $http_body" || return 1
  if [[ -z "$post_as" ]]; then
    skip_note="reply_to routing passes; the barge-in answer needs --post-as"
    return 0
  fi

  local barge answer head
  barge="$(cli "$owner_token_file" send --as "$post_as" --room "$dm_room" --body "smoke barge-in 2" | jq -r .message_id)" || {
    note="owner barge-in failed: $(cli_error)"
    return 1
  }
  head="$(cli "$owner_token_file" delivery "$post_as" | jq -r .inbox_head_seq)" || return 1
  json="$(cli "$key_a" send --reply-to "$barge" --mention "$post_as" --body "smoke answer to $post_as")" || {
    note="answer with --mention failed: $(cli_error)"
    return 1
  }
  answer="$(jq -r .message_id <<<"$json")"
  expect_json "$json" ".room_id == \"$dm_room\" and .mentions == [\"$post_as\"]" "answer: $json" || return 1
  api GET "/v1/agents/$post_as/inbox?after_seq=$head&limit=100" "$owner_token_file"
  expect_status 200 "owner view of $post_as's inbox" || return 1
  expect_json "$http_body" "any(.items[]; .message_id == \"$answer\")" \
    "$post_as's inbox lacks the answer that mentions it" || return 1
  note="reply in $dm_room, CLI exit 2, 422; $post_as got the mention (its own history 403 needs its key)"
}

# Polls AGENT's delivery until `last_accepted_seq` is SEQ.
wait_accepted() {
  local agent="$1" seq="$2" json deadline=$((SECONDS + RECEIVER_WAIT_SECS))
  while :; do
    json="$(cli "$owner_token_file" delivery "$agent")" || return 1
    jq -e --arg seq "$seq" '.last_accepted_seq == $seq' >/dev/null <<<"$json" && return 0
    if ((SECONDS >= deadline)); then
      note="$agent: no 202 for $seq within ${RECEIVER_WAIT_SECS}s: $json"
      return 1
    fi
    sleep 1
  done
}

# Watches AGENT for RECEIVER_WAIT_SECS: it must stay idle with
# `last_accepted_seq` LAST (a JSON value, e.g. null or "4").
stays_quiet() {
  local agent="$1" last="$2" json deadline=$((SECONDS + RECEIVER_WAIT_SECS))
  while ((SECONDS < deadline)); do
    json="$(cli "$owner_token_file" delivery "$agent")" || return 1
    jq -e --argjson last "$last" '.state == "idle" and .pending_seq == null and .last_accepted_seq == $last' \
      >/dev/null <<<"$json" || {
      note="$agent woke for a quiet event: $json"
      return 1
    }
    sleep 1
  done
}

# Scenario 19: with B's marker at its head and a receiver set, a quiet DM
# moves B's head but wakes nothing; the next default DM is posted with the
# new head, covering the quiet row.
check_quiet_send() {
  local key_a last head json
  key_a="$(token_file "$agent_a")"
  ack_head "$agent_b" || return 1
  cli "$(token_file "$agent_b")" receiver set "$agent_b" "$receiver_url" >/dev/null || {
    note="receiver set $agent_b failed: $(cli_error)"
    return 1
  }
  last="$(delivery_field "$agent_b" last_accepted_seq)" || return 1
  head="$(head_of "$agent_b")" || return 1
  json="$(cli "$key_a" send --to "$agent_b" --quiet --body "smoke quiet")" || {
    note="quiet send failed: $(cli_error)"
    return 1
  }
  expect_json "$json" '.wake == false' "quiet send output: $json" || return 1
  item_woke_is "$agent_b" "$(jq -r .message_id <<<"$json")" false || return 1
  [[ "$(head_of "$agent_b")" == $((head + 1)) ]] || {
    note="the quiet row did not move $agent_b's head"
    return 1
  }
  stays_quiet "$agent_b" "$last" || return 1
  json="$(cli "$key_a" send --to "$agent_b" --body "smoke wake")" || return 1
  wait_accepted "$agent_b" "$((head + 2))" || return 1
  item_woke_is "$agent_b" "$(jq -r .message_id <<<"$json")" true || return 1
  note="head moved, idle for ${RECEIVER_WAIT_SECS}s, then 202 at $((head + 2))"
}

# Scenario 20: in a topic room of A, B and C, a quiet send mentioning B
# wakes B only; mentioning a non-member is 422 mention_not_member and
# mentioning oneself 422 invalid_field (the CLI does not filter it).
check_mention_wake() {
  wake_room="room:smoke-$(new_ulid)"
  local key_a key_c last_c head_b
  key_a="$(token_file "$agent_a")"
  key_c="$(token_file "$agent_c")"
  cli "$owner_token_file" room create "$wake_room" >/dev/null || {
    note="room create failed: $(cli_error)"
    return 1
  }
  local agent
  for agent in "$agent_a" "$agent_b" "$agent_c"; do
    cli "$owner_token_file" room invite "$wake_room" "$agent" >/dev/null || return 1
  done
  ack_head "$agent_b" && ack_head "$agent_c" || return 1
  cli "$key_c" receiver set "$agent_c" "$receiver_url" >/dev/null || {
    note="receiver set $agent_c failed: $(cli_error)"
    return 1
  }
  last_c="$(delivery_field "$agent_c" last_accepted_seq)" || return 1
  head_b="$(head_of "$agent_b")" || return 1
  local mentioned
  mentioned="$(cli "$key_a" send --room "$wake_room" --quiet --mention "$agent_b" --body "smoke b?" |
    jq -r .message_id)" || {
    note="quiet send with --mention failed: $(cli_error)"
    return 1
  }
  wait_accepted "$agent_b" "$((head_b + 1))" || return 1
  stays_quiet "$agent_c" "$last_c" || return 1
  item_woke_is "$agent_b" "$mentioned" true && item_woke_is "$agent_c" "$mentioned" false || return 1
  api POST /v1/messages "$key_a" \
    "$(jq -nc --arg to "$agent_b" --arg c "$agent_c" '{to: $to, body: "x", client_key: "smoke-outsider", mentions: [$c]}')"
  expect_status 422 "mention of a non-member" || return 1
  expect_json "$http_body" '.error.code == "mention_not_member"' "outsider code: $http_body" || return 1
  expect_cli_error invalid_field "self mention" "$key_a" send --room "$wake_room" --mention "$agent_a" --body x || return 1
  note="$agent_b woken, $agent_c idle, outsider 422, self 422"
}

# Scenario 21: C switches to wake_policy=always and wakes for a quiet send
# while B does not; the wake body stays exactly {v, agent_id, inbox_seq}
# (read from --receiver-log when given).
check_wake_policy_always() {
  local key_a key_c json last_b head_c
  key_a="$(token_file "$agent_a")"
  key_c="$(token_file "$agent_c")"
  json="$(cli "$key_c" receiver set "$agent_c" "$receiver_url" --wake-policy always)" || {
    note="receiver set --wake-policy failed: $(cli_error)"
    return 1
  }
  expect_json "$json" '.wake_policy == "always"' "receiver set output: $json" || return 1
  ack_head "$agent_b" || return 1
  last_b="$(delivery_field "$agent_b" last_accepted_seq)" || return 1
  head_c="$(head_of "$agent_c")" || return 1
  local fyi
  fyi="$(cli "$key_a" send --room "$wake_room" --quiet --body "smoke fyi" | jq -r .message_id)" || return 1
  wait_accepted "$agent_c" "$((head_c + 1))" || return 1
  # Woken by its own wake_policy, not by the sender's rule.
  item_woke_is "$agent_c" "$fyi" false || return 1
  stays_quiet "$agent_b" "$last_b" || return 1
  if [[ -z "$receiver_log" ]]; then
    note="$agent_c woken, $agent_b idle (wake body keys unchecked: no --receiver-log)"
    return 0
  fi
  local body
  body="$(grep -F "\"agent_id\":\"$agent_c\"" "$receiver_log" | tail -n 1 | sed 's/.*body=//')"
  expect_json "$body" "(keys == [\"agent_id\", \"inbox_seq\", \"v\"]) and .inbox_seq == \"$((head_c + 1))\" and .v == 1" \
    "wake body is not exactly {v, agent_id, inbox_seq}: $body" || return 1
  note="$agent_c woken, $agent_b idle, body $body"
}

# Deletes the smoke agents so they stop accumulating; their messages stay.
check_cleanup() {
  local agent failed=""
  if [[ -n "$wake_room" ]]; then
    cli "$owner_token_file" room archive "$wake_room" >/dev/null || failed=" $wake_room:$(cli_error)"
  fi
  for agent in "$agent_a" "$agent_b" ${agent_c:+"$agent_c"} ${agent_p:+"$agent_p"} ${agent_r:+"$agent_r"}; do
    cli "$owner_token_file" agent delete "$agent" --yes >/dev/null || failed="$failed $agent:$(cli_error)"
  done
  [[ -z "$failed" ]] || {
    note="cleanup failed:$failed"
    return 1
  }
  note="smoke agents deleted, smoke rooms archived"
}

# ---- main ------------------------------------------------------------------

print_table() {
  local index failures=0
  printf '\n%-36s %-5s %s\n' SCENARIO RESULT NOTE
  for ((index = 0; index < ${#result_names[@]}; index++)); do
    printf '%-36s %-5s %s\n' "${result_names[$index]}" "${result_states[$index]}" "${result_notes[$index]}"
    [[ "${result_states[$index]}" != FAIL ]] || failures=$((failures + 1))
  done
  printf '\nagents: %s %s %s %s %s%s\n' "$agent_a" "$agent_b" "$agent_c" "$agent_z" "$agent_p" "${agent_r:+ $agent_r}"
  if [[ "$failures" -gt 0 ]]; then
    printf 'smoke: %d FAIL\n' "$failures"
    return 1
  fi
  printf 'smoke: all PASS\n'
}

main() {
  parse_args "$@"
  require_cmd curl jq od seq cmp head "$platform_bin"
  workdir="$(mktemp -d)"
  chmod 0700 "$workdir"
  # shellcheck disable=SC2064 # expand now
  trap "rm -rf '$workdir'" EXIT

  agent_a="smoke-$(new_ulid)"
  agent_b="smoke-$(new_ulid)"
  agent_z="smoke-$(new_ulid)"
  agent_c="smoke-$(new_ulid)"
  agent_p="smoke-$(new_ulid)"
  log "smoke against $base_url as $agent_a, $agent_b"

  scenario "healthz / platform health" check_health
  scenario "register (one-time credentials)" check_register
  if [[ "${result_states[1]}" != PASS ]]; then
    print_table
    die "registration failed; later scenarios need the agents"
  fi
  scenario "directory hides credentials" check_directory_hides_credentials
  scenario "directory profile (13)" check_directory_profile
  scenario "directory avatar (15)" check_directory_avatar
  scenario "all fanout (1)" check_all_fanout
  scenario "DM to both members (2)" check_dm
  scenario "unknown 404 / self DM 422 (2)" check_dm_errors
  scenario "idempotency 200 / 409 (4)" check_idempotency
  scenario "inbox paging snapshot (10)" check_inbox_paging
  scenario "read marker ahead/no-op (10)" check_read_marker
  scenario "owner reads do not ack (12)" check_owner_no_ack
  scenario "delivery no_receiver" check_delivery_no_receiver
  scenario "rotate: old key 401 (12)" check_rotate
  scenario "rooms list (14)" check_rooms_list
  scenario "owner room post (14)" check_owner_room_post
  if [[ -n "$receiver_url" ]]; then
    agent_r="smoke-$(new_ulid)"
    scenario "receiver 202 keeps marker (5)" check_receiver
  else
    record "receiver 202 keeps marker (5)" SKIP "no --receiver-url"
  fi
  scenario "identity token" check_identity_token
  scenario "agent deletion (16)" check_agent_deletion
  scenario "presence (22-24)" check_presence
  scenario "topic room (17)" check_topic_room
  scenario "reply_to routing (18)" check_reply_to_routing
  if [[ -n "$receiver_url" ]]; then
    scenario "quiet send (19)" check_quiet_send
    scenario "mention wake (20)" check_mention_wake
    scenario "wake_policy always (21)" check_wake_policy_always
  else
    local skipped
    for skipped in "quiet send (19)" "mention wake (20)" "wake_policy always (21)"; do
      record "$skipped" SKIP "no --receiver-url"
    done
  fi
  scenario "cleanup (delete smoke agents)" check_cleanup
  print_table
}

main "$@"
