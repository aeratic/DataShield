//! TUI application state and input handling (sketch 2).
//!
//! The right-hand `~` panel is a *contextual view* of the current page: by
//! default it shows the page help (the command palette), and some commands
//! open an interactive view inside it (currently `language`). The left panel
//! is the page navigation (the vault list on the Access page).
//!
//! Which commands exist, where they're usable, and what they do is driven by
//! the [`Registry`] loaded from `commands.toml` — see `registry.rs`.

#[allow(unused_imports)]
use crate::content_ext::VaultContentExt;

use crate::chrome::Chrome;
use crate::crypto::{Algorithm, KdfSpec};
use crate::i18n::{self, Key, Lang};
use crate::paths::AppPaths;
use crate::registry::{Action, CommandDef, Registry};
use crate::settings::{Background, Blur, Settings};
use crate::settings_ui::SettingsState;
use crate::theme::Palette;
use crate::vault::{self, UnlockError, Vault};
use ratatui::crossterm::event::{KeyCode, KeyModifiers};
use std::path::{Path, PathBuf};
use zeroize::{Zeroize, Zeroizing};

/// A filesystem-safe default file name for an exported vault: the username with
/// anything but ASCII alphanumerics / `-` / `_` replaced, plus `.vault`.
fn export_file_name(username: &str) -> String {
    let safe: String = username
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let stem = if safe.is_empty() { "vault" } else { &safe };
    format!("{stem}.vault")
}

/// Expands a leading `~` / `~/` to the home directory; trims whitespace.
fn resolve_path(input: &str) -> PathBuf {
    let trimmed = input.trim();
    if let Some(rest) = trimmed.strip_prefix('~') {
        if rest.is_empty() || rest.starts_with('/') {
            if let Some(dirs) = directories::UserDirs::new() {
                let home = dirs.home_dir();
                let rest = rest.strip_prefix('/').unwrap_or(rest);
                return if rest.is_empty() { home.to_path_buf() } else { home.join(rest) };
            }
        }
    }
    PathBuf::from(trimmed)
}

/// Best-effort owner-only (0600) permissions for an exported file on Unix.
fn set_private_perms(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// The page shown in the left panel. Referenced by name (`key`) in
/// `commands.toml`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Page {
    Access,
    Vault,
}

impl Page {
    pub fn key(self) -> &'static str {
        match self {
            Page::Access => "access",
            Page::Vault => "vault",
        }
    }

    pub fn from_key(s: &str) -> Option<Page> {
        match s.to_ascii_lowercase().as_str() {
            "access" => Some(Page::Access),
            "vault" => Some(Page::Vault),
            _ => None,
        }
    }
}

/// Which panel the arrow keys and Enter act on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Vaults,
    Panel,
}

/// The meaning of a status-bar message, so it can be coloured accordingly.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StatusKind {
    Info,
    Success,
    Error,
}

/// A closed-set chooser shown in the `~` panel (language, `~` direction, …).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PickerKind {
    Language,
    PanelHistory,
    /// Step 1 of `add`: choose the category of the new item.
    ItemCategory,
}

/// Prefix of every render-cache key holding a decoded ITEM picture (as opposed
/// to a public favicon) — see [`App::forget_item_images`].
pub const IMAGE_KEY_PREFIX: &str = "imgfield|";

/// The same, for a photo sent to the Memorandum. A DISTINCT prefix on purpose:
/// a memo message and an item are both keyed by a UUID, and one namespace would
/// let a message hand its decode to an item that happens to share an id.
pub const MEMO_IMAGE_KEY_PREFIX: &str = "memoimg|";

/// Rows one PgUp/PgDn moves the Memorandum. A fixed step, not the viewport's
/// height: the app never learns the terminal size (the renderer owns geometry).
/// How far back the thread GOES is the renderer's answer too — it hands the
/// last laid-out bound back through [`App::memo_max_scroll`], which is what
/// keeps the offset from running past the top of the thread.
const MEMO_SCROLL_STEP: usize = 10;

/// Ticks between two samples of the model cache while it is being downloaded
/// (see [`crate::rag::RagState::sample_download`]): 8 × 120 ms ≈ 1 s, which is
/// as often as a progress bar over half a gigabyte has anything new to say.
pub const RAG_SAMPLE_TICKS: u64 = 8;

/// Ticks between two sweeps for expired API sessions (≈2 s). The request path
/// reaps on every call already, so this exists for the session nobody asks
/// about again: without it an abandoned token would keep a decrypted vault in
/// memory until the app quit. A walk of at most eight `Instant`s — the tick
/// cannot feel it, and a second's slack on an idle timeout costs nothing.
pub const API_REAP_TICKS: u64 = 16;

/// Longest results list a Memorandum search shows. A search inside one
/// conversation is "which message was it?", not a ranking exercise: past a
/// handful of rows the list stops being readable and the answer is a better
/// query.
pub const MEMO_SEARCH_HITS: usize = 8;

/// The `HH:MM` of a stored RFC 3339 stamp, for a results row — on the USER'S
/// clock, `offset` seconds east of Greenwich (see [`crate::model::wall_clock`]):
/// the stamps are UTC on disk, and printing those raw shows a note written at
/// 00:30 as "22:30". A stamp this build didn't write keeps its raw columns —
/// never a panic (`get` on a byte range answers `None` on a boundary it cannot
/// cut).
fn memo_hhmm(ts: &str, offset: i32) -> String {
    crate::model::wall_clock(ts, offset)
        .map(|(_, time)| time)
        .unwrap_or_else(|| ts.get(11..16).unwrap_or_default().to_string())
}

/// Which search a query box runs — the Memorandum's Ctrl+F bar and the item
/// list's `/` box alike. The two are different tools, not two settings of one
/// (see [`crate::rag`]): the literal one needs nothing and finds the words you
/// typed, the semantic one needs the on-device model and finds the thing you
/// meant. Tab flips between them, wherever the box is.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SearchMode {
    /// Every box opens on this one, whatever the engine's state is: it always
    /// works, costs nothing and needs no model — the one mode that can never
    /// disappoint is the one to start in.
    #[default]
    Literal,
    Rag,
}

impl SearchMode {
    /// The other one — what Tab moves to.
    fn flipped(self) -> SearchMode {
        match self {
            SearchMode::Literal => SearchMode::Rag,
            SearchMode::Rag => SearchMode::Literal,
        }
    }
}

/// One row of the results list: which message it points at, its stamp, the
/// words to show for it, and — for a semantic hit only — the similarity that
/// ranked it. A literal hit's score orders the list and means nothing on
/// screen, so it is not shown.
pub struct MemoHit {
    pub id: String,
    /// `HH:MM` as stored (empty when the stamp isn't that shape).
    pub time: String,
    pub snippet: String,
    pub score: Option<f32>,
}

impl Drop for MemoHit {
    fn drop(&mut self) {
        // the snippet is vault text, cut down; it leaves no copy behind either
        self.snippet.zeroize();
    }
}

/// The Memorandum's search bar — Ctrl+F over the composer, with the chat still
/// on screen behind it. Everything here is vault-shaped (a query is what you
/// are looking for INSIDE the vault), so the whole thing goes when the memo
/// closes or the vault locks.
pub struct MemoSearch {
    pub query: crate::textinput::TextInput,
    /// The mode the next ⏎ will ASK for; Tab flips it.
    pub mode: SearchMode,
    /// The mode that actually RAN. `Some` exactly while `hits` describes a
    /// finished search — which is what makes the chip honest when a RAG search
    /// had to fall back to the literal one.
    pub ran: Option<SearchMode>,
    pub hits: Vec<MemoHit>,
    pub selected: usize,
    /// The sequence number of the query embedding still out on the worker (see
    /// [`crate::rag::RagState::start_query`]) — the "searching…" state.
    pub pending: Option<u64>,
    /// True while the thread is being caught up so the semantic search can see
    /// the messages written since the last pass. It is the OTHER half of
    /// "searching…": the query goes out on the tick that pass lands.
    pub indexing: bool,
}

impl MemoSearch {
    /// A fresh bar. It opens LITERAL whatever the engine's state is: that
    /// search always works, costs nothing and needs no model, so the one mode
    /// that can never disappoint is the one the bar starts in.
    fn new() -> MemoSearch {
        MemoSearch {
            query: crate::textinput::TextInput::new(),
            mode: SearchMode::Literal,
            ran: None,
            hits: Vec::new(),
            selected: 0,
            pending: None,
            indexing: false,
        }
    }

    /// The mode the chip shows: what ran, or — before anything has — what the
    /// next ⏎ will ask for.
    pub fn chip_mode(&self) -> SearchMode {
        self.ran.unwrap_or(self.mode)
    }

    pub fn searching(&self) -> bool {
        self.pending.is_some() || self.indexing
    }

    /// Drops the results of the last run. Every edit to the query and every
    /// flip of the mode calls this: a list that no longer answers what the bar
    /// says would make the chip lie about what produced it.
    fn invalidate(&mut self) {
        self.hits.clear();
        self.selected = 0;
        self.ran = None;
        self.pending = None;
        self.indexing = false;
    }

    /// Wipes the bar's text before it is dropped.
    fn wipe(&mut self) {
        self.query.zeroize();
        self.query.clear();
        self.hits.clear();
    }
}

/// The item list's `/` box, beside [`App::vault_query`] — the same two searches
/// the Memorandum's bar offers, over the items instead of the thread.
///
/// The literal one needs nothing stored: it is [`App::item_matches`], recomputed
/// by the filter on every frame. The semantic one is what lives here: a query
/// embedded on the worker, ranked against the `x_rag` vectors, and kept as a
/// ranking that REPLACES the filter until the query is cleared.
#[derive(Default)]
pub struct VaultSearch {
    /// The mode the box is ASKING for; Tab flips it.
    pub mode: SearchMode,
    /// The mode that actually RAN, which is what the chip shows — so a semantic
    /// search that had to fall back advertises "literal", like the memo's.
    pub ran: Option<SearchMode>,
    /// The last finished ranking, `(item id, score)`, best first. Items with no
    /// vector yet are simply not in it (they rank last, without a badge).
    pub ranked: Vec<(String, f32)>,
    /// The query the ranking — or the embedding still out — was asked for. An
    /// edit makes it differ from `vault_query`, which is what re-runs the
    /// search once the worker is free again (the seq-tag IS the debounce: one
    /// query in flight at a time, and the last thing typed wins).
    pub asked: String,
    /// The sequence number of the embedding out on the worker (see
    /// [`crate::rag::RagState::start_query`]) — the "searching" state.
    pub pending: Option<u64>,
    /// Whether the reason a RAG search ran literally has already been said for
    /// this box. A toast per keystroke would be noise; the chip keeps saying it
    /// silently for as long as it lasts.
    pub told: bool,
}

impl VaultSearch {
    /// The mode the chip shows: what ran, or — before anything has — what the
    /// box is asking for.
    pub fn chip_mode(&self) -> SearchMode {
        self.ran.unwrap_or(self.mode)
    }

    /// True while a query embedding is out for this box.
    pub fn searching(&self) -> bool {
        self.pending.is_some()
    }

    /// Drops the last run: the ranking, what it was asked for, and the answer
    /// still coming for it. A ranking left standing under a chip that no longer
    /// describes it is exactly the lie the chip exists to prevent.
    fn invalidate(&mut self) {
        self.ran = None;
        self.ranked.clear();
        self.asked.zeroize();
        self.asked.clear();
        self.pending = None;
    }

    /// Back to a fresh box, its text wiped — a query names what is INSIDE the
    /// vault, so it goes when the vault does.
    fn wipe(&mut self) {
        self.invalidate();
        self.mode = SearchMode::Literal;
        self.told = false;
    }
}

/// The category picker's "+ new category" pseudo-choice (fig-05): picking it
/// opens the custom-category form instead of an add form.
pub const NEW_CATEGORY: &str = "__new_category__";

impl PickerKind {
    fn from_action(action: Action) -> Option<PickerKind> {
        match action {
            Action::Language => Some(PickerKind::Language),
            Action::Panel => Some(PickerKind::PanelHistory),
            Action::AddItem => Some(PickerKind::ItemCategory),
            _ => None,
        }
    }

    fn action(self) -> Action {
        match self {
            PickerKind::Language => Action::Language,
            PickerKind::PanelHistory => Action::Panel,
            PickerKind::ItemCategory => Action::AddItem,
        }
    }
}

/// One selectable option in a picker.
pub struct Choice {
    pub value: String,
    pub label: String,
}

/// What the `~` panel currently shows.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum PanelView {
    /// The resting view: nothing layered — the access list, or the vault list
    /// with its always-docked live detail.
    Splash,
    /// A closed-set chooser (language, `~` direction, add-category), floating.
    Picker(PickerKind),
    /// The vault's (or one item's) non-secret encryption metadata, docked.
    EncInfo(Option<String>),
}

/// One field in a command form: a free-text input (optionally secret) or a
/// choice cycled through a closed set of options. No `Debug`/`Clone` on
/// purpose (a field may hold a secret); its value is wiped from memory on drop.
pub struct FormField {
    pub name: String,
    pub description: String,
    pub secret: bool,
    pub value: crate::textinput::TextInput,
    /// Non-empty makes this a choice field (cycled with ←/→).
    pub options: Vec<String>,
    pub selected: usize,
    /// Set once the user types into this field, so a pre-filled default (e.g.
    /// the export path) can stop auto-updating once it's been edited.
    pub dirty: bool,
    /// A `+ add field` row: typing into it NAMES a new custom field. As soon as
    /// it holds text a fresh `+ add field` row appears below it, so the list
    /// grows for as long as you keep adding (no fixed slots).
    pub adder: bool,
    /// Whether the live zxcvbn meter rides this row while it is typed. Only a
    /// PASSWORD earns it (see [`shows_strength`]): a CVV, PIN, card number or
    /// TOTP seed is masked too, but has no "strength" to score. Never set on a
    /// non-secret field.
    pub show_strength: bool,
}

impl FormField {
    pub fn text(name: &str, description: String, secret: bool) -> FormField {
        FormField {
            name: name.to_string(),
            description,
            secret,
            value: crate::textinput::TextInput::new(),
            options: Vec::new(),
            selected: 0,
            dirty: false,
            adder: false,
            show_strength: false,
        }
    }

    /// A master-passphrase field: secret, and the one non-item row that carries
    /// the strength meter (create / unlock / re-key all judge a passphrase).
    pub fn password(name: &str, description: String) -> FormField {
        let mut f = FormField::text(name, description, true);
        f.show_strength = true;
        f
    }

    pub fn choice(name: &str, description: String, options: Vec<String>, selected: usize) -> FormField {
        FormField {
            name: name.to_string(),
            description,
            secret: false,
            value: crate::textinput::TextInput::new(),
            options,
            selected,
            dirty: false,
            adder: false,
            show_strength: false,
        }
    }

    /// The growing `+ add field` row (see [`FormField::adder`]).
    pub fn adder(lang: Lang) -> FormField {
        let (name, hint) = match lang {
            Lang::En => ("add field", "type a name to add a custom field"),
            Lang::It => ("aggiungi campo", "scrivi un nome per aggiungere un campo"),
        };
        FormField {
            name: name.to_string(),
            description: hint.to_string(),
            secret: false,
            value: crate::textinput::TextInput::new(),
            options: Vec::new(),
            selected: 0,
            dirty: false,
            adder: true,
            show_strength: false,
        }
    }

    pub fn is_choice(&self) -> bool {
        !self.options.is_empty()
    }

    /// The current value: the picked option for a choice, else the typed text.
    pub fn current(&self) -> &str {
        if self.is_choice() {
            self.options.get(self.selected).map(String::as_str).unwrap_or("")
        } else {
            self.value.as_str()
        }
    }

    fn cycle(&mut self, delta: isize) {
        let n = self.options.len();
        if n > 0 {
            self.selected = (self.selected as isize + delta).rem_euclid(n as isize) as usize;
        }
    }
}

impl Drop for FormField {
    fn drop(&mut self) {
        self.value.zeroize();
    }
}

/// What a form does when submitted.
#[allow(clippy::enum_variant_names)]
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FormPurpose {
    CreateVault,
    /// Delete a vault (vault chooser + password).
    DeleteVault,
    /// Empty a vault's contents (vault chooser + password).
    NukeVault,
    /// Export a vault to a file (vault chooser + destination path).
    ExportVault,
    /// Configure this vault's sync endpoint (host/user/dir/key/auto).
    SyncSetup(String),
    /// Import a vault from a file (source path + password).
    ImportVault,
    /// Add an item of the given category to the open vault.
    AddItem(String),
    /// Delete an item: chooser + default-No confirm. Carries the ids (UUIDs,
    /// non-secret) parallel to the chooser options so a duplicate label can
    /// never delete the wrong item.
    /// Edit the item with this id (fields pre-filled from the item itself).
    EditItem(String),
    /// Create a custom category (name + repeated field-spec groups).
    CreateCategory,
    /// Re-key + re-encrypt the open vault (password + algorithm/KDF choices).
    RerollEnc,
    /// Wipe every item of the OPEN vault (password + typed name + confirm).
    NukeItems,
    /// Change this vault's master password (current + new + confirm). The
    /// re-encryption runs on a worker thread, so the form closes on submit and
    /// even a wrong current password only surfaces later.
    ChangePassword(String),
    /// Attach a photo to the Memorandum (file path + an optional caption).
    MemoPhoto,
}

/// The interactive form of a command with free-text parameters. When present
/// on [`App`], the app is in "form mode": keystrokes edit the focused field.
pub struct FormState {
    pub command: String,
    pub title: String,
    pub purpose: FormPurpose,
    pub fields: Vec<FormField>,
    pub focused: usize,
    pub error: Option<String>,
}

impl FormState {
    /// Builds a form for `cmd` (one field per input parameter). Non-secret
    /// text fields are pre-filled from `args` in order. For createVault the
    /// encryption choice fields (algorithm + KDF) are appended.
    fn from_command(cmd: &CommandDef, lang: Lang, args: &[&str]) -> Option<FormState> {
        if !cmd.has_input_form() {
            return None;
        }
        let mut fields = Vec::new();
        let mut args = args.iter();
        for p in cmd.params.iter().filter(|p| p.kind == crate::registry::ParamKind::Input) {
            let mut field = FormField::text(&p.name, p.description(lang).to_string(), p.secret);
            // a secret parameter earns the meter only when its name reads as a
            // password (the same inference custom item fields use)
            field.show_strength = shows_strength(p.secret, &implied_ftype(&p.name));
            // never pre-fill secrets from the (cleartext) command line
            if !p.secret {
                field.value.set(args.next().map(|s| s.to_string()).unwrap_or_default());
            }
            fields.push(field);
        }
        if cmd.action == Action::CreateVault {
            fields.push(FormField::choice(
                "algorithm",
                i18n::t(lang, &Key::AlgorithmHint),
                Algorithm::ALL.iter().map(|a| a.name().to_string()).collect(),
                0,
            ));
            fields.push(FormField::choice(
                "kdf",
                i18n::t(lang, &Key::KdfHint),
                KdfSpec::ALL.iter().map(|k| k.name().to_string()).collect(),
                0,
            ));
        }
        let focused = fields
            .iter()
            .position(|f| !f.is_choice() && f.value.is_empty())
            .unwrap_or(0);
        Some(FormState {
            command: cmd.name.clone(),
            title: cmd.description(lang).to_string(),
            purpose: FormPurpose::CreateVault,
            fields,
            focused,
            error: None,
        })
    }

    /// A vault-management form (delete / nuke): a vault chooser + password.
    fn manage(purpose: FormPurpose, vaults: &[String], selected: usize, lang: Lang) -> FormState {
        let (command, title) = match purpose {
            FormPurpose::NukeVault => ("nukeVault", i18n::t(lang, &Key::NukeTitle)),
            _ => ("deleteVault", i18n::t(lang, &Key::DeleteTitle)),
        };
        FormState {
            command: command.to_string(),
            title,
            purpose,
            fields: vec![
                FormField::choice(
                    "vault",
                    i18n::t(lang, &Key::VaultChoiceHint),
                    vaults.to_vec(),
                    selected.min(vaults.len().saturating_sub(1)),
                ),
                FormField::password("password", i18n::t(lang, &Key::UnlockHint)),
            ],
            // start on the password field (the vault is pre-chosen)
            focused: 1,
            error: None,
        }
    }

    /// Export form: a vault chooser + a destination path (pre-filled with a
    /// real, ready-to-use path).
    fn export(vaults: &[String], selected: usize, default_path: String, lang: Lang) -> FormState {
        let mut dest = FormField::text("destination", i18n::t(lang, &Key::ExportPathHint), false);
        dest.value.set(default_path);
        FormState {
            command: "exportVault".to_string(),
            title: i18n::t(lang, &Key::ExportTitle),
            purpose: FormPurpose::ExportVault,
            fields: vec![
                FormField::choice(
                    "vault",
                    i18n::t(lang, &Key::VaultChoiceHint),
                    vaults.to_vec(),
                    selected.min(vaults.len().saturating_sub(1)),
                ),
                dest,
            ],
            // the vault is pre-chosen; start on the path the user may tweak
            focused: 1,
            error: None,
        }
    }

    /// Sync-setup form (first time only): the VPS endpoint. No password — sync
    /// carries already-encrypted bytes; auth is your SSH key. Prefilled from the
    /// existing sidecar when re-editing.
    fn sync_setup(name: &str, existing: Option<crate::sync::SyncConfig>, lang: Lang) -> FormState {
        let cfg = existing.unwrap_or_default();
        let mut host = FormField::text("host", i18n::t(lang, &Key::SyncHostHint), false);
        host.value.set(cfg.host);
        let mut user = FormField::text("user", i18n::t(lang, &Key::SyncUserHint), false);
        user.value.set(cfg.user);
        let mut dir = FormField::text("remote_dir", i18n::t(lang, &Key::SyncDirHint), false);
        dir.value.set(if cfg.remote_dir.is_empty() { "~/.datashield".to_string() } else { cfg.remote_dir });
        let mut key = FormField::text("key_path", i18n::t(lang, &Key::SyncKeyHint), false);
        key.value.set(cfg.key_path);
        let auto = FormField::choice(
            "auto_sync",
            i18n::t(lang, &Key::SyncAutoHint),
            vec![i18n::t(lang, &Key::ToggleOff), i18n::t(lang, &Key::ToggleOn)],
            cfg.auto as usize,
        );
        FormState {
            command: format!("sync {name}"),
            title: i18n::t(lang, &Key::SyncTitle),
            purpose: FormPurpose::SyncSetup(name.to_string()),
            fields: vec![host, user, dir, key, auto],
            focused: 0,
            error: None,
        }
    }

    /// Attach-photo form: the picture's path and a caption to go under it. The
    /// path row is never masked (a path is not a secret) and the caption may be
    /// left blank, so this form validates itself — see `submit_memo_photo`.
    fn memo_photo(lang: Lang) -> FormState {
        let photo = FormField::text(
            &i18n::t(lang, &Key::MemoPhotoField),
            i18n::t(lang, &Key::ImageFieldHint),
            false,
        );
        let caption = FormField::text(
            &i18n::t(lang, &Key::MemoCaptionField),
            i18n::t(lang, &Key::MemoCaptionHint),
            false,
        );
        FormState {
            command: "memo".to_string(),
            title: i18n::t(lang, &Key::MemoPhotoTitle),
            purpose: FormPurpose::MemoPhoto,
            fields: vec![photo, caption],
            focused: 0,
            error: None,
        }
    }

    /// Import form: a source path + the vault's password (needed to decrypt and
    /// verify it before it is homed into the data directory).
    fn import(source_prefill: String, lang: Lang) -> FormState {
        let mut src = FormField::text("source", i18n::t(lang, &Key::ImportPathHint), false);
        src.value.set(source_prefill);
        FormState {
            command: "importVault".to_string(),
            title: i18n::t(lang, &Key::ImportTitle),
            purpose: FormPurpose::ImportVault,
            fields: vec![
                src,
                FormField::password("password", i18n::t(lang, &Key::ImportPasswordHint)),
            ],
            focused: 0,
            error: None,
        }
    }

    /// Reroll form: the master password + algorithm/KDF choices defaulting to the
    /// vault's current primitives (so fill-password-and-Enter is a pure re-key).
    fn reroll(current_algo: Algorithm, current_kdf: KdfSpec, lang: Lang) -> FormState {
        let algo_idx = Algorithm::ALL.iter().position(|a| *a == current_algo).unwrap_or(0);
        let kdf_idx = KdfSpec::ALL.iter().position(|k| k.name() == current_kdf.name()).unwrap_or(0);
        FormState {
            command: "rerollEnc".to_string(),
            title: i18n::t(lang, &Key::RerollTitle),
            purpose: FormPurpose::RerollEnc,
            fields: vec![
                FormField::password("password", i18n::t(lang, &Key::UnlockHint)),
                FormField::choice(
                    "algorithm",
                    i18n::t(lang, &Key::AlgorithmHint),
                    Algorithm::ALL.iter().map(|a| a.name().to_string()).collect(),
                    algo_idx,
                ),
                FormField::choice(
                    "kdf",
                    i18n::t(lang, &Key::KdfHint),
                    KdfSpec::ALL.iter().map(|k| k.name().to_string()).collect(),
                    kdf_idx,
                ),
            ],
            focused: 0,
            error: None,
        }
    }

    /// Change-password form: the current passphrase plus the new one twice. No
    /// vault chooser — the command already picked the vault, and its name rides
    /// in the purpose so the worker knows which file to re-key.
    fn change_password(name: &str, lang: Lang) -> FormState {
        // only the NEW passphrase earns the meter: the current one already is
        // what it is, and scoring the confirm row twice is just noise.
        let mut current = FormField::password("current", i18n::t(lang, &Key::PasswdCurrentHint));
        current.show_strength = false;
        let mut confirm = FormField::password("confirm", i18n::t(lang, &Key::PasswdConfirmHint));
        confirm.show_strength = false;
        FormState {
            command: format!("passwd {name}"),
            title: i18n::t(lang, &Key::PasswdTitle(name)),
            purpose: FormPurpose::ChangePassword(name.to_string()),
            fields: vec![
                current,
                FormField::password("new", i18n::t(lang, &Key::PasswdNewHint)),
                confirm,
            ],
            focused: 0,
            error: None,
        }
    }

    /// In-vault nuke form: the master password + the exact vault name + a
    /// default-No confirm (all validated together on submit).
    fn nuke_items(lang: Lang) -> FormState {
        FormState {
            command: "nuke".to_string(),
            title: i18n::t(lang, &Key::NukeItemsTitle),
            purpose: FormPurpose::NukeItems,
            fields: vec![
                FormField::password("password", i18n::t(lang, &Key::UnlockHint)),
                FormField::text("vault_name", i18n::t(lang, &Key::NukeItemsNameHint), false),
                confirm_field(lang, "confirm", &Key::NukeItemsConfirmHint),
            ],
            focused: 0,
            error: None,
        }
    }

    /// Create-category form: a name, then `CATEGORY_SLOTS` field-spec groups of
    /// three (name / type / secret).
    /// The create-category form: the category name, then a GROWING `+ add
    /// field` list — no fixed slots. Every custom category also always gets a
    /// `website` field (it feeds the favicon) and items always have a name
    /// (the label), so neither needs a row here. A field's type/secret are
    /// inferred from its name (`password`, `pin`, `url`, …).
    fn category(name_prefill: Option<&str>, lang: Lang) -> FormState {
        let mut name = FormField::text("name", i18n::t(lang, &Key::CategoryNameHint), false);
        name.value.set(name_prefill.unwrap_or("").to_string());
        FormState {
            command: "category".to_string(),
            title: i18n::t(lang, &Key::CategoryTitle),
            purpose: FormPurpose::CreateCategory,
            fields: vec![name, FormField::adder(lang)],
            focused: 0,
            error: None,
        }
    }
}

/// Whether a custom field NAME implies a secret value (masked, zeroized,
/// never searched): password/secret/key/pin/cvv/token/otp — EN and IT.
fn implied_secret(name: &str) -> bool {
    let n = name.to_lowercase();
    ["pass", "secret", "segret", "key", "chiave", "pin", "cvv", "token", "otp", "2fa", "seed"]
        .iter()
        .any(|w| n.contains(w))
}

/// Whether a custom field NAME reads as a picture slot — EN and IT. Kept apart
/// from [`implied_secret`] so it can be asked without recursing through
/// [`implied_ftype`].
fn implied_image(name: &str) -> bool {
    let n = name.to_lowercase();
    ["photo", "foto", "image", "immagine", "img"].iter().any(|w| n.contains(w))
}

/// A best-effort DCS-1 type for a custom field, from its name.
///
/// Only a name that reads as a CHOSEN passphrase becomes `Password`: the stored
/// type is what the strength meter reads (see [`shows_strength`]), so typing a
/// cvv, an api key or a one-time-code seed as a password would both lie in the
/// file and hang a meaningless score off it. A cvv/pin gets its own `Pin`; the
/// rest stay plain `Text` — they are still MASKED, because masking comes from
/// `secret` ([`implied_secret`]), never from the type.
fn implied_ftype(name: &str) -> crate::model::FieldType {
    use crate::model::FieldType;
    let n = name.to_lowercase();
    if implied_image(&n) {
        FieldType::Image
    } else if n.contains("url") || n.contains("website") || n.contains("sito") {
        FieldType::Url
    } else if n.contains("mail") {
        FieldType::Email
    } else if n.contains("user") || n.contains("utente") {
        FieldType::Username
    } else if n.contains("cvv") || n.contains("pin") {
        FieldType::Pin
    } else if n.contains("totp") {
        // `totp` in the NAME already drives the live-code row (see
        // `clipboard::is_totp_field`); the type just says so out loud. A
        // vaguer `otp`/`2fa`/`seed` is left alone on purpose — a recovery
        // phrase stored there must never be turned into a 6-digit code.
        FieldType::Totp
    } else if n.contains("pass") || n.contains("secret") || n.contains("segret") {
        FieldType::Password
    } else {
        FieldType::Text
    }
}

/// Whether a masked row earns the live zxcvbn meter: ONLY a field typed
/// [`FieldType::Password`](crate::model::FieldType::Password). A CVV, PIN, card
/// number, TOTP seed or any other masked value is not a chosen secret — a
/// strength score on it is meaningless noise, so it gets no meter (fig-03/06).
fn shows_strength(secret: bool, ftype: &crate::model::FieldType) -> bool {
    secret && *ftype == crate::model::FieldType::Password
}

/// Whether a form row of this type takes a FILE PATH instead of the value
/// itself. A path is not a secret, so such a row is never masked while typing —
/// the field it feeds is stored secret all the same (`FieldType::Image`).
fn takes_a_path(ftype: &crate::model::FieldType) -> bool {
    *ftype == crate::model::FieldType::Image
}

/// The one-character "take this off the item" token for a row that takes a FILE
/// PATH. A picture row inherits the secret rows' contract — confirming it blank
/// KEEPS what is stored — so blank cannot also mean "remove", and removal needs
/// a word of its own. `-` is it: no importable path is one character of dash, so
/// nothing legitimate is shadowed.
const REMOVE_MARKER: &str = "-";

/// Whether `typed` asks for this row's stored value to be dropped. Only a path
/// row can be removed this way: everywhere else `-` is simply a value one
/// character long (a password may perfectly well be `-`), and a non-secret text
/// row is already cleared by emptying it.
fn removes_the_value(ftype: &crate::model::FieldType, typed: &str) -> bool {
    takes_a_path(ftype) && typed.trim() == REMOVE_MARKER
}

/// What a form row is STORED as: `(value, extra)`. Everything but an image
/// stores the text as typed; an image row's text is a path, so the file is
/// read, size/format-checked and base64-encoded here, with its `x_image`
/// metadata. `Err` is the localized message for the form's error line.
fn stored_from_form(
    lang: Lang,
    ftype: &crate::model::FieldType,
    typed: &str,
) -> Result<(String, serde_json::Map<String, serde_json::Value>), String> {
    if !takes_a_path(ftype) {
        return Ok((typed.to_string(), serde_json::Map::new()));
    }
    match crate::image_field::import(&resolve_path(typed)) {
        Ok(image) => {
            let mut extra = serde_json::Map::new();
            extra.insert(crate::image_field::META_KEY.to_string(), image.meta());
            Ok((image.b64, extra))
        }
        Err(e) => Err(e.message(lang, typed)),
    }
}

/// A `[No, Yes]` choice field defaulting to No.
fn confirm_field(lang: Lang, name: &str, hint: &Key) -> FormField {
    FormField::choice(
        name,
        i18n::t(lang, hint),
        vec![i18n::t(lang, &Key::ConfirmNo), i18n::t(lang, &Key::ConfirmYes)],
        0,
    )
}

/// One slot of the Edit form, aligned 1:1 with `form.fields[1..]`.
enum EditSlot {
    /// The item's own field at this index.
    Existing(usize),
    /// A category-template slot the item lacks (key, type, secret).
    New(String, crate::model::FieldType, bool),
}

