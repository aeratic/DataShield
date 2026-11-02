//! Image fields: a picture file taken into the vault, and got back out again.
//!
//! The stored value is the base64 of the ORIGINAL file bytes — never a
//! re-encode: a scanned ID must stay exactly as legible as the file the user
//! picked. The shape metadata rides the field's `extra` map under
//! [`META_KEY`] (`{"mime": "image/jpeg", "bytes": 402931}`), which flattens
//! round-trip losslessly, so another DataShield reader keeps it untouched.
//!
//! Everything here is bounded, because both the file and the stored bytes are
//! untrusted input: an import is capped at [`MAX_BYTES`] and must really decode
//! as PNG/JPEG/GIF, and every decode is capped at [`MAX_DIM`] px per side (the
//! favicon path's rule, one size up).

use crate::i18n::{self, Key, Lang};
use crate::model::{FieldType, ItemField};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

/// The largest file an import accepts (the whole vault is rewritten on every
/// save, so a photo album in there would cost a re-encrypt per keystroke).
pub const MAX_BYTES: u64 = 8 * 1024 * 1024;
/// Refuse to decode absurdly large images (a decompression bomb allocates
/// `w * h * 4` bytes long before anything is drawn).
pub const MAX_DIM: u32 = 4096;
/// Where the metadata lives inside the field's `extra` map.
pub const META_KEY: &str = "x_image";

/// The formats an import accepts, as `(mime, display label)` in offer order.
const ACCEPTED: [(&str, &str); 3] = [("image/png", "PNG"), ("image/jpeg", "JPEG"), ("image/gif", "GIF")];

/// Whether a field holds a picture (its declared DCS-1 type — the only place
/// this is decided).
pub fn is_image(field: &ItemField) -> bool {
    field.ftype == FieldType::Image
}

/// A file read, checked and encoded, ready to become a field value.
pub struct Imported {
    /// base64 of the original file bytes.
    pub b64: String,
    pub mime: &'static str,
    pub bytes: u64,
}

impl Imported {
    /// The `x_image` value to store alongside `b64`.
    pub fn meta(&self) -> serde_json::Value {
        serde_json::json!({ "mime": self.mime, "bytes": self.bytes })
    }
}

/// Why an import was refused. Every arm becomes a localized form error.
/// `Debug` is safe here: it carries no path and no file content.
#[derive(Debug, PartialEq, Eq)]
pub enum ImportError {
    /// No such file, a directory, or unreadable.
    Unreadable,
    /// Over [`MAX_BYTES`].
    TooLarge,
    /// Not a PNG/JPEG/GIF, or it doesn't decode.
    Unsupported,
}

impl ImportError {
    /// The message shown on the form row that was refused.
    pub fn message(&self, lang: Lang, path: &str) -> String {
        match self {
            ImportError::Unreadable => i18n::t(lang, &Key::ImageUnreadable(path)),
            ImportError::TooLarge => i18n::t(lang, &Key::ImageTooLarge(&human_bytes(MAX_BYTES))),
            ImportError::Unsupported => {
                let formats = accepted_formats();
                i18n::t(lang, &Key::ImageUnsupported(&formats, &human_bytes(MAX_BYTES)))
            }
        }
    }
}

/// The accepted formats, listed for a message (`PNG, JPEG, GIF`).
pub fn accepted_formats() -> String {
    ACCEPTED.iter().map(|(_, label)| *label).collect::<Vec<_>>().join(", ")
}

/// Reads `path` and encodes it for storage. The size is checked BEFORE the
/// bytes are read, and the picture must really decode — a `.png` that is in
/// fact a zip is refused here, not on the frame that tries to draw it.
pub fn import(path: &Path) -> Result<Imported, ImportError> {
    let meta = std::fs::metadata(path).map_err(|_| ImportError::Unreadable)?;
    if !meta.is_file() {
        return Err(ImportError::Unreadable);
    }
    if meta.len() > MAX_BYTES {
        return Err(ImportError::TooLarge);
    }
    let bytes = std::fs::read(path).map_err(|_| ImportError::Unreadable)?;
    // re-check: the file may have grown between the stat and the read
    if bytes.len() as u64 > MAX_BYTES {
        return Err(ImportError::TooLarge);
    }
    let mime = mime_of(&bytes).ok_or(ImportError::Unsupported)?;
    decode_bytes(&bytes).ok_or(ImportError::Unsupported)?;
    Ok(Imported { b64: B64.encode(&bytes), mime, bytes: bytes.len() as u64 })
}

