// DataShield — popup / preferences: pair (token + port), check the connection.
const api = globalThis.browser ?? globalThis.chrome;
const $ = (id) => document.getElementById(id);
let lang = "en";
const t = () => globalThis.DS_I18N(lang);

function speak() {
  const s = t();
  $("tokenLabel").textContent = s.tokenLabel;
  $("token").placeholder = s.tokenPlaceholder;
  $("portLabel").textContent = s.portLabel;
  $("save").textContent = s.save;
  $("test").textContent = s.test;
  $("hint").textContent = s.hint;
}

async function load() {
  const s = await api.storage.local.get({ token: "", port: 7737 });
  $("token").value = s.token || "";
  $("port").value = s.port || 7737;
  speak();
  await test();
}

async function save() {
  const token = $("token").value.trim();
  const port = Number($("port").value) || 7737;
  if (token && !/^[0-9a-f]{64}$/.test(token)) return show(t().badToken, "bad");
  await api.storage.local.set({ token, port });
  // Firefox grants MV3 host permissions only when asked, from a user gesture:
  // this click is one. Chrome granted them at install and answers true.
  try {
    if (api.permissions?.request) {
      const ok = await api.permissions.request({ origins: ["http://127.0.0.1/*"] });
      if (!ok) return show(t().permRefused, "bad");
    }
  } catch (_) {}
  await test();
}

async function test() {
  const r = await new Promise((resolve) => api.runtime.sendMessage({ type: "status" }, (x) => resolve(x ?? null)));
  if (r?.lang) { lang = r.lang; speak(); }
  if (!r || !r.reachable) return show(t().unreachable, "bad");
  if (!r.loginAssist) return show(t().assistOff, "warn");
  if (r.unauthorized) return show(t().unauthorized, "warn");
  if (r.paired) return show(t().paired, "ok");
  show(t().rejected, "warn");
}

function show(text, cls) {
  const el = $("status");
  el.textContent = text;
  el.className = `status ${cls}`;
}

$("save").onclick = save;
$("test").onclick = test;
load();
