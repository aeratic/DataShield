//! Command registry loaded from `commands.toml`.
//!
//! `commands.toml` is the single source of truth: the palette, command
//! matching, dispatch and descriptions are all derived from it. A default is
//! embedded in the binary; on first run it is copied to the data directory so
//! it can be customised, and if the on-disk file is missing or malformed the
//! embedded default is used.

use crate::app::Page;
use crate::config::read_config_version;
use crate::i18n::Lang;
use serde::Deserialize;
use std::fs;
use std::path::Path;

/// The built-in registry, also written to disk on first run.
const DEFAULT_TOML: &str = include_str!("../commands.toml");

/// The behaviour a command triggers. This set is fixed (each maps to code);
/// the config file may only reference these values.
#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Help,
    Language,
    CreateVault,
    DeleteVault,
    NukeVault,
    ExportVault,
    ImportVault,
    /// Back/forward navigation through the `~` panel history.
    Panel,
    /// Open the settings page (theme, background, language, blur).
    Settings,
    /// Close the open vault and return to the access page.
    ExitVault,
    // ---- in-vault item management ----
    /// Add an item: pick a category, then fill its fields.
    AddItem,
    /// Delete a saved item.
    DeleteItem,
    /// Edit a saved item.
    EditItem,
    /// Search items by name or field.
    SearchItems,
    /// Create a custom category.
    ManageCategory,
    /// Show an item's (non-secret) encryption metadata.
    EncInfo,
    /// Re-derive the key and re-encrypt the whole vault.
    RerollEnc,
    /// Wipe all items in the vault (after verification).
    NukeItems,
    /// Toggle the ★ favorite flag on an item.
    StarItem,
    /// Sync this vault to its configured VPS (SFTP-over-SSH, E2E).
    Sync,
    /// Change a vault's master password (re-encrypted on a worker thread).
    ChangePassword,
    /// Open the Memorandum: the chat with yourself kept inside the vault.
    Memo,
    /// Copy the agent API's session token (through the timed-wipe clipboard).
    RagToken,
    /// Copy (or mint) the browser extension's pairing token, the same way.
    LoginToken,
}

/// How a command parameter is supplied.
#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum ParamKind {
    /// Free text — becomes an input field in the command's form.
    Input,
    /// A pick from a closed set (handled by a chooser, not a form field).
    Choice,
}

/// One command parameter.
pub struct ParamDef {
    pub name: String,
    pub kind: ParamKind,
    /// Rendered masked (dots) in the form — for passwords.
    pub secret: bool,
    desc_en: String,
    desc_it: String,
}

impl ParamDef {
    pub fn description(&self, lang: Lang) -> &str {
        match lang {
            Lang::En => &self.desc_en,
            Lang::It => &self.desc_it,
        }
    }
}

/// One resolved command.
pub struct CommandDef {
    pub name: String,
    pub aliases: Vec<String>,
    pub pages: Vec<Page>,
    pub action: Action,
    pub params: Vec<ParamDef>,
    desc_en: String,
    desc_it: String,
}

impl CommandDef {
    pub fn description(&self, lang: Lang) -> &str {
        match lang {
            Lang::En => &self.desc_en,
            Lang::It => &self.desc_it,
        }
    }

    /// The palette signature: `name [p1] [p2]`.
    pub fn signature(&self) -> String {
        let mut out = self.name.clone();
        for p in &self.params {
            out.push_str(&format!(" [{}]", p.name));
        }
        out
    }

    /// True if the command has at least one free-text parameter, i.e. it needs
    /// an input form when invoked without arguments.
    pub fn has_input_form(&self) -> bool {
        self.params.iter().any(|p| p.kind == ParamKind::Input)
    }

    fn matches(&self, token: &str) -> bool {
        self.name.eq_ignore_ascii_case(token)
            || self.aliases.iter().any(|a| a.eq_ignore_ascii_case(token))
    }
}

