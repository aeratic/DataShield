//! DataShield core — the vault format, the cryptography, sync and the shared
//! data model. Every DataShield front-end (the TUI today, the GUI next) reads
//! and writes vaults through this crate and nothing else, so there is exactly
//! one implementation of the storage standard (`docs/STORAGE-STANDARD.md`).

pub mod config;
pub mod crypto;
pub mod model;
pub mod sync;
pub mod totp;
pub mod vault;
