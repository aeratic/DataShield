//! Vault storage: one JSON file per vault.
//!
//! Account metadata (username, algorithm, KDF parameters, UI style) is stored
//! in clear; the item list is serialized to JSON and encrypted as a single
//! AEAD blob. Password verification happens implicitly through the AEAD tag:
//! there is no separate password hash to attack.

use crate::crypto::{self, Algorithm, KdfSpec, KEY_LEN, SALT_LEN};
use crate::model::{FieldType, Item, ItemField, SecretString, VaultContent};
use anyhow::{anyhow, bail, Context, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

/// Current on-disk file-format version. v4 stores a DCS-1 content document
/// (see `model.rs`); v3 stored a bare array of [`LegacyItem`]s.
pub const FORMAT_VERSION: u32 = 4;
/// File-format versions this build can open (older ones are migrated in memory
/// and rewritten as [`FORMAT_VERSION`] on the first save).
const READABLE_VERSIONS: &[u32] = &[3, 4];
const VAULT_EXT: &str = "vault";

/// The legacy (format 3) on-disk item — a fixed {name,email,password} record.
/// Kept private and read-only: it exists only to parse old vaults and migrate
/// them into the DCS-1 model.
#[derive(Deserialize, Default)]
struct LegacyItem {
    #[serde(default)]
    name: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    when_created: String,
    #[serde(default)]
    when_modified: String,
}

/// Sparse on-disk KDF description: only the fields relevant to the chosen
/// algorithm are written (Argon2: m/t/p, scrypt: log_n/r/p_cost,
/// PBKDF2: iterations, bcrypt: rounds, plain hashes: none).
#[derive(Serialize, Deserialize, Default)]
struct KdfSection {
    algorithm: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    m_cost_kib: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    t_cost: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    p_cost: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    log_n: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    r: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    iterations: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    rounds: u32,
    salt: String,
}

fn is_zero(v: &u32) -> bool {
    *v == 0
}

fn kdf_section(spec: &KdfSpec, salt_b64: String) -> KdfSection {
    let mut section = KdfSection {
        algorithm: spec.name().to_string(),
        salt: salt_b64,
        ..KdfSection::default()
    };
    match *spec {
        KdfSpec::Argon2 { m_cost_kib, t_cost, p_cost, .. } => {
            section.m_cost_kib = m_cost_kib;
            section.t_cost = t_cost;
            section.p_cost = p_cost;
        }
        KdfSpec::Scrypt { log_n, r, p } => {
            section.log_n = log_n;
            section.r = r;
            section.p_cost = p;
        }
        KdfSpec::Pbkdf2 { iterations, .. } => section.iterations = iterations,
        KdfSpec::BcryptPbkdf { rounds } => section.rounds = rounds,
        KdfSpec::Hkdf { .. } | KdfSpec::Blake3 => {}
    }
    section
}

fn kdf_spec_from_section(section: &KdfSection) -> Option<KdfSpec> {
    // from_name gives the variant with default parameters; the stored
    // parameters (if any) override them.
    let spec = KdfSpec::from_name(&section.algorithm)?;
    Some(match spec {
        KdfSpec::Argon2 { variant, .. } => KdfSpec::Argon2 {
            variant,
            m_cost_kib: section.m_cost_kib,
            t_cost: section.t_cost,
            p_cost: section.p_cost,
        },
        KdfSpec::Scrypt { .. } => KdfSpec::Scrypt {
            log_n: section.log_n,
            r: section.r,
            p: section.p_cost,
        },
        KdfSpec::Pbkdf2 { hash, .. } => KdfSpec::Pbkdf2 {
            hash,
            iterations: section.iterations,
        },
        KdfSpec::BcryptPbkdf { .. } => KdfSpec::BcryptPbkdf {
            rounds: section.rounds,
        },
        plain @ (KdfSpec::Hkdf { .. } | KdfSpec::Blake3) => plain,
    })
}

#[derive(Serialize, Deserialize)]
struct VaultFileRepr {
    version: u32,
    username: String,
    algorithm: String,
    style: String,
    kdf: KdfSection,
    nonce: String,
    data: String,
}

/// Why a vault could not be unlocked.
#[derive(Debug)]
pub enum UnlockError {
    WrongPassword,
    Corrupted(String),
}

impl std::fmt::Display for UnlockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UnlockError::WrongPassword => write!(f, "wrong password"),
            UnlockError::Corrupted(msg) => write!(f, "vault file unreadable: {msg}"),
        }
    }
}

impl std::error::Error for UnlockError {}

/// An unlocked, in-memory vault. The derived key lives here (zeroized on
/// drop); the master password itself is never retained.
pub struct Vault {
    pub path: PathBuf,
    pub username: String,
    pub algorithm: Algorithm,
    pub style: String,
    pub kdf: KdfSpec,
    salt: Vec<u8>,
    key: Zeroizing<[u8; KEY_LEN]>,
    /// The decrypted DCS-1 content (items + custom categories).
    pub content: VaultContent,
    /// The file-format version this vault was opened from (3 or 4). A v3 vault
    /// is upgraded to [`FORMAT_VERSION`] on the first `save()`; EncInfo surfaces
    /// this ("upgrades on next change").
    pub opened_format: u32,
}

impl Vault {
    pub fn create(
        dir: &Path,
        username: &str,
        password: &str,
        algorithm: Algorithm,
        kdf: KdfSpec,
        style: &str,
    ) -> Result<Vault> {
        let path = vault_path(dir, username);
        if path.exists() {
            bail!("a vault named {username} already exists");
        }
        let salt = crypto::random_bytes(SALT_LEN);
        let key = crypto::derive_key(password, &salt, &kdf)?;
        let vault = Vault {
            path,
            username: username.to_string(),
            algorithm,
            style: style.to_string(),
            kdf,
            salt,
            key,
            content: VaultContent::default(),
            opened_format: FORMAT_VERSION,
        };
        vault.save()?;
        Ok(vault)
    }

    pub fn open(dir: &Path, username: &str, password: &str) -> Result<Vault, UnlockError> {
        Vault::open_file(&vault_path(dir, username), password)
    }

