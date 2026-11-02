//! Local semantic search — the whole engine, running ON THIS MACHINE.
//!
//! A password manager may not ship its user's notes to somebody else's server
//! to be "understood", so the embedding model runs here: `multilingual-e5-small`
//! (384 dimensions) executed through ONNX Runtime by [`fastembed`]. It is
//! downloaded ONCE, on the user's word, into `<app data>/models` — about 490 MB
//! of fp32 weights and tokenizer, which is why it is opt-in and why the settings
//! line says so. From then on nothing in this module opens a socket.
//! Multilingual is the point — the user writes Italian and expects "dove ho
//! messo le chiavi" to find "sotto il tappeto", which no keyword match can do.
//!
//! Two searches live here, and they are different tools:
//!   * [`search_literal`] — accent- and case-insensitive substring, ranked by
//!     the command palette's fuzzy scorer, with a snippet of the match. Works
//!     with no model, no download and no CPU to speak of.
//!   * [`search_semantic`] — a dot product against the stored embeddings.
//!     Needs the model; finds what the words never said.
//!
//! WHERE THE VECTORS LIVE: inside the encrypted vault, on the message that
//! produced them (`x_memo.messages[].vec`). An embedding is not a hash — the
//! text is partially recoverable from it — so a vector is exactly as sensitive
//! as the message and must never be written to a cleartext sidecar, an index
//! file or a cache. The vault blob is the only place it is allowed to be.

use crate::i18n::{Key, Lang};
use crate::memo::MemoMessage;
use crate::model::{Item, ItemField};
use anyhow::{anyhow, Result};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use zeroize::Zeroize;

/// The dimensionality of `multilingual-e5-small`. A stored vector of any other
/// length was written by a different model and counts as stale.
pub const EMBED_DIM: usize = 384;

/// The repository fastembed pulls the ONNX + tokenizer from (documented here
/// because [`is_model_present`] has to know the cache layout it lands in).
pub const MODEL_REPO: &str = "intfloat/multilingual-e5-small";

/// E5 was trained with an asymmetric instruction pair: stored text is prefixed
/// `passage: ` and a search is prefixed `query: `. This is not decoration —
/// dropping the prefixes, or using the same one on both sides, measurably
/// degrades retrieval, so the prefixes are applied INSIDE every [`Embedder`]
/// and are never the caller's business.
pub const PASSAGE_PREFIX: &str = "passage: ";
pub const QUERY_PREFIX: &str = "query: ";

/// Which model produced a vector. It is part of the staleness fingerprint, so
/// swapping the engine re-embeds everything instead of silently comparing
/// vectors from two different spaces (whose dot product means nothing).
pub const LOCAL_TAG: &str = "multilingual-e5-small";
pub const MOCK_TAG: &str = "mock-v1";

/// How many passages one inference batch takes. The model is small but the
/// batch is held in memory as f32 tensors; 32 keeps the peak bounded on the
/// laptop this is meant to run on.
const BATCH: usize = 32;

// ---- the engine interface -------------------------------------------------

/// Anything that turns text into a vector. The app has exactly two: the
/// on-device [`LocalEmbedder`] and the [`MockEmbedder`] the tests use — no test
/// in this crate needs a network, a model file or a display.
pub trait Embedder: Send + Sync {
    /// Which model this is (see [`LOCAL_TAG`]).
    fn tag(&self) -> &'static str;

    /// Embeds text FOR STORAGE. Applies [`PASSAGE_PREFIX`]; returns L2-normalized
    /// vectors, so similarity is a plain dot product downstream.
    fn embed_passages(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;

    /// Embeds a SEARCH. Applies [`QUERY_PREFIX`]; L2-normalized like the above.
    ///
    /// Worker thread only, like everything else here: the FIRST call after a
    /// lock (or after startup) also loads the session, which is seconds — on
    /// the render loop that is a frozen screen, not a slow search.
    fn embed_query(&self, text: &str) -> Result<Vec<f32>>;

    /// Makes the impl ready to embed. For the on-device model that means
    /// downloading it (once) and building the ONNX session, which can take
    /// minutes on a slow line — call it from a worker thread only.
    fn warm(&self) -> Result<()> {
        Ok(())
    }

    /// Frees whatever the impl is holding (the ONNX session, whose weights are
    /// most of a gigabyte). Called when the vault locks: the model holds no
    /// secrets, but there is no reason to keep it resident behind a lock screen.
    ///
    /// IT RUNS ON THE UI THREAD and must therefore never block: an impl whose
    /// engine is busy defers the free instead of waiting for it.
    fn unload(&self) {}
}

// ---- why the engine could not answer ---------------------------------------

/// A failure the USER is going to read. Every reason this crate writes itself
/// gets a variant (and an [`i18n::Key`] behind it); only [`RagFault::Engine`]
/// passes text through untranslated, because those are the model's own words
/// (a path, an HTTP failure) and not ours to invent an Italian for.
///
/// The [`crate::image_field::ImportError`] shape: a typed error the caller
/// localizes at the moment it shows it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum RagFault {
    /// The model/index worker died without answering.
    ModelWorkerStopped,
    /// …and the same for the query worker.
    SearchWorkerStopped,
    /// A batch came back with a count its messages cannot be matched to.
    VectorCount,
    /// An embed that returned nothing at all.
    NoVector,
    /// The model mutex was poisoned by a panic inside an inference.
    ModelLock,
    /// The engine's own reason — never anything out of the vault.
    Engine(String),
}

impl RagFault {
    /// The localized reason, ready to be interpolated into a status line.
    pub fn message(&self, lang: Lang) -> String {
        match self {
            RagFault::ModelWorkerStopped => crate::i18n::t(lang, &Key::RagFaultModelWorker),
            RagFault::SearchWorkerStopped => crate::i18n::t(lang, &Key::RagFaultSearchWorker),
            RagFault::VectorCount => crate::i18n::t(lang, &Key::RagFaultVectorCount),
            RagFault::NoVector => crate::i18n::t(lang, &Key::RagFaultNoVector),
            RagFault::ModelLock => crate::i18n::t(lang, &Key::RagFaultModelLock),
            RagFault::Engine(why) => why.clone(),
        }
    }
}

impl std::fmt::Display for RagFault {
    /// English, for a log and for [`anyhow`]'s own chain — the screen goes
    /// through [`RagFault::message`] instead.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RagFault::ModelWorkerStopped => write!(f, "the model worker stopped"),
            RagFault::SearchWorkerStopped => write!(f, "the search worker stopped"),
            RagFault::VectorCount => write!(f, "the model returned the wrong number of vectors"),
            RagFault::NoVector => write!(f, "the model returned no vector"),
            RagFault::ModelLock => write!(f, "the embedding model lock is poisoned"),
            RagFault::Engine(why) => write!(f, "{why}"),
        }
    }
}

impl std::error::Error for RagFault {}

/// The fault behind an [`Embedder`] error. The trait answers `anyhow` (fastembed
/// does, and the API layer only needs "it failed"), so a reason THIS crate wrote
/// travels as a downcastable payload and everything else is the engine's words.
fn fault_of(error: anyhow::Error) -> RagFault {
    error.downcast::<RagFault>().unwrap_or_else(|e| RagFault::Engine(e.to_string()))
}

/// Scales `v` to unit length, in place. A zero (or non-finite) vector is left
/// alone rather than turned into NaNs — [`crate::memo::set_vec`] refuses
/// non-finite floats, and a refused write is better than a poisoned index.
pub fn l2_normalize(mut v: Vec<f32>) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm.is_finite() && norm > f32::EPSILON {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
    v
}

// ---- the on-device model --------------------------------------------------

/// `multilingual-e5-small` through ONNX Runtime, loaded lazily and shared by
/// every worker: building the session costs about a second and the RAM of the
/// weights, so it happens once per run at most.
pub struct LocalEmbedder {
    /// The fastembed/hf-hub cache root — `<app data>/models`.
    dir: PathBuf,
    /// `None` until the first use, and again after [`Embedder::unload`].
    /// The lock also serializes inference, which ONNX Runtime sessions want.
    /// Behind an `Arc` so [`Embedder::unload`] can hand the wait to a thread
    /// instead of doing it on the caller's (see there).
    model: Arc<Mutex<Option<fastembed::TextEmbedding>>>,
}

impl LocalEmbedder {
    pub fn new(dir: PathBuf) -> LocalEmbedder {
        LocalEmbedder { dir, model: Arc::new(Mutex::new(None)) }
    }

    /// Runs `f` against the loaded session, loading (and, the first time,
    /// DOWNLOADING) it if needed.
    fn with_model<T>(
        &self,
        f: impl FnOnce(&mut fastembed::TextEmbedding) -> Result<T>,
    ) -> Result<T> {
        let mut guard =
            self.model.lock().map_err(|_| anyhow::Error::new(RagFault::ModelLock))?;
        if guard.is_none() {
            crate::paths::create_private_dir(&self.dir)?;
            let options = fastembed::InitOptions::new(fastembed::EmbeddingModel::MultilingualE5Small)
                .with_cache_dir(self.dir.clone())
                // NEVER true: fastembed's progress bar writes to stdout, and in
                // an alternate-screen TUI stdout IS the screen.
                .with_show_download_progress(false);
            let model = fastembed::TextEmbedding::try_new(options)
                .map_err(|e| anyhow!("{e}"))?;
            *guard = Some(model);
        }
        f(guard.as_mut().expect("just loaded"))
    }

    /// The shared body of both embed calls: prefix, infer, normalize. The
    /// prefixed copies are wiped afterwards — they are vault text.
    fn run(&self, texts: &[String], prefix: &str) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let mut out: Vec<Vec<f32>> = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(BATCH) {
            let mut prefixed: Vec<String> =
                chunk.iter().map(|t| format!("{prefix}{t}")).collect();
            let batch = self.with_model(|m| {
                m.embed(prefixed.as_slice(), None).map_err(|e| anyhow!("{e}"))
            });
            for text in prefixed.iter_mut() {
                text.zeroize();
            }
            out.extend(batch?.into_iter().map(l2_normalize));
        }
        Ok(out)
    }
}

