#!/usr/bin/env bash
# One-command release from a Mac:
#   dist/release.sh            # uses the version in DataShield-TUI/Cargo.toml
#
# 1. pushes main and the tag v<version> (GitHub Actions builds Linux and
#    creates the release)
# 2. builds the macOS bundle here, packs the dmg, uploads it
# 3. refreshes the Homebrew tap (../homebrew-tap) with the new checksums
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tap="${TAP_DIR:-$root/../homebrew-tap}"
cd "$root"

version="$(grep -m1 '^version' DataShield-TUI/Cargo.toml | sed 's/.*"\(.*\)"/\1/')"
tag="v$version"
echo "== releasing $tag"

[[ -z "$(git status --porcelain)" ]] || { echo "commit your changes first" >&2; exit 1; }
git push origin main
git rev-parse -q --verify "refs/tags/$tag" >/dev/null || git tag "$tag"
git push origin "$tag"

echo "== macOS bundle + dmg"
dist/macos/build-app.sh
stage="target/dmg.noindex"            # .noindex: Spotlight/Launchpad never see it
rm -rf "$stage" && mkdir -p "$stage"
ditto target/macos.noindex/DataShield.app "$stage/DataShield.app"
ln -s /Applications "$stage/Applications"
dmg="target/DataShield-TUI-macos-aarch64.dmg"
hdiutil create -volname DataShield -srcfolder "$stage" -ov -format UDZO "$dmg" -quiet
rm -rf "$stage"

echo "== waiting for the GitHub release (Linux job creates it)"
until gh release view "$tag" >/dev/null 2>&1; do sleep 20; done
gh release upload "$tag" "$dmg" --clobber

echo "== checksums"
sha_dmg="$(shasum -a 256 "$dmg" | cut -d' ' -f1)"
base="https://github.com/aeratic/DataShield/releases/download/$tag"
until curl -fsSL -o /dev/null "$base/datashield-tui-linux-x86_64.tar.gz" 2>/dev/null; do sleep 20; done
sha_x86="$(curl -fsSL "$base/datashield-tui-linux-x86_64.tar.gz"  | shasum -a 256 | cut -d' ' -f1)"
sha_arm="$(curl -fsSL "$base/datashield-tui-linux-aarch64.tar.gz" | shasum -a 256 | cut -d' ' -f1)"

echo "== homebrew tap"
[[ -d "$tap" ]] || { echo "tap not found at $tap (set TAP_DIR)" >&2; exit 1; }
python3 -I - "$tap" "$version" "$sha_dmg" "$sha_x86" "$sha_arm" <<'PY'
import re, sys
tap, ver, dmg, x86, arm = sys.argv[1:]
def bump(path, shas):
    s = open(path).read()
    s = re.sub(r'version "[^"]+"', f'version "{ver}"', s, count=1)
    found = re.findall(r'sha256 "([0-9a-f]{64})"', s)
    for old, new in zip(found, shas):
        s = s.replace(old, new, 1)
    open(path, 'w').write(s)
bump(f"{tap}/Casks/datashield.rb", [dmg])
bump(f"{tap}/Formula/datashield.rb", [x86, arm])
PY
git -C "$tap" commit -qam "datashield $version" && git -C "$tap" push -q origin main
echo "== done: https://github.com/aeratic/DataShield/releases/tag/$tag"