pub struct Registry {
    version: u32,
    commands: Vec<CommandDef>,
}

impl Registry {
    /// Loads the registry from `path`, seeding it with the embedded default on
    /// first run. Behaviour:
    /// - missing/unreadable file → seed the default, no warning;
    /// - on-disk `version` older than the built-in one (read through a
    ///   schema-TOLERANT probe, BEFORE the full parse — an old file may use an
    ///   older command schema the full parse rejects, and it must still be
    ///   recognised as stale) → the shipped commands changed: back the old
    ///   file up to `commands.toml.bak`, regenerate, and warn (this is what
    ///   pulls in newly-added commands on upgrade);
    /// - malformed or zero-command file at the current version → use the
    ///   embedded default and warn, leaving the file untouched (the user's
    ///   broken edits are theirs to fix, never silently reverted);
    /// - otherwise use the file (warning on duplicate names/aliases).
    pub fn load(path: &Path) -> (Registry, Option<String>) {
        let embedded = Registry::embedded_default();
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(_) => {
                Registry::seed(path);
                return (embedded, None);
            }
        };
        // The version gate runs FIRST, through the schema-tolerant probe: a
        // stale file may not survive the strict full parse at all (e.g. a
        // renamed action), yet it must regenerate, not warn "invalid" forever.
        // An unreadable version (not valid TOML) cannot prove staleness, so it
        // falls through to the hand-broken handling below — warn, don't
        // clobber.
        if read_config_version(&text).is_some_and(|v| v < embedded.version) {
            let _ = fs::rename(path, path.with_extension("toml.bak"));
            Registry::seed(path);
            return (
                embedded,
                Some(
                    "commands.toml updated to newer built-in commands (old file saved as commands.toml.bak)"
                        .to_string(),
                ),
            );
        }
        match Registry::parse(&text) {
            Err(_) => (
                embedded,
                Some("commands.toml is invalid — using built-in defaults".to_string()),
            ),
            Ok(reg) if reg.commands.is_empty() => (
                embedded,
                Some("commands.toml defines no commands — using built-in defaults".to_string()),
            ),
            Ok(reg) => {
                let warning = reg.duplicate_warning();
                (reg, warning)
            }
        }
    }

    fn seed(path: &Path) {
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let _ = fs::write(path, DEFAULT_TOML);
    }

    fn embedded_default() -> Registry {
        Registry::parse(DEFAULT_TOML).expect("embedded commands.toml is valid")
    }

    /// A warning if any token (name or alias) is claimed by more than one
    /// command on the same page — the first in file order wins, which can make
    /// typing a command disagree with picking it from the palette.
    fn duplicate_warning(&self) -> Option<String> {
        let mut seen: Vec<(Page, String)> = Vec::new();
        for command in &self.commands {
            for token in std::iter::once(&command.name).chain(command.aliases.iter()) {
                let lower = token.to_ascii_lowercase();
                for &page in &command.pages {
                    if seen.iter().any(|(p, t)| *p == page && *t == lower) {
                        return Some(format!(
                            "commands.toml: duplicate command '{token}' — the first one wins"
                        ));
                    }
                    seen.push((page, lower.clone()));
                }
            }
        }
        None
    }

    fn parse(text: &str) -> Result<Registry, toml::de::Error> {
        let file: RawFile = toml::from_str(text)?;
        let commands = file
            .command
            .into_iter()
            .map(|c| CommandDef {
                name: c.name,
                aliases: c.aliases,
                pages: c.pages.iter().filter_map(|p| Page::from_key(p)).collect(),
                action: c.action,
                params: c
                    .params
                    .into_iter()
                    .map(|p| ParamDef {
                        name: p.name,
                        kind: p.kind,
                        secret: p.secret,
                        desc_en: p.description.en,
                        desc_it: p.description.it,
                    })
                    .collect(),
                desc_en: c.description.en,
                desc_it: c.description.it,
            })
            .collect();
        Ok(Registry {
            version: file.version,
            commands,
        })
    }

    /// Commands usable on `page`, in file order.
    pub fn for_page(&self, page: Page) -> Vec<&CommandDef> {
        self.commands
            .iter()
            .filter(|c| c.pages.contains(&page))
            .collect()
    }

    /// The command `token` refers to on `page` (by name or alias,
    /// case-insensitively), if any.
    pub fn resolve(&self, page: Page, token: &str) -> Option<&CommandDef> {
        self.commands
            .iter()
            .find(|c| c.pages.contains(&page) && c.matches(token))
    }

    /// The command on `page` that triggers `action` (used e.g. to find the
    /// current name of the language command even if the user renamed it).
    pub fn command_for_action(&self, page: Page, action: Action) -> Option<&CommandDef> {
        self.commands
            .iter()
            .find(|c| c.pages.contains(&page) && c.action == action)
    }
}

