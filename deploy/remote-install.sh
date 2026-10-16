#!/usr/bin/env bash
# Remote stage of deploy/deploy.sh. Runs on the Pi as root, once per deploy,
# so the operator types the sudo password once:
#
#   sudo bash /tmp/platformd-deploy/remote-install.sh [--first-install]
#
# Installs platformd's own files only. The single unit it writes to, starts or
# restarts is platformd.service (UNIT below). The coexisting units are only
# read with `systemctl show`, before and after, to prove they were untouched.
#
# Exit codes: 0 installed and healthy; 1 failure; 2 usage; 3 the operator has
# to create or fix /etc/platformd files first (instructions are printed).
set -euo pipefail

readonly UNIT=platformd.service
readonly SERVICE_USER=platformd
readonly STAGING_DIR=/tmp/platformd-deploy
readonly CONFIG_DIR=/etc/platformd
readonly CONFIG_FILE=$CONFIG_DIR/platformd.toml
readonly TOKEN_FILE=$CONFIG_DIR/owner.token
# Suggested path in the operator steps. Install requires a key file only
# when the config enables [identity] (see identity_key_file).
readonly IDENTITY_KEY_FILE=$CONFIG_DIR/identity.key
readonly DATA_DIR=/var/lib/platformd
readonly BIN_DIR=/usr/local/bin
readonly UNIT_PATH=/etc/systemd/system/$UNIT
readonly DEFAULT_LISTEN=127.0.0.1:7340
readonly HEALTH_WAIT_SECS=20
readonly EXIT_NEEDS_OPERATOR=3
readonly STAGED_FILES=(platformd platform "$UNIT" platformd.pi.example.toml SHA256SUMS)
# Read with `systemctl show` only (TECH_STACK_AND_OPERATIONS.md "배포와 기존
# 서비스 공존"). Never passed to any other systemctl verb.
readonly COEXISTING_UNITS=(
  cloudflared.service
  garage-docs.service
  my-garage-lab-webserver.service
  tailscaled.service
)

first_install=false
coexisting_before=""

log() {
  printf '== [pi] %s\n' "$*"
}

die() {
  printf 'error: [pi] %s\n' "$*" >&2
  exit 1
}

# The only systemctl entry point that can change state: the unit is fixed.
platformd_systemctl() {
  systemctl "$@" "$UNIT"
}

parse_args() {
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --first-install) first_install=true ;;
      *)
        printf 'usage: remote-install.sh [--first-install]\n' >&2
        exit 2
        ;;
    esac
    shift
  done
}

require_root() {
  [[ $EUID -eq 0 ]] || die "run with sudo"
}

# The staging directory is created 0700 by the deploying user; anything
# else could have been tampered with by another account.
verify_staging() {
  log "verify staged files in $STAGING_DIR"
  [[ -d "$STAGING_DIR" && ! -L "$STAGING_DIR" ]] || die "$STAGING_DIR is missing"
  local mode
  mode="$(stat -c '%a' "$STAGING_DIR")"
  [[ "$mode" == 700 ]] || die "$STAGING_DIR has mode $mode, expected 700"
  local file
  for file in "${STAGED_FILES[@]}"; do
    [[ -f "$STAGING_DIR/$file" ]] || die "staged file missing: $file"
  done
  (cd "$STAGING_DIR" && sha256sum --quiet --strict -c SHA256SUMS) ||
    die "staged files do not match SHA256SUMS"
}

# One line per unit: Id=… ActiveState=… ExecMainStartTimestamp=…
snapshot_coexisting_units() {
  systemctl show --property=Id,ActiveState,ExecMainStartTimestamp -- "${COEXISTING_UNITS[@]}" |
    awk 'BEGIN { RS = ""; FS = "\n"; OFS = "  " } { $1 = $1; print }'
}

# Runs on exit once the "before" snapshot exists, on success and failure
# alike. A difference turns a successful exit into a failure.
report_coexisting_units() {
  local status=$?
  local after
  after="$(snapshot_coexisting_units)"
  log "coexisting units after the deploy"
  printf '%s\n' "$after"
  if [[ "$after" == "$coexisting_before" ]]; then
    log "coexisting units: unchanged (same ActiveState and ExecMainStartTimestamp as before)"
    exit "$status"
  fi
  printf 'error: [pi] coexisting units changed during the deploy:\n' >&2
  diff <(printf '%s\n' "$coexisting_before") <(printf '%s\n' "$after") >&2 || true
  exit 1
}

