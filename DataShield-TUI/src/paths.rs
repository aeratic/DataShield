//! Data directory resolution.
//!
//! The C version wrote `FileSourceData/` relative to the working directory;
//! an installed binary needs a stable per-user location instead. The
//! `DATASHIELD_HOME` environment variable overrides it (handy for backups,
//! USB sticks and tests).

use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;

pub struct AppPaths {
    /// Directory holding the *.vault files.
    pub vaults_dir: PathBuf,
    /// Editable copy of the command registry (`commands.toml`).
    pub commands_config: PathBuf,
    /// User preferences (`settings.toml`).
    pub settings_config: PathBuf,
    /// Editable interface chrome (`ui.toml`).
    pub ui_config: PathBuf,
    /// Editable header logo (`logo.txt`).
    pub logo_config: PathBuf,
    /// Where the local embedding model is cached (`models/`). Not created by
    /// [`resolve`]: a user who never turns semantic search on should not get an
    /// empty directory for it — [`crate::rag`] makes it when it downloads.
    pub models_dir: PathBuf,
}

pub fn resolve() -> Result<AppPaths> {
    // An empty DATASHIELD_HOME (e.g. from an unset shell variable) is
    // treated as unset, like the XDG spec does — otherwise vaults would
    // silently land in whatever directory the app was launched from.
    let base = match std::env::var_os("DATASHIELD_HOME").filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => directories::ProjectDirs::from("", "", "DataShield")
            .context("cannot determine a data directory for this platform")?
            .data_dir()
            .to_path_buf(),
    };
    let vaults_dir = base.join("UsersVaultData");
    create_private_dir(&vaults_dir)
        .with_context(|| format!("creating data directory {}", vaults_dir.display()))?;
    Ok(AppPaths {
        vaults_dir,
        commands_config: base.join("commands.toml"),
        settings_config: base.join("settings.toml"),
        ui_config: base.join("ui.toml"),
        logo_config: base.join("logo.txt"),
        models_dir: base.join("models"),
    })
}

/// A real, existing directory to pre-fill the export path with (and to seed the
/// import path). Prefers the user's Downloads folder, then their home, then the
/// current working directory — whichever actually exists on this machine.
pub fn default_export_dir() -> PathBuf {
    if let Some(dirs) = directories::UserDirs::new() {
        if let Some(dl) = dirs.download_dir() {
            if dl.is_dir() {
                return dl.to_path_buf();
            }
        }
        if dirs.home_dir().is_dir() {
            return dirs.home_dir().to_path_buf();
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// mkdir -p, owner-only (0700) on Unix: the data dir holds the vaults.
pub fn create_private_dir(dir: &std::path::Path) -> std::io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}
