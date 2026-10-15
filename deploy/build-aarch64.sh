#!/usr/bin/env bash
# Builds the deployment binaries for the Raspberry Pi (ADR-0002):
# aarch64-unknown-linux-gnu, inside a native linux/aarch64 Debian trixie
# container, so the binaries link against glibc <= the Pi's 2.41.
#
# Output: target/aarch64/aarch64-unknown-linux-gnu/release/{platformd,platform}
#
# Environment:
#   RUST_IMAGE         container image (default rust:1-trixie)
#   PLATFORMD_GIT_SHA  embedded in `platformd --version` (default: git short HEAD)
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

image="${RUST_IMAGE:-rust:1-trixie}"
triple="aarch64-unknown-linux-gnu"
target_dir="target/aarch64"
out_dir="$target_dir/$triple/release"
git_sha="${PLATFORMD_GIT_SHA:-$(git rev-parse --short HEAD 2>/dev/null || echo unknown)}"

# Runs inside the container. Build first, then report the toolchain's glibc
# and the newest GLIBC_x.y symbol version each binary requires.
container_script='
set -euo pipefail
cargo build --release --locked --workspace --target "$TRIPLE" --target-dir "$TARGET_DIR"
echo "== container glibc"
ldd --version | head -n 1
for bin in platformd platform; do
  path="$TARGET_DIR/$TRIPLE/release/$bin"
  max_glibc="$(objdump -T "$path" | grep -o "GLIBC_[0-9][0-9.]*" | sort -u -V | tail -n 1)"
  echo "== $bin"
  echo "path: $path"
  echo "glibc required (max symbol version): ${max_glibc:-none}"
done
'

echo "== build image: $image (linux/aarch64), git sha: $git_sha"
start_seconds=$SECONDS
docker run --rm --platform linux/aarch64 \
  -v "$repo_root":/work -w /work \
  -v platformd-cargo-registry:/usr/local/cargo/registry \
  -e PLATFORMD_GIT_SHA="$git_sha" \
  -e TRIPLE="$triple" \
  -e TARGET_DIR="$target_dir" \
  "$image" bash -c "$container_script"
elapsed=$((SECONDS - start_seconds))

echo "== host file(1)"
for bin in platformd platform; do
  file "$out_dir/$bin"
done
echo "== build time: ${elapsed}s"
