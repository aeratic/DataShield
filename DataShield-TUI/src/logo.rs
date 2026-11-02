//! The header logo, loaded from an editable `logo.txt`.
//!
//! Like `commands.toml`, a default is embedded in the binary and seeded to the
//! data directory on first run so it can be customised. The user may put any
//! text/ASCII/Unicode there; lines starting with `;` are treated as comments,
//! and leading/trailing blank lines are trimmed. Blanking the file (or turning
//! off `show_logo`) hides the header entirely.

use std::fs;
use std::path::Path;

/// The built-in logo, also written to disk on first run.
const DEFAULT_LOGO: &str = include_str!("../logo.txt");

/// The built-in COLOUR logo: a small RGBA grid (row-major) of the pixel-Saturn,
/// rendered with half-blocks so two pixel rows share one terminal row. Shown in
/// the header whenever the user hasn't provided their own text logo.
const COLOR_RGBA: &[u8] = include_bytes!("../logo_color.rgba");
/// Pixel dimensions of [`COLOR_RGBA`]. Terminal rows = `COLOR_HEIGHT / 2`.
pub const COLOR_WIDTH: u16 = 16;
pub const COLOR_HEIGHT: u16 = 6;

/// The RGBA pixel at `(col, row)` of the built-in colour logo (transparent if
/// out of bounds).
pub fn color_pixel(col: u16, row: u16) -> [u8; 4] {
    let (w, h) = (COLOR_WIDTH as usize, COLOR_HEIGHT as usize);
    let (c, r) = (col as usize, row as usize);
    if c >= w || r >= h {
        return [0, 0, 0, 0];
    }
    let i = (r * w + c) * 4;
    [COLOR_RGBA[i], COLOR_RGBA[i + 1], COLOR_RGBA[i + 2], COLOR_RGBA[i + 3]]
}

/// Loads the logo lines from `path`, seeding the embedded default the first
/// time. Never fails: an unreadable file falls back to the embedded default.
pub fn load(path: &Path) -> Vec<String> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(_) => {
            seed(path);
            DEFAULT_LOGO.to_string()
        }
    };
    parse(&text)
}

/// Strips `;` comment lines and leading/trailing blank lines. Interior blanks
/// are kept (they're part of the art); only the right edge is trimmed so the
/// block can be centred as a unit without its left-side shape shifting.
fn parse(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = text
        .lines()
        .filter(|l| !l.trim_start().starts_with(';'))
        .map(|l| l.trim_end().to_string())
        .collect();
    while lines.first().is_some_and(|l| l.trim().is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    // a header logo is never this tall; cap it so downstream `len() as u16`
    // can't wrap on a pathological hand-edited file.
    lines.truncate(256);
    lines
}

fn seed(path: &Path) {
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let _ = fs::write(path, DEFAULT_LOGO);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_default_is_comment_only_so_the_colour_logo_shows() {
        // the shipped logo.txt is only comments → no text logo → the built-in
        // colour Saturn is used by default.
        assert!(parse(DEFAULT_LOGO).is_empty());
    }

    #[test]
    fn colour_logo_data_matches_its_dimensions_and_has_pixels() {
        assert_eq!(
            COLOR_RGBA.len(),
            COLOR_WIDTH as usize * COLOR_HEIGHT as usize * 4
        );
        let opaque = (0..COLOR_HEIGHT).any(|r| (0..COLOR_WIDTH).any(|c| color_pixel(c, r)[3] > 50));
        assert!(opaque, "the colour logo has visible pixels");
    }

    #[test]
    fn comments_and_edge_blanks_are_stripped_but_shape_is_kept() {
        let logo = parse("; a comment\n\n  ▄██▄\n ████ \n\n; trailing\n\n");
        assert_eq!(logo, vec!["  ▄██▄".to_string(), " ████".to_string()]);
    }

    #[test]
    fn a_blank_file_yields_no_logo() {
        assert!(parse("; only comments\n\n\n").is_empty());
    }
}
