//! Interface "chrome" loaded from the editable, app-READ `ui.toml`.
//!
//! This is the counterpart to `settings.toml`: settings are preferences the app
//! *writes* (so they must round-trip and can't hold comments), whereas the
//! chrome here is hand-edited content the app only *reads* — the input prompt,
//! the panel split, and the brand colour/tagline. Loading is best-effort: every
//! field has a default (a missing key uses it) and a malformed file falls back
//! entirely to the embedded default, so a bad edit can never stop the app.

use crate::config::read_config_version;
use crate::i18n::Lang;
use crate::theme::Palette;
use ratatui::style::Color;
use serde::Deserialize;
use std::fs;
use std::path::Path;

/// The built-in chrome, also written to disk on first run.
const DEFAULT_UI_TOML: &str = include_str!("../ui.toml");

// ---- raw serde mirror of ui.toml (every field optional) ------------------

/// Built-in chrome version; an older on-disk `ui.toml` is regenerated (so new
/// defaults reach existing installs). Bump this whenever the shipped ui.toml
/// changes in a way users should pick up — and ALWAYS on any breaking change
/// to the `Raw*` structs below: an unbumped schema change strands old files
/// in the "invalid" warn-path (which, correctly, never clobbers them) instead
/// of regenerating them.
const EMBEDDED_UI_VERSION: u32 = 4;

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawUi {
    /// A missing field (old files) means 0, so it regenerates.
    version: u32,
    input: RawInput,
    panel: RawPanel,
    brand: RawBrand,
    header: RawHeader,
    extension: RawExtension,
}

