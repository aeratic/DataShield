//! TerminalDataShield — TUI front-end (ratatui).
//!
//! Shares the crypto and vault logic with the CLI (the `crypto`, `vault`,
//! `paths` modules are the same code as Datashield-FCLI).

// The shared core is a superset of what the TUI uses so far.
#![allow(dead_code)]

// the localhost agent API: a read-only search endpoint, open only while a
// vault is, bound to 127.0.0.1 and behind a per-session bearer token.
mod api;
mod app;
mod blur;
// the user's browser: which one is the default, and how a URL is handed to it.
mod browser;
mod chrome;
// the system clipboard: what a copy carries + its timed wipe.
mod clipboard;
mod content_ext;
mod favicon;
mod i18n;
// image fields: a picture file in, a bounded thumbnail out.
mod image_field;
// keymap drives the single-key verbs + the keybar/cheatsheet legend (one source).
mod keymap;
mod logo;
// the Memorandum: a chat with yourself, stored inside the vault under `x_memo`.
mod memo;
mod palette;
mod paths;
// local semantic search: the on-device embedding model + the two searches.
mod rag;
mod registry;
mod settings;
mod settings_ui;
// a one-line text buffer with a real insertion caret — shared by every input.
mod textinput;
mod theme;
mod ui;

// the shared core, re-exported so the TUI's modules keep their `crate::`
// paths (and so a GUI can read the same code with the same names)
pub use datashield_core::{config, crypto, model, sync, totp, vault};

use anyhow::Result;
use app::App;
use ratatui::crossterm::{
    event::{
        self, Event, KeyEventKind, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
        PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, supports_keyboard_enhancement, EnterAlternateScreen,
        LeaveAlternateScreen,
    },
};
use ratatui::crossterm::cursor::Show;
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io::{self, Stdout};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Set while the kitty keyboard protocol is pushed onto the terminal, so both
/// exit paths know to pop it again — the same shape [`blur`] uses for the
/// translucency it turns on: the flags belong to the TERMINAL and outlive us,
/// so a crash that skipped the pop would leave the user's shell reporting keys
/// nobody there understands.
static KEYBOARD_ENHANCED: AtomicBool = AtomicBool::new(false);

fn main() -> Result<()> {
    let paths = paths::resolve()?;
    let mut app = App::new(paths);
    // With `api_vault_unlock` saved on, the agent API must be listening BEFORE
    // any vault is opened — that toggle's whole point is an agent unlocking a
    // vault of its own from the Access page. Asked here and not inside
    // `App::new` on purpose: constructing an `App` must never bind a socket
    // (the test suite constructs hundreds of them), so the decision to listen
    // belongs to the binary, once.
    app.sync_api_server();

    // In DataShield's own window (the launcher sets `DATASHIELD_OWN_TERM`) the
    // terminal IS kitty: no need to ask it anything. Asking is not free — the
    // keyboard query and the graphics query below are answered on stdin, and
    // each has its own reader (crossterm's event thread vs ratatui-image's raw
    // read). When kitty answers late (its first start takes over a second) the
    // readers take each other's replies: one blocks on stdin for good, the
    // first frame never draws, and from then on that stuck reader swallows
    // every other keystroke (the "press Esc twice" symptom). So the own-window
    // path skips both queries; foreign terminals still get asked, but the
    // graphics query goes FIRST, before crossterm's reader exists.
    let own_term = std::env::var_os("DATASHIELD_OWN_TERM").is_some();
    // query the terminal for its graphics protocol (kitty/sixel/iTerm2) so the
    // item list can render crisp favicons; None => half-block/monogram fallback.
    app.picker = if own_term {
        Some(kitty_picker())
    } else {
        ratatui_image::picker::Picker::from_query_stdio().ok()
    };
    let (mut terminal, shift_enter_ok) = setup_terminal(own_term)?;
    // whether this terminal can tell Shift+⏎ apart from ⏎ at all (see
    // `setup_terminal`) — the Memorandum's keybar promises the one that works.
    app.shift_enter_ok = shift_enter_ok;
    let result = run(&mut terminal, &mut app);
    // undo any kitty translucency we turned on, then restore the terminal
    blur::restore_on_exit();
    restore_terminal(&mut terminal)?;
    result
}

/// Best-effort restore of the terminal from anywhere (used by the panic hook
/// so a crash doesn't leave the shell in raw mode / the alternate screen).
fn restore_terminal_raw() {
    blur::restore_on_exit();
    pop_keyboard_enhancement();
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
}

/// Gives the keyboard protocol back exactly once, if we ever took it. Both the
/// normal restore and the panic hook call it; the swap makes the second call a
/// no-op.
fn pop_keyboard_enhancement() {
    if KEYBOARD_ENHANCED.swap(false, Ordering::Relaxed) {
        let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
    }
}

/// Sets the terminal up, and answers whether it can spell out the modifiers on
/// ⏎ (see the push below).
/// A picker for DataShield's own kitty window, built without a terminal query:
/// the protocol is known, and the cell size comes from the tty ioctl (kitty
/// reports pixel dimensions there). A zero answer falls back to a typical cell.
fn kitty_picker() -> ratatui_image::picker::Picker {
    use ratatui::crossterm::terminal::window_size;
    use ratatui_image::picker::{Picker, ProtocolType};
    let font = window_size()
        .ok()
        .filter(|w| w.columns > 0 && w.rows > 0 && w.width > 0 && w.height > 0)
        .map(|w| (w.width / w.columns, w.height / w.rows))
        .unwrap_or((8, 16));
    let mut picker = Picker::from_fontsize(font);
    picker.set_protocol_type(ProtocolType::Kitty);
    picker
}

/// Sets the terminal up, and answers whether it can spell out the modifiers on
/// ⏎ (see the push below). `known_kitty` skips the capability query (see
/// `main`): kitty speaks its own keyboard protocol, so the flag is just pushed.
fn setup_terminal(known_kitty: bool) -> Result<(Terminal<CrosstermBackend<Stdout>>, bool)> {
    // restore the terminal before a panic prints its message, otherwise the
    // user's shell is left corrupted (no echo, garbled prompt).
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal_raw();
        default_hook(info);
    }));

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    // A legacy terminal sends the SAME byte for ⏎ and Shift+⏎, so the chat's
    // "newline" chord simply cannot exist on one. kitty's keyboard protocol
    // spells the modifier out: ask whether this terminal speaks it and, only
    // then, push the one flag that disambiguates (never the event-type
    // reporting — the loop below still wants Press and nothing else). The
    // answer travels to the keybar, which never advertises a key that cannot
    // be delivered; Alt+⏎ is the fallback everywhere, and always accepted.
    let enhanced = (known_kitty || supports_keyboard_enhancement().unwrap_or(false))
        && execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )
        .is_ok();
    KEYBOARD_ENHANCED.store(enhanced, Ordering::Relaxed);
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    terminal.hide_cursor()?;
    Ok((terminal, enhanced))
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    pop_keyboard_enhancement();
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

fn run(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> Result<()> {
    let tick = Duration::from_millis(120);
    let mut last_tick = Instant::now();
    loop {
        terminal.draw(|f| ui::draw(f, app))?;

        let timeout = tick.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press && app.on_key(key.code, key.modifiers) {
                    return Ok(());
                }
            }
        }
        if last_tick.elapsed() >= tick {
            app.on_tick();
            last_tick = Instant::now();
        }
    }
}
