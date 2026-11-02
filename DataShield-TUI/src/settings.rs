//! User preferences, persisted to `settings.toml` next to the vaults.
//!
//! These are cosmetic/UX choices (accent theme, whether we paint an opaque
//! backdrop, the interface language, an optional kitty translucency nudge) —
//! never secrets. Loading is best-effort: a missing or malformed file falls
//! back to the defaults so the app always starts.

use crate::i18n::Lang;
use crate::theme::Theme;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// Whether DataShield paints its own solid backdrop. `Transparent` (the
/// default) draws nothing behind the UI, so whatever the terminal shows
/// through — including its own blur/transparency — remains visible. This is
/// the cross-platform knob that always works.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Background {
    #[default]
    Transparent,
    Opaque,
}

/// Best-effort kitty window translucency. `On` asks kitty (via remote control)
/// to make its window translucent; the actual blur radius still comes from the
/// user's `kitty.conf`. On other terminals this is a no-op with a note.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Blur {
    #[default]
    Off,
    On,
}

/// The faint digit-rain drifting behind every screen. `Auto` shows it only
/// inside DataShield's own dedicated terminal (the `datashield` launcher sets
/// `DATASHIELD_OWN_TERM`), where its look is fully ours to control.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backdrop {
    #[default]
    Auto,
    On,
    Off,
}

/// Which animated wallpaper drifts behind the screens (when [`Backdrop`] shows
/// one): the brass digit-rain, or the crimson pixel-waves (the "Datashield
/// Background" shader — pixelated, domain-warped fbm on deep navy).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BackdropFx {
    #[default]
    Rain,
    Waves,
}

/// Idle auto-lock: after this long without a key press the open vault closes
/// (its key is zeroized) and the full-screen lock takes over.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AutoLock {
    Off,
    #[serde(rename = "1m")]
    OneMin,
    #[default]
    #[serde(rename = "5m")]
    FiveMin,
    #[serde(rename = "15m")]
    FifteenMin,
}

impl AutoLock {
    /// The limit in ticks (the app ticks every ~120 ms).
    pub fn ticks(self) -> Option<u64> {
        match self {
            AutoLock::Off => None,
            AutoLock::OneMin => Some(60 * 1000 / 120),
            AutoLock::FiveMin => Some(5 * 60 * 1000 / 120),
            AutoLock::FifteenMin => Some(15 * 60 * 1000 / 120),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AutoLock::Off => "off",
            AutoLock::OneMin => "1 min",
            AutoLock::FiveMin => "5 min",
            AutoLock::FifteenMin => "15 min",
        }
    }
}

/// How strongly the wallpaper effect shows over the base — a user-facing
/// "intensity slider" (discrete steps, rendered as a `▰▰▱▱▱` bar). Lower =
/// subtler; scales the colour the effect adds above the background.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BackdropLevel {
    Faint,
    #[default]
    Soft,
    Medium,
    Strong,
    Full,
}

impl BackdropLevel {
    /// Multiplier applied to the effect's colour deviation from the base.
    pub fn factor(self) -> f32 {
        match self {
            BackdropLevel::Faint => 0.20,
            BackdropLevel::Soft => 0.38,
            BackdropLevel::Medium => 0.58,
            BackdropLevel::Strong => 0.80,
            BackdropLevel::Full => 1.0,
        }
    }

    /// A 5-cell slider bar for the settings row (█ filled · ░ empty — glyphs
    /// present in effectively every monospace font, so it never renders tofu).
    pub fn bar(self) -> &'static str {
        match self {
            BackdropLevel::Faint => "\u{2588}\u{2591}\u{2591}\u{2591}\u{2591}",
            BackdropLevel::Soft => "\u{2588}\u{2588}\u{2591}\u{2591}\u{2591}",
            BackdropLevel::Medium => "\u{2588}\u{2588}\u{2588}\u{2591}\u{2591}",
            BackdropLevel::Strong => "\u{2588}\u{2588}\u{2588}\u{2588}\u{2591}",
            BackdropLevel::Full => "\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}",
        }
    }

    /// The ordered steps, low → high (for cycling the slider).
    pub const STEPS: [BackdropLevel; 5] = [
        BackdropLevel::Faint,
        BackdropLevel::Soft,
        BackdropLevel::Medium,
        BackdropLevel::Strong,
        BackdropLevel::Full,
    ];
}

