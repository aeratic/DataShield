//! DataShield Content Schema v1 (DCS-1) — the portable item storage standard.
//!
//! This is the decrypted content that lives *inside* a vault's single AEAD blob.
//! It is a self-identifying, versioned JSON document so a future DataShield GUI
//! can read/write the exact same `.vault` files: a `.vault` file plus the master
//! password is the entire interchange — no key ever travels (see
//! `docs/STORAGE-STANDARD.md`).
//!
//! Design rules that make it portable:
//!   * every field is self-describing (carries its own `type`, `secret`, and an
//!     optional display `label`), so losing the category registry loses nothing;
//!   * unknown JSON members and unknown field-type tokens are preserved verbatim
//!     on rewrite (`extra` flatten maps + `FieldType::Other`), so a newer writer
//!     never has its data dropped by an older reader;
//!   * `secret` — never `type` — is the authoritative masking flag, so an unknown
//!     type can never leak.

use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

/// Marker written into every DCS document so readers can self-identify it.
pub const CONTENT_SCHEMA: &str = "datashield.content";
/// The content schema version (independent of the file-format version).
pub const CONTENT_SCHEMA_VERSION: u32 = 1;

/// The whole decrypted document inside the AEAD blob (file format 4).
#[derive(Serialize, Deserialize)]
pub struct VaultContent {
    pub schema: String,
    pub schema_version: u32,
    /// CUSTOM categories only — built-ins live in code (see [`BUILTINS`]).
    #[serde(default)]
    pub categories: Vec<CategoryDef>,
    #[serde(default)]
    pub items: Vec<Item>,
    /// Unknown members written by other/newer DataShield apps — round-tripped.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Default for VaultContent {
    fn default() -> Self {
        VaultContent {
            schema: CONTENT_SCHEMA.to_string(),
            schema_version: CONTENT_SCHEMA_VERSION,
            categories: Vec::new(),
            items: Vec::new(),
            extra: serde_json::Map::new(),
        }
    }
}

