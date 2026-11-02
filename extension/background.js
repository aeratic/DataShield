// DataShield — background (Chrome: service worker · Firefox: event page).
//
// THE ONE PLACE THAT TALKS TO DATASHIELD. Content scripts never see the pairing
// token and never fetch: they ask here, and this script decides what to ask the
// API on 127.0.0.1 — using the ORIGIN OF THE TAB THAT ASKED (sender.url), never
// one the page could have made up. A page that injected a message claiming to
// be gmail.com still only ever gets gmail.com's credentials if it IS gmail.com.
//
// What is kept: the token and the port in `storage.local` (extension-private);
// in memory only, for two minutes at most: the nonce a DataShield `l` key put
// in a tab's URL, the ITEM ID picked on step one of a two-step login, and a
// login captured on a form the user submitted by hand, until they say whether
// to keep it. Credentials pass through on their way to a page and are kept
// nowhere else.

const api = globalThis.browser ?? globalThis.chrome;

const DEFAULT_PORT = 7737;
const TTL_MS = 2 * 60 * 1000;

const pendingByTab = new Map();  // tabId -> { nonce, expires }   (the `l` key)
const chosenByTab = new Map();   // tabId -> { id, expires }      (step one → step two)
const capturedByTab = new Map(); // tabId -> { host, username, password, expires } (a login to maybe keep)

let lang = "en";

const fresh = (map, tabId) => {
  const v = map.get(tabId);
  if (!v) return null;
  if (v.expires < Date.now()) { map.delete(tabId); return null; }
  return v;
};
const remember = (map, tabId, value) => { if (tabId !== undefined) map.set(tabId, { ...value, expires: Date.now() + TTL_MS }); };

async function settings() {
  const stored = await api.storage.local.get({ token: "", port: DEFAULT_PORT });
  return { token: String(stored.token || "").trim(), port: Number(stored.port) || DEFAULT_PORT };
}

// One request to the API. `null` = DataShield is not reachable at all.
async function call(path, body) {
  const { token, port } = await settings();
  const headers = { "Content-Type": "application/json" };
  if (token) headers["Authorization"] = `Bearer ${token}`;
  try {
    const res = await fetch(`http://127.0.0.1:${port}${path}`, {
      method: body === undefined ? "GET" : "POST",
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    let json = null;
    try { json = await res.json(); } catch (_) {}
    return { status: res.status, json };
  } catch (_) {
    return null;
  }
}

// the app's state as the extension needs it: language + is a vault open
async function info() {
  const r = await call("/login/info", {});
  if (r && r.status === 200 && r.json) {
    lang = r.json.lang === "it" ? "it" : "en";
    return { reachable: true, authorized: true, assist: true, vaultOpen: !!r.json.vault_open, lang };
  }
  return {
    reachable: r !== null,
    authorized: r ? r.status !== 401 : false,
    assist: r ? r.status !== 404 : false,
    vaultOpen: false,
    lang,
  };
}

const originOf = (url) => { try { return new URL(url).origin; } catch (_) { return null; } };
const hostOf = (url) => { try { return new URL(url).hostname.toLowerCase(); } catch (_) { return ""; } };

api.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  handle(msg, sender).then(sendResponse, () => sendResponse({ error: "internal" }));
  return true;
});

