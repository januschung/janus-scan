#!/bin/sh
# Install Janus Scan Server in a Debian 13 LXC container (or any Debian-like
# system with systemd). Run as root from the folder holding the dist files
# (see deploy/lxc/README.md), with Brother's driver next to them:
#
#   sh install.sh brscan4-0.4.11-1.amd64.deb
set -eu

DEB="${1:?usage: sh install.sh path/to/brscan4-*.amd64.deb}"
HERE="$(cd "$(dirname "$0")" && pwd)"
[ "$(id -u)" = 0 ] || { echo "Run as root." >&2; exit 1; }
[ "$(dpkg --print-architecture)" = amd64 ] || { echo "Brother's driver is x86-64 only." >&2; exit 1; }

step() { printf '\n==> %s\n' "$*"; }

step "packages"
# netbase: without it Brother's driver crashes opening a network scanner.
# libjpeg62-turbo: the driver decodes what the scanner sends over Wi-Fi.
apt-get update
DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends netbase libjpeg62-turbo tzdata

step "Brother's driver"
mkdir -p /etc/udev/rules.d   # the package installs a USB rule; harmless without udev
dpkg -i "$DEB"

step "Janus Scan Server"
# 1000:1000 when free, like the Docker image, so NAS permissions match.
if ! getent group janus-scan >/dev/null; then
    if getent group 1000 >/dev/null; then groupadd --system janus-scan; else groupadd --gid 1000 janus-scan; fi
fi
if ! id janus-scan >/dev/null 2>&1; then
    if getent passwd 1000 >/dev/null; then
        useradd --system --gid janus-scan --no-create-home --shell /usr/sbin/nologin janus-scan
    else
        useradd --uid 1000 --gid janus-scan --no-create-home --shell /usr/sbin/nologin janus-scan
    fi
fi
install -m 0755 "$HERE/bin/janus-scan-server" /usr/local/bin/janus-scan-server
install -D -m 0755 "$HERE/register-scanner.sh" /usr/local/lib/janus-scan/register-scanner
rm -rf /usr/share/janus-scan/web
mkdir -p /usr/share/janus-scan
cp -R "$HERE/web" /usr/share/janus-scan/web
install -m 0644 "$HERE/janus-scan.service" /etc/systemd/system/janus-scan.service

# Settings: kept if they already exist.
mkdir -p /etc/janus-scan
[ -e /etc/janus-scan/server.yaml ] || install -m 0644 "$HERE/server.yaml" /etc/janus-scan/server.yaml
[ -e /etc/default/janus-scan ] || cat > /etc/default/janus-scan <<'ENV'
# Janus Scan Server settings (restart after changes: systemctl restart janus-scan)
# The scanner's IP address; reserve it in the router.
SCANNER_IP=
PORT=8080
# Unsaved pages are deleted after this long without activity (0: never).
IDLE_TIMEOUT_MINUTES=60
# File names use the container's time zone: dpkg-reconfigure tzdata, or TZ=...
ENV

if [ -d /run/systemd/system ]; then
    systemctl daemon-reload
fi

cat <<'MSG'

Installed. Next:
  1. Set SCANNER_IP in /etc/default/janus-scan
  2. List the folders to save to in /etc/janus-scan/server.yaml
  3. systemctl enable --now janus-scan
  4. Open http://<this container's IP>:8080
MSG
