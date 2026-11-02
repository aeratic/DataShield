//! Per-vault sync to a user-owned VPS over SFTP-on-SSH.
//!
//! A vault is already ONE end-to-end-encrypted AEAD blob, so sync is dumb:
//! push/pull those opaque bytes. The server only ever sees ciphertext — the
//! master key never leaves the machine. We shell out to the system `ssh`/`sftp`
//! (public-key auth via the user's ssh-agent + `~/.ssh/config` + `known_hosts`)
//! so there is no server software, no stored password, and no native SSH dep.
//!
//! Version ordering (which side is newer, and rollback detection) uses the
//! monotonic counter stored INSIDE the encrypted content (`VaultContent`), so
//! the server can neither read nor forge it. The local "last synced" baseline
//! lives in a cleartext sidecar (`<hex>.sync.toml`) that holds only non-secret
//! endpoint config + the baseline counter — never a password or key bytes.

use crate::vault;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

/// SSH options shared by every invocation: key auth only (never hang on a
/// hidden password prompt), pin the host key on first contact, fail fast.
const SSH_OPTS: &[&str] = &[
    "-o", "BatchMode=yes",
    "-o", "PasswordAuthentication=no",
    "-o", "ConnectTimeout=8",
    "-o", "StrictHostKeyChecking=accept-new",
];

/// The non-secret per-vault sync config + local baseline (the sidecar file).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SyncConfig {
    pub host: String,
    pub user: String,
    /// Remote directory (one file per vault lives here). Default `~/.datashield`.
    #[serde(default = "default_remote_dir")]
    pub remote_dir: String,
    /// Explicit SSH key path; empty = use the ssh-agent / `~/.ssh/config`.
    #[serde(default)]
    pub key_path: String,
    /// Push automatically after each save + pull on unlock.
    #[serde(default)]
    pub auto: bool,
    /// RFC-3339 of the last successful sync (display only).
    #[serde(default)]
    pub last_synced: Option<String>,
    /// The counter we last agreed with the server on — the baseline the
    /// rollback/divergence check compares against. Local-only, never uploaded.
    #[serde(default)]
    pub base_counter: u64,
    /// Set when this vault's master password was changed since the last sync:
    /// the blob still on the server is the RETIRED key's copy, so it can no
    /// longer be decrypted here — and leaving it there would keep the whole
    /// vault readable with the password the user just retired. The next sync
    /// replaces it wholesale (see [`run_open_vault_sync`]) and clears this.
    #[serde(default)]
    pub rekeyed: bool,
}

fn default_remote_dir() -> String {
    "~/.datashield".to_string()
}

impl SyncConfig {
    /// Loads the sidecar for `username`, or `None` if sync isn't configured.
    pub fn load(dir: &Path, username: &str) -> Option<SyncConfig> {
        let text = std::fs::read_to_string(vault::sync_path(dir, username)).ok()?;
        toml::from_str(&text).ok()
    }

    /// True when a sidecar exists (sync is configured for this vault).
    pub fn exists(dir: &Path, username: &str) -> bool {
        vault::sync_path(dir, username).is_file()
    }

