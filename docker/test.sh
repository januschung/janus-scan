#!/bin/sh
# Build + test steps run inside every image. Expects the repo mounted at /src
# (read-only is fine) and CARGO_TARGET_DIR on a volume so builds are cached.
set -eu

step() { printf '\n==> %s\n' "$*"; }

step "distro"
. /etc/os-release && echo "$PRETTY_NAME"
# Mint's Docker images keep Ubuntu's os-release; the Mint repo line says which Mint.
grep -hs -m1 packages.linuxmint.com /etc/apt/sources.list.d/*.list || true

step "webkit2gtk-4.1"
pkg-config --modversion webkit2gtk-4.1

step "SANE test backend"
scanimage --version
scanimage -L | tee /tmp/scanimage-L.txt
grep -q "test:0" /tmp/scanimage-L.txt || { echo "test:0 not listed by scanimage -L" >&2; exit 1; }

# Work on a copy so the host checkout (and its Cargo.lock / target/) is never
# written to. Copying preserves mtimes, so cargo's fingerprints stay valid and
# the cached target dir is reused across runs.
step "copy sources"
mkdir -p /work
tar -C /src --exclude=./target --exclude=./.git --exclude=node_modules -cf - . | tar -C /work -xf -
cd /work

step "cargo test --workspace"
cargo --version
cargo test --workspace -- --nocapture
