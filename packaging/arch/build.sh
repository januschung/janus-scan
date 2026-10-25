#!/bin/sh
# Builds the Arch package (CachyOS) from the .deb, inside an archlinux
# container. Used by scripts/package-linux.sh and the release workflow:
#
#   docker run --rm --platform linux/amd64 -v <packages dir>:/out \
#     -v "$PWD/packaging/arch":/pkg:ro -v "$PWD/LICENSE":/pkg-license/LICENSE:ro \
#     archlinux:latest sh /pkg/build.sh
#
# /out must hold janus-scan_<version>_amd64.deb; the package is written there.
set -eu
# pacman's download sandbox needs kernel features that x86-64 emulation on
# Apple Silicon lacks; it isn't needed in a throwaway build container.
sed -i '/^\[options\]/a DisableSandbox' /etc/pacman.conf
pacman -Sy --noconfirm --needed base-devel >/dev/null
useradd -m builder
mkdir /build
cp /pkg/PKGBUILD /pkg-license/LICENSE /build/
deb=$(ls /out/janus-scan_*_amd64.deb | tail -n1)
cp "$deb" /build/
chown -R builder /build
version=$(basename "$deb" | sed 's/^janus-scan_\(.*\)_amd64\.deb$/\1/')
su builder -c "cd /build && JANUS_VERSION=$version makepkg --nodeps --noconfirm"
cp /build/janus-scan-*.pkg.tar.zst /out/
