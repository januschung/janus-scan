#!/bin/sh
# Register the scanner with Brother's driver by its IP address (SCANNER_IP).
# Used at start-up by the Docker image and the systemd unit (LXC); replaces
# any earlier registration, since the address may have changed.
set -eu

if [ -z "${SCANNER_IP:-}" ]; then
    echo "janus-scan: SCANNER_IP is not set; the scanner can't be found over Wi-Fi" >&2
    exit 0
fi
if ! command -v brsaneconfig4 >/dev/null; then
    echo "janus-scan: Brother's driver (brscan4) isn't installed" >&2
    exit 0
fi

name="${SCANNER_NAME:-ADS1000W}"
brsaneconfig4 -r "$name" >/dev/null 2>&1 || true
brsaneconfig4 -a name="$name" model="${SCANNER_MODEL:-ADS-1000W}" ip="$SCANNER_IP"
# brsaneconfig4 exits 0 even when it can't write its config, and its -q
# listing needs Avahi, which isn't installed; check the file itself.
if grep -q "$SCANNER_IP" /etc/opt/brother/scanner/brscan4/brsanenetdevice4.cfg; then
    echo "janus-scan: registered $name at $SCANNER_IP"
else
    echo "janus-scan: couldn't register the scanner with Brother's driver" >&2
fi