/// Where the "install extension" settings row sends each browser family. In
/// the app-READ file on purpose: the store pages exist only once the extension
/// is published, and filling them in must not need a rebuild.
#[derive(Deserialize, Default)]
#[serde(default)]
struct RawExtension {
    /// The Chrome Web Store listing (Chrome, Brave, Edge, Vivaldi, Opera…).
    chrome_url: String,
    /// The addons.mozilla.org listing (Firefox, Zen, LibreWolf…).
    firefox_url: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawHeader {
    /// Language-agnostic quotes; empty (or missing) falls back to the built-ins.
    phrases: Vec<String>,
}

/// The built-in header phrases: a mix of famous lines in several languages,
/// shown one at a time (typed and erased) in a random order beside the logo.
/// They are NOT localized — each is displayed as-is whatever the UI language.
const DEFAULT_PHRASES: &[&str] = &[
    "Imagination is more important than knowledge.",
    "E pur si muove.",
    "Je pense, donc je suis.",
    "The universe is under no obligation to make sense to you.",
    "La natura non fa salti.",
    "If I have seen further, it is by standing on the shoulders of giants.",
    "An unexamined life is not worth living.",
    "La ciencia es la mejor herramienta que tenemos.",
    "科学は疑問から始まる。",
    "The important thing is not to stop questioning.",
    "Conosci te stesso.",
    "El que no arriesga, no gana.",
    "L'enfer, c'est les autres.",
    "一期一会。",
    "The only true wisdom is knowing you know nothing.",
    "Carpe diem.",
    "七転び八起き。",
    "Lo que no te mata, te hace más fuerte.",
    "Il faut imaginer Sisyphe heureux.",
    "Not all those who wander are lost.",
    "There's no place like 127.0.0.1",
    "It works on my machine.",
    "sudo make me a sandwich.",
    "Errare humanum est, sed in errare perseverare diabolicum.",
    "In pizza we trust.",
    "42.",
    "May the Force be with you.",
    "話せばわかる。",
    "Veni, vidi, vici.",
    "Live long and prosper.",
    "Nel mezzo del cammin di nostra vita.",
    "To be or not to be.",
    "La vie en rose.",
    "El amor es eterno mientras dura.",
    "音楽は言葉にできないものを語る。",
    "Lasciate ogni speranza, voi ch'entrate.",
    "L'art pour l'art.",
    "Two roads diverged in a wood.",
    "La belleza está en los ojos del que mira.",
    "Cogito ergo sum.",
    "Panta rhei.",
    "Time flies like an arrow; fruit flies like a banana.",
    "La mer, la mer, toujours recommencée.",
    "El tiempo es oro.",
    "水善利万物而不争。",
    "Ogni fine è un nuovo inizio.",
    "The sky is not the limit.",
    "花は咲き、散る。",
    "Après la pluie, le beau temps.",
    "In the beginning was the Word.",
    "With great power comes great responsibility.",
    "Quis custodiet ipsos custodes?",
    "Trust no one.",
    "La porta è chiusa ma la finestra è aperta.",
    "鍵は心の中にある。",
    "Keep it secret, keep it safe.",
    "Omnia mea mecum porto.",
    "Securitas ante omnia.",
    "El secreto mejor guardado es el que no se dice.",
    "Ce qui se conçoit bien s'énonce clairement.",
];

#[derive(Deserialize)]
#[serde(default)]
struct RawInput {
    prompt: String,
}
impl Default for RawInput {
    fn default() -> Self {
        RawInput { prompt: "/".to_string() }
    }
}

#[derive(Deserialize)]
#[serde(default)]
struct RawPanel {
    left_percent: u16,
}
impl Default for RawPanel {
    fn default() -> Self {
        RawPanel { left_percent: 38 }
    }
}

#[derive(Deserialize)]
#[serde(default)]
struct RawBrand {
    color: String,
    tagline: RawTagline,
}
impl Default for RawBrand {
    fn default() -> Self {
        RawBrand { color: "green".to_string(), tagline: RawTagline::default() }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawTagline {
    en: String,
    it: String,
}

// ---- resolved, validated chrome ------------------------------------------

/// How the logo is coloured.
#[derive(Clone, Copy)]
enum LogoColor {
    /// The brand green — constant across accent themes.
    Green,
    /// Follows the current theme's accent.
    Accent,
    /// A fixed truecolor value.
    Rgb(u8, u8, u8),
}

impl LogoColor {
    fn parse(s: &str) -> LogoColor {
        match s.trim().to_ascii_lowercase().as_str() {
            "accent" => LogoColor::Accent,
            "green" => LogoColor::Green,
            hex if hex.starts_with('#') => parse_hex(hex).unwrap_or(LogoColor::Green),
            _ => LogoColor::Green,
        }
    }
}

fn parse_hex(s: &str) -> Option<LogoColor> {
    let h = s.trim_start_matches('#');
    if h.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&h[0..2], 16).ok()?;
    let g = u8::from_str_radix(&h[2..4], 16).ok()?;
    let b = u8::from_str_radix(&h[4..6], 16).ok()?;
    Some(LogoColor::Rgb(r, g, b))
}

pub struct Chrome {
    /// The command-line prompt (sanitized: no control chars, clamped width).
    pub prompt: String,
    /// Left-panel width percentage, clamped to a sane 20–60.
    pub left_percent: u16,
    logo_color: LogoColor,
    tagline_en: String,
    tagline_it: String,
    phrases: Vec<String>,
    extension_chrome_url: String,
    extension_firefox_url: String,
}

impl Default for Chrome {
    fn default() -> Self {
        Chrome::from_raw(RawUi::default())
    }
}

impl Chrome {
    /// Loads chrome from `path`, seeding the embedded default on first run.
    /// Never fails; a malformed file falls back to the defaults and returns a
    /// warning (surfaced in the status bar) instead of silently reverting.
    pub fn load(path: &Path) -> (Chrome, Option<String>) {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(_) => {
                seed(path);
                return (Chrome::default(), None);
            }
        };
        // The version gate runs FIRST, through the schema-tolerant probe: a
        // stale file may use an older schema that the strict RawUi parse
        // rejects, and it must still be regenerated (not reported "invalid").
        // A file that isn't even valid TOML has no readable version; treat it
        // as a hand-broken current file below — warn, don't clobber.
        // Shipped chrome changed since this file was written: back it up and
        // regenerate, so new defaults (e.g. the header phrases) reach the user.
        if read_config_version(&text).is_some_and(|v| v < EMBEDDED_UI_VERSION) {
            let _ = fs::rename(path, path.with_extension("toml.bak"));
            seed(path);
            return (
                Chrome::default(),
                Some(
                    "ui.toml updated to newer built-in chrome (old file saved as ui.toml.bak)"
                        .to_string(),
                ),
            );
        }
        // Current (or unreadable) version: use the file as-is when it parses;
        // otherwise it's the user's own edit gone wrong — fall back with a
        // warning but leave the file alone so their work isn't destroyed.
        match toml::from_str::<RawUi>(&text) {
            Err(_) => (
                Chrome::default(),
                Some("ui.toml is invalid — using built-in chrome defaults".to_string()),
            ),
            Ok(raw) => (Chrome::from_raw(raw), None),
        }
    }