/// Idle expiry of an API vault session (see `api.rs`): a token nobody has used
/// for this long stops being a token. Sliding — every authorized request stamps
/// the session — so a working agent is never cut off mid-task, and a forgotten
/// one does not stay a key to the vault for the rest of the day.
///
/// `Off` is deliberately spellable: an agent that runs for hours needs it, and
/// the honest answer to "how long?" is then `null` in the `/unlock` body rather
/// than a number the server does not mean.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApiSessionTtl {
    #[serde(rename = "5m")]
    FiveMin,
    #[default]
    #[serde(rename = "15m")]
    FifteenMin,
    #[serde(rename = "1h")]
    OneHour,
    Off,
}

impl ApiSessionTtl {
    /// The cycle order of the settings row.
    pub const ALL: [ApiSessionTtl; 4] = [
        ApiSessionTtl::FiveMin,
        ApiSessionTtl::FifteenMin,
        ApiSessionTtl::OneHour,
        ApiSessionTtl::Off,
    ];

    /// The idle limit in seconds — `None` = never expires.
    pub fn secs(self) -> Option<u64> {
        match self {
            ApiSessionTtl::FiveMin => Some(5 * 60),
            ApiSessionTtl::FifteenMin => Some(15 * 60),
            ApiSessionTtl::OneHour => Some(60 * 60),
            ApiSessionTtl::Off => None,
        }
    }

    /// The row's value column. Untranslated on the [`AutoLock::label`]
    /// precedent: these are durations, and "5 min" reads the same in both.
    pub fn label(self) -> &'static str {
        match self {
            ApiSessionTtl::FiveMin => "5 min",
            ApiSessionTtl::FifteenMin => "15 min",
            ApiSessionTtl::OneHour => "1 h",
            ApiSessionTtl::Off => "off",
        }
    }
}

/// The ports the settings page cycles through for the localhost agent API.
/// Deliberately a short list of high, unassigned ports: the row is a ←/→ chooser
/// like every other one, and anybody who needs a different number edits
/// `settings.toml` by hand (any `u16` is loaded and used verbatim).
pub const API_PORTS: [u16; 4] = [7737, 7738, 7739, 17737];

/// The default of [`Settings::api_port`].
pub const API_PORT_DEFAULT: u16 = API_PORTS[0];

