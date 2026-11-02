//! The Memorandum: a chat with yourself, kept INSIDE the vault.
//!
//! The whole conversation lives under the app-private `x_memo` member of the
//! content's flatten map (the `x_sync` / `x_image` precedent), so it is
//! encrypted with everything else, travels with an export and a sync, and a
//! reader that knows nothing about it round-trips it untouched — DCS-1 stays at
//! version 1 (see `docs/STORAGE-STANDARD.md`).
//!
//! ```json
//! "x_memo": { "messages": [
//!   { "id": "…uuidv4…", "ts": "2026-08-19T18:00:00Z", "text": "the safe code is on the fridge",
//!     "vec": [0.031, -0.114, …], "vfp": "…blake3 hex…" },
//!   { "id": "…", "ts": "…", "text": "",
//!     "image": { "b64": "iVBORw0KGgo…", "mime": "image/png", "bytes": 40213,
//!                "caption": "the receipt" } }
//! ] }
//! ```
//!
//! Every WRITE here edits the stored json in place instead of re-serializing a
//! parsed `Vec<MemoMessage>`: a message this build cannot read (a newer
//! writer's, a corrupted one) must survive somebody else's append, exactly like
//! the unknown members inside a message do. Reading is the tolerant half —
//! [`messages`] skips what it can't parse and never panics.

use crate::model::{new_uuid_v4, now_rfc3339, FieldType, ItemField, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Where the conversation lives inside `VaultContent::extra`.
pub const MEMO_KEY: &str = "x_memo";
/// The array member inside it.
const MESSAGES: &str = "messages";

/// One entry of the Memorandum: text, a photo, or both.
#[derive(Serialize, Deserialize)]
pub struct MemoMessage {
    /// UUIDv4, lowercase — the stable identity an embedding is attached BY.
    pub id: String,
    /// RFC 3339 UTC. The array is kept in append order; this is what that order
    /// means, and the only thing a reader should sort on.
    pub ts: String,
    /// What the user typed. Empty is legal when the message carries a photo;
    /// nothing here refuses it (the composer is what decides "nothing to send").
    #[serde(default)]
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<MemoImage>,
    /// The semantic-search embedding, attached later and in place by
    /// [`set_vec`] — absent until then, and never serialized as `null`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vec: Option<Vec<f32>>,
    /// Fingerprint of the exact text `vec` was computed from, written by the
    /// same call. This is the ONLY staleness signal: an edited message (or one
    /// embedded by a different model) has a `vfp` that no longer matches, so it
    /// is re-embedded; everything else is left alone and costs nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vfp: Option<String>,
    /// Unknown members written by another/newer DataShield — round-tripped.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl MemoMessage {
    /// A message stamped now (fresh id, current time), ready to be pushed.
    pub fn new(text: impl Into<String>, image: Option<MemoImage>) -> MemoMessage {
        MemoMessage {
            id: new_uuid_v4(),
            ts: now_rfc3339(),
            text: text.into(),
            image,
            vec: None,
            vfp: None,
            extra: Map::new(),
        }
    }
}

/// The text a copy of one message puts on the clipboard: its words VERBATIM,
/// every line of them (a memo message is a paragraph — the composer's Shift/Alt+⏎
/// wrote the newlines, and they all come out), so a whole multi-line note lifts
/// in one keystroke where a mouse selection would stop at the first line.
///
/// A photo-only message copies its caption instead, or — captionless — a
/// localized note NAMING the picture; the base64 is never text and never
/// leaves this way. Empty only for the impossible message that is neither words
/// nor picture, which the caller reads as "nothing to copy".
///
/// Pure like [`crate::clipboard::copy_payload`]: the "which bytes leave the
/// vault" decision is unit-tested with no clipboard anywhere near it.
pub fn memo_copy_payload(msg: &MemoMessage, lang: crate::i18n::Lang) -> String {
    if !msg.text.trim().is_empty() {
        return msg.text.clone();
    }
    if let Some(img) = msg.image.as_ref() {
        return match img.caption.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
            Some(caption) => caption.to_string(),
            None => crate::i18n::t(lang, &crate::i18n::Key::MemoCopyImage),
        };
    }
    String::new()
}

