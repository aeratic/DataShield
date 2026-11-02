//! Crypto layer: selectable two-way (AEAD) and one-way (KDF) algorithms.
//!
//! Replaces the old custom LME (Log Modular Encryption) algorithm with real,
//! authenticated encryption, and the old MD5 login hash with a selectable
//! password-based key derivation. Weak one-way choices (plain hashes, MD5,
//! SHA-1) are offered for study/compatibility but clearly flagged in the UI.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use anyhow::{anyhow, Result};
use zeroize::Zeroizing;

pub const KEY_LEN: usize = 32;
pub const SALT_LEN: usize = 16;

// ------------------------------------------------------------- two-way

/// The AEAD algorithms a vault can be encrypted with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Algorithm {
    Aes256Gcm,
    Aes128Gcm,
    Aes256GcmSiv,
    Aes128GcmSiv,
    ChaCha20Poly1305,
    XChaCha20Poly1305,
    Aes256Eax,
    Aes256Ccm,
    Aes256Ocb3,
    Ascon128a,
    DeoxysII256,
}

type Aes256EaxImpl = eax::Eax<aes::Aes256>;
type Aes256CcmImpl = ccm::Ccm<aes::Aes256, ccm::consts::U16, ccm::consts::U13>;
type Aes256Ocb3Impl = ocb3::Ocb3<aes::Aes256>;

impl Algorithm {
    pub const ALL: [Algorithm; 11] = [
        Algorithm::Aes256Gcm,
        Algorithm::Aes128Gcm,
        Algorithm::Aes256GcmSiv,
        Algorithm::Aes128GcmSiv,
        Algorithm::ChaCha20Poly1305,
        Algorithm::XChaCha20Poly1305,
        Algorithm::Aes256Eax,
        Algorithm::Aes256Ccm,
        Algorithm::Aes256Ocb3,
        Algorithm::Ascon128a,
        Algorithm::DeoxysII256,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Algorithm::Aes256Gcm => "AES-256-GCM",
            Algorithm::Aes128Gcm => "AES-128-GCM",
            Algorithm::Aes256GcmSiv => "AES-256-GCM-SIV",
            Algorithm::Aes128GcmSiv => "AES-128-GCM-SIV",
            Algorithm::ChaCha20Poly1305 => "ChaCha20-Poly1305",
            Algorithm::XChaCha20Poly1305 => "XChaCha20-Poly1305",
            Algorithm::Aes256Eax => "AES-256-EAX",
            Algorithm::Aes256Ccm => "AES-256-CCM",
            Algorithm::Aes256Ocb3 => "AES-256-OCB3",
            Algorithm::Ascon128a => "Ascon-128a",
            Algorithm::DeoxysII256 => "Deoxys-II-256",
        }
    }

    /// Short note shown in the selection menu.
    pub fn note(self) -> &'static str {
        match self {
            Algorithm::Aes256Gcm => "industry standard (default)",
            Algorithm::Aes128Gcm => "industry standard, 128-bit key",
            Algorithm::Aes256GcmSiv => "nonce-misuse resistant",
            Algorithm::Aes128GcmSiv => "nonce-misuse resistant, 128-bit key",
            Algorithm::ChaCha20Poly1305 => "modern stream AEAD, RFC 8439",
            Algorithm::XChaCha20Poly1305 => "extended 192-bit nonce",
            Algorithm::Aes256Eax => "two-pass AES mode",
            Algorithm::Aes256Ccm => "AES counter with CBC-MAC",
            Algorithm::Aes256Ocb3 => "single-pass AES mode, RFC 7253",
            Algorithm::Ascon128a => "NIST lightweight-crypto winner",
            Algorithm::DeoxysII256 => "CAESAR competition winner",
        }
    }

    /// Case-insensitive lookup; '-', '_', '.' and '/' are ignored.
    pub fn from_name(s: &str) -> Option<Algorithm> {
        match normalize(s).as_str() {
            "aes256gcm" | "aes" => Some(Algorithm::Aes256Gcm),
            "aes128gcm" => Some(Algorithm::Aes128Gcm),
            "aes256gcmsiv" | "gcmsiv" => Some(Algorithm::Aes256GcmSiv),
            "aes128gcmsiv" => Some(Algorithm::Aes128GcmSiv),
            "chacha20poly1305" | "chacha20" | "chacha" => Some(Algorithm::ChaCha20Poly1305),
            "xchacha20poly1305" | "xchacha20" | "xchacha" => Some(Algorithm::XChaCha20Poly1305),
            "aes256eax" | "eax" => Some(Algorithm::Aes256Eax),
            "aes256ccm" | "ccm" => Some(Algorithm::Aes256Ccm),
            "aes256ocb3" | "ocb3" | "ocb" => Some(Algorithm::Aes256Ocb3),
            "ascon128a" | "ascon" => Some(Algorithm::Ascon128a),
            "deoxysii256" | "deoxys" => Some(Algorithm::DeoxysII256),
            _ => None,
        }
    }

    pub fn nonce_len(self) -> usize {
        match self {
            Algorithm::Aes256Gcm
            | Algorithm::Aes128Gcm
            | Algorithm::Aes256GcmSiv
            | Algorithm::Aes128GcmSiv
            | Algorithm::ChaCha20Poly1305
            | Algorithm::Aes256Ocb3 => 12,
            Algorithm::Aes256Ccm => 13,
            Algorithm::DeoxysII256 => 15,
            Algorithm::Aes256Eax | Algorithm::Ascon128a => 16,
            Algorithm::XChaCha20Poly1305 => 24,
        }
    }

    pub fn key_len(self) -> usize {
        match self {
            Algorithm::Aes128Gcm | Algorithm::Aes128GcmSiv | Algorithm::Ascon128a => 16,
            _ => 32,
        }
    }
}

fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| !matches!(c, '-' | '_' | '.' | '/' | ' '))
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

// ------------------------------------------------------------- one-way

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Argon2Variant {
    Id,
    I,
    D,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pbkdf2Hash {
    Sha256,
    Sha512,
    Sha1,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HashKind {
    Sha256,
    Sha512,
    Sha3_256,
    Sha3_512,
    Blake2b,
    Sha1,
    Md5,
}

/// How resistant a one-way choice is against password brute-forcing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hardness {
    /// Purpose-built password KDF (memory- and/or CPU-hard).
    Hardened,
    /// Sound hash, but a single fast pass: cheap to brute-force.
    Fast,
    /// Cryptographically broken hash. Study use only.
    Broken,
}

/// A one-way algorithm plus its work parameters, as stored in the vault file.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KdfSpec {
    Argon2 { variant: Argon2Variant, m_cost_kib: u32, t_cost: u32, p_cost: u32 },
    Scrypt { log_n: u32, r: u32, p: u32 },
    Pbkdf2 { hash: Pbkdf2Hash, iterations: u32 },
    BcryptPbkdf { rounds: u32 },
    /// Single HKDF (RFC 5869) pass over the given hash — NOT hardened.
    Hkdf { hash: HashKind },
    /// BLAKE3 in derive-key mode — fast by design, NOT hardened.
    Blake3,
}

impl Default for KdfSpec {
    fn default() -> Self {
        KdfSpec::Argon2 {
            variant: Argon2Variant::Id,
            m_cost_kib: 64 * 1024,
            t_cost: 3,
            p_cost: 1,
        }
    }
}

pub const MAX_M_COST_KIB: u32 = 2 * 1024 * 1024; // 2 GiB
pub const MAX_T_COST: u32 = 64;
pub const MAX_P_COST: u32 = 8;

impl KdfSpec {
    /// Menu/selection order: hardened KDFs first, then fast hashes.
    pub const ALL: [KdfSpec; 16] = [
        KdfSpec::Argon2 { variant: Argon2Variant::Id, m_cost_kib: 64 * 1024, t_cost: 3, p_cost: 1 },
        KdfSpec::Argon2 { variant: Argon2Variant::I, m_cost_kib: 64 * 1024, t_cost: 3, p_cost: 1 },
        KdfSpec::Argon2 { variant: Argon2Variant::D, m_cost_kib: 64 * 1024, t_cost: 3, p_cost: 1 },
        KdfSpec::Scrypt { log_n: 15, r: 8, p: 1 },
        KdfSpec::BcryptPbkdf { rounds: 16 },
        KdfSpec::Pbkdf2 { hash: Pbkdf2Hash::Sha256, iterations: 600_000 },
        KdfSpec::Pbkdf2 { hash: Pbkdf2Hash::Sha512, iterations: 210_000 },
        KdfSpec::Pbkdf2 { hash: Pbkdf2Hash::Sha1, iterations: 1_300_000 },
        KdfSpec::Hkdf { hash: HashKind::Sha256 },
        KdfSpec::Hkdf { hash: HashKind::Sha512 },
        KdfSpec::Hkdf { hash: HashKind::Sha3_256 },
        KdfSpec::Hkdf { hash: HashKind::Sha3_512 },
        KdfSpec::Hkdf { hash: HashKind::Blake2b },
        KdfSpec::Blake3,
        KdfSpec::Hkdf { hash: HashKind::Sha1 },
        KdfSpec::Hkdf { hash: HashKind::Md5 },
    ];

