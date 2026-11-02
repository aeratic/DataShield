//! Shared helper for the seeded, versioned, app-READ config files
//! (`ui.toml`, `commands.toml`).
//!
//! Those files carry a `version` key so that a newer shipped default can
//! regenerate an older on-disk copy (backing it up to `.bak`). The version
//! MUST be readable independently of the rest of the schema: an old file may
//! have been written against an older schema that the loader's full mirror
//! struct no longer deserializes, and it must still be recognised as stale
//! and regenerated — not reported "invalid" forever.

use serde::Deserialize;

/// Minimal, schema-TOLERANT view of a versioned config file: just `version`.
/// Serde ignores unknown fields, so this parses any well-formed TOML document
/// whatever the rest of the schema looks like.
#[derive(Deserialize)]
struct VersionProbe {
    /// A missing key (pre-versioning files) means 0, so they regenerate.
    #[serde(default)]
    version: u32,
}

/// The `version` key of a seeded, versioned config file, read tolerantly.
///
/// - `Some(n)` — valid TOML with `version = n`;
/// - `Some(0)` — valid TOML with no `version` key (an old, pre-versioning
///   file — stale by definition, so the caller regenerates it);
/// - `None` — not valid TOML at all, or `version` has the wrong type: the
///   version is genuinely unreadable, so staleness cannot be proven. The
///   caller must then treat the file as a hand-broken CURRENT file — warn
///   and fall back to defaults, but NEVER clobber it.
pub fn read_config_version(text: &str) -> Option<u32> {
    toml::from_str::<VersionProbe>(text).ok().map(|p| p.version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_probe_semantics() {
        // old schema, unknown keys: still readable
        assert_eq!(
            read_config_version("version = 2\n[header]\nphrases.en = [\"x\"]\n"),
            Some(2)
        );
        // missing version key => 0 (stale, regenerate)
        assert_eq!(read_config_version("[input]\nprompt = \"/\"\n"), Some(0));
        assert_eq!(read_config_version(""), Some(0));
        // not TOML at all => unreadable (never clobber)
        assert_eq!(read_config_version("this is ][ not toml"), None);
        // mistyped version => unreadable (never clobber)
        assert_eq!(read_config_version("version = \"2\""), None);
    }
}
