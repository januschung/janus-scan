#!/bin/sh
# Builds Janus Scan and installs it as an app with a menu/Launchpad icon.
#
#   sh scripts/install.sh               # build and install
#   sh scripts/install.sh --build-only  # build the installer, don't install it
#   sh scripts/install.sh --uninstall   # remove the installed app
#
# macOS: installs "Janus Scan.app" into /Applications.
# Ubuntu, Linux Mint, Linux Lite: builds and installs a .deb.
# CachyOS (and other pacman systems): builds an AppImage and adds a menu entry.
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TARGET="${CARGO_TARGET_DIR:-$ROOT/target}"
BUNDLE="$TARGET/release/bundle"
APP_NAME="Janus Scan"
MODE="${1:-install}"

say() { printf '\n==> %s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

# True when version $1 (e.g. 1.85.0) is at least $2.
at_least() {
    awk -v a="$1" -v b="$2" 'BEGIN {
        split(a, x, "."); split(b, y, ".")
        for (i = 1; i <= 3; i++) { if (x[i] + 0 > y[i] + 0) exit 0; if (x[i] + 0 < y[i] + 0) exit 1 }
        exit 0 }'
}

OS="$(uname -s)"
case "$OS" in
    Darwin) KIND=mac ;;
    Linux)
        if command -v apt-get >/dev/null 2>&1; then KIND=deb
        elif command -v pacman >/dev/null 2>&1; then KIND=appimage
        else die "unsupported Linux distribution (needs apt or pacman)"; fi ;;
    *) die "unsupported system: $OS" ;;
esac

APPIMAGE_DEST="$HOME/.local/bin/janus-scan.AppImage"
DESKTOP_FILE="$HOME/.local/share/applications/janus-scan.desktop"
ICON_DEST="$HOME/.local/share/icons/hicolor/128x128/apps/janus-scan.png"

uninstall() {
    case "$KIND" in
        mac) rm -rf "/Applications/$APP_NAME.app" "$HOME/Applications/$APP_NAME.app" ;;
        deb)
            pkg="$(dpkg-query -W -f='${Package}\n' 2>/dev/null | grep -i '^janus-scan' || true)"
            [ -n "$pkg" ] && sudo apt-get remove -y "$pkg" ;;
        appimage) rm -f "$APPIMAGE_DEST" "$DESKTOP_FILE" "$ICON_DEST" ;;
    esac
    echo "$APP_NAME removed."
}

if [ "$MODE" = --uninstall ]; then uninstall; exit 0; fi

# ---- Prerequisites -----------------------------------------------------------

say "checking prerequisites"
command -v cargo >/dev/null 2>&1 || die "Rust isn't installed. Install it with:
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  . \"\$HOME/.cargo/env\"
then run this script again."
rust="$(rustc --version | awk '{print $2}')"
at_least "$rust" 1.85.0 || die "Rust $rust is too old; 1.85 or newer is needed. Install Rust with rustup (not your distro's cargo package) and run 'rustup update'."
echo "Rust $rust"

command -v node >/dev/null 2>&1 || die "Node.js isn't installed. Install Node 22 with nvm (https://github.com/nvm-sh/nvm): nvm install 22"
node="$(node --version | tr -d v)"
at_least "$node" 20.19.0 || die "Node $node is too old; 20.19 or newer is needed. Install Node 22 with nvm: nvm install 22"
echo "Node $node"

if [ "$KIND" != mac ]; then
    if ! pkg-config --exists gtk+-3.0 webkit2gtk-4.1 2>/dev/null || ! command -v scanimage >/dev/null 2>&1; then
        say "installing system packages (needs your password)"
        sudo sh "$ROOT/scripts/linux-deps.sh"
    fi
    echo "GTK $(pkg-config --modversion gtk+-3.0), WebKitGTK $(pkg-config --modversion webkit2gtk-4.1)"
fi

# Brother's driver is separate and only needed to scan, so missing it is a warning.
case "$KIND" in
    mac) [ -d "/Library/Image Capture/Devices/Brother Scanner.app" ] ||
        warn "Brother's scanner driver (ICA) isn't installed; the app won't find the scanner. Get it from support.brother.com." ;;
    *) command -v brsaneconfig4 >/dev/null 2>&1 ||
        warn "Brother's brscan4 driver isn't installed; the app won't find the scanner. Get it from support.brother.com." ;;
esac

# ---- Build -------------------------------------------------------------------

say "installing front-end packages"
cd "$ROOT/app"
npm ci --no-audit --no-fund

case "$KIND" in
    mac) bundles=app ;;
    deb) bundles=deb ;;
    appimage) bundles=appimage ;;
esac
say "building $APP_NAME ($bundles); the first build takes a while"
npm run tauri build -- --bundles "$bundles"

case "$KIND" in
    mac) built="$BUNDLE/macos/$APP_NAME.app" ;;
    deb) built="$(ls -t "$BUNDLE"/deb/*.deb | head -1)" ;;
    appimage) built="$(ls -t "$BUNDLE"/appimage/*.AppImage | head -1)" ;;
esac
[ -e "$built" ] || die "build finished but no installer was found in $BUNDLE"
echo "Built: $built"

if [ "$MODE" = --build-only ]; then exit 0; fi

# ---- Install -----------------------------------------------------------------

say "installing"
case "$KIND" in
    mac)
        dest=/Applications
        [ -w "$dest" ] || { dest="$HOME/Applications"; mkdir -p "$dest"; }
        rm -rf "$dest/$APP_NAME.app"
        ditto "$built" "$dest/$APP_NAME.app"
        echo "Installed in $dest. Open it from Launchpad or Spotlight."
        ;;
    deb)
        # apt reads the file as its own user, which can't see into your home folder.
        cp "$built" /tmp/janus-scan.deb
        sudo apt-get install -y /tmp/janus-scan.deb
        rm -f /tmp/janus-scan.deb
        echo "Installed. Find $APP_NAME in your app menu under Graphics."
        ;;
    appimage)
        mkdir -p "$(dirname "$APPIMAGE_DEST")" "$(dirname "$DESKTOP_FILE")" "$(dirname "$ICON_DEST")"
        cp "$built" "$APPIMAGE_DEST"
        chmod +x "$APPIMAGE_DEST"
        cp "$ROOT/app/src-tauri/icons/128x128.png" "$ICON_DEST"
        cat > "$DESKTOP_FILE" <<EOF
[Desktop Entry]
Type=Application
Name=$APP_NAME
Comment=Scan documents with the Brother ADS-1000W
Exec=$APPIMAGE_DEST
Icon=janus-scan
Categories=Graphics;Scanning;
Terminal=false
EOF
        echo "Installed. Find $APP_NAME in your app menu under Graphics."
        ;;
esac
