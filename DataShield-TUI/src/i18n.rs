//! Minimal internationalization. For now: English and Italian.
//!
//! Add a new language by extending [`Lang`], its `ALL`/`code`/`name`/`from_arg`
//! arms, and the match in [`t`].

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    #[default]
    En,
    It,
}

impl Lang {
    pub const ALL: [Lang; 2] = [Lang::En, Lang::It];

    /// Short code used in the `language <code>` command.
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::It => "it",
        }
    }

    /// Endonym shown in the language list.
    pub fn name(self) -> &'static str {
        match self {
            Lang::En => "English",
            Lang::It => "Italiano",
        }
    }

    /// The key that answers YES to a `y`/`n` confirm in this language (`y` /
    /// `s`ì). The ONE place it is decided: the prompt reads it (see
    /// `App::confirm_key`) and so does [`crate::keymap::RESERVED`], which has to
    /// keep every locale's own out of the hands of a customized verb — that key
    /// is answered before the keymap is ever consulted.
    pub fn confirm_key(self) -> char {
        match self {
            Lang::En => 'y',
            Lang::It => 's',
        }
    }

    /// Accepts codes and names in either language, case-insensitively.
    pub fn from_arg(s: &str) -> Option<Lang> {
        match s.to_ascii_lowercase().as_str() {
            "en" | "eng" | "english" | "inglese" => Some(Lang::En),
            "it" | "ita" | "italian" | "italiano" => Some(Lang::It),
            _ => None,
        }
    }
}