impl Embedder for LocalEmbedder {
    fn tag(&self) -> &'static str {
        LOCAL_TAG
    }

    fn embed_passages(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.run(texts, PASSAGE_PREFIX)
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        let mut owned = vec![text.to_string()];
        let result = self.run(&owned, QUERY_PREFIX);
        owned[0].zeroize(); // a search over a vault is vault-shaped too
        result?.pop().ok_or_else(|| anyhow::Error::new(RagFault::NoVector))
    }

    fn warm(&self) -> Result<()> {
        // an inference, not just a load: a truncated download or a model this
        // build cannot run must fail HERE, on the worker, not on the first search
        self.with_model(|m| m.embed(["passage: ok"].as_slice(), None).map_err(|e| anyhow!("{e}")))?;
        Ok(())
    }

    /// NEVER WAITS FOR THE LOCK. This is called from the UI thread — the vault
    /// locking, closing, the settings toggle going off — while a worker (or an
    /// agent-API request) may be holding the model across a cold session build,
    /// which is seconds and, with the cache gone, a download. Blocking here
    /// would be a frozen screen with the vault's ledger still painted on it.
    /// A model that is busy is dropped by a thread of its own, the moment its
    /// holder lets go.
    fn unload(&self) {
        match self.model.try_lock() {
            Ok(mut guard) => *guard = None,
            // a panic inside an inference poisoned it: the session is still
            // ours to drop, and leaving it resident would be the worse answer
            Err(std::sync::TryLockError::Poisoned(held)) => *held.into_inner() = None,
            Err(std::sync::TryLockError::WouldBlock) => {
                let model = Arc::clone(&self.model);
                std::thread::Builder::new()
                    .name("rag-unload".into())
                    .spawn(move || match model.lock() {
                        Ok(mut guard) => *guard = None,
                        Err(held) => *held.into_inner() = None,
                    })
                    .ok();
            }
        }
    }
}

/// Whether the model is already on this machine (so a search can run offline
/// and a settings line can say so). Checks the hf-hub cache layout fastembed
/// downloads into: `models--<org>--<repo>/snapshots/<revision>/…`.
pub fn is_model_present(dir: &Path) -> bool {
    let repo = dir.join(format!("models--{}", MODEL_REPO.replace('/', "--")));
    let Ok(snapshots) = std::fs::read_dir(repo.join("snapshots")) else {
        return false;
    };
    // every file fastembed asks the repo for; a half-finished download leaves
    // some of them missing, and "present" has to mean "loadable offline".
    const NEEDED: [&str; 5] = [
        "onnx/model.onnx",
        "tokenizer.json",
        "config.json",
        "special_tokens_map.json",
        "tokenizer_config.json",
    ];
    snapshots.flatten().any(|entry| {
        let snap = entry.path();
        NEEDED.iter().all(|f| snap.join(f).is_file())
    })
}

// ---- how far the download has got ------------------------------------------

/// How many bytes of the model are on this machine RIGHT NOW: every regular
/// file under `dir`, recursively. It is the only progress there is to have —
/// fastembed's own is a stdout bar an alternate-screen TUI cannot host — and
/// the hf-hub cache makes it honest: a download in flight is a `<etag>.part`
/// growing inside `models--*/blobs/`, renamed over the blob when it lands.
///
/// SYMLINKS ARE NOT FOLLOWED, on purpose: the `snapshots/` tree is links INTO
/// `blobs/`, so following them would count the half-gigabyte weights twice.
/// (Windows hf-hub, which cannot always make a link, MOVES the blob to the
/// snapshot instead — still exactly one copy either way.) Not following them
/// also means the walk cannot loop, which is half of "never panics"; the other
/// half is that every error — a missing directory, a file that vanished
/// mid-walk, one we may not read — is simply zero bytes.
pub fn dir_bytes(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    let mut total = 0u64;
    for entry in entries.flatten() {
        // `file_type` here is the LINK's own (read_dir does not follow), which
        // is what makes a symlinked snapshot weigh nothing.
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            total = total.saturating_add(dir_bytes(&entry.path()));
        } else if kind.is_file() {
            total = total.saturating_add(entry.metadata().map(|m| m.len()).unwrap_or(0));
        }
    }
    total
}

/// What a COMPLETE cache of [`MODEL_REPO`] weighs, in the tree fastembed 6 /
/// hf-hub 0.5 materialize for revision `614241f622f53c4eeff9890bdc4f31cfecc418b3`
/// — measured with [`dir_bytes`] on a finished download: the fp32
/// `onnx/model.onnx` (470 MB) and `tokenizer.json` (17 MB), the three small
/// JSON files and `refs/main`.
///
/// It is the DENOMINATOR of the status-row bar and nothing else. A revision
/// that weighs a little more than this one would run past the end, so
/// [`download_pct`] clamps at 99 %: the hundredth point is not ours to give —
/// only [`ModelEvent::Ready`] (weights on disk AND a session that ran an
/// inference) means done, and that is what takes the bar away.
pub const EXPECTED_MODEL_BYTES: u64 = 487_352_545;

/// How much of the model is down, as a percentage of [`EXPECTED_MODEL_BYTES`],
/// never above 99 (see there) and never overflowing on a number the cache
/// could not really hold.
pub fn download_pct(bytes: u64) -> u8 {
    (bytes.saturating_mul(100) / EXPECTED_MODEL_BYTES).min(99) as u8
}

/// Whole MiB (floor), for the `200/464 MiB` half of the bar.
pub fn mib(bytes: u64) -> u64 {
    bytes / (1024 * 1024)
}

// ---- the model manager (a detached first-init) -----------------------------

/// What the model worker reports back: honestly start / done / failed, because
/// fastembed reports nothing in between. The bytes the bar draws are measured
/// off the cache directory instead ([`dir_bytes`]) — the worker never sends a
/// number, so it can never lie about one.
pub enum ModelEvent {
    Ready,
    Failed(RagFault),
}

/// Starts the download on a detached thread and answers on `tx` (drained from
/// `App::on_tick`, like `poll_sync`). fastembed has no separate "fetch" step:
/// it downloads on the first `TextEmbedding` init, so this IS a background
/// first-init — the model is loaded and left loaded, ready for the first search.
pub fn start_model_download(embedder: Arc<dyn Embedder>, tx: Sender<ModelEvent>) {
    std::thread::Builder::new()
        .name("rag-model".into())
        .spawn(move || {
            let event = match embedder.warm() {
                Ok(()) => ModelEvent::Ready,
                Err(e) => ModelEvent::Failed(fault_of(e)),
            };
            let _ = tx.send(event);
        })
        .ok();
}

// ---- literal search --------------------------------------------------------

/// One literal hit: which item, how well it matched, and the words around the
/// match to show for it.
pub struct LiteralHit {
    pub id: String,
    pub score: i32,
    pub snippet: String,
}

/// Longest snippet handed back, in characters, and how much of it sits BEFORE
/// the match so the hit reads in context rather than starting on it.
const SNIPPET_CHARS: usize = 72;
const SNIPPET_LEAD: usize = 16;

/// Case- and accent-insensitive substring search over `items` (`(id, text)`),
/// best first. A hit requires the query to appear CONTIGUOUSLY — that is what
/// makes this the literal search — and is then ranked with the command
/// palette's [`crate::palette::fuzzy_score`], which already pays for word
/// starts and contiguous runs, plus a bonus for matching early in the text.
///
/// An empty query has no hits: "everything" is a list, not a search result.
pub fn search_literal(items: &[(String, String)], query: &str) -> Vec<LiteralHit> {
    let needle = fold(&flatten(query));
    if needle.is_empty() {
        return Vec::new();
    }
    let needle_s: String = needle.iter().collect();
    let mut scored: Vec<(i32, usize, LiteralHit)> = Vec::new();
    for (rank, (id, text)) in items.iter().enumerate() {
        let flat = flatten(text);
        let folded = fold(&flat);
        let Some(at) = find_sub(&folded, &needle) else { continue };
        let folded_s: String = folded.iter().collect();
        // the contiguous hit is the qualification; the fuzzy score and the two
        // bonuses only order the hits among themselves.
        let mut score = 100 + crate::palette::fuzzy_score(&folded_s, &needle_s).unwrap_or(0);
        if at == 0 || !folded[at - 1].is_alphanumeric() {
            score += 25; // the match starts a word
        }
        score += 32 - (at as i32).min(32); // …and starts early in the text
        scored.push((score, rank, LiteralHit { id: id.clone(), score, snippet: snippet(&flat, at) }));
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, _, hit)| hit).collect()
}

/// Whitespace collapsed to single spaces and trimmed. Every later index — the
/// match position, the snippet window — is into THIS string, so matching and
/// display can never disagree about where the hit was.
fn flatten(text: &str) -> Vec<char> {
    let mut out: Vec<char> = Vec::with_capacity(text.len());
    let mut space = true; // leading whitespace is dropped
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !space {
                out.push(' ');
                space = true;
            }
        } else {
            out.push(ch);
            space = false;
        }
    }
    if out.last() == Some(&' ') {
        out.pop();
    }
    out
}

/// Lowercase + accent-stripped, ONE character out per character in — the
/// snippet indexes the original by the position the folded match was found at,
/// so the two must stay the same length. (That is why `ß` folds to a single
/// `s` and the multi-character lowercases are truncated: neither occurs in the
/// languages this ships with, and alignment is worth more than either.)
fn fold(chars: &[char]) -> Vec<char> {
    chars.iter().map(|&c| fold_char(c)).collect()
}

fn fold_char(c: char) -> char {
    let c = c.to_lowercase().next().unwrap_or(c);
    match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' | 'æ' => 'a',
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => 'c',
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => 'e',
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'į' => 'i',
        'ñ' | 'ń' | 'ņ' | 'ň' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => 'o',
        'ß' | 'ś' | 'ŝ' | 'ş' | 'š' => 's',
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ů' | 'ű' | 'ų' => 'u',
        'ý' | 'ÿ' => 'y',
        'ź' | 'ż' | 'ž' => 'z',
        other => other,
    }
}

/// First position of `needle` inside `hay`, both already folded.
fn find_sub(hay: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| hay[i..i + needle.len()] == *needle)
}

/// The one-line preview of a message for a results list, when there is no
/// match position to build a window around — a semantic hit matched the WHOLE
/// message, not a substring of it. Same flattening and same width as a literal
/// snippet, so the two kinds of hit read as one list.
pub fn preview(text: &str) -> String {
    let flat = flatten(text);
    snippet(&flat, 0)
}

