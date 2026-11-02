//! Colour palette for the TUI — the "BRASS LEDGER" identity.
//!
//! A vault is a ledger: warm-black paper, brass fittings, entries on ruled
//! lines. One structural hue (brass) carries chrome and focus so the favicons —
//! the only saturated pixels on screen — become the wayfinding. Cool steel is
//! reserved for *your data* (values, emails, keys) so "your stuff" stays
//! visually distinct from "the app". Red means one thing only: destruction.
//!
//! The identity is **hue-parametric**: the warm neutral scale (paper, surface,
//! rules, text) and the semantic colours (steel data, green success, orange
//! warning, red danger) are *constant across themes* so meaning and mood never
//! shift; only the accent family (focus/keys/titles/★) remaps. Swap brass for
//! phosphor green or lavender on the `settings` page and every role still holds.
//!
//! kitty-only → truecolor is guaranteed; no 256-colour fallback is needed.

use ratatui::style::Color;
use serde::{Deserialize, Serialize};

/// A complete set of UI colours. Cheap to copy (all fields are `Color`), so it
/// is passed by value to the rendering helpers.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    /// The paper: warm black. Painted only when the background is `opaque`.
    pub bg: Color,
    /// Raised surface — modal / float fill, one step above the paper.
    pub surface: Color,
    /// Selected-row background: a brass-tinted band (the only row treatment).
    pub selection: Color,
    /// Primary text: names, the things you read first. Warm off-white.
    pub text: Color,
    /// Secondary text: labels, subtitles, inactive tabs.
    pub muted: Color,
    /// Faintest text: hints, empty states, placeholders.
    pub faint: Color,
    /// Accent — focus, keys, titles, ★, active chip. The one remappable hue.
    pub accent: Color,
    /// Brighter accent — selected text, the active/armed chip.
    pub accent_hi: Color,
    /// A quieter shade of the accent (unfocused markers, dimmed focus).
    pub accent_soft: Color,
    /// Your data: values, emails, usernames, keys. Cool steel (semantic).
    pub steel: Color,
    /// Success — toasts, checks. Soft green (semantic, constant).
    pub green: Color,
    /// Warning — weak/expiring/lint. Orange, deliberately *not* brass (semantic).
    pub warning: Color,
    /// Danger — destructive actions only. Red (semantic, constant).
    pub danger: Color,
    /// Fill behind an armed destructive confirm. Deep red-black (semantic).
    pub danger_surface: Color,
    /// Hairline: rules, dot-leaders, inactive borders. The quietest structure.
    pub border: Color,
    /// Focused pane title / active input surface — the accent.
    pub border_focus: Color,
    /// The vertical divider between the list and the docked detail — one step
    /// darker than the hairline so it separates without shouting.
    pub divider: Color,
    /// Dark text ON an accent-filled surface (the active chip pill).
    pub on_accent: Color,
    /// Keybar key-chip background (a slightly raised warm block).
    pub chip_bg: Color,
    /// Keybar key-chip text — a lighter cousin of the accent.
    pub chip_fg: Color,
}

// ── The warm neutral scale — the ledger paper. Constant across every theme. ──
const BG: Color = Color::Rgb(0x0F, 0x0D, 0x0A);
const SURFACE: Color = Color::Rgb(0x17, 0x13, 0x10);
const SELECTION: Color = Color::Rgb(0x2A, 0x22, 0x13);
const TEXT: Color = Color::Rgb(0xE8, 0xE2, 0xD3);
const MUTED: Color = Color::Rgb(0x9C, 0x93, 0x84);
const FAINT: Color = Color::Rgb(0x6A, 0x62, 0x53);
const HAIRLINE: Color = Color::Rgb(0x3E, 0x38, 0x2E);

// ── Semantic colours — the same in every theme so meaning never shifts. ──
const STEEL: Color = Color::Rgb(0x8F, 0xB0, 0xCE);
const GREEN: Color = Color::Rgb(0x7F, 0xC6, 0x6E);
const WARNING: Color = Color::Rgb(0xE0, 0x8F, 0x3F);
const DANGER: Color = Color::Rgb(0xE2, 0x5D, 0x5D);
const DANGER_SURFACE: Color = Color::Rgb(0x24, 0x11, 0x11);
const DIVIDER: Color = Color::Rgb(0x24, 0x1F, 0x18);
const ON_ACCENT: Color = Color::Rgb(0x17, 0x10, 0x06);
const CHIP_BG: Color = Color::Rgb(0x24, 0x1F, 0x17);

