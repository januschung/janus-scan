#!/bin/sh
# Builds the .deb inside the Ubuntu build image (see scripts/package-linux.sh).
# Expects the repo at /src (read-only), writes to /out, caches in /target.
set -eu
apt-get update -qq >/dev/null
DEBIAN_FRONTEND=noninteractive apt-get install -y -qq nodejs npm >/dev/null 2>&1

mkdir -p /work
tar -C /src --exclude=./target --exclude=./.git --exclude=node_modules --exclude=./deploy -cf - . | tar -C /work -xf -
export CARGO_TARGET_DIR=/target
rm -f /target/release/bundle/deb/*.deb

cd /work/app
# The web front end is built beforehand (app/build), outside the container.
npx --yes @tauri-apps/cli@2 build --bundles deb --config '{"build":{"beforeBuildCommand":""}}'

# Debian's usual name: package_version_arch.deb (Tauri's has spaces).
for f in /target/release/bundle/deb/*.deb; do
    name="$(dpkg-deb -f "$f" Package)_$(dpkg-deb -f "$f" Version)_$(dpkg-deb -f "$f" Architecture).deb"
    cp "$f" "/out/$name"
    echo "built $name"
done