    /// Single-token name (used in commands and in the vault file).
    pub fn name(self) -> &'static str {
        match self {
            KdfSpec::Argon2 { variant: Argon2Variant::Id, .. } => "Argon2id",
            KdfSpec::Argon2 { variant: Argon2Variant::I, .. } => "Argon2i",
            KdfSpec::Argon2 { variant: Argon2Variant::D, .. } => "Argon2d",
            KdfSpec::Scrypt { .. } => "scrypt",
            KdfSpec::BcryptPbkdf { .. } => "bcrypt",
            KdfSpec::Pbkdf2 { hash: Pbkdf2Hash::Sha256, .. } => "PBKDF2-SHA256",
            KdfSpec::Pbkdf2 { hash: Pbkdf2Hash::Sha512, .. } => "PBKDF2-SHA512",
            KdfSpec::Pbkdf2 { hash: Pbkdf2Hash::Sha1, .. } => "PBKDF2-SHA1",
            KdfSpec::Hkdf { hash: HashKind::Sha256 } => "SHA-256",
            KdfSpec::Hkdf { hash: HashKind::Sha512 } => "SHA-512",
            KdfSpec::Hkdf { hash: HashKind::Sha3_256 } => "SHA3-256",
            KdfSpec::Hkdf { hash: HashKind::Sha3_512 } => "SHA3-512",
            KdfSpec::Hkdf { hash: HashKind::Blake2b } => "BLAKE2b",
            KdfSpec::Blake3 => "BLAKE3",
            KdfSpec::Hkdf { hash: HashKind::Sha1 } => "SHA-1",
            KdfSpec::Hkdf { hash: HashKind::Md5 } => "MD5",
        }
    }

    /// Lookup by name with default parameters. Case-insensitive; '-', '_'
    /// and '.' are ignored.
    pub fn from_name(s: &str) -> Option<KdfSpec> {
        let n = normalize(s);
        // "aragon" kept as a friendly alias — the request that added this
        // feature spelled it that way.
        let n = match n.as_str() {
            "argon" | "argon2" | "aragon" => "argon2id".to_string(),
            "pbkdf2" => "pbkdf2sha256".to_string(),
            "bcryptpbkdf" => "bcrypt".to_string(),
            "sha3" => "sha3256".to_string(),
            "blake2" | "blake2b512" => "blake2b".to_string(),
            other => other.to_string(),
        };
        KdfSpec::ALL
            .into_iter()
            .find(|spec| normalize(spec.name()) == n)
    }

    pub fn hardness(self) -> Hardness {
        match self {
            KdfSpec::Argon2 { .. }
            | KdfSpec::Scrypt { .. }
            | KdfSpec::Pbkdf2 { .. }
            | KdfSpec::BcryptPbkdf { .. } => Hardness::Hardened,
            KdfSpec::Hkdf { hash: HashKind::Sha1 | HashKind::Md5 } => Hardness::Broken,
            KdfSpec::Hkdf { .. } | KdfSpec::Blake3 => Hardness::Fast,
        }
    }

    /// True when the PARAMETERS sit below the OWASP password-storage baseline
    /// (the family may be fine — Argon2id with m=8 KiB is still weak). Used by
    /// the encryption card's `!` lint; `:rerollEnc` re-keys with safe defaults.
    /// Baselines (OWASP): Argon2id m=19456 KiB/t=2 (or m=9216/t=4), scrypt
    /// N=2^17, PBKDF2 600k iterations, bcrypt work factor 10; plain fast
    /// hashes are never password-grade.
    pub fn below_owasp(self) -> bool {
        match self {
            KdfSpec::Argon2 { m_cost_kib, t_cost, .. } => {
                !((m_cost_kib >= 19_456 && t_cost >= 2) || (m_cost_kib >= 9_216 && t_cost >= 4))
            }
            KdfSpec::Scrypt { log_n, .. } => log_n < 17,
            KdfSpec::Pbkdf2 { iterations, .. } => iterations < 600_000,
            KdfSpec::BcryptPbkdf { rounds } => rounds < 10,
            KdfSpec::Hkdf { .. } | KdfSpec::Blake3 => true,
        }
    }

    /// Canonical parameter string, bound into the AEAD header AAD.
    pub fn canonical(self) -> String {
        match self {
            KdfSpec::Argon2 { m_cost_kib, t_cost, p_cost, .. } => {
                format!("{};m={m_cost_kib};t={t_cost};p={p_cost}", self.name())
            }
            KdfSpec::Scrypt { log_n, r, p } => format!("scrypt;log_n={log_n};r={r};p={p}"),
            KdfSpec::Pbkdf2 { iterations, .. } => format!("{};iter={iterations}", self.name()),
            KdfSpec::BcryptPbkdf { rounds } => format!("bcrypt;rounds={rounds}"),
            KdfSpec::Hkdf { .. } | KdfSpec::Blake3 => self.name().to_string(),
        }
    }

    /// Human description for the encInfo screen.
    pub fn description(self) -> String {
        match self {
            KdfSpec::Argon2 { m_cost_kib, t_cost, p_cost, .. } => format!(
                "{} (memory {} MiB, iterations {t_cost}, parallelism {p_cost})",
                self.name(),
                m_cost_kib / 1024
            ),
            KdfSpec::Scrypt { log_n, r, p } => format!("scrypt (N=2^{log_n}, r={r}, p={p})"),
            KdfSpec::Pbkdf2 { iterations, .. } => {
                format!("{} (HMAC, {iterations} iterations)", self.name())
            }
            KdfSpec::BcryptPbkdf { rounds } => format!("bcrypt-pbkdf ({rounds} rounds)"),
            KdfSpec::Blake3 => {
                "BLAKE3 derive-key (single pass — not brute-force resistant)".to_string()
            }
            KdfSpec::Hkdf { hash } => {
                let warning = match hash {
                    HashKind::Md5 | HashKind::Sha1 => "broken hash — study use only",
                    _ => "single pass — not brute-force resistant",
                };
                format!("{} via HKDF ({warning})", self.name())
            }
        }
    }

    /// Bounds check for parameters read back from a vault file, so a crafted
    /// header cannot request absurd time or memory.
    pub fn is_sane(self) -> bool {
        match self {
            KdfSpec::Argon2 { m_cost_kib, t_cost, p_cost, .. } => {
                (8..=MAX_M_COST_KIB).contains(&m_cost_kib)
                    && (1..=MAX_T_COST).contains(&t_cost)
                    && (1..=MAX_P_COST).contains(&p_cost)
            }
            KdfSpec::Scrypt { log_n, r, p } => {
                (10..=24).contains(&log_n)
                    && (1..=32).contains(&r)
                    && (1..=16).contains(&p)
                    // 128 * r * 2^log_n bytes must stay under 2 GiB
                    && (128u64 * r as u64) << log_n <= (MAX_M_COST_KIB as u64) * 1024
            }
            KdfSpec::Pbkdf2 { iterations, .. } => (1_000..=100_000_000).contains(&iterations),
            KdfSpec::BcryptPbkdf { rounds } => (4..=256).contains(&rounds),
            KdfSpec::Hkdf { .. } | KdfSpec::Blake3 => true,
        }
    }
}