/// The accepted mime of `bytes` from their magic number, or `None` for anything
/// else (an `.ico`, a PDF, a zip…).
fn mime_of(bytes: &[u8]) -> Option<&'static str> {
    let mime = match image::guess_format(bytes).ok()? {
        image::ImageFormat::Png => "image/png",
        image::ImageFormat::Jpeg => "image/jpeg",
        image::ImageFormat::Gif => "image/gif",
        _ => return None,
    };
    Some(mime)
}

/// Decodes image bytes with a per-side dimension cap.
fn decode_bytes(bytes: &[u8]) -> Option<image::DynamicImage> {
    use image::ImageReader;
    let mut reader = ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DIM);
    limits.max_image_height = Some(MAX_DIM);
    reader.limits(limits);
    reader.decode().ok()
}

/// The picture a stored image field holds. `None` when the field isn't an
/// image, is empty, isn't valid base64, or won't decode — every one of which
/// the caller renders as the plain placeholder.
pub fn decode(field: &ItemField) -> Option<image::DynamicImage> {
    if !is_image(field) || field.value.is_empty() {
        return None;
    }
    let bytes = B64.decode(field.value.as_str()).ok()?;
    decode_bytes(&bytes)
}

/// An RGBA thumbnail of `field`, `w×h` px, the picture scaled to FIT (aspect
/// kept) and centred on transparency — the source of the half-block fallback.
pub fn thumbnail(field: &ItemField, w: u32, h: u32) -> Option<Vec<u8>> {
    let (w, h) = (w.max(1), h.max(1));
    let img = decode(field)?
        .resize(w, h, image::imageops::FilterType::Triangle)
        .to_rgba8();
    let mut canvas = image::RgbaImage::from_pixel(w, h, image::Rgba([0, 0, 0, 0]));
    let (dx, dy) = ((w - img.width()) / 2, (h - img.height()) / 2);
    image::imageops::overlay(&mut canvas, &img, dx as i64, dy as i64);
    Some(canvas.into_raw())
}

/// RGBA of the pixel at `(col, row)` of a `w`-wide RGBA buffer; transparent when
/// out of range.
pub fn pixel(rgba: &[u8], w: u32, col: u32, row: u32) -> [u8; 4] {
    if col >= w {
        return [0, 0, 0, 0];
    }
    let i = ((row * w + col) * 4) as usize;
    rgba.get(i..i + 4)
        .map(|s| [s[0], s[1], s[2], s[3]])
        .unwrap_or([0, 0, 0, 0])
}

/// The stored `(mime, bytes)` of an image field, from its `x_image` metadata.
/// Either half is `None` when written by something that didn't store it.
pub fn meta(field: &ItemField) -> (Option<String>, Option<u64>) {
    let meta = field.extra.get(META_KEY);
    (
        meta.and_then(|m| m.get("mime")).and_then(|m| m.as_str()).map(str::to_string),
        meta.and_then(|m| m.get("bytes")).and_then(|m| m.as_u64()),
    )
}

/// The ledger-row text of an image field — `image · JPEG · 1.2 MiB`, localized.
/// This is ALL a value ever shows: the base64 never reaches the screen, whatever
/// the reveal toggle says.
pub fn placeholder(lang: Lang, field: &ItemField) -> String {
    let (mime, bytes) = meta(field);
    let size = human_bytes(bytes.unwrap_or_else(|| decoded_len(field.value.as_str().len())));
    i18n::t(lang, &Key::ImageField(&format_label(mime.as_deref()), &size))
}

