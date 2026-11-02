//! The localhost agent API: a search endpoint an agent on THIS machine can ask,
//! so "find the note about the notary" does not require a human to scroll the
//! Memorandum — and, when the user has said so, a door an agent that already
//! KNOWS a vault's master password can open that vault through.
//!
//! Everything about this module is a refusal to widen the trust boundary:
//!
//!   * IT BINDS THE LOOPBACK, EXPLICITLY. `("127.0.0.1", port)` — never
//!     `0.0.0.0`, never a hostname that could resolve to a routable address.
//!   * THE ADMIN SIDE NEVER SEES A KEY. The open TUI vault reaches this thread
//!     as an [`ApiIndex`] SNAPSHOT — ids, timestamps, labels, non-secret text
//!     and vectors — and as nothing else. The `Vault` and its derived key stay
//!     on the UI thread. It exists only while a vault is open: locking, closing
//!     and quitting all empty it.
//!   * EVERY ENDPOINT BUT `/health` NEEDS A TOKEN — 32 random bytes, hex,
//!     compared in constant time against EVERY candidate (the admin token and
//!     each live session's), with no early exit. A failure answers `401` with
//!     no detail: which half of the guess was right is not information this
//!     server hands out.
//!   * A SESSION IS THE SCOPE. `POST /unlock` mints a token that stands for ONE
//!     vault, and no later request names a vault at all — there is nowhere in
//!     any body to put a name, so a token can never be pointed at a vault it
//!     was not minted for.
//!   * SECRET VALUES CROSS AT EXACTLY ONE ENDPOINT. `/search` answers out of
//!     text that was already non-secret inside the vault (see
//!     [`rag::indexable_fields`]); `/reveal` is the one place a stored secret
//!     leaves, it is off by default, and it says so on its own settings row.
//!   * NOTHING A SESSION HOLDS IS EVER WRITTEN. Key and content live in this
//!     process's memory, are wiped when the session dies (`/lock`, the idle
//!     TTL, a re-key of that vault, the server stopping, quitting), and never
//!     touch the disk.
//!   * A MASTER PASSWORD EXISTS ON ONE PATH ONLY: the `/unlock` request body,
//!     read into a `Zeroizing` buffer and parsed into a `Zeroizing` field —
//!     which wipes itself however the parse ends, not only when the whole
//!     request was built — and never logged, echoed or named in an answer.
//!
//! It is a BLOCKING server (`tiny_http`): an accept loop on its own thread that
//! also watches a stop flag, answering each request on a thread of its own so
//! no client can hold that loop — and with it the lock key — hostage (see
//! [`serve`]). The crate has no async runtime and this is not the place to
//! introduce one.

#[allow(unused_imports)]
use crate::content_ext::VaultContentExt;

use crate::crypto::KEY_LEN;
use crate::i18n::{self, Key, Lang};
use crate::model::{Item, VaultContent};
use crate::rag::{self, Embedder};
use crate::vault::{self, UnlockError, Vault};
use serde::Deserialize;
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, Zeroizing};

/// How long the request loop blocks before it re-reads the stop flag. Only a
/// belt for the [`tiny_http::Server::unblock`] braces, which wakes it at once.
const POLL: std::time::Duration = std::time::Duration::from_millis(200);

/// The most a request body may be. A local agent sends a sentence; anything
/// past this is a mistake or a memory bomb, and neither gets read.
const MAX_BODY: u64 = 64 * 1024;

/// How many hits a search returns when it doesn't say, and the most it may ask
/// for (a cap, so one request cannot make the app copy the whole index out).
const DEFAULT_K: usize = 8;
const MAX_K: usize = 50;

/// The most vaults an agent may hold open at once. Each live session is a
/// decrypted vault in this process's memory, so this is a ceiling on how much
/// plaintext one runaway agent can accumulate — and the 9th `/unlock` is
/// refused (409) rather than silently evicting somebody else's session.
const MAX_SESSIONS: usize = 8;

/// What the user has allowed, re-read on every request so a settings change
/// takes effect at once — including the one that takes something away. `Copy`
/// and tiny: it is pushed whole by [`ApiServer::set_policy`] rather than
/// merged, so there is no state here to drift out of step with the settings
/// page.
#[derive(Clone, Copy)]
pub struct ApiPolicy {
    /// `POST /unlock` is on offer (`settings.api_vault_unlock`).
    pub allow_unlock: bool,
    /// `POST /reveal` is on offer (`settings.api_reveal`).
    pub allow_reveal: bool,
    /// The `/login/*` endpoints are on offer (`settings.login_assist`) — the
    /// browser extension's door (see `docs/LOGIN-ASSIST.md`).
    pub allow_login: bool,
    /// Idle expiry of a session; `None` = it never expires by time alone.
    pub session_ttl: Option<Duration>,
    /// The interface language — the ONE thing the server borrows from the UI,
    /// so a session's item search matches on "carta di credito" exactly as the
    /// open vault's snapshot does (categories are indexed localized).
    pub lang: Lang,
}

impl Default for ApiPolicy {
    /// Everything the user has not asked for is off.
    fn default() -> ApiPolicy {
        ApiPolicy {
            allow_unlock: false,
            allow_reveal: false,
            allow_login: false,
            session_ttl: None,
            lang: Lang::default(),
        }
    }
}

/// One item as `/reveal` is allowed to answer it — SECRET VALUES INCLUDED.
///
/// This is the only type in the crate that carries a vault's secrets outside a
/// `Vault`/`SecretString`, and it exists so that fact is impossible to miss:
/// building one is the decision to hand a value over, and it happens in exactly
/// two places (the app filling [`ApiIndex::reveal`] for the open TUI vault, and
/// a session reading its own decrypted content). No `Debug`, no `Clone`, no
/// `Display` — the `SecretString` rules, on the type that broke them.
pub struct RevealField {
    pub key: String,
    pub label: Option<String>,
    /// The DCS-1 type token (`password`, `totp`, `image`, …).
    pub ftype: String,
    /// Wiped when this drops. Empty for an image field — see [`image_bytes`].
    ///
    /// [`image_bytes`]: RevealField::image_bytes
    pub value: Zeroizing<String>,
    pub secret: bool,
    /// `Some(n)` marks a picture: the base64 is NOT sent (a megabyte through a
    /// JSON body helps nobody), only how big it is.
    pub image_bytes: Option<u64>,
}

/// An item, as the two reveal paths both see it.
pub struct RevealItem {
    pub id: String,
    pub label: String,
    pub fields: Vec<RevealField>,
}

/// THE door a secret value leaves an [`Item`] through. Both `/reveal` paths go
/// via here, so what a revealed field looks like is decided once.
pub fn reveal_item(item: &Item) -> RevealItem {
    RevealItem {
        id: item.id.clone(),
        label: item.label.clone(),
        fields: item
            .fields
            .iter()
            .map(|f| {
                let image = crate::image_field::is_image(f).then(|| {
                    // the original file's size when the writer recorded it,
                    // else what is stored (base64, so ~4/3 of the file) —
                    // either way a NUMBER, never the bytes themselves.
                    f.extra
                        .get(crate::image_field::META_KEY)
                        .and_then(|m| m.get("bytes"))
                        .and_then(|b| b.as_u64())
                        .unwrap_or(f.value.as_str().len() as u64)
                });
                RevealField {
                    key: f.key.clone(),
                    label: f.label.clone(),
                    ftype: f.ftype.token().to_string(),
                    value: match image {
                        Some(_) => Zeroizing::new(String::new()),
                        None => Zeroizing::new(f.value.as_str().to_string()),
                    },
                    secret: f.secret,
                    image_bytes: image,
                }
            })
            .collect(),
    }
}

// ---- /login/*: what the browser extension may take ------------------------
//
// THE SECOND DOOR A SECRET LEAVES THROUGH, after `/reveal`, and fenced the
// same way: it does not exist unless `settings.login_assist` is on; it answers
// ONE item at a time, named by id, on an explicit choice the user made in the
// browser (`/login/fill`) or an explicit key press in the TUI (`/login/claim`);
// and `/login/match` — the one call the extension makes on its own — hands
// over identifiers and usernames and nothing else.

/// One item as the extension may see it: the website it belongs to, who it
/// logs in as, and — only on `/login/fill` or a claimed `/login/claim` — the
/// password. No `Debug`/`Clone`: it carries a secret (the `RevealField` rule).
pub struct LoginEntry {
    pub id: String,
    pub label: String,
    pub username: String,
    /// The item's website as a bare host (`accounts.google.com`), lowercased.
    pub domain: String,
    /// Wiped when this drops.
    pub password: Zeroizing<String>,
    /// The item's TOTP seed, when it has one (`/login/totp` turns it into the
    /// code of the moment; the seed itself never leaves).
    pub totp: Option<Zeroizing<String>>,
}

/// The entries of `items` that CAN log somebody in: a website, a user row and
/// a password row (resolved by [`crate::model::item_user_index`] and
/// [`crate::model::item_password_index`] — the same rows `u` and `c` copy).
/// The one builder both the admin snapshot and a session go through.
pub fn login_entries(items: &[Item]) -> Vec<LoginEntry> {
    items
        .iter()
        .filter_map(|item| {
            let domain = crate::favicon::domain_of(crate::model::item_website(item)?)?;
            let user = &item.fields[crate::model::item_user_index(item)?];
            let pass = &item.fields[crate::model::item_password_index(item)?];
            if pass.value.is_empty() {
                return None;
            }
            let totp = item
                .fields
                .iter()
                .find(|f| crate::clipboard::is_totp_field(f) && !f.value.is_empty())
                .map(|f| Zeroizing::new(f.value.as_str().to_string()));
            Some(LoginEntry {
                id: item.id.clone(),
                label: item.label.clone(),
                username: user.value.as_str().to_string(),
                domain,
                password: Zeroizing::new(pass.value.as_str().to_string()),
                totp,
            })
        })
        .collect()
}

/// Whether a page at `origin` (a URL, an origin or a bare host) belongs to
/// `domain`: the same host, or a subdomain of it — `accounts.google.com` is
/// `google.com`'s, `notgoogle.com` is not. Pure, so the one rule that decides
/// which site may be handed which credentials is unit-tested on its own.
pub fn domain_matches(origin: &str, domain: &str) -> bool {
    let Some(host) = crate::favicon::domain_of(origin) else { return false };
    let domain = domain.to_ascii_lowercase();
    // `www.` is how people write a site down, not a different site: an item
    // stored as `www.github.com` logs in at `github.com` too
    let domain = domain.strip_prefix("www.").unwrap_or(&domain);
    let within = |d: &str| host == d || host.strip_suffix(d).is_some_and(|head| head.ends_with('.'));
    within(domain) || sign_in_domains(domain).iter().any(|d| within(d))
}

/// Where a well-known service REALLY signs its users in, when that is not the
/// domain anybody writes down for it: Gmail's login page is on
/// `accounts.google.com`, Outlook's on `login.live.com`. A short, fixed table
/// of same-owner pairs — never a guess, and never a pattern a hostile site
/// could fit itself into.
fn sign_in_domains(domain: &str) -> &'static [&'static str] {
    match domain {
        "gmail.com" | "googlemail.com" | "youtube.com" | "mail.google.com" => &["google.com"],
        "outlook.com" | "hotmail.com" | "hotmail.it" | "live.com" | "live.it" | "msn.com"
        | "office.com" | "microsoft.com" | "xbox.com" => {
            &["live.com", "microsoftonline.com", "microsoft.com"]
        }
        "icloud.com" | "me.com" => &["apple.com", "icloud.com"],
        "x.com" => &["twitter.com"],
        "twitter.com" => &["x.com"],
        "instagram.com" | "messenger.com" => &["facebook.com"],
        "primevideo.com" | "audible.com" => &["amazon.com"],
        _ => &[],
    }
}

/// A login the TUI asked for with the `l` key: the extension claims it once,
/// by nonce, from a page of the item's own site, within [`PENDING_TTL`].
struct PendingLogin {
    nonce: Zeroizing<String>,
    item_id: String,
    expires: Instant,
}

/// The browser asked for a login while no vault was open (`/login/wake`):
/// the app brings itself forward and says so. Carries the page's host only.
pub struct WakeRequest {
    pub host: String,
}

/// A login the user made by hand in the browser and chose to keep
/// (`/login/save`). It waits here for the app's tick, which writes it into the
/// open vault through the ordinary mutation path — the server thread never
/// touches a `Vault`. The password is wiped when this drops.
pub struct SaveRequest {
    pub host: String,
    pub username: String,
    pub password: Zeroizing<String>,
}

/// How long a pending login stays claimable: long enough for a browser to
/// start, short enough that a key pressed and forgotten is not an open offer.
pub const PENDING_TTL: Duration = Duration::from_secs(120);

/// One searchable entry of the snapshot. Every member is either an identifier
/// or text that was ALREADY non-secret inside the vault — see
/// [`rag::indexable_fields`], which is the only door item text comes through.
#[derive(Clone, Default)]
pub struct ApiEntry {
    pub id: String,
    /// RFC 3339, for a Memorandum message. `None` for an item.
    pub ts: Option<String>,
    /// The item's display name. `None` for a message (it has no title).
    pub label: Option<String>,
    /// The item's localized category. `None` for a message.
    pub category: Option<String>,
    /// A photo's caption, when a message carries one.
    pub caption: Option<String>,
    /// The exact text that was indexed — words, captions, field names and
    /// NON-SECRET field values. Never a password, a PIN or a picture: a secret
    /// value never leaves [`rag::indexable_fields`], and neither does an image
    /// field's base64, whatever a foreign writer flagged it.
    pub text: String,
    /// Its embedding, when one has been computed.
    pub vec: Option<Vec<f32>>,
}

/// What the server thread is allowed to know. Rebuilt by the app on unlock and
/// after every mutation; emptied when the server stops.
#[derive(Default)]
pub struct ApiIndex {
    /// The Memorandum, when `api_expose_memo` is on (and empty otherwise).
    pub memo: Vec<ApiEntry>,
    /// The items, when `api_expose_items` is on (and empty otherwise).
    pub items: Vec<ApiEntry>,
    /// The engine a `rag` search embeds the query with, present only while the
    /// model really is loadable. `None` ⇒ a `rag` search answers 503
    /// `model-not-ready` instead of blocking on a download.
    pub embedder: Option<Arc<dyn Embedder>>,
    /// The open TUI vault's items WITH their secret values — filled by the app
    /// ONLY while `settings.api_reveal` is on, and empty the instant it is off.
    /// This is the one member of the snapshot that is not safe to serve
    /// wholesale: nothing but `/reveal` may read it, and `/reveal` only exists
    /// while the same toggle does.
    pub reveal: Vec<RevealItem>,
    /// The open TUI vault's logins — filled by the app ONLY while
    /// `settings.login_assist` is on, and read by nothing but `/login/*`.
    pub logins: Vec<LoginEntry>,
    /// Whether a vault is open in the TUI at all — what `/login/info` says,
    /// so the extension can tell "nothing for this site" from "locked".
    pub vault_open: bool,
}

