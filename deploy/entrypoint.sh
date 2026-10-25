#!/bin/sh
# Container start-up: register the scanner, then run the server.
set -eu

if [ ! -e "${JANUS_SCAN_LIBSANE:-/usr/lib64/sane/libsane-brother4.so.1}" ]; then
    echo "janus-scan: Brother's scanner driver isn't in this image. Build it with" >&2
    echo "  driver.Dockerfile (docker-compose.yml does this), which adds the brscan4" >&2
    echo "  .deb you download from support.brother.com." >&2
    exit 1
fi

janus-scan-register

# No config file mounted: download only.
if [ ! -e "$JANUS_CONFIG" ]; then
    unset JANUS_CONFIG
fi

exec janus-scan-server "$@"