/// A passphrase prompt bound to one vault: the inline unlock on the access
/// list and the full-screen lock share this shape. The typed passphrase is
/// wiped (zeroize) on submit, close and drop.
pub struct PassPrompt {
    /// The vault this prompt unlocks.
    pub name: String,
    /// The passphrase being typed (masked on screen; never rendered as text).
    pub input: crate::textinput::TextInput,
    /// Failed attempts so far (`✗ wrong passphrase — attempt N of 5`).
    pub attempts: u32,
    /// Whether the last attempt failed (shows the ✗ line).
    pub error: bool,
}

impl PassPrompt {
    pub fn new(name: &str) -> Self {
        PassPrompt {
            name: name.to_string(),
            input: crate::textinput::TextInput::new(),
            attempts: 0,
            error: false,
        }
    }
}

impl Drop for PassPrompt {
    fn drop(&mut self) {
        self.input.zeroize();
    }
}

/// An inline edit of ONE field, typed straight on its row in the docked detail
/// (⏎ starts it, ⏎ confirms, Esc cancels). The buffer is wiped on drop, so a
/// cancelled password never outlives the keystroke that abandoned it.
pub struct DetailEdit {
    /// Index into the item's `fields` — the row being rewritten. What the
    /// RENDERER draws the input on; never what the commit writes to (a sync
    /// pull can renumber the rows under an open edit — see `item`/`key`).
    pub field: usize,
    /// The id of the item the edit started on, and the KEY of its row. The
    /// commit resolves both again: `poll_sync` can replace the whole vault
    /// content mid-edit, and an index alone would then land the buffer on a
    /// different item — or a different field of it.
    pub item: String,
    pub key: String,
    /// What has been typed so far. A SECRET row starts empty (confirming empty
    /// keeps the stored value); a plain row starts pre-filled (caret at the end).
    pub buffer: crate::textinput::TextInput,
}

/// The field rows the docked detail shows for `item`, as indices into
/// `item.fields`. The SINGLE source shared by the renderer, the detail cursor,
/// copy and inline edit — so what the cursor points at and what a copy takes
/// can never drift apart. (Today every field is a row; a future filtered detail
/// changes this one function and the four callers follow.)
pub fn detail_field_rows(item: &crate::model::Item) -> Vec<usize> {
    (0..item.fields.len()).collect()
}

pub struct App {
    pub paths: AppPaths,
    pub registry: Registry,
    pub page: Page,
    pub vaults: Vec<String>,
    pub selected: usize,
    pub focus: Focus,
    pub view: PanelView,
    /// Browser-style history of `~` panel views (reset when the main page
    /// changes). `back`/`forward` drive the `~ back` / `~ forw` command.
    back: Vec<PanelView>,
    forward: Vec<PanelView>,
    /// Highlighted item within the current panel view.
    pub panel_selected: usize,
    pub input: String,
    /// When present, the `:` command palette is open (fuzzy command finder).
    pub palette: Option<crate::palette::Palette>,
    /// Whether the `?` key cheatsheet overlay is showing.
    pub cheatsheet: bool,
    /// A pending item-level delete confirm (`y`/`n`): the `(id, label)` of the
    /// item to remove. Rung 1 of the confirm ladder — no `◂No▸` flipper.
    pub pending_confirm: Option<(String, String)>,
    /// The inline unlock unfolded under the selected vault row (fig-01): Enter
    /// on a vault opens it — no page swap, no modal, focus never moves.
    pub unlock: Option<PassPrompt>,
    /// The full-screen lock (fig-02): `Ctrl+L` drops the open vault (its key is
    /// zeroized) and the brass rain takes the whole screen until re-unlocked.
    pub locked: Option<PassPrompt>,
    /// When present, the app is in form mode (editing this command's fields).
    pub form: Option<FormState>,
    /// The currently open vault (on the Vault page). Holds the derived key,
    /// zeroized when this is dropped (exit / app close).
    pub vault: Option<Vault>,
    /// Transient one-line feedback shown in the status bar, with its kind.
    pub status: Option<(StatusKind, String)>,
    /// Persisted appearance & language preferences. The interface language
    /// lives here (`settings.lang`) — there is no separate `App.lang`, so the
    /// `language` command and the settings page can't drift out of sync.
    pub settings: Settings,
    /// The header logo lines, loaded from the editable `logo.txt`.
    pub logo: Vec<String>,
    /// Interface chrome (prompt, panel split, brand) from the editable `ui.toml`.
    pub chrome: Chrome,
    /// When present, the tabbed settings page is open (editing a draft).
    pub settings_ui: Option<SettingsState>,
    /// The desktop's default browser, asked ONCE when the settings page opens
    /// (the `browser` row reads it every frame; the question spawns a process).
    pub detected_browser: Option<crate::browser::DefaultBrowser>,
    /// Set after a first quit-attempt when `confirm_exit` is on; a second Esc
    /// confirms. Cleared by any other key.
    pending_quit: bool,
    /// A stable-per-run device id, stamped into synced content for conflict
    /// display. (Per-run is fine — it only labels; the encrypted counter orders.)
    pub device_id: String,
    /// The per-vault sync status shown in the top-bar / access list.
    pub sync_status: std::collections::HashMap<String, crate::sync::SyncStatus>,
    /// Receiver for a sync worker thread's result (vault name + result).
    sync_rx: Option<std::sync::mpsc::Receiver<(String, crate::sync::SyncResult)>>,
    /// Receiver for the change-password worker (vault name + outcome).
    passwd_rx: Option<std::sync::mpsc::Receiver<(String, crate::vault::RekeyOutcome)>>,
    /// The vault whose re-encryption is running right now: it drives the
    /// progress bar next to that name and every "wait for it" guard. Only one
    /// at a time — a second `passwd` is refused while this is set.
    pub passwd_busy: Option<String>,
    /// Animation frame counter, advanced on every tick (cursor blink).
    pub frame: u64,
    /// Ticks since the last key press — drives the idle auto-lock and the
    /// top-bar "locks in" countdown. Reset on every key.
    pub idle_ticks: u64,
    /// Random seed reshuffled each time the idle splash is (re)entered — picks
    /// which playful effect the `~` canvas shows and seeds its animation.
    pub splash_seed: u64,
    /// While viewing an item's detail, whether its secret fields are revealed
    /// (toggled with Enter; reset whenever the view changes).
    pub detail_reveal: bool,
    /// Cursor over the docked detail's field rows (an index into
    /// [`detail_field_rows`]), live only while `focus == Focus::Panel` on the
    /// vault page. Reset whenever the highlighted item can change.
    pub detail_cursor: usize,
    /// The inline field edit in flight on that cursor, if any.
    pub detail_edit: Option<DetailEdit>,
    /// The app's lazily-opened clipboard handle (see `clipboard::Clip`).
    clip: crate::clipboard::Clip,
    /// Monotonic copy counter: every copy bumps it and hands its value to a
    /// detached 20 s timer, so the wipe that comes back can tell "still my
    /// copy" from "someone copied something else since".
    pub copy_gen: u64,
    /// Ends of the timer channel those threads report on (created on the first
    /// copy; drained in `poll_clipboard`, like `poll_sync`).
    clip_tx: Option<std::sync::mpsc::Sender<u64>>,
    clip_rx: Option<std::sync::mpsc::Receiver<u64>>,
    /// Whether a secret WE copied is still sitting on the system clipboard.
    /// The 20 s timer is not the only wipe: locking and quitting wipe too (a
    /// detached timer dies with the process), and this says whether there is
    /// anything of ours left to wipe.
    clip_armed: bool,
    /// The exact bytes we last put on the clipboard, kept so a wipe can check
    /// that what is sitting there is still OURS before erasing it (see
    /// [`App::wipe_clipboard`]). Zeroized the moment it stops being needed.
    clip_payload: Zeroizing<String>,
    /// The in-vault left panel: live search query (matched against labels and
    /// non-secret field content), the active category tab (0 = All), and whether
    /// the search box currently has focus (typing edits the query, not the
    /// command line). All reset when the vault is closed.
    pub vault_query: crate::textinput::TextInput,
    pub vault_tab: usize,
    pub search_focus: bool,
    /// Which search that box runs, and what the semantic one last ranked (see
    /// [`VaultSearch`]). Reset with the rest of the browser state.
    pub vault_search: VaultSearch,
    /// Background fetcher + cache for website favicons (gated by
    /// `settings.favicons`; fetches directly from each site).
    pub favicons: crate::favicon::FaviconCache,
    /// Terminal graphics-protocol picker (kitty/sixel/iTerm2), set at startup
    /// once the terminal is queried. `None` = no graphics support → the list
    /// renders favicons with half-blocks (or a monogram) instead.
    pub picker: Option<ratatui_image::picker::Picker>,
    /// Per-domain crisp favicon render protocols, built lazily from the cached
    /// source image (interior mutability so `draw` can build them with `&App`).
    pub icon_protocols: std::cell::RefCell<
        std::collections::HashMap<String, ratatui_image::protocol::StatefulProtocol>,
    >,
    /// Every picture handed to the system viewer by `o`, as the path it was
    /// written to. These are DECRYPTED secrets sitting on the disk — the one
    /// place that is ever true — so the list exists to take them back again:
    /// lock, close and quit all run [`App::purge_exports`].
    exported_images: Vec<std::path::PathBuf>,
    /// Downscaled RGBA thumbnails of the image fields on show, for the
    /// half-block fallback (no graphics protocol) — keyed like `icon_protocols`
    /// plus the cell size. An EMPTY entry is the "this value doesn't decode"
    /// marker, so a corrupt field costs one decode attempt, never one per frame.
    pub image_cells: std::cell::RefCell<std::collections::HashMap<String, Vec<u8>>>,
    /// The MEMORANDUM (a chat with yourself, stored under `x_memo`): whether it
    /// has the vault body, how many rows the thread is scrolled BACK from the
    /// newest message (0 = pinned to it, which is where every send returns it),
    /// and the composer's buffer — one message, `\n`-separated, because
    /// Shift/Alt+⏎ writes a newline into it and ⏎ alone sends the lot. All
    /// three go away with the vault: the buffer is vault content half-written,
    /// so lock and close wipe it like any other.
    pub memo_open: bool,
    pub memo_scroll: usize,
    /// How far back that thread can go, as the RENDERER last laid it out (rows
    /// above the viewport). The pane's height and the way the messages wrapped
    /// are the draw's business and nothing the app can work out, so the bound
    /// comes back from down there (interior mutability, like `image_cells`) and
    /// the scroll keys clamp to it — see [`App::scroll_memo_back`].
    /// `usize::MAX` until this thread has been drawn once.
    pub memo_max_scroll: std::cell::Cell<usize>,
    pub memo_input: crate::textinput::TextInput,
    /// The Ctrl+F search bar, while it is up. It replaces the composer's row
    /// and holds the query, the mode chip and the hits (see [`MemoSearch`]).
    pub memo_search: Option<MemoSearch>,
    /// The message a search jump landed on: the thread is pinned to it, and
    /// its rows wear the selection band, until the user scrolls, sends or
    /// leaves. An ID and not a row number — rows depend on the pane width,
    /// which only the renderer knows.
    pub memo_jump: Option<String>,
    /// The message the copy-band is on: the one Up/Down last picked, as an
    /// index into the readable thread (`VaultContent::memo_messages`, oldest
    /// first). It is what Ctrl+Y copies, it wears the selection band, and it
    /// pulls the viewport to keep itself on screen — the search-jump machinery,
    /// reused (see the renderer's `memo_top`). `None` = nothing picked, the
    /// thread pinned to its newest row; reset on open / close / lock / send and
    /// dropped the moment the thread is scrolled or typed into by hand.
    pub memo_sel: Option<usize>,
    /// Whether the TERMINAL can tell Shift+⏎ apart from ⏎ — the kitty keyboard
    /// protocol, pushed by `main` when it is on offer. A legacy terminal sends
    /// one byte for both, so the composer's newline chord is Alt+⏎ there (which
    /// is accepted either way); this flag only decides WHICH of the two the
    /// keybar promises. False until `main` says otherwise, so a test — and any
    /// terminal that never answered — gets the honest fallback.
    pub shift_enter_ok: bool,
    /// Local semantic search: the on-device embedding engine, its workers and
    /// what the settings page says about them (see [`crate::rag`]).
    pub rag: crate::rag::RagState,
    /// Every URL/path handed to the desktop's opener, IN TESTS ONLY — where
    /// nothing may really spawn the machine's browser or image viewer. The
    /// one opener door ([`App::open_externally`]) records here instead.
    #[cfg(test)]
    pub opened: Vec<String>,
    /// The localhost agent API while it is up (see [`crate::api`]). `Some` ONLY
    /// with a vault open and at least one exposure toggle on: unlock and the
    /// settings commit start it, lock / close / quit stop it. Dropping it stops
    /// the thread and empties the snapshot it was serving.
    pub api: Option<crate::api::ApiServer>,
}

/// Seconds since the unix epoch — what a TOTP code is computed at. A clock set
/// before 1970 yields 0 (a wrong code, never a panic).
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A fresh random `u64` (best-effort; 0 on the vanishingly rare RNG failure).
fn random_u64() -> u64 {
    let mut b = [0u8; 8];
    getrandom::getrandom(&mut b).ok();
    u64::from_le_bytes(b)
}

impl Drop for App {
    fn drop(&mut self) {
        // the input line may hold a cleartext one-shot password; don't leave it
        // in a freed heap buffer (the open vault key is wiped by Vault's Drop).
        self.input.zeroize();
        self.vault_query.zeroize();
        // …nor the copy of it the semantic search was asked for
        self.vault_search.wipe();
        // …nor may the half-written memo message the composer is holding, nor
        // the search query beside it (which names what is IN the vault).
        self.memo_input.zeroize();
        if let Some(search) = self.memo_search.as_mut() {
            search.wipe();
        }
        // …and a copied secret must not outlive the app: the 20 s timer is a
        // detached thread that dies with the process, so quitting inside that
        // window would otherwise leave the value on the system clipboard (and
        // in the clipboard manager's history) forever.
        self.wipe_clipboard();
        // …and neither must a picture we decrypted onto the disk for a viewer.
        self.purge_exports();
        // …nor may a socket outlive the process it was serving the vault from.
        self.stop_api_server();
    }
}

impl App {
    pub fn new(paths: AppPaths) -> Self {
        let vaults = vault::list_vaults(&paths.vaults_dir);
        let favicon_dir = paths.vaults_dir.join("favicons");
        let mut rag = crate::rag::RagState::new(paths.models_dir.clone());
        let (registry, config_warning) = Registry::load(&paths.commands_config);
        let settings = Settings::load(&paths.settings_config);
        // the settings toggles own the engine: a model sitting in the cache is
        // not consent to index with it.
        rag.set_enabled(settings.rag_enabled());
        let logo = crate::logo::load(&paths.logo_config);
        let (chrome, ui_warning) = Chrome::load(&paths.ui_config);
        // a hand-edited `shortcut_overrides` that asks for a reserved, taken or
        // malformed key is running on the defaults — say which, once, here.
        let refused = crate::keymap::refused_overrides(&settings);
        let shortcut_warning = (!refused.is_empty()).then(|| {
            let slugs = refused.iter().map(|v| v.slug()).collect::<Vec<_>>().join(", ");
            i18n::t(settings.lang, &Key::ShortcutOverridesRefused(&slugs))
        });
        // combine any config-file warnings into one status line
        let warnings: Vec<String> =
            [config_warning, ui_warning, shortcut_warning].into_iter().flatten().collect();
        // re-apply a saved kitty translucency preference, best-effort. Do it on
        // a detached thread so a slow/unresponsive `kitty @` can never stall
        // startup before the first frame is drawn.
        if settings.blur == Blur::On {
            std::thread::spawn(|| {
                let _ = crate::blur::set_translucent(true);
            });
        }
        App {
            paths,
            registry,
            page: Page::Access,
            vaults,
            selected: 0,
            focus: Focus::Vaults,
            view: PanelView::Splash,
            back: Vec::new(),
            forward: Vec::new(),
            panel_selected: 0,
            input: String::new(),
            palette: None,
            cheatsheet: false,
            pending_confirm: None,
            unlock: None,
            locked: None,
            form: None,
            vault: None,
            // surface any config problem in the status bar instead of silently
            // reverting the user's edits.
            status: (!warnings.is_empty()).then(|| (StatusKind::Error, warnings.join(" · "))),
            logo,
            chrome,
            settings,
            settings_ui: None,
            detected_browser: None,
            pending_quit: false,
            device_id: crate::model::new_uuid_v4(),
            sync_status: std::collections::HashMap::new(),
            sync_rx: None,
            passwd_rx: None,
            passwd_busy: None,
            frame: 0,
            idle_ticks: 0,
            splash_seed: random_u64(),
            detail_reveal: false,
            detail_cursor: 0,
            detail_edit: None,
            clip: crate::clipboard::Clip::new(),
            copy_gen: 0,
            clip_tx: None,
            clip_rx: None,
            clip_armed: false,
            clip_payload: Zeroizing::new(String::new()),
            vault_query: crate::textinput::TextInput::new(),
            vault_tab: 0,
            search_focus: false,
            vault_search: VaultSearch::default(),
            favicons: crate::favicon::FaviconCache::new(favicon_dir),
            picker: None,
            exported_images: Vec::new(),
            icon_protocols: std::cell::RefCell::new(std::collections::HashMap::new()),
            image_cells: std::cell::RefCell::new(std::collections::HashMap::new()),
            memo_open: false,
            memo_scroll: 0,
            memo_max_scroll: std::cell::Cell::new(usize::MAX),
            memo_input: crate::textinput::TextInput::new(),
            memo_search: None,
            memo_jump: None,
            memo_sel: None,
            shift_enter_ok: false,
            #[cfg(test)]
            opened: Vec::new(),
            rag,
            api: None,
        }
    }

    // ---- image fields on screen (lazy, memoized decodes) -----------------

    /// Drops every decoded picture that came OUT OF THE VAULT — an item's field
    /// and a Memorandum photo alike; the render caches are the only place that
    /// plaintext image data outlives its [`Vault`]. Called wherever the vault
    /// goes away (lock, close). Favicon protocols are left alone: they came off
    /// a public website, not out of the vault.
    fn forget_item_images(&mut self) {
        self.icon_protocols
            .borrow_mut()
            .retain(|k, _| !k.starts_with(IMAGE_KEY_PREFIX) && !k.starts_with(MEMO_IMAGE_KEY_PREFIX));
        self.image_cells.borrow_mut().clear();
    }

    /// Deletes every picture `o` wrote out for the system viewer, and the export
    /// directory itself once it is empty. Best-effort by design: a viewer may
    /// still hold the file open, the disk may be gone — none of that is worth a
    /// panic on the way out. Runs beside [`App::forget_item_images`] wherever
    /// the vault's secrets stop being ours to keep (lock, close, quit).
    ///
    /// The user was TOLD the copy is temporary, so this is a promise, not a
    /// tidy-up: it runs even when the list is the only thing left standing (the
    /// `Drop` path, where the terminal is already going away).
    fn purge_exports(&mut self) {
        if self.exported_images.is_empty() {
            return;
        }
        for path in self.exported_images.drain(..) {
            let _ = std::fs::remove_file(&path);
        }
        // `remove_dir` refuses a non-empty directory, which is exactly the
        // wanted "only if empty" — a second instance's export is not ours.
        let _ = std::fs::remove_dir(crate::image_field::export_dir());
    }

    /// Whether `key`'s stored picture already failed to decode once. Every
    /// renderer asks first, so a corrupt value is never decoded twice.
    pub fn image_decode_failed(&self, key: &str) -> bool {
        self.image_cells.borrow().get(key).is_some_and(Vec::is_empty)
    }

    /// Remembers that `key` doesn't decode (see [`App::image_decode_failed`]).
    pub fn mark_image_undecodable(&self, key: &str) {
        self.image_cells.borrow_mut().insert(key.to_string(), Vec::new());
    }

    /// Whether the picture behind `key` is ALREADY drawable at this size —
    /// either as a terminal-protocol image or as a memoized half-block grid.
    ///
    /// The Memorandum asks BEFORE it materializes a photo's `ItemField`:
    /// building one copies the whole base64 and wipes it again, byte by byte,
    /// when the frame ends. A thread of pictures cannot pay that on every frame
    /// of a 120 ms loop for something the caches have already decoded.
    pub fn image_thumb_cached(&self, key: &str, w: u32, h: u32) -> bool {
        self.icon_protocols.borrow().contains_key(key)
            || self.image_cells.borrow().contains_key(&format!("{key}|{w}x{h}"))
    }

    /// The memoized `w×h` RGBA thumbnail of an image field — decoded at most
    /// once per key and size. `None` = the value doesn't decode (remembered).
    pub fn image_grid(
        &self,
        key: &str,
        field: &crate::model::ItemField,
        w: u32,
        h: u32,
    ) -> Option<Vec<u8>> {
        let cache_key = format!("{key}|{w}x{h}");
        if let Some(hit) = self.image_cells.borrow().get(&cache_key) {
            return (!hit.is_empty()).then(|| hit.clone());
        }
        let grid = crate::image_field::thumbnail(field, w, h).unwrap_or_default();
        self.image_cells.borrow_mut().insert(cache_key, grid.clone());
        (!grid.is_empty()).then_some(grid)
    }

    /// The active colour palette. While the settings page is open it reflects
    /// the theme currently drafted (live preview); otherwise the saved one.
    pub fn palette(&self) -> Palette {
        self.draft_settings().theme.palette()
    }

    /// Whether to paint a solid backdrop (honours the settings draft live).
    pub fn background_opaque(&self) -> bool {
        self.draft_settings().background == Background::Opaque
    }

    /// Whether the faint digit-rain drifts behind the screens. `Auto` = only
    /// inside DataShield's own dedicated terminal (the launcher sets
    /// `DATASHIELD_OWN_TERM`), where the whole look is ours to control.
    pub fn backdrop_on(&self) -> bool {
        match self.draft_settings().backdrop {
            crate::settings::Backdrop::On => true,
            crate::settings::Backdrop::Off => false,
            crate::settings::Backdrop::Auto => std::env::var_os("DATASHIELD_OWN_TERM").is_some(),
        }
    }

    /// Whether the animated backdrop holds a FIXED frame this tick instead of
    /// drifting — under reduce-motion always, and while the Memorandum is open.
    /// The memo owns mouse text selection (the app never captures the mouse, so
    /// the terminal does), and a backdrop cell that changes every frame under a
    /// stretched selection makes the terminal drop it mid-drag — so the whole
    /// idle memo view has to be byte-identical frame to frame. Everywhere else
    /// this is exactly `reduce_motion`, so no other screen's wallpaper changes.
    pub fn backdrop_frozen(&self) -> bool {
        self.draft_settings().reduce_motion || self.memo_open
    }

    /// The settings in effect for rendering: the open page's draft if any (live
    /// cosmetic preview), else the saved settings.
    pub fn draft_settings(&self) -> &Settings {
        self.settings_ui
            .as_ref()
            .map(|s| &s.draft)
            .unwrap_or(&self.settings)
    }

    /// The language in effect for rendering: the settings draft's while that
    /// page is open (so cycling Language previews everywhere), else the saved
    /// one.
    pub fn effective_lang(&self) -> Lang {
        self.draft_settings().lang
    }

    /// The current name of the language command on this page (the user may
    /// have renamed it in `commands.toml`).
    pub fn language_command_name(&self) -> Option<&str> {
        self.registry
            .command_for_action(self.page, Action::Language)
            .map(|c| c.name.as_str())
    }

    /// The command word for a picker (respects config renames).
    pub fn picker_command_name(&self, kind: PickerKind) -> &str {
        self.registry
            .command_for_action(self.page, kind.action())
            .map(|c| c.name.as_str())
            .unwrap_or(match kind {
                PickerKind::Language => "language",
                PickerKind::PanelHistory => "~",
                PickerKind::ItemCategory => "add",
            })
    }

    fn note(&mut self, text: String) {
        self.status = Some((StatusKind::Info, text));
    }

    fn ok(&mut self, text: String) {
        self.status = Some((StatusKind::Success, text));
    }

    fn warn(&mut self, text: String) {
        self.status = Some((StatusKind::Error, text));
    }

    /// Switches the main (left-panel) page, resetting the `~` history.
    pub fn set_page(&mut self, page: Page) {
        if page != self.page {
            self.page = page;
            self.back.clear();
            self.forward.clear();
            self.view = PanelView::Splash;
            self.splash_seed = random_u64();
            self.panel_selected = 0;
        }
    }

    /// Resets the `~` panel to the idle splash and clears its history — used
    /// after an action completes so `~ back` doesn't replay stale views.
    fn go_home(&mut self) {
        self.reset_detail();
        self.view = PanelView::Splash;
        self.splash_seed = random_u64(); // a fresh effect each time we come home
        self.panel_selected = 0;
        self.back.clear();
        self.forward.clear();
    }

    /// Commits a new `~` view, recording the previous one in the back history
    /// (and dropping the forward history, browser-style).
    fn set_view(&mut self, view: PanelView) {
        // a different surface is taking over: the detail cursor (and any inline
        // edit typed into it) has nothing left to point at.
        self.reset_detail();
        if view != self.view {
            self.back.push(self.view.clone());
            self.forward.clear();
            self.detail_reveal = false; // secrets never carry across views
        }
        // the splash isn't interactive (0 items), so never leave focus parked on
        // it, and pick a fresh effect each time we land there.
        if view == PanelView::Splash {
            self.focus = Focus::Vaults;
            self.splash_seed = random_u64();
        }
        self.view = view;
        self.panel_selected = 0;
    }

    fn history_back(&mut self) {
        match self.back.pop() {
            Some(prev) => {
                let current = std::mem::replace(&mut self.view, prev);
                self.forward.push(current);
                self.panel_selected = 0;
                self.focus = self.focus_for_view();
                self.status = None;
            }
            None => self.note(i18n::t(self.settings.lang, &Key::NoBack)),
        }
    }

    fn history_forward(&mut self) {
        match self.forward.pop() {
            Some(next) => {
                let current = std::mem::replace(&mut self.view, next);
                self.back.push(current);
                self.panel_selected = 0;
                self.focus = self.focus_for_view();
                self.status = None;
            }
            None => self.note(i18n::t(self.settings.lang, &Key::NoForward)),
        }
    }

    /// Focus the panel only when the current view has something to select;
    /// otherwise the list (so we never park on a no-op view like the splash).
    fn focus_for_view(&self) -> Focus {
        if self.panel_len() > 0 {
            Focus::Panel
        } else {
            Focus::Vaults
        }
    }

    fn panel_history(&mut self, arg: Option<&str>) {
        match arg.map(|a| a.to_ascii_lowercase()).as_deref() {
            Some("back") | Some("b") => self.history_back(),
            Some("forw") | Some("forward") | Some("fwd") | Some("f") => self.history_forward(),
            _ => self.note(i18n::t(self.settings.lang, &Key::PanelUsage)),
        }
    }

    pub fn reload_vaults(&mut self) {
        self.vaults = vault::list_vaults(&self.paths.vaults_dir);
        if self.selected >= self.vaults.len() {
            self.selected = self.vaults.len().saturating_sub(1);
        }
    }

    pub fn on_tick(&mut self) {
        self.frame = self.frame.wrapping_add(1);
        self.idle_ticks = self.idle_ticks.saturating_add(1);
        // idle auto-lock: with a vault open and no key for the configured span,
        // lock NOW — the vault drops (its key is zeroized) and the full-screen
        // lock takes over. Idle time is exactly when a password manager locks.
        if self.vault.is_some() && self.locked.is_none() {
            if let Some(limit) = self.settings.autolock.ticks() {
                if self.idle_ticks >= limit {
                    self.lock_now();
                }
            }
        }
        // apply any finished sync worker result
        self.poll_sync();
        // …and any finished change-password re-encryption
        self.poll_passwd();
        // …and any due clipboard auto-clear
        self.poll_clipboard();
        // …and any finished embedding batch (or model download)
        self.poll_rag();
        // …and, while the one-time download runs, the bytes its bar draws.
        // Once every `RAG_SAMPLE_TICKS` and never otherwise: the walk is a
        // dozen `stat`s (see `rag::dir_bytes`), which the 120 ms loop cannot
        // feel, but half a gigabyte does not move fast enough to be worth more.
        if self.frame.is_multiple_of(RAG_SAMPLE_TICKS) {
            self.rag.sample_download();
        }
        // …and, as rarely, the API sessions the idle TTL has run out on (see
        // `API_REAP_TICKS`): the request path cannot reap a token nobody uses.
        if self.frame.is_multiple_of(API_REAP_TICKS) {
            if let Some(server) = self.api.as_ref() {
                server.reap_sessions();
            }
        }
        // …and whatever the browser extension left for us: an "unlock, please"
        // and the logins it was asked to keep
        self.poll_browser_requests();
        // queue favicon fetches for the items currently on screen (best-effort)
        self.prefetch_favicons();
    }

    /// Drains the browser extension's requests (called each tick): a login page
    /// that found no vault open — say so, notify, come forward — and the logins
    /// the user chose to keep, written through the ordinary mutation path.
    fn poll_browser_requests(&mut self) {
        let Some(server) = self.api.as_ref() else { return };
        let wake = server.take_wake();
        let saves = server.take_saves();
        if let Some(wake) = wake {
            let lang = self.settings.lang;
            let line = i18n::t(lang, &Key::BrowserWantsLogin(&wake.host));
            // (never under test: the suite must not pop notifications or
            // poke the window manager of the machine it runs on)
            #[cfg(not(test))]
            {
                crate::browser::notify("DataShield", &line);
                crate::browser::bring_to_front();
            }
            self.note(line);
        }
        for save in saves {
            self.save_browser_login(save);
        }
    }

    /// Keeps a login made in the browser: an item of this vault that already
    /// logs in at that site with that username gets its password replaced;
    /// otherwise a new `login` item is made, named after the host. Through the
    /// same path every mutation takes (memory → counter → save → roll back on
    /// Err → auto-sync, which also refreshes what the extension sees).
    fn save_browser_login(&mut self, save: crate::api::SaveRequest) {
        use crate::model::{FieldType, Item, ItemField, SecretString};
        if self.refuse_if_open_vault_rekeying() {
            return;
        }
        let lang = self.settings.lang;
        let host = save.host.clone();
        let Some(vault) = self.vault.as_mut() else { return };
        // an existing item for this site + username: update its password
        let existing = vault.items().iter().position(|item| {
            crate::model::item_website(item)
                .is_some_and(|site| crate::api::domain_matches(&format!("https://{host}/"), &crate::favicon::domain_of(site).unwrap_or_default()))
                && crate::model::item_user_index(item)
                    .is_some_and(|i| item.fields[i].value.as_str().eq_ignore_ascii_case(&save.username))
                && crate::model::item_password_index(item).is_some()
        });
        match existing {
            Some(idx) => {
                let item = &mut vault.items_mut()[idx];
                let pi = crate::model::item_password_index(item).expect("checked above");
                if item.fields[pi].value.as_str() == save.password.as_str() {
                    return; // the same password again: nothing to write
                }
                let prev = std::mem::replace(
                    &mut item.fields[pi].value,
                    SecretString::new(save.password.as_str()),
                );
                let prev_modified = item.modified_at.replace(crate::model::now_rfc3339());
                vault.content.bump_sync_counter("local");
                match vault.save() {
                    Ok(()) => {
                        self.ok(i18n::t(lang, &Key::BrowserLoginUpdated(&host)));
                        self.auto_sync_after_change();
                    }
                    Err(_) => {
                        let item = &mut self.vault.as_mut().unwrap().items_mut()[idx];
                        item.fields[pi].value = prev;
                        item.modified_at = prev_modified;
                        self.warn(i18n::t(lang, &Key::SaveFailed));
                    }
                }
            }
            None => {
                let label = host.strip_prefix("www.").unwrap_or(&host).to_string();
                let mut item = Item::new("login", &label);
                let field = |key: &str, ftype: FieldType, value: &str, secret: bool| ItemField {
                    key: key.to_string(),
                    ftype,
                    label: None,
                    value: SecretString::new(value),
                    secret,
                    extra: serde_json::Map::new(),
                };
                if !save.username.is_empty() {
                    let ftype = if save.username.contains('@') { FieldType::Email } else { FieldType::Username };
                    let key = if ftype == FieldType::Email { "email" } else { "username" };
                    item.fields.push(field(key, ftype, &save.username, false));
                }
                item.fields.push(field("password", FieldType::Password, save.password.as_str(), true));
                item.fields.push(field("url", FieldType::Url, &host, false));
                vault.items_mut().push(item);
                vault.content.bump_sync_counter("local");
                match vault.save() {
                    Ok(()) => {
                        self.ok(i18n::t(lang, &Key::BrowserLoginSaved(&host)));
                        self.auto_sync_after_change();
                    }
                    Err(_) => {
                        if let Some(v) = self.vault.as_mut() {
                            v.items_mut().pop();
                        }
                        self.warn(i18n::t(lang, &Key::SaveFailed));
                    }
                }
            }
        }
    }