#[derive(Deserialize)]
struct RawFile {
    /// Built-in command-set version; a missing field (old files) means 0.
    /// Bump `version` in the shipped commands.toml on ANY breaking change to
    /// these `Raw*` structs (or to the `Action` set): an unbumped schema
    /// change strands old files in the "invalid" warn-path (which, correctly,
    /// never clobbers them) instead of regenerating them.
    #[serde(default)]
    version: u32,
    #[serde(default)]
    command: Vec<RawCommand>,
}

#[derive(Deserialize)]
struct RawCommand {
    name: String,
    #[serde(default)]
    aliases: Vec<String>,
    pages: Vec<String>,
    action: Action,
    #[serde(default)]
    params: Vec<RawParam>,
    description: RawDescription,
}

#[derive(Deserialize)]
struct RawParam {
    name: String,
    kind: ParamKind,
    #[serde(default)]
    secret: bool,
    description: RawDescription,
}

#[derive(Deserialize)]
struct RawDescription {
    en: String,
    it: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_default_parses() {
        let reg = Registry::parse(DEFAULT_TOML).unwrap();
        // help + language on both pages; create/delete/nuke on access only.
        assert!(reg.resolve(Page::Access, "help").is_some());
        assert_eq!(reg.resolve(Page::Access, "help").unwrap().action, Action::Help);
        assert!(reg.resolve(Page::Access, "createVault").is_some());
        assert!(reg.for_page(Page::Access).len() >= 5);
    }

    #[test]
    fn params_are_parsed_and_signature_includes_them() {
        let reg = Registry::parse(DEFAULT_TOML).unwrap();
        let cv = reg.resolve(Page::Access, "createVault").unwrap();
        assert_eq!(
            cv.signature(),
            "createVault [vault_name] [vault_password] [algorithm] [kdf]"
        );
        assert!(cv.has_input_form());
        // name + password (input) then the optional algorithm + kdf (choice)
        assert_eq!(cv.params.len(), 4);
        assert_eq!(cv.params[0].kind, ParamKind::Input);
        assert_eq!(cv.params[2].kind, ParamKind::Choice);
        assert_eq!(cv.params[3].kind, ParamKind::Choice);

        // language has a single choice param, so no input form
        let lang = reg.resolve(Page::Access, "language").unwrap();
        assert_eq!(lang.signature(), "language [lang]");
        assert!(!lang.has_input_form());
        assert_eq!(lang.params[0].kind, ParamKind::Choice);

        // help has no params
        assert_eq!(reg.resolve(Page::Access, "help").unwrap().signature(), "help");
    }

