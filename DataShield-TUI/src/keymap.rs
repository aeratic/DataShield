//! Single-key verb keymap (BRASS LEDGER "P1").
//!
//! The product is *look up → copy → leave*, so the hourly verbs live on single
//! keys (k9s-style), advertised in the keybar; the long tail lives in the `:`
//! palette. This table is the SINGLE SOURCE for both the dispatch and the
//! rendered legend (keybar + `?` cheatsheet), so what the screen advertises can
//! never drift from what the keys actually do.
//!
//! It is **per-locale**: the mnemonics are English-derived in `en` and
//! Italian-derived in `it`, so no English letter is load-bearing. Collisions
//! are resolved *page-scoped* (e.g. on Access `e` = elimina→deleteVault, in a
//! vault `e` = elimina→deleteItem while `m` = modifica→editItem).

//!
//! The table below holds the DEFAULTS. What the app actually binds is that
//! table read through the user's `shortcut_overrides` — see
//! [`effective_binds`], the one resolution point every consumer (dispatch,
//! keybar, cheatsheet) goes through.

use crate::app::Page;
use crate::i18n::Lang;
use crate::settings::Settings;
use ratatui::crossterm::event::KeyCode;

/// A verb a single key triggers. Some map straight to a registry [`Action`]
/// (dispatched via `run_command`); others are direct app behaviors. A chip is a
/// PROMISE — copy/user/open/reveal all act now — so the one verb still to land
/// (rename = P3) is the only one `dispatch_verb` answers with a "coming soon"
/// note.
///
/// [`Action`]: crate::registry::Action
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verb {
    // access
    NewVault,
    RenameVault,
    DeleteVault,
    ImportVault,
    ExportVault,
    // in-vault item management
    AddItem,
    EditItem,
    DeleteItem,
    StarItem,
    CopyPassword,
    CopyUser,
    Reveal,
    OpenSite,
    /// Open the item's site in the browser AND offer the browser extension
    /// its username + password for that page (see `docs/LOGIN-ASSIST.md`).
    Login,
    LockAndBack,
    /// Open the Memorandum — the chat with yourself kept inside the vault.
    Memo,
    // shared
    Settings,
}

impl Verb {
    /// Every verb a single key can carry, in settings-page order: the access
    /// table, then the vault one, then `Settings` — which is bound on BOTH
    /// pages and so belongs to neither group.
    pub const ALL: [Verb; 17] = [
        Verb::NewVault,
        Verb::RenameVault,
        Verb::DeleteVault,
        Verb::ImportVault,
        Verb::ExportVault,
        Verb::CopyPassword,
        Verb::CopyUser,
        Verb::AddItem,
        Verb::EditItem,
        Verb::DeleteItem,
        Verb::StarItem,
        Verb::Reveal,
        Verb::OpenSite,
        Verb::Login,
        Verb::Memo,
        Verb::LockAndBack,
        Verb::Settings,
    ];

    /// The verb's STABLE name in `settings.toml` (`shortcut_overrides`). It is
    /// written to disk, so it never follows a rename of the enum variant and it
    /// is never localized: the file is the same in every language.
    pub fn slug(self) -> &'static str {
        match self {
            Verb::NewVault => "new_vault",
            Verb::RenameVault => "rename_vault",
            Verb::DeleteVault => "delete_vault",
            Verb::ImportVault => "import_vault",
            Verb::ExportVault => "export_vault",
            Verb::AddItem => "add_item",
            Verb::EditItem => "edit_item",
            Verb::DeleteItem => "delete_item",
            Verb::StarItem => "star_item",
            Verb::CopyPassword => "copy_password",
            Verb::CopyUser => "copy_user",
            Verb::Reveal => "reveal",
            Verb::OpenSite => "open_site",
            Verb::Login => "login",
            Verb::LockAndBack => "lock",
            Verb::Memo => "memo",
            Verb::Settings => "settings",
        }
    }

    /// The pages this verb is advertised on (`Settings` is on both).
    fn pages(self) -> Vec<Page> {
        [Page::Access, Page::Vault]
            .into_iter()
            .filter(|p| binds(*p).iter().any(|b| b.verb == self))
            .collect()
    }

    /// The verb's DEFAULT bind (the table row it comes from).
    fn default_bind(self) -> &'static Bind {
        ACCESS
            .iter()
            .chain(VAULT.iter())
            .find(|b| b.verb == self)
            .expect("every Verb is in one of the two tables")
    }

    /// The verb's keybar label in `lang` — the name the user knows it by, used
    /// by the keybar, the cheatsheet and its settings row alike.
    pub fn label(self, lang: Lang) -> &'static str {
        self.default_bind().label(lang)
    }
}