// ------------------------------------------------------------- primitives

pub fn random_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    getrandom::getrandom(&mut buf).expect("OS random generator unavailable");
    buf
}

/// Derives the 32-byte vault key from the master password with the chosen
/// one-way algorithm. AEADs with 128-bit keys use the first 16 bytes.
pub fn derive_key(
    password: &str,
    salt: &[u8],
    spec: &KdfSpec,
) -> Result<Zeroizing<[u8; KEY_LEN]>> {
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    let pw = password.as_bytes();

    match *spec {
        KdfSpec::Argon2 { variant, m_cost_kib, t_cost, p_cost } => {
            let algorithm = match variant {
                Argon2Variant::Id => argon2::Algorithm::Argon2id,
                Argon2Variant::I => argon2::Algorithm::Argon2i,
                Argon2Variant::D => argon2::Algorithm::Argon2d,
            };
            let params = argon2::Params::new(m_cost_kib, t_cost, p_cost, Some(KEY_LEN))
                .map_err(|e| anyhow!("invalid Argon2 parameters: {e}"))?;
            argon2::Argon2::new(algorithm, argon2::Version::V0x13, params)
                .hash_password_into(pw, salt, key.as_mut())
                .map_err(|e| anyhow!("Argon2 failed: {e}"))?;
        }
        KdfSpec::Scrypt { log_n, r, p } => {
            let params = scrypt::Params::new(log_n as u8, r, p, KEY_LEN)
                .map_err(|e| anyhow!("invalid scrypt parameters: {e}"))?;
            scrypt::scrypt(pw, salt, &params, key.as_mut())
                .map_err(|e| anyhow!("scrypt failed: {e}"))?;
        }
        KdfSpec::Pbkdf2 { hash, iterations } => match hash {
            Pbkdf2Hash::Sha256 => {
                pbkdf2::pbkdf2_hmac::<sha2::Sha256>(pw, salt, iterations, key.as_mut())
            }
            Pbkdf2Hash::Sha512 => {
                pbkdf2::pbkdf2_hmac::<sha2::Sha512>(pw, salt, iterations, key.as_mut())
            }
            Pbkdf2Hash::Sha1 => {
                pbkdf2::pbkdf2_hmac::<sha1::Sha1>(pw, salt, iterations, key.as_mut())
            }
        },
        KdfSpec::BcryptPbkdf { rounds } => {
            bcrypt_pbkdf::bcrypt_pbkdf(pw, salt, rounds, key.as_mut())
                .map_err(|e| anyhow!("bcrypt-pbkdf failed: {e}"))?;
        }
        KdfSpec::Blake3 => {
            let mut hasher = blake3::Hasher::new_derive_key("datashield v3 vault key");
            hasher.update(salt);
            hasher.update(pw);
            hasher.finalize_xof().fill(key.as_mut());
        }
        KdfSpec::Hkdf { hash } => {
            const INFO: &[u8] = b"datashield v3 vault key";
            macro_rules! hkdf_case {
                ($hash:ty) => {
                    hkdf::Hkdf::<$hash>::new(Some(salt), pw)
                        .expand(INFO, key.as_mut())
                        .map_err(|_| anyhow!("HKDF expand failed"))?
                };
            }
            match hash {
                HashKind::Sha256 => hkdf_case!(sha2::Sha256),
                HashKind::Sha512 => hkdf_case!(sha2::Sha512),
                HashKind::Sha3_256 => hkdf_case!(sha3::Sha3_256),
                HashKind::Sha3_512 => hkdf_case!(sha3::Sha3_512),
                // BLAKE2 has a lazy buffer kind, incompatible with Hmac's
                // eager one; SimpleHmac (block-level HMAC) handles it.
                HashKind::Blake2b => hkdf::SimpleHkdf::<blake2::Blake2b512>::new(Some(salt), pw)
                    .expand(INFO, key.as_mut())
                    .map_err(|_| anyhow!("HKDF expand failed"))?,
                HashKind::Sha1 => hkdf_case!(sha1::Sha1),
                HashKind::Md5 => hkdf_case!(md5::Md5),
            }
        }
    }
    Ok(key)
}

