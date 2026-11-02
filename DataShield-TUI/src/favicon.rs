//! Website favicon fetching + caching for the item-list icons (opt-in).
//!
//! Each item's website (`url` field) yields a domain; a background worker GETs
//! `https://<domain>/favicon.ico` directly from that site (no third-party
//! service), decodes it, and keeps two forms: a 128×128 "source" for crisp
//! rendering via the terminal graphics protocol (kitty/sixel, `ratatui-image`),
//! and a tiny 4×4 grid for the half-block fallback. Below that, a colored
//! monogram. Nothing is fetched unless the `favicons` setting is on.
//!
//! The source is persisted to `<favicons dir>/<domain>.rgba`, so from the second
//! run on every icon comes off the disk with no network at all — they show up
//! offline too. A fetch that fails leaves a `<domain>.miss` marker holding a
//! retry-after timestamp, so an offline (or icon-less) site isn't dialled again
//! on every later run; a later success deletes it.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Half-block tile size in terminal cells (`ICON_COLS` px wide × `ICON_ROWS*2`
/// px tall). Used only for the fallback render.
pub const ICON_COLS: usize = 4;
pub const ICON_ROWS: usize = 2;
const SMALL_W: usize = ICON_COLS; // 4 px
const SMALL_H: usize = ICON_ROWS * 2; // 4 px
const SMALL_LEN: usize = SMALL_W * SMALL_H * 4; // 64

/// The crisp "source" favicon size kept for graphics-protocol rendering (a
/// larger source downscales more cleanly for the graphics protocol).
pub const SRC_DIM: u32 = 128;
const SRC_LEN: usize = (SRC_DIM * SRC_DIM * 4) as usize; // 65536

/// Fetch guards (untrusted network + image data).
const MAX_BODY: usize = 512 * 1024; // never read more than 512 KiB
const MAX_DIM: u32 = 1024; // refuse to decode absurdly large images

/// How long a failed fetch is remembered on disk before the site is tried
/// again (offline runs must not re-dial every domain, every run).
const MISS_TTL_SECS: u64 = 24 * 60 * 60;

struct Icon {
    source: Vec<u8>, // SRC_DIM×SRC_DIM RGBA (crisp)
    small: Vec<u8>,  // SMALL_W×SMALL_H RGBA (half-block fallback)
}

enum IconState {
    Pending,
    Ready(Icon),
    Failed,
}

/// A background favicon fetcher with an in-memory + on-disk cache.
pub struct FaviconCache {
    map: Arc<Mutex<HashMap<String, IconState>>>,
    tx: Option<Sender<String>>,
}

impl FaviconCache {
    pub fn new(dir: PathBuf) -> FaviconCache {
        let map: Arc<Mutex<HashMap<String, IconState>>> = Arc::new(Mutex::new(HashMap::new()));
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let worker_map = Arc::clone(&map);
        std::thread::Builder::new()
            .name("favicon-fetch".into())
            .spawn(move || worker(rx, dir, worker_map))
            .ok();
        FaviconCache { map, tx: Some(tx) }
    }

    /// Requests a favicon for `domain` (idempotent; queues at most one fetch).
    /// The map doubles as the per-run negative set: a domain that ended up
    /// `Failed` keeps its entry, so it is never queued a second time this run.
    pub fn request(&self, domain: &str) {
        if domain.is_empty() {
            return;
        }
        let mut map = self.map.lock().unwrap();
        if map.contains_key(domain) {
            return;
        }
        map.insert(domain.to_string(), IconState::Pending);
        drop(map);
        if let Some(tx) = &self.tx {
            let _ = tx.send(domain.to_string());
        }
    }

    /// The ready 4×4 grid for `domain` (half-block fallback), if fetched.
    pub fn pixels(&self, domain: &str) -> Option<Vec<u8>> {
        match self.map.lock().unwrap().get(domain) {
            Some(IconState::Ready(icon)) => Some(icon.small.clone()),
            _ => None,
        }
    }

    /// The ready 128×128 RGBA source for `domain` (crisp render), if fetched.
    pub fn source(&self, domain: &str) -> Option<Vec<u8>> {
        match self.map.lock().unwrap().get(domain) {
            Some(IconState::Ready(icon)) => Some(icon.source.clone()),
            _ => None,
        }
    }
}

fn worker(rx: Receiver<String>, dir: PathBuf, map: Arc<Mutex<HashMap<String, IconState>>>) {
    let _ = std::fs::create_dir_all(&dir);
    // Drain the queue disk-first: before every (slow) fetch, everything already
    // queued that the disk can answer is settled straight away, so one
    // unreachable site can't hold the cached icons behind it for three connect
    // timeouts.
    while let Ok(first) = rx.recv() {
        let mut queue: std::collections::VecDeque<String> = std::iter::once(first).collect();
        while !queue.is_empty() {
            queue.extend(rx.try_iter());
            queue.retain(|domain| !serve_from_disk(&dir, domain, &map));
            let Some(domain) = queue.pop_front() else { continue };
            let miss = miss_path(&dir, &domain);
            let state = match fetch(&domain) {
                Some(icon) => {
                    // persist the 128×128 source; the site answered, so any
                    // earlier miss marker is stale.
                    let _ = std::fs::write(icon_path(&dir, &domain), &icon.source);
                    let _ = std::fs::remove_file(&miss);
                    IconState::Ready(icon)
                }
                None => {
                    mark_miss(&miss, now_secs());
                    IconState::Failed
                }
            };
            if let Ok(mut m) = map.lock() {
                m.insert(domain, state);
            }
        }
    }
}