    /// Opens (and verifies the password of) a vault at an explicit file path,
    /// regardless of the data directory. Used by import, which reads a vault
    /// from anywhere on disk before homing it into the data directory.
    pub fn open_file(path: &Path, password: &str) -> Result<Vault, UnlockError> {
        let repr = read_repr(path).map_err(|e| UnlockError::Corrupted(e.to_string()))?;

        if !READABLE_VERSIONS.contains(&repr.version) {
            // a version ABOVE what we know means a newer DataShield wrote it;
            // never best-effort parse (dropped members could lose secrets).
            let msg = if repr.version > FORMAT_VERSION {
                format!("vault written by a newer DataShield (format {}) — update the app", repr.version)
            } else {
                format!("unsupported vault format version {}", repr.version)
            };
            return Err(UnlockError::Corrupted(msg));
        }
        // an empty username would home to `<dir>/.vault` (an invisible dotfile)
        // and can't be produced by the create path — treat it as corruption.
        if repr.username.is_empty() {
            return Err(UnlockError::Corrupted("vault has an empty username".into()));
        }
        let algorithm = Algorithm::from_name(&repr.algorithm).ok_or_else(|| {
            UnlockError::Corrupted(format!("unknown algorithm {:?}", repr.algorithm))
        })?;
        let kdf = kdf_spec_from_section(&repr.kdf).ok_or_else(|| {
            UnlockError::Corrupted(format!("unknown KDF {:?}", repr.kdf.algorithm))
        })?;
        let salt = B64
            .decode(&repr.kdf.salt)
            .map_err(|e| UnlockError::Corrupted(format!("bad salt encoding: {e}")))?;
        let nonce = B64
            .decode(&repr.nonce)
            .map_err(|e| UnlockError::Corrupted(format!("bad nonce encoding: {e}")))?;
        let ciphertext = B64
            .decode(&repr.data)
            .map_err(|e| UnlockError::Corrupted(format!("bad data encoding: {e}")))?;

        // Structural sanity checks BEFORE the expensive key derivation: a
        // crafted header must not be able to request terabytes of memory,
        // and structural corruption must be distinguishable from a wrong
        // password (cmd_delete_vault offers force-deletion only for
        // corrupted vaults).
        if !kdf.is_sane() {
            return Err(UnlockError::Corrupted(format!(
                "unreasonable KDF parameters ({})",
                kdf.canonical()
            )));
        }
        if !(8..=64).contains(&salt.len()) {
            return Err(UnlockError::Corrupted(format!("bad salt length {}", salt.len())));
        }
        if nonce.len() != algorithm.nonce_len() {
            return Err(UnlockError::Corrupted(format!(
                "bad nonce length {} for {}",
                nonce.len(),
                algorithm.name()
            )));
        }

        let key = crypto::derive_key(password, &salt, &kdf)
            .map_err(|e| UnlockError::Corrupted(e.to_string()))?;

        // The header is bound as AEAD associated data, exactly as the file's
        // OWN version wrote it (v3 blobs were sealed with the `datashield-v3`
        // prefix, so we must verify against `repr.version`, not FORMAT_VERSION).
        // AEAD tag failure means wrong password (or a tampered file/header —
        // indistinguishable by construction).
        let aad = header_aad(repr.version, &repr.username, &repr.algorithm, &kdf, &repr.kdf.salt);
        let plaintext = Zeroizing::new(
            crypto::decrypt(algorithm, &key, &nonce, &ciphertext, &aad)
                .map_err(|_| UnlockError::WrongPassword)?,
        );

        // The header version selects the parser (no content sniffing): v3 is a
        // bare array of legacy items, migrated in memory; v4 is a DCS-1 document.
        let content = match repr.version {
            3 => {
                let legacy: Vec<LegacyItem> = serde_json::from_slice(&plaintext)
                    .map_err(|e| UnlockError::Corrupted(format!("bad item data: {e}")))?;
                migrate_legacy(legacy)
            }
            _ => {
                let content: VaultContent = serde_json::from_slice(&plaintext)
                    .map_err(|e| UnlockError::Corrupted(format!("bad content: {e}")))?;
                if content.schema_version > crate::model::CONTENT_SCHEMA_VERSION {
                    return Err(UnlockError::Corrupted(format!(
                        "content created by a newer DataShield (schema {})",
                        content.schema_version
                    )));
                }
                content
            }
        };

        Ok(Vault {
            path: path.to_path_buf(),
            username: repr.username,
            algorithm,
            style: repr.style,
            kdf,
            salt,
            key,
            content,
            opened_format: repr.version,
        })
    }

    /// Re-encrypts the whole item list with a fresh random nonce and writes
    /// the file atomically and durably: private (0600) temp file with a
    /// per-process name, fsync, rename over the vault, fsync the directory.
    /// A clone of the in-memory key bytes — handed to a sync worker thread so it
    /// can decrypt a peer's blob off the render loop. Wrapped in `Zeroizing`, so
    /// it wipes when the thread's copy drops.
    pub fn key_material(&self) -> Zeroizing<[u8; KEY_LEN]> {
        Zeroizing::new(*self.key)
    }

    pub fn save(&self) -> Result<()> {
        use std::io::Write;

        // always write the current format (v4): a v3 vault is upgraded to DCS-1
        // on this first save — same salt/key, fresh nonce, v4 AAD.
        let plaintext = Zeroizing::new(serde_json::to_vec(&self.content)?);
        let salt_b64 = B64.encode(&self.salt);
        let aad = header_aad(FORMAT_VERSION, &self.username, self.algorithm.name(), &self.kdf, &salt_b64);
        let (nonce, ciphertext) = crypto::encrypt(self.algorithm, &self.key, &plaintext, &aad)?;
        let repr = VaultFileRepr {
            version: FORMAT_VERSION,
            username: self.username.clone(),
            algorithm: self.algorithm.name().to_string(),
            style: self.style.clone(),
            kdf: kdf_section(&self.kdf, salt_b64),
            nonce: B64.encode(&nonce),
            data: B64.encode(&ciphertext),
        };
        let json = serde_json::to_string_pretty(&repr)?;

        let dir = self
            .path
            .parent()
            .ok_or_else(|| anyhow!("vault path has no parent directory"))?;
        create_private_dir(dir)?;
        let file_name = self
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| anyhow!("bad vault file name"))?;
        let tmp = self
            .path
            .with_file_name(format!("{file_name}.{}.tmp", std::process::id()));

        let mut file =
            open_private_file(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        file.write_all(json.as_bytes())
            .with_context(|| format!("writing {}", tmp.display()))?;
        // fsync before rename: otherwise a power loss can commit the rename
        // while the data blocks are still unwritten, truncating the only
        // copy of the vault.
        file.sync_all()
            .with_context(|| format!("syncing {}", tmp.display()))?;
        drop(file);
        if let Err(e) = fs::rename(&tmp, &self.path) {
            let _ = fs::remove_file(&tmp);
            return Err(e).with_context(|| format!("saving {}", self.path.display()));
        }
        #[cfg(unix)]
        if let Ok(d) = fs::File::open(dir) {
            let _ = d.sync_all();
        }
        Ok(())
    }

