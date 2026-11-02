// DataShield — content script: finds login forms, offers the vault's logins
// for this site in a small menu, fills the one the user picks. It never submits
// a form, never sees the pairing token, and never keeps a credential longer
// than the fill takes.
//
// How a fill starts:
//   * the page opened with `#dsl=<nonce>` (the TUI's `l` key): the nonce goes
//     to the background and the FIRST login form that appears — on this page
//     or after a redirect, same tab — is filled without a click;
//   * a login form is on the page: a brass `◆` appears beside the password
//     field — or beside the email/username field on the first step of a
//     two-step login (Google, Microsoft…) when the vault has something for
//     this site. Clicking it (or Ctrl+Shift+L in the field) opens the menu.
//   * no vault is open in DataShield: the menu says so, asks DataShield to
//     come forward, and waits for the unlock; the list then appears by itself.
//   * after a filled login, a one-time-code field is filled with the item's
//     TOTP code, when the item has one.
//   * a login typed by hand and submitted: the next page offers to keep it in
//     DataShield — only on your "save".

(() => {
  if (window.__datashieldLoaded) return;
  window.__datashieldLoaded = true;

  const api = globalThis.browser ?? globalThis.chrome;
  const send = (msg) => new Promise((resolve) => {
    try { api.runtime.sendMessage(msg, (r) => resolve(r ?? null)); } catch (_) { resolve(null); }
  });
  let lang = "en";
  const t = () => globalThis.DS_I18N(lang);

  // ---- the `l` key's nonce: read, stripped, handed over ------------------
  const m = location.hash.match(/^#dsl=([0-9a-f]{64})$/);
  if (m) {
    send({ type: "nonce", nonce: m[1] });
    try { history.replaceState(null, "", location.pathname + location.search); } catch (_) {}
  }

  // ---- finding the fields ------------------------------------------------
  const visible = (el) => {
    if (!el || !el.isConnected) return false;
    const r = el.getBoundingClientRect();
    const cs = getComputedStyle(el);
    return r.width > 0 && r.height > 0 && cs.visibility !== "hidden" && cs.display !== "none";
  };
  // a field the page itself hides from people (Google keeps a dummy
  // `hiddenPassword` input on its email step) is not a field to fill
  const usable = (el) =>
    visible(el) && el.getAttribute("aria-hidden") !== "true" && el.tabIndex !== -1 && !el.disabled && !el.readOnly;

  const USER_HINT = /user|email|e-mail|mail|login|account|utente|nome|phone|tel/i;
  const IDENT_HINT = /identifier|user|login|email|e-mail|account|utente/i;
  const OTP_HINT = /otp|totp|one-?time|verification|verify|2fa|mfa|codice|code|token|pin/i;

  const passwordFields = () => [...document.querySelectorAll('input[type="password"]')].filter(usable);

  // The FIRST step of a two-step login: an email/username field and no
  // password field yet. Only fields that say what they are count, and the
  // mark is drawn only when the vault HAS a login for this site.
  function identifierFields() {
    if (passwordFields().length) return [];
    return [...document.querySelectorAll('input:not([type]), input[type="text"], input[type="email"], input[type="tel"]')]
      .filter(usable)
      .filter((el) => {
        const ac = (el.getAttribute("autocomplete") || "").toLowerCase();
        return el.type === "email" || /username|email/.test(ac) || IDENT_HINT.test(el.name || "") || IDENT_HINT.test(el.id || "");
      });
  }

  // a one-time-code field: says so, or is a short numeric box
  function otpFields() {
    return [...document.querySelectorAll('input:not([type]), input[type="text"], input[type="tel"], input[type="number"]')]
      .filter(usable)
      .filter((el) => {
        const ac = (el.getAttribute("autocomplete") || "").toLowerCase();
        if (ac === "one-time-code") return true;
        const named = OTP_HINT.test(el.name || "") || OTP_HINT.test(el.id || "") || OTP_HINT.test(el.getAttribute("aria-label") || "");
        const shortNumeric = (el.inputMode === "numeric" || el.pattern?.includes("[0-9]")) && el.maxLength > 0 && el.maxLength <= 8;
        return named && (shortNumeric || el.maxLength <= 8);
      });
  }

  function usernameFor(pw) {
    const scope = pw.form || document;
    const candidates = [...scope.querySelectorAll('input:not([type]), input[type="text"], input[type="email"], input[type="tel"]')]
      .filter(usable)
      .filter((el) => el.compareDocumentPosition(pw) & Node.DOCUMENT_POSITION_FOLLOWING);
    if (!candidates.length) return null;
    const score = (el) => {
      const ac = (el.getAttribute("autocomplete") || "").toLowerCase();
      let s = 0;
      if (/username|email/.test(ac)) s += 10;
      if (el.type === "email") s += 6;
      if (USER_HINT.test(el.name) || USER_HINT.test(el.id) || USER_HINT.test(el.placeholder || "")) s += 4;
      if (USER_HINT.test(el.getAttribute("aria-label") || "")) s += 3;
      return s;
    };
    candidates.sort((a, b) => score(b) - score(a));
    return score(candidates[0]) > 0 ? candidates[0] : candidates[candidates.length - 1];
  }

  // ---- filling -----------------------------------------------------------
  const nativeSet = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
  function setValue(el, value) {
    if (!el) return;
    el.focus();
    if (nativeSet) nativeSet.call(el, value); else el.value = value;
    el.dispatchEvent(new Event("input", { bubbles: true }));
    el.dispatchEvent(new Event("change", { bubbles: true }));
  }

  function fill(pw, creds) {
    const user = usernameFor(pw);
    if (user && creds.username) setValue(user, creds.username);
    if (creds.password) setValue(pw, creds.password);
    pw.focus();
  }

  // ---- the menu ------------------------------------------------------------
  let host = null, panel = null, anchor = null, waitTimer = null;

  function ensureMenu() {
    if (host) return;
    host = document.createElement("div");
    host.setAttribute("data-datashield", "");
    host.style.cssText = "all:initial;position:absolute;z-index:2147483647;";
    const shadow = host.attachShadow({ mode: "closed" });
    const style = document.createElement("style");
    style.textContent = `
      :host { all: initial; }
      .panel { position: fixed; min-width: 240px; max-width: 360px; background: #171310; color: #E8E2D3;
               border: 1px solid #D9A855; border-radius: 6px; box-shadow: 0 6px 24px rgba(0,0,0,.5);
               font: 13px/1.4 ui-monospace, "JetBrains Mono", Menlo, Consolas, monospace; overflow: hidden; }
      .title { padding: 6px 10px; color: #D9A855; font-weight: 700; border-bottom: 1px solid #3E382E; display:flex; justify-content:space-between; }
      .title .x { color: #6A6253; cursor: pointer; }
      .row { padding: 7px 10px; cursor: pointer; display: flex; flex-direction: column; gap: 1px; }
      .row:hover { background: #2A2213; }
      .label { color: #F4CE82; }
      .user { color: #8FB0CE; font-size: 12px; }
      .note { padding: 8px 10px; color: #9C9384; font-size: 12px; }
      .note.wait { color: #E08F3F; }
      .btns { display:flex; gap:8px; padding: 6px 10px 10px; }
      .btn { flex:1; text-align:center; padding:5px; border:1px solid #3E382E; border-radius:4px; color:#E3B96B; cursor:pointer; background:#241F17; }
      .btn:hover { border-color:#D9A855; }
    `;
    panel = document.createElement("div");
    panel.className = "panel";
    shadow.append(style, panel);
    document.documentElement.append(host);
    document.addEventListener("keydown", (e) => { if (e.key === "Escape") hideMenu(); }, true);
    document.addEventListener("mousedown", (e) => { if (e.target !== host) hideMenu(); }, true);
    window.addEventListener("scroll", place, true);
    window.addEventListener("resize", place);
  }

  function place() {
    if (!host || host.style.display === "none") return;
    if (!anchor || !visible(anchor)) { if (anchor) hideMenu(); return; }
    const r = anchor.getBoundingClientRect();
    panel.style.left = `${Math.max(8, Math.min(r.left, innerWidth - 260))}px`;
    panel.style.top = `${r.bottom + 4}px`;
  }

  function hideMenu() {
    if (host) host.style.display = "none";
    anchor = null;
    if (waitTimer) { clearInterval(waitTimer); waitTimer = null; }
  }

  function titleRow() {
    const title = document.createElement("div");
    title.className = "title";
    title.innerHTML = `<span>${t().title}</span><span class="x"></span>`;
    title.querySelector(".x").textContent = "✕";
    title.querySelector(".x").title = t().close;
    title.querySelector(".x").onclick = hideMenu;
    return title;
  }

  function note(text, cls = "") {
    const n = document.createElement("div");
    n.className = `note ${cls}`;
    n.textContent = text;
    panel.append(n);
  }

  // `field` is what the menu hangs from; `onPick(creds)` does the filling.
  // With no vault open it asks DataShield to come forward and WAITS: every
  // two seconds for a minute it asks again, and the list appears by itself.
  function showMenu(field, m, onPick) {
    ensureMenu();
    anchor = field;
    panel.replaceChildren(titleRow());
    const { state, logins } = m;
    if (!state.reachable) note(t().notRunning);
    else if (!state.authorized) note(t().notPaired);
    else if (!state.vaultOpen) {
      note(t().wokeUp, "wait");
      send({ type: "wake" });
      let left = 30;
      if (waitTimer) clearInterval(waitTimer);
      waitTimer = setInterval(async () => {
        if (--left <= 0 || anchor !== field) return hideMenu();
        const again = await ask(field.type === "password");
        if (again.state.vaultOpen) {
          lastMatch = again;
          if (again.fill) { hideMenu(); return onPick(again.fill); }
          showMenu(field, again, onPick);
        }
      }, 2000);
    } else if (!logins.length) note(t().noLogins);
    for (const l of logins) {
      const row = document.createElement("div");
      row.className = "row";
      row.innerHTML = `<span class="label"></span><span class="user"></span>`;
      row.querySelector(".label").textContent = l.label;
      row.querySelector(".user").textContent = l.username;
      row.onmousedown = (e) => e.preventDefault(); // keep the field's focus
      row.onclick = async () => {
        const r = await send({ type: "fill", id: l.id });
        hideMenu();
        if (r?.creds) onPick(r.creds);
      };
      panel.append(row);
    }
    host.style.display = "";
    place();
  }

  // ---- the trigger beside a field ------------------------------------------
  const wired = new WeakSet();
  let lastMatch = null;

  async function ask(hasPassword = true) {
    const r = await send({ type: "match", hasPassword });
    if (!r) return { fill: null, logins: [], state: { reachable: false, authorized: false, vaultOpen: false } };
    if (r.state?.lang) lang = r.state.lang;
    return { fill: r.fill, logins: r.logins || [], state: r.state };
  }

  function hotkey(el, open) {
    el.addEventListener("keydown", (e) => {
      if ((e.ctrlKey || e.metaKey) && e.shiftKey && e.key.toLowerCase() === "l") { e.preventDefault(); open(); }
    });
  }

  async function wire(pw) {
    if (wired.has(pw)) return;
    wired.add(pw);
    const first = await ask(true);
    lastMatch = first;
    if (first.fill) { fill(pw, first.fill); return; }
    if (!first.state.reachable) return; // DataShield isn't running: no mark at all
    const open = async () => {
      const m = await ask(true);
      lastMatch = m;
      if (m.fill) { fill(pw, m.fill); return; }
      showMenu(pw, m, (creds) => fill(pw, creds));
    };
    pw.addEventListener("focus", () => { if (lastMatch?.logins?.length) open(); });
    hotkey(pw, open);
    addMark(pw, open);
  }

  // step one of a two-step login: the username goes in now, and the item is
  // remembered (by id, in the background) for the password step to come
  async function wireIdentifier(user) {
    if (wired.has(user)) return;
    wired.add(user);
    const first = await ask(false);
    lastMatch = first;
    if (first.fill) { setValue(user, first.fill.username); return; }
    if (!first.state.reachable) return;
    // no mark for an email box the vault has nothing for — unless the vault
    // is simply closed, in which case the mark is the way to ask for it
    if (!first.logins.length && first.state.vaultOpen) return;
    const open = async () => {
      const m = await ask(false);
      lastMatch = m;
      if (m.fill) { setValue(user, m.fill.username); return; }
      showMenu(user, m, (creds) => setValue(user, creds.username));
    };
    user.addEventListener("focus", () => { if (lastMatch?.logins?.length) open(); });
    hotkey(user, open);
    addMark(user, open);
    if (document.activeElement === user) open();
  }

  // a one-time-code field after a login filled from this tab
  async function wireOtp(el) {
    if (wired.has(el)) return;
    wired.add(el);
    const r = await send({ type: "totp" });
    if (r?.code) setValue(el, r.code);
  }

  function addMark(field, open) {
    const mark = document.createElement("button");
    mark.type = "button";
    mark.title = t().fillTitle;
    mark.textContent = "◆";
    mark.style.cssText = "all:initial;position:absolute;z-index:2147483646;cursor:pointer;color:#D9A855;" +
      "font:14px ui-monospace,monospace;line-height:1;padding:2px 4px;background:#171310;border:1px solid #3E382E;border-radius:4px;";
    mark.onmousedown = (e) => e.preventDefault();
    mark.onclick = open;
    document.documentElement.append(mark);
    const placeMark = () => {
      if (!visible(field)) { mark.style.display = "none"; return; }
      const r = field.getBoundingClientRect();
      mark.style.display = "";
      mark.style.left = `${scrollX + r.right - 24}px`;
      mark.style.top = `${scrollY + r.top + (r.height - 20) / 2}px`;
    };
    placeMark();
    window.addEventListener("scroll", placeMark, true);
    window.addEventListener("resize", placeMark);
    new ResizeObserver(placeMark).observe(document.documentElement);
  }

  // ---- a login typed by hand: capture on submit, offer to keep it next page --
  document.addEventListener("submit", (e) => {
    const form = e.target;
    if (!(form instanceof HTMLFormElement)) return;
    const pw = [...form.querySelectorAll('input[type="password"]')].find(usable);
    if (!pw || !pw.value) return;
    const user = usernameFor(pw);
    send({ type: "captured", username: user?.value || "", password: pw.value });
  }, true);
  // many sites submit from a button handler without a `submit` event: a click
  // on a submit-looking button inside a form with a filled password counts too
  document.addEventListener("click", (e) => {
    const btn = e.target?.closest?.('button, input[type="submit"]');
    if (!btn) return;
    const form = btn.form || btn.closest("form");
    if (!form) return;
    const pw = [...form.querySelectorAll('input[type="password"]')].find(usable);
    if (!pw || !pw.value) return;
    const user = usernameFor(pw);
    send({ type: "captured", username: user?.value || "", password: pw.value });
  }, true);

  async function offerToKeep() {
    const r = await send({ type: "offer" });
    if (!r?.offer) return;
    if (r.lang) lang = r.lang;
    ensureMenu();
    anchor = null;
    panel.replaceChildren(titleRow());
    note(t().saveAsk(r.offer.host) + (r.offer.username ? ` (${r.offer.username})` : ""));
    const btns = document.createElement("div");
    btns.className = "btns";
    const yes = document.createElement("div"); yes.className = "btn"; yes.textContent = t().saveYes;
    const no = document.createElement("div"); no.className = "btn"; no.textContent = t().saveNo;
    yes.onclick = async () => {
      const k = await send({ type: "keep" });
      panel.replaceChildren(titleRow());
      note(k?.ok ? t().saved : k?.error === "vault-locked" ? t().saveLocked : t().saveFailed);
      setTimeout(hideMenu, 2500);
    };
    no.onclick = async () => { await send({ type: "discard" }); hideMenu(); };
    btns.append(yes, no);
    panel.append(btns);
    host.style.display = "";
    panel.style.left = `${innerWidth - 380}px`;
    panel.style.top = "16px";
  }

  // ---- watch the page --------------------------------------------------------
  let scheduled = false;
  function scan() {
    scheduled = false;
    for (const pw of passwordFields()) wire(pw);
    for (const user of identifierFields()) wireIdentifier(user);
    for (const otp of otpFields()) wireOtp(otp);
  }
  function schedule() {
    if (scheduled) return;
    scheduled = true;
    setTimeout(scan, 250);
  }
  scan();
  if (window === window.top) offerToKeep();
  new MutationObserver(schedule).observe(document.documentElement, {
    childList: true, subtree: true, attributes: true, attributeFilter: ["class", "style", "hidden", "aria-hidden", "type"],
  });
})();