/// Settles `domain` from the disk cache alone — no network. A stored source
/// becomes `Ready` (the half-block grid is rebuilt from it), an unexpired miss
/// marker becomes `Failed`. `false` = the network is the only way left.
fn serve_from_disk(dir: &Path, domain: &str, map: &Mutex<HashMap<String, IconState>>) -> bool {
    let state = match load_disk(&icon_path(dir, domain)) {
        Some(icon) => IconState::Ready(icon),
        None if miss_active(&miss_path(dir, domain), now_secs()) => IconState::Failed,
        None => return false,
    };
    if let Ok(mut m) = map.lock() {
        m.insert(domain.to_string(), state);
    }
    true
}

fn icon_path(dir: &Path, domain: &str) -> PathBuf {
    dir.join(format!("{}.rgba", safe_name(domain)))
}

fn miss_path(dir: &Path, domain: &str) -> PathBuf {
    dir.join(format!("{}.miss", safe_name(domain)))
}

/// Writes the retry-after stamp for a failed fetch (best-effort: a marker we
/// can't write only costs one retry next run).
fn mark_miss(path: &Path, now: u64) {
    let _ = std::fs::write(path, (now + MISS_TTL_SECS).to_string());
}

/// Whether a miss marker still suppresses a fetch at `now`. Missing, expired or
/// unreadable markers all mean "go ahead and try".
fn miss_active(path: &Path, now: u64) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .is_some_and(|retry_after| now < retry_after)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn load_disk(path: &Path) -> Option<Icon> {
    let source = std::fs::read(path).ok()?;
    if source.len() != SRC_LEN {
        return None;
    }
    let small = downscale(&source, SRC_DIM, SRC_DIM, SMALL_W as u32, SMALL_H as u32)?;
    Some(Icon { source, small })
}

fn fetch(domain: &str) -> Option<Icon> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(5))
        .build();
    // Prefer the high-res apple-touch-icon (usually 180×180) over favicon.ico
    // (often only 16×16) for a much crisper result; fall back down the list.
    for path in ["/apple-touch-icon.png", "/apple-touch-icon-precomposed.png", "/favicon.ico"] {
        if let Some(icon) = fetch_one(&agent, domain, path) {
            return Some(icon);
        }
    }
    None
}

fn fetch_one(agent: &ureq::Agent, domain: &str, path: &str) -> Option<Icon> {
    let resp = agent.get(&format!("https://{domain}{path}")).call().ok()?;
    let mut buf = Vec::new();
    resp.into_reader()
        .take(MAX_BODY as u64)
        .read_to_end(&mut buf)
        .ok()?;
    process(&buf)
}

/// Decodes image bytes into a crisp 32×32 source + a 4×4 fallback grid. Pure and
/// bounded (input byte cap by the caller, decode dimension cap here).
fn process(bytes: &[u8]) -> Option<Icon> {
    use image::ImageReader;
    use std::io::Cursor;
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DIM);
    limits.max_image_height = Some(MAX_DIM);
    reader.limits(limits);
    let img = reader.decode().ok()?;
    let source = img
        .resize_exact(SRC_DIM, SRC_DIM, image::imageops::FilterType::Lanczos3)
        .to_rgba8()
        .into_raw();
    if source.len() != SRC_LEN {
        return None;
    }
    let small = downscale(&source, SRC_DIM, SRC_DIM, SMALL_W as u32, SMALL_H as u32)?;
    Some(Icon { source, small })
}

/// Downscales an RGBA buffer to `dw×dh` RGBA.
fn downscale(rgba: &[u8], w: u32, h: u32, dw: u32, dh: u32) -> Option<Vec<u8>> {
    let img = image::RgbaImage::from_raw(w, h, rgba.to_vec())?;
    let small = image::imageops::resize(&img, dw, dh, image::imageops::FilterType::Triangle);
    Some(small.into_raw())
}

/// RGBA of the pixel at `(col, row)` of the 4×4 fallback grid; transparent if
/// out of range.
pub fn pixel(grid: &[u8], col: usize, row: usize) -> [u8; 4] {
    if col >= SMALL_W || row >= SMALL_H {
        return [0, 0, 0, 0];
    }
    let i = (row * SMALL_W + col) * 4;
    grid.get(i..i + 4)
        .map(|s| [s[0], s[1], s[2], s[3]])
        .unwrap_or([0, 0, 0, 0])
}