    fn from_raw(raw: RawUi) -> Chrome {
        // sanitize the prompt: drop control chars, clamp to a few columns so a
        // pathological value can't eat the input line. An empty string is legal
        // (no prompt).
        let prompt: String = raw
            .input
            .prompt
            .chars()
            .filter(|c| !c.is_control())
            .take(4)
            .collect();
        // an explicitly-empty phrases list falls back to the built-in ones (a
        // missing key already does via serde default) so the header is never
        // silently text-less.
        let phrases = if raw.header.phrases.is_empty() {
            DEFAULT_PHRASES.iter().map(|s| s.to_string()).collect()
        } else {
            raw.header.phrases
        };
        Chrome {
            prompt,
            left_percent: raw.panel.left_percent.clamp(20, 60),
            logo_color: LogoColor::parse(&raw.brand.color),
            tagline_en: raw.brand.tagline.en,
            tagline_it: raw.brand.tagline.it,
            phrases,
            extension_chrome_url: raw.extension.chrome_url.trim().to_string(),
            extension_firefox_url: raw.extension.firefox_url.trim().to_string(),
        }
    }

    /// The extension's store page for `family`, if one is configured — only
    /// an `http(s)` URL is ever handed to the opener, whatever the file says.
    pub fn extension_url(&self, family: crate::browser::BrowserFamily) -> Option<&str> {
        use crate::browser::BrowserFamily;
        let url = match family {
            BrowserFamily::Chromium => &self.extension_chrome_url,
            BrowserFamily::Gecko => &self.extension_firefox_url,
            BrowserFamily::Safari | BrowserFamily::Unknown => return None,
        };
        (url.starts_with("https://") || url.starts_with("http://")).then_some(url.as_str())
    }

    /// The header's typewriter phrases (language-agnostic — the same pool
    /// whatever the UI language).
    pub fn phrases(&self) -> &[String] {
        &self.phrases
    }

    /// The concrete logo colour against the active palette.
    pub fn logo_color(&self, p: &Palette) -> Color {
        match self.logo_color {
            LogoColor::Green => p.green,
            LogoColor::Accent => p.accent,
            LogoColor::Rgb(r, g, b) => Color::Rgb(r, g, b),
        }
    }

    /// The tagline for `lang`, or empty if none is configured.
    pub fn tagline(&self, lang: Lang) -> &str {
        match lang {
            Lang::En => &self.tagline_en,
            Lang::It => &self.tagline_it,
        }
    }
}

fn seed(path: &Path) {
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let _ = fs::write(path, DEFAULT_UI_TOML);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_default_parses_with_expected_values() {
        let c = Chrome::from_raw(toml::from_str(DEFAULT_UI_TOML).unwrap());
        assert_eq!(c.prompt, "/");
        assert_eq!(c.left_percent, 38);
    }

    #[test]
    fn empty_file_is_valid_and_uses_defaults() {
        let c: RawUi = toml::from_str("").unwrap();
        let c = Chrome::from_raw(c);
        assert_eq!(c.prompt, "/");
        assert_eq!(c.left_percent, 38);
    }

    #[test]
    fn prompt_is_sanitized_and_clamped_and_split_is_bounded() {
        let raw: RawUi = toml::from_str(
            "[input]\nprompt=\"####nope\"\n[panel]\nleft_percent=99\n",
        )
        .unwrap();
        let c = Chrome::from_raw(raw);
        assert_eq!(c.prompt, "####", "clamped to 4 columns");
        assert_eq!(c.left_percent, 60, "clamped to the 20..=60 range");
    }

    #[test]
    fn an_empty_prompt_is_allowed() {
        let raw: RawUi = toml::from_str("[input]\nprompt=\"\"\n").unwrap();
        assert!(Chrome::from_raw(raw).prompt.is_empty());
    }

    #[test]
    fn an_explicitly_empty_phrases_list_falls_back_to_defaults() {
        let raw: RawUi = toml::from_str("[header]\nphrases = []\n").unwrap();
        let c = Chrome::from_raw(raw);
        assert!(!c.phrases().is_empty(), "empty list must not blank the header");
    }

    #[test]
    fn custom_phrases_are_used_as_given() {
        let raw: RawUi =
            toml::from_str("[header]\nphrases = [\"alpha\", \"beta\"]\n").unwrap();
        let c = Chrome::from_raw(raw);
        assert_eq!(c.phrases(), &["alpha".to_string(), "beta".to_string()]);
    }

    #[test]
    fn the_extension_store_urls_are_read_per_family_and_must_be_web_urls() {
        use crate::browser::BrowserFamily;
        let raw: RawUi = toml::from_str(
            "[extension]\nchrome_url = \"https://chromewebstore.google.com/detail/x\"\nfirefox_url = \" file:///etc/passwd \"\n",
        )
        .unwrap();
        let c = Chrome::from_raw(raw);
        assert_eq!(c.extension_url(BrowserFamily::Chromium), Some("https://chromewebstore.google.com/detail/x"));
        assert_eq!(c.extension_url(BrowserFamily::Gecko), None, "not a web URL: never opened");
        assert_eq!(c.extension_url(BrowserFamily::Safari), None);
        // the shipped default has nothing to open yet (not published)
        assert_eq!(Chrome::default().extension_url(BrowserFamily::Chromium), None);
    }

    #[test]
    fn malformed_ui_toml_falls_back_with_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ui.toml");
        std::fs::write(&path, "this is ][ not toml").unwrap();
        let (chrome, warn) = Chrome::load(&path);
        assert_eq!(chrome.prompt, "/", "falls back to defaults");
        assert!(warn.unwrap().contains("invalid"), "surfaces a warning");
    }