/// A photo inside a message. Same storage contract as an image FIELD: the
/// base64 of the ORIGINAL file bytes (never a re-encode) plus the shape the
/// `x_image` metadata carries, so [`crate::image_field`] validates, decodes and
/// exports a memo photo with no second implementation of any of it.
#[derive(Serialize, Deserialize)]
pub struct MemoImage {
    /// base64 (standard alphabet, padded) of the file exactly as it was picked.
    pub b64: String,
    /// `image/png`, `image/jpeg` or `image/gif` — what the import accepted.
    pub mime: String,
    /// The length of the ORIGINAL file, not of the base64.
    pub bytes: u64,
    /// The words that came with the photo, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl MemoImage {
    /// The picture a validated import produced, with its caption.
    pub fn from_import(img: &crate::image_field::Imported, caption: Option<String>) -> MemoImage {
        MemoImage {
            b64: img.b64.clone(),
            mime: img.mime.to_string(),
            bytes: img.bytes,
            caption,
            extra: Map::new(),
        }
    }

    /// The photo seen as an [`ItemField`], so every reader of an image field
    /// (decode, thumbnail, placeholder, export) works on a memo photo unchanged.
    /// `secret` is true like any stored picture: the base64 is never text.
    ///
    /// IT COPIES THE WHOLE PICTURE, and the copy is wiped byte by byte when the
    /// field drops (`SecretString`), so it belongs to the rows that really draw
    /// one — [`MemoImage::as_meta_field`] is what a row that only NAMES the
    /// photo asks for.
    pub fn as_field(&self) -> ItemField {
        self.field(SecretString::new(self.b64.clone()))
    }

    /// The same field WITHOUT the picture in it. Mime and byte count are all
    /// [`crate::image_field::placeholder`] reads, and the thread's head row
    /// (drawn for every message, on every frame) has no business copying — and
    /// then zeroizing — a megabyte of base64 to say "image · JPEG · 1.2 MiB".
    pub fn as_meta_field(&self) -> ItemField {
        self.field(SecretString::new(String::new()))
    }

    fn field(&self, value: SecretString) -> ItemField {
        let mut extra = Map::new();
        extra.insert(
            crate::image_field::META_KEY.to_string(),
            serde_json::json!({ "mime": self.mime, "bytes": self.bytes }),
        );
        ItemField {
            key: "photo".to_string(),
            ftype: FieldType::Image,
            label: None,
            value,
            secret: true,
            extra,
        }
    }
}

// ---- the stored json (all reads and writes go through these) --------------

/// The messages array as stored, if there is one.
fn array(extra: &Map<String, Value>) -> Option<&Vec<Value>> {
    extra.get(MEMO_KEY)?.get(MESSAGES)?.as_array()
}

/// Every readable message, in stored order. A message this build cannot parse
/// (a member of the wrong type, a missing `id`) is SKIPPED — never a panic, and
/// never a reason to drop the others; it also stays on disk, because no write
/// goes through this list.
pub fn messages(extra: &Map<String, Value>) -> Vec<MemoMessage> {
    let Some(arr) = array(extra) else { return Vec::new() };
    arr.iter().filter_map(|v| MemoMessage::deserialize(v).ok()).collect()
}

/// How many messages are stored, readable or not — without deep-copying a
/// single photo (which is what [`messages`] costs).
pub fn len(extra: &Map<String, Value>) -> usize {
    array(extra).map(Vec::len).unwrap_or(0)
}

/// One message as a SEARCH reads it: BORROWED from the stored json, with the
/// photo left exactly where it is.
///
/// [`messages`] deserializes every member of every message into fresh `String`s,
/// each picture's base64 included — a megabyte a photo, copied out and then
/// freed un-wiped, for a caller that only ranks words. This is
/// [`MemoImage::as_meta_field`]'s rule one level up: the reader that never draws
/// a picture does not copy one.
///
/// What it keeps and what it skips match [`MemoMessage`] exactly — `b64` and
/// `mime` are borrowed rather than dropped from the shape precisely so a message
/// missing one is unreadable to BOTH readings. The two must never disagree about
/// which messages exist.
#[derive(Deserialize)]
pub struct MemoRow<'a> {
    pub id: &'a str,
    pub ts: &'a str,
    #[serde(default)]
    pub text: &'a str,
    #[serde(default)]
    image: Option<RowImage<'a>>,
    #[serde(default)]
    pub vec: Option<Vec<f32>>,
}