/// The persisted preferences. Serialised as a small, human-editable TOML file.
///
/// `#[serde(default)]` means any field missing from the file falls back to the
/// value in [`Settings::default`], so older files (and hand-edited ones) keep
/// working as new preferences are added.
/// (Not `Copy`: [`Settings::shortcut_overrides`] owns a map. The draft the
/// settings page edits is a `clone()` of the saved one — same shape as before,
/// one word longer.)
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    // General.
    pub lang: Lang,
    // TUI (appearance).
    pub theme: Theme,
    pub background: Background,
    pub blur: Blur,
    /// The faint digit-rain behind every screen (auto = own terminal only).
    pub backdrop: Backdrop,
    /// Which wallpaper effect (rain | waves).
    pub backdrop_fx: BackdropFx,
    /// How visible the wallpaper effect is (the intensity slider).
    pub backdrop_level: BackdropLevel,
    /// Idle auto-lock for an open vault.
    pub autolock: AutoLock,
    /// Show the ASCII logo header (edit its art in `logo.txt`).
    pub show_logo: bool,
    /// Fetch each item's website favicon (a network request to that site) to
    /// show its real logo in the list. Off = a local colored monogram only.
    pub favicons: bool,
    // Accessibility / interaction toggles (see the `settings` page).
    /// Blinking cursor and small animations (off = steady, calmer).
    pub reduce_motion: bool,
    /// Ask for confirmation before quitting from the top level.
    pub confirm_exit: bool,
    // ---- RAG + the localhost agent API (see `rag.rs` / `api.rs`) ----------
    /// Index the Memorandum for semantic search. Off out of the box: turning it
    /// on is what asks for the ~490 MB one-time model download.
    pub rag_memo: bool,
    /// Index the ITEMS — labels, categories, field names and NON-secret values.
    /// A secret value is never embedded (an embedding partially inverts).
    pub rag_items: bool,
    /// Serve the memorandum index on `127.0.0.1:<api_port>`. Meaningless — and
    /// refused by the settings page — while [`Settings::rag_memo`] is off.
    pub api_expose_memo: bool,
    /// The same for the item index, gated on [`Settings::rag_items`].
    pub api_expose_items: bool,
    /// The port the agent API binds, on the LOOPBACK interface only. The page
    /// cycles [`API_PORTS`]; the file takes any number.
    pub api_port: u16,
    /// Allow `POST /unlock`: an agent that KNOWS a vault's master password may
    /// open that vault THROUGH the API and read it with a per-vault token, with
    /// nothing open in the TUI. Off out of the box, and the row says out loud
    /// what turning it on means — any local process holding the admin token can
    /// then try passwords against this socket (the KDF is the only rate limit).
    ///
    /// It is also the PANIC SWITCH: turning it off drops every live session at
    /// once, and stops the server outright unless an open vault + [`api_enabled`]
    /// were keeping it up for their own sake.
    ///
    /// [`api_enabled`]: Settings::api_enabled
    pub api_vault_unlock: bool,
    /// Allow `POST /reveal`: the ONE endpoint where secret VALUES leave over the
    /// loopback. Off out of the box; every other endpoint stays non-secret
    /// whatever this says.
    pub api_reveal: bool,
    /// Idle expiry stamped on an API session (read through
    /// [`Settings::api_session_secs`], never off this field).
    pub api_session_ttl: ApiSessionTtl,
    // ---- the browser extension (see `docs/LOGIN-ASSIST.md`) ---------------
    /// Let the DataShield browser extension ask the API for the username and
    /// password of an item, to fill them into a login form. Off out of the
    /// box: it is the second door a secret value leaves through (`/reveal` is
    /// the first), and its row says so.
    pub login_assist: bool,
    /// The PAIRING token the extension presents on `/login/*` — persistent
    /// (the admin token changes every run, which would mean re-pairing the
    /// extension every morning), 32 random bytes as hex, minted the first time
    /// `login_assist` is switched on and regenerated by `logintoken new`.
    /// Empty = never minted. A credential: it leaves only through the
    /// clipboard path, never the screen.
    pub login_token: String,
    // ---- single-key shortcuts (see `keymap.rs`) --------------------------
    /// The user's say over the single-key verbs, keyed by the verb's stable
    /// slug (`add_item`, `copy_password`, …): either `"off"` — no chip in the
    /// keybar, no key — or ONE lowercase ascii letter/digit to use instead of
    /// the built-in mnemonic, in every language. A verb with no entry follows
    /// the table in `keymap.rs`.
    ///
    /// Nothing here is trusted: an entry that is malformed, [`RESERVED`] or
    /// already taken is refused at resolution time and the default stands in
    /// (see `keymap::effective_binds`). MUST STAY LAST — TOML writes tables
    /// after values, so a map in the middle would make `save` fail.
    ///
    /// [`RESERVED`]: crate::keymap::RESERVED
    pub shortcut_overrides: BTreeMap<String, String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            lang: Lang::default(),
            theme: Theme::default(),
            background: Background::default(),
            blur: Blur::default(),
            backdrop: Backdrop::default(),
            backdrop_fx: BackdropFx::default(),
            backdrop_level: BackdropLevel::default(),
            autolock: AutoLock::default(),
            show_logo: true,
            favicons: true,
            reduce_motion: false,
            confirm_exit: false,
            rag_memo: false,
            rag_items: false,
            api_expose_memo: false,
            api_expose_items: false,
            api_port: API_PORT_DEFAULT,
            api_vault_unlock: false,
            api_reveal: false,
            api_session_ttl: ApiSessionTtl::default(),
            login_assist: false,
            login_token: String::new(),
            shortcut_overrides: BTreeMap::new(),
        }
    }
}