    /// Ticks left before the idle auto-lock fires (`None` = off / no vault).
    pub fn autolock_remaining_ticks(&self) -> Option<u64> {
        self.vault.as_ref()?;
        self.settings.autolock.ticks().map(|l| l.saturating_sub(self.idle_ticks))
    }

    pub fn cursor_on(&self) -> bool {
        // reduce-motion keeps the cursor steady (no blink)
        if self.settings.reduce_motion {
            return true;
        }
        (self.frame / 4).is_multiple_of(2)
    }

    // ---- panel state ---------------------------------------------------

    /// The view actually shown right now.
    pub fn active_view(&self) -> PanelView {
        self.view.clone()
    }

    /// Where arrows/Enter act right now: the explicit focus (a form starts on
    /// the panel but the user can move off with ←).
    pub fn effective_focus(&self) -> Focus {
        self.focus
    }

    /// The picker options for `kind` (language codes, `~` directions, or the
    /// item categories).
    pub fn picker_choices(&self, kind: PickerKind) -> Vec<Choice> {
        match kind {
            PickerKind::Language => Lang::ALL
                .iter()
                .map(|l| Choice {
                    value: l.code().to_string(),
                    label: l.name().to_string(),
                })
                .collect(),
            PickerKind::PanelHistory => vec![
                Choice {
                    value: "back".to_string(),
                    label: i18n::t(self.settings.lang, &Key::PanelBackLabel),
                },
                Choice {
                    value: "forw".to_string(),
                    label: i18n::t(self.settings.lang, &Key::PanelForwLabel),
                },
            ],
            PickerKind::ItemCategory => {
                // built-ins in standard order, then the vault's custom categories
                let mut out: Vec<Choice> = crate::model::BUILTINS
                    .iter()
                    .map(|c| Choice {
                        value: c.id.to_string(),
                        label: i18n::t(self.settings.lang, &Key::CategoryName(c.id)),
                    })
                    .collect();
                if let Some(v) = &self.vault {
                    for def in &v.content.categories {
                        out.push(Choice { value: def.id.clone(), label: def.name.clone() });
                    }
                }
                // fig-05: "+ new category — your own fields" is a real row
                out.push(Choice {
                    value: NEW_CATEGORY.to_string(),
                    label: match self.settings.lang {
                        Lang::En => "+ new category".to_string(),
                        Lang::It => "+ nuova categoria".to_string(),
                    },
                });
                out
            }
        }
    }

    /// Number of selectable items in the current panel view.
    pub fn panel_len(&self) -> usize {
        match self.active_view() {
            PanelView::Picker(kind) => self.picker_choices(kind).len(),
            PanelView::Splash | PanelView::EncInfo(_) => 0,
        }
    }

    /// The highlighted index, clamped to the current view's length.
    pub fn panel_index(&self) -> usize {
        self.panel_selected.min(self.panel_len().saturating_sub(1))
    }

    // ---- input handling ------------------------------------------------

    /// Handles a key press. Returns `true` when the app should quit.
    pub fn on_key(&mut self, code: KeyCode, mods: KeyModifiers) -> bool {
        if mods.contains(KeyModifiers::CONTROL) && matches!(code, KeyCode::Char('c')) {
            // the input line may hold a cleartext one-shot password — wipe it
            // before quitting rather than freeing the buffer unwiped
            self.input.zeroize();
            return true;
        }
        self.idle_ticks = 0;
        // Ctrl+Q quits from anywhere (lock ≠ quit; this is the quit).
        if mods.contains(KeyModifiers::CONTROL) && matches!(code, KeyCode::Char('q')) {
            self.input.zeroize();
            return true;
        }
        // The full-screen lock owns every key until the vault re-opens.
        if self.locked.is_some() {
            return self.on_lock_key(code, mods);
        }
        // A second consecutive Esc confirms a quit; any other key cancels it.
        if !matches!(code, KeyCode::Esc) {
            self.pending_quit = false;
        }
        // the cheatsheet overlay is dismissed by any key.
        if self.cheatsheet {
            self.cheatsheet = false;
            return false;
        }
        // item-level delete confirm (rung 1): the locale's yes key deletes;
        // Esc / n / anything else keeps it (default no). No flipper to point at.
        if self.pending_confirm.is_some() {
            match code {
                KeyCode::Char(c) if c.to_ascii_lowercase() == self.confirm_key() => self.confirm_delete_now(),
                _ => self.pending_confirm = None,
            }
            return false;
        }
        // the command palette, when open, owns every key.
        if self.palette.is_some() {
            return self.on_palette_key(code, mods);
        }
        if self.settings_ui.is_some() {
            return self.on_settings_key(code);
        }
        if self.form.is_some() {
            return self.on_form_mode_key(code, mods);
        }
        // the inline unlock (unfolded under the selected vault row) owns keys —
        // letters are the passphrase there, never verbs.
        if self.page == Page::Access && self.unlock.is_some() {
            return self.on_unlock_key(code, mods);
        }
        // Ctrl+L locks now: the vault closes (its key is zeroized) and the
        // full-screen lock takes over. Lock ≠ quit.
        if mods.contains(KeyModifiers::CONTROL) && matches!(code, KeyCode::Char('l')) {
            if self.page == Page::Vault {
                self.lock_now();
            }
            return false;
        }
        // The MEMORANDUM owns the whole vault body, and with it every key: its
        // composer is always focused, so a bare letter is the message being
        // typed. That puts it ahead of the ":"/"/"/"?" triggers, of the docked
        // detail and of the verb lookup — and behind only what floats OVER it
        // (lock, cheatsheet, confirm, palette, settings, form, unlock) plus the
        // three chords that are never text anywhere: Ctrl+C, Ctrl+Q, Ctrl+L.
        if self.memo_open {
            return self.on_memo_key(code, mods);
        }
        // The DOCKED DETAIL, once ⏎ moved focus onto it, owns its keys — and it
        // sits here on purpose, ahead of the ":"/"/"/"?" triggers and of the
        // single-key verb lookup below: while a field is being edited inline a
        // typed '/' or 'c' is TEXT, and even outside an edit the copy/edit/
        // delete/star keys are the detail's (copy FIELD · edit form · delete ·
        // star), not the list's. Anything it doesn't claim falls through.
        if self.detail_focused() && self.on_detail_key(code, mods) {
            return false;
        }
        // command / filter / cheatsheet triggers — never text characters.
        if !self.search_focus
            && !mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
        {
            match code {
                KeyCode::Char(':') => {
                    self.open_palette();
                    return false;
                }
                // spec-exact: "/" filters inside a vault list, opens the palette
                // on the access page (which has no list to filter).
                KeyCode::Char('/') => {
                    if self.page == Page::Vault && self.view == PanelView::Splash {
                        self.set_search_focus(true);
                    } else {
                        self.open_palette();
                    }
                    return false;
                }
                KeyCode::Char('?') => {
                    self.cheatsheet = true;
                    return false;
                }
                _ => {}
            }
        }
        // in-vault: category tabs + the summoned filter (chips row)
        if self.page == Page::Vault
            && self.view == PanelView::Splash
            && self.on_vault_browser_key(code, mods)
        {
            return false;
        }
        // single-key VERBS: the hourly actions live on bare keys (k9s-style).
        // The command line is gone — commands come from the ":" palette — so a
        // bare letter is a verb, never text. Verbs never fire while the search
        // box has focus (there, letters edit the query).
        if !self.search_focus {
            if let Some(verb) = self.verb_for(self.page, code) {
                self.dispatch_verb(verb);
                return false;
            }
        }
        match code {
            KeyCode::Esc => return self.on_escape(),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Enter => self.on_enter(),
            // every other bare key is inert now: there is no command line to type
            // into — the ":" palette (and the keybar verbs) is the command surface.
            _ => {}
        }
        false
    }

    fn on_escape(&mut self) -> bool {
        if !self.input.is_empty() {
            // wipe (not just truncate) — the line may hold a cleartext password
            self.input.zeroize();
            self.panel_selected = 0;
        } else if self.view != PanelView::Splash {
            self.set_view(PanelView::Splash);
        } else if self.focus != Focus::Vaults {
            self.focus = Focus::Vaults;
            self.reset_detail();
        } else if self.page == Page::Vault {
            // at the top of a vault, Esc closes it rather than quitting
            self.exit_vault();
        } else {
            // access top level: quit — optionally after a confirmation
            if self.settings.confirm_exit && !self.pending_quit {
                self.pending_quit = true;
                self.note(i18n::t(self.settings.lang, &Key::ConfirmQuit));
                return false;
            }
            return true;
        }
        false
    }

    // ---- inline unlock (fig-01) + the full-screen lock (fig-02) --------

    /// Enters an unlocked vault: page swap + a clean browser state.
    fn enter_vault(&mut self, vault: Vault) {
        let name = vault.username.clone();
        self.vault = Some(vault);
        self.set_page(Page::Vault);
        self.selected = 0;
        self.focus = Focus::Vaults;
        self.reset_vault_browser();
        self.ok(i18n::t(self.settings.lang, &Key::VaultUnlocked(&name)));
        // auto-pull the latest on unlock, when this vault opted in
        if crate::sync::SyncConfig::load(&self.paths.vaults_dir, &name).is_some_and(|c| c.auto) {
            self.sync_vault(&name);
        }
        // …and catch both indexes up with whatever changed while this vault was
        // closed (a no-op when nothing did)
        self.rag_reindex();
        // NO UNLOCK, NO RAG: the agent API exists only from here, and only when
        // the user asked for it. Its token is minted now, for this session.
        self.sync_api_server();
    }