/// A window of `flat` around the match at `at`, with `…` where it was cut.
fn snippet(flat: &[char], at: usize) -> String {
    if flat.len() <= SNIPPET_CHARS {
        return flat.iter().collect();
    }
    let start = at.saturating_sub(SNIPPET_LEAD).min(flat.len() - SNIPPET_CHARS);
    let end = start + SNIPPET_CHARS;
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.extend(&flat[start..end]);
    if end < flat.len() {
        out.push('…');
    }
    out
}

// ---- semantic search -------------------------------------------------------

/// One semantic hit: which item, and the cosine similarity that ranked it
/// (a dot product, because every vector here left its [`Embedder`] normalized).
pub struct SemanticHit {
    pub id: String,
    pub score: f32,
}

/// The `k` nearest entries of `index` to `query`, best first. Ties keep index
/// order, so the same query twice returns the same list.
///
/// A stored vector of the wrong length is SKIPPED, not compared: it came from
/// another model, and a dot product across two embedding spaces is noise
/// dressed as a score. The next index pass replaces it (see [`memo_pending`]).
pub fn search_semantic(index: &[(String, Vec<f32>)], query: &[f32], k: usize) -> Vec<SemanticHit> {
    if query.is_empty() || k == 0 {
        return Vec::new();
    }
    let mut scored: Vec<(usize, SemanticHit)> = index
        .iter()
        .enumerate()
        .filter(|(_, (_, v))| v.len() == query.len())
        .map(|(i, (id, v))| {
            let score = v.iter().zip(query).map(|(a, b)| a * b).sum::<f32>();
            (i, SemanticHit { id: id.clone(), score })
        })
        .filter(|(_, hit)| hit.score.is_finite())
        .collect();
    scored.sort_by(|a, b| {
        b.1.score
            .partial_cmp(&a.1.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    scored.truncate(k);
    scored.into_iter().map(|(_, hit)| hit).collect()
}

// ---- the memorandum index --------------------------------------------------

/// What a message contributes to the index: its words and its photo's caption,
/// never the picture. Image bytes are not text, embedding them is meaningless,
/// and a base64 blob would blow past the model's token window on its own.
/// Empty (both parts blank) means "nothing to embed" — a bare photo is found by
/// scrolling, not by searching.
pub fn memo_embed_text(msg: &MemoMessage) -> String {
    let caption = msg.image.as_ref().and_then(|i| i.caption.as_deref()).unwrap_or("");
    memo_text(&msg.text, caption)
}

/// The same join over the two parts on their own — what a BORROWED read of the
/// stored json ([`crate::memo::rows`]) has to work with, and what
/// [`memo_embed_text`] is once a message has been unpacked. One implementation,
/// so the app's full read and the API's light one index a message identically.
pub fn memo_text(text: &str, caption: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in [text.trim(), caption.trim()] {
        if !part.is_empty() {
            parts.push(part);
        }
    }
    parts.join("\n")
}

/// The staleness fingerprint stored beside a vector: blake3 over the model tag
/// AND the exact text that was embedded. Editing the text, or changing the
/// model, changes this — which is the whole re-embed trigger.
pub fn fingerprint(tag: &str, text: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(tag.as_bytes());
    hasher.update(&[0]); // a separator, so ("ab","c") and ("a","bc") differ
    hasher.update(text.as_bytes());
    hasher.finalize().to_hex().to_string()
}

/// One message waiting to be embedded. The text is vault content, so it is
/// wiped when this is dropped rather than left in a freed heap buffer.
pub struct Pending {
    pub id: String,
    pub text: String,
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.text.zeroize();
    }
}

/// The messages whose vectors are missing or stale, in stored order. Nothing
/// else is touched: an unchanged conversation yields an EMPTY list, and an
/// empty list means no worker and no save at all.
pub fn memo_pending(messages: &[MemoMessage], tag: &str) -> Vec<Pending> {
    let mut out = Vec::new();
    for msg in messages {
        let text = memo_embed_text(msg);
        if text.is_empty() {
            continue;
        }
        let fresh = msg.vec.as_ref().is_some_and(|v| v.len() == EMBED_DIM)
            && msg.vfp.as_deref() == Some(fingerprint(tag, &text).as_str());
        if !fresh {
            out.push(Pending { id: msg.id.clone(), text });
        }
    }
    out
}

/// The searchable index of a conversation: every message that already carries a
/// well-formed vector. Messages still waiting on one are simply not findable
/// semantically yet — the literal search covers them meanwhile.
pub fn memo_index(messages: &[MemoMessage]) -> Vec<(String, Vec<f32>)> {
    messages
        .iter()
        .filter_map(|m| {
            let v = m.vec.as_ref()?;
            (v.len() == EMBED_DIM && v.iter().all(|f| f.is_finite()))
                .then(|| (m.id.clone(), v.clone()))
        })
        .collect()
}

/// One finished embedding on its way back to the vault.
pub struct Embedded {
    pub id: String,
    pub vec: Vec<f32>,
    pub fp: String,
}

/// Which of the two indexes a worker (or a finished batch) is about. One engine,
/// one worker slot, two destinations inside the vault — the scope is what says
/// where a batch of vectors belongs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IndexScope {
    /// The conversation, stored ON each message (`x_memo.messages[].vec`).
    Memo,
    /// The items, stored in one side table (`x_rag.items`).
    Items,
}

/// What the index worker reports back.
pub enum IndexEvent {
    Done { vault: String, scope: IndexScope, vectors: Vec<Embedded> },
    Failed { vault: String, scope: IndexScope, fault: RagFault },
}

/// Embeds `pending` on a detached thread and answers on `tx`. `vault` rides
/// along so the tick that receives it can check the answer still belongs to the
/// vault that is open — a lock, a close or an unlock of another vault in the
/// meantime makes the batch worthless, never misfiled; `scope` says which index
/// the vectors are for.
pub fn start_index_pass(
    embedder: Arc<dyn Embedder>,
    vault: String,
    scope: IndexScope,
    pending: Vec<Pending>,
    tx: Sender<IndexEvent>,
) {
    std::thread::Builder::new()
        .name("rag-index".into())
        .spawn(move || {
            let mut texts: Vec<String> = pending.iter().map(|p| p.text.clone()).collect();
            let event = match embedder.embed_passages(&texts) {
                Ok(vectors) if vectors.len() == pending.len() => IndexEvent::Done {
                    vault,
                    scope,
                    vectors: pending
                        .iter()
                        .zip(vectors)
                        .map(|(p, vec)| Embedded {
                            id: p.id.clone(),
                            fp: fingerprint(embedder.tag(), &p.text),
                            vec,
                        })
                        .collect(),
                },
                // a model that answers with the wrong count is a model whose
                // vectors cannot be matched to messages: refuse the whole batch
                Ok(_) => IndexEvent::Failed { vault, scope, fault: RagFault::VectorCount },
                Err(e) => IndexEvent::Failed { vault, scope, fault: fault_of(e) },
            };
            // …the clones the model was handed. They are vault text like the
            // `Pending` they came from, and freeing a `Vec<String>` leaves it
            // in the heap: wipe each one, exactly as `Pending::drop` does.
            for text in texts.iter_mut() {
                text.zeroize();
            }
            let _ = tx.send(event);
        })
        .ok();
}

// ---- the ITEM index --------------------------------------------------------
//
// AN EMBEDDING IS NOT A HASH. The text it was computed from is partially
// recoverable from the vector alone, so embedding a password would be storing
// that password in a second, weaker form — and, with the API on, SERVING it.
// No secret value is ever indexed. That is not a convention the callers have to
// remember: every field the index builder can see arrives through
// [`indexable_fields`], which hands over `value: None` for `secret == true` and
// for every image field. There is no other path into [`item_embed_text`].

/// One item field as the index is ALLOWED to see it: its machine key, its
/// display label, and its value ONLY when the field is not secret.
pub struct Indexable<'a> {
    pub key: &'a str,
    pub label: Option<&'a str>,
    /// `None` for every `secret` field — a password, a PIN — and for every
    /// image field, whose base64 is not text.
    pub value: Option<&'a str>,
}

/// THE filter. `field.secret` is the authoritative masking flag everywhere else
/// in the app (see `model`), and it is the authoritative INDEXING flag here.
///
/// The image test beside it is NOT a second masking rule but a "this value is
/// not text" one, and it is the same defence `ui::item_subtitle` keeps: a
/// foreign vault that forgot `secret: true` on a picture (the member is
/// `#[serde(default)]`, so missing reads as false) must still never push a
/// megabyte of base64 through the tokenizer, into the stored vectors and out
/// of the agent API as a snippet.
pub fn indexable_fields(fields: &[ItemField]) -> Vec<Indexable<'_>> {
    fields
        .iter()
        .map(|f| Indexable {
            key: f.key.as_str(),
            label: f.label.as_deref(),
            value: (!f.secret && !crate::image_field::is_image(f)).then(|| f.value.as_str()),
        })
        .collect()
}

/// What an item contributes to the index: its label, the localized name of its
/// category, the name of every field, and the value of every NON-SECRET one.
/// `category` is passed in already localized — "carta di credito" is what an
/// Italian user would search for, and only the app knows the language.
///
/// An item picture contributes nothing of its own: an image FIELD has no
/// caption member (only a Memorandum photo does), its label IS its caption and
/// is already carried by the field-name pass, and its base64 is dropped by
/// [`indexable_fields`] — as a picture, not merely as a secret — before any of
/// this runs.
pub fn item_embed_text(item: &Item, category: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    let label = item.label.trim();
    if !label.is_empty() {
        parts.push(label);
    }
    let category = category.trim();
    if !category.is_empty() {
        parts.push(category);
    }
    let fields = indexable_fields(&item.fields);
    for field in &fields {
        let name = field.label.map(str::trim).filter(|s| !s.is_empty()).unwrap_or(field.key);
        if !name.is_empty() {
            parts.push(name);
        }
        if let Some(value) = field.value.map(str::trim).filter(|s| !s.is_empty()) {
            parts.push(value);
        }
    }
    parts.join("\n")
}

/// The `(id, indexed text)` pair of every item, built through the filter above.
/// `category_name` localizes a category id (`|id| i18n::t(lang, …)`).
pub fn item_texts(items: &[Item], category_name: impl Fn(&str) -> String) -> Vec<(String, String)> {
    items
        .iter()
        .map(|item| (item.id.clone(), item_embed_text(item, &category_name(&item.category))))
        .filter(|(_, text)| !text.is_empty())
        .collect()
}