/// The photo of a [`MemoRow`] — its caption, and the presence of everything
/// [`MemoImage`] requires.
#[derive(Deserialize)]
struct RowImage<'a> {
    /// Borrowed and never read: only its PRESENCE matters here (see [`MemoRow`]).
    #[serde(rename = "b64")]
    _b64: &'a str,
    #[serde(rename = "mime")]
    _mime: &'a str,
    #[serde(rename = "bytes")]
    _bytes: u64,
    #[serde(default)]
    caption: Option<&'a str>,
}

impl MemoRow<'_> {
    /// The words that came with the photo, if there is one and they are there.
    pub fn caption(&self) -> Option<&str> {
        self.image.as_ref()?.caption
    }
}

/// Every readable message as a search row, in stored order — the borrowing half
/// of [`messages`], skipping exactly what it skips.
pub fn rows(extra: &Map<String, Value>) -> Vec<MemoRow<'_>> {
    let Some(arr) = array(extra) else { return Vec::new() };
    arr.iter().filter_map(|v| MemoRow::deserialize(v).ok()).collect()
}

/// Appends one message to the stored json. Sibling members of `x_memo` and
/// unreadable messages are left exactly as they were.
///
/// A container of the wrong json type is REPAIRED (replaced), not preserved:
/// no DataShield writes anything but an object there, and the message the user
/// just wrote has to land somewhere. The caller then bumps the sync counter and
/// saves; [`pop`] is the rollback for a save that fails.
pub fn push(extra: &mut Map<String, Value>, msg: MemoMessage) {
    let Ok(value) = serde_json::to_value(&msg) else { return };
    let container = extra.entry(MEMO_KEY).or_insert_with(|| Value::Object(Map::new()));
    if !container.is_object() {
        *container = Value::Object(Map::new());
    }
    let obj = container.as_object_mut().expect("just made an object");
    let list = obj.entry(MESSAGES).or_insert_with(|| Value::Array(Vec::new()));
    if !list.is_array() {
        *list = Value::Array(Vec::new());
    }
    list.as_array_mut().expect("just made an array").push(value);
}

/// Takes the LAST message off — the exact inverse of [`push`], so a refused
/// save is rolled back without copying the whole conversation first.
///
/// An emptied Memorandum leaves NOTHING behind (the container goes too, unless
/// a foreign member still lives in it), so the content fingerprint of a
/// rolled-back append is the one it had before. Returns whether a message went.
pub fn pop(extra: &mut Map<String, Value>) -> bool {
    let Some(container) = extra.get_mut(MEMO_KEY).and_then(Value::as_object_mut) else {
        return false;
    };
    let removed = container
        .get_mut(MESSAGES)
        .and_then(Value::as_array_mut)
        .is_some_and(|a| a.pop().is_some());
    let emptied = container
        .get(MESSAGES)
        .and_then(Value::as_array)
        .is_some_and(|a| a.is_empty());
    if emptied && container.len() == 1 {
        extra.remove(MEMO_KEY);
    }
    removed
}

/// What one message's embedding slot held before [`set_vec`] overwrote it.
/// The index writes a whole batch and then saves ONCE; a refused save has to
/// put the content back to the bytes the fingerprint was taken over, so each
/// overwrite hands back its predecessor for [`restore_vec`]. `None` members
/// mean "the member was absent", which is what restoring removes it back to.
pub struct VecSlot {
    id: String,
    vec: Option<Value>,
    vfp: Option<Value>,
}