/// The value a `shortcut_overrides` entry carries to switch a verb OFF: no
/// chip in the keybar, no row in the cheatsheet, and the key goes inert.
pub const SHORTCUT_OFF: &str = "off";

/// Characters an override may never claim. `:` `/` `?` open the palette, the
/// filter and the cheatsheet before the keymap is ever consulted; `j`/`k` move
/// the list; `n` and the YES keys of every locale — `y` in English, `s` in
/// Italian ([`Lang::confirm_key`]) — answer a delete confirm. A verb parked on
/// one of these would simply never fire; parked on a yes key it would be worse
/// than dead, because the confirm answers first and DELETES. So the choice is
/// refused instead of granted and then swallowed. (The DEFAULTS are exempt:
/// `n` = new/note and `s` = star are answered by the keymap on a page where no
/// confirm is up — a test keeps this list in step with `Lang::confirm_key`.)
pub const RESERVED: [char; 8] = [':', '/', '?', 'j', 'k', 'y', 's', 'n'];

/// One key binding: the localized keys, the verb, and its localized keybar
/// labels. (Whether a verb's behavior is fully wired yet is decided by
/// `dispatch_verb`, which surfaces a "coming soon" note for the last phase.)
pub struct Bind {
    pub en: char,
    pub it: char,
    pub verb: Verb,
    pub en_label: &'static str,
    pub it_label: &'static str,
}

impl Bind {
    /// The key shown for this bind in the current locale.
    pub fn key(&self, lang: Lang) -> char {
        match lang {
            Lang::En => self.en,
            Lang::It => self.it,
        }
    }
    /// The keybar label for this bind in the current locale.
    pub fn label(&self, lang: Lang) -> &'static str {
        match lang {
            Lang::En => self.en_label,
            Lang::It => self.it_label,
        }
    }
}

// Access-page verbs: manage the vault list.
const ACCESS: &[Bind] = &[
    Bind { en: 'n', it: 'n', verb: Verb::NewVault, en_label: "new", it_label: "nuovo" },
    Bind { en: 'r', it: 'r', verb: Verb::RenameVault, en_label: "rename", it_label: "rinomina" },
    Bind { en: 'd', it: 'e', verb: Verb::DeleteVault, en_label: "delete", it_label: "elimina" },
    Bind { en: 'i', it: 'i', verb: Verb::ImportVault, en_label: "import", it_label: "importa" },
    Bind { en: 'x', it: 'x', verb: Verb::ExportVault, en_label: "export", it_label: "esporta" },
    Bind { en: ',', it: ',', verb: Verb::Settings, en_label: "settings", it_label: "opzioni" },
];