    /// Writes the sidecar (owner-only). Non-secret content, but kept 0600 for
    /// tidiness alongside the vault files.
    pub fn save(&self, dir: &Path, username: &str) -> std::io::Result<()> {
        let path = vault::sync_path(dir, username);
        let body = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(&path, format!("# DataShield sync (endpoint only — no secrets)\n{body}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    /// `user@host` for ssh/sftp.
    fn target(&self) -> String {
        format!("{}@{}", self.user, self.host)
    }

    /// `~/.datashield/<hex(username)>.vault` — the remote path for this vault.
    fn remote_blob(&self, username: &str) -> String {
        let hex: String = username.bytes().map(|b| format!("{b:02x}")).collect();
        format!("{}/{hex}.vault", self.remote_dir.trim_end_matches('/'))
    }

    /// The `-i <key>` args, if an explicit key was set.
    fn key_args(&self) -> Vec<String> {
        if self.key_path.trim().is_empty() {
            Vec::new()
        } else {
            vec!["-i".to_string(), self.key_path.clone()]
        }
    }
}

/// The status glyph shown per-vault in the top-bar / access list.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SyncStatus {
    /// No sidecar — this vault doesn't sync.
    #[default]
    Unconfigured,
    /// Local and remote agree.
    Synced,
    /// A push/pull is in flight.
    Syncing,
    /// Both sides moved independently — needs the user's choice.
    Diverged,
    /// Last attempt failed / host unreachable.
    Offline,
}

/// The result of one sync attempt (sent back over the channel to the UI).
#[derive(Clone, Debug)]
pub enum SyncOutcome {
    /// Local was newer; the remote now matches (counter uploaded).
    Pushed,
    /// Remote was newer; the local blob was replaced (verified first).
    Pulled(u64),
    /// Nothing to do.
    UpToDate,
    /// Both sides changed since the baseline — no automatic resolution.
    Diverged { local: u64, remote: u64 },
    /// A transport/verification error (host down, no key, tampered blob, …).
    Failed(String),
}

/// The three-way version decision (pure — unit-testable without any IO).
/// `remote` is `None` when the server has no blob yet (first push).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SyncDecision {
    Push,
    Pull,
    UpToDate,
    Diverged,
    /// The server serves a strictly OLDER blob than our baseline — a rollback.
    Rollback,
}

/// Decides push/pull/diverge from the counters. `local` = our current content
/// counter, `remote` = the peer blob's counter (None = absent), `base` = the
/// counter we last synced to.
pub fn decide(local: u64, remote: Option<u64>, base: u64) -> SyncDecision {
    let Some(remote) = remote else {
        return SyncDecision::Push; // server has nothing yet
    };
    if remote == local {
        return SyncDecision::UpToDate;
    }
    if remote < base {
        return SyncDecision::Rollback; // server replaying an old blob
    }
    let local_moved = local > base;
    let remote_moved = remote > base;
    match (local_moved, remote_moved) {
        (true, false) => SyncDecision::Push,   // only we advanced
        (false, true) => SyncDecision::Pull,   // only the server advanced
        (true, true) => SyncDecision::Diverged, // both advanced independently
        (false, false) => {
            // neither advanced past base yet the counters differ — treat the
            // higher one as authoritative (fast-forward), never lose data
            if local > remote {
                SyncDecision::Push
            } else {
                SyncDecision::Pull
            }
        }
    }
}

/// Probes reachability + that the account/key works (`ssh … true`). Returns an
/// actionable error message on failure.
fn probe(cfg: &SyncConfig) -> Result<(), String> {
    let mut cmd = Command::new("ssh");
    cmd.args(SSH_OPTS).args(cfg.key_args()).arg(cfg.target()).arg("true");
    match cmd.output() {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr);
            let first = err.lines().find(|l| !l.trim().is_empty()).unwrap_or("ssh failed");
            Err(classify(first))
        }
        Err(e) => Err(format!("couldn't run ssh: {e}")),
    }
}

/// Turns a raw ssh stderr line into an actionable message.
fn classify(line: &str) -> String {
    let l = line.to_lowercase();
    if l.contains("permission denied") {
        "key rejected — add it with `ssh-add`, or set IdentityFile in ~/.ssh/config".to_string()
    } else if l.contains("host key verification failed") {
        "the server's host key changed — re-trust it in ~/.ssh/known_hosts".to_string()
    } else if l.contains("could not resolve") || l.contains("name or service") {
        "host not found — check the address".to_string()
    } else if l.contains("connection refused") {
        "connection refused — is sshd running / the port open?".to_string()
    } else if l.contains("timed out") || l.contains("timeout") {
        "connection timed out — the server is unreachable".to_string()
    } else {
        line.trim().to_string()
    }
}

/// Downloads the remote blob to `dest` (a private temp). `Ok(false)` = the
/// server has no blob yet (first sync); `Ok(true)` = downloaded.
fn download(cfg: &SyncConfig, username: &str, dest: &Path) -> Result<bool, String> {
    // one batched sftp session: try to get the blob; a missing file is fine.
    let batch = format!("get {} {}\n", cfg.remote_blob(username), dest.display());
    let out = run_sftp(cfg, &batch)?;
    if dest.is_file() {
        return Ok(true);
    }
    // sftp exits non-zero on a missing get; distinguish "no such file" (first
    // sync) from a real error.
    let low = out.to_lowercase();
    if low.contains("no such file") || low.contains("not found") || out.trim().is_empty() {
        Ok(false) // no remote blob yet (first sync)
    } else {
        Err(classify(&out))
    }
}

