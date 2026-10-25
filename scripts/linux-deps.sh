#!/bin/sh
# Installs the system packages Janus Scan needs on Linux: build tools, SANE,
# and the Tauri 2 (WebKitGTK 4.1) build dependencies. Used on real machines
# (`sudo sh scripts/linux-deps.sh`) and by the Docker images (docker/setup.sh).
# Rust, Node.js and Brother's driver are installed separately; see the
# README's Install section.
set -eu

if command -v apt-get >/dev/null 2>&1; then
    export DEBIAN_FRONTEND=noninteractive
    apt-get update
    apt-get install -y --no-install-recommends \
        build-essential ca-certificates curl file pkg-config git \
        libsane1 sane-utils usbutils \
        libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev \
        librsvg2-dev libssl-dev libxdo-dev
elif command -v pacman >/dev/null 2>&1; then
    pacman -Syu --noconfirm --needed \
        base-devel ca-certificates curl file pkgconf git \
        sane usbutils \
        webkit2gtk-4.1 gtk3 libayatana-appindicator librsvg openssl xdotool
else
    echo "linux-deps.sh: no supported package manager (apt-get or pacman)" >&2
    exit 1
fi
