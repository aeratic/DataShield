# DataShield — browser extension

Fills login forms with the credentials kept in your DataShield vault. One
code base, two packages: Chromium browsers (Chrome, Brave, Edge, Vivaldi,
Opera) and Gecko browsers (Firefox, Zen, LibreWolf). Everything happens on
this machine: the extension only ever talks to `datashield-tui` on
`127.0.0.1`.

## How it works

1. In DataShield, settings → `BROWSER` → **login assist** on. Saving mints a
   pairing token; the `install extension` row opens your browser on the
   store page once the extension is published.
2. Run `logintoken` in DataShield (the `:` palette): the token lands on your
   clipboard for 20 s. Paste it in the extension's popup, **save**, **test**.
3. Open a vault in DataShield. On any login page, click the brass `◆` beside
   the password field (or press `Ctrl+Shift+L`): the vault's logins for that
   site are listed — pick one and username + password are filled. The form is
   never submitted for you; 2FA is yours.
4. From the TUI, `l` on an item opens its site in the browser and the first
   login form that appears is filled without a click (valid once, for two
   minutes, only on that item's own site).

What leaves DataShield, and when: `/login/match` (which items exist for this
site: id, label, username — never a password) on every login page the
extension sees; `/login/fill` (one item's username + password) only when you
pick it; `/login/claim` only for an `l` key press. All of it is off until you
switch login assist on.

## Try it without publishing

Build both packages:

```sh
./build.sh
```

- **Firefox / Zen / LibreWolf**: `about:debugging` → *This Firefox* → *Load
  Temporary Add-on…* → pick `dist/firefox/manifest.json`. (Temporary add-ons
  go away when the browser quits; the signed `.xpi` from AMO does not.)
- **Chrome / Brave / Edge / Vivaldi**: `chrome://extensions` → enable
  *Developer mode* → *Load unpacked* → pick the `dist/chrome/` folder.

## Publishing

- **Chrome Web Store**: a one-time 5 USD developer registration, then upload
  `dist/datashield-chrome.zip`. Put the listing URL in DataShield's `ui.toml`
  under `[extension] chrome_url` so the settings row can open it.
- **Firefox Add-ons (AMO)**: free. Upload `dist/datashield-firefox.zip`;
  Mozilla signs it. Put the listing URL under `[extension] firefox_url`.
  Change the `gecko.id` in `manifest.firefox.json` to an id you own before
  the first upload — it is permanent.
- **Edge Add-ons**: free, takes the Chrome package.
- Safari is out of scope (Apple Developer Program + a different package).

## Files

| file | role |
|---|---|
| `background.js` | the one script that talks to the API; keeps the token, the port and the `l`-key nonces |
| `content.js` | finds login forms (two-step ones too), draws the `◆` mark and the menu, fills the fields, the TOTP code, and offers to keep a login typed by hand |
| `i18n.js` | the extension's words in DataShield's interface language (EN/IT) |
| `popup.html/js` | pairing: token, port, connection test |
| `manifest.chrome.json` / `manifest.firefox.json` | the two store manifests (MV3) |
| `build.sh` | assembles `dist/chrome`, `dist/firefox` and the two zips |