    /// Constant-time check of a re-typed master password (used by nuke and
    /// by set_kdf before re-deriving the key).
    pub fn verify_password(&self, password: &str) -> bool {
        match crypto::derive_key(password, &self.salt, &self.kdf) {
            Ok(candidate) => candidate.ct_eq(self.key.as_ref()).into(),
            Err(_) => false,
        }
    }

    /// Toggles ★ on the item with `id` and persists it, restoring the in-memory
    /// flag if the write fails. Returns the NEW state. Deliberately does NOT bump
    /// `modified_at`: starring is display metadata, not a content edit.
    pub fn toggle_favorite(&mut self, id: &str) -> Result<bool> {
        let idx = self.find_by_id(id).ok_or_else(|| anyhow!("no item {id}"))?;
        self.content.items[idx].favorite = !self.content.items[idx].favorite;
        if let Err(e) = self.save() {
            self.content.items[idx].favorite = !self.content.items[idx].favorite;
            return Err(e);
        }
        Ok(self.content.items[idx].favorite)
    }

    pub fn set_algorithm(&mut self, algorithm: Algorithm) -> Result<()> {
        let previous = self.algorithm;
        self.algorithm = algorithm;
        if let Err(e) = self.save() {
            self.algorithm = previous;
            return Err(e);
        }
        Ok(())
    }

    /// Switches the one-way algorithm: fresh salt, new key derived from the
    /// (already verified) master password, vault re-encrypted.
    pub fn set_kdf(&mut self, kdf: KdfSpec, password: &str) -> Result<()> {
        let new_salt = crypto::random_bytes(SALT_LEN);
        let new_key = crypto::derive_key(password, &new_salt, &kdf)?;

        let prev_kdf = self.kdf;
        let prev_salt = std::mem::replace(&mut self.salt, new_salt);
        let prev_key = std::mem::replace(&mut self.key, new_key);
        self.kdf = kdf;

        if let Err(e) = self.save() {
            self.kdf = prev_kdf;
            self.salt = prev_salt;
            self.key = prev_key;
            return Err(e);
        }
        Ok(())
    }

    pub fn set_style(&mut self, style: &str) -> Result<()> {
        let previous = std::mem::replace(&mut self.style, style.to_string());
        if let Err(e) = self.save() {
            self.style = previous;
            return Err(e);
        }
        Ok(())
    }

    /// Re-keys the vault: fresh random salt → new key from the (already verified)
    /// master password, optionally switching the AEAD/KDF, then one atomic
    /// re-encrypt (save() supplies the fresh nonce). CONTRACT: the caller MUST
    /// have checked `verify_password(password)` first.
    pub fn reroll(&mut self, password: &str, algorithm: Algorithm, kdf: KdfSpec) -> Result<()> {
        let new_salt = crypto::random_bytes(SALT_LEN);
        let new_key = crypto::derive_key(password, &new_salt, &kdf)?;
        let prev_algorithm = self.algorithm;
        let prev_kdf = self.kdf;
        let prev_salt = std::mem::replace(&mut self.salt, new_salt);
        let prev_key = std::mem::replace(&mut self.key, new_key);
        self.algorithm = algorithm;
        self.kdf = kdf;
        if let Err(e) = self.save() {
            self.algorithm = prev_algorithm;
            self.kdf = prev_kdf;
            self.salt = prev_salt;
            self.key = prev_key; // the discarded new key (Zeroizing) wipes on drop
            return Err(e);
        }
        Ok(())
    }

    /// Sets a NEW master password on an already-unlocked vault: fresh random
    /// salt → new key from `new_password`, algorithm and KDF untouched, then one
    /// atomic re-encrypt (save() supplies the fresh nonce). CONTRACT: the caller
    /// must already have proven the CURRENT password — opening the vault is that
    /// proof, so `rekey_file` does exactly that.
    pub fn change_password(&mut self, new_password: &str) -> Result<()> {
        let new_salt = crypto::random_bytes(SALT_LEN);
        let new_key = crypto::derive_key(new_password, &new_salt, &self.kdf)?;
        let prev_salt = std::mem::replace(&mut self.salt, new_salt);
        let prev_key = std::mem::replace(&mut self.key, new_key);
        if let Err(e) = self.save() {
            self.salt = prev_salt;
            self.key = prev_key; // the discarded new key (Zeroizing) wipes on drop
            return Err(e);
        }
        Ok(())
    }

    /// The salt/key/kdf this handle is using right now, so ANOTHER handle on the
    /// same file can adopt them (see [`Vault::adopt_rekey`]) without paying the
    /// KDF again. The only door key material leaves a `Vault` through besides
    /// `key_material`; the fields themselves stay private.
    pub fn rekey_material(&self) -> RekeyMaterial {
        RekeyMaterial {
            salt: self.salt.clone(),
            key: Zeroizing::new(*self.key),
            kdf: self.kdf,
        }
    }

    /// Installs salt/key/kdf produced by a worker's [`Vault::change_password`]
    /// on this (same-file) handle: the file on disk was already rewritten by the
    /// worker, so nothing is saved here — this only stops the open handle from
    /// encrypting its next save under the retired key.
    pub fn adopt_rekey(&mut self, m: RekeyMaterial) {
        self.salt = m.salt;
        self.key = m.key;
        self.kdf = m.kdf;
        // the worker's save() wrote the current format, whatever we opened from
        self.opened_format = FORMAT_VERSION;
    }

    /// A read-only snapshot of the vault's NON-secret encryption metadata (for
    /// the `encInfo` command). Never exposes key material or field values.
    pub fn enc_info(&self) -> EncInfo {
        let disk = read_repr(&self.path).ok().map(|repr| {
            let ciphertext_len = B64.decode(&repr.data).map(|b| b.len()).unwrap_or(0);
            DiskEcho {
                nonce_b64: repr.nonce,
                nonce_len: self.algorithm.nonce_len(),
                ciphertext_len,
                file_len: std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0),
                on_disk_version: repr.version,
            }
        });
        EncInfo {
            algorithm: self.algorithm,
            kdf: self.kdf,
            salt_b64: B64.encode(&self.salt),
            salt_len: self.salt.len(),
            item_count: self.content.items.len(),
            category_count: self.content.categories.len(),
            opened_format: self.opened_format,
            disk,
        }
    }

    /// The vault's items (DCS-1). Prefer this over touching `content` directly.
    pub fn items(&self) -> &[Item] {
        &self.content.items
    }

    pub fn items_mut(&mut self) -> &mut Vec<Item> {
        &mut self.content.items
    }

    /// Index of the item with this id (the stable identity for edit/delete).
    pub fn find_by_id(&self, id: &str) -> Option<usize> {
        self.content.items.iter().position(|i| i.id == id)
    }

    /// Index of the first item whose label matches (case-insensitively) — used
    /// to resolve a one-shot command argument. Labels may duplicate.
    pub fn position_by_label(&self, label: &str) -> Option<usize> {
        self.content
            .items
            .iter()
            .position(|i| i.label.eq_ignore_ascii_case(label))
    }
}