/// Uploads `src` to the remote blob path ATOMICALLY (temp + rename) and ensures
/// the remote dir exists (`mkdir -p` + chmod). Idempotent.
fn upload(cfg: &SyncConfig, username: &str, src: &Path) -> Result<(), String> {
    let remote = cfg.remote_blob(username);
    let tmp = format!("{remote}.tmp");
    let dir = cfg.remote_dir.trim_end_matches('/');
    let batch = format!(
        "-mkdir {dir}\nchmod 700 {dir}\nput {} {tmp}\nchmod 600 {tmp}\nrename {tmp} {remote}\n",
        src.display()
    );
    let out = run_sftp(cfg, &batch)?;
    // sftp with `-mkdir` (the leading `-` ignores its error if the dir exists);
    // a non-empty error after the rename step is a real failure.
    if out.to_lowercase().contains("failure") && !out.to_lowercase().contains("mkdir") {
        return Err(classify(&out));
    }
    Ok(())
}

/// Runs one batched `sftp` session, feeding `batch` on stdin. Returns combined
/// stdout+stderr for the caller to interpret.
fn run_sftp(cfg: &SyncConfig, batch: &str) -> Result<String, String> {
    use std::io::Write;
    let mut child = Command::new("sftp")
        .args(SSH_OPTS)
        .args(cfg.key_args())
        .arg("-b")
        .arg("-") // read the batch from stdin
        .arg(cfg.target())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("couldn't run sftp: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(batch.as_bytes());
    }
    let out = child.wait_with_output().map_err(|e| format!("sftp failed: {e}"))?;
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    Ok(s)
}

/// One full sync round for an OPEN vault: probe → pull remote (verify with the
/// vault's key) → decide → push or pull. The blob is opaque ciphertext on the
/// wire; freshness comes from the encrypted counter. Runs on a worker thread.
///
/// Returns the outcome plus, on a pull, the new content to adopt (the caller
/// applies it on the UI thread — this fn never touches the live App).
pub struct SyncResult {
    pub outcome: SyncOutcome,
    /// On `Pulled`, the remote content to install into the open vault.
    pub pulled_content: Option<crate::model::VaultContent>,
    /// The new baseline counter to persist, if it advanced.
    pub new_base: Option<u64>,
}

