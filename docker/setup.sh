#!/bin/sh
# Image setup shared by apt.Dockerfile and pacman.Dockerfile: the system
# packages (scripts/linux-deps.sh, the same script real machines use), SANE's
# virtual `test` backend, and Rust via rustup.
set -eu

sh /opt/janus-scan/linux-deps.sh
rm -rf /var/lib/apt/lists/* /var/cache/pacman/pkg/*

# SANE ships the `test` backend disabled; enable it so `test:0` shows up.
sed -i -E 's/^#[[:space:]]*test[[:space:]]*$/test/' /etc/sane.d/dll.conf
grep -qx 'test' /etc/sane.d/dll.conf || echo test >> /etc/sane.d/dll.conf

# Use rustup rather than the distro rustc so every image has the same, current toolchain.
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --profile minimal --default-toolchain stable --no-modify-path
chmod -R a+w "$RUSTUP_HOME" "$CARGO_HOME"
rustc --version
cargo --version
