//! The settings page (fig-09): ONE scrolling list under section rules —
//! `── GENERAL ──` / `── TUI ──` / `── SECURITY ──` / `── RAG ──` and the two
//! `── SHORTCUTS ──` rules — no tabs (a handful of options doesn't need them).
//! Editing mutates a working [`Settings`] *draft*; cosmetic choices (theme,
//! background) preview live off the draft, while behavioural toggles take
//! effect once the draft is committed on Enter.
//!
//! Interaction: ↑/↓ move through the rows, ←/→ change the focused value,
//! Enter saves, Esc cancels.

use crate::i18n::{self, Key, Lang};
use crate::keymap::{self, ShortcutChoice, Verb};
use crate::settings::{
    ApiSessionTtl, AutoLock, Backdrop, BackdropFx, BackdropLevel, Background, Blur, Settings,
    API_PORTS,
};
use crate::theme::Theme;

/// A section rule of the single list (fig-09).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Section {
    General,
    Tui,
    Security,
    Rag,
    /// The browser extension: login assist, the detected browser, the store
    /// page, the pairing token.
    Browser,
    /// The single-key verbs of the vault LIST…
    ShortcutsAccess,
    /// …and those of an open vault. Two rules rather than one because the two
    /// keymaps are page-scoped: the same word can be two different verbs on the
    /// two pages (in Italian both delete rows read "elimina"), and only the
    /// scope tells them apart.
    ShortcutsVault,
}

impl Section {
    pub fn title(self, lang: Lang) -> String {
        match self {
            Section::General => "GENERAL".into(),
            Section::Tui => "TUI".into(),
            Section::Security => "SECURITY".into(),
            // an acronym, the same word in both languages — like the search
            // bar's mode chip, this is what the thing is called.
            Section::Rag => "RAG".into(),
            // a product word, the same in both languages
            Section::Browser => "BROWSER".into(),
            // …unlike these, which are ordinary words.
            Section::ShortcutsAccess => i18n::t(lang, &Key::SettingSectionShortcutsAccess),
            Section::ShortcutsVault => i18n::t(lang, &Key::SettingSectionShortcutsVault),
        }
    }
}

/// One editable (or read-only informational) setting row.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Row {
    // General
    Language,
    VaultCount,
    // TUI
    Theme,
    Background,
    Blur,
    Backdrop,
    BackdropFx,
    BackdropLevel,
    ShowLogo,
    Favicons,
    ReduceMotion,
    // Security
    LockDelay,
    ConfirmExit,
    // RAG + the localhost agent API
    RagMemo,
    RagItems,
    ApiExposeMemo,
    ApiExposeItems,
    ApiPort,
    /// Allow `POST /unlock` — an agent opening a vault of its own with the
    /// master password it already holds (see [`crate::api`]).
    ApiVaultUnlock,
    /// Allow `POST /reveal` — the one endpoint secret values leave through.
    ApiReveal,
    /// Idle expiry of an API session, gated on [`Row::ApiVaultUnlock`].
    ApiSessionTtl,
    /// Read-only: the first characters of the session token, so the user can
    /// tell which one is live. The FULL token only ever leaves through the
    /// `ragtoken` command (the clipboard path, timed wipe included).
    ApiToken,
    // The browser extension (see `docs/LOGIN-ASSIST.md`).
    /// Allow the extension to fetch an item's username + password.
    LoginAssist,
    /// Read-only: the browser the desktop opens links in, and its family.
    BrowserDetected,
    /// An ACTION row: `←/→` opens that family's extension store page.
    InstallExtension,
    /// An ACTION row: shows the head of the pairing token, and `←/→` copies
    /// the whole of it (the `logintoken` command does the same from outside).
    LoginToken,
    // Shortcuts: one row per single-key verb.
    /// Which character fires this verb — or "off". [`Verb`] is `Copy`, so the
    /// row stays const-friendly like every other one.
    ShortcutKey(Verb),
}