pub fn run_open_vault_sync(
    cfg: &SyncConfig,
    key: &[u8; crate::crypto::KEY_LEN],
    vault_path: &Path,
    username: &str,
    local: u64,
    local_fp: &str,
    vaults_dir: &Path,
) -> SyncResult {
    let fail = |m: String| SyncResult { outcome: SyncOutcome::Failed(m), pulled_content: None, new_base: None };

    if let Err(e) = probe(cfg) {
        return fail(e);
    }

    // pull the remote blob into a private temp to read its counter
    let tmp: PathBuf = vaults_dir.join(format!(".sync-pull-{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    // set when the unreadable remote is OUR own pre-rotation copy: it is then
    // replaced, not reconciled (there is no key left that can read it).
    let mut replace_retired = false;
    let remote_counter = match download(cfg, username, &tmp) {
        Ok(true) => match crate::vault::decrypt_blob_with(&tmp, key) {
            Ok(content) => Some((content.sync_counter(), content)),
            // we re-keyed since the last sync: this is our own blob under the
            // retired key. Leaving it up would keep the vault readable with the
            // password the user just retired, so push over it.
            Err(crate::vault::UnlockError::WrongPassword) if cfg.rekeyed => {
                let _ = std::fs::remove_file(&tmp);
                replace_retired = true;
                None
            }
            Err(crate::vault::UnlockError::WrongPassword) => {
                let _ = std::fs::remove_file(&tmp);
                return fail("remote was re-keyed elsewhere — unlock it to reconcile".into());
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return fail(format!("remote blob unreadable: {e:?}"));
            }
        },
        Ok(false) => None,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return fail(e);
        }
    };

    let mut decision = if replace_retired {
        SyncDecision::Push
    } else {
        decide(local, remote_counter.as_ref().map(|(c, _)| *c), cfg.base_counter)
    };
    // equal counters with DIFFERENT data = a real divergence (both sides edited
    // from the same base into different content) — never silently up-to-date.
    if decision == SyncDecision::UpToDate {
        if let Some((_, content)) = &remote_counter {
            if content.content_fingerprint() != local_fp {
                decision = SyncDecision::Diverged;
            }
        }
    }
    let result = match decision {
        SyncDecision::UpToDate => SyncResult {
            outcome: SyncOutcome::UpToDate,
            pulled_content: None,
            new_base: Some(local),
        },
        SyncDecision::Push => match upload(cfg, username, vault_path) {
            Ok(()) => SyncResult { outcome: SyncOutcome::Pushed, pulled_content: None, new_base: Some(local) },
            Err(e) => fail(e),
        },
        SyncDecision::Pull => {
            let (rc, content) = remote_counter.unwrap();
            // adopt the remote blob byte-for-byte (already verified via decrypt)
            match std::fs::rename(&tmp, vault_path) {
                Ok(()) => SyncResult {
                    outcome: SyncOutcome::Pulled(rc),
                    pulled_content: Some(content),
                    new_base: Some(rc),
                },
                Err(e) => fail(format!("couldn't install pulled blob: {e}")),
            }
        }
        SyncDecision::Diverged => SyncResult {
            outcome: SyncOutcome::Diverged {
                local,
                remote: remote_counter.as_ref().map(|(c, _)| *c).unwrap_or(0),
            },
            pulled_content: None,
            new_base: None,
        },
        SyncDecision::Rollback => fail("the server served an OLDER copy (rollback) — refused".into()),
    };
    let _ = std::fs::remove_file(&tmp);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_push_when_server_empty() {
        assert_eq!(decide(1, None, 0), SyncDecision::Push);
    }

    #[test]
    fn strictly_newer_side_wins() {
        assert_eq!(decide(5, Some(3), 3), SyncDecision::Push); // we advanced
        assert_eq!(decide(3, Some(5), 3), SyncDecision::Pull); // they advanced
    }

    #[test]
    fn equal_counters_are_up_to_date() {
        assert_eq!(decide(4, Some(4), 4), SyncDecision::UpToDate);
    }

    #[test]
    fn both_moved_is_divergence() {
        assert_eq!(decide(6, Some(7), 4), SyncDecision::Diverged);
    }

    #[test]
    fn server_replaying_an_old_blob_is_a_rollback() {
        assert_eq!(decide(5, Some(2), 5), SyncDecision::Rollback);
    }

    #[test]
    fn config_round_trips_without_secrets() {
        let c = SyncConfig {
            host: "vps.example.com".into(),
            user: "deploy".into(),
            remote_dir: "~/.datashield".into(),
            key_path: String::new(),
            auto: true,
            last_synced: None,
            base_counter: 3,
            rekeyed: true,
        };
        let toml = toml::to_string(&c).unwrap();
        assert!(!toml.to_lowercase().contains("password"));
        assert!(!toml.to_lowercase().contains("key\n") && !toml.contains("private"));
        let back: SyncConfig = toml::from_str(&toml).unwrap();
        assert_eq!(back.host, "vps.example.com");
        assert_eq!(back.base_counter, 3);
        assert!(back.rekeyed, "a pending re-key survives the sidecar round trip");
        assert_eq!(back.remote_blob("John"), "~/.datashield/4a6f686e.vault");
    }

    #[test]
    fn a_sidecar_written_before_the_rekey_flag_still_loads() {
        // older sidecars have no `rekeyed` key at all — they must keep loading
        // (and default to "the server copy is fine"), never fail the sync.
        let c: SyncConfig = toml::from_str(
            "host = \"vps.example.com\"\nuser = \"deploy\"\nbase_counter = 7\n",
        )
        .expect("an older sidecar still parses");
        assert!(!c.rekeyed);
        assert_eq!(c.base_counter, 7);
    }
}