// In-vault verbs: item management + the copy/reveal/open verbs.
const VAULT: &[Bind] = &[
    // `c` for copy in BOTH locales: the vi-ism `y` (yank) meant nothing to the
    // people this is for, and `c` is the letter every one of them reaches for.
    // The `y`/`n` delete confirm is answered before the keymap is consulted, so
    // freeing `y` here costs nothing.
    Bind { en: 'c', it: 'c', verb: Verb::CopyPassword, en_label: "copy", it_label: "copia" },
    Bind { en: 'u', it: 'u', verb: Verb::CopyUser, en_label: "user", it_label: "utente" },
    Bind { en: 'a', it: 'a', verb: Verb::AddItem, en_label: "add", it_label: "aggiungi" },
    Bind { en: 'e', it: 'm', verb: Verb::EditItem, en_label: "edit", it_label: "modifica" },
    Bind { en: 'd', it: 'e', verb: Verb::DeleteItem, en_label: "del", it_label: "elimina" },
    Bind { en: 's', it: 'p', verb: Verb::StarItem, en_label: "star", it_label: "preferito" },
    Bind { en: 'r', it: 'r', verb: Verb::Reveal, en_label: "reveal", it_label: "rivela" },
    Bind { en: 'o', it: 'o', verb: Verb::OpenSite, en_label: "open", it_label: "apri" },
    // `l` is free in both locales: login/accedi — open the site AND hand the
    // browser extension this item's credentials for it.
    Bind { en: 'l', it: 'l', verb: Verb::Login, en_label: "login", it_label: "accedi" },
    // the Memorandum. `m` is free in `en` (edit is `e` there), but taken in `it`
    // by modifica → EditItem, so the Italian mnemonic comes from what the thing
    // IS rather than from its name: `n` for *note*, which nothing on this page
    // claims (the access-page `n` = nuovo is another table).
    Bind { en: 'm', it: 'n', verb: Verb::Memo, en_label: "memo", it_label: "note" },
    Bind { en: 'q', it: 'q', verb: Verb::LockAndBack, en_label: "lock", it_label: "blocca" },
    Bind { en: ',', it: ',', verb: Verb::Settings, en_label: "settings", it_label: "opzioni" },
];

/// The DEFAULT binds of `page` (the table, in file order).
///
/// This is the defaults-only view: it does not know about the user's
/// `shortcut_overrides`. Everything the user can see or press goes through
/// [`effective_binds`] / [`lookup`] instead, which wrap this one source — so
/// the keybar, the cheatsheet and the dispatch stay the same table by
/// construction, customized or not.
pub fn binds(page: Page) -> &'static [Bind] {
    match page {
        Page::Access => ACCESS,
        Page::Vault => VAULT,
    }
}

// ---- user overrides: the ONE resolution point -----------------------------

/// A bind as it really is right now: the table row, with the user's chosen
/// character (the same one in every language — somebody who picks `x` for
/// delete wants `x` whatever the interface speaks) and whether the verb is on
/// at all.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct EffectiveBind {
    pub verb: Verb,
    pub en: char,
    pub it: char,
    pub en_label: &'static str,
    pub it_label: &'static str,
    /// `false` = switched off in the settings: no chip, no key.
    pub enabled: bool,
    /// `true` = the file asked for a character that could not be granted
    /// (reserved, malformed, or taken) and the DEFAULT is standing in.
    pub refused: bool,
}

impl EffectiveBind {
    /// The key shown for this bind in the current locale.
    pub fn key(&self, lang: Lang) -> char {
        match lang {
            Lang::En => self.en,
            Lang::It => self.it,
        }
    }
    /// The keybar label for this bind in the current locale.
    pub fn label(&self, lang: Lang) -> &'static str {
        match lang {
            Lang::En => self.en_label,
            Lang::It => self.it_label,
        }
    }
}

/// What a `shortcut_overrides` entry asks for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Want {
    /// No entry: the table's own per-locale mnemonics.
    Default,
    /// One character, in every language.
    Key(char),
    /// The verb is switched off.
    Off,
    /// The entry is not a single a-z/0-9 character, or asks for a [`RESERVED`]
    /// one. Hand-edited files get the default back, and a word about it.
    Bad,
}