/// A short format name from a mime (`image/jpeg` → `JPEG`).
fn format_label(mime: Option<&str>) -> String {
    match mime {
        Some(m) => m.rsplit('/').next().unwrap_or(m).to_uppercase(),
        None => "IMG".to_string(),
    }
}

/// How many bytes a base64 string of this length carries (close enough for a
/// size shown to a human; padding costs at most two bytes).
fn decoded_len(b64_len: usize) -> u64 {
    (b64_len as u64 / 4) * 3
}

// ---- handing a picture to the system viewer -------------------------------

/// Why an export was refused. Like [`ImportError`], `Debug` is safe: it carries
/// neither the path nor a byte of the picture.
#[derive(Debug, PartialEq, Eq)]
pub enum ExportError {
    /// The stored value isn't valid base64 — there is no picture to hand over.
    Undecodable,
    /// The private directory, or the file in it, could not be written.
    Unwritable,
}

impl ExportError {
    /// The toast shown when the picture could not be handed over.
    pub fn message(&self, lang: Lang) -> String {
        match self {
            ExportError::Undecodable => i18n::t(lang, &Key::ImageUndecodable),
            ExportError::Unwritable => i18n::t(lang, &Key::ImageExportFailed),
        }
    }
}

/// The extension a stored mime is written out under — the only thing that makes
/// the desktop pick a picture viewer at all. An absent or unknown mime lands on
/// `bin`: better a file nothing opens than a `.png` that is not one.
fn extension_for(mime: Option<&str>) -> &'static str {
    match mime {
        Some("image/jpeg") => "jpg",
        Some("image/png") => "png",
        Some("image/gif") => "gif",
        _ => "bin",
    }
}

/// The name a picture is written under: `<item>-<field>-<id>.<ext>`. Label and
/// key are BOTH slugged — a DCS-1 field key already is a slug, but this becomes
/// a path component, and a stored key must never be able to carry a `/` or a
/// `..` out of the export directory.
fn export_name(label: &str, field_key: &str, item_id: &str, mime: Option<&str>) -> String {
    let stem = crate::model::slugify(label);
    let key = crate::model::slugify(field_key);
    // a symbols-only label slugs to nothing; the file still needs a name
    let mut name = if stem.is_empty() { "image".to_string() } else { stem };
    if !key.is_empty() {
        name.push('-');
        name.push_str(&key);
    }
    // two items may share a label AND a field key (two "Passport" scans): a
    // slice of the id keeps one export from landing on top of the other.
    // slugify's output is ASCII, so byte-slicing cannot split a character.
    let id = crate::model::slugify(item_id);
    if !id.is_empty() {
        name.push('-');
        name.push_str(&id[..id.len().min(8)]);
    }
    format!("{name}.{}", extension_for(mime))
}

/// Where a picture handed to the system viewer is written: the session's own
/// runtime directory when there is one (tmpfs, already owner-only, emptied at
/// logout), else a private directory under the system temp dir.
pub fn export_dir() -> PathBuf {
    // an empty XDG_RUNTIME_DIR is treated as unset, like `paths::resolve` does
    match std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir).join("datashield"),
        None => std::env::temp_dir().join("datashield-export"),
    }
}

/// Writes an image field's picture out as a real file the desktop can open, and
/// says where it landed.
///
/// This is the ONE path on which a vault secret reaches the disk in the clear,
/// so it lands where only its owner can read it (0700 dir, 0600 file) and the
/// caller MUST remember the path and take it back again — see
/// `App::purge_exports`, which lock, close and quit all go through.
pub fn export_for_viewing(
    b64: &str,
    mime: Option<&str>,
    label: &str,
    field_key: &str,
    item_id: &str,
) -> Result<PathBuf, ExportError> {
    export_into(&export_dir(), b64, mime, label, field_key, item_id)
}

