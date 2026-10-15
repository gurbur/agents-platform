#!/usr/bin/env bash
# Takes an online snapshot of the Pi's database and copies it to this machine.
#
# Usage: deploy/backup.sh <ssh-alias> <local-dir>
#   <ssh-alias>  Host alias from ~/.ssh/config (never written to the repository)
#   <local-dir>  Directory that receives platform-<UTC>.db (created if missing)
#
# On the Pi, as the platformd account (`ssh -t`; sudo asks for the password):
#   platformd backup --config /etc/platformd/platformd.toml --out /tmp/platformd-backup/platform-<UTC>.db
# then scp to <local-dir>, delete the remote copy, and, if sqlite3 is on PATH,
# run PRAGMA integrity_check locally and compare every table's row count with
# the counts platformd printed on the Pi.
#
# The snapshot holds receiver secrets in plain text: keep <local-dir> private.
set -euo pipefail

# shellcheck source=deploy/lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

readonly REMOTE_BIN=/usr/local/bin/platformd
readonly REMOTE_CONFIG=/etc/platformd/platformd.toml
readonly REMOTE_DIR=/tmp/platformd-backup
readonly SERVICE_USER=platformd

usage() {
  sed -n '4,6p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2
  exit 2
}

# One remote command: sudo runs a root shell that prepares the directory,
# runs the backup as the service account and hands the file to the SSH user
# so scp can read it.
remote_backup_command() {
  local remote_file="$1"
  printf "sudo sh -c '%s'" \
    "install -d -m 0700 -o $SERVICE_USER -g $SERVICE_USER $REMOTE_DIR \
&& sudo -u $SERVICE_USER $REMOTE_BIN backup --config $REMOTE_CONFIG --out $remote_file \
&& chown -R \"\$SUDO_USER\" $REMOTE_DIR"
}

take_snapshot() {
  local alias="$1" remote_file="$2" report="$3"
  log "snapshot on $alias -> $remote_file"
  # tee passes the sudo prompt through unbuffered. The pty turns line ends
  # into CRLF, so the CRs are stripped afterwards for parsing.
  local raw="$report.raw"
  ssh -t "$alias" "$(remote_backup_command "$remote_file")" | tee "$raw"
  tr -d '\r' <"$raw" >"$report"
  rm -f "$raw"
  grep -q '^integrity_check: ok$' "$report" || die "remote backup did not report integrity_check: ok"
}

fetch_snapshot() {
  local alias="$1" remote_file="$2" local_file="$3"
  log "copy to $local_file"
  scp -q "$alias:$remote_file" "$local_file"
  chmod 0600 "$local_file"
  log "remove the remote copy"
  # shellcheck disable=SC2029 # the paths are meant to expand here
  ssh "$alias" "rm -f -- $remote_file && rmdir -- $REMOTE_DIR"
}

# Local check with sqlite3, when it is installed.
verify_locally() {
  local local_file="$1" report="$2"
  if ! command -v sqlite3 >/dev/null 2>&1; then
    log "sqlite3 not on PATH: local integrity_check and row-count comparison skipped"
    return
  fi
  log "local integrity_check ($(command -v sqlite3))"
  local integrity
  integrity="$(sqlite3 -readonly "$local_file" 'PRAGMA integrity_check;')"
  [[ "$integrity" == ok ]] || die "local integrity_check: $integrity"
  log "local integrity_check: ok"

  local mismatches=0 table expected actual
  while read -r _ table expected; do
    actual="$(sqlite3 -readonly "$local_file" "SELECT COUNT(*) FROM \"$table\";")"
    if [[ "$actual" == "$expected" ]]; then
      printf '  %-22s %8s rows  match\n' "$table" "$actual"
    else
      printf '  %-22s %8s rows  MISMATCH (Pi reported %s)\n' "$table" "$actual" "$expected"
      mismatches=$((mismatches + 1))
    fi
  done < <(grep '^row_count ' "$report")
  [[ "$mismatches" -eq 0 ]] || die "$mismatches table(s) differ from the Pi's report"
  log "row counts match the Pi's report"
}

main() {
  [[ $# -eq 2 ]] || usage
  local alias="$1" local_dir="$2"
  validate_ssh_alias "$alias"
  require_cmd ssh scp
  mkdir -p "$local_dir"

  local stamp name
  stamp="$(date -u +%Y%m%dT%H%M%SZ)"
  name="platform-$stamp.db"
  local remote_file="$REMOTE_DIR/$name"
  local local_file="$local_dir/$name"
  [[ ! -e "$local_file" ]] || die "$local_file already exists"
  local report
  report="$(mktemp)"
  # shellcheck disable=SC2064 # expand now: report is local
  trap "rm -f '$report' '$report.raw'" EXIT

  take_snapshot "$alias" "$remote_file" "$report"
  fetch_snapshot "$alias" "$remote_file" "$local_file"
  verify_locally "$local_file" "$report"
  log "backup saved: $local_file"
}

main "$@"