impl VaultContent {
    /// The monotonic sync counter kept in `extra["x_sync"]` — bumped on every
    /// content-changing save, so a peer can tell "strictly newer" from a
    /// replayed OLDER blob. It lives INSIDE the encrypted content, so it is
    /// authenticated + confidential (a server can neither read nor forge it).
    /// The `x_` prefix keeps [`CONTENT_SCHEMA_VERSION`] at 1 and other readers
    /// round-trip it untouched.
    pub fn sync_counter(&self) -> u64 {
        self.extra
            .get("x_sync")
            .and_then(|v| v.get("counter"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
    }

    /// Bumps the sync counter (+1) and stamps the device/time. Call once per
    /// content-changing save, right before encrypting.
    pub fn bump_sync_counter(&mut self, device_id: &str) -> u64 {
        let n = self.sync_counter() + 1;
        self.extra.insert(
            "x_sync".to_string(),
            serde_json::json!({ "counter": n, "device": device_id, "updated_at": now_rfc3339() }),
        );
        n
    }

    /// A fingerprint of the actual DATA (items, custom categories, and every
    /// app-private `x_` member), EXCLUDING the `x_sync` metadata — so two blobs
    /// with identical data but different sync stamps compare equal. Used to
    /// catch an equal-counter divergence (both sides edited from the same base
    /// into different content).
    ///
    /// The `extra` walk is what makes a Memorandum message count as an edit:
    /// `x_memo` is content, not metadata. `x_sync` is the one member left out —
    /// it is the stamp. The digest is stable across processes because
    /// `serde_json::Map` is a `BTreeMap`: both this walk and each value's
    /// serialization are key-sorted.
    pub fn content_fingerprint(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        if let Ok(items) = serde_json::to_vec(&self.items) {
            hasher.update(&items);
        }
        if let Ok(cats) = serde_json::to_vec(&self.categories) {
            hasher.update(&cats);
        }
        for (key, value) in self.extra.iter().filter(|(k, _)| k.as_str() != "x_sync") {
            hasher.update(key.as_bytes());
            if let Ok(bytes) = serde_json::to_vec(value) {
                hasher.update(&bytes);
            }
        }
        hasher.finalize().to_hex().to_string()
    }
}

/// One stored secret/record. `id` is the stable identity; `label` is display.
#[derive(Serialize, Deserialize)]
pub struct Item {
    /// UUIDv4, lowercase, immutable — the target of every edit/delete/reference.
    pub id: String,
    /// Category id (a built-in reserved id, a custom id, or one this reader
    /// doesn't know — unknown ids still render fully because items are
    /// self-describing).
    pub category: String,
    /// Display name in the list (may duplicate; UI disambiguates).
    pub label: String,
    /// Ordered — the array order IS the display order and is preserved on save.
    pub fields: Vec<ItemField>,
    /// RFC 3339 UTC; `None` = unknown (some migrated legacy items).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// RFC 3339 UTC; `None` = never modified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<String>,
    /// Favorite flag (the `star` command). Additive to DCS-1: absent ⇒ false and
    /// false is never serialized, so unstarred vaults round-trip byte-identically
    /// and CONTENT_SCHEMA_VERSION stays 1.
    #[serde(default, skip_serializing_if = "is_false")]
    pub favorite: bool,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

fn is_false(v: &bool) -> bool {
    !*v
}

impl Item {
    /// A fresh, empty item of `category` with `label`, stamped created-now.
    pub fn new(category: &str, label: &str) -> Item {
        Item {
            id: new_uuid_v4(),
            category: category.to_string(),
            label: label.to_string(),
            fields: Vec::new(),
            favorite: false,
            created_at: Some(now_rfc3339()),
            modified_at: None,
            extra: serde_json::Map::new(),
        }
    }
}

/// One field of an item. `secret` is the authoritative masking flag.
#[derive(Serialize, Deserialize)]
pub struct ItemField {
    /// Machine key, `[a-z0-9_]{1,64}`, unique within the item.
    pub key: String,
    #[serde(rename = "type", default)]
    pub ftype: FieldType,
    /// `None` ⇒ the app localizes the built-in `key`; `Some` ⇒ display verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Always a JSON string (preserves leading zeros in PINs/card numbers).
    pub value: SecretString,
    /// Authoritative masking flag. Missing ⇒ false; masking follows this, never
    /// `type`, so an unknown type can never leak.
    #[serde(default)]
    pub secret: bool,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// A `String` wiped on drop. Transparent to serde; deliberately no
/// `Debug`/`Clone`/`Display` so a secret value can't leak into a log or a clone.
#[derive(Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(transparent)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(s: impl Into<String>) -> Self {
        SecretString(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Drop for SecretString {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// The DCS-1 field-type set. Unknown tokens are preserved verbatim
/// ([`FieldType::Other`]) and rendered as `Text`; `type` never gates parsing or
/// masking, only rendering/validation hints.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum FieldType {
    #[default]
    Text,
    Note,
    Password,
    Pin,
    Email,
    Username,
    Url,
    Phone,
    Number,
    Date,
    Expiry,
    CardNumber,
    Totp,
    /// A picture file kept inside the vault: the value is the base64 of the
    /// ORIGINAL file bytes (no re-encode — a scanned document must stay as
    /// legible as the file it came from) and the field's `extra["x_image"]`
    /// carries `{"mime": …, "bytes": N}`.
    ///
    /// Its [`default_secret`](FieldType::default_secret) is TRUE on purpose:
    /// an older DataShield reader maps the unknown `image` token to
    /// [`FieldType::Other`] and masks by `secret`, so it renders eight bullets
    /// instead of dumping megabytes of base64 onto the screen.
    Image,
    Other(String),
}

impl FieldType {
    /// Every known token, in a stable order (used by the type chooser).
    pub const KNOWN: [FieldType; 14] = [
        FieldType::Text,
        FieldType::Note,
        FieldType::Password,
        FieldType::Pin,
        FieldType::Email,
        FieldType::Username,
        FieldType::Url,
        FieldType::Phone,
        FieldType::Number,
        FieldType::Date,
        FieldType::Expiry,
        FieldType::CardNumber,
        FieldType::Totp,
        FieldType::Image,
    ];

    pub fn token(&self) -> &str {
        match self {
            FieldType::Text => "text",
            FieldType::Note => "note",
            FieldType::Password => "password",
            FieldType::Pin => "pin",
            FieldType::Email => "email",
            FieldType::Username => "username",
            FieldType::Url => "url",
            FieldType::Phone => "phone",
            FieldType::Number => "number",
            FieldType::Date => "date",
            FieldType::Expiry => "expiry",
            FieldType::CardNumber => "card_number",
            FieldType::Totp => "totp",
            FieldType::Image => "image",
            FieldType::Other(s) => s,
        }
    }

    /// Types whose fields default to secret in the UI (the user may override).
    /// [`FieldType::Image`] is in here for interop, not for privacy: masking by
    /// `secret` is what keeps an older reader from printing its base64.
    pub fn default_secret(&self) -> bool {
        matches!(
            self,
            FieldType::Password
                | FieldType::Pin
                | FieldType::Totp
                | FieldType::CardNumber
                | FieldType::Image
        )
    }
}

impl From<String> for FieldType {
    fn from(s: String) -> Self {
        FieldType::KNOWN
            .iter()
            .find(|t| t.token() == s)
            .cloned()
            .unwrap_or(FieldType::Other(s))
    }
}

impl From<FieldType> for String {
    fn from(t: FieldType) -> String {
        t.token().to_string()
    }
}

/// A CUSTOM category definition. Built-ins live in code ([`BUILTINS`]); these
/// live in the vault so they travel with it on export.
#[derive(Serialize, Deserialize)]
pub struct CategoryDef {
    /// Slug `[a-z0-9_]{1,64}`, unique per vault. Custom defs resolve BEFORE
    /// built-ins with the same id, so future built-in additions never break an
    /// existing vault.
    pub id: String,
    /// As the user typed it (not localized).
    pub name: String,
    /// The Add-form template.
    pub fields: Vec<FieldSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// One field slot in a category template.
#[derive(Serialize, Deserialize)]
pub struct FieldSpec {
    pub key: String,
    #[serde(rename = "type", default)]
    pub ftype: FieldType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default)]
    pub secret: bool,
    #[serde(default)]
    pub required: bool,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

// ---- built-in categories (static; ids reserved by the standard) -----------

/// One slot of a built-in category template.
pub struct BuiltinField {
    pub key: &'static str,
    pub ftype: FieldType,
    pub secret: bool,
    pub required: bool,
}

/// A built-in category: a reserved id and its ordered field template.
pub struct BuiltinCategory {
    pub id: &'static str,
    pub fields: &'static [BuiltinField],
}

macro_rules! bf {
    ($key:literal, $ty:expr, $secret:expr, $req:expr) => {
        BuiltinField { key: $key, ftype: $ty, secret: $secret, required: $req }
    };
}

use FieldType as F;

/// The nine reserved built-in categories, in tab/picker order. `database` is
/// deliberately NOT here — it is the canonical *custom* category example.
pub static BUILTINS: &[BuiltinCategory] = &[
    BuiltinCategory {
        id: "email",
        fields: &[
            bf!("email", F::Email, false, true),
            bf!("password", F::Password, true, true),
            bf!("totp", F::Totp, true, false),
            bf!("recovery_email", F::Email, false, false),
            bf!("url", F::Url, false, false),
            bf!("notes", F::Note, false, false),
        ],
    },
    BuiltinCategory {
        id: "account",
        fields: &[
            bf!("username", F::Username, false, true),
            bf!("email", F::Email, false, false),
            bf!("password", F::Password, true, true),
            bf!("url", F::Url, false, false),
            bf!("totp", F::Totp, true, false),
            bf!("notes", F::Note, false, false),
        ],
    },
    BuiltinCategory {
        id: "login",
        fields: &[
            bf!("username", F::Username, false, false),
            bf!("email", F::Email, false, false),
            bf!("password", F::Password, true, true),
            bf!("url", F::Url, false, false),
            bf!("notes", F::Note, false, false),
        ],
    },
    BuiltinCategory {
        id: "credit_card",
        fields: &[
            bf!("cardholder", F::Text, false, true),
            bf!("brand", F::Text, false, false),
            bf!("number", F::CardNumber, true, true),
            bf!("expiry", F::Expiry, false, true),
            bf!("cvv", F::Pin, true, true),
            bf!("pin", F::Pin, true, false),
            bf!("issuing_bank", F::Text, false, false),
            bf!("url", F::Url, false, false),
            bf!("notes", F::Note, false, false),
        ],
    },
    BuiltinCategory {
        id: "api_key",
        fields: &[
            bf!("service", F::Text, false, true),
            bf!("key_id", F::Text, false, false),
            bf!("api_key", F::Password, true, true),
            bf!("api_secret", F::Password, true, false),
            bf!("environment", F::Text, false, false),
            bf!("url", F::Url, false, false),
            bf!("expires", F::Date, false, false),
            bf!("notes", F::Note, false, false),
        ],
    },
    BuiltinCategory {
        id: "note",
        fields: &[
            bf!("body", F::Note, true, true),
            bf!("url", F::Url, false, false),
        ],
    },
    BuiltinCategory {
        id: "ssh_key",
        fields: &[
            bf!("host", F::Text, false, true),
            bf!("port", F::Number, false, false),
            bf!("username", F::Username, false, false),
            bf!("private_key", F::Note, true, true),
            bf!("passphrase", F::Password, true, false),
            bf!("public_key", F::Note, false, false),
            bf!("url", F::Url, false, false),
            bf!("notes", F::Note, false, false),
        ],
    },
    BuiltinCategory {
        id: "wifi",
        fields: &[
            bf!("ssid", F::Text, false, true),
            bf!("password", F::Password, true, true),
            bf!("security", F::Text, false, false),
            bf!("url", F::Url, false, false),
            bf!("notes", F::Note, false, false),
        ],
    },
    BuiltinCategory {
        id: "identity",
        fields: &[
            bf!("full_name", F::Text, false, true),
            bf!("birth_date", F::Date, false, false),
            bf!("fiscal_code", F::Text, true, false),
            bf!("document_type", F::Text, false, false),
            bf!("document_number", F::Text, true, false),
            bf!("issued", F::Date, false, false),
            bf!("expires", F::Date, false, false),
            bf!("url", F::Url, false, false),
            bf!("notes", F::Note, false, false),
            // the two document photos: imported from a file, stored as base64
            // and always secret (see FieldType::Image). Additive to DCS-1 —
            // items are self-describing, so no schema bump.
            bf!("photo_front", F::Image, true, false),
            bf!("photo_back", F::Image, true, false),
        ],
    },
];

/// The first URL/website value on an item, if any (used to derive the favicon
/// domain). Prefers a field of type `Url`, then a field keyed `url`/`website`.
pub fn item_website(item: &Item) -> Option<&str> {
    item.fields
        .iter()
        .find(|f| f.ftype == FieldType::Url)
        .or_else(|| {
            item.fields
                .iter()
                .find(|f| matches!(f.key.as_str(), "url" | "website"))
        })
        .map(|f| f.value.as_str())
        .filter(|s| !s.is_empty())
}

/// The row a "copy the password" verb takes, as an index into `item.fields`:
/// the first field DECLARED [`FieldType::Password`], else the first one KEYED
/// `password` (a hand-added row carries plain `Text`). An index, not a value,
/// so the copy still runs through `clipboard::copy_payload` like every other
/// one — the "which bytes leave the vault" decision stays in one place.
pub fn item_password_index(item: &Item) -> Option<usize> {
    item.fields
        .iter()
        .position(|f| f.ftype == FieldType::Password)
        .or_else(|| item.fields.iter().position(|f| f.key.eq_ignore_ascii_case("password")))
}

/// The row a "copy the user" verb takes: the first field declared a username or
/// an email, else the first one keyed `username`/`user`/`email`/`login`. Same
/// contract as [`item_password_index`] — an index into `item.fields`.
pub fn item_user_index(item: &Item) -> Option<usize> {
    item.fields
        .iter()
        .position(|f| f.ftype == FieldType::Username || f.ftype == FieldType::Email)
        .or_else(|| {
            item.fields.iter().position(|f| {
                matches!(f.key.to_ascii_lowercase().as_str(), "username" | "user" | "email" | "login")
            })
        })
}

/// A stored website made openable: a value that already names a scheme is
/// handed over as it is, anything else gets `https://` — a browser opener is
/// given a URL, not a bare host, and http would be the wrong default for a
/// password manager. Trimmed, because the field is free text.
pub fn website_url(site: &str) -> String {
    let site = site.trim();
    // a scheme is `letter [letter|digit|+|-|.]* ':'` (RFC 3986) — checked by
    // hand so `mail.example.com/x:1` can't pass for one
    let scheme = site
        .split_once("://")
        .map(|(s, _)| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)))
        .unwrap_or(false);
    if scheme {
        site.to_string()
    } else {
        format!("https://{site}")
    }
}

/// The reserved built-in id set (custom category ids must not shadow these at
/// creation time).
pub fn is_reserved_category_id(id: &str) -> bool {
    BUILTINS.iter().any(|c| c.id == id) || id == "database" || id == "bank_account"
}

/// Derives a DCS-1 slug (`[a-z0-9_]{1,64}`) from free text: ASCII alphanumerics
/// lowercased, every other run collapsed to a single `_`, no leading/trailing
/// `_`, truncated to 64. Empty when nothing survives.
pub fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut prev_us = true; // suppress a leading underscore
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_us = false;
        } else if !prev_us {
            out.push('_');
            prev_us = true;
        }
        if out.len() == 64 {
            break;
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    out
}

// ---- helpers (no extra crates) --------------------------------------------

/// A UUIDv4 string from OS randomness — no new dependency.
pub fn new_uuid_v4() -> String {
    let mut b = [0u8; 16];
    getrandom::getrandom(&mut b).expect("OS random generator unavailable");
    b[6] = (b[6] & 0x0f) | 0x40; // version 4
    b[8] = (b[8] & 0x3f) | 0x80; // RFC 4122 variant
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

/// RFC 3339 UTC "now" (e.g. `2026-07-11T09:30:00Z`) without a date crate, using
/// Howard Hinnant's days→civil-date algorithm. Falls back to the epoch if the
/// clock is before it.
pub fn now_rfc3339() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    rfc3339_from_unix(secs)
}

/// This machine's offset from UTC right now, in SECONDS (Rome in August ⇒
/// `7200`). The one thing the civil-date arithmetic here cannot do on its own:
/// it needs the system's zone database, which is exactly what `chrono`'s
/// `clock` feature is for and the only thing it is used for.
pub fn local_utc_offset() -> i32 {
    use chrono::{Local, Offset};
    Local::now().offset().fix().local_minus_utc()
}

/// A stored RFC 3339 UTC stamp as the WALL CLOCK `offset` seconds east of
/// Greenwich reads: `("2026-08-20", "00:30")`. `None` for a stamp this build
/// cannot read — the caller then shows what it was given rather than inventing
/// a time.
///
/// A DISPLAY conversion, and nothing else: storage stays UTC (`ts` is the
/// Memorandum's sort key, and `docs/STORAGE-STANDARD.md` mandates it), so this
/// result must never be written back.
pub fn wall_clock(ts: &str, offset: i32) -> Option<(String, String)> {
    let shifted = unix_from_rfc3339(ts)?.checked_add(offset as i64)?;
    let stamp = rfc3339_from_unix(u64::try_from(shifted).ok()?);
    Some((stamp.get(..10)?.to_string(), stamp.get(11..16)?.to_string()))
}

/// Seconds since the epoch of an RFC 3339 UTC stamp — the inverse of
/// [`rfc3339_from_unix`], down to the same civil-date algorithm. Only the shape
/// this build writes (`YYYY-MM-DDTHH:MM:SSZ`) is accepted: a stamp carrying its
/// own offset is somebody else's convention, and guessing at one would move a
/// message to the wrong day.
fn unix_from_rfc3339(ts: &str) -> Option<i64> {
    let (date, rest) = ts.split_once('T')?;
    let time = rest.strip_suffix('Z')?;
    let mut parts = date.split('-');
    let (y, m, d) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let (y, m, d) = (y.parse::<i64>().ok()?, m.parse::<i64>().ok()?, d.parse::<i64>().ok()?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let mut clock = time.split(':');
    let (h, mi, s) = (clock.next()?, clock.next()?, clock.next()?);
    if clock.next().is_some() {
        return None;
    }
    let (h, mi, s) = (h.parse::<i64>().ok()?, mi.parse::<i64>().ok()?, s.parse::<i64>().ok()?);
    if h > 23 || mi > 59 || s > 60 {
        return None;
    }
    Some(days_from_civil(y, m, d) * 86_400 + h * 3600 + mi * 60 + s)
}

/// days_from_civil (Howard Hinnant), the inverse of the algorithm in
/// [`rfc3339_from_unix`]: days since 1970-01-01 of a civil date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if m > 2 { m - 3 } else { m + 9 }; // [0, 11]
    let doy = (153 * mp + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// Formats a Unix timestamp (seconds) as RFC 3339 UTC.
fn rfc3339_from_unix(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // civil_from_days (Howard Hinnant), epoch shifted to 0000-03-01
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_v4_has_the_right_shape_and_bits() {
        let id = new_uuid_v4();
        assert_eq!(id.len(), 36);
        let bytes: Vec<char> = id.chars().collect();
        assert_eq!(bytes[8], '-');
        assert_eq!(bytes[13], '-');
        assert_eq!(bytes[14], '4', "version nibble is 4");
        assert!(matches!(bytes[19], '8' | '9' | 'a' | 'b'), "RFC 4122 variant");
        assert!(id.chars().all(|c| c.is_ascii_hexdigit() || c == '-'));
        assert_ne!(new_uuid_v4(), new_uuid_v4(), "unique per call");
    }

    #[test]
    fn rfc3339_formats_known_timestamps() {
        assert_eq!(rfc3339_from_unix(0), "1970-01-01T00:00:00Z");
        // the well-known Unix "billennium"
        assert_eq!(rfc3339_from_unix(1_000_000_000), "2001-09-09T01:46:40Z");
        // a leap day (2024-02-29) to exercise the civil-date math
        assert_eq!(rfc3339_from_unix(1_709_164_800), "2024-02-29T00:00:00Z");
    }

    #[test]
    fn a_stored_utc_stamp_is_read_back_on_the_users_own_clock() {
        // the offset is PINNED here: a test that read the machine's zone would
        // assert nothing on one machine and fail on another.
        // half past midnight in Rome is half past ten of the day BEFORE in UTC
        assert_eq!(
            wall_clock("2026-08-19T22:30:00Z", 2 * 3600),
            Some(("2026-08-20".to_string(), "00:30".to_string())),
            "the day rolls over with the clock"
        );
        assert_eq!(
            wall_clock("2026-08-19T22:30:00Z", 0),
            Some(("2026-08-19".to_string(), "22:30".to_string())),
            "…and at Greenwich the stamp is shown as stored"
        );
        // …and west of it the day rolls the other way
        assert_eq!(
            wall_clock("2026-08-19T02:30:00Z", -5 * 3600),
            Some(("2026-08-18".to_string(), "21:30".to_string()))
        );
        // every stamp this build writes reads back exactly
        for secs in [0u64, 1, 1_000_000_000, 1_709_164_800, 2_000_000_000] {
            let stamp = rfc3339_from_unix(secs);
            assert_eq!(unix_from_rfc3339(&stamp), Some(secs as i64), "{stamp}");
        }
        // anything else is not a time: the caller shows what it was given
        for odd in [
            "",
            "not a date",
            "2026-08-19",
            "2026-08-19T22:30:00+02:00",
            "2026-13-01T00:00:00Z",
            "2026-08-19T25:00:00Z",
            "2026-08-19T22:30Z",
        ] {
            assert_eq!(wall_clock(odd, 7200), None, "{odd}");
        }
    }

    #[test]
    fn unknown_field_type_tokens_round_trip() {
        let ft: FieldType = "quantum_key".to_string().into();
        assert!(matches!(ft, FieldType::Other(ref s) if s == "quantum_key"));
        assert_eq!(ft.token(), "quantum_key");
        // known tokens map back to their variant
        let pw: FieldType = "password".to_string().into();
        assert_eq!(pw, FieldType::Password);
    }

    #[test]
    fn secret_string_serializes_transparently() {
        let f = ItemField {
            key: "password".into(),
            ftype: FieldType::Password,
            label: None,
            value: SecretString::new("hunter2"),
            secret: true,
            extra: serde_json::Map::new(),
        };
        let json = serde_json::to_string(&f).unwrap();
        assert!(json.contains("\"value\":\"hunter2\""), "value is a bare string");
        assert!(json.contains("\"type\":\"password\""));
    }

    #[test]
    fn an_image_field_round_trips_with_its_metadata() {
        // the token, the secret default and the `x_image` extra are the whole
        // contract another DataShield reader sees.
        assert!(FieldType::KNOWN.contains(&FieldType::Image));
        assert_eq!(FieldType::Image.token(), "image");
        assert!(FieldType::Image.default_secret(), "an older reader must mask it, not print it");
        let src = r#"{"key":"photo_front","type":"image","value":"aGk=","secret":true,
            "x_image":{"mime":"image/png","bytes":2}}"#;
        let field: ItemField = serde_json::from_str(src).unwrap();
        assert_eq!(field.ftype, FieldType::Image, "the token maps to the variant");
        assert_eq!(field.extra["x_image"]["mime"], "image/png");
        assert_eq!(field.extra["x_image"]["bytes"], 2);
        let out = serde_json::to_string(&field).unwrap();
        assert!(out.contains("\"type\":\"image\""));
        assert!(out.contains("\"x_image\""), "the metadata survives a rewrite");
        assert!(out.contains("\"value\":\"aGk=\""), "the value is bare base64");
    }

    #[test]
    fn the_identity_category_carries_both_document_photos() {
        let identity = BUILTINS.iter().find(|c| c.id == "identity").expect("built-in identity");
        for key in ["photo_front", "photo_back"] {
            let slot = identity.fields.iter().find(|f| f.key == key).expect(key);
            assert_eq!(slot.ftype, FieldType::Image);
            assert!(slot.secret, "{key} is stored secret");
            assert!(!slot.required, "{key} is optional");
        }
    }

    /// A field with just the parts these resolvers read.
    fn f(key: &str, ftype: FieldType) -> ItemField {
        ItemField {
            key: key.to_string(),
            ftype,
            label: None,
            value: SecretString::new("v"),
            secret: false,
            extra: serde_json::Map::new(),
        }
    }

    fn item_of(fields: Vec<ItemField>) -> Item {
        let mut item = Item::new("login", "Acme");
        item.fields = fields;
        item
    }

    #[test]
    fn the_copy_verbs_resolve_their_row_by_type_first_then_by_key() {
        // the declared type wins wherever there is one…
        let typed = item_of(vec![
            f("note", FieldType::Note),
            f("account", FieldType::Username),
            f("secret_word", FieldType::Password),
        ]);
        assert_eq!(item_password_index(&typed), Some(2));
        assert_eq!(item_user_index(&typed), Some(1));
        // …and a hand-added row (everything plain `Text`) is found by its key
        let keyed = item_of(vec![
            f("notes", FieldType::Text),
            f("password", FieldType::Text),
            f("login", FieldType::Text),
        ]);
        assert_eq!(item_password_index(&keyed), Some(1));
        assert_eq!(item_user_index(&keyed), Some(2), "login counts as the user row");
        // an email-typed row is the user row too (an email IS the login here)
        let mail = item_of(vec![f("address", FieldType::Email)]);
        assert_eq!(item_user_index(&mail), Some(0));
        // …and an item with neither resolves to nothing (the caller toasts)
        let bare = item_of(vec![f("body", FieldType::Note)]);
        assert_eq!(item_password_index(&bare), None);
        assert_eq!(item_user_index(&bare), None);
    }

    #[test]
    fn a_website_is_given_a_scheme_before_it_is_opened() {
        assert_eq!(website_url("https://amazon.it"), "https://amazon.it", "kept as it is");
        assert_eq!(website_url("http://intranet.local"), "http://intranet.local");
        assert_eq!(website_url("amazon.it"), "https://amazon.it", "https, never http");
        assert_eq!(website_url("  amazon.it/x  "), "https://amazon.it/x", "trimmed");
        // a colon in the PATH is not a scheme
        assert_eq!(website_url("example.com/a:b"), "https://example.com/a:b");
    }

    #[test]
    fn unknown_json_members_are_preserved_on_round_trip() {
        let src = r#"{"schema":"datashield.content","schema_version":1,"items":[
            {"id":"x","category":"login","label":"L","fields":[
                {"key":"password","type":"password","value":"p","secret":true,"x_future":42}
            ],"x_note":"hi"}
        ],"x_root":"keep"}"#;
        let content: VaultContent = serde_json::from_str(src).unwrap();
        let out = serde_json::to_string(&content).unwrap();
        assert!(out.contains("x_root"), "unknown root member kept");
        assert!(out.contains("x_note"), "unknown item member kept");
        assert!(out.contains("x_future"), "unknown field member kept");
    }

    #[test]
    fn slugify_makes_dcs1_slugs() {
        assert_eq!(slugify("My DBs"), "my_dbs");
        assert_eq!(slugify("Data—Base!!"), "data_base");
        assert_eq!(slugify("  leading/trailing  "), "leading_trailing");
        assert_eq!(slugify("***"), "", "symbols-only yields empty");
        assert_eq!(slugify(&"x".repeat(100)).len(), 64, "truncated to 64");
        assert!(is_reserved_category_id(&slugify("Database")), "reserved check runs on the slug");
    }

    #[test]
    fn builtin_ids_are_unique_and_reserved() {
        let mut seen = std::collections::HashSet::new();
        for c in BUILTINS {
            assert!(seen.insert(c.id), "duplicate built-in id {}", c.id);
            assert!(is_reserved_category_id(c.id));
            assert!(!c.fields.is_empty(), "{} has fields", c.id);
        }
        assert!(is_reserved_category_id("database"), "database reserved (custom example)");
        assert!(!is_reserved_category_id("streaming"), "a real custom id is free");
    }
}