/// Every row in display order, with the section it belongs to. The section
/// rule is drawn whenever it changes between consecutive rows.
///
/// The shortcut rows follow [`Verb::ALL`](crate::keymap::Verb::ALL) under two
/// rules — the vault-list verbs, then the in-vault ones, closing with the
/// settings verb that is bound on both pages. A test keeps the two lists in
/// step, and each row under the rule for the page it fires on.
pub const ALL_ROWS: [(Section, Row); 43] = [
    (Section::General, Row::Language),
    (Section::General, Row::VaultCount),
    (Section::Tui, Row::Theme),
    (Section::Tui, Row::Background),
    (Section::Tui, Row::Blur),
    (Section::Tui, Row::Backdrop),
    (Section::Tui, Row::BackdropFx),
    (Section::Tui, Row::BackdropLevel),
    (Section::Tui, Row::ShowLogo),
    (Section::Tui, Row::Favicons),
    (Section::Tui, Row::ReduceMotion),
    (Section::Security, Row::LockDelay),
    (Section::Security, Row::ConfirmExit),
    (Section::Rag, Row::RagMemo),
    (Section::Rag, Row::RagItems),
    (Section::Rag, Row::ApiExposeMemo),
    (Section::Rag, Row::ApiExposeItems),
    (Section::Rag, Row::ApiPort),
    (Section::Rag, Row::ApiVaultUnlock),
    (Section::Rag, Row::ApiReveal),
    (Section::Rag, Row::ApiSessionTtl),
    (Section::Rag, Row::ApiToken),
    (Section::Browser, Row::LoginAssist),
    (Section::Browser, Row::BrowserDetected),
    (Section::Browser, Row::InstallExtension),
    (Section::Browser, Row::LoginToken),
    (Section::ShortcutsAccess, Row::ShortcutKey(Verb::NewVault)),
    (Section::ShortcutsAccess, Row::ShortcutKey(Verb::RenameVault)),
    (Section::ShortcutsAccess, Row::ShortcutKey(Verb::DeleteVault)),
    (Section::ShortcutsAccess, Row::ShortcutKey(Verb::ImportVault)),
    (Section::ShortcutsAccess, Row::ShortcutKey(Verb::ExportVault)),
    (Section::ShortcutsVault, Row::ShortcutKey(Verb::CopyPassword)),
    (Section::ShortcutsVault, Row::ShortcutKey(Verb::CopyUser)),
    (Section::ShortcutsVault, Row::ShortcutKey(Verb::AddItem)),
    (Section::ShortcutsVault, Row::ShortcutKey(Verb::EditItem)),
    (Section::ShortcutsVault, Row::ShortcutKey(Verb::DeleteItem)),
    (Section::ShortcutsVault, Row::ShortcutKey(Verb::StarItem)),
    (Section::ShortcutsVault, Row::ShortcutKey(Verb::Reveal)),
    (Section::ShortcutsVault, Row::ShortcutKey(Verb::OpenSite)),
    (Section::ShortcutsVault, Row::ShortcutKey(Verb::Login)),
    (Section::ShortcutsVault, Row::ShortcutKey(Verb::Memo)),
    (Section::ShortcutsVault, Row::ShortcutKey(Verb::LockAndBack)),
    (Section::ShortcutsVault, Row::ShortcutKey(Verb::Settings)),
];

impl Row {
    /// The row's label, localized. A shortcut row wears the verb's own keybar
    /// label ("add"/"aggiungi"), so the list reads exactly like the bar it is
    /// editing.
    pub fn label(self, lang: Lang) -> String {
        match self {
            Row::ShortcutKey(verb) => verb.label(lang).to_string(),
            _ => self.label_key().map(|k| i18n::t(lang, &k)).unwrap_or_default(),
        }
    }