/// [`export_for_viewing`] with the directory named, so the tests can prove the
/// filename and the permissions without writing into the real session dir.
pub fn export_into(
    dir: &Path,
    b64: &str,
    mime: Option<&str>,
    label: &str,
    field_key: &str,
    item_id: &str,
) -> Result<PathBuf, ExportError> {
    // decoded FIRST: a value that is not base64 must not leave an empty file
    // behind for the user to open and puzzle over. Zeroized on the way out —
    // this is the picture itself, not a description of it.
    let bytes = Zeroizing::new(B64.decode(b64).map_err(|_| ExportError::Undecodable)?);
    create_private_dir(dir).map_err(|_| ExportError::Unwritable)?;
    let path = dir.join(export_name(label, field_key, item_id, mime));
    write_private_file(&path, &bytes).map_err(|_| ExportError::Unwritable)?;
    Ok(path)
}

/// mkdir -p, owner-only (0700) on Unix: everything under here is cleartext.
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)?;
    // `mode` above only shapes a dir made by THIS call: the /tmp fallback dir
    // survives across sessions with whatever mode it had, so 0700 is re-asserted
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Writes `bytes` to a FRESH owner-only (0600) file at `path`.
///
/// The name is predictable and the fallback directory can live in a shared
/// `/tmp`, so the path is unlinked first (which removes a symlink rather than
/// following it) and then created exclusively: if anything reappears in the
/// gap, the write fails instead of landing in someone else's file.
fn write_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let _ = std::fs::remove_file(path);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    std::io::Write::write_all(&mut file, bytes)
}