    #[test]
    fn ragtoken_is_registered_on_both_pages_and_takes_no_argument() {
        // the token is a credential: it is COPIED, never typed as an argument
        // and never shown — so the command has nothing to fill in.
        let reg = Registry::parse(DEFAULT_TOML).unwrap();
        for page in [Page::Access, Page::Vault] {
            let cmd = reg.resolve(page, "ragtoken").expect("ragtoken is registered");
            assert_eq!(cmd.action, Action::RagToken);
            assert!(cmd.params.is_empty());
            assert!(!cmd.has_input_form());
        }
        assert_eq!(reg.resolve(Page::Vault, "TOKEN").map(|c| c.action), Some(Action::RagToken));
    }

    #[test]
    fn matching_is_case_insensitive_and_honours_aliases() {
        let reg = Registry::parse(DEFAULT_TOML).unwrap();
        assert!(reg.resolve(Page::Access, "LANGUAGE").is_some());
        // "commands" is an alias of help
        assert_eq!(
            reg.resolve(Page::Access, "commands").unwrap().action,
            Action::Help
        );
    }

    #[test]
    fn page_scoping_filters_commands() {
        let reg = Registry::parse(DEFAULT_TOML).unwrap();
        // createVault is access-only, so it must not resolve on the vault page
        assert!(reg.resolve(Page::Vault, "createVault").is_none());
        // help works on both
        assert!(reg.resolve(Page::Vault, "help").is_some());
    }

    #[test]
    fn unknown_action_falls_back_to_default() {
        let bad = r#"
            [[command]]
            name = "x"
            pages = ["access"]
            action = "not_a_real_action"
            description.en = "x"
            description.it = "x"
        "#;
        assert!(Registry::parse(bad).is_err());
        // load() would substitute the embedded default here.
    }

