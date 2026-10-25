#!/bin/sh
# Installs Brother's brscan4 driver into the Janus Scan Server image. Runs as
# root while building on top of the published image (see driver.Dockerfile).
set -eu
deb="${1:?usage: janus-scan-install-driver brscan4-<version>.amd64.deb}"

# The package also installs a USB udev rule; harmless without udev.
mkdir -p /etc/udev/rules.d
dpkg -i "$deb"

# Registering the scanner at start-up runs as the unprivileged user. The
# files under /etc/opt are symlinks into /opt; only the network device list
# needs to be writable.
cfg=/opt/brother/scanner/brscan4
touch "$cfg/brsanenetdevice4.cfg"
chmod a+rwx "$cfg"
chmod a+rw "$cfg/brsanenetdevice4.cfg"
echo "janus-scan: Brother's driver installed"