/// Attaches (or replaces) the embedding of the message with `id` and the
/// fingerprint of the text it came from, IN PLACE on the stored json — every
/// other member of that message, known or not, is left byte-identical. `None`
/// when there is no such message (nothing changed, nothing to roll back).
///
/// A non-finite float is refused: json has no spelling for one (serde writes
/// `null`), and a `vec` full of nulls would make the whole message unreadable.
pub fn set_vec(
    extra: &mut Map<String, Value>,
    id: &str,
    embedding: &[f32],
    fingerprint: &str,
) -> Option<VecSlot> {
    if embedding.iter().any(|f| !f.is_finite()) {
        return None;
    }
    let list = extra
        .get_mut(MEMO_KEY)
        .and_then(|m| m.get_mut(MESSAGES))
        .and_then(Value::as_array_mut)?;
    for message in list.iter_mut() {
        let Some(obj) = message.as_object_mut() else { continue };
        if obj.get("id").and_then(Value::as_str) != Some(id) {
            continue;
        }
        let value = serde_json::to_value(embedding).ok()?;
        let previous = VecSlot {
            id: id.to_string(),
            vec: obj.insert("vec".to_string(), value),
            vfp: obj.insert("vfp".to_string(), Value::String(fingerprint.to_string())),
        };
        return Some(previous);
    }
    None
}