async function handle(msg, sender) {
  const tabId = sender.tab?.id;
  const origin = originOf(sender.url || sender.tab?.url || "");

  switch (msg?.type) {
    case "nonce": {
      if (/^[0-9a-f]{64}$/.test(String(msg.nonce || ""))) remember(pendingByTab, tabId, { nonce: msg.nonce });
      return { ok: true };
    }

    // a login form is on the page: the state, the `l` key's claim, the second
    // step of a login started here, and the candidates for this site
    case "match": {
      const state = await info();
      if (!origin || !state.reachable) return { state, logins: [], fill: null };
      let fill = null;
      const p = fresh(pendingByTab, tabId);
      if (p) {
        const r = await call("/login/claim", { nonce: p.nonce, origin });
        pendingByTab.delete(tabId); // spent either way
        if (r?.status === 200 && r.json) { fill = r.json; remember(chosenByTab, tabId, { id: r.json.id }); }
      }
      if (!fill && msg.hasPassword) {
        const c = fresh(chosenByTab, tabId);
        if (c) {
          const r = await call("/login/fill", { id: c.id, origin });
          if (r?.status === 200 && r.json) fill = r.json;
        }
      }
      const r = await call("/login/match", { origin });
      const logins = r?.status === 200 && Array.isArray(r.json?.logins) ? r.json.logins : [];
      return { state, fill, logins };
    }

    // the user picked an entry in the menu
    case "fill": {
      const id = String(msg.id || "");
      if (!id || !origin) return { error: "bad-request" };
      const r = await call("/login/fill", { id, origin });
      if (!r) return { error: "unreachable" };
      if (r.status !== 200 || !r.json) return { error: r.json?.error || `http-${r.status}` };
      remember(chosenByTab, tabId, { id });
      return { creds: { username: r.json.username, password: r.json.password } };
    }

    // a one-time-code field appeared after a login filled from this tab
    case "totp": {
      const c = fresh(chosenByTab, tabId);
      if (!c || !origin) return { code: null };
      const r = await call("/login/totp", { id: c.id, origin });
      return { code: r?.status === 200 ? r.json?.code : null };
    }

    // no vault open: ask DataShield to come forward
    case "wake": {
      if (!origin) return { ok: false };
      const r = await call("/login/wake", { origin });
      return { ok: r?.status === 200 };
    }

    // a login form was submitted by hand: hold what was typed until the next
    // page says whether it is worth keeping (a login the vault lacks)
    case "captured": {
      const username = String(msg.username || "").trim();
      const password = String(msg.password || "");
      if (!origin || !password) return { ok: false };
      remember(capturedByTab, tabId, { host: hostOf(origin), username, password });
      return { ok: true };
    }

    // the next page in that tab asks: is there something to offer to keep?
    case "offer": {
      const c = fresh(capturedByTab, tabId);
      if (!c || !origin) return { offer: null };
      const state = await info();
      if (!state.vaultOpen) return { offer: null, lang: state.lang };
      // not a login the vault already has for that site + username
      const r = await call("/login/match", { origin: `https://${c.host}/` });
      const logins = r?.status === 200 && Array.isArray(r.json?.logins) ? r.json.logins : [];
      if (logins.some((l) => (l.username || "").toLowerCase() === c.username.toLowerCase())) {
        capturedByTab.delete(tabId);
        return { offer: null, lang: state.lang };
      }
      return { offer: { host: c.host, username: c.username }, lang: state.lang };
    }

    case "keep": {
      const c = fresh(capturedByTab, tabId);
      capturedByTab.delete(tabId);
      if (!c) return { error: "gone" };
      const r = await call("/login/save", { origin: `https://${c.host}/`, username: c.username, password: c.password });
      if (!r) return { error: "unreachable" };
      if (r.status === 409) return { error: "vault-locked" };
      return r.status === 200 ? { ok: true } : { error: r.json?.error || `http-${r.status}` };
    }

    case "discard": {
      capturedByTab.delete(tabId);
      return { ok: true };
    }

    // the popup / preferences page
    case "status": {
      const health = await call("/health");
      if (!health) return { reachable: false, lang };
      const state = await info();
      return { reachable: true, paired: state.authorized && state.assist, loginAssist: state.assist, unauthorized: !state.authorized, lang: state.lang };
    }

    default:
      return { error: "unknown" };
  }
}

api.tabs?.onRemoved?.addListener((tabId) => { pendingByTab.delete(tabId); chosenByTab.delete(tabId); capturedByTab.delete(tabId); });
