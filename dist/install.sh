#!/bin/sh
# DataShield installer — https://datashield.aeratic.com/install
#
#   curl -fsSL https://datashield.aeratic.com/install | sh -s -- tui
#
# Picks the right package for this machine from the latest GitHub release and
# installs it (DATASHIELD_DEST overrides the destination). `tui` (default) installs DataShield TUI; `gui` will install
# DataShield GUI once it ships. Set DATASHIELD_VERSION=v0.2.0 to pin a release.
set -eu

REPO="${DATASHIELD_REPO:-aeratic/DataShield}"
APP="${1:-tui}"
VERSION="${DATASHIELD_VERSION:-latest}"

say()  { printf '%s\n' "$*" >&2; }
fail() { say "datashield: $*"; exit 1; }

case "$APP" in
    tui) ;;
    gui) fail "DataShield GUI is not released yet — install the TUI with: ... | sh -s -- tui" ;;
    *)   fail "unknown app '$APP' (expected: tui)" ;;
esac

os="$(uname -s)"; arch="$(uname -m)"
case "$arch" in
    x86_64|amd64)  arch=x86_64 ;;
    arm64|aarch64) arch=aarch64 ;;
    *) fail "unsupported architecture: $arch" ;;
esac

if [ "$VERSION" = latest ]; then
    base="https://github.com/$REPO/releases/latest/download"
else
    base="https://github.com/$REPO/releases/download/$VERSION"
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

case "$os" in
Darwin)
    asset="DataShield-TUI-macos-$arch.dmg"
    say "downloading ${asset}..."
    curl -fsSL -o "$tmp/$asset" "$base/$asset" || fail "download failed: $base/$asset"
    dest="${DATASHIELD_DEST:-/Applications}"; [ -w "$dest" ] || dest="$HOME/Applications"
    mkdir -p "$dest"
    mnt="$(hdiutil attach -nobrowse -readonly "$tmp/$asset" | awk -F'\t' '/\/Volumes\//{print $NF}')"
    rm -rf "$dest/DataShield.app"
    cp -R "$mnt/DataShield.app" "$dest/DataShield.app"
    hdiutil detach "$mnt" -quiet
    # the download came through curl, not a browser, so there is no quarantine
    # flag to clear; Gatekeeper may still ask once if the build is unsigned.
    say "installed $dest/DataShield.app"
    say "open it from Launchpad, or: open \"$dest/DataShield.app\""
    ;;
Linux)
    asset="datashield-tui-linux-$arch.tar.gz"
    say "downloading ${asset}..."
    curl -fsSL -o "$tmp/$asset" "$base/$asset" || fail "download failed: $base/$asset"
    tar -xzf "$tmp/$asset" -C "$tmp"
    bindir="${DATASHIELD_DEST:-$HOME/.local/bin}"; mkdir -p "$bindir"
    install -m755 "$tmp/datashield-tui" "$bindir/datashield-tui"
    install -m755 "$tmp/datashield"     "$bindir/datashield"
    mkdir -p "$HOME/.config/datashield" "$HOME/.local/share/applications" "$HOME/.local/share/icons/hicolor/128x128/apps"
    install -m644 "$tmp/datashield.kitty.conf" "$HOME/.config/datashield/datashield.kitty.conf"
    install -m644 "$tmp/datashield.desktop"    "$HOME/.local/share/applications/datashield.desktop"
    [ -f "$tmp/datashield.png" ] && install -m644 "$tmp/datashield.png" "$HOME/.local/share/icons/hicolor/128x128/apps/datashield.png"
    say "installed to $bindir"
    command -v kitty >/dev/null || say "note: install kitty for DataShield's own window (apt/dnf/pacman install kitty); without it DataShield runs in your current terminal"
    case ":$PATH:" in *":$bindir:"*) ;; *) say "note: add $bindir to your PATH" ;; esac
    say "run: datashield"
    ;;
*)
    fail "unsupported OS: $os (Windows: coming soon)"
    ;;
esac