    /// The row's label key, or `None` for a shortcut row — whose label is the
    /// verb's own, out of the keymap (see [`Row::label`], the one the page
    /// draws).
    fn label_key(self) -> Option<Key<'static>> {
        Some(match self {
            Row::Language => Key::SettingLabelLanguage,
            Row::VaultCount => Key::SettingLabelVaultCount,
            Row::Theme => Key::SettingLabelTheme,
            Row::Background => Key::SettingLabelBackground,
            Row::Blur => Key::SettingLabelBlur,
            Row::Backdrop => Key::SettingLabelBackdrop,
            Row::BackdropFx => Key::SettingLabelBackdropFx,
            Row::BackdropLevel => Key::SettingLabelBackdropLevel,
            Row::ShowLogo => Key::SettingLabelShowLogo,
            Row::Favicons => Key::SettingLabelFavicons,
            Row::ReduceMotion => Key::SettingLabelReduceMotion,
            Row::LockDelay => Key::SettingLabelAutoLock,
            Row::ConfirmExit => Key::SettingLabelConfirmExit,
            Row::RagMemo => Key::SettingLabelRagMemo,
            Row::RagItems => Key::SettingLabelRagItems,
            Row::ApiExposeMemo => Key::SettingLabelApiMemo,
            Row::ApiExposeItems => Key::SettingLabelApiItems,
            Row::ApiPort => Key::SettingLabelApiPort,
            Row::ApiVaultUnlock => Key::SettingLabelApiUnlock,
            Row::ApiReveal => Key::SettingLabelApiReveal,
            Row::ApiSessionTtl => Key::SettingLabelApiSessionTtl,
            Row::ApiToken => Key::SettingLabelApiToken,
            Row::LoginAssist => Key::SettingLabelLoginAssist,
            Row::BrowserDetected => Key::SettingLabelBrowser,
            Row::InstallExtension => Key::SettingLabelInstallExtension,
            Row::LoginToken => Key::SettingLabelLoginToken,
            Row::ShortcutKey(_) => return None,
        })
    }

    /// A one-line explanation shown for the focused row.
    pub fn hint_key(self) -> Key<'static> {
        match self {
            Row::Language => Key::SettingHintLanguage,
            Row::VaultCount => Key::SettingHintVaultCount,
            Row::Theme => Key::SettingHintTheme,
            Row::Background => Key::SettingHintBackground,
            Row::Blur => Key::SettingHintBlur,
            Row::Backdrop => Key::SettingHintBackdrop,
            Row::BackdropFx => Key::SettingHintBackdropFx,
            Row::BackdropLevel => Key::SettingHintBackdropLevel,
            Row::ShowLogo => Key::SettingHintShowLogo,
            Row::Favicons => Key::SettingHintFavicons,
            Row::ReduceMotion => Key::SettingHintReduceMotion,
            Row::LockDelay => Key::SettingHintAutoLock,
            Row::ConfirmExit => Key::SettingHintConfirmExit,
            Row::RagMemo => Key::SettingHintRagMemo,
            Row::RagItems => Key::SettingHintRagItems,
            Row::ApiExposeMemo => Key::SettingHintApiMemo,
            Row::ApiExposeItems => Key::SettingHintApiItems,
            Row::ApiPort => Key::SettingHintApiPort,
            Row::ApiVaultUnlock => Key::SettingHintApiUnlock,
            Row::ApiReveal => Key::SettingHintApiReveal,
            Row::ApiSessionTtl => Key::SettingHintApiSessionTtl,
            Row::ApiToken => Key::SettingHintApiToken,
            Row::LoginAssist => Key::SettingHintLoginAssist,
            Row::BrowserDetected => Key::SettingHintBrowser,
            Row::InstallExtension => Key::SettingHintInstallExtension,
            Row::LoginToken => Key::SettingHintLoginToken,
            // one sentence for all of them: what "off" costs, and the `:`
            // palette that keeps every verb reachable whatever is chosen here.
            Row::ShortcutKey(_) => Key::SettingHintShortcut,
        }
    }

    /// Read-only rows (informational) can't be cycled.
    pub fn read_only(self) -> bool {
        matches!(self, Row::VaultCount | Row::ApiToken | Row::BrowserDetected)
    }

    /// An ACTION row: `←/→` does something (opens a page) instead of changing
    /// a value — drawn with a single `▸` rather than the `◂ value ▸` pair.
    pub fn is_action(self) -> bool {
        matches!(self, Row::InstallExtension | Row::LoginToken)
    }

    /// The toggle this row is a slave to, if it has one. Exposing an index that
    /// is not built is a promise the server cannot keep, and an expiry for a
    /// session that cannot exist is a setting about nothing — so each pair is
    /// enforced HERE (the cycle refuses) and again at READ time
    /// ([`Settings::exposes_memo`], [`Settings::api_session_secs`]), where the
    /// value is read as off whatever the file says.
    ///
    /// [`Settings::exposes_memo`]: crate::settings::Settings::exposes_memo
    /// [`Settings::api_session_secs`]: crate::settings::Settings::api_session_secs
    fn gated_on(self, s: &Settings) -> Option<bool> {
        match self {
            Row::ApiExposeMemo => Some(s.rag_memo),
            Row::ApiExposeItems => Some(s.rag_items),
            Row::ApiSessionTtl => Some(s.api_vault_unlock),
            _ => None,
        }
    }

    /// Whether this row is currently frozen at "off" by the toggle above it.
    pub fn gated(self, s: &Settings) -> bool {
        self.gated_on(s) == Some(false)
    }

    /// What a refused ←/→ says out loud: WHICH switch above this row is holding
    /// it shut. A key press that silently does nothing reads as a broken page.
    fn gate_hint(self) -> Key<'static> {
        match self {
            Row::ApiSessionTtl => Key::SettingHintApiNeedsUnlock,
            _ => Key::SettingHintApiNeedsRag,
        }
    }
}

