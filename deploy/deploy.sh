#!/usr/bin/env bash
# Builds platformd for the Pi and installs it as platformd.service.
#
# Usage: deploy/deploy.sh <ssh-alias> [--first-install] [--dry-run]
#   <ssh-alias>       Host alias from ~/.ssh/config (never written to the repository)
#   --first-install   Create the service account and directories, enable --now the unit
#                     (default: redeploy = install the new binaries and restart the unit)
#   --dry-run         Print every build, scp and ssh command without running any of them
#
# Steps: deploy/build-aarch64.sh -> scp the binaries, the unit, the Pi example
# config, remote-install.sh and SHA256SUMS to <alias>:/tmp/platformd-deploy/
# -> `ssh -t <alias> sudo bash .../remote-install.sh` (one sudo password
# prompt; see that script for the remote steps) -> local summary.
set -euo pipefail

# shellcheck source=deploy/lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

readonly UNIT=platformd.service
readonly REMOTE_STAGING=/tmp/platformd-deploy
readonly EXIT_NEEDS_OPERATOR=3

ROOT="$(repo_root)"
readonly ROOT
readonly BUILD_SCRIPT="$ROOT/deploy/build-aarch64.sh"
readonly BIN_OUT="$ROOT/target/aarch64/aarch64-unknown-linux-gnu/release"
readonly STAGE_SOURCES=(
  "$BIN_OUT/platformd"
  "$BIN_OUT/platform"
  "$ROOT/deploy/$UNIT"
  "$ROOT/deploy/platformd.pi.example.toml"
  "$ROOT/deploy/remote-install.sh"
)

alias_name=""
first_install=false
dry_run=false

usage() {
  sed -n '4,8p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2
  exit 2
}

parse_args() {
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --first-install) first_install=true ;;
      --dry-run) dry_run=true ;;
      -h | --help) usage ;;
      -*) usage ;;
      *)
        [[ -z "$alias_name" ]] || usage
        alias_name="$1"
        ;;
    esac
    shift
  done
  [[ -n "$alias_name" ]] || usage
  validate_ssh_alias "$alias_name"
}

# Prints the command; runs it unless --dry-run.
run() {
  printf '+ %s\n' "$*" >&2
  [[ "$dry_run" == true ]] && return 0
  "$@"
}

build() {
  log "build aarch64 binaries"
  run "$BUILD_SCRIPT"
}

# SHA256SUMS lists the staged basenames; remote-install.sh checks it.
write_checksums() {
  local sums="$1"
  log "checksums of the staged files"
  if [[ "$dry_run" == true ]]; then
    printf '+ shasum -a 256 <staged files> > %s\n' "$sums" >&2
    return
  fi
  local source
  for source in "${STAGE_SOURCES[@]}"; do
    [[ -f "$source" ]] || die "missing build output or deploy file: $source"
  done
  (
    cd "$BIN_OUT" && shasum -a 256 platformd platform
    cd "$ROOT/deploy" && shasum -a 256 "$UNIT" platformd.pi.example.toml remote-install.sh
  ) >"$sums"
  cat "$sums" >&2
}

stage() {
  local sums="$1"
  log "stage files in $alias_name:$REMOTE_STAGING (0700)"
  run ssh "$alias_name" "rm -rf $REMOTE_STAGING && mkdir -m 0700 $REMOTE_STAGING"
  run scp -q "${STAGE_SOURCES[@]}" "$sums" "$alias_name:$REMOTE_STAGING/"
}

remote_install() {
  local args=()
  [[ "$first_install" == true ]] && args+=(--first-install)
  log "remote install (sudo asks for the password once)"
  # ${args[@]+...}: bash 3.2 treats an empty array as unset under `set -u`.
  run ssh -t "$alias_name" sudo bash "$REMOTE_STAGING/remote-install.sh" ${args[@]+"${args[@]}"}
}

summary() {
  local status="$1"
  local mode=redeploy
  [[ "$first_install" == true ]] && mode="first install"
  log "summary: host $alias_name, $mode, unit $UNIT, git $(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)"
  if [[ "$dry_run" == true ]]; then
    log "dry run: nothing was built, copied or run remotely"
    return
  fi
  case "$status" in
    0) log "result: installed, healthz 200, coexisting units unchanged" ;;
    "$EXIT_NEEDS_OPERATOR") log "result: stopped before installing; create the files listed above on the Pi and rerun" ;;
    *) log "result: FAILED (remote exit $status); see the output above" ;;
  esac
}

main() {
  parse_args "$@"
  if [[ "$dry_run" == false ]]; then
    require_cmd ssh scp shasum docker git
  fi

  local workdir
  workdir="$(mktemp -d)"
  # shellcheck disable=SC2064 # expand now: workdir is local
  trap "rm -rf '$workdir'" EXIT
  local sums="$workdir/SHA256SUMS"

  build
  write_checksums "$sums"
  stage "$sums"
  local status=0
  remote_install || status=$?
  summary "$status"
  return "$status"
}

main "$@"