/// Where the item vectors live inside `VaultContent::extra` — the `x_sync` /
/// `x_memo` / `x_image` precedent, so DCS-1 stays at version 1 and a reader
/// that knows nothing about it round-trips the whole block untouched.
///
/// ```json
/// "x_rag": { "model": "multilingual-e5-small",
///            "items": { "…uuid…": { "fp": "…blake3 hex…", "vec": [0.03, …] } } }
/// ```
pub const RAG_KEY: &str = "x_rag";

/// The stored side table, read back. `model` is part of the staleness test:
/// vectors from two different models live in two different spaces and their dot
/// product means nothing, so a changed engine re-embeds everything.
#[derive(Default)]
pub struct ItemVectors {
    pub model: String,
    /// `id -> (fingerprint, vector)`, skipping anything malformed.
    pub items: std::collections::HashMap<String, (String, Vec<f32>)>,
}

/// Reads `x_rag`. Tolerant like every other read of the flatten map: a member
/// of the wrong shape is skipped, never a panic and never a reason to drop the
/// rest (the next pass simply re-embeds what it could not read).
pub fn read_item_vectors(extra: &Map<String, Value>) -> ItemVectors {
    let Some(block) = extra.get(RAG_KEY).and_then(Value::as_object) else {
        return ItemVectors::default();
    };
    let model = block.get("model").and_then(Value::as_str).unwrap_or_default().to_string();
    let mut items = std::collections::HashMap::new();
    if let Some(stored) = block.get("items").and_then(Value::as_object) {
        for (id, entry) in stored {
            let Some(entry) = entry.as_object() else { continue };
            let Some(fp) = entry.get("fp").and_then(Value::as_str) else { continue };
            let Some(raw) = entry.get("vec").and_then(Value::as_array) else { continue };
            let vec: Vec<f32> = raw.iter().filter_map(|v| v.as_f64().map(|f| f as f32)).collect();
            if vec.len() != raw.len() {
                continue; // a null or a string among the floats: unusable
            }
            items.insert(id.clone(), (fp.to_string(), vec));
        }
    }
    ItemVectors { model, items }
}

/// What `x_rag` held before a write, so a refused save can put the content back
/// to the bytes its fingerprint was taken over. `None` = the member was absent,
/// which is what restoring removes it back to.
pub struct RagSlot(Option<Value>);

/// Merges `vectors` into `x_rag` and prunes every entry whose item is gone
/// (`live` is the id set of the items that still exist). A stored block from
/// ANOTHER model is dropped whole rather than merged into: mixing two embedding
/// spaces in one index is worse than an empty one.
///
/// Returns the rollback slot. A non-finite float is refused for the whole
/// entry — json has no spelling for one, and serde would write `null`.
pub fn set_item_vectors(
    extra: &mut Map<String, Value>,
    tag: &str,
    vectors: &[Embedded],
    live: &[String],
) -> RagSlot {
    let slot = RagSlot(extra.get(RAG_KEY).cloned());
    let mut stored = read_item_vectors(extra);
    if stored.model != tag {
        stored.items.clear();
    }
    for embedded in vectors {
        if embedded.vec.iter().any(|f| !f.is_finite()) {
            continue;
        }
        stored.items.insert(embedded.id.clone(), (embedded.fp.clone(), embedded.vec.clone()));
    }
    stored.items.retain(|id, _| live.iter().any(|live_id| live_id == id));
    // BTreeMap ordering (serde_json's Map): the same content serializes to the
    // same bytes on every machine, which is what `content_fingerprint` needs.
    let mut items = Map::new();
    for (id, (fp, vec)) in stored.items {
        let Ok(vec) = serde_json::to_value(&vec) else { continue };
        items.insert(id, serde_json::json!({ "fp": fp, "vec": vec }));
    }
    extra.insert(
        RAG_KEY.to_string(),
        serde_json::json!({ "model": tag, "items": Value::Object(items) }),
    );
    slot
}

/// Puts `x_rag` back exactly as [`set_item_vectors`] found it.
pub fn restore_item_vectors(extra: &mut Map<String, Value>, slot: RagSlot) {
    match slot.0 {
        Some(value) => {
            extra.insert(RAG_KEY.to_string(), value);
        }
        None => {
            extra.remove(RAG_KEY);
        }
    }
}

/// The items whose vectors are missing or stale, in stored order — the exact
/// counterpart of [`memo_pending`]. An unchanged vault yields an EMPTY list, so
/// no worker starts and no save happens.
pub fn items_pending(
    texts: &[(String, String)],
    tag: &str,
    stored: &ItemVectors,
) -> Vec<Pending> {
    if stored.model != tag {
        // another model: everything is stale, whatever the fingerprints say
        return texts
            .iter()
            .map(|(id, text)| Pending { id: id.clone(), text: text.clone() })
            .collect();
    }
    texts
        .iter()
        .filter(|(id, text)| {
            !stored.items.get(id).is_some_and(|(fp, vec)| {
                vec.len() == EMBED_DIM && fp.as_str() == fingerprint(tag, text)
            })
        })
        .map(|(id, text)| Pending { id: id.clone(), text: text.clone() })
        .collect()
}

/// The searchable item index: every stored vector of the right width, in the
/// order the items are in. Items still waiting on one are simply not findable
/// semantically yet — the literal search covers them meanwhile.
pub fn item_index(texts: &[(String, String)], stored: &ItemVectors) -> Vec<(String, Vec<f32>)> {
    texts
        .iter()
        .filter_map(|(id, _)| {
            let (_, vec) = stored.items.get(id)?;
            (vec.len() == EMBED_DIM && vec.iter().all(|f| f.is_finite()))
                .then(|| (id.clone(), vec.clone()))
        })
        .collect()
}

// ---- the query worker (a search the tick loop must not wait for) -----------

/// What the query worker reports back. `seq` says WHICH search it belongs to:
/// embedding a query is seconds on a cold model, and by then the bar may have
/// been re-typed, re-moded or closed — an answer whose seq is no longer the
/// live one is dropped rather than shown for a query nobody can see.
pub enum QueryEvent {
    Done { seq: u64, vec: Vec<f32> },
    Failed { seq: u64, fault: RagFault },
}

/// Embeds one search on a detached thread and answers on `tx`. The query is
/// vault-shaped text (it is what the user is looking for IN their vault), so
/// the worker's copy is wiped before it goes.
pub fn start_query_embed(
    embedder: Arc<dyn Embedder>,
    seq: u64,
    query: String,
    tx: Sender<QueryEvent>,
) {
    std::thread::Builder::new()
        .name("rag-query".into())
        .spawn(move || {
            let mut query = query;
            let event = match embedder.embed_query(&query) {
                Ok(vec) => QueryEvent::Done { seq, vec },
                Err(e) => QueryEvent::Failed { seq, fault: fault_of(e) },
            };
            query.zeroize();
            let _ = tx.send(event);
        })
        .ok();
}

// ---- app-facing state ------------------------------------------------------

/// What the settings page / status line says about semantic search.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub enum RagStatus {
    /// The user hasn't turned it on. Literal search still works.
    #[default]
    Off,
    /// On, but the model isn't on this machine yet.
    Absent,
    /// A first-init worker is fetching it.
    Downloading,
    /// A worker is embedding the messages that need it.
    Indexing,
    Ready,
    /// Carries WHY, typed: a reason this crate wrote is localized like every
    /// other line on screen, and only the engine's own words pass through
    /// untranslated (see [`RagFault`]). Never anything out of the vault.
    Failed(RagFault),
}

impl RagStatus {
    /// The localized line for this state.
    pub fn label(&self, lang: Lang) -> String {
        match self {
            RagStatus::Off => crate::i18n::t(lang, &Key::RagStatusOff),
            RagStatus::Absent => crate::i18n::t(lang, &Key::RagStatusAbsent),
            RagStatus::Downloading => crate::i18n::t(lang, &Key::RagStatusDownloading),
            RagStatus::Indexing => crate::i18n::t(lang, &Key::RagStatusIndexing),
            RagStatus::Ready => crate::i18n::t(lang, &Key::RagStatusReady),
            RagStatus::Failed(fault) => {
                crate::i18n::t(lang, &Key::RagStatusFailed(&fault.message(lang)))
            }
        }
    }
}

/// What a tick of [`RagState::poll`] hands the app to act on.
pub enum RagOutcome {
    ModelReady,
    ModelFailed(RagFault),
    /// Vectors to write into the open vault, in one batched save.
    Indexed { vault: String, scope: IndexScope, vectors: Vec<Embedded> },
    IndexFailed { vault: String, scope: IndexScope, fault: RagFault },
    /// A search's query vector, ready to be ranked against the index.
    Query { seq: u64, vec: Vec<f32> },
    QueryFailed { seq: u64, fault: RagFault },
}

/// The app's semantic-search state: the engine handle, the worker channels and
/// what to say about all of it.
pub struct RagState {
    /// The engine every worker shares. The app's is the on-device
    /// [`LocalEmbedder`]; [`RagState::use_mock`] swaps in the test double.
    embedder: Arc<dyn Embedder>,
    /// Where the model is cached (`<app data>/models`).
    dir: PathBuf,
    /// Whether semantic search is on. Seeded from "is the model already here?"
    /// — downloading it IS the opt-in — and then owned by the settings toggle
    /// through [`RagState::set_enabled`].
    enabled: bool,
    pub status: RagStatus,
    model_rx: Option<Receiver<ModelEvent>>,
    index_rx: Option<Receiver<IndexEvent>>,
    query_rx: Option<Receiver<QueryEvent>>,
    /// The sequence number of the newest search. Bumped by every start AND by
    /// every cancel, so "is this answer still wanted?" is one comparison.
    query_seq: u64,
    /// A finished batch the open vault could not take yet, because a re-key or
    /// a sync is mid-flight over the very same file. Retried on the next tick.
    deferred: Option<(String, IndexScope, Vec<Embedded>)>,
    /// Bytes seen in the model cache on the last sample, while a download runs.
    /// `None` until the first one lands (and again when it ends): the status
    /// row draws an indeterminate bar then rather than a lying 0 %.
    download_bytes: Option<u64>,
}

