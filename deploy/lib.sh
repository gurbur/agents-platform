# shellcheck shell=bash
# Shared helpers for the scripts in deploy/ that run on the development
# machine. Sourced, not executed. Written for bash 3.2 (macOS /bin/bash).

# One log line per step, on stderr so stdout stays usable for data.
log() {
  printf '== %s\n' "$*" >&2
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

require_cmd() {
  local cmd
  for cmd in "$@"; do
    command -v "$cmd" >/dev/null 2>&1 || die "required command not found: $cmd"
  done
}

# An SSH host alias from ~/.ssh/config. Rejects option-looking and
# shell-special values; the alias is passed to ssh/scp as a word.
validate_ssh_alias() {
  local alias="$1"
  [[ -n "$alias" ]] || die "missing <ssh-alias>"
  [[ "$alias" != -* ]] || die "ssh alias must not start with '-': $alias"
  [[ "$alias" =~ ^[A-Za-z0-9._@-]+$ ]] || die "ssh alias has unexpected characters: $alias"
}

# The repository root, resolved from this file's location.
repo_root() {
  cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd
}