/// Reads the raw entry for `verb` out of `settings`.
fn want(verb: Verb, settings: &Settings) -> Want {
    let Some(raw) = settings.shortcut_overrides.get(verb.slug()) else {
        return Want::Default;
    };
    let raw = raw.trim();
    if raw.is_empty() {
        return Want::Default;
    }
    if raw.eq_ignore_ascii_case(SHORTCUT_OFF) {
        return Want::Off;
    }
    let mut chars = raw.chars();
    match (chars.next(), chars.next()) {
        // an upper-case letter is granted as its lower-case self: the lookup
        // folds case anyway, so refusing `X` would be pedantry.
        (Some(c), None) => {
            let c = c.to_ascii_lowercase();
            if (c.is_ascii_lowercase() || c.is_ascii_digit()) && !RESERVED.contains(&c) {
                Want::Key(c)
            } else {
                Want::Bad
            }
        }
        _ => Want::Bad,
    }
}

/// Resolves EVERY verb against the user's overrides at once.
///
/// One pass over all pages, so a character is judged the same wherever the verb
/// is advertised (`Settings` is on both pages): every enabled verb starts out
/// holding its own default — which is what a refused override falls back to —
/// and an override is granted only when nothing else on any of the verb's pages
/// holds that character, in either locale. Granted, the verb hands its defaults
/// back. The invariant that comes out is the same one the table has always had:
/// no two live verbs share a key on a page.
fn resolve(settings: &Settings) -> Vec<EffectiveBind> {
    let mut out: Vec<EffectiveBind> = Verb::ALL
        .iter()
        .map(|&verb| {
            let b = verb.default_bind();
            EffectiveBind {
                verb,
                en: b.en,
                it: b.it,
                en_label: b.en_label,
                it_label: b.it_label,
                enabled: want(verb, settings) != Want::Off,
                refused: false,
            }
        })
        .collect();
    // who holds which character on which page — BOTH locales in one list,
    // because a character taken by any live verb in any language is taken.
    let mut held: Vec<(Page, char, Verb)> = Vec::new();
    for page in [Page::Access, Page::Vault] {
        for b in binds(page) {
            if out.iter().any(|e| e.verb == b.verb && e.enabled) {
                held.push((page, b.en, b.verb));
                held.push((page, b.it, b.verb));
            }
        }
    }
    for &verb in Verb::ALL.iter() {
        let ch = match want(verb, settings) {
            Want::Key(c) => c,
            Want::Bad => {
                if let Some(e) = out.iter_mut().find(|e| e.verb == verb) {
                    e.refused = true;
                }
                continue;
            }
            Want::Default | Want::Off => continue,
        };
        let pages = verb.pages();
        let free = held.iter().all(|(p, c, v)| !pages.contains(p) || *c != ch || *v == verb);
        let Some(entry) = out.iter_mut().find(|e| e.verb == verb) else {
            continue;
        };
        if !free {
            entry.refused = true;
            continue;
        }
        // granted: the verb gives its default mnemonics back and takes `ch`
        held.retain(|(p, _, v)| !(pages.contains(p) && *v == verb));
        held.extend(pages.iter().map(|p| (*p, ch, verb)));
        entry.en = ch;
        entry.it = ch;
    }
    out
}

/// The binds `page` really advertises under `settings`: the table, resolved,
/// with the switched-off verbs GONE. The keybar, the cheatsheet and the
/// dispatch all read this — a verb missing here is missing everywhere.
pub fn effective_binds(page: Page, settings: &Settings) -> Vec<EffectiveBind> {
    let resolved = resolve(settings);
    binds(page)
        .iter()
        .filter_map(|b| resolved.iter().find(|e| e.verb == b.verb).copied())
        .filter(|e| e.enabled)
        .collect()
}

/// One verb's resolved bind, switched off or not — what the settings row shows.
pub fn effective(verb: Verb, settings: &Settings) -> EffectiveBind {
    resolve(settings)
        .into_iter()
        .find(|e| e.verb == verb)
        .expect("every Verb is resolved")
}

/// The verbs whose stored override could not be granted — a hand-edited
/// `settings.toml` asking for a reserved/taken/malformed character. They are
/// running on their defaults; the caller says so once, at startup.
pub fn refused_overrides(settings: &Settings) -> Vec<Verb> {
    resolve(settings).into_iter().filter(|e| e.refused).map(|e| e.verb).collect()
}