impl ApiIndex {
    fn scope(&self, name: &str) -> Option<&[ApiEntry]> {
        match name {
            "memo" => Some(&self.memo),
            "items" => Some(&self.items),
            _ => None,
        }
    }
}

/// One vault an agent has open through `/unlock`.
///
/// It holds the derived key and the decrypted content IN MEMORY and nowhere
/// else: no file, no cache, no second process. Dropping it wipes the key
/// (`Zeroizing`) and every `SecretString` in the content — which is what
/// `/lock`, the idle TTL, a re-key of that vault and the server stopping all
/// do. No `Debug`/`Clone` for the same reason a `SecretString` has none.
struct Session {
    /// 32 random bytes as hex — the whole scope of this session.
    token: Zeroizing<String>,
    /// Which vault it opened. Compared by name when a re-key kills its sessions.
    vault: String,
    /// The key that opened it. Nothing here re-encrypts (a session is read-only
    /// by construction — there is no mutating endpoint), so this is kept for the
    /// `key_material` idiom's own sake: the session's authority is the key it
    /// derived, and it dies with it.
    key: Zeroizing<[u8; KEY_LEN]>,
    /// The decrypted DCS-1 document, moved out of the `Vault` that opened it.
    content: VaultContent,
    /// When it was minted, and when it was last used — the sliding TTL reads
    /// the second one, so an agent at work is never cut off mid-task.
    created: Instant,
    used: Instant,
}

impl Session {
    /// Whether `ttl` has passed since the last authorized request on it.
    fn expired(&self, ttl: Option<Duration>) -> bool {
        ttl.is_some_and(|ttl| self.used.elapsed() >= ttl)
    }
}

/// Drops every session `ttl` has run out on. Called before authentication on
/// every request (so an expired token cannot authenticate anything) and from
/// the app's tick, so a session that is simply never asked for again does not
/// sit decrypted in memory until the app quits.
fn reap(sessions: &mut Vec<Session>, ttl: Option<Duration>) {
    sessions.retain(|s| !s.expired(ttl));
}

/// A fresh session token: 32 random bytes as lowercase hex. Zeroizing because
/// it is a credential — it is copied through the same clipboard path (timed
/// wipe included) as a password.
pub fn new_token() -> Zeroizing<String> {
    let mut bytes = Zeroizing::new([0u8; 32]);
    // a token we could not randomize is not a token: getrandom failing here is
    // as fatal as it is unlikely, so the caller gets an EMPTY string and
    // `ApiServer::start` refuses to come up on one.
    if getrandom::getrandom(bytes.as_mut()).is_err() {
        return Zeroizing::new(String::new());
    }
    let mut out = String::with_capacity(64);
    for byte in bytes.iter() {
        out.push_str(&format!("{byte:02x}"));
    }
    Zeroizing::new(out)
}

/// Whether `presented` is the token, compared in constant time. Length is part
/// of the comparison (`subtle` answers false for a mismatch) so a short guess
/// cannot be told from a wrong one by how long the answer took.
fn token_matches(expected: &str, presented: &str) -> bool {
    !expected.is_empty() && bool::from(expected.as_bytes().ct_eq(presented.as_bytes()))
}

/// The running server: the thread, its stop flag, and the snapshot both sides
/// share. Dropping it stops the server — which is what makes "quit" safe even
/// on the paths that never call [`ApiServer::stop`] by name.
pub struct ApiServer {
    server: Arc<tiny_http::Server>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
    /// What was ASKED for — 0 means "any free port". Kept apart from [`port`]
    /// so "did the setting change?" compares like with like: an OS-assigned
    /// port never equals the 0 that was asked for, and comparing those would
    /// rebind the socket on every refresh.
    requested: u16,
    port: u16,
    token: Arc<Zeroizing<String>>,
    index: Arc<RwLock<ApiIndex>>,
    /// Everything the vault-session half needs, shared with the handlers.
    ctx: Ctx,
}

/// What one request handler is allowed to reach. Every member is an `Arc`, so
/// cloning this per request costs a refcount and the server owns exactly one
/// copy of each thing.
#[derive(Clone)]
struct Ctx {
    /// The ADMIN token: minted at start, scoped to the open TUI vault's snapshot
    /// exactly as it always was, and the only token `/unlock` accepts.
    token: Arc<Zeroizing<String>>,
    index: Arc<RwLock<ApiIndex>>,
    sessions: Arc<RwLock<Vec<Session>>>,
    policy: Arc<RwLock<ApiPolicy>>,
    /// The extension's PAIRING token — persistent across runs (it lives in
    /// `settings.toml`), scoped to the open TUI vault's logins and to nothing
    /// else. Empty = none is accepted.
    login_token: Arc<RwLock<Zeroizing<String>>>,
    /// Logins the TUI offered with the `l` key, waiting to be claimed.
    pending: Arc<Mutex<Vec<PendingLogin>>>,
    /// The browser's "unlock, please" (the newest only) and its "keep this
    /// login" requests, drained by the app's tick.
    wake: Arc<Mutex<Option<WakeRequest>>>,
    saves: Arc<Mutex<Vec<SaveRequest>>>,
    /// Where the `.vault` files live — the ONLY path this thread ever resolves,
    /// and it resolves it through [`vault::vault_path`], which hex-encodes the
    /// name: a `vault` member full of `../` names a file that does not exist,
    /// not a file outside the directory.
    vaults_dir: Arc<PathBuf>,
    /// The unlock queue. `Vault::open_file` pays a full KDF, which IS the
    /// brute-force rate limit — holding this across it makes that limit
    /// serial, so a hundred parallel guesses cost a hundred KDFs' TIME rather
    /// than a hundred cores. It is never taken by anything else and never held
    /// across the accept loop, so a slow unlock costs one parked request
    /// thread and nothing else.
    unlock_gate: Arc<Mutex<()>>,
}

impl ApiServer {
    /// Binds `127.0.0.1:port` and starts serving `index`. `port` 0 asks the OS
    /// for a free one (what the tests use — [`ApiServer::port`] then says which).
    /// `vaults_dir` is where `/unlock` looks for a vault by name, and `policy`
    /// what the user has allowed (pushed again by [`ApiServer::set_policy`]).
    ///
    /// `Err` = the port is taken, or the RNG refused to mint a token. Either
    /// way nothing is listening and the caller shows a toast.
    pub fn start(
        requested: u16,
        index: ApiIndex,
        vaults_dir: PathBuf,
        policy: ApiPolicy,
    ) -> Result<ApiServer, String> {
        let token = new_token();
        if token.is_empty() {
            return Err("no random session token".to_string());
        }
        // LOOPBACK, spelled out: a password manager's index may be reachable
        // from this machine and from nowhere else.
        let server = tiny_http::Server::http(("127.0.0.1", requested)).map_err(|e| e.to_string())?;
        let port = match server.server_addr().to_ip() {
            Some(addr) => addr.port(),
            None => return Err("the server did not bind an IP port".to_string()),
        };
        let server = Arc::new(server);
        let stop = Arc::new(AtomicBool::new(false));
        let ctx = Ctx {
            token: Arc::new(token),
            index: Arc::new(RwLock::new(index)),
            sessions: Arc::new(RwLock::new(Vec::new())),
            policy: Arc::new(RwLock::new(policy)),
            login_token: Arc::new(RwLock::new(Zeroizing::new(String::new()))),
            pending: Arc::new(Mutex::new(Vec::new())),
            wake: Arc::new(Mutex::new(None)),
            saves: Arc::new(Mutex::new(Vec::new())),
            vaults_dir: Arc::new(vaults_dir),
            unlock_gate: Arc::new(Mutex::new(())),
        };
        let handle = {
            let (server, stop, ctx) = (Arc::clone(&server), Arc::clone(&stop), ctx.clone());
            std::thread::Builder::new()
                .name("agent-api".into())
                .spawn(move || serve(&server, &stop, &ctx))
                .map_err(|e| e.to_string())?
        };
        let (token, index) = (Arc::clone(&ctx.token), Arc::clone(&ctx.index));
        Ok(ApiServer { server, stop, handle: Some(handle), requested, port, token, index, ctx })
    }

    /// The port it really bound (the asked-for one, or the OS's pick for 0).
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Whether this server is the one `want` asks for (see [`requested`]).
    pub fn serves_port(&self, want: u16) -> bool {
        self.requested == want
    }

    /// The whole token — the `ragtoken` command's payload, and nothing else's.
    pub fn token(&self) -> Zeroizing<String> {
        Zeroizing::new(self.token.as_str().to_string())
    }

    /// The first characters, for the read-only settings row: enough to tell one
    /// session's token from another's, useless to anybody who reads the screen.
    pub fn token_hint(&self) -> String {
        let head: String = self.token.chars().take(8).collect();
        format!("{head}\u{2026}")
    }

    /// Swaps in a freshly built snapshot. Called on unlock and after every
    /// vault mutation, so an agent never searches a thread that has moved on.
    pub fn refresh(&self, index: ApiIndex) {
        if let Ok(mut guard) = self.index.write() {
            *guard = index;
        }
    }

    /// Pushes what the user has allowed. Called wherever the settings can have
    /// changed, so turning `/reveal` off stops the endpoint answering NOW —
    /// not on the next restart.
    pub fn set_policy(&self, policy: ApiPolicy) {
        if let Ok(mut guard) = self.ctx.policy.write() {
            *guard = policy;
        }
    }

    /// Installs (or, with `None`, retires) the extension's pairing token. Pushed
    /// with the policy on every sync, so `logintoken new` and the switch going
    /// off both take effect on the next request.
    pub fn set_login_token(&self, token: Option<&str>) {
        if let Ok(mut guard) = self.ctx.login_token.write() {
            *guard = Zeroizing::new(token.unwrap_or_default().to_string());
        }
    }

    /// Offers one login for the extension to claim: a fresh nonce bound to
    /// `item_id`, claimable once within [`PENDING_TTL`] from a page of that
    /// item's site. Answers the nonce (the TUI puts it in the URL it opens).
    pub fn offer_login(&self, item_id: &str) -> Zeroizing<String> {
        let nonce = new_token();
        if nonce.is_empty() {
            return nonce; // no RNG: nothing offered, and the caller sees ""
        }
        if let Ok(mut guard) = self.ctx.pending.lock() {
            let now = Instant::now();
            guard.retain(|p| p.expires > now);
            guard.push(PendingLogin {
                nonce: Zeroizing::new(nonce.as_str().to_string()),
                item_id: item_id.to_string(),
                expires: now + PENDING_TTL,
            });
        }
        nonce
    }

    /// The browser's latest "a login page wants a vault open" request, if one
    /// came in since the last tick.
    pub fn take_wake(&self) -> Option<WakeRequest> {
        self.ctx.wake.lock().ok().and_then(|mut g| g.take())
    }

    /// The logins the browser asked to keep since the last tick.
    pub fn take_saves(&self) -> Vec<SaveRequest> {
        self.ctx.saves.lock().map(|mut g| std::mem::take(&mut *g)).unwrap_or_default()
    }

    /// How many logins are still waiting to be claimed (the tests' window on
    /// the queue; expired ones are not counted).
    pub fn pending_logins(&self) -> usize {
        let now = Instant::now();
        self.ctx.pending.lock().map(|g| g.iter().filter(|p| p.expires > now).count()).unwrap_or(0)
    }

    /// How many vaults an agent currently holds open through this server. Drives
    /// the `api·N` indicator; cheap enough for the tick (one read lock).
    pub fn session_count(&self) -> usize {
        self.ctx.sessions.read().map(|s| s.len()).unwrap_or(0)
    }

    /// Drops whatever the TTL has run out on. The request path does this too;
    /// this is the app's tick doing it for the sessions nobody is asking about,
    /// so an abandoned one does not stay a decrypted vault in memory.
    pub fn reap_sessions(&self) {
        let ttl = self.policy_ttl();
        if ttl.is_none() {
            return; // nothing can expire; don't take the write lock at all
        }
        if let Ok(mut guard) = self.ctx.sessions.write() {
            reap(&mut guard, ttl);
        }
    }

    /// Kills every session opened on `vault` and answers how many died. The
    /// re-key hook: after a `passwd` or a `rerollEnc` the key those sessions
    /// hold is dead anyway, and a session that outlived the password it was
    /// opened with would be a way to read a vault with a retired credential.
    pub fn drop_sessions_for(&self, vault: &str) -> usize {
        let Ok(mut guard) = self.ctx.sessions.write() else { return 0 };
        let before = guard.len();
        guard.retain(|s| s.vault != vault);
        before - guard.len()
    }

    /// Kills them all — the panic switch behind `api_vault_unlock`, and what
    /// stopping the server does on its way down.
    pub fn drop_all_sessions(&self) {
        if let Ok(mut guard) = self.ctx.sessions.write() {
            guard.clear();
        }
    }

    fn policy_ttl(&self) -> Option<Duration> {
        self.ctx.policy.read().ok().and_then(|p| p.session_ttl)
    }

    /// Stops the server and WAITS for its accept loop, then empties the
    /// snapshot.
    ///
    /// The wait is what makes "the API stopped" true rather than eventual: the
    /// next start may want the very same port, and a settings change that
    /// rebinds must not race the socket it is replacing. It is bounded by one
    /// [`POLL`], and by nothing a client does — which is the whole reason
    /// [`serve`] answers each request on a thread of its own.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.server.unblock(); // wake the loop out of its recv, now
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        // No NEW request can be taken now, so emptying the snapshot is what
        // makes "the server stopped" mean "there is nothing left to serve".
        //
        // A handler already out is not waited for (that is the whole reason
        // each one runs on its own thread) and may still be inside a KDF pass:
        // a session it pushes after this line lands in a `Vec` nothing can
        // reach any more — the socket is closed — and is wiped when that
        // thread's last `Ctx` clone drops moments later.
        self.refresh(ApiIndex::default());
        // …and the sessions go with it: a decrypted vault whose door has just
        // been bricked up is a decrypted vault nobody can reach and nobody
        // asked to keep. Their keys and content are wiped on this drop.
        self.drop_all_sessions();
    }
}