impl Settings {
    /// Whether the memorandum is really served: the exposure toggle ALONE is
    /// never enough, because there would be no index behind it. Every consumer
    /// asks this rather than reading `api_expose_memo`, so a hand-edited
    /// `settings.toml` cannot express "expose an index I never built".
    pub fn exposes_memo(&self) -> bool {
        self.rag_memo && self.api_expose_memo
    }

    /// The same for the item index (see [`Settings::exposes_memo`]).
    pub fn exposes_items(&self) -> bool {
        self.rag_items && self.api_expose_items
    }

    /// Whether the agent API has anything at all to serve OUT OF THE OPEN TUI
    /// VAULT — half of the lifecycle switch the server is started and stopped
    /// by (see [`Settings::api_server_wanted`]).
    pub fn api_enabled(&self) -> bool {
        self.exposes_memo() || self.exposes_items()
    }

    /// Whether the server must be up at all: either an open vault is being
    /// served, or `/unlock` is on offer — the second is what lets an agent open
    /// its own vault from the Access page with everything closed.
    pub fn api_server_wanted(&self, vault_open: bool) -> bool {
        // login assist keeps it up with every vault closed too: the extension
        // has to be able to PAIR (and to say "no vault is open") without one —
        // the snapshot it reads is simply empty until a vault opens.
        (vault_open && self.api_enabled()) || self.api_vault_unlock || self.login_assist
    }

    /// The pairing token the `/login/*` endpoints accept — `None` while login
    /// assist is off, whatever the file holds, so a hand-edited token for a
    /// feature that is switched off opens nothing (the `api_session_secs`
    /// precedent). Also `None` when none was ever minted.
    pub fn login_token(&self) -> Option<&str> {
        if !self.login_assist || self.login_token.is_empty() {
            return None;
        }
        Some(&self.login_token)
    }

    /// Mints the pairing token if there is none yet. Answers whether one was
    /// minted (so the caller knows the file needs saving).
    pub fn ensure_login_token(&mut self) -> bool {
        if !self.login_token.is_empty() {
            return false;
        }
        self.login_token = crate::api::new_token().as_str().to_string();
        !self.login_token.is_empty()
    }

    /// A fresh pairing token, retiring the old one (the extension has to be
    /// paired again).
    pub fn regenerate_login_token(&mut self) {
        self.login_token.clear();
        self.ensure_login_token();
    }

    /// The idle expiry a NEW session is stamped with, in seconds (`None` = it
    /// never expires). Read through here and never off the raw field: with
    /// [`Settings::api_vault_unlock`] off there is no session to expire, so a
    /// hand-edited `settings.toml` cannot express a TTL for something that
    /// cannot exist — the [`Settings::exposes_memo`] precedent.
    pub fn api_session_secs(&self) -> Option<u64> {
        if !self.api_vault_unlock {
            return None;
        }
        self.api_session_ttl.secs()
    }

    /// Whether the embedding engine is wanted by either index.
    pub fn rag_enabled(&self) -> bool {
        self.rag_memo || self.rag_items
    }