check_install_mode() {
  if [[ "$first_install" == true ]]; then
    if [[ -e "$UNIT_PATH" ]] && platformd_systemctl is-active --quiet; then
      die "$UNIT is already installed and running; redeploy without --first-install"
    fi
    log "mode: first install"
    return
  fi
  [[ -e "$UNIT_PATH" ]] || die "$UNIT_PATH does not exist; run deploy.sh with --first-install"
  log "mode: redeploy"
}

ensure_service_account() {
  if id -u "$SERVICE_USER" >/dev/null 2>&1; then
    log "service account $SERVICE_USER exists"
    return
  fi
  log "create system account $SERVICE_USER (no login, no home)"
  /usr/sbin/useradd --system --user-group --no-create-home \
    --home-dir /nonexistent --shell /usr/sbin/nologin "$SERVICE_USER"
}

ensure_config_dir() {
  log "ensure $CONFIG_DIR (0750 root:$SERVICE_USER)"
  install -d -m 0750 -o root -g "$SERVICE_USER" "$CONFIG_DIR"
}

print_operator_steps() {
  cat >&2 <<STEPS

The operator creates these files on the Pi, then runs deploy.sh again
with the same arguments. Nothing was started.

  # owner token: 48 random bytes, base64 (the token never leaves the Pi)
  sudo sh -c 'umask 0027 && openssl rand -base64 48 > $TOKEN_FILE'
  sudo chgrp $SERVICE_USER $TOKEN_FILE

  # configuration: copy the example and set listen to the Tailscale address
  # (\`tailscale ip -4\`), port 7340
  sudo install -m 0640 -o root -g $SERVICE_USER $STAGING_DIR/platformd.pi.example.toml $CONFIG_FILE
  sudoedit $CONFIG_FILE

  # OPTIONAL identity attestation signing key (skip unless [identity] is
  # wanted). keygen prints kid and public_key for the consumer's config; the
  # seed never leaves the Pi. Then uncomment [identity] in $CONFIG_FILE.
  # Before the first install completes, run $STAGING_DIR/platformd instead.
  sudo platformd identity keygen --out $IDENTITY_KEY_FILE
  sudo chown root:$SERVICE_USER $IDENTITY_KEY_FILE
  sudo chmod 0640 $IDENTITY_KEY_FILE

STEPS
}