impl Drop for ApiServer {
    fn drop(&mut self) {
        // the last line of defence: whatever path takes the App down — quit,
        // a panic unwind — must not leave a socket serving the vault.
        self.shutdown();
    }
}

/// The accept loop. Never panics on a client: a malformed request is an answer,
/// not a dead thread.
///
/// IT NEVER TOUCHES A REQUEST ITSELF. `tiny_http` hands the handler a reader
/// over the LIVE socket for anything but a small body, and sets no read timeout
/// on it: a peer that declares `Content-Length: 65536` and then says nothing
/// blocks whoever reads that body — and blocks the DROP of the request too,
/// which drains what was declared, 401 or not. On this loop that would wedge
/// the thread [`ApiServer::shutdown`] joins, and with it Ctrl+L, closing the
/// vault, the settings commit and quitting. So each request is answered on a
/// thread of its own: a stalled peer costs one parked thread until it hangs up,
/// and costs the interface nothing.
fn serve(server: &tiny_http::Server, stop: &AtomicBool, ctx: &Ctx) {
    while !stop.load(Ordering::SeqCst) {
        match server.recv_timeout(POLL) {
            Ok(Some(request)) => {
                let ctx = ctx.clone();
                std::thread::Builder::new()
                    .name("agent-api-request".into())
                    .spawn(move || handle(request, &ctx))
                    .ok();
            }
            Ok(None) => {}
            // an accept that failed. The flag decides whether to carry on, and
            // the pause is what keeps a permanently broken socket from turning
            // this loop into a busy one.
            Err(_) => std::thread::sleep(POLL),
        }
    }
}

/// Who is asking. There is no third kind: a request is the admin (the token
/// minted with the server, scoped to the open TUI vault) or one session (scoped
/// to the vault it unlocked, and to nothing else — which is why no body on this
/// API takes a vault name).
enum Auth {
    Admin,
    Session(Zeroizing<String>),
    /// The browser extension, by its pairing token: the open TUI vault's
    /// logins and nothing else — not a search, not a reveal, not an unlock.
    Extension,
}

/// One request, start to finish.
fn handle(mut request: tiny_http::Request, ctx: &Ctx) {
    // the path without its query string — this API takes its arguments in the
    // body, and `/health?x=1` is still `/health`
    let path = request.url().split('?').next().unwrap_or("").to_string();
    let method = request.method().clone();

    // /health is the ONE unauthenticated endpoint, and it says nothing beyond
    // "something is listening" — no version, no vault name, no counts.
    if path == "/health" {
        respond(request, 200, &serde_json::json!({ "status": "ok" }));
        return;
    }
    let policy = ctx.policy.read().map(|p| *p).unwrap_or_default();
    // BEFORE authentication: a token the idle TTL has run out on must not be
    // able to authenticate anything, not even to be told it expired. With no
    // TTL set there is nothing to reap and the write lock is not taken at all.
    if policy.session_ttl.is_some() {
        if let Ok(mut guard) = ctx.sessions.write() {
            reap(&mut guard, policy.session_ttl);
        }
    }
    let Some(auth) = resolve_auth(&request, ctx) else {
        // no detail, ever: not whether the header was missing, malformed, an
        // expired session's or simply wrong. All four are the same answer.
        respond(request, 401, &serde_json::json!({ "error": "unauthorized" }));
        return;
    };
    // the sliding half of the TTL: an authorized request pushes the deadline
    // back, so an agent at work is never cut off in the middle of a task.
    if let Auth::Session(token) = &auth {
        touch_session(ctx, token);
    }
    // which endpoints EXIST depends on the policy: one that is switched off is
    // not "forbidden", it is not there — the same 404 an unknown path gets, so
    // the answer never reports the state of the user's settings. The same goes
    // for an endpoint asked for with the wrong kind of token (a session cannot
    // mint a session; the admin has no session to lock).
    let extension = matches!(auth, Auth::Extension);
    let known = match path.as_str() {
        "/search" => !extension,
        "/unlock" => policy.allow_unlock && matches!(auth, Auth::Admin),
        "/reveal" => policy.allow_reveal && !extension,
        "/lock" => matches!(auth, Auth::Session(_)),
        // the extension's three doors: for every kind of token, but only
        // while the user has them on
        "/login/match" | "/login/fill" | "/login/claim" | "/login/info" | "/login/wake"
        | "/login/totp" | "/login/save" => policy.allow_login,
        _ => false,
    };
    if !known {
        respond(request, 404, &serde_json::json!({ "error": "not-found" }));
        return;
    }
    if method != tiny_http::Method::Post {
        respond(request, 405, &serde_json::json!({ "error": "method-not-allowed" }));
        return;
    }
    // EVERY body is read into a wiped buffer, not just `/unlock`'s: a search
    // query names what is inside somebody's vault, and the one body that
    // carries a master password must not be a special case anybody can forget.
    //
    // …and it is sized from the declared length first, because a buffer that
    // GROWS leaves each of its outgrown copies — a prefix of the body, password
    // included — in freed heap that no `Zeroizing` ever reaches.
    let declared = request.body_length().unwrap_or(0).min(MAX_BODY as usize);
    let mut body = Zeroizing::new(String::with_capacity(declared));
    if request.as_reader().take(MAX_BODY).read_to_string(&mut body).is_err() {
        respond(request, 400, &serde_json::json!({ "error": "bad-request" }));
        return;
    }
    let answer = match path.as_str() {
        "/search" => run_search(ctx, &auth, &policy, &body),
        "/unlock" => run_unlock(ctx, &policy, &body),
        "/reveal" => run_reveal(ctx, &auth, &body),
        "/lock" => run_lock(ctx, &auth),
        "/login/match" => run_login_match(ctx, &auth, &body),
        "/login/fill" => run_login_fill(ctx, &auth, &body),
        "/login/claim" => run_login_claim(ctx, &auth, &body),
        "/login/info" => run_login_info(ctx, &policy),
        "/login/wake" => run_login_wake(ctx, &body),
        "/login/totp" => run_login_totp(ctx, &auth, &body),
        "/login/save" => run_login_save(ctx, &body),
        _ => Err((404, "not-found")),
    };
    match answer {
        Ok(mut value) => {
            respond(request, 200, &value);
            // …and the copy the ANSWER is made of goes the same way the body it
            // came from did: `/reveal` puts a stored secret in there and
            // `/unlock` a fresh session token, and a `serde_json::Value` wipes
            // nothing when it drops.
            wipe_json(&mut value);
        }
        Err((code, reason)) => respond(request, code, &serde_json::json!({ "error": reason })),
    }
}

/// Zeroizes every string inside a response body that has already gone out.
///
/// A `Value` is where the `Zeroizing`/`SecretString` discipline runs out:
/// `json!({ "value": f.value.as_str() })` CLONES the secret into a plain
/// `String` the map owns. Member NAMES are left alone — they are `value`,
/// `token`, `label`, never a secret.
fn wipe_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => text.zeroize(),
        serde_json::Value::Array(items) => items.iter_mut().for_each(wipe_json),
        serde_json::Value::Object(members) => members.values_mut().for_each(wipe_json),
        _ => {}
    }
}

/// The `Authorization: Bearer <token>` value, if there is one.
fn bearer(request: &tiny_http::Request) -> Option<&str> {
    request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Authorization"))
        .and_then(|h| h.value.as_str().strip_prefix("Bearer "))
        .map(str::trim)
}

/// Who the presented token is, compared in CONSTANT TIME against every
/// candidate — the admin's and each live session's.
///
/// It deliberately does not stop at the first match: an early exit would make
/// "how long did the 401 take?" count how many sessions are live and how far
/// down the list a guess got. Every request costs the same walk.
fn resolve_auth(request: &tiny_http::Request, ctx: &Ctx) -> Option<Auth> {
    let presented = bearer(request)?;
    let mut found: Option<Auth> = None;
    if token_matches(ctx.token.as_str(), presented) {
        found = Some(Auth::Admin);
    }
    if let Ok(sessions) = ctx.sessions.read() {
        for session in sessions.iter() {
            if token_matches(&session.token, presented) {
                found = Some(Auth::Session(Zeroizing::new(presented.to_string())));
            }
        }
    }
    // …and the pairing token, on the same no-early-exit walk
    if let Ok(pairing) = ctx.login_token.read() {
        if token_matches(&pairing, presented) {
            found = Some(Auth::Extension);
        }
    }
    found
}

// ---- /login/*: the browser extension ----------------------------------------

/// The logins `auth` may see: the open TUI vault's (admin and extension) or
/// the session's own, built through the one builder. `f` runs under the lock
/// so nothing is copied out that the caller does not need.
fn with_logins<T>(
    ctx: &Ctx,
    auth: &Auth,
    f: impl FnOnce(&[LoginEntry]) -> T,
) -> Result<T, (u16, &'static str)> {
    match auth {
        Auth::Admin | Auth::Extension => {
            let guard = ctx.index.read().map_err(|_| (503u16, "unavailable"))?;
            Ok(f(&guard.logins))
        }
        Auth::Session(token) => {
            let sessions = ctx.sessions.read().map_err(|_| (503u16, "unavailable"))?;
            let session = sessions
                .iter()
                .find(|s| token_matches(&s.token, token))
                .ok_or((401u16, "unauthorized"))?;
            Ok(f(&login_entries(&session.content.items)))
        }
    }
}

/// `POST /login/match {origin}`: which items log in at this site — id, label,
/// username, domain. NEVER a password: this is the one call the extension
/// makes on every login page it sees, unasked.
#[derive(Deserialize)]
struct MatchRequest {
    origin: String,
}

fn run_login_match(ctx: &Ctx, auth: &Auth, body: &str) -> Result<serde_json::Value, (u16, &'static str)> {
    let request: MatchRequest = serde_json::from_str(body).map_err(|_| (400u16, "bad-request"))?;
    if crate::favicon::domain_of(&request.origin).is_none() {
        return Err((400, "bad-origin"));
    }
    let hits = with_logins(ctx, auth, |logins| {
        logins
            .iter()
            .filter(|e| domain_matches(&request.origin, &e.domain))
            .map(|e| {
                serde_json::json!({
                    "id": e.id, "label": e.label, "username": e.username, "domain": e.domain,
                })
            })
            .collect::<Vec<_>>()
    })?;
    Ok(serde_json::json!({ "logins": hits }))
}

/// The credentials of one entry, as both `/login/fill` and `/login/claim`
/// send them — the only shape a password leaves through on this path.
fn credentials(entry: &LoginEntry) -> serde_json::Value {
    serde_json::json!({
        "id": entry.id, "label": entry.label,
        "username": entry.username, "password": entry.password.as_str(),
    })
}

/// `POST /login/fill {id}`: the username and password of ONE item the user
/// picked from the extension's menu. An unknown id is the generic 404.
#[derive(Deserialize)]
struct FillRequest {
    id: String,
    /// The page the credentials are for. The EXTENSION must name it, and the
    /// item must belong to it — the same rule `/login/match` listed it by —
    /// so an id can never be redeemed on a site it was not offered to.
    #[serde(default)]
    origin: Option<String>,
}

fn run_login_fill(ctx: &Ctx, auth: &Auth, body: &str) -> Result<serde_json::Value, (u16, &'static str)> {
    let request: FillRequest = serde_json::from_str(body).map_err(|_| (400u16, "bad-request"))?;
    if matches!(auth, Auth::Extension) && request.origin.is_none() {
        return Err((400, "bad-request"));
    }
    with_logins(ctx, auth, |logins| {
        logins
            .iter()
            .find(|e| e.id == request.id)
            .filter(|e| request.origin.as_deref().is_none_or(|o| domain_matches(o, &e.domain)))
            .map(credentials)
            .ok_or((404u16, "not-found"))
    })?
}

/// `POST /login/claim {nonce, origin}`: the login the TUI offered with the `l`
/// key. ONCE: the nonce is spent whether or not the credentials are handed
/// over; and only from the item's own site — a nonce that reached a page on
/// another domain (a redirect gone elsewhere, a copied URL) opens nothing.
#[derive(Deserialize)]
struct ClaimRequest {
    nonce: String,
    origin: String,
}

fn run_login_claim(ctx: &Ctx, auth: &Auth, body: &str) -> Result<serde_json::Value, (u16, &'static str)> {
    let request: ClaimRequest = serde_json::from_str(body).map_err(|_| (400u16, "bad-request"))?;
    let item_id = {
        let mut pending = ctx.pending.lock().map_err(|_| (503u16, "unavailable"))?;
        let now = Instant::now();
        pending.retain(|p| p.expires > now);
        let at = pending
            .iter()
            .position(|p| token_matches(&p.nonce, &request.nonce))
            .ok_or((404u16, "not-found"))?;
        pending.remove(at).item_id
    };
    with_logins(ctx, auth, |logins| {
        logins
            .iter()
            .find(|e| e.id == item_id && domain_matches(&request.origin, &e.domain))
            .map(credentials)
            .ok_or((404u16, "not-found"))
    })?
}

/// Stamps a session as used, now. A token that cannot be found (it died between
/// the resolution and here) is simply not stamped — the endpoint that follows
/// looks it up again and answers 404 rather than trusting this.
fn touch_session(ctx: &Ctx, token: &str) {
    if let Ok(mut sessions) = ctx.sessions.write() {
        if let Some(session) = sessions.iter_mut().find(|s| token_matches(&s.token, token)) {
            session.used = Instant::now();
        }
    }
}

/// What a client asks for. `k` and `mode` are optional; an unknown `scope` is a
/// bad request rather than an empty result, because "no hits" would read as an
/// answer about the vault.
///
/// THERE IS NO `vault` MEMBER, AND THERE MUST NEVER BE ONE. With a session
/// token the scope IS the token; with the admin token it is whatever is open in
/// the TUI. A name here would be a way to point a token at a vault it was not
/// minted for, so the shape of this struct is the enforcement.
#[derive(Deserialize)]
struct SearchRequest {
    scope: String,
    query: String,
    #[serde(default)]
    k: Option<usize>,
    #[serde(default)]
    mode: Option<String>,
}

/// One hit as it goes out. `snippet` is a window of the INDEXED text, which is
/// non-secret by construction — there is no branch here that could reach a
/// secret value, because none is in the snapshot to reach.
#[derive(serde::Serialize)]
struct Hit {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    ts: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    category: Option<String>,
    snippet: String,
    score: f32,
}