/// What the settings row for `verb` offers, in cycling order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ShortcutChoice {
    /// The built-in, per-locale mnemonics (edit: en `e` · it `m`).
    Default,
    /// One character, the same in every language.
    Key(char),
    /// Off: the verb keeps working from the `:` palette and nowhere else.
    Off,
}

impl ShortcutChoice {
    /// The choice `settings` currently stores for `verb`. A stored character
    /// that was REFUSED reads as [`ShortcutChoice::Default`] — which is what
    /// the app is really doing with it — so cycling from there heals the file.
    pub fn stored(verb: Verb, settings: &Settings) -> ShortcutChoice {
        match want(verb, settings) {
            Want::Off => ShortcutChoice::Off,
            Want::Key(c) if !effective(verb, settings).refused => ShortcutChoice::Key(c),
            _ => ShortcutChoice::Default,
        }
    }

    /// Writes this choice into `settings` for `verb`. The default is written as
    /// the ABSENCE of an entry, so a verb left alone keeps following the table
    /// (including a future change to it).
    pub fn store(self, verb: Verb, settings: &mut Settings) {
        match self {
            ShortcutChoice::Default => {
                settings.shortcut_overrides.remove(verb.slug());
            }
            ShortcutChoice::Off => {
                settings.shortcut_overrides.insert(verb.slug().to_string(), SHORTCUT_OFF.to_string());
            }
            ShortcutChoice::Key(c) => {
                settings.shortcut_overrides.insert(verb.slug().to_string(), c.to_string());
            }
        }
    }
}

/// Everything the settings row for `verb` may cycle through: the default
/// first, then every character (a-z then 0-9, minus the [`RESERVED`] ones) the
/// resolution would really grant this verb, then "off".
///
/// Each candidate is TRIED against [`resolve`] rather than compared to a list
/// of taken characters built beside it. A `taken` list read the DEFAULT
/// mnemonics of the verbs `resolve` had not reached yet, so a character a
/// later verb had already moved off looked free here and was refused there:
/// the row offered a choice that snapped straight back to the default, cycling
/// could never walk past the handful of characters in front of it, and the
/// refused entry was persisted and reported at the next startup. Asking the
/// resolution is the only test that cannot drift from it — and it also catches
/// the mirror case, a choice that would knock ANOTHER verb's granted key out.
pub fn shortcut_candidates(verb: Verb, settings: &Settings) -> Vec<ShortcutChoice> {
    let mine = verb.default_bind();
    // whatever a hand-edited file already spoiled is not this row's doing: the
    // candidates only have to avoid making it worse.
    let before = refused_overrides(settings);
    let mut out = vec![ShortcutChoice::Default];
    out.extend(
        ('a'..='z')
            .chain('0'..='9')
            .filter(|c| !RESERVED.contains(c))
            // the verb's own mnemonics are the first entry already
            .filter(|c| *c != mine.en && *c != mine.it)
            .filter(|c| {
                let mut probe = settings.clone();
                ShortcutChoice::Key(*c).store(verb, &mut probe);
                let after = refused_overrides(&probe);
                !after.contains(&verb) && after.iter().all(|v| before.contains(v))
            })
            .map(ShortcutChoice::Key),
    );
    out.push(ShortcutChoice::Off);
    out
}