/// Migrates a format-3 legacy item list into a DCS-1 [`VaultContent`]. The
/// `{name,email,password}` triple maps to the `login` category; the free-text
/// legacy timestamps are best-effort parsed and always preserved verbatim in
/// each item's `extra` map (`x_legacy_when_*`) so nothing is lost.
fn migrate_legacy(legacy: Vec<LegacyItem>) -> VaultContent {
    let items = legacy.into_iter().map(migrate_legacy_item).collect();
    VaultContent { items, ..VaultContent::default() }
}

fn migrate_legacy_item(old: LegacyItem) -> Item {
    let mut fields = Vec::new();
    if !old.email.is_empty() {
        fields.push(ItemField {
            key: "email".into(),
            ftype: FieldType::Email,
            label: None,
            value: SecretString::new(old.email),
            secret: false,
            extra: serde_json::Map::new(),
        });
    }
    if !old.password.is_empty() {
        fields.push(ItemField {
            key: "password".into(),
            ftype: FieldType::Password,
            label: None,
            value: SecretString::new(old.password),
            secret: true,
            extra: serde_json::Map::new(),
        });
    }
    let mut extra = serde_json::Map::new();
    let created_at = parse_legacy_ts(&old.when_created);
    if !old.when_created.is_empty() {
        extra.insert("x_legacy_when_created".into(), old.when_created.into());
    }
    // "Not already modified." is the legacy sentinel for "never modified".
    let modified_at = if old.when_modified == "Not already modified." {
        None
    } else {
        if !old.when_modified.is_empty() {
            extra.insert("x_legacy_when_modified".into(), old.when_modified.clone().into());
        }
        parse_legacy_ts(&old.when_modified)
    };
    Item {
        id: crate::model::new_uuid_v4(),
        category: "login".into(),
        label: old.name,
        fields,
        created_at,
        modified_at,
        favorite: false,
        extra,
    }
}

/// Strict best-effort parse of the ONE known legacy writer pattern
/// `Created on DD/MM/YYYY at HH:MM` into RFC 3339 UTC. Anything else → `None`
/// (the verbatim original is kept in `extra`, so a wrong guess loses nothing).
/// Caveat: legacy times were local wall-clock, migrated as-if-UTC — ordering is
/// preserved, the offset is unknowable.
fn parse_legacy_ts(s: &str) -> Option<String> {
    let rest = s.strip_prefix("Created on ")?;
    let (date, time) = rest.split_once(" at ")?;
    let mut d = date.split('/');
    let (dd, mm, yyyy) = (d.next()?, d.next()?, d.next()?);
    if d.next().is_some() {
        return None;
    }
    let (hh, min) = time.split_once(':')?;
    let dd: u32 = dd.parse().ok()?;
    let mm: u32 = mm.parse().ok()?;
    let yyyy: i64 = yyyy.parse().ok()?;
    let hh: u32 = hh.parse().ok()?;
    let min: u32 = min.parse().ok()?;
    if !(1..=31).contains(&dd) || !(1..=12).contains(&mm) || hh > 23 || min > 59 {
        return None;
    }
    Some(format!("{yyyy:04}-{mm:02}-{dd:02}T{hh:02}:{min:02}:00Z"))
}

/// Echo of the sealed header currently on disk (all plaintext by design).
pub struct DiskEcho {
    pub nonce_b64: String,
    pub nonce_len: usize,
    pub ciphertext_len: usize,
    pub file_len: u64,
    pub on_disk_version: u32,
}

/// A read-only snapshot of a vault's non-secret encryption metadata.
pub struct EncInfo {
    pub algorithm: Algorithm,
    pub kdf: KdfSpec,
    pub salt_b64: String,
    pub salt_len: usize,
    pub item_count: usize,
    pub category_count: usize,
    pub opened_format: u32,
    /// `None` only if the file is momentarily unreadable.
    pub disk: Option<DiskEcho>,
}

/// Vault file name: hex-encoded username, so any username is filesystem-safe.
pub fn vault_path(dir: &Path, username: &str) -> PathBuf {
    let hex: String = username.bytes().map(|b| format!("{b:02x}")).collect();
    dir.join(format!("{hex}.{VAULT_EXT}"))
}

pub fn vault_exists(dir: &Path, username: &str) -> bool {
    vault_path(dir, username).is_file()
}

pub fn delete_vault_file(dir: &Path, username: &str) -> Result<()> {
    fs::remove_file(vault_path(dir, username)).context("deleting vault file")
}

/// Public header info of a vault (readable without the password): the AEAD
/// algorithm and the KDF name. The item count is NOT here — it lives inside
/// the encrypted blob.
pub fn header_info(dir: &Path, username: &str) -> Option<(String, String)> {
    let repr = read_repr(&vault_path(dir, username)).ok()?;
    Some((repr.algorithm, repr.kdf.algorithm))
}

/// The vault file's last-modified time — the "age" column of the access list
/// (plaintext filesystem metadata; no password needed).
pub fn file_modified(dir: &Path, username: &str) -> Option<std::time::SystemTime> {
    fs::metadata(vault_path(dir, username)).ok().and_then(|m| m.modified().ok())
}

/// The salt/key/kdf a re-keyed vault hands back, so an OPEN handle on the same
/// file can adopt the new password without a second (slow) KDF pass. No
/// `Debug`/`Clone`: it carries live key bytes, wiped when it drops.
pub struct RekeyMaterial {
    salt: Vec<u8>,
    key: Zeroizing<[u8; KEY_LEN]>,
    kdf: KdfSpec,
}

/// What the change-password worker thread sends back to the UI.
pub enum RekeyOutcome {
    /// Re-encrypted and written; adopt this into an open handle of that vault.
    Done(RekeyMaterial),
    /// The typed CURRENT password didn't open the vault — nothing was written.
    WrongPassword,
    /// The file couldn't be read or rewritten (message is already readable).
    Failed(String),
}

/// Changes the master password of the vault file at `path`, off the render loop
/// (stateless — for the change-password worker thread). Opening it IS the
/// verification of `current` (and the expensive KDF pass); on success the whole
/// vault is re-encrypted under a fresh salt + key derived from `new_password`.
pub fn rekey_file(path: &Path, current: &str, new_password: &str) -> RekeyOutcome {
    let mut vault = match Vault::open_file(path, current) {
        Ok(v) => v,
        Err(UnlockError::WrongPassword) => return RekeyOutcome::WrongPassword,
        Err(UnlockError::Corrupted(msg)) => return RekeyOutcome::Failed(msg),
    };
    match vault.change_password(new_password) {
        Ok(()) => RekeyOutcome::Done(vault.rekey_material()),
        Err(e) => RekeyOutcome::Failed(e.to_string()),
    }
}