/// Runs the search. `Err((status, reason))` is the whole error surface.
///
/// NO LOCK IS HELD ACROSS THE INFERENCE. The app refreshes this snapshot from
/// the UI thread after every vault mutation, and a write that had to queue
/// behind an embedding would be a frozen interface — so the entries of the
/// asked-for scope are COPIED out under one short read, and everything after
/// that (the embed, the ranking) runs with nothing held.
///
/// The answer says which search really ran. An agent that asked for `rag` over
/// a vault whose vectors were never computed gets the literal search and is
/// TOLD so, rather than an empty list that reads as "nothing in there".
fn run_search(
    ctx: &Ctx,
    auth: &Auth,
    policy: &ApiPolicy,
    body: &str,
) -> Result<serde_json::Value, (u16, &'static str)> {
    let request: SearchRequest = serde_json::from_str(body).map_err(|_| (400u16, "bad-request"))?;
    let k = request.k.unwrap_or(DEFAULT_K).clamp(1, MAX_K);
    let query = request.query.trim().to_string();
    // the engine is the app's own — a session borrows it exactly as the open
    // vault's snapshot does, and never loads one of its own.
    let embedder = ctx.index.read().map_err(|_| (503u16, "unavailable"))?.embedder.clone();
    // WHAT COULD REFUSE THIS REQUEST IS SETTLED BEFORE ANYTHING IS COPIED. An
    // unknown scope is a bad request, not an empty result ("no hits" would read
    // as an answer about the vault), and a session that died between the
    // authentication and here is still nobody. Only then does an empty query
    // take its exit — building the entries first would make "everything", which
    // is a list and not a search result, cost a full copy of the asked-for scope
    // to answer with none of it.
    match auth {
        Auth::Admin => {
            let guard = ctx.index.read().map_err(|_| (503u16, "unavailable"))?;
            guard.scope(&request.scope).ok_or((400u16, "bad-scope"))?;
        }
        Auth::Session(token) => session_scope(ctx, token, &request.scope)?,
        Auth::Extension => return Err((404, "not-found")), // (never routed here)
    }
    if query.is_empty() {
        return Ok(serde_json::json!({ "hits": Vec::<Hit>::new(), "mode": "literal" }));
    }
    let entries = match auth {
        Auth::Admin => {
            let guard = ctx.index.read().map_err(|_| (503u16, "unavailable"))?;
            guard.scope(&request.scope).ok_or((400u16, "bad-scope"))?.to_vec()
        }
        Auth::Session(token) => session_entries(ctx, token, &request.scope, policy, &embedder)?,
        Auth::Extension => return Err((404, "not-found")),
    };
    // the default is the good search when it can run, and the honest one when
    // it cannot — an agent that says nothing gets an answer either way.
    let wants_rag = match request.mode.as_deref() {
        Some("rag") => true,
        Some("literal") => false,
        Some(_) => return Err((400, "bad-mode")),
        None => embedder.is_some(),
    };
    // …and asking for `rag` over a scope with no stored vectors is not an
    // error either: there is a real answer available, so give it and label it.
    let has_vectors = entries.iter().any(|e| e.vec.is_some());
    let vector = match wants_rag && has_vectors {
        false => None,
        true => {
            let embedder = embedder.ok_or((503u16, "model-not-ready"))?;
            Some(embedder.embed_query(&query).map_err(|_| (503u16, "model-not-ready"))?)
        }
    };
    let hits: Vec<Hit> = match &vector {
        Some(vector) => {
            let vectors: Vec<(String, Vec<f32>)> = entries
                .iter()
                .filter_map(|e| e.vec.as_ref().map(|v| (e.id.clone(), v.clone())))
                .collect();
            rag::search_semantic(&vectors, vector, k)
                .into_iter()
                .map(|hit| {
                    let entry = entries.iter().find(|e| e.id == hit.id);
                    let snippet = entry.map(|e| rag::preview(&e.text)).unwrap_or_default();
                    hit_of(entry, hit.id, snippet, hit.score)
                })
                .collect()
        }
        None => {
            let texts: Vec<(String, String)> =
                entries.iter().map(|e| (e.id.clone(), e.text.clone())).collect();
            rag::search_literal(&texts, &query)
                .into_iter()
                .take(k)
                .map(|hit| {
                    let entry = entries.iter().find(|e| e.id == hit.id);
                    hit_of(entry, hit.id, hit.snippet, hit.score as f32)
                })
                .collect()
        }
    };
    let mode = if vector.is_some() { "rag" } else { "literal" };
    Ok(serde_json::json!({ "hits": hits, "mode": mode }))
}

/// The two refusals [`session_entries`] would answer with — an unknown scope,
/// and a token whose session died between the authentication and here — settled
/// without copying one byte out of the vault. It is what lets an empty query
/// answer before the entries are built.
fn session_scope(ctx: &Ctx, token: &str, scope: &str) -> Result<(), (u16, &'static str)> {
    if !matches!(scope, "memo" | "items") {
        return Err((400, "bad-scope"));
    }
    let sessions = ctx.sessions.read().map_err(|_| (503u16, "unavailable"))?;
    sessions.iter().find(|s| token_matches(&s.token, token)).ok_or((401u16, "unauthorized"))?;
    Ok(())
}

/// The searchable entries of ONE session's own vault, built the same way the
/// app builds the open vault's snapshot — through [`rag::memo_embed_text`] and
/// [`rag::item_texts`], which go through [`rag::indexable_fields`].
///
/// THAT IS THE WHOLE NON-SECRET GUARANTEE, and it is the same door: a secret
/// field's value has no path into this text, whatever the caller asks for. A
/// session searches a decrypted vault and still cannot search a password out of
/// it — only `/reveal` sends one, and only when the user turned it on.
///
/// Vectors come from what that vault already has stored (`x_memo` per message,
/// `x_rag` for the items) — this thread embeds nothing but the query, and a
/// side table written by ANOTHER model is ignored rather than mixed in: a dot
/// product across two embedding spaces is noise.
fn session_entries(
    ctx: &Ctx,
    token: &str,
    scope: &str,
    policy: &ApiPolicy,
    embedder: &Option<Arc<dyn Embedder>>,
) -> Result<Vec<ApiEntry>, (u16, &'static str)> {
    if !matches!(scope, "memo" | "items") {
        return Err((400, "bad-scope"));
    }
    let sessions = ctx.sessions.read().map_err(|_| (503u16, "unavailable"))?;
    let session =
        sessions.iter().find(|s| token_matches(&s.token, token)).ok_or((401u16, "unauthorized"))?;
    let content = &session.content;
    if scope == "memo" {
        // through the BORROWING read: a search ranks words, and copying every
        // stored photo's base64 out of the vault — per request, freed un-wiped —
        // to rank none of it is a cost with nothing on the other side of it.
        return Ok(content
            .memo_rows()
            .iter()
            .map(|m| ApiEntry {
                id: m.id.to_string(),
                ts: Some(m.ts.to_string()),
                label: None,
                category: None,
                caption: m.caption().map(str::to_string),
                text: rag::memo_text(m.text, m.caption().unwrap_or("")),
                vec: m.vec.clone(),
            })
            .filter(|e| !e.text.is_empty())
            .collect());
    }
    let lang = policy.lang;
    let stored = content.item_vectors();
    let fresh = embedder.as_ref().is_some_and(|e| e.tag() == stored.model);
    Ok(rag::item_texts(&content.items, |id| i18n::t(lang, &Key::CategoryName(id)))
        .into_iter()
        .map(|(id, text)| {
            let item = content.items.iter().find(|i| i.id == id);
            ApiEntry {
                label: item.map(|i| i.label.clone()),
                category: item.map(|i| i18n::t(lang, &Key::CategoryName(&i.category))),
                vec: fresh.then(|| stored.items.get(&id).map(|(_, v)| v.clone())).flatten(),
                id,
                ts: None,
                caption: None,
                text,
            }
        })
        .collect())
}

// ---- /unlock: a vault opened through the API -------------------------------

/// The one body in this crate that carries a master password.
///
/// THE WIPE BELONGS TO THE FIELD, NOT TO THE STRUCT. A `Drop` on the whole
/// request only runs on a request that was fully built: serde reads the members
/// one at a time, so a body that fails on a LATER one — `{"password":"…",
/// "vault":123}`, a duplicate key, a malformed member after it — drops the
/// password serde had already collected without the struct ever existing, and a
/// struct-level wipe never runs at all. A `Zeroizing` field is dropped whichever
/// way the parse ends. The RAW body it is parsed out of is a `Zeroizing<String>`
/// in [`handle`], so that copy goes too.
///
/// What is NOT covered, and cannot be from here: `serde_json` unescapes a string
/// through a scratch buffer of its own, so a password containing `\"`, `\\`,
/// `\n` or a `\uXXXX` escape leaves one copy in that buffer — inside the crate,
/// out of reach of anything but not parsing with it.
///
/// Nothing here is ever logged, echoed, or named in an answer — the response to
/// `/unlock` is a token or a five-word refusal, and never a word about which
/// part was wrong.
#[derive(Deserialize)]
struct UnlockRequest {
    vault: String,
    #[serde(deserialize_with = "zeroizing_string")]
    password: Zeroizing<String>,
}

/// Deserializes straight into a `Zeroizing<String>`: `Zeroizing::new` MOVES the
/// buffer serde filled rather than copying it, so the allocation that held the
/// password is the one that gets wiped.
fn zeroizing_string<'de, D>(de: D) -> Result<Zeroizing<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    String::deserialize(de).map(Zeroizing::new)
}

/// The floor every refused `/unlock` answers on, measured from the moment the
/// request reached [`run_unlock`].
///
/// The refusals are byte-identical, but they are not all reached the same way: a
/// name with no file behind it is settled by one `stat`, while a wrong password
/// pays a full KDF pass first — hundreds of milliseconds. Byte-identical answers
/// a stopwatch can still separate are a vault-name oracle with an extra step, so
/// every refusal waits out the same floor before it goes back. Comfortably past
/// a default Argon2id pass on ordinary hardware, and it costs the machine
/// NOTHING while it waits: a decoy derivation would hide the same gap by making
/// every bogus name a 64 MiB memory bomb.
const UNLOCK_REFUSAL_FLOOR: Duration = Duration::from_millis(300);

/// Opens a vault by name and password and mints a session for it.
///
/// EVERY WAY TO FAIL IS ONE ANSWER. A wrong password, a vault that does not
/// exist and a vault file that is there and unreadable all answer
/// `401 {"error":"unauthorized"}`, byte for byte and no sooner than
/// [`UNLOCK_REFUSAL_FLOOR`]: an agent that could tell them apart would have a
/// vault-name oracle, and the list of a user's vaults is not this endpoint's to
/// hand out. The unreadable case used to answer 500, which was both of those
/// things at once — a file this build cannot parse is reached with NO password
/// (so the 500 confirmed a name), and one that fails to parse AFTER the AEAD
/// opened is reached only with the RIGHT one (so it confirmed a guess).
///
/// The KDF pass runs under [`Ctx::unlock_gate`], one at a time; the floor is
/// waited out with the gate released, so a refusal parks its own thread and
/// nobody else's.
fn run_unlock(
    ctx: &Ctx,
    policy: &ApiPolicy,
    body: &str,
) -> Result<serde_json::Value, (u16, &'static str)> {
    let started = Instant::now();
    let answer = unlock_attempt(ctx, policy, body);
    if let Err((401, _)) = answer {
        if let Some(rest) = UNLOCK_REFUSAL_FLOOR.checked_sub(started.elapsed()) {
            std::thread::sleep(rest);
        }
    }
    answer
}

/// The attempt itself — see [`run_unlock`], which is this plus the floor every
/// refusal waits out.
fn unlock_attempt(
    ctx: &Ctx,
    policy: &ApiPolicy,
    body: &str,
) -> Result<serde_json::Value, (u16, &'static str)> {
    let request: UnlockRequest = serde_json::from_str(body).map_err(|_| (400u16, "bad-request"))?;
    if request.vault.trim().is_empty() {
        return Err((401, "unauthorized"));
    }
    // ONE unlock at a time. Concurrent guesses queue here; they never run in
    // parallel, so the KDF's cost stays the cost of guessing.
    let _queue = ctx.unlock_gate.lock().unwrap_or_else(|e| e.into_inner());
    // the cap is checked BEFORE the KDF: a refused request should not cost the
    // machine a full derivation first.
    {
        let mut sessions = ctx.sessions.write().map_err(|_| (503u16, "unavailable"))?;
        reap(&mut sessions, policy.session_ttl);
        if sessions.len() >= MAX_SESSIONS {
            return Err((409, "too-many-sessions"));
        }
    }
    let path = vault::vault_path(&ctx.vaults_dir, &request.vault);
    if !path.is_file() {
        // the same refusal a wrong password gets, and — through the floor
        // [`run_unlock`] holds every refusal to — after the same wait.
        return Err((401, "unauthorized"));
    }
    let mut opened = match Vault::open_file(&path, &request.password) {
        Ok(vault) => vault,
        Err(UnlockError::WrongPassword) => return Err((401, "unauthorized")),
        // a file this build cannot read gets the same one. Never the file's own
        // words (they name paths and formats), and never a status of its own:
        // WHICH way a vault refused to open is the oracle (see above).
        Err(UnlockError::Corrupted(_)) => return Err((401, "unauthorized")),
    };
    let token = new_token();
    if token.is_empty() {
        return Err((503, "unavailable")); // no RNG, no token, no session
    }
    let now = Instant::now();
    let session = Session {
        token: Zeroizing::new(token.as_str().to_string()),
        vault: opened.username.clone(),
        key: opened.key_material(),
        // moved, not copied: the `Vault` is dropped at the end of this function
        // and must not take the content with it.
        content: std::mem::take(&mut opened.content),
        created: now,
        used: now,
    };
    let mut sessions = ctx.sessions.write().map_err(|_| (503u16, "unavailable"))?;
    reap(&mut sessions, policy.session_ttl);
    if sessions.len() >= MAX_SESSIONS {
        return Err((409, "too-many-sessions"));
    }
    sessions.push(session);
    drop(sessions);
    // the token goes out in the clear on the loopback, which is the whole
    // point of it; `expires_in` is null when the user turned expiry off, and
    // never a number the server does not mean.
    Ok(serde_json::json!({
        "token": token.as_str(),
        "expires_in": policy.session_ttl.map(|d| d.as_secs()),
    }))
}