    #[test]
    fn a_missing_ui_toml_is_seeded_without_warning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/ui.toml");
        let (_chrome, warn) = Chrome::load(&path);
        assert!(path.exists(), "seeded on first run");
        assert!(warn.is_none());
    }

    #[test]
    fn a_stale_ui_toml_is_regenerated_with_a_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ui.toml");
        // an old file (version below the embedded one) with a custom prompt
        std::fs::write(&path, "version = 1\n[input]\nprompt = \"X\"\n").unwrap();
        let (chrome, warn) = Chrome::load(&path);
        assert_eq!(chrome.prompt, "/", "regenerated to the new default");
        assert!(warn.unwrap().contains("updated"));
        assert!(path.with_extension("toml.bak").exists(), "old file backed up");
    }

    #[test]
    fn a_stale_old_schema_ui_toml_is_regenerated_not_reported_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ui.toml");
        // the real-world regression: a version-2 file with the OLD header
        // schema (phrases.en/.it TABLES, not the current phrases ARRAY). The
        // strict RawUi parse rejects this, but the version gate must still
        // see version=2 and regenerate instead of warning "invalid" forever.
        std::fs::write(
            &path,
            "version = 2\n\
             [header]\n\
             phrases.en = [\"welcome to DataShield\"]\n\
             phrases.it = [\"benvenuto in DataShield\"]\n",
        )
        .unwrap();
        let (chrome, warn) = Chrome::load(&path);
        let warn = warn.unwrap();
        assert!(warn.contains("updated"), "must regenerate, got: {warn}");
        assert!(!warn.contains("invalid"));
        assert!(path.with_extension("toml.bak").exists(), "old file backed up");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            DEFAULT_UI_TOML,
            "regenerated to the embedded default"
        );
        assert_eq!(chrome.prompt, "/");
    }

    #[test]
    fn a_broken_current_version_ui_toml_warns_but_is_not_clobbered() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ui.toml");
        // valid TOML at the CURRENT version, but a hand-edit broke a known
        // field's type — respect the user's file: warn, don't rewrite it.
        let body = "version = 4\n[panel]\nleft_percent = \"wide\"\n";
        std::fs::write(&path, body).unwrap();
        let (chrome, warn) = Chrome::load(&path);
        assert_eq!(chrome.prompt, "/", "falls back to defaults");
        assert!(warn.unwrap().contains("invalid"));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            body,
            "the user's file must be left untouched"
        );
        assert!(!path.with_extension("toml.bak").exists());
    }

    #[test]
    fn a_non_toml_file_warns_and_is_not_clobbered() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ui.toml");
        // no version is readable at all → cannot prove it's stale → treat it
        // as a broken current file: warn, leave it alone.
        std::fs::write(&path, "this is ][ not toml").unwrap();
        let (chrome, warn) = Chrome::load(&path);
        assert_eq!(chrome.prompt, "/");
        assert!(warn.unwrap().contains("invalid"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "this is ][ not toml");
        assert!(!path.with_extension("toml.bak").exists());
    }

    #[test]
    fn a_current_version_ui_toml_is_used_as_is() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ui.toml");
        std::fs::write(&path, "version = 4\n[input]\nprompt = \">\"\n").unwrap();
        let (chrome, warn) = Chrome::load(&path);
        assert_eq!(chrome.prompt, ">");
        assert!(warn.is_none());
    }
}
