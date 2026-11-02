//! Best-effort terminal translucency, currently only for kitty.
//!
//! A TUI can't turn on window blur/transparency portably: those are properties
//! of the terminal emulator window and the OS compositor, not of the character
//! grid we draw into. The one thing we *can* do live and reversibly, on kitty,
//! is ask it (via remote control) to make its window translucent with
//! `kitty @ set-background-opacity`. The visible blur still comes from the
//! user's `kitty.conf` (`background_blur`); we only nudge the opacity.
//!
//! Requirements on the kitty side: `allow_remote_control yes` and
//! `dynamic_background_opacity yes`. When those aren't set, kitty refuses and
//! we surface its message rather than pretending it worked.

use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

/// Set while we hold kitty at a translucent opacity, so the exit/panic paths
/// know to restore it (kitty keeps the opacity after we quit otherwise).
static APPLIED: AtomicBool = AtomicBool::new(false);

/// The terminal we're running under, as far as translucency support goes.
enum Terminal {
    Kitty,
    Other,
}

/// The result of a translucency attempt, so the caller can give honest
/// feedback.
pub enum Outcome {
    /// kitty accepted the change.
    Applied,
    /// Not running under kitty — nothing we can do from here.
    NotKitty,
    /// kitty was detected but the command failed (carries its first message
    /// line, e.g. remote control disabled).
    Failed(String),
}

/// Opacity used for the translucent state (0 = fully transparent, 1 = opaque).
const TRANSLUCENT: &str = "0.85";
const OPAQUE: &str = "1.0";

fn detect() -> Terminal {
    // Inside kitty this variable is always set for the running window.
    if std::env::var_os("KITTY_WINDOW_ID").is_some() {
        return Terminal::Kitty;
    }
    if std::env::var("TERM").map(|t| t.contains("kitty")).unwrap_or(false) {
        return Terminal::Kitty;
    }
    Terminal::Other
}

/// True when we could plausibly control translucency (i.e. we're on kitty).
pub fn is_supported() -> bool {
    matches!(detect(), Terminal::Kitty)
}

/// Makes the terminal window translucent (`on`) or fully opaque again
/// (`!on`). A no-op returning [`Outcome::NotKitty`] off kitty.
pub fn set_translucent(on: bool) -> Outcome {
    if !is_supported() {
        return Outcome::NotKitty;
    }
    let value = if on { TRANSLUCENT } else { OPAQUE };
    match Command::new("kitty")
        .args(["@", "set-background-opacity", value])
        .output()
    {
        Ok(out) if out.status.success() => {
            APPLIED.store(on, Ordering::Relaxed);
            Outcome::Applied
        }
        Ok(out) => {
            let msg = String::from_utf8_lossy(&out.stderr);
            let first = msg
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or("kitty remote control failed")
                .to_string();
            Outcome::Failed(first)
        }
        Err(e) => Outcome::Failed(e.to_string()),
    }
}

/// Restores full opacity if we ever made the window translucent. Safe to call
/// from the exit path and the panic hook (a no-op otherwise); best-effort.
pub fn restore_on_exit() {
    if APPLIED.swap(false, Ordering::Relaxed) {
        let _ = Command::new("kitty")
            .args(["@", "set-background-opacity", OPAQUE])
            .output();
    }
}