/// `POST /lock`: this session, now. Its key and its decrypted content are wiped
/// by the drop, and the token stops being a token.
fn run_lock(ctx: &Ctx, auth: &Auth) -> Result<serde_json::Value, (u16, &'static str)> {
    let Auth::Session(token) = auth else {
        return Err((404, "not-found")); // the admin token has no session
    };
    let mut sessions = ctx.sessions.write().map_err(|_| (503u16, "unavailable"))?;
    let before = sessions.len();
    sessions.retain(|s| !token_matches(&s.token, token));
    Ok(serde_json::json!({ "locked": before != sessions.len() }))
}

// ---- /reveal: THE endpoint secret values leave through ---------------------
//
// EVERYTHING ELSE IN THIS FILE IS NON-SECRET BY CONSTRUCTION. `/search` answers
// out of text that went through `rag::indexable_fields`, which hands over no
// value for a secret field and none for a picture; the admin snapshot has no
// secret in it to serve at all. This is the exception, and it is deliberate:
// the user asked for an agent that can fetch a password, and there is no
// honest way to do that without sending one.
//
// So it is fenced three ways:
//   * it does not exist unless `settings.api_reveal` is on — off out of the
//     box, and its settings row says in both languages what turning it on does;
//   * it answers ONE item at a time, by id, and optionally ONE field of it —
//     there is no "give me everything" shape here;
//   * a picture is never sent, only its size: `{"type":"image","bytes":N}`.
//     Megabytes of base64 through a JSON body help nobody, and an image field
//     is a scanned passport as often as it is a logo.
//
// A session reveals out of its own decrypted content. The admin token reveals
// out of the OPEN TUI VAULT — and only because the app put those values in the
// snapshot, which it does only while the same toggle is on.

/// What `/reveal` takes. `field` is optional: absent = every field of the item.
/// No vault name here either — see [`SearchRequest`].
#[derive(Deserialize)]
struct RevealRequest {
    id: String,
    #[serde(default)]
    field: Option<String>,
}

fn run_reveal(ctx: &Ctx, auth: &Auth, body: &str) -> Result<serde_json::Value, (u16, &'static str)> {
    let request: RevealRequest = serde_json::from_str(body).map_err(|_| (400u16, "bad-request"))?;
    let wanted = request.field.as_deref();
    match auth {
        // out of the open TUI vault's snapshot, which carries values only while
        // `api_reveal` is on (and is empty the moment it is off).
        Auth::Admin => {
            let guard = ctx.index.read().map_err(|_| (503u16, "unavailable"))?;
            let item =
                guard.reveal.iter().find(|i| i.id == request.id).ok_or((404u16, "not-found"))?;
            reveal_body(item, wanted)
        }
        // …or out of this session's own vault, built through the one door.
        Auth::Session(token) => {
            let sessions = ctx.sessions.read().map_err(|_| (503u16, "unavailable"))?;
            let session = sessions
                .iter()
                .find(|s| token_matches(&s.token, token))
                .ok_or((401u16, "unauthorized"))?;
            let item = session
                .content
                .items
                .iter()
                .find(|i| i.id == request.id)
                .ok_or((404u16, "not-found"))?;
            reveal_body(&reveal_item(item), wanted)
        }
        Auth::Extension => Err((404, "not-found")), // (never routed here)
    }
}

/// One item's fields as JSON — `value` for everything but a picture, whose
/// payload is replaced by its size. An unknown `field` name is a 404, not an
/// empty list: an empty list would read as "that field is blank".
fn reveal_body(
    item: &RevealItem,
    wanted: Option<&str>,
) -> Result<serde_json::Value, (u16, &'static str)> {
    let fields: Vec<serde_json::Value> = item
        .fields
        .iter()
        .filter(|f| wanted.is_none_or(|w| f.key == w))
        .map(|f| match f.image_bytes {
            Some(bytes) => serde_json::json!({
                "key": f.key, "label": f.label, "type": "image",
                "bytes": bytes, "secret": f.secret,
            }),
            None => serde_json::json!({
                "key": f.key, "label": f.label, "type": f.ftype,
                "value": f.value.as_str(), "secret": f.secret,
            }),
        })
        .collect();
    if fields.is_empty() && wanted.is_some() {
        return Err((404, "not-found"));
    }
    Ok(serde_json::json!({ "id": item.id, "label": item.label, "fields": fields }))
}

/// `POST /login/info`: what the extension needs to speak and to behave — the
/// interface language, and whether a vault is open in the TUI.
fn run_login_info(ctx: &Ctx, policy: &ApiPolicy) -> Result<serde_json::Value, (u16, &'static str)> {
    let open = ctx.index.read().map_err(|_| (503u16, "unavailable"))?.vault_open;
    Ok(serde_json::json!({ "lang": policy.lang.code(), "vault_open": open }))
}

/// `POST /login/wake {origin}`: a login page wants credentials and no vault is
/// open — the app brings itself forward and asks for one. Only the host is
/// kept, and only the newest request.
fn run_login_wake(ctx: &Ctx, body: &str) -> Result<serde_json::Value, (u16, &'static str)> {
    let request: MatchRequest = serde_json::from_str(body).map_err(|_| (400u16, "bad-request"))?;
    let host = crate::favicon::domain_of(&request.origin).ok_or((400u16, "bad-origin"))?;
    if let Ok(mut wake) = ctx.wake.lock() {
        *wake = Some(WakeRequest { host });
    }
    Ok(serde_json::json!({ "ok": true }))
}

/// `POST /login/totp {id, origin}`: the one-time code of the moment for an
/// item that has a TOTP seed — the code, never the seed — on the same site
/// rule as `/login/fill`.
fn run_login_totp(ctx: &Ctx, auth: &Auth, body: &str) -> Result<serde_json::Value, (u16, &'static str)> {
    let request: FillRequest = serde_json::from_str(body).map_err(|_| (400u16, "bad-request"))?;
    if matches!(auth, Auth::Extension) && request.origin.is_none() {
        return Err((400, "bad-request"));
    }
    with_logins(ctx, auth, |logins| {
        let entry = logins
            .iter()
            .find(|e| e.id == request.id)
            .filter(|e| request.origin.as_deref().is_none_or(|o| domain_matches(o, &e.domain)))
            .ok_or((404u16, "not-found"))?;
        let seed = entry.totp.as_ref().ok_or((404u16, "not-found"))?;
        let (code, left) = crate::totp::code_now(seed).ok_or((404u16, "not-found"))?;
        Ok(serde_json::json!({ "id": entry.id, "code": code, "seconds_left": left }))
    })?
}

/// `POST /login/save {origin, username, password}`: a login made by hand in
/// the browser that the user chose to keep. Queued for the app's tick (the
/// server thread never writes a vault); `409 vault-locked` when there is no
/// open vault to keep it in. The password is read into a wiped buffer like
/// every body, and travels in a `Zeroizing` field from here on.
#[derive(Deserialize)]
struct SaveRequest_ {
    origin: String,
    username: String,
    #[serde(deserialize_with = "zeroizing_string")]
    password: Zeroizing<String>,
}

fn run_login_save(ctx: &Ctx, body: &str) -> Result<serde_json::Value, (u16, &'static str)> {
    let request: SaveRequest_ = serde_json::from_str(body).map_err(|_| (400u16, "bad-request"))?;
    let host = crate::favicon::domain_of(&request.origin).ok_or((400u16, "bad-origin"))?;
    if request.password.is_empty() {
        return Err((400, "bad-request"));
    }
    let open = ctx.index.read().map_err(|_| (503u16, "unavailable"))?.vault_open;
    if !open {
        return Err((409, "vault-locked"));
    }
    if let Ok(mut saves) = ctx.saves.lock() {
        saves.push(SaveRequest { host, username: request.username.trim().to_string(), password: request.password });
    }
    Ok(serde_json::json!({ "queued": true }))
}

fn hit_of(entry: Option<&ApiEntry>, id: String, snippet: String, score: f32) -> Hit {
    Hit {
        id,
        ts: entry.and_then(|e| e.ts.clone()),
        label: entry.and_then(|e| e.label.clone()),
        category: entry.and_then(|e| e.category.clone()),
        snippet,
        score,
    }
}

/// Writes `body` as JSON with `code`. A response that cannot be sent (the
/// client hung up) is dropped — never a panic on the server thread.
///
/// The serialized bytes are LENT to the response, never moved into it:
/// `from_string` would hand the allocation over and leave `/reveal`'s password
/// (or `/unlock`'s token) in freed heap once the response drops. Here the
/// buffer outlives the send and is wiped on the way out of this function.
fn respond(request: tiny_http::Request, code: u16, body: &serde_json::Value) {
    let text = Zeroizing::new(
        serde_json::to_string(body).unwrap_or_else(|_| "{\"error\":\"internal\"}".into()),
    );
    let mut headers = Vec::new();
    if let Ok(header) =
        tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
    {
        headers.push(header);
    }
    let response = tiny_http::Response::new(
        tiny_http::StatusCode(code),
        headers,
        std::io::Cursor::new(text.as_bytes()),
        Some(text.len()),
        None,
    );
    let _ = request.respond(response);
}

/// Every test that binds a real loopback port takes this first. `cargo test`
/// runs tests in parallel, and a port freed by one is fair game for another the
/// instant it closes — which would make "is anything still listening?" answer
/// about somebody else's server. One at a time.
#[cfg(test)]
pub static PORT_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Takes [`PORT_GUARD`], ignoring a poisoning left by an unrelated failure (the
/// mutex guards a port, not any state a panic could have corrupted).
#[cfg(test)]
pub fn port_guard() -> std::sync::MutexGuard<'static, ()> {
    PORT_GUARD.lock().unwrap_or_else(|e| e.into_inner())
}

/// Waits (briefly, bounded) for `port` to stop accepting connections. The
/// listening socket is closed by `tiny_http`'s own accept thread, which the
/// `Server`'s drop only POKES — so "stopped" is true a moment after the join,
/// not on its instruction. Answers whether it really went.
#[cfg(test)]
pub fn wait_port_closed(port: u16) -> bool {
    for _ in 0..400 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    false
}

