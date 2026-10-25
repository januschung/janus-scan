#!/bin/sh
# Build the Linux packages with Docker, on a Mac or any machine with Docker:
#   - a .deb for Ubuntu, Linux Mint and Linux Lite
#   - an Arch package for CachyOS (the same app, packaged for pacman)
# Packages land in target/linux-packages/. Compiled code is cached in Docker
# volumes, so later runs are much faster than the first.
#
#   sh scripts/package-linux.sh
set -eu
cd "$(dirname "$0")/.."
OUT=target/linux-packages
mkdir -p "$OUT"

step() { printf '\n==> %s\n' "$*"; }

step "web front end"
(cd app && npm run build)

step "Ubuntu 24.04 build image"
docker build --platform linux/amd64 --build-arg BASE=ubuntu:24.04 -f docker/apt.Dockerfile -t janus-scan-build:ubuntu-24-04 .

step ".deb (Ubuntu, Linux Mint, Linux Lite)"
docker run --rm --platform linux/amd64 \
    -v "$PWD":/src:ro -v "$PWD/$OUT":/out \
    -v janus-scan-cargo-registry:/usr/local/cargo/registry -v janus-scan-linux-target:/target \
    janus-scan-build:ubuntu-24-04 sh /src/docker/package-deb.sh

step "Arch package (CachyOS)"
docker run --rm --platform linux/amd64 \
    -v "$PWD/$OUT":/out -v "$PWD/packaging/arch":/pkg:ro -v "$PWD/LICENSE":/pkg-license/LICENSE:ro \
    archlinux:latest sh /pkg/build.sh

step "done"
ls -l "$OUT"