impl RagState {
    pub fn new(dir: PathBuf) -> RagState {
        let present = is_model_present(&dir);
        RagState {
            embedder: Arc::new(LocalEmbedder::new(dir.clone())),
            dir,
            enabled: present,
            status: if present { RagStatus::Ready } else { RagStatus::Off },
            model_rx: None,
            index_rx: None,
            query_rx: None,
            query_seq: 0,
            deferred: None,
            download_bytes: None,
        }
    }

    /// Test hook: replaces the on-device engine with the deterministic mock, so
    /// the whole index path runs with no model, no network and no display.
    #[cfg(test)]
    pub fn use_mock(&mut self) {
        self.embedder = Arc::new(MockEmbedder);
        self.enabled = true;
        self.status = RagStatus::Ready;
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// The settings toggle. Turning it OFF frees the model straight away;
    /// turning it on says plainly whether the model still has to be fetched.
    pub fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
        if on {
            if !matches!(self.status, RagStatus::Downloading) {
                self.status =
                    if self.model_present() { RagStatus::Ready } else { RagStatus::Absent };
            }
        } else {
            self.embedder.unload();
            self.status = RagStatus::Off;
        }
    }

    pub fn model_present(&self) -> bool {
        is_model_present(&self.dir)
    }

    /// True while the one-time download is out — what the status row draws its
    /// progress bar for.
    pub fn downloading(&self) -> bool {
        matches!(self.status, RagStatus::Downloading)
    }

    /// Bytes measured in the model cache on the last sample; `None` before the
    /// first one (the bar is indeterminate then) and once the download is over.
    pub fn download_bytes(&self) -> Option<u64> {
        self.download_bytes
    }

    /// Re-measures the model cache, if a download is out at all. Called from
    /// `App::on_tick`, THROTTLED to about a second: the walk is a dozen `stat`s
    /// over a directory the OS has just written, which the 120 ms loop cannot
    /// feel, but there is also nothing to see faster than that.
    pub fn sample_download(&mut self) {
        if self.downloading() {
            self.download_bytes = Some(dir_bytes(&self.dir));
        }
    }

    /// Test hook: the bar's state without a download — no network, no worker,
    /// and no half a gigabyte written to a temp dir just to paint a percentage.
    #[cfg(test)]
    pub fn set_download_bytes(&mut self, bytes: Option<u64>) {
        self.download_bytes = bytes;
    }

    /// Whether a semantic search can run RIGHT NOW. Deliberately the STATUS and
    /// not the model directory: `Indexing` means the engine is up and merely
    /// busy, and the test double has no files on disk at all.
    pub fn searchable(&self) -> bool {
        self.enabled && matches!(self.status, RagStatus::Ready | RagStatus::Indexing)
    }

    /// The tag that goes into every staleness fingerprint (see [`fingerprint`]).
    pub fn tag(&self) -> &'static str {
        self.embedder.tag()
    }

    pub fn embedder(&self) -> Arc<dyn Embedder> {
        Arc::clone(&self.embedder)
    }

    /// Drops the loaded model. Called when the vault locks or closes: it holds
    /// no secrets, but there is no reason to keep half a gigabyte of weights
    /// resident behind a lock screen. The next search reloads it from the
    /// local cache — no network, whatever happened in between.
    ///
    /// It RETURNS AT ONCE, whatever a worker is doing with the engine (see
    /// [`Embedder::unload`]): this is called from the UI thread.
    pub fn unload(&mut self) {
        self.cancel_query();
        self.embedder.unload();
    }

    /// True while a worker of any kind is out.
    pub fn busy(&self) -> bool {
        self.model_rx.is_some() || self.index_rx.is_some() || self.query_rx.is_some()
    }

    /// True while an INDEX pass is out, its batch included until the vault has
    /// taken it — what a search waiting for the thread to be caught up asks.
    pub fn indexing(&self) -> bool {
        self.index_rx.is_some() || self.deferred.is_some()
    }

    /// True while a QUERY embedding is out. There is one slot for it (see
    /// [`RagState::start_query`]), and two search boxes that can want it: this
    /// is how the one that lost it finds out its answer will never come.
    pub fn query_out(&self) -> bool {
        self.query_rx.is_some()
    }

    /// Embeds a search on a worker and hands back the sequence number its
    /// answer will carry. A query already out is ABANDONED rather than waited
    /// for: the last thing typed is the search that matters, and an embed can
    /// take seconds on a model that is still cold.
    pub fn start_query(&mut self, query: String) -> u64 {
        self.query_seq = self.query_seq.wrapping_add(1);
        let seq = self.query_seq;
        let (tx, rx) = std::sync::mpsc::channel();
        self.query_rx = Some(rx);
        start_query_embed(self.embedder(), seq, query, tx);
        seq
    }

    /// Abandons whatever search is out — the bar closed, the vault locked, the
    /// query changed. The worker finishes into a dropped channel; its answer,
    /// which is about text the user can no longer see, is never ranked.
    pub fn cancel_query(&mut self) {
        self.query_seq = self.query_seq.wrapping_add(1);
        self.query_rx = None;
    }

    /// Starts the one-time model download (a background first-init — see
    /// [`start_model_download`]). Ignored while one is already running.
    pub fn start_download(&mut self) {
        if self.model_rx.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.model_rx = Some(rx);
        self.status = RagStatus::Downloading;
        // a fresh bar: the first sample is a tick away, and until it lands the
        // row says "starting", not "0 %"
        self.download_bytes = None;
        start_model_download(self.embedder(), tx);
    }

    /// Starts an embedding pass over `pending`. Ignored when one is already out:
    /// the two scopes share the one engine and the one worker slot, and the next
    /// call (each finished batch makes one) picks up whatever is still stale.
    pub fn start_index(&mut self, vault: String, scope: IndexScope, pending: Vec<Pending>) {
        if pending.is_empty() || self.index_rx.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.index_rx = Some(rx);
        self.status = RagStatus::Indexing;
        start_index_pass(self.embedder(), vault, scope, pending, tx);
    }

    /// Stashes a batch the vault could not take this tick.
    pub fn defer(&mut self, vault: String, scope: IndexScope, vectors: Vec<Embedded>) {
        self.deferred = Some((vault, scope, vectors));
    }

    pub fn take_deferred(&mut self) -> Option<(String, IndexScope, Vec<Embedded>)> {
        self.deferred.take()
    }

    /// True while a finished batch is still waiting for the vault file.
    pub fn has_deferred(&self) -> bool {
        self.deferred.is_some()
    }

    /// Marks the end of a successful write (nothing left to do).
    pub fn mark_ready(&mut self) {
        if matches!(self.status, RagStatus::Indexing) {
            self.status = RagStatus::Ready;
        }
    }

    /// Drains both worker channels, at most one answer each per tick — exactly
    /// what `poll_sync` does. A worker that died without answering frees its
    /// slot rather than leaving "is one out?" wedged for the session.
    pub fn poll(&mut self) -> Vec<RagOutcome> {
        use std::sync::mpsc::TryRecvError;
        let mut out = Vec::new();
        // however the download ends — done, failed, or a worker that died
        // without a word — the bar goes with it: every arm below frees the
        // slot, and `download_bytes` is the bar's half of that same slot.
        match self.model_rx.as_ref().map(Receiver::try_recv) {
            Some(Ok(ModelEvent::Ready)) => {
                self.model_rx = None;
                self.download_bytes = None;
                // the toggle owns `enabled`: someone who turned it back OFF
                // while the download ran keeps the model on disk and the
                // feature off, rather than having it switch itself on.
                self.status = if self.enabled { RagStatus::Ready } else { RagStatus::Off };
                out.push(RagOutcome::ModelReady);
            }
            Some(Ok(ModelEvent::Failed(fault))) => {
                self.model_rx = None;
                self.download_bytes = None;
                self.status = RagStatus::Failed(fault.clone());
                out.push(RagOutcome::ModelFailed(fault));
            }
            Some(Err(TryRecvError::Disconnected)) => {
                self.model_rx = None;
                self.download_bytes = None;
                let fault = RagFault::ModelWorkerStopped;
                self.status = RagStatus::Failed(fault.clone());
                out.push(RagOutcome::ModelFailed(fault));
            }
            _ => {}
        }
        match self.index_rx.as_ref().map(Receiver::try_recv) {
            Some(Ok(IndexEvent::Done { vault, scope, vectors })) => {
                self.index_rx = None;
                out.push(RagOutcome::Indexed { vault, scope, vectors });
            }
            Some(Ok(IndexEvent::Failed { vault, scope, fault })) => {
                self.index_rx = None;
                self.status = RagStatus::Failed(fault.clone());
                out.push(RagOutcome::IndexFailed { vault, scope, fault });
            }
            Some(Err(TryRecvError::Disconnected)) => {
                self.index_rx = None;
                self.status = RagStatus::Ready;
            }
            _ => {}
        }
        match self.query_rx.as_ref().map(Receiver::try_recv) {
            Some(Ok(QueryEvent::Done { seq, vec })) => {
                self.query_rx = None;
                out.push(RagOutcome::Query { seq, vec });
            }
            Some(Ok(QueryEvent::Failed { seq, fault })) => {
                self.query_rx = None;
                // an engine that cannot embed a search cannot embed a passage
                // either: say so once here, and every later search falls back
                // to the literal one with this reason attached.
                self.status = RagStatus::Failed(fault.clone());
                out.push(RagOutcome::QueryFailed { seq, fault });
            }
            Some(Err(TryRecvError::Disconnected)) => {
                // a worker that died without answering: free the slot, and let
                // the search that is waiting fall back instead of hanging.
                self.query_rx = None;
                let seq = self.query_seq;
                out.push(RagOutcome::QueryFailed { seq, fault: RagFault::SearchWorkerStopped });
            }
            _ => {}
        }
        out
    }
}

// ---- the test double -------------------------------------------------------

/// A deterministic stand-in for the on-device model: the vector is a hash of
/// the text it is HANDED — the E5 prefix included — so a test can prove the
/// prefixes are applied without a model, a network or a display.
pub struct MockEmbedder;

impl MockEmbedder {
    /// The vector of a literal string, with NO prefix added. `embed_passages`
    /// and `embed_query` are exactly this over the prefixed text, which is what
    /// makes the prefix observable from a test.
    pub fn vector_of(text: &str) -> Vec<f32> {
        let mut bytes = vec![0u8; EMBED_DIM * 4];
        let mut hasher = blake3::Hasher::new();
        hasher.update(text.as_bytes());
        hasher.finalize_xof().fill(&mut bytes);
        let raw: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|c| {
                let n = u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
                (n as f64 / u32::MAX as f64 * 2.0 - 1.0) as f32
            })
            .collect();
        l2_normalize(raw)
    }
}