/// Decrypts a `.vault` blob at `path` with raw key bytes (stateless — for the
/// sync worker thread). `WrongPassword` = the key doesn't match (remote was
/// re-keyed elsewhere). Mirrors [`Vault::open_file`]'s crypto exactly.
pub fn decrypt_blob_with(path: &Path, key: &[u8; crypto::KEY_LEN]) -> Result<VaultContent, UnlockError> {
    let repr = read_repr(path).map_err(|e| UnlockError::Corrupted(e.to_string()))?;
    if !READABLE_VERSIONS.contains(&repr.version) {
        return Err(UnlockError::Corrupted(format!("unsupported format {}", repr.version)));
    }
    let algorithm = Algorithm::from_name(&repr.algorithm)
        .ok_or_else(|| UnlockError::Corrupted(format!("unknown algorithm {:?}", repr.algorithm)))?;
    let kdf = kdf_spec_from_section(&repr.kdf)
        .ok_or_else(|| UnlockError::Corrupted(format!("unknown KDF {:?}", repr.kdf.algorithm)))?;
    let nonce = B64.decode(&repr.nonce).map_err(|e| UnlockError::Corrupted(e.to_string()))?;
    let ciphertext = B64.decode(&repr.data).map_err(|e| UnlockError::Corrupted(e.to_string()))?;
    if nonce.len() != algorithm.nonce_len() {
        return Err(UnlockError::Corrupted("bad nonce length".into()));
    }
    let aad = header_aad(repr.version, &repr.username, &repr.algorithm, &kdf, &repr.kdf.salt);
    let plaintext = Zeroizing::new(
        crypto::decrypt(algorithm, key, &nonce, &ciphertext, &aad).map_err(|_| UnlockError::WrongPassword)?,
    );
    serde_json::from_slice(&plaintext).map_err(|e| UnlockError::Corrupted(e.to_string()))
}

/// The per-vault sync-config sidecar path (`<hex(username)>.sync.toml`), beside
/// the `.vault`. Non-secret (endpoint host/user/path only) — never a password.
pub fn sync_path(dir: &Path, username: &str) -> PathBuf {
    let hex: String = username.bytes().map(|b| format!("{b:02x}")).collect();
    dir.join(format!("{hex}.sync.toml"))
}

/// Usernames of every vault in the data directory (from plaintext metadata).
pub fn list_vaults(dir: &Path) -> Vec<String> {
    let mut names = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return names;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some(VAULT_EXT) {
            if let Ok(repr) = read_repr(&path) {
                names.push(repr.username);
            }
        }
    }
    names.sort();
    names
}

fn read_repr(path: &Path) -> Result<VaultFileRepr> {
    let json = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let repr: VaultFileRepr = serde_json::from_str(&json)
        .with_context(|| format!("parsing {}", path.display()))?;
    Ok(repr)
}

/// Canonical serialization of the plaintext header, bound into the AEAD tag
/// so header tampering is detected. `algorithm` and `salt_b64` are passed as
/// the exact strings that appear in the file.
///
/// The username is the only free-form field and may contain the `|`
/// delimiter, so it is length-prefixed (`u<len>:<username>`) to keep the
/// encoding unambiguous — no two distinct field-sets can produce the same
/// AAD string.
fn header_aad(version: u32, username: &str, algorithm: &str, kdf: &KdfSpec, salt_b64: &str) -> Vec<u8> {
    format!(
        "datashield-v{version}|u{}:{username}|{algorithm}|{}|{salt_b64}",
        username.len(),
        kdf.canonical()
    )
    .into_bytes()
}

/// mkdir -p with 0700 on Unix (default permissions elsewhere).
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