/// Extracts the host from a URL/domain string (lowercased, no scheme/path/port/
/// userinfo). Returns `None` for something that isn't a plausible host.
pub fn domain_of(url: &str) -> Option<String> {
    let s = url.trim();
    let s = s.split_once("://").map(|(_, r)| r).unwrap_or(s);
    let s = s.split(['/', '?', '#']).next().unwrap_or(s);
    let s = s.rsplit('@').next().unwrap_or(s); // drop any userinfo
    let host = s.split(':').next().unwrap_or(s).trim().to_ascii_lowercase(); // drop port
    if host.is_empty() || !host.contains('.') || host.contains(' ') {
        return None;
    }
    Some(host)
}

fn safe_name(domain: &str) -> String {
    domain
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_extraction() {
        assert_eq!(domain_of("https://www.amazon.it/foo/bar?x=1").as_deref(), Some("www.amazon.it"));
        assert_eq!(domain_of("http://Example.COM:8080/").as_deref(), Some("example.com"));
        assert_eq!(domain_of("user:pw@github.com/x").as_deref(), Some("github.com"));
        assert_eq!(domain_of("spotify.com").as_deref(), Some("spotify.com"));
        assert_eq!(domain_of(""), None);
        assert_eq!(domain_of("not a url"), None);
        assert_eq!(domain_of("localhost"), None, "no dot => not a plausible host");
    }

    #[test]
    fn process_produces_source_and_small() {
        use image::{ImageFormat, RgbaImage};
        use std::io::Cursor;
        let img = RgbaImage::from_pixel(64, 64, image::Rgba([12, 200, 90, 255]));
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
            .unwrap();
        let icon = process(&png).expect("decodes and downscales");
        assert_eq!(icon.source.len(), SRC_LEN, "32×32 crisp source");
        assert_eq!(icon.small.len(), SMALL_LEN, "4×4 fallback grid");
        let c = pixel(&icon.small, 1, 1);
        assert!(c[1] > c[0] && c[1] > c[2], "still green-ish");
        assert_eq!(c[3], 255, "opaque");
    }

    #[test]
    fn garbage_bytes_decode_to_none() {
        assert!(process(b"this is not an image").is_none());
    }

    #[test]
    fn miss_marker_suppresses_retries_until_it_expires() {
        let dir = tempfile::tempdir().unwrap();
        let p = miss_path(dir.path(), "example.com");
        assert!(!miss_active(&p, 1_000), "no marker => nothing to suppress");
        mark_miss(&p, 1_000);
        assert!(miss_active(&p, 1_000), "a fresh failure isn't retried");
        assert!(miss_active(&p, 1_000 + MISS_TTL_SECS - 1));
        assert!(!miss_active(&p, 1_000 + MISS_TTL_SECS), "expired => try again");
        std::fs::write(&p, b"junk").unwrap();
        assert!(!miss_active(&p, 1_000), "an unreadable marker never suppresses");
    }

    #[test]
    fn disk_cache_and_miss_marker_settle_domains_without_network() {
        let dir = tempfile::tempdir().unwrap();
        let map: Mutex<HashMap<String, IconState>> = Mutex::new(HashMap::new());
        assert!(!serve_from_disk(dir.path(), "unknown.example", &map), "network is the only way left");
        // a stored source is enough: Ready, with the 4×4 grid rebuilt from it
        std::fs::write(icon_path(dir.path(), "cached.example"), vec![9u8; SRC_LEN]).unwrap();
        assert!(serve_from_disk(dir.path(), "cached.example", &map));
        match map.lock().unwrap().get("cached.example") {
            Some(IconState::Ready(icon)) => {
                assert_eq!(icon.source.len(), SRC_LEN);
                assert_eq!(icon.small.len(), SMALL_LEN, "half-block grid rebuilt from the cache");
            }
            _ => panic!("a cached domain must be Ready without touching the network"),
        }
        // a live marker settles the domain as Failed, still offline
        mark_miss(&miss_path(dir.path(), "dead.example"), now_secs());
        assert!(serve_from_disk(dir.path(), "dead.example", &map));
        assert!(matches!(map.lock().unwrap().get("dead.example"), Some(IconState::Failed)));
    }

    #[test]
    fn crisp_favicon_protocol_renders_across_frames_without_panicking() {
        // the graphics-protocol path (build a StatefulProtocol + render it every
        // frame, as the redraw loop does) must not panic. TestBackend can't show
        // the real kitty output, but this guards the integration plumbing.
        use ratatui::{backend::TestBackend, layout::Rect, Terminal};
        use ratatui_image::{picker::Picker, StatefulImage};
        let picker = Picker::from_fontsize((8, 16));
        let img = image::RgbaImage::from_pixel(SRC_DIM, SRC_DIM, image::Rgba([200, 100, 50, 255]));
        let mut proto = picker.new_resize_protocol(image::DynamicImage::ImageRgba8(img));
        let mut t = Terminal::new(TestBackend::new(12, 4)).unwrap();
        for _ in 0..3 {
            t.draw(|f| {
                f.render_stateful_widget(StatefulImage::new(), Rect::new(2, 1, 4, 2), &mut proto);
            })
            .unwrap();
        }
    }
}
