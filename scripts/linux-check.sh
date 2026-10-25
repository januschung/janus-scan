#!/bin/sh
# Checks a Linux machine for Janus Scan and writes a report to send back:
#   sh scripts/linux-check.sh          # environment, drivers, scanner list
#   sh scripts/linux-check.sh --scan   # also a timed two-sided scan
# The report holds versions, device names, timings and file sizes only. Scanned
# pages go to a temporary folder that is deleted at the end.
# Set DEVICE to pick a scanner by id or name fragment (default: "ADS").
set -u

cd "$(dirname "$0")/.."
REPORT="$PWD/linux-check-report.txt"
DEVICE="${DEVICE:-ADS}"
SCAN=0
[ "${1:-}" = "--scan" ] && SCAN=1

step() { printf '\n==> %s\n' "$*"; }
# Run a command, show its output, and keep going if it fails.
try() { "$@" 2>&1 || echo "(failed: exit $?)"; }

{
step "machine"
. /etc/os-release && echo "$PRETTY_NAME"
uname -m
[ "$(uname -m)" = x86_64 ] || echo "WARNING: Brother's driver only runs on x86_64."

step "tools"
for t in rustc cargo node npm scanimage brsaneconfig4; do
    printf '%-14s ' "$t"; command -v "$t" >/dev/null && "$t" --version 2>&1 | head -1 || echo "missing"
done
printf '%-14s ' webkit2gtk-4.1; pkg-config --modversion webkit2gtk-4.1 2>/dev/null || echo "missing"

step "Brother driver"
if command -v dpkg >/dev/null; then try dpkg -s brscan4 | grep -E '^(Status|Version)'; fi
if command -v pacman >/dev/null; then try pacman -Q brscan4; fi
try brsaneconfig4 -q | grep -iE 'ADS|Devices on network'

step "USB"
if command -v lsusb >/dev/null; then lsusb | grep -i '04f9:60a6' || echo "ADS-1000W not on USB"; else echo "lsusb missing (package usbutils)"; fi

step "scanimage -L"
try scanimage -L
echo "-- as root (if this lists a scanner the line above doesn't, it's a USB permission problem):"
if command -v sudo >/dev/null; then try sudo -n scanimage -L; else echo "(sudo not installed; skipped)"; fi

step "build janus-scan"
try cargo build -q -p janus-scan-cli
BIN="${CARGO_TARGET_DIR:-target}/debug/janus-scan"

step "janus-scan list"
try "$BIN" list

step "janus-scan caps -d $DEVICE"
try "$BIN" caps -d "$DEVICE"

if [ "$SCAN" = 1 ]; then
    step "janus-scan scan: two-sided, color, 300 dpi"
    OUT="$(mktemp -d)"
    try "$BIN" scan -d "$DEVICE" --duplex --dpi 300 --out "$OUT"
    ls -l "$OUT" | awk 'NR > 1 { print $5, $9 }'
    rm -rf "$OUT"
fi
} 2>&1 | tee "$REPORT"

echo
echo "Report saved to $REPORT"