// ------------------------------------------------------------- AEAD

fn aead_encrypt<C: Aead + KeyInit>(
    key: &[u8],
    nonce: &[u8],
    plaintext: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>> {
    C::new_from_slice(key)
        .map_err(|_| anyhow!("bad key length"))?
        .encrypt(
            aes_gcm::aead::Nonce::<C>::from_slice(nonce),
            Payload { msg: plaintext, aad },
        )
        .map_err(|_| anyhow!("encryption failed"))
}

fn aead_decrypt<C: Aead + KeyInit>(
    key: &[u8],
    nonce: &[u8],
    ciphertext: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>> {
    C::new_from_slice(key)
        .map_err(|_| anyhow!("bad key length"))?
        .decrypt(
            aes_gcm::aead::Nonce::<C>::from_slice(nonce),
            Payload { msg: ciphertext, aad },
        )
        .map_err(|_| anyhow!("decryption failed: wrong password or corrupted data"))
}

/// Encrypts with a freshly generated random nonce, binding `aad` (the vault
/// header) into the authentication tag. Returns (nonce, ciphertext-with-tag).
pub fn encrypt(
    alg: Algorithm,
    key: &[u8; KEY_LEN],
    plaintext: &[u8],
    aad: &[u8],
) -> Result<(Vec<u8>, Vec<u8>)> {
    let nonce = random_bytes(alg.nonce_len());
    let k = &key[..alg.key_len()];
    let ciphertext = match alg {
        Algorithm::Aes256Gcm => aead_encrypt::<aes_gcm::Aes256Gcm>(k, &nonce, plaintext, aad),
        Algorithm::Aes128Gcm => aead_encrypt::<aes_gcm::Aes128Gcm>(k, &nonce, plaintext, aad),
        Algorithm::Aes256GcmSiv => {
            aead_encrypt::<aes_gcm_siv::Aes256GcmSiv>(k, &nonce, plaintext, aad)
        }
        Algorithm::Aes128GcmSiv => {
            aead_encrypt::<aes_gcm_siv::Aes128GcmSiv>(k, &nonce, plaintext, aad)
        }
        Algorithm::ChaCha20Poly1305 => {
            aead_encrypt::<chacha20poly1305::ChaCha20Poly1305>(k, &nonce, plaintext, aad)
        }
        Algorithm::XChaCha20Poly1305 => {
            aead_encrypt::<chacha20poly1305::XChaCha20Poly1305>(k, &nonce, plaintext, aad)
        }
        Algorithm::Aes256Eax => aead_encrypt::<Aes256EaxImpl>(k, &nonce, plaintext, aad),
        Algorithm::Aes256Ccm => aead_encrypt::<Aes256CcmImpl>(k, &nonce, plaintext, aad),
        Algorithm::Aes256Ocb3 => aead_encrypt::<Aes256Ocb3Impl>(k, &nonce, plaintext, aad),
        Algorithm::Ascon128a => aead_encrypt::<ascon_aead::Ascon128a>(k, &nonce, plaintext, aad),
        Algorithm::DeoxysII256 => aead_encrypt::<deoxys::DeoxysII256>(k, &nonce, plaintext, aad),
    }?;
    Ok((nonce, ciphertext))
}

/// Fails when the key is wrong or the data/header was tampered with (AEAD
/// tag check). The caller must validate `nonce.len()` beforehand if it wants
/// to distinguish structural corruption from a wrong password.
pub fn decrypt(
    alg: Algorithm,
    key: &[u8; KEY_LEN],
    nonce: &[u8],
    ciphertext: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>> {
    if nonce.len() != alg.nonce_len() {
        return Err(anyhow!("bad nonce length for {}", alg.name()));
    }
    let k = &key[..alg.key_len()];
    match alg {
        Algorithm::Aes256Gcm => aead_decrypt::<aes_gcm::Aes256Gcm>(k, nonce, ciphertext, aad),
        Algorithm::Aes128Gcm => aead_decrypt::<aes_gcm::Aes128Gcm>(k, nonce, ciphertext, aad),
        Algorithm::Aes256GcmSiv => {
            aead_decrypt::<aes_gcm_siv::Aes256GcmSiv>(k, nonce, ciphertext, aad)
        }
        Algorithm::Aes128GcmSiv => {
            aead_decrypt::<aes_gcm_siv::Aes128GcmSiv>(k, nonce, ciphertext, aad)
        }
        Algorithm::ChaCha20Poly1305 => {
            aead_decrypt::<chacha20poly1305::ChaCha20Poly1305>(k, nonce, ciphertext, aad)
        }
        Algorithm::XChaCha20Poly1305 => {
            aead_decrypt::<chacha20poly1305::XChaCha20Poly1305>(k, nonce, ciphertext, aad)
        }
        Algorithm::Aes256Eax => aead_decrypt::<Aes256EaxImpl>(k, nonce, ciphertext, aad),
        Algorithm::Aes256Ccm => aead_decrypt::<Aes256CcmImpl>(k, nonce, ciphertext, aad),
        Algorithm::Aes256Ocb3 => aead_decrypt::<Aes256Ocb3Impl>(k, nonce, ciphertext, aad),
        Algorithm::Ascon128a => aead_decrypt::<ascon_aead::Ascon128a>(k, nonce, ciphertext, aad),
        Algorithm::DeoxysII256 => aead_decrypt::<deoxys::DeoxysII256>(k, nonce, ciphertext, aad),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn fast_variant(spec: KdfSpec) -> KdfSpec {
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

    #[test]
    fn roundtrip_all_algorithms() {
        let key = [7u8; KEY_LEN];
        let msg = b"la password di Gmail: S3greta!";
        for alg in Algorithm::ALL {
            let (nonce, ct) = encrypt(alg, &key, msg, b"header").unwrap();
            assert_eq!(nonce.len(), alg.nonce_len(), "{}", alg.name());
            let back = decrypt(alg, &key, &nonce, &ct, b"header").unwrap();
            assert_eq!(back, msg, "{}", alg.name());
        }
    }

    #[test]
    fn tampering_and_aad_mismatch_are_detected() {
        let key = [1u8; KEY_LEN];
        for alg in Algorithm::ALL {
            let (nonce, mut ct) = encrypt(alg, &key, b"dati importanti", b"h1").unwrap();
            assert!(decrypt(alg, &key, &nonce, &ct, b"h2").is_err(), "{}", alg.name());
            ct[0] ^= 0x01;
            assert!(decrypt(alg, &key, &nonce, &ct, b"h1").is_err(), "{}", alg.name());
        }
    }

    #[test]
    fn wrong_key_fails() {
        let key = [2u8; KEY_LEN];
        let other = [3u8; KEY_LEN];
        for alg in Algorithm::ALL {
            let (nonce, ct) = encrypt(alg, &key, b"segreto", b"").unwrap();
            assert!(decrypt(alg, &other, &nonce, &ct, b"").is_err(), "{}", alg.name());
        }
    }

    #[test]
    fn all_kdfs_derive_deterministically_and_are_salt_sensitive() {
        for spec in KdfSpec::ALL {
            let spec = fast_variant(spec);
            let k1 = derive_key("password", &[0u8; SALT_LEN], &spec).unwrap();
            let k2 = derive_key("password", &[0u8; SALT_LEN], &spec).unwrap();
            let k3 = derive_key("password", &[1u8; SALT_LEN], &spec).unwrap();
            let k4 = derive_key("Password", &[0u8; SALT_LEN], &spec).unwrap();
            assert_eq!(*k1, *k2, "{}", spec.name());
            assert_ne!(*k1, *k3, "{}", spec.name());
            assert_ne!(*k1, *k4, "{}", spec.name());
        }
    }

    #[test]
    fn kdf_names_roundtrip_and_aliases_work() {
        for spec in KdfSpec::ALL {
            assert_eq!(KdfSpec::from_name(spec.name()), Some(spec), "{}", spec.name());
        }
        assert_eq!(KdfSpec::from_name("aragon"), Some(KdfSpec::default()));
        assert_eq!(KdfSpec::from_name("md5").unwrap().hardness(), Hardness::Broken);
        assert_eq!(KdfSpec::from_name("sha256").unwrap().hardness(), Hardness::Fast);
        assert_eq!(KdfSpec::from_name("scrypt").unwrap().hardness(), Hardness::Hardened);
        assert_eq!(KdfSpec::from_name("nonsense"), None);
    }

    #[test]
    fn algorithm_names_roundtrip() {
        for alg in Algorithm::ALL {
            assert_eq!(Algorithm::from_name(alg.name()), Some(alg), "{}", alg.name());
        }
        assert_eq!(Algorithm::from_name("aes"), Some(Algorithm::Aes256Gcm));
        assert_eq!(Algorithm::from_name("nonsense"), None);
    }

    #[test]
    fn kdf_sanity_bounds() {
        for spec in KdfSpec::ALL {
            assert!(spec.is_sane(), "{}", spec.name());
        }
        assert!(!KdfSpec::Argon2 {
            variant: Argon2Variant::Id,
            m_cost_kib: u32::MAX,
            t_cost: 3,
            p_cost: 1
        }
        .is_sane());
        assert!(!KdfSpec::Scrypt { log_n: 30, r: 8, p: 1 }.is_sane());
        assert!(!KdfSpec::Pbkdf2 { hash: Pbkdf2Hash::Sha256, iterations: 1 }.is_sane());
    }
}
