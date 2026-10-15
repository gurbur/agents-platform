#!/usr/bin/env bash
# Read-only preflight of the operations Pi: prints disk, memory, listening
# ports, running services, OS/glibc, cgroup controllers, whether platformd is
# already installed and whether sudo needs a password. Runs no command that
# changes the Pi, and no sudo except the non-interactive `sudo -n true` probe.
#
# Usage: deploy/preflight.sh <ssh-alias>
#   <ssh-alias>  Host alias from ~/.ssh/config (never written to the repository)
set -euo pipefail

# shellcheck source=deploy/lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

usage() {
  cat >&2 <<'USAGE'
Usage: deploy/preflight.sh <ssh-alias>
  <ssh-alias>  Host alias from ~/.ssh/config (never written to the repository)
USAGE
  exit 2
}

# The script that runs on the Pi via `bash -s`. Every command only reads.
remote_preflight() {
  cat <<'REMOTE'
set -uo pipefail
section() { printf "\n## %s\n" "$1"; }
show_path() {
  if [ -e "$1" ]; then ls -ld "$1"; else echo "$1: absent"; fi
}

section "measured at (UTC)"
date -u +%Y-%m-%dT%H:%M:%SZ
section "os"
sed -n "s/^PRETTY_NAME=//p" /etc/os-release
uname -srm
section "glibc"
ldd --version | head -n 1
section "cpu"
echo "nproc: $(nproc)"
echo "loadavg: $(cat /proc/loadavg)"
section "memory"
free -h
section "disk"
df -h / /tmp /var/lib
section "journald"
journalctl --disk-usage 2>&1 || echo "journalctl --disk-usage: not readable as this user"
section "cgroup v2 controllers (MemoryMax/CPUQuota/TasksMax need memory, cpu, pids)"
cat /sys/fs/cgroup/cgroup.controllers 2>&1 || echo "cgroup.controllers: not readable"
section "listening tcp"
ss -ltn
section "port 7340"
if ss -ltnH "sport = :7340" | grep -q .; then echo "7340: in use"; else echo "7340: free"; fi
section "running services"
systemctl list-units --type=service --state=running --no-pager --no-legend --plain
section "platformd footprint"
id platformd 2>/dev/null || echo "user platformd: absent"
for path in /etc/platformd /var/lib/platformd /usr/local/bin/platformd \
            /usr/local/bin/platform /etc/systemd/system/platformd.service; do
  show_path "$path"
done
echo "platformd.service: $(systemctl is-active platformd.service 2>/dev/null || true)"
section "sudo"
if sudo -n true 2>/dev/null; then echo "sudo -n: ok (no password)"; else echo "sudo -n: password required"; fi
section "tools"
for tool in systemd-analyze curl install sha256sum journalctl; do
  command -v "$tool" || echo "$tool: missing"
done
if [ -x /usr/sbin/useradd ]; then echo /usr/sbin/useradd; else echo "useradd: missing"; fi
REMOTE
}

main() {
  [[ $# -eq 1 ]] || usage
  local alias="$1"
  validate_ssh_alias "$alias"
  require_cmd ssh

  log "preflight (read-only) on $alias"
  remote_preflight | ssh "$alias" bash -s
  log "preflight done; record the figures in TECH_STACK_AND_OPERATIONS.md with the date"
}

main "$@"
