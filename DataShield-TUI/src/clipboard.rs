//! The system clipboard: what a copy actually puts there, and the timed wipe.
//!
//! Two rules the rest of the app leans on:
//!   * a TOTP field copies the CODE it generates right now, never the stored
//!     seed — the code is what you paste into a login, and it dies in 30 s;
//!   * nothing we copy is allowed to linger: [`CLEAR_AFTER_SECS`] later the
//!     main thread wipes it, but only when our copy is still the latest one
//!     (the generation counter lives on `App` — see `App::copy_gen`). That
//!     timer is a detached thread, so it can't be the ONLY wipe: locking,
//!     closing the vault and quitting wipe at once (`App::wipe_clipboard`),
//!     or a quit inside the window would hand the secret to the desktop's
//!     clipboard manager for good.
//!
//! The `arboard` handle is created LAZILY on the first copy and then kept: on
//! X11 the clipboard's bytes live in the owning window, so dropping the handle
//! after every copy would hand the data straight to a clipboard manager — or
//! lose it outright where none runs.
//!
//! Both backends are compiled in (`arboard`'s `wayland-data-control` feature —
//! see Cargo.toml): arboard picks wlr-data-control when `WAYLAND_DISPLAY` is
//! set and X11 otherwise, so under a compositor the copy stops landing in the
//! XWayland clipboard nobody reads. The rule above survives the switch — a
//! Wayland copy is served by a thread INSIDE this process too (arboard asks
//! `wl-clipboard-rs` not to wait, which spawns a serving thread rather than
//! forking a daemon), so the selection dies with us and the wipes below stay
//! the only thing standing between a secret and the desktop. Wayland's handle
//! is stateless, though — each copy spawns a fresh server and the previous one
//! exits when it loses the selection — so there the lazy handle only spares us
//! re-probing the compositor.

use crate::model::{FieldType, Item, ItemField};
use zeroize::Zeroizing;

/// How long a copied value may sit on the system clipboard.
pub const CLEAR_AFTER_SECS: u64 = 20;

/// Whether a field carries a TOTP seed: its declared DCS-1 type, or a key that
/// says so (hand-added `totp` fields carry the plain `Text` type).
pub fn is_totp_field(field: &ItemField) -> bool {
    field.ftype == FieldType::Totp || field.key.to_lowercase().contains("totp")
}

/// The text a copy of `item.fields[field_idx]` puts on the clipboard: the LIVE
/// 6-digit code for a TOTP seed (at unix time `now`), else the stored value
/// verbatim. `None` = nothing to copy (no such field, or it is empty).
///
/// Pure on purpose — the "which bytes leave the vault" decision is unit-tested
/// without ever touching a real clipboard.
pub fn copy_payload(item: &Item, field_idx: usize, now: u64) -> Option<Zeroizing<String>> {
    let field = item.fields.get(field_idx)?;
    if field.value.is_empty() {
        return None;
    }
    if is_totp_field(field) {
        // a valid seed copies its code; garbage base32 falls through to the
        // raw value (it is not a seed, so it is just text the user stored)
        if let Some(code) = crate::totp::code_at(field.value.as_str(), now) {
            return Some(Zeroizing::new(code));
        }
    }
    Some(Zeroizing::new(field.value.as_str().to_string()))
}

/// Whether a wipe may go ahead: ONLY when what is sitting on the clipboard is
/// still, byte for byte, what we put there. `current` is the read-back —
/// `None` means the clipboard couldn't be read at all, and then the answer is
/// NO: destroying data we can't even see would be a password manager erasing
/// somebody else's copy (see `App::wipe_clipboard`).
///
/// Pure on purpose — the "may we erase this?" decision is unit-tested without a
/// display server, exactly like [`copy_payload`].
pub fn should_clear(current: Option<&str>, ours: &str) -> bool {
    current == Some(ours)
}

/// The app's one clipboard handle. Empty until the first copy, so a headless
/// run (tests, CI, a TTY with no display server) never pays for — or fails on —
/// a clipboard it doesn't use.
pub struct Clip {
    inner: Option<arboard::Clipboard>,
}

impl Clip {
    pub fn new() -> Clip {
        Clip { inner: None }
    }

