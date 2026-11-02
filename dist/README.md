# DataShield — standalone-terminal distribution

DataShield ships **with its own terminal**. The `datashield` launcher opens
`datashield-tui` inside a dedicated kitty window driven by
`datashield.kitty.conf`, so the app fully owns its typography, paper colour,
padding, cursor and graphics — independent of (and never touching) the user's
everyday terminal. To the desktop it looks and behaves like a regular GUI app
(`--class DataShield`, its own `.desktop` entry), while staying a pure TUI.

Why kitty: DataShield already targets the kitty graphics protocol for crisp
favicons, and kitty supports per-instance configs (`--config`), remote-control
opacity (the in-app `blur` setting) and font control.

## Install

```sh
# 1. the app itself
cargo install --path .

# 2. the dedicated terminal (package dependency)
#    Arch: pacman -S kitty · Debian/Ubuntu: apt install kitty

# 3. launcher + terminal profile + desktop entry
install -Dm755 dist/datashield            ~/.local/bin/datashield
install -Dm644 dist/datashield.kitty.conf ~/.config/datashield/datashield.kitty.conf
install -Dm644 dist/datashield.desktop    ~/.local/share/applications/datashield.desktop
```

Run `datashield` (or launch it from the app menu). Without kitty installed the
launcher falls back to the current terminal (favicons become monograms).

## The "border" around an opaque background

A TUI can only paint character cells; the window **padding** around the grid is
painted by the terminal. In a foreign terminal (e.g. ghostty) that padding
shows the terminal's own background — the border you saw. In DataShield's
dedicated window the padding colour IS the BRASS LEDGER paper (`#0F0D0A`), so
there is nothing foreign to see. If you still want to run it inside ghostty,
add to your ghostty config:

```
window-padding-color = extend
```

which extends the cells' background colour into the padding.

## macOS: a self-contained `DataShield.app`

On macOS the dedicated terminal is **bundled**: `dist/macos/build-app.sh`
copies kitty.app, re-badges it as DataShield (name, icon, bundle id, Dock
entry) and puts `datashield-tui`, the terminal profile and the icon inside
`Contents/Resources`. The result needs nothing on the user's machine — no
kitty, no cargo, no `~/.config` — and never touches their own kitty (separate
bundle id and instance group). Cmd+N opens another DataShield window.

```sh
cargo build --release
dist/macos/build-app.sh            # → target/macos.noindex/DataShield.app
cp -R target/macos.noindex/DataShield.app /Applications/
```

The build machine needs kitty.app (`brew install --cask kitty`, or set
`KITTY_APP=/path/to/kitty.app`) and the Xcode command-line tools (for the
tiny C launcher in `dist/macos/launcher.c`). The bundle is ad-hoc signed; for
distribution outside your own Macs, sign and notarize with a Developer ID.