/// A byte count as a human reads it: `842 B`, `31 KiB`, `1.2 MiB`.
pub fn human_bytes(n: u64) -> String {
    const KIB: f64 = 1024.0;
    let f = n as f64;
    if f < KIB {
        format!("{n} B")
    } else if f < KIB * KIB {
        format!("{:.0} KiB", f / KIB)
    } else {
        format!("{:.1} MiB", f / (KIB * KIB))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SecretString;

    /// A real `w×h` PNG of one colour, as file bytes.
    fn png_bytes(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([10, 120, 220, 255]));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .expect("encodes");
        out
    }

    fn field_of(value: &str, extra: serde_json::Map<String, serde_json::Value>) -> ItemField {
        ItemField {
            key: "photo_front".into(),
            ftype: FieldType::Image,
            label: None,
            value: SecretString::new(value),
            secret: true,
            extra,
        }
    }

    #[test]
    fn importing_a_png_stores_the_original_bytes_base64() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("id.png");
        let bytes = png_bytes(24, 16);
        std::fs::write(&path, &bytes).unwrap();
        let imported = import(&path).expect("a real png imports");
        assert_eq!(imported.mime, "image/png");
        assert_eq!(imported.bytes, bytes.len() as u64);
        assert_eq!(B64.decode(&imported.b64).unwrap(), bytes, "the ORIGINAL file, byte for byte");
        assert_eq!(imported.meta()["mime"], "image/png");
        assert_eq!(imported.meta()["bytes"], bytes.len() as u64);
    }

    #[test]
    fn oversize_and_unsupported_files_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        // over the cap: refused before a single byte is read into a value
        let big = dir.path().join("huge.png");
        std::fs::write(&big, vec![0u8; MAX_BYTES as usize + 1]).unwrap();
        assert!(matches!(import(&big), Err(ImportError::TooLarge)));
        // not an image at all
        let text = dir.path().join("notes.txt");
        std::fs::write(&text, b"this is not an image").unwrap();
        assert!(matches!(import(&text), Err(ImportError::Unsupported)));
        // a supported extension over the wrong bytes is still refused
        let liar = dir.path().join("id.png");
        std::fs::write(&liar, b"PK\x03\x04 not a png").unwrap();
        assert!(matches!(import(&liar), Err(ImportError::Unsupported)));
        // an accepted-by-the-crate format we don't take (ico)
        let mut ico = Vec::new();
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(8, 8, image::Rgba([1, 2, 3, 255])))
            .write_to(&mut std::io::Cursor::new(&mut ico), image::ImageFormat::Ico)
            .unwrap();
        let ico_path = dir.path().join("id.ico");
        std::fs::write(&ico_path, &ico).unwrap();
        assert!(matches!(import(&ico_path), Err(ImportError::Unsupported)));
        // and nothing at all
        assert!(matches!(import(&dir.path().join("nope.png")), Err(ImportError::Unreadable)));
        assert!(matches!(import(dir.path()), Err(ImportError::Unreadable)), "a directory is not a file");
    }

    #[test]
    fn a_stored_field_decodes_back_to_the_same_picture() {
        let bytes = png_bytes(40, 5);
        let field = field_of(&B64.encode(&bytes), serde_json::Map::new());
        let img = decode(&field).expect("decodes");
        assert_eq!((img.width(), img.height()), (40, 5));
        let thumb = thumbnail(&field, 8, 4).expect("thumbnails");
        assert_eq!(thumb.len(), 8 * 4 * 4, "RGBA of exactly the asked grid");
        // the aspect is kept, so an 8:1 picture centred in a 2:1 box lands on
        // one row with transparency above it
        assert_eq!(pixel(&thumb, 8, 0, 0)[3], 0, "padded, not stretched");
        assert!(pixel(&thumb, 8, 4, 1)[2] > 100, "…and the picture is in the middle");
        assert_eq!(pixel(&thumb, 8, 99, 0), [0, 0, 0, 0], "out of range reads transparent");
    }

    #[test]
    fn garbage_and_non_image_fields_decode_to_nothing() {
        assert!(decode(&field_of("not base64 !!", serde_json::Map::new())).is_none());
        assert!(decode(&field_of("", serde_json::Map::new())).is_none());
        assert!(decode(&field_of(&B64.encode(b"still not an image"), serde_json::Map::new())).is_none());
        let mut text = field_of(&B64.encode(png_bytes(4, 4)), serde_json::Map::new());
        text.ftype = FieldType::Text;
        assert!(decode(&text).is_none(), "only an image-typed field is ever decoded");
    }

    #[test]
    fn the_placeholder_reads_the_metadata_and_never_the_value() {
        let mut extra = serde_json::Map::new();
        extra.insert(META_KEY.into(), serde_json::json!({ "mime": "image/jpeg", "bytes": 1_258_291 }));
        let field = field_of("QUJDRA==", extra);
        let text = placeholder(Lang::It, &field);
        assert_eq!(text, "immagine · JPEG · 1.2 MiB");
        assert_eq!(placeholder(Lang::En, &field), "image · JPEG · 1.2 MiB");
        assert!(!text.contains("QUJDRA"), "the payload never reaches the screen");
        // no metadata (another writer): the size falls back to the base64 length
        let bare = field_of(&B64.encode(vec![7u8; 4096]), serde_json::Map::new());
        assert_eq!(placeholder(Lang::En, &bare), "image · IMG · 4 KiB");
    }

    #[test]
    fn an_export_is_named_after_the_item_and_typed_by_its_mime() {
        let dir = tempfile::tempdir().unwrap();
        let b64 = B64.encode(png_bytes(4, 4));
        let id = "9f8e7d6c-0000-4000-8000-000000000000"; // first 8 slug chars ride the name
        let name = |mime: Option<&str>, label: &str, key: &str| {
            export_into(dir.path(), &b64, mime, label, key, id)
                .expect("writes")
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_string()
        };
        // the mime decides the extension — it is what makes the desktop reach
        // for a picture viewer at all
        assert_eq!(name(Some("image/jpeg"), "Passport", "photo_front"), "passport-photo_front-9f8e7d6c.jpg");
        assert_eq!(name(Some("image/png"), "Passport", "photo_front"), "passport-photo_front-9f8e7d6c.png");
        assert_eq!(name(Some("image/gif"), "Passport", "photo_front"), "passport-photo_front-9f8e7d6c.gif");
        // a mime nobody wrote, or one we don't know: never a lying extension
        assert_eq!(name(None, "Passport", "photo_front"), "passport-photo_front-9f8e7d6c.bin");
        assert_eq!(name(Some("application/pdf"), "Passport", "photo_front"), "passport-photo_front-9f8e7d6c.bin");
        // the label is SLUGGED, so a real-world label stays one path component
        assert_eq!(name(Some("image/png"), "Carta d'Identità!", "photo_back"), "carta_d_identit-photo_back-9f8e7d6c.png");
        // …and so is the key: it becomes a path, and a stored value must never
        // be able to walk out of the export directory
        assert_eq!(name(Some("image/png"), "ID", "../../etc/passwd"), "id-etc_passwd-9f8e7d6c.png");
        assert_eq!(name(Some("image/png"), "***", "***"), "image-9f8e7d6c.png", "a nameless field still gets a file");
    }

    #[test]
    fn two_items_sharing_a_label_do_not_share_an_export() {
        // two DISTINCT "Passport" items, each with a photo_front: without the
        // id in the name, opening the second silently replaced the first's
        // decrypted scan on disk — possibly under a viewer still showing it
        let dir = tempfile::tempdir().unwrap();
        let (first, second) = (png_bytes(4, 4), png_bytes(8, 2));
        let a = export_into(dir.path(), &B64.encode(&first), Some("image/png"), "Passport", "photo_front", "aaaa1111-0000-4000-8000-000000000000")
            .expect("writes");
        let b = export_into(dir.path(), &B64.encode(&second), Some("image/png"), "Passport", "photo_front", "bbbb2222-0000-4000-8000-000000000000")
            .expect("writes");
        assert_ne!(a, b, "same label + same field, still two files");
        assert_eq!(std::fs::read(&a).unwrap(), first, "the first item's bytes survived the second export");
        assert_eq!(std::fs::read(&b).unwrap(), second);
    }

    #[test]
    fn an_export_is_the_original_bytes_in_an_owner_only_file() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("datashield");
        let bytes = png_bytes(9, 7);
        let id = "11112222-0000-4000-8000-000000000000";
        let path = export_into(&nested, &B64.encode(&bytes), Some("image/png"), "Passport", "photo_front", id)
            .expect("writes");
        assert_eq!(path, nested.join("passport-photo_front-11112222.png"));
        assert_eq!(std::fs::read(&path).unwrap(), bytes, "the ORIGINAL file, byte for byte");
        // …and re-exporting the same row overwrites it rather than piling up
        let again = export_into(&nested, &B64.encode(&bytes), Some("image/png"), "Passport", "photo_front", id)
            .expect("writes again");
        assert_eq!(again, path);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // this is a decrypted secret sitting on the disk: nobody else on the
            // machine may read it, and nobody else may list the directory either
            let file = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(file & 0o777, 0o600, "export file mode is {file:o}");
            let made = std::fs::metadata(&nested).unwrap().permissions().mode();
            assert_eq!(made & 0o777, 0o700, "export dir mode is {made:o}");
        }
    }

    #[test]
    fn a_value_that_is_not_base64_is_refused_before_a_file_is_made() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("datashield");
        assert_eq!(
            export_into(&nested, "not base64 at all !!", Some("image/png"), "ID", "photo", "cccc3333-0000-4000-8000-000000000000"),
            Err(ExportError::Undecodable)
        );
        assert!(!nested.exists(), "nothing was created for a value with no picture in it");
        // every refusal reads as a sentence, in both languages
        assert_eq!(ExportError::Undecodable.message(Lang::It), "immagine illeggibile — non c'è nulla da aprire");
        assert_eq!(ExportError::Unwritable.message(Lang::En), "the temporary copy could not be written");
    }

    #[test]
    fn the_export_dir_is_the_session_runtime_dir_when_there_is_one() {
        // read, never written: the test only asserts the SHAPE, so it can't race
        // another test over a process-wide environment variable.
        let dir = export_dir();
        match std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
            Some(runtime) => assert_eq!(dir, std::path::PathBuf::from(runtime).join("datashield")),
            None => assert_eq!(dir, std::env::temp_dir().join("datashield-export")),
        }
    }

    #[test]
    fn byte_sizes_read_like_a_human_wrote_them() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(842), "842 B");
        assert_eq!(human_bytes(1024), "1 KiB");
        assert_eq!(human_bytes(MAX_BYTES), "8.0 MiB");
    }
}