    /// Keys while the inline unlock is unfolded under a vault row.
    fn on_unlock_key(&mut self, code: KeyCode, mods: KeyModifiers) -> bool {
        // ←/→/Home/End move the caret, Backspace/Delete edit at it — the passphrase
        // is masked, so the caret is not drawn, but editing it still lands where
        // it should (Up/Down keep folding the prompt back up, below).
        if !matches!(code, KeyCode::Up | KeyCode::Down) {
            if let Some(u) = self.unlock.as_mut() {
                if crate::textinput::edit_key(&mut u.input, code) != crate::textinput::Edit::Ignored {
                    u.error = false;
                    return false;
                }
            }
        }
        match code {
            KeyCode::Esc => self.unlock = None,
            KeyCode::Enter => self.try_unlock_inline(),
            KeyCode::Up | KeyCode::Down => {
                // moving the selection folds the prompt back up
                self.unlock = None;
                self.move_selection(if code == KeyCode::Up { -1 } else { 1 });
            }
            KeyCode::Char(ch)
                if !mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER) =>
            {
                if let Some(u) = self.unlock.as_mut() {
                    u.input.insert(ch);
                    u.error = false;
                }
            }
            _ => {}
        }
        false
    }

    /// Submits the inline unlock: opens the vault or counts a failed attempt.
    fn try_unlock_inline(&mut self) {
        let Some(name) = self.unlock.as_ref().map(|u| u.name.clone()) else { return };
        // the prompt may have been unfolded before the re-encryption started
        if self.refuse_if_rekeying(&name) {
            return;
        }
        let Some(u) = self.unlock.as_mut() else { return };
        let password = std::mem::take(&mut u.input);
        match Vault::open(&self.paths.vaults_dir, &name, &password) {
            Ok(vault) => {
                self.unlock = None;
                self.enter_vault(vault);
            }
            Err(UnlockError::WrongPassword) => {
                if let Some(u) = self.unlock.as_mut() {
                    u.attempts += 1;
                    u.error = true;
                }
            }
            Err(e) => {
                self.unlock = None;
                self.warn(format!("{e:?}"));
            }
        }
        let mut pw = password;
        pw.zeroize();
    }

    /// Locks the open vault NOW: drops it (the derived key is zeroized on drop)
    /// and raises the full-screen lock bound to the same vault.
    fn lock_now(&mut self) {
        let Some(name) = self.vault.as_ref().map(|v| v.username.clone()) else { return };
        self.vault = None; // Zeroizing<key> wiped here
        // a locked vault must leave nothing behind: a value copied out of it is
        // still pasteable by whoever is at the keyboard until its timer fires
        self.wipe_clipboard();
        self.forget_item_images();
        self.purge_exports();
        self.go_home();
        self.page = Page::Access;
        self.vault_query.zeroize();
        self.vault_query.clear();
        self.vault_search.wipe();
        self.vault_tab = 0;
        self.search_focus = false;
        self.detail_reveal = false;
        self.reset_detail();
        // the thread is gone with the vault; so is the line half-typed into it
        self.close_memo();
        // the model holds no secrets, but there is no reason to keep it
        // resident behind a lock screen — the next search loads it back. It
        // never WAITS for a worker that is holding it: locking a vault cannot
        // depend on how long somebody else's inference takes.
        self.rag.unload();
        // …and the agent API stops serving the vault it was serving: a locked
        // vault that still answers searches would be no lock at all. With
        // nothing else asking for the server it goes down entirely and its
        // token dies, so the next unlock mints a new one; with
        // `api_vault_unlock` on it stays up for `/unlock` alone, its snapshot
        // emptied — the lock screen is about THIS vault, and an agent's own
        // sessions were never opened by what is behind it.
        self.sync_api_server();
        self.locked = Some(PassPrompt::new(&name));
    }

    /// Keys while the full-screen lock is up: type the passphrase, Enter
    /// re-unlocks, Esc falls back to the access list (lock ≠ quit).
    fn on_lock_key(&mut self, code: KeyCode, mods: KeyModifiers) -> bool {
        // ←/→/Home/End move the caret, Backspace/Delete edit at it (the mask hides
        // the caret, but editing still lands where it should).
        if let Some(l) = self.locked.as_mut() {
            if crate::textinput::edit_key(&mut l.input, code) != crate::textinput::Edit::Ignored {
                l.error = false;
                return false;
            }
        }
        match code {
            KeyCode::Esc => self.locked = None,
            KeyCode::Enter => {
                let Some(name) = self.locked.as_ref().map(|l| l.name.clone()) else { return false };
                // the idle lock can fall while its own vault is re-encrypting;
                // re-unlocking now would just count a bogus failed attempt
                if self.refuse_if_rekeying(&name) {
                    return false;
                }
                let Some(l) = self.locked.as_mut() else { return false };
                let password = std::mem::take(&mut l.input);
                match Vault::open(&self.paths.vaults_dir, &name, &password) {
                    Ok(vault) => {
                        self.locked = None;
                        self.enter_vault(vault);
                    }
                    Err(UnlockError::WrongPassword) => {
                        if let Some(l) = self.locked.as_mut() {
                            l.attempts += 1;
                            l.error = true;
                        }
                    }
                    Err(_) => self.locked = None,
                }
                let mut pw = password;
                pw.zeroize();
            }
            KeyCode::Char(ch)
                if !mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER) =>
            {
                if let Some(l) = self.locked.as_mut() {
                    l.input.insert(ch);
                    l.error = false;
                }
            }
            _ => {}
        }
        false
    }

    // ---- the `:` command palette ---------------------------------------

    /// Opens the fuzzy command palette, wiping any half-typed command line.
    fn open_palette(&mut self) {
        self.input.zeroize();
        self.palette = Some(crate::palette::Palette::new());
        self.status = None;
    }

    /// The commands matching the palette's current query, best first.
    pub fn palette_matches(&self) -> Vec<&crate::registry::CommandDef> {
        let cmds = self.registry.for_page(self.page);
        let q = self.palette.as_ref().map(|p| p.query.as_str()).unwrap_or("");
        crate::palette::ranked(&cmds, self.settings.lang, q)
    }

    fn on_palette_key(&mut self, code: KeyCode, mods: KeyModifiers) -> bool {
        // ←/→/Home/End move the caret; Backspace/Delete edit at it and, like a
        // fresh keystroke, drop the highlight back to the best match.
        if let Some(p) = self.palette.as_mut() {
            match crate::textinput::edit_key(&mut p.query, code) {
                crate::textinput::Edit::Changed => {
                    p.selected = 0;
                    return false;
                }
                crate::textinput::Edit::Moved => return false,
                crate::textinput::Edit::Ignored => {}
            }
        }
        match code {
            KeyCode::Esc => self.palette = None,
            KeyCode::Enter => self.palette_submit(),
            KeyCode::Up => self.palette_move(-1),
            KeyCode::Down => self.palette_move(1),
            KeyCode::Char(ch)
                if !mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER) =>
            {
                if let Some(p) = self.palette.as_mut() {
                    p.query.insert(ch);
                    p.selected = 0;
                }
            }
            _ => {}
        }
        false
    }

    fn palette_move(&mut self, delta: isize) {
        let n = self.palette_matches().len();
        if n == 0 {
            return;
        }
        if let Some(p) = self.palette.as_mut() {
            p.selected = (p.selected as isize + delta).rem_euclid(n as isize) as usize;
        }
    }

    /// Runs a command from the palette and closes it. If the query's first word
    /// resolves to a command exactly (name or alias), that command runs with the
    /// remaining words as inline arguments — `language it`, `category Database` —
    /// EXCEPT a guarded command (one with a secret field) ignores inline args and
    /// opens its masked modal, so a secret never rides in the palette query.
    /// Otherwise the highlighted fuzzy match runs with no arguments.
    fn palette_submit(&mut self) {
        let query = self.palette.as_ref().map(|p| p.query.as_str().to_string()).unwrap_or_default();
        let tokens: Vec<String> = query.split_whitespace().map(|s| s.to_string()).collect();
        let (name, args): (Option<String>, Vec<String>) = match tokens.first() {
            Some(first) if self.registry.resolve(self.page, first).is_some() => {
                let guarded = self
                    .registry
                    .resolve(self.page, first)
                    .map(|c| c.params.iter().any(|p| p.secret))
                    .unwrap_or(false);
                let args = if guarded { Vec::new() } else { tokens[1..].to_vec() };
                (Some(first.clone()), args)
            }
            _ => {
                let matches = self.palette_matches();
                let sel = self.palette.as_ref().map(|p| p.selected).unwrap_or(0);
                (matches.get(sel).map(|c| c.name.clone()), Vec::new())
            }
        };
        self.palette = None;
        if let Some(name) = name {
            let argrefs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            self.run_command(&name, &argrefs);
        }
    }

    /// Number of entries in the left panel (vaults on Access, the *visible*
    /// items — filtered by tab + search — in Vault).
    pub fn left_len(&self) -> usize {
        match self.page {
            Page::Access => self.vaults.len(),
            Page::Vault => self.visible_item_indices().len(),
        }
    }

    fn move_selection(&mut self, delta: isize) {
        match self.effective_focus() {
            Focus::Panel => {
                let n = self.panel_len();
                if n > 0 {
                    self.panel_selected =
                        (self.panel_index() as isize + delta).rem_euclid(n as isize) as usize;
                }
            }
            Focus::Vaults => {
                let n = self.left_len();
                if n > 0 {
                    self.selected = (self.selected as isize + delta).rem_euclid(n as isize) as usize;
                }
                // the docked detail follows the cursor: a new item means a new
                // field list, so its own cursor starts over.
                self.reset_detail();
            }
        }
    }

    fn on_enter(&mut self) {
        // there is no command line anymore; Enter acts on the focused surface.
        match self.effective_focus() {
            Focus::Panel => self.activate_panel(),
            Focus::Vaults => self.activate_left(),
        }
    }

    /// Enter on the highlighted left entry: unfold the inline unlock under a
    /// vault (Access), or hand the keyboard to the docked detail (Vault).
    fn activate_left(&mut self) {
        match self.page {
            Page::Access => {
                if let Some(name) = self.vaults.get(self.selected).cloned() {
                    // its file is mid-rewrite: the old password is on its way out
                    if self.refuse_if_rekeying(&name) {
                        return;
                    }
                    self.unlock = Some(PassPrompt::new(&name));
                    self.status = None;
                }
            }
            Page::Vault => {
                // the detail is already docked and already showing this item —
                // ⏎ just moves the keyboard onto it (fields become navigable,
                // `c` copies one, ⏎ edits it in place). "Open item" is still
                // not a screen.
                if self.highlighted_item_id().is_some() {
                    self.focus_detail();
                } else {
                    self.note(i18n::t(self.settings.lang, &Key::NoItems));
                }
            }
        }
    }

    /// Enter over the highlighted item of the focused panel view.
    fn activate_panel(&mut self) {
        match self.active_view() {
            PanelView::Picker(kind) => {
                let index = self.panel_index();
                let value = self.picker_choices(kind).get(index).map(|c| c.value.as_str().to_string());
                if let Some(value) = value {
                    self.apply_choice(kind, &value);
                }
            }
            PanelView::Splash | PanelView::EncInfo(_) => {}
        }
    }

    fn apply_choice(&mut self, kind: PickerKind, value: &str) {
        match kind {
            PickerKind::Language => {
                if let Some(l) = Lang::from_arg(value) {
                    self.apply_language(l);
                }
            }
            PickerKind::PanelHistory => {
                if value == "back" {
                    self.history_back();
                } else {
                    self.history_forward();
                }
            }
            PickerKind::ItemCategory => {
                if value == NEW_CATEGORY {
                    self.open_category_form(None);
                } else {
                    self.open_add_form(value);
                }
            }
        }
    }

    /// Runs a command by canonical name. `args` are cleartext arguments used to
    /// pre-fill a form's non-secret fields.
    fn run_command(&mut self, name: &str, args: &[&str]) {
        let Some(cmd) = self.registry.resolve(self.page, name) else {
            self.warn(i18n::t(self.settings.lang, &Key::UnknownCommand(name)));
            return;
        };
        let action = cmd.action;
        let form = FormState::from_command(cmd, self.settings.lang, args);
        // (registry borrow ends here — everything below is owned/Copy)
        let arg = args.first().copied().map(str::to_string);

        match action {
            // `help`/`commands` open the palette — it IS the command surface.
            Action::Help => self.open_palette(),
            // an inline arg picks directly (`language it`, `~ back`); bare opens
            // the chooser. (This used to live in the free-text parser.)
            Action::Language => match arg.as_deref() {
                Some(a) => self.set_language(a),
                None => self.enter_view(PanelView::Picker(PickerKind::Language)),
            },
            Action::Panel => match arg.as_deref() {
                Some(a) => self.panel_history(Some(a)),
                None => self.enter_view(PanelView::Picker(PickerKind::PanelHistory)),
            },
            Action::ExitVault => self.exit_vault(),
            // in-vault: Add is wired (category picker → dynamic form); the rest
            // are registered and discoverable but not built yet.
            Action::AddItem => match arg.as_deref() {
                Some(cat) if self.resolve_category(cat).is_some() => {
                    let id = self.resolve_category(cat).unwrap();
                    self.open_add_form(&id);
                }
                _ => self.enter_view(PanelView::Picker(PickerKind::ItemCategory)),
            },
            Action::SearchItems => {
                // wire `search [query]` to the always-present search box: a query
                // filters the list; a bare `search` just focuses the empty box.
                self.set_view(PanelView::Splash);
                self.vault_query.set(args.join(" "));
                self.selected = 0;
                self.search_focus = self.vault_query.is_empty();
                self.focus = Focus::Vaults;
                // …through the same door a typed query goes: a box left in RAG
                // mode answers `search` semantically too (a no-op otherwise)
                self.run_vault_search();
            }
            Action::StarItem => {
                let label = (!args.is_empty()).then(|| args.join(" "));
                self.star_item(label.as_deref());
            }
            Action::DeleteItem => {
                let label = (!args.is_empty()).then(|| args.join(" "));
                self.open_delete_form(label.as_deref());
            }
            Action::EditItem => {
                let label = (!args.is_empty()).then(|| args.join(" "));
                match label {
                    Some(ref lbl) => {
                        match self.vault.as_ref().and_then(|v| v.position_by_label(lbl)) {
                            Some(idx) => {
                                let id = self.vault.as_ref().unwrap().items()[idx].id.clone();
                                self.open_edit_form(&id);
                            }
                            None => self.warn(i18n::t(self.settings.lang, &Key::ItemNotFound(lbl))),
                        }
                    }
                    None => match self.highlighted_item_id() {
                        Some(id) => self.open_edit_form(&id),
                        None => self.note(i18n::t(self.settings.lang, &Key::NoItems)),
                    },
                }
            }
            Action::ManageCategory => {
                let name = (!args.is_empty()).then(|| args.join(" "));
                self.open_category_form(name.as_deref());
            }
            Action::EncInfo => {
                let label = (!args.is_empty()).then(|| args.join(" "));
                let target = label.as_deref().and_then(|lbl| {
                    self.vault
                        .as_ref()
                        .and_then(|v| v.position_by_label(lbl).map(|i| v.items()[i].id.clone()))
                });
                if label.is_some() && target.is_none() {
                    self.warn(i18n::t(self.settings.lang, &Key::ItemNotFound(label.as_deref().unwrap_or(""))));
                } else {
                    self.enter_view(PanelView::EncInfo(target));
                }
            }
            Action::RerollEnc => self.open_reroll_form(),
            Action::NukeItems => self.open_nuke_items_form(),
            Action::CreateVault => {
                // always the step-by-step form: the password is entered masked
                // and never rides in a command argument (guarded command).
                if let Some(form) = form {
                    self.open_form(form);
                }
            }
            Action::DeleteVault => self.open_manage_form(FormPurpose::DeleteVault, arg.as_deref()),
            Action::NukeVault => self.open_manage_form(FormPurpose::NukeVault, arg.as_deref()),
            Action::ExportVault => self.open_export_form(arg.as_deref()),
            Action::ImportVault => self.open_import_form(arg.as_deref()),
            Action::Settings => self.open_settings(),
            Action::Sync => self.start_sync(arg.as_deref()),
            Action::ChangePassword => self.open_change_password_form(arg.as_deref()),
            Action::Memo => self.open_memo(),
            Action::RagToken => self.copy_api_token(),
            Action::LoginToken => self.copy_login_token(arg.as_deref()),
        }
    }

    // ---- single-key verb dispatch (BRASS LEDGER keymap) ----------------

    /// Runs the canonical command bound to `action` on the current page.
    fn dispatch_action(&mut self, action: Action) {
        if let Some(name) = self.registry.command_for_action(self.page, action).map(|c| c.name.clone()) {
            self.run_command(&name, &[]);
        }
    }

    /// A "not built yet" note for the one verb whose behavior still lands in a
    /// later phase (rename = P3).
    fn note_coming_soon(&mut self) {
        let msg = match self.settings.lang {
            Lang::En => "coming soon",
            Lang::It => "in arrivo",
        };
        self.note(msg.to_string());
    }

    /// The verb `code` fires on `page` — the keymap read through the user's own
    /// shortcut choices, which is the only way this app asks the question (the
    /// keybar and the cheatsheet draw from the same resolution). The SAVED
    /// settings, never the settings-page draft: while that page is open it owns
    /// the keyboard anyway, and a half-cycled draft must not dispatch.
    fn verb_for(&self, page: Page, code: KeyCode) -> Option<crate::keymap::Verb> {
        crate::keymap::lookup(page, self.settings.lang, code, &self.settings)
    }

    /// Whether `verb` is still live on the current page, read through that same
    /// resolution. The chord aliases (Ctrl+S, Ctrl+E) and `Del` fire the very
    /// handlers the letters do, so a verb the user switched off has to go inert
    /// on them too — otherwise "off" would take the chip off the bar and leave
    /// the action reachable. The `:` palette is the way in that stays.
    fn verb_live(&self, verb: crate::keymap::Verb) -> bool {
        crate::keymap::effective_binds(self.page, &self.settings).iter().any(|b| b.verb == verb)
    }

    /// Dispatches a single-key [`Verb`](crate::keymap::Verb) to its handler. The
    /// hourly verbs reuse the exact same handlers the palette/commands drive.
    fn dispatch_verb(&mut self, verb: crate::keymap::Verb) {
        use crate::keymap::Verb;
        match verb {
            Verb::NewVault => self.dispatch_action(Action::CreateVault),
            Verb::RenameVault => self.note_coming_soon(),
            Verb::DeleteVault => self.dispatch_action(Action::DeleteVault),
            Verb::ImportVault => self.dispatch_action(Action::ImportVault),
            Verb::ExportVault => self.dispatch_action(Action::ExportVault),
            Verb::AddItem => self.enter_view(PanelView::Picker(PickerKind::ItemCategory)),
            Verb::EditItem => match self.highlighted_item_id() {
                Some(id) => self.open_edit_form(&id),
                None => self.note(i18n::t(self.settings.lang, &Key::NoItems)),
            },
            Verb::DeleteItem => self.open_delete_form(None),
            Verb::StarItem => self.star_item(None),
            // reveal toggles the docked detail's secrets (auto-rehide lands P3)
            Verb::Reveal => {
                if self.vault.as_ref().is_some_and(|v| !v.items().is_empty()) {
                    self.detail_reveal = !self.detail_reveal;
                }
            }
            // the three "look up → copy → leave" verbs the keybar advertises.
            // They read the item on show, so they behave the same from the list
            // and from the docked detail (which routes `u`/`o` straight here).
            Verb::CopyPassword => self.copy_item_row(
                crate::model::item_password_index,
                i18n::t(self.settings.lang, &Key::NoPasswordField),
            ),
            Verb::CopyUser => self.copy_item_row(
                crate::model::item_user_index,
                i18n::t(self.settings.lang, &Key::NoUserField),
            ),
            Verb::OpenSite => self.open_item_site(),
            Verb::Login => self.login_in_browser(),
            Verb::Memo => self.open_memo(),
            Verb::LockAndBack => self.exit_vault(),
            Verb::Settings => self.open_settings(),
        }
    }

    // ---- in-vault items ------------------------------------------------

    /// Resolves a typed category token to a known category id (custom defs in
    /// the open vault first, then the built-ins), case-insensitively.
    fn resolve_category(&self, token: &str) -> Option<String> {
        let t = token.trim().to_ascii_lowercase();
        if let Some(v) = &self.vault {
            if let Some(def) = v.content.categories.iter().find(|c| c.id.eq_ignore_ascii_case(&t)) {
                return Some(def.id.clone());
            }
        }
        crate::model::BUILTINS
            .iter()
            .find(|c| c.id.eq_ignore_ascii_case(&t))
            .map(|c| c.id.to_string())
    }

    /// The (key, field-type, secret) template for a category: a custom def in
    /// the open vault takes precedence over a built-in; an unknown id yields no
    /// preset fields (just the mandatory label).
    fn category_template(&self, category_id: &str) -> Vec<(String, crate::model::FieldType, bool)> {
        if let Some(v) = &self.vault {
            if let Some(def) = v.content.categories.iter().find(|c| c.id == category_id) {
                return def
                    .fields
                    .iter()
                    .map(|f| (f.key.clone(), f.ftype.clone(), f.secret))
                    .collect();
            }
        }
        crate::model::BUILTINS
            .iter()
            .find(|c| c.id == category_id)
            .map(|c| {
                c.fields
                    .iter()
                    .map(|f| (f.key.to_string(), f.ftype.clone(), f.secret))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Opens the Add form for `category_id`: a mandatory label plus one field
    /// per template slot (secret slots masked). Empty optional fields are simply
    /// not stored.
    fn open_add_form(&mut self, category_id: &str) {
        if self.refuse_if_open_vault_rekeying() {
            return;
        }
        let lang = self.settings.lang;
        let cat_name = i18n::t(lang, &Key::CategoryName(category_id));
        // display labels are localized; submit maps fields to keys BY POSITION
        // against the same template, so the field name here is only a label.
        let mut fields = vec![FormField::text(
            &i18n::t(lang, &Key::FieldLabel("label")),
            i18n::t(lang, &Key::ItemLabelHint),
            false,
        )];
        for (key, ftype, secret) in self.category_template(category_id) {
            // an image slot types a FILE PATH: shown as it is typed (a path is
            // not a secret) and hinted with the formats it takes; submit reads
            // the file and stores it as base64.
            let path_row = takes_a_path(&ftype);
            let hint = if path_row { i18n::t(lang, &Key::ImageFieldHint) } else { String::new() };
            let mut ff = FormField::text(&i18n::t(lang, &Key::FieldLabel(&key)), hint, secret && !path_row);
            // only a password slot gets the live meter (never cvv/pin/number/seed)
            ff.show_strength = shows_strength(ff.secret, &ftype);
            fields.push(ff);
        }
        // this specific item may need something extra: the growing add-field row
        fields.push(FormField::adder(lang));
        let form = FormState {
            command: "add".to_string(),
            title: i18n::t(lang, &Key::AddItemTitle(&cat_name)),
            purpose: FormPurpose::AddItem(category_id.to_string()),
            fields,
            focused: 0,
            error: None,
        };
        self.open_form(form);
    }

    /// Builds an item from the Add form and saves it. The label (field 0) is
    /// mandatory; every other field is optional and omitted when empty.
    fn submit_add_item(&mut self, category_id: &str) {
        let lang = self.settings.lang;
        let label = self
            .form
            .as_ref()
            .and_then(|f| f.fields.first())
            .map(|f| f.value.trim().to_string())
            .unwrap_or_default();
        if label.is_empty() {
            if let Some(form) = self.form.as_mut() {
                let l = i18n::t(lang, &Key::FieldLabel("label"));
                form.error = Some(i18n::t(lang, &Key::FieldRequired(&l)));
            }
            return;
        }
        // map form fields to DCS-1 keys/types BY POSITION against the template
        // (the form's field-0 is the label; fields[1..] line up with the template
        // in order), so the displayed labels can be freely localized.
        let template = self.category_template(category_id);
        let mut item = crate::model::Item::new(category_id, &label);
        // a refused import (missing file, oversize, wrong format) stops the
        // whole submit: nothing is stored and the form says why.
        let mut refused: Option<String> = None;
        if let Some(form) = self.form.as_ref() {
            for (spec, field) in template.iter().zip(form.fields.iter().skip(1)) {
                let value = field.value.trim();
                // `-` on a picture slot means "remove" where there IS something
                // to remove; on the ADD form there never is, so it just leaves
                // the slot empty — the same thing blank does.
                if value.is_empty() || removes_the_value(&spec.1, value) {
                    continue; // empty optional fields are not stored
                }
                let (value, extra) = match stored_from_form(lang, &spec.1, value) {
                    Ok(v) => v,
                    Err(e) => {
                        refused = Some(e);
                        break;
                    }
                };
                item.fields.push(crate::model::ItemField {
                    key: spec.0.clone(),
                    ftype: spec.1.clone(),
                    label: None,
                    value: crate::model::SecretString::new(value),
                    secret: spec.2,
                    extra,
                });
            }
            // beyond the template: this item's own extra fields, added via the
            // `+ add field` rows (converted in place; trailing adder skipped)
            for field in form.fields.iter().skip(1 + template.len()) {
                let value = field.value.trim();
                if refused.is_some() {
                    break;
                }
                if field.adder || value.is_empty() {
                    continue;
                }
                let ftype = implied_ftype(&field.name);
                let (value, extra) = match stored_from_form(lang, &ftype, value) {
                    Ok(v) => v,
                    Err(e) => {
                        refused = Some(e);
                        break;
                    }
                };
                item.fields.push(crate::model::ItemField {
                    key: crate::model::slugify(&field.name),
                    label: Some(field.name.clone()),
                    value: crate::model::SecretString::new(value),
                    // an image row is typed unmasked (it is a path) but the
                    // field it makes is secret — see FieldType::Image
                    secret: field.secret || ftype.default_secret(),
                    ftype,
                    extra,
                });
            }
        }
        if let Some(message) = refused {
            if let Some(form) = self.form.as_mut() {
                form.error = Some(message);
            }
            return;
        }

        let Some(vault) = self.vault.as_mut() else { return };
        vault.items_mut().push(item);
        vault.content.bump_sync_counter("local");
        match vault.save() {
            Ok(()) => {
                self.form = None;
                self.go_home();
                self.focus = Focus::Vaults;
                self.ok(i18n::t(lang, &Key::ItemAdded(&label)));
                self.auto_sync_after_change();
            }
            Err(_) => {
                // roll back the in-memory push so the view matches the disk
                if let Some(v) = self.vault.as_mut() {
                    v.items_mut().pop();
                }
                if let Some(form) = self.form.as_mut() {
                    form.error = Some(i18n::t(lang, &Key::CreateFailed));
                }
            }
        }
    }

    /// The id of the highlighted VISIBLE item, if any — read off
    /// [`highlighted_item`](App::highlighted_item), so a verb can never act on a
    /// different item than the one the pane is drawing (an edit can drop its own
    /// item out of the active filter, leaving `selected` past the end).
    fn highlighted_item_id(&self) -> Option<String> {
        self.highlighted_item().map(|i| i.id.clone())
    }

    /// Pulls `selected` back onto the list after a change that can shrink it (an
    /// edit whose item no longer matches the query, say) — the delete path's
    /// idiom, so the ledger's cursor and the docked detail stay on one row.
    fn clamp_selection(&mut self) {
        self.selected = self.selected.min(self.left_len().saturating_sub(1));
    }

    /// Re-points the cursor after something OTHER than a keystroke reordered (or
    /// re-narrowed) the visible list — a semantic ranking landing on a tick,
    /// seconds after the query was typed, or the fall back to the literal filter
    /// when the embedding failed. Every path the USER drives resets the cursor
    /// itself; this one has no key press to hang that on, so the item that was
    /// under the cursor keeps it wherever it moved to, and only one that is no
    /// longer visible at all sends the cursor home (detail and inline edit with
    /// it). A verb must never fire on a row nobody chose.
    fn keep_cursor_on(&mut self, id: Option<String>) {
        let visible = self.visible_item_indices();
        let at = match (id, self.vault.as_ref()) {
            (Some(id), Some(vault)) => visible
                .iter()
                .position(|&i| vault.items().get(i).is_some_and(|item| item.id == id)),
            _ => None,
        };
        match at {
            Some(at) => self.selected = at,
            None => {
                self.selected = 0;
                self.reset_detail();
            }
        }
    }

    /// The highlighted VISIBLE item — the one the docked detail is showing.
    /// Clamped exactly like the renderer, so the two never disagree.
    pub fn highlighted_item(&self) -> Option<&crate::model::Item> {
        let vault = self.vault.as_ref()?;
        let visible = self.visible_item_indices();
        let idx = *visible.get(self.selected.min(visible.len().checked_sub(1)?))?;
        vault.items().get(idx)
    }

    // ---- the docked detail: field cursor, copy, inline edit -------------

    /// Whether the docked detail owns the keyboard right now: inside a vault,
    /// on the resting view, with focus moved right by ⏎ on the list — and with
    /// nothing layered over it (a form, the palette, the settings page and the
    /// lock all take the keyboard first, so the detail must not claim it, nor
    /// paint a cursor band under them).
    pub fn detail_focused(&self) -> bool {
        self.page == Page::Vault
            && self.view == PanelView::Splash
            && self.focus == Focus::Panel
            && !self.search_focus
            && self.vault.is_some()
            && self.form.is_none()
            && self.palette.is_none()
            && self.settings_ui.is_none()
            && self.locked.is_none()
    }

    /// The detail's field rows for the highlighted item — see
    /// [`detail_field_rows`], the single source the renderer draws from.
    pub fn detail_rows(&self) -> Vec<usize> {
        self.highlighted_item().map(detail_field_rows).unwrap_or_default()
    }

    /// The cursor, clamped to the rows that actually exist.
    pub fn detail_index(&self) -> usize {
        self.detail_cursor.min(self.detail_rows().len().saturating_sub(1))
    }

    /// Which of the item's `fields` the cursor points at (`None` = no rows).
    fn detail_field_index(&self) -> Option<usize> {
        let rows = self.detail_rows();
        rows.get(self.detail_cursor.min(rows.len().checked_sub(1)?)).copied()
    }

    /// Drops the detail cursor and any inline edit (whose buffer is wiped on
    /// drop). Called wherever the highlighted item — or the whole surface —
    /// can change under it: selection, tab, search, view, lock, exit.
    fn reset_detail(&mut self) {
        self.detail_cursor = 0;
        self.detail_edit = None;
    }

    /// ⏎ on an item row: move the keyboard onto the docked detail.
    fn focus_detail(&mut self) {
        self.focus = Focus::Panel;
        self.detail_cursor = 0;
        self.detail_edit = None;
        self.status = None;
    }

    fn move_detail_cursor(&mut self, delta: isize) {
        let n = self.detail_rows().len();
        if n > 0 {
            self.detail_cursor = (self.detail_index() as isize + delta).rem_euclid(n as isize) as usize;
        }
    }

    /// Keys while the docked detail has focus; `true` = consumed. An inline
    /// edit is tested FIRST: inside it every bare character is text, never a
    /// verb. Outside it the arrows move the field cursor and the LOCALIZED
    /// copy/edit/delete/star keys ([`keymap`](crate::keymap)) act on the shown
    /// item; modifier chords are handed back so the global ones (Ctrl+L,
    /// Ctrl+E, Ctrl+S, Ctrl+F) keep working.
    fn on_detail_key(&mut self, code: KeyCode, mods: KeyModifiers) -> bool {
        use crate::keymap::Verb;
        if self.detail_edit.is_some() {
            // ←/→/Home/End move the caret and Backspace/Delete edit at it, so a
            // typo in the middle of a value is fixed in place (Backspace before
            // the caret, Delete the char on it).
            if let Some(edit) = self.detail_edit.as_mut() {
                if crate::textinput::edit_key(&mut edit.buffer, code)
                    != crate::textinput::Edit::Ignored
                {
                    return true;
                }
            }
            match code {
                KeyCode::Esc => self.cancel_detail_edit(),
                KeyCode::Enter => self.commit_detail_edit(),
                // Modifier chords (Ctrl+U/W/V, Alt+…) must not be typed as
                // their bare letter — the rule the form path follows too.
                KeyCode::Char(ch)
                    if !mods.intersects(
                        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                    ) =>
                {
                    if let Some(edit) = self.detail_edit.as_mut() {
                        edit.buffer.insert(ch);
                    }
                }
                _ => {}
            }
            return true;
        }
        if mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER) {
            return false;
        }
        match code {
            KeyCode::Up => self.move_detail_cursor(-1),
            KeyCode::Down => self.move_detail_cursor(1),
            // Esc hands the keyboard back to the list; closing the vault is the
            // NEXT Esc, from there (the ladder never skips a rung).
            KeyCode::Esc => {
                self.focus = Focus::Vaults;
                self.reset_detail();
            }
            KeyCode::Enter => self.start_detail_edit(),
            // vim's own down/up. They have to be answered HERE: the fall-through
            // lands them in `move_selection`, whose panel branch counts the
            // picker rows (zero on the resting view) — so j/k would simply go
            // dead the moment ⏎ handed the keyboard to the detail. Bare only:
            // the modifier chords were handed back above, and inside an inline
            // edit (returned from earlier) a `j` is a typed letter.
            KeyCode::Char('j') => self.move_detail_cursor(1),
            KeyCode::Char('k') => self.move_detail_cursor(-1),
            // the item verbs keep working from here, on the item on show — but
            // through the KEYMAP, never a hardcoded letter: the mnemonics are
            // per-locale (in `it` copy is `c`, edit `m`, delete `e`, star `p`),
            // so a literal 'e' here would mean "modifica" in the detail while it
            // means "elimina" one ⏎ away on the list. Anything the keymap
            // doesn't bind to these four falls through to the global handling.
            KeyCode::Char(_) => match self.verb_for(Page::Vault, code) {
                // the detail owns the copy: it copies the row under the cursor,
                // not the item's password
                Some(Verb::CopyPassword) => self.copy_detail_field(),
                // …and on a picture `o` opens THAT picture rather than the
                // item's website: one key, whichever "open" the cursor is on.
                // Every other row falls through to the website (below).
                Some(Verb::OpenSite) if self.detail_on_image() => self.open_detail_image(),
                Some(Verb::EditItem) => match self.highlighted_item_id() {
                    Some(id) => self.open_edit_form(&id),
                    None => self.note(i18n::t(self.settings.lang, &Key::NoItems)),
                },
                Some(Verb::DeleteItem) => self.open_delete_form(None),
                Some(Verb::StarItem) => self.star_item(None),
                _ => return false,
            },
            _ => return false,
        }
        true
    }

    /// Whether the detail cursor is sitting on an image row. The one place that
    /// is decided, because TWO verbs read it: `c` refuses there, and `o` swaps
    /// the item's website for the picture itself.
    fn detail_on_image(&self) -> bool {
        match (self.detail_field_index(), self.highlighted_item()) {
            (Some(i), Some(item)) => item.fields.get(i).is_some_and(crate::image_field::is_image),
            _ => false,
        }
    }

    /// `c`: copies the row under the cursor. A TOTP row copies the CODE it
    /// generates right now, never the seed; a secret is copied without ever
    /// being drawn. What leaves the vault is decided by
    /// [`clipboard::copy_payload`](crate::clipboard::copy_payload).
    fn copy_detail_field(&mut self) {
        let lang = self.settings.lang;
        let now = unix_now();
        // an image is not text: say so rather than push a megabyte of base64
        // (which no paste target could use anyway) onto the clipboard.
        if self.detail_on_image() {
            self.note(i18n::t(lang, &Key::ImageNotCopyable));
            return;
        }
        let payload = match (self.detail_field_index(), self.highlighted_item()) {
            (Some(i), Some(item)) => crate::clipboard::copy_payload(item, i, now),
            _ => None,
        };
        let Some(payload) = payload else {
            self.note(i18n::t(lang, &Key::NothingToCopy));
            return;
        };
        self.copy_to_clipboard(payload);
    }

    /// The two item-level copy verbs (`c` password · `u` user), which act on the
    /// item ON SHOW rather than on a cursor row: `resolve` picks the field (see
    /// [`model::item_password_index`](crate::model::item_password_index)) and
    /// `missing` is what to say when the item simply hasn't got one. The bytes
    /// still come from [`clipboard::copy_payload`](crate::clipboard::copy_payload)
    /// — a `totp`-shaped row copies its live code here too — and they leave
    /// through the one clipboard path, wipe timer included.
    fn copy_item_row(
        &mut self,
        resolve: fn(&crate::model::Item) -> Option<usize>,
        missing: String,
    ) {
        let lang = self.settings.lang;
        let Some(item) = self.highlighted_item() else {
            self.note(i18n::t(lang, &Key::NoItems));
            return;
        };
        let Some(idx) = resolve(item) else {
            self.note(missing);
            return;
        };
        // the row is there but holds nothing: that is a different sentence
        let Some(payload) = crate::clipboard::copy_payload(item, idx, unix_now()) else {
            self.note(i18n::t(lang, &Key::NothingToCopy));
            return;
        };
        self.copy_to_clipboard(payload);
    }

    /// The ONE way anything reaches the system clipboard: bump the generation,
    /// remember the bytes (so the wipe can tell them from a later copy of
    /// somebody else's), arm the 20 s timer and say when it clears. Every copy
    /// verb lands here, so none of them can forget the wipe.
    fn copy_to_clipboard(&mut self, payload: Zeroizing<String>) {
        let lang = self.settings.lang;
        match self.clip.set_text(payload.as_str()) {
            Ok(()) => {
                self.copy_gen = self.copy_gen.wrapping_add(1);
                self.clip_armed = true;
                // the previous payload's buffer is wiped as it is replaced
                self.clip_payload = payload;
                self.arm_clipboard_clear();
                self.ok(i18n::t(lang, &Key::Copied(crate::clipboard::CLEAR_AFTER_SECS)));
            }
            // no display server, a locked-down session… never a panic
            Err(_) => self.warn(i18n::t(lang, &Key::CopyFailed)),
        }
    }

    /// `o`: hands the item's website to the desktop's browser. DETACHED on
    /// purpose — a TUI must never block on a browser starting up, and a chatty
    /// opener must never scribble over the frame, so its output goes to
    /// /dev/null and the child is not waited on. A missing opener (no desktop
    /// session, a bare TTY) is a toast, never a panic.
    fn open_item_site(&mut self) {
        let lang = self.settings.lang;
        if self.highlighted_item().is_none() {
            self.note(i18n::t(lang, &Key::NoItems));
            return;
        }
        let site = self
            .highlighted_item()
            .and_then(crate::model::item_website)
            .map(|s| s.to_string());
        let Some(site) = site else {
            self.note(i18n::t(lang, &Key::NoWebsiteField));
            return;
        };
        // a stored `amazon.it` is a site, not a relative file path: the opener
        // is given a real URL (see `model::website_url`).
        let url = crate::model::website_url(&site);
        match self.open_externally(&url) {
            Ok(()) => self.ok(i18n::t(lang, &Key::OpeningSite(&url))),
            Err(_) => self.warn(i18n::t(lang, &Key::OpenSiteFailed)),
        }
    }

    /// `o` with the cursor on an IMAGE row: shows the user that picture. The
    /// same key, the same promise ("apri"/"open") — what gets opened is simply
    /// whatever the cursor is on, the website everywhere else.
    ///
    /// A terminal cannot show a scan of a passport well enough to read it, and
    /// the whole point of storing one is getting it back out, so the picture is
    /// written to an owner-only file and handed to the desktop's viewer, which
    /// can zoom it and save it wherever the user wants it.
    ///
    /// That file is a DECRYPTED secret on the disk. It is written before the
    /// opener is spawned (a failed spawn must not leave a phantom path
    /// untracked), its path is remembered the moment it exists, and the toast
    /// says out loud that it is temporary — [`App::purge_exports`] takes it back
    /// on lock, on close and on quit. Nothing else in the app exports anything.
    fn open_detail_image(&mut self) {
        let lang = self.settings.lang;
        let Some(path) = self.export_detail_image() else { return };
        match self.open_externally(&path.to_string_lossy()) {
            Ok(()) => self.ok(i18n::t(lang, &Key::ImageOpened)),
            Err(_) => self.warn(i18n::t(lang, &Key::ImageOpenFailed)),
        }
    }

    /// The half of [`App::open_detail_image`] that touches the disk: writes the
    /// hovered picture out, remembers the path so it can be taken back, and
    /// hands it over. `None` (with the refusal already on screen) when there is
    /// nothing to write. Separate from the spawn so the write is provably
    /// complete — and provably tracked — before any opener exists.
    pub(crate) fn export_detail_image(&mut self) -> Option<std::path::PathBuf> {
        let lang = self.settings.lang;
        // the whole export happens inside the item borrow, so the base64 is
        // never copied into a second buffer just to be handed along.
        let exported = match (self.detail_field_index(), self.highlighted_item()) {
            (Some(i), Some(item)) => item
                .fields
                .get(i)
                .filter(|f| crate::image_field::is_image(f))
                .map(|f| {
                    let (mime, _) = crate::image_field::meta(f);
                    crate::image_field::export_for_viewing(
                        f.value.as_str(),
                        mime.as_deref(),
                        &item.label,
                        &f.key,
                        &item.id,
                    )
                }),
            _ => None,
        };
        // the caller only routes here from an image row, so this is the
        // "the row went away under us" arm, not a normal outcome
        let Some(exported) = exported else {
            self.note(i18n::t(lang, &Key::NoItems));
            return None;
        };
        match exported {
            // tracked the moment it exists, and before anything is spawned: it
            // is ours to delete whether or not a viewer ever opens it.
            Ok(path) => {
                self.exported_images.push(path.clone());
                Some(path)
            }
            Err(err) => {
                self.warn(err.message(lang));
                None
            }
        }
    }

    /// Starts the countdown that wipes THIS copy: a detached thread sleeps, then
    /// reports the generation it was armed with. The decision stays on the main
    /// thread (see [`App::poll_clipboard`]) — a worker can't know whether a
    /// newer copy has landed meanwhile.
    fn arm_clipboard_clear(&mut self) {
        if self.clip_tx.is_none() {
            let (tx, rx) = std::sync::mpsc::channel();
            self.clip_tx = Some(tx);
            self.clip_rx = Some(rx);
        }
        let Some(tx) = self.clip_tx.clone() else { return };
        let generation = self.copy_gen;
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(crate::clipboard::CLEAR_AFTER_SECS));
            // a dead receiver just means the app closed — nothing to wipe
            let _ = tx.send(generation);
        });
    }

    /// Applies any due clipboard wipe (called each tick, like `poll_sync`).
    fn poll_clipboard(&mut self) {
        let mut due = false;
        if let Some(rx) = self.clip_rx.as_ref() {
            while let Ok(generation) = rx.try_recv() {
                // ONLY our still-current copy is wiped: anything newer on the
                // clipboard (ours or another app's) is not ours to erase.
                due |= generation == self.copy_gen;
            }
        }
        if due {
            self.wipe_clipboard();
        }
    }

    /// Wipes a copy of ours off the system clipboard NOW, whatever the timer
    /// says: the timer is a detached thread, so locking or quitting inside its
    /// 20 s window would otherwise leave the secret pasteable (and picked up by
    /// a clipboard manager on exit). Bumping the generation retires the pending
    /// timer, so it can never wipe a LATER copy in its place.
    ///
    /// The generation only knows about OUR copies, though, and the clipboard is
    /// shared: in those 20 s the user may well have copied a paragraph, an
    /// address, another app's password. So the bytes are read BACK first and
    /// erased only while they are still, byte for byte, the ones we put there
    /// ([`clipboard::should_clear`](crate::clipboard::should_clear)).
    ///
    /// And when the read FAILS we deliberately clear nothing: we cannot see
    /// whose data is on the clipboard, and a password manager that erases on a
    /// guess eats the user's work. The cost is a secret that may outlive its
    /// window on a clipboard we can't talk to; the alternative is destroying
    /// data that was never ours, which is worse and unrecoverable.
    fn wipe_clipboard(&mut self) {
        // `clip_payload` is set and cleared with `clip_armed`, so there is
        // nothing stored to zeroize on this path either (and App's Drop wipes
        // the buffer regardless — it is a `Zeroizing`).
        if !self.clip_armed {
            return;
        }
        let current = self.clip.get_text().ok();
        if crate::clipboard::should_clear(
            current.as_ref().map(|s| s.as_str()),
            self.clip_payload.as_str(),
        ) {
            self.clip.clear();
        }
        // the read-back is a copy of the secret too: drop it (zeroized) here,
        // and the stored payload with it — armed or not, it is done with.
        drop(current);
        self.clip_armed = false;
        self.clip_payload = Zeroizing::new(String::new());
        self.copy_gen = self.copy_gen.wrapping_add(1);
    }

    /// ⏎ on a field row: opens the inline edit in place. A plain row starts
    /// pre-filled (cursor at the end); a SECRET row starts EMPTY and confirming
    /// it empty keeps the stored value — the edit form's exact contract.
    fn start_detail_edit(&mut self) {
        if self.refuse_if_open_vault_rekeying() {
            return;
        }
        let lang = self.settings.lang;
        let Some(field_idx) = self.detail_field_index() else {
            self.note(i18n::t(lang, &Key::ItemDetailEmpty));
            return;
        };
        // WHAT is being edited is pinned here, by identity: the id of the item
        // and the key of the row. The commit re-resolves both, so a sync pull
        // landing mid-edit can't hand the buffer to whatever ended up at this
        // index instead (see `commit_detail_edit`).
        let Some(item_id) = self.highlighted_item_id() else { return };
        let Some(field_key) = self
            .highlighted_item()
            .and_then(|item| item.fields.get(field_idx))
            .map(|f| f.key.clone())
        else {
            return;
        };
        let prefill = self
            .highlighted_item()
            .and_then(|item| item.fields.get(field_idx))
            // an image row edits a PATH, so it starts empty too — its stored
            // base64 must never land in an input buffer
            .map(|f| {
                if f.secret || crate::image_field::is_image(f) {
                    String::new()
                } else {
                    f.value.as_str().to_string()
                }
            })
            .unwrap_or_default();
        self.detail_edit = Some(DetailEdit {
            field: field_idx,
            item: item_id,
            key: field_key,
            buffer: crate::textinput::TextInput::from(prefill),
        });
        self.status = None;
    }

    /// Esc while editing: the row goes back to showing the stored value and the
    /// typed buffer is wiped (Zeroizing's drop). Focus stays on the detail.
    fn cancel_detail_edit(&mut self) {
        self.detail_edit = None;
        self.status = None;
    }

    /// ⏎ while editing: writes the field through the standard mutation path
    /// (memory → sync counter → save → roll back on Err → auto-sync on Ok) and
    /// stamps `modified_at`, exactly as `submit_edit_item` does. An inline edit
    /// REWRITES a row (or, on a picture row typed `-`, removes it) — it never
    /// adds one, so the cursor still points inside the list it walked.
    fn commit_detail_edit(&mut self) {
        let lang = self.settings.lang;
        let Some(edit) = self.detail_edit.take() else { return };
        // armed before a re-encryption started: those bytes are on their way out
        if self.refuse_if_open_vault_rekeying() {
            return;
        }
        let typed = Zeroizing::new(edit.buffer.trim().to_string());
        // WHERE the buffer goes is resolved fresh, and by IDENTITY: `poll_sync`
        // can adopt a pulled content while a row is being typed, and the index
        // captured at edit start would then point at whatever ended up there —
        // another field of this item, or a field of another item entirely. So
        // the item is found by its id and the row by its key. (The captured
        // index is still preferred while it holds that same key: it is the
        // ordinary case, and the only thing that can tell duplicate keys apart.)
        let Some(idx) = self.vault.as_ref().and_then(|v| v.find_by_id(&edit.item)) else {
            self.warn(i18n::t(lang, &Key::FieldGone));
            return;
        };
        let resolved = {
            let fields = &self.vault.as_ref().unwrap().items()[idx].fields;
            if fields.get(edit.field).is_some_and(|f| f.key == edit.key) {
                Some(edit.field)
            } else {
                fields.iter().position(|f| f.key == edit.key)
            }
        };
        let Some(field_idx) = resolved else {
            self.warn(i18n::t(lang, &Key::FieldGone));
            return;
        };
        // a confirm that changes nothing never bumps the counter or rewrites the
        // file (a blank secret is exactly that: "keep what is stored").
        let (label, unchanged, ftype) = {
            let Some(field) = self
                .vault
                .as_ref()
                .and_then(|v| v.items()[idx].fields.get(field_idx))
            else {
                return;
            };
            // an image row keeps its picture when confirmed empty, exactly as a
            // secret row keeps its value
            let keeps_when_blank = field.secret || crate::image_field::is_image(field);
            let same =
                (keeps_when_blank && typed.is_empty()) || field.value.as_str() == typed.as_str();
            let label = field
                .label
                .clone()
                .unwrap_or_else(|| i18n::t(lang, &Key::FieldLabel(&field.key)));
            (label, same, field.ftype.clone())
        };
        // `-` on a picture row takes it off the item — asked BEFORE "unchanged",
        // so a row somehow holding a literal `-` is still removable.
        if removes_the_value(&ftype, typed.as_str()) {
            self.remove_item_field(idx, field_idx, &label);
            return;
        }
        if unchanged {
            self.note(i18n::t(lang, &Key::FieldUnchanged));
            return;
        }
        // an image row typed a file path: read + validate + encode BEFORE the
        // vault is touched, so a refused import changes nothing at all
        let (new_value, meta) = match stored_from_form(lang, &ftype, typed.as_str()) {
            Ok(v) => v,
            Err(message) => {
                self.warn(message);
                return;
            }
        };
        let vault = self.vault.as_mut().unwrap();
        let item = &mut vault.items_mut()[idx];
        let prev_value = std::mem::replace(
            &mut item.fields[field_idx].value,
            crate::model::SecretString::new(new_value),
        );
        let prev_extra = (!meta.is_empty()).then(|| {
            let field = &mut item.fields[field_idx];
            let previous = field.extra.clone();
            for (k, m) in meta {
                field.extra.insert(k, m);
            }
            previous
        });
        let prev_modified = item.modified_at.replace(crate::model::now_rfc3339());
        vault.content.bump_sync_counter("local");
        match vault.save() {
            Ok(()) => {
                if prev_extra.is_some() {
                    // a replaced picture: the render caches key on the value's
                    // LENGTH, which two different files can share
                    self.forget_item_images();
                }
                // the rewritten value may no longer match the active filter:
                // keep the cursor on a row that still exists
                self.clamp_selection();
                self.ok(i18n::t(lang, &Key::FieldUpdated(&label)));
                self.auto_sync_after_change();
            }
            Err(_) => {
                // roll the in-memory row back so the screen matches the disk
                let item = &mut self.vault.as_mut().unwrap().items_mut()[idx];
                item.fields[field_idx].value = prev_value;
                if let Some(extra) = prev_extra {
                    item.fields[field_idx].extra = extra;
                }
                item.modified_at = prev_modified;
                self.warn(i18n::t(lang, &Key::SaveFailed));
            }
        }
    }

    /// Takes one field OFF an item, through the same mutation path everything
    /// else uses (memory → sync counter → save → roll back on Err → auto-sync
    /// on Ok). The inline `-` on a picture row is the caller: a template slot
    /// simply goes back to being unfilled, and the add/edit form offers it again.
    fn remove_item_field(&mut self, idx: usize, field_idx: usize, label: &str) {
        let lang = self.settings.lang;
        let vault = self.vault.as_mut().unwrap();
        let item = &mut vault.items_mut()[idx];
        let removed = item.fields.remove(field_idx);
        let prev_modified = item.modified_at.replace(crate::model::now_rfc3339());
        vault.content.bump_sync_counter("local");
        match vault.save() {
            Ok(()) => {
                // the picture is gone from the vault: no decoded copy of it may
                // outlive it in the render caches
                self.forget_item_images();
                // one row fewer under the cursor — and the item may have left
                // the active filter along with the value that matched it
                let rows = self.detail_rows().len();
                self.detail_cursor = self.detail_cursor.min(rows.saturating_sub(1));
                self.clamp_selection();
                self.ok(i18n::t(lang, &Key::FieldRemoved(label)));
                self.auto_sync_after_change();
            }
            Err(_) => {
                // put it back so the screen matches the disk
                let item = &mut self.vault.as_mut().unwrap().items_mut()[idx];
                item.fields.insert(field_idx, removed);
                item.modified_at = prev_modified;
                self.warn(i18n::t(lang, &Key::SaveFailed));
            }
        }
    }

    // ---- memorandum -----------------------------------------------------

    /// Opens the Memorandum over the vault body. Only inside an open vault:
    /// the thread IS vault content, so there is nothing to show without one.
    /// Always lands pinned to the newest message.
    fn open_memo(&mut self) {
        if self.page != Page::Vault || self.vault.is_none() {
            return;
        }
        self.memo_open = true;
        self.memo_scroll = 0;
        // this thread has not been laid out yet: no bound to clamp against
        self.memo_max_scroll.set(usize::MAX);
        // a fresh room: no bar, no jump and no copy-band left from the last visit
        self.close_memo_search();
        self.memo_jump = None;
        self.memo_sel = None;
        // the composer is the only focus in here; the list's is meaningless
        self.search_focus = false;
        self.set_view(PanelView::Splash);
        self.status = None;
        // messages sent since the last pass have no vector yet; embedding them
        // now is what makes the thread searchable while it is on screen
        self.rag_index_memo();
    }

    /// Leaves the Memorandum, taking the composer's line with it — a message
    /// abandoned half-written is vault content that never reached the vault —
    /// and the search with it: a query says what you were looking for inside
    /// this vault, which is the vault's business and nobody else's.
    fn close_memo(&mut self) {
        self.memo_open = false;
        self.memo_scroll = 0;
        self.memo_max_scroll.set(usize::MAX);
        self.memo_input.zeroize();
        self.memo_input.clear();
        self.close_memo_search();
        self.memo_jump = None;
        self.memo_sel = None;
    }

    /// Puts the search bar away and wipes it. The worker that may still be
    /// embedding the query is abandoned in the same breath — its answer is
    /// about text that no longer exists anywhere on this machine.
    fn close_memo_search(&mut self) {
        if let Some(mut search) = self.memo_search.take() {
            search.wipe();
        }
        self.rag.cancel_query();
    }

    /// Scrolls the thread `rows` further back, and no further than the top of
    /// it. Geometry belongs to the renderer, so the bound is the one IT last
    /// laid out ([`App::memo_max_scroll`]): without it the offset kept growing
    /// past the top and the next few PgDn presses moved nothing at all.
    /// `usize::MAX` (nothing drawn yet) leaves the step unclamped.
    fn scroll_memo_back(&mut self, rows: usize) {
        self.memo_scroll = self.memo_scroll.saturating_add(rows).min(self.memo_max_scroll.get());
    }

    /// How many messages the open vault's Memorandum holds (0 without one).
    pub fn memo_len(&self) -> usize {
        self.vault.as_ref().map(|v| v.content.memo_len()).unwrap_or(0)
    }

    /// Keys while the Memorandum has the body. The composer is ALWAYS focused,
    /// so a bare letter is text — never a verb, never the `:`/`/`/`?` triggers
    /// (see the slot this sits in inside `on_key`). Everything else is scroll.
    fn on_memo_key(&mut self, code: KeyCode, mods: KeyModifiers) -> bool {
        // "is this a chord?" decides what is TEXT; the chips promise CONTROL
        // specifically, so each of them is matched on control alone.
        let chord = mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER);
        let ctrl = mods.contains(KeyModifiers::CONTROL);
        // Ctrl+F raises the search bar over the composer — and takes it away
        // again, so the key that opened it is also the way out of it.
        if ctrl && matches!(code, KeyCode::Char('f')) {
            if self.memo_search.is_some() {
                self.close_memo_search();
            } else {
                self.memo_search = Some(MemoSearch::new());
                // a fresh search owns the band through its own jump; a copy-pick
                // left over from before it would only fight it for the highlight
                self.memo_sel = None;
                // the messages written since the last pass have no vector yet;
                // starting here means the thread is usually caught up by the
                // time a query is typed (and `run_memo_search` waits if it
                // isn't, rather than searching an index that cannot see them)
                self.rag_index_memo();
            }
            return false;
        }
        // …and while it is up, the bar owns the keyboard: a letter is the query
        // being typed, not the message. (The composer's line is untouched
        // underneath, waiting for the Esc that gives it back.)
        if self.memo_search.is_some() {
            return self.on_memo_search_key(code, mods);
        }
        // Tab / Shift+Tab step OFF the memo's chip: the thread is a stop on the
        // chips row (still drawn above it), and moving to the next one takes
        // the view with it — Tab to ALL, Shift+Tab back to the last category.
        // Below the search bar on purpose: while THAT is up, Tab flips its mode.
        // A half-written message holds it back: leaving zeroizes the composer,
        // and Esc — the key that IS on the bar for this — deliberately refuses
        // to cost you the line and the view in one press, so a key the bar
        // never promised must not do it either.
        if matches!(code, KeyCode::Tab | KeyCode::BackTab) {
            if self.memo_input.is_empty() {
                self.cycle_vault_stop(if code == KeyCode::Tab { 1 } else { -1 });
            }
            return false;
        }
        // Ctrl+P attaches a photo: the same import rules an image FIELD uses,
        // asked for in a modal because a path is not a thing to type in a chat.
        if ctrl && matches!(code, KeyCode::Char('p')) {
            self.open_memo_photo_form();
            return false;
        }
        // Ctrl+Y copies the WHOLE of the selected message (Up/Down pick it) onto
        // the system clipboard, wipe timer and all — the terminal-independent way
        // to lift a multi-line note out in one keystroke, when a mouse selection
        // stops at one line. Not Ctrl+C (that quits), and out of the composer's
        // reach on purpose: every printable char is text down here.
        if ctrl && matches!(code, KeyCode::Char('y')) {
            self.copy_memo_selection();
            return false;
        }
        // any hand movement of the thread leaves BOTH the search jump and the
        // copy-band behind — the band marks what a search found or what a copy
        // will take, not wherever a page-scroll has since carried the eye.
        if matches!(code, KeyCode::PageUp | KeyCode::PageDown)
            || (ctrl && matches!(code, KeyCode::Up | KeyCode::Down))
        {
            self.memo_jump = None;
            self.memo_sel = None;
        }
        match code {
            // one screen of thread at a time. Ctrl+↑/↓ is the same verb for the
            // keyboards that have no PgUp — and going back stops AT the top of
            // the thread (see `scroll_memo_back`), so the way down is never a
            // string of presses that move nothing.
            KeyCode::PageUp => self.scroll_memo_back(MEMO_SCROLL_STEP),
            KeyCode::PageDown => self.memo_scroll = self.memo_scroll.saturating_sub(MEMO_SCROLL_STEP),
            KeyCode::Up if ctrl => self.scroll_memo_back(1),
            KeyCode::Down if ctrl => self.memo_scroll = self.memo_scroll.saturating_sub(1),
            // bare ↑/↓ (over an EMPTY composer) walk the copy-selection instead:
            // ↑ toward older messages, ↓ toward newer, entering the thread at the
            // newest on the first press. It never fights the scroll keys — those
            // are Ctrl+↑/↓ and PgUp/PgDn — and the band it moves is kept on
            // screen by the renderer. With text half-written a bare arrow is
            // nothing (the composer has no caret to move), so the note is safe.
            KeyCode::Up if self.memo_input.is_empty() => self.move_memo_selection(-1),
            KeyCode::Down if self.memo_input.is_empty() => self.move_memo_selection(1),
            // ←/→/Home/End walk the caret through the composed line(s) and
            // Backspace/Delete edit at it — a linear caret over the whole buffer,
            // newlines and all (Up/Down keep meaning message selection, above).
            // A caret move is not "typing", so it leaves the scroll/jump/band
            // where they are; only a text CHANGE is a return to the present.
            KeyCode::Left | KeyCode::Right | KeyCode::Home | KeyCode::End => {
                crate::textinput::edit_key(&mut self.memo_input, code);
            }
            KeyCode::Delete => {
                self.memo_input.delete();
            }
            // ⏎ with a modifier is a NEWLINE, not a send: a memo message is a
            // note to yourself, and those have paragraphs. Shift+⏎ is the chord
            // everybody's fingers know, and it only ever ARRIVES on a terminal
            // that speaks the kitty keyboard protocol (see `main`); Alt+⏎ is
            // the same verb on the terminals that cannot spell the first one,
            // so both are taken here and the keybar promises whichever works.
            KeyCode::Enter if mods.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) => {
                self.memo_input.insert('\n');
                // a newline is typing, and typing is a return to the present
                self.memo_scroll = 0;
                self.memo_jump = None;
                self.memo_sel = None;
            }
            KeyCode::Enter => self.send_memo_message(),
            KeyCode::Backspace => {
                self.memo_input.backspace();
            }
            // Esc empties the composer first, then leaves — the same two-stage
            // escape the command line has, and for the same reason: a line typed
            // by mistake should not also cost you the view.
            KeyCode::Esc => {
                if self.memo_input.is_empty() {
                    self.close_memo();
                } else {
                    // wipe, don't truncate: the buffer held vault content
                    self.memo_input.zeroize();
                    self.memo_input.clear();
                }
            }
            // Modifier chords are never typed as their bare letter (the form
            // path's rule): Ctrl+W must not leave a `w` in the message.
            KeyCode::Char(ch) if !chord => {
                self.memo_input.insert(ch);
                // typing is a return to the present, like every chat
                self.memo_scroll = 0;
                self.memo_jump = None;
                self.memo_sel = None;
            }
            _ => {}
        }
        false
    }

    // ---- searching the thread (Ctrl+F) -----------------------------------

    /// Keys while the search bar is up. It is a bar, not a page: the whole of
    /// what it does is type, flip the mode, walk the hits, jump, leave — and
    /// every one of those five is on the keybar while it is showing.
    fn on_memo_search_key(&mut self, code: KeyCode, mods: KeyModifiers) -> bool {
        let chord = mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER);
        match code {
            // Tab flips the mode and drops the results with it: a list the
            // OTHER search produced under a chip that now says this one would
            // be exactly the lie the chip exists to prevent.
            KeyCode::Tab | KeyCode::BackTab => {
                if let Some(search) = self.memo_search.as_mut() {
                    search.mode = search.mode.flipped();
                    search.invalidate();
                }
                self.rag.cancel_query();
            }
            // ⏎ runs the search, then walks off it: once there are hits it is
            // the jump instead, which is the only thing left to want here.
            KeyCode::Enter => {
                if self.memo_search.as_ref().is_some_and(|s| s.hits.is_empty()) {
                    self.run_memo_search();
                } else {
                    self.jump_to_memo_hit();
                }
            }
            KeyCode::Up => self.move_memo_hit(-1),
            KeyCode::Down => self.move_memo_hit(1),
            // Esc gives the composer back, with its half-written line still on
            // it — searching for something is not a reason to lose it.
            KeyCode::Esc => self.close_memo_search(),
            KeyCode::Char(ch) if !chord => {
                if let Some(search) = self.memo_search.as_mut() {
                    search.query.insert(ch);
                    search.invalidate();
                }
                self.rag.cancel_query();
            }
            // ←/→/Home/End move the caret; Backspace/Delete edit the query at it,
            // and an edited query drops the stale hits like a fresh keystroke.
            _ => {
                if let Some(search) = self.memo_search.as_mut() {
                    match crate::textinput::edit_key(&mut search.query, code) {
                        crate::textinput::Edit::Changed => {
                            search.invalidate();
                            self.rag.cancel_query();
                        }
                        crate::textinput::Edit::Moved | crate::textinput::Edit::Ignored => {}
                    }
                }
            }
        }
        false
    }

    /// Moves the highlighted hit by `delta`, saturating at both ends (a list of
    /// eight rows is not worth wrapping).
    fn move_memo_hit(&mut self, delta: isize) {
        let Some(search) = self.memo_search.as_mut() else { return };
        if search.hits.is_empty() {
            return;
        }
        let last = search.hits.len() - 1;
        search.selected = if delta < 0 {
            search.selected.saturating_sub(1)
        } else {
            (search.selected + 1).min(last)
        };
    }

    /// The open thread (empty without a vault) — what both searches look at.
    fn memo_thread(&self) -> Vec<crate::memo::MemoMessage> {
        self.vault.as_ref().map(|v| v.content.memo_messages()).unwrap_or_default()
    }

    /// Why a semantic search cannot run right now, localized — `None` when it
    /// can. Every branch is a reason to say out loud: the alternative is a
    /// search that silently does something other than what the chip promised.
    ///
    /// `scope_on` is the toggle that builds the index being searched —
    /// `rag_memo` for the thread, `rag_items` for the ledger. The two are
    /// independent: with only `items RAG` on, the engine is up and the THREAD
    /// has no vector at all, so a "RAG" run over it would be a truthful chip
    /// over an untruthful "no results". Both bars ask this one question.
    fn rag_unavailable(&self, scope_on: bool) -> Option<String> {
        use crate::rag::RagStatus;
        let lang = self.effective_lang();
        if self.rag.searchable() && scope_on {
            return None;
        }
        Some(match &self.rag.status {
            _ if !self.rag.enabled() || !scope_on => i18n::t(lang, &Key::SearchRagOff),
            RagStatus::Downloading => i18n::t(lang, &Key::SearchRagDownloading),
            RagStatus::Failed(fault) => i18n::t(lang, &Key::RagStatusFailed(&fault.message(lang))),
            _ => i18n::t(lang, &Key::SearchRagAbsent),
        })
    }

    /// Runs what the bar asks for. The literal search answers here and now; the
    /// semantic one goes to a worker, because embedding a query on a cold model
    /// is seconds and this is called from a key press on the 120 ms tick loop.
    ///
    /// A RAG search that CANNOT run is never a dead search: it runs literally
    /// instead, says why in the status line, and the chip then shows the mode
    /// that really ran. One that can, but over a thread that has messages
    /// nothing has embedded yet, WAITS for the pass instead of ranking without
    /// them (see [`App::resume_memo_search`]).
    fn run_memo_search(&mut self) {
        let Some(search) = self.memo_search.as_ref() else { return };
        if search.searching() {
            return; // one query is already out; the answer is what ⏎ is waiting on
        }
        let mut query = search.query.trim().to_string();
        if query.is_empty() {
            return;
        }
        if search.mode == SearchMode::Rag {
            match self.rag_unavailable(self.settings.rag_memo) {
                Some(why) => self.note(why),
                None => {
                    // a message written since the last pass carries no vector,
                    // and ranking against an index that cannot see it answers
                    // "no results" for a message the user is looking at. Catch
                    // the thread up first; `poll_rag` sends the query out on
                    // the tick that pass lands.
                    if self.memo_index_stale() {
                        query.zeroize(); // the copy this call made; the bar keeps its own
                        self.rag_index_memo();
                        if let Some(search) = self.memo_search.as_mut() {
                            search.hits.clear();
                            search.selected = 0;
                            search.ran = None;
                            search.pending = None;
                            search.indexing = true;
                        }
                        return;
                    }
                    self.start_memo_query(query);
                    return;
                }
            }
        }
        let hits = self.literal_hits(&query);
        query.zeroize(); // a search over a vault is vault-shaped text too
        self.finish_memo_search(hits, SearchMode::Literal);
    }

    /// Sends the query to the embedding worker and puts the bar in its
    /// "searching…" state.
    fn start_memo_query(&mut self, query: String) {
        let seq = self.rag.start_query(query);
        if let Some(search) = self.memo_search.as_mut() {
            search.pending = Some(seq);
            search.hits.clear();
            search.selected = 0;
            search.ran = None;
            search.indexing = false;
        }
    }

    /// Whether the thread holds a message the semantic index cannot see yet —
    /// which is EVERY message written since the last pass, because the
    /// Memorandum is not indexed per keystroke-sized message (see
    /// [`App::auto_sync_after_change`]).
    fn memo_index_stale(&self) -> bool {
        let tag = self.rag.tag();
        self.vault.as_ref().is_some_and(|v| {
            !crate::rag::memo_pending(&v.content.memo_messages(), tag).is_empty()
        })
    }

    /// Sends the search that was waiting for the thread to be caught up, once
    /// it is — or once the pass that was going to catch it up is over anyway. A
    /// search that waited forever would be worse than one ranked over what
    /// there is, so a failed pass releases it too.
    fn resume_memo_search(&mut self) {
        if !self.memo_search.as_ref().is_some_and(|s| s.indexing) {
            return;
        }
        if self.memo_index_stale() && self.rag.indexing() {
            return; // the pass is still out: keep waiting, and keep saying so
        }
        let mut query =
            self.memo_search.as_ref().map(|s| s.query.trim().to_string()).unwrap_or_default();
        if query.is_empty() {
            query.zeroize();
            if let Some(search) = self.memo_search.as_mut() {
                search.indexing = false;
            }
            return;
        }
        self.start_memo_query(query);
    }

    /// The literal hits for `query`, capped to what the list shows. The text
    /// matched is the one the index embeds (words plus a photo's caption, never
    /// the picture), so the two searches look at exactly the same words.
    fn literal_hits(&self, query: &str) -> Vec<MemoHit> {
        let messages = self.memo_thread();
        let items: Vec<(String, String)> = messages
            .iter()
            .map(|m| (m.id.clone(), crate::rag::memo_embed_text(m)))
            .collect();
        let offset = crate::model::local_utc_offset();
        crate::rag::search_literal(&items, query)
            .into_iter()
            .take(MEMO_SEARCH_HITS)
            .map(|hit| {
                let time = messages
                    .iter()
                    .find(|m| m.id == hit.id)
                    .map(|m| memo_hhmm(&m.ts, offset))
                    .unwrap_or_default();
                MemoHit { id: hit.id, time, snippet: hit.snippet, score: None }
            })
            .collect()
    }

    /// Hands a finished run to the bar: the hits, and the mode that produced
    /// them (which the chip then shows, fallback or not).
    fn finish_memo_search(&mut self, hits: Vec<MemoHit>, ran: SearchMode) {
        let Some(search) = self.memo_search.as_mut() else { return };
        search.hits = hits;
        search.selected = 0;
        search.ran = Some(ran);
        search.pending = None;
    }

    /// Ranks the thread against a query vector the worker just finished. An
    /// answer for a search that has moved on — the bar closed, the query was
    /// re-typed, the mode was flipped — is dropped: `seq` is what says so.
    fn finish_memo_query(&mut self, seq: u64, query: Vec<f32>) {
        if self.memo_search.as_ref().and_then(|s| s.pending) != Some(seq) {
            return;
        }
        let messages = self.memo_thread();
        let index = crate::rag::memo_index(&messages);
        let offset = crate::model::local_utc_offset();
        let hits = crate::rag::search_semantic(&index, &query, MEMO_SEARCH_HITS)
            .into_iter()
            .map(|hit| {
                let (time, snippet) = messages
                    .iter()
                    .find(|m| m.id == hit.id)
                    .map(|m| {
                        (memo_hhmm(&m.ts, offset), crate::rag::preview(&crate::rag::memo_embed_text(m)))
                    })
                    .unwrap_or_default();
                MemoHit { id: hit.id, time, snippet, score: Some(hit.score) }
            })
            .collect();
        self.finish_memo_search(hits, SearchMode::Rag);
    }

    /// A query embedding that failed: say why — localized, whether the reason
    /// is ours or the engine's — and answer the search literally, so the user
    /// gets a result either way.
    fn fall_back_to_literal(&mut self, seq: u64, fault: crate::rag::RagFault) {
        if self.memo_search.as_ref().and_then(|s| s.pending) != Some(seq) {
            return;
        }
        let lang = self.effective_lang();
        self.warn(i18n::t(lang, &Key::RagStatusFailed(&fault.message(lang))));
        let mut query =
            self.memo_search.as_ref().map(|s| s.query.trim().to_string()).unwrap_or_default();
        let hits = self.literal_hits(&query);
        query.zeroize();
        self.finish_memo_search(hits, SearchMode::Literal);
    }

    /// ⏎ on a hit: pin the thread to that message, band it, and put the bar
    /// away — the found message is the answer, and the bar has nothing left to
    /// say about it.
    fn jump_to_memo_hit(&mut self) {
        let Some(id) = self
            .memo_search
            .as_ref()
            .and_then(|s| s.hits.get(s.selected))
            .map(|hit| hit.id.clone())
        else {
            return;
        };
        self.memo_jump = Some(id);
        self.memo_scroll = 0; // the jump is the scroll now (the renderer pins it)
        self.close_memo_search();
    }

    /// Sends what the composer holds, newlines and all — a message is as many
    /// lines as were typed into it. An empty (or blank) buffer sends nothing —
    /// there is no such message. A refused save KEEPS the text, so the user can
    /// try again instead of retyping it.
    fn send_memo_message(&mut self) {
        let text = self.memo_input.trim().to_string();
        if text.is_empty() {
            return;
        }
        if self.append_memo_message(crate::memo::MemoMessage::new(text, None)) {
            self.memo_input.zeroize();
            self.memo_input.clear();
            self.memo_scroll = 0;
            self.memo_jump = None; // a send is a return to the present
            self.memo_sel = None; // …and the copy-band goes with it
        }
    }

    /// Walks the copy-selection `delta` messages over the READABLE thread
    /// (−1 = older/up, +1 = newer/down), entering it at the newest message on
    /// the first press and clamping at either end. The band both shows what
    /// Ctrl+Y will copy and pulls the thread to keep the pick on screen (the
    /// renderer's `memo_top`, shared with a search jump — so a stale jump is
    /// dropped here, this being the hand that now drives the band). A thread
    /// with nothing readable in it has nothing to select.
    fn move_memo_selection(&mut self, delta: i32) {
        // borrowed count, no photo deep-copied: `memo_rows` and `memo_messages`
        // never disagree about which messages exist, and the index is into both
        let len = match self.vault.as_ref() {
            Some(v) => v.content.memo_rows().len(),
            None => 0,
        };
        if len == 0 {
            return;
        }
        let last = len - 1;
        let next = match self.memo_sel.map(|i| i.min(last)) {
            None => last, // newest-focused by default: the first press lands here
            Some(i) => (i as i64 + delta as i64).clamp(0, last as i64) as usize,
        };
        self.memo_sel = Some(next);
        self.memo_jump = None;
    }

    /// Copies the FULL text of the selected message — every line — onto the
    /// system clipboard, down the one clipboard path (wipe timer, "copied"
    /// toast, generation bump included). Without a live selection it takes the
    /// NEWEST message, which is what a bare "copy" almost always means, and
    /// lands the band on it so what left the vault is the thing now highlighted.
    /// A photo copies its caption (or a localized note) and never its base64.
    fn copy_memo_selection(&mut self) {
        let lang = self.settings.lang;
        let sel = self.memo_sel;
        // the target index rides the borrowed row count (no photo deep-copied);
        // `memo_messages` is asked for only to build the one payload we take
        let picked: Option<(usize, String)> = self.vault.as_ref().and_then(|v| {
            let len = v.content.memo_rows().len();
            if len == 0 {
                return None;
            }
            let idx = sel.unwrap_or(len - 1).min(len - 1);
            let messages = v.content.memo_messages();
            Some((idx, crate::memo::memo_copy_payload(messages.get(idx)?, lang)))
        });
        let Some((idx, payload)) = picked else {
            self.note(i18n::t(lang, &Key::NothingToCopy));
            return;
        };
        self.memo_sel = Some(idx); // band what was copied
        if payload.is_empty() {
            self.note(i18n::t(lang, &Key::NothingToCopy));
            return;
        }
        self.copy_to_clipboard(Zeroizing::new(payload));
    }

    /// Opens the attach-photo modal over the thread.
    fn open_memo_photo_form(&mut self) {
        if self.refuse_if_open_vault_rekeying() {
            return;
        }
        let form = FormState::memo_photo(self.settings.lang);
        self.open_form(form);
    }

    /// Sends the photo the modal names, with its caption. The path goes through
    /// the image-field import rules (size, format, a file that really decodes),
    /// so a refusal is that machinery's own localized message on the form's
    /// error line — the modal stays open on the row that was wrong.
    fn submit_memo_photo(&mut self) {
        let lang = self.settings.lang;
        let Some((path, caption)) = self
            .form
            .as_ref()
            .filter(|f| f.fields.len() >= 2)
            .map(|f| (f.fields[0].value.trim().to_string(), f.fields[1].value.trim().to_string()))
        else {
            return;
        };
        if path.is_empty() {
            if let Some(form) = self.form.as_mut() {
                let name = i18n::t(lang, &Key::MemoPhotoField);
                form.error = Some(i18n::t(lang, &Key::FieldRequired(&name)));
            }
            return;
        }
        let imported = match crate::image_field::import(&resolve_path(&path)) {
            Ok(img) => img,
            Err(e) => {
                if let Some(form) = self.form.as_mut() {
                    form.error = Some(e.message(lang, &path));
                }
                return;
            }
        };
        let photo = crate::memo::MemoImage::from_import(&imported, (!caption.is_empty()).then_some(caption));
        if self.append_memo_message(crate::memo::MemoMessage::new("", Some(photo))) {
            self.form = None;
            self.focus = Focus::Vaults;
            self.memo_scroll = 0;
            self.memo_sel = None; // a send returns to the present, band and all
        }
    }

    /// Appends one message to the open vault's Memorandum, down the same path
    /// every content mutation takes (memory → sync counter → save → roll back
    /// on Err → auto-sync on Ok). Returns whether it landed, so the composer can
    /// keep what the user typed when the save is refused.
    pub fn append_memo_message(&mut self, msg: crate::memo::MemoMessage) -> bool {
        if self.refuse_if_open_vault_rekeying() {
            return false;
        }
        let lang = self.settings.lang;
        let Some(vault) = self.vault.as_mut() else { return false };
        vault.content.push_memo_message(msg);
        vault.content.bump_sync_counter("local");
        match vault.save() {
            Ok(()) => {
                self.auto_sync_after_change();
                true
            }
            Err(_) => {
                // take the message back off so the thread matches the disk —
                // the exact inverse of the push, container included
                if let Some(v) = self.vault.as_mut() {
                    v.content.pop_memo_message();
                }
                self.warn(i18n::t(lang, &Key::SaveFailed));
                false
            }
        }
    }

    // ---- local semantic search (the two indexes) -------------------------

    /// Queues the embeddings the open vault is missing, memorandum first.
    /// Called where the content can have changed under us — on unlock, on
    /// memo-open and after every mutation — and cheap when it hasn't: nothing
    /// stale means no worker and no save at all.
    ///
    /// The two scopes share one engine and one worker slot, so this starts at
    /// most one pass; the tick that finishes it calls back here for the next.
    pub fn rag_reindex(&mut self) {
        self.rag_index_memo();
        self.rag_index_items();
    }

    /// Test hook: the deterministic engine AND the two toggles that own it —
    /// without them an index pass is correctly a no-op, which is not what a
    /// test of the index wants to prove.
    #[cfg(test)]
    pub fn use_mock_rag(&mut self) {
        self.settings.rag_memo = true;
        self.settings.rag_items = true;
        self.rag.use_mock();
    }

    /// The Memorandum's pass (gated on `settings.rag_memo`).
    pub fn rag_index_memo(&mut self) {
        if !self.rag.enabled() || !self.settings.rag_memo {
            return;
        }
        let tag = self.rag.tag();
        let Some(vault) = self.vault.as_ref() else { return };
        let pending = crate::rag::memo_pending(&vault.content.memo_messages(), tag);
        if pending.is_empty() {
            return;
        }
        let name = vault.username.clone();
        self.rag.start_index(name, crate::rag::IndexScope::Memo, pending);
    }

    /// The items' pass (gated on `settings.rag_items`). The texts it embeds are
    /// built by [`App::item_index_texts`], which is the only place item text is
    /// assembled and which cannot see a secret value.
    pub fn rag_index_items(&mut self) {
        if !self.rag.enabled() || !self.settings.rag_items {
            return;
        }
        let tag = self.rag.tag();
        let Some(vault) = self.vault.as_ref() else { return };
        let texts = self.item_index_texts();
        let stored = vault.content.item_vectors();
        let pending = crate::rag::items_pending(&texts, tag, &stored);
        if pending.is_empty() {
            return;
        }
        let name = vault.username.clone();
        self.rag.start_index(name, crate::rag::IndexScope::Items, pending);
    }

    /// The `(item id, indexed text)` pairs of the open vault, localized. NO
    /// SECRET VALUE IS IN HERE: `rag::item_texts` builds every string through
    /// `rag::indexable_fields`, which hands the builder `None` for the value of
    /// any field flagged `secret` — an embedding partially inverts, so indexing
    /// a password would be storing it a second time, in a weaker form.
    pub fn item_index_texts(&self) -> Vec<(String, String)> {
        let lang = self.settings.lang;
        let Some(vault) = self.vault.as_ref() else { return Vec::new() };
        crate::rag::item_texts(vault.items(), |id| i18n::t(lang, &Key::CategoryName(id)))
    }

    /// Applies anything the embedding workers finished (called each tick).
    fn poll_rag(&mut self) {
        use crate::rag::RagOutcome;
        for outcome in self.rag.poll() {
            match outcome {
                RagOutcome::ModelReady => {
                    self.ok(i18n::t(self.settings.lang, &Key::RagModelReady));
                    // the content that was unsearchable while the model was
                    // missing is the first thing worth indexing now
                    self.rag_reindex();
                }
                RagOutcome::ModelFailed(fault) | RagOutcome::IndexFailed { fault, .. } => {
                    let lang = self.settings.lang;
                    self.warn(i18n::t(lang, &Key::RagStatusFailed(&fault.message(lang))));
                }
                RagOutcome::Indexed { vault, scope, vectors } => {
                    self.write_rag_vectors(vault, scope, vectors);
                    // one worker slot, two scopes: whatever is still stale
                    // (usually the other index) gets its turn now. A batch that
                    // had to WAIT for a re-key is still the pending one, and
                    // re-queuing it here would embed the same text twice.
                    if !self.rag.has_deferred() {
                        self.rag_reindex();
                    }
                }
                // one worker slot, two boxes that can want it (never at the same
                // time: the thread takes the whole body, `/` lives on the list).
                // Each answer goes to whoever is still waiting on that seq.
                RagOutcome::Query { seq, vec } => {
                    if self.vault_search.pending == Some(seq) {
                        self.finish_vault_query(seq, vec);
                    } else {
                        self.finish_memo_query(seq, vec);
                    }
                }
                // an engine that cannot embed the search still owes an answer:
                // say why, then hand the same query to the literal one
                RagOutcome::QueryFailed { seq, fault } => {
                    if self.vault_search.pending == Some(seq) {
                        self.fall_back_vault_to_literal(seq, fault);
                    } else {
                        self.fall_back_to_literal(seq, fault);
                    }
                }
            }
        }
        // retry a batch an earlier tick could not write
        if let Some((vault, scope, vectors)) = self.rag.take_deferred() {
            self.write_rag_vectors(vault, scope, vectors);
            if !self.rag.has_deferred() {
                self.rag_reindex();
            }
        }
        // a search held back until the thread was caught up goes out now
        self.resume_memo_search();
        // …and one whose worker slot was taken from it asks again
        self.resume_vault_search();
    }

    /// Writes a finished batch into the open vault: ONE save for the whole
    /// batch, down the same path every content mutation takes (memory → sync
    /// counter → save → roll back on Err → auto-sync on Ok).
    ///
    /// The vectors are derived data, so anything that makes the write unsafe or
    /// pointless simply drops them — the next unlock recomputes them.
    fn write_rag_vectors(
        &mut self,
        name: String,
        scope: crate::rag::IndexScope,
        vectors: Vec<crate::rag::Embedded>,
    ) {
        if vectors.is_empty() {
            return;
        }
        // the re-key and the sync workers both end by renaming their own blob
        // over this very file: wait for them instead of racing. Nothing here is
        // urgent enough to deserve a lost pull.
        if self.rekeying(&name) || self.syncing(&name) {
            self.rag.defer(name, scope, vectors);
            return;
        }
        // a vault that closed, locked or was swapped for another one while the
        // worker was out gets nothing written to it
        if self.vault.as_ref().map(|v| v.username.as_str()) != Some(name.as_str()) {
            return;
        }
        let tag = self.rag.tag();
        let live: Vec<String> = self
            .vault
            .as_ref()
            .map(|v| v.items().iter().map(|i| i.id.clone()).collect())
            .unwrap_or_default();
        let Some(vault) = self.vault.as_mut() else { return };
        // each scope has its own home inside the content, and its own rollback
        enum Undo {
            Memo(Vec<crate::memo::VecSlot>),
            Items(crate::rag::RagSlot),
        }
        let undo = match scope {
            crate::rag::IndexScope::Memo => {
                let mut slots = Vec::with_capacity(vectors.len());
                for embedded in &vectors {
                    if let Some(slot) =
                        vault.content.set_memo_vec(&embedded.id, &embedded.vec, &embedded.fp)
                    {
                        slots.push(slot);
                    }
                }
                if slots.is_empty() {
                    // every message the batch was about is gone: nothing
                    // changed, so there is nothing to bump or save
                    self.rag.mark_ready();
                    return;
                }
                Undo::Memo(slots)
            }
            crate::rag::IndexScope::Items => {
                Undo::Items(vault.content.set_item_vectors(tag, &vectors, &live))
            }
        };
        vault.content.bump_sync_counter("local");
        match vault.save() {
            Ok(()) => {
                self.rag.mark_ready();
                self.auto_sync_after_change();
            }
            Err(_) => {
                // put the content back so it matches the disk again
                if let Some(v) = self.vault.as_mut() {
                    match undo {
                        Undo::Memo(slots) => {
                            for slot in &slots {
                                v.content.restore_memo_vec(slot);
                            }
                        }
                        Undo::Items(slot) => v.content.restore_item_vectors(slot),
                    }
                }
                self.rag.mark_ready();
                self.warn(i18n::t(self.settings.lang, &Key::SaveFailed));
            }
        }
    }

    // ---- the localhost agent API ----------------------------------------

    /// The snapshot the server thread is allowed to hold: identifiers,
    /// timestamps, labels, the text that was indexed and its vectors. No key,
    /// no `SecretString`, no `Vault` — the item text comes from
    /// [`App::item_index_texts`], which is built through the secret filter, so
    /// a secret VALUE is not reachable from the server at all.
    pub fn build_api_index(&self) -> crate::api::ApiIndex {
        let mut index = crate::api::ApiIndex::default();
        let Some(vault) = self.vault.as_ref() else { return index };
        if self.settings.exposes_memo() {
            index.memo = vault
                .content
                .memo_messages()
                .iter()
                .map(|m| crate::api::ApiEntry {
                    id: m.id.clone(),
                    ts: Some(m.ts.clone()),
                    label: None,
                    category: None,
                    caption: m.image.as_ref().and_then(|i| i.caption.clone()),
                    text: crate::rag::memo_embed_text(m),
                    vec: m.vec.clone(),
                })
                .filter(|e| !e.text.is_empty())
                .collect();
        }
        if self.settings.exposes_items() {
            let lang = self.settings.lang;
            let stored = vault.content.item_vectors();
            let fresh = stored.model == self.rag.tag();
            index.items = self
                .item_index_texts()
                .into_iter()
                .map(|(id, text)| {
                    let item = vault.items().iter().find(|i| i.id == id);
                    crate::api::ApiEntry {
                        label: item.map(|i| i.label.clone()),
                        category: item
                            .map(|i| i18n::t(lang, &Key::CategoryName(&i.category))),
                        // a vector from another model is not offered: a dot
                        // product across two embedding spaces is noise
                        vec: fresh.then(|| stored.items.get(&id).map(|(_, v)| v.clone())).flatten(),
                        id,
                        ts: None,
                        caption: None,
                        text,
                    }
                })
                .collect();
        }
        // the engine only rides along when it can really answer; otherwise a
        // `rag` search says `model-not-ready` instead of blocking on a download
        if self.rag.searchable() {
            index.embedder = Some(self.rag.embedder());
        }
        // …and, ONLY while `api_reveal` is on, the open vault's items WITH their
        // values, for the one endpoint that sends one. This is the single line
        // in the app that puts a secret where the server thread can reach it,
        // and it is guarded by the toggle whose row says exactly that. With the
        // toggle off the member stays empty, so `/reveal` — which does not even
        // exist then — has nothing to find either way.
        if self.settings.api_reveal {
            index.reveal = vault.items().iter().map(crate::api::reveal_item).collect();
        }
        // …and, ONLY while `login_assist` is on, the logins the browser
        // extension may ask for (the second such line, same guard shape)
        if self.settings.login_assist {
            index.logins = crate::api::login_entries(vault.items());
            index.vault_open = true;
        }
        index
    }

    /// What the user has allowed the server to do, as the server reads it.
    fn api_policy(&self) -> crate::api::ApiPolicy {
        crate::api::ApiPolicy {
            allow_unlock: self.settings.api_vault_unlock,
            allow_reveal: self.settings.api_reveal,
            allow_login: self.settings.login_assist,
            // read through the gate, never off the raw field
            session_ttl: self.settings.api_session_secs().map(std::time::Duration::from_secs),
            lang: self.settings.lang,
        }
    }

    /// Starts, stops or refreshes the server so it matches the current state.
    /// Called from unlock, from the settings commit, and from every place the
    /// vault goes away.
    ///
    /// TWO reasons to be up, and either is enough: an open vault whose indexes
    /// the user chose to serve, or `api_vault_unlock` — which is what lets an
    /// agent open a vault of its own from the Access page with everything
    /// closed. Neither ⇒ nothing is listening, and the sessions die with it.
    pub fn sync_api_server(&mut self) {
        let wanted = self.settings.api_server_wanted(self.vault.is_some());
        if !wanted {
            self.stop_api_server();
            return;
        }
        // a changed port is a different socket: rebind rather than keep serving
        // on the one the user just moved away from.
        if self.api.as_ref().is_some_and(|s| !s.serves_port(self.settings.api_port)) {
            self.stop_api_server();
        }
        match self.api.as_ref() {
            Some(_) => self.refresh_api_index(),
            None => self.start_api_server(),
        }
        // the policy is pushed on EVERY sync, start or refresh: turning
        // `/reveal` off has to stop the endpoint answering now, not next run.
        let policy = self.api_policy();
        if let Some(server) = self.api.as_ref() {
            server.set_policy(policy);
            // the pairing token travels with the policy: read through the
            // switch, so turning login assist off retires it at once
            server.set_login_token(self.settings.login_token());
        }
    }

    fn start_api_server(&mut self) {
        let lang = self.settings.lang;
        let port = self.settings.api_port;
        let dir = self.paths.vaults_dir.clone();
        let policy = self.api_policy();
        match crate::api::ApiServer::start(port, self.build_api_index(), dir, policy) {
            Ok(server) => {
                let bound = server.port();
                self.api = Some(server);
                self.ok(i18n::t(lang, &Key::ApiStarted(bound)));
            }
            // a busy port is the everyday failure: say which one, stay off
            Err(_) => self.warn(i18n::t(lang, &Key::ApiPortBusy(port))),
        }
    }

    /// How many vaults an agent is holding open through the API right now (0
    /// when nothing is listening). Drives the `api·N` indicator in the top bar.
    pub fn api_sessions(&self) -> usize {
        self.api.as_ref().map(|s| s.session_count()).unwrap_or(0)
    }

    /// Kills every API session opened on `name`. Called where that vault's key
    /// stops being the key — a finished `passwd`, a `rerollEnc` — because a
    /// session holding the retired one would be a way back into a vault whose
    /// password the user has just changed; and where the vault itself stops
    /// being — a `deletevault`, a `nukevault` — because a session answers out
    /// of the copy in its own memory and would outlive the file entirely.
    fn drop_api_sessions_for(&mut self, name: &str) {
        if let Some(server) = self.api.as_ref() {
            server.drop_sessions_for(name);
        }
    }

    /// Stops the server if one is up (and empties the snapshot with it).
    pub fn stop_api_server(&mut self) {
        if let Some(server) = self.api.take() {
            server.stop();
        }
    }

    /// Hands the server a freshly built snapshot. Cheap and a no-op when the
    /// server is not running, so every mutation path can call it blindly.
    pub fn refresh_api_index(&mut self) {
        if self.api.is_none() {
            return;
        }
        let index = self.build_api_index();
        if let Some(server) = self.api.as_ref() {
            server.refresh(index);
        }
    }

    /// Whether a copy of ours is on the clipboard with its wipe timer armed.
    #[cfg(test)]
    pub fn clipboard_armed(&self) -> bool {
        self.clip_armed
    }

    /// `ragtoken`: copies the WHOLE session token, through the one clipboard
    /// path — timed wipe included, because a bearer token is a credential and
    /// leaving it on the clipboard is leaving the vault's index open.
    fn copy_api_token(&mut self) {
        let lang = self.settings.lang;
        let Some(token) = self.api.as_ref().map(|s| s.token()) else {
            self.note(i18n::t(lang, &Key::ApiTokenUnavailable));
            return;
        };
        self.copy_to_clipboard(token);
    }

    // ---- star (favorite) ----------------------------------------------

    /// Toggles ★ on the target item: an explicit label, else the open detail's
    /// item, else the highlighted one. Leaves the current view in place.
    fn star_item(&mut self, label: Option<&str>) {
        if self.refuse_if_open_vault_rekeying() {
            return;
        }
        let lang = self.settings.lang;
        let id = match label {
            Some(lbl) => match self.vault.as_ref().and_then(|v| v.position_by_label(lbl)) {
                Some(idx) => self.vault.as_ref().unwrap().items()[idx].id.clone(),
                None => {
                    self.warn(i18n::t(lang, &Key::ItemNotFound(lbl)));
                    return;
                }
            },
            None => match self.highlighted_item_id() {
                Some(id) => id,
                None => {
                    self.note(i18n::t(lang, &Key::NoItems));
                    return;
                }
            },
        };
        let Some(vault) = self.vault.as_mut() else { return };
        match vault.toggle_favorite(&id) {
            Ok(now_fav) => {
                let label = self
                    .vault
                    .as_ref()
                    .and_then(|v| v.find_by_id(&id).map(|i| v.items()[i].label.clone()))
                    .unwrap_or_default();
                // follow the row to its new (favorites-first) position
                if let Some(pos) = self.visible_item_indices().iter().position(|&i| {
                    self.vault.as_ref().is_some_and(|v| v.items()[i].id == id)
                }) {
                    self.selected = pos;
                }
                let key = if now_fav { Key::ItemStarred(&label) } else { Key::ItemUnstarred(&label) };
                self.ok(i18n::t(lang, &key));
            }
            Err(_) => self.warn(i18n::t(lang, &Key::SaveFailed)),
        }
    }

    // ---- delete -------------------------------------------------------

    /// Opens the delete form. A named item → a one-entry chooser; a bare call →
    /// a chooser scoped to the current tab+search filter, preselected on the
    /// highlighted row.
    /// Arms the item-level delete confirm (`y`/`n`) for a target item — by label
    /// (from `:delete <name>`) or, with no label, the highlighted item. Rung 1 of
    /// the confirm ladder: a two-key y/n, no `◂No▸` flipper, no chooser.
    fn open_delete_form(&mut self, label: Option<&str>) {
        if self.refuse_if_open_vault_rekeying() {
            return;
        }
        let lang = self.settings.lang;
        let target = match label {
            Some(lbl) => {
                match self
                    .vault
                    .as_ref()
                    .and_then(|v| v.position_by_label(lbl).map(|i| (v.items()[i].id.clone(), v.items()[i].label.clone())))
                {
                    Some(t) => Some(t),
                    None => {
                        self.warn(i18n::t(lang, &Key::ItemNotFound(lbl)));
                        return;
                    }
                }
            }
            None => match self.highlighted_item_id() {
                Some(id) => self
                    .vault
                    .as_ref()
                    .and_then(|v| v.find_by_id(&id).map(|i| (id.clone(), v.items()[i].label.clone()))),
                None => {
                    self.note(i18n::t(lang, &Key::NoItems));
                    return;
                }
            },
        };
        self.pending_confirm = target;
    }

    /// Arms the delete confirm for a specific id (used by `d`/Del on the detail).
    fn open_delete_form_for_id(&mut self, id: &str) {
        if self.refuse_if_open_vault_rekeying() {
            return;
        }
        self.pending_confirm = self
            .vault
            .as_ref()
            .and_then(|v| v.find_by_id(id).map(|i| (v.items()[i].id.clone(), v.items()[i].label.clone())));
    }

    /// The per-locale key that confirms a `y`/`n` prompt (`y` / sì) — the
    /// language's own, out of the one place that decides it, so the keymap can
    /// reserve exactly the characters this arm swallows.
    pub fn confirm_key(&self) -> char {
        self.settings.lang.confirm_key()
    }

    /// Executes the pending item-level delete, then clears the confirm.
    fn confirm_delete_now(&mut self) {
        let lang = self.settings.lang;
        // a confirm armed before the re-encryption started must not save now
        if self.refuse_if_open_vault_rekeying() {
            self.pending_confirm = None;
            return;
        }
        let Some((id, label)) = self.pending_confirm.take() else { return };
        let Some(vault) = self.vault.as_mut() else { return };
        let Some(pos) = vault.find_by_id(&id) else { return };
        let removed = vault.items_mut().remove(pos);
        vault.content.bump_sync_counter("local");
        match vault.save() {
            Ok(()) => {
                self.go_home();
                self.focus = Focus::Vaults;
                self.selected = self.selected.min(self.left_len().saturating_sub(1));
                self.ok(i18n::t(lang, &Key::ItemDeleted(&label)));
                self.auto_sync_after_change();
            }
            Err(_) => {
                self.vault.as_mut().unwrap().items_mut().insert(pos, removed);
                self.warn(i18n::t(lang, &Key::SaveFailed));
            }
        }
    }

    // ---- edit ---------------------------------------------------------

    /// The Edit form slots: the item's own fields, then any category-template
    /// slots it lacks. Computed identically at open and submit.
    fn edit_field_plan(&self, item: &crate::model::Item) -> Vec<EditSlot> {
        let mut plan: Vec<EditSlot> = (0..item.fields.len()).map(EditSlot::Existing).collect();
        for (key, ftype, secret) in self.category_template(&item.category) {
            if !item.fields.iter().any(|f| f.key == key) {
                plan.push(EditSlot::New(key, ftype, secret));
            }
        }
        plan
    }

    fn open_edit_form(&mut self, id: &str) {
        if self.refuse_if_open_vault_rekeying() {
            return;
        }
        let lang = self.settings.lang;
        let Some(vault) = self.vault.as_ref() else { return };
        let Some(idx) = vault.find_by_id(id) else {
            self.warn(i18n::t(lang, &Key::ItemNotFound(id)));
            return;
        };
        let item = &vault.items()[idx];
        let plan = self.edit_field_plan(item);
        let item = &self.vault.as_ref().unwrap().items()[idx]; // re-borrow (plan borrow ended)
        let mut label = FormField::text("label", i18n::t(lang, &Key::FieldLabel("label")), false);
        label.value.set(item.label.clone());
        let mut fields = vec![label];
        for slot in &plan {
            match slot {
                EditSlot::Existing(i) => {
                    let field = &item.fields[*i];
                    let name = field
                        .label
                        .clone()
                        .unwrap_or_else(|| i18n::t(lang, &Key::FieldLabel(&field.key)));
                    // an image row is a path box: never masked, and NEVER
                    // pre-filled with the stored base64 — empty keeps it.
                    let path_row = takes_a_path(&field.ftype);
                    let mut ff = FormField::text(&name, String::new(), field.secret && !path_row);
                    ff.show_strength = shows_strength(ff.secret, &field.ftype);
                    if path_row {
                        ff.description = i18n::t(lang, &Key::ImageEditHint);
                    } else if field.secret {
                        // never pre-fill a secret: an empty box reveals no length,
                        // typing starts fresh, and an empty submit KEEPS the old
                        // value (see submit_edit_item).
                        ff.description = i18n::t(lang, &Key::EditSecretHint);
                    } else {
                        ff.value.set(field.value.as_str().to_string());
                    }
                    fields.push(ff);
                }
                EditSlot::New(key, ftype, secret) => {
                    let path_row = takes_a_path(ftype);
                    let hint = if path_row { i18n::t(lang, &Key::ImageFieldHint) } else { String::new() };
                    let mut ff =
                        FormField::text(&i18n::t(lang, &Key::FieldLabel(key)), hint, *secret && !path_row);
                    ff.show_strength = shows_strength(ff.secret, ftype);
                    fields.push(ff);
                }
            }
        }
        // this specific item may need something extra: the growing add-field row
        fields.push(FormField::adder(lang));
        let form = FormState {
            command: "edit".to_string(),
            title: i18n::t(lang, &Key::EditItemTitle(&item.label)),
            purpose: FormPurpose::EditItem(item.id.clone()),
            fields,
            focused: 0,
            error: None,
        };
        self.open_form(form);
    }

    fn submit_edit_item(&mut self, id: &str) {
        let lang = self.settings.lang;
        let label = self
            .form
            .as_ref()
            .and_then(|f| f.fields.first())
            .map(|f| f.value.trim().to_string())
            .unwrap_or_default();
        if label.is_empty() {
            if let Some(form) = self.form.as_mut() {
                let l = i18n::t(lang, &Key::FieldLabel("label"));
                form.error = Some(i18n::t(lang, &Key::FieldRequired(&l)));
            }
            return;
        }
        let Some(idx) = self.vault.as_ref().and_then(|v| v.find_by_id(id)) else {
            self.form = None;
            self.warn(i18n::t(lang, &Key::ItemNotFound(id)));
            return;
        };
        // recompute the plan (identical to open — nothing mutated the vault meanwhile)
        let plan = {
            let item = &self.vault.as_ref().unwrap().items()[idx];
            self.edit_field_plan(item)
        };
        // the form holds label + plan + (converted extras + trailing adder)
        if self.form.as_ref().map(|f| f.fields.len()).unwrap_or(0) < 1 + plan.len() {
            if let Some(form) = self.form.as_mut() {
                form.error = Some(i18n::t(lang, &Key::SaveFailed));
            }
            return;
        }
        // build the new field list. A secret field is BLANK in the form (never
        // pre-filled): empty ⇒ keep the stored secret, non-empty ⇒ replace it.
        // A non-secret field: empty ⇒ drop it, non-empty ⇒ its typed value.
        let mut new_fields: Vec<crate::model::ItemField> = Vec::new();
        // a refused image import aborts the submit with nothing written
        let mut refused: Option<String> = None;
        {
            let item = &self.vault.as_ref().unwrap().items()[idx];
            let form = self.form.as_ref().unwrap();
            for (slot, ff) in plan.iter().zip(form.fields.iter().skip(1)) {
                let value = ff.value.trim();
                match slot {
                    EditSlot::Existing(i) => {
                        let old = &item.fields[*i];
                        // …and `-` takes the picture off the item altogether:
                        // blank already means "keep it", so removal needs a word
                        // of its own. Dropping the field is exactly what
                        // clearing a non-secret text row does.
                        if removes_the_value(&old.ftype, value) {
                            continue;
                        }
                        // an image row behaves exactly like a secret one: empty
                        // keeps the stored picture, a path replaces it.
                        let keeps_when_blank = old.secret || takes_a_path(&old.ftype);
                        let mut extra = old.extra.clone();
                        let new_value = if value.is_empty() {
                            if keeps_when_blank {
                                crate::model::SecretString::new(old.value.as_str())
                            } else {
                                continue; // non-secret cleared ⇒ remove
                            }
                        } else {
                            match stored_from_form(lang, &old.ftype, value) {
                                Ok((v, meta)) => {
                                    // a replacement brings fresh metadata
                                    for (k, m) in meta {
                                        extra.insert(k, m);
                                    }
                                    crate::model::SecretString::new(v)
                                }
                                Err(e) => {
                                    refused = Some(e);
                                    break;
                                }
                            }
                        };
                        new_fields.push(crate::model::ItemField {
                            key: old.key.clone(),
                            ftype: old.ftype.clone(),
                            label: old.label.clone(),
                            value: new_value,
                            secret: old.secret,
                            extra,
                        });
                    }
                    EditSlot::New(key, ftype, secret) => {
                        // an unfilled template slot has nothing to remove, so
                        // `-` here reads the same as blank (see the add form)
                        if value.is_empty() || removes_the_value(ftype, value) {
                            continue;
                        }
                        let (value, extra) = match stored_from_form(lang, ftype, value) {
                            Ok(v) => v,
                            Err(e) => {
                                refused = Some(e);
                                break;
                            }
                        };
                        new_fields.push(crate::model::ItemField {
                            key: key.clone(),
                            ftype: ftype.clone(),
                            label: None,
                            value: crate::model::SecretString::new(value),
                            secret: *secret,
                            extra,
                        });
                    }
                }
            }
            // beyond the plan: extra fields added via `+ add field` rows
            for ff in form.fields.iter().skip(1 + plan.len()) {
                let value = ff.value.trim();
                if refused.is_some() {
                    break;
                }
                if ff.adder || value.is_empty() {
                    continue;
                }
                let ftype = implied_ftype(&ff.name);
                let (value, extra) = match stored_from_form(lang, &ftype, value) {
                    Ok(v) => v,
                    Err(e) => {
                        refused = Some(e);
                        break;
                    }
                };
                new_fields.push(crate::model::ItemField {
                    key: crate::model::slugify(&ff.name),
                    label: Some(ff.name.clone()),
                    value: crate::model::SecretString::new(value),
                    secret: ff.secret || ftype.default_secret(),
                    ftype,
                    extra,
                });
            }
        }
        if let Some(message) = refused {
            if let Some(form) = self.form.as_mut() {
                form.error = Some(message);
            }
            return;
        }
        let vault = self.vault.as_mut().unwrap();
        let item = &mut vault.items_mut()[idx];
        let prev_label = std::mem::replace(&mut item.label, label.clone());
        let prev_fields = std::mem::replace(&mut item.fields, new_fields);
        let prev_modified = item.modified_at.replace(crate::model::now_rfc3339());
        vault.content.bump_sync_counter("local");
        match vault.save() {
            Ok(()) => {
                self.form = None;
                // an edit may have swapped a picture for one of the same size,
                // which the render caches (keyed on length) couldn't tell apart
                self.forget_item_images();
                self.go_home();
                self.focus = Focus::Vaults;
                // the edited item may have left the active filter with it
                self.clamp_selection();
                self.ok(i18n::t(lang, &Key::ItemEdited(&label)));
                self.auto_sync_after_change();
            }
            Err(_) => {
                let item = &mut self.vault.as_mut().unwrap().items_mut()[idx];
                item.label = prev_label;
                item.fields = prev_fields;
                item.modified_at = prev_modified;
                if let Some(form) = self.form.as_mut() {
                    form.error = Some(i18n::t(lang, &Key::SaveFailed));
                }
            }
        }
    }

    // ---- category -----------------------------------------------------

    fn open_category_form(&mut self, name: Option<&str>) {
        if self.refuse_if_open_vault_rekeying() {
            return;
        }
        let form = FormState::category(name, self.settings.lang);
        self.open_form(form);
    }

    fn submit_create_category(&mut self) {
        use crate::model::{slugify, is_reserved_category_id, CategoryDef, FieldSpec, FieldType};
        let lang = self.settings.lang;
        let name = self
            .form
            .as_ref()
            .and_then(|f| f.fields.first())
            .map(|f| f.value.trim().to_string())
            .unwrap_or_default();
        if name.is_empty() {
            if let Some(form) = self.form.as_mut() {
                let l = i18n::t(lang, &Key::CategoryNameHint);
                form.error = Some(i18n::t(lang, &Key::FieldRequired(&l)));
            }
            return;
        }
        let id = slugify(&name);
        let err = |app: &mut Self, k: String| {
            if let Some(form) = app.form.as_mut() {
                form.error = Some(k);
            }
        };
        if id.is_empty() {
            return err(self, i18n::t(lang, &Key::CategoryBadName));
        }
        if is_reserved_category_id(&id) {
            return err(self, i18n::t(lang, &Key::CategoryReserved(&id)));
        }
        if self.vault.as_ref().is_some_and(|v| v.content.categories.iter().any(|c| c.id == id)) {
            return err(self, i18n::t(lang, &Key::CategoryExists(&id)));
        }
        // every custom category always carries a website field (it feeds the
        // favicon — the item's name is its label, so neither needs a row);
        // then one spec per named `+ add field` row, type/secret inferred.
        let mut specs: Vec<FieldSpec> = vec![FieldSpec {
            key: "website".to_string(),
            ftype: FieldType::Url,
            label: None,
            secret: false,
            required: false,
            extra: serde_json::Map::new(),
        }];
        {
            let form = self.form.as_ref().unwrap();
            for field in form.fields[1..].iter().filter(|f| f.adder) {
                let raw = field.value.trim();
                if raw.is_empty() {
                    continue; // the trailing empty add-field row
                }
                let key = slugify(raw);
                if key.is_empty() {
                    return err(self, i18n::t(lang, &Key::CategoryBadName));
                }
                if specs.iter().any(|s| s.key == key) {
                    return err(self, i18n::t(lang, &Key::CategoryDuplicateField(&key)));
                }
                let ftype = implied_ftype(raw);
                specs.push(FieldSpec {
                    key: key.clone(),
                    label: (raw != key).then(|| raw.to_string()),
                    // an image slot is always stored secret (FieldType::Image)
                    secret: implied_secret(raw) || ftype.default_secret(),
                    ftype,
                    required: false,
                    extra: serde_json::Map::new(),
                });
            }
        }
        let def = CategoryDef {
            id: id.clone(),
            name: name.clone(),
            fields: specs,
            created_at: Some(crate::model::now_rfc3339()),
            extra: serde_json::Map::new(),
        };
        let Some(vault) = self.vault.as_mut() else { return };
        vault.content.categories.push(def);
        vault.content.bump_sync_counter("local");
        match vault.save() {
            Ok(()) => {
                self.form = None;
                self.go_home();
                self.focus = Focus::Vaults;
                self.ok(i18n::t(lang, &Key::CategoryCreated(&name)));
                self.auto_sync_after_change();
            }
            Err(_) => {
                self.vault.as_mut().unwrap().content.categories.pop();
                err(self, i18n::t(lang, &Key::SaveFailed));
            }
        }
    }

    // ---- rerollEnc ----------------------------------------------------

    fn open_reroll_form(&mut self) {
        if self.refuse_if_open_vault_rekeying() {
            return;
        }
        let Some(vault) = self.vault.as_ref() else { return };
        let form = FormState::reroll(vault.algorithm, vault.kdf, self.settings.lang);
        self.open_form(form);
    }

    fn submit_reroll(&mut self) {
        use crate::crypto::{Algorithm, KdfSpec};
        let lang = self.settings.lang;
        let (pw, algo_name, kdf_name) = match self.form.as_ref() {
            Some(f) if f.fields.len() >= 3 => (
                zeroize::Zeroizing::new(f.fields[0].value.as_str().to_string()),
                f.fields[1].current().to_string(),
                f.fields[2].current().to_string(),
            ),
            _ => return,
        };
        let Some(vault) = self.vault.as_mut() else { return };
        if !vault.verify_password(&pw) {
            if let Some(form) = self.form.as_mut() {
                form.error = Some(i18n::t(lang, &Key::WrongPassword));
            }
            return;
        }
        let algorithm = Algorithm::from_name(&algo_name).unwrap_or(vault.algorithm);
        let kdf = if kdf_name == vault.kdf.name() {
            vault.kdf
        } else {
            KdfSpec::from_name(&kdf_name).unwrap_or(vault.kdf)
        };
        let name = vault.username.clone();
        let result = vault.reroll(&pw, algorithm, kdf);
        match result {
            Ok(()) => {
                // fresh salt, fresh key: any API session on this vault is
                // holding a retired one — the same rule the `passwd` path
                // follows, at the other place a re-key completes.
                self.drop_api_sessions_for(&name);
                self.form = None;
                self.go_home();
                self.focus = Focus::Vaults;
                self.ok(i18n::t(lang, &Key::VaultRerolled(algorithm.name(), kdf.name())));
            }
            Err(e) => {
                if let Some(form) = self.form.as_mut() {
                    form.error = Some(e.to_string());
                }
            }
        }
    }

    // ---- nuke (in-vault) ----------------------------------------------

    fn open_nuke_items_form(&mut self) {
        if self.refuse_if_open_vault_rekeying() {
            return;
        }
        let lang = self.settings.lang;
        if self.vault.as_ref().is_none_or(|v| v.items().is_empty()) {
            self.note(i18n::t(lang, &Key::NoItems));
            return;
        }
        self.open_form(FormState::nuke_items(lang));
    }

    fn submit_nuke_items(&mut self) {
        let lang = self.settings.lang;
        let (pw, typed_name, confirmed) = match self.form.as_ref() {
            Some(f) if f.fields.len() >= 3 => (
                zeroize::Zeroizing::new(f.fields[0].value.as_str().to_string()),
                f.fields[1].value.trim().to_string(),
                f.fields[2].selected == 1,
            ),
            _ => return,
        };
        let err = |app: &mut Self, k: Key| {
            let msg = i18n::t(app.settings.lang, &k);
            if let Some(form) = app.form.as_mut() {
                form.error = Some(msg);
            }
        };
        // cheap checks first so an unarmed submit never burns an Argon2 pass
        if !confirmed {
            return err(self, Key::NukeItemsNotConfirmed);
        }
        let Some(vault) = self.vault.as_ref() else { return };
        if vault.username != typed_name {
            return err(self, Key::NukeItemsNameMismatch);
        }
        if !self.vault.as_ref().unwrap().verify_password(&pw) {
            return err(self, Key::WrongPassword);
        }
        let vault = self.vault.as_mut().unwrap();
        let removed = std::mem::take(vault.items_mut());
        let count = removed.len();
        vault.content.bump_sync_counter("local");
        match vault.save() {
            Ok(()) => {
                drop(removed);
                self.form = None;
                self.reset_vault_browser();
                self.selected = 0;
                self.go_home();
                self.focus = Focus::Vaults;
                self.ok(i18n::t(lang, &Key::ItemsNuked(count)));
                self.auto_sync_after_change();
            }
            Err(e) => {
                *self.vault.as_mut().unwrap().items_mut() = removed;
                let msg = e.to_string();
                if let Some(form) = self.form.as_mut() {
                    form.error = Some(msg);
                }
            }
        }
    }

    /// The category ids that appear on at least one item, in built-in order then
    /// custom/unknown ids — these become the tabs after "All".
    pub fn vault_categories(&self) -> Vec<String> {
        let Some(v) = &self.vault else { return Vec::new() };
        let mut present: Vec<String> = Vec::new();
        // built-ins first (in standard order), if any item uses them…
        for b in crate::model::BUILTINS {
            if v.items().iter().any(|i| i.category == b.id) {
                present.push(b.id.to_string());
            }
        }
        // …then custom / unknown category ids, in first-seen order
        for item in v.items() {
            if !present.iter().any(|c| c == &item.category) {
                present.push(item.category.clone());
            }
        }
        present
    }

    /// The category id filtered by the active tab, or `None` for the "All" tab.
    fn active_category(&self) -> Option<String> {
        if self.vault_tab == 0 {
            return None;
        }
        self.vault_categories().get(self.vault_tab - 1).cloned()
    }

    /// Indices (into `vault.items()`) of the items shown under the current tab
    /// and search query. The query matches labels, the localized category name,
    /// field keys/labels, and NON-secret field values only (never secrets).
    ///
    /// In RAG mode the query does not filter at all: every item of the tab stays
    /// on screen and the SCORE decides the order (see [`App::vault_rank`]), so
    /// the answer to "which of my logins is the router one?" is the top row and
    /// not an empty list. The category tab is a different question from the
    /// query, so it still narrows the list under both modes.
    pub fn visible_item_indices(&self) -> Vec<usize> {
        let Some(v) = &self.vault else { return Vec::new() };
        let category = self.active_category();
        let ranking = self.vault_ranking();
        let q = if ranking { String::new() } else { self.vault_query.trim().to_lowercase() };
        let mut out: Vec<usize> = v
            .items()
            .iter()
            .enumerate()
            .filter(|(_, item)| category.as_deref().is_none_or(|c| item.category == c))
            .filter(|(_, item)| q.is_empty() || self.item_matches(item, &q))
            .map(|(i, _)| i)
            .collect();
        if ranking {
            // score order, best first — and favorites-first is suspended with
            // the filter: a ranking the user asked for is not something a star
            // gets to overrule. An item with no vector yet (a brand new one,
            // or one the pass hasn't reached) has no rank at all and lands
            // after every ranked one, in stored order (`sort_by_key` is stable).
            let rank = self.vault_rank();
            let pos: std::collections::HashMap<&str, usize> =
                rank.iter().enumerate().map(|(at, (id, _))| (id.as_str(), at)).collect();
            out.sort_by_key(|&i| pos.get(v.items()[i].id.as_str()).copied().unwrap_or(usize::MAX));
            return out;
        }
        // favorites first (stable: stored order preserved within each group).
        // View-layer only — the on-disk order is never reordered.
        out.sort_by_key(|&i| !v.items()[i].favorite);
        out
    }

    /// The ready favicon pixels for an item (its website's cached icon), or
    /// `None` when favicons are off, the item has no website, or it isn't
    /// fetched yet — the list then falls back to a monogram.
    pub fn favicon_pixels(&self, item: &crate::model::Item) -> Option<Vec<u8>> {
        if !self.settings.favicons {
            return None;
        }
        let domain = crate::favicon::domain_of(crate::model::item_website(item)?)?;
        self.favicons.pixels(&domain)
    }

    /// Queues a favicon fetch for every visible item's website (called each
    /// tick; idempotent, so it only fetches each domain once).
    fn prefetch_favicons(&self) {
        if !self.settings.favicons || self.page != Page::Vault {
            return;
        }
        let Some(v) = &self.vault else { return };
        for &idx in &self.visible_item_indices() {
            if let Some(url) = v.items().get(idx).and_then(crate::model::item_website) {
                if let Some(domain) = crate::favicon::domain_of(url) {
                    self.favicons.request(&domain);
                }
            }
        }
    }

    fn item_matches(&self, item: &crate::model::Item, q: &str) -> bool {
        // fuzzy over the label (fig-10: `se` finds "Sentry" and "Stripe live");
        // everything else matches by substring.
        if crate::palette::fuzzy_score(&item.label, q).is_some() {
            return true;
        }
        let cat = i18n::t(self.settings.lang, &Key::CategoryName(&item.category));
        if cat.to_lowercase().contains(q) || item.category.to_lowercase().contains(q) {
            return true;
        }
        item.fields.iter().any(|field| {
            let label = field
                .label
                .clone()
                .unwrap_or_else(|| i18n::t(self.settings.lang, &Key::FieldLabel(&field.key)));
            field.key.to_lowercase().contains(q)
                || label.to_lowercase().contains(q)
                // secret values are NEVER matched (that would be a reveal oracle)
                || (!field.secret && field.value.as_str().to_lowercase().contains(q))
        })
    }

    // ---- searching the item list (the `/` box) ---------------------------

    /// Whether the query RANKS the list instead of filtering it: the box is
    /// asking for RAG, there is something to search for, and nothing has made
    /// it fall back. True from the keystroke that asks — membership must not
    /// flicker between the question and the answer — so all the ranking itself
    /// changes is the ORDER, once it lands.
    fn vault_ranking(&self) -> bool {
        self.vault_search.mode == SearchMode::Rag
            && self.vault_search.ran != Some(SearchMode::Literal)
            && !self.vault_query.trim().is_empty()
    }

    /// The ranking the list is wearing, `(item id, score)` best first. Empty
    /// until the first answer lands, and again the moment the query is cleared.
    fn vault_rank(&self) -> &[(String, f32)] {
        if self.vault_ranking() {
            &self.vault_search.ranked
        } else {
            &[]
        }
    }

    /// The relevance badge for a row: the score as whole percent, or `None` when
    /// this list is not ranked at all or that item has no vector yet — a row
    /// with no number is exactly a row nothing has scored.
    pub fn item_rank_score(&self, id: &str) -> Option<u8> {
        let (_, score) = self.vault_rank().iter().find(|(hit, _)| hit == id)?;
        // a cosine similarity runs -1..1 and only its positive half says
        // "related": below zero the badge is 0, never a negative number.
        Some((score.max(0.0) * 100.0).round().min(100.0) as u8)
    }

    /// Tab in the box: flips the mode chip literal ⇄ RAG, drops whatever the
    /// other search left on the list, and asks the new one straight away — the
    /// box has no ⏎-to-run (the list answers every keystroke), so the flip IS
    /// the run.
    fn flip_vault_search_mode(&mut self) {
        self.vault_search.mode = self.vault_search.mode.flipped();
        self.vault_search.invalidate();
        self.vault_search.told = false; // a fresh ask earns its reason again
        self.rag.cancel_query();
        self.selected = 0;
        self.run_vault_search();
    }

    /// Drops the ranking and the answer still coming for it, leaving the MODE
    /// alone: clearing a query is not changing your mind about which search to
    /// run (a fallback does not change it either).
    fn clear_vault_ranking(&mut self) {
        self.vault_search.invalidate();
        self.rag.cancel_query();
    }

    /// Runs what the box asks for, after every edit of the query. The literal
    /// search has nothing to run — the filter IS the search, recomputed on the
    /// next frame — so this is the semantic one's path and no other.
    ///
    /// A RAG search that CANNOT run is never a dead search: the list keeps
    /// filtering literally, the chip owns up to which search answered, and the
    /// reason is said out loud once (see [`App::rag_unavailable`]).
    fn run_vault_search(&mut self) {
        if self.vault_search.mode != SearchMode::Rag {
            return;
        }
        let mut query = self.vault_query.trim().to_string();
        if query.is_empty() {
            self.clear_vault_ranking();
            return;
        }
        if let Some(why) = self.rag_unavailable(self.settings.rag_items) {
            query.zeroize(); // the copy this call made; the box keeps its own
            self.vault_search.ranked.clear();
            self.vault_search.pending = None;
            self.vault_search.ran = Some(SearchMode::Literal);
            if !self.vault_search.told {
                self.vault_search.told = true;
                self.note(why);
            }
            return;
        }
        // ONE embedding at a time: the answer looks at the query again on its
        // way in and starts the next one if it moved (see
        // [`App::rerun_vault_search`]), so a fast typist queues no workers —
        // the seq-tag is what makes the answer nobody wants any more harmless.
        // A query that is already ANSWERED starts nothing either: typing a
        // character and taking it straight back is not a new question.
        if self.vault_search.searching()
            || (self.vault_search.ran == Some(SearchMode::Rag)
                && self.vault_search.asked == query)
        {
            query.zeroize();
            return;
        }
        self.start_vault_query(query);
    }

    /// Sends the query to the embedding worker, remembering what was asked so
    /// the answer can tell whether the box has moved on since.
    fn start_vault_query(&mut self, query: String) {
        self.vault_search.asked.zeroize();
        self.vault_search.asked = query.clone();
        let seq = self.rag.start_query(query);
        self.vault_search.pending = Some(seq);
        self.vault_search.ran = None;
    }

    /// Ranks the open vault's items against a query vector the worker just
    /// finished. An answer for a search that has moved on — the query cleared,
    /// the mode flipped, the vault locked — is dropped: `seq` says which.
    ///
    /// Items still waiting on a vector are not in the index and so not in the
    /// ranking; the list puts them last (see [`App::visible_item_indices`]).
    fn finish_vault_query(&mut self, seq: u64, query: Vec<f32>) {
        if self.vault_search.pending != Some(seq) {
            return;
        }
        self.vault_search.pending = None;
        // which item the cursor is on, before the order under it changes
        let anchor = self.highlighted_item_id();
        let texts = self.item_index_texts();
        let stored = self.vault.as_ref().map(|v| v.content.item_vectors()).unwrap_or_default();
        // vectors written by ANOTHER model are not ranked with: a dot product
        // across two embedding spaces is noise, and noise painted with relevance
        // badges under a chip that says RAG is worse than no order at all. The
        // pass that re-embeds everything is already queued; until it lands the
        // literal filter is the honest answer (the same test `build_api_index`
        // makes before it hands a stored vector out).
        let index = if stored.model == self.rag.tag() {
            crate::rag::item_index(&texts, &stored)
        } else {
            Vec::new()
        };
        self.vault_search.ranked = crate::rag::search_semantic(&index, &query, index.len())
            .into_iter()
            .map(|hit| (hit.id, hit.score))
            .collect();
        self.vault_search.ran = Some(SearchMode::Rag);
        self.keep_cursor_on(anchor);
        self.rerun_vault_search();
    }

    /// A query embedding that failed: say why — localized, whether the reason is
    /// ours or the engine's — and let the literal filter answer instead, so the
    /// list is never left standing on a search that produced nothing.
    fn fall_back_vault_to_literal(&mut self, seq: u64, fault: crate::rag::RagFault) {
        if self.vault_search.pending != Some(seq) {
            return;
        }
        let lang = self.effective_lang();
        self.warn(i18n::t(lang, &Key::RagStatusFailed(&fault.message(lang))));
        // the literal filter comes back on with this, so the visible set can
        // shrink under the cursor — same reason as the ranking above
        let anchor = self.highlighted_item_id();
        self.vault_search.pending = None;
        self.vault_search.ranked.clear();
        self.vault_search.ran = Some(SearchMode::Literal);
        self.keep_cursor_on(anchor);
    }

    /// Starts the search the query moved on to while the worker was out — the
    /// debounce: at most one embedding in flight, and the last thing typed is
    /// what ends up on screen.
    fn rerun_vault_search(&mut self) {
        if self.vault_query.trim() != self.vault_search.asked {
            self.run_vault_search();
        }
    }

    /// The box's half of the tick: one engine means ONE query slot, and the
    /// Memorandum's bar can take it (open the thread mid-search, search that).
    /// An answer that can no longer come would leave the row saying "searching"
    /// for the rest of the session, so it is asked for again instead.
    fn resume_vault_search(&mut self) {
        // …but not while the thread has the body: the list is not on screen to
        // wait for anything, and the bar down there is using the slot. It asks
        // again on the first tick after the thread closes.
        if self.memo_open || self.vault_search.pending.is_none() || self.rag.query_out() {
            return;
        }
        self.vault_search.pending = None;
        self.run_vault_search();
    }

    /// Cycles the category tab (←/→ in the search box), clamping the selection
    /// and the query scope to the new tab. Categories only — the Memorandum's
    /// pseudo-chip is [`Self::cycle_vault_stop`]'s business.
    fn cycle_vault_tab(&mut self, delta: isize) {
        let n = self.vault_categories().len() + 1; // + "All"
        self.vault_tab = (self.vault_tab as isize + delta).rem_euclid(n as isize) as usize;
        self.selected = 0;
        self.reset_detail();
    }

    /// The Memorandum's seat in the Tab cycle: one past the last category, so
    /// it sits between that one and the ALL tab. It is a POSITION and nothing
    /// else — `vault_categories`, `active_category` and every filtering path
    /// below them never see it, because it is not in the list they read.
    fn memo_tab_stop(&self) -> usize {
        self.vault_categories().len() + 1
    }

    /// How many stops the Tab cycle has: ALL + one per category + the memo's.
    fn vault_tab_stops(&self) -> usize {
        self.memo_tab_stop() + 1
    }

    /// Where the cycle currently IS: the memo's stop while its view is up (the
    /// real tab is remembered underneath, so Esc lands back on it), otherwise
    /// the tab itself.
    fn vault_tab_stop(&self) -> usize {
        if self.memo_open {
            self.memo_tab_stop()
        } else {
            self.vault_tab
        }
    }

    /// Tab / Shift+Tab along the chips row: the category tabs AND the
    /// Memorandum's pseudo-chip at the end of them. Landing on that chip opens
    /// the thread — the chip IS the view — and stepping off it closes the
    /// thread behind you onto the neighbouring REAL tab (Tab → ALL, Shift+Tab →
    /// the last category), so the tab state is never left on the pseudo-stop.
    fn cycle_vault_stop(&mut self, delta: isize) {
        let stops = self.vault_tab_stops() as isize;
        let memo = self.memo_tab_stop();
        let mut next = (self.vault_tab_stop() as isize + delta).rem_euclid(stops) as usize;
        if next == memo {
            self.open_memo();
            if self.memo_open {
                return;
            }
            // nothing to show a thread from (no open vault): step over the chip
            // rather than stalling the cycle on it
            next = (next as isize + delta).rem_euclid(stops) as usize;
        }
        if self.memo_open {
            self.close_memo();
        }
        self.vault_tab = next;
        self.selected = 0;
        self.reset_detail();
    }

    /// Enters/leaves the search box. When leaving, focus returns to the list.
    fn set_search_focus(&mut self, on: bool) {
        self.search_focus = on;
        self.reset_detail();
        if on {
            self.focus = Focus::Vaults;
        } else {
            self.selected = 0;
        }
    }

    /// Resets the in-vault browser state (tab, query, search mode and whatever
    /// it ranked, search focus) — used when a vault is opened or closed. The
    /// query and the ranking both describe what is INSIDE that vault, so they
    /// leave with it rather than greeting the next one.
    fn reset_vault_browser(&mut self) {
        self.vault_query.zeroize();
        self.vault_search.wipe();
        self.rag.cancel_query();
        self.vault_tab = 0;
        self.search_focus = false;
        self.reset_detail();
    }

    /// Category-tab and search-box keys for the in-vault list. Returns `true`
    /// when it consumed the key (so the general handler is skipped).
    fn on_vault_browser_key(&mut self, code: KeyCode, mods: KeyModifiers) -> bool {
        // Tab / Shift+Tab walk the chips row from anywhere on the list — the
        // categories and, at the end of them, the Memorandum's chip. Shift+Tab
        // from ALL is one step BACK, which lands straight on it.
        // In the search box the row IS the query, so the pseudo-chip is out of
        // reach from there: Shift+Tab stays on the categories, like the ←/→
        // next to it, while Tab belongs to the box's own mode chip (below).
        if code == KeyCode::BackTab {
            if self.search_focus {
                self.cycle_vault_tab(-1);
            } else {
                self.cycle_vault_stop(-1);
            }
            return true;
        }
        if code == KeyCode::Tab && !self.search_focus {
            self.cycle_vault_stop(1);
            return true;
        }
        // Ctrl+F focuses the search box (the reliable, terminal-independent path).
        if mods.contains(KeyModifiers::CONTROL) && matches!(code, KeyCode::Char('f')) {
            self.set_search_focus(true);
            return true;
        }
        // Ctrl+S = star, Ctrl+E = edit the highlighted item (not while searching).
        // Always consumed (so the bare letter is never typed), but they only ACT
        // when the command line is empty, so a half-typed command isn't
        // clobbered — and only while the verb they are an alias FOR is still
        // switched on (see `verb_live`).
        if mods.contains(KeyModifiers::CONTROL) && !self.search_focus {
            match code {
                KeyCode::Char('s') => {
                    if self.input.is_empty() && self.verb_live(crate::keymap::Verb::StarItem) {
                        self.star_item(None);
                    }
                    return true;
                }
                KeyCode::Char('e') => {
                    if self.input.is_empty() && self.verb_live(crate::keymap::Verb::EditItem) {
                        if let Some(id) = self.highlighted_item_id() {
                            self.open_edit_form(&id);
                        }
                    }
                    return true;
                }
                _ => {}
            }
        }
        // Del opens the (default-No) delete form for the highlighted item — the
        // same verb the delete key carries, so it goes with it when that is off
        if code == KeyCode::Delete && !self.search_focus && self.input.is_empty() {
            if self.verb_live(crate::keymap::Verb::DeleteItem) {
                self.open_delete_form(None);
            }
            return true;
        }
        if self.search_focus {
            // ←/→/Home/End move the query caret and Backspace/Delete edit at it —
            // the box IS the query, so the arrows belong to the text here (the
            // category tabs move on ←/→ off the box, below). An edit re-runs the
            // search and drops the highlight; a bare caret move touches neither.
            match crate::textinput::edit_key(&mut self.vault_query, code) {
                crate::textinput::Edit::Changed => {
                    self.selected = 0;
                    self.run_vault_search();
                    return true;
                }
                crate::textinput::Edit::Moved => return true,
                crate::textinput::Edit::Ignored => {}
            }
            match code {
                KeyCode::Esc => {
                    if self.vault_query.is_empty() {
                        self.set_search_focus(false);
                    } else {
                        // the query goes, and with it the ranking a semantic run
                        // put on the list: an order nothing on screen explains
                        // any more would be worse than no order at all
                        self.vault_query.zeroize();
                        self.clear_vault_ranking();
                        self.selected = 0;
                    }
                }
                // Tab flips the mode chip — literal ⇄ RAG — exactly as it does
                // in the Memorandum's bar. Dropping into the list is ⏎ (and ↓),
                // which is where that role went when this one arrived.
                KeyCode::Tab => self.flip_vault_search_mode(),
                // Down/Enter drop into the list, keeping whatever the box did to
                // it (the filter, or the ranking) — leaving the box is not
                // leaving the search.
                KeyCode::Down | KeyCode::Enter => self.set_search_focus(false),
                KeyCode::Char(ch)
                    if !mods.intersects(
                        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                    ) =>
                {
                    self.vault_query.insert(ch);
                    self.selected = 0;
                    self.run_vault_search();
                }
                _ => {}
            }
            return true;
        }
        // Not in the search box: ←/→ switch the category tab while BROWSING THE
        // LIST (this is where that role lives now that the box's own arrows edit
        // the query). Only from the list — inside the docked detail an arrow is
        // the detail's, and a chord falls through to the global handlers.
        if matches!(code, KeyCode::Left | KeyCode::Right)
            && self.focus == Focus::Vaults
            && !mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
        {
            self.cycle_vault_tab(if code == KeyCode::Left { -1 } else { 1 });
            return true;
        }
        // Not in the search box: ↑ from the first item reaches the search box
        // (only when the command line is empty, so typing a command still works).
        if code == KeyCode::Up
            && self.focus == Focus::Vaults
            && self.input.is_empty()
            && self.selected == 0
        {
            self.set_search_focus(true);
            return true;
        }
        false
    }

    /// The default (ready-to-use) export path for a vault: `<downloads>/<safe
    /// name>.vault`.
    fn export_default_path(&self, name: &str) -> String {
        crate::paths::default_export_dir()
            .join(export_file_name(name))
            .to_string_lossy()
            .into_owned()
    }

    /// The default source-path prefill for import: the export directory with a
    /// trailing separator, so the user just appends the file name.
    fn import_default_prefill(&self) -> String {
        let mut dir = crate::paths::default_export_dir().to_string_lossy().into_owned();
        if !dir.ends_with(std::path::MAIN_SEPARATOR) {
            dir.push(std::path::MAIN_SEPARATOR);
        }
        dir
    }

    /// Opens the export form. The vault chooser defaults to `name` (if it
    /// matches a vault) or the highlighted one; the path is pre-filled.
    fn open_export_form(&mut self, name: Option<&str>) {
        if self.vaults.is_empty() {
            self.note(i18n::t(self.settings.lang, &Key::NoVaults));
            return;
        }
        let selected = name
            .and_then(|n| self.vaults.iter().position(|v| v.eq_ignore_ascii_case(n)))
            .unwrap_or_else(|| self.selected.min(self.vaults.len() - 1));
        let default_path = self.export_default_path(&self.vaults[selected]);
        let form = FormState::export(&self.vaults, selected, default_path, self.settings.lang);
        self.open_form(form);
    }

    /// Keeps the export destination in sync with the chosen vault while the path
    /// hasn't been hand-edited, so cycling the vault chooser updates the default
    /// filename instead of leaving a stale one (which would mislabel the file).
    fn refresh_export_default(&mut self) {
        let name = match self.form.as_ref() {
            Some(form) if form.purpose == FormPurpose::ExportVault => {
                match (form.fields.first(), form.fields.get(1)) {
                    (Some(vault), Some(dest)) if !dest.dirty => vault.current().to_string(),
                    _ => return,
                }
            }
            _ => return,
        };
        let new_default = self.export_default_path(&name);
        if let Some(dest) = self.form.as_mut().and_then(|f| f.fields.get_mut(1)) {
            dest.value.set(new_default);
        }
    }

    /// Opens the import form, pre-filling the source path with `path` (if given)
    /// or the default export directory.
    fn open_import_form(&mut self, path: Option<&str>) {
        let prefill = path
            .map(str::to_string)
            .unwrap_or_else(|| self.import_default_prefill());
        let form = FormState::import(prefill, self.settings.lang);
        self.open_form(form);
    }

    /// Opens the tabbed settings page, drafting from the saved preferences.
    fn open_settings(&mut self) {
        // the browser row's answer, fetched once per visit (never per frame)
        self.detected_browser = crate::browser::default_browser();
        self.settings_ui = Some(SettingsState::new(self.settings.clone()));
        self.form = None;
        self.focus = Focus::Panel;
        self.status = None;
    }

    /// Opens a delete/nuke form. The vault chooser defaults to `name` (if it
    /// matches an existing vault) or the currently-highlighted vault.
    fn open_manage_form(&mut self, purpose: FormPurpose, name: Option<&str>) {
        if self.vaults.is_empty() {
            self.note(i18n::t(self.settings.lang, &Key::NoVaults));
            return;
        }
        let selected = name
            .and_then(|n| self.vaults.iter().position(|v| v.eq_ignore_ascii_case(n)))
            .unwrap_or_else(|| self.selected.min(self.vaults.len() - 1));
        let form = FormState::manage(purpose, &self.vaults, selected, self.settings.lang);
        self.open_form(form);
    }

    /// Header info (algorithm, KDF) for the vault currently chosen in a
    /// delete/nuke form — rendered live under the chooser.
    pub fn manage_target_info(&self) -> Option<(String, String, String)> {
        let form = self.form.as_ref()?;
        if !matches!(form.purpose, FormPurpose::DeleteVault | FormPurpose::NukeVault) {
            return None;
        }
        let name = form.fields.first()?.current().to_string();
        let (algo, kdf) = vault::header_info(&self.paths.vaults_dir, &name)?;
        Some((name, algo, kdf))
    }

    /// Closes the open vault (its key is zeroized on drop) and returns to the
    /// access page.
    fn exit_vault(&mut self) {
        self.vault = None;
        // closing the vault takes its secrets with it, the clipboard included
        self.wipe_clipboard();
        self.forget_item_images();
        self.purge_exports();
        self.form = None;
        self.detail_reveal = false;
        self.close_memo();
        // …and the embedding model goes with the thread it was indexing
        self.rag.unload();
        // …and the agent API gives up the snapshot it was serving. NOT always
        // the server: with `api_vault_unlock` on it keeps listening for an
        // agent's own `/unlock` from the Access page, and the sessions an agent
        // already holds are not this vault's to close — they were opened with a
        // password, not with what the TUI had open. The refresh inside is what
        // empties the closed vault's half of the snapshot.
        self.sync_api_server();
        self.reset_vault_browser();
        self.set_page(Page::Access);
        self.reload_vaults();
        self.selected = 0;
        self.focus = Focus::Vaults;
        self.note(i18n::t(self.settings.lang, &Key::VaultClosed));
    }

    /// Commits a plain panel view, leaving any form.
    fn enter_view(&mut self, view: PanelView) {
        self.form = None;
        self.focus = Focus::Panel;
        self.status = None;
        self.set_view(view);
    }

    fn open_form(&mut self, form: FormState) {
        self.form = Some(form);
        self.focus = Focus::Panel;
        self.status = None;
    }

    // ---- form mode -----------------------------------------------------

    /// Key handling while a form is open. The form is non-modal: ← moves focus
    /// to the left panel (keeping the form open), → returns to it. Only when
    /// the panel is focused do keys edit the form.
    fn on_form_mode_key(&mut self, code: KeyCode, mods: KeyModifiers) -> bool {
        // A floating form is a LAYER: it owns every key, and the list under the
        // dimmed screen never moves. ←/→ cycle a focused CHOICE field; on a TEXT
        // field they move the caret instead (that, and Home/End/Delete, is
        // on_form_key's business below).
        if matches!(code, KeyCode::Left | KeyCode::Right) && self.focused_field_is_choice() {
            let delta = if code == KeyCode::Left { -1 } else { 1 };
            if let Some(form) = self.form.as_mut() {
                if let Some(field) = form.fields.get_mut(form.focused) {
                    field.cycle(delta);
                }
            }
            // cycling the export vault chooser refreshes the default path
            self.refresh_export_default();
            return false;
        }
        self.on_form_key(code, mods);
        false
    }

    fn on_form_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        // ←/→/Home/End move the caret through the focused TEXT field and
        // Backspace/Delete edit at it — the same shared caret every input has. A
        // choice field has no text, so it is left out (its ←/→ cycling was
        // handled by the caller). Backspace/Delete mark the field dirty, exactly
        // as typing does, so a pre-filled default stops auto-updating once edited.
        if let Some(form) = self.form.as_mut() {
            if let Some(field) = form.fields.get_mut(form.focused) {
                if !field.is_choice() {
                    match crate::textinput::edit_key(&mut field.value, code) {
                        crate::textinput::Edit::Changed => {
                            form.error = None;
                            field.dirty = true;
                            return;
                        }
                        crate::textinput::Edit::Moved => return,
                        crate::textinput::Edit::Ignored => {}
                    }
                }
            }
        }
        match code {
            KeyCode::Esc => {
                self.form = None;
                self.focus = Focus::Vaults;
                self.note(i18n::t(self.settings.lang, &Key::FormCancelled));
            }
            // ⏎ next · on the last field ⏎ saves (fig-06). On a NAMED add-field
            // row of an item form, ⏎ turns it into that field right here — the
            // row becomes the value input — so "name it, fill it" is two ⏎s.
            KeyCode::Enter => {
                let (focused, len, named_adder) = match self.form.as_ref() {
                    Some(f) => (
                        f.focused,
                        f.fields.len(),
                        f.fields.get(f.focused).is_some_and(|fd| fd.adder && !fd.value.trim().is_empty()),
                    ),
                    None => return,
                };
                let item_form = matches!(
                    self.form.as_ref().map(|f| f.purpose.clone()),
                    Some(FormPurpose::AddItem(_)) | Some(FormPurpose::EditItem(_))
                );
                if named_adder && item_form {
                    self.convert_adder(focused);
                } else if focused + 1 < len {
                    self.form_move(1);
                } else {
                    self.submit_form();
                }
            }
            KeyCode::Tab | KeyCode::Down => self.form_move(1),
            KeyCode::BackTab | KeyCode::Up => self.form_move(-1),
            // Modifier chords (Ctrl+U/W/V, Alt+…) must not be typed as their
            // bare letter — especially dangerous silently masked in a password.
            KeyCode::Char(ch)
                if !mods
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER) =>
            {
                let lang = self.settings.lang;
                if let Some(form) = self.form.as_mut() {
                    form.error = None;
                    let focused = form.focused;
                    let last = focused + 1 == form.fields.len();
                    if let Some(field) = form.fields.get_mut(focused) {
                        if !field.is_choice() {
                            field.value.insert(ch);
                            field.dirty = true;
                            // an add-field row that just got its first character
                            // grows a fresh `+ add field` row under it
                            if field.adder && last && field.value.chars().count() == 1 {
                                form.fields.push(FormField::adder(lang));
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Turns a NAMED `+ add field` row of an item form into the real field it
    /// names: the typed text becomes the field's name, the row becomes its
    /// (empty) value input, and secrecy is inferred from the name.
    fn convert_adder(&mut self, idx: usize) {
        let lang = self.settings.lang;
        let Some(form) = self.form.as_mut() else { return };
        let Some(field) = form.fields.get_mut(idx) else { return };
        if !field.adder || field.value.trim().is_empty() {
            return;
        }
        let name = field.value.trim().to_string();
        let ftype = implied_ftype(&name);
        // a name that reads as a picture makes a PATH row: typed in the clear
        // (a path is not a secret), stored secret all the same
        let path_row = takes_a_path(&ftype);
        field.secret = !path_row && implied_secret(&name);
        // the meter follows the type the name implies — the same one the field
        // is stored with on submit, so screen and vault never disagree
        field.show_strength = shows_strength(field.secret, &ftype);
        field.name = name;
        field.value.zeroize();
        field.value.clear();
        field.description = if path_row { i18n::t(lang, &Key::ImageFieldHint) } else { String::new() };
        field.adder = false;
    }

    fn form_move(&mut self, delta: isize) {
        // leaving a NAMED add-field row of an item form converts it (the name
        // is committed; come back any time to fill its value)
        let leaving = self.form.as_ref().map(|f| f.focused);
        let item_form = matches!(
            self.form.as_ref().map(|f| f.purpose.clone()),
            Some(FormPurpose::AddItem(_)) | Some(FormPurpose::EditItem(_))
        );
        if item_form {
            if let Some(idx) = leaving {
                self.convert_adder(idx);
            }
        }
        if let Some(form) = self.form.as_mut() {
            let n = form.fields.len() as isize;
            if n > 0 {
                form.focused = (form.focused as isize + delta).rem_euclid(n) as usize;
            }
        }
    }

    fn focused_field_is_choice(&self) -> bool {
        self.form
            .as_ref()
            .and_then(|f| f.fields.get(f.focused))
            .is_some_and(|field| field.is_choice())
    }

    fn submit_form(&mut self) {
        // Add/Edit/Category have their own validation (optional fields), so skip
        // the blanket "no empty field" check for them.
        match self.form.as_ref().map(|f| f.purpose.clone()) {
            Some(FormPurpose::AddItem(category)) => return self.submit_add_item(&category),
            Some(FormPurpose::EditItem(id)) => return self.submit_edit_item(&id),
            Some(FormPurpose::CreateCategory) => return self.submit_create_category(),
            // sync-setup validates its own required fields (host/user); the key
            // path and remote dir are optional.
            Some(FormPurpose::SyncSetup(name)) => return self.submit_sync_setup(&name),
            // the caption is optional, so the blanket check would refuse a
            // perfectly good photo for having no words under it.
            Some(FormPurpose::MemoPhoto) => return self.submit_memo_photo(),
            _ => {}
        }
        // reject empty required fields (choice fields always have a value)
        let missing = self.form.as_ref().and_then(|form| {
            form.fields
                .iter()
                .find(|f| f.current().trim().is_empty())
                .map(|f| f.name.replace('_', " "))
        });
        if let Some(label) = missing {
            if let Some(form) = self.form.as_mut() {
                form.error = Some(i18n::t(self.settings.lang, &Key::FieldRequired(&label)));
            }
            return;
        }

        match self.form.as_ref().map(|f| f.purpose.clone()) {
            Some(FormPurpose::CreateVault) => self.submit_create(),
            Some(FormPurpose::DeleteVault) => self.submit_manage(false),
            Some(FormPurpose::NukeVault) => self.submit_manage(true),
            Some(FormPurpose::ExportVault) => self.submit_export(),
            Some(FormPurpose::SyncSetup(name)) => self.submit_sync_setup(&name),
            Some(FormPurpose::ImportVault) => self.submit_import(),
            Some(FormPurpose::AddItem(category)) => self.submit_add_item(&category),
            Some(FormPurpose::EditItem(id)) => self.submit_edit_item(&id),
            Some(FormPurpose::CreateCategory) => self.submit_create_category(),
            Some(FormPurpose::RerollEnc) => self.submit_reroll(),
            Some(FormPurpose::NukeItems) => self.submit_nuke_items(),
            Some(FormPurpose::ChangePassword(name)) => self.submit_change_password(&name),
            Some(FormPurpose::MemoPhoto) => self.submit_memo_photo(),
            None => {}
        }
    }

    /// Copies the chosen vault's encrypted file to the destination path. No
    /// password needed — the file is already an encrypted, self-contained blob.
    /// Fields are positional: [vault, destination].
    fn submit_export(&mut self) {
        let Some((name, dest_input)) = self
            .form
            .as_ref()
            .filter(|f| f.fields.len() >= 2)
            .map(|f| (f.fields[0].current().to_string(), f.fields[1].value.as_str().to_string()))
        else {
            return;
        };
        // copying a file mid-rewrite would export a torn / already-stale blob
        if self.refuse_if_rekeying(&name) {
            return;
        }

        let source = vault::vault_path(&self.paths.vaults_dir, &name);
        let mut dest = resolve_path(&dest_input);
        // a folder destination gets the default file name appended
        if dest.is_dir() {
            dest.push(export_file_name(&name));
        }
        // refuse to write into a directory that doesn't exist (don't create trees)
        let parent_ok = match dest.parent() {
            Some(p) => p.as_os_str().is_empty() || p.is_dir(),
            None => false,
        };
        if !parent_ok {
            if let Some(form) = self.form.as_mut() {
                form.error = Some(i18n::t(self.settings.lang, &Key::ExportFailed));
            }
            return;
        }
        // Refuse if anything already exists at the destination. `symlink_metadata`
        // does not follow links, so this single guard prevents silently
        // overwriting a file, writing *through* a symlink to some other target,
        // and truncating the live vault by exporting onto its own file.
        if std::fs::symlink_metadata(&dest).is_ok() {
            if let Some(form) = self.form.as_mut() {
                form.error = Some(i18n::t(self.settings.lang, &Key::ExportDestinationExists));
            }
            return;
        }

        match std::fs::copy(&source, &dest) {
            Ok(_) => {
                set_private_perms(&dest);
                self.form = None;
                self.go_home();
                self.focus = Focus::Vaults;
                let dest_str = dest.to_string_lossy().into_owned();
                self.ok(i18n::t(self.settings.lang, &Key::VaultExported(&name, &dest_str)));
            }
            Err(_) => {
                if let Some(form) = self.form.as_mut() {
                    form.error = Some(i18n::t(self.settings.lang, &Key::ExportFailed));
                }
            }
        }
    }

    // ---- per-vault sync (SFTP-over-SSH, E2E) ---------------------------

    /// `sync [name]`: configured → run it; not configured → open the setup form.
    fn start_sync(&mut self, name_arg: Option<&str>) {
        let name = match name_arg {
            Some(n) => n.to_string(),
            None => match self.page {
                Page::Vault => match self.vault.as_ref() {
                    Some(v) => v.username.clone(),
                    None => return,
                },
                Page::Access => match self.vaults.get(self.selected).cloned() {
                    Some(n) => n,
                    None => return,
                },
            },
        };
        if crate::sync::SyncConfig::exists(&self.paths.vaults_dir, &name) {
            self.sync_vault(&name);
        } else {
            let form = FormState::sync_setup(&name, None, self.settings.lang);
            self.open_form(form);
        }
    }

    /// Saves the endpoint sidecar and kicks off the first sync.
    fn submit_sync_setup(&mut self, name: &str) {
        let lang = self.settings.lang;
        let (host, user, dir, key, auto) = match self.form.as_ref() {
            Some(f) if f.fields.len() >= 5 => (
                f.fields[0].value.trim().to_string(),
                f.fields[1].value.trim().to_string(),
                f.fields[2].value.trim().to_string(),
                f.fields[3].value.trim().to_string(),
                f.fields[4].selected == 1,
            ),
            _ => return,
        };
        if host.is_empty() || user.is_empty() {
            if let Some(form) = self.form.as_mut() {
                let l = i18n::t(lang, &Key::SyncHostHint);
                form.error = Some(i18n::t(lang, &Key::FieldRequired(&l)));
            }
            return;
        }
        // preserve the baseline counter if a sidecar already existed
        let prev = crate::sync::SyncConfig::load(&self.paths.vaults_dir, name);
        let cfg = crate::sync::SyncConfig {
            host,
            user,
            remote_dir: if dir.is_empty() { "~/.datashield".to_string() } else { dir },
            key_path: key,
            auto,
            last_synced: prev.as_ref().and_then(|c| c.last_synced.clone()),
            // a pending re-key survives a re-setup: the server's copy is still
            // the retired key's until a push actually replaces it
            rekeyed: prev.as_ref().is_some_and(|c| c.rekeyed),
            base_counter: prev.map(|c| c.base_counter).unwrap_or(0),
        };
        if cfg.save(&self.paths.vaults_dir, name).is_err() {
            if let Some(form) = self.form.as_mut() {
                form.error = Some(i18n::t(lang, &Key::SaveFailed));
            }
            return;
        }
        self.form = None;
        self.focus = Focus::Vaults;
        // sync now if the vault is open (we have the key); else just confirm
        if self.vault.as_ref().map(|v| v.username.as_str()) == Some(name) {
            self.sync_vault(name);
        } else {
            self.note(i18n::t(lang, &Key::SyncConfigured));
        }
    }

    /// Spawns a sync worker for the OPEN vault `name` (needs the key to read the
    /// remote counter). Bumps + saves locally first so the pushed blob is fresh.
    fn sync_vault(&mut self, name: &str) {
        // the blob is being rewritten under a new key: pushing it now would
        // upload a half-retired copy (and its key is about to change)
        if self.refuse_if_rekeying(name) {
            return;
        }
        let Some(cfg) = crate::sync::SyncConfig::load(&self.paths.vaults_dir, name) else {
            return;
        };
        // only the OPEN vault can be synced (E2E: reading the remote counter
        // needs the in-memory key). From Access, ask the user to unlock first.
        if self.vault.as_ref().map(|v| v.username.as_str()) != Some(name) {
            self.note(i18n::t(self.settings.lang, &Key::SyncConfigured));
            return;
        }
        self.spawn_sync(name, &cfg);
    }

    /// Detaches the sync IO onto a worker thread (the key material is cloned and
    /// zeroized when the thread's copy drops); the result returns on the tick.
    fn spawn_sync(&mut self, name: &str, cfg: &crate::sync::SyncConfig) {
        use std::sync::mpsc;
        let Some(vault) = self.vault.as_ref() else { return };
        if vault.username != name {
            return;
        }
        self.sync_status.insert(name.to_string(), crate::sync::SyncStatus::Syncing);
        let (tx, rx) = mpsc::channel();
        self.sync_rx = Some(rx);
        // snapshot everything the thread needs (it must not touch &self): the
        // key bytes, the local path/counter, the endpoint config.
        let cfg = cfg.clone();
        let key = vault.key_material();
        let vault_path = vault.path.clone();
        let username = vault.username.clone();
        let local = vault.content.sync_counter();
        let local_fp = vault.content.content_fingerprint();
        let dir = self.paths.vaults_dir.clone();
        let name_owned = name.to_string();
        std::thread::spawn(move || {
            let result = crate::sync::run_open_vault_sync(
                &cfg, &key, &vault_path, &username, local, &local_fp, &dir,
            );
            let _ = tx.send((name_owned, result));
        });
    }

    /// Applies any finished sync result (called each tick).
    fn poll_sync(&mut self) {
        use crate::sync::{SyncOutcome, SyncStatus};
        let Some(rx) = self.sync_rx.as_ref() else { return };
        let (name, result) = match rx.try_recv() {
            Ok(got) => got,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            // the worker died without answering: free the slot rather than
            // leave every guard that asks "is a sync out?" wedged forever —
            // and don't leave its vault reading "syncing" for the session
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.sync_rx = None;
                for status in self.sync_status.values_mut() {
                    if *status == SyncStatus::Syncing {
                        *status = SyncStatus::Offline;
                    }
                }
                return;
            }
        };
        self.sync_rx = None;
        // a round that reached the remote and agreed with it settles any
        // pending "the server copy is still under the retired key" flag
        let reconciled = matches!(
            result.outcome,
            SyncOutcome::Pushed | SyncOutcome::Pulled(_) | SyncOutcome::UpToDate
        );
        // persist an advanced baseline
        if let Some(base) = result.new_base {
            if let Some(mut cfg) = crate::sync::SyncConfig::load(&self.paths.vaults_dir, &name) {
                cfg.base_counter = base;
                cfg.last_synced = Some(crate::model::now_rfc3339());
                if reconciled {
                    cfg.rekeyed = false;
                }
                let _ = cfg.save(&self.paths.vaults_dir, &name);
            }
        }
        // adopt pulled content into the open vault
        let mut pulled = false;
        if let Some(content) = result.pulled_content {
            if let Some(v) = self.vault.as_mut() {
                if v.username == name {
                    v.content = content;
                    pulled = true;
                }
            }
        }
        if pulled {
            // a pull replaces the whole content, which is every bit as much a
            // change as a local edit — and the two derived views of it are
            // built from the content, not from the mutation that made it. The
            // agent API would otherwise keep answering with messages another
            // machine deleted, and the pulled text would have no vectors.
            self.refresh_api_index();
            self.rag_reindex();
        }
        let (status, msg): (SyncStatus, String) = match result.outcome {
            SyncOutcome::Pushed => (SyncStatus::Synced, i18n::t(self.settings.lang, &Key::SyncPushed)),
            SyncOutcome::Pulled(_) => (SyncStatus::Synced, i18n::t(self.settings.lang, &Key::SyncPulled)),
            SyncOutcome::UpToDate => (SyncStatus::Synced, i18n::t(self.settings.lang, &Key::SyncUpToDate)),
            SyncOutcome::Diverged { local, remote } => (
                SyncStatus::Diverged,
                format!("diverged — local #{local} vs remote #{remote} (edit on one side, then sync)"),
            ),
            SyncOutcome::Failed(e) => (SyncStatus::Offline, format!("sync failed — {e}")),
        };
        self.sync_status.insert(name, status);
        match status {
            SyncStatus::Synced => self.ok(msg),
            SyncStatus::Diverged | SyncStatus::Offline => self.warn(msg),
            _ => {}
        }
    }

    /// Auto-sync after a mutating save, when this vault opted in.
    ///
    /// This is the hook EVERY LOCAL content mutation ends on, which makes it
    /// the place the agent API's snapshot is refreshed from: an agent must never
    /// search a vault that has moved on under it. A PULL is the other way the
    /// content changes, and [`App::poll_sync`] refreshes it there for the same
    /// reason.
    fn auto_sync_after_change(&mut self) {
        // an added, edited or deleted item leaves a stale vector behind: catch
        // the item index up here, which is the hook every mutation ends on (a
        // no-op when nothing it cares about changed). The Memorandum keeps its
        // own trigger points — unlock, memo-open and the search bar — because a
        // conversation is read where it is written, and one pass per
        // keystroke-sized message would be a save per message.
        self.rag_index_items();
        self.refresh_api_index();
        let Some(name) = self.vault.as_ref().map(|v| v.username.clone()) else { return };
        if crate::sync::SyncConfig::load(&self.paths.vaults_dir, &name).is_some_and(|c| c.auto) {
            self.sync_vault(&name);
        }
    }

    // ---- passwd (change the master password) ---------------------------

    /// True while `name`'s vault file is being re-encrypted by the passwd
    /// worker. Public so the UI can hang the progress bar off that vault's name.
    pub fn rekeying(&self, name: &str) -> bool {
        self.passwd_busy.as_deref() == Some(name)
    }

    /// The guard every entry point that touches a vault file calls: while its
    /// re-encryption is in flight the bytes on disk are about to be rewritten
    /// under a NEW key, so reading, replacing or deleting them now would race
    /// the worker. Returns true (after the toast) when the caller must bail.
    fn refuse_if_rekeying(&mut self, name: &str) -> bool {
        if !self.rekeying(name) {
            return false;
        }
        self.warn(i18n::t(self.settings.lang, &Key::PasswdBusy(name)));
        true
    }

    /// True while a sync worker for `name` is still out. It was handed that
    /// vault's path and, on a pull, ends by renaming its own blob over it.
    pub fn syncing(&self, name: &str) -> bool {
        self.sync_rx.is_some()
            && self.sync_status.get(name) == Some(&crate::sync::SyncStatus::Syncing)
    }

    /// Test hook: puts the app in the state a detached sync worker leaves it
    /// in (the real one does network IO no test can stand up). The returned
    /// sender keeps the channel connected — dropping it ends the "in flight".
    #[cfg(test)]
    pub fn pretend_syncing(
        &mut self,
        name: &str,
    ) -> std::sync::mpsc::Sender<(String, crate::sync::SyncResult)> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.sync_rx = Some(rx);
        self.sync_status.insert(name.to_string(), crate::sync::SyncStatus::Syncing);
        tx
    }

    /// Test hook: puts the app in the state the passwd worker leaves it in
    /// (the real one runs a KDF over a real file). Dropping the returned sender
    /// is exactly a worker that DIED without answering.
    #[cfg(test)]
    pub fn pretend_rekeying(
        &mut self,
        name: &str,
    ) -> std::sync::mpsc::Sender<(String, crate::vault::RekeyOutcome)> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.passwd_rx = Some(rx);
        self.passwd_busy = Some(name.to_string());
        tx
    }

    /// The MIRROR of [`refuse_if_rekeying`](App::refuse_if_rekeying), and just
    /// as load-bearing: a re-encryption must not start while a sync worker is
    /// out, because both end in a `rename` over the very same file and the last
    /// one wins — either the pulled blob is dropped, or the file is left under
    /// the RETIRED password while the app reports the change landed.
    fn refuse_if_syncing(&mut self, name: &str) -> bool {
        if !self.syncing(name) {
            return false;
        }
        self.warn(i18n::t(self.settings.lang, &Key::SyncBusy(name)));
        true
    }

    /// The same guard for the OPEN vault — every in-vault content mutation goes
    /// through it (the worker holds its own handle on the very same file).
    fn refuse_if_open_vault_rekeying(&mut self) -> bool {
        let Some(name) = self.vault.as_ref().map(|v| v.username.clone()) else {
            return false;
        };
        self.refuse_if_rekeying(&name)
    }

    /// Opens the change-password form for the vault the page implies: the open
    /// one inside a vault, the named or highlighted one on the access list.
    fn open_change_password_form(&mut self, name_arg: Option<&str>) {
        let lang = self.settings.lang;
        // an explicitly NAMED vault must resolve to that vault or to nothing:
        // silently falling back to whatever is highlighted turns a typo into a
        // re-key of the wrong file — and the form asks for the CURRENT password,
        // so the mistake surfaces as "wrong passphrase" long after the fact.
        if let Some(arg) = name_arg.map(str::trim).filter(|a| !a.is_empty()) {
            if !self.vaults.iter().any(|v| v.eq_ignore_ascii_case(arg)) {
                self.warn(i18n::t(lang, &Key::UnknownVault(arg)));
                return;
            }
        }
        let name = match self.page {
            // inside a vault the target is the open one: a name that resolves to
            // a DIFFERENT vault is refused, not silently retargeted — the form
            // asks for the current password, so the user would type the other
            // vault's passphrase at the open one and only see "wrong passphrase"
            Page::Vault => {
                let open = self.vault.as_ref().map(|v| v.username.clone());
                if let (Some(arg), Some(open_name)) = (
                    name_arg.map(str::trim).filter(|a| !a.is_empty()),
                    open.as_deref(),
                ) {
                    if !open_name.eq_ignore_ascii_case(arg) {
                        self.warn(i18n::t(lang, &Key::PasswdOtherVault(arg)));
                        return;
                    }
                }
                open
            }
            Page::Access => name_arg
                .and_then(|n| self.vaults.iter().find(|v| v.eq_ignore_ascii_case(n)).cloned())
                .or_else(|| self.vaults.get(self.selected).cloned()),
        };
        let Some(name) = name else {
            self.note(i18n::t(lang, &Key::NoVaults));
            return;
        };
        // one re-encryption at a time (per vault AND overall: the worker slot
        // is single) — a second passwd would race the first one's save().
        if let Some(busy) = self.passwd_busy.clone() {
            self.warn(i18n::t(lang, &Key::PasswdBusy(&busy)));
            return;
        }
        // …and never while a sync worker still holds this vault's path
        if self.refuse_if_syncing(&name) {
            return;
        }
        self.open_form(FormState::change_password(&name, lang));
    }

    /// Validates the three passphrases, then CLOSES the form and hands the slow
    /// part (KDF + re-encrypt) to a worker: verifying the current password is
    /// itself the slow part, so a wrong one surfaces later as an error toast.
    /// Fields are positional: [current, new, confirm].
    fn submit_change_password(&mut self, name: &str) {
        let (current, new, confirm) = match self.form.as_ref() {
            Some(f) if f.fields.len() >= 3 => (
                Zeroizing::new(f.fields[0].value.as_str().to_string()),
                Zeroizing::new(f.fields[1].value.as_str().to_string()),
                Zeroizing::new(f.fields[2].value.as_str().to_string()),
            ),
            _ => return,
        };
        let err = |app: &mut Self, k: Key| {
            let msg = i18n::t(app.settings.lang, &k);
            if let Some(form) = app.form.as_mut() {
                form.error = Some(msg);
            }
        };
        // (the blanket empty-field check already ran; this keeps the rule local)
        if new.trim().is_empty() {
            return err(self, Key::FieldRequired("new"));
        }
        if *new != *confirm {
            return err(self, Key::PasswdMismatch);
        }
        if *new == *current {
            return err(self, Key::PasswdSameAsCurrent);
        }
        if self.passwd_busy.is_some() {
            return err(self, Key::PasswdBusy(name));
        }
        // a sync worker out on this vault ends in its own rename over the same
        // file: whichever lands last wins, so never start the two together
        if self.syncing(name) {
            return err(self, Key::SyncBusy(name));
        }
        // the form leaves NOW (spec): the app is back on the normal screen while
        // the bar next to the vault's name carries the progress.
        self.form = None;
        self.focus = Focus::Vaults;
        self.spawn_change_password(name, current, new);
    }

    /// Detaches the re-encryption onto a worker thread (the two passphrases are
    /// moved into it and wiped when its copies drop); the result returns on the
    /// tick, in `poll_passwd`.
    fn spawn_change_password(&mut self, name: &str, current: Zeroizing<String>, new: Zeroizing<String>) {
        use std::sync::mpsc;
        let path = vault::vault_path(&self.paths.vaults_dir, name);
        let (tx, rx) = mpsc::channel();
        self.passwd_rx = Some(rx);
        self.passwd_busy = Some(name.to_string());
        let name_owned = name.to_string();
        std::thread::spawn(move || {
            let outcome = vault::rekey_file(&path, &current, &new);
            let _ = tx.send((name_owned, outcome));
        });
        self.note(i18n::t(self.settings.lang, &Key::PasswdWorking(name)));
    }

    /// Applies a finished re-encryption (called each tick): the bar clears and
    /// an open handle on that vault adopts the new salt/key, so the session
    /// survives its own password change without a re-unlock.
    fn poll_passwd(&mut self) {
        let Some(rx) = self.passwd_rx.as_ref() else { return };
        let (name, outcome) = match rx.try_recv() {
            Ok(got) => got,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            // the worker died without answering (a panic in the KDF, an OOM
            // kill): free the slot rather than leave `passwd_busy` pinned for
            // the session — it drives the progress bar AND every
            // `refuse_if_rekeying` guard, so a wedged one locks the user out of
            // unlocking, syncing, editing and deleting that vault forever.
            // Exactly what `poll_sync` does with its own dead worker.
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.passwd_rx = None;
                self.passwd_busy = None;
                self.warn(i18n::t(self.settings.lang, &Key::PasswdLost));
                return;
            }
        };
        self.passwd_rx = None;
        self.passwd_busy = None;
        let lang = self.settings.lang;
        match outcome {
            crate::vault::RekeyOutcome::Done(material) => {
                if let Some(v) = self.vault.as_mut() {
                    if v.username == name {
                        v.adopt_rekey(material);
                    }
                }
                // an agent holding this vault open through the API is holding
                // the key the password that just died derived: the vault it
                // could still read is one the user believes is now behind a new
                // password. Its sessions go, by name.
                self.drop_api_sessions_for(&name);
                self.ok(i18n::t(lang, &Key::PasswdChanged(&name)));
                // a rotation that stops at this machine protects nothing: the
                // blob on the server still opens with the RETIRED password.
                // Flag it (the flag outlives the session) and push over it now
                // when we can — the flag is cleared once a sync agrees again.
                self.mark_remote_rekeyed(&name);
            }
            crate::vault::RekeyOutcome::WrongPassword => {
                self.warn(i18n::t(lang, &Key::PasswdWrongCurrent))
            }
            crate::vault::RekeyOutcome::Failed(msg) => {
                let head = i18n::t(lang, &Key::PasswdFailed);
                self.warn(format!("{head} — {msg}"))
            }
        }
    }

    /// Records — in the sync sidecar, so it survives the session — that this
    /// vault's remote copy is still encrypted under the password that was just
    /// retired, and replaces it right away when the vault is open (the only
    /// state that can push). A vault the user re-keyed from the access list is
    /// told what is left to do: the flag makes the next sync push over it.
    fn mark_remote_rekeyed(&mut self, name: &str) {
        let Some(mut cfg) = crate::sync::SyncConfig::load(&self.paths.vaults_dir, name) else {
            return; // this vault doesn't sync — nothing is out there
        };
        cfg.rekeyed = true;
        if cfg.save(&self.paths.vaults_dir, name).is_err() {
            self.warn(i18n::t(self.settings.lang, &Key::SaveFailed));
            return;
        }
        if self.vault.as_ref().map(|v| v.username.as_str()) == Some(name) {
            self.sync_vault(name);
        } else {
            self.warn(i18n::t(self.settings.lang, &Key::PasswdSyncPending(name)));
        }
    }

    /// Verifies the password of the vault at the source path, then homes it into
    /// the data directory (won't overwrite an existing vault of the same name).
    /// Fields are positional: [source, password].
    fn submit_import(&mut self) {
        let Some((source_input, password)) = self
            .form
            .as_ref()
            .filter(|f| f.fields.len() >= 2)
            .map(|f| (f.fields[0].value.as_str().to_string(), Zeroizing::new(f.fields[1].value.as_str().to_string())))
        else {
            return;
        };

        let source = resolve_path(&source_input);
        if !source.is_file() {
            if let Some(form) = self.form.as_mut() {
                form.error = Some(i18n::t(self.settings.lang, &Key::ImportFileNotFound));
            }
            return;
        }

        let mut opened = match Vault::open_file(&source, &password) {
            Ok(v) => v,
            Err(UnlockError::WrongPassword) => {
                if let Some(form) = self.form.as_mut() {
                    form.error = Some(i18n::t(self.settings.lang, &Key::WrongPassword));
                }
                return;
            }
            Err(UnlockError::Corrupted(_)) => {
                if let Some(form) = self.form.as_mut() {
                    form.error = Some(i18n::t(self.settings.lang, &Key::NotAValidVault));
                }
                return;
            }
        };

        // an import that would land ON the vault being re-encrypted must wait,
        // even before the "already exists" check (its file is mid-rewrite)
        if self.rekeying(&opened.username) {
            let msg = i18n::t(self.settings.lang, &Key::PasswdBusy(&opened.username));
            if let Some(form) = self.form.as_mut() {
                form.error = Some(msg);
            }
            return;
        }

        if vault::vault_exists(&self.paths.vaults_dir, &opened.username) {
            // the colliding name is the file's *internal* username (never typed
            // by the user), so name it explicitly to avoid a "this name" mystery
            let msg = i18n::t(self.settings.lang, &Key::VaultExistsNamed(&opened.username));
            if let Some(form) = self.form.as_mut() {
                form.error = Some(msg);
            }
            return;
        }

        // re-home into the data dir: re-encrypts with a fresh nonce and writes
        // atomically with owner-only permissions.
        opened.path = vault::vault_path(&self.paths.vaults_dir, &opened.username);
        let name = opened.username.clone();
        match opened.save() {
            Ok(()) => {
                self.form = None;
                self.reload_vaults();
                self.selected = self.vaults.iter().position(|v| *v == name).unwrap_or(0);
                self.go_home();
                self.focus = Focus::Vaults;
                self.ok(i18n::t(self.settings.lang, &Key::VaultImported(&name)));
            }
            Err(e) => {
                if let Some(form) = self.form.as_mut() {
                    form.error = Some(e.to_string());
                }
            }
        }
    }

    // ---- settings page -------------------------------------------------

    /// Key handling while the tabbed settings page is open. Arrow-driven: on the
    /// tab bar ←/→ switch section and ↓ enters the rows; on a row ↑/↓ move and
    /// ←/→ change the value. Enter saves, Esc cancels (discarding the draft).
    fn on_settings_key(&mut self, code: KeyCode) -> bool {
        let Some(state) = self.settings_ui.as_mut() else {
            return false;
        };
        match code {
            KeyCode::Esc => self.cancel_settings(),
            KeyCode::Enter => self.commit_settings(),
            KeyCode::Up | KeyCode::BackTab => state.move_field(-1),
            KeyCode::Down | KeyCode::Tab => state.move_field(1),
            // a refused change says why: a key press that silently does
            // nothing reads as a broken page, not as a locked row.
            KeyCode::Left | KeyCode::Right => {
                let delta = if code == KeyCode::Left { -1 } else { 1 };
                match state.cycle(delta) {
                    Some(crate::settings_ui::Effect::Hint(why)) => {
                        let lang = self.effective_lang();
                        self.note(i18n::t(lang, &why));
                    }
                    Some(crate::settings_ui::Effect::OpenExtensionPage) => self.open_extension_page(),
                    // the SAVED token, never the draft's: one that only exists
                    // in an unsaved draft is not one the server accepts yet
                    Some(crate::settings_ui::Effect::CopyLoginToken) => {
                        match self.settings.login_token().map(|t| Zeroizing::new(t.to_string())) {
                            Some(token) => self.copy_to_clipboard(token),
                            None => {
                                let lang = self.effective_lang();
                                self.note(i18n::t(lang, &Key::LoginTokenSaveFirst));
                            }
                        }
                    }
                    None => {}
                }
            }
            _ => {}
        }
        false
    }

    /// The `install extension` row: hands the detected browser its own store
    /// page. Every way this cannot happen is a sentence, never silence — no
    /// browser detected, a browser the extension does not exist for, a store
    /// page not configured yet (the extension is not published).
    fn open_extension_page(&mut self) {
        use crate::browser::BrowserFamily;
        let lang = self.effective_lang();
        let Some(browser) = self.detected_browser.clone() else {
            self.warn(i18n::t(lang, &Key::BrowserNotDetected));
            return;
        };
        if matches!(browser.family, BrowserFamily::Safari | BrowserFamily::Unknown) {
            self.warn(i18n::t(lang, &Key::ExtensionUnsupportedBrowser(&browser.name)));
            return;
        }
        let Some(url) = self.chrome.extension_url(browser.family).map(str::to_string) else {
            self.note(i18n::t(lang, &Key::ExtensionNotPublished));
            return;
        };
        match self.open_externally(&url) {
            Ok(()) => self.ok(i18n::t(lang, &Key::ExtensionPageOpened(browser.family.label()))),
            Err(_) => self.warn(i18n::t(lang, &Key::OpenSiteFailed)),
        }
    }

    /// THE one door to the desktop's opener (a browser, an image viewer): the
    /// platform's own, detached (see [`crate::browser::open_url`]). Under test
    /// it spawns nothing and records the target instead, so the suite can prove
    /// what would have opened without opening it.
    fn open_externally(&mut self, target: &str) -> std::io::Result<()> {
        #[cfg(test)]
        {
            self.opened.push(target.to_string());
            Ok(())
        }
        #[cfg(not(test))]
        crate::browser::open_url(target)
    }

    /// `l`: opens the highlighted item's site in the browser AND offers the
    /// browser extension that item's username + password for it — a nonce in
    /// the URL's fragment, claimable once within two minutes from a page of
    /// that site (see `/login/claim`). Every way it cannot happen is a
    /// sentence: no item, no site/user/password on it, login assist off, the
    /// API not listening.
    fn login_in_browser(&mut self) {
        let lang = self.settings.lang;
        let Some(item) = self.highlighted_item() else {
            self.note(i18n::t(lang, &Key::NoItems));
            return;
        };
        let Some(site) = crate::model::item_website(item).map(str::to_string) else {
            self.note(i18n::t(lang, &Key::NoWebsiteField));
            return;
        };
        if crate::model::item_user_index(item).is_none() {
            self.note(i18n::t(lang, &Key::NoUserField));
            return;
        }
        let has_password = crate::model::item_password_index(item)
            .is_some_and(|i| !item.fields[i].value.is_empty());
        if !has_password {
            self.note(i18n::t(lang, &Key::NoPasswordField));
            return;
        }
        let id = item.id.clone();
        if !self.settings.login_assist {
            self.note(i18n::t(lang, &Key::LoginAssistOff));
            return;
        }
        // the switch wants the server up with a vault open; make sure it is
        self.sync_api_server();
        let Some(nonce) = self.api.as_ref().map(|s| s.offer_login(&id)).filter(|n| !n.is_empty()) else {
            self.warn(i18n::t(lang, &Key::LoginNoServer));
            return;
        };
        // the nonce rides the fragment: never sent to the server, read by the
        // extension's content script and stripped from the URL at once. A
        // fragment the stored URL already carries is replaced.
        let base = crate::model::website_url(&site);
        let base = base.split('#').next().unwrap_or(&base).to_string();
        let url = format!("{base}#dsl={}", nonce.as_str());
        let host = crate::favicon::domain_of(&site).unwrap_or(site);
        match self.open_externally(&url) {
            Ok(()) => self.ok(i18n::t(lang, &Key::LoginOffered(&host))),
            Err(_) => self.warn(i18n::t(lang, &Key::OpenSiteFailed)),
        }
    }

    /// `logintoken [new]`: copies the extension's pairing token through the one
    /// clipboard path (timed wipe included — it is a credential), or mints a
    /// fresh one first. With login assist off there is nothing to copy, and it
    /// says where the switch is.
    fn copy_login_token(&mut self, arg: Option<&str>) {
        let lang = self.settings.lang;
        if !self.settings.login_assist {
            self.note(i18n::t(lang, &Key::LoginTokenUnavailable));
            return;
        }
        let renew = arg.is_some_and(|a| a.eq_ignore_ascii_case("new") || a.eq_ignore_ascii_case("nuovo"));
        if renew || self.settings.login_token.is_empty() {
            if renew {
                self.settings.regenerate_login_token();
            } else {
                self.settings.ensure_login_token();
            }
            if self.settings.save(&self.paths.settings_config).is_err() {
                self.warn(i18n::t(lang, &Key::SettingsSaveFailed));
                return;
            }
            // the server checks the token on every request: push the new one
            self.sync_api_server();
            if renew {
                self.note(i18n::t(lang, &Key::LoginTokenRegenerated));
            }
        }
        let Some(token) = self.settings.login_token().map(|t| Zeroizing::new(t.to_string())) else {
            self.note(i18n::t(lang, &Key::LoginTokenUnavailable));
            return;
        };
        self.copy_to_clipboard(token);
    }

    /// Closes the settings page without saving (the live cosmetic preview
    /// reverts automatically once the draft is gone).
    fn cancel_settings(&mut self) {
        self.settings_ui = None;
        self.focus = Focus::Vaults;
        self.note(i18n::t(self.settings.lang, &Key::FormCancelled));
    }

    /// Applies and persists the drafted settings, then closes the page. Also
    /// performs the best-effort kitty translucency nudge.
    fn commit_settings(&mut self) {
        let Some(state) = self.settings_ui.take() else {
            return;
        };
        let was_on = self.settings.blur == Blur::On;
        let mut draft = state.draft;
        // switching login assist on is what mints the pairing token: the
        // extension has nothing to present until then, and a token that only
        // exists once the user asked for the feature is one less secret in a
        // file for everybody who never did
        if draft.login_assist {
            draft.ensure_login_token();
        }
        // the draft moves into `self.settings` below; the handful of scalars the
        // tail of this function still needs are taken while it is here.
        let (drafted_blur, wants_rag) = (draft.blur, draft.rag_enabled());
        // THE PANIC SWITCH. `api_vault_unlock` going off is the user saying "no
        // agent holds a vault of mine open any more": every live session dies
        // here, before anything else, whether or not the server itself survives
        // (an open vault with its indexes served keeps it listening).
        let unlock_revoked = self.settings.api_vault_unlock && !draft.api_vault_unlock;
        self.focus = Focus::Vaults;

        // Persist BEFORE touching live state or nudging kitty, so a write
        // failure leaves the session exactly as it was (nothing half-applied).
        if draft.save(&self.paths.settings_config).is_err() {
            self.warn(i18n::t(self.settings.lang, &Key::SettingsSaveFailed));
            return;
        }
        self.settings = draft;

        // Best-effort translucency: apply when turning on, restore when off
        // (only worth a subprocess if it was on before).
        let outcome = match drafted_blur {
            Blur::On => Some(crate::blur::set_translucent(true)),
            Blur::Off => {
                if was_on {
                    let _ = crate::blur::set_translucent(false);
                }
                None
            }
        };

        match outcome {
            Some(crate::blur::Outcome::Applied) => self.ok(i18n::t(self.settings.lang, &Key::BlurApplied)),
            Some(crate::blur::Outcome::NotKitty) => self.note(i18n::t(self.settings.lang, &Key::BlurUnsupported)),
            Some(crate::blur::Outcome::Failed(e)) => {
                self.warn(format!("{}: {e}", i18n::t(self.settings.lang, &Key::BlurFailed)))
            }
            None => self.ok(i18n::t(self.settings.lang, &Key::SettingsSaved)),
        }

        // Last, so that what it has to say wins the status line: "the model is
        // coming down" and "the API is listening on 7737" are both news, and
        // "settings saved" is not.
        //
        // The RAG toggles own the engine. Turning either on with no model on
        // this machine starts the one-time download in the background; turning
        // both off frees the model at once.
        self.rag.set_enabled(wants_rag);
        if wants_rag && !self.rag.model_present() {
            // saying "fetching the model" again over a download that is already
            // running would read as a second one starting
            let already = matches!(self.rag.status, crate::rag::RagStatus::Downloading);
            self.rag.start_download();
            if !already {
                self.note(i18n::t(self.settings.lang, &Key::RagModelDownloadStarted));
            }
        }
        self.rag_reindex();
        if unlock_revoked {
            if let Some(server) = self.api.as_ref() {
                server.drop_all_sessions();
            }
        }
        // …and the API follows the exposure toggles, the unlock switch and the
        // port, start to stop (and re-reads the whole policy on the way).
        self.sync_api_server();
    }

    /// Delete (`nuke == false`) or empty (`nuke == true`) the chosen vault
    /// after verifying its password. Fields are positional: [vault, password].
    fn submit_manage(&mut self, nuke: bool) {
        let Some((name, password)) = self.form.as_ref().filter(|f| f.fields.len() >= 2).map(|f| {
            (
                f.fields[0].current().to_string(),
                Zeroizing::new(f.fields[1].value.as_str().to_string()),
            )
        }) else {
            return;
        };
        // deleting / emptying a file the passwd worker is rewriting would race it
        if self.refuse_if_rekeying(&name) {
            return;
        }

        let opened = match Vault::open(&self.paths.vaults_dir, &name, &password) {
            Ok(v) => v,
            Err(UnlockError::WrongPassword) => {
                if let Some(form) = self.form.as_mut() {
                    form.error = Some(i18n::t(self.settings.lang, &Key::WrongPassword));
                }
                return;
            }
            Err(UnlockError::Corrupted(msg)) => {
                if let Some(form) = self.form.as_mut() {
                    form.error = Some(msg);
                }
                return;
            }
        };
        let items = opened.items().len();

        let result = if nuke {
            let mut v = opened;
            v.items_mut().clear();
            v.save()
        } else {
            drop(opened); // release the vault before removing its file
            vault::delete_vault_file(&self.paths.vaults_dir, &name)
        };

        match result {
            Ok(()) => {
                self.form = None;
                // an agent holding this vault open through the API is holding
                // the whole of what the user has just destroyed: the session
                // reads its own decrypted copy in memory, so unlinking the file
                // (or clearing every item in it) does not touch what it can
                // still answer with. Deleting a vault says more than a re-key
                // does, and the re-key is the path that already drops them.
                self.drop_api_sessions_for(&name);
                self.reload_vaults();
                self.selected = 0;
                self.go_home();
                self.focus = Focus::Vaults;
                let key = if nuke {
                    Key::VaultNuked(&name, items)
                } else {
                    Key::VaultDeleted(&name, items)
                };
                self.ok(i18n::t(self.settings.lang, &key));
            }
            Err(e) => {
                if let Some(form) = self.form.as_mut() {
                    form.error = Some(e.to_string());
                }
            }
        }
    }

    fn submit_create(&mut self) {
        // Fields are positional: name, password, algorithm(choice), kdf(choice).
        let Some(form) = self.form.as_ref().filter(|f| f.fields.len() >= 4) else {
            return;
        };
        let name = form.fields[0].value.trim().to_string();
        let password = Zeroizing::new(form.fields[1].value.as_str().to_string());
        let algorithm = Algorithm::from_name(form.fields[2].current()).unwrap_or(Algorithm::Aes256Gcm);
        let kdf = KdfSpec::from_name(form.fields[3].current()).unwrap_or_default();

        match Vault::create(
            &self.paths.vaults_dir,
            &name,
            &password,
            algorithm,
            kdf,
            "Vanilla",
        ) {
            Ok(_) => {
                self.form = None;
                self.reload_vaults();
                self.selected = self.vaults.iter().position(|v| *v == name).unwrap_or(0);
                self.go_home();
                self.focus = Focus::Vaults;
                self.ok(i18n::t(self.settings.lang, &Key::VaultCreated(&name)));
            }
            Err(e) => {
                let msg = self.create_error_message(&e);
                if let Some(form) = self.form.as_mut() {
                    form.error = Some(msg);
                }
            }
        }
    }

    /// A friendly, localized message for the common vault-creation failures.
    fn create_error_message(&self, error: &anyhow::Error) -> String {
        if error.to_string().contains("already exists") {
            i18n::t(self.settings.lang, &Key::VaultExists)
        } else {
            i18n::t(self.settings.lang, &Key::CreateFailed)
        }
    }

    fn set_language(&mut self, arg: &str) {
        match Lang::from_arg(arg) {
            Some(l) => self.apply_language(l),
            None => self.warn(i18n::t(self.settings.lang, &Key::UnknownLanguage(arg))),
        }
    }

    fn apply_language(&mut self, l: Lang) {
        // the language lives in settings.lang (the single source of truth);
        // persist it so the `language` command survives a restart and can't be
        // reverted by a later save from the settings page.
        self.settings.lang = l;
        let _ = self.settings.save(&self.paths.settings_config);
        self.ok(i18n::t(l, &Key::LanguageChanged(l)));
    }
}