/// What a `←/→` on a row asks the APP to do, beyond editing the draft. The
/// page edits a draft and nothing else; anything that reaches outside it (a
/// toast, a browser window) is handed back here and done by `App`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Effect {
    /// Say this out loud: the change was refused, or it has a consequence.
    Hint(Key<'static>),
    /// Open the extension's store page for the detected browser.
    OpenExtensionPage,
    /// Copy the extension's pairing token to the clipboard.
    CopyLoginToken,
}

/// The live state of the open settings page.
pub struct SettingsState {
    /// The working copy being edited.
    pub draft: Settings,
    /// The focused row (an index into [`ALL_ROWS`]).
    pub selected: usize,
}

impl SettingsState {
    pub fn new(settings: Settings) -> SettingsState {
        SettingsState { draft: settings, selected: 0 }
    }

    /// The row the keyboard is on.
    pub fn focused_row(&self) -> Option<Row> {
        ALL_ROWS.get(self.selected).map(|(_, r)| *r)
    }

    /// Moves within the rows, clamped to the list.
    pub fn move_field(&mut self, delta: isize) {
        let last = ALL_ROWS.len().saturating_sub(1) as isize;
        self.selected = (self.selected as isize + delta).clamp(0, last) as usize;
    }

    /// Changes the focused row's value by `delta` (choice rows), or toggles a
    /// boolean row. No-op on read-only rows.
    ///
    /// Returns the message the caller must say out loud, if any: the change was
    /// REFUSED (an exposure row whose `rag_*` toggle is still off — a key press
    /// that silently does nothing reads as a broken page), or it went through
    /// and has a consequence the user needs to hear (a verb switched off, and
    /// the palette that still reaches it).
    pub fn cycle(&mut self, delta: isize) -> Option<Effect> {
        let Some(row) = self.focused_row() else {
            return None;
        };
        if row.gated(&self.draft) {
            return Some(Effect::Hint(row.gate_hint()));
        }
        let d = &mut self.draft;
        match row {
            Row::Language => d.lang = cycle_enum(&Lang::ALL, d.lang, delta),
            Row::Theme => d.theme = cycle_enum(&Theme::ALL, d.theme, delta),
            Row::Background => {
                d.background = match d.background {
                    Background::Transparent => Background::Opaque,
                    Background::Opaque => Background::Transparent,
                }
            }
            Row::Blur => {
                d.blur = match d.blur {
                    Blur::Off => Blur::On,
                    Blur::On => Blur::Off,
                }
            }
            Row::Backdrop => {
                d.backdrop = match d.backdrop {
                    Backdrop::Auto => Backdrop::On,
                    Backdrop::On => Backdrop::Off,
                    Backdrop::Off => Backdrop::Auto,
                }
            }
            Row::BackdropFx => {
                d.backdrop_fx = match d.backdrop_fx {
                    BackdropFx::Rain => BackdropFx::Waves,
                    BackdropFx::Waves => BackdropFx::Rain,
                }
            }
            Row::BackdropLevel => {
                // a slider: ←/→ step within [Faint, Full] and clamp at the ends
                let steps = &BackdropLevel::STEPS;
                let i = steps.iter().position(|x| *x == d.backdrop_level).unwrap_or(0) as isize;
                let j = (i + delta).clamp(0, steps.len() as isize - 1) as usize;
                d.backdrop_level = steps[j];
            }
            Row::LockDelay => {
                let all = [AutoLock::Off, AutoLock::OneMin, AutoLock::FiveMin, AutoLock::FifteenMin];
                d.autolock = cycle_enum(&all, d.autolock, delta);
            }
            Row::ShowLogo => d.show_logo = !d.show_logo,
            Row::Favicons => d.favicons = !d.favicons,
            Row::VaultCount => {}
            Row::ReduceMotion => d.reduce_motion = !d.reduce_motion,
            Row::ConfirmExit => d.confirm_exit = !d.confirm_exit,
            Row::RagMemo => {
                d.rag_memo = !d.rag_memo;
                // the index the exposure was serving is gone with the toggle;
                // leaving the flag set would re-expose it on the next flip
                // without the user ever saying so again.
                if !d.rag_memo {
                    d.api_expose_memo = false;
                }
            }
            Row::RagItems => {
                d.rag_items = !d.rag_items;
                if !d.rag_items {
                    d.api_expose_items = false;
                }
            }
            Row::ApiExposeMemo => d.api_expose_memo = !d.api_expose_memo,
            Row::ApiExposeItems => d.api_expose_items = !d.api_expose_items,
            Row::ApiPort => {
                // a hand-edited port is not in the cycle: ←/→ then move
                // relative to the first preset, which is how the user gets
                // back onto the list without opening the file again.
                d.api_port = cycle_enum(&API_PORTS, d.api_port, delta);
            }
            Row::ApiVaultUnlock => {
                d.api_vault_unlock = !d.api_vault_unlock;
                // the sessions the expiry was about are gone with the switch;
                // leaving a hand-picked TTL behind would quietly re-apply it on
                // the next flip without the user ever saying so again — the
                // `rag_memo` → `api_expose_memo` precedent, one row down.
                if !d.api_vault_unlock {
                    d.api_session_ttl = ApiSessionTtl::default();
                }
            }
            Row::ApiReveal => d.api_reveal = !d.api_reveal,
            Row::ApiSessionTtl => {
                d.api_session_ttl = cycle_enum(&ApiSessionTtl::ALL, d.api_session_ttl, delta)
            }
            Row::ApiToken => {}
            Row::LoginAssist => d.login_assist = !d.login_assist,
            Row::BrowserDetected => {}
            Row::LoginToken => return Some(Effect::CopyLoginToken),
            // the one row that DOES something rather than setting something
            Row::InstallExtension => return Some(Effect::OpenExtensionPage),
            Row::ShortcutKey(verb) => {
                // default → every free character → off → round again. The list
                // is built against the draft, so what it offers is exactly what
                // the resolution will grant (see `keymap::shortcut_candidates`).
                let all = keymap::shortcut_candidates(verb, d);
                let current = ShortcutChoice::stored(verb, d);
                let next = cycle_enum(&all, current, delta);
                next.store(verb, d);
                // taking a verb off the keybar is the one choice here with a
                // consequence worth hearing — including the way back to it.
                if next == ShortcutChoice::Off {
                    return Some(Effect::Hint(Key::SettingHintShortcut));
                }
            }
        }
        None
    }
}