# `signing_key_file` of an uncommented [identity] table, relative paths
# resolved against the config directory (IF-CFG-001); empty without one.
identity_key_file() {
  local path
  path="$(awk '
    /^[[:space:]]*\[/ { in_identity = /^[[:space:]]*\[identity\]/; next }
    in_identity && /^[[:space:]]*signing_key_file[[:space:]]*=/ {
      sub(/^[^"]*"/, ""); sub(/".*$/, ""); print; exit
    }' "$CONFIG_FILE")"
  [[ -z "$path" || "$path" == /* ]] || path="$CONFIG_DIR/$path"
  printf '%s\n' "$path"
}

# Stops before anything is installed when the operator's files are missing
# or still hold the example placeholder. The script never creates the token
# or the identity key; the key is required only when [identity] is enabled.
check_operator_files() {
  local missing=() identity_key=""
  [[ -f "$CONFIG_FILE" ]] || missing+=("$CONFIG_FILE")
  [[ -f "$TOKEN_FILE" ]] || missing+=("$TOKEN_FILE")
  if [[ -f "$CONFIG_FILE" ]]; then
    identity_key="$(identity_key_file)"
    [[ -z "$identity_key" || -f "$identity_key" ]] || missing+=("$identity_key")
  fi
  if [[ ${#missing[@]} -gt 0 ]]; then
    printf 'error: [pi] missing: %s\n' "${missing[*]}" >&2
    print_operator_steps
    exit "$EXIT_NEEDS_OPERATOR"
  fi
  if grep -q '100\.x\.y\.z' "$CONFIG_FILE"; then
    printf 'error: [pi] %s still has the placeholder listen address 100.x.y.z\n' "$CONFIG_FILE" >&2
    print_operator_steps
    exit "$EXIT_NEEDS_OPERATOR"
  fi
  local secured=("$CONFIG_FILE" "$TOKEN_FILE")
  [[ -z "$identity_key" ]] || secured+=("$identity_key")
  log "operator files present; enforce 0640 root:$SERVICE_USER on ${secured[*]}"
  chown root:"$SERVICE_USER" "${secured[@]}"
  chmod 0640 "${secured[@]}"
}

ensure_data_dir() {
  log "ensure $DATA_DIR (0750 $SERVICE_USER:$SERVICE_USER)"
  install -d -m 0750 -o "$SERVICE_USER" -g "$SERVICE_USER" "$DATA_DIR"
}

install_binaries() {
  local bin
  for bin in platformd platform; do
    log "install $BIN_DIR/$bin (0755 root:root)"
    install -m 0755 -o root -g root "$STAGING_DIR/$bin" "$BIN_DIR/$bin"
    cmp -s "$STAGING_DIR/$bin" "$BIN_DIR/$bin" || die "$BIN_DIR/$bin differs from the staged file"
  done
  log "installed: $("$BIN_DIR/platformd" --version)"
}

# Verified from staging, after the binaries and the account exist, so a unit
# that fails verification is never installed.
verify_unit() {
  log "systemd-analyze verify $UNIT"
  local output
  if ! output="$(systemd-analyze verify "$STAGING_DIR/$UNIT" 2>&1)"; then
    printf '%s\n' "$output" >&2
    die "systemd-analyze verify failed"
  fi
  if [[ -n "$output" ]]; then
    printf '%s\n' "$output"
    log "systemd-analyze verify: passed with the messages above"
  else
    log "systemd-analyze verify: no messages"
  fi
}

install_unit() {
  log "install $UNIT_PATH (0644 root:root) and daemon-reload"
  install -m 0644 -o root -g root "$STAGING_DIR/$UNIT" "$UNIT_PATH"
  systemctl daemon-reload
}

start_service() {
  if [[ "$first_install" == true ]]; then
    log "systemctl enable --now $UNIT"
    platformd_systemctl enable --now
  else
    log "systemctl restart $UNIT"
    platformd_systemctl restart
  fi
}

# `listen = "host:port"` from the config, or the IF-CFG-001 default.
listen_address() {
  local listen
  listen="$(sed -n 's/^[[:space:]]*listen[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$CONFIG_FILE" | head -n 1)"
  printf '%s\n' "${listen:-$DEFAULT_LISTEN}"
}

show_journal() {
  journalctl -u "$UNIT" -n 20 --no-pager || true
}

wait_for_health() {
  local url
  url="http://$(listen_address)/healthz"
  log "wait up to ${HEALTH_WAIT_SECS}s for $url"
  local deadline=$((SECONDS + HEALTH_WAIT_SECS))
  local body
  until body="$(curl -fsS --max-time 2 "$url" 2>/dev/null)"; do
    if ((SECONDS >= deadline)); then
      show_journal >&2
      die "$url did not answer 200 within ${HEALTH_WAIT_SECS}s"
    fi
    sleep 0.5
  done
  log "healthz 200: $body"
  local state
  state="$(platformd_systemctl is-active)" || die "$UNIT is not active: $state"
  log "is-active: $state"
}

main() {
  parse_args "$@"
  require_root
  verify_staging

  coexisting_before="$(snapshot_coexisting_units)"
  log "coexisting units before the deploy"
  printf '%s\n' "$coexisting_before"
  trap report_coexisting_units EXIT

  check_install_mode
  ensure_service_account
  ensure_config_dir
  check_operator_files
  ensure_data_dir
  install_binaries
  verify_unit
  install_unit
  start_service
  wait_for_health
  log "journalctl -u $UNIT -n 20"
  show_journal

  log "remove $STAGING_DIR"
  rm -rf -- "$STAGING_DIR"
  log "deploy finished"
}

main "$@"
