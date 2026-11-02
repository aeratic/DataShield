//! Live RFC-6238 codes for stored TOTP seeds.
//!
//! The vault stores the SEED (a secret, masked and zeroized like any other);
//! the detail pane shows the CODE it generates right now — `824 019 ⟳ 12 s` —
//! which is what you actually type into a login. The code is derived and
//! expires within 30 s, so showing it is the whole point; the seed itself is
//! never rendered.
//!
//! NOTE: the decoded seed bytes live briefly on the un-zeroized heap inside
//! the TOTP engine (the same trade-off as any revealed value); the stored
//! seed stays a Drop-zeroized [`SecretString`](crate::model::SecretString).

use totp_rs::{Algorithm, Secret, TOTP};

/// The 6-digit code for a base32 `seed` at unix time `t` (`None` = the seed
/// isn't valid base32). Whitespace and case in the seed are forgiven — pasted
/// seeds often arrive as `jbsw y3dp ehpk 3pxp`.
pub fn code_at(seed: &str, t: u64) -> Option<String> {
    let cleaned: String = seed.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_uppercase();
    if cleaned.is_empty() {
        return None;
    }
    let bytes = Secret::Encoded(cleaned).to_bytes().ok()?;
    if bytes.is_empty() {
        return None;
    }
    // unchecked: real-world seeds are often shorter than RFC 4226's 128-bit
    // minimum; refusing them would just push people back to their phone.
    let totp = TOTP::new_unchecked(Algorithm::SHA1, 6, 1, 30, bytes);
    Some(totp.generate(t))
}

/// The current code and the seconds left before it rotates.
pub fn code_now(seed: &str) -> Option<(String, u64)> {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some((code_at(seed, t)?, 30 - (t % 30)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 6238 appendix B vectors (SHA-1, ASCII secret "12345678901234567890"
    /// = base32 GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ), truncated to 6 digits.
    const SEED: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";

    #[test]
    fn rfc6238_vectors_generate_the_documented_codes() {
        assert_eq!(code_at(SEED, 59).as_deref(), Some("287082"));
        assert_eq!(code_at(SEED, 1_111_111_109).as_deref(), Some("081804"));
        assert_eq!(code_at(SEED, 1_234_567_890).as_deref(), Some("005924"));
    }

    #[test]
    fn seeds_are_forgiven_whitespace_and_case() {
        assert_eq!(
            code_at("gezd gnbv gy3t qojq gezd gnbv gy3t qojq", 59).as_deref(),
            Some("287082")
        );
    }

    #[test]
    fn garbage_seeds_yield_none_not_garbage_codes() {
        assert_eq!(code_at("not base32 at all!!", 59), None);
        assert_eq!(code_at("", 59), None);
    }
}