    /// Loads preferences from `path`, falling back to the defaults if the file
    /// is missing or unreadable/malformed (never fails the app).
    pub fn load(path: &Path) -> Settings {
        fs::read_to_string(path)
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Writes preferences to `path` (creating the parent directory if needed).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let body = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
        let text = format!(
            "# DataShield TUI — user preferences (edit freely, or use the `settings` page).\n{body}"
        );
        fs::write(path, text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file written by an OLDER version has no `[shortcut_overrides]` table —
    /// and must still load, with every verb on its default key.
    #[test]
    fn an_older_file_still_loads() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("settings.toml");
        fs::write(&path, "lang = \"it\"\ntheme = \"brass\"\n").unwrap();
        let s = Settings::load(&path);
        assert_eq!(s.lang, Lang::It);
        assert!(s.shortcut_overrides.is_empty());
        // …and everything the file never heard of is OFF: a preference that was
        // not in it is not consent, least of all the two that open a door.
        assert!(!s.api_vault_unlock && !s.api_reveal);
        assert_eq!(s.api_session_secs(), None);
    }

    /// The three session rows survive the file, and the gate on the expiry is
    /// re-applied when they are read back — a hand-edited "expire in an hour"
    /// with no `/unlock` is a setting about nothing.
    #[test]
    fn the_api_session_preferences_round_trip_and_are_re_gated_on_load() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("settings.toml");
        let mut s = Settings::default();
        s.api_vault_unlock = true;
        s.api_reveal = true;
        s.api_session_ttl = ApiSessionTtl::OneHour;
        s.save(&path).expect("the file is written");
        let back = Settings::load(&path);
        assert!(back.api_vault_unlock && back.api_reveal);
        assert_eq!(back.api_session_secs(), Some(60 * 60));
        // the unlock switch alone is enough to want the server, with no vault
        // open and nothing exposed
        assert!(back.api_server_wanted(false));

        // …and with the switch off the expiry is read as "no sessions", however
        // the file spells it
        let mut hand_edited = Settings::default();
        hand_edited.api_session_ttl = ApiSessionTtl::OneHour;
        assert_eq!(hand_edited.api_session_secs(), None);
        assert!(!hand_edited.api_server_wanted(false));
        assert!(!hand_edited.api_server_wanted(true), "…and an open vault alone is not it either");
    }

    /// The pairing token is gated on the switch, like every other door here:
    /// a token in the file with login assist off opens nothing.
    #[test]
    fn the_login_token_is_minted_once_and_read_through_the_switch() {
        let mut s = Settings::default();
        assert_eq!(s.login_token(), None, "nothing minted, nothing on offer");
        assert!(!s.api_server_wanted(true), "…and nothing listens for it");
        assert!(s.ensure_login_token(), "the first call mints one");
        assert_eq!(s.login_token.len(), 64, "32 bytes, hex");
        assert!(!s.ensure_login_token(), "the second call keeps it");
        assert_eq!(s.login_token(), None, "still off: the token is not on offer");
        s.login_assist = true;
        assert_eq!(s.login_token().map(str::len), Some(64));
        assert!(s.api_server_wanted(true), "login assist wants the server with a vault open");
        assert!(s.api_server_wanted(false), "…and with every vault closed, so the extension can pair");
        let before = s.login_token.clone();
        s.regenerate_login_token();
        assert_ne!(s.login_token, before, "regenerated");
        // …and it survives the file
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("settings.toml");
        s.save(&path).expect("the file is written");
        let back = Settings::load(&path);
        assert!(back.login_assist);
        assert_eq!(back.login_token, s.login_token);
    }

    /// The map round-trips through the file. It is the only TOML *table* in the
    /// struct, which is why it has to be written last — a map declared between
    /// two scalars would make `to_string_pretty` refuse the whole file.
    #[test]
    fn the_shortcut_overrides_survive_a_save_and_a_load() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("settings.toml");
        let mut s = Settings::default();
        s.shortcut_overrides.insert("add_item".into(), "off".into());
        s.shortcut_overrides.insert("delete_item".into(), "x".into());
        s.save(&path).expect("the file is written");
        let back = Settings::load(&path);
        assert_eq!(back.shortcut_overrides.get("add_item").map(String::as_str), Some("off"));
        assert_eq!(back.shortcut_overrides.get("delete_item").map(String::as_str), Some("x"));
        // …and the rest of the file came back untouched
        assert_eq!(back.api_port, API_PORT_DEFAULT);
        assert!(back.show_logo);
    }
}