impl Embedder for MockEmbedder {
    fn tag(&self) -> &'static str {
        MOCK_TAG
    }

    fn embed_passages(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|t| MockEmbedder::vector_of(&format!("{PASSAGE_PREFIX}{t}")))
            .collect())
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        Ok(MockEmbedder::vector_of(&format!("{QUERY_PREFIX}{text}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memo::{MemoImage, MemoMessage};

    fn message(id: &str, text: &str) -> MemoMessage {
        let mut m = MemoMessage::new(text, None);
        m.id = id.to_string();
        m
    }

    #[test]
    fn the_e5_prefixes_are_applied_and_are_not_the_same_one() {
        // E5 is asymmetric: stored text is a "passage", a search is a "query".
        // Using one prefix for both (or neither) is the classic way to ship a
        // semantic search that quietly retrieves worse than grep.
        let e = MockEmbedder;
        let passage = &e.embed_passages(&["le chiavi sono sotto il tappeto".into()]).unwrap()[0];
        let query = e.embed_query("le chiavi sono sotto il tappeto").unwrap();
        assert_eq!(*passage, MockEmbedder::vector_of("passage: le chiavi sono sotto il tappeto"));
        assert_eq!(query, MockEmbedder::vector_of("query: le chiavi sono sotto il tappeto"));
        assert_ne!(*passage, query, "the same text embeds differently on the two sides");
        // …and the prefixes are the documented E5 ones, not something invented
        assert_eq!((PASSAGE_PREFIX, QUERY_PREFIX), ("passage: ", "query: "));
    }

    #[test]
    fn every_vector_leaves_normalized_so_similarity_is_a_dot_product() {
        let e = MockEmbedder;
        let texts: Vec<String> = ["primo", "secondo", ""].iter().map(|s| s.to_string()).collect();
        for v in e.embed_passages(&texts).unwrap() {
            assert_eq!(v.len(), EMBED_DIM);
            let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            assert!((norm - 1.0).abs() < 1e-4, "not unit length: {norm}");
        }
        let q = e.embed_query("primo").unwrap();
        let norm = q.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4);
        // a vector with nothing to scale is left alone rather than made NaN
        assert_eq!(l2_normalize(vec![0.0, 0.0]), vec![0.0, 0.0]);
        assert_eq!(l2_normalize(vec![3.0, 4.0]), vec![0.6, 0.8]);
    }

    #[test]
    fn semantic_search_ranks_by_dot_product_and_stops_at_k() {
        // hand-built vectors: the ordering is arithmetic, not model behaviour
        let index = vec![
            ("far".to_string(), vec![0.0, 1.0, 0.0]),
            ("near".to_string(), vec![1.0, 0.0, 0.0]),
            ("mid".to_string(), vec![0.7071, 0.7071, 0.0]),
            ("away".to_string(), vec![-1.0, 0.0, 0.0]),
        ];
        let hits = search_semantic(&index, &[1.0, 0.0, 0.0], 3);
        assert_eq!(hits.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(), ["near", "mid", "far"]);
        assert!((hits[0].score - 1.0).abs() < 1e-4);
        assert!(hits[0].score > hits[1].score && hits[1].score > hits[2].score);
        // k caps the list; k = 0 and an empty query are simply no results
        assert_eq!(search_semantic(&index, &[1.0, 0.0, 0.0], 1).len(), 1);
        assert!(search_semantic(&index, &[1.0, 0.0, 0.0], 0).is_empty());
        assert!(search_semantic(&index, &[], 3).is_empty());
        // a vector from another model is skipped, not compared across spaces
        let mixed = vec![
            ("wrong-dims".to_string(), vec![1.0, 0.0]),
            ("right".to_string(), vec![0.1, 0.0, 0.0]),
        ];
        let hits = search_semantic(&mixed, &[1.0, 0.0, 0.0], 5);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "right");
        // ties keep index order, so the same query twice gives the same list
        let tied = vec![
            ("a".to_string(), vec![1.0, 0.0]),
            ("b".to_string(), vec![1.0, 0.0]),
        ];
        let hits = search_semantic(&tied, &[1.0, 0.0], 2);
        assert_eq!(hits.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(), ["a", "b"]);
    }

    #[test]
    fn literal_search_is_accent_blind_and_ranks_the_better_match_first() {
        let items: Vec<(String, String)> = vec![
            ("1".into(), "Ho pagato il caffè al bar".into()),
            ("2".into(), "una lunga premessa che non c'entra nulla e poi CAFFE".into()),
            ("3".into(), "il tè è finito".into()),
        ];
        // "caffe" finds "caffè" and "CAFFE": case and accents both fold away
        let hits = search_literal(&items, "caffe");
        assert_eq!(hits.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(), ["1", "2"]);
        assert!(hits[0].score > hits[1].score, "the earlier, word-start match wins");
        // …and it works the other way round too (an accented query, plain text)
        assert_eq!(search_literal(&items, "CAFFÈ").len(), 2);
        // a query that isn't there, and the empty query, have no hits
        assert!(search_literal(&items, "grappa").is_empty());
        assert!(search_literal(&items, "   ").is_empty());
        // the match must be CONTIGUOUS — this is the literal search, so a
        // scattered subsequence ("c…a…f…f…e") is not a hit
        assert!(search_literal(&[("x".into(), "c a f f e".into())], "caffe").is_empty());
    }

    #[test]
    fn a_literal_hit_carries_the_words_around_the_match() {
        let long = format!("{}il codice della cassaforte è 4821{}", "x ".repeat(60), " y".repeat(60));
        let items = vec![("1".to_string(), long)];
        let hit = &search_literal(&items, "cassaforte")[0];
        assert!(hit.snippet.contains("cassaforte"), "the match is in the snippet");
        assert!(hit.snippet.chars().count() <= SNIPPET_CHARS + 2, "…and it is bounded");
        assert!(hit.snippet.starts_with('…') && hit.snippet.ends_with('…'), "cut on both sides");
        // a short text is shown whole, with newlines flattened onto one row
        let items = vec![("1".to_string(), "prima riga\n\n  seconda riga".to_string())];
        assert_eq!(search_literal(&items, "seconda")[0].snippet, "prima riga seconda riga");
    }

    #[test]
    fn only_missing_or_stale_messages_are_queued_for_embedding() {
        let e = MockEmbedder;
        let mut messages = vec![message("a", "le chiavi"), message("b", "la cassaforte")];
        // nothing embedded yet: both are pending
        let pending = memo_pending(&messages, e.tag());
        assert_eq!(pending.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        // …attach what the worker would have produced
        let texts: Vec<String> = pending.iter().map(|p| p.text.clone()).collect();
        for (p, v) in pending.iter().zip(e.embed_passages(&texts).unwrap()) {
            let msg = messages.iter_mut().find(|m| m.id == p.id).unwrap();
            msg.vfp = Some(fingerprint(e.tag(), &p.text));
            msg.vec = Some(v);
        }
        assert!(memo_pending(&messages, e.tag()).is_empty(), "an unchanged thread costs nothing");
        // editing the text invalidates exactly that message
        messages[0].text = "le chiavi sono sotto il tappeto".to_string();
        let pending = memo_pending(&messages, e.tag());
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, "a");
        // …and so does changing the model: vectors from two spaces never mix
        assert_eq!(memo_pending(&messages, "some-other-model").len(), 2);
        // a vector of the wrong width is stale whatever its fingerprint says
        messages[1].vec = Some(vec![0.0; 7]);
        assert!(memo_pending(&messages, e.tag()).iter().any(|p| p.id == "b"));
    }

    #[test]
    fn a_photo_contributes_its_caption_and_never_its_bytes() {
        let b64 = "iVBORw0KGgoAAAANSUhEUg==";
        let photo = MemoImage {
            b64: b64.to_string(),
            mime: "image/png".to_string(),
            bytes: 40213,
            caption: Some("lo scontrino del notaio".to_string()),
            extra: serde_json::Map::new(),
        };
        let msg = MemoMessage::new("", Some(photo));
        let text = memo_embed_text(&msg);
        assert_eq!(text, "lo scontrino del notaio");
        assert!(!text.contains(b64), "image bytes are never embedded");
        // a photo with no caption and no words has nothing to embed at all
        let bare = MemoMessage::new("", None);
        assert!(memo_embed_text(&bare).is_empty());
        assert!(memo_pending(std::slice::from_ref(&bare), MOCK_TAG).is_empty());
    }

    #[test]
    fn only_well_formed_vectors_enter_the_search_index() {
        let mut messages = vec![message("a", "uno"), message("b", "due"), message("c", "tre")];
        messages[0].vec = Some(vec![0.0; EMBED_DIM]);
        messages[1].vec = Some(vec![0.0; 3]); // another model's width
        // c has none yet — it is simply not findable semantically for now
        let index = memo_index(&messages);
        assert_eq!(index.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["a"]);
    }

    #[test]
    fn the_fingerprint_separates_the_model_from_the_text() {
        assert_ne!(fingerprint("ab", "c"), fingerprint("a", "bc"), "the separator earns its byte");
        assert_eq!(fingerprint(MOCK_TAG, "ciao"), fingerprint(MOCK_TAG, "ciao"));
        assert_ne!(fingerprint(MOCK_TAG, "ciao"), fingerprint(LOCAL_TAG, "ciao"));
    }

    #[test]
    fn the_index_worker_answers_with_one_vector_per_message() {
        let (tx, rx) = std::sync::mpsc::channel();
        let pending = vec![
            Pending { id: "a".into(), text: "le chiavi".into() },
            Pending { id: "b".into(), text: "la cassaforte".into() },
        ];
        start_index_pass(Arc::new(MockEmbedder), "bruno".into(), IndexScope::Memo, pending, tx);
        let event = rx.recv_timeout(std::time::Duration::from_secs(5)).expect("the worker answered");
        let IndexEvent::Done { vault, scope, vectors } = event else { panic!("the batch failed") };
        assert_eq!(vault, "bruno", "the answer says which vault it belongs to");
        assert_eq!(scope, IndexScope::Memo, "…and which index it is for");
        assert_eq!(vectors.len(), 2);
        assert_eq!(vectors[0].id, "a");
        assert_eq!(vectors[0].vec, MockEmbedder::vector_of("passage: le chiavi"));
        assert_eq!(vectors[0].fp, fingerprint(MOCK_TAG, "le chiavi"));
    }

    #[test]
    fn a_query_embedding_comes_back_tagged_with_the_search_it_belongs_to() {
        let mut rag = RagState::new(std::env::temp_dir().join("datashield-rag-not-a-real-dir"));
        rag.use_mock();
        let seq = rag.start_query("dove sono le chiavi".to_string());
        assert!(rag.busy(), "the embed is on a worker, not on the caller");
        let outcome = loop {
            if let Some(out) = rag.poll().into_iter().next() {
                break out;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        };
        let RagOutcome::Query { seq: answered, vec } = outcome else { panic!("not a query answer") };
        assert_eq!(answered, seq, "the answer says which search it is for");
        assert_eq!(vec, MockEmbedder::vector_of("query: dove sono le chiavi"));
        assert!(!rag.busy(), "…and the slot is free again");

        // a search that was abandoned answers nothing at all: the bar it
        // belonged to is gone, and its query is text the user can no longer see
        let first = rag.start_query("le chiavi".to_string());
        rag.cancel_query();
        assert!(!rag.busy());
        let second = rag.start_query("la cassaforte".to_string());
        assert_ne!(second, first, "…and its seq can never be answered by accident");
        let outcome = loop {
            if let Some(out) = rag.poll().into_iter().next() {
                break out;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        };
        let RagOutcome::Query { seq: answered, .. } = outcome else { panic!("not a query answer") };
        assert_eq!(answered, second);
    }

    #[test]
    fn a_semantic_hit_previews_the_message_the_same_width_a_literal_one_does() {
        // the two kinds of hit sit in one list, so they are cut the same way
        let long = "x ".repeat(80);
        assert!(preview(&long).chars().count() <= SNIPPET_CHARS + 1);
        assert!(preview(&long).ends_with('…'), "cut at the end, never at the start");
        assert_eq!(preview("prima riga\n\n  seconda riga"), "prima riga seconda riga");
        assert_eq!(preview(""), "");
    }

    #[test]
    fn unloading_the_model_never_waits_for_whoever_is_holding_it() {
        // `unload` runs on the UI THREAD — Ctrl+L, closing the vault, the
        // settings toggle — while a worker (or an agent-API request) can be
        // holding the model across a cold session build, which is seconds and,
        // with the cache gone, a download. Waiting for it would leave the last
        // frame of the open vault painted on a terminal that takes no keys.
        let e = LocalEmbedder::new(std::env::temp_dir().join("datashield-rag-not-a-real-dir"));
        let held = e.model.lock().expect("the lock is free");
        let start = std::time::Instant::now();
        e.unload();
        let waited = start.elapsed();
        drop(held);
        assert!(waited < std::time::Duration::from_millis(500), "unload waited {waited:?}");
        // …and the free really happens, once the holder lets go
        for _ in 0..200 {
            if e.model.lock().map(|g| g.is_none()).unwrap_or(false) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("the deferred unload never freed the model");
    }

    #[test]
    fn a_failure_this_crate_wrote_is_localized_like_every_other_line() {
        // `RagStatus::Failed` is shown as "ricerca semantica non disponibile:
        // {why}" — a raw English {why} would be half a sentence.
        for fault in [
            RagFault::ModelWorkerStopped,
            RagFault::SearchWorkerStopped,
            RagFault::VectorCount,
            RagFault::NoVector,
            RagFault::ModelLock,
        ] {
            let it = fault.message(Lang::It);
            let en = fault.message(Lang::En);
            assert_ne!(it, en, "{fault:?} says the same in both languages");
            assert_eq!(en, fault.to_string(), "…and English is the Display text");
            assert!(RagStatus::Failed(fault.clone()).label(Lang::It).contains(&it));
        }
        // the ENGINE's own words are not ours to translate: they pass through
        let engine = RagFault::Engine("model.onnx: no such file".to_string());
        assert_eq!(engine.message(Lang::It), "model.onnx: no such file");
        assert!(RagStatus::Failed(engine).label(Lang::It).starts_with("ricerca semantica"));
    }

    // ---- the one-time download's progress bar ------------------------------

    #[test]
    fn dir_bytes_walks_the_whole_cache_and_forgives_what_is_not_there() {
        let dir = tempfile::TempDir::new().unwrap();
        assert_eq!(dir_bytes(&dir.path().join("models")), 0, "a cache nobody made yet weighs 0");
        assert_eq!(dir_bytes(dir.path()), 0, "…and so does an empty one");
        // the hf-hub layout, nested exactly as fastembed leaves it
        let repo = dir.path().join(format!("models--{}", MODEL_REPO.replace('/', "--")));
        std::fs::create_dir_all(repo.join("blobs")).unwrap();
        std::fs::create_dir_all(repo.join("snapshots/abc123/onnx")).unwrap();
        std::fs::create_dir_all(repo.join("refs")).unwrap();
        std::fs::write(repo.join("blobs/aaa"), vec![0u8; 1000]).unwrap();
        // a download IN FLIGHT is a `.part` growing inside blobs: it counts,
        // otherwise the bar would sit at 0 % for the whole of the big file
        std::fs::write(repo.join("blobs/bbb.part"), vec![0u8; 250]).unwrap();
        std::fs::write(repo.join("blobs/aaa.lock"), []).unwrap();
        std::fs::write(repo.join("snapshots/abc123/onnx/model.onnx"), vec![0u8; 7]).unwrap();
        std::fs::write(repo.join("refs/main"), b"deadbeef").unwrap();
        assert_eq!(dir_bytes(dir.path()), 1000 + 250 + 7 + 8);
        // the snapshot tree is SYMLINKS into blobs: following them would count
        // the weights twice and put the bar past the end on the first sample
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                repo.join("blobs/aaa"),
                repo.join("snapshots/abc123/tokenizer.json"),
            )
            .unwrap();
            assert_eq!(dir_bytes(dir.path()), 1265, "a link to a blob is not more bytes");
        }
    }

    #[test]
    fn the_download_percentage_clamps_short_of_a_hundred() {
        assert_eq!(download_pct(0), 0);
        // floored, never rounded up: the bar may not run ahead of the bytes
        assert_eq!(download_pct(EXPECTED_MODEL_BYTES / 2), 49);
        assert_eq!(download_pct(EXPECTED_MODEL_BYTES / 100 * 43), 42);
        // a finished download still stops at 99: bytes on disk are not a loaded
        // session, and only `ModelEvent::Ready` ends the bar
        assert_eq!(download_pct(EXPECTED_MODEL_BYTES), 99);
        // …and a revision heavier than the one measured overshoots harmlessly
        assert_eq!(download_pct(EXPECTED_MODEL_BYTES * 3), 99);
        assert_eq!(download_pct(u64::MAX), 99, "no overflow on a nonsense number");
        // the counter beside it
        assert_eq!(mib(0), 0);
        assert_eq!(mib(7 * 1024 * 1024 + 5), 7);
        assert_eq!(mib(EXPECTED_MODEL_BYTES), 464);
    }

    #[test]
    fn the_download_bar_appears_with_the_download_and_leaves_with_it() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut rag = RagState::new(dir.path().to_path_buf());
        assert!(!rag.downloading() && rag.download_bytes().is_none());
        // nothing is out: the sampler is a no-op, so an idle tick pays nothing
        std::fs::write(dir.path().join("junk"), vec![0u8; 64]).unwrap();
        rag.sample_download();
        assert_eq!(rag.download_bytes(), None);
        // the state the status row draws from — no network, no worker
        rag.status = RagStatus::Downloading;
        assert!(rag.downloading());
        assert_eq!(rag.download_bytes(), None, "indeterminate until the first sample lands");
        std::fs::write(dir.path().join("blob.part"), vec![0u8; 4096]).unwrap();
        rag.sample_download();
        assert_eq!(rag.download_bytes(), Some(4096 + 64));
        // Done: the status flows on and the bar is gone with it
        let (tx, rx) = std::sync::mpsc::channel();
        rag.model_rx = Some(rx);
        tx.send(ModelEvent::Ready).unwrap();
        assert!(matches!(rag.poll().as_slice(), [RagOutcome::ModelReady]));
        assert!(!rag.downloading(), "{:?}", rag.status);
        assert_eq!(rag.download_bytes(), None, "a finished download draws no bar");
        // …and a failure takes it away just the same
        rag.status = RagStatus::Downloading;
        rag.sample_download();
        assert!(rag.download_bytes().is_some());
        let (tx, rx) = std::sync::mpsc::channel();
        rag.model_rx = Some(rx);
        tx.send(ModelEvent::Failed(RagFault::Engine("no route to host".into()))).unwrap();
        assert!(matches!(rag.poll().as_slice(), [RagOutcome::ModelFailed(_)]));
        assert_eq!(rag.download_bytes(), None);
    }

    #[test]
    fn only_a_working_engine_is_searchable() {
        // the model DIRECTORY is not the question — `Indexing` is a live engine
        // that happens to be busy, and the test double has no files at all.
        let mut rag = RagState::new(std::env::temp_dir().join("datashield-rag-not-a-real-dir"));
        assert!(!rag.searchable(), "off out of the box, with no model on this machine");
        rag.use_mock();
        assert!(rag.searchable());
        for status in [RagStatus::Indexing, RagStatus::Ready] {
            rag.status = status;
            assert!(rag.searchable());
        }
        for status in [
            RagStatus::Absent,
            RagStatus::Downloading,
            RagStatus::Failed(RagFault::Engine("nope".into())),
        ] {
            rag.status = status.clone();
            assert!(!rag.searchable(), "{status:?}");
        }
        rag.status = RagStatus::Ready;
        rag.set_enabled(false);
        assert!(!rag.searchable(), "the toggle is the master switch");
    }

    // ---- the item index ---------------------------------------------------

    fn field(key: &str, value: &str, secret: bool) -> ItemField {
        ItemField {
            key: key.to_string(),
            ftype: crate::model::FieldType::Text,
            label: None,
            value: crate::model::SecretString::new(value),
            secret,
            extra: Map::new(),
        }
    }

    fn login(label: &str, password: &str) -> Item {
        let mut item = Item::new("login", label);
        item.fields = vec![
            field("username", "bruno@example.com", false),
            field("password", password, true),
            field("url", "acme.example.com", false),
            field("notes", "il piano tariffario è annuale", false),
        ];
        item
    }

    #[test]
    fn a_secret_value_can_never_reach_the_index() {
        // AN EMBEDDING IS NOT A HASH: the text is partially recoverable from
        // the vector, so indexing a password would store it a second time — and
        // with the API on, serve it. The filter is structural: the builder is
        // handed `None` for a secret value and has nothing to leak.
        let item = login("Acme", "hunter2-CORRECT-HORSE");
        let seen = indexable_fields(&item.fields);
        assert_eq!(seen.len(), 4, "every field is offered…");
        assert_eq!(seen[1].key, "password");
        assert_eq!(seen[1].value, None, "…but a secret one arrives without its value");
        assert_eq!(seen[0].value, Some("bruno@example.com"));

        let text = item_embed_text(&item, "Login");
        assert!(!text.contains("hunter2"), "the password is not in the indexed text: {text:?}");
        assert!(!text.contains("CORRECT-HORSE"));
        // …and everything that is NOT secret is: the label, the localized
        // category, the field names, the values, the website.
        for part in [
            "Acme",
            "Login",
            "username",
            "bruno@example.com",
            "password", // the NAME of the secret field is not the secret
            "url",
            "acme.example.com",
            "il piano tariffario è annuale",
        ] {
            assert!(text.contains(part), "{part:?} missing from {text:?}");
        }
        // a stored picture is `secret` too (its base64 is not text), so the
        // filter drops it exactly like a password — only its label survives.
        let mut with_photo = Item::new("id_document", "Passaporto");
        with_photo.fields = vec![field("photo_front", "iVBORw0KGgoAAAANSUhEUg==", true)];
        let text = item_embed_text(&with_photo, "Documento");
        assert!(text.contains("photo_front") && text.contains("Passaporto"));
        assert!(!text.contains("iVBORw0KGgo"), "image bytes are never embedded");
    }

    #[test]
    fn a_picture_a_foreign_vault_forgot_to_flag_is_still_never_indexed() {
        // `secret` is `#[serde(default)]` (missing ⇒ false) and no load path
        // normalizes it, so a picture written by another DCS-1 implementation
        // can arrive unflagged. Indexing it would put a megabyte of base64
        // through the tokenizer, into the stored vectors, and out of the agent
        // API as a snippet — the same defence `ui::item_subtitle` keeps.
        let raw = serde_json::json!({
            "key": "photo_front",
            "type": "image",
            "value": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJ"
        });
        let field: ItemField = serde_json::from_value(raw).expect("a field this build can read");
        assert!(!field.secret, "the foreign writer left it unflagged");
        assert!(crate::image_field::is_image(&field), "…but it is still a picture");
        assert_eq!(indexable_fields(std::slice::from_ref(&field))[0].value, None);

        let mut item = Item::new("id_document", "Passaporto");
        item.fields = vec![field];
        let text = item_embed_text(&item, "Documento");
        assert!(text.contains("Passaporto") && text.contains("photo_front"), "{text:?}");
        assert!(!text.contains("iVBORw0KGgo"), "the picture reached the index: {text:?}");
    }

    #[test]
    fn an_items_text_uses_the_label_a_field_carries_and_skips_empty_ones() {
        let mut item = Item::new("login", "  Acme  ");
        item.fields = vec![field("f1", "", false), field("f2", "value", false)];
        item.fields[1].label = Some("Codice cliente".to_string());
        let text = item_embed_text(&item, " Login ");
        assert!(text.starts_with("Acme\nLogin\n"), "trimmed, label then category: {text:?}");
        assert!(text.contains("Codice cliente"), "a custom label wins over the key");
        assert!(!text.contains("f2"), "…and replaces it");
        assert!(text.contains("f1"), "an empty field still contributes its name");
        // an item with nothing at all in it is not in the index
        let bare = Item::new("login", "");
        assert!(item_embed_text(&bare, "").is_empty());
        assert!(item_texts(std::slice::from_ref(&bare), |_| String::new()).is_empty());
    }

    #[test]
    fn only_missing_or_stale_items_are_queued_and_the_store_round_trips() {
        let e = MockEmbedder;
        let items = vec![login("Acme", "pw1"), login("Sentry", "pw2")];
        let texts = item_texts(&items, |id| id.to_uppercase());
        let mut extra: Map<String, Value> = Map::new();

        // nothing stored yet: both are pending
        let stored = read_item_vectors(&extra);
        let pending = items_pending(&texts, e.tag(), &stored);
        assert_eq!(pending.len(), 2);
        // …attach what the worker would have produced
        let raw: Vec<String> = pending.iter().map(|p| p.text.clone()).collect();
        let vectors: Vec<Embedded> = pending
            .iter()
            .zip(e.embed_passages(&raw).unwrap())
            .map(|(p, vec)| Embedded { id: p.id.clone(), fp: fingerprint(e.tag(), &p.text), vec })
            .collect();
        let live: Vec<String> = items.iter().map(|i| i.id.clone()).collect();
        set_item_vectors(&mut extra, e.tag(), &vectors, &live);

        let stored = read_item_vectors(&extra);
        assert_eq!(stored.model, MOCK_TAG);
        assert!(items_pending(&texts, e.tag(), &stored).is_empty(), "an unchanged vault costs nothing");
        assert_eq!(item_index(&texts, &stored).len(), 2, "both are searchable");
        // the vector really is the PASSAGE side of the model, over the text the
        // filter built (and not over the password inside the item)
        assert_eq!(
            stored.items[&items[0].id].1,
            MockEmbedder::vector_of(&format!("{PASSAGE_PREFIX}{}", texts[0].1))
        );

        // editing an item invalidates exactly that one
        let mut edited = items;
        edited[0].label = "Acme Corp".to_string();
        let texts = item_texts(&edited, |id| id.to_uppercase());
        let pending = items_pending(&texts, e.tag(), &stored);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, edited[0].id);
        // …and so does changing the model: two embedding spaces never mix
        assert_eq!(items_pending(&texts, "some-other-model", &stored).len(), 2);
    }

    #[test]
    fn a_deleted_items_vector_is_pruned_and_a_refused_save_rolls_the_block_back() {
        let e = MockEmbedder;
        let items = vec![login("Acme", "pw1"), login("Sentry", "pw2")];
        let live: Vec<String> = items.iter().map(|i| i.id.clone()).collect();
        let vectors: Vec<Embedded> = live
            .iter()
            .map(|id| Embedded {
                id: id.clone(),
                fp: fingerprint(e.tag(), "x"),
                vec: vec![0.0; EMBED_DIM],
            })
            .collect();
        let mut extra: Map<String, Value> = Map::new();
        // a foreign member of the content map is none of our business
        extra.insert("x_other".to_string(), serde_json::json!({ "keep": "me" }));
        let before = extra.clone();
        set_item_vectors(&mut extra, e.tag(), &vectors, &live);
        assert_eq!(read_item_vectors(&extra).items.len(), 2);

        // the second item is deleted: the next pass prunes its vector
        let slot = set_item_vectors(&mut extra, e.tag(), &[], &live[..1]);
        let stored = read_item_vectors(&extra);
        assert_eq!(stored.items.len(), 1);
        assert!(stored.items.contains_key(&live[0]));
        // …and a save that failed puts the block back byte for byte
        restore_item_vectors(&mut extra, slot);
        assert_eq!(read_item_vectors(&extra).items.len(), 2, "the refused write left no trace");

        // rolling the FIRST write back removes the member entirely, so the
        // content fingerprint is the one it had before
        let slot = set_item_vectors(&mut Map::new(), e.tag(), &vectors, &live);
        drop(slot);
        let mut fresh = before.clone();
        let slot = set_item_vectors(&mut fresh, e.tag(), &vectors, &live);
        restore_item_vectors(&mut fresh, slot);
        assert_eq!(fresh, before, "x_rag is gone, x_other is untouched");
    }

    #[test]
    fn an_unreadable_stored_block_is_skipped_rather_than_fatal() {
        // the flatten map is somebody else's to write too: nothing in here may
        // panic on a shape this build did not expect.
        for junk in [
            serde_json::json!("not an object"),
            serde_json::json!({ "items": 7 }),
            serde_json::json!({ "model": 3, "items": { "a": { "fp": "x" } } }),
            serde_json::json!({ "model": "m", "items": { "a": { "fp": "x", "vec": [1.0, null] } } }),
        ] {
            let mut extra: Map<String, Value> = Map::new();
            extra.insert(RAG_KEY.to_string(), junk);
            let stored = read_item_vectors(&extra);
            assert!(stored.items.is_empty());
            assert!(item_index(&[("a".into(), "text".into())], &stored).is_empty());
        }
    }

    /// The one live test: it downloads ~490 MB the first time and needs the
    /// network, so it never runs in the suite. `cargo test -- --ignored
    /// the_real_model_understands_italian` when you want to prove the on-device
    /// engine is really doing semantic work (it does — that is the whole point
    /// of shipping a multilingual model to a user who writes Italian).
    #[test]
    #[ignore]
    fn the_real_model_understands_italian() {
        // a fixed cache outside the app's data dir: repeat runs reuse the
        // download and no test ever touches the user's real vault directory.
        let dir = std::env::temp_dir().join("datashield-rag-live");
        let e = LocalEmbedder::new(dir);
        let passages: Vec<String> = vec![
            "le chiavi di casa sono sotto il tappeto".to_string(),
            "la ricetta della torta di mele di mia nonna".to_string(),
        ];
        let vectors = e.embed_passages(&passages).expect("the model embedded the passages");
        assert_eq!(vectors[0].len(), EMBED_DIM);
        let query = e.embed_query("dove ho nascosto le chiavi?").expect("the query embedded");
        let similar: f32 = vectors[0].iter().zip(&query).map(|(a, b)| a * b).sum();
        let different: f32 = vectors[1].iter().zip(&query).map(|(a, b)| a * b).sum();
        assert!(
            similar > different,
            "the keys sentence must beat the cake one: {similar} vs {different}"
        );
        // …and the model really is on this machine now
        assert!(is_model_present(&std::env::temp_dir().join("datashield-rag-live")));
    }
}