/// The verb bound to `code` on `page` in `lang` under `settings`, if any. Only
/// bare character keys are matched here; `:`/`/` (palette), `?` (cheatsheet),
/// Tab (category), Ctrl-chords and the arrows are handled directly in `on_key`.
///
/// This is the ONLY dispatch door, and it reads the same resolution the keybar
/// draws: a verb the user switched off is not found here either.
pub fn lookup(page: Page, lang: Lang, code: KeyCode, settings: &Settings) -> Option<Verb> {
    let KeyCode::Char(ch) = code else { return None };
    let ch = ch.to_ascii_lowercase();
    effective_binds(page, settings).iter().find(|b| b.key(lang) == ch).map(|b| b.verb)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The untouched keymap: what every test that isn't about overrides means.
    fn plain() -> Settings {
        Settings::default()
    }

    /// A `settings.toml` with one `shortcut_overrides` entry.
    fn with(verb: Verb, value: &str) -> Settings {
        let mut s = Settings::default();
        s.shortcut_overrides.insert(verb.slug().to_string(), value.to_string());
        s
    }

    /// Two verbs on one key would make [`lookup`] pick by table ORDER while the
    /// keybar advertises both — the one drift this table exists to prevent.
    #[test]
    fn no_page_binds_the_same_key_twice_in_either_locale() {
        for page in [Page::Access, Page::Vault] {
            for lang in Lang::ALL {
                let mut seen: Vec<(char, Verb)> = Vec::new();
                for bind in binds(page) {
                    let key = bind.key(lang);
                    if let Some((_, other)) = seen.iter().find(|(k, _)| *k == key) {
                        panic!("{page:?}/{lang:?}: '{key}' is both {other:?} and {:?}", bind.verb);
                    }
                    seen.push((key, bind.verb));
                    // …and every advertised key really dispatches to its verb
                    assert_eq!(lookup(page, lang, KeyCode::Char(key), &plain()), Some(bind.verb));
                }
            }
        }
    }

    /// `j`/`k` fall through to the list movement in `on_key`, which is consulted
    /// only AFTER this table: a verb there would silently eat both.
    #[test]
    fn the_movement_letters_are_left_alone() {
        for page in [Page::Access, Page::Vault] {
            for lang in Lang::ALL {
                for ch in ['j', 'k'] {
                    assert_eq!(
                        lookup(page, lang, KeyCode::Char(ch), &plain()),
                        None,
                        "{page:?}/{lang:?} '{ch}'"
                    );
                }
            }
        }
    }

    /// The settings file names verbs by [`Verb::slug`] forever: two verbs on one
    /// slug would silently share a customization.
    #[test]
    fn every_verb_has_its_own_slug_and_a_table_row() {
        let mut seen: Vec<&str> = Vec::new();
        for &verb in Verb::ALL.iter() {
            assert!(!seen.contains(&verb.slug()), "{:?} reuses a slug", verb);
            seen.push(verb.slug());
            assert!(!verb.pages().is_empty(), "{verb:?} is advertised nowhere");
        }
        // …and nothing in the tables is missing from `Verb::ALL` (which is what
        // the settings page enumerates).
        for page in [Page::Access, Page::Vault] {
            for b in binds(page) {
                assert!(Verb::ALL.contains(&b.verb), "{:?} has no settings row", b.verb);
            }
        }
    }

    /// The user's word: off means gone from the legend AND inert.
    #[test]
    fn off_removes_the_verb_from_the_legend_and_from_the_dispatch() {
        let s = with(Verb::AddItem, SHORTCUT_OFF);
        assert!(effective_binds(Page::Vault, &s).iter().all(|b| b.verb != Verb::AddItem));
        assert_eq!(lookup(Page::Vault, Lang::En, KeyCode::Char('a'), &s), None);
        // …and the rest of the page is untouched
        assert_eq!(
            lookup(Page::Vault, Lang::En, KeyCode::Char('c'), &s),
            Some(Verb::CopyPassword)
        );
    }

    /// A chosen character replaces the default in EVERY language, and the old
    /// one goes dead.
    #[test]
    fn a_chosen_character_wins_in_both_locales() {
        let s = with(Verb::DeleteItem, "x");
        for lang in Lang::ALL {
            assert_eq!(lookup(Page::Vault, lang, KeyCode::Char('x'), &s), Some(Verb::DeleteItem));
        }
        // `d` (en) and `e` (it) were its defaults — now nothing answers `d`,
        // while `e` is back to being just the English "edit"
        assert_eq!(lookup(Page::Vault, Lang::En, KeyCode::Char('d'), &s), None);
        assert_eq!(lookup(Page::Vault, Lang::En, KeyCode::Char('e'), &s), Some(Verb::EditItem));
        assert_eq!(lookup(Page::Vault, Lang::It, KeyCode::Char('e'), &s), None);
    }

    /// A hand-edited file cannot break the page: a reserved, malformed or
    /// already-taken character is refused and the default stands in — visibly,
    /// so the app can say so at startup.
    #[test]
    fn a_bad_override_falls_back_to_the_default_and_is_reported() {
        for bad in ["j", ":", "", "no", "ù", "off!"] {
            let s = with(Verb::AddItem, bad);
            let e = effective(Verb::AddItem, &s);
            assert_eq!((e.en, e.it), ('a', 'a'), "'{bad}' should not have been granted");
            assert!(e.enabled, "'{bad}' is not 'off'");
            if !bad.trim().is_empty() {
                assert_eq!(refused_overrides(&s), vec![Verb::AddItem], "'{bad}' went unreported");
            }
        }
        // 'e' is the English "edit" on this page — taken
        let s = with(Verb::AddItem, "e");
        assert_eq!(effective(Verb::AddItem, &s).en, 'a');
        assert_eq!(refused_overrides(&s), vec![Verb::AddItem]);
        // …while the same letter on the OTHER page's verb is nobody's business
        let s = with(Verb::ImportVault, "a");
        assert_eq!(effective(Verb::ImportVault, &s).en, 'a');
        assert!(refused_overrides(&s).is_empty());
        assert_eq!(lookup(Page::Access, Lang::It, KeyCode::Char('a'), &s), Some(Verb::ImportVault));
        assert_eq!(lookup(Page::Vault, Lang::It, KeyCode::Char('a'), &s), Some(Verb::AddItem));
    }

    /// Whatever the file says, no two LIVE verbs ever answer the same key on a
    /// page — the invariant the whole legend rests on.
    #[test]
    fn the_resolution_never_lets_two_live_verbs_share_a_key() {
        // a spiteful file: collisions, reserved keys, junk and an "off"
        let mut s = Settings::default();
        for (verb, value) in [
            (Verb::AddItem, "e"),
            (Verb::CopyUser, "c"),
            (Verb::EditItem, "z"),
            (Verb::StarItem, "j"),
            (Verb::Reveal, "z"),
            (Verb::Memo, "off"),
            (Verb::NewVault, "x"),
            (Verb::Settings, "9"),
        ] {
            s.shortcut_overrides.insert(verb.slug().to_string(), value.to_string());
        }
        for page in [Page::Access, Page::Vault] {
            for lang in Lang::ALL {
                let mut seen: Vec<(char, Verb)> = Vec::new();
                for b in effective_binds(page, &s) {
                    let k = b.key(lang);
                    if let Some((_, other)) = seen.iter().find(|(c, _)| *c == k) {
                        panic!("{page:?}/{lang:?}: '{k}' is both {other:?} and {:?}", b.verb);
                    }
                    seen.push((k, b.verb));
                    assert_eq!(lookup(page, lang, KeyCode::Char(k), &s), Some(b.verb));
                }
            }
        }
        // `Settings` lives on both pages: its chosen key is the same on each
        for page in [Page::Access, Page::Vault] {
            assert_eq!(lookup(page, Lang::En, KeyCode::Char('9'), &s), Some(Verb::Settings));
        }
    }

    /// The row can only offer what the resolution would actually grant: every
    /// candidate, applied, comes back as itself.
    #[test]
    fn the_cycle_never_offers_a_character_that_would_collide() {
        let base = with(Verb::EditItem, "z"); // one override already in play
        for &verb in Verb::ALL.iter() {
            let candidates = shortcut_candidates(verb, &base);
            assert_eq!(candidates.first(), Some(&ShortcutChoice::Default));
            assert_eq!(candidates.last(), Some(&ShortcutChoice::Off));
            for choice in &candidates {
                let ShortcutChoice::Key(c) = choice else { continue };
                assert!(!RESERVED.contains(c), "{verb:?} was offered the reserved '{c}'");
                let mut s = base.clone();
                choice.store(verb, &mut s);
                let e = effective(verb, &s);
                assert!(!e.refused, "{verb:?} was offered '{c}', which the resolution refuses");
                assert_eq!((e.en, e.it), (*c, *c));
                assert_eq!(ShortcutChoice::stored(verb, &s), *choice);
            }
        }
    }

    /// The gap the old `taken` list had: a verb LATER in [`Verb::ALL`] that has
    /// moved off its default frees that character, but the walk has not reached
    /// it yet — so `resolve` still holds it while an EARLIER verb is judged.
    /// Offering it there dead-ended the row (the value snapped back to the
    /// default, and the forward cycle became a closed loop that could never
    /// reach the far end of the list, "off" included).
    #[test]
    fn a_character_a_later_verb_only_vacates_later_is_not_offered() {
        // Reveal (index 11) moves off `r`; CopyPassword (index 5) is judged first
        let s = with(Verb::Reveal, "z");
        assert!(refused_overrides(&s).is_empty(), "(the base override is granted)");
        let offered = shortcut_candidates(Verb::CopyPassword, &s);
        assert!(
            !offered.contains(&ShortcutChoice::Key('r')),
            "`r` is free by the time Reveal is resolved, never before it"
        );
        // …nor the character Reveal is standing on, which taking would refuse IT
        assert!(!offered.contains(&ShortcutChoice::Key('z')), "'z' belongs to Reveal now");
        // the far end of the list is reachable: the cycle is not a closed loop
        assert!(offered.contains(&ShortcutChoice::Key('x')), "{offered:?}");
        assert_eq!(offered.last(), Some(&ShortcutChoice::Off));
        // and NOTHING offered here refuses anything, this verb or another one
        for choice in &offered {
            let ShortcutChoice::Key(c) = choice else { continue };
            let mut probe = s.clone();
            choice.store(Verb::CopyPassword, &mut probe);
            assert!(refused_overrides(&probe).is_empty(), "'{c}' cost somebody their key");
            assert_eq!(ShortcutChoice::stored(Verb::CopyPassword, &probe), *choice);
        }
    }

    /// The yes key of a delete confirm is answered in `on_key` BEFORE the keymap
    /// is consulted, so a verb parked on it does not merely go dead — it
    /// DELETES. Every locale's own must be reserved, not only English's.
    #[test]
    fn every_locale_confirm_key_is_reserved_from_the_overrides() {
        for lang in Lang::ALL {
            let c = lang.confirm_key();
            assert!(RESERVED.contains(&c), "{lang:?} confirms with '{c}', which an override could take");
            // the reachable case: `s` is only kept out of the vault page by
            // StarItem's default, so switch that off and ask again.
            let free = with(Verb::StarItem, SHORTCUT_OFF);
            for &verb in Verb::ALL.iter() {
                assert!(
                    !shortcut_candidates(verb, &free).contains(&ShortcutChoice::Key(c)),
                    "{verb:?} is offered the {lang:?} confirm key '{c}'"
                );
                // …and a hand-edited file asking for it is refused, not granted
                let s = with(verb, &c.to_string());
                assert_eq!(refused_overrides(&s), vec![verb], "'{c}' was granted to {verb:?}");
            }
        }
    }

    /// Cycling a row is a round trip: the default is stored as no entry at all,
    /// so a verb left alone keeps following the table.
    #[test]
    fn the_default_is_stored_as_the_absence_of_an_entry() {
        let mut s = Settings::default();
        ShortcutChoice::Key('7').store(Verb::AddItem, &mut s);
        assert_eq!(s.shortcut_overrides.get("add_item").map(String::as_str), Some("7"));
        ShortcutChoice::Off.store(Verb::AddItem, &mut s);
        assert_eq!(s.shortcut_overrides.get("add_item").map(String::as_str), Some("off"));
        ShortcutChoice::Default.store(Verb::AddItem, &mut s);
        assert!(s.shortcut_overrides.is_empty());
        assert_eq!(ShortcutChoice::stored(Verb::AddItem, &s), ShortcutChoice::Default);
    }
}
