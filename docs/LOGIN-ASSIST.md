# Login assist — DataShield and the browser extension

DataShield can fill a login form in your everyday browser with the
credentials of an item in the open vault. It does so through a small browser
extension (`extension/`) that talks only to `datashield-tui` on
`127.0.0.1`. Nothing leaves the machine; nothing is filled, sent or stored
without a switch you turned on and a choice you made.

## Setup, once

1. Settings (`,` or `:settings`) → section `BROWSER`:
   - **login assist** → on. Saving mints a persistent *pairing token*.
   - **browser** shows the browser your desktop opens links in, and its
     family (Firefox or Chrome).
   - **install extension**: `←/→` opens that browser on the extension's
     store page — once the extension is published and its URL is set in
     `ui.toml` (`[extension] chrome_url` / `firefox_url`). Until then the row
     says so; see `extension/README.md` for loading it by hand.
   - **extension token** shows the first characters of the pairing token.
2. `:logintoken` copies the whole token to the clipboard (wiped after 20 s,
   like any secret). Paste it in the extension's popup, **save**, **test**.
   `:logintoken new` retires the token and mints another — the extension must
   be paired again.

## Use

- **From the browser.** With a vault open in DataShield, every login page
  shows a brass `◆` beside the password field. Click it (or press
  `Ctrl+Shift+L` in the field): the vault's logins for that site are listed;
  pick one and username + password are filled. Nothing is submitted for you.
- **From DataShield.** On an item with a website, a username and a password,
  press `l` (`login` / `accedi`): the site opens in the browser and the first
  login form that appears — on that page or after a redirect, same tab — is
  filled without a click. The offer is valid once, for two minutes, only on
  that item's own site.

The item's site is its `website`/`url` field; a page matches when its host is
that domain or a subdomain of it (`accounts.google.com` is `google.com`'s). A
`www.` on the stored site is ignored, and a short fixed table maps services
that sign in on their owner's domain (Gmail → `google.com`, Outlook/Hotmail →
`live.com` / `microsoftonline.com`, iCloud → `apple.com`, Instagram →
`facebook.com`, X ↔ Twitter).

Two-step logins (Google, Microsoft…) work: the `◆` appears beside the email
field when the vault has a login for the site; the password step that follows
is filled by itself, and so is a one-time-code field when the item carries a
TOTP seed. 2FA that is not TOTP stays yours.

With **no vault open**, clicking `◆` makes DataShield say so in its status
line, send a desktop notification and come forward (in its own kitty window,
or on Hyprland); the menu then appears by itself once you unlock.

A login you type by hand and submit is offered on the next page: "keep this
login in DataShield?" — only on your *save* it becomes a `login` item named
after the host (or updates the password of the item that already logs in
there with that username). The extension speaks DataShield's own interface
language (EN/IT).

## What crosses, and when

| endpoint | who calls it | carries | when |
|---|---|---|---|
| `POST /login/match` | the extension, on every login page it sees | item id, label, username, domain — never a password | login assist on |
| `POST /login/fill` | the extension, when you pick an entry | that item's username + password | login assist on, your click |
| `POST /login/claim` | the extension, after an `l` key press | that item's username + password, once | login assist on, within 2 min, from the item's site |
| `POST /login/info` | the extension, before it draws anything | the interface language, whether a vault is open | login assist on |
| `POST /login/wake` | the extension, when you click `◆` with no vault open | the page's host | login assist on — DataShield says so, notifies, comes forward |
| `POST /login/totp` | the extension, on a one-time-code field after a fill | the code of the moment — never the seed | login assist on, same site rule as `fill` |
| `POST /login/save` | the extension, when you answer "save" to a login you typed by hand | that login, queued for the app's tick | login assist on, a vault open (`409` otherwise) |

All seven answer `404` while login assist is off — indistinguishable from an
endpoint that does not exist. They accept the pairing token, the admin token
and an agent session's token (scoped to that session's vault). The pairing
token opens **only** these doors: not `/search`, not `/reveal`, not
`/unlock`.

The pairing token lives in `settings.toml` (owner-only file) and in the
extension's private storage. It is read through the switch: with login assist
off, a token in the file opens nothing.

## What stays local

- The vault's items are offered only while the vault is open in DataShield
  (or opened through `/unlock`, if that is on). Lock or close it and the
  extension sees nothing.
- The extension never sees the pairing token on a page: content scripts ask
  the background script, which uses the tab's real origin — a page cannot
  claim to be another site.
- Credentials are passed to the page's fields and kept nowhere in the
  extension. The server wipes its response buffers; the TUI keeps using
  `SecretString`/`Zeroizing` as everywhere else.