    /// Puts `text` on the system clipboard. `Err` = no clipboard available
    /// (no display server, a locked-down session); the caller shows a toast —
    /// a password manager must never die on a missing X11 socket.
    pub fn set_text(&mut self, text: &str) -> Result<(), String> {
        self.handle()?.set_text(text).map_err(|e| e.to_string())
    }

    /// Reads the clipboard back. `Err` = no clipboard, or nothing text-shaped
    /// on it. Zeroizing because the bytes coming back may be OUR secret — the
    /// only reason we ever read is to check it is still ours before wiping.
    pub fn get_text(&mut self) -> Result<Zeroizing<String>, String> {
        let text = self.handle()?.get_text().map_err(|e| e.to_string())?;
        Ok(Zeroizing::new(text))
    }

    /// Wipes the clipboard (best effort — a failure here is not worth a toast:
    /// the user asked for a copy, not for a clear).
    pub fn clear(&mut self) {
        if let Some(clip) = self.inner.as_mut() {
            let _ = clip.clear();
        }
    }

    /// The lazy handle, and the one place a backend is ever chosen. `arboard`
    /// picks Wayland or X11 here — and a compositor that doesn't speak
    /// wlr-data-control is NOT an error: arboard quietly falls back to X11, so
    /// only a session with neither leaves us with the `Err` the callers toast.
    /// (Its fallback grumbles through the `log` crate; nothing installs a
    /// logger, so it can't scribble over the frame.)
    fn handle(&mut self) -> Result<&mut arboard::Clipboard, String> {
        if self.inner.is_none() {
            self.inner = Some(arboard::Clipboard::new().map_err(|e| e.to_string())?);
        }
        Ok(self.inner.as_mut().expect("just created"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Item, ItemField, SecretString};

    /// RFC 6238 appendix B seed (see `totp::tests`).
    const SEED: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";

    fn item_with(fields: Vec<ItemField>) -> Item {
        let mut item = Item::new("login", "Acme");
        item.fields = fields;
        item
    }

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

    #[test]
    fn a_totp_field_copies_the_code_never_the_seed() {
        let item = item_with(vec![field("totp", FieldType::Totp, SEED, true)]);
        let payload = copy_payload(&item, 0, 59).expect("a code");
        assert_eq!(&*payload, "287082", "the live code, not the seed");
        assert_ne!(&*payload, SEED);
        // …and it rotates with time, so no copy is ever a stand-in for the seed
        let later = copy_payload(&item, 0, 1_111_111_109).expect("a code");
        assert_eq!(&*later, "081804");
    }

    #[test]
    fn a_totp_keyed_text_field_still_copies_the_code() {
        // hand-added `2fa totp` rows arrive as plain Text; the key decides too
        let item = item_with(vec![field("totp_seed", FieldType::Text, SEED, true)]);
        assert_eq!(&*copy_payload(&item, 0, 59).unwrap(), "287082");
    }

    #[test]
    fn other_fields_copy_their_value_and_empty_ones_copy_nothing() {
        let item = item_with(vec![
            field("password", FieldType::Password, "S3CRETXYZ", true),
            field("username", FieldType::Username, "", false),
        ]);
        assert_eq!(&*copy_payload(&item, 0, 59).unwrap(), "S3CRETXYZ");
        assert!(copy_payload(&item, 1, 59).is_none(), "an empty field copies nothing");
        assert!(copy_payload(&item, 9, 59).is_none(), "no such row");
    }

    #[test]
    fn a_wipe_only_ever_erases_our_own_copy() {
        // the timer fires 20 s later: by then the user may well have copied
        // something else — an address, a paragraph, another app's password.
        assert!(should_clear(Some("S3CRETXYZ"), "S3CRETXYZ"), "still ours: wipe it");
        assert!(!should_clear(Some("a shopping list"), "S3CRETXYZ"), "somebody else's");
        assert!(!should_clear(Some(""), "S3CRETXYZ"), "already emptied");
        // …and an unreadable clipboard is never erased blind: we cannot tell
        // whose bytes are on it, and guessing wrong destroys the user's data.
        assert!(!should_clear(None, "S3CRETXYZ"));
    }

    #[test]
    fn a_totp_row_holding_garbage_falls_back_to_its_text() {
        let item = item_with(vec![field("totp", FieldType::Totp, "not base32!!", false)]);
        assert_eq!(&*copy_payload(&item, 0, 59).unwrap(), "not base32!!");
    }
}