/// Puts a slot back exactly as [`set_vec`] found it — the rollback for a batch
/// whose save failed. False when the message went away meanwhile.
pub fn restore_vec(extra: &mut Map<String, Value>, slot: &VecSlot) -> bool {
    let Some(list) = extra
        .get_mut(MEMO_KEY)
        .and_then(|m| m.get_mut(MESSAGES))
        .and_then(Value::as_array_mut)
    else {
        return false;
    };
    for message in list.iter_mut() {
        let Some(obj) = message.as_object_mut() else { continue };
        if obj.get("id").and_then(Value::as_str) != Some(slot.id.as_str()) {
            continue;
        }
        for (member, was) in [("vec", &slot.vec), ("vfp", &slot.vfp)] {
            match was {
                Some(value) => obj.insert(member.to_string(), value.clone()),
                None => obj.remove(member),
            };
        }
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The content-level `extra` map a vault would carry, from a json literal.
    fn extra_of(src: &str) -> Map<String, Value> {
        serde_json::from_str(src).expect("test fixture parses")
    }

    fn photo(caption: Option<&str>) -> MemoImage {
        MemoImage {
            b64: "iVBORw0KGgo=".to_string(),
            mime: "image/png".to_string(),
            bytes: 40213,
            caption: caption.map(str::to_string),
            extra: Map::new(),
        }
    }

    #[test]
    fn a_text_message_round_trips_through_the_stored_json() {
        let mut extra = Map::new();
        push(&mut extra, MemoMessage::new("the safe code is on the fridge", None));
        // the shape another DataShield reader sees
        let stored = &extra[MEMO_KEY][MESSAGES][0];
        assert!(stored["id"].as_str().is_some_and(|s| s.len() == 36), "a uuid");
        assert!(stored["ts"].as_str().is_some_and(|s| s.ends_with('Z')), "rfc 3339 utc");
        assert_eq!(stored["text"], "the safe code is on the fridge");
        assert!(stored.get("image").is_none(), "no photo, no member");
        assert!(stored.get("vec").is_none(), "no embedding until one is attached");

        let read = messages(&extra);
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].text, "the safe code is on the fridge");
        assert!(read[0].image.is_none() && read[0].vec.is_none());
        assert_eq!(len(&extra), 1);
    }

    #[test]
    fn a_photo_message_keeps_its_caption_and_its_byte_count() {
        let mut extra = Map::new();
        push(&mut extra, MemoMessage::new("", Some(photo(Some("the receipt")))));
        let read = messages(&extra);
        let img = read[0].image.as_ref().expect("the photo survived");
        assert_eq!(read[0].text, "", "empty text is legal with a photo");
        assert_eq!(img.mime, "image/png");
        assert_eq!(img.bytes, 40213, "the ORIGINAL file length, not the base64's");
        assert_eq!(img.caption.as_deref(), Some("the receipt"));
        // a photo reads as an image field, so the picture machinery takes it
        let field = img.as_field();
        assert_eq!(field.ftype, FieldType::Image);
        assert!(field.secret, "the base64 is never text");
        assert_eq!(field.extra[crate::image_field::META_KEY]["bytes"], 40213);
        assert_eq!(crate::image_field::meta(&field).0.as_deref(), Some("image/png"));
        // …and the row that only NAMES the photo gets the same field WITHOUT
        // the picture in it: building one copies the whole base64 and wipes it
        // again on drop, which the thread must not pay per off-screen photo on
        // every frame.
        let meta = img.as_meta_field();
        assert!(meta.value.is_empty(), "the metadata field carries no picture");
        assert_eq!(
            crate::image_field::placeholder(crate::i18n::Lang::En, &meta),
            crate::image_field::placeholder(crate::i18n::Lang::En, &field),
            "…and still says exactly what the picture is"
        );
        assert_eq!(meta.ftype, FieldType::Image);
        assert!(meta.secret);

        // a caption-less photo doesn't carry an empty member
        let mut bare = Map::new();
        push(&mut bare, MemoMessage::new("", Some(photo(None))));
        assert!(bare[MEMO_KEY][MESSAGES][0]["image"].get("caption").is_none());
    }

    #[test]
    fn a_copy_takes_the_whole_message_never_the_base64() {
        use crate::i18n::Lang;
        // a multi-line note copies every line, joined by the newlines it was
        // written with — not just the first, which is the whole point of the key
        let multi = MemoMessage::new("line one\nline two\nline three", None);
        assert_eq!(memo_copy_payload(&multi, Lang::En), "line one\nline two\nline three");
        // a photo with a caption copies the caption; without one, a localized
        // note that NAMES the picture — and in neither case any base64
        let captioned = MemoMessage::new("", Some(photo(Some("the receipt"))));
        assert_eq!(memo_copy_payload(&captioned, Lang::En), "the receipt");
        let bare = MemoMessage::new("", Some(photo(None)));
        assert_eq!(memo_copy_payload(&bare, Lang::En), "image");
        assert_eq!(memo_copy_payload(&bare, Lang::It), "immagine");
        assert!(
            !memo_copy_payload(&bare, Lang::En).contains("iVBOR"),
            "the base64 is never copied"
        );
        // a photo that also carries words copies the words
        let both = MemoMessage::new("look at this", Some(photo(Some("cap"))));
        assert_eq!(memo_copy_payload(&both, Lang::En), "look at this");
    }

    #[test]
    fn the_stored_json_has_a_stable_key_order() {
        // the fingerprint hashes these bytes: two writes of the same message
        // must be the same bytes, and the order comes from the serde struct.
        let mut extra = Map::new();
        let msg = MemoMessage {
            id: "id-1".into(),
            ts: "2026-08-19T18:00:00Z".into(),
            text: "hi".into(),
            image: None,
            vec: Some(vec![0.5, -0.25]),
            vfp: None,
            extra: Map::new(),
        };
        push(&mut extra, msg);
        assert_eq!(
            serde_json::to_string(&extra).unwrap(),
            r#"{"x_memo":{"messages":[{"id":"id-1","text":"hi","ts":"2026-08-19T18:00:00Z","vec":[0.5,-0.25]}]}}"#
        );
        assert_eq!(messages(&extra)[0].vec.as_deref(), Some(&[0.5f32, -0.25][..]));
    }

    #[test]
    fn unknown_members_survive_somebody_elses_append() {
        // a newer writer's message members, and its own member next to the
        // array: an append by THIS build may touch neither.
        let mut extra = extra_of(
            r#"{"x_memo":{"messages":[
                {"id":"old","ts":"2026-01-01T00:00:00Z","text":"first","x_pinned":true}
            ],"x_draft":"half a thought"}}"#,
        );
        push(&mut extra, MemoMessage::new("second", None));
        assert_eq!(extra[MEMO_KEY]["x_draft"], "half a thought", "the sibling member is untouched");
        assert_eq!(extra[MEMO_KEY][MESSAGES][0]["x_pinned"], true, "the unknown member is untouched");
        let read = messages(&extra);
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].extra["x_pinned"], true, "…and it is readable as `extra`");
        // and it goes back out the way it came in
        let out = serde_json::to_string(&extra).unwrap();
        assert!(out.contains(r#""x_pinned":true"#) && out.contains(r#""x_draft":"half a thought""#));
    }

    #[test]
    fn a_malformed_message_is_skipped_and_never_lost() {
        let mut extra = extra_of(
            r#"{"x_memo":{"messages":[
                {"id":"ok-1","ts":"2026-01-01T00:00:00Z","text":"readable"},
                42,
                {"id":7,"ts":"2026-01-01T00:00:00Z"},
                {"id":"ok-2","ts":"2026-01-02T00:00:00Z","text":"also readable"}
            ]}}"#,
        );
        let read = messages(&extra);
        assert_eq!(read.len(), 2, "the two readable ones, no panic");
        assert_eq!(read[0].id, "ok-1");
        assert_eq!(read[1].id, "ok-2");
        assert_eq!(len(&extra), 4, "the count is of what is STORED");
        // an append must not rewrite the array from what could be parsed
        push(&mut extra, MemoMessage::new("third", None));
        let stored = extra[MEMO_KEY][MESSAGES].as_array().unwrap();
        assert_eq!(stored.len(), 5);
        assert_eq!(stored[1], 42, "the unparseable entry is still on disk");
        assert_eq!(stored[2]["id"], 7);
    }

    #[test]
    fn a_search_row_reads_the_same_conversation_without_the_photos() {
        // the API's per-request read. It must agree with `messages` about which
        // messages exist and what they say — and must never copy a picture out.
        let mut extra = extra_of(
            r#"{"x_memo":{"messages":[
                {"id":"ok-1","ts":"2026-01-01T00:00:00Z","text":"readable"},
                42,
                {"id":7,"ts":"2026-01-01T00:00:00Z"},
                {"id":"no-photo-shape","ts":"2026-01-01T00:00:00Z","image":{"mime":"image/png"}}
            ]}}"#,
        );
        push(&mut extra, MemoMessage::new("con foto", Some(photo(Some("lo scontrino")))));
        let full = messages(&extra);
        let light = rows(&extra);
        assert_eq!(light.len(), full.len(), "the two readings disagree about what exists");
        for (row, msg) in light.iter().zip(full.iter()) {
            assert_eq!(row.id, msg.id);
            assert_eq!(row.ts, msg.ts);
            assert_eq!(row.text, msg.text);
            assert_eq!(
                row.caption(),
                msg.image.as_ref().and_then(|i| i.caption.as_deref()),
                "the caption is the one part of a photo a search reads"
            );
            assert_eq!(row.vec, msg.vec);
            // …and the two of them index a message identically
            assert_eq!(
                crate::rag::memo_text(row.text, row.caption().unwrap_or("")),
                crate::rag::memo_embed_text(msg)
            );
        }
        assert_eq!(light.last().expect("the photo message").caption(), Some("lo scontrino"));
    }

    #[test]
    fn a_memo_that_is_not_there_reads_as_empty() {
        // nothing stored, a container of the wrong type, an array of the wrong
        // type: all of them read as "no messages" rather than panicking…
        for src in [r#"{}"#, r#"{"x_memo":"nonsense"}"#, r#"{"x_memo":{"messages":"nonsense"}}"#] {
            let mut extra = extra_of(src);
            assert!(messages(&extra).is_empty(), "{src}");
            assert_eq!(len(&extra), 0, "{src}");
            assert!(!pop(&mut extra), "{src}");
            assert!(set_vec(&mut extra, "id", &[1.0], "fp").is_none(), "{src}");
            // …and a write repairs the container instead of refusing forever
            push(&mut extra, MemoMessage::new("first", None));
            assert_eq!(messages(&extra).len(), 1, "{src}");
        }
    }

    #[test]
    fn pop_undoes_a_push_exactly() {
        // this is the save-failed rollback: the content must go back to the
        // bytes the fingerprint was taken over, INCLUDING no container at all.
        let mut extra = Map::new();
        let before = serde_json::to_string(&extra).unwrap();
        push(&mut extra, MemoMessage::new("oops", None));
        assert!(pop(&mut extra));
        assert_eq!(serde_json::to_string(&extra).unwrap(), before, "the memo left no trace");

        // with a conversation already there, only the last one goes
        let mut extra = extra_of(r#"{"x_memo":{"messages":[{"id":"old","ts":"t","text":"keep"}]}}"#);
        let before = serde_json::to_string(&extra).unwrap();
        push(&mut extra, MemoMessage::new("oops", None));
        assert!(pop(&mut extra));
        assert_eq!(serde_json::to_string(&extra).unwrap(), before);
        // an emptied memo that still carries a foreign member keeps its container
        let mut extra = extra_of(r#"{"x_memo":{"messages":[],"x_draft":"kept"}}"#);
        push(&mut extra, MemoMessage::new("oops", None));
        assert!(pop(&mut extra));
        assert_eq!(extra[MEMO_KEY]["x_draft"], "kept");
    }

    #[test]
    fn an_embedding_is_attached_in_place_by_id() {
        let mut extra = extra_of(
            r#"{"x_memo":{"messages":[
                {"id":"a","ts":"t","text":"one","x_pinned":true},
                {"id":"b","ts":"t","text":"two"}
            ]}}"#,
        );
        assert!(set_vec(&mut extra, "a", &[0.5, -0.25], "fp-1").is_some());
        assert_eq!(messages(&extra)[0].vec.as_deref(), Some(&[0.5f32, -0.25][..]));
        assert_eq!(messages(&extra)[0].vfp.as_deref(), Some("fp-1"), "the text it came from");
        assert_eq!(extra[MEMO_KEY][MESSAGES][0]["x_pinned"], true, "everything else is untouched");
        assert!(messages(&extra)[1].vec.is_none(), "…and only that message got one");
        // replacing one is the same call
        assert!(set_vec(&mut extra, "a", &[1.0], "fp-2").is_some());
        assert_eq!(messages(&extra)[0].vec.as_deref(), Some(&[1.0f32][..]));
        assert_eq!(messages(&extra)[0].vfp.as_deref(), Some("fp-2"));
        // an id nobody has, and a vector json cannot spell, change nothing
        assert!(set_vec(&mut extra, "nobody", &[1.0], "fp").is_none());
        assert!(set_vec(&mut extra, "b", &[f32::NAN], "fp").is_none(), "a non-finite float is refused");
        assert!(set_vec(&mut extra, "b", &[1.0, f32::INFINITY], "fp").is_none());
        assert!(messages(&extra)[1].vec.is_none(), "…and left the message readable");
    }

    #[test]
    fn a_refused_save_puts_the_embedding_slots_back_exactly() {
        // the index writes a batch then saves once: if that save is refused the
        // content must go back to the bytes on disk, member for member — a
        // message that HAD a vector keeps its old one, one that had none loses
        // the member entirely (an empty `vfp` would read as "fresh" forever).
        let mut extra = extra_of(
            r#"{"x_memo":{"messages":[
                {"id":"a","ts":"t","text":"one","vec":[0.25],"vfp":"old"},
                {"id":"b","ts":"t","text":"two"}
            ]}}"#,
        );
        let before = serde_json::to_string(&extra).unwrap();
        let slots: Vec<VecSlot> = ["a", "b"]
            .iter()
            .filter_map(|id| set_vec(&mut extra, id, &[9.0], "fp-new"))
            .collect();
        assert_eq!(slots.len(), 2);
        assert_ne!(serde_json::to_string(&extra).unwrap(), before, "the batch landed");
        for slot in &slots {
            assert!(restore_vec(&mut extra, slot));
        }
        assert_eq!(serde_json::to_string(&extra).unwrap(), before, "the rollback is exact");
        // a message that went away meanwhile simply has nothing to restore
        let mut gone = extra_of(r#"{"x_memo":{"messages":[]}}"#);
        assert!(!restore_vec(&mut gone, &slots[0]));
    }
}
