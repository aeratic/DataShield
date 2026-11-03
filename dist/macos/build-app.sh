#!/usr/bin/env bash
# Builds a self-contained DataShield.app for macOS.
#
# The bundle IS a private copy of kitty (the dedicated terminal DataShield
# ships with), re-badged as DataShield: own name, icon, bundle id, Dock entry
# and Cmd+N behaviour. Nothing in it depends on the user's kitty, cargo or
# config — the TUI binary, the terminal profile and the icon all live inside.
#
# usage: dist/macos/build-app.sh [output-dir]   (default: target/macos.noindex — Spotlight/Launchpad skip .noindex dirs)
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"   # the DataShield workspace
out="${1:-$root/target/macos.noindex}"
app="$out/DataShield.app"

# ---- inputs ------------------------------------------------------------
kitty_app="${KITTY_APP:-/Applications/kitty.app}"
[[ -d "$kitty_app" ]] || { echo "kitty.app not found at $kitty_app (set KITTY_APP=...)" >&2; exit 1; }
bin="$root/target/release/datashield-tui"
if [[ ! -x "$bin" ]]; then
    echo "building release binary…" >&2
    (cd "$root" && cargo build --release -p datashield-tui)
fi
version="$(grep -m1 '^version' "$root/DataShield-TUI/Cargo.toml" | sed 's/.*"\(.*\)"/\1/')"

# ---- assemble ----------------------------------------------------------
rm -rf "$app"
mkdir -p "$out"
cp -R "$kitty_app" "$app"
res="$app/Contents/Resources"
macos="$app/Contents/MacOS"

cp "$bin"                              "$res/datashield-tui"
cp "$root/dist/datashield.kitty.conf"  "$res/datashield.kitty.conf"
cp "$here/DataShield.icns"             "$res/DataShield.icns"
# GPLv3 notice for the bundled kitty (required when redistributing it)
cp "$here/LICENSE-kitty.txt" "$here/THIRD-PARTY.txt" "$res/"
# kitty's asset catalog (CFBundleIconName) would win over the .icns: drop both
rm -f "$res/kitty.icns" "$res/Assets.car"

# macOS-only knobs appended to the shared terminal profile
cat >> "$res/datashield.kitty.conf" <<'CONF'

# ---- macOS bundle ------------------------------------------------------
macos_quit_when_last_window_closed yes
macos_show_window_title_in none
CONF

# the executable Finder/Dock run: a compiled launcher (macOS refuses a script
# as the main executable of a signed bundle) that starts the bundled kitty with
# DataShield's profile and binary. `shell` is overridden so Cmd+N / "New OS
# Window" open another DataShield, never a plain shell.
clang -O2 -Wall -o "$macos/DataShield" "$here/launcher.c"

# ---- identity ----------------------------------------------------------
plist="$app/Contents/Info.plist"
pb=/usr/libexec/PlistBuddy
$pb -c "Set :CFBundleName DataShield"                      "$plist"
$pb -c "Set :CFBundleDisplayName DataShield"               "$plist"
$pb -c "Set :CFBundleIdentifier dev.brunogalimi.datashield" "$plist"
$pb -c "Set :CFBundleExecutable DataShield"                "$plist"
$pb -c "Set :CFBundleIconFile DataShield"                  "$plist"
$pb -c "Delete :CFBundleIconName"       "$plist" 2>/dev/null || true
$pb -c "Set :CFBundleShortVersionString $version"          "$plist" 2>/dev/null || $pb -c "Add :CFBundleShortVersionString string $version" "$plist"
$pb -c "Set :CFBundleVersion $version"                     "$plist" 2>/dev/null || $pb -c "Add :CFBundleVersion string $version" "$plist"
# kitty registers itself as a handler for shell/ssh URLs and as a terminal; DataShield is neither
# read from the SOURCE bundle's plist: the copy inside $app is mid-surgery and
# must not be executed here (a failing substitution would abort the script
# under `set -e` before the signature below)
kitty_version="$($pb -c "Print :CFBundleShortVersionString" "$kitty_app/Contents/Info.plist" 2>/dev/null || true)"
$pb -c "Add :KittyVersion string ${kitty_version:-unknown}" "$plist" 2>/dev/null || true
$pb -c "Delete :CFBundleURLTypes"      "$plist" 2>/dev/null || true
$pb -c "Delete :CFBundleDocumentTypes" "$plist" 2>/dev/null || true
$pb -c "Delete :NSServices"            "$plist" 2>/dev/null || true
$pb -c "Delete :LSArchitecturePriority" "$plist" 2>/dev/null || true

# the Dock menu's "New OS Window" is a kitty string compiled into its extension
fdt="$res/Python/lib/kitty-extensions/kitty.fast_data_types.so"
python3 -I "$here/patch-strings.py" "$fdt"
# --deep below does not reach libraries under Resources/Python: re-sign the
# patched one explicitly or the kernel kills kitty (Code Signature Invalid)
codesign --force --sign - "$fdt" >/dev/null 2>&1

# ad-hoc signature so Gatekeeper and the Dock accept the modified bundle
codesign --force --deep --sign - "$app" >/dev/null 2>&1 || { echo "error: codesign failed" >&2; exit 1; }
codesign --verify --deep "$app" || { echo "error: bundle signature does not verify" >&2; exit 1; }

echo "built $app"