/// The selectable accent families shown on the `settings` page. Each remaps
/// only the accent hue (focus/keys/titles/★) over the shared warm paper.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    /// The identity: brass fittings on warm-black paper.
    #[default]
    Brass,
    Lavender,
    Sky,
    Rose,
    Sand,
    /// The brand green (the pixel-Saturn's colour) — a "phosphor" accent.
    Matrix,
}

impl Theme {
    pub const ALL: [Theme; 6] =
        [Theme::Brass, Theme::Lavender, Theme::Sky, Theme::Rose, Theme::Sand, Theme::Matrix];

    /// Name shown in the picker and stored in `settings.toml`.
    pub fn name(self) -> &'static str {
        match self {
            Theme::Brass => "Brass",
            Theme::Lavender => "Lavender",
            Theme::Sky => "Sky",
            Theme::Rose => "Rose",
            Theme::Sand => "Sand",
            Theme::Matrix => "Matrix",
        }
    }

    /// Parses a theme name case-insensitively.
    pub fn from_name(s: &str) -> Option<Theme> {
        Theme::ALL.iter().copied().find(|t| t.name().eq_ignore_ascii_case(s))
    }

    /// The full colour set for this theme. Only the accent family varies; the
    /// warm neutral scale and the semantic colours are shared constants.
    pub fn palette(self) -> Palette {
        // (accent, accent-hi, accent-soft, chip text — a lighter cousin)
        let (accent, accent_hi, accent_soft, chip_fg) = match self {
            // brass — the identity
            Theme::Brass => (
                Color::Rgb(0xD9, 0xA8, 0x55),
                Color::Rgb(0xF4, 0xCE, 0x82),
                Color::Rgb(0x8A, 0x6E, 0x32),
                Color::Rgb(0xE3, 0xB9, 0x6B),
            ),
            Theme::Lavender => (
                Color::Rgb(0xB7, 0x94, 0xF4),
                Color::Rgb(0xD9, 0xC4, 0xFF),
                Color::Rgb(0x6E, 0x5A, 0x9A),
                Color::Rgb(0xC6, 0xAA, 0xF8),
            ),
            Theme::Sky => (
                Color::Rgb(0x8C, 0xBE, 0xF0),
                Color::Rgb(0xC4, 0xDE, 0xFF),
                Color::Rgb(0x56, 0x76, 0x9E),
                Color::Rgb(0xA4, 0xCC, 0xF4),
            ),
            Theme::Rose => (
                Color::Rgb(0xEE, 0xA0, 0xC0),
                Color::Rgb(0xFF, 0xC8, 0xDE),
                Color::Rgb(0xA4, 0x6E, 0x86),
                Color::Rgb(0xF4, 0xB4, 0xCE),
            ),
            Theme::Sand => (
                Color::Rgb(0xEA, 0xC6, 0x92),
                Color::Rgb(0xFF, 0xE0, 0xB0),
                Color::Rgb(0xA4, 0x88, 0x4E),
                Color::Rgb(0xF0, 0xD2, 0xA4),
            ),
            // a touch brighter than the semantic GREEN so focus ≠ success
            Theme::Matrix => (
                Color::Rgb(0x96, 0xDC, 0xA5),
                Color::Rgb(0xC4, 0xF0, 0xCE),
                Color::Rgb(0x5A, 0x8A, 0x66),
                Color::Rgb(0xAA, 0xE4, 0xB6),
            ),
        };
        Palette {
            bg: BG,
            surface: SURFACE,
            selection: SELECTION,
            text: TEXT,
            muted: MUTED,
            faint: FAINT,
            accent,
            accent_hi,
            accent_soft,
            steel: STEEL,
            green: GREEN,
            warning: WARNING,
            danger: DANGER,
            danger_surface: DANGER_SURFACE,
            border: HAIRLINE,
            border_focus: accent,
            divider: DIVIDER,
            on_accent: ON_ACCENT,
            chip_bg: CHIP_BG,
            chip_fg,
        }
    }
}
