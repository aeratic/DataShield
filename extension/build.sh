#!/usr/bin/env bash
# Builds the two store packages of the DataShield browser extension:
#   dist/chrome/   — Chrome, Brave, Edge, Vivaldi, Opera (manifest.chrome.json)
#   dist/firefox/  — Firefox, Zen, LibreWolf (manifest.firefox.json)
# plus a .zip of each, ready to upload. Run from anywhere.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$here"
common=(background.js content.js i18n.js popup.html popup.js icons)
for target in chrome firefox; do
    out="dist/$target"
    rm -rf "$out"
    mkdir -p "$out"
    cp -r "${common[@]}" "$out/"
    cp "manifest.$target.json" "$out/manifest.json"
    (cd "$out" && rm -f "../datashield-$target.zip" && zip -qr "../datashield-$target.zip" .)
    echo "built $out (dist/datashield-$target.zip)"
done
