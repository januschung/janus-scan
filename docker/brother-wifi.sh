#!/bin/sh
# Real-scanner test over Wi-Fi inside a Linux container, using Brother's own
# driver. Docker on a Mac can't pass USB through, but the container can reach
# the scanner's IP address.
#
# Run from the repo root (Brother's .deb is not in the repo; you accept its
# license when downloading it from support.brother.com):
#   docker run --rm --platform linux/amd64 \
#     -v "$PWD:/src:ro" -v /path/to/brscan4.deb:/drv/brscan4.deb:ro \
#     -v janus-scan-target-ubuntu-24-04:/target \
#     -v janus-scan-cargo-registry:/usr/local/cargo/registry \
#     -e CARGO_TARGET_DIR=/target -e SCANNER_IP=192.168.1.50 \
#     janus-scan-test:ubuntu-24-04 sh /src/docker/brother-wifi.sh [list|scan]
set -eu
: "${SCANNER_IP:?set SCANNER_IP to the scanner's address}"
MODE="${1:-list}"

step() { printf '\n==> %s\n' "$*"; }

step "install Brother driver"
dpkg -i /drv/brscan4.deb >/dev/null
brsaneconfig4 -a name=ADS1000W model=ADS-1000W ip="$SCANNER_IP"
brsaneconfig4 -q | grep -i "ADS1000W"

step "scanimage -L"
scanimage -L

step "build janus-scan"
mkdir -p /work
tar -C /src --exclude=./target --exclude=./.git --exclude=node_modules -cf - . | tar -C /work -xf -
cd /work
cargo build -q -p janus-scan-cli
BIN="$CARGO_TARGET_DIR/debug/janus-scan"

step "janus-scan list"
"$BIN" list

step "janus-scan caps"
"$BIN" caps -d ADS1000W

if [ "$MODE" = scan ]; then
    step "janus-scan scan (two-sided, 300 dpi, color)"
    mkdir -p /tmp/out
    "$BIN" scan -d ADS1000W --duplex --dpi 300 --out /tmp/out
    # Sizes only; the pages themselves stay in the container and are discarded.
    ls -l /tmp/out
fi