/// One request to a running server, by hand: no HTTP client crate, and
/// `Connection: close` so reading to EOF is reading the whole response.
/// Returns `(status, body)`. Test-only, and shared with `ui`'s lifecycle tests.
#[cfg(test)]
pub fn probe(
    port: u16,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<&str>,
) -> (u16, String) {
    use std::io::Write;
    let mut stream =
        std::net::TcpStream::connect(("127.0.0.1", port)).expect("the server is listening");
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10))).ok();
    let body = body.unwrap_or("");
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n",
        body.len()
    );
    if let Some(token) = token {
        head.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).expect("wrote the head");
    stream.write_all(body.as_bytes()).expect("wrote the body");
    stream.flush().ok();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).expect("read the answer");
    let status = raw.split_whitespace().nth(1).and_then(|c| c.parse::<u16>().ok()).unwrap_or(0);
    let body = raw.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_default();
    (status, body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rag::MockEmbedder;

    /// The password that must never appear in a response body. A literal, so a
    /// `contains` over the raw JSON is the whole assertion.
    const SECRET: &str = "hunter2-CORRECT-HORSE-BATTERY";

    use super::probe as request;

    /// A server on an OS-picked port with the everyday policy: no `/unlock`, no
    /// `/reveal`, no vault directory to reach — the shape every search-only
    /// test wants, and the shape the app runs in out of the box.
    fn start(index: ApiIndex) -> ApiServer {
        ApiServer::start(0, index, PathBuf::new(), ApiPolicy::default())
            .expect("bound a free loopback port")
    }

    /// A snapshot the app would build: one memorandum message and one item —
    /// the item's text carrying everything BUT its password, exactly as
    /// `rag::item_embed_text` builds it.
    fn snapshot() -> ApiIndex {
        let memo_text = "le chiavi di casa sono sotto il tappeto";
        let item_text = "Acme\nLogin\nusername\nbruno@example.com\npassword\nurl\nacme.example.com";
        let embedder = MockEmbedder;
        let vectors = embedder
            .embed_passages(&[memo_text.to_string(), item_text.to_string()])
            .expect("the mock embeds");
        ApiIndex {
            memo: vec![ApiEntry {
                id: "m1".to_string(),
                ts: Some("2026-08-19T18:00:00Z".to_string()),
                label: None,
                category: None,
                caption: Some("lo scontrino".to_string()),
                text: memo_text.to_string(),
                vec: Some(vectors[0].clone()),
            }],
            items: vec![ApiEntry {
                id: "i1".to_string(),
                ts: None,
                label: Some("Acme".to_string()),
                category: Some("Login".to_string()),
                caption: None,
                text: item_text.to_string(),
                vec: Some(vectors[1].clone()),
            }],
            embedder: Some(Arc::new(MockEmbedder)),
            // the everyday snapshot: `api_reveal` is off, so the app put no
            // value in here at all and there is nothing for a bug to send.
            reveal: Vec::new(),
            logins: Vec::new(),
            vault_open: true,
        }
    }

    #[test]
    fn the_server_answers_on_loopback_only_with_the_session_token() {
        // port 0: the OS picks a free one, so the suite never fights the user's
        // machine for 7737 and two runs can never collide.
        let _guard = port_guard();
        let server = start(snapshot());
        let port = server.port();
        let token = server.token();

        // /health is the one unauthenticated endpoint, and says nothing else
        let (status, body) = request(port, "GET", "/health", None, None);
        assert_eq!(status, 200);
        assert_eq!(body.trim(), "{\"status\":\"ok\"}", "nothing but 'ok'");

        // …everything else needs the token, and a failure says NOTHING
        let query = "{\"scope\":\"memo\",\"query\":\"chiavi\",\"mode\":\"literal\"}";
        for wrong in [None, Some(""), Some("not-the-token"), Some(&token[..63])] {
            let (status, body) = request(port, "POST", "/search", wrong, Some(query));
            assert_eq!(status, 401, "a bad token is refused: {body}");
            assert_eq!(body.trim(), "{\"error\":\"unauthorized\"}", "…with no detail");
        }

        // a literal search finds the message, with its timestamp and a snippet
        let (status, body) = request(port, "POST", "/search", Some(&token), Some(query));
        assert_eq!(status, 200, "{body}");
        let json: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
        let hits = json["hits"].as_array().expect("a hits array");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["id"], "m1");
        assert_eq!(hits[0]["ts"], "2026-08-19T18:00:00Z");
        assert!(hits[0]["snippet"].as_str().unwrap().contains("chiavi"));

        // the RAG mode runs through the embedder the snapshot carries
        let rag = "{\"scope\":\"memo\",\"query\":\"dove sono le chiavi\",\"mode\":\"rag\",\"k\":1}";
        let (status, body) = request(port, "POST", "/search", Some(&token), Some(rag));
        assert_eq!(status, 200, "{body}");
        assert_eq!(serde_json::from_str::<serde_json::Value>(&body).unwrap()["hits"][0]["id"], "m1");

        // malformed JSON is a 400, not a panic on the server thread
        for bad in ["}{", "", "{\"scope\":\"memo\"}", "{\"scope\":\"vault\",\"query\":\"x\"}"] {
            let (status, _) = request(port, "POST", "/search", Some(&token), Some(bad));
            assert!((400..500).contains(&status), "{bad:?} answered {status}");
        }
        // …and the server is still up after all of them
        assert_eq!(request(port, "GET", "/health", None, None).0, 200);
        server.stop();
    }

    #[test]
    fn no_response_body_can_carry_a_secret_value() {
        // structural, not hopeful: the snapshot has no secret in it to serve.
        // The test greps the RAW JSON anyway, because that is the thing that
        // would actually be on the wire.
        let _guard = port_guard();
        let mut index = snapshot();
        // an agent asking for the password by name must not find it either
        index.items[0].text.push_str("\nnote: la password è nel gestore");
        let server = start(index);
        let port = server.port();
        let token = server.token();
        let bodies: Vec<String> = [
            "{\"scope\":\"items\",\"query\":\"password\",\"mode\":\"literal\"}",
            "{\"scope\":\"items\",\"query\":\"acme\",\"mode\":\"literal\"}",
            "{\"scope\":\"items\",\"query\":\"hunter2\",\"mode\":\"literal\"}",
            "{\"scope\":\"items\",\"query\":\"la password di acme\",\"mode\":\"rag\"}",
            "{\"scope\":\"memo\",\"query\":\"chiavi\",\"mode\":\"rag\"}",
        ]
        .iter()
        .map(|q| {
            let (status, body) = request(port, "POST", "/search", Some(&token), Some(q));
            assert_eq!(status, 200, "{q} -> {body}");
            body
        })
        .collect();
        for body in &bodies {
            assert!(!body.contains(SECRET), "a secret value went out: {body}");
            assert!(!body.contains("hunter2"), "…not even a piece of one: {body}");
        }
        // the item IS findable by its non-secret text, so the absence above is
        // the filter working and not an empty index
        let found: serde_json::Value = serde_json::from_str(&bodies[1]).unwrap();
        assert_eq!(found["hits"][0]["label"], "Acme");
        server.stop();
    }

    #[test]
    fn a_stopped_server_refuses_the_connection_it_was_taking() {
        let _guard = port_guard();
        let server = start(snapshot());
        let port = server.port();
        assert_eq!(request(port, "GET", "/health", None, None).0, 200);
        server.stop();
        // the socket is closed, so this is a refused connection and not a
        // timeout — nothing is listening on that port any more.
        assert!(wait_port_closed(port), "port {port} is still accepting after stop");
    }

    #[test]
    fn a_client_that_stalls_mid_body_wedges_neither_the_loop_nor_the_shutdown() {
        // `tiny_http` sets NO read timeout and, past a small body, hands the
        // handler a reader over the live socket — and dropping the request
        // drains whatever length was declared, 401 or not. Answered on the
        // accept loop, one silent peer (no token needed) would freeze the join
        // in `shutdown`, and with it Ctrl+L, closing the vault and quitting.
        use std::io::Write;
        let _guard = port_guard();
        let server = start(snapshot());
        let port = server.port();
        let mut stalled =
            std::net::TcpStream::connect(("127.0.0.1", port)).expect("the server is listening");
        // the promise of a body, and then nothing — no token anywhere
        stalled
            .write_all(b"POST /search HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 65536\r\n\r\n")
            .expect("wrote the head");
        stalled.flush().ok();

        // the loop is still taking requests while that one hangs
        assert_eq!(request(port, "GET", "/health", None, None).0, 200, "the loop is wedged");
        let start = std::time::Instant::now();
        server.stop();
        let waited = start.elapsed();
        assert!(waited < std::time::Duration::from_secs(10), "the shutdown waited {waited:?}");
        assert!(wait_port_closed(port), "port {port} is still accepting after stop");
        drop(stalled); // the parked handler is released with the socket
    }

    #[test]
    fn a_rag_search_with_no_engine_says_so_instead_of_hanging() {
        let _guard = port_guard();
        let mut index = snapshot();
        index.embedder = None; // the model is not on this machine
        let server = start(index);
        let port = server.port();
        let token = server.token();
        let (status, body) = request(
            port,
            "POST",
            "/search",
            Some(&token),
            Some("{\"scope\":\"memo\",\"query\":\"chiavi\",\"mode\":\"rag\"}"),
        );
        assert_eq!(status, 503);
        assert_eq!(body.trim(), "{\"error\":\"model-not-ready\"}");
        // …and with no mode asked for it falls to the literal search, which
        // needs no model at all: an agent always gets an answer.
        let (status, body) = request(
            port,
            "POST",
            "/search",
            Some(&token),
            Some("{\"scope\":\"memo\",\"query\":\"chiavi\"}"),
        );
        assert_eq!(status, 200, "{body}");
        assert_eq!(serde_json::from_str::<serde_json::Value>(&body).unwrap()["hits"][0]["id"], "m1");
        server.stop();
    }

    // ---- per-vault sessions: /unlock, /search, /reveal, /lock --------------

    use crate::crypto::{Algorithm, Argon2Variant, KdfSpec};
    use crate::memo::MemoMessage;
    use crate::model::{FieldType, Item, ItemField, SecretString};
    use tempfile::TempDir;

    /// The weakest sane Argon2 — the same parameters `ui::app_with` builds its
    /// vaults with, so a REAL unlock (a real KDF pass, a real AEAD open) through
    /// the API costs the suite milliseconds.
    fn fast_kdf() -> KdfSpec {
        KdfSpec::Argon2 { variant: Argon2Variant::Id, m_cost_kib: 8, t_cost: 1, p_cost: 1 }
    }

    /// A real vault file for `name`, password `pw`: one Acme login whose
    /// password is [`SECRET`], one image field, and one memorandum message.
    /// Answers the item's id, which is what `/reveal` is asked for.
    fn make_vault(dir: &TempDir, name: &str) -> String {
        let mut vault =
            Vault::create(dir.path(), name, "pw", Algorithm::Aes256Gcm, fast_kdf(), "Vanilla")
                .expect("created");
        let mut item = Item::new("login", "Acme");
        item.fields = vec![
            ItemField {
                key: "username".into(),
                ftype: FieldType::Username,
                label: None,
                value: SecretString::new("bruno@example.com"),
                secret: false,
                extra: serde_json::Map::new(),
            },
            ItemField {
                key: "password".into(),
                ftype: FieldType::Password,
                label: None,
                value: SecretString::new(SECRET),
                secret: true,
                extra: serde_json::Map::new(),
            },
            ItemField {
                key: "url".into(),
                ftype: FieldType::Url,
                label: None,
                value: SecretString::new("https://acme.example.com/"),
                secret: false,
                extra: serde_json::Map::new(),
            },
            ItemField {
                key: "scan".into(),
                ftype: FieldType::Image,
                label: Some("passport".into()),
                // stands in for a megabyte of base64: what matters is that it
                // is NOT what comes back out.
                value: SecretString::new("AAAABBBBCCCC"),
                secret: true,
                extra: serde_json::Map::from_iter([(
                    crate::image_field::META_KEY.to_string(),
                    serde_json::json!({ "mime": "image/png", "bytes": 4096u64 }),
                )]),
            },
        ];
        let id = item.id.clone();
        vault.items_mut().push(item);
        vault.content.push_memo_message(MemoMessage::new("le chiavi sono sotto il tappeto", None));
        vault.save().expect("saved");
        id
    }

    fn policy(allow_unlock: bool, allow_reveal: bool, ttl: Option<Duration>) -> ApiPolicy {
        ApiPolicy { allow_unlock, allow_reveal, allow_login: false, session_ttl: ttl, lang: Lang::En }
    }

    /// A server over a real vault directory, with an engine but an EMPTY
    /// snapshot: nothing is open in the TUI, which is the whole point of the
    /// unlock endpoint.
    fn session_server(dir: &TempDir, policy: ApiPolicy) -> ApiServer {
        let index = ApiIndex { embedder: Some(Arc::new(MockEmbedder)), ..ApiIndex::default() };
        ApiServer::start(0, index, dir.path().to_path_buf(), policy).expect("bound")
    }

    /// The token out of a successful `/unlock` body.
    fn token_of(body: &str) -> String {
        serde_json::from_str::<serde_json::Value>(body)
            .expect("valid JSON")["token"]
            .as_str()
            .expect("a token")
            .to_string()
    }

    #[test]
    fn an_agent_unlocks_a_vault_by_password_and_then_needs_no_vault_name_at_all() {
        let _guard = port_guard();
        let dir = TempDir::new().unwrap();
        make_vault(&dir, "bruno");
        let server = session_server(&dir, policy(true, false, None));
        let (port, admin) = (server.port(), server.token());

        // the vault is opened with the password an agent already holds
        let (status, body) = request(
            port,
            "POST",
            "/unlock",
            Some(&admin),
            Some("{\"vault\":\"bruno\",\"password\":\"pw\"}"),
        );
        assert_eq!(status, 200, "{body}");
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(json["expires_in"].is_null(), "expiry off means null, not a number");
        let token = token_of(&body);
        assert_eq!(token.len(), 64, "32 bytes, hex");
        assert_ne!(token, *admin, "a session token is not the admin one");
        assert_eq!(server.session_count(), 1);

        // …and from here the token IS the scope: no request names a vault, and
        // a name smuggled into a body changes nothing (there is nothing to
        // parse it into — see `SearchRequest`).
        for query in [
            "{\"scope\":\"items\",\"query\":\"acme\",\"mode\":\"literal\"}",
            "{\"scope\":\"items\",\"query\":\"acme\",\"mode\":\"literal\",\"vault\":\"qualcun-altro\"}",
        ] {
            let (status, body) = request(port, "POST", "/search", Some(&token), Some(query));
            assert_eq!(status, 200, "{body}");
            let json: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert_eq!(json["hits"][0]["label"], "Acme");
            assert_eq!(
                json["hits"][0]["category"],
                i18n::t(Lang::En, &Key::CategoryName("login")),
                "the category rides along LOCALIZED — it is what a search would use"
            );
            // the vault has no stored vectors, so the honest answer says which
            // search really ran rather than pretending to be the good one
            assert_eq!(json["mode"], "literal");
            assert!(!body.contains(SECRET), "a secret value went out: {body}");
        }
        // the memorandum of THAT vault, by its words
        let (status, memo) = request(
            port,
            "POST",
            "/search",
            Some(&token),
            Some("{\"scope\":\"memo\",\"query\":\"chiavi\"}"),
        );
        assert_eq!(status, 200, "{memo}");
        let json: serde_json::Value = serde_json::from_str(&memo).unwrap();
        assert!(json["hits"][0]["snippet"].as_str().unwrap().contains("chiavi"));
        assert_eq!(json["mode"], "literal", "no vectors stored, and it says so");

        // an empty query is a list and not a search result, and it is answered
        // BEFORE the scope is copied out of the vault (a memorandum copy carries
        // every stored photo). What could refuse the request still does.
        let (status, body) =
            request(port, "POST", "/search", Some(&token), Some("{\"scope\":\"memo\",\"query\":\" \"}"));
        assert_eq!(status, 200, "{body}");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&body).unwrap()["hits"].as_array().map(Vec::len),
            Some(0)
        );
        assert_eq!(
            request(port, "POST", "/search", Some(&token), Some("{\"scope\":\"vault\",\"query\":\"\"}")).0,
            400,
            "an unknown scope is a bad request whatever the query says"
        );

        // NOTHING a search can be asked answers with the password
        for query in [
            "{\"scope\":\"items\",\"query\":\"password\",\"mode\":\"literal\"}",
            "{\"scope\":\"items\",\"query\":\"hunter2\",\"mode\":\"literal\"}",
            "{\"scope\":\"items\",\"query\":\"la password di acme\"}",
            "{\"scope\":\"items\",\"query\":\"scan\",\"mode\":\"literal\"}",
        ] {
            let (status, body) = request(port, "POST", "/search", Some(&token), Some(query));
            assert_eq!(status, 200, "{query} -> {body}");
            assert!(!body.contains(SECRET), "the password went out: {body}");
            assert!(!body.contains("hunter2"), "…not even a piece of it: {body}");
            assert!(!body.contains("AAAABBBB"), "…and neither did the picture: {body}");
        }

        // /lock is the end of it: the token stops being a token
        let (status, body) = request(port, "POST", "/lock", Some(&token), Some("{}"));
        assert_eq!(status, 200, "{body}");
        assert_eq!(body.trim(), "{\"locked\":true}");
        assert_eq!(server.session_count(), 0, "the key and the content went with it");
        let (status, body) = request(
            port,
            "POST",
            "/search",
            Some(&token),
            Some("{\"scope\":\"items\",\"query\":\"acme\"}"),
        );
        assert_eq!(status, 401, "a locked session is nobody");
        assert_eq!(body.trim(), "{\"error\":\"unauthorized\"}");
        server.stop();
    }

    #[test]
    fn a_wrong_password_and_a_vault_that_does_not_exist_are_the_same_refusal() {
        // an agent that could tell these apart would have a vault-name oracle,
        // and the list of somebody's vaults is not this endpoint's to hand out.
        let _guard = port_guard();
        let dir = TempDir::new().unwrap();
        make_vault(&dir, "bruno");
        let server = session_server(&dir, policy(true, false, None));
        let (port, admin) = (server.port(), server.token());

        let wrong = request(
            port,
            "POST",
            "/unlock",
            Some(&admin),
            Some("{\"vault\":\"bruno\",\"password\":\"sbagliata\"}"),
        );
        let unknown = request(
            port,
            "POST",
            "/unlock",
            Some(&admin),
            Some("{\"vault\":\"non-esiste\",\"password\":\"pw\"}"),
        );
        let empty = request(
            port,
            "POST",
            "/unlock",
            Some(&admin),
            Some("{\"vault\":\"\",\"password\":\"pw\"}"),
        );
        assert_eq!(wrong.0, 401);
        assert_eq!(wrong, unknown, "byte for byte the same answer");
        assert_eq!(wrong, empty);
        assert_eq!(wrong.1.trim(), "{\"error\":\"unauthorized\"}", "…and it says nothing");
        assert_eq!(server.session_count(), 0, "no failure minted anything");
        // …and none of them named the vault back
        assert!(!wrong.1.contains("bruno") && !unknown.1.contains("non-esiste"));
        server.stop();
    }

    #[test]
    fn a_vault_file_that_cannot_be_read_is_that_same_refusal_and_not_a_status_of_its_own() {
        // this used to answer 500, which was the oracle twice over: corruption
        // found BEFORE the decrypt is reached with no password at all (so the
        // 500 confirmed the name), and corruption found AFTER it is reached only
        // with the right one (so the 500 confirmed the guess).
        let _guard = port_guard();
        let dir = TempDir::new().unwrap();
        make_vault(&dir, "bruno");
        make_vault(&dir, "rotta");
        std::fs::write(vault::vault_path(dir.path(), "rotta"), b"{\"version\":99}")
            .expect("clobbered the file");
        let server = session_server(&dir, policy(true, false, None));
        let (port, admin) = (server.port(), server.token());
        let ask = |name: &str, pw: &str| {
            let body = format!("{{\"vault\":\"{name}\",\"password\":\"{pw}\"}}");
            request(port, "POST", "/unlock", Some(&admin), Some(&body))
        };
        let wrong = ask("bruno", "sbagliata");
        let broken = ask("rotta", "qualsiasi");
        assert_eq!(wrong.0, 401);
        assert_eq!(broken, wrong, "an unreadable file answered for its own name");
        assert!(!broken.1.contains("rotta") && !broken.1.contains("version"));
        assert_eq!(server.session_count(), 0);
        server.stop();
    }

    #[test]
    fn every_refused_unlock_answers_on_the_same_clock() {
        // byte-identical answers a stopwatch can separate are the vault-name
        // oracle with one extra step: a name with no file is settled by a
        // `stat`, a wrong password by a whole KDF pass. The vault here is built
        // with the WEAKEST Argon2 (`fast_kdf`), so without the floor the two
        // would be three orders of magnitude apart.
        let _guard = port_guard();
        let dir = TempDir::new().unwrap();
        make_vault(&dir, "bruno");
        let server = session_server(&dir, policy(true, false, None));
        let (port, admin) = (server.port(), server.token());
        let timed = |name: &str| {
            let body = format!("{{\"vault\":\"{name}\",\"password\":\"sbagliata\"}}");
            let started = Instant::now();
            let answer = request(port, "POST", "/unlock", Some(&admin), Some(&body));
            (answer, started.elapsed())
        };
        let (wrong, wrong_took) = timed("bruno");
        let (unknown, unknown_took) = timed("non-esiste");
        assert_eq!(wrong.0, 401);
        assert_eq!(wrong, unknown, "byte for byte the same answer");
        for took in [wrong_took, unknown_took] {
            assert!(took >= UNLOCK_REFUSAL_FLOOR, "a refusal came back in {took:?}");
        }
        server.stop();
    }

    #[test]
    fn reveal_is_the_one_endpoint_a_secret_leaves_through_and_it_is_off_by_default() {
        let _guard = port_guard();
        let dir = TempDir::new().unwrap();
        let id = make_vault(&dir, "bruno");
        // …and it starts OFF, as it does out of the box
        let server = session_server(&dir, policy(true, false, None));
        let (port, admin) = (server.port(), server.token());
        let (_, body) = request(
            port,
            "POST",
            "/unlock",
            Some(&admin),
            Some("{\"vault\":\"bruno\",\"password\":\"pw\"}"),
        );
        let token = token_of(&body);
        let ask = format!("{{\"id\":\"{id}\"}}");

        // switched off, the endpoint is not forbidden — it is NOT THERE, and
        // says exactly what an unknown path says (the answer never reports the
        // state of the user's settings)
        let off = request(port, "POST", "/reveal", Some(&token), Some(&ask));
        let nowhere = request(port, "POST", "/nonsense", Some(&token), Some("{}"));
        assert_eq!(off.0, 404);
        assert_eq!(off, nowhere, "a switched-off endpoint is indistinguishable from no endpoint");
        // …and the admin token gets the same nothing
        assert_eq!(request(port, "POST", "/reveal", Some(&admin), Some(&ask)), off);

        // the user turns it on: NOW the value crosses, and only now
        server.set_policy(policy(true, true, None));
        let (status, body) = request(port, "POST", "/reveal", Some(&token), Some(&ask));
        assert_eq!(status, 200, "{body}");
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["label"], "Acme");
        let fields = json["fields"].as_array().expect("an array");
        let password = fields.iter().find(|f| f["key"] == "password").expect("the field");
        assert_eq!(password["value"], SECRET, "this is the endpoint that sends it");
        assert_eq!(password["type"], "password");
        assert_eq!(password["secret"], true);
        // a picture is a NUMBER, never its bytes — a scanned passport is not
        // something to push through a JSON body
        let scan = fields.iter().find(|f| f["key"] == "scan").expect("the image field");
        assert_eq!(scan["type"], "image");
        assert_eq!(scan["bytes"], 4096);
        assert_eq!(scan["label"], "passport");
        assert!(scan.get("value").is_none(), "no payload: {scan}");
        assert!(!body.contains("AAAABBBB"), "…not anywhere in the body: {body}");

        // one field at a time, when that is what was asked for
        let one = format!("{{\"id\":\"{id}\",\"field\":\"password\"}}");
        let (status, body) = request(port, "POST", "/reveal", Some(&token), Some(&one));
        assert_eq!(status, 200);
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["fields"].as_array().map(Vec::len), Some(1));
        assert_eq!(json["fields"][0]["value"], SECRET);

        // an item (or a field) that is not there is the same generic 404
        let missing = format!("{{\"id\":\"{id}\",\"field\":\"nope\"}}");
        assert_eq!(request(port, "POST", "/reveal", Some(&token), Some(&missing)).0, 404);
        assert_eq!(
            request(port, "POST", "/reveal", Some(&token), Some("{\"id\":\"nessuno\"}")).0,
            404
        );
        server.stop();
    }

    #[test]
    fn a_session_expires_when_it_is_left_alone_and_slides_when_it_is_not() {
        let _guard = port_guard();
        let dir = TempDir::new().unwrap();
        make_vault(&dir, "bruno");
        // a TTL a test can outlive on purpose. Short, but with room: the
        // "still alive" half below pokes it every third of it, so the check
        // holds unless one loopback request takes four tenths of a second.
        let ttl = Duration::from_millis(600);
        let server = session_server(&dir, policy(true, false, Some(ttl)));
        let (port, admin) = (server.port(), server.token());
        let (status, body) = request(
            port,
            "POST",
            "/unlock",
            Some(&admin),
            Some("{\"vault\":\"bruno\",\"password\":\"pw\"}"),
        );
        assert_eq!(status, 200, "{body}");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&body).unwrap()["expires_in"],
            ttl.as_secs(),
            "the answer says how long it has"
        );
        let token = token_of(&body);
        let search = "{\"scope\":\"items\",\"query\":\"acme\",\"mode\":\"literal\"}";

        // SLIDING: kept busy, it stays alive well past one TTL
        for i in 0..4 {
            std::thread::sleep(ttl / 3);
            let (status, body) = request(port, "POST", "/search", Some(&token), Some(search));
            assert_eq!(status, 200, "poke {i} was refused after {:?}: {body}", ttl / 3 * (i + 1));
        }
        // …left alone, it is gone, and the token that opened it is nobody
        std::thread::sleep(ttl + ttl / 2);
        let (status, body) = request(port, "POST", "/search", Some(&token), Some(search));
        assert_eq!(status, 401, "an expired token still worked");
        assert_eq!(body.trim(), "{\"error\":\"unauthorized\"}", "…and it is not told why");
        assert_eq!(server.session_count(), 0, "the reap on the request path took it");
        server.stop();
    }

    #[test]
    fn the_ninth_vault_an_agent_tries_to_hold_open_is_refused() {
        let _guard = port_guard();
        let dir = TempDir::new().unwrap();
        for i in 0..=MAX_SESSIONS {
            make_vault(&dir, &format!("v{i}"));
        }
        let server = session_server(&dir, policy(true, false, None));
        let (port, admin) = (server.port(), server.token());
        for i in 0..MAX_SESSIONS {
            let body = format!("{{\"vault\":\"v{i}\",\"password\":\"pw\"}}");
            let (status, body) = request(port, "POST", "/unlock", Some(&admin), Some(&body));
            assert_eq!(status, 200, "session {i} was refused: {body}");
        }
        assert_eq!(server.session_count(), MAX_SESSIONS);
        // the ninth is refused rather than evicting somebody else's
        let ninth = format!("{{\"vault\":\"v{MAX_SESSIONS}\",\"password\":\"pw\"}}");
        let (status, _) = request(port, "POST", "/unlock", Some(&admin), Some(&ninth));
        assert_eq!(status, 409);
        assert_eq!(server.session_count(), MAX_SESSIONS, "and nothing was thrown out for it");
        server.stop();
    }

    #[test]
    fn a_token_only_opens_the_doors_its_own_kind_has() {
        let _guard = port_guard();
        let dir = TempDir::new().unwrap();
        make_vault(&dir, "bruno");
        make_vault(&dir, "anna");
        let server = session_server(&dir, policy(true, false, None));
        let (port, admin) = (server.port(), server.token());
        let unlock = |name: &str, token: &str| {
            let body = format!("{{\"vault\":\"{name}\",\"password\":\"pw\"}}");
            request(port, "POST", "/unlock", Some(token), Some(&body))
        };
        let bruno = token_of(&unlock("bruno", &admin).1);
        let anna = token_of(&unlock("anna", &admin).1);
        assert_eq!(server.session_count(), 2);

        // a session cannot mint another session — no privilege climbs here
        assert_eq!(unlock("anna", &bruno).0, 404);
        // …the admin has no session, so it has nothing to lock
        assert_eq!(request(port, "POST", "/lock", Some(&admin), Some("{}")).0, 404);
        // …and the admin's own scope is the (empty) TUI snapshot, not a session's
        let (status, body) = request(
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
            "nothing is open in the TUI, so the admin sees nothing"
        );

        // a re-key of ONE vault kills its sessions and leaves the other alone
        assert_eq!(server.drop_sessions_for("bruno"), 1);
        assert_eq!(
            request(port, "POST", "/search", Some(&bruno), Some("{\"scope\":\"items\",\"query\":\"a\"}")).0,
            401
        );
        assert_eq!(
            request(port, "POST", "/search", Some(&anna), Some("{\"scope\":\"items\",\"query\":\"a\"}")).0,
            200,
            "anna's session was not the one that was re-keyed"
        );
        // …and the panic switch takes what is left
        server.drop_all_sessions();
        assert_eq!(server.session_count(), 0);
        server.stop();
    }

    #[test]
    fn with_unlock_off_the_endpoint_is_simply_not_there() {
        let _guard = port_guard();
        let dir = TempDir::new().unwrap();
        make_vault(&dir, "bruno");
        let server = session_server(&dir, ApiPolicy::default());
        let (port, admin) = (server.port(), server.token());
        let ask = "{\"vault\":\"bruno\",\"password\":\"pw\"}";
        let off = request(port, "POST", "/unlock", Some(&admin), Some(ask));
        assert_eq!(off.0, 404);
        assert_eq!(off, request(port, "POST", "/nonsense", Some(&admin), Some("{}")));
        assert_eq!(server.session_count(), 0, "and the password was never even tried");
        // the user turns it on, and the same request now opens the vault
        server.set_policy(policy(true, false, None));
        assert_eq!(request(port, "POST", "/unlock", Some(&admin), Some(ask)).0, 200);
        assert_eq!(server.session_count(), 1);
        // stopping the server takes every session with it
        server.stop();
    }

    #[test]
    fn a_token_is_thirty_two_random_bytes_of_hex_and_compares_exactly() {
        let token = new_token();
        assert_eq!(token.len(), 64, "32 bytes, hex");
        assert!(token.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_ne!(*new_token(), *token, "a token is not a constant");
        assert!(token_matches(&token, &token));
        assert!(!token_matches(&token, &token[..63]), "a prefix is not the token");
        assert!(!token_matches(&token, ""), "…and neither is nothing");
        // an empty expectation can never be matched: a server with no token is
        // a server nobody can talk to, not one everybody can.
        assert!(!token_matches("", ""));
    }
    // ---- /login/*: the browser extension ----------------------------------

    #[test]
    fn a_page_belongs_to_a_domain_only_as_itself_or_a_subdomain() {
        assert!(domain_matches("https://google.com/signin", "google.com"));
        assert!(domain_matches("https://accounts.google.com/v3/signin", "google.com"));
        assert!(domain_matches("ACCOUNTS.GOOGLE.COM", "Google.com"), "case-blind");
        assert!(!domain_matches("https://notgoogle.com/", "google.com"), "a suffix is not a subdomain");
        assert!(!domain_matches("https://google.com.evil.io/", "google.com"), "…nor is a prefix");
        assert!(!domain_matches("https://example.com/", "google.com"));
        assert!(!domain_matches("", "google.com"));
        assert!(!domain_matches("not a url", "google.com"));
        // `www.` on the stored site is not a different site
        assert!(domain_matches("https://github.com/login", "www.github.com"));
        // a service that signs in on its owner's domain: Gmail is Google's…
        assert!(domain_matches("https://accounts.google.com/v3/signin/identifier", "gmail.com"));
        assert!(domain_matches("https://login.live.com/", "outlook.com"));
        assert!(domain_matches("https://login.microsoftonline.com/", "hotmail.com"));
        // …and the table never widens anything else
        assert!(!domain_matches("https://accounts.google.com/", "github.com"));
        assert!(!domain_matches("https://evil-google.com/", "gmail.com"));
        assert!(!domain_matches("https://gmail.com.evil.io/", "gmail.com"));
    }

    /// A login-capable item needs a website, a user row and a non-empty
    /// password row; everything else is simply not offered.
    #[test]
    fn only_items_that_can_log_in_become_login_entries() {
        let dir = TempDir::new().unwrap();
        make_vault(&dir, "bruno");
        let vault = Vault::open(dir.path(), "bruno", "pw").unwrap();
        let entries = login_entries(vault.items());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].domain, "acme.example.com");
        assert_eq!(entries[0].username, "bruno@example.com");
        assert_eq!(entries[0].password.as_str(), SECRET);
        // no website → not a login; an empty password → not a login
        let mut bare = Item::new("login", "Bare");
        bare.fields = vec![ItemField {
            key: "password".into(),
            ftype: FieldType::Password,
            label: None,
            value: SecretString::new("x"),
            secret: true,
            extra: serde_json::Map::new(),
        }];
        assert!(login_entries(std::slice::from_ref(&bare)).is_empty());
    }

    /// A server whose snapshot carries the test vault's logins, with
    /// `login_assist` on and a pairing token installed.
    fn login_server(dir: &TempDir) -> (ApiServer, String) {
        let vault = Vault::open(dir.path(), "bruno", "pw").unwrap();
        let index = ApiIndex { logins: login_entries(vault.items()), ..ApiIndex::default() };
        let mut policy = policy(false, false, None);
        policy.allow_login = true;
        let server = ApiServer::start(0, index, dir.path().to_path_buf(), policy).expect("bound");
        let pairing = new_token().as_str().to_string();
        server.set_login_token(Some(&pairing));
        (server, pairing)
    }

    #[test]
    fn match_names_the_logins_of_a_site_and_fill_hands_one_over() {
        let _guard = port_guard();
        let dir = TempDir::new().unwrap();
        let id = make_vault(&dir, "bruno");
        let (server, pairing) = login_server(&dir);
        let port = server.port();
        // the item's own site, and a page on a subdomain of it: both match
        for origin in ["https://acme.example.com/login", "https://sso.acme.example.com/"] {
            let body = format!("{{\"origin\":\"{origin}\"}}");
            let (status, body) = request(port, "POST", "/login/match", Some(&pairing), Some(&body));
            assert_eq!(status, 200, "{origin} -> {body}");
            let json: serde_json::Value = serde_json::from_str(&body).unwrap();
            let logins = json["logins"].as_array().expect("an array");
            assert_eq!(logins.len(), 1, "{origin}");
            assert_eq!(logins[0]["id"], id);
            assert_eq!(logins[0]["username"], "bruno@example.com");
            assert!(!body.contains(SECRET), "a match never carries a password: {body}");
            assert!(logins[0].get("password").is_none());
        }
        // another site sees nothing; a non-origin is a bad request
        let (status, body) = request(port, "POST", "/login/match", Some(&pairing), Some("{\"origin\":\"https://example.org/\"}"));
        assert_eq!(status, 200);
        assert_eq!(serde_json::from_str::<serde_json::Value>(&body).unwrap()["logins"].as_array().map(Vec::len), Some(0));
        assert_eq!(request(port, "POST", "/login/match", Some(&pairing), Some("{\"origin\":\"nope\"}")).0, 400);

        // the user picked it in the menu: now, and only now, the password
        let ask = format!("{{\"id\":\"{id}\",\"origin\":\"https://acme.example.com/login\"}}");
        // …for the site it belongs to, and for no other (nor with none named)
        let elsewhere = format!("{{\"id\":\"{id}\",\"origin\":\"https://evil.example.org/\"}}");
        assert_eq!(request(port, "POST", "/login/fill", Some(&pairing), Some(&elsewhere)).0, 404);
        assert_eq!(request(port, "POST", "/login/fill", Some(&pairing), Some(&format!("{{\"id\":\"{id}\"}}"))).0, 400);
        let (status, body) = request(port, "POST", "/login/fill", Some(&pairing), Some(&ask));
        assert_eq!(status, 200, "{body}");
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["username"], "bruno@example.com");
        assert_eq!(json["password"], SECRET);
        assert_eq!(request(port, "POST", "/login/fill", Some(&pairing), Some("{\"id\":\"nobody\",\"origin\":\"https://acme.example.com/\"}")).0, 404);
        server.stop();
    }

    #[test]
    fn a_claim_is_spent_once_and_only_from_the_items_own_site() {
        let _guard = port_guard();
        let dir = TempDir::new().unwrap();
        let id = make_vault(&dir, "bruno");
        let (server, pairing) = login_server(&dir);
        let port = server.port();
        // the TUI pressed `l`: one offer, claimable once
        let nonce = server.offer_login(&id);
        assert_eq!(nonce.len(), 64);
        assert_eq!(server.pending_logins(), 1);
        // from the wrong site it opens nothing — and it is spent all the same
        let wrong = format!("{{\"nonce\":\"{}\",\"origin\":\"https://evil.example.org/\"}}", nonce.as_str());
        assert_eq!(request(port, "POST", "/login/claim", Some(&pairing), Some(&wrong)).0, 404);
        assert_eq!(server.pending_logins(), 0, "a nonce that reached the wrong site is gone");
        let right = format!("{{\"nonce\":\"{}\",\"origin\":\"https://acme.example.com/signin\"}}", nonce.as_str());
        assert_eq!(request(port, "POST", "/login/claim", Some(&pairing), Some(&right)).0, 404, "…for good");
        // a fresh offer, claimed from the right site: the credentials, once
        let nonce = server.offer_login(&id);
        let right = format!("{{\"nonce\":\"{}\",\"origin\":\"https://accounts.acme.example.com/\"}}", nonce.as_str());
        let (status, body) = request(port, "POST", "/login/claim", Some(&pairing), Some(&right));
        assert_eq!(status, 200, "{body}");
        assert_eq!(serde_json::from_str::<serde_json::Value>(&body).unwrap()["password"], SECRET);
        assert_eq!(request(port, "POST", "/login/claim", Some(&pairing), Some(&right)).0, 404, "spent");
        // a nonce nobody offered, and a malformed body
        assert_eq!(request(port, "POST", "/login/claim", Some(&pairing), Some("{\"nonce\":\"x\",\"origin\":\"https://acme.example.com/\"}")).0, 404);
        assert_eq!(request(port, "POST", "/login/claim", Some(&pairing), Some("{}")).0, 400);
        server.stop();
    }

    #[test]
    fn the_pairing_token_opens_the_login_doors_and_no_other() {
        let _guard = port_guard();
        let dir = TempDir::new().unwrap();
        let id = make_vault(&dir, "bruno");
        let (server, pairing) = login_server(&dir);
        let (port, admin) = (server.port(), server.token());
        // every other endpoint is NOT THERE for it — the same 404 an unknown
        // path gets, even where the admin would be let in
        let mut policy = policy(true, true, None);
        policy.allow_login = true;
        server.set_policy(policy);
        let nowhere = request(port, "POST", "/nonsense", Some(&pairing), Some("{}"));
        for (path, body) in [
            ("/search", "{\"scope\":\"items\",\"query\":\"acme\"}"),
            ("/reveal", &format!("{{\"id\":\"{id}\"}}")),
            ("/unlock", "{\"vault\":\"bruno\",\"password\":\"pw\"}"),
            ("/lock", "{}"),
        ] {
            let answer = request(port, "POST", path, Some(&pairing), Some(body));
            assert_eq!(answer.0, 404, "{path} answered the extension");
            assert_eq!(answer, nowhere, "{path} is indistinguishable from no endpoint");
        }
        assert_eq!(server.session_count(), 0, "…and no session was minted");
        // …while the admin token walks through the login doors too
        let (status, _) = request(port, "POST", "/login/match", Some(&admin), Some("{\"origin\":\"https://acme.example.com/\"}"));
        assert_eq!(status, 200);
        // retiring the pairing token (the switch off, `logintoken new`): 401
        server.set_login_token(None);
        let (status, body) = request(port, "POST", "/login/match", Some(&pairing), Some("{\"origin\":\"https://acme.example.com/\"}"));
        assert_eq!(status, 401, "{body}");
        assert_eq!(body.trim(), "{\"error\":\"unauthorized\"}");
        // …and a wrong one never was anything
        assert_eq!(request(port, "POST", "/login/match", Some("not-it"), Some("{\"origin\":\"https://acme.example.com/\"}")).0, 401);
        server.stop();
    }

    #[test]
    fn with_login_assist_off_the_login_doors_are_simply_not_there() {
        let _guard = port_guard();
        let dir = TempDir::new().unwrap();
        let id = make_vault(&dir, "bruno");
        let (server, pairing) = login_server(&dir);
        let (port, admin) = (server.port(), server.token());
        server.set_policy(policy(true, true, None)); // allow_login: false
        let nowhere = request(port, "POST", "/nonsense", Some(&admin), Some("{}"));
        for token in [&admin[..], &pairing] {
            for (path, body) in [
                ("/login/match", "{\"origin\":\"https://acme.example.com/\"}".to_string()),
                ("/login/fill", format!("{{\"id\":\"{id}\"}}")),
                ("/login/claim", "{\"nonce\":\"x\",\"origin\":\"https://acme.example.com/\"}".to_string()),
            ] {
                let answer = request(port, "POST", path, Some(token), Some(&body));
                assert_eq!(answer.0, 404, "{path} is there with the switch off");
                if token == admin.as_str() {
                    assert_eq!(answer, nowhere);
                }
            }
        }
        server.stop();
    }

    #[test]
    fn a_session_sees_the_logins_of_its_own_vault() {
        let _guard = port_guard();
        let dir = TempDir::new().unwrap();
        let id = make_vault(&dir, "bruno");
        // nothing open in the TUI: an EMPTY snapshot, the login doors on
        let mut policy = policy(true, false, None);
        policy.allow_login = true;
        let server = ApiServer::start(0, ApiIndex::default(), dir.path().to_path_buf(), policy).expect("bound");
        let (port, admin) = (server.port(), server.token());
        let match_body = "{\"origin\":\"https://acme.example.com/\"}";
        // the admin's scope is the (empty) TUI
        let (status, body) = request(port, "POST", "/login/match", Some(&admin), Some(match_body));
        assert_eq!(status, 200);
        assert_eq!(serde_json::from_str::<serde_json::Value>(&body).unwrap()["logins"].as_array().map(Vec::len), Some(0));
        // a session brings its own vault, logins included
        let (_, body) = request(port, "POST", "/unlock", Some(&admin), Some("{\"vault\":\"bruno\",\"password\":\"pw\"}"));
        let token = token_of(&body);
        let (status, body) = request(port, "POST", "/login/match", Some(&token), Some(match_body));
        assert_eq!(status, 200, "{body}");
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["logins"][0]["id"], id);
        assert!(!body.contains(SECRET));
        let (status, body) = request(port, "POST", "/login/fill", Some(&token), Some(&format!("{{\"id\":\"{id}\"}}")));
        assert_eq!(status, 200, "{body}");
        assert_eq!(serde_json::from_str::<serde_json::Value>(&body).unwrap()["password"], SECRET);
        server.stop();
    }
    #[test]
    fn info_wake_totp_and_save_speak_for_the_app() {
        let _guard = port_guard();
        let dir = TempDir::new().unwrap();
        let id = make_vault(&dir, "bruno");
        // give the item a TOTP seed (the RFC 6238 one) so /login/totp has a code
        {
            let mut v = Vault::open(dir.path(), "bruno", "pw").unwrap();
            v.items_mut()[0].fields.push(ItemField {
                key: "totp".into(),
                ftype: FieldType::Totp,
                label: None,
                value: SecretString::new("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ"),
                secret: true,
                extra: serde_json::Map::new(),
            });
            v.save().unwrap();
        }
        let (server, pairing) = login_server(&dir);
        let port = server.port();
        let origin = "https://acme.example.com/login";

        // info: the language and whether a vault is open
        let (status, body) = request(port, "POST", "/login/info", Some(&pairing), Some("{}"));
        assert_eq!(status, 200, "{body}");
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["lang"], "en");
        assert_eq!(json["vault_open"], false, "the test snapshot never said a vault was open");

        // totp: the code of the moment, never the seed
        let ask = format!("{{\"id\":\"{id}\",\"origin\":\"{origin}\"}}");
        let (status, body) = request(port, "POST", "/login/totp", Some(&pairing), Some(&ask));
        assert_eq!(status, 200, "{body}");
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["code"].as_str().map(str::len), Some(6));
        assert!(!body.contains("GEZDGNBV"), "the seed never leaves: {body}");
        let elsewhere = format!("{{\"id\":\"{id}\",\"origin\":\"https://evil.example.org/\"}}");
        assert_eq!(request(port, "POST", "/login/totp", Some(&pairing), Some(&elsewhere)).0, 404);

        // wake: the newest host waits for the app's tick
        assert!(server.take_wake().is_none());
        let (status, _) = request(port, "POST", "/login/wake", Some(&pairing), Some("{\"origin\":\"https://github.com/login\"}"));
        assert_eq!(status, 200);
        assert_eq!(server.take_wake().map(|w| w.host), Some("github.com".to_string()));
        assert!(server.take_wake().is_none(), "taken once");
        assert_eq!(request(port, "POST", "/login/wake", Some(&pairing), Some("{\"origin\":\"nope\"}")).0, 400);

        // save: refused while no vault is open, queued once one is
        let save = "{\"origin\":\"https://github.com/login\",\"username\":\"bruno\",\"password\":\"hunter2\"}";
        assert_eq!(request(port, "POST", "/login/save", Some(&pairing), Some(save)).0, 409);
        assert!(server.take_saves().is_empty());
        server.refresh(ApiIndex { vault_open: true, ..ApiIndex::default() });
        assert_eq!(request(port, "POST", "/login/save", Some(&pairing), Some(save)).0, 200);
        let saves = server.take_saves();
        assert_eq!(saves.len(), 1);
        assert_eq!(saves[0].host, "github.com");
        assert_eq!(saves[0].username, "bruno");
        assert_eq!(saves[0].password.as_str(), "hunter2");
        assert!(server.take_saves().is_empty(), "taken once");
        // an empty password is nothing to keep
        assert_eq!(request(port, "POST", "/login/save", Some(&pairing), Some("{\"origin\":\"https://github.com/\",\"username\":\"b\",\"password\":\"\"}")).0, 400);
        // …and with login assist off, none of the four exists
        server.set_policy(policy(false, false, None));
        for (path, body) in [("/login/info", "{}"), ("/login/wake", "{\"origin\":\"https://x.com/\"}"), ("/login/totp", &ask), ("/login/save", save)] {
            assert_eq!(request(port, "POST", path, Some(&pairing), Some(body)).0, 404, "{path}");
        }
        server.stop();
    }
}