    #[test]
    fn missing_config_is_seeded_without_warning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/commands.toml");
        let (reg, warn) = Registry::load(&path);
        assert!(path.exists(), "file should be seeded");
        assert!(warn.is_none());
        assert!(reg.resolve(Page::Access, "help").is_some());
    }

    #[test]
    fn empty_config_falls_back_with_warning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("commands.toml");
        // a CURRENT-version file with no commands: warn, don't regenerate (a
        // version-less empty file is stale and regenerates instead — below).
        let version = Registry::embedded_default().version;
        std::fs::write(&path, format!("version = {version}\n")).unwrap();
        let (reg, warn) = Registry::load(&path);
        assert!(!reg.for_page(Page::Access).is_empty(), "must not be unusable");
        assert!(warn.unwrap().contains("no commands"));
        assert!(!path.with_extension("toml.bak").exists(), "not regenerated");
    }

    #[test]
    fn a_versionless_empty_config_is_treated_as_stale_and_regenerated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("commands.toml");
        // whitespace-only: valid TOML, no version key => version 0 => stale.
        std::fs::write(&path, "   \n\n").unwrap();
        let (reg, warn) = Registry::load(&path);
        assert!(reg.resolve(Page::Access, "help").is_some());
        assert!(warn.unwrap().contains("updated"));
        assert!(path.with_extension("toml.bak").exists(), "old file backed up");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), DEFAULT_TOML);
    }

    #[test]
    fn a_stale_old_schema_config_is_regenerated_not_reported_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("commands.toml");
        // an old-version file whose command schema the CURRENT strict parse
        // rejects (unknown action variant — e.g. one renamed since). The
        // version gate must still see it is stale and regenerate it.
        let stale = Registry::embedded_default().version - 1;
        std::fs::write(
            &path,
            format!(
                r#"
                    version = {stale}

                    [[command]]
                    name = "x"
                    pages = ["access"]
                    action = "action_renamed_since"
                    description.en = "x"
                    description.it = "x"
                "#
            ),
        )
        .unwrap();
        let (reg, warn) = Registry::load(&path);
        let warn = warn.unwrap();
        assert!(warn.contains("updated"), "must regenerate, got: {warn}");
        assert!(!warn.contains("invalid"));
        assert!(path.with_extension("toml.bak").exists(), "old file backed up");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), DEFAULT_TOML);
        assert!(reg.resolve(Page::Access, "help").is_some());
    }

    #[test]
    fn a_broken_current_version_config_warns_but_is_not_clobbered() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("commands.toml");
        // valid TOML at the CURRENT version, but a hand-edit broke a command
        // (unknown action) — respect the user's file: warn, don't rewrite it.
        let version = Registry::embedded_default().version;
        let body = format!(
            "version = {version}\n\
             [[command]]\n\
             name = \"x\"\n\
             pages = [\"access\"]\n\
             action = \"not_a_real_action\"\n\
             description.en = \"x\"\n\
             description.it = \"x\"\n"
        );
        std::fs::write(&path, &body).unwrap();
        let (reg, warn) = Registry::load(&path);
        assert!(reg.resolve(Page::Access, "help").is_some(), "falls back");
        assert!(warn.unwrap().contains("invalid"));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            body,
            "the user's file must be left untouched"
        );
        assert!(!path.with_extension("toml.bak").exists());
    }

    #[test]
    fn stale_config_is_regenerated_with_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("commands.toml");
        // an old file: has commands but no version field (=> version 0)
        std::fs::write(
            &path,
            r#"
                [[command]]
                name = "help"
                pages = ["access"]
                action = "help"
                description.en = "h"
                description.it = "h"
            "#,
        )
        .unwrap();
        let (reg, warn) = Registry::load(&path);
        // regenerated to the embedded set, which includes the newer commands
        assert!(reg.resolve(Page::Access, "commands").is_some());
        assert!(reg.resolve(Page::Access, "~").is_some());
        assert!(warn.unwrap().contains("updated"));
        assert!(
            path.with_extension("toml.bak").exists(),
            "old file should be backed up"
        );
    }

    #[test]
    fn current_version_config_is_used_as_is() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("commands.toml");
        // seed the current default, then load: no warning, no regeneration
        Registry::load(&path);
        let (_reg, warn) = Registry::load(&path);
        assert!(warn.is_none());
        assert!(!path.with_extension("toml.bak").exists());
    }

    #[test]
    fn malformed_config_falls_back_with_warning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("commands.toml");
        // not valid TOML: no version is readable, so staleness cannot be
        // proven — warn and leave the file alone (never clobber).
        std::fs::write(&path, "this is ][ not toml").unwrap();
        let (reg, warn) = Registry::load(&path);
        assert!(reg.resolve(Page::Access, "help").is_some());
        assert!(warn.unwrap().contains("invalid"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "this is ][ not toml");
        assert!(!path.with_extension("toml.bak").exists());
    }

    #[test]
    fn duplicate_alias_is_warned() {
        let toml = r#"
            [[command]]
            name = "help"
            pages = ["access"]
            action = "help"
            description.en = "h"
            description.it = "h"

            [[command]]
            name = "language"
            aliases = ["help"]
            pages = ["access"]
            action = "language"
            description.en = "l"
            description.it = "l"
        "#;
        let reg = Registry::parse(toml).unwrap();
        assert!(reg.duplicate_warning().unwrap().contains("help"));
    }

    #[test]
    fn command_for_action_finds_renamed_language() {
        let toml = r#"
            [[command]]
            name = "lingua"
            pages = ["access"]
            action = "language"
            description.en = "x"
            description.it = "x"
        "#;
        let reg = Registry::parse(toml).unwrap();
        let cmd = reg.command_for_action(Page::Access, Action::Language).unwrap();
        assert_eq!(cmd.name, "lingua");
    }

    #[test]
    fn unknown_page_keys_are_skipped() {
        let toml = r#"
            [[command]]
            name = "x"
            pages = ["access", "moon"]
            action = "help"
            description.en = "x"
            description.it = "x"
        "#;
        let reg = Registry::parse(toml).unwrap();
        let cmd = reg.resolve(Page::Access, "x").unwrap();
        assert_eq!(cmd.pages, vec![Page::Access]); // "moon" dropped
    }
}