/// The display value of a shortcut row: the chosen character, "off", or the
/// default one tagged as such (`a · default`) — read through the RESOLUTION, so
/// a character the file asked for but could not have reads as the default that
/// is really answering for it.
pub fn shortcut_value(verb: Verb, settings: &Settings, lang: Lang) -> String {
    let bind = keymap::effective(verb, settings);
    if !bind.enabled {
        return i18n::t(lang, &Key::SettingShortcutOff);
    }
    match ShortcutChoice::stored(verb, settings) {
        ShortcutChoice::Key(c) => c.to_string(),
        _ => format!("{} \u{b7} {}", bind.key(lang), i18n::t(lang, &Key::SettingShortcutDefault)),
    }
}

/// Returns the neighbour of `current` in `all`, wrapping around.
fn cycle_enum<T: Copy + PartialEq>(all: &[T], current: T, delta: isize) -> T {
    let n = all.len() as isize;
    if n == 0 {
        return current;
    }
    let i = all.iter().position(|x| *x == current).unwrap_or(0) as isize;
    all[((i + delta).rem_euclid(n)) as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A verb with no row could never be customized; a row with no verb would
    /// edit nothing. The keymap and this list are one list.
    #[test]
    fn every_verb_has_exactly_one_shortcut_row() {
        for &verb in Verb::ALL.iter() {
            let n = ALL_ROWS.iter().filter(|(_, r)| *r == Row::ShortcutKey(verb)).count();
            assert_eq!(n, 1, "{verb:?} has {n} rows");
        }
        let rows = ALL_ROWS
            .iter()
            .filter(|(s, _)| matches!(s, Section::ShortcutsAccess | Section::ShortcutsVault))
            .count();
        assert_eq!(rows, Verb::ALL.len(), "the section holds a row for every verb and nothing else");
        // …and each row sits under the rule for the page it fires on
        for (section, row) in ALL_ROWS.iter() {
            let Row::ShortcutKey(verb) = row else { continue };
            let access = keymap::binds(crate::app::Page::Access).iter().any(|b| b.verb == *verb);
            let wanted = if access && *verb != Verb::Settings {
                Section::ShortcutsAccess
            } else {
                // `Settings` is bound on both pages; it is listed once, last
                Section::ShortcutsVault
            };
            assert_eq!(*section, wanted, "{verb:?} is filed under the wrong rule");
        }
    }

    /// ←/→ walks default → characters → off → default again, writing the draft
    /// as it goes. Nothing in the walk can collide (the candidates are built
    /// against the draft), and the round trip lands back on the default.
    #[test]
    fn a_shortcut_row_cycles_through_the_free_characters_and_off() {
        let mut state = SettingsState::new(Settings::default());
        let verb = Verb::AddItem;
        state.selected =
            ALL_ROWS.iter().position(|(_, r)| *r == Row::ShortcutKey(verb)).expect("the row exists");
        let steps = keymap::shortcut_candidates(verb, &state.draft).len();
        assert_eq!(ShortcutChoice::stored(verb, &state.draft), ShortcutChoice::Default);
        // one step right: the first free character
        assert!(state.cycle(1).is_none(), "a shortcut row is never refused");
        assert!(matches!(ShortcutChoice::stored(verb, &state.draft), ShortcutChoice::Key(_)));
        // one step LEFT from the default is "off" — the far end of the list,
        // and the one step that says something out loud
        state.draft.shortcut_overrides.clear();
        assert!(state.cycle(-1).is_some(), "switching a verb off names the way back to it");
        assert_eq!(ShortcutChoice::stored(verb, &state.draft), ShortcutChoice::Off);
        // …and a full lap comes home
        state.draft.shortcut_overrides.clear();
        for _ in 0..steps {
            state.cycle(1);
        }
        assert_eq!(ShortcutChoice::stored(verb, &state.draft), ShortcutChoice::Default);
        assert!(state.draft.shortcut_overrides.is_empty(), "the default is stored as no entry");
    }

    /// The browser rows: a toggle, two read-only lines and one ACTION row that
    /// hands the page's one outward effect back to the app.
    #[test]
    fn the_browser_rows_toggle_read_and_act() {
        let mut state = SettingsState::new(Settings::default());
        let at = |row: Row| ALL_ROWS.iter().position(|(_, r)| *r == row).expect("the row exists");
        state.selected = at(Row::LoginAssist);
        assert!(state.cycle(1).is_none());
        assert!(state.draft.login_assist, "the toggle flipped");
        state.selected = at(Row::BrowserDetected);
        assert!(Row::BrowserDetected.read_only() && state.cycle(1).is_none());
        state.selected = at(Row::LoginToken);
        assert!(Row::LoginToken.is_action());
        assert_eq!(state.cycle(-1), Some(Effect::CopyLoginToken), "←/→ copies the token");
        state.selected = at(Row::InstallExtension);
        assert!(Row::InstallExtension.is_action());
        assert_eq!(state.cycle(1), Some(Effect::OpenExtensionPage), "←/→ asks the app to open the store");
        assert_eq!(state.cycle(-1), Some(Effect::OpenExtensionPage), "…either way");
        // the four sit under one rule, in this order
        let browser: Vec<Row> = ALL_ROWS.iter().filter(|(s, _)| *s == Section::Browser).map(|(_, r)| *r).collect();
        assert_eq!(browser, [Row::LoginAssist, Row::BrowserDetected, Row::InstallExtension, Row::LoginToken]);
    }

    /// The value column reads the RESOLUTION, not the file: an override that
    /// could not be granted shows the default that is really answering.
    #[test]
    fn the_row_value_says_what_the_key_really_is() {
        let mut s = Settings::default();
        assert_eq!(shortcut_value(Verb::AddItem, &s, Lang::En), "a · default");
        // the Italian row shows the Italian mnemonic, tagged in Italian
        assert_eq!(shortcut_value(Verb::EditItem, &s, Lang::It), "m · predefinito");
        ShortcutChoice::Key('x').store(Verb::AddItem, &mut s);
        assert_eq!(shortcut_value(Verb::AddItem, &s, Lang::En), "x");
        ShortcutChoice::Off.store(Verb::AddItem, &mut s);
        assert_eq!(shortcut_value(Verb::AddItem, &s, Lang::En), i18n::t(Lang::En, &Key::SettingShortcutOff));
        // a hand-edited collision: `e` is the English "edit" here
        s.shortcut_overrides.insert(Verb::AddItem.slug().into(), "e".into());
        assert_eq!(shortcut_value(Verb::AddItem, &s, Lang::En), "a · default");
    }
}
