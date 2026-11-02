//! Rendering (sketch 2): transparent background, vault list on the left, the
//! contextual `~` panel on the right (page help / language chooser / …), a
//! status bar, and the command input at the bottom. The focused panel gets an
//! accented border.

#[allow(unused_imports)]
use crate::content_ext::VaultContentExt;

use crate::app::{App, FormPurpose, FormState, Page, PanelView, PickerKind, StatusKind};
use crate::i18n::{self, Key, Lang};
use crate::registry::Action;
use crate::settings::{Background, Blur};
use crate::settings_ui::{self, Row, SettingsState};
use crate::theme::Palette;
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
    Frame,
};

/// Every secret mask is exactly this many bullets, always — so a stored
/// secret's length never leaks to anyone looking at the screen (BRASS LEDGER
/// security contract). Feedback while *typing* a new secret comes from the
/// strength meter, not from the mask growing.
const MASK_LEN: usize = 8;

pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();

    // Paint a solid backdrop only when the user chose an opaque background;
    // otherwise leave every cell untouched so the terminal (and its own
    // transparency/blur) shows through. No widget below sets a background, so
    // this single fill persists behind the whole UI.
    if app.background_opaque() {
        f.render_widget(
            Block::default().style(Style::default().bg(app.palette().bg)),
            area,
        );
    }

    // The full-screen lock replaces everything (fig-02): brass rain + a
    // centered passphrase box. Any key focuses the passphrase.
    if let Some(lock) = &app.locked {
        draw_lock(f, area, app, lock);
        return;
    }

    // the wallpaper: barely-there animation behind every screen (content
    // paints over it — it only lives in empty cells). Effect per settings.
    if app.backdrop_on() {
        // draft-aware: cycling the effect / intensity in settings previews live
        match app.draft_settings().backdrop_fx {
            crate::settings::BackdropFx::Rain => draw_backdrop_rain(f, area, app),
            crate::settings::BackdropFx::Waves => draw_backdrop_waves(f, area, app),
        }
    }

    // Two-row chrome (BRASS LEDGER): a one-row top-bar (context · locale) and a
    // one-row keybar at the bottom. That's the whole budget — the free-text
    // command line is gone (commands come from the `:` palette), and the pixel
    // logo moved to the lock screen.
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // top-bar (context · locale)
            Constraint::Length(1), // hairline under the top-bar
            Constraint::Min(1),    // body
            Constraint::Length(1), // keybar / transient toast
        ])
        .split(area);

    let p = app.palette();
    draw_topbar(f, rows[0], app);
    // the full-width hairline that seats the top-bar (mockups: a 1px rule)
    put_rule(f, rows[1].x, rows[1].y, rows[1].width, p);

    match app.page {
        // Access (fig-01) is a single full-width pane — no split, no idle canvas.
        Page::Access => draw_access(f, rows[2], app),
        // In a vault (fig-03): chips row full-width, then list │ docked detail.
        Page::Vault => draw_vault_body(f, rows[2], app),
    }
    draw_status(f, rows[3], app);

    // ---- floating layers (each dims the screen; a box = an Esc-able layer) --
    if let Some(state) = &app.settings_ui {
        dim_buffer(f);
        draw_settings(f, area, app, state, true, true);
    }
    if let PanelView::Picker(kind) = app.active_view() {
        dim_buffer(f);
        draw_picker(f, area, app, kind);
    }
    if app.settings_ui.is_none() {
        if let Some(form) = &app.form {
            dim_buffer(f);
            let mrect = modal_rect(area, form.fields.len());
            draw_form(f, mrect, app, form, true, true);
        }
    }
    // the command palette is the topmost layer when open.
    if let Some(pal) = &app.palette {
        dim_buffer(f);
        draw_palette(f, area, app, pal);
    }
    // the item-level delete confirm floats above everything.
    if let Some((_, label)) = &app.pending_confirm {
        dim_buffer(f);
        draw_confirm(f, area, app, label);
    }
    // the ? cheatsheet floats above everything.
    if app.cheatsheet {
        dim_buffer(f);
        draw_cheatsheet(f, area, app);
    }
}

/// Dims every cell toward the warm-black paper — the backdrop for a floating
/// modal, so "a box = a layer over a dimmed screen" reads instantly.
fn dim_buffer(f: &mut Frame) {
    let buf = f.buffer_mut();
    for cell in &mut buf.content {
        cell.fg = dim_toward_paper(cell.fg);
        cell.bg = dim_toward_paper(cell.bg);
        cell.modifier.remove(Modifier::BOLD | Modifier::ITALIC);
    }
}

/// Blends a colour ~62% toward the paper (`#0F0D0A`); non-RGB colours (rare
/// now) pass through unchanged.
fn dim_toward_paper(c: Color) -> Color {
    match c {
        Color::Rgb(r, g, b) => Color::Rgb(
            (r as u16 * 38 / 100 + 6) as u8,
            (g as u16 * 38 / 100 + 5) as u8,
            (b as u16 * 38 / 100 + 4) as u8,
        ),
        other => other,
    }
}

/// A centered modal rect sized to hold `n_fields` (~6 rows each) plus chrome,
/// clamped to the screen: width = min(58, cols−8).
fn modal_rect(full: Rect, n_fields: usize) -> Rect {
    let w = 58u16.min(full.width.saturating_sub(8)).max(20);
    // ~1 row per field + a helper row for the focused one + title/warning/error.
    let want = 4 + n_fields as u16 + 4;
    let h = want.min(full.height.saturating_sub(4)).max(6);
    let x = full.x + full.width.saturating_sub(w) / 2;
    let y = full.y + full.height.saturating_sub(h) / 2;
    Rect { x, y, width: w, height: h }
}

/// A floating-layer frame: clears the rect and draws a rounded brass box with
/// `title`, returning the inner rect. The caller has already dimmed the screen
/// behind it, so the box reads as a layer you can `Esc` out of.
fn draw_modal_frame(f: &mut Frame, area: Rect, title: &str, p: Palette) -> Rect {
    // callers size a modal to a minimum and centre it arithmetically, so on a
    // pathologically small terminal the box can come out larger than the
    // screen: clip it before the first cell write (callers bail on the tiny
    // inner rect that comes back).
    let area = area.intersection(f.area());
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(p.accent))
        .title(Span::styled(format!(" {title} "), accent_bold(p)));
    let inner = block.inner(area);
    f.render_widget(block, area);
    inner
}

/// The `:` command palette — a floating fuzzy command finder over the dimmed
/// screen (fig-08): a `: query▌` prompt, then ranked hits with their
/// description inline and a `guarded` tag where a command opens a protected
/// modal; the highlighted row carries the `▌` brass bar. The caller has already
/// dimmed the screen.
fn draw_palette(f: &mut Frame, full: Rect, app: &App, pal: &crate::palette::Palette) {
    let p = app.palette();
    let lang = app.effective_lang();
    let matches = app.palette_matches();
    let w = 62u16.min(full.width.saturating_sub(6)).max(24);
    let shown = (matches.len() as u16).min(16);
    let h = (shown + 4).min(full.height.saturating_sub(2)).max(5);
    let x = full.x + full.width.saturating_sub(w) / 2;
    let y = full.y + full.height.saturating_sub(h) / 3; // sit a little above center
    let inner = draw_modal_frame(f, Rect { x, y, width: w, height: h }, "commands", p);
    if inner.width < 6 || inner.height < 3 {
        return;
    }
    let cx = inner.x;
    let cw = inner.width;
    // the ": qu▌ery" prompt — the caret sits between the chars it is on, and the
    // window scrolls to keep it in view when the query outgrows the box.
    let (before, after) = pal.query.view(cw.saturating_sub(2) as usize);
    let mut prompt = vec![Span::styled(": ", accent_bold(p)), Span::styled(before, Style::default().fg(p.text))];
    if app.cursor_on() {
        prompt.push(Span::styled("▌", Style::default().fg(p.accent)));
    }
    prompt.push(Span::styled(after, Style::default().fg(p.text)));
    f.render_widget(Paragraph::new(Line::from(prompt)), Rect { x: cx, y: inner.y, width: cw, height: 1 });
    // a hairline under the prompt
    put_rule(f, cx, inner.y + 1, cw, p);
    // ranked hits
    let list_y = inner.y + 2;
    let foot_y = inner.y + inner.height.saturating_sub(1);
    for (i, cmd) in matches.iter().enumerate().take((foot_y.saturating_sub(list_y)) as usize) {
        let ry = list_y + i as u16;
        let selected = i == pal.selected;
        let bg = if selected { Some(p.selection) } else { None };
        if selected {
            puts(f, cx, ry, &" ".repeat(cw as usize), Style::default().bg(p.selection), inner);
        }
        let with_bg = |s: Style| match bg {
            Some(b) => s.bg(b),
            None => s,
        };
        let marker = if selected { "▌ " } else { "  " };
        let name_style = if selected {
            Style::default().fg(p.accent_hi).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.accent)
        };
        // "guarded" = the command opens a protected modal (has a secret field).
        let guarded = cmd.params.iter().any(|pm| pm.secret);
        let tag = if guarded { "guarded" } else { "" };
        let tag_w = tag.len();
        let name = cmd.name.clone();
        let desc_avail = (cw as usize).saturating_sub(2 + name.chars().count() + 2 + tag_w + 2);
        let desc = truncate(cmd.description(lang), desc_avail.max(1));
        let mut spans = vec![
            Span::styled(marker, with_bg(Style::default().fg(p.accent))),
            Span::styled(name, with_bg(name_style)),
            Span::styled("  ", with_bg(Style::default())),
            Span::styled(desc, with_bg(Style::default().fg(p.muted))),
        ];
        if guarded {
            let pad = (cw as usize).saturating_sub(2 + spans_w(&spans) + tag_w);
            spans.push(Span::styled(" ".repeat(pad), with_bg(Style::default())));
            spans.push(Span::styled(tag, with_bg(Style::default().fg(p.warning))));
        }
        let mut para = Paragraph::new(Line::from(spans));
        if let Some(b) = bg {
            para = para.style(Style::default().bg(b));
        }
        f.render_widget(para, Rect { x: cx, y: ry, width: cw, height: 1 });
    }
    if matches.is_empty() {
        puts(f, cx + 2, list_y, "no match", Style::default().fg(p.faint).add_modifier(Modifier::ITALIC), inner);
    }
    // footer
    puts(
        f,
        cx,
        foot_y,
        "↑↓ · ⏎ run · Esc — ranked by your usage",
        Style::default().fg(p.faint),
        inner,
    );
}

/// The item-level delete confirm (fig-07, rung 1): a small floating brass box
/// — `delete 'X'?` / `this cannot be undone` / `y delete · n keep (default)`.
/// No `◂No▸` flipper: arrow keys can never point at "destroy".
fn draw_confirm(f: &mut Frame, full: Rect, app: &App, label: &str) {
    let p = app.palette();
    let lang = app.effective_lang();
    let yes = app.confirm_key();
    let title = match lang {
        Lang::En => "delete item",
        Lang::It => "elimina elemento",
    };
    let q = match lang {
        Lang::En => format!("delete '{label}' ?"),
        Lang::It => format!("elimina '{label}' ?"),
    };
    let keys = match lang {
        Lang::En => "delete · n keep (default)".to_string(),
        Lang::It => "elimina · n mantieni (default)".to_string(),
    };
    let warn = i18n::t(lang, &Key::DeleteItemWarning);
    let w = 46u16.min(full.width.saturating_sub(6)).max(24);
    let h = 6u16.min(full.height.saturating_sub(2)).max(5);
    let x = full.x + full.width.saturating_sub(w) / 2;
    let y = full.y + full.height.saturating_sub(h) / 2;
    let inner = draw_modal_frame(f, Rect { x, y, width: w, height: h }, title, p);
    let cx = inner.x + 1;
    let cw = inner.width.saturating_sub(1);
    let line = |f: &mut Frame, row: u16, span: Span<'static>| {
        if row < inner.y + inner.height {
            f.render_widget(Paragraph::new(Line::from(span)), Rect { x: cx, y: row, width: cw, height: 1 });
        }
    };
    line(f, inner.y, Span::styled(q, Style::default().fg(p.text).add_modifier(Modifier::BOLD)));
    line(f, inner.y + 1, Span::styled(warn, Style::default().fg(p.danger)));
    // the yes-key chip is brass, "keep (default)" muted; pinned to the last row.
    let keyy = inner.y + inner.height.saturating_sub(1);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!("{yes} "), accent_bold(p)),
            Span::styled(keys, Style::default().fg(p.muted)),
        ])),
        Rect { x: cx, y: keyy, width: cw, height: 1 },
    );
}

/// One tap of the shader's value noise: `fract(sin(dot)*k)` hash on the integer
/// lattice, smoothstep-blended. Ported 1:1 from the "Datashield Background"
/// WebGL fragment shader so the terminal wallpaper reads the same.
fn shash(x: f32, y: f32) -> f32 {
    let s = (x * 127.1 + y * 311.7).sin() * 43758.545;
    s - s.floor()
}
fn snoise(x: f32, y: f32) -> f32 {
    let (xi, yi) = (x.floor(), y.floor());
    let (xf, yf) = (x - xi, y - yi);
    let (u, v) = (xf * xf * (3.0 - 2.0 * xf), yf * yf * (3.0 - 2.0 * yf));
    let a = shash(xi, yi);
    let b = shash(xi + 1.0, yi);
    let c = shash(xi, yi + 1.0);
    let d = shash(xi + 1.0, yi + 1.0);
    (a + (b - a) * u) + ((c + (d - c) * u) - (a + (b - a) * u)) * v
}
/// Five octaves of [`snoise`] with the shader's rotate-and-scale lacunarity.
fn sfbm(mut x: f32, mut y: f32) -> f32 {
    let mut val = 0.0;
    let mut a = 0.5;
    for _ in 0..5 {
        val += a * snoise(x, y);
        let (nx, ny) = (0.8 * x - 0.6 * y, 0.6 * x + 0.8 * y);
        x = nx * 2.0 + 3.7;
        y = ny * 2.0 + 1.3;
        a *= 0.5;
    }
    val
}
/// GLSL `smoothstep(e0, e1, x)`.
fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The pixel-wave wallpaper — a faithful terminal port of the "Datashield
/// Background" shader: coarse pixel blocks of domain-warped fbm, coloured deep
/// navy → crimson/magenta with a soft glow and film grain. The coordinate is
/// quantized (the shader's `u_pixel`) so the waves read as chunky pixels, never
/// smoke; half-blocks make each block roughly square. Frozen under reduce-motion.
fn draw_backdrop_waves(f: &mut Frame, area: Rect, app: &App) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    // frozen under reduce-motion AND under the open Memorandum, so its idle
    // view stops changing and the terminal keeps a mouse selection over it
    let anim = !app.backdrop_frozen();
    // Terminal clock scaled so the warp crawls at the pace already dialled in
    // for the wallpaper (the shader's per-second drift, matched to ~8 ticks/s).
    let t = if anim { app.frame as f32 * 0.5 } else { 0.0 };
    let slow = if anim { (app.frame / 4) as f32 } else { 0.0 }; // ~2 Hz grain step
    // The pixel field is width × (height*2) square-ish taps; quantize p (=uv*asp)
    // into ~26 cells per unit, exactly like the shader's u_pixel = 26.
    let (pw, ph) = (area.width as f32, (area.height as f32 * 2.0).max(1.0));
    let asp = pw / ph;
    let cells = 26.0_f32;
    // How visible the whole effect is — set by the user's intensity slider. The
    // shader (navy base + waves) is computed faithfully, then blended toward the
    // app paper by this factor, so lower = subtler (Faint ≈ a whisper on paper,
    // Full = the pure shader). Math, motion, pixelation and palette unchanged.
    let dim = app.draft_settings().backdrop_level.factor();
    let (pr, pg, pb) = match app.palette().bg {
        Color::Rgb(r, g, b) => (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0),
        _ => (0.0, 0.0, 0.0),
    };
    for yy in 0..area.height {
        for xx in 0..area.width {
            let mut px = [(0u8, 0u8, 0u8); 2]; // ▀ top/bottom halves
            for (half, slot) in px.iter_mut().enumerate() {
                let uvy = (yy as f32 * 2.0 + half as f32) / ph;
                let mut pxc = (xx as f32 / pw) * asp;
                let mut pyc = uvy;
                pxc = ((pxc * cells).floor() + 0.5) / cells;
                pyc = ((pyc * cells).floor() + 0.5) / cells;

                // domain-warped fbm — shader body, verbatim coefficients
                let qx = sfbm(pxc * 1.4 + t * 0.15, pyc * 1.4 - t * 0.08);
                let qy = sfbm(pxc * 1.4 - t * 0.11 + 5.2, pyc * 1.4 + t * 0.13);
                let w = sfbm(pxc * 1.6 + qx * 1.8, pyc * 1.6 + qy * 1.8 + t * 0.1);
                let wave = smoothstep(0.35, 0.75, w);

                // hue drift in [0.82,1.03] → magenta→red wrap → RGB
                let hn = sfbm(pxc * 0.9 - qx * 1.2 + t * 0.07, pyc * 0.9 - qy * 1.2 + 9.1 + t * 0.05);
                let m = 0.82 + (1.03 - 0.82) * (hn * 1.4 - 0.2).clamp(0.0, 1.0);
                let hue = m - m.floor();
                let hcomp = |o: f32| ((((hue * 6.0 + o) % 6.0) - 3.0).abs() - 1.0).clamp(0.0, 1.0);
                let warm = |c: f32, floor: f32| floor + (c * 0.62 - floor) * 0.7;
                let (wr, wg, wb) = (warm(hcomp(0.0), 0.16), warm(hcomp(4.0), 0.04), warm(hcomp(2.0), 0.10));

                // deep-navy base (u_base #101018) with a faint top-down lift
                let lift = |c: f32| c + (c * 1.08 + 0.03 - c) * (uvy * 0.6);
                let (br, bg, bb) = (lift(16.0 / 255.0), lift(16.0 / 255.0), lift(24.0 / 255.0));

                let mut cr = br + (wr - br) * wave * 0.85;
                let mut cg = bg + (wg - bg) * wave * 0.85;
                let mut cb = bb + (wb - bb) * wave * 0.85;

                // soft glow on the wave crests
                let glow = smoothstep(0.55, 0.9, w) * 0.18;
                cr += glow;
                cg += glow;
                cb += glow;

                // film grain: static hash blended with a ~2 Hz one, + paper grain
                let (fx, fy) = (xx as f32, yy as f32 * 2.0 + half as f32);
                let g_static = shash(fx, fy);
                let g_slow = shash(fx + slow, fy);
                let g = (g_static + (g_slow - g_static) * 0.25 - 0.5) * 0.07 * (0.7 + 0.6 * wave);
                let grain_p = (snoise(fx * 0.15, fy * 0.15) - 0.5) * 0.07 * 0.5;
                cr += g + grain_p;
                cg += g + grain_p;
                cb += g + grain_p;

                // fade the whole effect toward the app paper by the slider level
                let blend = |c: f32, base: f32| base + (c - base) * dim;
                let to8 = |c: f32, base: f32| (blend(c, base).clamp(0.0, 1.0) * 255.0) as u8;
                *slot = (to8(cr, pr), to8(cg, pg), to8(cb, pb));
            }
            let style = Style::default()
                .fg(Color::Rgb(px[0].0, px[0].1, px[0].2))
                .bg(Color::Rgb(px[1].0, px[1].1, px[1].2));
            put(f, area.x + xx, area.y + yy, "▀", style, area);
        }
    }
}

/// Linear blend of two `Color::Rgb`s (non-Rgb operands treated as black).
fn lerp_rgb(a: Color, b: Color, t: f32) -> Color {
    let ex = |c: Color| match c {
        Color::Rgb(r, g, b) => (r as f32, g as f32, b as f32),
        _ => (0.0, 0.0, 0.0),
    };
    let (ar, ag, ab) = ex(a);
    let (br, bg, bb) = ex(b);
    let m = |x: f32, y: f32| (x + (y - x) * t).clamp(0.0, 255.0) as u8;
    Color::Rgb(m(ar, br), m(ag, bg), m(ab, bb))
}

/// The barely-there digit-rain wallpaper: the lock screen's rain at a fraction
/// of its intensity — darkest brass shades only, sparser, and slower. Content
/// is drawn AFTER it, so it survives only in genuinely empty cells and can
/// never crowd the ledger. The intensity slider fades it toward the paper.
/// Frozen under reduce-motion.
fn draw_backdrop_rain(f: &mut Frame, area: Rect, app: &App) {
    const GLYPHS: [char; 10] = ['7', '=', '1', '*', '0', '.', ':', '2', '+', '9'];
    // frozen under reduce-motion AND under the open Memorandum: a glyph that
    // falls a cell every frame under a mouse selection makes the terminal drop it
    let reduce = app.backdrop_frozen();
    // the intensity slider fades the glyphs toward the paper (lower = subtler)
    let lvl = app.draft_settings().backdrop_level.factor();
    let paper = app.palette().bg;
    // dimmer than the lock rain: it must read as texture, not content
    let dark = lerp_rgb(paper, Color::Rgb(0x22, 0x1C, 0x12), lvl);
    let mid = lerp_rgb(paper, Color::Rgb(0x2E, 0x26, 0x17), lvl);
    for xx in 0..area.width {
        let col = splitmix(0xBACD ^ ((xx as u64) << 17));
        // slower than the lock: one cell every ~1–2.5 s per column
        let fall = if reduce { 0 } else { app.frame / (8 + (col % 13)) };
        for yy in 0..area.height {
            let vy = (yy as u64).wrapping_sub(fall) & 0xFFFF;
            let h = splitmix(((xx as u64) << 32) | vy);
            if !h.is_multiple_of(19) {
                continue; // sparser than the lock rain
            }
            let g = GLYPHS[(h / 19 % 10) as usize];
            // texture only — never bright enough to be mistaken for content
            let color = if (h / 190).is_multiple_of(3) { mid } else { dark };
            put(f, area.x + xx, area.y + yy, &g.to_string(), Style::default().fg(color), area);
        }
    }
}

/// The full-screen lock (fig-02): the vault is closed (key zeroized) and a
/// brass digit-rain takes the whole screen; a centered surface box holds the
/// passphrase. Any key types into it; Enter re-unlocks; lock ≠ quit.
fn draw_lock(f: &mut Frame, area: Rect, app: &App, lock: &crate::app::PassPrompt) {
    let p = app.palette();
    let lang = app.effective_lang();
    // paper behind everything (the lock owns the whole screen)
    f.render_widget(Block::default().style(Style::default().bg(p.bg)), area);

    // brass rain: sparse ledger glyphs in dark brass shades, a few bright.
    // Each column FALLS smoothly at its own pace (the pattern translates down
    // one cell at a time — no reshuffle flicker); barely-there by design.
    const GLYPHS: [char; 10] = ['7', '=', '1', '*', '0', '.', ':', '2', '+', '9'];
    const DARK: Color = Color::Rgb(0x4A, 0x3D, 0x22);
    const MID: Color = Color::Rgb(0x6E, 0x5A, 0x2E);
    for xx in 0..area.width {
        let col = splitmix(0x5EED ^ ((xx as u64) << 17));
        // per-column speed: one cell every 4–10 ticks (~0.5–1.2 s); calm.
        let fall = if app.settings.reduce_motion {
            0
        } else {
            app.frame / (4 + (col % 7))
        };
        for yy in 0..area.height {
            // subtract the fall so the pattern moves DOWN as frames advance
            let vy = (yy as u64).wrapping_sub(fall) & 0xFFFF;
            let h = splitmix(((xx as u64) << 32) | vy);
            if !h.is_multiple_of(13) {
                continue; // sparse
            }
            let g = GLYPHS[(h / 13 % 10) as usize];
            let color = match h / 130 % 24 {
                0 => p.accent,
                1 => p.accent_hi,
                2..=9 => MID,
                _ => DARK,
            };
            put(f, area.x + xx, area.y + yy, &g.to_string(), Style::default().fg(color), area);
        }
    }

    // the centered surface box. The floors keep it usable on a small terminal;
    // the final clamp keeps it INSIDE one that is smaller than its own floor,
    // where a box wider than the screen would paint off the buffer.
    let w = 40u16.min(area.width.saturating_sub(4)).max(24).min(area.width);
    let h = 7u16.min(area.height.saturating_sub(2)).max(6).min(area.height);
    let bx = area.x + area.width.saturating_sub(w) / 2;
    let by = area.y + area.height.saturating_sub(h) / 2;
    let rect = Rect { x: bx, y: by, width: w, height: h };
    f.render_widget(Clear, rect);
    f.render_widget(Block::default().style(Style::default().bg(p.surface)), rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(p.accent));
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    let cx = inner.x + 1;
    let cw = inner.width.saturating_sub(2);
    let locked_word = match lang {
        Lang::En => " — locked",
        Lang::It => " — bloccato",
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!("◆ {}", lock.name), Style::default().fg(p.text).add_modifier(Modifier::BOLD)),
            Span::styled(locked_word, Style::default().fg(p.muted)),
        ])),
        Rect { x: cx, y: inner.y, width: cw, height: 1 },
    );
    // the passphrase on a brass underline
    let bullets = "●".repeat(lock.input.chars().count());
    let cursor = if app.cursor_on() { "▌" } else { " " };
    let used = bullets.chars().count() + 1 + 11;
    let pad = (cw as usize).saturating_sub(used).min(18);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("passphrase ", Style::default().fg(p.muted)),
            Span::styled(bullets, Style::default().fg(p.muted).add_modifier(Modifier::UNDERLINED)),
            Span::styled(cursor.to_string(), Style::default().fg(p.accent_hi)),
            Span::styled("─".repeat(pad), Style::default().fg(p.accent)),
        ])),
        Rect { x: cx, y: inner.y + 2, width: cw, height: 1 },
    );
    let msg = if lock.error {
        match lang {
            Lang::En => format!("✗ wrong passphrase — attempt {}", lock.attempts),
            Lang::It => format!("✗ passphrase errata — tentativo {}", lock.attempts),
        }
    } else {
        String::new()
    };
    if !msg.is_empty() {
        puts(f, cx, inner.y + 3, &msg, Style::default().fg(p.danger), inner);
    }
    let hint = match lang {
        Lang::En => "⏎ unlock · Ctrl+Q quit",
        Lang::It => "⏎ sblocca · Ctrl+Q esci",
    };
    puts(f, cx, inner.y + inner.height.saturating_sub(1), hint, Style::default().fg(p.faint), inner);

    // …and, under the box, the one thing THIS screen must not hide: how many
    // vaults an agent is still holding open through the API. A lock closes the
    // vault behind it and nothing else — an agent's session was opened with its
    // own password and keeps answering (see `App::lock_now`) — while the top bar
    // that carries the `api·N` chip is not on this screen at all. An indicator
    // that disappears exactly when the machine is left unattended is not one.
    // It counts and does not name, like the bar's.
    if let Some(chip) = api_sessions_chip(app) {
        let line = format!("{chip} \u{b7} {}", i18n::t(lang, &Key::LockApiSessions));
        let y = rect.y + rect.height;
        if y < area.y + area.height {
            let w = line.chars().count().min(area.width as usize) as u16;
            let x = area.x + area.width.saturating_sub(w) / 2;
            puts(f, x, y, &line, Style::default().fg(p.accent), area);
        }
    }
}

/// Bullet colors for the built-in categories (fig-05); customs hash like the
/// monograms so every category keeps a stable identity color.
fn category_bullet_color(id: &str) -> Color {
    match id {
        "email" => Color::Rgb(0xD9, 0x8B, 0x65),
        "account" => Color::Rgb(0x8F, 0xB0, 0xCE),
        "login" => Color::Rgb(0xA8, 0xC9, 0x8F),
        "credit_card" => Color::Rgb(0xE8, 0xA9, 0x3D),
        "api_key" => Color::Rgb(0x9F, 0x8F, 0xCE),
        "note" => Color::Rgb(0x7F, 0xC6, 0xB7),
        "ssh_key" => Color::Rgb(0xCE, 0x8F, 0xA8),
        "wifi" => Color::Rgb(0x8F, 0xC6, 0xCE),
        "identity" => Color::Rgb(0xC9, 0x7B, 0xE0),
        other => monogram_color(other),
    }
}

/// A floating chooser (fig-05 for the add-category picker; languages and `~`
/// directions share the frame): `▌ ● Name  field · field` rows, the selected
/// one on the brass-tinted band, `type to filter · ⏎ choose · Esc` footer.
fn draw_picker(f: &mut Frame, full: Rect, app: &App, kind: PickerKind) {
    let p = app.palette();
    let lang = app.effective_lang();
    let choices = app.picker_choices(kind);
    let title = match kind {
        PickerKind::ItemCategory => {
            let vault = app.vault.as_ref().map(|v| v.username.to_uppercase()).unwrap_or_default();
            match lang {
                Lang::En => format!("ADD — {vault}"),
                Lang::It => format!("AGGIUNGI — {vault}"),
            }
        }
        PickerKind::Language => "LANGUAGE".to_string(),
        PickerKind::PanelHistory => "~".to_string(),
    };
    let w = 54u16.min(full.width.saturating_sub(6)).max(28);
    let h = ((choices.len() as u16) + 4).min(full.height.saturating_sub(2)).max(6);
    let x = full.x + full.width.saturating_sub(w) / 2;
    let y = full.y + full.height.saturating_sub(h) / 3;
    let inner = draw_modal_frame(f, Rect { x, y, width: w, height: h }, &title, p);
    if inner.width < 8 || inner.height < 2 {
        return;
    }
    let cx = inner.x + 1;
    let cw = inner.width.saturating_sub(2);
    let mut yy = inner.y;
    let index = app.panel_index();
    let foot_y = inner.y + inner.height.saturating_sub(1);
    // name column sized to the longest choice label
    let name_w = choices.iter().map(|c| c.label.chars().count()).max().unwrap_or(8).clamp(8, 16);
    for (i, choice) in choices.iter().enumerate() {
        if yy >= foot_y {
            break;
        }
        let selected = i == index;
        if selected {
            puts(f, cx, yy, &" ".repeat(cw as usize), Style::default().bg(p.selection), inner);
        }
        let with_bg = |s: Style| if selected { s.bg(p.selection) } else { s };
        let name_style = if selected {
            Style::default().fg(p.accent_hi).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.text)
        };
        let mut spans = vec![Span::styled(
            if selected { "▌ " } else { "  " },
            with_bg(Style::default().fg(p.accent)),
        )];
        // the category picker gets its identity bullet + the field list
        if kind == PickerKind::ItemCategory {
            spans.push(Span::styled("● ", with_bg(Style::default().fg(category_bullet_color(&choice.value)))));
            spans.push(Span::styled(format!("{:<name_w$}", choice.label.clone()), with_bg(name_style)));
            let fields = crate::model::BUILTINS
                .iter()
                .find(|c| c.id == choice.value)
                .map(|c| c.fields.iter().map(|fs| fs.key.replace('_', " ")).collect::<Vec<_>>().join(" · "))
                .unwrap_or_default();
            let avail = (cw as usize).saturating_sub(4 + name_w);
            spans.push(Span::styled(truncate(&fields, avail), with_bg(Style::default().fg(p.faint))));
        } else {
            spans.push(Span::styled(choice.label.clone(), with_bg(name_style)));
        }
        let mut para = Paragraph::new(Line::from(spans));
        if selected {
            para = para.style(Style::default().bg(p.selection));
        }
        f.render_widget(para, Rect { x: cx, y: yy, width: cw, height: 1 });
        yy += 1;
    }
    let footer = match lang {
        Lang::En => "↑↓ · ⏎ choose · Esc",
        Lang::It => "↑↓ · ⏎ scegli · Esc",
    };
    puts(f, cx, foot_y, footer, Style::default().fg(p.faint), inner);
}

/// The `?` key cheatsheet: a floating card grouping the keys by scope
/// (`── GLOBAL ──`, `── ACCESS ──` / `── IN-VAULT ──`), generated from the SAME
/// keymap that renders the keybar and drives dispatch — so it can never lie.
fn draw_cheatsheet(f: &mut Frame, full: Rect, app: &App) {
    let p = app.palette();
    let lang = app.effective_lang();
    // (section title, rows of (key, label))
    let mut global: Vec<(String, String)> = vec![
        (":".into(), "command palette".into()),
        ("/".into(), if app.page == Page::Vault { "filter".into() } else { "command palette".into() }),
        ("?".into(), "this cheatsheet".into()),
        ("Esc".into(), "back · close a layer".into()),
        ("Ctrl+Q".into(), "quit".into()),
    ];
    // the top bar's `api·N` chip is the one glyph up there with no key behind
    // it, so this is where it gets its sentence — listed only while it is on
    // screen, so the card never explains something nobody can see.
    if let Some(chip) = api_sessions_chip(app) {
        global.push((chip, i18n::t(lang, &Key::CheatApiSessions)));
    }
    let (sec_title, mut sec) = match app.page {
        Page::Access => ("ACCESS", vec![("↑↓ j k".to_string(), "move".to_string()), ("⏎".to_string(), "unlock".to_string())]),
        Page::Vault => ("IN-VAULT", vec![
            ("↑↓ j k".to_string(), "move".to_string()),
            ("⏎".to_string(), "copy password".to_string()),
            // one cycle, both ways: the categories and — last of them, one
            // Shift+Tab back from ALL — the memorandum's own chip
            ("Tab / ⇧Tab".to_string(), i18n::t(lang, &Key::CheatTabCycle)),
            // …and inside the filter box that same Tab is the mode chip, which
            // is the only other thing it can mean there — named with the chip's
            // own two words, so the card cannot say one thing and the box another
            (
                "/ + Tab".to_string(),
                format!(
                    "{} \u{b7} {}",
                    i18n::t(lang, &Key::SearchModeLiteral),
                    i18n::t(lang, &Key::SearchModeRag)
                ),
            ),
        ]),
    };
    // …through the EFFECTIVE binds: a verb the user switched off is not listed
    // here either, and a re-keyed one is listed on its chosen character.
    for b in crate::keymap::effective_binds(app.page, app.draft_settings()) {
        sec.push((b.key(lang).to_string(), b.label(lang).to_string()));
    }
    // layout
    let rows = (2 + global.len() + 2 + sec.len()) as u16;
    let keyw = global.iter().chain(sec.iter()).map(|(k, _)| k.chars().count()).max().unwrap_or(6).max(6);
    let w = 46u16.min(full.width.saturating_sub(6)).max(28);
    let h = (rows + 2).min(full.height.saturating_sub(2)).max(6);
    let x = full.x + full.width.saturating_sub(w) / 2;
    let y = full.y + full.height.saturating_sub(h) / 2;
    let inner = draw_modal_frame(f, Rect { x, y, width: w, height: h }, "keys", p);
    let mut yy = inner.y;
    let bottom = inner.y + inner.height;
    let section = |f: &mut Frame, yy: &mut u16, title: &str, rows: &[(String, String)]| {
        if *yy >= bottom {
            return;
        }
        section_rule(f, inner.x, *yy, inner.width, title, p, inner);
        *yy += 1;
        for (k, label) in rows {
            if *yy >= bottom {
                break;
            }
            let spans = vec![
                Span::styled(format!("{k:<keyw$}  "), accent_bold(p)),
                Span::styled(label.clone(), Style::default().fg(p.muted)),
            ];
            f.render_widget(Paragraph::new(Line::from(spans)), Rect { x: inner.x, y: *yy, width: inner.width, height: 1 });
            *yy += 1;
        }
        *yy += 1; // blank row between groups
    };
    section(f, &mut yy, "GLOBAL", &global);
    section(f, &mut yy, sec_title, &sec);
}

/// Sum of the display widths of a span list (ascii-ish; good enough for layout).
fn spans_w(spans: &[Span]) -> usize {
    spans.iter().map(|s| s.content.chars().count()).sum()
}

/// A one-row hairline rule across `w` cells at `(x,y)`.
fn put_rule(f: &mut Frame, x: u16, y: u16, w: u16, p: Palette) {
    f.render_widget(
        Paragraph::new(Line::from(Span::styled("─".repeat(w as usize), Style::default().fg(p.border)))),
        Rect { x, y, width: w, height: 1 },
    );
}

/// The per-vault sync status glyph for the top-bar / access list (`None` when
/// this vault has no sync configured): ⇅ syncing · ◈ synced · ⚠ diverged ·
/// ⌀ offline.
fn sync_glyph(app: &App, username: &str) -> Option<(char, Color)> {
    use crate::sync::SyncStatus;
    let p = app.palette();
    // configured but no live status yet still shows the "sealed" synced diamond
    if !crate::sync::SyncConfig::exists(&app.paths.vaults_dir, username) {
        return None;
    }
    Some(match app.sync_status.get(username).copied().unwrap_or(SyncStatus::Synced) {
        SyncStatus::Syncing => ('\u{21c5}', p.accent), // ⇅
        SyncStatus::Diverged => ('\u{26a0}', p.danger), // ⚠
        SyncStatus::Offline => ('\u{2300}', p.faint),  // ⌀
        SyncStatus::Synced | SyncStatus::Unconfigured => ('\u{25c8}', p.steel), // ◈
    })
}

/// The top-bar chip for live API vault sessions — `api·2` — or `None` when an
/// agent is holding nothing open (which is nearly always, and is the state that
/// deserves no ink at all).
///
/// Untranslated on purpose, like the `RAG` section rule and the mode chips:
/// `api` is what the thing is called, in both languages. The word for what it
/// MEANS is one `?` away — see the cheatsheet row it shares its text with.
fn api_sessions_chip(app: &App) -> Option<String> {
    match app.api_sessions() {
        0 => None,
        n => Some(format!("api\u{b7}{n}")),
    }
}

/// Width (in cells) of the indeterminate re-encryption bar, and the length of
/// the block travelling inside it.
const REKEY_BAR_W: usize = 10;
const REKEY_BAR_RUN: usize = 3;

/// The indeterminate progress bar shown next to a vault's name while its
/// password change re-encrypts it: a brass block ping-ponging along a dotted
/// track, driven by `app.frame` like every other animation here. There is no
/// percentage to show (one KDF pass + one atomic write), hence indeterminate.
/// With reduce-motion on it freezes into a static "in progress" bar.
fn rekey_bar(app: &App) -> String {
    let mut cells = vec!["·"; REKEY_BAR_W];
    let pos = if app.settings.reduce_motion {
        0 // static: the block simply sits at the start of the track
    } else {
        // ping-pong so the block never appears to jump back to the left
        let span = (REKEY_BAR_W - REKEY_BAR_RUN) * 2;
        let t = (app.frame / 2) as usize % span;
        if t <= REKEY_BAR_W - REKEY_BAR_RUN { t } else { span - t }
    };
    for c in cells.iter_mut().skip(pos).take(REKEY_BAR_RUN) {
        *c = "█";
    }
    format!("[{}]", cells.concat())
}

/// The one-row top-bar: `▌ DATASHIELD · vaults` on the Access page, or
/// `▌ USERNAME ◇ · N items` inside an unlocked vault, with the locale code
/// pinned right. Replaces the old multi-row pixel-logo header.
fn draw_topbar(f: &mut Frame, area: Rect, app: &App) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let p = app.palette();
    let lang = app.effective_lang();
    let mut left = vec![Span::styled("▌ ", Style::default().fg(p.accent))];
    match app.page {
        Page::Access => {
            left.push(Span::styled("DATASHIELD", accent_bold(p)));
            left.push(Span::styled(
                format!(" · {}", i18n::t(lang, &Key::VaultsTitle).to_lowercase()),
                Style::default().fg(p.muted),
            ));
        }
        Page::Vault => match app.vault.as_ref() {
            Some(v) => {
                left.push(Span::styled(v.username.to_uppercase(), accent_bold(p)));
                left.push(Span::styled(format!("  ◇ · {} items", v.items().len()), Style::default().fg(p.muted)));
                if let Some((glyph, color)) = sync_glyph(app, &v.username) {
                    left.push(Span::styled(format!("  {glyph}"), Style::default().fg(color)));
                }
                // the open vault is being re-keyed: the bar rides its own name
                if app.rekeying(&v.username) {
                    left.push(Span::styled(
                        format!("  {} ", rekey_bar(app)),
                        Style::default().fg(p.accent),
                    ));
                    left.push(Span::styled(
                        i18n::t(lang, &Key::PasswdInProgress),
                        Style::default().fg(p.muted),
                    ));
                }
            }
            None => left.push(Span::styled("DATASHIELD", accent_bold(p))),
        },
    }
    // …and, on either page, how many vaults an agent is holding open through
    // the API. It is drawn only while there is at least one — a chip that says
    // "0" would be furniture, and this one is meant to be noticed. The count is
    // all it says: naming the vault would put on screen what the token is for.
    if let Some(chip) = api_sessions_chip(app) {
        left.push(Span::styled(format!("  {chip}"), Style::default().fg(p.accent)));
    }
    f.render_widget(
        Paragraph::new(Line::from(left)),
        Rect { x: area.x + 1, y: area.y, width: area.width.saturating_sub(2), height: 1 },
    );
    // right side: `locks in M:SS · EN` (the countdown only inside a vault
    // with auto-lock on — spec fig-03)
    let mut right_spans: Vec<Span> = Vec::new();
    if let Some(ticks) = app.autolock_remaining_ticks() {
        let secs = ticks * 120 / 1000;
        right_spans.push(Span::styled(
            match lang {
                Lang::En => "locks in ",
                Lang::It => "blocco tra ",
            },
            Style::default().fg(p.muted),
        ));
        right_spans.push(Span::styled(
            format!("{}:{:02}", secs / 60, secs % 60),
            Style::default().fg(p.text),
        ));
        right_spans.push(Span::styled(" · ", Style::default().fg(p.faint)));
    }
    right_spans.push(Span::styled(lang.code().to_uppercase(), Style::default().fg(p.faint)));
    let rw = spans_w(&right_spans) as u16;
    if area.width > rw + 3 {
        f.render_widget(
            Paragraph::new(Line::from(right_spans)),
            Rect { x: area.x + area.width - rw - 1, y: area.y, width: rw, height: 1 },
        );
    }
}

/// Left margin before the logo in the header.
const HDR_LEFT: u16 = 2;

/// The active logo's `(width, rows)` in terminal cells: the user's text logo if
/// they set one, else the built-in colour logo.
fn logo_dims(app: &App) -> (u16, u16) {
    if app.logo.is_empty() {
        (crate::logo::COLOR_WIDTH, crate::logo::COLOR_HEIGHT / 2)
    } else {
        // saturating (not wrapping) usize→u16 so a pathologically wide hand-
        // edited line can't wrap to a tiny width and mis-place the header.
        let w = app
            .logo
            .iter()
            .map(|l| l.chars().count())
            .max()
            .unwrap_or(0)
            .min(u16::MAX as usize) as u16;
        (w, app.logo.len() as u16)
    }
}

/// Height reserved for the header (a blank pad row above and below the logo).
/// Shown only when the logo is enabled, the logo fits the terminal width, and
/// the terminal stays tall enough for a usable body — otherwise 0, so a cramped
/// or too-narrow terminal renders like it did before the header existed.
fn header_height(app: &App, total_w: u16, total_h: u16) -> u16 {
    // the logo/phrases header belongs to the vault-selection screen only; inside
    // an open vault the panels use the full height.
    if app.page == Page::Vault {
        return 0;
    }
    if !app.settings.show_logo {
        return 0;
    }
    let (lw, lr) = logo_dims(app);
    // saturating: logo_dims caps a pathological text logo at u16::MAX, so a plain
    // `HDR_LEFT + lw + 2` could overflow under debug overflow-checks.
    if lw == 0 || lr == 0 || HDR_LEFT.saturating_add(lw).saturating_add(2) > total_w {
        return 0;
    }
    let h = lr + 2; // one pad row above + logo + one pad row below
    if total_h >= h + 12 {
        h
    } else {
        0
    }
}

/// Header: the logo on the LEFT, a vertical wall, and typewriter phrases to its
/// right. The built-in logo is a multi-colour pixel-Saturn with a light shimmer
/// running along its ring; a user's text logo is drawn flat in the brand colour.
/// Everything is rendered into rects bounded by the buffer, so it can't panic on
/// a narrow terminal.
fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let p = app.palette();
    let (lw, lr) = logo_dims(app);
    if lw == 0 || lr == 0 {
        return;
    }
    let x = area.x + HDR_LEFT;
    let y0 = area.y + 1; // top pad row
    let n = lr.min(area.height.saturating_sub(2)); // keep the bottom pad row
    if n == 0 {
        return;
    }
    let right = area.x + area.width;

    if app.logo.is_empty() {
        draw_color_logo(f, x, y0, n, app);
    } else {
        let color = app.chrome.logo_color(&p);
        let w = lw.min(right.saturating_sub(x));
        let lines: Vec<Line> = app
            .logo
            .iter()
            .take(n as usize)
            .map(|l| Line::from(Span::styled(l.clone(), Style::default().fg(color).add_modifier(Modifier::BOLD))))
            .collect();
        f.render_widget(Paragraph::new(lines), Rect { x, y: y0, width: w, height: n });
    }

    // vertical wall + typewriter phrases (only if there's room to the right).
    // saturating: a pathological text logo can push lw to u16::MAX.
    let wall_x = x.saturating_add(lw).saturating_add(2);
    if wall_x.saturating_add(1) >= right {
        return;
    }
    for row in 0..n {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled("│", Style::default().fg(p.border)))),
            Rect { x: wall_x, y: y0 + row, width: 1, height: 1 },
        );
    }
    let phrase_x = wall_x.saturating_add(2);
    if phrase_x >= right {
        return;
    }
    let (text, cursor) = typewriter(app);
    let mut spans = vec![Span::styled(text, Style::default().fg(p.accent))];
    if cursor {
        spans.push(Span::styled("▏", Style::default().fg(p.accent_soft)));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect { x: phrase_x, y: y0 + n / 2, width: right - phrase_x, height: 1 },
    );
}

/// Renders the built-in colour logo (half-blocks: two pixel rows per cell) with
/// a light shimmer sweeping along it. Bounded to a rect the caller guarantees
/// fits the buffer.
fn draw_color_logo(f: &mut Frame, x: u16, y: u16, rows: u16, app: &App) {
    let frame = app.frame;
    let reduce = app.settings.reduce_motion;
    let w = crate::logo::COLOR_WIDTH;
    for row in 0..rows {
        let pr = row * 2;
        let mut spans: Vec<Span> = Vec::with_capacity(w as usize);
        for c in 0..w {
            let top = shimmer(crate::logo::color_pixel(c, pr), c, frame, reduce);
            let bot = shimmer(crate::logo::color_pixel(c, pr + 1), c, frame, reduce);
            spans.push(halfblock_cell(top, bot));
        }
        f.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect { x, y: y + row, width: w, height: 1 },
        );
    }
}

/// One terminal cell from a top/bottom pixel pair, using the upper-half block.
fn halfblock_cell(top: [u8; 4], bot: [u8; 4]) -> Span<'static> {
    let (ta, ba) = (top[3] > 50, bot[3] > 50);
    let tc = Color::Rgb(top[0], top[1], top[2]);
    let bc = Color::Rgb(bot[0], bot[1], bot[2]);
    match (ta, ba) {
        (true, true) => Span::styled("▀", Style::default().fg(tc).bg(bc)),
        (true, false) => Span::styled("▀", Style::default().fg(tc)),
        (false, true) => Span::styled("▄", Style::default().fg(bc)),
        (false, false) => Span::raw(" "),
    }
}

/// Brightens a pixel when the shimmer band passes over its column (a moving
/// highlight that reads as light catching the ring). Steady under reduce-motion.
fn shimmer(px: [u8; 4], col: u16, frame: u64, reduce: bool) -> [u8; 4] {
    if reduce || px[3] <= 50 {
        return px;
    }
    let period = crate::logo::COLOR_WIDTH as u64 + 8;
    let band = (frame % period) as i32 - 4;
    let d = (col as i32 - band).abs();
    if d > 2 {
        return px;
    }
    let boost = 1.0 + 0.30 * (1.0 - d as f32 / 3.0);
    let up = |v: u8| (v as f32 * boost).min(255.0) as u8;
    [up(px[0]), up(px[1]), up(px[2]), px[3]]
}

/// The current typewriter frame: the visible prefix of the current phrase and
/// whether to show the caret. Deterministic from `app.frame`; under
/// reduce-motion it holds the first phrase steady.
fn typewriter(app: &App) -> (String, bool) {
    let phrases = app.chrome.phrases();
    if phrases.is_empty() {
        return (String::new(), false);
    }
    if app.settings.reduce_motion {
        // a steady frame: show the first phrase fully, no caret
        return (phrases[0].clone(), false);
    }
    // per-phrase cycle: type (1 char/frame) · hold · delete (2 chars/frame) · gap
    let period = |l: u64| l + 12 + l.div_ceil(2) + 6;
    let total: u64 = phrases.iter().map(|s| period(s.chars().count() as u64)).sum();
    if total == 0 {
        return (String::new(), false);
    }
    // each pass through the whole set uses a fresh random order, so the phrases
    // don't march in a fixed sequence.
    let order = shuffled_indices(phrases.len(), app.frame / total);
    let mut t = app.frame % total;
    for &i in &order {
        let s = &phrases[i];
        let l = s.chars().count() as u64;
        let per = period(l);
        if t < per {
            let (shown, cursor) = if t < l {
                (t, true)
            } else if t < l + 12 {
                (l, true)
            } else if t < l + 12 + l.div_ceil(2) {
                (l.saturating_sub((t - l - 12) * 2), true)
            } else {
                (0, false)
            };
            return (s.chars().take(shown as usize).collect(), cursor);
        }
        t -= per;
    }
    (String::new(), false)
}

/// A deterministic shuffle of `0..n`, varied by `seed` (Fisher-Yates over
/// SplitMix64) — used so the header phrases appear in a fresh random order each
/// pass while staying stable within a single frame.
fn shuffled_indices(n: usize, seed: u64) -> Vec<usize> {
    let mut v: Vec<usize> = (0..n).collect();
    let mut i = n;
    while i > 1 {
        i -= 1;
        let j = (splitmix(seed.wrapping_add(i as u64)) % (i as u64 + 1)) as usize;
        v.swap(i, j);
    }
    v
}

/// A docked pane's header: a title RULE (`── title ───────`) across the top
/// row, no side or bottom borders. Focused panes get a brass title; unfocused
/// ones a muted one. Returns the content rect below the rule (one-cell inset),
/// so callers keep their existing `inner`-relative layout math — it is a
/// near-drop-in for the old bordered `block.inner(area)` (one row taller at the
/// bottom, since there's no bottom border). A box now means a *floating layer*
/// (see `draw_modal`), never a docked pane.
fn pane_header(f: &mut Frame, area: Rect, title: &str, focused: bool, p: Palette) -> Rect {
    if area.width == 0 || area.height == 0 {
        return area;
    }
    let title_style = if focused {
        Style::default().fg(p.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(p.muted).add_modifier(Modifier::BOLD)
    };
    let rule = Style::default().fg(p.border);
    let w = area.width as usize;
    let label_cols = title.chars().count() + 4; // "── " + title + " "
    let tail = w.saturating_sub(label_cols);
    let spans = vec![
        Span::styled("── ", rule),
        Span::styled(title.to_string(), title_style),
        Span::styled(" ", rule),
        Span::styled("─".repeat(tail), rule),
    ];
    f.render_widget(Paragraph::new(Line::from(spans)), Rect { x: area.x, y: area.y, width: area.width, height: 1 });
    Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(1),
    }
}

fn accent_bold(p: Palette) -> Style {
    Style::default().fg(p.accent).add_modifier(Modifier::BOLD)
}

/// Marker + (marker style, text style) for a selectable list row. The `▸`
/// marker ALWAYS marks the current position; its colour just reflects whether
/// the panel is focused — so you never lose track of where you are.
fn row_marker(selected: bool, focused: bool, p: Palette) -> (&'static str, Style, Style) {
    if !selected {
        return ("  ", Style::default(), Style::default().fg(p.text));
    }
    // ▌ selection bar — the only selection treatment (no ▸ arrows). Brass when
    // the pane is focused, dimmed when not; the name brightens to accent-hi.
    let bar = if focused { p.accent } else { p.accent_soft };
    let name = if focused {
        Style::default().fg(p.accent_hi).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(p.text).add_modifier(Modifier::BOLD)
    };
    ("▌ ", Style::default().fg(bar), name)
}

/// The most identifying NON-secret field value of an item, shown greyed under
/// the label in the list (never a secret — those stay masked in the detail).
fn item_subtitle(item: &crate::model::Item) -> Option<String> {
    item.fields
        .iter()
        // `secret` is what decides masking, here as everywhere; the image test
        // is not a second masking rule but a "this value is not text" one — a
        // foreign vault that forgot `secret: true` must still never spill a
        // megabyte of base64 across the list row.
        .find(|fld| !fld.secret && !fld.value.is_empty() && !crate::image_field::is_image(fld))
        .map(|fld| fld.value.as_str().to_string())
}

/// The left panel: the single-line vault list on the Access page, or the open
/// vault's items (category tabs + search box + two-line entries) on the Vault
/// page.
/// Humanized age of a timestamp: `now`, `5 m`, `12 h`, `4 d`, `3 mo`, `2 y`.
fn age_of(t: std::time::SystemTime) -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(t)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    match secs {
        0..=59 => "now".to_string(),
        60..=3599 => format!("{} m", secs / 60),
        3600..=86_399 => format!("{} h", secs / 3600),
        86_400..=2_591_999 => format!("{} d", secs / 86_400),
        2_592_000..=31_535_999 => format!("{} mo", secs / 2_592_000),
        _ => format!("{} y", secs / 31_536_000),
    }
}

/// The dot-leader color on a selected (brass-tinted) row.
const SELECTED_LEADER: Color = Color::Rgb(0x4A, 0x41, 0x32);

/// The access page (fig-01): a single full-width ledger of vaults —
/// `── VAULTS (N) ──`, one line per vault (`◆ name ····· CIPHER · age`, all
/// plaintext-header facts), and the inline unlock unfolded under the selected
/// row: no page swap, no modal, focus never moves.
fn draw_access(f: &mut Frame, area: Rect, app: &App) {
    let p = app.palette();
    let lang = app.effective_lang();
    if area.width < 8 || area.height < 3 {
        return;
    }
    let x = area.x + 2;
    let w = area.width.saturating_sub(4);
    let right = x + w;
    let bottom = area.y + area.height;
    let mut y = area.y + 1;

    let title = format!("{} ({})", i18n::t(lang, &Key::VaultsTitle).to_uppercase(), app.vaults.len());
    section_rule(f, x, y, w, &title, p, area);
    y += 2;

    if app.vaults.is_empty() {
        // the Access page's twin of the empty-vault line: it NAMES a key, so it
        // reads the same resolution the keybar and the cheatsheet do — a
        // re-keyed `new` moves the hint with it, and a switched-off one is
        // pointed at the palette rather than left advertising a dead letter.
        let new = crate::keymap::effective_binds(Page::Access, app.draft_settings())
            .into_iter()
            .find(|b| b.verb == crate::keymap::Verb::NewVault)
            .map(|b| b.key(lang));
        let hint = match new {
            Some(k) => i18n::t(lang, &Key::AccessEmptyHint(&k.to_string())),
            None => i18n::t(lang, &Key::AccessEmptyHintPalette),
        };
        puts(f, x, y, &hint, Style::default().fg(p.faint).add_modifier(Modifier::ITALIC), area);
        return;
    }

    for (i, name) in app.vaults.iter().enumerate() {
        if y >= bottom {
            break;
        }
        let selected = i == app.selected;
        let row_bg = if selected { Some(p.selection) } else { None };
        let with_bg = |s: Style| match row_bg {
            Some(b) => s.bg(b),
            None => s,
        };
        if selected {
            puts(f, x, y, &" ".repeat(w as usize), Style::default().bg(p.selection), area);
        }
        // ◆ sealed (this list only shows locked vaults); brass when selected
        let glyph_style = if selected { Style::default().fg(p.accent) } else { Style::default().fg(p.muted) };
        let name_style = if selected {
            Style::default().fg(p.accent_hi).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.text)
        };
        let marker = if selected { "▌ " } else { "  " };
        // meta: plaintext header facts — the cipher and the file's age. While a
        // password change re-encrypts this vault, the progress bar takes that
        // slot instead (the cipher/age it would show are about to change).
        let meta = if app.rekeying(name) {
            format!("{} {}", rekey_bar(app), i18n::t(lang, &Key::PasswdInProgress))
        } else {
            let cipher = crate::vault::header_info(&app.paths.vaults_dir, name)
                .map(|(algo, _)| algo)
                .unwrap_or_default();
            let age = crate::vault::file_modified(&app.paths.vaults_dir, name)
                .map(age_of)
                .unwrap_or_default();
            match (cipher.is_empty(), age.is_empty()) {
                (false, false) => format!("{cipher} · {age}"),
                (false, true) => cipher,
                (true, false) => age,
                (true, true) => String::new(),
            }
        };
        let avail = (w as usize).saturating_sub(4);
        let shown_name = truncate(name, avail.saturating_sub(meta.chars().count() + 2).max(1));
        let left_w = shown_name.chars().count();
        let gap = avail.saturating_sub(left_w + meta.chars().count());
        let leader = if gap >= 4 { format!(" {} ", "·".repeat(gap - 2)) } else { " ".repeat(gap) };
        let leader_color = if selected { SELECTED_LEADER } else { p.border };
        let spans = vec![
            Span::styled(marker, with_bg(Style::default().fg(p.accent))),
            Span::styled("◆ ", with_bg(glyph_style)),
            Span::styled(shown_name, with_bg(name_style)),
            Span::styled(leader, with_bg(Style::default().fg(leader_color))),
            // the re-encryption bar earns the brass; plain facts stay faint
            Span::styled(
                meta,
                with_bg(Style::default().fg(if app.rekeying(name) { p.accent } else { p.faint })),
            ),
        ];
        f.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect { x, y, width: w, height: 1 },
        );
        y += 1;

        // the inline unlock, unfolded under the selected vault's row
        if selected {
            if let Some(u) = app.unlock.as_ref().filter(|u| u.name == *name) {
                let ux = x + 5;
                if y < bottom {
                    let bullets = "●".repeat(u.input.chars().count());
                    let cursor = if app.cursor_on() { "▌" } else { " " };
                    let field_w = 24usize;
                    let used = bullets.chars().count() + 1;
                    let pad = field_w.saturating_sub(used);
                    let label = match lang {
                        Lang::En => "passphrase ",
                        Lang::It => "passphrase ",
                    };
                    f.render_widget(
                        Paragraph::new(Line::from(vec![
                            Span::styled(label, Style::default().fg(p.muted)),
                            Span::styled(bullets, Style::default().fg(p.muted).add_modifier(Modifier::UNDERLINED)),
                            Span::styled(cursor.to_string(), Style::default().fg(p.accent_hi)),
                            Span::styled("─".repeat(pad), Style::default().fg(p.accent)),
                        ])),
                        Rect { x: ux, y, width: right.saturating_sub(ux), height: 1 },
                    );
                    y += 1;
                }
                if y < bottom {
                    let err = if u.error {
                        match lang {
                            Lang::En => format!("✗ wrong passphrase — attempt {} of 5", u.attempts),
                            Lang::It => format!("✗ passphrase errata — tentativo {} di 5", u.attempts),
                        }
                    } else {
                        String::new()
                    };
                    let hint = match lang {
                        Lang::En => "⏎ unlock · Esc close",
                        Lang::It => "⏎ sblocca · Esc chiudi",
                    };
                    let hint_w = hint.chars().count() as u16;
                    puts(f, ux, y, &err, Style::default().fg(p.danger), area);
                    if right > hint_w {
                        puts(f, right - hint_w, y, hint, Style::default().fg(p.faint), area);
                    }
                    y += 1;
                }
            }
        }
    }
}

/// Counts the visible-tab-independent items per category id.
fn category_count(app: &App, cat: &str) -> usize {
    app.vault
        .as_ref()
        .map(|v| v.items().iter().filter(|i| i.category == cat).count())
        .unwrap_or(0)
}

/// The Memorandum's glyph on the chips row — the one chip that is not a
/// category, so it does not look like one.
const MEMO_CHIP_GLYPH: &str = "✎";

/// The chips row (fig-03): `ALL 4  email 1  login 2  ✎ memo 3  +   Tab…`, the
/// active chip a brass-filled pill (dark text on accent). While filtering
/// (fig-10) the same row becomes `/ query▌  N of M    ⏎ best · Esc clear` —
/// nothing reflows. The `✎ memo` chip is the LAST stop of the Tab cycle and
/// wears the pill while its view is open, which is why the row stays on screen
/// there (see `draw_vault_body`).
fn chips_row(f: &mut Frame, area: Rect, app: &App) {
    let p = app.palette();
    let lang = app.effective_lang();
    if area.width < 8 {
        return;
    }
    // …and while the thread has the body there is no query to show: the memo
    // is a stop on this row, not a filter over the ledger underneath it.
    let filtering = !app.memo_open && (app.search_focus || !app.vault_query.is_empty());
    if filtering {
        // `/ query▌  N of M  [aa literal]` + right hint. The mode chip is the
        // Memorandum's, to the glyph: it is the same search, and the same Tab
        // flips it — so it had better not look like a different thing here.
        let total = app.vault.as_ref().map(|v| v.items().len()).unwrap_or(0);
        let shown = app.visible_item_indices().len();
        let cursor = if app.search_focus && app.cursor_on() { "▌" } else { "" };
        let counts = format!("{shown} of {total}");
        let chip = mode_chip(app.vault_search.chip_mode(), lang);
        // the query is shown whole (this row never scrolls it) with the caret
        // dropped in between the chars it is on — a filter chip drawn while
        // browsing the list has no caret, only the focused box does.
        let caret = app.vault_query.caret();
        let underlined = Style::default().fg(p.text).add_modifier(Modifier::UNDERLINED);
        let before: String = app.vault_query.chars().take(caret).collect();
        let after: String = app.vault_query.chars().skip(caret).collect();
        let mut spans = vec![
            Span::styled("/ ", accent_bold(p)),
            Span::styled(before, underlined),
            Span::styled(cursor.to_string(), Style::default().fg(p.accent_hi)),
            Span::styled(after, underlined),
            Span::styled("   ", Style::default()),
            Span::styled(counts, Style::default().fg(p.faint)),
            Span::raw("  "),
            Span::styled(chip, Style::default().bg(p.chip_bg).fg(p.chip_fg).add_modifier(Modifier::BOLD)),
        ];
        // …and while the query is out on the embedding worker the row says so,
        // because the order under it is still the PREVIOUS answer's.
        if app.vault_search.searching() {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                i18n::t(lang, &Key::SearchWorking),
                Style::default().fg(p.faint).add_modifier(Modifier::ITALIC),
            ));
        }
        // the hint pinned right, longest first — the same rule the chips have
        // below: whichever still fits beside the row, or none at all.
        let hints: &[&str] = match lang {
            Lang::En => &["⏎ list · Tab mode · Esc clear", "Tab mode"],
            Lang::It => &["⏎ elenco · Tab modo · Esc pulisci", "Tab modo"],
        };
        let row_w = spans_w(&spans);
        let width = area.width as usize;
        let hint = hints.iter().copied().find(|h| {
            let hw = h.chars().count();
            width > hw + 2 && row_w + hw + 2 <= width
        });
        f.render_widget(Paragraph::new(Line::from(spans)), area);
        if let Some(h) = hint {
            let hw = h.chars().count() as u16;
            puts(f, area.x + area.width - hw, area.y, h, Style::default().fg(p.faint), area);
        }
        return;
    }

    let cats = app.vault_categories();
    let mut spans: Vec<Span> = Vec::new();
    // the ALL chip (uppercase) then one chip per category, each with its count.
    // Exactly ONE pill at a time: the thread is a stop of its own, so while it
    // is up the tab underneath it steps back to muted (it is still where Esc
    // will land, but it is not where you are).
    let total = app.vault.as_ref().map(|v| v.items().len()).unwrap_or(0);
    let all_label = i18n::t(lang, &Key::TabAll).to_uppercase();
    if app.vault_tab == 0 && !app.memo_open {
        spans.push(Span::styled(
            format!(" {all_label} {total} "),
            Style::default().bg(p.accent).fg(p.on_accent).add_modifier(Modifier::BOLD),
        ));
    } else {
        spans.push(Span::styled(all_label, Style::default().fg(p.muted)));
        spans.push(Span::styled(format!(" {total}"), Style::default().fg(p.faint)));
    }
    for (i, cat) in cats.iter().enumerate() {
        spans.push(Span::raw("  "));
        let name = i18n::t(lang, &Key::CategoryName(cat)).to_lowercase();
        let count = category_count(app, cat);
        if app.vault_tab == i + 1 && !app.memo_open {
            spans.push(Span::styled(
                format!(" {name} {count} "),
                Style::default().bg(p.accent).fg(p.on_accent).add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(name, Style::default().fg(p.muted)));
            spans.push(Span::styled(format!(" {count}"), Style::default().fg(p.faint)));
        }
    }
    // …then the MEMORANDUM, in the same pill grammar (glyph, name, count) but
    // NOT as a category: no item is ever filtered by it — it is the last stop
    // of the Tab cycle, and landing on it opens the thread.
    spans.push(Span::raw("  "));
    let memo_label = format!("{MEMO_CHIP_GLYPH} {}", i18n::t(lang, &Key::TabMemo));
    let memo_count = app.memo_len();
    if app.memo_open {
        spans.push(Span::styled(
            format!(" {memo_label} {memo_count} "),
            Style::default().bg(p.accent).fg(p.on_accent).add_modifier(Modifier::BOLD),
        ));
    } else {
        spans.push(Span::styled(memo_label, Style::default().fg(p.muted)));
        spans.push(Span::styled(format!(" {memo_count}"), Style::default().fg(p.faint)));
    }
    spans.push(Span::raw("  "));
    spans.push(Span::styled("+", Style::default().fg(p.faint)));
    // the hint pinned right (extra-faint affordance), longest FIRST: the row
    // keeps whichever one still fits beside the chips — the whole cycle, the
    // bare filter, or none at all. The chips come first because they are the
    // row; a hint that would sit on top of one is simply not drawn, and the
    // chips then take the full width and truncate at the edge like any other
    // over-long line. Inside the thread there is no `/` to promise (a slash is
    // text down there), so only the cycle is offered.
    let hints: &[&str] = match (lang, app.memo_open) {
        (Lang::En, false) => &["Tab/⇧Tab tabs · memo · / filter", "/ filter"],
        (Lang::It, false) => &["Tab/⇧Tab schede · memo · / filtra", "/ filtra"],
        (Lang::En, true) => &["Tab/⇧Tab tabs"],
        (Lang::It, true) => &["Tab/⇧Tab schede"],
    };
    // (counted in `usize`: a vault with hundreds of categories makes a row far
    // longer than the terminal, and that must stay a comparison, not a wrap)
    let row_w = spans_w(&spans);
    let width = area.width as usize;
    let hint = hints.iter().copied().find(|h| {
        let hw = h.chars().count();
        width > hw + 2 && row_w + hw + 2 <= width
    });
    let chips_w = match hint {
        Some(h) => area.width - h.chars().count() as u16 - 2,
        None => area.width,
    };
    f.render_widget(Paragraph::new(Line::from(spans)), Rect { width: chips_w, ..area });
    if let Some(h) = hint {
        let hw = h.chars().count() as u16;
        puts(f, area.x + area.width - hw, area.y, h, Style::default().fg(p.faint), area);
    }
}

/// The in-vault body (fig-03, the hero): the chips row full-width, then the
/// item ledger on the left, a vertical divider, and the ALWAYS-docked live
/// detail of the highlighted item on the right — j/k moves, the detail follows.
fn draw_vault_body(f: &mut Frame, area: Rect, app: &App) {
    let p = app.palette();
    let lang = app.effective_lang();
    if area.width < 12 || area.height < 4 {
        return;
    }
    let Some(vault) = app.vault.as_ref() else { return };

    // the MEMORANDUM takes the body — ledger and detail go — but NOT the chips
    // row: the thread is a stop on it (the ✎ chip, wearing the pill), and the
    // row that says which stop you are on has to be on screen while you are on
    // it. Under four rows there is nothing left to give it, so the thread takes
    // all. It sits ahead of the empty-vault hint on purpose: a vault with
    // nothing in it is exactly where the first note gets written.
    if app.memo_open {
        let mut body = area;
        if area.height >= 4 {
            chips_row(f, Rect { x: area.x + 2, y: area.y, width: area.width.saturating_sub(4), height: 1 }, app);
            body = Rect { y: area.y + 2, height: area.height - 2, ..area };
        }
        draw_memo(f, body, app);
        return;
    }

    // chips (or the summoned filter) — one full-width row, inset 2 cols
    chips_row(f, Rect { x: area.x + 2, y: area.y, width: area.width.saturating_sub(4), height: 1 }, app);

    let body_y = area.y + 2; // one blank row under the chips
    let body_h = (area.y + area.height).saturating_sub(body_y);
    if body_h == 0 {
        return;
    }

    if vault.items().is_empty() {
        // the one place outside the keybar that NAMES the add key: it goes
        // through the same resolution, so a re-keyed verb moves the hint with
        // it and a switched-off one is pointed at the palette instead.
        let add = crate::keymap::effective_binds(Page::Vault, app.draft_settings())
            .into_iter()
            .find(|b| b.verb == crate::keymap::Verb::AddItem)
            .map(|b| b.key(lang));
        let hint = match add {
            Some(k) => i18n::t(lang, &Key::VaultEmptyHint(&k.to_string())),
            None => i18n::t(lang, &Key::VaultEmptyHintPalette),
        };
        puts(f, area.x + 2, body_y, &hint, Style::default().fg(p.faint).add_modifier(Modifier::ITALIC), area);
        return;
    }

    // split: list | divider │ | docked detail (gutter 2 cols each side)
    let list_w = (area.width as u32 * app.chrome.left_percent as u32 / 100) as u16;
    let div_x = area.x + list_w;
    let list_area = Rect { x: area.x, y: body_y, width: list_w.saturating_sub(1), height: body_h };
    for dy in 0..body_h {
        put(f, div_x, body_y + dy, "│", Style::default().fg(p.divider), area);
    }
    let detail_x = div_x + 3;
    let detail_area = Rect {
        x: detail_x,
        y: body_y,
        width: (area.x + area.width).saturating_sub(detail_x + 2),
        height: body_h,
    };

    draw_item_ledger(f, list_area, app, vault);

    // the right side: the encryption card when summoned, else the live detail
    match app.active_view() {
        PanelView::EncInfo(id) => {
            let lines = enc_info_lines(app, id.as_deref());
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), detail_area);
        }
        _ => {
            let visible = app.visible_item_indices();
            if let Some(&idx) = visible.get(app.selected.min(visible.len().saturating_sub(1))) {
                if let Some(item) = vault.items().get(idx) {
                    draw_docked_detail(f, detail_area, app, item);
                }
            }
        }
    }
}

/// First visible index for a `cap`-row viewport over `len` rows, keeping the
/// `selected` row on-screen with a centered margin. Stateless — derived purely
/// from the selection so it survives the immutable draw path and index wrap.
fn scroll_start(selected: usize, len: usize, cap: usize) -> usize {
    if cap == 0 || len <= cap {
        return 0;
    }
    let half = cap / 2;
    selected.saturating_sub(half).min(len - cap)
}

/// The one-line item ledger (left of the divider): `▌ [icon] Name ★ ··· sec`,
/// plus a right-aligned relevance badge on the rows a semantic search ranked.
fn draw_item_ledger(f: &mut Frame, area: Rect, app: &App, vault: &crate::vault::Vault) {
    let p = app.palette();
    let lang = app.effective_lang();
    let visible = app.visible_item_indices();
    if visible.is_empty() {
        puts(
            f,
            area.x + 2,
            area.y,
            &i18n::t(lang, &Key::SearchPlaceholder),
            Style::default().fg(p.faint).add_modifier(Modifier::ITALIC),
            area,
        );
        return;
    }
    let name_x = area.x + 5;
    let right = area.x + area.width;
    let avail = right.saturating_sub(name_x) as usize;
    // scroll the list so the cursor is always on-screen (was: clip off bottom)
    let cap = area.height as usize;
    let sel = app.selected.min(visible.len() - 1);
    let first = scroll_start(sel, visible.len(), cap);
    for (row, &idx) in visible.iter().enumerate().skip(first).take(cap) {
        let ty = area.y + (row - first) as u16;
        let item = &vault.items()[idx];
        // the clamped index, exactly as the detail pane resolves it: a filter
        // that shrank under the cursor must not leave the ledger unmarked
        let selected = row == sel;
        let (marker, mut mstyle, tstyle) = row_marker(selected, !app.search_focus, p);
        if selected {
            puts(f, area.x, ty, &" ".repeat(area.width as usize), Style::default().bg(p.selection), area);
            mstyle = mstyle.bg(p.selection);
        }
        puts(f, area.x, ty, marker, mstyle, area);
        draw_item_icon_1(f, area.x + 2, ty, item, app, area);
        if avail == 0 {
            continue;
        }
        // the relevance badge of a RANKED list (fig-10, RAG mode): whole
        // percent, right-aligned past the subtitle. A row without one is a row
        // nothing scored — an item whose vector isn't built yet — and it simply
        // shows no number rather than a zero it hasn't earned.
        let badge = app.item_rank_score(&item.id).map(|s| s.to_string());
        let badge_w = badge.as_ref().map(|b| b.chars().count() + 2).unwrap_or(0);
        // what is left for the name, the leader and the subtitle
        let text_w = avail.saturating_sub(badge_w);
        let star_w = if item.favorite { 2 } else { 0 };
        let name = truncate(&item.label, text_w.saturating_sub(6).max(1));
        let left_w = name.chars().count() + star_w;
        let sub = item_subtitle(item).unwrap_or_default();
        let sec = truncate(&sub, text_w.saturating_sub(left_w + 1));
        let sec_w = sec.chars().count();
        let gap = text_w.saturating_sub(left_w + sec_w);
        let leader = if gap >= 4 { format!(" {} ", "·".repeat(gap - 2)) } else { " ".repeat(gap) };
        let leader_color = if selected { SELECTED_LEADER } else { p.border };
        let row_bg = if selected { Some(p.selection) } else { None };
        let with_bg = |s: Style| match row_bg {
            Some(b) => s.bg(b),
            None => s,
        };
        let mut spans = vec![Span::styled(name, with_bg(tstyle))];
        if item.favorite {
            spans.push(Span::styled(" ★", with_bg(Style::default().fg(STAR_GOLD))));
        }
        spans.push(Span::styled(leader, with_bg(Style::default().fg(leader_color))));
        spans.push(Span::styled(sec, with_bg(Style::default().fg(p.muted))));
        if let Some(badge) = badge {
            spans.push(Span::styled(format!("  {badge}"), with_bg(Style::default().fg(p.steel))));
        }
        let mut para = Paragraph::new(Line::from(spans));
        if let Some(b) = row_bg {
            para = para.style(Style::default().bg(b));
        }
        f.render_widget(para, Rect { x: name_x, y: ty, width: avail as u16, height: 1 });
    }
}

/// The gold used for the ★ favorite glyph (palettes have no gold slot; a fixed
/// RGB is the established precedent, cf. `monogram_color`).
const STAR_GOLD: Color = Color::Rgb(255, 196, 0);

/// A deterministic pleasant colour for an item's monogram, from its label.
fn monogram_color(label: &str) -> Color {
    const PALETTE: [(u8, u8, u8); 10] = [
        (219, 68, 55),
        (66, 133, 244),
        (15, 157, 88),
        (244, 180, 0),
        (171, 71, 188),
        (0, 172, 193),
        (255, 112, 67),
        (92, 107, 192),
        (38, 166, 154),
        (236, 64, 122),
    ];
    let h = label.bytes().fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
    let (r, g, b) = PALETTE[(h as usize) % PALETTE.len()];
    Color::Rgb(r, g, b)
}

/// Draws the item's icon in a 4×2-cell slot at `(x, y)`. In order of preference:
/// (1) the real website favicon rendered CRISP via the terminal graphics
/// protocol (kitty/sixel) when available; (2) the same favicon as low-res
/// half-blocks; (3) a colored monogram of the label's initial.
/// Renders the cached favicon of `domain` into `rect` at full crisp resolution
/// via the terminal graphics protocol. `key` separates render sizes (list tile
/// vs detail) so each keeps its own protocol. Returns `false` (→ caller falls
/// back) when there's no graphics support or the favicon isn't fetched yet.
fn draw_crisp_favicon(f: &mut Frame, rect: Rect, key: &str, domain: &str, app: &App) -> bool {
    let Some(picker) = &app.picker else {
        return false;
    };
    let mut protos = app.icon_protocols.borrow_mut();
    if !protos.contains_key(key) {
        let Some(src) = app.favicons.source(domain) else {
            return false;
        };
        let Some(rgba) = image::RgbaImage::from_raw(crate::favicon::SRC_DIM, crate::favicon::SRC_DIM, src)
        else {
            return false;
        };
        protos.insert(key.to_string(), picker.new_resize_protocol(image::DynamicImage::ImageRgba8(rgba)));
    }
    match protos.get_mut(key) {
        Some(proto) => {
            f.render_stateful_widget(ratatui_image::StatefulImage::new(), rect, proto);
            true
        }
        None => false,
    }
}

/// The compact 2×1-cell icon slot for the dense list rows: a crisp favicon
/// (kitty/sixel), else a half-block tint of the small grid, else a 2-cell
/// colored monogram. At this size it reads as a wayfinding *tint*, not a legible
/// logo — the "cozy" density (future config) restores the larger 4×2 tile.
fn draw_item_icon_1(f: &mut Frame, x: u16, y: u16, item: &crate::model::Item, app: &App, clip: Rect) {
    let domain = if app.settings.favicons {
        crate::model::item_website(item).and_then(crate::favicon::domain_of)
    } else {
        None
    };
    // (1) crisp graphics-protocol render into a 2×1 slot
    if let Some(domain) = domain.as_ref() {
        let key = format!("l1|{domain}");
        let w = 2u16.min((clip.x + clip.width).saturating_sub(x));
        if w > 0 && draw_crisp_favicon(f, Rect { x, y, width: w, height: 1 }, &key, domain, app) {
            return;
        }
    }
    // (2) half-block tint: two cells, each a top/bottom pixel pair of the grid
    if let Some(px) = domain.as_ref().and_then(|d| app.favicons.pixels(d)) {
        let spans: Vec<Span> = (0..2u16)
            .map(|c| {
                let top = crate::favicon::pixel(&px, (c * 2) as usize, 0);
                let bot = crate::favicon::pixel(&px, (c * 2) as usize, 2);
                halfblock_cell(top, bot)
            })
            .collect();
        let w = 2u16.min((clip.x + clip.width).saturating_sub(x));
        if w > 0 {
            f.render_widget(Paragraph::new(Line::from(spans)), Rect { x, y, width: w, height: 1 });
        }
        return;
    }
    // (3) monogram: a 2-cell colored chip with the initial in the first cell
    let color = monogram_color(&item.label);
    let initial = item
        .label
        .chars()
        .find(|c| c.is_alphanumeric())
        .and_then(|c| c.to_uppercase().next())
        .unwrap_or('•');
    put(f, x, y, &initial.to_string(), Style::default().bg(color).fg(Color::Rgb(255, 255, 255)).add_modifier(Modifier::BOLD), clip);
    put(f, x + 1, y, " ", Style::default().bg(color), clip);
}

#[allow(dead_code)]
fn draw_item_icon(f: &mut Frame, x: u16, y: u16, item: &crate::model::Item, app: &App, clip: Rect) {
    let domain = if app.settings.favicons {
        crate::model::item_website(item).and_then(crate::favicon::domain_of)
    } else {
        None
    };

    // (1) crisp graphics-protocol render from the source (kitty/sixel)
    if let Some(domain) = domain.as_ref() {
        let w = (crate::favicon::ICON_COLS as u16).min((clip.x + clip.width).saturating_sub(x));
        let h = (crate::favicon::ICON_ROWS as u16).min((clip.y + clip.height).saturating_sub(y));
        if w > 0 && h > 0 && draw_crisp_favicon(f, Rect { x, y, width: w, height: h }, domain, domain, app) {
            return;
        }
    }

    // (2) low-res half-block render of the 4×4 grid
    if let Some(px) = domain.as_ref().and_then(|d| app.favicons.pixels(d)) {
        let cols = crate::favicon::ICON_COLS as u16;
        for r in 0..crate::favicon::ICON_ROWS as u16 {
            let yy = y + r;
            if yy < clip.y || yy >= clip.y + clip.height {
                continue;
            }
            let spans: Vec<Span> = (0..cols)
                .map(|c| {
                    let top = crate::favicon::pixel(&px, c as usize, (r * 2) as usize);
                    let bot = crate::favicon::pixel(&px, c as usize, (r * 2 + 1) as usize);
                    halfblock_cell(top, bot)
                })
                .collect();
            let w = cols.min((clip.x + clip.width).saturating_sub(x));
            if w > 0 {
                f.render_widget(Paragraph::new(Line::from(spans)), Rect { x, y: yy, width: w, height: 1 });
            }
        }
        return;
    }

    // (3) monogram fallback: a 4×2 colored tile with the label's initial
    let color = monogram_color(&item.label);
    let fill = Style::default().bg(color);
    for dy in 0..2u16 {
        for dx in 0..4u16 {
            put(f, x + dx, y + dy, " ", fill, clip);
        }
    }
    let initial = item
        .label
        .chars()
        .find(|c| c.is_alphanumeric())
        .and_then(|c| c.to_uppercase().next())
        .unwrap_or('•');
    let letter = Style::default()
        .bg(color)
        .fg(Color::Rgb(255, 255, 255))
        .add_modifier(Modifier::BOLD);
    put(f, x + 1, y, &initial.to_string(), letter, clip);
}

/// A muted (optionally italic) indented line for empty-state text.
fn muted_line(text: &str, p: Palette, italic: bool) -> Line<'static> {
    let mut style = Style::default().fg(p.muted);
    if italic {
        style = style.add_modifier(Modifier::ITALIC);
    }
    Line::from(Span::styled(text.to_string(), style))
}

/// Truncates `s` to `max` display columns, adding an ellipsis if it was cut.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    if max <= 1 {
        return s.chars().take(max).collect();
    }
    let head: String = s.chars().take(max - 1).collect();
    format!("{head}…")
}

/// Draws a single styled glyph at `(x, y)` if it's inside `clip`.
fn put(f: &mut Frame, x: u16, y: u16, ch: &str, style: Style, clip: Rect) {
    if x < clip.x || x >= clip.x + clip.width || y < clip.y || y >= clip.y + clip.height {
        return;
    }
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(ch.to_string(), style))),
        Rect { x, y, width: 1, height: 1 },
    );
}

/// Draws a short string starting at `(x, y)`, clipped to `clip`.
fn puts(f: &mut Frame, x: u16, y: u16, s: &str, style: Style, clip: Rect) {
    for (i, ch) in s.chars().enumerate() {
        put(f, x + i as u16, y, &ch.to_string(), style, clip);
    }
}

/// PAC-MAN KILL SCREEN: the glitched level-256 chaos — garbled glyphs in the
/// arcade palette, plus a pac-man gobbling a row of dots.
fn splash_pacman(f: &mut Frame, area: Rect, app: &App, _p: Palette) {
    const GARBLE: &[char] = &[
        'A', 'B', 'C', 'H', 'K', 'M', 'O', 'R', 'Y', 'F', 'G', 'L', '2', '3', '6', '0', '1', '9',
        ':', '.', '=', '¤', '●', '◗', 'ᗣ', 'ᗧ',
    ];
    let colors = [
        Color::Rgb(255, 255, 0),   // pac yellow
        Color::Rgb(255, 40, 40),   // blinky red
        Color::Rgb(255, 184, 255), // pinky
        Color::Rgb(0, 255, 222),   // inky cyan
        Color::Rgb(255, 184, 82),  // clyde orange
        Color::Rgb(70, 90, 255),   // maze blue
        Color::Rgb(240, 240, 240), // dots
    ];
    let (w, h) = (area.width, area.height);
    let reduce = app.settings.reduce_motion;
    let flick = if reduce { 0 } else { app.frame / 3 };
    for row in 0..h {
        let mut spans: Vec<Span> = Vec::with_capacity(w as usize);
        for col in 0..w {
            let s = splitmix(app.splash_seed ^ ((col as u64) << 20) ^ ((row as u64) << 4) ^ flick);
            if s % 100 >= 52 {
                spans.push(Span::raw(" "));
                continue;
            }
            let g = GARBLE[(s as usize / 7) % GARBLE.len()];
            let color = colors[(s as usize / 101) % colors.len()];
            spans.push(Span::styled(g.to_string(), Style::default().fg(color)));
        }
        f.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect { x: area.x, y: area.y + row, width: w, height: 1 },
        );
    }
    // a pac-man eating a row of dots across the middle
    if h >= 3 {
        let py = area.y + h / 2;
        let px = area.x + (if reduce { w / 2 } else { (app.frame as u16) % w });
        let yellow = Style::default().fg(Color::Rgb(255, 255, 0)).add_modifier(Modifier::BOLD);
        let mf = if reduce { 0 } else { app.frame };
        let mouth = if (mf / 3).is_multiple_of(2) { "ᗧ" } else { "●" };
        for c in 0..w {
            let x = area.x + c;
            if x > px {
                put(f, x, py, "·", Style::default().fg(Color::Rgb(240, 240, 240)), area);
            }
        }
        put(f, px, py, mouth, yellow, area);
    }
}

/// One of the ten PONG match variants. All parameters are chosen so the rally
/// is lively and at least one side is beatable, so the score actually moves.
#[derive(Clone, Copy)]
struct PongCfg {
    vx: f32,      // horizontal ball speed (cells/step)
    vy: f32,      // initial vertical ball speed
    l_skill: f32, // left paddle max travel per step
    r_skill: f32, // right paddle max travel per step
    ph: u16,      // paddle half-height in cells
    spin: f32,    // how much an off-centre hit bends the ball
}

/// Ten hand-tuned, fully deterministic PONG matches. When PONG comes up a
/// random one is played (picked from the per-visit splash seed).
const PONG_GAMES: [PongCfg; 10] = [
    PongCfg { vx: 1.00, vy: 0.50, l_skill: 0.55, r_skill: 0.55, ph: 2, spin: 0.08 },
    PongCfg { vx: 0.85, vy: 0.60, l_skill: 0.85, r_skill: 0.40, ph: 2, spin: 0.07 },
    PongCfg { vx: 0.85, vy: 0.60, l_skill: 0.40, r_skill: 0.85, ph: 2, spin: 0.07 },
    PongCfg { vx: 0.70, vy: 0.80, l_skill: 0.60, r_skill: 0.60, ph: 2, spin: 0.10 },
    PongCfg { vx: 0.65, vy: 0.40, l_skill: 0.50, r_skill: 0.50, ph: 2, spin: 0.05 },
    PongCfg { vx: 1.10, vy: 0.55, l_skill: 0.70, r_skill: 0.65, ph: 1, spin: 0.09 },
    PongCfg { vx: 0.90, vy: 0.70, l_skill: 0.45, r_skill: 0.75, ph: 1, spin: 0.09 },
    PongCfg { vx: 0.80, vy: 0.45, l_skill: 0.60, r_skill: 0.55, ph: 2, spin: 0.12 },
    PongCfg { vx: 1.05, vy: 0.65, l_skill: 0.75, r_skill: 0.70, ph: 2, spin: 0.08 },
    PongCfg { vx: 0.75, vy: 0.50, l_skill: 0.50, r_skill: 0.55, ph: 1, spin: 0.07 },
];

/// Simulated PONG state after `steps` frames.
struct PongState {
    bx: f32,
    by: f32,
    lc: f32, // left paddle centre
    rc: f32, // right paddle centre
    ls: u32, // left score
    rs: u32, // right score
}

/// Moves a paddle centre one step toward the ball (when it's coming at it) or
/// drifts it back to centre, capped at `skill` and kept inside the field.
fn pong_move_paddle(c: &mut f32, ball_y: f32, skill: f32, chasing: bool, fh: f32, ph: f32) {
    let goal = if chasing { ball_y } else { fh * 0.5 };
    let d = goal - *c;
    *c += d.clamp(-skill, skill);
    let hi = (fh - ph).max(ph);
    *c = c.clamp(ph, hi);
}

/// Deterministically simulates `steps` frames of one PONG match on a field of
/// `fw × fh` cells. A point is scored only when the ball passes a paddle and
/// reaches the wall behind it — never at mid-court.
fn pong_sim(fw: f32, fh: f32, ph: f32, cfg: PongCfg, steps: u64) -> PongState {
    let lx = 1.0_f32; // left paddle plane
    let rx = fw - 1.0; // right paddle plane
    let vymax = 1.0_f32;
    let mut bx = fw * 0.5;
    let mut by = fh * 0.5;
    let mut vx = cfg.vx;
    let mut vy = cfg.vy;
    let mut lc = fh * 0.5;
    let mut rc = fh * 0.5;
    let mut ls = 0u32;
    let mut rs = 0u32;
    let mut serve = 0u32;
    for _ in 0..steps {
        pong_move_paddle(&mut lc, by, cfg.l_skill, vx < 0.0, fh, ph);
        pong_move_paddle(&mut rc, by, cfg.r_skill, vx > 0.0, fh, ph);
        let px = bx;
        bx += vx;
        by += vy;
        // top / bottom walls (bounded reflection, then clamp for safety)
        for _ in 0..4 {
            if by < 0.0 {
                by = -by;
                vy = vy.abs();
            } else if by > fh {
                by = 2.0 * fh - by;
                vy = -vy.abs();
            } else {
                break;
            }
        }
        by = by.clamp(0.0, fh);
        // left paddle: bounce if it's there, otherwise the ball sails past
        if vx < 0.0 && px > lx && bx <= lx && (by - lc).abs() <= ph + 1.0 {
            bx = lx + (lx - bx);
            vx = vx.abs();
            vy = (vy + (by - lc) * cfg.spin).clamp(-vymax, vymax);
        }
        if bx < 0.0 {
            rs += 1;
            serve += 1;
            bx = fw * 0.5;
            by = fh * 0.5;
            vx = cfg.vx.abs() * if serve.is_multiple_of(2) { 1.0 } else { -1.0 };
            vy = cfg.vy.abs() * if (serve / 2).is_multiple_of(2) { 1.0 } else { -1.0 };
            lc = fh * 0.5;
            rc = fh * 0.5;
            continue;
        }
        // right paddle (mirror)
        if vx > 0.0 && px < rx && bx >= rx && (by - rc).abs() <= ph + 1.0 {
            bx = rx - (bx - rx);
            vx = -vx.abs();
            vy = (vy + (by - rc) * cfg.spin).clamp(-vymax, vymax);
        }
        if bx > fw {
            ls += 1;
            serve += 1;
            bx = fw * 0.5;
            by = fh * 0.5;
            vx = cfg.vx.abs() * if serve.is_multiple_of(2) { -1.0 } else { 1.0 };
            vy = cfg.vy.abs() * if (serve / 2).is_multiple_of(2) { -1.0 } else { 1.0 };
            lc = fh * 0.5;
            rc = fh * 0.5;
        }
    }
    PongState { bx, by, lc, rc, ls: ls % 100, rs: rs % 100 }
}

/// PONG: a real, deterministic match (one of ten, chosen at random per visit) —
/// bouncing ball, ball-tracking paddles, a dotted net, a live score that only
/// changes when the ball gets past a paddle, and the PONG wordmark.
fn splash_pong(f: &mut Frame, area: Rect, app: &App, p: Palette) {
    let (w, h) = (area.width, area.height);
    if w < 12 || h < 6 {
        return;
    }
    let white = Style::default().fg(Color::Rgb(235, 235, 235));
    let reduce = app.settings.reduce_motion;
    // run several physics steps per displayed frame so the match plays at a
    // brisk pace (the per-step ball speed stays small, so collisions stay exact).
    let speed = 4u64;
    // loop the match over a long period so frames never grow unbounded; the
    // wrap lands on a fresh serve, which just reads as a new point.
    let period = 900u64;
    let fcount = if reduce { 45 } else { app.frame % period };
    // pick one of the ten matches from the seed (independent of effect choice)
    let game = (splitmix(app.splash_seed ^ 0xA5A5_5A5A_C3C3_3C3C) % PONG_GAMES.len() as u64) as usize;
    let cfg = PONG_GAMES[game];

    // field interior: reserve the top row for the score and the bottom for the
    // wordmark, so the ball/paddles never hide under them.
    let fw = (w - 1) as f32;
    let fh = (h - 3) as f32;
    let ph = cfg.ph.min((h.saturating_sub(3)) / 2);
    let st = pong_sim(fw, fh, ph as f32, cfg, fcount * speed);

    // dotted centre net
    let netx = area.x + w / 2;
    for row in 0..h {
        if row.is_multiple_of(2) {
            put(f, netx, area.y + row, "┊", Style::default().fg(p.border), area);
        }
    }
    // paddles
    let top = area.y + 1;
    let lc = st.lc.round() as i32;
    let rc = st.rc.round() as i32;
    for i in -(ph as i32)..=(ph as i32) {
        let yl = lc + i;
        let yr = rc + i;
        if yl >= 0 {
            put(f, area.x + 1, top + yl as u16, "█", white, area);
        }
        if yr >= 0 {
            put(f, area.x + w - 2, top + yr as u16, "█", white, area);
        }
    }
    // ball
    let bxp = area.x + st.bx.round().clamp(0.0, fw) as u16;
    let byp = top + st.by.round().clamp(0.0, fh) as u16;
    put(f, bxp, byp, "●", white.add_modifier(Modifier::BOLD), area);

    // live score (top) and PONG wordmark (bottom)
    let score = format!("{}   {}", st.ls, st.rs);
    puts(f, netx.saturating_sub(score.len() as u16 / 2), area.y, &score, white.add_modifier(Modifier::BOLD), area);
    let mark = "P O N G";
    puts(f, (area.x + w / 2).saturating_sub(3), area.y + h - 1, mark, Style::default().fg(p.muted).add_modifier(Modifier::BOLD), area);
}

/// A small fast integer hash (SplitMix64) for deterministic per-cell noise.
fn splitmix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// The classic green "digital rain": per-column drops of glyphs that fall and
/// fade. Steady (frozen) under reduce-motion.
fn splash_matrix(f: &mut Frame, area: Rect, app: &App, _p: Palette) {
    const GLYPHS: &[char] = &[
        'ﾊ', 'ﾋ', 'ﾎ', 'ｱ', 'ｶ', 'ｷ', 'ｹ', 'ｻ', 'ﾂ', 'ﾈ', 'ﾟ', '0', '1', '2', '7', '9', ':', '.',
        '=', '*', '+',
    ];
    let (w, h) = (area.width, area.height);
    let reduce = app.settings.reduce_motion;
    let frame = if reduce { 0 } else { app.frame };
    let seed = app.splash_seed;
    let tail: i32 = 6;
    // per-column head position + phase seed
    let heads: Vec<(i32, u64)> = (0..w)
        .map(|col| {
            let ch = splitmix(seed ^ (col as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15));
            let period = h as u64 + tail as u64 + (ch % 10) + 4;
            let phase = if reduce { h as u64 / 2 } else { frame.wrapping_add(ch % period) % period };
            (phase as i32, ch)
        })
        .collect();
    for row in 0..h {
        let mut spans: Vec<Span> = Vec::with_capacity(w as usize);
        for col in 0..w {
            let (head, cseed) = heads[col as usize];
            let d = head - row as i32; // rows above the (falling) head
            if d < 0 || d > tail {
                spans.push(Span::raw(" "));
                continue;
            }
            let flick = frame / 2;
            let hh = splitmix(cseed ^ ((row as u64) << 20) ^ flick.wrapping_shl(3));
            let glyph = GLYPHS[(hh % GLYPHS.len() as u64) as usize];
            let color = if d == 0 {
                Color::Rgb(205, 255, 210) // bright head
            } else {
                let fade = 1.0 - d as f32 / tail as f32;
                Color::Rgb(
                    (18.0 + 34.0 * fade) as u8,
                    (52.0 + 200.0 * fade) as u8,
                    (28.0 + 60.0 * fade) as u8,
                )
            };
            spans.push(Span::styled(glyph.to_string(), Style::default().fg(color)));
        }
        f.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect { x: area.x, y: area.y + row, width: w, height: 1 },
        );
    }
}

/// A horizontally centred single line at `*y`, advancing `*y`.
fn centered(f: &mut Frame, x: u16, width: u16, y: &mut u16, bottom: u16, span: Span<'static>) {
    if *y < bottom {
        let rect = Rect { x, y: *y, width, height: 1 };
        f.render_widget(
            Paragraph::new(Line::from(span)).alignment(Alignment::Center),
            rect,
        );
    }
    *y += 1;
}

/// A horizontally centred, wrapped block of `height` rows at `*y`.
fn centered_wrapped(
    f: &mut Frame,
    x: u16,
    width: u16,
    y: &mut u16,
    bottom: u16,
    height: u16,
    span: Span<'static>,
) {
    let h = height.min(bottom.saturating_sub(*y));
    if h > 0 {
        let rect = Rect { x, y: *y, width, height: h };
        f.render_widget(
            Paragraph::new(Line::from(span))
                .wrap(Wrap { trim: true })
                .alignment(Alignment::Center),
            rect,
        );
    }
    *y += height;
}

/// A command form: a floating modal (its command name on the border), one row
/// per field with underline inputs. `interactive` shows the focused cursor.
fn draw_form(f: &mut Frame, area: Rect, app: &App, form: &FormState, focused: bool, interactive: bool) {
    let p = app.palette();
    let _ = focused;
    let _ = interactive;
    let inner = draw_modal_frame(f, area, &form.command.to_uppercase(), p);
    if inner.width < 8 || inner.height < 5 {
        return;
    }

    let bottom = inner.y + inner.height;
    let mut y = inner.y + 1;
    centered(f, inner.x, inner.width, &mut y, bottom, Span::styled(form.title.clone(), accent_bold(p)));
    y += 1;

    // one row per field (fig-06): a right-aligned label column + an underline
    // value surface; the helper prose shows only under the focused field.
    let lx = inner.x + 2;
    let label_w = form
        .fields
        .iter()
        // adder rows render as `+ name`: two extra columns
        .map(|fd| fd.name.replace('_', " ").chars().count() + if fd.adder { 2 } else { 0 })
        .max()
        .unwrap_or(6)
        .clamp(6, 18);
    let val_x = lx + label_w as u16 + 1;
    let val_w = (inner.x + inner.width).saturating_sub(val_x + 1).max(1) as usize;

    for (i, field) in form.fields.iter().enumerate() {
        if y >= bottom.saturating_sub(1) {
            puts(f, val_x, y, "▾ more", Style::default().fg(p.muted), inner);
            break;
        }
        let current = interactive && i == form.focused;
        // a `+ add field` row: the growing custom-field adder (faint `+ name`)
        let label = if field.adder {
            format!("+ {}", field.name.replace('_', " "))
        } else {
            field.name.replace('_', " ")
        };
        let label_style = if field.adder && !current {
            Style::default().fg(p.faint).add_modifier(Modifier::ITALIC)
        } else if current && focused {
            accent_bold(p)
        } else if current {
            Style::default().fg(p.accent_soft).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.muted)
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(if current { "▌ " } else { "  " }, Style::default().fg(p.accent)),
                Span::styled(format!("{label:>label_w$}"), label_style),
            ])),
            Rect { x: inner.x, y, width: val_x.saturating_sub(inner.x), height: 1 },
        );

        if field.is_choice() {
            // a choice cycles with ←/→, shown as ◂ value ▸ (steel value)
            let arrow = if current { p.accent } else { p.accent_soft };
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("◂ ", Style::default().fg(arrow)),
                    Span::styled(field.current().to_string(), Style::default().fg(p.steel)),
                    Span::styled(" ▸", Style::default().fg(arrow)),
                ])),
                Rect { x: val_x, y, width: val_w as u16, height: 1 },
            );
        } else {
            // an EDIT form's untouched secret keeps the old value: quiet eight
            // dots + an "unchanged — type to replace" tag (fig-06 field states).
            let editing = matches!(form.purpose, FormPurpose::EditItem(_));
            if editing && field.secret && field.value.is_empty() {
                let tag = match app.effective_lang() {
                    Lang::En => "unchanged — type to replace",
                    Lang::It => "invariato — digita per sostituire",
                };
                f.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled("•".repeat(MASK_LEN), Style::default().fg(p.muted)),
                        Span::styled(format!("  {tag}"), Style::default().fg(p.faint)),
                    ])),
                    Rect { x: val_x, y, width: val_w as u16, height: 1 },
                );
                y += 1;
                if current && y < bottom.saturating_sub(1) {
                    y += 0; // helper suppressed — the tag already explains it
                }
                continue;
            }
            // the live strength meter rides a non-empty PASSWORD row (fig-06):
            // a cvv/pin/card number/seed is masked too, but has no strength
            let meter: Vec<Span> = if field.show_strength && !field.value.is_empty() {
                strength_spans(&field.value, p)
            } else {
                Vec::new()
            };
            let meter_w = spans_w(&meter) + if meter.is_empty() { 0 } else { 1 };
            let maxw = val_w.saturating_sub(1 + meter_w);
            let show_cursor = current && focused && app.cursor_on();
            // underline value surface (not a box). A SECRET row is masked — one
            // bullet per typed char, its caret NOT drawn mid-mask (the position
            // must not leak) so the cursor stays on the end and only the tail
            // shows when it is long. A PLAIN row splits at the caret and scrolls
            // its window to keep the caret in view.
            let (before, after) = if field.secret {
                let full = "•".repeat(field.value.chars().count());
                let shown: String = if full.chars().count() > maxw {
                    full.chars().skip(full.chars().count() - maxw).collect()
                } else {
                    full
                };
                (shown, String::new())
            } else {
                // + 1: view reserves a column for the caret glyph, and `maxw`
                // already left one for it
                field.value.view(maxw + 1)
            };
            let used =
                before.chars().count() + after.chars().count() + usize::from(show_cursor) + meter_w;
            let pad = val_w.saturating_sub(used);
            let text_style = Style::default().fg(p.text).add_modifier(Modifier::UNDERLINED);
            let mut spans = vec![Span::styled(before, text_style)];
            if show_cursor {
                spans.push(Span::styled("▏", Style::default().fg(p.accent)));
            }
            spans.push(Span::styled(after, text_style));
            spans.push(Span::styled("─".repeat(pad), Style::default().fg(p.border)));
            if !meter.is_empty() {
                spans.push(Span::raw(" "));
                spans.extend(meter);
            }
            f.render_widget(
                Paragraph::new(Line::from(spans)),
                Rect { x: val_x, y, width: val_w as u16, height: 1 },
            );
        }
        y += 1;

        // helper prose ONLY under the focused field (quiet — never an alarm).
        if current && !field.description.is_empty() && y < bottom.saturating_sub(1) {
            puts(f, val_x, y, &truncate(&field.description, val_w), Style::default().fg(p.faint), inner);
            y += 1;
        }
    }

    // delete/nuke (vault OR item): the chosen vault's info + a warning strip
    if matches!(
        form.purpose,
        FormPurpose::DeleteVault | FormPurpose::NukeVault | FormPurpose::NukeItems
    ) {
        if let Some((_, algo, kdf)) = app.manage_target_info() {
            centered(
                f,
                inner.x,
                inner.width,
                &mut y,
                bottom,
                Span::styled(format!("{algo} · {kdf}"), Style::default().fg(p.steel)),
            );
        }
        let warning = match &form.purpose {
            FormPurpose::NukeVault => Key::NukeWarning,
            FormPurpose::NukeItems => Key::NukeItemsWarning,
            _ => Key::DeleteWarning,
        };
        centered_wrapped(
            f,
            inner.x,
            inner.width,
            &mut y,
            bottom,
            2,
            Span::styled(i18n::t(app.effective_lang(), &warning), Style::default().fg(p.danger)),
        );
    }

    if let Some(err) = &form.error {
        centered(
            f,
            inner.x,
            inner.width,
            &mut y,
            bottom,
            Span::styled(
                format!("⚠ {err}"),
                Style::default().fg(p.danger).add_modifier(Modifier::BOLD),
            ),
        );
    }

    // footer: the form's interaction contract (fig-06), pinned to the bottom
    let footer = match app.effective_lang() {
        Lang::En => "⏎ next · on the last field ⏎ saves · Esc cancel",
        Lang::It => "⏎ avanti · sull'ultimo campo ⏎ salva · Esc annulla",
    };
    let fy = bottom.saturating_sub(1);
    if fy > y {
        puts(f, inner.x + 1, fy, footer, Style::default().fg(p.faint), inner);
    }
}

/// A generic closed-set chooser (languages, `~` directions), rendered like the
/// old language list: `command value  — label`, navigable, with a keys hint.
fn picker_lines(app: &App, kind: PickerKind, focused: bool) -> Vec<Line<'static>> {
    let p = app.palette();
    let header = match kind {
        PickerKind::Language => Key::AvailableLanguages,
        PickerKind::PanelHistory => Key::AvailableDirections,
        PickerKind::ItemCategory => Key::ChooseCategory,
    };
    let mut out = vec![Line::from(Span::styled(
        i18n::t(app.effective_lang(), &header),
        Style::default().fg(p.muted),
    ))];
    // navigated with ↑↓ + Enter (no command to type), so just the labels.
    let index = app.panel_index();
    for (i, choice) in app.picker_choices(kind).into_iter().enumerate() {
        let selected = i == index;
        let (marker, mstyle, tstyle) = row_marker(selected, focused, p);
        out.push(Line::from(vec![
            Span::styled(marker, mstyle),
            Span::styled(choice.label, tstyle),
        ]));
    }
    out
}

/// The `encInfo` view: the vault's (or one item's) NON-secret encryption
/// metadata — never a field value or key byte.
/// The encryption card (fig-09): `── ENCRYPTION — VAULT ──`, 11-ch
/// right-aligned muted labels, steel values with faint annotations, blank rows
/// between groups, and — when the KDF sits below the OWASP baseline — a `!`
/// lint line that tells you `:rerollEnc` fixes it.
fn enc_info_lines(app: &App, item_id: Option<&str>) -> Vec<Line<'static>> {
    let p = app.palette();
    let lang = app.effective_lang();
    let Some(vault) = app.vault.as_ref() else {
        return Vec::new();
    };
    let muted = Style::default().fg(p.muted);
    let faint = Style::default().fg(p.faint);
    let steel = Style::default().fg(p.steel);
    // 11-ch right-aligned muted label column + value spans
    let kv = |label: &str, mut spans: Vec<Span<'static>>| -> Line<'static> {
        let mut v = vec![Span::styled(format!("{label:>11} "), muted)];
        v.append(&mut spans);
        Line::from(v)
    };
    let rule = |label: String| -> Line<'static> {
        Line::from(vec![
            Span::styled("── ", Style::default().fg(p.border)),
            Span::styled(label, accent_bold(p)),
            Span::styled(" ──────────────────────────────", Style::default().fg(p.border)),
        ])
    };

    if let Some(id) = item_id {
        let Some(item) = vault.items().iter().find(|i| i.id == id) else {
            return vec![Line::from(Span::styled(i18n::t(lang, &Key::ItemNotFound(id)), muted))];
        };
        let secret_count = item.fields.iter().filter(|f| f.secret).count();
        return vec![
            rule(item.label.to_uppercase()),
            Line::from(""),
            kv("id", vec![Span::styled(item.id.clone(), steel)]),
            kv("created", vec![Span::styled(item.created_at.clone().unwrap_or_else(|| "—".into()), steel)]),
            kv(
                "modified",
                vec![Span::styled(
                    item.modified_at.clone().unwrap_or_else(|| i18n::t(lang, &Key::EncInfoNeverModified)),
                    steel,
                )],
            ),
            kv("fields", vec![Span::styled(i18n::t(lang, &Key::EncInfoItemFields(item.fields.len(), secret_count)), steel)]),
            Line::from(""),
            Line::from(Span::styled(i18n::t(lang, &Key::EncInfoItemNote), faint.add_modifier(Modifier::ITALIC))),
        ];
    }

    let info = vault.enc_info();
    let (nonce_len, ct_len, file_len, disk_ver) = match &info.disk {
        Some(d) => (d.nonce_len, d.ciphertext_len, d.file_len, d.on_disk_version),
        None => (0usize, 0usize, 0u64, info.opened_format),
    };
    let format_str = if disk_ver == 3 || info.opened_format == 3 {
        i18n::t(lang, &Key::EncInfoUpgradePending)
    } else {
        "v4 (DCS-1)".to_string()
    };
    let mut out = vec![
        rule(format!("ENCRYPTION — {}", vault.username.to_uppercase())),
        Line::from(""),
        kv("algorithm", vec![Span::styled(info.algorithm.name().to_string(), steel)]),
        kv("key", vec![
            Span::styled(format!("{}-bit", info.algorithm.key_len() * 8), steel),
            Span::styled(format!(" · {}", i18n::t(lang, &Key::EncInfoKeyNeverStored)), faint),
        ]),
        kv("nonce", vec![
            Span::styled(format!("{nonce_len} B"), steel),
            Span::styled(format!(" · {}", i18n::t(lang, &Key::EncInfoFreshNonce)), faint),
        ]),
        Line::from(""),
        kv("kdf", vec![Span::styled(info.kdf.description(), steel)]),
        kv("canonical", vec![Span::styled(info.kdf.canonical(), steel)]),
        kv("salt", vec![
            Span::styled(format!("{} B", info.salt_len), steel),
            Span::styled(format!(" · {}", info.salt_b64), faint),
        ]),
        Line::from(""),
        kv("sealed", vec![Span::styled(format!("{ct_len} B data · {file_len} B file · {format_str}"), steel)]),
        kv("contents", vec![Span::styled(i18n::t(lang, &Key::EncInfoContents(info.item_count, info.category_count)), steel)]),
    ];
    // the OWASP lint (fig-09): a reverse `!` badge + what fixes it — fires on
    // weak parameters, not just a weak family (m=8 KiB Argon2id is still weak)
    if info.kdf.below_owasp() || !matches!(info.kdf.hardness(), crate::crypto::Hardness::Hardened) {
        out.push(Line::from(""));
        out.push(Line::from(vec![
            Span::styled("          ", Style::default()),
            Span::styled("!", Style::default().bg(p.warning).fg(p.on_accent).add_modifier(Modifier::BOLD)),
            Span::styled(" ", Style::default()),
            Span::styled(
                format!("{} — below OWASP baseline · ", info.kdf.canonical()),
                Style::default().fg(p.warning),
            ),
            Span::styled(":rerollEnc", Style::default().fg(p.chip_fg)),
            Span::styled(" fixes this", Style::default().fg(p.warning)),
        ]));
    }
    out
}

/// A section rule inside a pane: `── LABEL ─────────` in hairline, the label
/// muted. Used to group the detail view (FIELDS · ACTIVITY).
fn section_rule(f: &mut Frame, x: u16, y: u16, w: u16, label: &str, p: Palette, clip: Rect) {
    if w == 0 {
        return;
    }
    let used = label.chars().count() + 4;
    let tail = (w as usize).saturating_sub(used);
    let spans = vec![
        Span::styled("── ", Style::default().fg(p.border)),
        Span::styled(label.to_string(), Style::default().fg(p.muted).add_modifier(Modifier::BOLD)),
        Span::styled(" ", Style::default().fg(p.border)),
        Span::styled("─".repeat(tail), Style::default().fg(p.border)),
    ];
    if y >= clip.y && y < clip.y + clip.height {
        f.render_widget(Paragraph::new(Line::from(spans)), Rect { x, y, width: w, height: 1 });
    }
}

/// The 0–4 strength bar for a password: `▮▮▮▮▯ strong` — filled cells green
/// (or orange when weak, red at 0), the rest hairline (zxcvbn score).
fn strength_spans(password: &str, p: Palette) -> Vec<Span<'static>> {
    let score = zxcvbn::zxcvbn(password, &[]).score() as u8;
    let (color, word) = match score {
        0 => (p.danger, "weak"),
        1 => (p.warning, "weak"),
        2 => (p.warning, "fair"),
        3 => (p.green, "good"),
        _ => (p.green, "strong"),
    };
    let filled = (score as usize).clamp(1, 5);
    vec![
        Span::styled("▮".repeat(filled), Style::default().fg(color)),
        Span::styled("▯".repeat(5 - filled), Style::default().fg(p.border)),
        Span::styled(format!(" {word}"), Style::default().fg(color)),
    ]
}

/// The ALWAYS-docked live detail (fig-03/04): a favicon and header top-left,
/// then `── FIELDS ──` as a numbered left-aligned ledger (`1 label  value`,
/// digits faint, values steel, masks eight bullets, a strength meter riding the
/// password row) and `── ACTIVITY ──`. It follows the cursor — "open item" is
/// not a screen.
fn draw_docked_detail(f: &mut Frame, inner: Rect, app: &App, item: &crate::model::Item) {
    let p = app.palette();
    let lang = app.effective_lang();
    if inner.width < 8 || inner.height < 4 {
        return;
    }
    let x = inner.x;
    let width = inner.width;
    let right = inner.x + inner.width;
    let bottom = inner.y + inner.height;
    let mut y = inner.y;

    // ── header: favicon (left) + name / category / website (right) ──────────
    let fav_w = 10u16.min(width.saturating_sub(2));
    let fav_h = 4u16.min(inner.height.saturating_sub(2)).max(1);
    draw_detail_icon(f, x, y, fav_w, fav_h, item, app, inner);
    let hx = x + fav_w + 2;
    if hx < right {
        let hw = right - hx;
        let mut name = vec![Span::styled(item.label.clone(), Style::default().fg(p.text).add_modifier(Modifier::BOLD))];
        if item.favorite {
            name.push(Span::styled(" ★", Style::default().fg(p.accent)));
        }
        f.render_widget(Paragraph::new(Line::from(name)), Rect { x: hx, y, width: hw, height: 1 });
        let vault_name = app.vault.as_ref().map(|v| v.username.clone()).unwrap_or_default();
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(i18n::t(lang, &Key::CategoryName(&item.category)), Style::default().fg(p.steel)),
                Span::styled(format!(" · {vault_name}"), Style::default().fg(p.faint)),
            ])),
            Rect { x: hx, y: y + 1, width: hw, height: 1 },
        );
        if let Some(site) = crate::model::item_website(item) {
            puts(f, hx, y + 2, &format!("{site} ↗"), Style::default().fg(p.steel), inner);
        }
    }
    y += fav_h.max(3) + 1;

    // ── FIELDS ──────────────────────────────────────────────────────────────
    section_rule(f, x, y, width, "FIELDS", p, inner);
    y += 1;
    if item.fields.is_empty() {
        puts(f, x, y, &i18n::t(lang, &Key::ItemDetailEmpty), Style::default().fg(p.faint), inner);
        y += 1;
    }
    // a label column sized to the longest localized field label (EN/IT safe)
    let label_w = item
        .fields
        .iter()
        .map(|fld| fld.label.clone().unwrap_or_else(|| i18n::t(lang, &Key::FieldLabel(&fld.key))).chars().count())
        .max()
        .unwrap_or(0)
        .clamp(6, 14);
    // the rows the cursor walks, the row a copy takes and the rows drawn here
    // are ONE list (`detail_field_rows`), so they can never drift apart.
    let rows = crate::app::detail_field_rows(item);
    let focused = app.detail_focused();
    let cursor = app.detail_index();
    let edit = app.detail_edit.as_ref().filter(|_| focused);
    // …and the pane SCROLLS to the cursor (the ledger's idiom): the cursor walks
    // every row, so a taller item than the pane must never leave it off-screen —
    // a copy or an inline edit on an invisible row is typing blind.
    let cap = bottom.saturating_sub(1).saturating_sub(y) as usize;
    let first = scroll_start(cursor, rows.len(), cap);
    for (row, &fi) in rows.iter().enumerate().skip(first).take(cap) {
        if y >= bottom.saturating_sub(1) {
            break;
        }
        let Some(field) = item.fields.get(fi) else { continue };
        let label = field.label.clone().unwrap_or_else(|| i18n::t(lang, &Key::FieldLabel(&field.key)));
        let vw = right.saturating_sub(x + 4 + label_w as u16) as usize;
        // the row under the field cursor wears the selection band: the
        // background is painted FIRST and everything rides on it (the ledger's
        // idiom — see `draw_item_ledger`).
        let on_cursor = focused && row == cursor;
        if on_cursor {
            puts(f, x, y, &" ".repeat(width as usize), Style::default().bg(p.selection), inner);
        }
        let band = |s: Style| if on_cursor { s.bg(p.selection) } else { s };
        let row_bg = |para: Paragraph<'static>| {
            if on_cursor {
                para.style(Style::default().bg(p.selection))
            } else {
                para
            }
        };

        // an image row types a FILE PATH — never masked, and its stored base64
        // never reaches an input box (see App::start_detail_edit)
        let image = crate::image_field::is_image(field);

        // ── the row being rewritten in place: the input replaces the value ──
        if edit.is_some_and(|e| e.field == fi) {
            let typed = edit.map(|e| e.buffer.as_str()).unwrap_or("");
            let mut spans = vec![
                Span::styled(format!("{} ", row + 1), band(Style::default().fg(p.faint))),
                Span::styled(format!("{label:<label_w$}  "), band(Style::default().fg(p.muted))),
            ];
            if typed.is_empty() && (field.secret || image) {
                // an untouched secret still holds its stored value — confirming
                // empty KEEPS it, so say that instead of implying an erase. An
                // image says the same thing about the picture it is holding.
                let (stored, hint) = if image {
                    (crate::image_field::placeholder(lang, field), Key::ImageEditHint)
                } else {
                    ("•".repeat(MASK_LEN), Key::InlineSecretHint)
                };
                spans.push(Span::styled(stored, band(Style::default().fg(p.muted))));
                spans.push(Span::styled(
                    format!("  {}", i18n::t(lang, &hint)),
                    band(Style::default().fg(p.faint)),
                ));
            } else {
                let maxw = vw.saturating_sub(1).max(1);
                let underlined = Style::default().fg(p.text).add_modifier(Modifier::UNDERLINED);
                // a SECRET row echoes one bullet per TYPED character (the form's
                // live-input idiom, mirroring the buffer — never a stored value)
                // with the caret kept off the mask so its position can't leak; a
                // PLAIN row (an image PATH, a non-secret field) splits at the
                // caret and scrolls to keep it in view.
                let (before, after) = if field.secret && !image {
                    let full = "•".repeat(typed.chars().count());
                    let shown: String = if full.chars().count() > maxw {
                        full.chars().skip(full.chars().count() - maxw).collect()
                    } else {
                        full
                    };
                    (shown, String::new())
                } else {
                    edit.map(|e| e.buffer.view(maxw + 1)).unwrap_or_default()
                };
                spans.push(Span::styled(before, band(underlined)));
                if app.cursor_on() {
                    spans.push(Span::styled("▏", band(Style::default().fg(p.accent))));
                }
                spans.push(Span::styled(after, band(underlined)));
            }
            f.render_widget(
                row_bg(Paragraph::new(Line::from(spans))),
                Rect { x, y, width, height: 1 },
            );
            y += 1;
            continue;
        }

        // an image field NEVER prints its payload: the ledger row is a
        // placeholder read off `x_image` (`image · JPEG · 1.2 MiB`) whatever the
        // reveal toggle says — the picture itself is drawn under the ledger.
        if image {
            let spans = vec![
                Span::styled(format!("{} ", row + 1), band(Style::default().fg(p.faint))),
                Span::styled(format!("{label:<label_w$}  "), band(Style::default().fg(p.muted))),
                Span::styled(
                    truncate(&crate::image_field::placeholder(lang, field), vw.max(1)),
                    band(Style::default().fg(p.muted)),
                ),
            ];
            f.render_widget(
                row_bg(Paragraph::new(Line::from(spans))),
                Rect { x, y, width, height: 1 },
            );
            y += 1;
            continue;
        }

        // a TOTP field shows the LIVE code it generates (`824 019 ⟳ 12 s`),
        // never the seed — the code is what you type, and it dies in 30 s.
        if crate::clipboard::is_totp_field(field) && !field.value.is_empty() {
            if let Some((code, left)) = crate::totp::code_now(field.value.as_str()) {
                let grouped = format!("{} {}", &code[..3], &code[3..]);
                let spans = vec![
                    Span::styled(format!("{} ", row + 1), band(Style::default().fg(p.faint))),
                    Span::styled(format!("{label:<label_w$}  "), band(Style::default().fg(p.muted))),
                    Span::styled(grouped, band(Style::default().fg(p.text).add_modifier(Modifier::BOLD))),
                ];
                f.render_widget(
                    row_bg(Paragraph::new(Line::from(spans))),
                    Rect { x, y, width, height: 1 },
                );
                // the rotation countdown rides the row, right-aligned (fig-03)
                let tick = format!("⟳ {left} s");
                let tw = tick.chars().count() as u16;
                if right > x + tw + 2 {
                    puts(f, right - tw, y, &tick, band(Style::default().fg(p.faint)), inner);
                }
                y += 1;
                continue;
            }
        }

        let masked = field.secret && !app.detail_reveal;
        let shown = if masked { "•".repeat(MASK_LEN) } else { field.value.as_str().to_string() };
        let value_style = if masked {
            Style::default().fg(p.muted)
        } else {
            Style::default().fg(p.steel).add_modifier(Modifier::BOLD)
        };
        let spans = vec![
            // the row number: what the cursor counts, faint so it never shouts
            Span::styled(format!("{} ", row + 1), band(Style::default().fg(p.faint))),
            Span::styled(format!("{label:<label_w$}  "), band(Style::default().fg(p.muted))),
            Span::styled(truncate(&shown, vw.max(1)), band(value_style)),
        ];
        f.render_widget(
            row_bg(Paragraph::new(Line::from(spans))),
            Rect { x, y, width, height: 1 },
        );
        // the strength meter rides the PASSWORD row, right-aligned (fig-03) —
        // and only that row: a cvv, pin, card number or seed is masked too, but
        // scoring its "strength" would be noise
        if field.secret
            && field.ftype == crate::model::FieldType::Password
            && !field.value.is_empty()
        {
            let meter = strength_spans(field.value.as_str(), p);
            let mw = spans_w(&meter) as u16;
            if right > x + mw + 2 {
                f.render_widget(
                    row_bg(Paragraph::new(Line::from(meter))),
                    Rect { x: right - mw, y, width: mw, height: 1 },
                );
            }
        }
        y += 1;
    }

    // ── IMAGES ────────────────────────────────────────────────────────────
    // the pictures the ledger only named, decoded lazily and only for the item
    // on show
    draw_image_thumbs(f, x, &mut y, width, bottom, item, app, inner);

    // ── ACTIVITY ──────────────────────────────────────────────────────────
    // Every row here is clipped to the pane exactly like `section_rule` does:
    // a tall item (an identity with both photo slots pushes the IMAGES strip
    // right down to the last row) leaves `y` ON the bottom edge, and a bare
    // `render_widget` one row further would land OUTSIDE the pane — on the
    // keybar, which is the one row that must always be readable.
    if y + 1 < bottom {
        y += 1;
        section_rule(f, x, y, width, "ACTIVITY", p, inner);
        y += 1;
        let added = item.created_at.clone().unwrap_or_else(|| "—".into());
        let changed = item.modified_at.clone().unwrap_or_else(|| i18n::t(lang, &Key::EncInfoNeverModified));
        if y >= inner.y && y < bottom {
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("added ", Style::default().fg(p.faint)),
                    Span::styled(added, Style::default().fg(p.muted)),
                    Span::styled(" · changed ", Style::default().fg(p.faint)),
                    Span::styled(changed, Style::default().fg(p.muted)),
                ])),
                Rect { x, y, width, height: 1 },
            );
        }
    }
}

/// The item's icon for the detail view: a big crisp favicon (kitty/sixel) in a
/// `w×h` slot, or a big colored monogram fallback.
#[allow(clippy::too_many_arguments)]
fn draw_detail_icon(f: &mut Frame, x: u16, y: u16, w: u16, h: u16, item: &crate::model::Item, app: &App, clip: Rect) {
    let domain = if app.settings.favicons {
        crate::model::item_website(item).and_then(crate::favicon::domain_of)
    } else {
        None
    };
    if let Some(domain) = domain.as_ref() {
        // a separate protocol key from the list tile (different render size)
        let key = format!("det|{domain}");
        if draw_crisp_favicon(f, Rect { x, y, width: w, height: h }, &key, domain, app) {
            return;
        }
    }
    // big monogram: a w×h colored tile with the label's initial centred
    let color = monogram_color(&item.label);
    let fill = Style::default().bg(color);
    for dy in 0..h {
        for dx in 0..w {
            put(f, x + dx, y + dy, " ", fill, clip);
        }
    }
    let initial = item
        .label
        .chars()
        .find(|c| c.is_alphanumeric())
        .and_then(|c| c.to_uppercase().next())
        .unwrap_or('•');
    let letter = Style::default().bg(color).fg(Color::Rgb(255, 255, 255)).add_modifier(Modifier::BOLD);
    put(f, x + w / 2, y + h / 2, &initial.to_string(), letter, clip);
}

/// The item's pictures, side by side under the ledger. Only the item on show
/// is ever decoded, and only once per stored value (the memo key carries the
/// value's length, so replacing a picture rebuilds it and nothing else does).
/// Advances `*y` past the strip; draws nothing when the item has no image field
/// or the pane has no room left.
#[allow(clippy::too_many_arguments)]
fn draw_image_thumbs(
    f: &mut Frame,
    x: u16,
    y: &mut u16,
    width: u16,
    bottom: u16,
    item: &crate::model::Item,
    app: &App,
    clip: Rect,
) {
    let p = app.palette();
    let lang = app.effective_lang();
    let shots: Vec<&crate::model::ItemField> = item
        .fields
        .iter()
        .filter(|fld| crate::image_field::is_image(fld) && !fld.value.is_empty())
        .collect();
    // the rule plus at least two rows of picture, or it isn't worth the space
    if shots.is_empty() || *y + 3 >= bottom || width < 8 {
        return;
    }
    section_rule(f, x, *y, width, "IMAGES", p, clip);
    *y += 1;
    let cols = shots.len() as u16;
    let tw = (width / cols).saturating_sub(1).clamp(4, 24);
    let th = 6u16.min(bottom.saturating_sub(*y));
    for (i, field) in shots.iter().enumerate() {
        let rect = Rect { x: x + i as u16 * (tw + 1), y: *y, width: tw, height: th }.intersection(clip);
        if rect.is_empty() {
            continue;
        }
        // the value's length in the key: replacing the picture invalidates it
        let key = format!(
            "{}{}|{}|{}",
            crate::app::IMAGE_KEY_PREFIX,
            item.id,
            field.key,
            field.value.as_str().len()
        );
        draw_image_thumb(f, rect, &key, field, app, p, lang);
    }
    *y += th;
}

/// One thumbnail, in order of preference: (1) the picture rendered CRISP via
/// the terminal graphics protocol (kitty/sixel), (2) the same picture as
/// half-blocks (two pixel rows per cell), (3) a quiet box naming what is stored
/// — a value that doesn't decode still says it is there.
fn draw_image_thumb(
    f: &mut Frame,
    rect: Rect,
    key: &str,
    field: &crate::model::ItemField,
    app: &App,
    p: Palette,
    lang: Lang,
) {
    // (1) crisp: build the protocol once per key, then render it every frame
    if let Some(picker) = app.picker.as_ref() {
        if !app.image_decode_failed(key) {
            let mut protos = app.icon_protocols.borrow_mut();
            if !protos.contains_key(key) {
                match crate::image_field::decode(field) {
                    Some(img) => {
                        protos.insert(key.to_string(), picker.new_resize_protocol(img));
                    }
                    None => app.mark_image_undecodable(key),
                }
            }
            if let Some(proto) = protos.get_mut(key) {
                f.render_stateful_widget(ratatui_image::StatefulImage::new(), rect, proto);
                return;
            }
        }
    }

    // (2) half-blocks: one cell per top/bottom pixel pair of a memoized grid
    let (pw, ph) = (rect.width as u32, rect.height as u32 * 2);
    if let Some(grid) = app.image_grid(key, field, pw, ph) {
        for r in 0..rect.height {
            let spans: Vec<Span> = (0..rect.width)
                .map(|c| {
                    let top = crate::image_field::pixel(&grid, pw, c as u32, r as u32 * 2);
                    let bot = crate::image_field::pixel(&grid, pw, c as u32, r as u32 * 2 + 1);
                    halfblock_cell(top, bot)
                })
                .collect();
            f.render_widget(
                Paragraph::new(Line::from(spans)),
                Rect { x: rect.x, y: rect.y + r, width: rect.width, height: 1 },
            );
        }
        return;
    }

    // (3) nothing decodes: a hairline box with the placeholder inside it
    let border = Style::default().fg(p.border);
    for r in 0..rect.height {
        let edge = r == 0 || r + 1 == rect.height;
        let line = if edge {
            "─".repeat(rect.width as usize)
        } else {
            format!("│{}│", " ".repeat(rect.width.saturating_sub(2) as usize))
        };
        // the box is its own clip: nothing drawn for it may leave it
        puts(f, rect.x, rect.y + r, &line, border, rect);
    }
    let text = truncate(&crate::image_field::placeholder(lang, field), rect.width.saturating_sub(2) as usize);
    puts(f, rect.x + 1, rect.y + rect.height / 2, &text, Style::default().fg(p.faint), rect);
}

/// A horizontally centred line of pre-styled spans at `*y`, advancing `*y`.
fn centered_line(f: &mut Frame, x: u16, width: u16, y: &mut u16, bottom: u16, line: Line<'static>) {
    if *y < bottom {
        let rect = Rect { x, y: *y, width, height: 1 };
        f.render_widget(Paragraph::new(line).alignment(Alignment::Center), rect);
    }
    *y += 1;
}

// ---- the Memorandum (a chat with yourself, on the vault body) -------------

/// Columns the time column costs: `HH:MM` plus two of gutter. Every row of a
/// message lines up under it, so continuation lines and captions read as one
/// paragraph instead of a ragged left edge.
const MEMO_GUTTER: u16 = 7;
/// Rows one photo occupies in the thread. Fixed, so scrolling is arithmetic on
/// rows and never has to decode a picture to know how tall the thread is.
const MEMO_PHOTO_H: usize = 6;
/// Columns a photo occupies — roughly square once the ~1:2 cell aspect is
/// accounted for (six rows ≈ twelve pixel rows).
const MEMO_PHOTO_W: u16 = 14;
/// Rows the composer may grow to before it starts scrolling inside itself. It
/// eats the thread as it grows, and the thread is the reason the view exists —
/// five lines is a paragraph, which is as much as a note to yourself needs to
/// see at once.
const MEMO_COMPOSER_MAX_ROWS: u16 = 5;

/// One laid-out run of thread rows, tagged with the message it belongs to
/// (`None` for a day separator, which belongs to none of them). The thread is
/// measured in ROWS, not messages: a wrapped message and a photo are each
/// several, and the scroll offset counts what the eye counts. The id is what a
/// search jump aims at — rows depend on the pane width, so only this pass
/// knows where a message actually sits.
struct MemoBlock {
    id: Option<String>,
    kind: MemoKind,
}

enum MemoKind {
    /// A day separator (`section_rule`), its date already localized.
    Day(String),
    /// One text row, styled.
    Row(Line<'static>),
    /// A picture: its render-cache key, and WHICH message carries it. The
    /// photo is turned into an image field (so the item-picture machinery draws
    /// it with no second implementation) in the draw arm and only for the rows
    /// that are actually on screen — building one copies the whole base64 and
    /// wipes it again on drop, which off-screen rows must not pay for on every
    /// frame of a 120 ms loop.
    Photo(String, usize),
}

impl MemoBlock {
    fn height(&self) -> usize {
        match self.kind {
            MemoKind::Photo(..) => MEMO_PHOTO_H,
            _ => 1,
        }
    }
}

/// Greedy word wrap to `w` columns. A word no line can hold is CUT across as
/// many as it needs rather than overflowing the pane; `w == 0` yields nothing
/// (there is no column to put a character in).
fn wrap_words(s: &str, w: usize) -> Vec<String> {
    if w == 0 {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    let mut line = String::new();
    let mut len = 0usize;
    for word in s.split_whitespace() {
        let wl = word.chars().count();
        if wl > w {
            if len > 0 {
                out.push(std::mem::take(&mut line));
            }
            let mut chunk = String::new();
            for ch in word.chars() {
                chunk.push(ch);
                if chunk.chars().count() == w {
                    out.push(std::mem::take(&mut chunk));
                }
            }
            len = chunk.chars().count();
            line = chunk;
            continue;
        }
        // a word costs its own width, plus the space before it when it isn't first
        let extra = if len == 0 { wl } else { wl + 1 };
        if len + extra > w {
            out.push(std::mem::take(&mut line));
            line.push_str(word);
            len = wl;
        } else {
            if len > 0 {
                line.push(' ');
            }
            line.push_str(word);
            len += extra;
        }
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

/// [`wrap_words`] over a text that may hold NEWLINES: each of them starts a row
/// of its own, and each of those rows is then wrapped like any other. A line the
/// user left blank stays one blank row — it is the paragraph break they typed,
/// and a chat that swallowed it would render their message as something else.
///
/// A text with nothing in it is still NO rows (a photo message carries an empty
/// one, and it must not grow a blank line under the picture).
fn wrap_lines(s: &str, w: usize) -> Vec<String> {
    if w == 0 || s.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    for line in s.split('\n') {
        let wrapped = wrap_words(line, w);
        if wrapped.is_empty() {
            out.push(String::new());
        } else {
            out.extend(wrapped);
        }
    }
    out
}

/// One thread row: the time column (filled only on a message's FIRST row) and
/// its text.
fn memo_row(id: &str, time: &str, text: &str, style: Style, p: Palette) -> MemoBlock {
    MemoBlock {
        id: Some(id.to_string()),
        kind: MemoKind::Row(Line::from(vec![
            Span::styled(format!("{time:<5}  "), Style::default().fg(p.faint)),
            Span::styled(text.to_string(), style),
        ])),
    }
}

/// The whole thread laid out, oldest first, for a `width`-wide pane, WITH the
/// messages it was laid out from (a photo block names its message by index, and
/// the draw arm needs that message back). Pure: the scroll offset is applied by
/// the caller, so measuring costs one pass and the window is arithmetic.
fn memo_blocks(app: &App, width: u16) -> (Vec<crate::memo::MemoMessage>, Vec<MemoBlock>) {
    let p = app.palette();
    let lang = app.effective_lang();
    let Some(vault) = app.vault.as_ref() else { return (Vec::new(), Vec::new()) };
    let text_w = width.saturating_sub(MEMO_GUTTER) as usize;
    let messages = vault.content.memo_messages();
    // the stamps are UTC on disk (`docs/STORAGE-STANDARD.md`); the day rule and
    // the clock column are what the USER's day and clock say, or a note written
    // at 00:30 files itself under yesterday at "22:30".
    let offset = crate::model::local_utc_offset();
    let mut blocks = Vec::new();
    let mut day = String::new();
    for (i, msg) in messages.iter().enumerate() {
        // a stamp this build didn't write is shown as it came, never a panic
        // (`get` on a byte range answers None on a boundary it cannot cut).
        let (this_day, time) = crate::model::wall_clock(&msg.ts, offset).unwrap_or_else(|| {
            (
                msg.ts.get(..10).unwrap_or_default().to_string(),
                msg.ts.get(11..16).unwrap_or_default().to_string(),
            )
        });
        if this_day != day {
            blocks.push(MemoBlock {
                id: None,
                kind: MemoKind::Day(i18n::t(lang, &Key::MemoDay(&this_day))),
            });
            day = this_day;
        }
        // a photo announces what it is (the placeholder — the base64 never
        // reaches the screen), then the picture, then the words under it
        if let Some(img) = msg.image.as_ref() {
            // the metadata field, NOT the picture: the head row only names it
            let head = crate::image_field::placeholder(lang, &img.as_meta_field());
            blocks.push(memo_row(&msg.id, &time, &head, Style::default().fg(p.muted), p));
            // the stored length in the key: a message is immutable, but a
            // reader that rewrites one must not keep the old decode
            let key = format!(
                "{}{}|{}",
                crate::app::MEMO_IMAGE_KEY_PREFIX,
                msg.id,
                img.b64.len()
            );
            blocks.push(MemoBlock { id: Some(msg.id.clone()), kind: MemoKind::Photo(key, i) });
            let caption = img.caption.as_deref().unwrap_or_default();
            let style = Style::default().fg(p.muted).add_modifier(Modifier::ITALIC);
            for line in wrap_lines(caption, text_w) {
                blocks.push(memo_row(&msg.id, "", &line, style, p));
            }
        }
        // a message is as many rows as it has lines, each of them wrapped: the
        // composer writes newlines (Shift/Alt+⏎), so a message is a paragraph
        // and not a sentence, and the thread is measured accordingly.
        for (row, line) in wrap_lines(&msg.text, text_w).into_iter().enumerate() {
            // the time rides the first row of the message only
            let stamp = if row == 0 && msg.image.is_none() { time.as_str() } else { "" };
            blocks.push(memo_row(&msg.id, stamp, &line, Style::default().fg(p.text), p));
        }
    }
    (messages, blocks)
}

/// The MEMORANDUM: the vault body becomes a chat with yourself — a title rule,
/// the thread oldest-to-newest under it, and a composer pinned to the last row.
/// It REPLACES the ledger/detail split (and the chips): a different room, not a
/// panel, which is why its composer owns every key.
///
/// The thread is pinned to its newest row; `app.memo_scroll` is how far BACK of
/// that the viewport sits, clamped here to what there is to scroll — so an
/// over-scroll is simply the top, and a shrinking terminal can never strand the
/// view past the end. A search jump overrides both: `app.memo_jump` names a
/// MESSAGE, and the viewport is placed around wherever that message turned out
/// to sit at this width.
fn draw_memo(f: &mut Frame, area: Rect, app: &App) {
    let p = app.palette();
    let lang = app.effective_lang();
    if area.width < 12 || area.height < 2 {
        return;
    }
    let x = area.x + 2;
    let width = area.width.saturating_sub(4);
    section_rule(f, x, area.y, width, &i18n::t(lang, &Key::MemoTitle), p, area);

    // the bottom rows are the composer's — or the search bar's, which REPLACES
    // it rather than floating over the thread: the chat is what you are
    // searching, so it stays on screen. The composer GROWS with what is being
    // written (the bar never does — a query is one line), and what it takes
    // comes off the thread, never off the title rule.
    let input_h = match app.memo_search.is_some() {
        true => 1,
        false => memo_composer_rows(&app.memo_input),
    }
    .clamp(1, area.height - 1);
    let input_y = area.y + area.height - input_h;
    match app.memo_search.as_ref() {
        Some(search) => draw_memo_search_bar(f, x, input_y, width, app, search, area),
        None => draw_memo_composer(f, x, input_y, width, input_h, app, area),
    }
    let view_y = area.y + 1;
    let mut view_h = input_y.saturating_sub(view_y);
    if view_h == 0 {
        return;
    }
    // …and the results sit in a strip just above it, taking the OLDEST rows of
    // the thread.
    if let Some(search) = app.memo_search.as_ref() {
        // half the thread at most: the conversation is what the hits point AT,
        // and a strip that swallows it hides the very thing it found. Under two
        // rows there is no room for the rule and a result, so there is no strip.
        let strip_h = memo_strip_height(search).min(view_h / 2);
        if strip_h >= 2 {
            let strip = Rect { x, y: input_y - strip_h, width, height: strip_h };
            draw_memo_hits(f, strip, app, search);
            view_h -= strip_h;
        }
    }
    if view_h == 0 {
        return;
    }

    let (messages, blocks) = memo_blocks(app, width);
    if blocks.is_empty() {
        let hint = truncate(&i18n::t(lang, &Key::MemoEmpty), width as usize);
        let style = Style::default().fg(p.faint).add_modifier(Modifier::ITALIC);
        puts(f, x, view_y, &hint, style, area);
        return;
    }

    // the message the thread is FOCUSED on: the one the copy-band is on (Up/Down
    // picked it, Ctrl+Y copies it) or, failing that, where a search jump landed.
    // Either way it is banded and kept on screen — the same window arithmetic. A
    // selection index is turned into an ID here, so it survives the width-driven
    // re-layout the way a jump's ID does.
    let focus = app
        .memo_sel
        .and_then(|i| messages.get(i))
        .map(|m| m.id.clone())
        .or_else(|| app.memo_jump.clone());

    let view = Rect { x, y: view_y, width, height: view_h };
    let total: usize = blocks.iter().map(MemoBlock::height).sum();
    let top = memo_top(app, &blocks, total, view_h as usize, focus.as_deref());
    let mut row = 0usize; // this block's first row, in thread space
    for block in &blocks {
        let end = row + block.height();
        // does [row, end) meet the viewport [top, top + view_h)?
        if end <= top || row >= top + view_h as usize {
            row = end;
            continue;
        }
        let skip = top.saturating_sub(row); // rows of this block above the view
        let y = view_y + row.saturating_sub(top) as u16;
        let avail = (view_y + view_h).saturating_sub(y);
        let h = ((block.height() - skip) as u16).min(avail);
        // the focused message wears the selection band, for as long as the
        // focus stands — a search's answer, findable by eye the moment the bar
        // goes away, or the message a copy is about to lift.
        let banded = focus.is_some() && block.id.is_some() && block.id.as_deref() == focus.as_deref();
        if banded {
            for by in y..y + h.max(1) {
                puts(f, x, by, &" ".repeat(width as usize), Style::default().bg(p.selection), view);
            }
        }
        match &block.kind {
            MemoKind::Day(label) => section_rule(f, x, y, width, label, p, view),
            MemoKind::Row(line) => {
                let mut para = Paragraph::new(line.clone());
                if banded {
                    para = para.style(Style::default().bg(p.selection));
                }
                f.render_widget(para, Rect { x, y, width, height: 1 });
            }
            MemoKind::Photo(key, at) => {
                // a half-scrolled photo simply draws into the rows it still
                // has — the picture is scaled to its rect, never clipped by it
                let pw = MEMO_PHOTO_W.min(width.saturating_sub(MEMO_GUTTER));
                let rect = Rect { x: x + MEMO_GUTTER, y, width: pw, height: h }.intersection(view);
                // the picture is copied out of the message HERE, and only when
                // something really has to decode it: a field carries the whole
                // base64 and wipes it again on drop, which is a frame's entire
                // budget for a thread of photos — off-screen ones must not pay
                // it at all, and drawn ones only until the caches hold them.
                let photo = messages.get(*at).and_then(|m| m.image.as_ref());
                if let (false, Some(img)) = (rect.is_empty(), photo) {
                    let field =
                        match app.image_thumb_cached(key, rect.width as u32, rect.height as u32 * 2)
                        {
                            true => img.as_meta_field(),
                            false => img.as_field(),
                        };
                    draw_image_thumb(f, rect, key, &field, app, p, lang);
                }
            }
        }
        row = end;
    }
}

/// The first visible thread row. Normally the newest row minus `memo_scroll`;
/// with a `focus` message standing — the copy-selection or a search jump — the
/// window that keeps THAT message on screen (the ledger's own [`scroll_start`],
/// so it lands with context around it rather than glued to an edge). A focus
/// whose message is gone falls back to the ordinary pinned-to-newest view.
///
/// A message is several rows (its lines, its photo, its caption), so the focus
/// aims at the WHOLE of it: the window is pulled up as far as it takes for the
/// last of those rows to fit, and no further than the first — a message taller
/// than the viewport starts at its own top rather than showing only its tail.
fn memo_top(app: &App, blocks: &[MemoBlock], total: usize, view_h: usize, focus: Option<&str>) -> usize {
    let max_back = total.saturating_sub(view_h);
    // geometry is this side's: hand the scroll keys the only honest bound on how
    // far back this thread goes, so the offset they accumulate cannot run past
    // its top (see `App::scroll_memo_back`).
    app.memo_max_scroll.set(max_back);
    if let Some(target) = focus {
        let mut row = 0usize;
        let mut span: Option<(usize, usize)> = None; // [first row, last row + 1)
        for block in blocks {
            let end = row + block.height();
            if block.id.as_deref() == Some(target) {
                span = Some((span.map_or(row, |(start, _)| start), end));
            }
            row = end;
        }
        if let Some((start, end)) = span {
            let mut top = scroll_start(start, total, view_h);
            if end > top + view_h {
                top = end.saturating_sub(view_h).min(start);
            }
            return top;
        }
    }
    max_back.saturating_sub(app.memo_scroll.min(max_back))
}

// ---- searching the thread (Ctrl+F) ----------------------------------------

/// The search prompt's glyph, and the two mode chips'. `aa` is the editor's own
/// mark for "these letters, literally"; `◈` is already the app's vocabulary for
/// "something is being derived" (the sync chip uses it). The two mode glyphs are
/// the ITEM list's as well — one search, two boxes (see [`mode_chip`]).
const MEMO_SEARCH_GLYPH: &str = "⌕";
const LITERAL_GLYPH: &str = "aa";
const RAG_GLYPH: &str = "◈";

/// The mode chip's text — glyph plus the localized word. Callers hand it the
/// mode that RAN wherever one has (`chip_mode`), so a RAG search that fell back
/// to the literal one advertises "literal" and not what was asked for.
fn mode_chip(mode: crate::app::SearchMode, lang: Lang) -> String {
    let (glyph, word) = match mode {
        crate::app::SearchMode::Literal => (LITERAL_GLYPH, Key::SearchModeLiteral),
        crate::app::SearchMode::Rag => (RAG_GLYPH, Key::SearchModeRag),
    };
    format!(" {glyph} {} ", i18n::t(lang, &word))
}

/// Rows the results strip WANTS: the rule plus a row per hit, or the single row
/// that says "searching…" / "no results". Nothing at all before the first run —
/// an empty bar has no results to be empty about.
fn memo_strip_height(search: &crate::app::MemoSearch) -> u16 {
    if search.searching() {
        return 2;
    }
    match search.ran {
        None => 0,
        Some(_) => 1 + search.hits.len().max(1) as u16,
    }
}

/// The results strip, directly over the bar: `▌ HH:MM  snippet … score`, the
/// highlighted row banded like any other list. An INLINE strip and not a
/// floating modal on purpose — what is being searched is the conversation right
/// behind it, and a box over the chat would hide the thing the hits point at.
fn draw_memo_hits(f: &mut Frame, area: Rect, app: &App, search: &crate::app::MemoSearch) {
    let p = app.palette();
    let lang = app.effective_lang();
    let width = area.width;
    section_rule(f, area.x, area.y, width, &i18n::t(lang, &Key::MemoSearchResults), p, area);
    let rows = area.height.saturating_sub(1) as usize;
    if rows == 0 {
        return;
    }
    let faint = Style::default().fg(p.faint).add_modifier(Modifier::ITALIC);
    if search.searching() {
        let line = truncate(&i18n::t(lang, &Key::SearchWorking), width as usize);
        puts(f, area.x + 2, area.y + 1, &line, faint, area);
        return;
    }
    if search.hits.is_empty() {
        let line = truncate(&i18n::t(lang, &Key::MemoSearchNoResults), width as usize);
        puts(f, area.x + 2, area.y + 1, &line, faint, area);
        return;
    }
    let first = scroll_start(search.selected, search.hits.len(), rows);
    for (i, hit) in search.hits.iter().enumerate().skip(first).take(rows) {
        let y = area.y + 1 + (i - first) as u16;
        let selected = i == search.selected;
        let (marker, mut mstyle, tstyle) = row_marker(selected, true, p);
        if selected {
            puts(f, area.x, y, &" ".repeat(width as usize), Style::default().bg(p.selection), area);
            mstyle = mstyle.bg(p.selection);
        }
        puts(f, area.x, y, marker, mstyle, area);
        let band = |s: Style| if selected { s.bg(p.selection) } else { s };
        // a score belongs to a SEMANTIC hit only: a literal hit's number ranks
        // the list and would mean nothing on screen (see `MemoHit::score`).
        let score = hit.score.map(|s| format!("{s:.2}")).unwrap_or_default();
        let text_x = area.x + 2;
        let avail = width.saturating_sub(2) as usize;
        let stamp = format!("{:<5}  ", hit.time);
        let room = avail.saturating_sub(stamp.chars().count() + score.chars().count() + 1);
        let snippet = truncate(&hit.snippet, room);
        let pad = room.saturating_sub(snippet.chars().count()) + 1;
        let spans = vec![
            Span::styled(stamp, band(Style::default().fg(p.faint))),
            Span::styled(snippet, band(tstyle)),
            Span::styled(" ".repeat(pad), band(Style::default())),
            Span::styled(score, band(Style::default().fg(p.steel))),
        ];
        let mut para = Paragraph::new(Line::from(spans));
        if selected {
            para = para.style(Style::default().bg(p.selection));
        }
        f.render_widget(para, Rect { x: text_x, y, width: avail as u16, height: 1 });
    }
}

/// The search bar, on the row the composer had: `⌕ query▌` with the mode chip
/// pinned right. The composer's own line is untouched underneath — Esc gives it
/// back exactly as it was.
fn draw_memo_search_bar(
    f: &mut Frame,
    x: u16,
    y: u16,
    width: u16,
    app: &App,
    search: &crate::app::MemoSearch,
    clip: Rect,
) {
    let p = app.palette();
    let lang = app.effective_lang();
    if y < clip.y || y >= clip.y + clip.height {
        return;
    }
    let chip = mode_chip(search.chip_mode(), lang);
    let chip_w = chip.chars().count() as u16;
    // the chip is the whole point of the bar; it is dropped only when the row
    // is too narrow to hold it AND a character of query at the same time.
    let show_chip = width > chip_w + 6;
    let field_w = if show_chip { width.saturating_sub(chip_w + 1) } else { width };
    // a STEADY block, never blinking: the memo view is otherwise idle, and a
    // caret that flips on and off under a mouse selection makes the terminal
    // drop it (it clears the moment a covered cell changes). It still SHOWS —
    // this is where a query goes — it only stops toggling.
    let cursor = "▌";
    let room = (field_w as usize).saturating_sub(3);
    // the caret splits the query so it can sit mid-word; the window follows it
    // when the query outgrows the bar.
    let (before, after) = search.query.view(room);
    let mut spans = vec![Span::styled(format!("{MEMO_SEARCH_GLYPH} "), accent_bold(p))];
    if search.query.is_empty() {
        spans.push(Span::styled(cursor.to_string(), Style::default().fg(p.accent_hi)));
        spans.push(Span::styled(
            truncate(&i18n::t(lang, &Key::MemoSearchHint), room),
            Style::default().fg(p.faint).add_modifier(Modifier::ITALIC),
        ));
    } else {
        spans.push(Span::styled(before, Style::default().fg(p.text)));
        spans.push(Span::styled(cursor.to_string(), Style::default().fg(p.accent_hi)));
        spans.push(Span::styled(after, Style::default().fg(p.text)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), Rect { x, y, width: field_w, height: 1 });
    if show_chip {
        let style = Style::default().bg(p.chip_bg).fg(p.chip_fg).add_modifier(Modifier::BOLD);
        puts(f, x + width - chip_w, y, &chip, style, clip);
    }
}

/// Rows the composer WANTS: one per line of what has been written into it (the
/// buffer is `\n`-separated — see [`crate::app::App::memo_input`]), never fewer
/// than one and never more than [`MEMO_COMPOSER_MAX_ROWS`]. Past the cap the
/// composer scrolls inside itself instead of eating the thread.
fn memo_composer_rows(input: &str) -> u16 {
    let lines = input.split('\n').count().max(1);
    (lines as u16).min(MEMO_COMPOSER_MAX_ROWS)
}

/// The composer, pinned to the body's last rows: `› text▌`, one row per line
/// that was written into it. It is ALWAYS focused in this view — there is
/// nowhere else in here for a key to go — so it carries the cursor
/// unconditionally, on the last line, which is the one being typed.
///
/// Two truncations, and they say different things. The line under the cursor
/// shows its TAIL (what is being typed has to stay visible); the finished lines
/// above it show their head, cut with an ellipsis where the row ran out. A
/// buffer with more lines than `height` shows its LAST `height` of them, so the
/// cursor is on screen however long the note grows.
fn draw_memo_composer(
    f: &mut Frame,
    x: u16,
    y: u16,
    width: u16,
    height: u16,
    app: &App,
    clip: Rect,
) {
    let p = app.palette();
    let lang = app.effective_lang();
    // a STEADY block, never blinking: the composer is always focused, so the
    // caret is always shown — but a cell that toggled every ~½ s under a mouse
    // selection would make the terminal drop it mid-drag (kitty clears a
    // selection the instant a covered cell changes). Steady, the idle memo view
    // is byte-identical frame to frame and a native selection survives.
    let cursor = "▌";
    let room = (width as usize).saturating_sub(3);
    let lines: Vec<&str> = app.memo_input.split('\n').collect();
    // where the caret lives: its linear char index maps to a LINE and a COLUMN,
    // so ←/→ carry the cursor to any line, not only the one being appended to.
    let caret = app.memo_input.caret();
    let head: String = app.memo_input.chars().take(caret).collect();
    let caret_line = head.matches('\n').count();
    let caret_col = head.rsplit('\n').next().map(|s| s.chars().count()).unwrap_or(0);
    // last `height` lines, but never scrolled past the caret's own line
    let first = lines.len().saturating_sub(height as usize).min(caret_line);
    for (row, line) in lines[first..].iter().enumerate() {
        let li = first + row;
        let ry = y + row as u16;
        if ry < clip.y || ry >= clip.y + clip.height {
            continue;
        }
        // the prompt marks where the composer starts; its continuation rows are
        // indented under it, so a multi-line message reads as one paragraph.
        let marker = if row == 0 { "› " } else { "  " };
        let mut spans = vec![Span::styled(marker, accent_bold(p))];
        if app.memo_input.is_empty() {
            spans.push(Span::styled(cursor.to_string(), Style::default().fg(p.accent_hi)));
            spans.push(Span::styled(
                i18n::t(lang, &Key::MemoComposerHint),
                Style::default().fg(p.faint).add_modifier(Modifier::ITALIC),
            ));
        } else if li == caret_line {
            // the caret's line: split at its column so the cursor sits between
            // the chars it is on, the window scrolling to keep it in view
            let (before, after) = crate::textinput::window_line(line, caret_col, room);
            spans.push(Span::styled(before, Style::default().fg(p.text)));
            spans.push(Span::styled(cursor.to_string(), Style::default().fg(p.accent_hi)));
            spans.push(Span::styled(after, Style::default().fg(p.text)));
        } else {
            // a finished line above/below the caret: its head, cut with an
            // ellipsis where the row ran out — no cursor lives here.
            spans.push(Span::styled(truncate(line, room), Style::default().fg(p.text)));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), Rect { x, y: ry, width, height: 1 });
    }
}

/// The canonical name of the settings command (honours config renames).
fn settings_cmd_name(app: &App) -> String {
    app.registry
        .command_for_action(app.page, Action::Settings)
        .map(|c| c.name.clone())
        .unwrap_or_else(|| "settings".to_string())
}

/// The tabbed settings page: a section bar, then the rows of the active
/// section as `label  ◂ value ▸` (read-only rows show a plain value). Text is
/// localized to the *draft* language, so cycling the language previews live.
fn draw_settings(
    f: &mut Frame,
    area: Rect,
    app: &App,
    state: &SettingsState,
    focused: bool,
    interactive: bool,
) {
    let p = app.palette();
    let lang = state.draft.lang;
    let _ = (focused, interactive);
    // a floating modal (fig-09): the SETTINGS title sits on the border and ONE
    // scrolling list runs under section rules — no tabs.
    let w = 52u16.min(area.width.saturating_sub(6)).max(30);
    // Size to the content when the terminal allows it (section rules + gaps +
    // rows + footer + border), so nothing scrolls needlessly; on short
    // terminals it clamps and the list scrolls to keep the cursor on-screen.
    let sections = {
        let (mut n, mut last) = (0u16, None);
        for (s, _) in settings_ui::ALL_ROWS.iter() {
            if last != Some(*s) {
                n += 1;
                last = Some(*s);
            }
        }
        n
    };
    let needed = settings_ui::ALL_ROWS.len() as u16 + sections + sections.saturating_sub(1) + 3;
    let h = needed.min(area.height.saturating_sub(2)).max(8);
    let mx = area.x + area.width.saturating_sub(w) / 2;
    let my = area.y + area.height.saturating_sub(h) / 2;
    let title = settings_cmd_name(app).to_uppercase();
    let inner = draw_modal_frame(f, Rect { x: mx, y: my, width: w, height: h }, &title, p);
    if inner.width < 12 || inner.height < 6 {
        return;
    }
    let bottom = inner.y + inner.height;
    let cx = inner.x + 1;
    let cw = inner.width.saturating_sub(2);
    let mut y = inner.y;

    let labels: Vec<String> =
        settings_ui::ALL_ROWS.iter().map(|(_, r)| r.label(lang)).collect();
    let label_w = labels.iter().map(|s| s.chars().count()).max().unwrap_or(8).clamp(8, 16);

    // Flatten to a display list (section rules + gaps interleaved with rows),
    // then window it so the selected row is always on-screen — the modal is
    // often shorter than the full list, so a fixed top would clip the cursor.
    enum Disp {
        Gap,
        Header(settings_ui::Section),
        Field(usize),
    }
    let mut lines: Vec<Disp> = Vec::new();
    let mut last_section: Option<settings_ui::Section> = None;
    for (i, (section, _)) in settings_ui::ALL_ROWS.iter().enumerate() {
        if last_section != Some(*section) {
            if last_section.is_some() {
                lines.push(Disp::Gap);
            }
            lines.push(Disp::Header(*section));
            last_section = Some(*section);
        }
        lines.push(Disp::Field(i));
    }
    let sel_line =
        lines.iter().position(|d| matches!(d, Disp::Field(i) if *i == state.selected)).unwrap_or(0);
    let cap = (bottom.saturating_sub(1)).saturating_sub(inner.y) as usize; // reserve the footer row
    let first = scroll_start(sel_line, lines.len(), cap);

    for disp in lines.iter().skip(first).take(cap) {
        match disp {
            Disp::Gap => y += 1,
            Disp::Header(section) => {
                section_rule(f, cx, y, cw, &section.title(lang), p, inner);
                y += 1;
            }
            Disp::Field(i) => {
                let i = *i;
                let (_, row) = &settings_ui::ALL_ROWS[i];
                let current = i == state.selected;
                if current {
                    puts(f, cx, y, &" ".repeat(cw as usize), Style::default().bg(p.selection), inner);
                }
                let with_bg = |s: Style| if current { s.bg(p.selection) } else { s };
                let label = &labels[i];
                let pad = " ".repeat(label_w.saturating_sub(label.chars().count()) + 2);
                let label_style = if current {
                    Style::default().fg(p.accent)
                } else {
                    Style::default().fg(p.muted)
                };
                let value = settings_value(app, state, *row, lang);
                let mut spans = vec![Span::styled(format!("{label}{pad}"), with_bg(label_style))];
                if row.read_only() {
                    spans.push(Span::styled(value, with_bg(Style::default().fg(p.muted))));
                } else if row.is_action() {
                    // an action row: one arrow, the verb in accent — it DOES
                    // something, it does not hold a value to cycle
                    let arrow = if current { p.accent } else { p.faint };
                    spans.push(Span::styled(value, with_bg(Style::default().fg(p.accent))));
                    spans.push(Span::styled(" \u{25b8}", with_bg(Style::default().fg(arrow))));
                } else {
                    let arrow = if current { p.accent } else { p.faint };
                    spans.push(Span::styled("\u{25c2} ", with_bg(Style::default().fg(arrow))));
                    spans.push(Span::styled(value, with_bg(Style::default().fg(p.steel))));
                    spans.push(Span::styled(" \u{25b8}", with_bg(Style::default().fg(arrow))));
                }
                let mut para = Paragraph::new(Line::from(spans));
                if current {
                    para = para.style(Style::default().bg(p.selection));
                }
                f.render_widget(para, Rect { x: cx, y, width: cw, height: 1 });
                y += 1;
            }
        }
    }

    // footer: the interaction contract (fig-09)
    let footer = match lang {
        Lang::En => "\u{2191}\u{2193} \u{b7} \u{25c2} \u{25b8} change \u{2014} \u{23ce} save \u{b7} Esc cancel",
        Lang::It => "\u{2191}\u{2193} \u{b7} \u{25c2} \u{25b8} cambia \u{2014} \u{23ce} salva \u{b7} Esc annulla",
    };
    puts(f, cx, bottom.saturating_sub(1), footer, Style::default().fg(p.faint), inner);
}

/// The localized, display-ready value of a settings row (read from the draft).
fn settings_value(app: &App, state: &SettingsState, row: Row, lang: Lang) -> String {
    let d = &state.draft;
    match row {
        Row::Language => d.lang.name().to_string(),
        Row::VaultCount => app.vaults.len().to_string(),
        Row::Theme => d.theme.name().to_string(),
        Row::Background => i18n::t(
            lang,
            if d.background == Background::Opaque {
                &Key::BackgroundOpaque
            } else {
                &Key::BackgroundTransparent
            },
        ),
        Row::Blur => i18n::t(
            lang,
            if d.blur == Blur::On {
                &Key::BlurKitty
            } else {
                &Key::BlurOff
            },
        ),
        Row::Backdrop => match d.backdrop {
            crate::settings::Backdrop::Auto => "auto".to_string(),
            crate::settings::Backdrop::On => i18n::t(lang, &Key::ToggleOn),
            crate::settings::Backdrop::Off => i18n::t(lang, &Key::ToggleOff),
        },
        Row::BackdropFx => match d.backdrop_fx {
            crate::settings::BackdropFx::Rain => "rain".to_string(),
            crate::settings::BackdropFx::Waves => "waves".to_string(),
        },
        Row::BackdropLevel => d.backdrop_level.bar().to_string(),
        Row::LockDelay => d.autolock.label().to_string(),
        Row::ShowLogo => toggle_label(lang, d.show_logo),
        Row::Favicons => toggle_label(lang, d.favicons),
        Row::ReduceMotion => toggle_label(lang, d.reduce_motion),
        Row::ConfirmExit => toggle_label(lang, d.confirm_exit),
        Row::RagMemo => toggle_label(lang, d.rag_memo),
        Row::RagItems => toggle_label(lang, d.rag_items),
        // an exposure row reads "off" while its index is off, whatever the flag
        // underneath says — that is exactly what the server will do with it.
        Row::ApiExposeMemo => toggle_label(lang, d.exposes_memo()),
        Row::ApiExposeItems => toggle_label(lang, d.exposes_items()),
        Row::ApiPort => d.api_port.to_string(),
        Row::ApiVaultUnlock => toggle_label(lang, d.api_vault_unlock),
        Row::ApiReveal => toggle_label(lang, d.api_reveal),
        // the expiry reads "off" while `/unlock` is off, whatever the field
        // underneath says — exactly what the server will make of it (see
        // `Settings::api_session_secs`), and the exposure rows' own grammar.
        Row::ApiSessionTtl => match d.api_vault_unlock {
            true => d.api_session_ttl.label().to_string(),
            false => i18n::t(lang, &Key::ToggleOff),
        },
        // the live session's token, first characters only. The whole one leaves
        // through `ragtoken` (the clipboard, timed wipe included) and nowhere
        // else — a token on screen is a token in a screenshot.
        Row::ApiToken => app
            .api
            .as_ref()
            .map(|s| s.token_hint())
            .unwrap_or_else(|| "\u{2014}".to_string()),
        Row::LoginAssist => toggle_label(lang, d.login_assist),
        // detected once, when the page opened (see `App::open_settings`): a
        // `xdg-settings` spawn per frame would be a steady drip of processes
        Row::BrowserDetected => match app.detected_browser.as_ref() {
            Some(b) => format!("{} \u{b7} {}", b.name, b.family.label()),
            None => i18n::t(lang, &Key::BrowserNotDetected),
        },
        Row::InstallExtension => i18n::t(lang, &Key::SettingValueInstallExtension),
        // the head of the pairing token, like the API token row: the whole one
        // leaves through `logintoken` and the clipboard only
        Row::LoginToken => match d.login_token() {
            Some(token) => format!(
                "{}\u{2026} \u{b7} {}",
                token.chars().take(8).collect::<String>(),
                i18n::t(lang, &Key::SettingValueCopy)
            ),
            None => "\u{2014}".to_string(),
        },
        // the character this verb really answers to right now — read through
        // the same resolution the keybar draws from.
        Row::ShortcutKey(verb) => settings_ui::shortcut_value(verb, d, lang),
    }
}

fn toggle_label(lang: Lang, on: bool) -> String {
    i18n::t(lang, if on { &Key::ToggleOn } else { &Key::ToggleOff })
}

/// Width (in cells) of the model-download bar: `BackdropLevel::bar`'s glyphs
/// (█ for what is on disk, ░ for what is not) at the length of the
/// re-encryption bar above, so the two read as the same instrument.
const RAG_BAR_W: usize = 10;

/// The bottom-left element while the one-time model download runs:
/// `⇣ RAG model ████░░░░░░ 43% · 200/464 MiB`.
///
/// The fill is DATA, not a clock — bytes measured off the cache directory
/// (`rag::dir_bytes`) — so reduce-motion leaves it alone: it is a measurement
/// that happens to change, not an animation. There is deliberately no spinner
/// and no travelling block for the moment before the first sample lands
/// either: that state simply shows the empty track and an ellipsis instead of
/// a percentage, which reduce-motion has nothing to freeze and which never
/// claims the 0 % that would read as a stall.
///
/// It sheds parts as the row narrows — the MiB counter first, then the label,
/// then the glyph — and the bar with its percentage is the last thing left;
/// below that the `Paragraph` clips like every other line here.
fn rag_download_line(app: &App, width: u16) -> Line<'static> {
    let p = app.palette();
    let lang = app.effective_lang();
    let bytes = app.rag.download_bytes();
    let pct = bytes.map(crate::rag::download_pct);
    let filled = pct.map_or(0, |v| v as usize * RAG_BAR_W / 100);
    let glyph = Span::styled("\u{21e3} ", Style::default().fg(p.accent)); // ⇣
    let label = Span::styled(
        format!("{} ", i18n::t(lang, &Key::RagModelDownloadLabel)),
        Style::default().fg(p.muted),
    );
    let done = Span::styled("█".repeat(filled), Style::default().fg(p.accent));
    let track = Span::styled("░".repeat(RAG_BAR_W - filled), Style::default().fg(p.faint));
    let percent = Span::styled(
        match pct {
            Some(v) => format!(" {v}%"),
            None => " \u{2026}".to_string(), // …
        },
        Style::default().fg(p.accent),
    );
    let counter = bytes.map(|b| {
        Span::styled(
            format!(
                " · {}/{} MiB",
                crate::rag::mib(b),
                crate::rag::mib(crate::rag::EXPECTED_MODEL_BYTES)
            ),
            Style::default().fg(p.faint),
        )
    });
    let row = |with_glyph: bool, with_label: bool, with_counter: bool| {
        let mut spans = vec![Span::raw(" ")];
        if with_glyph {
            spans.push(glyph.clone());
        }
        if with_label {
            spans.push(label.clone());
        }
        spans.push(done.clone());
        spans.push(track.clone());
        spans.push(percent.clone());
        if with_counter {
            spans.extend(counter.clone());
        }
        spans
    };
    // widest first; the last one is also the fallback, for a row too narrow
    // even for that (the `Paragraph` clips it — 1x1 included).
    let ladder = [
        row(true, true, true),
        row(true, true, false),
        row(true, false, false),
        row(false, false, false),
    ];
    Line::from(
        ladder
            .into_iter()
            .find(|spans| spans_w(spans) <= width as usize)
            .unwrap_or_else(|| row(false, false, false)),
    )
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let p = app.palette();
    // The one-time model download holds this row while it runs — but it may
    // not hide a toast for the whole multi-minute fetch: `App::note` has no
    // timer (only the next key clears it), so during a download the row is
    // SPLIT — the bar in its compact form pinned left, the message in the
    // remainder, right-clipped. Copy confirmations, sync results and errors
    // stay visible while the percentage keeps moving; the keybar's keys are
    // still one `?` away, and the row goes back to normal the instant the
    // download ends.
    if app.rag.downloading() {
        // " ⇣ " + the 10-cell bar + " NN%" — the ladder's glyph+bar+pct rung
        const COMPACT: u16 = 17;
        if let Some((kind, msg)) = &app.status {
            if area.width > COMPACT + 4 {
                let bar_rect = Rect { width: COMPACT, ..area };
                let msg_rect =
                    Rect { x: area.x + COMPACT, width: area.width - COMPACT, ..area };
                f.render_widget(Paragraph::new(rag_download_line(app, COMPACT)), bar_rect);
                let color = match kind {
                    StatusKind::Info => p.accent,
                    StatusKind::Success => p.green,
                    StatusKind::Error => p.danger,
                };
                f.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        format!(" · {msg}"),
                        Style::default().fg(color),
                    ))),
                    msg_rect,
                );
                return;
            }
        }
        f.render_widget(Paragraph::new(rag_download_line(app, area.width)), area);
        return;
    }
    // A transient message (coloured by kind) takes precedence.
    if let Some((kind, msg)) = &app.status {
        let color = match kind {
            StatusKind::Info => p.accent,
            StatusKind::Success => p.green,
            StatusKind::Error => p.danger,
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(format!(" {msg}"), Style::default().fg(color)))),
            area,
        );
        return;
    }
    // Forms / settings keep their own key hints; everything else gets the
    // k9s-style keybar, rendered from the keymap so it can never lie.
    if app.settings_ui.is_some() || app.form.is_some() {
        let key = if app.settings_ui.is_some() { Key::SettingsKeys } else { Key::FormKeys };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(" {}", i18n::t(app.effective_lang(), &key)),
                Style::default().fg(p.muted),
            ))),
            area,
        );
        return;
    }
    f.render_widget(Paragraph::new(keybar_line(app)), area);
}

/// The always-visible keybar: `↑↓ move · ⏎ … · <page verbs> · : cmd · ? keys`,
/// built from the keymap (single source, so it matches the `?` cheatsheet and
/// the actual dispatch) and localized like everything else on screen. Key chips
/// are brass, labels muted, separators hairline.
///
/// A chip is a PROMISE: it is only listed when that key really does that here
/// and now — which is why the docked detail (and its inline edit) swap the whole
/// bar for their own, shorter set.
fn keybar_line(app: &App) -> Line<'static> {
    let p = app.palette();
    let lang = app.effective_lang();
    // (key, label) chips, in reading order — the label always through i18n.
    let chip = |k: &str, key: Key| (k.to_string(), i18n::t(lang, &key));
    let mut chips: Vec<(String, String)> = Vec::new();
    if let Some(search) = app.memo_search.as_ref() {
        // the search bar owns the keyboard while it is up, and these four are
        // the whole of what it does. ⏎ changes meaning once there is something
        // to jump TO, so the chip changes with it.
        chips.push(chip(
            "⏎",
            if search.hits.is_empty() { Key::KeybarSearch } else { Key::KeybarJump },
        ));
        chips.push(chip("Tab", Key::KeybarSearchMode));
        chips.push(chip("↑↓", Key::KeybarHits));
        chips.push(chip("Esc", Key::KeybarClose));
    } else if app.memo_open {
        // the composer has every key: these are the whole of what it does with
        // one, so the bar lists exactly them. (Tab steps off the memo's chip,
        // but only over an EMPTY composer — it is the chips row's key, promised
        // up there, and it never touches a message being written.)
        chips.push(chip("⏎", Key::KeybarSend));
        // …and the newline chip names the chord this TERMINAL can actually
        // deliver: Shift+⏎ reaches us only where the kitty keyboard protocol
        // is up (`main` pushed it and said so), and a chip is a promise, so
        // everywhere else it is the Alt+⏎ that legacy terminals do send. Both
        // work when both arrive; only one of them is ever advertised.
        let newline = if app.shift_enter_ok { "Shift+⏎" } else { "Alt+⏎" };
        chips.push(chip(newline, Key::KeybarNewline));
        // Ctrl+Y copies the picked message whole (↑/↓ over an empty composer
        // pick it) — the terminal-independent way to lift a multi-line note out.
        chips.push(chip("Ctrl+Y", Key::KeybarCopy));
        chips.push(chip("Ctrl+P", Key::KeybarPhoto));
        chips.push(chip("Ctrl+F", Key::KeybarSearch));
        chips.push(chip("PgUp", Key::KeybarScroll));
        chips.push(chip("Esc", Key::KeybarClose));
    } else if app.detail_focused() {
        // the detail owns the keyboard: these — and only these — fire.
        if app.detail_edit.is_some() {
            chips.push(chip("⏎", Key::KeybarConfirm));
            chips.push(chip("Esc", Key::KeybarCancel));
        } else {
            // the four item verbs are the KEYMAP's, so the chips carry the
            // locale's own letters (it: c · m · e · p) — never English ones,
            // and never one the user re-keyed or switched off: a chip is a
            // promise, so it is drawn only when the EFFECTIVE binds still have
            // that verb.
            use crate::keymap::Verb;
            let live = crate::keymap::effective_binds(Page::Vault, app.draft_settings());
            let vault_bind = |verb: Verb| live.iter().find(|b| b.verb == verb).copied();
            let verb_chip = |chips: &mut Vec<(String, String)>, verb: Verb, key: Key| {
                if let Some(b) = vault_bind(verb) {
                    chips.push((b.key(lang).to_string(), i18n::t(lang, &key)));
                }
            };
            chips.push(chip("↑↓", Key::KeybarMove));
            verb_chip(&mut chips, Verb::CopyPassword, Key::KeybarCopy);
            chips.push(chip("⏎", Key::KeybarEditField));
            verb_chip(&mut chips, Verb::EditItem, Key::KeybarEditForm);
            verb_chip(&mut chips, Verb::DeleteItem, Key::KeybarDelete);
            verb_chip(&mut chips, Verb::StarItem, Key::KeybarStar);
            // `o` means two things here — the item's website, or the PICTURE
            // when the cursor is on an image row — so the chip wears the bind's
            // own label ("open"/"apri"), which is true of both.
            if let Some(b) = vault_bind(Verb::OpenSite) {
                chips.push((b.key(lang).to_string(), b.label(lang).to_string()));
            }
            chips.push(chip("Esc", Key::KeybarList));
        }
    } else if app.page == Page::Vault && app.search_focus {
        // the `/` box owns the keyboard exactly like the memo's bar does: a
        // letter is the query, so none of the list's verbs may be promised
        // here. These four are the whole of what is not text in it.
        chips.push(chip("⏎", Key::KeybarList));
        chips.push(chip("Tab", Key::KeybarSearchMode));
        chips.push(chip("←→", Key::KeybarCategory));
        chips.push(chip("Esc", Key::KeybarClear));
    } else {
        chips.push(chip("↑↓", Key::KeybarMove));
        chips.push(match app.page {
            Page::Access => chip("⏎", Key::KeybarUnlock),
            // ⏎ hands the keyboard to the docked detail — it does NOT copy: the
            // copy lives on `c`, once a field is under the cursor.
            Page::Vault => chip("⏎", Key::KeybarDetail),
        });
        // the page's verbs, as they REALLY are: the user's own characters, and
        // nothing for a verb they switched off (the draft is read, so the
        // settings page previews the change before it is saved).
        for b in crate::keymap::effective_binds(app.page, app.draft_settings()) {
            chips.push((b.key(lang).to_string(), b.label(lang).to_string()));
        }
        if app.page == Page::Vault {
            chips.push(chip("/", Key::KeybarFilter));
            chips.push(chip("Tab", Key::KeybarCategory));
        }
        chips.push(chip(":", Key::KeybarCmd));
        chips.push(chip("?", Key::KeybarKeys));
    }

    let mut spans = vec![Span::raw(" ")];
    for (i, (k, label)) in chips.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        // the key on a raised chip (mockups: a bordered block), label muted
        spans.push(Span::styled(
            format!(" {k} "),
            Style::default().bg(p.chip_bg).fg(p.chip_fg).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(label, Style::default().fg(p.muted)));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scroll_start_keeps_the_cursor_on_screen() {
        // fits entirely → never scrolls
        assert_eq!(scroll_start(0, 5, 10), 0);
        assert_eq!(scroll_start(4, 5, 10), 0);
        // top rows stay pinned (centered margin only kicks in past the half)
        assert_eq!(scroll_start(0, 100, 10), 0);
        assert_eq!(scroll_start(4, 100, 10), 0);
        // mid-list centers the cursor
        assert_eq!(scroll_start(50, 100, 10), 45);
        // the tail clamps so the last page is full (no blank overscroll)
        assert_eq!(scroll_start(99, 100, 10), 90);
        // the selected row is ALWAYS within [first, first+cap)
        for len in [1usize, 7, 12, 40] {
            for cap in [1usize, 3, 8, 40] {
                for sel in 0..len {
                    let first = scroll_start(sel, len, cap);
                    assert!(sel >= first && sel < first + cap, "sel {sel} not in window at len {len} cap {cap}");
                }
            }
        }
    }

    use crate::app::{Focus, Page, PanelView, PickerKind};
    use crate::crypto::{Algorithm, Argon2Variant, KdfSpec};
    use crate::i18n::Lang;
    use crate::paths::AppPaths;
    use crate::vault::Vault;
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};
    use ratatui::{backend::TestBackend, Terminal};
    use tempfile::TempDir;

    /// The app's file layout inside a scratch directory — shared by `app_with`
    /// and by the tests that have to plant a config file BEFORE `App::new`
    /// reads it.
    fn paths_in(dir: &TempDir) -> AppPaths {
        AppPaths {
            vaults_dir: dir.path().to_path_buf(),
            commands_config: dir.path().join("commands.toml"),
            settings_config: dir.path().join("settings.toml"),
            ui_config: dir.path().join("ui.toml"),
            logo_config: dir.path().join("logo.txt"),
            // empty and never filled: no test downloads a model (the RAG tests
            // swap in `MockEmbedder` instead — see `app.use_mock_rag()`).
            models_dir: dir.path().join("models"),
        }
    }

    fn app_with(vaults: &[&str]) -> (App, TempDir) {
        let dir = TempDir::new().unwrap();
        let fast = KdfSpec::Argon2 {
            variant: Argon2Variant::Id,
            m_cost_kib: 8,
            t_cost: 1,
            p_cost: 1,
        };
        for v in vaults {
            Vault::create(dir.path(), v, "pw", Algorithm::Aes256Gcm, fast, "Vanilla").unwrap();
        }
        let mut app = App::new(paths_in(&dir));
        // hide the logo header by default in tests so the body layout matches
        // the panels the assertions look at; dedicated tests cover the header.
        app.settings.show_logo = false;
        (app, dir)
    }

    fn key(app: &mut App, code: KeyCode) {
        app.on_key(code, KeyModifiers::NONE);
    }

    fn key_ctrl(app: &mut App, code: KeyCode) {
        app.on_key(code, KeyModifiers::CONTROL);
    }

    /// Shift+key — what a terminal speaking the kitty keyboard protocol sends
    /// (a legacy one cannot spell this out; see `main::setup_terminal`).
    fn key_shift(app: &mut App, code: KeyCode) {
        app.on_key(code, KeyModifiers::SHIFT);
    }

    /// Alt+key — the same chord where the protocol is not up.
    fn key_alt(app: &mut App, code: KeyCode) {
        app.on_key(code, KeyModifiers::ALT);
    }

    /// Enters a command through the `:` palette (the command surface since the
    /// free-text parser was removed): opens the palette unless a form / settings
    /// page / the search box is already capturing text, then types the query.
    /// The caller presses Enter to run it.
    fn typed(app: &mut App, s: &str) {
        if app.palette.is_none()
            && app.form.is_none()
            && app.settings_ui.is_none()
            && app.unlock.is_none()
            && app.locked.is_none()
            && !app.search_focus
        {
            key(app, KeyCode::Char(':'));
        }
        for ch in s.chars() {
            key(app, KeyCode::Char(ch));
        }
    }

    /// Opens the `:` command palette (the command surface — `help`/`commands`
    /// land here too).
    fn open_help(app: &mut App) {
        if app.palette.is_none() {
            key(app, KeyCode::Char(':'));
        }
    }

    #[test]
    fn sync_command_opens_setup_then_saves_the_endpoint() {
        let (mut app, _d) = app_with(&["bruno"]);
        // `sync` with no config opens the setup form (no password field)
        typed(&mut app, "sync");
        key(&mut app, KeyCode::Enter);
        assert!(app.form.is_some(), "sync opens the setup form");
        assert!(
            app.form.as_ref().unwrap().fields.iter().all(|f| !f.secret),
            "the sync form never has a password field"
        );
        // fill host + user, submit
        {
            let f = app.form.as_mut().unwrap();
            f.fields[0].value = "vps.example.com".into();
            f.fields[1].value = "deploy".into();
        }
        submit_now(&mut app);
        assert!(app.form.is_none());
        // the non-secret sidecar exists and holds the endpoint (no secrets)
        let cfg = crate::sync::SyncConfig::load(&app.paths.vaults_dir, "bruno").expect("sidecar saved");
        assert_eq!(cfg.host, "vps.example.com");
        assert_eq!(cfg.user, "deploy");
        let sidecar = std::fs::read_to_string(crate::vault::sync_path(&app.paths.vaults_dir, "bruno")).unwrap();
        assert!(!sidecar.to_lowercase().contains("password"));
    }

    #[test]
    fn idle_auto_lock_fires_and_zeroizes_the_vault() {
        use crate::settings::AutoLock;
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        app.settings.autolock = AutoLock::OneMin;
        let limit = AutoLock::OneMin.ticks().unwrap();
        // a key press keeps it awake
        for _ in 0..limit / 2 {
            app.on_tick();
        }
        key(&mut app, KeyCode::Down); // resets the idle clock
        for _ in 0..limit - 1 {
            app.on_tick();
        }
        assert!(app.locked.is_none(), "still awake before the limit");
        app.on_tick();
        let lock = app.locked.as_ref().expect("auto-locked at the limit");
        assert_eq!(lock.name, "bruno");
        assert!(app.vault.is_none(), "the vault (and its key) is gone");
        // the top-bar countdown rendered while it was open is covered below
    }

    #[test]
    fn topbar_shows_the_autolock_countdown() {
        use crate::settings::AutoLock;
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        app.settings.autolock = AutoLock::FiveMin;
        let text = render_sized(&app, 110, 30);
        assert!(text.contains("locks in"), "the countdown shows inside a vault");
        app.settings.autolock = AutoLock::Off;
        assert!(!render_sized(&app, 110, 30).contains("locks in"), "off = no countdown");
    }

    #[test]
    fn totp_field_shows_the_live_code_never_the_seed() {
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        push_item(
            &mut app,
            "email",
            "Gmail",
            &[("totp", "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ", true)],
        );
        let text = render_sized(&app, 110, 34);
        assert!(!text.contains("GEZDGNBV"), "the seed never renders");
        assert!(text.contains("⟳"), "the rotation countdown shows");
        // a live 6-digit code sits on the ⟳ row (6 code digits + the countdown)
        let has_code = text.lines().any(|l| {
            l.contains('⟳') && l.chars().filter(|c| c.is_ascii_digit()).count() >= 7
        });
        assert!(has_code, "a generated code is on the totp row");
    }

    #[test]
    fn add_field_grows_and_stores_custom_fields_on_items() {
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        typed(&mut app, "add");
        key(&mut app, KeyCode::Enter); // category picker
        key(&mut app, KeyCode::Enter); // email -> add form
        typed(&mut app, "Gmail"); // label
        key(&mut app, KeyCode::Up); // wrap to the trailing `+ add field` row
        let before = app.form.as_ref().unwrap().fields.len();
        typed(&mut app, "porta"); // naming it grows a fresh adder below
        assert_eq!(app.form.as_ref().unwrap().fields.len(), before + 1);
        key(&mut app, KeyCode::Enter); // convert: the row becomes the value input
        {
            let f = app.form.as_ref().unwrap();
            let fld = &f.fields[f.focused];
            assert!(!fld.adder, "the named adder became a real field");
            assert_eq!(fld.name, "porta");
            assert!(fld.value.is_empty(), "ready for its value");
        }
        typed(&mut app, "8080");
        submit_now(&mut app);
        let v = app.vault.as_ref().unwrap();
        let item = &v.items()[0];
        let custom = item.fields.iter().find(|f| f.key == "porta").expect("custom field stored");
        assert_eq!(custom.value.as_str(), "8080");
        assert_eq!(custom.label.as_deref(), Some("porta"));
    }

    #[test]
    fn category_field_secrecy_is_inferred_from_the_name() {
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        typed(&mut app, "category Server");
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Down); // -> add-field
        typed(&mut app, "api key");
        submit_now(&mut app);
        let v = app.vault.as_ref().unwrap();
        let spec = v.content.categories[0].fields.iter().find(|f| f.key == "api_key").unwrap();
        assert!(spec.secret, "a name containing 'key' implies a secret field");
    }

    /// Submits the open form: Enter now ADVANCES through the fields and only
    /// saves from the last one (fig-06), so walk until the form closes or an
    /// error surfaces.
    fn submit_now(app: &mut App) {
        for _ in 0..64 {
            if app.form.is_none() || app.form.as_ref().unwrap().error.is_some() {
                return;
            }
            key(app, KeyCode::Enter);
        }
        panic!("form never submitted");
    }

    fn render(app: &App) -> String {
        render_sized(app, 80, 24)
    }

    fn render_sized(app: &App, w: u16, h: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buf = terminal.backend().buffer();
        let area = buf.area;
        let mut out = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn help_view_shows_the_palette_with_descriptions() {
        let (mut app, _d) = app_with(&["bruno", "alice"]);
        open_help(&mut app);
        let text = render_sized(&app, 110, 40);
        assert!(text.contains("createVault") && text.contains("nukeVault"), "commands missing");
        assert!(text.contains("create a new encrypted vault"), "description missing");
    }

    #[test]
    fn topbar_shows_the_brand_on_access() {
        // the one-row top-bar carries the brand + context on the vault chooser.
        let (app, _d) = app_with(&["bruno"]);
        assert_eq!(app.page, Page::Access);
        let text = render_sized(&app, 90, 40);
        assert!(text.contains("DATASHIELD"), "the top-bar shows the brand");
    }

    #[test]
    fn topbar_shows_the_username_inside_a_vault() {
        // inside an unlocked vault the top-bar shows the vault name (uppercased)
        // and the item count, so you always know where you are.
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        let text = render_sized(&app, 90, 40);
        assert!(text.contains("BRUNO"), "the top-bar shows the vault name");
        assert!(text.contains("items"), "the top-bar shows the item count");
    }

    #[test]
    fn header_with_a_pathologically_wide_text_logo_never_panics() {
        // logo_dims caps a hand-edited logo width at u16::MAX; the header
        // geometry must use saturating arithmetic so debug overflow-checks
        // don't panic on `HDR_LEFT + lw + 2` / `x + lw + 2`.
        let (mut app, _d) = app_with(&["bruno"]);
        app.settings.show_logo = true;
        app.logo = vec!["#".repeat(70_000)];
        for (w, h) in [(1u16, 1u16), (20, 18), (90, 40), (200, 60)] {
            let _ = render_sized(&app, w, h); // must not panic
        }
    }

    #[test]
    fn header_with_multilingual_phrases_never_panics() {
        // the phrase pool mixes Latin/CJK (wide) chars and long lines; typing and
        // erasing them beside the logo must never panic at any width or frame.
        let (mut app, _d) = app_with(&["bruno"]);
        app.settings.show_logo = true;
        for frame in [0u64, 5, 33, 120, 777, 5000, u64::MAX - 3, u64::MAX] {
            app.frame = frame;
            for (w, h) in [(1u16, 1u16), (10, 3), (20, 18), (40, 20), (90, 40), (200, 60)] {
                let _ = render_sized(&app, w, h); // must not panic
            }
        }
    }

    #[test]
    fn landing_on_the_splash_never_parks_focus_on_the_empty_panel() {
        let (mut app, _d) = app_with(&["bruno"]);
        // navigate into a picker (focus = Panel), then Esc back to the splash
        typed(&mut app, "language");
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.effective_focus(), Focus::Panel);
        key(&mut app, KeyCode::Esc);
        assert_eq!(app.view, PanelView::Splash);
        assert_eq!(app.focus, Focus::Vaults, "splash has no items; focus the list");
        // and → can't park focus on the empty splash either
        key(&mut app, KeyCode::Right);
        assert_eq!(app.effective_focus(), Focus::Vaults);
    }

    #[test]
    fn splash_effects_do_not_panic_at_any_size_or_frame() {
        let (mut app, _d) = app_with(&["bruno"]);
        assert_eq!(app.view, PanelView::Splash);
        // seeds 0..12 cover the 3 effects (matrix/pacman/pong) several times, and
        // spread across many of pong's 10 randomly-picked matches
        for seed in 0..12u64 {
            app.splash_seed = seed;
            // include u64::MAX to catch add/shift overflow under debug checks
            for frame in [0u64, 3, 25, 121, 240, 999, u64::MAX - 1, u64::MAX] {
                app.frame = frame;
                for (w, h) in [(1u16, 1u16), (6, 3), (12, 6), (40, 20), (80, 24), (200, 60)] {
                    let _ = render_sized(&app, w, h); // must not panic
                    // also with reduce-motion (steady frames)
                    app.settings.reduce_motion = true;
                    let _ = render_sized(&app, w, h);
                    app.settings.reduce_motion = false;
                }
            }
        }
    }

    #[test]
    fn reduce_motion_freezes_every_splash_effect() {
        // with reduce-motion on, advancing app.frame must not change the render
        // (regression guard: pacman mouth, pong score, matrix rain once drifted).
        let (mut app, _d) = app_with(&["bruno"]);
        app.settings.reduce_motion = true;
        for seed in 0..6u64 {
            app.splash_seed = seed;
            app.frame = 10;
            let a = render_sized(&app, 80, 24);
            app.frame = 11;
            let b = render_sized(&app, 80, 24);
            app.frame = 200;
            let c = render_sized(&app, 80, 24);
            assert_eq!(a, b, "seed {seed}: frame advance changed a frozen effect");
            assert_eq!(a, c, "seed {seed}: distant frame changed a frozen effect");
        }
    }

    #[test]
    fn pong_matches_score_and_stay_in_bounds() {
        // every one of the ten matches must keep the ball and paddles inside the
        // field at all times, and points must actually be scored over a period.
        let (fw, fh) = (40.0_f32, 18.0_f32);
        let mut total_points = 0u32;
        for cfg in PONG_GAMES {
            let ph = cfg.ph as f32;
            for steps in [1u64, 37, 200, 500, 899] {
                let st = pong_sim(fw, fh, ph, cfg, steps);
                assert!(st.bx >= 0.0 && st.bx <= fw, "ball x out of field: {}", st.bx);
                assert!(st.by >= -0.01 && st.by <= fh + 0.01, "ball y out of field: {}", st.by);
                assert!(st.lc >= ph - 0.01 && st.lc <= fh - ph + 0.01, "left paddle escaped");
                assert!(st.rc >= ph - 0.01 && st.rc <= fh - ph + 0.01, "right paddle escaped");
            }
            total_points += {
                let st = pong_sim(fw, fh, ph, cfg, 899);
                st.ls + st.rs
            };
        }
        assert!(total_points > 0, "no match scored any point across a full period");
    }

    /// Unlocks the selected vault (app_with creates them with password "pw").
    fn open_first_vault(app: &mut App) {
        key(app, KeyCode::Enter); // Enter on the row -> inline unlock unfolds
        assert!(app.unlock.is_some(), "inline unlock open");
        for ch in "pw".chars() {
            key(app, KeyCode::Char(ch)); // typed straight into the passphrase
        }
        key(app, KeyCode::Enter); // submit -> opens the vault
        assert_eq!(app.page, Page::Vault, "vault opened");
    }

    #[test]
    fn in_vault_help_lists_the_new_item_commands() {
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        open_help(&mut app);
        // tall terminal: the vault page now has many commands and the palette
        // doesn't scroll yet (a deferred slice), so render with room for all.
        let text = render_sized(&app, 100, 60);
        for cmd in ["add", "delete", "edit", "search", "category", "nuke"] {
            assert!(text.contains(cmd), "vault help is missing '{cmd}'");
        }
    }

    #[test]
    fn add_item_via_category_picker_stores_it() {
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        typed(&mut app, "add");
        key(&mut app, KeyCode::Enter); // -> category picker
        assert!(matches!(app.active_view(), PanelView::Picker(PickerKind::ItemCategory)));
        key(&mut app, KeyCode::Enter); // pick the first category (email) -> add form
        assert!(app.form.is_some(), "the add form opened");
        typed(&mut app, "Work Gmail"); // the label field
        submit_now(&mut app);
        assert!(app.form.is_none(), "form closed on success");
        let v = app.vault.as_ref().unwrap();
        assert_eq!(v.items().len(), 1);
        assert_eq!(v.items()[0].label, "Work Gmail");
        assert_eq!(v.items()[0].category, "email");
    }

    /// Rows carrying the zxcvbn bar (`▮▮▮▯▯ good`), by their label column.
    fn meter_rows(text: &str) -> Vec<&str> {
        text.lines().filter(|l| l.contains('▮')).collect()
    }

    /// The one screen row showing `label` (panics with the frame if missing).
    fn row_with<'a>(text: &'a str, label: &str) -> &'a str {
        text.lines()
            .find(|l| l.contains(label))
            .unwrap_or_else(|| panic!("no row shows '{label}':\n{text}"))
    }

    #[test]
    fn strength_meter_rides_passwords_only_never_card_numbers_or_cvv() {
        // the meter judges a CHOSEN secret. A card number and a CVV are masked
        // like a password but are given to you, not picked — scoring them is
        // noise, so neither the docked detail nor the form may show a bar.
        use crate::model::{FieldType, Item, ItemField, SecretString};
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        let mut card = Item::new("credit_card", "Visa Intesa");
        for (key, ftype, value) in [
            ("number", FieldType::CardNumber, "4111111111111111"),
            ("cvv", FieldType::Pin, "371"),
            ("password", FieldType::Password, "correct horse battery staple"),
        ] {
            card.fields.push(ItemField {
                key: key.to_string(),
                ftype,
                label: None,
                value: SecretString::new(value),
                secret: true,
                extra: serde_json::Map::new(),
            });
        }
        app.vault.as_mut().unwrap().content.items.push(card);

        // ── docked detail (fig-03): one bar, and it is the password's ──────
        let text = render_sized(&app, 100, 30);
        let bars = meter_rows(&text);
        assert_eq!(bars.len(), 1, "exactly one field row carries a meter:\n{text}");
        assert!(bars[0].contains("password"), "the meter rides the password row: {}", bars[0]);
        for label in ["card number", "CVV"] {
            let row = row_with(&text, label);
            assert!(!row.contains('▮') && !row.contains('▯'), "'{label}' must show no meter: {row}");
        }

        // ── add form (fig-06): typing a card number / CVV shows no bar ─────
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        typed(&mut app, "add");
        key(&mut app, KeyCode::Enter); // -> category picker (BUILTINS order)
        for _ in 0..3 {
            key(&mut app, KeyCode::Down); // email -> account -> login -> credit_card
        }
        key(&mut app, KeyCode::Enter); // -> form: label, cardholder, brand, number, …
        for _ in 0..3 {
            key(&mut app, KeyCode::Down); // -> the card number row
        }
        typed(&mut app, "4111111111111111");
        for _ in 0..2 {
            key(&mut app, KeyCode::Down); // -> expiry -> cvv
        }
        typed(&mut app, "371");
        let text = render_sized(&app, 100, 30);
        assert!(meter_rows(&text).is_empty(), "no meter while typing a card number / CVV:\n{text}");
        // and it never panics however small the terminal gets
        for (w, h) in [(1, 1), (6, 4), (40, 24), (80, 24), (200, 60)] {
            let _ = render_sized(&app, w, h);
        }

        // ── a real password row still gets its bar ────────────────────────
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        typed(&mut app, "add");
        key(&mut app, KeyCode::Enter); // -> category picker
        for _ in 0..2 {
            key(&mut app, KeyCode::Down); // -> login
        }
        key(&mut app, KeyCode::Enter); // -> form: label, username, email, password, …
        for _ in 0..3 {
            key(&mut app, KeyCode::Down); // -> the password row
        }
        typed(&mut app, "correct horse battery staple");
        let text = render_sized(&app, 100, 30);
        assert_eq!(meter_rows(&text).len(), 1, "the password row keeps its meter:\n{text}");
    }

    #[test]
    fn a_hand_added_cvv_or_seed_row_is_typed_so_no_meter_rides_it() {
        // the template's cvv slot is safe because it carries a real type; a row
        // the user adds by hand must be typed just as precisely, or the meter
        // (and the `type` written into the file) would lie about it.
        use crate::model::FieldType;
        for (name, ftype) in [
            ("cvv", FieldType::Pin),
            ("pin", FieldType::Pin),
            ("totp", FieldType::Totp),
            // vaguer names stay plain TEXT — masked by `secret`, never turned
            // into a live code (a recovery phrase lives on rows like these)
            ("seed", FieldType::Text),
            ("2fa", FieldType::Text),
            ("api token", FieldType::Text),
        ] {
            let (mut app, _d) = app_with(&["bruno"]);
            open_first_vault(&mut app);
            typed(&mut app, "add");
            key(&mut app, KeyCode::Enter); // category picker
            key(&mut app, KeyCode::Enter); // email -> add form
            typed(&mut app, "Card"); // the label row
            key(&mut app, KeyCode::Up); // wrap to the trailing `+ add field` row
            typed(&mut app, name);
            key(&mut app, KeyCode::Enter); // convert: the row takes its value
            typed(&mut app, "371");
            {
                let f = app.form.as_ref().unwrap();
                let row = &f.fields[f.focused];
                assert!(row.secret, "'{name}' is still masked while typing");
                assert!(!row.show_strength, "'{name}' must carry no meter in the form");
            }
            let text = render_sized(&app, 100, 30);
            assert!(meter_rows(&text).is_empty(), "no meter while typing '{name}':\n{text}");

            submit_now(&mut app);
            let stored = app.vault.as_ref().unwrap().items()[0]
                .fields
                .iter()
                .find(|f| f.key == crate::model::slugify(name))
                .unwrap_or_else(|| panic!("the '{name}' field was stored"));
            assert_eq!(stored.ftype, ftype, "'{name}' is written with its own DCS-1 type");
            assert!(stored.secret, "…and stays masked");
            let text = render_sized(&app, 100, 30);
            assert!(meter_rows(&text).is_empty(), "no meter on the stored '{name}' row:\n{text}");
        }
    }

    #[test]
    fn item_detail_masks_secrets_until_revealed() {
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        typed(&mut app, "add");
        key(&mut app, KeyCode::Enter); // category picker
        key(&mut app, KeyCode::Enter); // email -> form (label, email, password, …)
        typed(&mut app, "Gmail"); // label (field 0)
        key(&mut app, KeyCode::Down); // -> email
        key(&mut app, KeyCode::Down); // -> password
        typed(&mut app, "s3cr3t");
        submit_now(&mut app);
        assert_eq!(app.vault.as_ref().unwrap().items().len(), 1);

        // the detail is ALWAYS docked (fig-03): no screen to open
        let masked = render(&app);
        assert!(masked.contains("Gmail"), "the label shows");
        assert!(!masked.contains("s3cr3t"), "the secret is masked by default");
        key(&mut app, KeyCode::Char('r')); // reveal verb
        assert!(render(&app).contains("s3cr3t"), "the secret is revealed after r");
    }

    /// The RFC 6238 seed (see `totp::tests`) — its code at t=59 is 287082.
    const TOTP_SEED: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";

    /// Pushes a three-field item and hands the keyboard to the docked detail.
    fn detail_focused_app() -> (App, TempDir) {
        let (mut app, dir) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        push_item(
            &mut app,
            "email",
            "Gmail",
            &[("email", "bruno@gmail.com", false), ("password", "s3cr3t", true), ("totp", TOTP_SEED, true)],
        );
        key(&mut app, KeyCode::Enter); // ⏎ on the item row -> the detail takes focus
        assert!(app.detail_focused(), "⏎ moves focus onto the docked detail");
        (app, dir)
    }

    #[test]
    fn enter_on_an_item_focuses_the_detail_and_esc_gives_the_list_back() {
        let (mut app, _d) = detail_focused_app();
        assert_eq!(app.detail_index(), 0, "the cursor starts on the first field");
        key(&mut app, KeyCode::Down);
        assert_eq!(app.detail_index(), 1, "↓ walks the field rows");
        key(&mut app, KeyCode::Up);
        key(&mut app, KeyCode::Up);
        assert_eq!(app.detail_index(), 2, "↑ wraps to the last row");
        key(&mut app, KeyCode::Esc);
        assert_eq!(app.effective_focus(), Focus::Vaults, "Esc hands the list back");
        assert!(app.vault.is_some(), "…and never closes the vault on the way");
        assert_eq!(app.detail_index(), 0, "the cursor starts over next time");
    }

    #[test]
    fn c_on_a_totp_row_resolves_to_the_code_never_the_seed() {
        // the clipboard itself needs a display server; what MATTERS is which
        // bytes `c` picks, and that is the pure `copy_payload` the app calls.
        let (mut app, _d) = detail_focused_app();
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Down); // cursor -> the totp row
        let item = app.highlighted_item().expect("an item on show");
        let idx = crate::app::detail_field_rows(item)[app.detail_index()];
        let payload = crate::clipboard::copy_payload(item, idx, 59).expect("something to copy");
        assert_eq!(payload.as_str(), "287082", "the live code…");
        assert_ne!(payload.as_str(), TOTP_SEED, "…never the seed");
    }

    #[test]
    fn inline_edit_cancels_clean_and_confirms_all_the_way_to_disk() {
        let (mut app, _d) = detail_focused_app();
        key(&mut app, KeyCode::Enter); // ⏎ opens the inline edit on the email row
        assert_eq!(
            app.detail_edit.as_ref().map(|e| e.buffer.as_str()),
            Some("bruno@gmail.com"),
            "a plain row starts pre-filled"
        );
        retype(&mut app, "bruno@gmail.com".len(), "new@x.com");
        key(&mut app, KeyCode::Esc); // cancel
        assert!(app.detail_edit.is_none(), "the buffer is gone");
        assert!(app.detail_focused(), "…and focus stays on the detail");
        assert_eq!(field_value(&app, 0), "bruno@gmail.com", "the stored value is untouched");
        assert!(render(&app).contains("bruno@gmail.com"), "the row shows it again");

        // …and a confirmed edit goes through the normal save (disk included)
        key(&mut app, KeyCode::Enter);
        retype(&mut app, "bruno@gmail.com".len(), "new@x.com");
        key(&mut app, KeyCode::Enter); // confirm
        assert!(app.detail_edit.is_none());
        assert_eq!(field_value(&app, 0), "new@x.com");
        assert!(
            app.vault.as_ref().unwrap().items()[0].modified_at.is_some(),
            "an inline edit stamps modified_at, like the edit form"
        );
        let reopened = Vault::open(&app.paths.vaults_dir, "bruno", "pw").expect("reopens");
        assert_eq!(reopened.items()[0].fields[0].value.as_str(), "new@x.com", "it reached the file");
    }

    #[test]
    fn an_inline_edited_secret_confirmed_empty_keeps_the_stored_value() {
        let (mut app, _d) = detail_focused_app();
        key(&mut app, KeyCode::Down); // -> the password row
        key(&mut app, KeyCode::Enter);
        assert_eq!(
            app.detail_edit.as_ref().map(|e| e.buffer.as_str()),
            Some(""),
            "a secret row starts EMPTY — it never leaks its length"
        );
        assert!(
            render_sized(&app, 140, 30).contains("type to replace"),
            "…and says what confirming it empty means"
        );
        key(&mut app, KeyCode::Enter); // confirm empty
        assert_eq!(field_value(&app, 1), "s3cr3t", "the stored secret survives");
        // typing one DOES replace it
        key(&mut app, KeyCode::Enter);
        retype(&mut app, 0, "n3w");
        key(&mut app, KeyCode::Enter);
        assert_eq!(field_value(&app, 1), "n3w");
        assert!(!render(&app).contains("n3w"), "and it goes straight back under the mask");
    }

    #[test]
    fn the_keybar_promises_exactly_what_the_detail_does() {
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        push_item(&mut app, "email", "Gmail", &[("email", "bruno@gmail.com", false)]);
        app.status = None; // a toast hides the keybar; we want the bar itself
        let list = keybar_of(&render_sized(&app, 120, 30));
        // ⏎ hands over to the detail — the old "⏎ copy" and "1–9 field" chips
        // promised behaviour that never existed and are gone.
        assert!(list.contains("⏎  detail"), "⏎ hands over to the detail");
        assert!(!list.contains("1–9"), "the never-built field digits are gone");
        key(&mut app, KeyCode::Enter);
        let detail = keybar_of(&render_sized(&app, 120, 30));
        for chip in ["copy", "edit", "form", "delete", "star", "open", "list"] {
            assert!(detail.contains(chip), "the detail keybar is missing '{chip}'");
        }
        key(&mut app, KeyCode::Enter); // inline edit: only two keys fire
        let editing = keybar_of(&render_sized(&app, 120, 30));
        assert!(editing.contains("confirm") && editing.contains("cancel"));
        assert!(!editing.contains("delete"), "a key that can't fire is never promised");
    }

    #[test]
    fn the_detail_keybar_is_localized() {
        let (mut app, _d) = detail_focused_app();
        app.settings.lang = Lang::It;
        let bar = keybar_of(&render_sized(&app, 120, 30));
        for chip in ["copia", "modifica", "scheda", "elimina", "preferito", "apri", "lista"] {
            assert!(bar.contains(chip), "the italian keybar is missing '{chip}'");
        }
        // …and the KEYS are the italian keymap's (c · m · e · p · o), never the
        // english ones: a chip is a promise, and `d` is bound to nothing here.
        for k in [" c ", " m ", " e ", " p ", " o "] {
            assert!(bar.contains(k), "the italian keybar is missing the '{k}' chip: {bar}");
        }
        assert!(!bar.contains(" d "), "`d` does nothing in italian — never advertise it: {bar}");
    }

    #[test]
    fn the_docked_detail_verbs_follow_the_locale_keymap() {
        // english: the vault keymap's own letters (c copy · e edit · d del)
        let (mut app, _d) = detail_focused_app();
        key(&mut app, KeyCode::Char('d'));
        assert!(app.pending_confirm.is_some(), "en `d` arms the delete confirm");
        app.pending_confirm = None;
        key(&mut app, KeyCode::Char('e'));
        assert!(app.form.is_some(), "en `e` opens the edit form");

        // italian: `e` is elimina on the list, so it must be elimina here too —
        // the same key can't mean "delete" one ⏎ away and "edit" in the detail.
        let (mut app, _d) = detail_focused_app();
        app.settings.lang = Lang::It;
        key(&mut app, KeyCode::Char('e'));
        assert!(app.pending_confirm.is_some(), "it `e` deletes, exactly as on the list");
        assert!(app.form.is_none(), "…and never opens the edit form");
        app.pending_confirm = None;
        key(&mut app, KeyCode::Char('m'));
        assert!(app.form.is_some(), "it `m` (modifica) opens the edit form");

        // …and a key the italian keymap binds to nothing stays inert
        let (mut app, _d) = detail_focused_app();
        app.settings.lang = Lang::It;
        key(&mut app, KeyCode::Char('d'));
        assert!(app.pending_confirm.is_none(), "`d` arms nothing in italian");
        assert!(app.form.is_none());
        key(&mut app, KeyCode::Char('p'));
        assert!(app.highlighted_item().unwrap().favorite, "it `p` (preferito) stars the item");
    }

    #[test]
    fn vim_keys_walk_the_detail_rows_like_the_arrows() {
        // j/k fall through to `move_selection`, whose panel branch counts the
        // PICKER rows — zero on the resting view. So without an answer here the
        // vim keys simply died the moment ⏎ handed the keyboard to the detail.
        let (mut app, _d) = detail_focused_app();
        assert_eq!(app.detail_index(), 0);
        key(&mut app, KeyCode::Char('j'));
        assert_eq!(app.detail_index(), 1, "j walks down, exactly like ↓");
        key(&mut app, KeyCode::Char('k'));
        assert_eq!(app.detail_index(), 0, "k walks up, exactly like ↑");
        key(&mut app, KeyCode::Char('k'));
        assert_eq!(app.detail_index(), 2, "…and wraps, exactly as the arrows do");
        // …and the list underneath never moved with them
        assert_eq!(app.selected, 0, "j/k are the detail's here, not the list's");

        // inside an inline edit they are TEXT again — a value may well start
        // with a 'j', and the buffer must get it.
        key(&mut app, KeyCode::Enter); // the totp row is secret: an empty buffer
        key(&mut app, KeyCode::Char('j'));
        key(&mut app, KeyCode::Char('k'));
        assert_eq!(app.detail_edit.as_ref().map(|e| e.buffer.as_str()), Some("jk"));
        assert_eq!(app.detail_index(), 2, "the cursor never moved while typing");
    }

    #[test]
    fn a_pull_landing_mid_edit_writes_to_the_item_the_edit_started_on() {
        // `poll_sync` replaces the whole vault content when a pull lands. An
        // inline edit that resolved its target by INDEX at commit would then
        // hand the buffer to whatever ended up at that position — silently
        // rewriting another item's field.
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        push_item(&mut app, "email", "Alpha", &[("email", "a@x.com", false)]);
        push_item(&mut app, "email", "Beta", &[("email", "b@x.com", false)]);
        let alpha = app.vault.as_ref().unwrap().items()[0].id.clone();
        key(&mut app, KeyCode::Enter); // ⏎ on Alpha -> the detail takes focus
        key(&mut app, KeyCode::Enter); // ⏎ on its email row -> inline edit
        retype(&mut app, "a@x.com".len(), "new@x.com");
        // …and now a pull lands, reordering the items under the open edit
        app.vault.as_mut().unwrap().items_mut().reverse();
        key(&mut app, KeyCode::Enter); // confirm
        let v = app.vault.as_ref().unwrap();
        let by_label = |l: &str| {
            v.items().iter().find(|i| i.label == l).unwrap().fields[0].value.as_str().to_string()
        };
        assert_eq!(by_label("Alpha"), "new@x.com", "the edit landed on the item it started on");
        assert_eq!(by_label("Beta"), "b@x.com", "…and never touched the one now in its place");
        assert_eq!(v.find_by_id(&alpha), Some(1), "(the pull really did reorder them)");
    }

    #[test]
    fn an_edit_whose_row_vanished_mid_flight_writes_nothing_and_says_so() {
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        push_item(
            &mut app,
            "email",
            "Alpha",
            &[("email", "a@x.com", false), ("password", "s3cr3t", true)],
        );
        key(&mut app, KeyCode::Enter); // -> the detail
        key(&mut app, KeyCode::Down); // -> the password row
        key(&mut app, KeyCode::Enter);
        retype(&mut app, 0, "n3w");
        // a pull drops that very row out of the item
        app.vault.as_mut().unwrap().items_mut()[0].fields.remove(1);
        key(&mut app, KeyCode::Enter); // confirm
        let v = app.vault.as_ref().unwrap();
        assert_eq!(v.items()[0].fields.len(), 1, "nothing was written back");
        assert_eq!(v.items()[0].fields[0].value.as_str(), "a@x.com", "…and nothing was clobbered");
        assert!(
            app.status.as_ref().is_some_and(|(_, s)| *s == i18n::t(Lang::En, &Key::FieldGone)),
            "…with a toast saying why: {:?}",
            app.status
        );
    }

    #[test]
    fn the_copy_and_open_verbs_act_on_the_item_instead_of_promising_later() {
        // the keybar advertises `c copy · u user · o open`: for as long as the
        // three verbs answered "coming soon" every one of those chips was a lie.
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        push_item(&mut app, "note", "Bare", &[("body", "nothing useful here", false)]);
        let toast = |app: &App| app.status.as_ref().map(|(_, s)| s.clone()).unwrap_or_default();
        // an item with none of the three says WHICH one it hasn't got
        for (k, expected) in [
            ('c', Key::NoPasswordField),
            ('u', Key::NoUserField),
            ('o', Key::NoWebsiteField),
        ] {
            key(&mut app, KeyCode::Char(k));
            assert_eq!(toast(&app), i18n::t(Lang::En, &expected), "'{k}' on a bare item");
            assert_ne!(toast(&app), "coming soon");
        }
        // …and an item that HAS those rows resolves them and goes down the real
        // copy path — proved on EMPTY ones, so the suite never writes to the
        // machine's actual clipboard (which bytes leave the vault is settled by
        // `clipboard::copy_payload` and `model::item_*_index`, both pure).
        push_item(&mut app, "login", "Acme", &[("username", "", false), ("password", "", true)]);
        key(&mut app, KeyCode::Down); // -> Acme
        for k in ['c', 'u'] {
            key(&mut app, KeyCode::Char(k));
            assert_eq!(
                toast(&app),
                i18n::t(Lang::En, &Key::NothingToCopy),
                "'{k}' found its row (an empty one) and took the copy path"
            );
        }
        // …and the italian keys reach the same handlers (`c` copia · `u` utente)
        app.settings.lang = Lang::It;
        key(&mut app, KeyCode::Char('c'));
        assert_eq!(toast(&app), "niente da copiare su questa riga");
        // `y` is the old vi mnemonic and is bound to NOTHING now: it must not
        // quietly keep copying (a freed key that still fires is a lie twice).
        app.settings.lang = Lang::En;
        app.status = None;
        key(&mut app, KeyCode::Char('y'));
        assert!(app.status.is_none(), "'y' no longer copies anything: {:?}", app.status);
    }

    #[test]
    fn the_copy_and_open_verbs_read_the_same_item_from_the_detail() {
        // `u` and `o` reach `dispatch_verb` through the detail's fall-through,
        // so they must answer there exactly as they do on the list.
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        // (an empty row on purpose: the suite must not write to the machine's
        // real clipboard just to prove where a key lands)
        push_item(&mut app, "note", "Bare", &[("body", "", false)]);
        key(&mut app, KeyCode::Enter); // -> the docked detail
        assert!(app.detail_focused());
        key(&mut app, KeyCode::Char('u'));
        assert_eq!(
            app.status.as_ref().map(|(_, s)| s.clone()).unwrap_or_default(),
            i18n::t(Lang::En, &Key::NoUserField)
        );
        key(&mut app, KeyCode::Char('o'));
        assert_eq!(
            app.status.as_ref().map(|(_, s)| s.clone()).unwrap_or_default(),
            i18n::t(Lang::En, &Key::NoWebsiteField)
        );
        assert!(app.detail_focused(), "…and the keyboard stayed on the detail");
        // …in italian too, on the italian keys (`c` copia · `u` utente)
        app.settings.lang = Lang::It;
        key(&mut app, KeyCode::Char('c'));
        assert_eq!(
            app.status.as_ref().map(|(_, s)| s.clone()).unwrap_or_default(),
            "niente da copiare su questa riga",
            "the detail's `c` still copies the ROW under the cursor"
        );
        key(&mut app, KeyCode::Char('u'));
        assert_eq!(
            app.status.as_ref().map(|(_, s)| s.clone()).unwrap_or_default(),
            "nessun utente in questo elemento"
        );
    }

    #[test]
    fn a_tall_item_scrolls_the_detail_so_the_cursor_row_is_always_drawn() {
        // the cursor walks EVERY field row, so the pane has to scroll with it:
        // a copy or an inline edit on a row that was never drawn is blind.
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        let owned: Vec<(String, String)> =
            (0..20).map(|i| (format!("f{i}"), format!("value{i}"))).collect();
        let fields: Vec<(&str, &str, bool)> =
            owned.iter().map(|(k, v)| (k.as_str(), v.as_str(), false)).collect();
        push_item(&mut app, "login", "Tall", &fields);
        key(&mut app, KeyCode::Enter); // ⏎ on the row -> the detail takes focus
        for row in 0..owned.len() {
            let text = render_sized(&app, 100, 24);
            assert_eq!(app.detail_index(), row);
            assert!(text.contains(&format!("value{row}")), "row {row} is off-screen:\n{text}");
            key(&mut app, KeyCode::Down);
        }
        // …and an inline edit deep down the list is drawn where it is typed
        for _ in 0..15 {
            key(&mut app, KeyCode::Down);
        }
        key(&mut app, KeyCode::Enter);
        for ch in "TYPED".chars() {
            key(&mut app, KeyCode::Char(ch));
        }
        assert!(render_sized(&app, 100, 24).contains("TYPED"), "the inline edit is on screen");
        // …at every size, cursor included
        for (w, h) in [(1u16, 1u16), (6, 4), (40, 24), (80, 24), (200, 60)] {
            let _ = render_sized(&app, w, h); // must not panic
        }
    }

    #[test]
    fn an_inline_edit_that_filters_its_own_item_out_saves_the_shown_one() {
        // the pane draws the CLAMPED selection, so every verb must resolve the
        // same way — otherwise an edit that drops its item out of the query
        // silently writes nothing (or writes to the item next door).
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        push_item(&mut app, "email", "Alpha", &[("email", "bruno@a.com", false)]);
        push_item(&mut app, "email", "Beta", &[("email", "bruno@b.com", false)]);
        key(&mut app, KeyCode::Char('/')); // the search box takes the keyboard
        for ch in "bruno@".chars() {
            key(&mut app, KeyCode::Char(ch));
        }
        key(&mut app, KeyCode::Enter); // back to the list, the query kept
        key(&mut app, KeyCode::Down); // -> Beta
        assert_eq!(app.visible_item_indices().len(), 2);
        assert_eq!(app.selected, 1);
        key(&mut app, KeyCode::Enter); // focus the detail
        key(&mut app, KeyCode::Enter); // inline edit on the email row
        retype(&mut app, "bruno@b.com".len(), "x@y.com");
        key(&mut app, KeyCode::Enter); // commit -> Beta stops matching
        assert_eq!(field_of(&app, 1, 0), "x@y.com", "the edit landed on the shown item");
        assert_eq!(app.visible_item_indices().len(), 1, "…which left the filter");
        assert_eq!(app.selected, 0, "the cursor comes back onto a row that exists");
        // a second edit writes to the item the pane is actually drawing
        key(&mut app, KeyCode::Enter);
        retype(&mut app, "bruno@a.com".len(), "z@y.com");
        key(&mut app, KeyCode::Enter);
        assert_eq!(field_of(&app, 0, 0), "z@y.com", "…never a silent no-op");
    }

    /// The stored value of item `i`'s field `f` (the point is what is IN the
    /// vault, not what is on screen).
    fn field_of(app: &App, i: usize, f: usize) -> String {
        app.vault.as_ref().unwrap().items()[i].fields[f].value.as_str().to_string()
    }

    #[test]
    fn the_focused_detail_and_its_inline_edit_never_panic_at_any_size() {
        const SIZES: [(u16, u16); 5] = [(1, 1), (6, 4), (40, 24), (80, 24), (200, 60)];
        let (mut app, _d) = detail_focused_app();
        // the cursor on every row (plain · secret · totp), at every size
        for _ in 0..4 {
            for (w, h) in SIZES {
                let _ = render_sized(&app, w, h); // must not panic
            }
            key(&mut app, KeyCode::Down);
        }
        // …and again with the inline edit open and typed into
        for _ in 0..3 {
            key(&mut app, KeyCode::Enter);
            for ch in "a longer value than the pane is wide".chars() {
                key(&mut app, KeyCode::Char(ch));
            }
            for (w, h) in SIZES {
                let _ = render_sized(&app, w, h);
            }
            key(&mut app, KeyCode::Esc);
            key(&mut app, KeyCode::Down);
        }
    }

    /// Erases `back` characters of the inline edit buffer, then types `text`.
    /// (The `typed` helper would open the `:` palette — it can't see an inline
    /// edit — so the keys go in raw here.)
    fn retype(app: &mut App, back: usize, text: &str) {
        for _ in 0..back {
            key(app, KeyCode::Backspace);
        }
        for ch in text.chars() {
            key(app, KeyCode::Char(ch));
        }
    }

    /// The stored value of the first item's field `i` (secrets included — the
    /// point of the assertion is what is IN the vault, not what is on screen).
    fn field_value(app: &App, i: usize) -> String {
        app.vault.as_ref().unwrap().items()[0].fields[i].value.as_str().to_string()
    }

    /// The keybar row of a rendered frame (the bottom line of the chrome).
    fn keybar_of(frame: &str) -> String {
        frame.lines().last().unwrap_or("").to_string()
    }

    /// Pushes an item straight into the open vault (bypasses the Add form).
    fn push_item(app: &mut App, category: &str, label: &str, fields: &[(&str, &str, bool)]) {
        use crate::model::{FieldType, Item, ItemField, SecretString};
        let mut it = Item::new(category, label);
        for (k, v, secret) in fields {
            it.fields.push(ItemField {
                key: (*k).to_string(),
                ftype: FieldType::Text,
                label: None,
                value: SecretString::new(*v),
                secret: *secret,
                extra: serde_json::Map::new(),
            });
        }
        app.vault.as_mut().unwrap().content.items.push(it);
    }

    #[test]
    fn vault_list_shows_label_and_secondary_field() {
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        push_item(&mut app, "email", "Amazon", &[("email", "bruno@gmail.com", false), ("password", "S3CRETXYZ", true)]);
        let text = render(&app);
        assert!(text.contains("Amazon"), "the label shows");
        assert!(text.contains("bruno@gmail.com"), "the first non-secret field is the subtitle");
        assert!(!text.contains("S3CRETXYZ"), "a secret is never used as the subtitle");
    }

    #[test]
    fn single_key_verbs_dispatch_in_a_vault() {
        let (mut app, _d) = app_with(&["personal"]);
        open_first_vault(&mut app);
        push_item(&mut app, "email", "Gmail", &[("email", "a@b.com", false), ("password", "x", true)]);
        // 'a' adds (opens the category picker), 's' stars
        key(&mut app, KeyCode::Char('a'));
        assert_eq!(app.active_view(), PanelView::Picker(PickerKind::ItemCategory), "'a' opens the add picker");
        key(&mut app, KeyCode::Esc);
        key(&mut app, KeyCode::Char('s'));
        assert!(app.vault.as_ref().unwrap().items()[0].favorite, "'s' stars the item");
    }

    #[test]
    fn single_key_verbs_on_access_manage_vaults() {
        let (mut app, _d) = app_with(&["personal"]);
        key(&mut app, KeyCode::Char('n'));
        assert!(app.form.is_some(), "'n' opens the new-vault form");
        key(&mut app, KeyCode::Esc);
        key(&mut app, KeyCode::Char(','));
        assert!(app.settings_ui.is_some(), "',' opens settings");
    }

    #[test]
    fn a_bare_letter_is_never_typed_as_text_anymore() {
        // the free-text command line is gone: bare letters are verbs or inert.
        let (mut app, _d) = app_with(&["personal"]);
        for ch in "zzq".chars() {
            // z has no verb; q on Access has none either → all inert
            key(&mut app, KeyCode::Char(ch));
        }
        assert!(app.input.is_empty(), "bare keys never fill a command line");
        assert!(app.palette.is_none(), "and don't open the palette");
    }

    #[test]
    fn palette_opens_with_colon_and_ranks_fuzzily() {
        let (mut app, _d) = app_with(&["personal"]);
        open_first_vault(&mut app);
        key(&mut app, KeyCode::Char(':'));
        assert!(app.palette.is_some(), "':' opens the command palette");
        typed(&mut app, "re");
        let m = app.palette_matches();
        assert!(!m.is_empty(), "'re' matches something");
        assert_eq!(m[0].name, "rerollEnc", "fuzzy 're' ranks rerollEnc first");
    }

    #[test]
    fn palette_matches_italian_aliases() {
        // "cerca" is the IT alias of `search` — the palette must find it so the
        // command surface isn't English-only.
        let (mut app, _d) = app_with(&["personal"]);
        open_first_vault(&mut app);
        key(&mut app, KeyCode::Char(':'));
        typed(&mut app, "cerca");
        let m = app.palette_matches();
        assert!(m.iter().any(|c| c.name == "search"), "'cerca' finds the search command");
    }

    #[test]
    fn palette_enter_runs_the_highlighted_command() {
        let (mut app, _d) = app_with(&["personal"]);
        open_first_vault(&mut app);
        key(&mut app, KeyCode::Char(':'));
        typed(&mut app, "encinfo");
        key(&mut app, KeyCode::Enter);
        assert!(app.palette.is_none(), "Enter closes the palette");
        assert!(matches!(app.active_view(), PanelView::EncInfo(_)), "and runs the command");
    }

    #[test]
    fn search_filters_and_never_matches_secrets() {
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        push_item(&mut app, "email", "Amazon", &[("email", "a@x.com", false), ("password", "topsecret", true)]);
        push_item(&mut app, "account", "Spid", &[("username", "galimibruno1", false)]);
        assert_eq!(app.left_len(), 2, "both visible under the All tab");

        key(&mut app, KeyCode::Up); // reach the search box from the first item
        assert!(app.search_focus, "Up from the top focuses the search box");
        typed(&mut app, "spid");
        assert_eq!(app.left_len(), 1, "only Spid matches by label");
        assert_eq!(app.visible_item_indices(), vec![1]);

        key(&mut app, KeyCode::Esc); // clear the query
        typed(&mut app, "topsecret");
        assert_eq!(app.left_len(), 0, "secret field values are never matched");
    }

    #[test]
    fn category_tab_filters_the_list() {
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        push_item(&mut app, "email", "Amazon", &[("email", "a@x.com", false)]);
        push_item(&mut app, "account", "Spid", &[("username", "u", false)]);
        assert_eq!(app.left_len(), 2);
        key(&mut app, KeyCode::Tab); // Tab -> first category tab (email)
        assert_eq!(app.vault_tab, 1);
        assert_eq!(app.left_len(), 1, "the Email tab shows only the email item");
        assert_eq!(app.visible_item_indices(), vec![0]);
    }

    /// The text of the chips row's brass-filled pill — the ACTIVE chip.
    /// `render_sized` sees symbols only, and the fill is a colour.
    fn active_pill(app: &App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buf = terminal.backend().buffer();
        let fill = app.palette().accent;
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                let cell = &buf[(x, y)];
                if cell.bg == fill {
                    out.push_str(cell.symbol());
                }
            }
        }
        out
    }

    #[test]
    fn the_memorandum_is_the_last_stop_of_the_tab_cycle() {
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        push_item(&mut app, "email", "Amazon", &[]);
        push_item(&mut app, "account", "Spid", &[]);
        assert_eq!(app.vault_tab, 0, "the ALL tab, to start");

        // Shift+Tab is one step BACK, and the memo's chip is what sits there
        key(&mut app, KeyCode::BackTab);
        assert!(app.memo_open, "Shift+Tab from ALL opened the memorandum");
        assert_eq!(app.vault_tab, 0, "…and left the tab state on a REAL tab");
        assert_eq!(active_pill(&app), " ✎ memo 0 ", "the chip wears the pill:\n{}", render(&app));

        // Tab steps off it — onto ALL, with the thread closed behind it
        key(&mut app, KeyCode::Tab);
        assert!(!app.memo_open, "Tab left the memorandum");
        assert_eq!(app.vault_tab, 0);
        assert_eq!(active_pill(&app), " ALL 2 ");

        // …and Shift+Tab off it lands on the LAST category instead
        key(&mut app, KeyCode::BackTab);
        assert!(app.memo_open);
        key(&mut app, KeyCode::BackTab);
        assert!(!app.memo_open, "Shift+Tab left it too");
        assert_eq!(app.vault_tab, 2, "the last category (account)");
        assert_eq!(app.visible_item_indices(), vec![1]);
        assert_eq!(active_pill(&app), " account 1 ");

        // …so Tab forward from that last category reaches the chip again
        key(&mut app, KeyCode::Tab);
        assert!(app.memo_open, "the chip sits between the last category and ALL");

        // and the command surface still opens exactly the same state
        key(&mut app, KeyCode::Esc);
        assert!(!app.memo_open);
        typed(&mut app, "memo");
        key(&mut app, KeyCode::Enter);
        assert!(app.memo_open, "the `memo` command opens it too");
        assert_eq!(active_pill(&app), " ✎ memo 0 ", "…chip and all");
    }

    #[test]
    fn the_memo_chip_is_never_a_category() {
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        push_item(&mut app, "email", "Amazon", &[]);
        let cats = app.vault_categories();
        assert_eq!(cats, vec!["email".to_string()], "one real category");

        key(&mut app, KeyCode::BackTab); // standing on the pseudo-chip
        assert!(app.memo_open);
        assert_eq!(app.vault_categories(), cats, "the chip is not in the list the filter reads");
        assert_eq!(app.left_len(), 1, "…and nothing was filtered by it");

        // Esc keeps its own behaviour — and leaves the tab state on a REAL tab
        key(&mut app, KeyCode::Esc);
        assert!(!app.memo_open, "Esc closed the thread back to the list");
        assert_eq!(app.vault_tab, 0);
        assert_eq!(app.visible_item_indices(), vec![0]);
    }

    #[test]
    fn a_chips_row_with_many_categories_and_the_memo_never_panics_at_any_size() {
        const SIZES: [(u16, u16); 5] = [(1, 1), (6, 4), (40, 24), (80, 24), (200, 60)];
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        for i in 0..12 {
            push_item(&mut app, &format!("category_number_{i}"), &format!("item {i}"), &[]);
        }
        // every stop of the cycle, the thread included, at every size
        for _ in 0..app.vault_categories().len() + 2 {
            for (w, h) in SIZES {
                let _ = render_sized(&app, w, h); // must not panic
            }
            key(&mut app, KeyCode::Tab);
        }
        assert!(!app.memo_open, "a full turn came back to a real tab");
        assert_eq!(app.vault_tab, 0, "…the one it started on");
        // …and the same row while a query is live under it
        key(&mut app, KeyCode::Char('/'));
        typed(&mut app, "item");
        for (w, h) in SIZES {
            let _ = render_sized(&app, w, h);
        }
        // the Italian row is longer: it has to truncate, not panic
        app.settings.lang = Lang::It;
        key(&mut app, KeyCode::Esc);
        key(&mut app, KeyCode::Esc);
        for (w, h) in SIZES {
            let _ = render_sized(&app, w, h);
        }
    }

    #[test]
    fn star_toggles_favorite_shows_and_sorts_first() {
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        push_item(&mut app, "login", "Aaa", &[]);
        push_item(&mut app, "login", "Zzz", &[]);
        // highlight the 2nd item, star it via Ctrl+S
        key(&mut app, KeyCode::Down);
        assert_eq!(app.selected, 1);
        key_ctrl(&mut app, KeyCode::Char('s'));
        let starred = &app.vault.as_ref().unwrap().content.items[1];
        assert!(starred.favorite, "Zzz starred");
        // it sorts to the top and the cursor follows it
        assert_eq!(app.visible_item_indices(), vec![1, 0]);
        assert_eq!(app.selected, 0, "cursor follows the starred row");
        assert!(render(&app).contains('★'), "★ renders");
        // toggle off
        key_ctrl(&mut app, KeyCode::Char('s'));
        assert!(!app.vault.as_ref().unwrap().content.items[1].favorite);
    }

    #[test]
    fn delete_default_no_is_safe_yes_removes() {
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        push_item(&mut app, "login", "Keep", &[]);
        push_item(&mut app, "login", "Gone", &[]);
        // 'd' on the highlighted item arms a two-key y/n confirm — no form, no
        // ◂No▸ flipper. 'n' (or anything but the yes key) keeps it (default no).
        key(&mut app, KeyCode::Char('d'));
        assert!(app.pending_confirm.is_some());
        assert!(app.form.is_none(), "delete is a two-key confirm, not a form");
        key(&mut app, KeyCode::Char('n'));
        assert!(app.pending_confirm.is_none());
        assert_eq!(app.vault.as_ref().unwrap().items().len(), 2, "n never deletes");
        // now confirm-delete the highlighted item with 'd' then 'y'
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Char('d'));
        let doomed = app.pending_confirm.as_ref().map(|(_, l)| l.clone());
        key(&mut app, KeyCode::Char('y'));
        let v = app.vault.as_ref().unwrap();
        assert_eq!(v.items().len(), 1, "y removes the item");
        if let Some(d) = doomed {
            assert!(v.items().iter().all(|it| it.label != d), "the confirmed item is gone");
        }
    }

    #[test]
    fn edit_changes_the_label_and_sets_modified() {
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        push_item(&mut app, "login", "Old", &[("password", "pw", true)]);
        key_ctrl(&mut app, KeyCode::Char('e')); // edit highlighted
        assert!(app.form.is_some());
        // field 0 is the label, prefilled "Old" — clear it and retype
        for _ in 0..3 {
            key(&mut app, KeyCode::Backspace);
        }
        typed(&mut app, "New");
        submit_now(&mut app);
        let v = app.vault.as_ref().unwrap();
        assert_eq!(v.items()[0].label, "New");
        assert!(v.items()[0].modified_at.is_some(), "modified_at stamped");
        // the password field round-tripped
        assert_eq!(v.items()[0].fields.iter().find(|f| f.key == "password").unwrap().value.as_str(), "pw");
    }

    #[test]
    fn edit_secret_blank_keeps_typed_replaces() {
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        push_item(&mut app, "login", "Site", &[("password", "oldpw", true)]);
        let pw = |app: &App| {
            app.vault.as_ref().unwrap().items()[0]
                .fields
                .iter()
                .find(|f| f.key == "password")
                .unwrap()
                .value
                .as_str()
                .to_string()
        };
        // leave the (blank) secret field untouched → the old secret is kept
        key_ctrl(&mut app, KeyCode::Char('e'));
        submit_now(&mut app);
        assert_eq!(pw(&app), "oldpw", "a blank secret keeps the stored value");
        // type into the secret field → it replaces (does not append)
        key_ctrl(&mut app, KeyCode::Char('e'));
        key(&mut app, KeyCode::Down); // label → password
        typed(&mut app, "newpw");
        submit_now(&mut app);
        assert_eq!(pw(&app), "newpw", "typing replaces the secret, never appends");
    }

    #[test]
    fn search_command_filters_the_list() {
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        push_item(&mut app, "login", "Amazon", &[]);
        push_item(&mut app, "login", "GitHub", &[]);
        typed(&mut app, "search git");
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.vault_query.as_str(), "git");
        assert_eq!(app.left_len(), 1, "filtered to GitHub");
    }

    #[test]
    fn category_creates_a_custom_category() {
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        typed(&mut app, "category MyDB");
        key(&mut app, KeyCode::Enter); // opens the category form (name prefilled)
        assert!(app.form.is_some());
        // the growing `+ add field` list: type into the adder and a fresh
        // adder row appears below it — no fixed slots
        key(&mut app, KeyCode::Down); // -> the first add-field row
        typed(&mut app, "server");
        assert_eq!(app.form.as_ref().unwrap().fields.len(), 3, "a fresh add-field row grew below");
        key(&mut app, KeyCode::Down); // -> the fresh adder
        typed(&mut app, "port");
        assert_eq!(app.form.as_ref().unwrap().fields.len(), 4, "and it keeps growing");
        submit_now(&mut app);
        let v = app.vault.as_ref().unwrap();
        assert_eq!(v.content.categories.len(), 1);
        assert_eq!(v.content.categories[0].id, "mydb");
        // website is always there (it feeds the favicon), then the typed fields
        assert_eq!(v.content.categories[0].fields[0].key, "website");
        assert_eq!(v.content.categories[0].fields[1].key, "server");
        assert_eq!(v.content.categories[0].fields[2].key, "port");
    }

    #[test]
    fn enc_info_shows_metadata_but_no_field_values() {
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        push_item(&mut app, "login", "Bank", &[("password", "TOPSECRETPW", true)]);
        typed(&mut app, "encInfo");
        key(&mut app, KeyCode::Enter);
        assert!(matches!(app.active_view(), PanelView::EncInfo(None)));
        let text = render_sized(&app, 100, 40);
        assert!(text.contains("Argon2id"), "shows the KDF");
        assert!(!text.contains("TOPSECRETPW"), "never a field value");
    }

    #[test]
    fn nuke_wipes_items_but_keeps_categories() {
        use crate::model::{CategoryDef, FieldType};
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        app.vault.as_mut().unwrap().content.categories.push(CategoryDef {
            id: "streaming".into(),
            name: "Streaming".into(),
            fields: vec![],
            created_at: None,
            extra: serde_json::Map::new(),
        });
        let _ = FieldType::Text;
        push_item(&mut app, "login", "A", &[]);
        push_item(&mut app, "login", "B", &[]);
        typed(&mut app, "nuke");
        key(&mut app, KeyCode::Enter); // open nuke form (password, name, confirm)
        assert!(app.form.is_some());
        typed(&mut app, "pw"); // password field
        key(&mut app, KeyCode::Tab); // -> vault_name
        typed(&mut app, "bruno");
        key(&mut app, KeyCode::Tab); // -> confirm
        key(&mut app, KeyCode::Right); // Yes
        key(&mut app, KeyCode::Enter); // submit
        let v = app.vault.as_ref().unwrap();
        assert_eq!(v.items().len(), 0, "items wiped");
        assert_eq!(v.content.categories.len(), 1, "custom category kept");
    }

    #[test]
    fn vault_list_with_icons_never_panics_at_any_size() {
        // the two-line rows + the hand-drawn icon tile use manual cell writes;
        // they must stay clipped to the panel at every size.
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        for i in 0..8 {
            push_item(&mut app, "email", &format!("Item{i}"), &[("email", "a@x.com", false)]);
        }
        for (w, h) in [(1u16, 1u16), (6, 4), (21, 10), (40, 24), (80, 24), (120, 50)] {
            let _ = render_sized(&app, w, h); // must not panic
        }
    }

    #[test]
    #[ignore]
    fn gen_design_screens() {
        use ratatui::style::{Color, Modifier};
        use std::fs;
        fs::create_dir_all("design-screens/dumps").unwrap();
        const W: u16 = 110;
        const H: u16 = 34;

        fn rgb(c: Color, bg: bool) -> [u8; 3] {
            match c {
                Color::Rgb(r, g, b) => [r, g, b],
                // warm-black paper / sand text (BRASS LEDGER), if any Reset slips through
                Color::Reset => if bg { [15, 13, 10] } else { [232, 226, 211] },
                Color::Black => [30, 30, 40],
                Color::White => [235, 235, 240],
                Color::Red => [219, 68, 55],
                Color::Green => [72, 187, 120],
                Color::Yellow => [244, 200, 90],
                Color::Blue => [66, 133, 244],
                Color::Magenta => [180, 120, 220],
                Color::Cyan => [80, 200, 210],
                Color::Gray => [160, 160, 170],
                Color::DarkGray => [90, 90, 105],
                Color::LightRed => [240, 120, 110],
                Color::LightGreen => [120, 220, 140],
                Color::LightYellow => [250, 230, 140],
                Color::LightBlue => [120, 180, 250],
                Color::LightMagenta => [210, 150, 240],
                Color::LightCyan => [150, 230, 235],
                _ => if bg { [24, 24, 37] } else { [160, 160, 170] },
            }
        }
        let dump = |app: &App, name: &str| {
            let mut term = Terminal::new(TestBackend::new(W, H)).unwrap();
            term.draw(|f| draw(f, app)).unwrap();
            let buf = term.backend().buffer();
            let mut rows = Vec::new();
            for y in 0..H {
                let mut row = Vec::new();
                for x in 0..W {
                    let cell = &buf[(x, y)];
                    let fg = rgb(cell.fg, false);
                    let bg = rgb(cell.bg, true);
                    let bold = cell.modifier.contains(Modifier::BOLD) as u8;
                    row.push(serde_json::json!([
                        cell.symbol(), fg[0], fg[1], fg[2], bg[0], bg[1], bg[2], bold
                    ]));
                }
                rows.push(serde_json::Value::Array(row));
            }
            let v = serde_json::json!({ "w": W, "h": H, "rows": rows });
            fs::write(format!("design-screens/dumps/{name}.json"), v.to_string()).unwrap();
        };
        fn base(app: &mut App) {
            app.settings.show_logo = true;
            app.settings.reduce_motion = true;
            // render on the warm-black ledger paper so the screenshots reflect
            // the real designed surface (not a transparent terminal default).
            app.settings.background = crate::settings::Background::Opaque;
            // preview the dedicated-terminal experience (faint backdrop rain)
            app.settings.backdrop = crate::settings::Backdrop::On;
            app.frame = 40;
        }
        fn build_vault(app: &mut App) {
            base(app);
            open_first_vault(app);
            push_item(app, "email", "Gmail", &[("email", "bruno.galimi@gmail.com", false), ("password", "x", true), ("totp", "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ", true), ("url", "gmail.com", false)]);
            push_item(app, "account", "Spotify", &[("username", "brxezz", false), ("password", "y", true), ("url", "spotify.com", false)]);
            push_item(app, "login", "GitHub", &[("username", "brxezz", false), ("url", "github.com", false)]);
            push_item(app, "credit_card", "Visa Intesa", &[("cardholder", "Bruno Galimi", false), ("number", "4111111111111111", true)]);
        }

        { let (mut a, _d) = app_with(&["personal", "work", "email-perso"]); base(&mut a); a.splash_seed = 2; dump(&a, "01-access-vaults"); }
        { let (mut a, _d) = app_with(&[]); base(&mut a); a.splash_seed = 0; dump(&a, "02-access-empty"); }
        { let (mut a, _d) = app_with(&["personal"]); base(&mut a); key(&mut a, KeyCode::Enter); for ch in "wrongpw".chars() { key(&mut a, KeyCode::Char(ch)); } key(&mut a, KeyCode::Enter); for ch in "pass".chars() { key(&mut a, KeyCode::Char(ch)); } dump(&a, "03-unlock-form"); }
        { let (mut a, _d) = app_with(&["personal"]); base(&mut a); typed(&mut a, "createvault"); key(&mut a, KeyCode::Enter); dump(&a, "04-create-vault"); }
        { let (mut a, _d) = app_with(&["personal", "work"]); base(&mut a); open_help(&mut a); dump(&a, "05-palette-access"); }
        { let (mut a, _d) = app_with(&["personal"]); base(&mut a); typed(&mut a, "settings"); key(&mut a, KeyCode::Enter); dump(&a, "06-settings"); }
        { let (mut a, _d) = app_with(&["personal"]); build_vault(&mut a); key_ctrl(&mut a, KeyCode::Char('l')); for ch in "pas".chars() { key(&mut a, KeyCode::Char(ch)); } dump(&a, "07-lock-screen"); }
        { let (mut a, _d) = app_with(&["personal"]); build_vault(&mut a); key(&mut a, KeyCode::Char('/')); for ch in "g".chars() { key(&mut a, KeyCode::Char(ch)); } dump(&a, "08-filter-state"); }
        { let (mut a, _d) = app_with(&["personal"]); build_vault(&mut a); key_ctrl(&mut a, KeyCode::Char('s')); a.splash_seed = 2; a.status = None; dump(&a, "09-vault-list"); }
        { let (mut a, _d) = app_with(&["personal"]); build_vault(&mut a); key(&mut a, KeyCode::Char('r')); dump(&a, "10-item-detail"); }
        { let (mut a, _d) = app_with(&["personal"]); build_vault(&mut a); typed(&mut a, "add"); key(&mut a, KeyCode::Enter); dump(&a, "11-add-category-picker"); }
        { let (mut a, _d) = app_with(&["personal"]); build_vault(&mut a); typed(&mut a, "add"); key(&mut a, KeyCode::Enter); key(&mut a, KeyCode::Enter); dump(&a, "12-add-form"); }
        { let (mut a, _d) = app_with(&["personal"]); build_vault(&mut a); key_ctrl(&mut a, KeyCode::Char('e')); dump(&a, "13-edit-form"); }
        { let (mut a, _d) = app_with(&["personal"]); build_vault(&mut a); key(&mut a, KeyCode::Delete); dump(&a, "14-delete-form"); }
        { let (mut a, _d) = app_with(&["personal"]); build_vault(&mut a); typed(&mut a, "category Database"); key(&mut a, KeyCode::Enter); dump(&a, "15-category-form"); }
        { let (mut a, _d) = app_with(&["personal"]); build_vault(&mut a); typed(&mut a, "encInfo"); key(&mut a, KeyCode::Enter); dump(&a, "16-encinfo"); }
        { let (mut a, _d) = app_with(&["personal"]); build_vault(&mut a); typed(&mut a, "rerollEnc"); key(&mut a, KeyCode::Enter); dump(&a, "17-reroll-form"); }
        { let (mut a, _d) = app_with(&["personal"]); build_vault(&mut a); typed(&mut a, "nuke"); key(&mut a, KeyCode::Enter); dump(&a, "18-nuke-form"); }
        { let (mut a, _d) = app_with(&["personal"]); build_vault(&mut a); open_help(&mut a); dump(&a, "19-vault-palette"); }
        { let (mut a, _d) = app_with(&["personal"]); build_vault(&mut a); key(&mut a, KeyCode::Char(':')); typed(&mut a, "re"); dump(&a, "20-palette"); }
        { let (mut a, _d) = app_with(&["personal"]); build_vault(&mut a); key(&mut a, KeyCode::Char('?')); dump(&a, "21-cheatsheet"); }
        { let (mut a, _d) = app_with(&["personal"]); build_vault(&mut a); a.settings.backdrop_fx = crate::settings::BackdropFx::Waves; a.settings.backdrop = crate::settings::Backdrop::On; a.settings.reduce_motion = false; a.frame = 40; a.status = None; dump(&a, "22-waves-backdrop"); }
        { let (mut a, _d) = app_with(&["personal"]); build_vault(&mut a); a.settings.backdrop_fx = crate::settings::BackdropFx::Waves; a.settings.backdrop = crate::settings::Backdrop::On; a.settings.reduce_motion = false; a.frame = 90; a.status = None; dump(&a, "22b-waves-frame90"); }
    }

    #[test]
    fn arrows_move_vaults_when_left_focused() {
        let (mut app, _d) = app_with(&["bruno", "alice", "lavoro"]);
        assert_eq!(app.effective_focus(), Focus::Vaults);
        key(&mut app, KeyCode::Down);
        assert_eq!(app.selected, 1);
        assert_eq!(app.panel_selected, 0, "panel must not move while left focused");
    }

    #[test]
    fn right_focuses_panel_and_arrows_move_commands() {
        // arrows move the open palette's selection, never the vault list
        let (mut app, _d) = app_with(&["bruno"]);
        open_help(&mut app);
        key(&mut app, KeyCode::Down);
        assert_eq!(app.palette.as_ref().unwrap().selected, 1);
        assert_eq!(app.selected, 0, "vault must not move while the palette is open");
    }

    #[test]
    fn selecting_language_in_palette_opens_language_view() {
        let (mut app, _d) = app_with(&["bruno"]);
        open_help(&mut app); // focus the palette
        // palette order: help, commands, createVault, deleteVault, nukeVault, language, …
        for _ in 0..5 {
            key(&mut app, KeyCode::Down);
        }
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.view, PanelView::Picker(PickerKind::Language));
        assert!(render(&app).contains("Italiano"));
    }

    #[test]
    fn commands_are_case_insensitive() {
        let (mut app, _d) = app_with(&["bruno"]);
        typed(&mut app, "LaNgUaGe It");
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.effective_lang(), Lang::It);
        assert!(app.status.as_ref().unwrap().1.contains("Italiano"));
    }

    #[test]
    fn commands_command_opens_the_palette() {
        let (mut app, _d) = app_with(&["bruno"]);
        typed(&mut app, "language");
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.view, PanelView::Picker(PickerKind::Language));
        typed(&mut app, "commands");
        key(&mut app, KeyCode::Enter);
        assert!(app.palette.is_some(), "`commands` opens the palette — it IS the command surface");
    }

    #[test]
    fn panel_history_back_and_forward() {
        let (mut app, _d) = app_with(&["bruno"]);
        assert_eq!(app.view, PanelView::Splash);

        typed(&mut app, "language");
        key(&mut app, KeyCode::Enter); // Splash -> Picker(Language)
        assert_eq!(app.view, PanelView::Picker(PickerKind::Language));

        typed(&mut app, "~ back");
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.view, PanelView::Splash);

        typed(&mut app, "~ forw");
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.view, PanelView::Picker(PickerKind::Language));

        // no more forward
        typed(&mut app, "~ forw");
        key(&mut app, KeyCode::Enter);
        assert!(app.status.as_ref().unwrap().1.contains("no next view"));
    }

    #[test]
    fn changing_main_page_resets_panel_history() {
        let (mut app, _d) = app_with(&["bruno"]);
        typed(&mut app, "language");
        key(&mut app, KeyCode::Enter); // back = [Splash]
        app.set_page(Page::Vault);
        assert_eq!(app.view, PanelView::Splash);
        typed(&mut app, "~ back");
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.view, PanelView::Splash); // history was cleared
        assert!(app.status.as_ref().unwrap().1.contains("no previous view"));
    }

    #[test]
    fn escape_steps_back_then_quits() {
        let (mut app, _d) = app_with(&["bruno"]);
        typed(&mut app, "language");
        key(&mut app, KeyCode::Enter); // view = Language, focus = Panel
        // Esc leaves the picker for the idle splash and drops focus to the list
        // (the splash has nothing to select), then Esc at the top quits.
        assert!(!app.on_key(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.view, PanelView::Splash);
        assert_eq!(app.focus, Focus::Vaults);
        assert!(app.on_key(KeyCode::Esc, KeyModifiers::NONE)); // quit
    }

    #[test]
    fn createvault_form_creates_a_vault() {
        let (mut app, _d) = app_with(&[]);
        assert!(app.vaults.is_empty());

        // open the form, fill both fields, submit
        typed(&mut app, "createvault");
        key(&mut app, KeyCode::Enter); // enters form mode
        assert!(app.form.is_some());
        typed(&mut app, "myvault"); // vault_name (first field)
        key(&mut app, KeyCode::Tab); // -> vault_password
        typed(&mut app, "s3cret");
        submit_now(&mut app);

        assert!(app.form.is_none(), "form closed after create");
        assert_eq!(app.vaults, vec!["myvault".to_string()]);
        assert!(app.status.as_ref().unwrap().1.contains("created"));

        // the vault really opens with that password
        assert!(crate::vault::Vault::open(&app.paths.vaults_dir, "myvault", "s3cret").is_ok());
        assert!(crate::vault::Vault::open(&app.paths.vaults_dir, "myvault", "wrong").is_err());
    }

    #[test]
    fn createvault_form_rejects_empty_fields_and_cancels() {
        let (mut app, _d) = app_with(&[]);
        typed(&mut app, "createvault");
        key(&mut app, KeyCode::Enter);
        // submit with empty fields -> error, no vault, still in form
        submit_now(&mut app);
        assert!(app.form.as_ref().unwrap().error.is_some());
        assert!(app.vaults.is_empty());
        // Esc cancels
        key(&mut app, KeyCode::Esc);
        assert!(app.form.is_none());
        assert!(app.status.as_ref().unwrap().1.contains("cancel"));
    }

    #[test]
    fn ctrl_and_alt_chords_are_not_typed_into_fields() {
        let (mut app, _d) = app_with(&[]);
        typed(&mut app, "createvault");
        key(&mut app, KeyCode::Enter);
        // modifier chords must be ignored, not injected as bare letters
        app.on_key(KeyCode::Char('u'), KeyModifiers::CONTROL);
        app.on_key(KeyCode::Char('x'), KeyModifiers::ALT);
        typed(&mut app, "ok");
        assert_eq!(app.form.as_ref().unwrap().fields[0].value.as_str(), "ok");
    }

    #[test]
    fn backspace_clears_the_form_error() {
        let (mut app, _d) = app_with(&[]);
        typed(&mut app, "createvault");
        key(&mut app, KeyCode::Enter);
        submit_now(&mut app); // submit empty → error
        assert!(app.form.as_ref().unwrap().error.is_some());
        typed(&mut app, "x");
        key(&mut app, KeyCode::Backspace);
        assert!(app.form.as_ref().unwrap().error.is_none());
    }

    #[test]
    fn duplicate_vault_shows_a_friendly_error_and_creates_nothing() {
        let (mut app, _d) = app_with(&["exists"]);
        typed(&mut app, "createvault");
        key(&mut app, KeyCode::Enter);
        typed(&mut app, "exists");
        key(&mut app, KeyCode::Tab);
        typed(&mut app, "pw");
        submit_now(&mut app); // submit → duplicate (fast: checked before KDF)
        assert!(app
            .form
            .as_ref()
            .unwrap()
            .error
            .as_deref()
            .unwrap()
            .contains("already exists"));
        assert_eq!(app.vaults.len(), 1);
    }

    #[test]
    fn unlock_enters_vault_page_and_exit_returns() {
        // app_with creates vaults with a fast KDF and password "pw"
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app); // inline unlock (fig-01), no modal
        assert!(app.unlock.is_none());
        assert_eq!(app.page, Page::Vault);
        assert!(app.vault.is_some());

        typed(&mut app, "exit");
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.page, Page::Access);
        assert!(app.vault.is_none());
    }

    #[test]
    fn deletevault_and_nukevault_verify_password_then_act() {
        let (mut app, _d) = app_with(&["bruno", "alice"]);

        // wrong password → error, nothing removed
        typed(&mut app, "deletevault bruno");
        key(&mut app, KeyCode::Enter); // manage form, focus on password
        typed(&mut app, "nope");
        key(&mut app, KeyCode::Enter);
        assert!(app.form.as_ref().unwrap().error.is_some());
        assert_eq!(app.vaults.len(), 2);

        // correct password → the vault file is removed
        let f = app.form.as_mut().unwrap();
        f.fields[1].value.set("pw");
        key(&mut app, KeyCode::Enter);
        assert!(app.form.is_none());
        assert_eq!(app.vaults, vec!["alice".to_string()]);
        assert!(app.status.as_ref().unwrap().1.contains("deleted"));

        // nuke the remaining vault: it stays but is emptied
        typed(&mut app, "nukevault alice");
        key(&mut app, KeyCode::Enter);
        typed(&mut app, "pw");
        key(&mut app, KeyCode::Enter);
        assert!(app.form.is_none());
        assert_eq!(app.vaults, vec!["alice".to_string()]); // still there
        assert!(app.status.as_ref().unwrap().1.contains("emptied"));
    }

    #[test]
    fn unlock_wrong_password_shows_error_and_stays_on_access() {
        let (mut app, _d) = app_with(&["bruno"]);
        key(&mut app, KeyCode::Enter); // unfold the inline unlock
        for ch in "wrong".chars() {
            key(&mut app, KeyCode::Char(ch));
        }
        key(&mut app, KeyCode::Enter);
        let u = app.unlock.as_ref().expect("prompt stays open");
        assert!(u.error, "the ✗ attempt line shows");
        assert_eq!(u.attempts, 1);
        assert!(u.input.is_empty(), "the failed passphrase is wiped");
        assert_eq!(app.page, Page::Access);
        assert!(app.vault.is_none());
        // the attempt counter renders (fig-01)
        assert!(render(&app).contains("attempt 1 of 5"));
    }

    #[test]
    fn esc_at_vault_top_closes_the_vault_not_the_app() {
        let (mut app, _d) = app_with(&["bruno"]);
        key(&mut app, KeyCode::Enter);
        typed(&mut app, "pw");
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.page, Page::Vault);
        assert!(!app.on_key(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.page, Page::Access);
        assert!(app.vault.is_none());
    }

    #[test]
    fn form_is_a_modal_layer_arrows_never_reach_the_list() {
        // a floating form owns every key: ←/→/↑/↓ must never move the list
        // beneath the dimmed screen (they cycle choices / move fields only).
        let (mut app, _d) = app_with(&["a", "b", "c"]);
        key(&mut app, KeyCode::Down); // list selection -> 1
        assert_eq!(app.selected, 1);
        typed(&mut app, "createvault");
        key(&mut app, KeyCode::Enter); // form open
        assert!(app.form.is_some());
        key(&mut app, KeyCode::Left);
        key(&mut app, KeyCode::Right);
        assert!(app.form.is_some(), "the form stays open");
        assert_eq!(app.selected, 1, "the list under the modal never moves");
        typed(&mut app, "abc");
        assert_eq!(app.form.as_ref().unwrap().fields[0].value.as_str(), "abc");
    }

    #[test]
    fn enter_on_left_with_open_form_does_not_clobber_it() {
        let (mut app, _d) = app_with(&["bruno"]);
        typed(&mut app, "createvault");
        key(&mut app, KeyCode::Enter); // createVault form open
        typed(&mut app, "typed-name");
        key(&mut app, KeyCode::Left); // step to the vault list
        key(&mut app, KeyCode::Enter); // must NOT replace the form with unlock
        assert!(matches!(
            app.form.as_ref().unwrap().purpose,
            crate::app::FormPurpose::CreateVault
        ));
        assert_eq!(app.form.as_ref().unwrap().fields[0].value.as_str(), "typed-name");
        assert_eq!(app.effective_focus(), Focus::Panel); // returned into the form
    }

    #[test]
    fn createvault_has_encryption_choices_and_uses_the_selected_one() {
        let (mut app, _d) = app_with(&[]);
        typed(&mut app, "createvault");
        key(&mut app, KeyCode::Enter);
        {
            let form = app.form.as_ref().unwrap();
            assert_eq!(form.fields.len(), 4);
            assert!(form.fields[2].is_choice() && form.fields[3].is_choice());
            assert_eq!(form.fields[2].current(), "AES-256-GCM"); // default AEAD
            assert_eq!(form.fields[3].current(), "Argon2id"); // default KDF
        }
        typed(&mut app, "v"); // name field (focus starts here)
        key(&mut app, KeyCode::Tab); // → password
        typed(&mut app, "pw");
        key(&mut app, KeyCode::Tab); // → algorithm choice
        key(&mut app, KeyCode::Right); // cycle to the next AEAD
        let chosen = app.form.as_ref().unwrap().fields[2].current().to_string();
        assert_ne!(chosen, "AES-256-GCM");
        submit_now(&mut app);

        assert_eq!(app.vaults, vec!["v".to_string()]);
        let v = crate::vault::Vault::open(&app.paths.vaults_dir, "v", "pw").unwrap();
        assert_eq!(v.algorithm.name(), chosen);
    }

    // ---- export / import -------------------------------------------------

    /// Opens the export form for `name` and points it at `dest`, then submits.
    fn export_to(app: &mut App, name: &str, dest: &std::path::Path) {
        typed(app, &format!("exportvault {name}"));
        key(app, KeyCode::Enter); // opens the form
        app.form.as_mut().unwrap().fields[1].value = dest.to_string_lossy().into_owned().into();
        key(app, KeyCode::Enter); // submit
    }

    #[test]
    fn export_then_import_roundtrips() {
        let (mut app, vdir) = app_with(&["bruno"]);
        let ext = TempDir::new().unwrap();
        let dest = ext.path().join("bruno-backup.vault");

        export_to(&mut app, "bruno", &dest);
        assert!(app.form.is_none());
        assert!(dest.is_file(), "export wrote the file");
        assert!(app.status.as_ref().unwrap().1.contains("exported"));

        // remove the original, then import the backup back in
        crate::vault::delete_vault_file(vdir.path(), "bruno").unwrap();
        app.reload_vaults();
        assert!(app.vaults.is_empty());

        typed(&mut app, "importvault");
        key(&mut app, KeyCode::Enter); // opens the import form
        {
            let f = app.form.as_mut().unwrap();
            f.fields[0].value = dest.to_string_lossy().into_owned().into();
            f.fields[1].value = "pw".to_string().into();
        }
        submit_now(&mut app);
        assert!(app.form.is_none());
        assert_eq!(app.vaults, vec!["bruno".to_string()]);
        assert!(app.status.as_ref().unwrap().1.contains("imported"));
        assert!(Vault::open(vdir.path(), "bruno", "pw").is_ok());
    }

    #[test]
    fn import_wrong_password_is_rejected() {
        let (mut app, vdir) = app_with(&["bruno"]);
        let ext = TempDir::new().unwrap();
        let dest = ext.path().join("b.vault");
        export_to(&mut app, "bruno", &dest);
        crate::vault::delete_vault_file(vdir.path(), "bruno").unwrap();
        app.reload_vaults();

        typed(&mut app, "importvault");
        key(&mut app, KeyCode::Enter);
        {
            let f = app.form.as_mut().unwrap();
            f.fields[0].value = dest.to_string_lossy().into_owned().into();
            f.fields[1].value = "nope".to_string().into();
        }
        submit_now(&mut app);
        assert!(app.form.as_ref().unwrap().error.is_some(), "wrong password → error");
        assert!(app.vaults.is_empty(), "nothing imported");
    }

    #[test]
    fn import_missing_file_reports_not_found() {
        let (mut app, _d) = app_with(&[]);
        typed(&mut app, "importvault");
        key(&mut app, KeyCode::Enter);
        {
            let f = app.form.as_mut().unwrap();
            f.fields[0].value = "/definitely/not/here.vault".to_string().into();
            f.fields[1].value = "x".to_string().into();
        }
        submit_now(&mut app);
        let err = app.form.as_ref().unwrap().error.clone().unwrap();
        assert!(err.contains("no file"), "got: {err}");
    }

    #[test]
    fn import_does_not_overwrite_an_existing_vault() {
        let (mut app, _vdir) = app_with(&["bruno"]);
        let ext = TempDir::new().unwrap();
        let dest = ext.path().join("b.vault");
        export_to(&mut app, "bruno", &dest);
        // bruno still exists — importing the backup must refuse, not clobber
        typed(&mut app, "importvault");
        key(&mut app, KeyCode::Enter);
        {
            let f = app.form.as_mut().unwrap();
            f.fields[0].value = dest.to_string_lossy().into_owned().into();
            f.fields[1].value = "pw".to_string().into();
        }
        submit_now(&mut app);
        assert!(app.form.as_ref().unwrap().error.is_some(), "must refuse to overwrite");
        assert_eq!(app.vaults, vec!["bruno".to_string()]);
    }

    #[test]
    fn export_form_prefills_a_real_looking_path() {
        let (mut app, _d) = app_with(&["bruno"]);
        typed(&mut app, "exportvault bruno");
        key(&mut app, KeyCode::Enter);
        let dest = app.form.as_ref().unwrap().fields[1].value.as_str().to_string();
        assert!(dest.ends_with(".vault"), "prefilled path was {dest}");
        assert!(dest.contains("bruno"), "filename should include the vault name: {dest}");
    }

    #[test]
    fn export_refuses_to_overwrite_an_existing_file() {
        let (mut app, _d) = app_with(&["bruno"]);
        let ext = TempDir::new().unwrap();
        let dest = ext.path().join("taken.vault");
        std::fs::write(&dest, b"precious").unwrap();

        typed(&mut app, "exportvault bruno");
        key(&mut app, KeyCode::Enter);
        app.form.as_mut().unwrap().fields[1].value = dest.to_string_lossy().into_owned().into();
        key(&mut app, KeyCode::Enter);
        assert!(app.form.as_ref().unwrap().error.is_some(), "must refuse to clobber");
        // the pre-existing file is untouched
        assert_eq!(std::fs::read(&dest).unwrap(), b"precious");
    }

    #[test]
    fn export_destination_follows_the_cycled_vault_until_edited() {
        // vaults sort to [alice, bruno]; the form opens on alice
        let (mut app, _d) = app_with(&["alice", "bruno"]);
        typed(&mut app, "exportvault");
        key(&mut app, KeyCode::Enter);
        assert!(app.form.as_ref().unwrap().fields[1].value.contains("alice"));

        // move to the vault chooser and cycle to bruno → default path updates
        key(&mut app, KeyCode::Up); // focus the chooser (field 0)
        key(&mut app, KeyCode::Right); // alice → bruno
        assert!(
            app.form.as_ref().unwrap().fields[1].value.contains("bruno"),
            "untouched default should follow the chosen vault"
        );
    }

    #[test]
    fn export_keeps_a_hand_edited_destination() {
        let (mut app, _d) = app_with(&["alice", "bruno"]);
        typed(&mut app, "exportvault");
        key(&mut app, KeyCode::Enter);
        // edit the destination (marks it dirty), then cycle the vault
        typed(&mut app, "X"); // focus starts on the path field
        key(&mut app, KeyCode::Up);
        key(&mut app, KeyCode::Right); // alice → bruno
        let dest = app.form.as_ref().unwrap().fields[1].value.as_str().to_string();
        assert!(dest.contains("alice") && !dest.contains("bruno"), "edited path must be kept: {dest}");
    }

    #[test]
    fn import_collision_names_the_vault() {
        let (mut app, _vdir) = app_with(&["bruno"]);
        let ext = TempDir::new().unwrap();
        let dest = ext.path().join("b.vault");
        export_to(&mut app, "bruno", &dest);
        // bruno still exists — importing its backup should name the collision
        typed(&mut app, "importvault");
        key(&mut app, KeyCode::Enter);
        {
            let f = app.form.as_mut().unwrap();
            f.fields[0].value = dest.to_string_lossy().into_owned().into();
            f.fields[1].value = "pw".to_string().into();
        }
        submit_now(&mut app);
        let err = app.form.as_ref().unwrap().error.clone().unwrap();
        assert!(err.contains("bruno"), "collision message should name the vault: {err}");
    }

    // ---- settings page ---------------------------------------------------

    use crate::settings::Background;
    use crate::theme::Theme;

    fn open_settings(app: &mut App) {
        typed(app, "settings");
        key(app, KeyCode::Enter);
    }

    #[test]
    fn settings_opens_as_one_list_with_section_rules() {
        // fig-09: no tabs — one scrolling list under GENERAL / TUI / SECURITY rules
        let (mut app, _d) = app_with(&["bruno", "alice"]);
        open_settings(&mut app);
        assert_eq!(app.settings_ui.as_ref().unwrap().selected, 0);
        let text = render_sized(&app, 100, 40);
        assert!(text.contains("GENERAL") && text.contains("TUI") && text.contains("SECURITY"));
        // ↑↓ move through the flat list (clamped at the ends)
        key(&mut app, KeyCode::Down);
        assert_eq!(app.settings_ui.as_ref().unwrap().selected, 1);
        key(&mut app, KeyCode::Up);
        key(&mut app, KeyCode::Up);
        assert_eq!(app.settings_ui.as_ref().unwrap().selected, 0, "clamped at the top");
    }

    #[test]
    fn settings_intensity_slider_steps_and_clamps() {
        use crate::settings::BackdropLevel;
        let (mut app, _d) = app_with(&["personal"]);
        open_settings(&mut app);
        // walk down to the `intensity` row (index 7 in ALL_ROWS)
        let idx = settings_ui::ALL_ROWS
            .iter()
            .position(|(_, r)| *r == settings_ui::Row::BackdropLevel)
            .unwrap();
        for _ in 0..idx {
            key(&mut app, KeyCode::Down);
        }
        assert_eq!(app.settings_ui.as_ref().unwrap().focused_row(), Some(settings_ui::Row::BackdropLevel));
        let level = |a: &App| a.settings_ui.as_ref().unwrap().draft.backdrop_level;
        assert_eq!(level(&app), BackdropLevel::Soft, "default is Soft");
        // → steps up and clamps at Full (never wraps to Faint)
        key(&mut app, KeyCode::Right);
        assert_eq!(level(&app), BackdropLevel::Medium);
        for _ in 0..5 {
            key(&mut app, KeyCode::Right);
        }
        assert_eq!(level(&app), BackdropLevel::Full, "clamps at the strong end");
        // ← steps down and clamps at Faint
        for _ in 0..10 {
            key(&mut app, KeyCode::Left);
        }
        assert_eq!(level(&app), BackdropLevel::Faint, "clamps at the subtle end");
        // the factor is strictly increasing across the steps
        let f: Vec<f32> = BackdropLevel::STEPS.iter().map(|l| l.factor()).collect();
        assert!(f.windows(2).all(|w| w[0] < w[1]), "intensity factor must increase monotonically");
    }

    #[test]
    fn settings_cycles_background_and_persists() {
        let (mut app, _d) = app_with(&[]);
        assert!(!app.background_opaque());
        open_settings(&mut app);
        key(&mut app, KeyCode::Down); // vaults
        key(&mut app, KeyCode::Down); // theme
        key(&mut app, KeyCode::Down); // background
        // live preview: the draft flips before saving
        key(&mut app, KeyCode::Right); // transparent -> opaque
        assert!(app.background_opaque(), "opaque previews live off the draft");
        key(&mut app, KeyCode::Enter); // save

        assert!(app.settings_ui.is_none());
        assert_eq!(app.settings.background, Background::Opaque);
        assert!(app.background_opaque());
        // and it is written to disk
        let saved = crate::settings::Settings::load(&app.paths.settings_config);
        assert_eq!(saved.background, Background::Opaque);
    }

    #[test]
    fn settings_language_change_applies_and_persists() {
        let (mut app, _d) = app_with(&[]);
        assert_eq!(app.effective_lang(), Lang::En);
        open_settings(&mut app);
        // language is the first row, already focused
        key(&mut app, KeyCode::Right); // cycle En -> It
        key(&mut app, KeyCode::Enter); // save
        assert_eq!(app.effective_lang(), Lang::It);
        let saved = crate::settings::Settings::load(&app.paths.settings_config);
        assert_eq!(saved.lang, Lang::It);
    }

    #[test]
    fn settings_esc_cancels_and_discards_the_draft() {
        let (mut app, _d) = app_with(&[]);
        assert_eq!(app.settings.theme, Theme::Brass);
        open_settings(&mut app);
        key(&mut app, KeyCode::Down); // vaults
        key(&mut app, KeyCode::Down); // theme row
        key(&mut app, KeyCode::Right); // cycle theme in the draft
        assert_ne!(app.settings_ui.as_ref().unwrap().draft.theme, Theme::Brass);
        key(&mut app, KeyCode::Esc); // cancel
        assert!(app.settings_ui.is_none());
        assert_eq!(app.settings.theme, Theme::Brass, "cancel discards the draft");
    }

    // ---- the RAG section of the settings page ----------------------------

    /// Moves the settings cursor onto `row`.
    fn focus_row(app: &mut App, row: settings_ui::Row) {
        let want = settings_ui::ALL_ROWS
            .iter()
            .position(|(_, r)| *r == row)
            .expect("the row is in ALL_ROWS");
        let at = app.settings_ui.as_ref().expect("settings are open").selected;
        for _ in 0..at {
            key(app, KeyCode::Up);
        }
        for _ in 0..want {
            key(app, KeyCode::Down);
        }
        assert_eq!(app.settings_ui.as_ref().unwrap().focused_row(), Some(row));
    }

    #[test]
    fn the_rag_rows_cycle_and_persist() {
        use crate::settings::API_PORTS;
        let (mut app, _d) = app_with(&["bruno"]);
        // saving with an index ON starts the one-time model fetch, and the real
        // engine would answer that by pulling ~500 MB off the network into the
        // temp dir — on a worker that outlives the test. The mock is the engine
        // here for the same reason it is everywhere else: no model, no network.
        app.rag.use_mock();
        // out of the box everything here is off: indexing and serving are both
        // opt-in, and neither happens because a model happens to be cached.
        assert!(!app.settings.rag_memo && !app.settings.rag_items);
        assert!(!app.settings.api_enabled());
        assert_eq!(app.settings.api_port, API_PORTS[0]);

        open_settings(&mut app);
        focus_row(&mut app, settings_ui::Row::RagMemo);
        key(&mut app, KeyCode::Right);
        focus_row(&mut app, settings_ui::Row::RagItems);
        key(&mut app, KeyCode::Right);
        // the port cycles the presets, wrapping
        focus_row(&mut app, settings_ui::Row::ApiPort);
        for expected in [API_PORTS[1], API_PORTS[2], API_PORTS[3], API_PORTS[0]] {
            key(&mut app, KeyCode::Right);
            assert_eq!(app.settings_ui.as_ref().unwrap().draft.api_port, expected);
        }
        key(&mut app, KeyCode::Left);
        assert_eq!(app.settings_ui.as_ref().unwrap().draft.api_port, API_PORTS[3], "…both ways");
        key(&mut app, KeyCode::Enter); // save

        assert!(app.settings_ui.is_none());
        assert!(app.settings.rag_memo && app.settings.rag_items);
        assert_eq!(app.settings.api_port, API_PORTS[3]);
        // …and it is on the disk, where a hand edit can reach it
        let saved = crate::settings::Settings::load(&app.paths.settings_config);
        assert!(saved.rag_memo && saved.rag_items);
        assert_eq!(saved.api_port, API_PORTS[3]);
        assert!(!saved.api_expose_memo, "serving is a separate decision");
    }

    #[test]
    fn an_exposure_row_refuses_to_turn_on_without_its_index() {
        // exposing an index that was never built is a promise the server cannot
        // keep: the row is frozen at "off" and says why.
        let (mut app, _d) = app_with(&["bruno"]);
        open_settings(&mut app);
        focus_row(&mut app, settings_ui::Row::ApiExposeMemo);
        key(&mut app, KeyCode::Right);
        assert!(!app.settings_ui.as_ref().unwrap().draft.api_expose_memo, "refused");
        assert_eq!(
            app.status.as_ref().map(|(_, s)| s.clone()).unwrap_or_default(),
            i18n::t(Lang::En, &Key::SettingHintApiNeedsRag),
            "…and it says why rather than doing nothing"
        );
        let text = render_sized(&app, 100, 44);
        assert!(text.contains("RAG"), "the section rule is drawn:\n{text}");

        // turn the memo index on: now the row takes
        focus_row(&mut app, settings_ui::Row::RagMemo);
        key(&mut app, KeyCode::Right);
        focus_row(&mut app, settings_ui::Row::ApiExposeMemo);
        key(&mut app, KeyCode::Right);
        let draft = &app.settings_ui.as_ref().unwrap().draft;
        assert!(draft.api_expose_memo && draft.exposes_memo());
        assert!(!draft.exposes_items(), "the other pair is untouched");

        // …and turning the index back off takes the exposure with it, so a
        // later flip cannot re-serve an index nobody asked to serve again
        focus_row(&mut app, settings_ui::Row::RagMemo);
        key(&mut app, KeyCode::Right);
        let draft = &app.settings_ui.as_ref().unwrap().draft;
        assert!(!draft.rag_memo && !draft.api_expose_memo);
        // a `settings.toml` that says "expose without an index" is read as off
        let mut hand_edited = crate::settings::Settings::default();
        hand_edited.api_expose_memo = true;
        hand_edited.api_expose_items = true;
        assert!(!hand_edited.exposes_memo() && !hand_edited.exposes_items());
        assert!(!hand_edited.api_enabled());
    }

    #[test]
    fn the_settings_page_never_panics_at_any_size() {
        const SIZES: [(u16, u16); 5] = [(1, 1), (6, 4), (40, 24), (80, 24), (200, 60)];
        let (mut app, _d) = app_with(&["bruno"]);
        open_settings(&mut app);
        for lang in [Lang::En, Lang::It] {
            app.settings.lang = lang;
            if let Some(state) = app.settings_ui.as_mut() {
                state.draft.lang = lang;
            }
            // every row focused in turn, at every size — the RAG rows include a
            // read-only one whose value comes from a server that isn't running,
            // and the SHORTCUTS section a row for every single-key verb
            for i in 0..settings_ui::ALL_ROWS.len() {
                if let Some(state) = app.settings_ui.as_mut() {
                    state.selected = i;
                }
                for (w, h) in SIZES {
                    let _ = render_sized(&app, w, h);
                }
            }
        }
    }

    // ---- user-configurable single-key shortcuts --------------------------

    /// Writes one `shortcut_overrides` entry into the live settings — what the
    /// settings page commits, and what a hand-edited file holds.
    fn set_shortcut(app: &mut App, verb: crate::keymap::Verb, value: &str) {
        app.settings.shortcut_overrides.insert(verb.slug().to_string(), value.to_string());
    }

    /// The `?` cheatsheet of a rendered frame.
    fn cheatsheet_of(app: &mut App) -> String {
        app.cheatsheet = true;
        let text = render_sized(app, 100, 40);
        app.cheatsheet = false;
        text
    }

    /// The user's word, end to end: a verb switched off leaves the bottom bar,
    /// leaves the cheatsheet, and its key stops doing anything at all.
    #[test]
    fn a_verb_switched_off_leaves_the_bar_the_cheatsheet_and_the_keyboard() {
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        // one item, so the empty-vault line (which names the add key too) is
        // not what the assertions below are reading
        push_item(&mut app, "email", "Gmail", &[("password", "s3cr3t", true)]);
        app.status = None; // a toast hides the keybar; we want the bar itself
        assert!(keybar_of(&render_sized(&app, 120, 30)).contains(" a "), "the chip starts there");

        set_shortcut(&mut app, crate::keymap::Verb::AddItem, crate::keymap::SHORTCUT_OFF);
        let bar = keybar_of(&render_sized(&app, 120, 30));
        assert!(!bar.contains(" a "), "the `a` chip is gone from the keybar: {bar}");
        assert!(bar.contains(" c "), "…and every other verb is untouched: {bar}");
        let keys = cheatsheet_of(&mut app);
        assert!(!keys.contains("add"), "the cheatsheet dropped it too:\n{keys}");
        assert!(keys.contains("copy"), "…and kept the rest:\n{keys}");

        // and the key itself is inert: no picker, no toast — nothing at all
        key(&mut app, KeyCode::Char('a'));
        assert_eq!(app.view, PanelView::Splash, "`a` opened nothing");
        assert!(app.status.is_none(), "a switched-off key says nothing either");
    }

    /// A chosen character replaces the built-in mnemonic everywhere — and the
    /// old one stops answering.
    #[test]
    fn a_re_keyed_verb_moves_its_chip_and_its_key() {
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        push_item(&mut app, "email", "Gmail", &[("password", "s3cr3t", true)]);
        set_shortcut(&mut app, crate::keymap::Verb::DeleteItem, "x");
        app.status = None;

        let bar = keybar_of(&render_sized(&app, 120, 30));
        assert!(bar.contains(" x "), "the delete chip moved to `x`: {bar}");
        assert!(!bar.contains(" d "), "…and left `d` behind: {bar}");

        key(&mut app, KeyCode::Char('d'));
        assert!(app.pending_confirm.is_none() && app.form.is_none(), "`d` is dead now");
        key(&mut app, KeyCode::Char('x'));
        assert!(app.pending_confirm.is_some(), "`x` arms the delete confirm");
    }

    /// The settings row is the whole feature: cycle it, save, and the bar the
    /// user is looking at follows — before and after the commit.
    #[test]
    fn the_shortcut_row_previews_live_and_persists_on_enter() {
        use crate::keymap::{ShortcutChoice, Verb};
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        open_settings(&mut app);
        focus_row(&mut app, settings_ui::Row::ShortcutKey(Verb::AddItem));
        // ← from the default is "off", the far end of the cycle
        key(&mut app, KeyCode::Left);
        let draft = &app.settings_ui.as_ref().unwrap().draft;
        assert_eq!(ShortcutChoice::stored(Verb::AddItem, draft), ShortcutChoice::Off);
        // …and the page says what that costs, and the way back to it
        let note = app.status.clone().expect("switching a verb off is said out loud");
        assert!(note.1.contains(':'), "the palette is named: {}", note.1);
        // the page renders the row's new value…
        let text = render_sized(&app, 100, 44);
        assert!(text.contains("SHORTCUTS"), "the section rule is drawn:\n{text}");
        // …and ⏎ writes it to the file and to the live settings
        key(&mut app, KeyCode::Enter);
        assert!(app.settings_ui.is_none(), "the page closed");
        let saved = crate::settings::Settings::load(&app.paths.settings_config);
        assert_eq!(saved.shortcut_overrides.get("add_item").map(String::as_str), Some("off"));
        app.status = None;
        let bar = keybar_of(&render_sized(&app, 120, 30));
        assert!(!bar.contains(" a "), "the saved choice reached the keybar: {bar}");
    }

    /// A hand-edited `settings.toml` cannot brick the page: an impossible key
    /// is refused, the default stands in, and the app says so once at startup.
    #[test]
    fn a_hand_edited_collision_falls_back_to_the_default_with_a_warning() {
        use crate::keymap::Verb;
        let dir = TempDir::new().unwrap();
        let paths = paths_in(&dir);
        let mut settings = crate::settings::Settings::default();
        // `e` is already the English "edit" on this page, and `j` moves the list
        settings.shortcut_overrides.insert(Verb::AddItem.slug().into(), "e".into());
        settings.shortcut_overrides.insert(Verb::StarItem.slug().into(), "j".into());
        settings.save(&paths.settings_config).unwrap();

        let app = App::new(paths);
        let (kind, text) = app.status.clone().expect("the refusal is on the status line");
        assert_eq!(kind, StatusKind::Error);
        assert!(text.contains("add_item") && text.contains("star_item"), "it names them: {text}");
        // …and both verbs are on their defaults, so the page still works
        assert_eq!(crate::keymap::effective(Verb::AddItem, &app.settings).en, 'a');
        assert_eq!(crate::keymap::effective(Verb::StarItem, &app.settings).en, 's');
        assert_eq!(
            crate::keymap::lookup(Page::Vault, Lang::En, KeyCode::Char('e'), &app.settings),
            Some(Verb::EditItem),
            "the letter it tried to steal still belongs to edit"
        );
    }

    /// The docked detail reads the same resolution the list does: its four item
    /// verbs move with an override and disappear with an "off".
    #[test]
    fn the_detail_verbs_follow_the_overrides() {
        use crate::keymap::{Verb, SHORTCUT_OFF};
        // copy → `1`, edit → `2`, delete → `3`, star → `4`
        let (mut app, _d) = detail_focused_app();
        for (verb, ch) in [
            (Verb::CopyPassword, "1"),
            (Verb::EditItem, "2"),
            (Verb::DeleteItem, "3"),
            (Verb::StarItem, "4"),
        ] {
            set_shortcut(&mut app, verb, ch);
        }
        app.status = None;
        let bar = keybar_of(&render_sized(&app, 120, 30));
        for chip in [" 1 ", " 2 ", " 3 ", " 4 "] {
            assert!(bar.contains(chip), "the detail keybar lost '{chip}': {bar}");
        }
        key(&mut app, KeyCode::Char('c'));
        assert!(app.status.is_none(), "the old copy key is dead in the detail too");
        key(&mut app, KeyCode::Char('4'));
        assert!(app.highlighted_item().unwrap().favorite, "`4` stars the item");
        key(&mut app, KeyCode::Char('2'));
        assert!(app.form.is_some(), "`2` opens the edit form");

        // …and a verb switched off is gone from the detail bar as well
        let (mut app, _d) = detail_focused_app();
        set_shortcut(&mut app, Verb::StarItem, SHORTCUT_OFF);
        app.status = None;
        let bar = keybar_of(&render_sized(&app, 120, 30));
        assert!(!bar.contains(" s "), "the star chip is gone: {bar}");
        key(&mut app, KeyCode::Char('s'));
        assert!(!app.highlighted_item().unwrap().favorite, "…and so is the key");
    }

    /// The Memorandum is opened by a verb like any other, so it obeys the same
    /// choice.
    #[test]
    fn the_memo_verb_follows_its_override() {
        use crate::keymap::{Verb, SHORTCUT_OFF};
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        set_shortcut(&mut app, Verb::Memo, "w");
        assert_eq!(memo_key_of(&app.settings, Lang::En), 'w');
        key(&mut app, KeyCode::Char('m'));
        assert!(!app.memo_open, "the default key no longer opens the chat");
        key(&mut app, KeyCode::Char('w'));
        assert!(app.memo_open, "the chosen one does");

        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        set_shortcut(&mut app, Verb::Memo, SHORTCUT_OFF);
        key(&mut app, KeyCode::Char('m'));
        assert!(!app.memo_open, "switched off, nothing opens it from the keyboard");
        app.status = None;
        let bar = keybar_of(&render_sized(&app, 120, 30));
        assert!(!bar.contains("memo"), "and the bar doesn't promise it: {bar}");
    }

    /// The Access page's empty state NAMES the key that makes a vault, so it
    /// has to read the same resolution the keybar and the `?` card do — three
    /// things on one screen must not contradict each other.
    #[test]
    fn the_empty_access_hint_names_the_key_that_really_makes_a_vault() {
        use crate::keymap::{Verb, SHORTCUT_OFF};
        let (mut app, _d) = app_with(&[]);
        assert!(app.vaults.is_empty(), "(nothing to unlock: the empty state is on screen)");
        let text = render_sized(&app, 90, 24);
        assert!(text.contains("n creates your first"), "the built-in key:\n{text}");

        // re-keyed: the hint moves with the verb, and the old letter goes quiet
        set_shortcut(&mut app, Verb::NewVault, "v");
        let text = render_sized(&app, 90, 24);
        assert!(text.contains("v creates your first"), "the chosen key:\n{text}");
        assert!(!text.contains("n creates"), "…and the built-in one is not promised:\n{text}");

        // switched off: the palette, which no choice on that page can take away
        set_shortcut(&mut app, Verb::NewVault, SHORTCUT_OFF);
        let text = render_sized(&app, 90, 24);
        assert!(text.contains("opens the commands"), "the palette is named:\n{text}");
        assert!(!text.contains("creates your first"), "…and no dead key is:\n{text}");

        // …and it is localized, exactly like the empty-vault line inside one
        app.settings.shortcut_overrides.clear();
        app.settings.lang = Lang::It;
        let text = render_sized(&app, 90, 24);
        assert!(text.contains("nessun vault — n crea il primo"), "the italian line:\n{text}");
        for (w, h) in [(1u16, 1u16), (6, 4), (40, 24), (80, 24), (200, 60)] {
            let _ = render_sized(&app, w, h); // must not panic
        }
    }

    /// `Del`, Ctrl+S and Ctrl+E are ALIASES for three keymap verbs, not doors of
    /// their own: switching a verb off has to close them too, or "off" would
    /// take the chip off the bar and leave the action reachable anyway.
    #[test]
    fn the_chord_aliases_go_inert_with_the_verb_they_stand_for() {
        use crate::keymap::{Verb, SHORTCUT_OFF};
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        push_item(&mut app, "email", "Gmail", &[("email", "bruno@gmail.com", false)]);
        // …while the verbs are on, all three fire
        key(&mut app, KeyCode::Delete);
        assert!(app.pending_confirm.is_some(), "Del arms the delete confirm");
        app.pending_confirm = None;
        key_ctrl(&mut app, KeyCode::Char('s'));
        assert!(app.highlighted_item().unwrap().favorite, "Ctrl+S stars the row");
        key_ctrl(&mut app, KeyCode::Char('e'));
        assert!(app.form.is_some(), "Ctrl+E opens the edit form");
        key(&mut app, KeyCode::Esc);

        // …and stop the moment the user says so
        for verb in [Verb::DeleteItem, Verb::StarItem, Verb::EditItem] {
            set_shortcut(&mut app, verb, SHORTCUT_OFF);
        }
        let starred = app.highlighted_item().unwrap().favorite;
        key(&mut app, KeyCode::Delete);
        assert!(app.pending_confirm.is_none(), "Del cannot arm a switched-off delete");
        key_ctrl(&mut app, KeyCode::Char('s'));
        assert_eq!(app.highlighted_item().unwrap().favorite, starred, "Ctrl+S changed nothing");
        key_ctrl(&mut app, KeyCode::Char('e'));
        assert!(app.form.is_none(), "Ctrl+E opened nothing");
        // a re-keyed verb is still ON, so its alias keeps working
        set_shortcut(&mut app, Verb::DeleteItem, "x");
        key(&mut app, KeyCode::Delete);
        assert!(app.pending_confirm.is_some(), "Del follows the verb, not the letter");
    }

    /// The `?` card's two Tab rows are words on screen like any other: they go
    /// through i18n, and the mode row borrows the search chips' own two words
    /// rather than spelling them a second time.
    #[test]
    fn the_cheatsheet_tab_rows_are_localized() {
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        let keys = cheatsheet_of(&mut app);
        assert!(keys.contains("cycle tabs"), "the english tab row:\n{keys}");
        assert!(keys.contains("literal \u{b7} RAG"), "…and the mode row:\n{keys}");
        app.settings.lang = Lang::It;
        let keys = cheatsheet_of(&mut app);
        assert!(keys.contains("cambia scheda"), "the italian tab row:\n{keys}");
        assert!(keys.contains("letterale \u{b7} RAG"), "…and the mode row:\n{keys}");
        assert!(!keys.contains("cycle tabs"), "no english left on the card:\n{keys}");
    }

    // ---- the localhost agent API -----------------------------------------

    /// A vault with one item that holds a real secret, plus a memorandum
    /// message — the two things the API can be asked to serve. The API is on,
    /// on an OS-assigned port, and both indexes are built with the mock engine.
    fn api_app() -> (App, TempDir, std::sync::MutexGuard<'static, ()>) {
        use crate::memo::MemoMessage;
        // one loopback-binding test at a time (see `api::PORT_GUARD`)
        let guard = crate::api::port_guard();
        use crate::model::{Item, ItemField, SecretString};
        let (mut app, dir) = app_with(&["bruno"]);
        app.use_mock_rag();
        app.settings.api_expose_memo = true;
        app.settings.api_expose_items = true;
        app.settings.api_port = 0; // the OS picks a free one for the test
        open_first_vault(&mut app);
        let mut item = Item::new("login", "Acme");
        item.fields = vec![
            ItemField {
                key: "username".into(),
                ftype: crate::model::FieldType::Username,
                label: None,
                value: SecretString::new("bruno@example.com"),
                secret: false,
                extra: serde_json::Map::new(),
            },
            ItemField {
                key: "password".into(),
                ftype: crate::model::FieldType::Password,
                label: None,
                value: SecretString::new(API_SECRET),
                secret: true,
                extra: serde_json::Map::new(),
            },
        ];
        let vault = app.vault.as_mut().expect("open");
        vault.items_mut().push(item);
        vault.save().expect("saved");
        assert!(app.append_memo_message(MemoMessage::new("le chiavi sono sotto il tappeto", None)));
        app.rag_reindex();
        pump_rag(&mut app);
        app.sync_api_server();
        (app, dir, guard)
    }

    /// The one value that must never come back out of the API.
    const API_SECRET: &str = "hunter2-CORRECT-HORSE-BATTERY";

    #[test]
    fn the_agent_api_serves_the_open_vault_and_dies_with_it() {
        let (mut app, _d, _guard) = api_app();
        let (port, token) = {
            let server = app.api.as_ref().expect("the server came up on unlock");
            (server.port(), server.token())
        };
        assert_eq!(crate::api::probe(port, "GET", "/health", None, None).0, 200);

        // an item is findable by its NON-secret text…
        let (status, body) = crate::api::probe(
            port,
            "POST",
            "/search",
            Some(&token),
            Some("{\"scope\":\"items\",\"query\":\"acme\",\"mode\":\"literal\"}"),
        );
        assert_eq!(status, 200, "{body}");
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["hits"][0]["label"], "Acme");
        assert_eq!(
            json["hits"][0]["category"],
            i18n::t(Lang::En, &Key::CategoryName("login")),
            "the category rides along LOCALIZED — it is what a search would use"
        );
        // …and the memorandum by its words
        let (_, memo) = crate::api::probe(
            port,
            "POST",
            "/search",
            Some(&token),
            Some("{\"scope\":\"memo\",\"query\":\"chiavi\",\"mode\":\"literal\"}"),
        );
        assert!(serde_json::from_str::<serde_json::Value>(&memo).unwrap()["hits"][0]["ts"].is_string());

        // NOTHING that comes back carries the password — not its value, not a
        // fragment of it, whatever is asked for and however it is asked.
        for query in [
            "{\"scope\":\"items\",\"query\":\"hunter2\",\"mode\":\"literal\"}",
            "{\"scope\":\"items\",\"query\":\"password\",\"mode\":\"literal\"}",
            "{\"scope\":\"items\",\"query\":\"la password di acme\",\"mode\":\"rag\"}",
            "{\"scope\":\"memo\",\"query\":\"password\",\"mode\":\"rag\"}",
        ] {
            let (status, body) = crate::api::probe(port, "POST", "/search", Some(&token), Some(query));
            assert_eq!(status, 200, "{query} -> {body}");
            assert!(!body.contains(API_SECRET), "the password went out: {body}");
            assert!(!body.contains("hunter2"), "…not even a piece of it: {body}");
        }
        // …and it is not in the vault FILE in the clear either (the index rides
        // inside the one encrypted blob, like every other derived value)
        let raw = std::fs::read(crate::vault::vault_path(&app.paths.vaults_dir, "bruno")).unwrap();
        for marker in [API_SECRET.as_bytes(), b"x_rag", b"tappeto"] {
            assert!(!raw.windows(marker.len()).any(|w| w == marker), "readable in the file");
        }

        // Ctrl+L locks: the vault goes, and the server goes with it.
        key_ctrl(&mut app, KeyCode::Char('l'));
        assert!(app.locked.is_some(), "the lock screen is up");
        assert!(app.api.is_none(), "the server was stopped");
        assert!(
            crate::api::wait_port_closed(port),
            "port {port} still answers behind a lock screen"
        );
    }

    #[test]
    fn the_api_index_holds_no_secret_and_follows_the_toggles() {
        let (mut app, _d, _guard) = api_app();
        // the snapshot itself — the only thing the server thread can reach —
        // has no secret value in it to serve. This is the structural half of
        // the promise the response bodies above test the other end of.
        let index = app.build_api_index();
        assert_eq!(index.items.len(), 1);
        assert!(index.items[0].text.contains("bruno@example.com"), "non-secret text is indexed");
        assert!(!index.items[0].text.contains(API_SECRET), "the password is not");
        assert!(index.items[0].vec.is_some(), "…and it carries its vector");
        assert_eq!(index.memo.len(), 1);

        // an exposure turned off empties its half of the snapshot at once
        app.settings.api_expose_items = false;
        let index = app.build_api_index();
        assert!(index.items.is_empty() && index.memo.len() == 1);
        // …and turning both off stops the server entirely
        app.settings.api_expose_memo = false;
        app.sync_api_server();
        assert!(app.api.is_none(), "nothing to serve, nothing listening");
    }

    #[test]
    fn a_sync_pull_replaces_what_the_agent_api_is_serving() {
        // a pull replaces the WHOLE content — a message deleted on another
        // machine is gone from the thread on this one. The snapshot the server
        // holds is built from that content, so an agent that kept getting the
        // deleted note back would be searching a vault that has moved on.
        use crate::sync::{SyncOutcome, SyncResult};
        let (mut app, _d, _guard) = api_app();
        let (port, token) = {
            let server = app.api.as_ref().expect("running");
            (server.port(), server.token())
        };
        let query = "{\"scope\":\"memo\",\"query\":\"chiavi\",\"mode\":\"literal\"}";
        let (status, body) = crate::api::probe(port, "POST", "/search", Some(&token), Some(query));
        assert_eq!(status, 200, "{body}");
        assert!(body.contains("tappeto"), "the message is served to begin with: {body}");

        // the other machine deleted it, and this one pulls that content
        let tx = app.pretend_syncing("bruno");
        tx.send((
            "bruno".to_string(),
            SyncResult {
                outcome: SyncOutcome::Pulled(2),
                pulled_content: Some(crate::model::VaultContent::default()),
                new_base: None,
            },
        ))
        .expect("the worker answered");
        app.on_tick();
        assert_eq!(app.vault.as_ref().unwrap().content.memo_len(), 0, "the pull landed");

        let (status, body) = crate::api::probe(port, "POST", "/search", Some(&token), Some(query));
        assert_eq!(status, 200, "{body}");
        assert!(!body.contains("tappeto"), "a deleted message is still served: {body}");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&body).unwrap()["hits"]
                .as_array()
                .map(Vec::len),
            Some(0)
        );
    }

    #[test]
    fn ragtoken_copies_the_whole_token_and_arms_the_wipe() {
        let (mut app, _d, _guard) = api_app();
        let token = app.api.as_ref().expect("running").token();
        assert_eq!(token.len(), 64);
        // the settings row shows only the head of it: a token on screen is a
        // token in a screenshot.
        let hint = app.api.as_ref().unwrap().token_hint();
        assert_eq!(hint, format!("{}\u{2026}", &token[..8]));
        assert!(!hint.contains(&token[8..]), "the row is not the token");

        typed(&mut app, "ragtoken");
        key(&mut app, KeyCode::Enter);
        let status = app.status.as_ref().map(|(_, s)| s.clone()).unwrap_or_default();
        // a headless test box may have no clipboard at all; either way the
        // command answered, and a copy that landed armed the 20 s wipe.
        let copied = i18n::t(Lang::En, &Key::Copied(crate::clipboard::CLEAR_AFTER_SECS));
        let failed = i18n::t(Lang::En, &Key::CopyFailed);
        assert!(status == copied || status == failed, "unexpected: {status}");
        if status == copied {
            assert!(app.clipboard_armed(), "a credential copy is always timed");
        }

        // with no server there is no token to copy, and it says exactly that
        app.stop_api_server();
        typed(&mut app, "ragtoken");
        key(&mut app, KeyCode::Enter);
        assert_eq!(
            app.status.as_ref().map(|(_, s)| s.clone()).unwrap_or_default(),
            i18n::t(Lang::En, &Key::ApiTokenUnavailable)
        );
    }

    // ---- per-vault API sessions (an agent's own unlock) -------------------

    /// `POST /unlock` with the admin token, as an agent would send it.
    fn api_unlock(app: &App, vault: &str, password: &str) -> (u16, String) {
        let server = app.api.as_ref().expect("the server is up");
        let body = format!("{{\"vault\":\"{vault}\",\"password\":\"{password}\"}}");
        crate::api::probe(server.port(), "POST", "/unlock", Some(&server.token()), Some(&body))
    }

    /// The session token out of a successful unlock.
    fn session_token(body: &str) -> String {
        serde_json::from_str::<serde_json::Value>(body)
            .expect("valid JSON")["token"]
            .as_str()
            .expect("a token")
            .to_string()
    }

    #[test]
    fn an_api_session_is_a_vault_of_its_own_and_outlives_the_one_in_the_tui() {
        // the user's own words: read a password through the API even with no
        // vault open here. The TUI's vault and an agent's are two different
        // things, and closing one must not close the other.
        let (mut app, _d, _guard) = api_app();
        app.settings.api_vault_unlock = true;
        app.sync_api_server();
        let (port, admin) = {
            let server = app.api.as_ref().expect("running");
            (server.port(), server.token())
        };

        let (status, body) = api_unlock(&app, "bruno", "pw");
        assert_eq!(status, 200, "{body}");
        let token = session_token(&body);
        assert_eq!(app.api_sessions(), 1);
        // the vectors this vault stored under `x_rag` are the session's to
        // search: it embeds the QUERY and nothing else.
        let (status, body) = crate::api::probe(
            port,
            "POST",
            "/search",
            Some(&token),
            Some("{\"scope\":\"items\",\"query\":\"la password di acme\",\"mode\":\"rag\"}"),
        );
        assert_eq!(status, 200, "{body}");
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["mode"], "rag", "the stored vectors were used: {body}");
        assert_eq!(json["hits"][0]["label"], "Acme");
        assert!(!body.contains(API_SECRET), "a search still never sends one: {body}");

        // the TUI closes ITS vault — the server stays up for `/unlock`, its own
        // snapshot empties, and the agent's session is untouched.
        typed(&mut app, "exit");
        key(&mut app, KeyCode::Enter);
        assert!(app.vault.is_none() && app.page == Page::Access, "the TUI vault is closed");
        assert!(app.api.is_some(), "the server stays up for /unlock");
        assert_eq!(app.api_sessions(), 1, "the agent's session is not the TUI's");
        let (status, body) = crate::api::probe(
            port,
            "POST",
            "/search",
            Some(&token),
            Some("{\"scope\":\"items\",\"query\":\"acme\",\"mode\":\"literal\"}"),
        );
        assert_eq!(status, 200, "{body}");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&body).unwrap()["hits"][0]["label"],
            "Acme",
            "the session still reads its own vault"
        );
        // …while the ADMIN token, whose scope is the TUI, now sees nothing
        let (status, body) = crate::api::probe(
            port,
            "POST",
            "/search",
            Some(&admin),
            Some("{\"scope\":\"items\",\"query\":\"acme\",\"mode\":\"literal\"}"),
        );
        assert_eq!(status, 200, "{body}");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&body).unwrap()["hits"].as_array().map(Vec::len),
            Some(0),
            "the closed vault left the snapshot with it"
        );

        // turning the switch back off is the panic switch: every session dies
        open_settings(&mut app);
        focus_row(&mut app, settings_ui::Row::ApiVaultUnlock);
        key(&mut app, KeyCode::Right);
        key(&mut app, KeyCode::Enter); // save
        assert!(!app.settings.api_vault_unlock);
        assert_eq!(app.api_sessions(), 0, "the panic switch took them");
        assert!(app.api.is_none(), "…and nothing was left listening");
        assert!(crate::api::wait_port_closed(port), "port {port} still answers");
    }

    #[test]
    fn changing_a_vaults_password_kills_the_sessions_holding_its_old_key() {
        use crate::vault::RekeyOutcome;
        let (mut app, _d, _guard) = api_app();
        app.settings.api_vault_unlock = true;
        app.sync_api_server();
        let (port, _) = {
            let server = app.api.as_ref().expect("running");
            (server.port(), server.token())
        };
        let token = session_token(&api_unlock(&app, "bruno", "pw").1);
        assert_eq!(app.api_sessions(), 1);

        // the re-encryption finishes on the worker: the key that session holds
        // is retired, so the session is too.
        let material = app.vault.as_ref().expect("open").rekey_material();
        let tx = app.pretend_rekeying("bruno");
        tx.send(("bruno".to_string(), RekeyOutcome::Done(material))).expect("the worker answered");
        app.on_tick();
        assert_eq!(app.api_sessions(), 0, "the session outlived the password it was opened with");
        let (status, body) = crate::api::probe(
            port,
            "POST",
            "/search",
            Some(&token),
            Some("{\"scope\":\"items\",\"query\":\"acme\"}"),
        );
        assert_eq!(status, 401, "{body}");
    }

    #[test]
    fn the_admin_token_reveals_the_open_vault_only_while_the_toggle_is_on() {
        let (mut app, _d, _guard) = api_app();
        let (port, admin) = {
            let server = app.api.as_ref().expect("running");
            (server.port(), server.token())
        };
        let id = app.vault.as_ref().unwrap().items()[0].id.clone();
        let ask = format!("{{\"id\":\"{id}\"}}");

        // out of the box the endpoint is not there, and the snapshot the server
        // holds has no value in it to send even if it were
        assert!(!app.settings.api_reveal);
        assert!(app.build_api_index().reveal.is_empty(), "no secret is handed over at all");
        let off = crate::api::probe(port, "POST", "/reveal", Some(&admin), Some(&ask));
        assert_eq!(off.0, 404);
        assert_eq!(off, crate::api::probe(port, "POST", "/nonsense", Some(&admin), Some("{}")));

        // the user turns it on through the settings page, and only then
        open_settings(&mut app);
        focus_row(&mut app, settings_ui::Row::ApiReveal);
        key(&mut app, KeyCode::Right);
        key(&mut app, KeyCode::Enter); // save -> the policy and the snapshot follow
        assert!(app.settings.api_reveal);
        let (status, body) = crate::api::probe(port, "POST", "/reveal", Some(&admin), Some(&ask));
        assert_eq!(status, 200, "{body}");
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        let fields = json["fields"].as_array().expect("an array");
        let password = fields.iter().find(|f| f["key"] == "password").expect("the field");
        assert_eq!(password["value"], API_SECRET, "the one endpoint that sends it");
        // …and a `/search` beside it still cannot be made to
        let (_, search) = crate::api::probe(
            port,
            "POST",
            "/search",
            Some(&admin),
            Some("{\"scope\":\"items\",\"query\":\"password\",\"mode\":\"literal\"}"),
        );
        assert!(!search.contains(API_SECRET), "the search grew a secret: {search}");
    }

    #[test]
    fn live_api_sessions_show_in_the_top_bar_and_the_cheatsheet() {
        let (mut app, _d, _guard) = api_app();
        app.settings.api_vault_unlock = true;
        app.sync_api_server();
        app.status = None; // a toast would cover the bar we are reading
        assert!(!render_sized(&app, 120, 30).contains("api\u{b7}"), "nothing open, no chip");

        let token = session_token(&api_unlock(&app, "bruno", "pw").1);
        let frame = render_sized(&app, 120, 30);
        assert!(frame.contains("api\u{b7}1"), "the top bar says an agent is holding one:\n{frame}");
        // …and `?` says what that means, in the reader's language
        let keys = cheatsheet_of(&mut app);
        assert!(keys.contains(&i18n::t(Lang::En, &Key::CheatApiSessions)), "no gloss:\n{keys}");
        assert!(!frame.contains("bruno\u{b7}"), "the chip counts, it does not name");

        // the agent locks its own session and the chip goes with it
        let port = app.api.as_ref().unwrap().port();
        assert_eq!(crate::api::probe(port, "POST", "/lock", Some(&token), Some("{}")).0, 200);
        assert_eq!(app.api_sessions(), 0);
        assert!(!render_sized(&app, 120, 30).contains("api\u{b7}"));
    }

    #[test]
    fn the_lock_screen_says_what_the_lock_did_not_close() {
        // Ctrl+L (and the idle auto-lock, which is the same call) closes THIS
        // vault and nothing else: with `/unlock` on, the server stays up and an
        // agent's own session keeps answering out of its own decrypted copy. The
        // chip is the one thing that says so, and the screen an unattended
        // machine is left on is exactly where it must not disappear.
        let (mut app, _d, _guard) = api_app();
        app.settings.api_vault_unlock = true;
        app.sync_api_server();
        app.status = None; // a toast would cover what we are reading
        let port = app.api.as_ref().expect("running").port();
        let token = session_token(&api_unlock(&app, "bruno", "pw").1);

        key_ctrl(&mut app, KeyCode::Char('l'));
        assert!(app.locked.is_some(), "the lock screen is up");
        assert_eq!(app.api_sessions(), 1, "the agent's session is not behind this lock");
        let frame = render_sized(&app, 120, 30);
        assert!(frame.contains("api\u{b7}1"), "the lock screen hides the chip:\n{frame}");
        assert!(
            frame.contains(&i18n::t(Lang::En, &Key::LockApiSessions)),
            "…and says nothing about what it means:\n{frame}"
        );
        assert!(!frame.contains("bruno\u{b7}"), "the chip counts, it does not name");
        // …and it is telling the truth: that session really is still serving
        let (status, body) = crate::api::probe(
            port,
            "POST",
            "/search",
            Some(&token),
            Some("{\"scope\":\"items\",\"query\":\"acme\",\"mode\":\"literal\"}"),
        );
        assert_eq!(status, 200, "{body}");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&body).unwrap()["hits"][0]["label"],
            "Acme",
            "the chip would have been an empty warning"
        );
        // the reader's language, like every other word on this screen
        app.settings.lang = Lang::It;
        let frame = render_sized(&app, 120, 30);
        assert!(frame.contains(&i18n::t(Lang::It, &Key::LockApiSessions)), "no italian:\n{frame}");
    }

    #[test]
    fn a_lock_screen_with_a_live_session_never_panics_at_any_size() {
        let (mut app, _d, _guard) = api_app();
        app.settings.api_vault_unlock = true;
        app.sync_api_server();
        let _ = api_unlock(&app, "bruno", "pw");
        key_ctrl(&mut app, KeyCode::Char('l'));
        assert!(app.locked.is_some() && app.api_sessions() == 1);
        for (w, h) in [(1u16, 1u16), (2, 1), (6, 4), (16, 8), (24, 7), (40, 24), (200, 60)] {
            for frame in [0u64, 3, 41] {
                app.frame = frame;
                let _ = render_sized(&app, w, h); // must not panic
            }
        }
        // …and in the language whose gloss is the longer one
        app.settings.lang = Lang::It;
        for (w, h) in [(1u16, 1u16), (10, 6), (40, 12), (120, 30)] {
            let _ = render_sized(&app, w, h);
        }
    }

    #[test]
    fn destroying_a_vault_takes_the_sessions_that_still_hold_its_plaintext() {
        // a session answers out of the decrypted copy in this process, never out
        // of the file — so unlinking the file (or clearing every item in it)
        // leaves an agent reading a vault the user believes is gone.
        let (mut app, _d, _guard) = api_app();
        app.settings.api_vault_unlock = true;
        app.sync_api_server();
        let port = app.api.as_ref().expect("running").port();
        let search = "{\"scope\":\"items\",\"query\":\"acme\",\"mode\":\"literal\"}";
        // the TUI leaves the vault: `deletevault` is an Access-page verb
        typed(&mut app, "exit");
        key(&mut app, KeyCode::Enter);

        // emptying it is the milder half of the same defect: the session keeps
        // the pre-nuke items
        let token = session_token(&api_unlock(&app, "bruno", "pw").1);
        assert_eq!(app.api_sessions(), 1);
        typed(&mut app, "nukevault bruno");
        key(&mut app, KeyCode::Enter); // the form, focused on the password
        typed(&mut app, "pw");
        key(&mut app, KeyCode::Enter);
        assert!(app.form.is_none() && app.status.as_ref().unwrap().1.contains("emptied"));
        assert_eq!(app.api_sessions(), 0, "the emptied vault is still being served");
        assert_eq!(crate::api::probe(port, "POST", "/search", Some(&token), Some(search)).0, 401);

        // …and deleting it says more than the re-key that already drops them
        let token = session_token(&api_unlock(&app, "bruno", "pw").1);
        assert_eq!(app.api_sessions(), 1);
        typed(&mut app, "deletevault bruno");
        key(&mut app, KeyCode::Enter);
        typed(&mut app, "pw");
        key(&mut app, KeyCode::Enter);
        assert!(app.vaults.is_empty(), "the file is gone");
        assert_eq!(app.api_sessions(), 0, "a deleted vault was still being served");
        let (status, body) = crate::api::probe(port, "POST", "/search", Some(&token), Some(search));
        assert_eq!(status, 401, "{body}");
    }

    #[test]
    fn the_session_rows_cycle_gate_and_persist() {
        let (mut app, _d) = app_with(&["bruno"]);
        app.rag.use_mock(); // a saved settings page must not fetch a model
        assert!(!app.settings.api_vault_unlock && !app.settings.api_reveal);
        assert_eq!(app.settings.api_session_ttl, crate::settings::ApiSessionTtl::FifteenMin);
        assert_eq!(app.settings.api_session_secs(), None, "…and no session can exist to expire");

        open_settings(&mut app);
        // the expiry row is frozen while `/unlock` is off, and says which
        // switch is holding it — not nothing
        focus_row(&mut app, settings_ui::Row::ApiSessionTtl);
        key(&mut app, KeyCode::Right);
        assert_eq!(
            app.settings_ui.as_ref().unwrap().draft.api_session_ttl,
            crate::settings::ApiSessionTtl::FifteenMin,
            "refused"
        );
        assert_eq!(
            app.status.as_ref().map(|(_, s)| s.clone()).unwrap_or_default(),
            i18n::t(Lang::En, &Key::SettingHintApiNeedsUnlock),
        );

        focus_row(&mut app, settings_ui::Row::ApiVaultUnlock);
        key(&mut app, KeyCode::Right);
        focus_row(&mut app, settings_ui::Row::ApiReveal);
        key(&mut app, KeyCode::Right);
        // …and now the expiry cycles, both ways, wrapping
        focus_row(&mut app, settings_ui::Row::ApiSessionTtl);
        for expected in [
            crate::settings::ApiSessionTtl::OneHour,
            crate::settings::ApiSessionTtl::Off,
            crate::settings::ApiSessionTtl::FiveMin,
            crate::settings::ApiSessionTtl::FifteenMin,
        ] {
            key(&mut app, KeyCode::Right);
            assert_eq!(app.settings_ui.as_ref().unwrap().draft.api_session_ttl, expected);
        }
        key(&mut app, KeyCode::Left);
        assert_eq!(
            app.settings_ui.as_ref().unwrap().draft.api_session_ttl,
            crate::settings::ApiSessionTtl::FiveMin,
            "…both ways"
        );
        key(&mut app, KeyCode::Enter); // save

        assert!(app.settings.api_vault_unlock && app.settings.api_reveal);
        assert_eq!(app.settings.api_session_secs(), Some(5 * 60));
        // …and it is on the disk, where a hand edit can reach it
        let saved = crate::settings::Settings::load(&app.paths.settings_config);
        assert!(saved.api_vault_unlock && saved.api_reveal);
        assert_eq!(saved.api_session_ttl, crate::settings::ApiSessionTtl::FiveMin);

        // switching `/unlock` back off takes the expiry with it, so a later
        // flip cannot quietly restore a TTL nobody chose again
        open_settings(&mut app);
        focus_row(&mut app, settings_ui::Row::ApiVaultUnlock);
        key(&mut app, KeyCode::Right);
        let draft = &app.settings_ui.as_ref().unwrap().draft;
        assert!(!draft.api_vault_unlock);
        assert_eq!(draft.api_session_ttl, crate::settings::ApiSessionTtl::FifteenMin);
        key(&mut app, KeyCode::Esc);

        // a hand-edited file that asks for an expiry with no unlock is read as
        // "there are no sessions", exactly as an exposure without its index is
        let mut hand_edited = crate::settings::Settings::default();
        hand_edited.api_session_ttl = crate::settings::ApiSessionTtl::OneHour;
        assert_eq!(hand_edited.api_session_secs(), None);
        assert!(!hand_edited.api_server_wanted(false), "…and nothing listens for it");
    }

    #[test]
    fn the_server_runs_with_every_vault_closed_when_api_unlock_is_on() {
        // the Access page, nothing open, no RAG exposure at all — and an agent
        // can still bring its own password.
        let _guard = crate::api::port_guard();
        let (mut app, _d) = app_with(&["bruno"]);
        app.settings.api_port = 0;
        assert!(!app.settings.api_enabled(), "nothing is being served out of the TUI");
        app.sync_api_server();
        assert!(app.api.is_none(), "…so nothing is listening yet");

        app.settings.api_vault_unlock = true;
        app.sync_api_server();
        let (port, admin) = {
            let server = app.api.as_ref().expect("the unlock switch alone brought it up");
            (server.port(), server.token())
        };
        assert!(app.vault.is_none() && app.page == Page::Access);
        assert_eq!(crate::api::probe(port, "GET", "/health", None, None).0, 200);

        let (status, body) = api_unlock(&app, "bruno", "pw");
        assert_eq!(status, 200, "{body}");
        let token = session_token(&body);
        // an empty vault searches empty — but it ANSWERS, which is the point
        let (status, body) = crate::api::probe(
            port,
            "POST",
            "/search",
            Some(&token),
            Some("{\"scope\":\"items\",\"query\":\"acme\"}"),
        );
        assert_eq!(status, 200, "{body}");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&body).unwrap()["hits"].as_array().map(Vec::len),
            Some(0)
        );
        // the admin token is still the TUI's scope, and the TUI has nothing
        assert_eq!(
            crate::api::probe(
                port,
                "POST",
                "/search",
                Some(&admin),
                Some("{\"scope\":\"memo\",\"query\":\"chiavi\"}")
            )
            .0,
            200
        );
        app.stop_api_server();
        assert!(crate::api::wait_port_closed(port), "port {port} still answers after the stop");
    }

    #[test]
    fn access_reduce_motion_keeps_cursor_steady() {
        let (mut app, _d) = app_with(&[]);
        app.frame = 4; // a frame where the cursor would normally be blinked off
        assert!(!app.cursor_on());
        app.settings.reduce_motion = true;
        assert!(app.cursor_on(), "reduce-motion keeps the cursor steady");
    }

    #[test]
    fn access_confirm_exit_needs_two_escapes() {
        let (mut app, _d) = app_with(&["bruno"]);
        app.settings.confirm_exit = true;
        // first Esc at the top level asks for confirmation, doesn't quit
        assert!(!app.on_key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.status.as_ref().unwrap().1.contains("Esc"));
        // second Esc quits
        assert!(app.on_key(KeyCode::Esc, KeyModifiers::NONE));
    }

    #[test]
    fn access_confirm_exit_reset_by_other_key() {
        let (mut app, _d) = app_with(&["bruno"]);
        app.settings.confirm_exit = true;
        assert!(!app.on_key(KeyCode::Esc, KeyModifiers::NONE)); // pending
        key(&mut app, KeyCode::Down); // any other key cancels the pending quit
        assert!(!app.on_key(KeyCode::Esc, KeyModifiers::NONE), "must ask again, not quit");
    }

    #[test]
    fn settings_tab_moves_down_the_single_list() {
        let (mut app, _d) = app_with(&[]);
        open_settings(&mut app);
        assert_eq!(app.settings_ui.as_ref().unwrap().selected, 0);
        key(&mut app, KeyCode::Tab);
        assert_eq!(app.settings_ui.as_ref().unwrap().selected, 1);
        key(&mut app, KeyCode::BackTab);
        assert_eq!(app.settings_ui.as_ref().unwrap().selected, 0);
    }

    // ---- logo header & configurable prompt -------------------------------

    #[test]
    fn header_hidden_when_show_logo_is_off() {
        let (mut app, _d) = app_with(&["bruno"]);
        app.logo = vec!["ZZZLOGO".to_string()];
        app.settings.show_logo = false;
        assert!(!render_sized(&app, 80, 24).contains("ZZZLOGO"));
    }

    #[test]
    fn header_hidden_on_a_short_terminal() {
        let (mut app, _d) = app_with(&["bruno"]);
        app.logo = vec!["ZZZLOGO".to_string()];
        app.settings.show_logo = true;
        // too short to keep a usable body under the header
        assert!(!render_sized(&app, 80, 10).contains("ZZZLOGO"));
    }

    #[test]
    fn header_does_not_panic_when_the_logo_is_wider_than_the_terminal() {
        let (mut app, _d) = app_with(&["bruno"]);
        app.settings.show_logo = true;
        // far wider than any of these terminals — the pre-fix crash
        app.logo = vec!["X".repeat(200), "DATASHIELD".to_string()];
        for (w, h) in [(24u16, 40u16), (80, 40), (30, 40)] {
            let text = render_sized(&app, w, h); // must not panic
            assert!(!text.contains(&"X".repeat(40)), "an over-wide logo is dropped, not clipped");
        }
    }

    #[test]
    fn settings_list_has_a_logo_toggle() {
        assert!(crate::settings_ui::ALL_ROWS
            .iter()
            .any(|(_, r)| *r == crate::settings_ui::Row::ShowLogo));
    }

    #[test]
    fn language_command_persists_and_survives_a_settings_save() {
        let (mut app, _d) = app_with(&[]);
        typed(&mut app, "language it");
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.settings.lang, Lang::It);
        // the `language` command persists to disk (single source of truth)
        assert_eq!(
            crate::settings::Settings::load(&app.paths.settings_config).lang,
            Lang::It
        );
        // saving from the page without touching anything must NOT revert it
        open_settings(&mut app);
        key(&mut app, KeyCode::Down); // move off the language row (no cycle)
        key(&mut app, KeyCode::Enter); // save
        assert_eq!(app.settings.lang, Lang::It, "settings save reverted the language");
    }

    // ---- passwd (change the master password) -----------------------------

    #[test]
    fn passwd_form_closes_immediately_and_marks_the_vault_busy() {
        let (mut app, _d) = app_with(&["bruno"]);
        typed(&mut app, "passwd");
        key(&mut app, KeyCode::Enter);
        {
            let form = app.form.as_ref().expect("passwd opens its form");
            assert_eq!(form.fields.len(), 3, "current + new + confirm");
            assert!(form.fields.iter().all(|f| f.secret), "all three fields are secret");
        }
        {
            let f = app.form.as_mut().unwrap();
            f.fields[0].value = "pw".into();
            f.fields[1].value = "nuova".into();
            f.fields[2].value = "nuova".into();
        }
        submit_now(&mut app);
        // the spec: Enter returns to the normal screen at once — the current
        // password is verified later, on the worker.
        assert!(app.form.is_none(), "the form closes on submit");
        assert!(app.rekeying("bruno"), "the vault is marked re-encrypting");
        // and the bar takes over the cipher/age slot of that vault's row
        let text = render_sized(&app, 80, 24);
        assert!(text.contains("re-encrypting"), "the progress bar labels the busy vault");
    }

    #[test]
    fn passwd_refuses_a_mismatched_or_unchanged_new_passphrase() {
        let (mut app, _d) = app_with(&["bruno"]);
        typed(&mut app, "passwd");
        key(&mut app, KeyCode::Enter);
        {
            let f = app.form.as_mut().unwrap();
            f.fields[0].value = "pw".into();
            f.fields[1].value = "una".into();
            f.fields[2].value = "altra".into();
        }
        submit_now(&mut app);
        assert!(app.form.as_ref().unwrap().error.is_some(), "a mismatch keeps the form open");
        assert!(app.passwd_busy.is_none(), "no worker was started");
        // the new passphrase being the current one is refused too
        {
            let f = app.form.as_mut().unwrap();
            f.fields[1].value = "pw".into();
            f.fields[2].value = "pw".into();
            f.error = None;
        }
        submit_now(&mut app);
        assert!(app.form.as_ref().unwrap().error.is_some(), "an unchanged passphrase is refused");
        assert!(app.passwd_busy.is_none());
    }

    #[test]
    fn passwd_is_refused_while_a_sync_worker_still_holds_the_file() {
        // the guard runs BOTH ways: a sync can't start during a re-encryption,
        // and a re-encryption can't start during a sync — the two end in a
        // rename over the same file, and whichever lands last wins.
        let (mut app, _d) = app_with(&["bruno"]);
        let _worker = app.pretend_syncing("bruno"); // held: the worker is out
        typed(&mut app, "passwd");
        key(&mut app, KeyCode::Enter);
        assert!(app.form.is_none(), "the form never opens while a sync holds the file");
        assert!(
            app.status.as_ref().unwrap().1.contains("syncing"),
            "…with a toast saying why: {:?}",
            app.status
        );

        // …and a form opened BEFORE the sync started refuses to submit
        let (mut app, _d) = app_with(&["bruno"]);
        typed(&mut app, "passwd");
        key(&mut app, KeyCode::Enter);
        let _worker = app.pretend_syncing("bruno");
        {
            let f = app.form.as_mut().unwrap();
            f.fields[0].value = "pw".into();
            f.fields[1].value = "nuova".into();
            f.fields[2].value = "nuova".into();
        }
        submit_now(&mut app);
        assert!(app.form.as_ref().unwrap().error.is_some(), "the form stays open, with the reason");
        assert!(app.passwd_busy.is_none(), "no worker was started");
    }

    #[test]
    fn a_password_change_marks_the_synced_remote_copy_as_retired() {
        // the point of a rotation is that the OLD password stops opening the
        // vault — including the copy sitting on the server.
        let (mut app, _d) = app_with(&["bruno"]);
        let cfg = crate::sync::SyncConfig {
            host: "vps.example.com".into(),
            user: "deploy".into(),
            remote_dir: "~/.datashield".into(),
            key_path: String::new(),
            auto: false, // no worker: this test never touches the network
            last_synced: None,
            base_counter: 1,
            rekeyed: false,
        };
        cfg.save(&app.paths.vaults_dir, "bruno").unwrap();
        typed(&mut app, "passwd");
        key(&mut app, KeyCode::Enter);
        {
            let f = app.form.as_mut().unwrap();
            f.fields[0].value = "pw".into();
            f.fields[1].value = "nuova".into();
            f.fields[2].value = "nuova".into();
        }
        submit_now(&mut app);
        assert!(app.rekeying("bruno"));
        for _ in 0..400 {
            app.on_tick();
            if app.passwd_busy.is_none() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(app.passwd_busy.is_none(), "the re-encryption landed");
        assert!(Vault::open(&app.paths.vaults_dir, "bruno", "nuova").is_ok(), "the file re-keyed");
        let after = crate::sync::SyncConfig::load(&app.paths.vaults_dir, "bruno").expect("sidecar");
        assert!(after.rekeyed, "the server's copy is flagged as the retired key's");
        assert!(
            app.status.as_ref().unwrap().1.contains("server"),
            "…and the user is told what is left to do: {:?}",
            app.status
        );
    }

    #[test]
    fn a_dead_passwd_worker_frees_the_vault_instead_of_wedging_it() {
        // `passwd_busy` drives the progress bar AND every refuse_if_rekeying
        // guard: a worker that dies without answering would otherwise pin it
        // for the session, locking the user out of unlocking, syncing, editing
        // and deleting that vault — for a change that never happened.
        let (mut app, _d) = app_with(&["bruno"]);
        {
            let _worker = app.pretend_rekeying("bruno");
            app.on_tick();
            assert!(app.rekeying("bruno"), "still out: there is nothing to report yet");
        } // …and here the worker dies without sending
        app.on_tick();
        assert!(app.passwd_busy.is_none(), "the slot is freed");
        assert_eq!(
            app.status.as_ref().map(|(_, s)| s.clone()).unwrap_or_default(),
            i18n::t(Lang::En, &Key::PasswdLost),
            "…with a toast saying the change was lost"
        );
        // …and the vault is usable again: its guards let go
        key(&mut app, KeyCode::Enter);
        assert!(app.unlock.is_some(), "the vault unlocks again");
        assert!(!render_sized(&app, 80, 24).contains("re-encrypting"), "the bar is gone");
    }

    #[test]
    fn passwd_refuses_a_vault_name_that_matches_nothing() {
        // falling back to the highlighted vault turns a typo into a re-key of
        // the WRONG file — and since the form asks for the current password,
        // the mistake only ever surfaces as "wrong passphrase", much later.
        let (mut app, _d) = app_with(&["bruno", "alice"]);
        typed(&mut app, "passwd brunoo");
        key(&mut app, KeyCode::Enter);
        assert!(app.form.is_none(), "a typo never opens the form");
        assert!(app.passwd_busy.is_none(), "…and never starts a worker");
        assert_eq!(
            app.status.as_ref().map(|(_, s)| s.clone()).unwrap_or_default(),
            i18n::t(Lang::En, &Key::UnknownVault("brunoo")),
            "…it names what it couldn't find"
        );
        // …in italian too
        app.settings.lang = Lang::It;
        typed(&mut app, "passwd nessuno");
        key(&mut app, KeyCode::Enter);
        assert_eq!(
            app.status.as_ref().map(|(_, s)| s.clone()).unwrap_or_default(),
            "vault sconosciuto: nessuno"
        );
        // a name that DOES match still opens the form on that vault, never on
        // the highlighted one (the list is alphabetical: alice is highlighted)
        app.settings.lang = Lang::En;
        assert_eq!(app.vaults[app.selected], "alice");
        typed(&mut app, "passwd bruno");
        key(&mut app, KeyCode::Enter);
        let form = app.form.as_ref().expect("an exact name opens the form");
        assert_eq!(form.purpose, crate::app::FormPurpose::ChangePassword("bruno".into()));
    }

    #[test]
    fn passwd_inside_a_vault_refuses_to_retarget_another_one() {
        // inside a vault the form asks for the OPEN vault's current password;
        // silently retargeting `:passwd alice` at bruno would have the user
        // type alice's passphrase into bruno's re-key and see "wrong passphrase"
        let (mut app, _d) = app_with(&["bruno", "alice"]);
        while app.vaults[app.selected] != "bruno" {
            key(&mut app, KeyCode::Down);
        }
        open_first_vault(&mut app);
        assert_eq!(app.page, Page::Vault);
        typed(&mut app, "passwd alice");
        key(&mut app, KeyCode::Enter);
        assert!(app.form.is_none(), "a different vault's name never opens the form");
        assert_eq!(
            app.status.as_ref().map(|(_, s)| s.clone()).unwrap_or_default(),
            i18n::t(Lang::En, &Key::PasswdOtherVault("alice")),
            "…it says the open vault must be closed first"
        );
        // the open vault's own name (and no name at all) still work
        typed(&mut app, "passwd bruno");
        key(&mut app, KeyCode::Enter);
        let form = app.form.as_ref().expect("the open vault's own name is fine");
        assert_eq!(form.purpose, crate::app::FormPurpose::ChangePassword("bruno".into()));
    }

    #[test]
    fn a_rekeying_vault_refuses_to_unlock() {
        let (mut app, _d) = app_with(&["bruno"]);
        app.passwd_busy = Some("bruno".to_string());
        key(&mut app, KeyCode::Enter); // Enter on the row would unfold the unlock
        assert!(app.unlock.is_none(), "unlock is refused while the file is rewritten");
        assert!(app.status.as_ref().unwrap().1.contains("re-encrypted"), "with a toast saying why");
        // …and it unfolds again once the worker has landed
        app.passwd_busy = None;
        key(&mut app, KeyCode::Enter);
        assert!(app.unlock.is_some());
    }

    #[test]
    fn the_rekey_bar_animates_but_freezes_under_reduce_motion() {
        let (mut app, _d) = app_with(&["bruno"]);
        app.passwd_busy = Some("bruno".to_string());
        app.frame = 0;
        let first = rekey_bar(&app);
        assert_eq!(first.chars().filter(|c| *c == '█').count(), REKEY_BAR_RUN);
        app.frame = 6;
        assert_ne!(first, rekey_bar(&app), "the block travels with the frame");
        app.settings.reduce_motion = true;
        let still = rekey_bar(&app);
        app.frame = 41;
        assert_eq!(still, rekey_bar(&app), "reduce-motion renders a static bar");
    }

    #[test]
    fn the_rekey_progress_bar_never_panics_at_any_size() {
        // on the access list the bar rides the vault's row…
        let (mut app, _d) = app_with(&["bruno", "alice"]);
        app.passwd_busy = Some("bruno".to_string());
        for (w, h) in [(1u16, 1u16), (6, 4), (40, 24), (80, 24), (200, 60)] {
            for frame in [0u64, 3, 7, 13, 41] {
                app.frame = frame;
                let _ = render_sized(&app, w, h); // must not panic
            }
        }
        // …and inside the open vault it rides the top-bar
        let (mut app, _d2) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        app.passwd_busy = Some("bruno".to_string());
        for (w, h) in [(1u16, 1u16), (6, 4), (40, 24), (80, 24), (200, 60)] {
            for frame in [0u64, 3, 7, 13, 41] {
                app.frame = frame;
                let _ = render_sized(&app, w, h); // must not panic
            }
        }
    }

    // ---- the one-time model download's progress bar --------------------

    /// Puts the app in the middle of the download, with a known number of bytes
    /// already in the cache. Nothing is fetched, of course: this is exactly the
    /// state `RagState::sample_download` leaves behind on a real one.
    fn downloading_with(app: &mut App, bytes: Option<u64>) {
        app.rag.status = crate::rag::RagStatus::Downloading;
        app.rag.set_download_bytes(bytes);
    }

    #[test]
    fn the_model_download_takes_the_status_row_with_a_measured_percentage() {
        let (mut app, _d) = app_with(&["bruno"]);
        // 200 MiB of the 464 the model weighs
        downloading_with(&mut app, Some(200 * 1024 * 1024));
        let text = render_sized(&app, 110, 34);
        assert!(
            text.contains("⇣ RAG model ████░░░░░░ 43% · 200/464 MiB"),
            "the whole element is missing:\n{text}"
        );
        // a toast splits the row instead of losing it: the bar drops to its
        // compact form on the left and the message keeps the remainder — this
        // status line has no timer, so neither may hide the other for the
        // minutes the download takes (see `draw_status`).
        app.status = Some((StatusKind::Info, "fetching the model".to_string()));
        let text = render_sized(&app, 110, 34);
        assert!(text.contains("⇣ ████░░░░░░ 43%"), "a toast hid the bar:\n{text}");
        assert!(text.contains("· fetching the model"), "…and the bar hid the toast:\n{text}");
        // it comes back the moment the download is over — `poll` clears both
        // halves of the state together (see the `rag` test of the same name)
        app.rag.status = crate::rag::RagStatus::Ready;
        app.rag.set_download_bytes(None);
        let text = render_sized(&app, 110, 34);
        assert!(!text.contains('⇣'), "the bar outlived the download:\n{text}");
        assert!(text.contains("fetching the model"), "…and the toast has the row back");
    }

    #[test]
    fn the_download_bar_sheds_its_parts_as_the_row_narrows() {
        let (mut app, _d) = app_with(&["bruno"]);
        downloading_with(&mut app, Some(200 * 1024 * 1024));
        // 40 cells: the counter goes first, the label and the bar stay
        let text = render_sized(&app, 40, 24);
        assert!(text.contains("⇣ RAG model ████░░░░░░ 43%"), "the bar is gone at 40 wide:\n{text}");
        assert!(!text.contains("MiB"), "the counter should be the first thing shed:\n{text}");
        // 20: the label goes too, the bar and its percentage never do
        let text = render_sized(&app, 20, 24);
        assert!(text.contains("⇣ ████░░░░░░ 43%"), "at 20 wide:\n{text}");
        assert!(!text.contains("RAG model"), "the label should be shed at 20 wide:\n{text}");
        // 16: even the glyph
        let text = render_sized(&app, 16, 24);
        assert!(text.contains("████░░░░░░ 43%"), "at 16 wide:\n{text}");
    }

    #[test]
    fn the_download_bar_is_indeterminate_before_the_first_sample() {
        let (mut app, _d) = app_with(&["bruno"]);
        // the download has just started: the bar paints NOW, and says nothing
        // it does not know — an ellipsis, never a 0 % that would read as a stall
        downloading_with(&mut app, None);
        let text = render_sized(&app, 110, 34);
        assert!(text.contains("⇣ RAG model ░░░░░░░░░░ …"), "no indeterminate bar:\n{text}");
        assert!(!text.contains('%'), "it claimed a percentage it has not measured:\n{text}");
        assert!(!text.contains("MiB"), "…or a byte count:\n{text}");
        // and it is a STILL bar: nothing in it moves with the frame, so
        // reduce-motion has nothing to freeze
        app.frame = 37;
        assert_eq!(text, render_sized(&app, 110, 34), "the indeterminate bar animates");
        app.settings.reduce_motion = true;
        assert_eq!(text, render_sized(&app, 110, 34));
    }

    #[test]
    fn the_download_bar_never_panics_at_any_size() {
        let (mut app, _d) = app_with(&["bruno", "alice"]);
        for bytes in [None, Some(0), Some(1), Some(200 * 1024 * 1024), Some(u64::MAX)] {
            downloading_with(&mut app, bytes);
            for (w, h) in [(1u16, 1u16), (2, 1), (6, 4), (16, 8), (40, 24), (80, 24), (200, 60)] {
                for frame in [0u64, 3, 7, 13, 41] {
                    app.frame = frame;
                    let _ = render_sized(&app, w, h); // must not panic
                }
            }
        }
        // …and the same inside an open vault, where the row is the keybar's
        let (mut app, _d2) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        downloading_with(&mut app, Some(1024));
        for (w, h) in [(1u16, 1u16), (6, 4), (40, 24), (80, 24), (200, 60)] {
            let _ = render_sized(&app, w, h);
        }
    }

    #[test]
    fn the_tick_samples_the_cache_about_once_a_second_and_only_while_it_matters() {
        let (mut app, dir) = app_with(&["bruno"]);
        let models = dir.path().join("models");
        std::fs::create_dir_all(&models).unwrap();
        std::fs::write(models.join("blob.part"), vec![0u8; 1024]).unwrap();
        // no download out: the tick never so much as opens the directory
        for _ in 0..20 {
            app.on_tick();
        }
        assert_eq!(app.rag.download_bytes(), None);
        // one starts (without a worker: `status` is the whole of what the
        // sampler looks at) — and the first sample lands inside a second
        app.frame = 0;
        app.rag.status = crate::rag::RagStatus::Downloading;
        for _ in 0..crate::app::RAG_SAMPLE_TICKS {
            app.on_tick();
        }
        assert_eq!(app.rag.download_bytes(), Some(1024), "no sample in the first second");
        // …and the next tick does NOT re-walk the tree: the throttle is the
        // whole reason this is free on a 120 ms loop
        std::fs::write(models.join("blob.part"), vec![0u8; 4096]).unwrap();
        app.on_tick();
        assert_eq!(app.rag.download_bytes(), Some(1024), "sampled on every tick");
        for _ in 0..crate::app::RAG_SAMPLE_TICKS {
            app.on_tick();
        }
        assert_eq!(app.rag.download_bytes(), Some(4096), "the bar stopped following the bytes");
    }

    #[test]
    fn the_download_bar_speaks_italian_too() {
        let (mut app, _d) = app_with(&["bruno"]);
        app.settings.lang = Lang::It;
        downloading_with(&mut app, Some(47 * 1024 * 1024));
        let text = render_sized(&app, 110, 34);
        assert!(text.contains("⇣ modello RAG █░░░░░░░░░ 10% · 47/464 MiB"), "{text}");
    }

    // ---- image fields --------------------------------------------------

    /// A real `w×h` PNG written into `dir`, and the bytes that landed there.
    fn write_png(dir: &std::path::Path, name: &str, w: u32, h: u32) -> (std::path::PathBuf, Vec<u8>) {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([10, 120, 220, 255]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
            .expect("encodes");
        let path = dir.join(name);
        std::fs::write(&path, &bytes).expect("writes");
        (path, bytes)
    }

    /// Opens the Add form for a category, walking the real picker to its row.
    fn open_add_form_for(app: &mut App, category: &str) {
        typed(app, "add");
        key(app, KeyCode::Enter); // -> the category picker
        let idx = app
            .picker_choices(PickerKind::ItemCategory)
            .iter()
            .position(|c| c.value == category)
            .unwrap_or_else(|| panic!("no '{category}' row in the picker"));
        for _ in 0..idx {
            key(app, KeyCode::Down);
        }
        key(app, KeyCode::Enter);
        assert!(app.form.is_some(), "the add form opened");
    }

    /// Fills the form row whose (localized) label is `label`.
    fn set_form_field(app: &mut App, key_name: &str, value: &str) {
        let label = i18n::t(app.settings.lang, &Key::FieldLabel(key_name));
        let form = app.form.as_mut().expect("a form is open");
        let i = form
            .fields
            .iter()
            .position(|f| f.name == label)
            .unwrap_or_else(|| panic!("no '{label}' row on the form"));
        form.fields[i].value = value.to_string().into();
        form.error = None; // typing clears it; setting the value must too
    }

    /// The open vault's first item's field by key.
    fn field_by_key<'a>(app: &'a App, key: &str) -> &'a crate::model::ItemField {
        app.vault.as_ref().unwrap().items()[0]
            .fields
            .iter()
            .find(|f| f.key == key)
            .unwrap_or_else(|| panic!("no '{key}' field on the item"))
    }

    #[test]
    fn an_imported_image_is_stored_as_base64_and_never_shown() {
        use base64::{engine::general_purpose::STANDARD as B64, Engine};
        let (mut app, dir) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        let (png, bytes) = write_png(dir.path(), "id-front.png", 64, 40);

        // the identity category now offers the two document-photo slots, and an
        // image row takes a FILE PATH — never masked while it is typed
        open_add_form_for(&mut app, "identity");
        let front_row = i18n::t(Lang::En, &Key::FieldLabel("photo_front"));
        {
            let form = app.form.as_ref().unwrap();
            let row = form.fields.iter().find(|f| f.name == front_row).expect("a photo row");
            assert!(!row.secret, "a path is not a secret: it is typed in the clear");
            assert!(row.description.contains("PNG"), "the row says which formats it takes");
        }
        set_form_field(&mut app, "label", "Passport");
        set_form_field(&mut app, "photo_front", png.to_str().unwrap());
        // …and it is drawn as typed, with its hint under it (fig-06)
        let row_idx = app
            .form
            .as_ref()
            .unwrap()
            .fields
            .iter()
            .position(|f| f.name == front_row)
            .unwrap();
        for _ in 0..row_idx {
            key(&mut app, KeyCode::Down);
        }
        let form_text = render_sized(&app, 120, 44);
        assert!(form_text.contains("id-front.png"), "the path is shown, not masked:\n{form_text}");
        assert!(form_text.contains("path to an image file"), "…over its hint:\n{form_text}");
        submit_now(&mut app);
        assert!(app.form.is_none(), "the form closed — the import was accepted");

        // stored: the ORIGINAL file bytes as base64, secret, with its metadata
        let payload = {
            let front = field_by_key(&app, "photo_front");
            assert_eq!(front.ftype, crate::model::FieldType::Image);
            assert!(front.secret, "an image field is stored secret (older readers mask it)");
            assert_eq!(B64.decode(front.value.as_str()).unwrap(), bytes, "byte for byte, no re-encode");
            assert_eq!(front.extra["x_image"]["mime"], "image/png");
            assert_eq!(front.extra["x_image"]["bytes"], bytes.len() as u64);
            front.value.as_str().to_string()
        };
        let reopened = Vault::open(&app.paths.vaults_dir, "bruno", "pw").expect("reopens");
        let on_disk = reopened.items()[0].fields.iter().find(|f| f.key == "photo_front").unwrap();
        assert_eq!(on_disk.value.as_str(), payload, "it reached the file");
        assert_eq!(on_disk.extra["x_image"]["mime"], "image/png", "…metadata and all");

        // on screen: a placeholder, never the payload — revealed or not
        let head: String = payload.chars().take(16).collect();
        for reveal in [false, true] {
            app.detail_reveal = reveal;
            let text = render_sized(&app, 120, 40);
            assert!(text.contains("front of document"), "the row is labelled:\n{text}");
            assert!(text.contains("image · PNG"), "…and shows a placeholder:\n{text}");
            assert!(!text.contains(&head), "the base64 must never be drawn (reveal={reveal})");
        }
        // and it is out of the search index (secret ⇒ never matched)
        app.vault_query = head.to_lowercase().into();
        assert!(app.visible_item_indices().is_empty(), "an image value is never searched");
    }

    #[test]
    fn a_refused_image_import_stores_nothing_and_says_why() {
        let (mut app, dir) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        // (1) something that isn't an image at all
        let junk = dir.path().join("scan.txt");
        std::fs::write(&junk, b"not an image at all").unwrap();
        open_add_form_for(&mut app, "identity");
        set_form_field(&mut app, "label", "Passport");
        set_form_field(&mut app, "photo_front", junk.to_str().unwrap());
        submit_now(&mut app);
        let error = app.form.as_ref().and_then(|f| f.error.clone()).expect("the form says why");
        assert!(error.contains("PNG, JPEG, GIF"), "it names the accepted formats: {error}");
        assert!(error.contains("8.0 MiB"), "…and the cap: {error}");
        assert!(app.vault.as_ref().unwrap().items().is_empty(), "nothing was stored");

        // (2) a real picture over the cap
        let huge = dir.path().join("huge.png");
        std::fs::write(&huge, vec![0u8; crate::image_field::MAX_BYTES as usize + 1]).unwrap();
        set_form_field(&mut app, "photo_front", huge.to_str().unwrap());
        submit_now(&mut app);
        let error = app.form.as_ref().and_then(|f| f.error.clone()).expect("still refused");
        assert!(error.contains("8.0 MiB"), "the cap is named: {error}");
        assert!(app.vault.as_ref().unwrap().items().is_empty(), "still nothing stored");

        // (3) a path that isn't there — in italian, since every string is
        // localized (the row is named while the form still speaks english)
        set_form_field(&mut app, "photo_front", "/nowhere/at/all.png");
        app.settings.lang = Lang::It;
        submit_now(&mut app);
        let error = app.form.as_ref().and_then(|f| f.error.clone()).expect("still refused");
        assert!(error.contains("impossibile leggere"), "localized: {error}");
    }

    /// An identity item carrying one real picture and one corrupt value (the
    /// placeholder-box fallback), with the keyboard on the docked detail.
    fn image_item_app() -> (App, TempDir, std::path::PathBuf) {
        use crate::model::{FieldType, Item, ItemField, SecretString};
        use base64::{engine::general_purpose::STANDARD as B64, Engine};
        let (mut app, dir) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        let (png, bytes) = write_png(dir.path(), "front.png", 48, 32);
        let mut item = Item::new("identity", "Passport");
        let mut meta = serde_json::Map::new();
        meta.insert(
            "x_image".into(),
            serde_json::json!({ "mime": "image/png", "bytes": bytes.len() }),
        );
        item.fields.push(ItemField {
            key: "photo_front".into(),
            ftype: FieldType::Image,
            label: None,
            value: SecretString::new(B64.encode(&bytes)),
            secret: true,
            extra: meta,
        });
        // a value that doesn't decode still has to draw something
        item.fields.push(ItemField {
            key: "photo_back".into(),
            ftype: FieldType::Image,
            label: None,
            value: SecretString::new("not base64 at all !!"),
            secret: true,
            extra: serde_json::Map::new(),
        });
        app.vault.as_mut().unwrap().content.items.push(item);
        key(&mut app, KeyCode::Enter); // ⏎ on the row -> the detail takes focus
        assert!(app.detail_focused());
        (app, dir, png)
    }

    #[test]
    fn the_copy_key_on_an_image_row_refuses_instead_of_copying_the_payload() {
        let (mut app, _d, _png) = image_item_app();
        key(&mut app, KeyCode::Char('c')); // the copy verb — `c` in both locales
        let status = app.status.as_ref().map(|(_, s)| s.clone()).unwrap_or_default();
        assert_eq!(status, i18n::t(Lang::En, &Key::ImageNotCopyable));
        // …and the same row in italian, where the refusal is worded its own way
        app.settings.lang = Lang::It;
        key(&mut app, KeyCode::Char('c'));
        let status = app.status.as_ref().map(|(_, s)| s.clone()).unwrap_or_default();
        assert_eq!(status, "immagine — non copiabile");
    }

    #[test]
    fn the_open_key_takes_the_picture_on_an_image_row_and_the_website_anywhere_else() {
        use crate::model::{FieldType, ItemField, SecretString};
        let (mut app, _d, _png) = image_item_app();
        // a plain row alongside the two pictures: the SAME key has to keep
        // meaning "this item's website" there.
        app.vault.as_mut().unwrap().content.items[0].fields.push(ItemField {
            key: "notes".into(),
            ftype: FieldType::Text,
            label: None,
            value: SecretString::new("in the drawer"),
            secret: false,
            extra: serde_json::Map::new(),
        });
        let toast = |app: &App| app.status.as_ref().map(|(_, s)| s.clone()).unwrap_or_default();
        // row 1 is the CORRUPT picture, deliberately: `o` proves it reached the
        // image path by refusing there — and nothing is spawned, so the suite
        // never launches a real viewer (the reason the OpenSite tests only ever
        // press `o` on items with no website).
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Char('o'));
        assert_eq!(toast(&app), i18n::t(Lang::En, &Key::ImageUndecodable));
        assert_ne!(
            toast(&app),
            i18n::t(Lang::En, &Key::NoWebsiteField),
            "an image row must not fall through to the website verb"
        );
        // row 2 is text: `o` is the website verb again, exactly as on the list
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Char('o'));
        assert_eq!(toast(&app), i18n::t(Lang::En, &Key::NoWebsiteField));
        // …and both meanings answer on the italian `o`, which is the same key
        app.settings.lang = Lang::It;
        key(&mut app, KeyCode::Up);
        key(&mut app, KeyCode::Char('o'));
        assert_eq!(toast(&app), "immagine illeggibile — non c'è nulla da aprire");
        assert!(app.detail_focused(), "…and the keyboard never left the detail");
    }

    #[test]
    fn the_exported_picture_is_owner_only_and_the_lock_takes_it_back() {
        use base64::{engine::general_purpose::STANDARD as B64, Engine};
        let (mut app, _d, _png) = image_item_app();
        let bytes = B64.decode(field_by_key(&app, "photo_front").value.as_str()).unwrap();
        // the EXPORT is what is asserted, never the spawn: the file has to be
        // written and tracked before any opener exists, and the suite must not
        // launch the machine's real image viewer to prove it.
        let path = app.export_detail_image().expect("the hovered picture is written out");
        // the item id (random per run) rides the name so same-label items never
        // land on one path; here the dir and the label/field/type are asserted
        assert_eq!(path.parent().unwrap(), crate::image_field::export_dir().as_path());
        let fname = path.file_name().unwrap().to_string_lossy().to_string();
        assert!(
            fname.starts_with("passport-photo_front-") && fname.ends_with(".png"),
            "unexpected export name: {fname}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes, "the picture, byte for byte");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "a decrypted secret on disk is owner-only: {mode:o}");
        }
        // the toast that goes with it tells the user what was just created…
        assert!(i18n::t(Lang::En, &Key::ImageOpened).contains("temporary copy"));
        assert!(i18n::t(Lang::It, &Key::ImageOpened).contains("copia temporanea"));
        // …and locking keeps that promise: the cleartext copy is gone
        key_ctrl(&mut app, KeyCode::Char('l'));
        assert!(app.locked.is_some(), "(the vault is locked)");
        assert!(!path.exists(), "the decrypted copy survived the lock: {}", path.display());
    }

    #[test]
    fn the_detail_keybar_advertises_the_open_key_it_actually_answers() {
        let (mut app, _d, _png) = image_item_app();
        app.status = None; // a toast hides the keybar; we want the bar itself
        let bar = keybar_of(&render_sized(&app, 120, 30));
        assert!(bar.contains(" o "), "the detail keybar promises `o`: {bar}");
        assert!(bar.contains("open"), "…under the bind's own label: {bar}");
        // the label is the BIND's, so it stays true of both meanings — the
        // picture under the cursor here, the item's website one row down.
        app.settings.lang = Lang::It;
        let bar = keybar_of(&render_sized(&app, 120, 30));
        assert!(bar.contains(" o ") && bar.contains("apri"), "…and in italian: {bar}");
        for (w, h) in [(1u16, 1u16), (6, 4), (40, 24), (80, 24), (200, 60)] {
            let _ = render_sized(&app, w, h); // must not panic
        }
    }

    #[test]
    fn an_inline_edited_image_row_takes_a_path_and_keeps_the_picture_when_empty() {
        use base64::{engine::general_purpose::STANDARD as B64, Engine};
        let (mut app, dir, _png) = image_item_app();
        let before = field_by_key(&app, "photo_front").value.as_str().to_string();
        key(&mut app, KeyCode::Enter); // ⏎ opens the inline edit on the image row
        assert_eq!(
            app.detail_edit.as_ref().map(|e| e.buffer.as_str()),
            Some(""),
            "an image row starts EMPTY — its base64 never lands in an input box"
        );
        let shown = render_sized(&app, 140, 34);
        assert!(shown.contains("image · PNG"), "it says what it is holding:\n{shown}");
        assert!(shown.contains("leave blank to keep"), "…and what confirming empty means");
        key(&mut app, KeyCode::Enter); // confirm empty
        assert_eq!(field_by_key(&app, "photo_front").value.as_str(), before, "the picture survives");

        // a path REPLACES it, through the same validation
        let (other, other_bytes) = write_png(dir.path(), "new-front.png", 20, 20);
        key(&mut app, KeyCode::Enter);
        retype(&mut app, 0, other.to_str().unwrap());
        // the path is typed in the clear — it is a path, not a secret
        assert!(render_sized(&app, 140, 34).contains("new-front.png"));
        key(&mut app, KeyCode::Enter); // confirm
        let front = field_by_key(&app, "photo_front");
        assert_eq!(B64.decode(front.value.as_str()).unwrap(), other_bytes, "replaced with the new file");
        assert_eq!(front.extra["x_image"]["bytes"], other_bytes.len() as u64, "fresh metadata");

        // a path that can't be imported changes nothing and warns
        let kept = front.value.as_str().to_string();
        key(&mut app, KeyCode::Enter);
        retype(&mut app, 0, "/nowhere/at/all.png");
        key(&mut app, KeyCode::Enter);
        assert_eq!(field_by_key(&app, "photo_front").value.as_str(), kept, "refused: nothing changed");
        assert!(app.status.as_ref().is_some_and(|(_, s)| s.contains("cannot read")));
    }

    #[test]
    fn a_dash_removes_an_imported_photo_from_the_detail_and_from_the_form() {
        // an image row keeps its picture when confirmed blank (it inherits the
        // secret rows' contract), so blank cannot also mean "remove" — without a
        // word of its own an imported photo could only ever be REPLACED.
        let (mut app, _d, _png) = image_item_app();
        // the hint says so, in both languages
        let shown = render_sized(&app, 140, 34);
        assert!(shown.contains("ACTIVITY"), "(the detail is on screen)");
        key(&mut app, KeyCode::Enter); // ⏎ opens the inline edit on photo_front
        assert!(
            render_sized(&app, 180, 34).contains("«-» removes it"),
            "the inline hint offers the removal"
        );
        app.settings.lang = Lang::It;
        assert!(render_sized(&app, 180, 34).contains("«-» la rimuove"), "…localized");
        app.settings.lang = Lang::En;

        retype(&mut app, 0, "-");
        key(&mut app, KeyCode::Enter); // confirm
        let item = &app.vault.as_ref().unwrap().items()[0];
        assert!(
            item.fields.iter().all(|f| f.key != "photo_front"),
            "the field is off the item, not blanked"
        );
        assert_eq!(item.fields.len(), 1, "…and only that one went");
        assert!(
            app.status.as_ref().is_some_and(|(_, s)| s.contains("removed")),
            "…with a toast saying so: {:?}",
            app.status
        );
        let reopened = Vault::open(&app.paths.vaults_dir, "bruno", "pw").expect("reopens");
        assert!(
            reopened.items()[0].fields.iter().all(|f| f.key != "photo_front"),
            "it reached the file"
        );

        // the template slot simply goes back to being unfilled: the edit form
        // offers it again, and `-` there removes the OTHER one the same way
        key_ctrl(&mut app, KeyCode::Char('e'));
        let front_row = i18n::t(Lang::En, &Key::FieldLabel("photo_front"));
        assert!(
            app.form.as_ref().unwrap().fields.iter().any(|f| f.name == front_row),
            "the emptied slot is offered again"
        );
        set_form_field(&mut app, "photo_back", "-");
        submit_now(&mut app);
        assert!(app.form.is_none(), "the form closed — `-` is not a refused import");
        let item = &app.vault.as_ref().unwrap().items()[0];
        assert!(item.fields.is_empty(), "the edit form removed the second photo too");
    }

    #[test]
    fn a_dash_in_the_add_form_just_leaves_the_picture_slot_empty() {
        // nothing is stored there yet, so there is nothing to remove — and it
        // must not be mistaken for a path either (that would refuse the submit).
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        open_add_form_for(&mut app, "identity");
        set_form_field(&mut app, "label", "Passport");
        set_form_field(&mut app, "photo_front", "-");
        submit_now(&mut app);
        assert!(app.form.is_none(), "the item was accepted");
        let item = &app.vault.as_ref().unwrap().items()[0];
        assert_eq!(item.label, "Passport");
        assert!(item.fields.iter().all(|f| f.key != "photo_front"), "the slot stayed empty");
        // …while `-` on an ordinary row is just a value one character long
        key_ctrl(&mut app, KeyCode::Char('e'));
        set_form_field(&mut app, "fiscal_code", "-");
        submit_now(&mut app);
        assert_eq!(field_by_key(&app, "fiscal_code").value.as_str(), "-", "a dash IS a value here");
    }

    #[test]
    fn locking_the_vault_drops_every_decoded_picture() {
        // the render caches are the only place a vault's plaintext image data
        // outlives the Vault whose key was just zeroized.
        let (mut app, _d, _png) = image_item_app();
        let _ = render_sized(&app, 120, 40); // draws the thumbnails
        assert!(!app.image_cells.borrow().is_empty(), "a decoded picture was memoized");
        key_ctrl(&mut app, KeyCode::Char('l')); // lock
        assert!(app.locked.is_some(), "the full-screen lock is up");
        assert!(app.image_cells.borrow().is_empty(), "no decoded picture outlives the vault");
        assert!(
            app.icon_protocols.borrow().keys().all(|k| !k.starts_with(crate::app::IMAGE_KEY_PREFIX)),
            "…nor any render protocol built from one"
        );
    }

    #[test]
    fn an_item_with_images_never_panics_at_any_size() {
        const SIZES: [(u16, u16); 5] = [(1, 1), (6, 4), (40, 24), (80, 24), (200, 60)];
        let (mut app, _d, _png) = image_item_app();
        // the cursor on both image rows (one decodable, one not), at every size
        for _ in 0..3 {
            for (w, h) in SIZES {
                let _ = render_sized(&app, w, h); // must not panic
            }
            key(&mut app, KeyCode::Down);
        }
        // …with the row revealed, and with the inline path edit open and typed
        app.detail_reveal = true;
        key(&mut app, KeyCode::Enter);
        for ch in "/a/very/long/path/to/an/image/file/that/is/wider/than/the/pane.png".chars() {
            key(&mut app, KeyCode::Char(ch));
        }
        for (w, h) in SIZES {
            let _ = render_sized(&app, w, h);
        }
        key(&mut app, KeyCode::Esc); // cancel the inline edit
        // …and back on the list, where the row's subtitle must not be base64
        key(&mut app, KeyCode::Esc);
        assert_eq!(app.effective_focus(), Focus::Vaults, "the list has the keyboard");
        assert!(app.vault.is_some(), "…and the vault is still open");
        for (w, h) in SIZES {
            let _ = render_sized(&app, w, h);
        }
        let text = render_sized(&app, 120, 40);
        assert!(!text.contains("iVBORw0"), "no PNG base64 anywhere on the screen:\n{text}");
    }


    /// A full identity item — nine text rows plus the two document photos —
    /// with the keyboard on its docked detail. The tallest item the built-ins
    /// can make, which is what pushes the ACTIVITY block onto the pane's edge.
    fn tall_identity_app() -> (App, TempDir) {
        use crate::model::{FieldType, Item, ItemField, SecretString};
        use base64::{engine::general_purpose::STANDARD as B64, Engine};
        let (mut app, dir) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        let (_png, bytes) = write_png(dir.path(), "front.png", 48, 32);
        let mut item = Item::new("identity", "Passport");
        for (k, v) in [
            ("full_name", "Bruno Galimi"),
            ("birth_date", "1990-01-01"),
            ("fiscal_code", "GLMBRN90A01H501X"),
            ("document_type", "passport"),
            ("document_number", "AA1234567"),
            ("issued", "2020-01-01"),
            ("expires", "2030-01-01"),
            ("url", "https://poliziadistato.it"),
            ("notes", "in the drawer"),
        ] {
            item.fields.push(ItemField {
                key: k.into(),
                ftype: FieldType::Text,
                label: None,
                value: SecretString::new(v),
                secret: false,
                extra: serde_json::Map::new(),
            });
        }
        for k in ["photo_front", "photo_back"] {
            let mut meta = serde_json::Map::new();
            meta.insert(
                "x_image".into(),
                serde_json::json!({ "mime": "image/png", "bytes": bytes.len() }),
            );
            item.fields.push(ItemField {
                key: k.into(),
                ftype: FieldType::Image,
                label: None,
                value: SecretString::new(B64.encode(&bytes)),
                secret: true,
                extra: meta,
            });
        }
        app.vault.as_mut().unwrap().content.items.push(item);
        key(&mut app, KeyCode::Enter); // ⏎ on the row -> the detail takes focus
        assert!(app.detail_focused());
        (app, dir)
    }

    #[test]
    fn a_tall_item_never_lets_the_activity_block_eat_the_keybar() {
        // eleven field rows leave the ACTIVITY rule sitting ON the pane's last
        // row: its text line, one further down, is OUTSIDE the pane — and the
        // row outside the pane's bottom is the keybar, the one row that must
        // stay readable whatever the item is. The ROW is what the geometry is
        // about (24 high), so the width is only ever "wide enough for the whole
        // detail bar plus slack to see a bleed in" — 84 columns of chips now
        // that `o` is advertised there too.
        let (mut app, _d) = tall_identity_app();
        app.status = None; // a toast hides the keybar; we want the bar itself
        let text = render_sized(&app, 100, 24);
        let bar = keybar_of(&text);
        for chip in ["copy", "edit", "delete", "star", "open", "list"] {
            assert!(bar.contains(chip), "the keybar lost '{chip}':\n{text}");
        }
        // the bar ends at its last chip: the keybar is painted over the body,
        // so a spill only shows where the chips run out — past `list`, in the
        // columns the detail pane sits over.
        assert!(
            bar.trim_end().ends_with("list"),
            "the ACTIVITY line bled onto the keybar row:\n{bar}"
        );
        // …and the pane still says its piece wherever there IS room for it
        let roomy = render_sized(&app, 120, 40);
        assert!(roomy.contains("ACTIVITY") && roomy.contains("added "), "clipped, not dropped");
        assert!(roomy.contains("IMAGES"), "…with the picture strip drawn too");
        for chip in ["copy", "edit", "delete"] {
            assert!(keybar_of(&roomy).contains(chip), "the tall keybar is intact too");
        }
        // …at every size, cursor walked down the whole item
        for _ in 0..12 {
            for (w, h) in [(1u16, 1u16), (6, 4), (40, 24), (80, 24), (200, 60)] {
                let _ = render_sized(&app, w, h); // must not panic
            }
            key(&mut app, KeyCode::Down);
        }
    }

    // ---- the Memorandum (storage) ---------------------------------------

    #[test]
    fn a_memo_message_lands_in_the_vault_file_through_the_full_mutation_path() {
        use base64::{engine::general_purpose::STANDARD as B64, Engine};
        use crate::memo::{MemoImage, MemoMessage};
        let (mut app, dir) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        let (counter, fingerprint) = {
            let c = &app.vault.as_ref().unwrap().content;
            (c.sync_counter(), c.content_fingerprint())
        };
        assert!(app.append_memo_message(MemoMessage::new("the safe code is on the fridge", None)));
        // a photo message goes through the very same import an image field
        // does — 8 MiB cap, real png/jpeg/gif, byte count kept
        let (png, bytes) = write_png(dir.path(), "receipt.png", 24, 24);
        let imported = crate::image_field::import(&png).expect("a real png imports");
        let photo = MemoImage::from_import(&imported, Some("the receipt".to_string()));
        assert!(app.append_memo_message(MemoMessage::new("", Some(photo))));

        let content = &app.vault.as_ref().unwrap().content;
        assert_eq!(content.memo_len(), 2);
        assert_eq!(content.sync_counter(), counter + 2, "one bump per append");
        assert_ne!(content.content_fingerprint(), fingerprint, "a memo edit IS content divergence");

        // …and it is on the disk, inside the one encrypted blob
        let reopened = Vault::open(&app.paths.vaults_dir, "bruno", "pw").expect("reopens");
        let stored = reopened.content.memo_messages();
        assert_eq!(stored.len(), 2);
        assert_eq!(stored[0].text, "the safe code is on the fridge");
        assert!(stored[0].ts.ends_with('Z') && stored[0].id.len() == 36, "stamped and identified");
        let photo = stored[1].image.as_ref().expect("the photo travelled with it");
        assert_eq!(stored[1].text, "", "a photo message may carry no words");
        assert_eq!(photo.mime, "image/png");
        assert_eq!(photo.bytes, bytes.len() as u64, "the ORIGINAL file length");
        assert_eq!(photo.caption.as_deref(), Some("the receipt"));
        assert_eq!(B64.decode(&photo.b64).unwrap(), bytes, "the picture, byte for byte");
        assert!(reopened.items().is_empty(), "…and the item store never moved");
    }

    #[test]
    fn a_refused_save_takes_the_memo_message_back_off() {
        // wedge the save: `save()` writes `<file>.<pid>.tmp` next to the vault
        // before renaming it over, so a DIRECTORY on that exact path fails the
        // write every time — no permissions, no root exemption.
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        assert!(app.append_memo_message(crate::memo::MemoMessage::new("first", None)));
        let (counter, fingerprint) = {
            let c = &app.vault.as_ref().unwrap().content;
            (c.sync_counter(), c.content_fingerprint())
        };
        let path = crate::vault::vault_path(&app.paths.vaults_dir, "bruno");
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let wedge = path.with_file_name(format!("{name}.{}.tmp", std::process::id()));
        std::fs::create_dir(&wedge).expect("wedges the save");

        assert!(!app.append_memo_message(crate::memo::MemoMessage::new("lost", None)));
        let content = &app.vault.as_ref().unwrap().content;
        assert_eq!(content.memo_len(), 1, "the refused message is off the thread");
        assert_eq!(content.memo_messages()[0].text, "first");
        assert_eq!(
            content.content_fingerprint(),
            fingerprint,
            "the rollback is exact — the content is what the disk still holds"
        );
        assert_eq!(
            app.status.as_ref().map(|(_, s)| s.clone()).unwrap_or_default(),
            i18n::t(Lang::En, &Key::SaveFailed),
            "…and the user is told nothing was saved"
        );
        let _ = counter; // the stamp is deliberately NOT rolled back (as everywhere)
        std::fs::remove_dir(&wedge).unwrap();
        // the file is untouched: it still holds exactly the first message
        let reopened = Vault::open(&app.paths.vaults_dir, "bruno", "pw").expect("reopens");
        assert_eq!(reopened.content.memo_len(), 1);
    }

    #[test]
    fn a_memo_append_is_refused_while_the_open_vault_is_being_rekeyed() {
        // the passwd worker is rewriting that very file under a NEW key: a save
        // from here races it, and the last rename wins.
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        let counter = app.vault.as_ref().unwrap().content.sync_counter();
        let _worker = app.pretend_rekeying("bruno"); // held: the worker is out
        assert!(!app.append_memo_message(crate::memo::MemoMessage::new("later", None)));
        let content = &app.vault.as_ref().unwrap().content;
        assert_eq!(content.memo_len(), 0, "nothing was written");
        assert_eq!(content.sync_counter(), counter, "…and nothing was stamped");
        assert_eq!(
            app.status.as_ref().map(|(_, s)| s.clone()).unwrap_or_default(),
            i18n::t(Lang::En, &Key::PasswdBusy("bruno")),
            "the guard says which vault is busy"
        );
    }

    // ---- the Memorandum index (local semantic search) --------------------

    /// Ticks until no embedding worker is out and NEITHER index has anything
    /// stale left. Bounded: a wedged worker fails the test instead of hanging
    /// the suite.
    fn pump_rag(app: &mut App) {
        for _ in 0..600 {
            app.on_tick();
            let tag = app.rag.tag();
            let texts = app.item_index_texts();
            let settled = !app.rag.busy()
                && !app.rag.has_deferred()
                && app.vault.as_ref().is_some_and(|v| {
                    crate::rag::memo_pending(&v.content.memo_messages(), tag).is_empty()
                        && crate::rag::items_pending(&texts, tag, &v.content.item_vectors())
                            .is_empty()
                });
            if settled {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("the embedding worker never settled");
    }

    #[test]
    fn memo_vectors_are_embedded_on_a_worker_and_land_inside_the_encrypted_vault() {
        use crate::memo::MemoMessage;
        use crate::rag::{fingerprint, memo_index, MockEmbedder, EMBED_DIM, MOCK_TAG};
        const SIZES: [(u16, u16); 5] = [(1, 1), (6, 4), (40, 24), (80, 24), (200, 60)];
        let (mut app, _d) = app_with(&["bruno"]);
        app.use_mock_rag(); // no model, no network, no display
        open_first_vault(&mut app);
        assert!(app.append_memo_message(MemoMessage::new("le chiavi sono sotto il tappeto", None)));
        assert!(app.append_memo_message(MemoMessage::new("la ricetta della torta di mele", None)));
        let counter = app.vault.as_ref().unwrap().content.sync_counter();

        app.rag_index_memo();
        assert!(app.rag.busy(), "the embedding runs on a worker, not on the key press");
        pump_rag(&mut app);

        let content = &app.vault.as_ref().unwrap().content;
        assert_eq!(content.sync_counter(), counter + 1, "ONE save for the whole batch");
        let stored = content.memo_messages();
        assert_eq!(stored[0].vec.as_ref().map(Vec::len), Some(EMBED_DIM));
        assert_eq!(
            stored[0].vec.as_ref().unwrap(),
            &MockEmbedder::vector_of("passage: le chiavi sono sotto il tappeto"),
            "the stored vector is the PASSAGE side of the model"
        );
        assert_eq!(
            stored[0].vfp.as_deref(),
            Some(fingerprint(MOCK_TAG, "le chiavi sono sotto il tappeto").as_str()),
            "…with the fingerprint that will tell an edit from an unchanged message"
        );

        // …and it is on the disk INSIDE the one encrypted blob. An embedding is
        // partially invertible — the text can be approximated back out of it —
        // so it is exactly as sensitive as the message and may never sit in a
        // cleartext index next to the vault.
        let reopened = Vault::open(&app.paths.vaults_dir, "bruno", "pw").expect("reopens");
        assert_eq!(memo_index(&reopened.content.memo_messages()).len(), 2, "both are searchable");
        let raw = std::fs::read(crate::vault::vault_path(&app.paths.vaults_dir, "bruno")).unwrap();
        for marker in [&b"\"vec\""[..], b"tappeto", b"passage: "] {
            assert!(
                !raw.windows(marker.len()).any(|w| w == marker),
                "{:?} is readable in the vault FILE",
                String::from_utf8_lossy(marker)
            );
        }
        // …and the thread still draws at every size with the vectors on it
        for (w, h) in SIZES {
            let _ = render_sized(&app, w, h); // must not panic
        }
    }

    #[test]
    fn an_unchanged_thread_costs_nothing_and_only_the_stale_message_is_re_embedded() {
        use crate::memo::MemoMessage;
        use crate::rag::{fingerprint, MockEmbedder, EMBED_DIM, MOCK_TAG};
        let (mut app, _d) = app_with(&["bruno"]);
        app.use_mock_rag();
        open_first_vault(&mut app);
        assert!(app.append_memo_message(MemoMessage::new("le chiavi", None)));
        assert!(app.append_memo_message(MemoMessage::new("la cassaforte", None)));
        app.rag_index_memo();
        pump_rag(&mut app);
        let counter = app.vault.as_ref().unwrap().content.sync_counter();
        let untouched = app.vault.as_ref().unwrap().content.memo_messages()[0]
            .vec
            .clone()
            .expect("the first message is embedded");

        // an up-to-date conversation starts no worker and saves nothing
        app.rag_index_memo();
        assert!(!app.rag.busy(), "nothing stale, nothing to do");
        assert_eq!(app.vault.as_ref().unwrap().content.sync_counter(), counter);

        // now make the SECOND message look edited: its fingerprint no longer
        // matches its text, which is the only staleness signal there is
        let id = app.vault.as_ref().unwrap().content.memo_messages()[1].id.clone();
        app.vault
            .as_mut()
            .unwrap()
            .content
            .set_memo_vec(&id, &[0.0; EMBED_DIM], "a fingerprint from before the edit")
            .expect("the slot was overwritten");
        app.rag_index_memo();
        assert!(app.rag.busy(), "a stale message queues a pass");
        pump_rag(&mut app);

        let stored = app.vault.as_ref().unwrap().content.memo_messages();
        assert_eq!(app.vault.as_ref().unwrap().content.sync_counter(), counter + 1);
        assert_eq!(stored[0].vec.as_ref(), Some(&untouched), "the fresh message was not touched");
        assert_eq!(
            stored[1].vec.as_ref().unwrap(),
            &MockEmbedder::vector_of("passage: la cassaforte"),
            "…and the stale one was computed again"
        );
        assert_eq!(stored[1].vfp.as_deref(), Some(fingerprint(MOCK_TAG, "la cassaforte").as_str()));
    }

    #[test]
    fn a_finished_batch_waits_for_the_rekey_that_is_rewriting_the_same_file() {
        // the passwd worker ends by renaming its own blob over this file; an
        // index save landing in between is a lost re-encryption. Vectors are
        // derived data — they can always wait.
        use crate::memo::MemoMessage;
        let (mut app, _d) = app_with(&["bruno"]);
        app.use_mock_rag();
        open_first_vault(&mut app);
        assert!(app.append_memo_message(MemoMessage::new("il codice della cassaforte", None)));
        let counter = app.vault.as_ref().unwrap().content.sync_counter();

        app.rag_index_memo();
        let worker = app.pretend_rekeying("bruno"); // held: the re-key is out
        for _ in 0..600 {
            app.on_tick();
            if app.rag.has_deferred() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(app.rag.has_deferred(), "the batch waited instead of racing the rename");
        let content = &app.vault.as_ref().unwrap().content;
        assert_eq!(content.sync_counter(), counter, "nothing was written meanwhile");
        assert!(content.memo_messages()[0].vec.is_none());

        drop(worker); // the re-key ends; the same tick frees the guard and writes
        pump_rag(&mut app);
        let content = &app.vault.as_ref().unwrap().content;
        assert_eq!(content.sync_counter(), counter + 1, "…and then it landed, once");
        assert!(content.memo_messages()[0].vec.is_some());
    }

    // ---- the Memorandum on screen ----------------------------------------

    /// The single-key verb that opens the Memorandum in `lang`, as `settings`
    /// really binds it (the default table unless a test says otherwise).
    fn memo_key_of(settings: &crate::settings::Settings, lang: Lang) -> char {
        crate::keymap::effective_binds(Page::Vault, settings)
            .iter()
            .find(|b| b.verb == crate::keymap::Verb::Memo)
            .expect("the vault page binds the memo verb")
            .key(lang)
    }

    /// The same, on the stock keymap.
    fn memo_key(lang: Lang) -> char {
        memo_key_of(&crate::settings::Settings::default(), lang)
    }

    /// An open vault with the Memorandum showing.
    fn memo_app() -> (App, TempDir) {
        let (mut app, dir) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        key(&mut app, KeyCode::Char(memo_key(Lang::En)));
        assert!(app.memo_open, "the memo verb opened the chat");
        (app, dir)
    }

    /// Types a line into the composer and sends it.
    fn send(app: &mut App, text: &str) {
        for ch in text.chars() {
            key(app, KeyCode::Char(ch));
        }
        key(app, KeyCode::Enter);
    }

    /// The same, several lines at a time: the newline chord between them, one
    /// plain ⏎ at the end — one message, as many lines as were typed.
    fn send_lines(app: &mut App, lines: &[&str]) {
        for (i, line) in lines.iter().enumerate() {
            if i > 0 {
                key_shift(app, KeyCode::Enter);
            }
            for ch in line.chars() {
                key(app, KeyCode::Char(ch));
            }
        }
        key(app, KeyCode::Enter);
    }

    #[test]
    fn the_memo_verb_opens_the_chat_and_esc_leaves_it() {
        let (mut app, _d) = memo_app();
        let text = render_sized(&app, 100, 30);
        assert!(text.contains("memorandum"), "the title rule names the view:\n{text}");
        assert!(text.contains("nothing written yet"), "an empty thread says so");
        assert!(!text.contains("IMAGES"), "the docked detail is gone — this is the whole body");
        // Esc on an empty composer leaves; the vault page comes back intact
        key(&mut app, KeyCode::Esc);
        assert!(!app.memo_open, "Esc left the memo");
        assert_eq!(app.page, Page::Vault, "…back to the vault, not out of it");
        assert!(app.vault.is_some());
        // …and the registry command is the other way in
        typed(&mut app, "memo");
        key(&mut app, KeyCode::Enter);
        assert!(app.memo_open, "the `memo` command opens it too");
    }

    #[test]
    fn the_italian_memo_key_is_the_localized_one() {
        // `m` is modifica→EditItem in `it`: pressing it there must open the EDIT
        // form, and the locale's own letter must open the memo.
        let (mut app, _d) = app_with(&["bruno"]);
        app.settings.lang = Lang::It;
        open_first_vault(&mut app);
        push_item(&mut app, "email", "Amazon", &[("email", "bruno@gmail.com", false)]);
        key(&mut app, KeyCode::Char('m'));
        assert!(app.form.is_some() && !app.memo_open, "`m` is still modifica in Italian");
        key(&mut app, KeyCode::Esc); // close the edit form
        key(&mut app, KeyCode::Char(memo_key(Lang::It)));
        assert!(app.memo_open, "the Italian memo key opened the chat");
        assert_ne!(memo_key(Lang::It), 'm', "…and it is not the one edit already owns");
    }

    #[test]
    fn a_sent_message_reaches_the_vault_file_and_shows_in_the_thread() {
        let (mut app, _d) = memo_app();
        send(&mut app, "the safe code is on the fridge");
        assert!(app.memo_input.is_empty(), "the composer emptied on send");
        assert_eq!(app.memo_len(), 1);
        // it is on the screen, oldest-first, with its time column
        let text = render_sized(&app, 100, 30);
        assert!(text.contains("the safe code is on the fridge"), "the message is in the thread:\n{text}");
        // …and it is on the DISK: a fresh open of the same vault finds it
        let reopened = Vault::open(&app.paths.vaults_dir, "bruno", "pw").expect("reopens");
        assert_eq!(reopened.content.memo_len(), 1);
        assert_eq!(reopened.content.memo_messages()[0].text, "the safe code is on the fridge");
        // a blank line is not a message
        key(&mut app, KeyCode::Char(' '));
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.memo_len(), 1, "whitespace alone sends nothing");
    }

    #[test]
    fn every_printable_key_is_text_in_the_composer_never_a_verb() {
        let (mut app, _d) = memo_app();
        // `:` `/` `?` are the command surface everywhere else; `q` locks; `a`
        // adds an item. In here they are all just letters.
        send(&mut app, "q: a/ ?");
        assert!(app.palette.is_none() && app.form.is_none(), "nothing was dispatched");
        assert!(app.memo_open && app.vault.is_some(), "…and the vault stayed open");
        assert_eq!(app.vault.as_ref().unwrap().content.memo_messages()[0].text, "q: a/ ?");
        // a modifier chord is never typed as its bare letter
        key_ctrl(&mut app, KeyCode::Char('w'));
        assert!(app.memo_input.is_empty(), "Ctrl+W left no 'w' in the message");
    }

    #[test]
    fn esc_clears_the_composer_before_it_leaves() {
        let (mut app, _d) = memo_app();
        for ch in "half a thought".chars() {
            key(&mut app, KeyCode::Char(ch));
        }
        key(&mut app, KeyCode::Backspace);
        assert_eq!(app.memo_input.as_str(), "half a though");
        key(&mut app, KeyCode::Esc);
        assert!(app.memo_input.is_empty(), "the first Esc wiped the line");
        assert!(app.memo_open, "…and kept the view");
        assert_eq!(app.memo_len(), 0, "nothing was sent");
        key(&mut app, KeyCode::Esc);
        assert!(!app.memo_open, "the second Esc left");
    }

    #[test]
    fn attaching_a_photo_stores_it_with_its_caption() {
        let (mut app, dir) = memo_app();
        let (png, bytes) = write_png(dir.path(), "receipt.png", 40, 24);
        key_ctrl(&mut app, KeyCode::Char('p'));
        let form = app.form.as_ref().expect("the attach-photo modal opened");
        assert_eq!(form.title, i18n::t(Lang::En, &Key::MemoPhotoTitle));
        assert!(form.fields.iter().all(|f| !f.secret), "a path is not a secret");
        // a path that is not an image is refused by the IMPORT rules, localized
        let bogus = dir.path().join("not-an-image.png");
        std::fs::write(&bogus, b"this is not a png").unwrap();
        app.form.as_mut().unwrap().fields[0].value = bogus.to_string_lossy().to_string().into();
        submit_now(&mut app);
        assert!(app.form.is_some(), "the modal stayed open on the refusal");
        assert!(
            app.form.as_ref().unwrap().error.as_deref().unwrap_or("").contains("supported"),
            "the image machinery's own message: {:?}",
            app.form.as_ref().unwrap().error
        );
        assert_eq!(app.memo_len(), 0, "nothing was sent");
        // the real one, with a caption (typing clears the error; so does this)
        let form = app.form.as_mut().unwrap();
        form.fields[0].value = png.to_string_lossy().to_string().into();
        form.fields[1].value = "the receipt".to_string().into();
        form.error = None;
        submit_now(&mut app);
        assert!(app.form.is_none(), "the modal closed on the send");
        let msgs = app.vault.as_ref().unwrap().content.memo_messages();
        assert_eq!(msgs.len(), 1);
        let img = msgs[0].image.as_ref().expect("the photo landed");
        assert_eq!(img.mime, "image/png");
        assert_eq!(img.bytes, bytes.len() as u64, "the ORIGINAL file length");
        assert_eq!(img.caption.as_deref(), Some("the receipt"));
        assert_eq!(msgs[0].text, "", "a photo message needs no words");
        // it reached the disk, and the thread names it without ever showing it
        let reopened = Vault::open(&app.paths.vaults_dir, "bruno", "pw").expect("reopens");
        assert_eq!(reopened.content.memo_messages()[0].image.as_ref().unwrap().mime, "image/png");
        let text = render_sized(&app, 100, 30);
        assert!(text.contains("the receipt"), "the caption sits under the photo:\n{text}");
        assert!(text.contains("image · PNG"), "…and the placeholder line names it");
        assert!(!text.contains("iVBORw0"), "no base64 anywhere on the screen:\n{text}");
    }

    #[test]
    fn a_caption_less_photo_is_accepted() {
        // the blanket "no empty field" check would refuse a perfectly good photo
        let (mut app, dir) = memo_app();
        let (png, _) = write_png(dir.path(), "shot.png", 20, 20);
        key_ctrl(&mut app, KeyCode::Char('p'));
        app.form.as_mut().unwrap().fields[0].value = png.to_string_lossy().to_string().into();
        submit_now(&mut app);
        assert!(app.form.is_none(), "no caption is not a missing field");
        let msgs = app.vault.as_ref().unwrap().content.memo_messages();
        assert!(msgs[0].image.as_ref().unwrap().caption.is_none());
        // …while no PATH at all is
        key_ctrl(&mut app, KeyCode::Char('p'));
        submit_now(&mut app);
        assert!(app.form.as_ref().is_some_and(|f| f.error.is_some()), "the photo row is required");
    }

    #[test]
    fn locking_wipes_the_composer_and_closes_the_memo() {
        let (mut app, dir) = memo_app();
        let (png, _) = write_png(dir.path(), "shot.png", 20, 20);
        key_ctrl(&mut app, KeyCode::Char('p'));
        app.form.as_mut().unwrap().fields[0].value = png.to_string_lossy().to_string().into();
        submit_now(&mut app);
        let _ = render_sized(&app, 100, 30); // decodes the photo into the caches
        assert!(!app.image_cells.borrow().is_empty(), "the photo was memoized");
        for ch in "never sent".chars() {
            key(&mut app, KeyCode::Char(ch));
        }
        key_ctrl(&mut app, KeyCode::Char('l')); // lock
        assert!(app.locked.is_some() && app.vault.is_none());
        assert!(!app.memo_open, "the memo went with the vault");
        assert!(app.memo_input.is_empty(), "…and so did the half-written line");
        assert!(app.image_cells.borrow().is_empty(), "no decoded memo photo outlives it");
        assert!(
            app.icon_protocols
                .borrow()
                .keys()
                .all(|k| !k.starts_with(crate::app::MEMO_IMAGE_KEY_PREFIX)),
            "…nor any render protocol built from one"
        );
        // closing the vault does the same
        key(&mut app, KeyCode::Esc); // leave the lock screen
        open_first_vault(&mut app);
        key(&mut app, KeyCode::Char(memo_key(Lang::En)));
        for ch in "also never sent".chars() {
            key(&mut app, KeyCode::Char(ch));
        }
        key(&mut app, KeyCode::Char('q')); // 'q' is text in here…
        assert!(app.memo_open, "…so it did NOT lock");
        key(&mut app, KeyCode::Esc);
        key(&mut app, KeyCode::Esc);
        key(&mut app, KeyCode::Char('q')); // …but it does out here
        assert_eq!(app.page, Page::Access);
        assert!(!app.memo_open && app.memo_input.is_empty(), "close wiped the composer too");
    }

    #[test]
    fn the_memo_keybar_advertises_exactly_what_the_composer_does() {
        let (mut app, _d) = memo_app();
        app.status = None; // a toast hides the keybar; we want the bar itself
        let bar = keybar_of(&render_sized(&app, 110, 30));
        for chip in ["send", "copy", "photo", "scroll", "close"] {
            assert!(bar.contains(chip), "the memo keybar lost '{chip}':\n{bar}");
        }
        for chip in ["⏎", "Ctrl+Y", "Ctrl+P", "PgUp", "Esc"] {
            assert!(bar.contains(chip), "the memo keybar lost the '{chip}' key:\n{bar}");
        }
        // a chip is a promise: the list's item verbs do NOT fire in here, so
        // none of their labels may sit on the bar (the copy above is the memo's
        // own Ctrl+Y, a whole-message copy — not the list's field copy)
        for chip in ["star", "lock", "filter", "category"] {
            assert!(!bar.contains(chip), "the memo keybar promises '{chip}', which is text here:\n{bar}");
        }
        // …and it is localized like everything else
        app.settings.lang = Lang::It;
        let bar = keybar_of(&render_sized(&app, 110, 30));
        for chip in ["invia", "copia", "foto", "scorri", "chiudi"] {
            assert!(bar.contains(chip), "the Italian memo keybar lost '{chip}':\n{bar}");
        }
    }

    #[test]
    fn the_idle_memo_view_is_byte_stable_across_frames() {
        // Fix 1, the whole of it: with the memo open and no key pressed, two
        // frames at DIFFERENT counters render byte-for-byte the same — the
        // composer caret has stopped blinking and the animated backdrop is
        // frozen — which is exactly what lets the terminal keep a native mouse
        // selection stretched across the thread instead of dropping it mid-drag.
        let (mut app, _d) = memo_app();
        send(&mut app, "the safe code is on the fridge");
        send_lines(&mut app, &["shopping:", "milk", "eggs", "coffee"]);
        // the very conditions that made the OLD view churn every frame: the
        // backdrop on and animating (its rain moves glyphs, which `render_sized`
        // sees), reduce-motion off, no toast hiding the keybar.
        app.settings.backdrop = crate::settings::Backdrop::On;
        app.settings.backdrop_fx = crate::settings::BackdropFx::Rain;
        app.settings.reduce_motion = false;
        app.status = None;
        app.frame = 0; // caret ON, rain unfallen
        let a = render_sized(&app, 90, 20);
        app.frame = 100; // the caret would have blinked and the rain fallen since
        let b = render_sized(&app, 90, 20);
        assert_eq!(a, b, "the idle memo view changed between frames — a selection would drop");
        // and the caret is genuinely THERE (steady), not simply gone both times:
        // the block sits right before the composer's hint (the top-bar has its
        // own ▌, so the caret has to be pinned to the composer to prove it out)
        assert!(a.contains("▌write a message"), "the composer still shows where typing goes:\n{a}");
    }

    #[test]
    fn the_memo_caret_shows_at_every_frame_instead_of_blinking() {
        // the steady caret, proven at frames where the old blink was OFF as well
        // as ON: it is present in ALL of them now, so it never winks out from
        // under a selection (`cursor_on` flips every 4 frames).
        let (mut app, _d) = memo_app();
        app.settings.reduce_motion = false;
        // the block is pinned to the composer's hint, so a stray top-bar ▌ can't
        // stand in for it (frames 4 and 12 are where the OLD blink was OFF)
        for frame in [0u64, 4, 8, 12] {
            app.frame = frame;
            let text = render_sized(&app, 80, 16);
            assert!(text.contains("▌write a message"), "the composer caret vanished at frame {frame}:\n{text}");
        }
        // …and the search bar's caret is steady too
        key_ctrl(&mut app, KeyCode::Char('f'));
        for frame in [2u64, 6] {
            app.frame = frame;
            let text = render_sized(&app, 80, 16);
            assert!(text.contains("▌search"), "the search caret vanished at frame {frame}:\n{text}");
        }
    }

    #[test]
    fn up_down_pick_a_message_and_the_band_shows_what_a_copy_takes() {
        // Fix 2's navigation: bare ↑/↓ over an empty composer walk a copy-band
        // across the thread (older/newer), the WHOLE of a multi-line message is
        // banded, and the payload a copy would lift is that message's every line.
        let (mut app, _d) = memo_app();
        send(&mut app, "first note");
        send_lines(&mut app, &["line one", "line two", "line three"]);
        send(&mut app, "last note");
        assert_eq!(app.memo_sel, None, "nothing is picked until a key asks");
        let newest = app.vault.as_ref().unwrap().content.memo_messages().len() - 1;
        // the first ↑ enters the selection at the newest message and bands it
        key(&mut app, KeyCode::Up);
        assert_eq!(app.memo_sel, Some(newest), "the first ↑ lands on the newest");
        let banded = banded_rows(&app, 100, 24);
        assert!(banded.iter().any(|r| r.contains("last note")), "the newest wears the band:\n{banded:?}");
        // ↑ again walks to the multi-line message; every one of its rows is banded
        key(&mut app, KeyCode::Up);
        let banded = banded_rows(&app, 100, 24);
        assert!(banded.iter().any(|r| r.contains("line one")), "…its first line banded:\n{banded:?}");
        assert!(banded.iter().any(|r| r.contains("line three")), "…and its last:\n{banded:?}");
        // the bytes a copy of the pick would put on the clipboard: all its lines
        let sel = app.memo_sel.unwrap();
        let msgs = app.vault.as_ref().unwrap().content.memo_messages();
        assert_eq!(
            crate::memo::memo_copy_payload(&msgs[sel], Lang::En),
            "line one\nline two\nline three",
        );
        // ↓ walks back toward the newest, clamped there
        key(&mut app, KeyCode::Down);
        assert_eq!(app.memo_sel, Some(newest));
        // and a keystroke into the composer is a return to the present: band gone
        key(&mut app, KeyCode::Char('x'));
        assert_eq!(app.memo_sel, None, "typing drops the copy-band");
        assert!(banded_rows(&app, 100, 24).is_empty(), "…and nothing is left highlighted");
    }

    #[test]
    fn ctrl_y_copies_the_selected_message_and_defaults_to_the_newest() {
        // the copy chord itself. A real clipboard is never asserted (headless):
        // what is checked is that the path RAN — it left a toast and banded what
        // it lifted — and that with nothing picked it takes the newest message.
        let (mut app, _d) = memo_app();
        send_lines(&mut app, &["riga uno", "riga due"]);
        send(&mut app, "l'ultima");
        let newest = app.vault.as_ref().unwrap().content.memo_messages().len() - 1;
        app.status = None;
        key_ctrl(&mut app, KeyCode::Char('y'));
        assert_eq!(app.memo_sel, Some(newest), "a bare copy takes the newest and marks it");
        assert!(app.status.is_some(), "the copy path ran (a 'copied' or 'no clipboard' toast)");
        // picking the multi-line message and copying leaves the band on it
        key(&mut app, KeyCode::Up); // from the newest to the message before it
        let sel = app.memo_sel.unwrap();
        let msgs = app.vault.as_ref().unwrap().content.memo_messages();
        assert!(msgs[sel].text.contains('\n'), "the pick is the multi-line message");
        key_ctrl(&mut app, KeyCode::Char('y'));
        assert_eq!(app.memo_sel, Some(sel), "the copy left its band on what it lifted");
    }

    #[test]
    fn the_memo_with_a_selected_multiline_message_never_panics_at_any_size() {
        const SIZES: [(u16, u16); 6] = [(1, 1), (6, 4), (20, 8), (40, 24), (80, 24), (200, 60)];
        let (mut app, _d) = memo_app();
        app.settings.backdrop = crate::settings::Backdrop::On;
        app.settings.backdrop_fx = crate::settings::BackdropFx::Rain;
        for i in 0..20 {
            send(&mut app, &format!("message {i}"));
        }
        send_lines(&mut app, &["a paragraph", "over several", "lines of text", "that wraps too"]);
        // walk the copy-band the length of the thread, drawing at every size at
        // each step — the band, the scroll-to-keep-it-visible and the frozen
        // backdrop all exercised together
        for _ in 0..25 {
            key(&mut app, KeyCode::Up);
            for (w, h) in SIZES {
                let _ = render_sized(&app, w, h); // must not panic
            }
        }
        // …and the copy chord on top of a live selection
        key_ctrl(&mut app, KeyCode::Char('y'));
        for (w, h) in SIZES {
            let _ = render_sized(&app, w, h); // must not panic
        }
    }

    #[test]
    fn the_thread_stays_pinned_to_the_newest_message_and_scrolls_back() {
        let (mut app, _d) = memo_app();
        for i in 0..40 {
            send(&mut app, &format!("message number {i}"));
        }
        assert_eq!(app.memo_scroll, 0, "a send returns to the present");
        let text = render_sized(&app, 100, 20);
        assert!(text.contains("message number 39"), "the newest is on screen:\n{text}");
        assert!(!text.contains("message number 0 "), "…and the oldest is scrolled off");
        // PgUp walks back, PgDn returns
        key(&mut app, KeyCode::PageUp);
        key(&mut app, KeyCode::PageUp);
        assert!(app.memo_scroll > 0);
        let back = render_sized(&app, 100, 20);
        assert!(!back.contains("message number 39"), "the newest scrolled off the bottom");
        for _ in 0..8 {
            key(&mut app, KeyCode::PageDown);
        }
        assert_eq!(app.memo_scroll, 0, "PgDn saturates at the newest");
        assert!(render_sized(&app, 100, 20).contains("message number 39"));
        // Ctrl+↑/↓ is the same verb one row at a time, and typing comes back
        key_ctrl(&mut app, KeyCode::Up);
        assert_eq!(app.memo_scroll, 1);
        key(&mut app, KeyCode::Char('x'));
        assert_eq!(app.memo_scroll, 0, "typing is a return to the present");
        key(&mut app, KeyCode::Esc);
        // an over-scroll is simply the top, never a panic or a blank thread
        app.memo_scroll = usize::MAX;
        let top = render_sized(&app, 100, 20);
        assert!(top.contains("message number 0"), "the oldest is at the top:\n{top}");
    }

    #[test]
    fn scrolling_past_the_top_of_the_thread_banks_no_dead_presses() {
        // the offset used to grow without a ceiling, so after a few PgUp the
        // first PgDn presses moved nothing at all — the thread looked frozen.
        let (mut app, _d) = memo_app();
        for i in 0..40 {
            send(&mut app, &format!("message number {i}"));
        }
        let _ = render_sized(&app, 100, 20); // the draw publishes the bound
        let max = app.memo_max_scroll.get();
        assert!(max > 0 && max < usize::MAX, "there is something to scroll back: {max}");

        for _ in 0..6 {
            key(&mut app, KeyCode::PageUp);
            let _ = render_sized(&app, 100, 20);
        }
        assert_eq!(app.memo_scroll, max, "back stops AT the top of the thread");
        let top = render_sized(&app, 100, 20);
        assert!(top.contains("message number 0"), "…which is where the view is:\n{top}");
        key(&mut app, KeyCode::PageDown);
        assert_ne!(render_sized(&app, 100, 20), top, "the very next press moves the view");
        // Ctrl+↑ is the same verb, and takes the same bound
        for _ in 0..60 {
            key_ctrl(&mut app, KeyCode::Up);
        }
        assert_eq!(app.memo_scroll, max, "one row at a time stops there too");
    }

    #[test]
    fn tab_never_throws_a_half_written_message_away() {
        // leaving the thread zeroizes the composer, and Esc — the key the bar
        // DOES promise for it — deliberately costs two presses so a line typed
        // by mistake doesn't also cost the view. Tab must not undercut that.
        let (mut app, _d) = memo_app();
        for ch in "first".chars() {
            key(&mut app, KeyCode::Char(ch));
        }
        key_shift(&mut app, KeyCode::Enter);
        for ch in "second".chars() {
            key(&mut app, KeyCode::Char(ch));
        }
        assert_eq!(app.memo_input.as_str(), "first\nsecond", "(a note nobody has sent yet)");

        key(&mut app, KeyCode::Tab);
        assert!(app.memo_open, "Tab did not take the view");
        assert_eq!(app.memo_input.as_str(), "first\nsecond", "…nor the draft");
        key(&mut app, KeyCode::BackTab);
        assert!(app.memo_open && app.memo_input == "first\nsecond", "Shift+Tab neither");
        assert_eq!(app.memo_len(), 0, "and nothing was sent on the way");

        // Esc empties it, and THEN Tab walks the chips row as it always did
        key(&mut app, KeyCode::Esc);
        assert!(app.memo_input.is_empty() && app.memo_open);
        key(&mut app, KeyCode::Tab);
        assert!(!app.memo_open, "an empty composer steps off the memo's chip");
        assert_eq!(app.vault_tab, 0, "…onto the ALL tab");
    }

    #[test]
    fn a_populated_memorandum_never_panics_at_any_size() {
        const SIZES: [(u16, u16); 5] = [(1, 1), (6, 4), (40, 24), (80, 24), (200, 60)];
        let (mut app, dir) = memo_app();
        // text, a photo with a caption, and a message far wider than any pane
        send(&mut app, "short one");
        let (png, _) = write_png(dir.path(), "shot.png", 64, 48);
        key_ctrl(&mut app, KeyCode::Char('p'));
        app.form.as_mut().unwrap().fields[0].value = png.to_string_lossy().to_string().into();
        app.form.as_mut().unwrap().fields[1].value =
            "a caption that is itself long enough to have to wrap across several rows".to_string().into();
        submit_now(&mut app);
        send(&mut app, &"lungrepetitionwithoutanyspaces".repeat(12));
        send(&mut app, &"a sentence with plenty of ordinary words in it ".repeat(6));
        // a message a foreign writer left that this build cannot fully read
        app.vault.as_mut().unwrap().content.push_memo_message(crate::memo::MemoMessage {
            id: "odd".into(),
            ts: "not a timestamp".into(),
            text: "però ~ 日本語 ~ ok".into(),
            image: None,
            vec: None,
            vfp: None,
            extra: serde_json::Map::new(),
        });
        for scroll in [0usize, 1, 7, 40, usize::MAX] {
            app.memo_scroll = scroll;
            for (w, h) in SIZES {
                let _ = render_sized(&app, w, h); // must not panic
            }
        }
        // …with the composer full, and with the attach modal over it
        app.memo_scroll = 0;
        for ch in "a line long enough to run past the right edge of the composer".chars() {
            key(&mut app, KeyCode::Char(ch));
        }
        key_ctrl(&mut app, KeyCode::Char('p'));
        for (w, h) in SIZES {
            let _ = render_sized(&app, w, h);
        }
        let text = render_sized(&app, 120, 40);
        assert!(!text.contains("iVBORw0"), "no base64 anywhere on the screen:\n{text}");
    }

    #[test]
    fn the_day_separator_is_localized_and_survives_a_stamp_it_cannot_read() {
        assert_eq!(i18n::t(Lang::En, &Key::MemoDay("2026-08-19")), "19 August 2026");
        assert_eq!(i18n::t(Lang::It, &Key::MemoDay("2026-08-19")), "19 agosto 2026");
        // anything that is not that shape is still a day: shown as it came
        for odd in ["", "not a date", "2026-13-01", "2026-08", "2026-xx-19"] {
            assert_eq!(i18n::t(Lang::It, &Key::MemoDay(odd)), odd, "{odd}");
        }
        // one rule per day, not per message — and the day is the USER's, not
        // UTC: a message written at 00:30 in Rome belongs to the day their
        // calendar says (see `model::wall_clock`, which pins the offset).
        let (mut app, _d) = memo_app();
        send(&mut app, "first");
        send(&mut app, "second");
        let stamp = app.vault.as_ref().unwrap().content.memo_messages()[0].ts.clone();
        let (today, _) = crate::model::wall_clock(&stamp, crate::model::local_utc_offset())
            .expect("this build's own stamp reads back");
        let rule = i18n::t(Lang::En, &Key::MemoDay(&today));
        let text = render_sized(&app, 100, 30);
        assert_eq!(text.matches(&rule).count(), 1, "one separator for both messages:\n{text}");
    }

    // ---- a message of several lines (Shift+⏎ / Alt+⏎) ---------------------

    #[test]
    fn a_newline_chord_makes_one_message_of_several_lines() {
        let (mut app, _d) = memo_app();
        for ch in "the safe code".chars() {
            key(&mut app, KeyCode::Char(ch));
        }
        key_shift(&mut app, KeyCode::Enter);
        assert_eq!(app.memo_input.as_str(), "the safe code\n", "Shift+⏎ wrote a newline…");
        assert_eq!(app.memo_len(), 0, "…and sent nothing");
        for ch in "is on the fridge".chars() {
            key(&mut app, KeyCode::Char(ch));
        }
        // Alt+⏎ is the same verb, for the terminals that cannot spell the first
        key_alt(&mut app, KeyCode::Enter);
        for ch in "behind the magnets".chars() {
            key(&mut app, KeyCode::Char(ch));
        }
        key(&mut app, KeyCode::Enter); // plain ⏎ still sends
        assert!(app.memo_input.is_empty(), "the composer emptied on send");
        assert_eq!(app.memo_len(), 1, "one message, not three");
        let written = "the safe code\nis on the fridge\nbehind the magnets";
        assert_eq!(app.vault.as_ref().unwrap().content.memo_messages()[0].text, written);
        // …and it reached the DISK with its lines intact
        let reopened = Vault::open(&app.paths.vaults_dir, "bruno", "pw").expect("reopens");
        assert_eq!(reopened.content.memo_messages()[0].text, written);

        // Backspace takes a newline back like any other character…
        for ch in "again".chars() {
            key(&mut app, KeyCode::Char(ch));
        }
        key_shift(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Backspace);
        assert_eq!(app.memo_input.as_str(), "again");
        // …and a buffer of nothing but newlines is still not a message
        key(&mut app, KeyCode::Esc);
        key_shift(&mut app, KeyCode::Enter);
        key_alt(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.memo_len(), 1, "blank lines alone send nothing");
    }

    #[test]
    fn a_multi_line_message_is_as_many_rows_in_the_thread() {
        let (mut app, _d) = memo_app();
        send_lines(&mut app, &["alpha", "beta", "gamma"]);
        assert_eq!(app.memo_len(), 1);
        let frame = render_sized(&app, 100, 30);
        let rows: Vec<&str> = frame.lines().collect();
        let at = rows.iter().position(|r| r.contains("alpha")).expect("the first line is drawn");
        assert!(rows[at + 1].contains("beta"), "the second line has its own row:\n{frame}");
        assert!(rows[at + 2].contains("gamma"), "…and so does the third:\n{frame}");
        // the time column rides the message's FIRST row only, so the three read
        // as one paragraph under it
        let stamp = app.vault.as_ref().unwrap().content.memo_messages()[0].ts.clone();
        let (_, time) = crate::model::wall_clock(&stamp, crate::model::local_utc_offset())
            .expect("this build's own stamp reads back");
        assert!(rows[at].contains(&time), "the first row is stamped:\n{frame}");
        assert!(!rows[at + 1].contains(&time) && !rows[at + 2].contains(&time), "the others are not");
        // a blank line the user typed survives as a blank row
        send_lines(&mut app, &["over", "", "under"]);
        let frame = render_sized(&app, 100, 30);
        let rows: Vec<&str> = frame.lines().collect();
        let at = rows.iter().position(|r| r.contains("over")).expect("drawn");
        assert!(rows[at + 1].trim().is_empty(), "the paragraph break is a row:\n{frame}");
        assert!(rows[at + 2].contains("under"));
    }

    #[test]
    fn the_composer_grows_with_its_lines_and_caps_at_five_rows() {
        // the pure rule first: one row per line, one at least, five at most
        assert_eq!(memo_composer_rows(""), 1);
        assert_eq!(memo_composer_rows("one"), 1);
        assert_eq!(memo_composer_rows("one\ntwo"), 2);
        assert_eq!(memo_composer_rows("one\n"), 2, "a trailing newline is a row to type on");
        assert_eq!(memo_composer_rows(&"x\n".repeat(40)), MEMO_COMPOSER_MAX_ROWS);

        let (mut app, _d) = memo_app();
        for (i, line) in ["primo", "secondo", "terzo"].iter().enumerate() {
            if i > 0 {
                key_shift(&mut app, KeyCode::Enter);
            }
            for ch in line.chars() {
                key(&mut app, KeyCode::Char(ch));
            }
        }
        let frame = render_sized(&app, 100, 30);
        let rows: Vec<&str> = frame.lines().collect();
        let at = rows.iter().position(|r| r.contains("› primo")).expect("the prompt row:\n{frame}");
        assert!(rows[at + 1].contains("secondo"), "the composer grew a row:\n{frame}");
        assert!(rows[at + 2].contains("terzo"), "…and another:\n{frame}");
        assert!(!rows[at + 1].contains('›'), "the prompt marks the composer once");
        assert_eq!(app.memo_len(), 0, "and nothing was sent for any of it");

        // past the cap it scrolls inside itself: the newest lines, with the
        // cursor, are the ones on screen — never the oldest.
        let (mut app, _d) = memo_app();
        for (i, line) in
            ["alfa", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel"]
                .iter()
                .enumerate()
        {
            if i > 0 {
                key_shift(&mut app, KeyCode::Enter);
            }
            for ch in line.chars() {
                key(&mut app, KeyCode::Char(ch));
            }
        }
        let frame = render_sized(&app, 100, 30);
        let rows: Vec<&str> = frame.lines().collect();
        let at = rows.iter().position(|r| r.contains("› delta")).expect("the cap slid the window");
        for (i, line) in ["echo", "foxtrot", "golf", "hotel"].iter().enumerate() {
            assert!(rows[at + 1 + i].contains(line), "'{line}' is on screen:\n{frame}");
        }
        for gone in ["alfa", "bravo", "charlie"] {
            assert!(!frame.contains(gone), "'{gone}' scrolled out of the composer:\n{frame}");
        }
    }

    #[test]
    fn the_thread_gives_up_its_rows_to_a_growing_composer() {
        let (mut app, _d) = memo_app();
        for i in 0..40 {
            send(&mut app, &format!("message number {i}"));
        }
        let visible = |app: &App| {
            render_sized(app, 100, 20).lines().filter(|l| l.contains("message number")).count()
        };
        let before = visible(&app);
        assert!(before > 5, "the thread fills the view to start with");
        for _ in 0..4 {
            key_shift(&mut app, KeyCode::Enter); // a five-row composer
        }
        let after = visible(&app);
        assert_eq!(after, before - 4, "the four rows the composer took came off the thread");
        assert!(
            render_sized(&app, 100, 20).contains("message number 39"),
            "…and the thread is still pinned to its newest message"
        );
        // past the cap the composer stops taking rows
        for _ in 0..6 {
            key_shift(&mut app, KeyCode::Enter);
        }
        assert_eq!(visible(&app), after, "the cap holds the thread's floor");
    }

    #[test]
    fn a_search_jump_lands_the_whole_of_a_multi_line_message() {
        let (mut app, _d) = memo_app();
        for i in 0..40 {
            send(&mut app, &format!("message number {i}"));
        }
        send_lines(&mut app, &["il codice", "della cassaforte", "è 4821"]);
        for i in 40..60 {
            send(&mut app, &format!("message number {i}"));
        }
        let target = app.vault.as_ref().unwrap().content.memo_messages()[40].id.clone();
        search_for(&mut app, "cassaforte");
        key(&mut app, KeyCode::Enter); // run
        key(&mut app, KeyCode::Enter); // jump
        assert_eq!(app.memo_jump.as_deref(), Some(target.as_str()));
        let banded = banded_rows(&app, 100, 20);
        assert_eq!(banded.len(), 3, "every row of the message wears the band: {banded:?}");
        assert!(banded[0].contains("il codice"), "{banded:?}");
        assert!(banded[1].contains("della cassaforte"), "{banded:?}");
        assert!(banded[2].contains("4821"), "…including the last one: {banded:?}");

        // a message taller than the viewport starts at its own first row rather
        // than landing with only its tail on screen
        let mut lines = vec!["primissima riga".to_string()];
        lines.extend((1..14).map(|i| format!("riga numero {i}")));
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        send_lines(&mut app, &refs);
        search_for(&mut app, "primissima");
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Enter);
        let frame = render_sized(&app, 100, 12);
        assert!(frame.contains("primissima riga"), "the message starts at its top:\n{frame}");
    }

    #[test]
    fn a_multi_line_memorandum_never_panics_at_any_size() {
        const SIZES: [(u16, u16); 5] = [(1, 1), (6, 4), (40, 24), (80, 24), (200, 60)];
        let (mut app, _d) = memo_app();
        send_lines(&mut app, &["one", "", "a third line, after a blank one"]);
        // a message far taller than any pane, whose lines are far wider than one
        let lines: Vec<String> = (0..30)
            .map(|i| format!("riga {i} ").repeat(6) + &"lungosenzaspazi".repeat(4))
            .collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        send_lines(&mut app, &refs);
        let tall = app.vault.as_ref().unwrap().content.memo_messages()[1].id.clone();
        for scroll in [0usize, 1, 7, 40, usize::MAX] {
            app.memo_scroll = scroll;
            for (w, h) in SIZES {
                let _ = render_sized(&app, w, h); // must not panic
            }
        }
        // …with the jump standing on the tall one, and the composer at its cap
        app.memo_scroll = 0;
        app.memo_jump = Some(tall);
        for i in 0..8 {
            for ch in format!("linea {i} del messaggio che sto scrivendo").chars() {
                key(&mut app, KeyCode::Char(ch));
            }
            key_alt(&mut app, KeyCode::Enter);
        }
        for (w, h) in SIZES {
            let _ = render_sized(&app, w, h);
        }
    }

    #[test]
    fn the_newline_chip_names_the_chord_this_terminal_can_deliver() {
        let (mut app, _d) = memo_app();
        app.status = None; // a toast hides the keybar; we want the bar itself
        assert!(!app.shift_enter_ok, "nothing pushed the keyboard protocol in a test");
        let bar = keybar_of(&render_sized(&app, 120, 30));
        assert!(bar.contains("Alt+⏎"), "the chord that really arrives is the one shown:\n{bar}");
        assert!(bar.contains("newline"), "…with its word:\n{bar}");
        assert!(!bar.contains("Shift+⏎"), "a key that cannot arrive is never promised:\n{bar}");
        // …and where the terminal does spell it out, it is the one advertised
        app.shift_enter_ok = true;
        let bar = keybar_of(&render_sized(&app, 120, 30));
        assert!(bar.contains("Shift+⏎"), "the enhanced chord:\n{bar}");
        assert!(!bar.contains("Alt+⏎"), "…and only it:\n{bar}");
        // localized like every other chip on the bar
        app.settings.lang = Lang::It;
        let bar = keybar_of(&render_sized(&app, 120, 30));
        assert!(bar.contains("a capo"), "the Italian newline chip:\n{bar}");
    }

    // ---- searching inside the Memorandum (Ctrl+F) ------------------------

    /// Every row of a rendered frame as `(text, banded)` — `banded` meaning the
    /// row carries the selection background. `render_sized` sees symbols only,
    /// and the jump's highlight is a colour.
    fn render_rows(app: &App, w: u16, h: u16) -> Vec<(String, bool)> {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buf = terminal.backend().buffer();
        let band = app.palette().selection;
        (0..buf.area.height)
            .map(|y| {
                let mut text = String::new();
                let mut banded = false;
                for x in 0..buf.area.width {
                    let cell = &buf[(x, y)];
                    text.push_str(cell.symbol());
                    banded |= cell.bg == band;
                }
                (text, banded)
            })
            .collect()
    }

    /// The rows the jump's selection band is on.
    fn banded_rows(app: &App, w: u16, h: u16) -> Vec<String> {
        render_rows(app, w, h).into_iter().filter(|(_, b)| *b).map(|(t, _)| t).collect()
    }

    /// Opens the search bar and types `query` into it (no ⏎).
    fn search_for(app: &mut App, query: &str) {
        if app.memo_search.is_none() {
            key_ctrl(app, KeyCode::Char('f'));
        }
        for ch in query.chars() {
            key(app, KeyCode::Char(ch));
        }
    }

    /// A Memorandum with the deterministic embedder behind it and every message
    /// already indexed — no model, no network, no display anywhere in here.
    fn memo_rag_app() -> (App, TempDir) {
        let (mut app, dir) = app_with(&["bruno"]);
        app.use_mock_rag();
        open_first_vault(&mut app);
        key(&mut app, KeyCode::Char(memo_key(Lang::En)));
        assert!(app.memo_open);
        (app, dir)
    }

    /// Ticks until the search bar has an answer (hits, or an empty run).
    fn pump_search(app: &mut App) {
        for _ in 0..600 {
            app.on_tick();
            if app.memo_search.as_ref().is_some_and(|s| s.ran.is_some()) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("the search never answered");
    }

    #[test]
    fn the_literal_search_finds_a_scrolled_off_message_and_jumps_to_it() {
        let (mut app, _d) = memo_app();
        for i in 0..40 {
            send(&mut app, &format!("message number {i}"));
        }
        send(&mut app, "il codice della cassaforte è 4821");
        for i in 40..60 {
            send(&mut app, &format!("message number {i}"));
        }
        let target = app.vault.as_ref().unwrap().content.memo_messages()[40].id.clone();
        assert!(!render_sized(&app, 100, 20).contains("cassaforte"), "it is scrolled off");

        // Ctrl+F replaces the composer with the bar; the chat stays on screen
        key_ctrl(&mut app, KeyCode::Char('f'));
        assert!(app.memo_search.is_some(), "Ctrl+F opened the bar");
        let text = render_sized(&app, 100, 20);
        assert!(text.contains("search the memorandum"), "the bar is up:\n{text}");
        assert!(text.contains("message number 59"), "…over a chat that is still there:\n{text}");
        assert!(app.memo_input.is_empty(), "nothing was typed into the composer");

        // the query is typed into the BAR, not the thread, and ⏎ runs it
        search_for(&mut app, "cassaforte");
        assert_eq!(app.memo_search.as_ref().unwrap().query.as_str(), "cassaforte");
        assert!(app.memo_input.is_empty(), "…and not into the message being written");
        key(&mut app, KeyCode::Enter);
        let search = app.memo_search.as_ref().expect("the bar is still up");
        assert_eq!(search.ran, Some(crate::app::SearchMode::Literal));
        assert_eq!(search.hits.len(), 1);
        assert_eq!(search.hits[0].id, target);
        assert!(search.hits[0].snippet.contains("cassaforte"), "the hit shows its words");
        assert!(search.hits[0].score.is_none(), "a literal hit has no number to show");
        assert_eq!(app.memo_len(), 61, "running a search sends nothing");
        // …and the strip lists it over the chat
        let text = render_sized(&app, 100, 20);
        assert!(text.contains("results"), "the results strip is up:\n{text}");
        assert!(text.contains("4821"), "…with the hit on it:\n{text}");

        // ⏎ on the hit scrolls the thread to that message, bands it, and goes
        key(&mut app, KeyCode::Enter);
        assert!(app.memo_search.is_none(), "the bar closed on the jump");
        assert_eq!(app.memo_jump.as_deref(), Some(target.as_str()), "the thread is pinned to it");
        let rows = render_rows(&app, 100, 20);
        let banded: Vec<&String> = rows.iter().filter(|(_, b)| *b).map(|(t, _)| t).collect();
        assert_eq!(banded.len(), 1, "one message wears the band");
        assert!(banded[0].contains("cassaforte"), "…and it is the one that was found: {banded:?}");
        let text: String = rows.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>().join("\n");
        assert!(text.contains("write a message"), "the composer came back:\n{text}");
        assert!(!text.contains("message number 59"), "the newest scrolled away for it");

        // typing into the composer again is a return to the present
        key(&mut app, KeyCode::Char('x'));
        assert!(app.memo_jump.is_none(), "the band went with it");
        assert!(banded_rows(&app, 100, 20).is_empty());
    }

    #[test]
    fn tab_flips_the_mode_chip_between_literal_and_rag() {
        let (mut app, _d) = memo_rag_app();
        send(&mut app, "le chiavi sono sotto il tappeto");
        key_ctrl(&mut app, KeyCode::Char('f'));
        let text = render_sized(&app, 100, 20);
        assert!(text.contains("aa literal"), "the bar opens on the literal search:\n{text}");
        assert!(!text.contains("◈ RAG"), "…and only that chip is shown:\n{text}");

        key(&mut app, KeyCode::Tab);
        let text = render_sized(&app, 100, 20);
        assert!(text.contains("◈ RAG"), "Tab flipped the chip:\n{text}");
        assert!(!text.contains("aa literal"), "…and the other one is gone:\n{text}");
        assert_eq!(app.memo_search.as_ref().unwrap().mode, crate::app::SearchMode::Rag);

        key(&mut app, KeyCode::Tab);
        assert!(render_sized(&app, 100, 20).contains("aa literal"), "…and back again");
        // the chip is localized like every other word on screen
        app.settings.lang = Lang::It;
        assert!(render_sized(&app, 100, 20).contains("aa letterale"));
        key(&mut app, KeyCode::Tab);
        assert!(render_sized(&app, 100, 20).contains("◈ RAG"), "the acronym is the same word");
    }

    #[test]
    fn a_flip_of_the_mode_drops_the_results_the_other_search_produced() {
        // the chip shows the mode that RAN: a list left standing under a flipped
        // chip would be exactly the lie the chip exists to prevent.
        let (mut app, _d) = memo_app();
        send(&mut app, "il codice della cassaforte");
        search_for(&mut app, "codice");
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.memo_search.as_ref().unwrap().hits.len(), 1);
        key(&mut app, KeyCode::Tab);
        let search = app.memo_search.as_ref().unwrap();
        assert!(search.hits.is_empty() && search.ran.is_none(), "the flip dropped the run");
        assert_eq!(search.query.as_str(), "codice", "…but kept what was typed");
        // and so does editing the query
        key(&mut app, KeyCode::Tab); // back to literal
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.memo_search.as_ref().unwrap().hits.len(), 1);
        key(&mut app, KeyCode::Backspace);
        assert!(app.memo_search.as_ref().unwrap().hits.is_empty());
    }

    #[test]
    fn the_rag_search_ranks_the_thread_on_a_worker_and_jumps_to_the_hit() {
        use crate::rag::MockEmbedder;
        let (mut app, _d) = memo_rag_app();
        for text in [
            "le chiavi di casa sono sotto il tappeto",
            "la ricetta della torta di mele della nonna",
            "il codice della cassaforte è 4821",
            "la targa della macchina",
        ] {
            send(&mut app, text);
        }
        // NO hand-run index pass: a user presses Ctrl+F, and the messages they
        // wrote a minute ago have no vector yet (the Memorandum is not indexed
        // per message). Catching the thread up is the search's own job.
        assert!(
            app.vault.as_ref().unwrap().content.memo_messages().iter().all(|m| m.vec.is_none()),
            "nothing is embedded when the bar opens"
        );

        // the mock's vectors are hashes, so what "similar" means here is
        // arithmetic: rank the passages against the query ourselves and demand
        // exactly that order back.
        let query = "dove ho messo le chiavi";
        let qv = MockEmbedder::vector_of(&format!("query: {query}"));
        let stored = app.vault.as_ref().unwrap().content.memo_messages();
        let mut expected: Vec<(String, f32)> = stored
            .iter()
            .map(|m| {
                let text = crate::rag::memo_embed_text(m);
                let v = MockEmbedder::vector_of(&format!("passage: {text}"));
                (m.id.clone(), v.iter().zip(&qv).map(|(a, b)| a * b).sum::<f32>())
            })
            .collect();
        expected.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

        key_ctrl(&mut app, KeyCode::Char('f'));
        key(&mut app, KeyCode::Tab); // → RAG
        search_for(&mut app, query);
        key(&mut app, KeyCode::Enter);
        // the embed is on the worker: the key press does NOT block on it
        assert!(app.memo_search.as_ref().unwrap().searching(), "…and says so meanwhile");
        assert!(app.memo_search.as_ref().unwrap().hits.is_empty());
        assert!(render_sized(&app, 100, 20).contains("searching…"), "a visible working state");
        pump_search(&mut app);

        let search = app.memo_search.as_ref().expect("the bar is still up");
        assert_eq!(search.ran, Some(crate::app::SearchMode::Rag), "the RAG search ran");
        assert_eq!(search.hits.len(), 4, "every message is ranked, not just the matching words");
        assert_eq!(
            search.hits.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(),
            expected.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
            "the hits come back in dot-product order"
        );
        for pair in search.hits.windows(2) {
            assert!(pair[0].score >= pair[1].score, "best first");
        }
        assert!(search.hits[0].score.is_some(), "a semantic hit shows its score");
        let text = render_sized(&app, 100, 20);
        assert!(text.contains("◈ RAG"), "the chip says what ran:\n{text}");
        assert!(text.contains(&format!("{:.2}", search.hits[0].score.unwrap())), "…and the score");

        // ↑↓ walk the hits, ⏎ jumps to the one under the cursor
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Down);
        assert_eq!(app.memo_search.as_ref().unwrap().selected, 2);
        key(&mut app, KeyCode::Up);
        let wanted = app.memo_search.as_ref().unwrap().hits[1].id.clone();
        key(&mut app, KeyCode::Enter);
        assert!(app.memo_search.is_none(), "the bar closed on the jump");
        assert_eq!(app.memo_jump.as_deref(), Some(wanted.as_str()));
        let wanted_text = stored.iter().find(|m| m.id == wanted).unwrap().text.clone();
        let banded = banded_rows(&app, 100, 20);
        assert!(
            banded.iter().any(|row| wanted_text.split(' ').all(|w| row.contains(w))),
            "the second hit is the banded message: {banded:?}"
        );
    }

    #[test]
    fn a_message_written_in_this_session_is_found_by_the_semantic_search() {
        // the Memorandum is not indexed per message on purpose (that would be
        // a save per keystroke-sized note), so everything written since the
        // last pass has NO vector. A search that ranked against that index
        // would truthfully chip "RAG" over an untruthful "no results" — for a
        // message the user typed thirty seconds ago and can see on screen.
        let (mut app, _d) = memo_rag_app();
        send(&mut app, "le chiavi di casa sono sotto il tappeto");
        send(&mut app, "la ricetta della torta di mele");
        for _ in 0..20 {
            app.on_tick(); // nothing catches it up by itself
        }
        assert!(
            app.vault.as_ref().unwrap().content.memo_messages().iter().all(|m| m.vec.is_none()),
            "no pass has run — this is the state a user reaches Ctrl+F in"
        );

        key_ctrl(&mut app, KeyCode::Char('f'));
        key(&mut app, KeyCode::Tab); // → RAG
        search_for(&mut app, "dove ho messo le chiavi");
        key(&mut app, KeyCode::Enter);
        assert!(app.memo_search.as_ref().unwrap().searching(), "it says it is working");
        pump_search(&mut app);

        let search = app.memo_search.as_ref().expect("the bar is still up");
        assert_eq!(search.ran, Some(crate::app::SearchMode::Rag), "the RAG search really ran");
        // both messages are ranked, which they can only be with a vector each
        assert_eq!(search.hits.len(), 2, "…over a thread it had to index first");
        assert!(search.hits.iter().all(|h| h.score.is_some()), "semantic hits carry their score");
        assert!(
            app.vault.as_ref().unwrap().content.memo_messages().iter().all(|m| m.vec.is_some()),
            "…and the vectors landed in the vault, once, on the way"
        );
    }

    #[test]
    fn a_memo_search_says_rag_is_off_when_only_the_item_index_is_on() {
        // the two toggles are independent (`settings_ui` flips them one by
        // one). With items-only, the engine is up but the thread was never
        // embedded, so "RAG" would be a chip over a search that could not run.
        let (mut app, _d) = app_with(&["bruno"]);
        app.rag.use_mock();
        app.settings.rag_items = true;
        app.settings.rag_memo = false;
        open_first_vault(&mut app);
        key(&mut app, KeyCode::Char(memo_key(Lang::En)));
        send(&mut app, "le chiavi sono sotto il tappeto");
        pump_rag_items(&mut app);
        assert!(
            app.vault.as_ref().unwrap().content.memo_messages()[0].vec.is_none(),
            "the memo toggle is off, so the thread has no vectors"
        );

        key_ctrl(&mut app, KeyCode::Char('f'));
        key(&mut app, KeyCode::Tab); // → RAG
        search_for(&mut app, "chiavi");
        key(&mut app, KeyCode::Enter);

        let search = app.memo_search.as_ref().expect("the bar is still up");
        assert_eq!(search.ran, Some(crate::app::SearchMode::Literal), "it ran literally");
        assert_eq!(search.hits.len(), 1, "…and really found the message");
        assert_eq!(
            app.status.as_ref().map(|(_, s)| s.clone()).unwrap_or_default(),
            i18n::t(Lang::En, &Key::SearchRagOff),
            "…and said why instead of answering 'no results'"
        );
        let text = render_sized(&app, 100, 20);
        assert!(text.contains("aa literal") && !text.contains("◈ RAG"), "the chip is honest:\n{text}");
    }

    /// Ticks until no worker is out (the memo half may stay unindexed on
    /// purpose, so `pump_rag`'s "nothing stale" is the wrong question here).
    fn pump_rag_items(app: &mut App) {
        for _ in 0..600 {
            app.on_tick();
            if !app.rag.busy() && !app.rag.has_deferred() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("the embedding worker never settled");
    }

    #[test]
    fn a_rag_search_with_no_model_runs_literally_and_says_so() {
        // never a dead search, never a silent wrong mode: the fallback answers
        // the question AND the chip owns up to which search answered it.
        let (mut app, _d) = memo_app(); // no mock ⇒ no model on this machine
        assert!(!app.rag.searchable());
        send(&mut app, "le chiavi sono sotto il tappeto");
        send(&mut app, "la torta di mele");
        key_ctrl(&mut app, KeyCode::Char('f'));
        key(&mut app, KeyCode::Tab);
        search_for(&mut app, "chiavi");
        key(&mut app, KeyCode::Enter);

        assert!(!app.rag.busy(), "no worker was started for a model that isn't there");
        let search = app.memo_search.as_ref().expect("the bar is still up");
        assert_eq!(search.mode, crate::app::SearchMode::Rag, "the ASK is unchanged");
        assert_eq!(search.ran, Some(crate::app::SearchMode::Literal), "…the RUN is honest");
        assert_eq!(search.hits.len(), 1, "and it really searched");
        let status = app.status.as_ref().map(|(_, s)| s.clone()).unwrap_or_default();
        assert_eq!(status, i18n::t(Lang::En, &Key::SearchRagOff), "it says why");
        let text = render_sized(&app, 100, 20);
        assert!(text.contains("aa literal"), "the chip shows the mode that RAN:\n{text}");
        assert!(!text.contains("◈ RAG"), "…not the one that was asked for:\n{text}");

        // …and the reason is localized like everything else. The ASK is still
        // RAG — the fallback answered one search, it did not change the mode —
        // so editing the query and running again asks for RAG again.
        app.settings.lang = Lang::It;
        key(&mut app, KeyCode::Backspace);
        assert_eq!(app.memo_search.as_ref().unwrap().mode, crate::app::SearchMode::Rag);
        key(&mut app, KeyCode::Enter);
        let status = app.status.as_ref().map(|(_, s)| s.clone()).unwrap_or_default();
        assert_eq!(status, i18n::t(Lang::It, &Key::SearchRagOff));
        assert!(render_sized(&app, 100, 20).contains("aa letterale"));
    }

    #[test]
    fn a_search_that_finds_nothing_says_nessun_risultato() {
        let (mut app, _d) = memo_app();
        send(&mut app, "le chiavi sono sotto il tappeto");
        search_for(&mut app, "grappa");
        key(&mut app, KeyCode::Enter);
        let search = app.memo_search.as_ref().unwrap();
        assert!(search.hits.is_empty() && search.ran.is_some(), "it ran and found nothing");
        assert!(render_sized(&app, 100, 20).contains("no results"));
        app.settings.lang = Lang::It;
        assert!(render_sized(&app, 100, 20).contains("nessun risultato"));
        // ⏎ on an empty list is another run, never a jump into nothing
        key(&mut app, KeyCode::Enter);
        assert!(app.memo_search.is_some() && app.memo_jump.is_none());
    }

    #[test]
    fn the_search_is_wiped_when_the_memorandum_closes_or_the_vault_locks() {
        // a query names what you were looking for INSIDE the vault: it goes
        // with the vault, exactly like the composer's half-written line.
        let (mut app, _d) = memo_app();
        send(&mut app, "il codice della cassaforte");
        search_for(&mut app, "cassaforte");
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Enter); // jump: the band is on, the bar is away
        assert!(app.memo_jump.is_some());

        // Esc alone gives the composer back with its line still on it
        for ch in "half a thought".chars() {
            key(&mut app, KeyCode::Char(ch));
        }
        search_for(&mut app, "cassa");
        key(&mut app, KeyCode::Esc);
        assert!(app.memo_search.is_none(), "Esc closed the bar");
        assert_eq!(app.memo_input.as_str(), "half a thought", "…and not the message being written");
        assert!(app.memo_open, "…nor the view");

        // leaving the Memorandum takes the whole search with it
        search_for(&mut app, "cassaforte");
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Esc); // the bar
        key(&mut app, KeyCode::Esc); // the composer's line
        key(&mut app, KeyCode::Esc); // the view
        assert!(!app.memo_open);
        assert!(app.memo_search.is_none() && app.memo_jump.is_none(), "nothing was left behind");

        // …and so does the lock, from inside the bar
        key(&mut app, KeyCode::Char(memo_key(Lang::En)));
        search_for(&mut app, "cassaforte");
        key(&mut app, KeyCode::Enter);
        assert!(!app.memo_search.as_ref().unwrap().hits.is_empty());
        key_ctrl(&mut app, KeyCode::Char('l'));
        assert!(app.locked.is_some() && app.vault.is_none(), "it locked");
        assert!(app.memo_search.is_none() && app.memo_jump.is_none() && !app.memo_open);
    }

    #[test]
    fn a_stale_query_answer_is_never_shown_for_the_search_on_screen() {
        // embedding takes seconds on a cold model; by then the bar may have
        // been re-typed, re-moded or closed. The answer belongs to a search
        // that no longer exists and must not land on the one that does.
        let (mut app, _d) = memo_rag_app();
        send(&mut app, "le chiavi sono sotto il tappeto");
        key_ctrl(&mut app, KeyCode::Char('f')); // the bar catches the thread up
        pump_rag(&mut app);
        key(&mut app, KeyCode::Tab);
        search_for(&mut app, "chiavi");
        key(&mut app, KeyCode::Enter);
        let stale = app.memo_search.as_ref().unwrap().pending.expect("a query is out");
        // one more character: the bar has moved on
        key(&mut app, KeyCode::Char('!'));
        assert!(app.memo_search.as_ref().unwrap().pending.is_none(), "the old query was abandoned");
        for _ in 0..40 {
            app.on_tick();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let search = app.memo_search.as_ref().unwrap();
        assert!(search.hits.is_empty() && search.ran.is_none(), "the stale answer was dropped");
        assert!(stale > 0);
    }

    #[test]
    fn the_search_bar_keybar_promises_only_what_the_bar_does() {
        let (mut app, _d) = memo_app();
        send(&mut app, "il codice della cassaforte");
        app.status = None;
        // the composer's bar advertises the way in
        let bar = keybar_of(&render_sized(&app, 120, 30));
        assert!(bar.contains("Ctrl+F") && bar.contains("search"), "no way in advertised:\n{bar}");

        key_ctrl(&mut app, KeyCode::Char('f'));
        app.status = None;
        let bar = keybar_of(&render_sized(&app, 120, 30));
        for chip in ["Tab", "mode", "↑↓", "results", "Esc", "close", "search"] {
            assert!(bar.contains(chip), "the search keybar lost '{chip}':\n{bar}");
        }
        for chip in ["send", "photo", "scroll"] {
            assert!(!bar.contains(chip), "the bar promises '{chip}', which it does not do:\n{bar}");
        }
        // ⏎ changes meaning once there is somewhere to jump: so does its chip
        search_for(&mut app, "codice");
        key(&mut app, KeyCode::Enter);
        app.status = None;
        let bar = keybar_of(&render_sized(&app, 120, 30));
        assert!(bar.contains("go to"), "⏎ is the jump now:\n{bar}");
        app.settings.lang = Lang::It;
        let bar = keybar_of(&render_sized(&app, 120, 30));
        for chip in ["vai a", "modo", "risultati", "chiudi"] {
            assert!(bar.contains(chip), "the Italian search keybar lost '{chip}':\n{bar}");
        }
    }

    #[test]
    fn the_search_bar_never_panics_at_any_size() {
        const SIZES: [(u16, u16); 5] = [(1, 1), (6, 4), (40, 24), (80, 24), (200, 60)];
        let (mut app, _d) = memo_rag_app();
        for i in 0..30 {
            send(&mut app, &format!("message number {i} — le chiavi sono sotto il tappeto"));
        }
        // (1) an empty bar, (2) a query with no run behind it, (3) a full list
        // of literal hits, (4) the same in RAG with its scores, (5) the working
        // state, (6) an empty result, (7) a jump's band — at every size.
        key_ctrl(&mut app, KeyCode::Char('f')); // …which is also the index pass
        pump_rag(&mut app);
        for (w, h) in SIZES {
            let _ = render_sized(&app, w, h);
        }
        search_for(&mut app, "chiavi");
        for (w, h) in SIZES {
            let _ = render_sized(&app, w, h);
        }
        key(&mut app, KeyCode::Enter);
        assert!(app.memo_search.as_ref().unwrap().hits.len() > 1);
        for scroll in [0usize, 3, usize::MAX] {
            app.memo_scroll = scroll;
            for (w, h) in SIZES {
                let _ = render_sized(&app, w, h);
            }
        }
        app.memo_scroll = 0;
        key(&mut app, KeyCode::Tab);
        key(&mut app, KeyCode::Enter); // → the worker: the "searching…" strip
        for (w, h) in SIZES {
            let _ = render_sized(&app, w, h);
        }
        pump_search(&mut app);
        for (w, h) in SIZES {
            let _ = render_sized(&app, w, h);
        }
        // an empty result
        for _ in 0.."chiavi".len() {
            key(&mut app, KeyCode::Backspace);
        }
        key(&mut app, KeyCode::Tab); // back to the literal search
        search_for(&mut app, "grappa");
        key(&mut app, KeyCode::Enter);
        for (w, h) in SIZES {
            let _ = render_sized(&app, w, h);
        }
        // …and the band a jump leaves behind, including on a message that went
        // away under it
        for _ in 0.."grappa".len() {
            key(&mut app, KeyCode::Backspace);
        }
        search_for(&mut app, "chiavi");
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Enter);
        assert!(app.memo_jump.is_some());
        for (w, h) in SIZES {
            let _ = render_sized(&app, w, h);
        }
        app.memo_jump = Some("a message nobody has".to_string());
        for (w, h) in SIZES {
            let _ = render_sized(&app, w, h);
        }
    }

    // ---- searching the item list (the `/` box) ---------------------------

    /// A vault with four items, the deterministic embedder behind it and every
    /// one of them already indexed — no model, no network, no display in here
    /// either.
    fn items_rag_app() -> (App, TempDir) {
        let (mut app, dir) = app_with(&["bruno"]);
        app.use_mock_rag();
        open_first_vault(&mut app);
        push_item(&mut app, "login", "Router di casa", &[("username", "admin", false), ("password", "s3cret", true)]);
        push_item(&mut app, "email", "Amazon", &[("email", "bruno@gmail.com", false)]);
        push_item(&mut app, "credit_card", "Carta Bancomat", &[("holder", "Bruno G", false)]);
        push_item(&mut app, "account", "Spid", &[("username", "galimibruno1", false)]);
        app.rag_index_items();
        pump_rag_items(&mut app);
        (app, dir)
    }

    /// Focuses the `/` box (if it isn't already) and types `query` into it.
    fn filter_for(app: &mut App, query: &str) {
        if !app.search_focus {
            key(app, KeyCode::Char('/'));
        }
        for ch in query.chars() {
            key(app, KeyCode::Char(ch));
        }
    }

    /// Ticks until the item search has answered THE QUERY THAT IS ON SCREEN —
    /// a run for a query the box has since moved on from is not the answer this
    /// is waiting for (see `App::rerun_vault_search`).
    fn pump_item_search(app: &mut App) {
        for _ in 0..600 {
            app.on_tick();
            let s = &app.vault_search;
            let settled = s.ran == Some(crate::app::SearchMode::Literal)
                || (s.ran.is_some() && !s.searching() && s.asked == app.vault_query.trim());
            if settled {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("the item search never answered");
    }

    /// The ids of the items the ledger is showing, in the order it shows them.
    fn visible_ids(app: &App) -> Vec<String> {
        let vault = app.vault.as_ref().expect("an open vault");
        app.visible_item_indices().iter().map(|&i| vault.items()[i].id.clone()).collect()
    }

    /// What the mock ranks `query` against the indexed items as, best first —
    /// its vectors are hashes, so "similar" here is arithmetic we can demand
    /// exactly.
    fn expected_rank(app: &App, query: &str) -> Vec<String> {
        use crate::rag::MockEmbedder;
        let qv = MockEmbedder::vector_of(&format!("query: {query}"));
        let mut scored: Vec<(String, f32)> = app
            .item_index_texts()
            .into_iter()
            .map(|(id, text)| {
                let v = MockEmbedder::vector_of(&format!("passage: {text}"));
                let score = v.iter().zip(&qv).map(|(a, b)| a * b).sum::<f32>();
                (id, score)
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        scored.into_iter().map(|(id, _)| id).collect()
    }

    /// The ledger half of a rendered row — everything left of the docked
    /// detail's divider.
    fn ledger_of(row: &str) -> &str {
        row.split('│').next().unwrap_or(row)
    }

    #[test]
    fn tab_flips_the_item_filter_between_literal_and_rag() {
        let (mut app, _d) = items_rag_app();
        filter_for(&mut app, "router");
        let text = render_sized(&app, 110, 24);
        assert!(text.contains("aa literal"), "the box opens on the literal search:\n{text}");
        assert!(!text.contains("◈ RAG"), "…and only that chip is shown:\n{text}");
        assert_eq!(app.left_len(), 1, "…which really filters");

        key(&mut app, KeyCode::Tab);
        assert_eq!(app.vault_search.mode, crate::app::SearchMode::Rag, "Tab asked for the other one");
        pump_item_search(&mut app);
        let text = render_sized(&app, 110, 24);
        assert!(text.contains("◈ RAG"), "Tab flipped the chip:\n{text}");
        assert!(!text.contains("aa literal"), "…and the other one is gone:\n{text}");

        key(&mut app, KeyCode::Tab);
        assert!(render_sized(&app, 110, 24).contains("aa literal"), "…and back again");
        assert_eq!(app.left_len(), 1, "…filtering exactly as it did before");
        // the chip is localized like every other word on screen
        app.settings.lang = Lang::It;
        assert!(render_sized(&app, 110, 24).contains("aa letterale"));
        key(&mut app, KeyCode::Tab);
        assert!(render_sized(&app, 110, 24).contains("◈ RAG"), "the acronym is the same word");
    }

    #[test]
    fn the_rag_filter_ranks_the_items_on_a_worker_and_badges_every_row() {
        let (mut app, _d) = items_rag_app();
        let query = "come entro nel modem di casa";
        let expected = expected_rank(&app, query);
        let stored = visible_ids(&app);
        assert_ne!(stored, expected, "the ranking is not the stored order (else this proves nothing)");

        key(&mut app, KeyCode::Char('/'));
        key(&mut app, KeyCode::Tab); // → RAG
        filter_for(&mut app, query);
        // the embed is on the worker: the key press does NOT block on it
        assert!(app.vault_search.searching(), "the query went out to the worker");
        assert!(render_sized(&app, 110, 24).contains("searching…"), "…and the row says so");
        pump_item_search(&mut app);

        assert_eq!(app.vault_search.ran, Some(crate::app::SearchMode::Rag), "the RAG search ran");
        assert_eq!(visible_ids(&app), expected, "the list came back in dot-product order");
        assert_eq!(app.left_len(), 4, "…with nothing filtered out: a ranking is not a filter");
        let scores: Vec<u8> = expected
            .iter()
            .map(|id| app.item_rank_score(id).expect("a ranked row carries its score"))
            .collect();
        for pair in scores.windows(2) {
            assert!(pair[0] >= pair[1], "best first: {scores:?}");
        }
        // …and the badge is on the row itself, right-aligned past the subtitle
        let top = app.vault.as_ref().unwrap().items().iter().find(|i| i.id == expected[0]).unwrap().label.clone();
        let text = render_sized(&app, 110, 24);
        let row = text.lines().find(|l| l.contains(&top)).expect("the best match is on screen");
        assert!(
            ledger_of(row).trim_end().ends_with(&scores[0].to_string()),
            "the badge sits at the end of its row: {row:?}"
        );

        // an item nothing has embedded yet ranks last, with no number at all
        push_item(&mut app, "wifi", "Rete di casa", &[("ssid", "casa", false)]);
        let fresh = app.vault.as_ref().unwrap().items().last().unwrap().id.clone();
        assert_eq!(app.item_rank_score(&fresh), None, "no vector, no badge");
        assert_eq!(visible_ids(&app).last(), Some(&fresh), "…and it sits after every ranked row");
    }

    #[test]
    fn a_rag_item_search_that_cannot_run_filters_literally_and_says_why_once() {
        // never a dead search, never a silent wrong mode: the list still
        // answers, and the chip owns up to WHICH search answered it.
        let (mut app, _d) = app_with(&["bruno"]);
        app.rag.use_mock();
        app.settings.rag_memo = true; // the OTHER index — this one is off
        app.settings.rag_items = false;
        open_first_vault(&mut app);
        push_item(&mut app, "email", "Amazon", &[("email", "bruno@gmail.com", false)]);
        push_item(&mut app, "account", "Spid", &[("username", "galimibruno1", false)]);

        key(&mut app, KeyCode::Char('/'));
        key(&mut app, KeyCode::Tab); // → RAG
        filter_for(&mut app, "spid");
        assert!(!app.rag.busy(), "no worker was started for an index that was never built");
        assert_eq!(app.vault_search.mode, crate::app::SearchMode::Rag, "the ASK is unchanged");
        assert_eq!(app.vault_search.ran, Some(crate::app::SearchMode::Literal), "…the RUN is honest");
        assert_eq!(app.left_len(), 1, "and it really searched");
        assert_eq!(
            app.status.as_ref().map(|(_, s)| s.clone()).unwrap_or_default(),
            i18n::t(Lang::En, &Key::SearchRagOff),
            "…and said why instead of ranking nothing"
        );
        let text = render_sized(&app, 110, 24);
        assert!(text.contains("aa literal") && !text.contains("◈ RAG"), "the chip is honest:\n{text}");

        // …once. A reason repeated on every keystroke is noise, not honesty.
        app.status = None;
        key(&mut app, KeyCode::Char('!'));
        assert!(app.status.is_none(), "the reason was already given");

        // …and the same with the model simply not on this machine yet
        let (mut app, _d) = app_with(&["bruno"]);
        app.settings.rag_items = true;
        app.rag.set_enabled(true); // opted in; the weights are not here
        assert!(!app.rag.searchable());
        open_first_vault(&mut app);
        push_item(&mut app, "account", "Spid", &[("username", "galimibruno1", false)]);
        key(&mut app, KeyCode::Char('/'));
        key(&mut app, KeyCode::Tab);
        filter_for(&mut app, "spid");
        assert!(!app.rag.busy(), "no worker without a model");
        assert_eq!(app.vault_search.ran, Some(crate::app::SearchMode::Literal));
        assert_eq!(app.left_len(), 1);
        assert_eq!(
            app.status.as_ref().map(|(_, s)| s.clone()).unwrap_or_default(),
            i18n::t(Lang::En, &Key::SearchRagAbsent),
            "the reason is the true one"
        );
        // …localized like everything else: the ASK survived, so a flip back and
        // forth asks for RAG again and gets the same reason in Italian
        app.settings.lang = Lang::It;
        app.status = None;
        key(&mut app, KeyCode::Tab); // → literal
        key(&mut app, KeyCode::Tab); // → RAG again
        assert_eq!(
            app.status.as_ref().map(|(_, s)| s.clone()).unwrap_or_default(),
            i18n::t(Lang::It, &Key::SearchRagAbsent)
        );
        assert!(render_sized(&app, 110, 24).contains("aa letterale"));
    }

    #[test]
    fn an_arriving_ranking_keeps_the_cursor_on_the_item_it_was_on() {
        // the answer lands on a TICK, seconds after the query was typed and with
        // no key pressed: the order under the cursor changes by itself. If the
        // cursor stayed on its index, `c` would copy — and `d` delete — a row
        // nobody ever chose.
        let (mut app, _d) = items_rag_app();
        let query = "come entro nel modem di casa";
        let stored = visible_ids(&app);
        let expected = expected_rank(&app, query);
        // a row the ranking really moves, else this would prove nothing
        let row = (0..stored.len())
            .find(|&i| expected[i] != stored[i])
            .expect("the ranking is not the stored order");

        key(&mut app, KeyCode::Char('/'));
        key(&mut app, KeyCode::Tab); // → RAG
        filter_for(&mut app, query);
        key(&mut app, KeyCode::Enter); // drop into the list, still unranked
        assert!(!app.search_focus && app.selected == 0);
        for _ in 0..row {
            key(&mut app, KeyCode::Down);
        }
        let watched = app.highlighted_item().expect("a row under the cursor").id.clone();
        assert_eq!(watched, stored[row], "(the cursor is on the row we are watching)");

        pump_item_search(&mut app);
        assert_eq!(visible_ids(&app), expected, "the answer reordered the ledger");
        assert_eq!(
            app.highlighted_item().unwrap().id,
            watched,
            "the cursor followed the ITEM, not the index it happened to sit at"
        );
        assert_eq!(app.selected, expected.iter().position(|id| *id == watched).unwrap());
    }

    #[test]
    fn leaving_the_box_keeps_the_ranking_and_esc_takes_it_away() {
        let (mut app, _d) = items_rag_app();
        let query = "come entro nel modem di casa";
        let expected = expected_rank(&app, query);
        let stored = visible_ids(&app);
        key(&mut app, KeyCode::Char('/'));
        key(&mut app, KeyCode::Tab);
        filter_for(&mut app, query);
        pump_item_search(&mut app);
        assert_eq!(visible_ids(&app), expected);

        // ⏎ drops into the list — the ranked one, on its best row
        key(&mut app, KeyCode::Enter);
        assert!(!app.search_focus, "⏎ handed the keyboard to the list");
        assert_eq!(app.selected, 0, "…on the best match");
        assert_eq!(visible_ids(&app), expected, "…which is still in score order");
        assert!(render_sized(&app, 110, 24).contains("◈ RAG"), "the chip stays with the query");

        // …and ⏎ again opens the detail of THAT row, not of the stored first item
        key(&mut app, KeyCode::Enter);
        assert!(app.detail_focused(), "the docked detail has the keyboard");
        assert_eq!(app.highlighted_item().unwrap().id, expected[0], "the right item");
        let top = app.vault.as_ref().unwrap().items().iter().find(|i| i.id == expected[0]).unwrap().label.clone();
        assert!(render_sized(&app, 110, 24).contains(&top), "…and it is what the pane shows");

        // back up into the box: Esc empties the query AND the ranking with it
        key(&mut app, KeyCode::Esc); // the detail gives the keyboard back
        key(&mut app, KeyCode::Up); // …and the top of the list reaches the box
        assert!(app.search_focus);
        key(&mut app, KeyCode::Esc);
        assert!(app.vault_query.is_empty(), "the query is gone");
        assert!(app.vault_search.ranked.is_empty() && app.vault_search.ran.is_none(), "…and so is the order it left");
        assert_eq!(visible_ids(&app), stored, "the list is back to the vault's own order");
        assert_eq!(app.vault_search.mode, crate::app::SearchMode::Rag, "the ASK survives a cleared box");
    }

    #[test]
    fn the_item_search_is_wiped_when_the_vault_locks_or_closes() {
        // a query — and the order it put the ledger in — says what you were
        // looking for INSIDE this vault. Both leave with it.
        use crate::app::SearchMode;
        let (mut app, _d) = items_rag_app();
        key(&mut app, KeyCode::Char('/'));
        key(&mut app, KeyCode::Tab);
        filter_for(&mut app, "come entro nel modem di casa");
        pump_item_search(&mut app);
        assert!(!app.vault_search.ranked.is_empty());

        key_ctrl(&mut app, KeyCode::Char('l'));
        assert!(app.locked.is_some() && app.vault.is_none(), "it locked");
        assert!(app.vault_query.is_empty(), "the query went with the vault");
        assert!(app.vault_search.ranked.is_empty() && app.vault_search.asked.is_empty());
        assert_eq!(app.vault_search.mode, SearchMode::Literal, "…and the box is fresh again");
        assert!(!app.search_focus);

        // …and closing it does exactly the same
        for ch in "pw".chars() {
            key(&mut app, KeyCode::Char(ch));
        }
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.page, Page::Vault, "re-unlocked");
        key(&mut app, KeyCode::Char('/'));
        key(&mut app, KeyCode::Tab);
        filter_for(&mut app, "modem");
        pump_item_search(&mut app);
        assert!(!app.vault_search.ranked.is_empty());
        key(&mut app, KeyCode::Esc); // the query
        key(&mut app, KeyCode::Esc); // the box
        key(&mut app, KeyCode::Esc); // the vault
        assert_eq!(app.page, Page::Access);
        assert_eq!(app.vault_search.mode, SearchMode::Literal);
        assert!(app.vault_search.ranked.is_empty() && app.vault_query.is_empty());
    }

    #[test]
    fn the_item_filter_keybar_promises_only_what_the_box_does() {
        let (mut app, _d) = items_rag_app();
        app.status = None;
        // (wide: the list's own bar carries every verb before the `/` chip)
        let bar = keybar_of(&render_sized(&app, 200, 30));
        assert!(bar.contains("filter"), "the list advertises the way in:\n{bar}");

        key(&mut app, KeyCode::Char('/'));
        app.status = None;
        let bar = keybar_of(&render_sized(&app, 120, 30));
        for chip in ["⏎", "list", "Tab", "mode", "←→", "category", "Esc", "clear"] {
            assert!(bar.contains(chip), "the filter keybar lost '{chip}':\n{bar}");
        }
        // a chip is a promise: the list's verbs are TEXT in here, so none of
        // their labels may sit on the bar
        for chip in ["copy", "star", "delete", "memo"] {
            assert!(!bar.contains(chip), "the bar promises '{chip}', which is text here:\n{bar}");
        }
        app.settings.lang = Lang::It;
        let bar = keybar_of(&render_sized(&app, 120, 30));
        for chip in ["lista", "modo", "categoria", "pulisci"] {
            assert!(bar.contains(chip), "the Italian filter keybar lost '{chip}':\n{bar}");
        }
    }

    #[test]
    fn the_ranked_item_list_never_panics_at_any_size() {
        const SIZES: [(u16, u16); 5] = [(1, 1), (6, 4), (40, 24), (80, 24), (200, 60)];
        let (mut app, _d) = items_rag_app();
        // …plus a dozen items nothing has embedded: they rank last, badgeless
        for i in 0..12 {
            push_item(
                &mut app,
                &format!("category_number_{i}"),
                &format!("voce numero {i} con un nome davvero lunghissimo"),
                &[("nota", "qualcosa di scritto qui dentro", false)],
            );
        }
        // (1) the empty box, (2) a literal query, (3) the RAG box while it is
        // working, (4) the ranking with its badges, (5) the list under it, and
        // (6) the detail of a ranked row — at every size.
        key(&mut app, KeyCode::Char('/'));
        for (w, h) in SIZES {
            let _ = render_sized(&app, w, h);
        }
        filter_for(&mut app, "modem di casa");
        for (w, h) in SIZES {
            let _ = render_sized(&app, w, h);
        }
        key(&mut app, KeyCode::Tab); // → RAG, and the query goes out
        for (w, h) in SIZES {
            let _ = render_sized(&app, w, h);
        }
        pump_item_search(&mut app);
        for (w, h) in SIZES {
            let _ = render_sized(&app, w, h);
        }
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Enter);
        for (w, h) in SIZES {
            let _ = render_sized(&app, w, h);
        }
    }

    #[test]
    fn wrap_words_fills_lines_without_ever_overflowing_one() {
        assert_eq!(wrap_words("a bb ccc", 3), vec!["a", "bb", "ccc"]);
        assert_eq!(wrap_words("a bb ccc", 6), vec!["a bb", "ccc"]);
        assert_eq!(wrap_words("", 10), Vec::<String>::new());
        // a width nothing fits in yields nothing rather than looping forever
        assert_eq!(wrap_words("anything", 0), Vec::<String>::new());
        // a word no line can hold is cut, not overflowed
        assert_eq!(wrap_words("abcdefgh", 3), vec!["abc", "def", "gh"]);
        assert_eq!(wrap_words("hi abcdefgh yo", 3), vec!["hi", "abc", "def", "gh", "yo"]);
        // …and no produced line is ever wider than asked, whatever the text
        for w in 1..12usize {
            for s in ["", "a", "però 日本語 ok", &"x".repeat(40), "one two three four five"] {
                for line in wrap_words(s, w) {
                    assert!(line.chars().count() <= w, "{s:?} at {w}: {line:?}");
                    assert!(!line.is_empty(), "no empty rows");
                }
            }
        }
    }

    #[test]
    fn wrap_lines_gives_every_typed_newline_a_row_of_its_own() {
        assert_eq!(wrap_lines("one\ntwo", 10), vec!["one", "two"]);
        // a blank line is the paragraph break the user typed, so it is a row
        assert_eq!(wrap_lines("a\n\nb", 10), vec!["a", "", "b"]);
        // …and each of those lines is still wrapped like any other
        assert_eq!(wrap_lines("aaa bbb\nccc", 3), vec!["aaa", "bbb", "ccc"]);
        assert!(wrap_lines("one\ntwo", 0).is_empty(), "no column, no rows");
        // …but a text with nothing in it is no rows at all: a photo message
        // carries one, and it must not grow a blank line under the picture.
        assert!(wrap_lines("", 10).is_empty());
        // a text with no newline in it is exactly what `wrap_words` made of it
        for s in ["", "a sentence with several ordinary words", &"x".repeat(30)] {
            assert_eq!(wrap_lines(s, 9), wrap_words(s, 9), "{s:?}");
        }
        // no row is ever wider than asked, newlines or not
        for w in 1..12usize {
            for line in wrap_lines("però\n日本語 ok\n\nlungorepetitionsenzaspazi", w) {
                assert!(line.chars().count() <= w, "at {w}: {line:?}");
            }
        }
    }

    // ---- the shared insertion caret, at every text-input site --------------
    //
    // "abc", Left, Left, insert 'X' -> "aXbc" is the one-line proof that the
    // caret lands in the MIDDLE, not the end, wherever text is typed.

    #[test]
    fn a_form_text_field_takes_an_insertion_caret() {
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        open_add_form_for(&mut app, "login");
        // the add form opens on the item-label row — a plain text field
        for ch in "abc".chars() {
            key(&mut app, KeyCode::Char(ch));
        }
        key(&mut app, KeyCode::Left);
        key(&mut app, KeyCode::Left);
        key(&mut app, KeyCode::Char('X'));
        let f = app.form.as_ref().unwrap();
        assert!(!f.fields[f.focused].is_choice(), "the focused row is text");
        assert_eq!(f.fields[f.focused].value.as_str(), "aXbc");
    }

    #[test]
    fn the_vault_search_query_takes_an_insertion_caret_and_draws_it_mid_string() {
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        key(&mut app, KeyCode::Char('/')); // focus the search box
        assert!(app.search_focus, "the / box has the keyboard");
        for ch in "abc".chars() {
            key(&mut app, KeyCode::Char(ch));
        }
        key(&mut app, KeyCode::Left);
        key(&mut app, KeyCode::Left);
        key(&mut app, KeyCode::Char('X'));
        assert_eq!(app.vault_query.as_str(), "aXbc");
        // the caret cell sits between the chars it is on, not off the end
        let text = render_sized(&app, 80, 24);
        assert!(text.contains("aX▌bc"), "the caret is drawn mid-query:\n{text}");
    }

    #[test]
    fn the_memo_composer_takes_an_insertion_caret() {
        let (mut app, _d) = memo_app();
        for ch in "abc".chars() {
            key(&mut app, KeyCode::Char(ch));
        }
        key(&mut app, KeyCode::Left);
        key(&mut app, KeyCode::Left);
        key(&mut app, KeyCode::Char('X'));
        assert_eq!(app.memo_input.as_str(), "aXbc");
        // …and the caret is drawn where it sits, on the line being typed
        let text = render_sized(&app, 80, 30);
        assert!(text.contains("aX▌bc"), "the composer caret is mid-line:\n{text}");
    }

    #[test]
    fn the_palette_query_takes_an_insertion_caret_and_forward_deletes() {
        let (mut app, _d) = app_with(&["bruno"]);
        key(&mut app, KeyCode::Char(':')); // open the command palette
        assert!(app.palette.is_some());
        for ch in "abc".chars() {
            key(&mut app, KeyCode::Char(ch));
        }
        key(&mut app, KeyCode::Left);
        key(&mut app, KeyCode::Left);
        key(&mut app, KeyCode::Char('X'));
        assert_eq!(app.palette.as_ref().unwrap().query.as_str(), "aXbc");
        let text = render_sized(&app, 80, 24);
        assert!(text.contains("aX▌bc"), "the palette caret is mid-string:\n{text}");
        // Delete removes the char AT the caret (forward): Home then Delete drops
        // the leading 'a'
        key(&mut app, KeyCode::Home);
        key(&mut app, KeyCode::Delete);
        assert_eq!(app.palette.as_ref().unwrap().query.as_str(), "Xbc", "Delete forward-deletes");
    }

    #[test]
    fn the_unlock_passphrase_takes_an_insertion_caret() {
        let (mut app, _d) = app_with(&["bruno"]);
        key(&mut app, KeyCode::Enter); // unfold the inline unlock
        assert!(app.unlock.is_some());
        for ch in "abc".chars() {
            key(&mut app, KeyCode::Char(ch));
        }
        key(&mut app, KeyCode::Left);
        key(&mut app, KeyCode::Left);
        key(&mut app, KeyCode::Char('X'));
        assert_eq!(app.unlock.as_ref().unwrap().input.as_str(), "aXbc");
    }

    #[test]
    fn a_form_choice_field_still_cycles_on_left_and_right() {
        let (mut app, _d) = app_with(&["alice", "bruno"]);
        typed(&mut app, "exportvault");
        key(&mut app, KeyCode::Enter);
        // step up onto the vault chooser (a CHOICE field) and cycle it
        key(&mut app, KeyCode::Up);
        let form = app.form.as_ref().unwrap();
        assert!(form.fields[form.focused].is_choice(), "the vault chooser is a choice");
        let before = form.fields[form.focused].selected;
        key(&mut app, KeyCode::Right);
        let after = app.form.as_ref().unwrap();
        assert_ne!(after.fields[after.focused].selected, before, "←/→ still cycle a choice");
    }

    #[test]
    fn category_tabs_still_cycle_on_left_right_off_the_search_box() {
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        push_item(&mut app, "email", "Amazon", &[("email", "a@b.com", false)]);
        assert!(!app.search_focus, "browsing the list, not the / box");
        assert_eq!(app.vault_tab, 0);
        key(&mut app, KeyCode::Right);
        assert_ne!(app.vault_tab, 0, "←/→ cycle the category tab off the box");
        key(&mut app, KeyCode::Left);
        assert_eq!(app.vault_tab, 0, "…and back the other way");
    }

    #[test]
    fn a_secret_form_field_edited_mid_string_never_leaks_its_length_or_chars() {
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        open_add_form_for(&mut app, "login");
        // focus the password row (the one secret field) and type into it
        let idx = {
            let f = app.form.as_ref().unwrap();
            f.fields.iter().position(|fd| fd.secret).expect("login has a secret field")
        };
        app.form.as_mut().unwrap().focused = idx;
        for ch in "abcdef".chars() {
            key(&mut app, KeyCode::Char(ch));
        }
        let bullets = |s: &str| s.chars().filter(|c| *c == '•').count();
        assert_eq!(bullets(&render_sized(&app, 80, 24)), 6, "six typed chars → six bullets");
        // walk the caret into the middle: the mask must NOT change with it
        key(&mut app, KeyCode::Left);
        key(&mut app, KeyCode::Left);
        assert_eq!(
            bullets(&render_sized(&app, 80, 24)),
            6,
            "the mask length does not follow the caret"
        );
        // insert mid-string: the value grows, and the cleartext never shows
        key(&mut app, KeyCode::Char('X'));
        let f = app.form.as_ref().unwrap();
        assert_eq!(f.fields[idx].value.as_str(), "abcdXef", "the char landed mid-string");
        let r = render_sized(&app, 80, 24);
        assert_eq!(bullets(&r), 7, "one more bullet for one more char, nothing else");
        assert!(!r.contains("abcd") && !r.contains("Xef"), "no secret char reaches the screen:\n{r}");
    }

    #[test]
    fn a_form_with_a_long_value_and_the_caret_anywhere_never_panics() {
        const SIZES: [(u16, u16); 5] = [(1, 1), (6, 4), (24, 8), (80, 24), (200, 60)];
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        open_add_form_for(&mut app, "login");
        for ch in "x".repeat(300).chars() {
            key(&mut app, KeyCode::Char(ch));
        }
        for pos in [0usize, 1, 150, 299, 300] {
            key(&mut app, KeyCode::Home);
            for _ in 0..pos {
                key(&mut app, KeyCode::Right);
            }
            for (w, h) in SIZES {
                let _ = render_sized(&app, w, h); // must not panic at any size
            }
        }
        // deletions at both edges (Delete at the end is a no-op, never a panic)
        key(&mut app, KeyCode::Home);
        key(&mut app, KeyCode::Delete);
        key(&mut app, KeyCode::End);
        key(&mut app, KeyCode::Delete);
        for (w, h) in SIZES {
            let _ = render_sized(&app, w, h);
        }
    }

    #[test]
    fn the_memo_composer_with_a_long_multiline_value_and_the_caret_anywhere_never_panics() {
        const SIZES: [(u16, u16); 5] = [(1, 1), (6, 4), (24, 8), (80, 24), (200, 60)];
        let (mut app, _d) = memo_app();
        for ch in "a rather long first line that will have to scroll sideways ".repeat(3).chars() {
            key(&mut app, KeyCode::Char(ch));
        }
        key_shift(&mut app, KeyCode::Enter); // a newline in the middle of the buffer
        for ch in "and a second line, itself none too short at all ".repeat(3).chars() {
            key(&mut app, KeyCode::Char(ch));
        }
        let n = app.memo_input.as_str().chars().count();
        for pos in [0usize, 1, n / 2, n.saturating_sub(1), n] {
            key(&mut app, KeyCode::Home);
            for _ in 0..pos {
                key(&mut app, KeyCode::Right);
            }
            for (w, h) in SIZES {
                let _ = render_sized(&app, w, h);
            }
        }
        key(&mut app, KeyCode::End);
        key(&mut app, KeyCode::Delete); // no-op at the end
        for (w, h) in SIZES {
            let _ = render_sized(&app, w, h);
        }
    }
    // ---- `l`: login in the browser through the extension -------------------

    /// A vault with one login-capable item and login assist on, the API on an
    /// OS-picked port — the state `l` is meant for.
    fn login_app() -> (App, TempDir, std::sync::MutexGuard<'static, ()>) {
        let guard = crate::api::port_guard();
        let (mut app, dir) = app_with(&["bruno"]);
        app.settings.api_port = 0;
        app.settings.login_assist = true;
        app.settings.ensure_login_token();
        open_first_vault(&mut app);
        push_item(
            &mut app,
            "login",
            "Acme",
            &[("username", "bruno@example.com", false), ("password", "s3cr3t", true), ("url", "acme.example.com", false)],
        );
        app.sync_api_server();
        (app, dir, guard)
    }

    #[test]
    fn l_opens_the_site_with_a_one_shot_nonce_the_extension_can_claim() {
        let (mut app, _d, _guard) = login_app();
        assert!(app.api.is_some(), "login assist keeps the API up with a vault open");
        key(&mut app, KeyCode::Char('l'));
        // the opener got the site, with the nonce in the fragment (never sent
        // to the server) and nothing else
        assert_eq!(app.opened.len(), 1, "exactly one open: {:?}", app.opened);
        let url = app.opened[0].clone();
        assert!(url.starts_with("https://acme.example.com#dsl="), "{url}");
        let nonce = url.split("#dsl=").nth(1).expect("a nonce").to_string();
        assert_eq!(nonce.len(), 64, "32 random bytes, hex");
        assert!(!url.contains("s3cr3t"), "the URL carries no secret");
        let server = app.api.as_ref().unwrap();
        assert_eq!(server.pending_logins(), 1, "the offer is waiting to be claimed");
        let status = app.status.as_ref().map(|(_, s)| s.clone()).unwrap_or_default();
        assert_eq!(status, i18n::t(Lang::En, &Key::LoginOffered("acme.example.com")));
        // …and the extension, with the pairing token, claims it from that site
        let token = app.settings.login_token().unwrap().to_string();
        let claim = format!("{{\"nonce\":\"{nonce}\",\"origin\":\"https://acme.example.com/login\"}}");
        let (status, body) = crate::api::probe(server.port(), "POST", "/login/claim", Some(&token), Some(&claim));
        assert_eq!(status, 200, "{body}");
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["username"], "bruno@example.com");
        assert_eq!(json["password"], "s3cr3t");
        assert_eq!(server.pending_logins(), 0, "claimed once");
        // the keybar promises it, in both languages, from the keymap
        app.status = None;
        let bar = keybar_of(&render_sized(&app, 200, 30));
        assert!(bar.contains(" l ") && bar.contains("login"), "{bar}");
        app.settings.lang = Lang::It;
        let bar = keybar_of(&render_sized(&app, 200, 30));
        assert!(bar.contains(" l ") && bar.contains("accedi"), "{bar}");
    }

    #[test]
    fn l_says_what_is_missing_and_opens_nothing_until_it_can() {
        let toast = |app: &App| app.status.as_ref().map(|(_, s)| s.clone()).unwrap_or_default();
        // login assist off: the switch is named, nothing opens, nothing is offered
        let (mut app, _d) = app_with(&["bruno"]);
        open_first_vault(&mut app);
        push_item(
            &mut app,
            "login",
            "Acme",
            &[("username", "u", false), ("password", "p", true), ("url", "acme.example.com", false)],
        );
        key(&mut app, KeyCode::Char('l'));
        assert_eq!(toast(&app), i18n::t(Lang::En, &Key::LoginAssistOff));
        assert!(app.opened.is_empty() && app.api.is_none());
        // an item that cannot log in says which part it lacks — before the
        // switch is even consulted
        push_item(&mut app, "note", "Bare", &[("body", "x", false)]);
        push_item(&mut app, "login", "NoPw", &[("username", "u", false), ("password", "", true), ("url", "acme.example.com", false)]);
        push_item(&mut app, "login", "NoUser", &[("password", "p", true), ("url", "acme.example.com", false)]);
        for (row, expected) in [(1, Key::NoWebsiteField), (2, Key::NoPasswordField), (3, Key::NoUserField)] {
            app.selected = row;
            key(&mut app, KeyCode::Char('l'));
            assert_eq!(toast(&app), i18n::t(Lang::En, &expected), "row {row}");
        }
        assert!(app.opened.is_empty(), "nothing was ever opened: {:?}", app.opened);
        // …and in italian, on the same key
        app.settings.lang = Lang::It;
        app.selected = 0;
        key(&mut app, KeyCode::Char('l'));
        assert_eq!(toast(&app), i18n::t(Lang::It, &Key::LoginAssistOff));
    }

    #[test]
    fn the_login_verb_follows_its_override_and_the_cheatsheet_lists_it() {
        use crate::keymap::{Verb, SHORTCUT_OFF};
        let (mut app, _d, _guard) = login_app();
        let keys = cheatsheet_of(&mut app);
        assert!(keys.contains("login"), "the cheatsheet lists it:\n{keys}");
        set_shortcut(&mut app, Verb::Login, "w");
        key(&mut app, KeyCode::Char('l'));
        assert!(app.opened.is_empty(), "the default key no longer fires");
        key(&mut app, KeyCode::Char('w'));
        assert_eq!(app.opened.len(), 1, "the chosen one does");
        set_shortcut(&mut app, Verb::Login, SHORTCUT_OFF);
        app.status = None;
        key(&mut app, KeyCode::Char('w'));
        assert_eq!(app.opened.len(), 1, "switched off, nothing fires");
        assert!(app.status.is_none());
        assert!(!keybar_of(&render_sized(&app, 200, 30)).contains("login"));
    }

    #[test]
    fn the_browser_section_renders_and_the_install_row_explains_itself() {
        let (mut app, _d) = app_with(&["bruno"]);
        open_settings(&mut app);
        let text = render_sized(&app, 100, 50);
        assert!(text.contains("BROWSER"), "the section rule is drawn:\n{text}");
        assert!(text.contains("login assist"), "{text}");
        assert!(text.contains("install extension"), "{text}");
        // the action row: ←/→ does something rather than cycling a value. With
        // no store page configured the row says so and opens nothing.
        focus_row(&mut app, settings_ui::Row::InstallExtension);
        app.detected_browser = Some(crate::browser::DefaultBrowser {
            name: "zen".into(),
            family: crate::browser::BrowserFamily::Gecko,
        });
        key(&mut app, KeyCode::Right);
        assert_eq!(
            app.status.as_ref().map(|(_, s)| s.clone()).unwrap_or_default(),
            i18n::t(Lang::En, &Key::ExtensionNotPublished)
        );
        assert!(app.opened.is_empty());
        // a browser the extension does not exist for
        app.detected_browser = Some(crate::browser::DefaultBrowser {
            name: "org.gnome.Epiphany".into(),
            family: crate::browser::BrowserFamily::Unknown,
        });
        key(&mut app, KeyCode::Left);
        assert!(app.status.as_ref().is_some_and(|(_, s)| s.contains("Epiphany")));
        // …and the store page, once configured, goes to the opener
        app.chrome = {
            let raw = "version = 4\n[extension]\nfirefox_url = \"https://addons.mozilla.org/firefox/addon/datashield/\"\n";
            let dir = TempDir::new().unwrap();
            let path = dir.path().join("ui.toml");
            std::fs::write(&path, raw).unwrap();
            crate::chrome::Chrome::load(&path).0
        };
        app.detected_browser = Some(crate::browser::DefaultBrowser {
            name: "zen".into(),
            family: crate::browser::BrowserFamily::Gecko,
        });
        key(&mut app, KeyCode::Right);
        assert_eq!(app.opened, vec!["https://addons.mozilla.org/firefox/addon/datashield/".to_string()]);
        assert!(app.settings_ui.is_some(), "the page stays open");
        // the toggle mints the pairing token on save, and only then
        assert!(app.settings.login_token.is_empty());
        focus_row(&mut app, settings_ui::Row::LoginAssist);
        key(&mut app, KeyCode::Right);
        key(&mut app, KeyCode::Enter);
        assert!(app.settings.login_assist);
        assert_eq!(app.settings.login_token.len(), 64, "minted on commit");
        let saved = crate::settings::Settings::load(&app.paths.settings_config);
        assert_eq!(saved.login_token, app.settings.login_token, "…and on the disk");
        // the token row shows its head only
        open_settings(&mut app);
        let text = render_sized(&app, 100, 50);
        assert!(text.contains(&format!("{}…", &app.settings.login_token[..8])), "{text}");
        assert!(!text.contains(&app.settings.login_token[8..]), "the whole token never reaches the screen");
        // ←/→ on that row copies the SAVED token (or says the clipboard is
        // missing, on a headless box) — and never closes the page
        focus_row(&mut app, settings_ui::Row::LoginToken);
        key(&mut app, KeyCode::Right);
        let status = app.status.as_ref().map(|(_, s)| s.clone()).unwrap_or_default();
        let copied = i18n::t(Lang::En, &Key::Copied(crate::clipboard::CLEAR_AFTER_SECS));
        let failed = i18n::t(Lang::En, &Key::CopyFailed);
        assert!(status == copied || status == failed, "unexpected: {status}");
        assert!(app.settings_ui.is_some());
        // the settings page never panics at any size with the new rows
        for i in 0..settings_ui::ALL_ROWS.len() {
            app.settings_ui.as_mut().unwrap().selected = i;
            for (w, h) in [(1u16, 1u16), (6, 4), (40, 24), (80, 24), (200, 60)] {
                let _ = render_sized(&app, w, h);
            }
        }
    }

    #[test]
    fn logintoken_copies_the_pairing_token_and_new_retires_it() {
        let (mut app, _d, _guard) = login_app();
        let before = app.settings.login_token.clone();
        typed(&mut app, "logintoken");
        key(&mut app, KeyCode::Enter);
        let status = app.status.as_ref().map(|(_, s)| s.clone()).unwrap_or_default();
        let copied = i18n::t(Lang::En, &Key::Copied(crate::clipboard::CLEAR_AFTER_SECS));
        let failed = i18n::t(Lang::En, &Key::CopyFailed);
        assert!(status == copied || status == failed, "unexpected: {status}");
        assert_eq!(app.settings.login_token, before, "a plain copy mints nothing");
        typed(&mut app, "logintoken new");
        key(&mut app, KeyCode::Enter);
        assert_ne!(app.settings.login_token, before, "`new` retired the token");
        assert_eq!(crate::settings::Settings::load(&app.paths.settings_config).login_token, app.settings.login_token);
        // the old token no longer opens the login doors; the new one does
        let port = app.api.as_ref().unwrap().port();
        let body = "{\"origin\":\"https://acme.example.com/\"}";
        assert_eq!(crate::api::probe(port, "POST", "/login/match", Some(&before), Some(body)).0, 401);
        let now = app.settings.login_token.clone();
        assert_eq!(crate::api::probe(port, "POST", "/login/match", Some(&now), Some(body)).0, 200);
        // with the switch off there is no token to copy
        app.settings.login_assist = false;
        typed(&mut app, "logintoken");
        key(&mut app, KeyCode::Enter);
        assert_eq!(
            app.status.as_ref().map(|(_, s)| s.clone()).unwrap_or_default(),
            i18n::t(Lang::En, &Key::LoginTokenUnavailable)
        );
    }
    #[test]
    fn the_browser_can_ask_for_a_vault_and_keep_a_login_it_made() {
        let (mut app, _d, _guard) = login_app();
        let server = app.api.as_ref().unwrap();
        let (port, token) = (server.port(), app.settings.login_token().unwrap().to_string());
        // a login page found no credentials: the next tick says so
        assert_eq!(crate::api::probe(port, "POST", "/login/wake", Some(&token), Some("{\"origin\":\"https://github.com/login\"}")).0, 200);
        app.on_tick();
        assert_eq!(
            app.status.as_ref().map(|(_, s)| s.clone()).unwrap_or_default(),
            i18n::t(Lang::En, &Key::BrowserWantsLogin("github.com"))
        );
        // a login made by hand on a NEW site becomes a login item
        let save = "{\"origin\":\"https://github.com/session\",\"username\":\"brxezz\",\"password\":\"gh-pass\"}";
        assert_eq!(crate::api::probe(port, "POST", "/login/save", Some(&token), Some(save)).0, 200);
        app.on_tick();
        let v = app.vault.as_ref().unwrap();
        assert_eq!(v.items().len(), 2);
        let gh = v.items().iter().find(|i| i.label == "github.com").expect("named after the host");
        assert_eq!(gh.category, "login");
        assert_eq!(crate::model::item_website(gh), Some("github.com"));
        assert_eq!(gh.fields[crate::model::item_user_index(gh).unwrap()].value.as_str(), "brxezz");
        assert_eq!(gh.fields[crate::model::item_password_index(gh).unwrap()].value.as_str(), "gh-pass");
        assert!(app.status.as_ref().is_some_and(|(_, s)| s.contains("saved")));
        // …and it reached the disk, and the extension's snapshot
        let reopened = Vault::open(&app.paths.vaults_dir, "bruno", "pw").unwrap();
        assert_eq!(reopened.items().len(), 2);
        let (status, body) = crate::api::probe(port, "POST", "/login/match", Some(&token), Some("{\"origin\":\"https://github.com/\"}"));
        assert_eq!(status, 200);
        assert!(body.contains("brxezz"), "{body}");
        // the same site + username again, new password: the item is updated, not doubled
        let again = "{\"origin\":\"https://github.com/session\",\"username\":\"BRXEZZ\",\"password\":\"gh-new\"}";
        assert_eq!(crate::api::probe(port, "POST", "/login/save", Some(&token), Some(again)).0, 200);
        app.on_tick();
        let v = app.vault.as_ref().unwrap();
        assert_eq!(v.items().len(), 2, "no duplicate");
        let gh = v.items().iter().find(|i| i.label == "github.com").unwrap();
        assert_eq!(gh.fields[crate::model::item_password_index(gh).unwrap()].value.as_str(), "gh-new");
        assert!(gh.modified_at.is_some());
        assert!(app.status.as_ref().is_some_and(|(_, s)| s.contains("updated")));
        // an email-shaped username lands in an `email` field
        let mail = "{\"origin\":\"https://example.org/\",\"username\":\"bruno@example.org\",\"password\":\"p\"}";
        assert_eq!(crate::api::probe(port, "POST", "/login/save", Some(&token), Some(mail)).0, 200);
        app.on_tick();
        let v = app.vault.as_ref().unwrap();
        let ex = v.items().iter().find(|i| i.label == "example.org").unwrap();
        assert_eq!(ex.fields[0].key, "email");
        assert_eq!(ex.fields[0].ftype, crate::model::FieldType::Email);
        // info says the vault is open, in the app's language
        app.settings.lang = Lang::It;
        app.sync_api_server();
        let (_, body) = crate::api::probe(port, "POST", "/login/info", Some(&token), Some("{}"));
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["lang"], "it");
        assert_eq!(json["vault_open"], true);
    }
}