/// Creates/truncates a file readable only by the owner on Unix (0600); the
/// mode survives the rename onto the final vault path.
fn open_private_file(path: &Path) -> std::io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn field(key: &str, ftype: FieldType, value: &str, secret: bool) -> ItemField {
        ItemField {
            key: key.to_string(),
            ftype,
            label: None,
            value: SecretString::new(value),
            secret,
            extra: serde_json::Map::new(),
        }
    }

    fn item(name: &str) -> Item {
        let mut it = Item::new("login", name);
        it.fields.push(field("email", FieldType::Email, &format!("{name}@example.com"), false));
        it.fields.push(field("password", FieldType::Password, "S3greta!", true));
        it
    }

    /// Same algorithm, minimum sane work parameters — keeps the suite fast.
    fn fast_spec(spec: KdfSpec) -> KdfSpec {
        match spec {
            KdfSpec::Argon2 { variant, .. } => {
                KdfSpec::Argon2 { variant, m_cost_kib: 8, t_cost: 1, p_cost: 1 }
            }
            KdfSpec::Scrypt { .. } => KdfSpec::Scrypt { log_n: 10, r: 1, p: 1 },
            KdfSpec::Pbkdf2 { hash, .. } => KdfSpec::Pbkdf2 { hash, iterations: 1_000 },
            KdfSpec::BcryptPbkdf { .. } => KdfSpec::BcryptPbkdf { rounds: 4 },
            other => other,
        }
    }

    fn fast_kdf() -> KdfSpec {
        fast_spec(KdfSpec::default())
    }

    /// Writes a genuine format-3 (legacy) vault file: `legacy_json` is a bare
    /// JSON array of {name,email,password,when_created,when_modified}, sealed
    /// with the v3 AAD prefix exactly as the old app would have.
    fn write_legacy_v3(
        dir: &Path,
        username: &str,
        password: &str,
        algorithm: Algorithm,
        kdf: KdfSpec,
        legacy_json: &str,
    ) -> PathBuf {
        let salt = crypto::random_bytes(SALT_LEN);
        let key = crypto::derive_key(password, &salt, &kdf).unwrap();
        let salt_b64 = B64.encode(&salt);
        let aad = header_aad(3, username, algorithm.name(), &kdf, &salt_b64);
        let (nonce, ct) = crypto::encrypt(algorithm, &key, legacy_json.as_bytes(), &aad).unwrap();
        let repr = VaultFileRepr {
            version: 3,
            username: username.to_string(),
            algorithm: algorithm.name().to_string(),
            style: "Vanilla".to_string(),
            kdf: kdf_section(&kdf, salt_b64),
            nonce: B64.encode(&nonce),
            data: B64.encode(&ct),
        };
        create_private_dir(dir).unwrap();
        let path = vault_path(dir, username);
        fs::write(&path, serde_json::to_string_pretty(&repr).unwrap()).unwrap();
        path
    }

    fn on_disk_version(path: &Path) -> u32 {
        let v: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        v["version"].as_u64().unwrap() as u32
    }

    #[test]
    fn create_open_roundtrip_with_items() {
        let dir = TempDir::new().unwrap();
        let mut v = Vault::create(dir.path(), "bruno", "pw", Algorithm::Aes256Gcm, fast_kdf(), "Debian").unwrap();
        v.items_mut().push(item("Gmail"));
        v.items_mut().push(item("Steam"));
        v.save().unwrap();

        let v2 = Vault::open(dir.path(), "bruno", "pw").unwrap();
        assert_eq!(v2.username, "bruno");
        assert_eq!(v2.algorithm, Algorithm::Aes256Gcm);
        assert_eq!(v2.style, "Debian");
        assert_eq!(v2.items().len(), 2);
        assert_eq!(v2.items()[0].label, "Gmail");
        let email = v2.items()[0].fields.iter().find(|f| f.key == "email").unwrap();
        assert_eq!(email.value.as_str(), "Gmail@example.com");
    }

    #[test]
    fn v3_legacy_vault_migrates_to_dcs1_login_items() {
        let dir = TempDir::new().unwrap();
        let legacy = r#"[
          {"name":"Gmail","email":"g@example.com","password":"pw1","when_created":"Created on 01/02/2026 at 10:00","when_modified":"Not already modified."},
          {"name":"Bank","email":"","password":"pw2","when_created":"garbage","when_modified":"Created on 03/04/2026 at 08:15"}
        ]"#;
        write_legacy_v3(dir.path(), "bruno", "pw", Algorithm::Aes256Gcm, fast_kdf(), legacy);

        let v = Vault::open(dir.path(), "bruno", "pw").unwrap();
        assert_eq!(v.opened_format, 3, "opened from a v3 file");
        assert_eq!(v.items().len(), 2);

        let g = &v.items()[0];
        assert_eq!(g.label, "Gmail");
        assert_eq!(g.category, "login", "the {{name,email,password}} triple is the Login preset");
        assert_eq!(g.created_at.as_deref(), Some("2026-02-01T10:00:00Z"), "DD/MM/YYYY parsed");
        assert_eq!(g.modified_at, None, "the sentinel means never modified");
        assert!(g.extra.contains_key("x_legacy_when_created"), "original preserved verbatim");
        let email = g.fields.iter().find(|f| f.key == "email").unwrap();
        assert_eq!(email.value.as_str(), "g@example.com");
        assert!(!email.secret);
        let pw = g.fields.iter().find(|f| f.key == "password").unwrap();
        assert!(pw.secret, "password field is secret");

        let b = &v.items()[1];
        assert!(b.fields.iter().all(|f| f.key != "email"), "empty email is omitted");
        assert_eq!(b.created_at, None, "garbage timestamp becomes None");
        assert_eq!(b.modified_at.as_deref(), Some("2026-04-03T08:15:00Z"));
    }

    #[test]
    fn opening_v3_does_not_rewrite_it_but_saving_upgrades_to_v4() {
        let dir = TempDir::new().unwrap();
        let legacy = r#"[{"name":"Gmail","email":"g@x.com","password":"pw1","when_created":"","when_modified":"Not already modified."}]"#;
        let path =
            write_legacy_v3(dir.path(), "bruno", "pw", Algorithm::Aes256Gcm, fast_kdf(), legacy);

        // opening a v3 vault must not touch the file (an older binary can still
        // read it, and export-before-edit yields a v3 file).
        let before = fs::read_to_string(&path).unwrap();
        let mut v = Vault::open(dir.path(), "bruno", "pw").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), before, "open must not rewrite");
        assert_eq!(on_disk_version(&path), 3);

        // the first mutating save upgrades it to v4 (same password reopens it).
        v.items_mut().push(item("Steam"));
        v.save().unwrap();
        assert_eq!(on_disk_version(&path), 4, "upgraded on first save");
        let v2 = Vault::open(dir.path(), "bruno", "pw").unwrap();
        assert_eq!(v2.opened_format, 4);
        assert_eq!(v2.items().len(), 2);
        assert_eq!(v2.items()[0].label, "Gmail");
    }

    #[test]
    fn v4_roundtrips_items_and_custom_categories() {
        use crate::model::CategoryDef;
        let dir = TempDir::new().unwrap();
        let mut v =
            Vault::create(dir.path(), "bruno", "pw", Algorithm::Aes256Gcm, fast_kdf(), "Vanilla").unwrap();
        v.content.categories.push(CategoryDef {
            id: "streaming".into(),
            name: "Streaming".into(),
            fields: Vec::new(),
            created_at: None,
            extra: serde_json::Map::new(),
        });
        let mut it = Item::new("streaming", "Netflix");
        it.fields.push(field("profile", FieldType::Text, "kids", false));
        v.items_mut().push(it);
        v.save().unwrap();

        let v2 = Vault::open(dir.path(), "bruno", "pw").unwrap();
        assert_eq!(v2.opened_format, 4);
        assert_eq!(v2.content.categories.len(), 1);
        assert_eq!(v2.content.categories[0].id, "streaming");
        assert_eq!(v2.items()[0].category, "streaming");
        assert_eq!(v2.items()[0].fields[0].value.as_str(), "kids");
    }

    #[test]
    fn favorite_round_trips_and_is_only_serialized_when_set() {
        let dir = TempDir::new().unwrap();
        let mut v = Vault::create(dir.path(), "bruno", "pw", Algorithm::Aes256Gcm, fast_kdf(), "Vanilla").unwrap();
        v.items_mut().push(item("Gmail"));
        v.items_mut().push(item("Steam"));
        let id = v.items()[0].id.clone();
        assert!(v.toggle_favorite(&id).unwrap(), "toggled on");

        let raw = fs::read_to_string(vault_path(dir.path(), "bruno")).unwrap();
        // the field never leaks in clear (it's inside the AEAD blob)
        assert!(!raw.contains("favorite"));
        let mut v2 = Vault::open(dir.path(), "bruno", "pw").unwrap();
        assert!(v2.items()[0].favorite, "starred item persisted");
        assert!(!v2.items()[1].favorite, "unstarred item stays false");

        // toggle_favorite on a bad id errors without saving
        assert!(v2.toggle_favorite("nope").is_err());
    }

    #[test]
    fn reroll_changes_salt_and_nonce_and_reopens() {
        let dir = TempDir::new().unwrap();
        let mut v = Vault::create(dir.path(), "bruno", "pw", Algorithm::Aes256Gcm, fast_kdf(), "Vanilla").unwrap();
        v.items_mut().push(item("Gmail"));
        v.save().unwrap();
        let before: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(vault_path(dir.path(), "bruno")).unwrap()).unwrap();

        v.reroll("pw", Algorithm::ChaCha20Poly1305, fast_kdf()).unwrap();
        let after: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(vault_path(dir.path(), "bruno")).unwrap()).unwrap();
        assert_ne!(before["kdf"]["salt"], after["kdf"]["salt"], "salt is fresh");
        assert_ne!(before["nonce"], after["nonce"], "nonce is fresh");
        assert_eq!(after["algorithm"], "ChaCha20-Poly1305");

        let v2 = Vault::open(dir.path(), "bruno", "pw").unwrap();
        assert_eq!(v2.algorithm, Algorithm::ChaCha20Poly1305);
        assert_eq!(v2.items()[0].label, "Gmail");
    }

    #[test]
    fn change_password_rekeys_and_retires_the_old_one() {
        let dir = TempDir::new().unwrap();
        let mut v = Vault::create(dir.path(), "bruno", "pw", Algorithm::Aes256Gcm, fast_kdf(), "Vanilla").unwrap();
        v.items_mut().push(item("Gmail"));
        v.save().unwrap();
        let before: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(vault_path(dir.path(), "bruno")).unwrap()).unwrap();

        v.change_password("nuova").unwrap();
        let after: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(vault_path(dir.path(), "bruno")).unwrap()).unwrap();
        assert_ne!(before["kdf"]["salt"], after["kdf"]["salt"], "salt is fresh");
        assert_ne!(before["nonce"], after["nonce"], "nonce is fresh");
        // the primitives are deliberately untouched (this is not a reroll)
        assert_eq!(before["algorithm"], after["algorithm"]);
        assert_eq!(before["kdf"]["algorithm"], after["kdf"]["algorithm"]);

        let v2 = Vault::open(dir.path(), "bruno", "nuova").expect("new password opens it");
        assert_eq!(v2.items()[0].label, "Gmail");
        match Vault::open(dir.path(), "bruno", "pw") {
            Err(UnlockError::WrongPassword) => {}
            other => panic!("old password must be retired, got {:?}", other.err()),
        }
        // the handle that changed it keeps working (its key was swapped in place)
        assert!(v.verify_password("nuova"));
    }

    #[test]
    fn rekey_file_reports_a_wrong_current_password_without_touching_the_file() {
        let dir = TempDir::new().unwrap();
        Vault::create(dir.path(), "bruno", "pw", Algorithm::Aes256Gcm, fast_kdf(), "Vanilla").unwrap();
        let path = vault_path(dir.path(), "bruno");
        let before = fs::read_to_string(&path).unwrap();
        match rekey_file(&path, "sbagliata", "nuova") {
            RekeyOutcome::WrongPassword => {}
            _ => panic!("expected WrongPassword"),
        }
        assert_eq!(before, fs::read_to_string(&path).unwrap(), "the file is untouched");
        // …and with the right one the material comes back for the open handle
        match rekey_file(&path, "pw", "nuova") {
            RekeyOutcome::Done(m) => {
                let mut open = Vault::open(dir.path(), "bruno", "nuova").unwrap();
                open.adopt_rekey(m);
                assert!(open.verify_password("nuova"));
            }
            _ => panic!("expected Done"),
        }
    }

    #[test]
    fn a_newer_format_version_is_refused_not_wrong_password() {
        let dir = TempDir::new().unwrap();
        Vault::create(dir.path(), "bruno", "pw", Algorithm::Aes256Gcm, fast_kdf(), "Vanilla").unwrap();
        let path = vault_path(dir.path(), "bruno");
        let mut repr: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        repr["version"] = serde_json::Value::from(5);
        fs::write(&path, serde_json::to_string(&repr).unwrap()).unwrap();
        match Vault::open(dir.path(), "bruno", "pw") {
            Err(UnlockError::Corrupted(m)) => assert!(m.contains("newer"), "got: {m}"),
            other => panic!("expected Corrupted, got {:?}", other.err()),
        }
    }

    #[test]
    fn wrong_password_is_rejected() {
        let dir = TempDir::new().unwrap();
        Vault::create(dir.path(), "bruno", "pw", Algorithm::ChaCha20Poly1305, fast_kdf(), "Vanilla").unwrap();
        match Vault::open(dir.path(), "bruno", "sbagliata") {
            Err(UnlockError::WrongPassword) => {}
            other => panic!("expected WrongPassword, got {other:?}", other = other.err()),
        }
    }

    #[test]
    fn tampered_file_is_rejected() {
        let dir = TempDir::new().unwrap();
        let mut v =
            Vault::create(dir.path(), "bruno", "pw", Algorithm::XChaCha20Poly1305, fast_kdf(), "Vanilla").unwrap();
        v.items_mut().push(item("Gmail"));
        v.save().unwrap();

        let path = vault_path(dir.path(), "bruno");
        let json = fs::read_to_string(&path).unwrap();
        let mut repr: serde_json::Value = serde_json::from_str(&json).unwrap();
        let data = repr["data"].as_str().unwrap();
        let mut raw = B64.decode(data).unwrap();
        raw[0] ^= 1;
        repr["data"] = serde_json::Value::String(B64.encode(&raw));
        fs::write(&path, serde_json::to_string(&repr).unwrap()).unwrap();

        assert!(Vault::open(dir.path(), "bruno", "pw").is_err());
    }

    #[test]
    fn switching_algorithm_reencrypts_and_reopens() {
        let dir = TempDir::new().unwrap();
        let mut v = Vault::create(dir.path(), "bruno", "pw", Algorithm::Aes256Gcm, fast_kdf(), "Vanilla").unwrap();
        v.items_mut().push(item("Gmail"));
        v.save().unwrap();

        v.set_algorithm(Algorithm::XChaCha20Poly1305).unwrap();

        let v2 = Vault::open(dir.path(), "bruno", "pw").unwrap();
        assert_eq!(v2.algorithm, Algorithm::XChaCha20Poly1305);
        assert_eq!(v2.items().len(), 1);
        assert_eq!(v2.items()[0].label, "Gmail");
    }

    #[test]
    fn structural_corruption_is_reported_as_corrupted_not_wrong_password() {
        let dir = TempDir::new().unwrap();
        Vault::create(dir.path(), "bruno", "pw", Algorithm::Aes256Gcm, fast_kdf(), "Vanilla").unwrap();
        let path = vault_path(dir.path(), "bruno");
        let mut repr: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();

        // Truncated nonce → structural corruption, must NOT read as wrong password
        let good_nonce = repr["nonce"].as_str().unwrap().to_string();
        repr["nonce"] = serde_json::Value::String(B64.encode([0u8; 3]));
        fs::write(&path, serde_json::to_string(&repr).unwrap()).unwrap();
        match Vault::open(dir.path(), "bruno", "pw") {
            Err(UnlockError::Corrupted(msg)) => assert!(msg.contains("nonce")),
            other => panic!("expected Corrupted, got {:?}", other.err()),
        }

        // Crafted KDF params must be rejected before any derivation happens
        repr["nonce"] = serde_json::Value::String(good_nonce);
        repr["kdf"]["m_cost_kib"] = serde_json::Value::from(u32::MAX);
        fs::write(&path, serde_json::to_string(&repr).unwrap()).unwrap();
        match Vault::open(dir.path(), "bruno", "pw") {
            Err(UnlockError::Corrupted(msg)) => assert!(msg.contains("KDF parameters")),
            other => panic!("expected Corrupted, got {:?}", other.err()),
        }
    }

    #[test]
    fn username_with_delimiter_roundtrips() {
        // A '|' in the username must not create an AAD ambiguity: the vault
        // still opens with the right password and rejects the wrong one.
        let dir = TempDir::new().unwrap();
        let mut v = Vault::create(
            dir.path(),
            "a|AES-128-GCM|x",
            "pw",
            Algorithm::Aes256Gcm,
            fast_kdf(),
            "Vanilla",
        )
        .unwrap();
        v.items_mut().push(item("Gmail"));
        v.save().unwrap();

        let v2 = Vault::open(dir.path(), "a|AES-128-GCM|x", "pw").unwrap();
        assert_eq!(v2.items().len(), 1);
        assert!(matches!(
            Vault::open(dir.path(), "a|AES-128-GCM|x", "no"),
            Err(UnlockError::WrongPassword)
        ));
    }

    #[test]
    fn open_file_rejects_an_empty_username() {
        // a hand-edited vault with a blank username must be refused, not homed
        // to an invisible `.vault` dotfile on import.
        let dir = TempDir::new().unwrap();
        Vault::create(dir.path(), "bruno", "pw", Algorithm::Aes256Gcm, fast_kdf(), "Vanilla").unwrap();
        let path = vault_path(dir.path(), "bruno");
        let mut repr: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        repr["username"] = serde_json::Value::String(String::new());
        fs::write(&path, serde_json::to_string(&repr).unwrap()).unwrap();
        match Vault::open_file(&path, "pw") {
            Err(UnlockError::Corrupted(msg)) => assert!(msg.contains("username")),
            other => panic!("expected Corrupted, got {:?}", other.err()),
        }
    }

    #[test]
    fn header_tampering_is_detected_via_aad() {
        let dir = TempDir::new().unwrap();
        let mut v = Vault::create(dir.path(), "bruno", "pw", Algorithm::Aes256Gcm, fast_kdf(), "Vanilla").unwrap();
        v.items_mut().push(item("Gmail"));
        v.save().unwrap();

        let path = vault_path(dir.path(), "bruno");
        let mut repr: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        repr["username"] = serde_json::Value::String("mallory".to_string());
        fs::write(&path, serde_json::to_string(&repr).unwrap()).unwrap();

        // Correct password, same file — but the tampered header breaks the
        // AEAD tag, so the vault refuses to open.
        assert!(Vault::open(dir.path(), "bruno", "pw").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn vault_file_is_owner_only_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        Vault::create(dir.path(), "bruno", "pw", Algorithm::Aes256Gcm, fast_kdf(), "Vanilla").unwrap();
        let mode = fs::metadata(vault_path(dir.path(), "bruno"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "vault file mode is {mode:o}");
    }

    #[test]
    fn every_kdf_creates_and_reopens_a_vault() {
        let dir = TempDir::new().unwrap();
        for (i, spec) in KdfSpec::ALL.iter().enumerate() {
            let spec = fast_spec(*spec);
            let user = format!("user{i}");
            let mut v =
                Vault::create(dir.path(), &user, "pw", Algorithm::Aes256Gcm, spec, "Vanilla")
                    .unwrap();
            v.items_mut().push(item("Gmail"));
            v.save().unwrap();

            let v2 = Vault::open(dir.path(), &user, "pw")
                .unwrap_or_else(|e| panic!("{}: {e}", spec.name()));
            assert_eq!(v2.kdf, spec, "{}", spec.name());
            assert_eq!(v2.items().len(), 1, "{}", spec.name());
            assert!(
                matches!(Vault::open(dir.path(), &user, "no"), Err(UnlockError::WrongPassword)),
                "{}",
                spec.name()
            );
        }
    }

    #[test]
    fn switching_kdf_reencrypts_and_reopens() {
        let dir = TempDir::new().unwrap();
        let mut v =
            Vault::create(dir.path(), "kdf", "pw", Algorithm::Aes256Gcm, fast_kdf(), "Vanilla")
                .unwrap();
        v.items_mut().push(item("Gmail"));
        v.save().unwrap();

        let md5 = KdfSpec::from_name("MD5").unwrap();
        v.set_kdf(md5, "pw").unwrap();
        assert!(v.verify_password("pw"));

        let v2 = Vault::open(dir.path(), "kdf", "pw").unwrap();
        assert_eq!(v2.kdf.name(), "MD5");
        assert_eq!(v2.items().len(), 1);
        assert!(matches!(
            Vault::open(dir.path(), "kdf", "sbagliata"),
            Err(UnlockError::WrongPassword)
        ));
    }

    #[test]
    fn verify_password_works() {
        let dir = TempDir::new().unwrap();
        let v = Vault::create(dir.path(), "bruno", "pw", Algorithm::Aes256Gcm, fast_kdf(), "Vanilla").unwrap();
        assert!(v.verify_password("pw"));
        assert!(!v.verify_password("altra"));
    }

    #[test]
    fn list_and_delete_vaults() {
        let dir = TempDir::new().unwrap();
        Vault::create(dir.path(), "anna", "a", Algorithm::Aes256Gcm, fast_kdf(), "Vanilla").unwrap();
        Vault::create(dir.path(), "bruno", "b", Algorithm::ChaCha20Poly1305, fast_kdf(), "Debian").unwrap();
        assert_eq!(list_vaults(dir.path()), vec!["anna", "bruno"]);
        assert!(vault_exists(dir.path(), "anna"));

        delete_vault_file(dir.path(), "anna").unwrap();
        assert_eq!(list_vaults(dir.path()), vec!["bruno"]);
        assert!(!vault_exists(dir.path(), "anna"));
    }
}