/// Translatable messages.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key<'a> {
    /// First line of the page help; carries the current language command name.
    LanguageHint(&'a str),
    /// Header above the language list.
    AvailableLanguages,
    /// Header above the `~` direction picker.
    AvailableDirections,
    /// Labels for the `~` picker options.
    PanelBackLabel,
    PanelForwLabel,
    /// Keys hint under a picker list.
    SuggestKeys,
    /// Status-bar key hints, contextual to page/focus.
    HintAccessList,
    HintVaultList,
    HintPanel,
    /// Left-panel title on the Access page.
    VaultsTitle,
    /// Empty-state lines for the vault list and the item list.
    NoVaults,
    NoVaultsHint,
    NoItems,
    NoItemsHint,
    /// Placeholder body for a command that isn't built yet.
    TodoNext,
    /// Usage shown when `~` is run without a valid direction.
    PanelUsage,
    /// `~ back` with nothing before it.
    NoBack,
    /// `~ forw` with nothing ahead.
    NoForward,
    /// Key hints shown while editing a form.
    FormKeys,
    /// A required form field left empty (carries the field label).
    FieldRequired(&'a str),
    /// A vault was created (carries its name).
    VaultCreated(&'a str),
    /// A vault with that name already exists.
    VaultExists,
    /// Vault creation failed for another reason.
    CreateFailed,
    /// Title of the unlock form (carries the vault name).
    UnlockTitle(&'a str),
    /// Hint under the unlock password field.
    UnlockHint,
    /// Hint under the createVault encryption-algorithm choice.
    AlgorithmHint,
    /// Hint under the createVault key-derivation choice.
    KdfHint,
    /// A vault was unlocked (carries its name).
    VaultUnlocked(&'a str),
    /// Wrong password when unlocking.
    WrongPassword,
    /// Titles + chooser hint + result messages for the manage (delete/nuke) forms.
    DeleteTitle,
    NukeTitle,
    VaultChoiceHint,
    DeleteWarning,
    NukeWarning,
    /// A vault was deleted / emptied (name + item count).
    VaultDeleted(&'a str, usize),
    VaultNuked(&'a str, usize),
    /// Export / import forms: titles, field hints and results.
    ExportTitle,
    ImportTitle,
    ExportPathHint,
    ImportPathHint,
    ImportPasswordHint,
    /// A vault was exported (name + destination path) / imported (name).
    VaultExported(&'a str, &'a str),
    VaultImported(&'a str),
    /// Export/import failures.
    ExportFailed,
    ExportDestinationExists,
    ImportFileNotFound,
    NotAValidVault,
    /// Import collision: a vault with this (file-internal) name already exists.
    VaultExistsNamed(&'a str),
    /// The vault was closed (returned to the access page).
    VaultClosed,
    /// Settings page: title, section tabs, per-row labels, value labels, hints.
    SettingsTitle,
    SettingsTabGeneral,
    SettingsTabTui,
    SettingsTabAccessibility,
    SettingLabelTheme,
    SettingLabelBackground,
    SettingLabelLanguage,
    SettingLabelBlur,
    SettingLabelBackdrop,
    SettingLabelBackdropFx,
    SettingLabelBackdropLevel,
    SettingLabelAutoLock,
    SettingLabelShowLogo,
    SettingLabelFavicons,
    SettingLabelVaultCount,
    SettingLabelLivePreview,
    SettingLabelArrowPanel,
    SettingLabelOneshot,
    SettingLabelAutocomplete,
    SettingLabelReduceMotion,
    SettingLabelConfirmExit,
    BackgroundTransparent,
    BackgroundOpaque,
    BlurOff,
    BlurKitty,
    /// Generic on/off value shown for boolean toggles.
    ToggleOn,
    ToggleOff,
    SettingHintTheme,
    SettingHintBackground,
    SettingHintLanguage,
    SettingHintBlur,
    SettingHintBackdrop,
    SettingHintBackdropFx,
    SettingHintBackdropLevel,
    SettingHintAutoLock,
    SyncTitle,
    SyncHostHint,
    SyncUserHint,
    SyncDirHint,
    SyncKeyHint,
    SyncAutoHint,
    SyncPushed,
    SyncPulled,
    SyncUpToDate,
    SyncConfigured,
    /// Something was attempted on a vault whose sync worker is still out (it
    /// holds that vault's file and may rewrite it when it lands).
    SyncBusy(&'a str),
    /// ---- change master password (`passwd`) ----------------------------
    /// Form title (carries the vault name) and the three field hints.
    PasswdTitle(&'a str),
    PasswdCurrentHint,
    PasswdNewHint,
    PasswdConfirmHint,
    /// The two new passphrases differ / the new one is the current one.
    PasswdMismatch,
    PasswdSameAsCurrent,
    /// The worker started (carries the vault name) and the short label riding
    /// the progress bar next to that vault's name.
    PasswdWorking(&'a str),
    PasswdInProgress,
    /// The re-encryption landed (carries the vault name).
    PasswdChanged(&'a str),
    /// The current passphrase turned out to be wrong (surfaced late, from the
    /// worker) / the file couldn't be rewritten.
    PasswdWrongCurrent,
    PasswdFailed,
    /// Something was attempted on a vault whose re-encryption is still running.
    PasswdBusy(&'a str),
    /// A command named a vault that isn't there (a typo must never be resolved
    /// to "whatever is highlighted" — see `open_change_password_form`).
    UnknownVault(&'a str),
    /// The passwd worker died without answering (its slot is freed).
    PasswdLost,
    /// The password changed, but the vault isn't open, so the copy on the
    /// server is still the retired key's until the next sync replaces it.
    PasswdSyncPending(&'a str),
    /// `passwd <name>` named a DIFFERENT vault while one is open: inside a
    /// vault only the open one can be re-keyed (see `open_change_password_form`).
    PasswdOtherVault(&'a str),
    SettingHintShowLogo,
    SettingHintFavicons,
    SettingHintVaultCount,
    SettingHintLivePreview,
    SettingHintArrowPanel,
    SettingHintOneshot,
    SettingHintAutocomplete,
    SettingHintReduceMotion,
    SettingHintConfirmExit,
    /// One-line hint pinned to the idle `~` splash.
    SplashHint,
    /// Key hints shown at the bottom of the settings page.
    SettingsKeys,
    /// Shown at the top level when confirm-on-exit is enabled.
    ConfirmQuit,
    /// Settings results shown in the status bar.
    SettingsSaved,
    SettingsSaveFailed,
    BlurApplied,
    BlurUnsupported,
    /// kitty translucency failed (a reason is appended by the caller).
    BlurFailed,
    /// The form was dismissed with Esc.
    FormCancelled,
    LanguageChanged(Lang),
    UnknownLanguage(&'a str),
    UnknownCommand(&'a str),
    /// A one-shot createVault got an unrecognised algorithm / KDF name.
    UnknownAlgorithm(&'a str),
    UnknownKdf(&'a str),
    /// One-shot createVault: too many arguments.
    TooManyArgs,
    /// The lone 2nd token is a crypto name — likely meant as algorithm/KDF, but
    /// it would become the password.
    PasswordLooksLikeCrypto(&'a str),
    /// A KDF name was given where the algorithm is expected (they are ordered).
    AlgorithmBeforeKdf(&'a str),
    /// ---- in-vault item management -------------------------------------
    /// Header above the Add category picker.
    ChooseCategory,
    /// Localized display name of a category id (built-ins); falls back to the id.
    CategoryName(&'a str),
    /// Localized label for a field key; falls back to the key with `_`→spaces.
    FieldLabel(&'a str),
    /// Title of the add-item form (carries the category display name).
    AddItemTitle(&'a str),
    /// Hint under the mandatory item label field.
    ItemLabelHint,
    /// An item was added to the vault (carries its label).
    ItemAdded(&'a str),
    /// A command exists but its flow isn't wired up yet.
    ComingSoon,
    /// Key hints while viewing an item's detail.
    HintItemDetail,
    /// An item with no fields.
    ItemDetailEmpty,
    /// The "All categories" tab label.
    TabAll,
    /// The Memorandum's pseudo-chip at the end of the same row: a stop in the
    /// Tab cycle, never a category (nothing is ever filtered by it).
    TabMemo,
    /// Placeholder shown in the (empty) search box.
    SearchPlaceholder,
    // ---- in-vault commands (shared) -----------------------------------
    /// A mutation couldn't be written to disk (nothing changed).
    SaveFailed,
    /// No item matches the given name.
    ItemNotFound(&'a str),
    /// Option labels for every default-No confirm choice (compared by index).
    ConfirmNo,
    ConfirmYes,
    // ---- star ---------------------------------------------------------
    ItemStarred(&'a str),
    ItemUnstarred(&'a str),
    // ---- delete -------------------------------------------------------
    DeleteItemTitle,
    ItemChoiceHint,
    ConfirmDeleteHint,
    ItemDeleted(&'a str),
    DeleteItemWarning,
    // ---- edit ---------------------------------------------------------
    EditItemTitle(&'a str),
    ItemEdited(&'a str),
    EditSecretHint,
    // ---- category -----------------------------------------------------
    CategoryTitle,
    CategoryNameHint,
    CategoryFieldNameLabel(usize),
    CategoryFieldTypeLabel(usize),
    CategoryFieldSecretLabel(usize),
    CategoryFieldNameHint,
    CategoryFieldTypeHint,
    CategoryFieldSecretHint,
    CategoryCreated(&'a str),
    CategoryReserved(&'a str),
    CategoryExists(&'a str),
    CategoryNeedsField,
    CategoryDuplicateField(&'a str),
    CategoryBadName,
    // ---- encInfo ------------------------------------------------------
    EncInfoTitle,
    EncInfoKeyNeverStored,
    EncInfoFreshNonce,
    EncInfoHardened,
    EncInfoFast,
    EncInfoBroken,
    EncInfoContents(usize, usize),
    EncInfoUpgradePending,
    EncInfoNeverModified,
    EncInfoItemFields(usize, usize),
    EncInfoItemNote,
    // ---- rerollEnc ----------------------------------------------------
    RerollTitle,
    VaultRerolled(&'a str, &'a str),
    // ---- nuke (in-vault) ----------------------------------------------
    NukeItemsTitle,
    NukeItemsNameHint,
    NukeItemsConfirmHint,
    NukeItemsNotConfirmed,
    NukeItemsNameMismatch,
    NukeItemsWarning,
    ItemsNuked(usize),
    // ---- docked detail: field cursor, copy, inline edit ----------------
    /// A value reached the clipboard (carries the auto-clear delay, seconds).
    Copied(u64),
    /// No clipboard to copy to (no display server / a locked-down session).
    CopyFailed,
    /// The row under the detail cursor holds nothing worth copying.
    NothingToCopy,
    /// Placeholder on an inline-edited SECRET row: confirming empty keeps the
    /// stored value (the same contract the edit form's secret rows have).
    InlineSecretHint,
    /// One field was rewritten in place (carries its label).
    FieldUpdated(&'a str),
    /// An inline edit confirmed without changing anything.
    FieldUnchanged,
    /// A field was taken off the item entirely (carries its label) — what `-`
    /// on a picture row does, since blank there means "keep it".
    FieldRemoved(&'a str),
    /// The row an inline edit started on is no longer there when it is
    /// confirmed (a sync pull landed mid-edit): nothing is written.
    FieldGone,
    /// `c`/`u` on an item that has no password / no user row at all.
    NoPasswordField,
    NoUserField,
    /// `o` on an item that carries no website.
    NoWebsiteField,
    /// The site was handed to the desktop's browser (carries the URL).
    OpeningSite(&'a str),
    /// No browser opener answered (`xdg-open` missing / no desktop session).
    OpenSiteFailed,
    // ---- image fields --------------------------------------------------
    /// What an image field's ledger row shows (format label + human size) —
    /// never its base64.
    ImageField(&'a str, &'a str),
    /// Hint under an image row of the add form: it takes a FILE PATH.
    ImageFieldHint,
    /// Hint under an image row of the edit form (empty keeps what is stored).
    ImageEditHint,
    /// `c` on an image row: there is nothing text-shaped to copy.
    ImageNotCopyable,
    /// The imported path doesn't exist / can't be read (carries the path).
    ImageUnreadable(&'a str),
    /// The file is over the import cap (carries the cap, e.g. `8.0 MiB`).
    ImageTooLarge(&'a str),
    /// The file isn't one of the accepted formats (carries the list + the cap).
    ImageUnsupported(&'a str, &'a str),
    /// `o` on an image row: the picture went to the system viewer. It says out
    /// loud that a CLEARTEXT copy now exists, and when it goes away again.
    ImageOpened,
    /// `o` on an image row whose stored value isn't base64: nothing to open.
    ImageUndecodable,
    /// The temporary copy couldn't be written (no writable runtime dir, a full
    /// disk, a name someone else got to first).
    ImageExportFailed,
    /// No image viewer answered (`xdg-open` missing / no desktop session).
    ImageOpenFailed,
    // ---- the Memorandum -------------------------------------------------
    /// The chat's title rule, and what the palette calls the view.
    MemoTitle,
    /// Shown in place of the thread when nothing has been written yet.
    MemoEmpty,
    /// A day separator, from a stored `YYYY-MM-DD`.
    MemoDay(&'a str),
    /// The composer's placeholder while it is empty.
    MemoComposerHint,
    /// Title of the attach-photo modal.
    MemoPhotoTitle,
    /// Its two field names (the path row, then the optional caption).
    MemoPhotoField,
    MemoCaptionField,
    /// Hint under the caption row: it may be left blank.
    MemoCaptionHint,
    /// What a copy of a captionless photo message puts on the clipboard — the
    /// picture named in words, never its base64.
    MemoCopyImage,
    // ---- searching the vault: the Memorandum (Ctrl+F) and the item list (/)
    // The two bars are two views of ONE search: the mode chip, the "working"
    // line and the fallback reasons are the same words on both, so they are the
    // same keys. Only the strip below the memo's bar is its own.
    /// The search bar's placeholder while the query is empty.
    MemoSearchHint,
    /// The two mode chips. The glyph is the renderer's; these are the words.
    SearchModeLiteral,
    SearchModeRag,
    /// The rule over the results strip.
    MemoSearchResults,
    /// The strip's empty state — a search that ran and found nothing.
    MemoSearchNoResults,
    /// …and the one while the query is being embedded on the worker.
    SearchWorking,
    /// Why a RAG search ran literally instead: the feature is off, the model
    /// is not here yet, or it is still coming down the wire. Never a dead
    /// search and never a silent wrong mode — the chip then says "literal".
    SearchRagOff,
    SearchRagAbsent,
    SearchRagDownloading,
    // ---- local semantic search -------------------------------------------
    /// The status line for each state of the on-device engine.
    RagStatusOff,
    RagStatusAbsent,
    RagStatusDownloading,
    RagStatusIndexing,
    RagStatusReady,
    /// Carries the reason, ALREADY localized (never anything out of the vault):
    /// one of the five below when this crate wrote it, the engine's own words
    /// when it did not. See `rag::RagFault`.
    RagStatusFailed(&'a str),
    /// The reasons this crate writes itself, so a failure line is never half
    /// English under an Italian sentence.
    RagFaultModelWorker,
    RagFaultSearchWorker,
    RagFaultVectorCount,
    RagFaultNoVector,
    RagFaultModelLock,
    /// Toast when the one-time download finishes.
    RagModelReady,
    // ---- the RAG settings section + the localhost agent API --------------
    /// The five editable rows and the read-only token row.
    SettingLabelRagMemo,
    SettingLabelRagItems,
    SettingLabelApiMemo,
    SettingLabelApiItems,
    SettingLabelApiPort,
    SettingLabelApiToken,
    SettingHintRagMemo,
    SettingHintRagItems,
    SettingHintApiMemo,
    SettingHintApiItems,
    SettingHintApiPort,
    SettingHintApiToken,
    /// Why an exposure row refused to turn on: its index is not built.
    SettingHintApiNeedsRag,
    /// The three per-vault-session rows: `/unlock`, `/reveal` and the idle
    /// expiry of a session. Their hints say PLAINLY what each one hands over —
    /// a toggle whose cost is not on the row is a toggle nobody consented to.
    SettingLabelApiUnlock,
    SettingLabelApiReveal,
    SettingLabelApiSessionTtl,
    SettingHintApiUnlock,
    SettingHintApiReveal,
    SettingHintApiSessionTtl,
    /// Why the session-expiry row is frozen: `/unlock` is off, so there is no
    /// session for it to expire.
    SettingHintApiNeedsUnlock,
    /// The cheatsheet row behind the `api·N` indicator: what those N are.
    CheatApiSessions,
    // ---- the BROWSER settings section (login assist, see docs/LOGIN-ASSIST.md)
    SettingLabelLoginAssist,
    SettingLabelBrowser,
    SettingLabelInstallExtension,
    SettingLabelLoginToken,
    SettingHintLoginAssist,
    SettingHintBrowser,
    SettingHintInstallExtension,
    SettingHintLoginToken,
    /// The value column of the action row (`open the store page ▸`).
    SettingValueInstallExtension,
    /// The browser row when the desktop named none.
    BrowserNotDetected,
    /// The store page went to the browser (carries the family name).
    ExtensionPageOpened(&'a str),
    /// No store page is configured yet: how to load it by hand meanwhile.
    ExtensionNotPublished,
    /// The detected browser is not one the extension can be installed in.
    ExtensionUnsupportedBrowser(&'a str),
    /// `logintoken` with login assist off / no token minted.
    LoginTokenUnavailable,
    /// The token row pressed before the settings that mint it were saved.
    LoginTokenSaveFirst,
    /// The word after the token's head on its row (`2144d90f… · copy ▸`).
    SettingValueCopy,
    /// The browser asked for a login at this host while no vault was open.
    BrowserWantsLogin(&'a str),
    /// A login made in the browser was written into the vault (new item).
    BrowserLoginSaved(&'a str),
    /// …or an existing item's password was replaced by it.
    BrowserLoginUpdated(&'a str),
    /// `l` on an item while login assist is off: where the switch is.
    LoginAssistOff,
    /// `l` opened the site and offered the extension the login (carries the
    /// host).
    LoginOffered(&'a str),
    /// `l` while the API could not come up (the port is taken).
    LoginNoServer,
    /// `logintoken new`: a fresh pairing token was minted.
    LoginTokenRegenerated,
    /// …and the same indicator's gloss on the LOCK screen, where there is no
    /// top bar to carry it and nobody at the keyboard to ask. A lock closes THIS
    /// vault; an agent's own sessions are not behind it, and the one place that
    /// says so must be the screen the machine is left on.
    LockApiSessions,
    // ---- the shortcuts settings section ---------------------------------
    /// The two section rules over the one-row-per-verb list: the keymap is
    /// page-scoped, so the rows are grouped by the page they fire on (the same
    /// word is two different verbs across them).
    SettingSectionShortcutsAccess,
    SettingSectionShortcutsVault,
    /// The one hint under every row of it: what "off" costs, and the `:`
    /// palette that reaches every verb whatever is chosen here.
    SettingHintShortcut,
    /// A row's value when the verb is switched off.
    SettingShortcutOff,
    /// …and the tag on the built-in key (`a · default`).
    SettingShortcutDefault,
    /// The empty-vault line, carrying the key that adds an item (the user's own
    /// character, whatever it is)…
    VaultEmptyHint(&'a str),
    /// …or the palette, when that verb is switched off: the way in that no
    /// choice on the shortcuts page can ever take away.
    VaultEmptyHintPalette,
    /// The same pair for the Access page's empty state, carrying the key that
    /// makes a vault — read through the same resolution, so it can never name a
    /// key the keybar and the cheatsheet next to it disagree with.
    AccessEmptyHint(&'a str),
    AccessEmptyHintPalette,
    /// A hand-edited `settings.toml` asked for keys that could not be granted
    /// (reserved, malformed or taken); carries the verbs' slugs. Said once, at
    /// startup, beside the other config warnings.
    ShortcutOverridesRefused(&'a str),
    /// The server came up / went down (the port it was listening on).
    ApiStarted(u16),
    ApiStopped,
    /// The port is taken by something else — the server stays off.
    ApiPortBusy(u16),
    /// `ragtoken` with no server running: there is no session token to copy.
    ApiTokenUnavailable,
    /// Toast when a `rag_*` toggle asks for the one-time model download.
    RagModelDownloadStarted,
    /// …and the label on the progress bar that download gets in the status row,
    /// next to the ⇣ glyph. Short: it shares one row with the bar, the
    /// percentage and the MiB counter.
    RagModelDownloadLabel,
    // ---- the `?` cheatsheet ---------------------------------------------
    /// The one cheatsheet row that is not a keymap bind: Tab walks the chips
    /// row, the Memorandum's own stop included. (The row under it says
    /// `literal · RAG`, which is the two mode chips' own words — the cheatsheet
    /// borrows them rather than spelling them a second time.)
    CheatTabCycle,
    // ---- keybar chips ---------------------------------------------------
    KeybarMove,
    KeybarUnlock,
    KeybarDetail,
    KeybarFilter,
    KeybarCategory,
    KeybarCmd,
    KeybarKeys,
    KeybarCopy,
    KeybarEditField,
    KeybarEditForm,
    KeybarDelete,
    KeybarStar,
    KeybarList,
    KeybarConfirm,
    KeybarCancel,
    KeybarSend,
    /// The composer's newline chord (Shift+⏎ where the terminal spells it out,
    /// Alt+⏎ everywhere else — the renderer picks; this is only the word).
    KeybarNewline,
    KeybarPhoto,
    KeybarScroll,
    KeybarClose,
    /// Ctrl+F in the Memorandum — and, once the bar is up, what ⏎ does there
    /// until there are hits to jump to.
    KeybarSearch,
    KeybarJump,
    KeybarSearchMode,
    KeybarHits,
    /// Esc in the item list's `/` box: it empties the query (and the ranking a
    /// RAG search left on the list) before it gives the keyboard back.
    KeybarClear,
}

pub fn t(lang: Lang, key: &Key) -> String {
    match key {
        Key::LanguageHint(cmd) => match lang {
            Lang::En => format!("to change language use the command '{cmd} [lang]'"),
            Lang::It => format!("per cambiare lingua usa il comando '{cmd} [lang]'"),
        },
        Key::AvailableLanguages => match lang {
            Lang::En => "available languages:".into(),
            Lang::It => "lingue disponibili:".into(),
        },
        Key::AvailableDirections => match lang {
            Lang::En => "move through the ~ history:".into(),
            Lang::It => "muoviti nello storico di ~:".into(),
        },
        Key::PanelBackLabel => match lang {
            Lang::En => "previous view".into(),
            Lang::It => "vista precedente".into(),
        },
        Key::PanelForwLabel => match lang {
            Lang::En => "next view".into(),
            Lang::It => "vista successiva".into(),
        },
        Key::SuggestKeys => match lang {
            Lang::En => "↑/↓ choose · Tab autocomplete · Enter confirm".into(),
            Lang::It => "↑/↓ scegli · Tab completa · Invio conferma".into(),
        },
        Key::HintAccessList => match lang {
            Lang::En => "↑/↓ move · Enter unlock · type help for commands · Esc quit".into(),
            Lang::It => "↑/↓ muovi · Invio sblocca · scrivi help per i comandi · Esc esci".into(),
        },
        Key::HintVaultList => match lang {
            Lang::En => {
                "↑/↓ move · Enter open · Ctrl+E edit · Ctrl+S ★ · Shift+Tab category · Ctrl+F search · help".into()
            }
            Lang::It => {
                "↑/↓ muovi · Invio apri · Ctrl+E modifica · Ctrl+S ★ · Shift+Tab categoria · Ctrl+F cerca · help".into()
            }
        },
        Key::HintPanel => match lang {
            Lang::En => "↑/↓ move · Enter select · ← back to list · Esc back".into(),
            Lang::It => "↑/↓ muovi · Invio seleziona · ← torna alla lista · Esc indietro".into(),
        },
        Key::VaultsTitle => match lang {
            Lang::En => "vaults".into(),
            Lang::It => "vault".into(),
        },
        Key::NoVaults => match lang {
            Lang::En => "no vault yet".into(),
            Lang::It => "ancora nessun vault".into(),
        },
        Key::NoVaultsHint => match lang {
            Lang::En => "type createVault".into(),
            Lang::It => "scrivi createVault".into(),
        },
        Key::NoItems => match lang {
            Lang::En => "no items yet".into(),
            Lang::It => "ancora nessun elemento".into(),
        },
        Key::NoItemsHint => match lang {
            Lang::En => "type add to add one".into(),
            Lang::It => "scrivi add per aggiungerne uno".into(),
        },
        Key::TodoNext => match lang {
            Lang::En => "not wired up yet — coming in the next step".into(),
            Lang::It => "non ancora pronto — arriva al prossimo step".into(),
        },
        Key::PanelUsage => match lang {
            Lang::En => "usage: ~ back / ~ forw".into(),
            Lang::It => "uso: ~ back / ~ forw".into(),
        },
        Key::NoBack => match lang {
            Lang::En => "no previous view in ~".into(),
            Lang::It => "nessuna vista precedente in ~".into(),
        },
        Key::NoForward => match lang {
            Lang::En => "no next view in ~".into(),
            Lang::It => "nessuna vista successiva in ~".into(),
        },
        Key::FormKeys => match lang {
            Lang::En => "Tab/↑↓ move · Enter confirm · Esc cancel".into(),
            Lang::It => "Tab/↑↓ muovi · Invio conferma · Esc annulla".into(),
        },
        Key::FieldRequired(field) => match lang {
            Lang::En => format!("{field} is required"),
            Lang::It => format!("{field} è obbligatorio"),
        },
        Key::VaultCreated(name) => match lang {
            Lang::En => format!("vault '{name}' created"),
            Lang::It => format!("vault '{name}' creato"),
        },
        Key::VaultExists => match lang {
            Lang::En => "a vault with this name already exists".into(),
            Lang::It => "esiste già un vault con questo nome".into(),
        },
        Key::CreateFailed => match lang {
            Lang::En => "could not create the vault".into(),
            Lang::It => "impossibile creare il vault".into(),
        },
        Key::UnlockTitle(name) => match lang {
            Lang::En => format!("unlock '{name}'"),
            Lang::It => format!("sblocca '{name}'"),
        },
        Key::UnlockHint => match lang {
            Lang::En => "the master password of this vault".into(),
            Lang::It => "la password principale di questo vault".into(),
        },
        Key::AlgorithmHint => match lang {
            Lang::En => "two-way encryption algorithm (←/→ to change)".into(),
            Lang::It => "algoritmo di cifratura bilaterale (←/→ per cambiare)".into(),
        },
        Key::KdfHint => match lang {
            Lang::En => "one-way key-derivation algorithm (←/→ to change)".into(),
            Lang::It => "algoritmo di derivazione unilaterale (←/→ per cambiare)".into(),
        },
        Key::VaultUnlocked(name) => match lang {
            Lang::En => format!("vault '{name}' unlocked"),
            Lang::It => format!("vault '{name}' sbloccato"),
        },
        Key::WrongPassword => match lang {
            Lang::En => "wrong password".into(),
            Lang::It => "password errata".into(),
        },
        Key::DeleteTitle => match lang {
            Lang::En => "delete a vault".into(),
            Lang::It => "elimina un vault".into(),
        },
        Key::NukeTitle => match lang {
            Lang::En => "empty a vault".into(),
            Lang::It => "svuota un vault".into(),
        },
        Key::VaultChoiceHint => match lang {
            Lang::En => "which vault (←/→ to change)".into(),
            Lang::It => "quale vault (←/→ per cambiare)".into(),
        },
        Key::DeleteWarning => match lang {
            Lang::En => "the vault and all its data are removed forever".into(),
            Lang::It => "il vault e tutti i suoi dati vengono rimossi per sempre".into(),
        },
        Key::NukeWarning => match lang {
            Lang::En => "every item in the vault is erased (the vault stays)".into(),
            Lang::It => "tutti gli elementi del vault vengono cancellati (il vault resta)".into(),
        },
        Key::VaultDeleted(name, items) => match lang {
            Lang::En => format!("vault '{name}' deleted ({items} items)"),
            Lang::It => format!("vault '{name}' eliminato ({items} elementi)"),
        },
        Key::VaultNuked(name, items) => match lang {
            Lang::En => format!("vault '{name}' emptied ({items} items removed)"),
            Lang::It => format!("vault '{name}' svuotato ({items} elementi rimossi)"),
        },
        Key::ExportTitle => match lang {
            Lang::En => "export a vault".into(),
            Lang::It => "esporta un vault".into(),
        },
        Key::ImportTitle => match lang {
            Lang::En => "import a vault".into(),
            Lang::It => "importa un vault".into(),
        },
        Key::ExportPathHint => match lang {
            Lang::En => "where to save the file (a folder appends <vault>.vault)".into(),
            Lang::It => "dove salvare il file (una cartella aggiunge <vault>.vault)".into(),
        },
        Key::ImportPathHint => match lang {
            Lang::En => "path of the .vault file to import".into(),
            Lang::It => "percorso del file .vault da importare".into(),
        },
        Key::ImportPasswordHint => match lang {
            Lang::En => "the master password of the vault being imported".into(),
            Lang::It => "la password principale del vault da importare".into(),
        },
        Key::VaultExported(name, path) => match lang {
            Lang::En => format!("vault '{name}' exported to {path}"),
            Lang::It => format!("vault '{name}' esportato in {path}"),
        },
        Key::VaultImported(name) => match lang {
            Lang::En => format!("vault '{name}' imported"),
            Lang::It => format!("vault '{name}' importato"),
        },
        Key::ExportFailed => match lang {
            Lang::En => "could not export the vault — check the destination path".into(),
            Lang::It => "impossibile esportare il vault — controlla il percorso di destinazione".into(),
        },
        Key::ExportDestinationExists => match lang {
            Lang::En => "a file already exists there — pick another name or path".into(),
            Lang::It => "esiste già un file lì — scegli un altro nome o percorso".into(),
        },
        Key::ImportFileNotFound => match lang {
            Lang::En => "no file at that path".into(),
            Lang::It => "nessun file in quel percorso".into(),
        },
        Key::NotAValidVault => match lang {
            Lang::En => "not a valid DataShield vault".into(),
            Lang::It => "non è un vault DataShield valido".into(),
        },
        Key::VaultExistsNamed(name) => match lang {
            Lang::En => format!("a vault named '{name}' already exists"),
            Lang::It => format!("esiste già un vault chiamato '{name}'"),
        },
        Key::VaultClosed => match lang {
            Lang::En => "vault closed".into(),
            Lang::It => "vault chiuso".into(),
        },
        Key::SettingsTitle => match lang {
            Lang::En => "settings".into(),
            Lang::It => "impostazioni".into(),
        },
        Key::SettingsTabGeneral => match lang {
            Lang::En => "General".into(),
            Lang::It => "Generale".into(),
        },
        Key::SettingsTabTui => match lang {
            Lang::En => "TUI".into(),
            Lang::It => "TUI".into(),
        },
        Key::SettingsTabAccessibility => match lang {
            Lang::En => "Accessibility".into(),
            Lang::It => "Accessibilità".into(),
        },
        Key::SettingLabelTheme => match lang {
            Lang::En => "theme".into(),
            Lang::It => "tema".into(),
        },
        Key::SettingLabelBackground => match lang {
            Lang::En => "background".into(),
            Lang::It => "sfondo".into(),
        },
        Key::SettingLabelLanguage => match lang {
            Lang::En => "language".into(),
            Lang::It => "lingua".into(),
        },
        Key::SettingLabelBlur => match lang {
            Lang::En => "blur".into(),
            Lang::It => "sfocatura".into(),
        },
        Key::SettingLabelBackdrop => match lang {
            Lang::En => "backdrop".into(),
            Lang::It => "sfondo animato".into(),
        },
        Key::SettingLabelBackdropFx => match lang {
            Lang::En => "effect".into(),
            Lang::It => "effetto".into(),
        },
        Key::SettingLabelBackdropLevel => match lang {
            Lang::En => "intensity".into(),
            Lang::It => "intensità".into(),
        },
        Key::SettingLabelAutoLock => match lang {
            Lang::En => "auto-lock".into(),
            Lang::It => "blocco automatico".into(),
        },
        Key::SettingLabelShowLogo => match lang {
            Lang::En => "logo".into(),
            Lang::It => "logo".into(),
        },
        Key::SettingLabelFavicons => match lang {
            Lang::En => "favicons".into(),
            Lang::It => "favicon".into(),
        },
        Key::SettingLabelVaultCount => match lang {
            Lang::En => "vaults".into(),
            Lang::It => "vault".into(),
        },
        Key::SettingLabelLivePreview => match lang {
            Lang::En => "live preview".into(),
            Lang::It => "anteprima live".into(),
        },
        Key::SettingLabelArrowPanel => match lang {
            Lang::En => "arrows reach ~".into(),
            Lang::It => "frecce verso ~".into(),
        },
        Key::SettingLabelOneshot => match lang {
            Lang::En => "one-shot commands".into(),
            Lang::It => "comandi one-shot".into(),
        },
        Key::SettingLabelAutocomplete => match lang {
            Lang::En => "autocomplete".into(),
            Lang::It => "autocompletamento".into(),
        },
        Key::SettingLabelReduceMotion => match lang {
            Lang::En => "reduce motion".into(),
            Lang::It => "riduci animazioni".into(),
        },
        Key::SettingLabelConfirmExit => match lang {
            Lang::En => "confirm on exit".into(),
            Lang::It => "conferma all'uscita".into(),
        },
        Key::BackgroundTransparent => match lang {
            Lang::En => "transparent".into(),
            Lang::It => "trasparente".into(),
        },
        Key::BackgroundOpaque => match lang {
            Lang::En => "opaque".into(),
            Lang::It => "opaco".into(),
        },
        Key::BlurOff => match lang {
            Lang::En => "off".into(),
            Lang::It => "off".into(),
        },
        Key::BlurKitty => match lang {
            Lang::En => "on · kitty".into(),
            Lang::It => "sì · kitty".into(),
        },
        Key::ToggleOn => match lang {
            Lang::En => "on".into(),
            Lang::It => "sì".into(),
        },
        Key::ToggleOff => match lang {
            Lang::En => "off".into(),
            Lang::It => "no".into(),
        },
        Key::SettingHintTheme => match lang {
            Lang::En => "the accent palette used across the interface".into(),
            Lang::It => "la palette di accento usata nell'interfaccia".into(),
        },
        Key::SettingHintBackground => match lang {
            Lang::En => {
                "opaque paints a solid backdrop; transparent lets your terminal show through".into()
            }
            Lang::It => {
                "opaco dipinge uno sfondo pieno; trasparente lascia intravedere il terminale".into()
            }
        },
        Key::SettingHintLanguage => match lang {
            Lang::En => "the interface language".into(),
            Lang::It => "la lingua dell'interfaccia".into(),
        },
        Key::SettingHintBlur => match lang {
            Lang::En => {
                "on kitty I make the window translucent (blur comes from your kitty.conf); elsewhere enable it in your terminal".into()
            }
            Lang::It => {
                "su kitty rendo la finestra traslucida (la sfocatura viene dal tuo kitty.conf); altrove abilitala dal terminale".into()
            }
        },
        Key::SettingHintBackdrop => match lang {
            Lang::En => "the faint digit-rain behind every screen (auto = only in DataShield's own terminal)".into(),
            Lang::It => "la pioggia di cifre dietro le schermate (auto = solo nel terminale dedicato)".into(),
        },
        Key::SettingHintBackdropFx => match lang {
            Lang::En => "which wallpaper drifts back there: the digit rain or the crimson pixel-waves".into(),
            Lang::It => "quale effetto scorre dietro: pioggia di cifre o onde pixel cremisi".into(),
        },
        Key::SettingHintBackdropLevel => match lang {
            Lang::En => "how strongly the wallpaper shows — slide left for subtler, right for bolder".into(),
            Lang::It => "quanto si vede lo sfondo — sinistra più tenue, destra più marcato".into(),
        },
        Key::SettingHintAutoLock => match lang {
            Lang::En => "lock the open vault after this long without a key press (the key is wiped)".into(),
            Lang::It => "blocca il vault dopo questa inattività (la chiave viene azzerata)".into(),
        },
        Key::SyncTitle => match lang {
            Lang::En => "sync — set up this vault's server".into(),
            Lang::It => "sync — configura il server di questo vault".into(),
        },
        Key::SyncHostHint => match lang {
            Lang::En => "your server's address (or a ~/.ssh/config alias)".into(),
            Lang::It => "l'indirizzo del tuo server (o un alias di ~/.ssh/config)".into(),
        },
        Key::SyncUserHint => match lang {
            Lang::En => "the SSH user on the server".into(),
            Lang::It => "l'utente SSH sul server".into(),
        },
        Key::SyncDirHint => match lang {
            Lang::En => "remote folder (DataShield creates it) — default ~/.datashield".into(),
            Lang::It => "cartella remota (la crea DataShield) — default ~/.datashield".into(),
        },
        Key::SyncKeyHint => match lang {
            Lang::En => "SSH key path (leave empty to use your ssh-agent)".into(),
            Lang::It => "percorso chiave SSH (vuoto = usa il tuo ssh-agent)".into(),
        },
        Key::SyncAutoHint => match lang {
            Lang::En => "sync automatically after each change and on unlock".into(),
            Lang::It => "sincronizza da solo dopo ogni modifica e allo sblocco".into(),
        },
        Key::SyncPushed => match lang {
            Lang::En => "synced — pushed to the server".into(),
            Lang::It => "sincronizzato — inviato al server".into(),
        },
        Key::SyncPulled => match lang {
            Lang::En => "synced — pulled newer changes".into(),
            Lang::It => "sincronizzato — scaricate modifiche più recenti".into(),
        },
        Key::SyncUpToDate => match lang {
            Lang::En => "already in sync".into(),
            Lang::It => "già sincronizzato".into(),
        },
        Key::SyncConfigured => match lang {
            Lang::En => "sync configured — run sync again to push".into(),
            Lang::It => "sync configurato — rilancia sync per inviare".into(),
        },
        Key::SyncBusy(name) => match lang {
            Lang::En => format!("{name} is syncing — wait for it to finish"),
            Lang::It => format!("{name} è in sincronizzazione — attendi che finisca"),
        },
        Key::PasswdTitle(name) => match lang {
            Lang::En => format!("passwd — change {name}'s master password"),
            Lang::It => format!("passwd — cambia la password principale di {name}"),
        },
        Key::PasswdCurrentHint => match lang {
            Lang::En => "your current passphrase (checked in the background)".into(),
            Lang::It => "la tua passphrase attuale (verificata in background)".into(),
        },
        Key::PasswdNewHint => match lang {
            Lang::En => "the new passphrase — everything is re-encrypted under it".into(),
            Lang::It => "la nuova passphrase — tutto viene ricifrato con questa".into(),
        },
        Key::PasswdConfirmHint => match lang {
            Lang::En => "type the new passphrase again".into(),
            Lang::It => "riscrivi la nuova passphrase".into(),
        },
        Key::PasswdMismatch => match lang {
            Lang::En => "the two new passphrases don't match".into(),
            Lang::It => "le due nuove passphrase non coincidono".into(),
        },
        Key::PasswdSameAsCurrent => match lang {
            Lang::En => "the new passphrase is the current one — nothing to change".into(),
            Lang::It => "la nuova passphrase è quella attuale — non c'è nulla da cambiare".into(),
        },
        Key::PasswdWorking(name) => match lang {
            Lang::En => format!("re-encrypting {name} — you can keep working elsewhere"),
            Lang::It => format!("ricifratura di {name} in corso — puoi continuare altrove"),
        },
        Key::PasswdInProgress => match lang {
            Lang::En => "re-encrypting".into(),
            Lang::It => "ricifratura".into(),
        },
        Key::PasswdChanged(name) => match lang {
            Lang::En => format!("{name} — password updated"),
            Lang::It => format!("{name} — password aggiornata"),
        },
        Key::PasswdWrongCurrent => match lang {
            Lang::En => "wrong current passphrase — the password was NOT changed".into(),
            Lang::It => "passphrase attuale errata — la password NON è stata cambiata".into(),
        },
        Key::PasswdFailed => match lang {
            Lang::En => "the password change failed".into(),
            Lang::It => "cambio password non riuscito".into(),
        },
        Key::PasswdBusy(name) => match lang {
            Lang::En => format!("{name} is being re-encrypted — wait for it to finish"),
            Lang::It => format!("{name} è in ricifratura — attendi che finisca"),
        },
        Key::UnknownVault(name) => match lang {
            Lang::En => format!("unknown vault: {name}"),
            Lang::It => format!("vault sconosciuto: {name}"),
        },
        Key::PasswdLost => match lang {
            Lang::En => "the password change was lost — nothing was changed; try again".into(),
            Lang::It => "il cambio password si è perso — non è cambiato niente; riprova".into(),
        },
        Key::PasswdSyncPending(name) => match lang {
            Lang::En => format!(
                "{name} — password updated; the copy on the server still opens with the old one until you unlock and sync"
            ),
            Lang::It => format!(
                "{name} — password aggiornata; la copia sul server si apre ancora con quella vecchia finché non sblocchi e sincronizzi"
            ),
        },
        Key::PasswdOtherVault(name) => match lang {
            Lang::En => format!("close this vault first to change {name}'s password"),
            Lang::It => format!("chiudi questo vault per cambiare la password di {name}"),
        },
        Key::SettingHintShowLogo => match lang {
            Lang::En => "show the ASCII logo header (edit its art in logo.txt)".into(),
            Lang::It => "mostra l'header con il logo ASCII (modifica l'arte in logo.txt)".into(),
        },
        Key::SettingHintFavicons => match lang {
            Lang::En => {
                "fetch each item's website logo from that site (a network request) — off shows a colored monogram only".into()
            }
            Lang::It => {
                "scarica il logo del sito di ogni voce da quel sito (richiesta di rete) — spento mostra solo un monogramma colorato".into()
            }
        },
        Key::SettingHintVaultCount => match lang {
            Lang::En => "how many vaults exist (read-only)".into(),
            Lang::It => "quanti vault esistono (sola lettura)".into(),
        },
        Key::SettingHintLivePreview => match lang {
            Lang::En => "show a command's view on the right while typing; off = press Enter to reveal it".into(),
            Lang::It => "mostra la vista del comando a destra mentre digiti; no = premi Invio per vederla".into(),
        },
        Key::SettingHintArrowPanel => match lang {
            Lang::En => "let arrow keys move to the ~ panel; off = the list keeps focus and you reach ~ by typing".into(),
            Lang::It => "le frecce possono spostarsi sul pannello ~; no = restano sulla lista e ~ si raggiunge digitando".into(),
        },
        Key::SettingHintOneshot => match lang {
            Lang::En => "run a full command with its arguments at once; off = always open the step-by-step form".into(),
            Lang::It => "esegui un comando completo con gli argomenti in un colpo; no = apri sempre la scheda passo-passo".into(),
        },
        Key::SettingHintAutocomplete => match lang {
            Lang::En => "Tab completion and its suggestions".into(),
            Lang::It => "completamento con Tab e i suoi suggerimenti".into(),
        },
        Key::SettingHintReduceMotion => match lang {
            Lang::En => "stop the blinking cursor and small animations".into(),
            Lang::It => "ferma il cursore lampeggiante e le piccole animazioni".into(),
        },
        Key::SettingHintConfirmExit => match lang {
            Lang::En => "ask before quitting from the top level (press Esc twice)".into(),
            Lang::It => "chiedi prima di uscire dal livello principale (premi Esc due volte)".into(),
        },
        Key::SplashHint => match lang {
            Lang::En => "type  help  or  commands  to see the commands".into(),
            Lang::It => "scrivi  help  o  commands  per vedere i comandi".into(),
        },
        Key::SettingsKeys => match lang {
            Lang::En => "←/→ section · ↓ enter · ↑/↓ move · ←/→ change · Enter save · Esc cancel".into(),
            Lang::It => "←/→ scheda · ↓ entra · ↑/↓ scorri · ←/→ cambia · Invio salva · Esc annulla".into(),
        },
        Key::ConfirmQuit => match lang {
            Lang::En => "press Esc again to quit".into(),
            Lang::It => "premi di nuovo Esc per uscire".into(),
        },
        Key::SettingsSaved => match lang {
            Lang::En => "settings saved".into(),
            Lang::It => "impostazioni salvate".into(),
        },
        Key::SettingsSaveFailed => match lang {
            Lang::En => "could not save the settings".into(),
            Lang::It => "impossibile salvare le impostazioni".into(),
        },
        Key::BlurApplied => match lang {
            Lang::En => "settings saved · kitty translucency on".into(),
            Lang::It => "impostazioni salvate · trasparenza kitty attiva".into(),
        },
        Key::BlurUnsupported => match lang {
            Lang::En => {
                "settings saved · translucency only on kitty — enable it in your terminal elsewhere".into()
            }
            Lang::It => {
                "impostazioni salvate · trasparenza solo su kitty — altrove abilitala dal terminale".into()
            }
        },
        Key::BlurFailed => match lang {
            Lang::En => "kitty translucency failed".into(),
            Lang::It => "trasparenza kitty non riuscita".into(),
        },
        Key::FormCancelled => match lang {
            Lang::En => "cancelled".into(),
            Lang::It => "annullato".into(),
        },
        Key::LanguageChanged(l) => match lang {
            Lang::En => format!("language set to {}", l.name()),
            Lang::It => format!("lingua impostata su {}", l.name()),
        },
        Key::UnknownLanguage(s) => match lang {
            Lang::En => format!("unknown language: {s}"),
            Lang::It => format!("lingua sconosciuta: {s}"),
        },
        Key::UnknownCommand(s) => match lang {
            Lang::En => format!("unknown command: {s}"),
            Lang::It => format!("comando sconosciuto: {s}"),
        },
        Key::UnknownAlgorithm(s) => match lang {
            Lang::En => format!("unknown algorithm: {s}"),
            Lang::It => format!("algoritmo sconosciuto: {s}"),
        },
        Key::UnknownKdf(s) => match lang {
            Lang::En => format!("unknown KDF: {s}"),
            Lang::It => format!("KDF sconosciuto: {s}"),
        },
        Key::TooManyArgs => match lang {
            Lang::En => "too many arguments — createVault <name> <password> [algorithm] [kdf]".into(),
            Lang::It => "troppi argomenti — createVault <nome> <password> [algoritmo] [kdf]".into(),
        },
        Key::PasswordLooksLikeCrypto(s) => match lang {
            Lang::En => format!(
                "'{s}' looks like a crypto name, but the 2nd argument is the password — createVault <name> <password> [algorithm] [kdf]"
            ),
            Lang::It => format!(
                "'{s}' sembra un nome di crittografia, ma il 2° argomento è la password — createVault <nome> <password> [algoritmo] [kdf]"
            ),
        },
        Key::AlgorithmBeforeKdf(s) => match lang {
            Lang::En => format!(
                "'{s}' is a KDF — the algorithm comes first: createVault <name> <password> [algorithm] [kdf]"
            ),
            Lang::It => format!(
                "'{s}' è un KDF — l'algoritmo viene prima: createVault <nome> <password> [algoritmo] [kdf]"
            ),
        },
        Key::ChooseCategory => match lang {
            Lang::En => "choose a category:".into(),
            Lang::It => "scegli una categoria:".into(),
        },
        Key::CategoryName(id) => category_name(lang, id),
        Key::FieldLabel(key) => field_label(lang, key),
        Key::AddItemTitle(cat) => match lang {
            Lang::En => format!("new {cat}"),
            Lang::It => format!("nuovo · {cat}"),
        },
        Key::ItemLabelHint => match lang {
            Lang::En => "a name for this item — how it shows in the list".into(),
            Lang::It => "un nome per questo elemento — come compare nella lista".into(),
        },
        Key::ItemAdded(label) => match lang {
            Lang::En => format!("added '{label}'"),
            Lang::It => format!("aggiunto '{label}'"),
        },
        Key::ComingSoon => match lang {
            Lang::En => "this command isn't wired up yet — coming soon".into(),
            Lang::It => "questo comando non è ancora attivo — in arrivo".into(),
        },
        Key::HintItemDetail => match lang {
            Lang::En => "Enter reveal · Ctrl+E edit · Ctrl+S ★ · Del delete · Esc back".into(),
            Lang::It => "Invio rivela · Ctrl+E modifica · Ctrl+S ★ · Canc elimina · Esc indietro".into(),
        },
        Key::ItemDetailEmpty => match lang {
            Lang::En => "this item has no fields".into(),
            Lang::It => "questo elemento non ha campi".into(),
        },
        Key::TabAll => match lang {
            Lang::En => "All".into(),
            Lang::It => "Tutte".into(),
        },
        // the same short word in both: it is what the thing is called on a
        // chip (the view's own title stays "memorandum")
        Key::TabMemo => match lang {
            Lang::En => "memo".into(),
            Lang::It => "memo".into(),
        },
        Key::SearchPlaceholder => match lang {
            Lang::En => "search…".into(),
            Lang::It => "cerca…".into(),
        },
        Key::SaveFailed => match lang {
            Lang::En => "could not save the vault — nothing was changed".into(),
            Lang::It => "impossibile salvare il vault — nulla è stato modificato".into(),
        },
        Key::ItemNotFound(s) => match lang {
            Lang::En => format!("no item named '{s}'"),
            Lang::It => format!("nessun elemento chiamato '{s}'"),
        },
        Key::ConfirmNo => match lang {
            Lang::En => "No".into(),
            Lang::It => "No".into(),
        },
        Key::ConfirmYes => match lang {
            Lang::En => "Yes".into(),
            Lang::It => "Sì".into(),
        },
        Key::ItemStarred(s) => match lang {
            Lang::En => format!("'{s}' starred ★"),
            Lang::It => format!("'{s}' aggiunto ai preferiti ★"),
        },
        Key::ItemUnstarred(s) => match lang {
            Lang::En => format!("'{s}' unstarred"),
            Lang::It => format!("'{s}' rimosso dai preferiti"),
        },
        Key::DeleteItemTitle => match lang {
            Lang::En => "delete an item".into(),
            Lang::It => "elimina un elemento".into(),
        },
        Key::ItemChoiceHint => match lang {
            Lang::En => "which item (←/→ to change)".into(),
            Lang::It => "quale elemento (←/→ per cambiare)".into(),
        },
        Key::ConfirmDeleteHint => match lang {
            Lang::En => "flip to Yes to confirm (←/→) — No cancels".into(),
            Lang::It => "metti Sì per confermare (←/→) — No annulla".into(),
        },
        Key::ItemDeleted(s) => match lang {
            Lang::En => format!("deleted '{s}'"),
            Lang::It => format!("eliminato '{s}'"),
        },
        Key::DeleteItemWarning => match lang {
            Lang::En => "this item is removed forever".into(),
            Lang::It => "questo elemento viene rimosso per sempre".into(),
        },
        Key::EditItemTitle(s) => match lang {
            Lang::En => format!("edit '{s}'"),
            Lang::It => format!("modifica '{s}'"),
        },
        Key::ItemEdited(s) => match lang {
            Lang::En => format!("saved '{s}'"),
            Lang::It => format!("salvato '{s}'"),
        },
        Key::EditSecretHint => match lang {
            Lang::En => "leave blank to keep the current value · type to replace it".into(),
            Lang::It => "lascia vuoto per mantenere il valore attuale · scrivi per sostituirlo".into(),
        },
        Key::CategoryTitle => match lang {
            Lang::En => "new category".into(),
            Lang::It => "nuova categoria".into(),
        },
        Key::CategoryNameHint => match lang {
            Lang::En => "the category name — its id becomes a slug (e.g. 'My DBs' → my_dbs)".into(),
            Lang::It => "il nome della categoria — l'id diventa uno slug (es. 'My DBs' → my_dbs)".into(),
        },
        Key::CategoryFieldNameLabel(n) => match lang {
            Lang::En => format!("field {n} name"),
            Lang::It => format!("campo {n} nome"),
        },
        Key::CategoryFieldTypeLabel(n) => match lang {
            Lang::En => format!("field {n} type"),
            Lang::It => format!("campo {n} tipo"),
        },
        Key::CategoryFieldSecretLabel(n) => match lang {
            Lang::En => format!("field {n} secret"),
            Lang::It => format!("campo {n} segreto"),
        },
        Key::CategoryFieldNameHint => match lang {
            Lang::En => "a field of this category — leave empty to skip".into(),
            Lang::It => "un campo di questa categoria — lascia vuoto per saltarlo".into(),
        },
        Key::CategoryFieldTypeHint => match lang {
            Lang::En => "the kind of value (←/→ to change)".into(),
            Lang::It => "il tipo di valore (←/→ per cambiare)".into(),
        },
        Key::CategoryFieldSecretHint => match lang {
            Lang::En => "masked and excluded from search? (←/→)".into(),
            Lang::It => "mascherato ed escluso dalla ricerca? (←/→)".into(),
        },
        Key::CategoryCreated(s) => match lang {
            Lang::En => format!("category '{s}' created"),
            Lang::It => format!("categoria '{s}' creata"),
        },
        Key::CategoryReserved(s) => match lang {
            Lang::En => format!("'{s}' is a reserved built-in id"),
            Lang::It => format!("'{s}' è un id riservato"),
        },
        Key::CategoryExists(s) => match lang {
            Lang::En => format!("a category '{s}' already exists"),
            Lang::It => format!("esiste già una categoria '{s}'"),
        },
        Key::CategoryNeedsField => match lang {
            Lang::En => "define at least one field".into(),
            Lang::It => "definisci almeno un campo".into(),
        },
        Key::CategoryDuplicateField(s) => match lang {
            Lang::En => format!("duplicate field '{s}'"),
            Lang::It => format!("campo duplicato '{s}'"),
        },
        Key::CategoryBadName => match lang {
            Lang::En => "the name must contain letters or digits".into(),
            Lang::It => "il nome deve contenere lettere o cifre".into(),
        },
        Key::EncInfoTitle => match lang {
            Lang::En => "encryption of this vault:".into(),
            Lang::It => "crittografia di questo vault:".into(),
        },
        Key::EncInfoKeyNeverStored => match lang {
            Lang::En => "derived from the master password — never stored".into(),
            Lang::It => "derivata dalla password principale — mai salvata".into(),
        },
        Key::EncInfoFreshNonce => match lang {
            Lang::En => "fresh on every save".into(),
            Lang::It => "nuovo a ogni salvataggio".into(),
        },
        Key::EncInfoHardened => match lang {
            Lang::En => "hardened against brute force".into(),
            Lang::It => "resistente al brute force".into(),
        },
        Key::EncInfoFast => match lang {
            Lang::En => "fast hash — cheap to brute-force".into(),
            Lang::It => "hash veloce — economico da forzare".into(),
        },
        Key::EncInfoBroken => match lang {
            Lang::En => "broken hash — study use only".into(),
            Lang::It => "hash compromesso — solo per studio".into(),
        },
        Key::EncInfoContents(n, m) => match lang {
            Lang::En => format!("{n} items · {m} custom categories"),
            Lang::It => format!("{n} elementi · {m} categorie personalizzate"),
        },
        Key::EncInfoUpgradePending => match lang {
            Lang::En => "format v3 — upgrades to v4 on the next save".into(),
            Lang::It => "formato v3 — passa al v4 al prossimo salvataggio".into(),
        },
        Key::EncInfoNeverModified => match lang {
            Lang::En => "never modified".into(),
            Lang::It => "mai modificato".into(),
        },
        Key::EncInfoItemFields(n, k) => match lang {
            Lang::En => format!("{n} fields ({k} secret)"),
            Lang::It => format!("{n} campi ({k} segreti)"),
        },
        Key::EncInfoItemNote => match lang {
            Lang::En => "all values live inside the vault's single encrypted blob".into(),
            Lang::It => "tutti i valori vivono nell'unico blob cifrato del vault".into(),
        },
        Key::RerollTitle => match lang {
            Lang::En => "re-encrypt the vault".into(),
            Lang::It => "ri-cifra il vault".into(),
        },
        Key::VaultRerolled(a, k) => match lang {
            Lang::En => format!("vault re-encrypted — fresh salt & nonce ({a} · {k})"),
            Lang::It => format!("vault ri-cifrato — salt e nonce nuovi ({a} · {k})"),
        },
        Key::NukeItemsTitle => match lang {
            Lang::En => "wipe every item".into(),
            Lang::It => "cancella tutti gli elementi".into(),
        },
        Key::NukeItemsNameHint => match lang {
            Lang::En => "type this vault's name exactly to confirm".into(),
            Lang::It => "scrivi esattamente il nome di questo vault per confermare".into(),
        },
        Key::NukeItemsConfirmHint => match lang {
            Lang::En => "←/→ — set to Yes to arm the wipe".into(),
            Lang::It => "←/→ — imposta Sì per armare la cancellazione".into(),
        },
        Key::NukeItemsNotConfirmed => match lang {
            Lang::En => "set confirm to Yes first".into(),
            Lang::It => "prima imposta la conferma su Sì".into(),
        },
        Key::NukeItemsNameMismatch => match lang {
            Lang::En => "that is not this vault's name".into(),
            Lang::It => "non è il nome di questo vault".into(),
        },
        Key::NukeItemsWarning => match lang {
            Lang::En => "every item is erased forever — custom categories stay".into(),
            Lang::It => "tutti gli elementi vengono cancellati per sempre — le categorie personalizzate restano".into(),
        },
        Key::ItemsNuked(n) => match lang {
            Lang::En => format!("{n} items wiped"),
            Lang::It => format!("{n} elementi cancellati"),
        },
        Key::Copied(secs) => match lang {
            Lang::En => format!("copied — clears in {secs}s"),
            Lang::It => format!("copiato — si cancella tra {secs}s"),
        },
        Key::CopyFailed => match lang {
            Lang::En => "no clipboard available — nothing was copied".into(),
            Lang::It => "appunti non disponibili — non è stato copiato niente".into(),
        },
        Key::NothingToCopy => match lang {
            Lang::En => "nothing to copy on this row".into(),
            Lang::It => "niente da copiare su questa riga".into(),
        },
        Key::InlineSecretHint => match lang {
            Lang::En => "unchanged — type to replace".into(),
            Lang::It => "invariato — scrivi per sostituire".into(),
        },
        Key::FieldUpdated(label) => match lang {
            Lang::En => format!("{label} updated"),
            Lang::It => format!("campo «{label}» aggiornato"),
        },
        Key::FieldUnchanged => match lang {
            Lang::En => "unchanged".into(),
            Lang::It => "invariato".into(),
        },
        Key::FieldRemoved(label) => match lang {
            Lang::En => format!("{label} removed"),
            Lang::It => format!("campo «{label}» rimosso"),
        },
        Key::FieldGone => match lang {
            Lang::En => "that row is gone — nothing was written".into(),
            Lang::It => "quella riga non c'è più — non è stato scritto niente".into(),
        },
        Key::NoPasswordField => match lang {
            Lang::En => "no password on this item".into(),
            Lang::It => "nessuna password in questo elemento".into(),
        },
        Key::NoUserField => match lang {
            Lang::En => "no username on this item".into(),
            Lang::It => "nessun utente in questo elemento".into(),
        },
        Key::NoWebsiteField => match lang {
            Lang::En => "no website on this item".into(),
            Lang::It => "nessun sito in questo elemento".into(),
        },
        Key::OpeningSite(url) => match lang {
            Lang::En => format!("opening {url}"),
            Lang::It => format!("apro {url}"),
        },
        Key::OpenSiteFailed => match lang {
            Lang::En => "no browser available — the site wasn't opened".into(),
            Lang::It => "nessun browser disponibile — il sito non è stato aperto".into(),
        },
        Key::ImageField(format, size) => match lang {
            Lang::En => format!("image · {format} · {size}"),
            Lang::It => format!("immagine · {format} · {size}"),
        },
        Key::ImageFieldHint => match lang {
            Lang::En => "path to an image file · PNG, JPEG or GIF, up to 8 MiB".into(),
            Lang::It => "percorso file immagine · PNG, JPEG o GIF, fino a 8 MiB".into(),
        },
        // three outcomes, so three clauses — and it has to stay short enough to
        // survive the inline edit's row, which shares its width with the value
        Key::ImageEditHint => match lang {
            Lang::En => "leave blank to keep it · a path replaces it · «-» removes it".into(),
            Lang::It => "vuoto la mantiene · un percorso la sostituisce · «-» la rimuove".into(),
        },
        Key::ImageNotCopyable => match lang {
            Lang::En => "image — not copyable".into(),
            Lang::It => "immagine — non copiabile".into(),
        },
        Key::ImageUnreadable(path) => match lang {
            Lang::En => format!("cannot read '{path}'"),
            Lang::It => format!("impossibile leggere «{path}»"),
        },
        Key::ImageTooLarge(max) => match lang {
            Lang::En => format!("image too large — up to {max}"),
            Lang::It => format!("immagine troppo grande — fino a {max}"),
        },
        Key::ImageUnsupported(formats, max) => match lang {
            Lang::En => format!("not a supported image — {formats}, up to {max}"),
            Lang::It => format!("formato immagine non supportato — {formats}, fino a {max}"),
        },
        Key::ImageOpened => match lang {
            Lang::En => "image opened — temporary copy, deleted on lock".into(),
            Lang::It => "immagine aperta — copia temporanea, eliminata al blocco".into(),
        },
        Key::ImageUndecodable => match lang {
            Lang::En => "unreadable image — there is nothing to open".into(),
            Lang::It => "immagine illeggibile — non c'è nulla da aprire".into(),
        },
        Key::ImageExportFailed => match lang {
            Lang::En => "the temporary copy could not be written".into(),
            Lang::It => "impossibile scrivere la copia temporanea".into(),
        },
        Key::ImageOpenFailed => match lang {
            Lang::En => "no viewer available — the image wasn't opened".into(),
            Lang::It => "nessun visualizzatore disponibile — l'immagine non è stata aperta".into(),
        },
        Key::MemoTitle => match lang {
            Lang::En => "memorandum".into(),
            Lang::It => "memorandum".into(),
        },
        Key::MemoEmpty => match lang {
            Lang::En => "nothing written yet — type and press ⏎".into(),
            Lang::It => "ancora niente — scrivi e premi ⏎".into(),
        },
        Key::MemoDay(iso) => long_date(lang, iso),
        Key::MemoComposerHint => match lang {
            Lang::En => "write a message…".into(),
            Lang::It => "scrivi un messaggio…".into(),
        },
        Key::MemoPhotoTitle => match lang {
            Lang::En => "attach photo".into(),
            Lang::It => "allega foto".into(),
        },
        Key::MemoPhotoField => match lang {
            Lang::En => "photo".into(),
            Lang::It => "foto".into(),
        },
        Key::MemoCaptionField => match lang {
            Lang::En => "caption".into(),
            Lang::It => "didascalia".into(),
        },
        Key::MemoCaptionHint => match lang {
            Lang::En => "a few words to go with the photo (optional)".into(),
            Lang::It => "due parole per accompagnare la foto (facoltativa)".into(),
        },
        Key::MemoCopyImage => match lang {
            Lang::En => "image".into(),
            Lang::It => "immagine".into(),
        },
        Key::MemoSearchHint => match lang {
            Lang::En => "search the memorandum…".into(),
            Lang::It => "cerca nel memorandum…".into(),
        },
        Key::SearchModeLiteral => match lang {
            Lang::En => "literal".into(),
            Lang::It => "letterale".into(),
        },
        // the acronym is the same word in both: it is what the thing is called
        Key::SearchModeRag => match lang {
            Lang::En => "RAG".into(),
            Lang::It => "RAG".into(),
        },
        Key::MemoSearchResults => match lang {
            Lang::En => "results".into(),
            Lang::It => "risultati".into(),
        },
        Key::MemoSearchNoResults => match lang {
            Lang::En => "no results".into(),
            Lang::It => "nessun risultato".into(),
        },
        Key::SearchWorking => match lang {
            Lang::En => "searching…".into(),
            Lang::It => "ricerca in corso…".into(),
        },
        Key::SearchRagOff => match lang {
            Lang::En => "semantic search is off — searched literally instead".into(),
            Lang::It => "ricerca semantica disattivata — ho cercato in modo letterale".into(),
        },
        Key::SearchRagAbsent => match lang {
            Lang::En => "the model isn't on this machine — searched literally instead".into(),
            Lang::It => "il modello non è su questo computer — ho cercato in modo letterale".into(),
        },
        Key::SearchRagDownloading => match lang {
            Lang::En => "the model is still downloading — searched literally instead".into(),
            Lang::It => "il modello si sta ancora scaricando — ho cercato in modo letterale".into(),
        },
        Key::RagStatusOff => match lang {
            Lang::En => "semantic search off — only literal matches".into(),
            Lang::It => "ricerca semantica disattivata — solo corrispondenze letterali".into(),
        },
        Key::RagStatusAbsent => match lang {
            Lang::En => "the model isn't on this machine yet".into(),
            Lang::It => "il modello non è ancora su questo computer".into(),
        },
        Key::RagStatusDownloading => match lang {
            Lang::En => "downloading the model — once only, then it works offline".into(),
            Lang::It => "scaricamento del modello — una volta sola, poi funziona offline".into(),
        },
        Key::RagStatusIndexing => match lang {
            Lang::En => "indexing the memorandum…".into(),
            Lang::It => "indicizzazione del memorandum…".into(),
        },
        Key::RagStatusReady => match lang {
            Lang::En => "semantic search ready — everything runs on this machine".into(),
            Lang::It => "ricerca semantica pronta — tutto avviene su questo computer".into(),
        },
        Key::RagStatusFailed(why) => match lang {
            Lang::En => format!("semantic search unavailable: {why}"),
            Lang::It => format!("ricerca semantica non disponibile: {why}"),
        },
        Key::RagFaultModelWorker => match lang {
            Lang::En => "the model worker stopped".into(),
            Lang::It => "il processo del modello si è interrotto".into(),
        },
        Key::RagFaultSearchWorker => match lang {
            Lang::En => "the search worker stopped".into(),
            Lang::It => "il processo di ricerca si è interrotto".into(),
        },
        Key::RagFaultVectorCount => match lang {
            Lang::En => "the model returned the wrong number of vectors".into(),
            Lang::It => "il modello ha restituito un numero di vettori sbagliato".into(),
        },
        Key::RagFaultNoVector => match lang {
            Lang::En => "the model returned no vector".into(),
            Lang::It => "il modello non ha restituito alcun vettore".into(),
        },
        Key::RagFaultModelLock => match lang {
            Lang::En => "the embedding model lock is poisoned".into(),
            Lang::It => "il blocco del modello di embedding è corrotto".into(),
        },
        Key::RagModelReady => match lang {
            Lang::En => "the model is ready — semantic search works offline from now on".into(),
            Lang::It => "il modello è pronto — da ora la ricerca semantica funziona offline".into(),
        },
        Key::SettingLabelRagMemo => match lang {
            Lang::En => "memo RAG".into(),
            Lang::It => "RAG memo".into(),
        },
        Key::SettingLabelRagItems => match lang {
            Lang::En => "items RAG".into(),
            Lang::It => "RAG voci".into(),
        },
        Key::SettingLabelApiMemo => match lang {
            Lang::En => "serve memo".into(),
            Lang::It => "esponi memo".into(),
        },
        Key::SettingLabelApiItems => match lang {
            Lang::En => "serve items".into(),
            Lang::It => "esponi voci".into(),
        },
        Key::SettingLabelApiPort => match lang {
            Lang::En => "API port".into(),
            Lang::It => "porta API".into(),
        },
        Key::SettingLabelApiToken => match lang {
            Lang::En => "API token".into(),
            Lang::It => "token API".into(),
        },
        Key::SettingHintRagMemo => match lang {
            Lang::En => {
                "index the memorandum for semantic search (downloads the model once, ~490 MB)".into()
            }
            Lang::It => {
                "indicizza il memorandum per la ricerca semantica (scarica il modello una volta, ~490 MB)"
                    .into()
            }
        },
        Key::SettingHintRagItems => match lang {
            Lang::En => {
                "index your items: labels, categories, field names and non-secret values — never a password"
                    .into()
            }
            Lang::It => {
                "indicizza le tue voci: etichette, categorie, nomi dei campi e valori non segreti — mai una password"
                    .into()
            }
        },
        Key::SettingHintApiMemo => match lang {
            Lang::En => {
                "serve the memorandum index on 127.0.0.1 for an agent — only while the vault is open"
                    .into()
            }
            Lang::It => {
                "esponi l'indice del memorandum su 127.0.0.1 per un agente — solo a vault aperto"
                    .into()
            }
        },
        Key::SettingHintApiItems => match lang {
            Lang::En => {
                "serve the item index on 127.0.0.1 — non-secret text only, secrets never leave".into()
            }
            Lang::It => {
                "esponi l'indice delle voci su 127.0.0.1 — solo testo non segreto, i segreti non escono mai"
                    .into()
            }
        },
        Key::SettingHintApiPort => match lang {
            Lang::En => {
                "loopback port for the agent API — for a different one, edit api_port in settings.toml"
                    .into()
            }
            Lang::It => {
                "porta di loopback per l'API dell'agente — per un'altra, modifica api_port in settings.toml"
                    .into()
            }
        },
        Key::SettingHintApiToken => match lang {
            Lang::En => "this session's token — copy the whole one with the `ragtoken` command".into(),
            Lang::It => {
                "il token di questa sessione — copialo per intero con il comando `ragtoken`".into()
            }
        },
        Key::SettingHintApiNeedsRag => match lang {
            Lang::En => "turn the matching RAG index on first — there is nothing to serve yet".into(),
            Lang::It => "prima attiva l'indice RAG corrispondente — non c'è ancora nulla da esporre".into(),
        },
        Key::SettingLabelApiUnlock => match lang {
            Lang::En => "API unlock".into(),
            Lang::It => "sblocco da API".into(),
        },
        Key::SettingLabelApiReveal => match lang {
            Lang::En => "API reveal".into(),
            Lang::It => "svela da API".into(),
        },
        Key::SettingLabelApiSessionTtl => match lang {
            Lang::En => "session expiry".into(),
            Lang::It => "scadenza sessione".into(),
        },
        // the whole cost of the toggle, on the row that turns it on: with this
        // on, anything on this machine holding the admin token can TRY vault
        // passwords against the API.
        Key::SettingHintApiUnlock => match lang {
            Lang::En => {
                "let an agent open a vault with its master password, no vault open here — local processes holding the API token can then try passwords against it"
                    .into()
            }
            Lang::It => {
                "permetti a un agente di aprire un vault con la sua password principale, senza vault aperto qui — i processi locali che hanno il token API possono poi provare password"
                    .into()
            }
        },
        Key::SettingHintApiReveal => match lang {
            Lang::En => {
                "allow /reveal: the secret VALUES of an item leave over 127.0.0.1 — the only endpoint that ever sends one"
                    .into()
            }
            Lang::It => {
                "abilita /reveal: i VALORI segreti di una voce escono su 127.0.0.1 — l'unico endpoint che ne invia uno"
                    .into()
            }
        },
        Key::SettingHintApiSessionTtl => match lang {
            Lang::En => {
                "an API session token unused for this long stops working — every request pushes the deadline back".into()
            }
            Lang::It => {
                "un token di sessione API inutilizzato per questo tempo smette di funzionare — ogni richiesta sposta avanti la scadenza"
                    .into()
            }
        },
        Key::SettingHintApiNeedsUnlock => match lang {
            Lang::En => "turn API unlock on first — without it there is no session to expire".into(),
            Lang::It => {
                "prima attiva lo sblocco da API — senza non esiste nessuna sessione da far scadere".into()
            }
        },
        // short on purpose: it shares the cheatsheet's 46-cell card with a key
        // column, and a gloss that is clipped explains nothing.
        Key::CheatApiSessions => match lang {
            Lang::En => "vaults an agent holds open".into(),
            Lang::It => "vault aperti da un agente".into(),
        },
        Key::SettingLabelLoginAssist => match lang {
            Lang::En => "login assist".into(),
            Lang::It => "login assistito".into(),
        },
        Key::SettingLabelBrowser => match lang {
            Lang::En => "browser".into(),
            Lang::It => "browser".into(),
        },
        Key::SettingLabelInstallExtension => match lang {
            Lang::En => "install extension".into(),
            Lang::It => "installa estensione".into(),
        },
        Key::SettingLabelLoginToken => match lang {
            Lang::En => "extension token".into(),
            Lang::It => "token estensione".into(),
        },
        // the whole cost of the toggle, on the row that turns it on
        Key::SettingHintLoginAssist => match lang {
            Lang::En => {
                "let the DataShield browser extension fetch an item's username and password to fill a login form — a second door a secret leaves through"
                    .into()
            }
            Lang::It => {
                "permetti all'estensione browser DataShield di prendere username e password di una voce per compilare un login — una seconda porta da cui esce un segreto"
                    .into()
            }
        },
        Key::SettingHintBrowser => match lang {
            Lang::En => "the browser your desktop opens links in, and the extension store it uses".into(),
            Lang::It => "il browser in cui il sistema apre i link, e lo store delle estensioni che usa".into(),
        },
        Key::SettingHintInstallExtension => match lang {
            Lang::En => "←/→ opens that browser on the extension's store page — install it once, then pair it with the token below".into(),
            Lang::It => "←/→ apre quel browser sulla pagina dello store dell'estensione — installala una volta, poi abbinala con il token qui sotto".into(),
        },
        Key::SettingHintLoginToken => match lang {
            Lang::En => "the pairing token the extension needs — ←/→ copies it (cleared after 20 s); `logintoken new` mints another".into(),
            Lang::It => "il token di abbinamento che serve all'estensione — ←/→ lo copia (rimosso dopo 20 s); `logintoken new` ne genera un altro".into(),
        },
        Key::SettingValueInstallExtension => match lang {
            Lang::En => "open the store page".into(),
            Lang::It => "apri la pagina dello store".into(),
        },
        Key::BrowserNotDetected => match lang {
            Lang::En => "not detected".into(),
            Lang::It => "non rilevato".into(),
        },
        Key::ExtensionPageOpened(family) => match lang {
            Lang::En => format!("opening the extension page for {family} in your browser"),
            Lang::It => format!("apro la pagina dell'estensione per {family} nel tuo browser"),
        },
        Key::ExtensionNotPublished => match lang {
            Lang::En => "the extension isn't on the stores yet — load it from the extension/ folder (see its README) or set the URL in ui.toml".into(),
            Lang::It => "l'estensione non è ancora sugli store — caricala dalla cartella extension/ (vedi il suo README) o imposta l'URL in ui.toml".into(),
        },
        Key::ExtensionUnsupportedBrowser(name) => match lang {
            Lang::En => format!("no extension for {name} — it works in Firefox- and Chrome-based browsers"),
            Lang::It => format!("nessuna estensione per {name} — funziona nei browser basati su Firefox e Chrome"),
        },
        Key::LoginTokenUnavailable => match lang {
            Lang::En => "no token: turn login assist on in the settings first".into(),
            Lang::It => "nessun token: prima attiva il login assistito nelle impostazioni".into(),
        },
        Key::LoginTokenSaveFirst => match lang {
            Lang::En => "turn login assist on and press ⏎ to save first — the token is minted on save".into(),
            Lang::It => "attiva il login assistito e premi ⏎ per salvare — il token nasce al salvataggio".into(),
        },
        Key::SettingValueCopy => match lang {
            Lang::En => "copy".into(),
            Lang::It => "copia".into(),
        },
        Key::BrowserWantsLogin(host) => match lang {
            Lang::En => format!("the browser is asking for a login at {host} — unlock a vault"),
            Lang::It => format!("il browser chiede un login per {host} — sblocca un vault"),
        },
        Key::BrowserLoginSaved(host) => match lang {
            Lang::En => format!("saved the login for {host} from the browser"),
            Lang::It => format!("salvato il login di {host} dal browser"),
        },
        Key::BrowserLoginUpdated(host) => match lang {
            Lang::En => format!("updated the password for {host} from the browser"),
            Lang::It => format!("aggiornata la password di {host} dal browser"),
        },
        Key::LoginAssistOff => match lang {
            Lang::En => "login assist is off — turn it on in the settings (BROWSER section)".into(),
            Lang::It => "il login assistito è spento — attivalo nelle impostazioni (sezione BROWSER)".into(),
        },
        Key::LoginOffered(host) => match lang {
            Lang::En => format!("opening {host} — the extension fills the login (valid 2 min, once)"),
            Lang::It => format!("apro {host} — l'estensione compila il login (valido 2 min, una volta)"),
        },
        Key::LoginNoServer => match lang {
            Lang::En => "the agent API isn't listening (port taken?) — the extension can't be told".into(),
            Lang::It => "l'API dell'agente non è in ascolto (porta occupata?) — l'estensione non può essere avvisata".into(),
        },
        Key::LoginTokenRegenerated => match lang {
            Lang::En => "new extension token minted — pair the extension again".into(),
            Lang::It => "nuovo token estensione generato — abbina di nuovo l'estensione".into(),
        },
        // said under a lock screen, so it says what the lock did NOT do
        Key::LockApiSessions => match lang {
            Lang::En => "still open to an agent — the lock did not close these".into(),
            Lang::It => "ancora aperti a un agente — il blocco non li ha chiusi".into(),
        },
        Key::SettingSectionShortcutsAccess => match lang {
            Lang::En => "SHORTCUTS · VAULT LIST".into(),
            Lang::It => "TASTI RAPIDI · ELENCO VAULT".into(),
        },
        Key::SettingSectionShortcutsVault => match lang {
            Lang::En => "SHORTCUTS · INSIDE A VAULT".into(),
            Lang::It => "TASTI RAPIDI · DENTRO UN VAULT".into(),
        },
        // said in two places — under the focused row, and out loud the moment a
        // verb is switched off — so it is phrased to work as both.
        Key::SettingHintShortcut => match lang {
            Lang::En => {
                "off takes this verb out of the bottom bar — the `:` palette always reaches it".into()
            }
            Lang::It => {
                "off toglie questo comando dalla barra in basso — la palette `:` lo raggiunge sempre"
                    .into()
            }
        },
        Key::SettingShortcutOff => match lang {
            Lang::En => "off".into(),
            Lang::It => "disattivato".into(),
        },
        Key::SettingShortcutDefault => match lang {
            Lang::En => "default".into(),
            Lang::It => "predefinito".into(),
        },
        Key::VaultEmptyHint(k) => match lang {
            Lang::En => format!("nothing here yet — {k} adds your first item"),
            Lang::It => format!("ancora niente — {k} aggiunge il primo elemento"),
        },
        Key::VaultEmptyHintPalette => match lang {
            Lang::En => "nothing here yet — `:` opens the commands that make your first item".into(),
            Lang::It => "ancora niente — con `:` apri i comandi che creano il primo elemento".into(),
        },
        Key::AccessEmptyHint(k) => match lang {
            Lang::En => format!("no vaults yet — {k} creates your first"),
            Lang::It => format!("nessun vault — {k} crea il primo"),
        },
        Key::AccessEmptyHintPalette => match lang {
            Lang::En => "no vaults yet — `:` opens the commands that make your first".into(),
            Lang::It => "nessun vault — con `:` apri i comandi che creano il primo".into(),
        },
        Key::ShortcutOverridesRefused(verbs) => match lang {
            Lang::En => format!("settings.toml: unusable shortcut for {verbs} — default key kept"),
            Lang::It => {
                format!("settings.toml: tasto rapido non utilizzabile per {verbs} — tenuto quello predefinito")
            }
        },
        Key::ApiStarted(port) => match lang {
            Lang::En => format!("agent API listening on 127.0.0.1:{port} — this machine only"),
            Lang::It => format!("API dell'agente in ascolto su 127.0.0.1:{port} — solo su questo computer"),
        },
        Key::ApiStopped => match lang {
            Lang::En => "agent API stopped".into(),
            Lang::It => "API dell'agente arrestata".into(),
        },
        Key::ApiPortBusy(port) => match lang {
            Lang::En => format!("port {port} is already taken — the agent API stays off"),
            Lang::It => format!("la porta {port} è già occupata — l'API dell'agente resta spenta"),
        },
        Key::ApiTokenUnavailable => match lang {
            Lang::En => "no token: the agent API isn't running".into(),
            Lang::It => "nessun token: l'API dell'agente non è in esecuzione".into(),
        },
        Key::RagModelDownloadStarted => match lang {
            Lang::En => "fetching the model — once only, then everything works offline".into(),
            Lang::It => "scaricamento del modello — una volta sola, poi tutto funziona offline".into(),
        },
        Key::RagModelDownloadLabel => match lang {
            Lang::En => "RAG model".into(),
            Lang::It => "modello RAG".into(),
        },
        // "note" is what the Italian keymap calls the Memorandum's own verb, so
        // the row that names its Tab stop uses that same word.
        Key::CheatTabCycle => match lang {
            Lang::En => "cycle tabs · memo".into(),
            Lang::It => "cambia scheda · note".into(),
        },
        Key::KeybarMove => match lang {
            Lang::En => "move".into(),
            Lang::It => "muovi".into(),
        },
        Key::KeybarUnlock => match lang {
            Lang::En => "unlock".into(),
            Lang::It => "sblocca".into(),
        },
        Key::KeybarDetail => match lang {
            Lang::En => "detail".into(),
            Lang::It => "dettaglio".into(),
        },
        Key::KeybarFilter => match lang {
            Lang::En => "filter".into(),
            Lang::It => "filtra".into(),
        },
        Key::KeybarCategory => match lang {
            Lang::En => "category".into(),
            Lang::It => "categoria".into(),
        },
        Key::KeybarCmd => match lang {
            Lang::En => "cmd".into(),
            Lang::It => "comandi".into(),
        },
        Key::KeybarKeys => match lang {
            Lang::En => "keys".into(),
            Lang::It => "tasti".into(),
        },
        Key::KeybarCopy => match lang {
            Lang::En => "copy".into(),
            Lang::It => "copia".into(),
        },
        Key::KeybarEditField => match lang {
            Lang::En => "edit".into(),
            Lang::It => "modifica".into(),
        },
        Key::KeybarEditForm => match lang {
            Lang::En => "form".into(),
            Lang::It => "scheda".into(),
        },
        Key::KeybarDelete => match lang {
            Lang::En => "delete".into(),
            Lang::It => "elimina".into(),
        },
        Key::KeybarStar => match lang {
            Lang::En => "star".into(),
            Lang::It => "preferito".into(),
        },
        Key::KeybarList => match lang {
            Lang::En => "list".into(),
            Lang::It => "lista".into(),
        },
        Key::KeybarConfirm => match lang {
            Lang::En => "confirm".into(),
            Lang::It => "conferma".into(),
        },
        Key::KeybarCancel => match lang {
            Lang::En => "cancel".into(),
            Lang::It => "annulla".into(),
        },
        Key::KeybarSend => match lang {
            Lang::En => "send".into(),
            Lang::It => "invia".into(),
        },
        Key::KeybarNewline => match lang {
            Lang::En => "newline".into(),
            Lang::It => "a capo".into(),
        },
        Key::KeybarPhoto => match lang {
            Lang::En => "photo".into(),
            Lang::It => "foto".into(),
        },
        Key::KeybarScroll => match lang {
            Lang::En => "scroll".into(),
            Lang::It => "scorri".into(),
        },
        Key::KeybarClose => match lang {
            Lang::En => "close".into(),
            Lang::It => "chiudi".into(),
        },
        Key::KeybarSearch => match lang {
            Lang::En => "search".into(),
            Lang::It => "cerca".into(),
        },
        Key::KeybarJump => match lang {
            Lang::En => "go to".into(),
            Lang::It => "vai a".into(),
        },
        Key::KeybarSearchMode => match lang {
            Lang::En => "mode".into(),
            Lang::It => "modo".into(),
        },
        Key::KeybarHits => match lang {
            Lang::En => "results".into(),
            Lang::It => "risultati".into(),
        },
        Key::KeybarClear => match lang {
            Lang::En => "clear".into(),
            Lang::It => "pulisci".into(),
        },
    }
}

/// A stored `YYYY-MM-DD` written the way a day separator says it out loud —
/// `19 August 2026` / `19 agosto 2026`. Anything that is not that exact shape
/// is handed back untouched: an unparseable stamp is still a day, and the
/// separator is not the place to lose a message over it.
fn long_date(lang: Lang, iso: &str) -> String {
    let parts: Vec<&str> = iso.split('-').collect();
    if parts.len() != 3 {
        return iso.to_string();
    }
    let (Ok(month), Ok(day)) = (parts[1].parse::<usize>(), parts[2].parse::<u32>()) else {
        return iso.to_string();
    };
    let Some(name) = month_name(lang, month) else {
        return iso.to_string();
    };
    format!("{day} {name} {}", parts[0])
}

/// The month's name in `lang`, 1-based. `None` for anything outside 1–12.
fn month_name(lang: Lang, month: usize) -> Option<&'static str> {
    const EN: [&str; 12] = [
        "January", "February", "March", "April", "May", "June", "July", "August", "September",
        "October", "November", "December",
    ];
    const IT: [&str; 12] = [
        "gennaio", "febbraio", "marzo", "aprile", "maggio", "giugno", "luglio", "agosto",
        "settembre", "ottobre", "novembre", "dicembre",
    ];
    let table = match lang {
        Lang::En => &EN,
        Lang::It => &IT,
    };
    table.get(month.checked_sub(1)?).copied()
}

/// Localized display name for a built-in category id (falls back to the id with
/// `_`→spaces for custom or unknown ids).
fn category_name(lang: Lang, id: &str) -> String {
    let pair = match id {
        "email" => ("Email", "Email"),
        "account" => ("Account", "Account"),
        "login" => ("Password", "Password"),
        "credit_card" => ("Credit card", "Carta di credito"),
        "api_key" => ("API key", "Chiave API"),
        "note" => ("Secure note", "Nota sicura"),
        "ssh_key" => ("SSH key", "Chiave SSH"),
        "wifi" => ("Wi-Fi", "Wi-Fi"),
        "identity" => ("Identity", "Identità"),
        other => return other.replace('_', " "),
    };
    match lang {
        Lang::En => pair.0.to_string(),
        Lang::It => pair.1.to_string(),
    }
}

/// Localized label for a built-in field key (falls back to the key with
/// `_`→spaces for custom keys).
fn field_label(lang: Lang, key: &str) -> String {
    let pair = match key {
        "label" => ("name", "nome"),
        "email" => ("email", "email"),
        "password" => ("password", "password"),
        "username" => ("username", "nome utente"),
        "url" => ("website", "sito"),
        "totp" => ("2FA code (TOTP)", "codice 2FA (TOTP)"),
        "recovery_email" => ("recovery email", "email di recupero"),
        "notes" => ("notes", "note"),
        "body" => ("content", "contenuto"),
        "cardholder" => ("cardholder", "intestatario"),
        "brand" => ("brand", "circuito"),
        "number" => ("card number", "numero carta"),
        "expiry" => ("expiry", "scadenza"),
        "cvv" => ("CVV", "CVV"),
        "pin" => ("PIN", "PIN"),
        "issuing_bank" => ("issuing bank", "banca emittente"),
        "service" => ("service", "servizio"),
        "key_id" => ("key id", "id chiave"),
        "api_key" => ("API key", "chiave API"),
        "api_secret" => ("API secret", "secret API"),
        "environment" => ("environment", "ambiente"),
        "expires" => ("expires", "scade"),
        "host" => ("host", "host"),
        "port" => ("port", "porta"),
        "private_key" => ("private key", "chiave privata"),
        "passphrase" => ("passphrase", "passphrase"),
        "public_key" => ("public key", "chiave pubblica"),
        "ssid" => ("network (SSID)", "rete (SSID)"),
        "security" => ("security", "sicurezza"),
        "full_name" => ("full name", "nome completo"),
        "birth_date" => ("birth date", "data di nascita"),
        "fiscal_code" => ("fiscal code", "codice fiscale"),
        "document_type" => ("document type", "tipo documento"),
        "document_number" => ("document number", "numero documento"),
        "issued" => ("issued", "rilasciato"),
        "photo_front" => ("front of document", "fronte documento"),
        "photo_back" => ("back of document", "retro documento"),
        other => return other.replace('_', " "),
    };
    match lang {
        Lang::En => pair.0.to_string(),
        Lang::It => pair.1.to_string(),
    }
}
