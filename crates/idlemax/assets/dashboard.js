(() => {
"use strict";

const POLL_MS = 5000;
const TOKEN_KEY = "idlemax.token";
const ACTIVE = new Set(["leased", "running", "paused"]);
const TERMINAL = new Set(["succeeded", "failed", "killed", "cancelled", "parked"]);
// Jobs that wait for something: GET /v1/why explains these.
const WAITING = new Set(["queued", "leased", "paused"]);

// Token: localStorage when available, memory otherwise.
let memToken = "";
const tokenStore = {
  get() { try { return localStorage.getItem(TOKEN_KEY) || memToken; } catch (_) { return memToken; } },
  set(v) {
    memToken = v;
    try { if (v) localStorage.setItem(TOKEN_KEY, v); else localStorage.removeItem(TOKEN_KEY); } catch (_) { /* private mode */ }
  },
};

// DOM helper. Children are appended as text nodes, never parsed as HTML.
function h(tag, props, ...kids) {
  const el = document.createElement(tag);
  if (props) {
    for (const [k, v] of Object.entries(props)) {
      if (v == null || v === false) continue;
      if (k === "class") el.className = v;
      else if (k.startsWith("on")) el.addEventListener(k.slice(2), v);
      else el.setAttribute(k, v === true ? "" : String(v));
    }
  }
  for (const kid of kids.flat(Infinity)) {
    if (kid == null || kid === false) continue;
    el.append(kid instanceof Node ? kid : String(kid));
  }
  return el;
}
const $ = (id) => document.getElementById(id);

// Formatting.
function fmtBytes(b) {
  if (b == null || !isFinite(b)) return "-";
  const gib = b / 1073741824;
  if (gib >= 1) return `${gib.toFixed(gib >= 100 ? 0 : 1)} GiB`;
  return `${Math.round(b / 1048576)} MiB`;
}
function fmtPct(p, digits = 0) { return p == null || !isFinite(p) ? "-" : `${Number(p).toFixed(digits)}%`; }
function fmtDur(secs) {
  if (secs == null || !isFinite(secs)) return "-";
  secs = Math.max(0, Math.round(secs));
  if (secs < 60) return `${secs}s`;
  const m = Math.floor(secs / 60);
  if (m < 60) return `${m}m`;
  const hr = Math.floor(m / 60);
  if (hr < 48) return m % 60 ? `${hr}h ${m % 60}m` : `${hr}h`;
  return `${Math.floor(hr / 24)}d ${hr % 24}h`;
}
function fmtAgo(ms) {
  if (!ms) return "-";
  const d = (Date.now() - ms) / 1000;
  if (Math.abs(d) < 5) return "now";
  return d > 0 ? `${fmtDur(d)} ago` : `in ${fmtDur(-d)}`;
}
function fmtTime(ms) {
  if (!ms) return "";
  const t = new Date(ms);
  return t.toLocaleString(undefined, { dateStyle: "medium", timeStyle: "medium" });
}
function clock(ms) {
  if (!ms) return "-";
  const t = new Date(ms);
  const sameDay = t.toDateString() === new Date().toDateString();
  return sameDay
    ? t.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit", second: "2-digit", hour12: false })
    : t.toLocaleDateString(undefined, { month: "short", day: "numeric" });
}
// Relative times change without new data, so renderUpdated() refreshes them in place.
function rel(ms, pre = "", cls = "num") { return h("span", { class: cls, "data-rel": ms, "data-pre": pre, title: fmtTime(ms) }, pre + fmtAgo(ms)); }
function when(ms) { return rel(ms); }
function ran(j) {
  if (!j.started_ms) return "";
  if (j.finished_ms) return `ran ${fmtDur((j.finished_ms - j.started_ms) / 1000)}`;
  return h("span", { "data-run": j.started_ms }, `ran ${fmtDur((Date.now() - j.started_ms) / 1000)}`);
}
const label = (s) => String(s || "").replace(/_/g, " ");

const JOB_TONE = { queued: "", leased: "info", running: "info", paused: "warn", succeeded: "ok", failed: "bad", killed: "bad", cancelled: "", parked: "warn" };
const GUARD_TONE = { run: "ok", hold: "", pause: "warn", kill: "bad" };
const KIND_TONE = { assign: "info", do: "info", fix: "bad", follow_up: "", warn: "warn" };
function badge(text, tone, dot) { return h("span", { class: `badge ${tone || ""}` }, dot ? h("span", { class: "dot" }) : null, text); }
function jobBadge(status) { return badge(label(status), JOB_TONE[status]); }
function guardText(action) {
  if (!action) return "unknown";
  if (action.kind === "run") return `run, ${action.slots} slot${action.slots === 1 ? "" : "s"}, ${action.mem_mib} MiB`;
  return action.kind;
}

// API.
class AuthError extends Error {}
async function api(method, path, body) {
  const headers = { Authorization: `Bearer ${tokenStore.get()}`, Accept: "application/json" };
  if (body !== undefined) headers["Content-Type"] = "application/json";
  const res = await fetch(path, {
    method, headers, cache: "no-store",
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await res.text();
  let data = null;
  if (text) { try { data = JSON.parse(text); } catch (_) { data = null; } }
  const msg = (data && typeof data.error === "string" && data.error) || `HTTP ${res.status}`;
  if (res.status === 401) throw new AuthError(msg);
  if (!res.ok) throw new Error(msg);
  return data;
}
const control = (scope, action, reason) => api("POST", "/v1/control", { scope, action, reason: reason || "dashboard" });
const scopes = {
  global: () => ({ type: "global" }),
  device: (id) => ({ type: "device", id }),
  account: (id) => ({ type: "account", id }),
  job: (id) => ({ type: "job", id }),
};

// State and polling.
const state = { status: null, jobs: null, schedules: null, quota: null, advice: null, events: null, why: null, errors: {}, lastOk: 0, allFailed: false, busy: false, timer: 0, showClosed: false };
// Full explanations are fetched only for the rows a user opened, and re-fetched once per poll.
const whyOpen = new Set();
const whyDetail = new Map();

async function refresh() {
  if (state.busy || !tokenStore.get()) return;
  state.busy = true;
  const filter = $("job-filter").value;
  const jobsPath = `/v1/jobs?limit=100${filter ? `&status=${encodeURIComponent(filter)}` : ""}`;
  const calls = [
    ["status", api("GET", "/v1/status")],
    ["jobs", api("GET", jobsPath)],
    ["schedules", api("GET", "/v1/schedules")],
    ["quota", api("GET", "/v1/quota")],
    ["advice", api("GET", "/v1/advice")],
    ["events", api("GET", "/v1/events?limit=60")],
    ["why", api("GET", "/v1/why")],
  ];
  const openIds = [...whyOpen];
  const detailCalls = openIds.map((id) => api("GET", whyPath(id)));
  const [results, details] = await Promise.all([Promise.allSettled(calls.map((c) => c[1])), Promise.allSettled(detailCalls)]);
  state.busy = false;
  state.allFailed = results.every((r) => r.status === "rejected");
  if ([...results, ...details].some((r) => r.status === "rejected" && r.reason instanceof AuthError)) {
    logout("The coordinator rejected this token. Paste the current one.");
    return;
  }
  let anyOk = false;
  results.forEach((r, i) => {
    const key = calls[i][0];
    if (r.status === "fulfilled") { state[key] = r.value; delete state.errors[key]; anyOk = true; }
    else state.errors[key] = r.reason && r.reason.message ? r.reason.message : "request failed";
  });
  if (anyOk) state.lastOk = Date.now();
  details.forEach((r, i) => whyDetail.set(openIds[i], r.status === "fulfilled" ? { data: r.value } : { error: r.reason && r.reason.message ? r.reason.message : "request failed" }));
  render();
}

function schedule() {
  clearInterval(state.timer);
  state.timer = setInterval(() => { if (!document.hidden) refresh(); else renderUpdated(); }, POLL_MS);
}
document.addEventListener("visibilitychange", () => { if (!document.hidden) refresh(); });

// Actions.
function toast(text, tone) {
  const el = h("div", { class: `toast ${tone || ""}`, role: tone === "bad" ? "alert" : "status" }, text);
  $("toasts").append(el);
  setTimeout(() => el.remove(), tone === "bad" ? 7000 : 3500);
}
async function act(btn, doneText, fn) {
  if (btn) btn.disabled = true;
  try {
    const out = await fn();
    if (doneText) toast(typeof doneText === "function" ? doneText(out) : doneText);
    return out;
  } catch (e) {
    if (e instanceof AuthError) logout("The coordinator rejected this token. Paste the current one.");
    else toast(`Failed: ${e.message}`, "bad");
    return undefined;
  } finally {
    if (btn) btn.disabled = false;
    refresh();
  }
}

let confirmResolve = null;
function confirmBox(title, text, yesLabel, danger) {
  $("confirm-title").textContent = title;
  $("confirm-text").textContent = text;
  $("confirm-yes").textContent = yesLabel || "Confirm";
  $("confirm-yes").className = danger ? "btn danger solid" : "btn primary";
  const dlg = $("confirm");
  dlg.showModal();
  $("confirm-no").focus();
  return new Promise((resolve) => { confirmResolve = resolve; });
}
function closeConfirm(answer) {
  const dlg = $("confirm");
  if (dlg.open) dlg.close();
  if (confirmResolve) { confirmResolve(answer); confirmResolve = null; }
}
$("confirm-yes").addEventListener("click", () => closeConfirm(true));
$("confirm-no").addEventListener("click", () => closeConfirm(false));
$("confirm").addEventListener("cancel", (e) => { e.preventDefault(); closeConfirm(false); });

const STOP_TEXT = "Running jobs get SIGTERM, then SIGKILL after 15 s, and nothing new starts until you resume.";
async function stopAll(btn, scope, where) {
  const ok = await confirmBox(`Stop all ${where}?`, STOP_TEXT, "Stop all", true);
  if (ok) act(btn, `Stop all sent ${where}`, () => control(scope, "stop_all"));
}

$("btn-refresh").addEventListener("click", () => refresh());
$("btn-pause").addEventListener("click", (e) => act(e.currentTarget, "Paused everywhere", () => control(scopes.global(), "pause")));
$("btn-resume").addEventListener("click", (e) => act(e.currentTarget, "Resumed everywhere", () => control(scopes.global(), "resume")));
$("btn-stop").addEventListener("click", (e) => stopAll(e.currentTarget, scopes.global(), "everywhere"));
$("btn-logout").addEventListener("click", () => logout(""));
$("btn-advice-refresh").addEventListener("click", (e) => act(e.currentTarget, "Advisor re-ran", () => api("POST", "/v1/advice/refresh")));
$("job-filter").addEventListener("change", () => refresh());

// Rendering. A section is rebuilt only when its data changed (the clock fields are ignored),
// and a rebuild keeps keyboard focus and open <details>, so polling never eats a click or a read.
const sig = {};
const NOT_SHOWN = new Set(["at_ms", "lease_expires_ms", "snapshot_ms"]); // clock fields that move without a visible change
function section(key, deps, boxId, fn) {
  const next = JSON.stringify(deps, (k, v) => (NOT_SHOWN.has(k) ? undefined : v));
  if (sig[key] === next) return;
  sig[key] = next;
  const box = $(boxId);
  const focusables = () => [...box.querySelectorAll("button, select, summary")];
  const focused = focusables().indexOf(document.activeElement);
  // Keyed <details> (the "why" rows) are restored from whyOpen by the code that builds them.
  const open = [...box.querySelectorAll("details:not([data-key])")].map((d) => d.open);
  fn();
  box.querySelectorAll("details:not([data-key])").forEach((d, i) => { d.open = Boolean(open[i]); });
  if (focused >= 0) { const el = focusables()[focused]; if (el) el.focus({ preventScroll: true }); }
}

function render() {
  const { status: s, errors: e } = state;
  renderUpdated();
  section("header", [s && s.version, s && s.global_paused, e.status], "banners", renderHeader);
  section("tiles", [s], "tiles", renderTiles);
  // last_seen_ms is shown only for offline devices; online ones move it on every heartbeat.
  const devs = s && (s.devices || []).map((d) => (d.online ? { ...d, last_seen_ms: 0 } : d));
  section("devices", [devs, s && s.global_paused, e.status], "devices", renderDevices);
  section("schedules", [state.schedules, e.schedules], "schedules", renderSchedules);
  section("jobs", [state.jobs, e.jobs, $("job-filter").value], "jobs", renderJobs);
  section("advice", [state.advice, e.advice, state.showClosed], "advice", renderAdvice);
  section("quota", [state.quota, s && s.paused_accounts, e.quota], "quota", renderQuota);
  section("events", [state.events, e.events], "events", renderEvents);
  renderWhy();
}

function renderUpdated() {
  const errs = Object.keys(state.errors).length;
  $("updated").textContent = !state.lastOk
    ? (errs ? "Cannot reach the coordinator" : "Loading")
    : state.allFailed ? `Cannot reach the coordinator, last update ${fmtAgo(state.lastOk)}`
    : `Updated ${fmtAgo(state.lastOk)}${errs ? `, ${errs} request${errs === 1 ? "" : "s"} failing` : ""}`;
  document.querySelectorAll("[data-rel]").forEach((el) => { el.textContent = el.dataset.pre + fmtAgo(Number(el.dataset.rel)); });
  document.querySelectorAll("[data-run]").forEach((el) => { el.textContent = `ran ${fmtDur((Date.now() - Number(el.dataset.run)) / 1000)}`; });
}

function renderHeader() {
  const s = state.status;
  $("ver").textContent = s && s.version ? `v${s.version}` : "";
  $("cluster-badge").replaceChildren(
    !s ? "" : s.global_paused ? badge("paused", "warn", true) : badge("running", "ok", true)
  );
  const banners = [];
  if (state.errors.status) banners.push(h("div", { class: "banner bad", role: "alert" }, `Status request failed: ${state.errors.status}`));
  if (s && s.global_paused) {
    banners.push(h("div", { class: "banner warn" }, "Everything is paused. No job starts on any device until you resume.",
      h("button", { class: "btn sm", type: "button", onclick: (e) => act(e.currentTarget, "Resumed everywhere", () => control(scopes.global(), "resume")) }, "Resume all")));
  }
  $("banners").replaceChildren(...banners);
}

function renderTiles() {
  const s = state.status;
  if (!s) { $("tiles").replaceChildren(); return; }
  const c = s.jobs || {};
  const devs = s.devices || [];
  const online = devs.filter((d) => d.online).length;
  const tile = (k, v, tone) => h("div", { class: "tile" }, h("div", { class: "k" }, k), h("div", { class: `v num ${tone || ""}` }, v));
  $("tiles").replaceChildren(
    tile("Devices online", `${online}/${devs.length}`, devs.length && online < devs.length ? "warn" : ""),
    tile("Queued", c.queued || 0),
    tile("Running", (c.running || 0) + (c.leased || 0)),
    tile("Paused", c.paused || 0, c.paused ? "warn" : ""),
    tile("Parked", c.parked || 0, c.parked ? "warn" : ""),
    tile("Failed", (c.failed || 0) + (c.killed || 0), c.failed || c.killed ? "bad" : ""),
    tile("Open advice", s.open_advice || 0),
  );
}

function stat(k, v, title) { return h("div", { class: "stat", title: title || null }, h("div", { class: "k" }, k), h("div", { class: "v num" }, v)); }

function renderDevices() {
  const box = $("devices");
  const s = state.status;
  if (!s) { box.replaceChildren(state.errors.status ? h("div", { class: "err" }, state.errors.status) : h("div", { class: "empty" }, "Loading")); return; }
  const devs = (s.devices || []).slice().sort((a, b) => (b.online - a.online) || String(a.info.id).localeCompare(String(b.info.id)));
  if (!devs.length) { box.replaceChildren(h("div", { class: "empty" }, "No device has sent a heartbeat yet. Start `idlemax agent` on a device.")); return; }
  box.replaceChildren(h("div", { class: "devices" }, devs.map((d) => deviceCard(d, Boolean(s.global_paused)))));
}

// Under Pause all every device reads as paused, and the API cannot tell a device's own pause from
// the global one. Resume on a device lifts only its own pause (resuming global clears no other
// scope), so under Pause all the button would toast success and change nothing: say why instead.
function pauseControl(id, paused, byAll) {
  if (byAll) return h("span", { class: "dev-note" }, "Paused by Pause all");
  return paused
    ? h("button", { class: "btn sm", type: "button", onclick: (e) => act(e.currentTarget, `Resumed ${id}`, () => control(scopes.device(id), "resume")) }, "Resume")
    : h("button", { class: "btn sm", type: "button", onclick: (e) => act(e.currentTarget, `Paused ${id}`, () => control(scopes.device(id), "pause")) }, "Pause");
}

// The label carries "on battery", so the value stays short enough for a four-column card.
function powerStat(m) {
  const pct = m.battery_pct != null ? fmtPct(m.battery_pct) : null;
  if (m.on_ac_power === false) return stat("On battery", pct || "yes");
  return stat("Power", m.on_ac_power == null ? "n/a" : ["AC", pct].filter(Boolean).join(" "));
}

// "macos laptop" already says macos, so the OS is shown only when the profile does not repeat it.
function osAndProfile(info) {
  const os = label(info.os);
  const profile = label(info.profile);
  return profile.startsWith(os) ? profile : [os, profile].filter(Boolean).join(" · ");
}

function deviceCard(d, byAll) {
  const info = d.info || {};
  const m = d.metrics || null;
  const g = d.guard || null;
  const kind = g && g.action ? g.action.kind : null;
  const badges = [
    d.online ? badge("online", "ok", true) : badge(rel(d.last_seen_ms, "offline, seen ", ""), "bad", true),
    d.paused ? badge(byAll ? "paused (all)" : "paused", "warn") : null,
    d.local_paused ? badge("kill switch", "warn") : null,
  ];
  const stats = m ? [
    stat("CPU busy", fmtPct(m.cpu_busy_pct)),
    stat("Load 1m", m.load_avg_1m != null ? Number(m.load_avg_1m).toFixed(2) : "-", `${m.cpu_count || info.cpu_count || "?"} cores`),
    stat("Mem free", fmtPct(m.mem_free_pct), `${fmtBytes(m.mem_available_bytes)} available of ${fmtBytes(m.mem_total_bytes)}`),
    stat("Swap used", fmtBytes(m.swap_used_bytes), `of ${fmtBytes(m.swap_total_bytes)}`),
    stat("Disk free", fmtBytes(m.disk_free_bytes), `of ${fmtBytes(m.disk_total_bytes)}`),
    stat("User idle", m.user_idle_secs == null ? "n/a" : fmtDur(m.user_idle_secs)),
    powerStat(m),
    stat("Thermal", m.thermal_limited == null ? "n/a" : m.thermal_limited ? "limited" : "ok"),
  ] : [];
  const running = d.running || [];
  const id = info.id;
  return h("article", { class: "dev" },
    h("div", { class: "dev-h" },
      h("div", null,
        h("div", { class: "name" }, id || "(no id)"),
        h("div", { class: "sub" }, [info.hostname && info.hostname !== id ? info.hostname : null, osAndProfile(info), info.agent_version ? `agent ${info.agent_version}` : null].filter(Boolean).join(" · "))),
      h("div", { class: "badges" }, badges)),
    g ? h("div", { class: "guard" }, h("span", { class: "muted" }, "Guard"), badge(guardText(g.action), GUARD_TONE[kind]), g.user_active ? badge("user active", "") : null) : null,
    g && g.reasons && g.reasons.length ? h("ul", { class: "reasons" }, g.reasons.map((r) => h("li", null, r))) : null,
    stats.length ? h("div", { class: "stats" }, stats) : h("div", { class: "muted", style: "margin-top:10px;font-size:12.5px" }, "No metrics yet."),
    running.length ? h("div", { class: "dev-jobs" }, running.map((j) => h("div", { class: "row" },
      jobBadge(j.status), h("span", { class: "t" }, `#${j.id} ${j.name}`),
      h("span", { class: "faint num", style: "margin-left:auto" }, j.rss_bytes != null ? fmtBytes(j.rss_bytes) : `${j.mem_mb} MiB cap`)))) : null,
    h("div", { class: "dev-a" },
      pauseControl(id, d.paused, byAll),
      h("button", { class: "btn sm danger", type: "button", onclick: (e) => stopAll(e.currentTarget, scopes.device(id), `on ${id}`) }, "Stop all")),
  );
}

function renderJobs() {
  const box = $("jobs");
  const jobs = state.jobs;
  if (state.errors.jobs) { box.replaceChildren(h("div", { class: "err" }, state.errors.jobs)); return; }
  if (!jobs) { box.replaceChildren(h("div", { class: "empty" }, "Loading")); return; }
  if (!jobs.length) {
    const f = $("job-filter").value;
    box.replaceChildren(h("div", { class: "empty" }, f ? `No ${f} jobs.` : "No jobs yet. Queue one with `idlemax job add <spec.toml>`."));
    return;
  }
  const rows = jobs.flatMap((j) => {
    const spec = j.spec || {};
    const acts = [];
    if (j.status === "running") acts.push(h("button", { class: "btn sm", type: "button", onclick: (e) => act(e.currentTarget, `Pausing job #${j.id}`, () => control(scopes.job(j.id), "pause")) }, "Pause"));
    if (j.status === "paused") acts.push(h("button", { class: "btn sm", type: "button", onclick: (e) => act(e.currentTarget, `Resuming job #${j.id}`, () => control(scopes.job(j.id), "resume")) }, "Resume"));
    if (!TERMINAL.has(j.status)) acts.push(h("button", { class: "btn sm danger", type: "button", onclick: (e) => act(e.currentTarget, `Cancelled job #${j.id}`, () => api("POST", `/v1/jobs/${encodeURIComponent(j.id)}/cancel`)) }, "Cancel"));
    const detail = [label(spec.cost), spec.ai_account ? `account ${spec.ai_account}` : null, spec.priority ? `priority ${spec.priority}` : null].filter(Boolean).join(" · ");
    const outcome = j.last_error || (j.exit_code != null ? `exit ${j.exit_code}` : "");
    const row = h("tr", null,
      h("td", { class: "c-id num mono" }, `#${j.id}`),
      h("td", { class: "c-job" }, h("div", { class: "job-name" }, spec.name || "(unnamed)"), h("div", { class: "t2" }, detail),
        outcome ? h("div", { class: "t2", style: j.last_error ? "color:var(--bad)" : null }, outcome) : null),
      h("td", { class: "c-status" }, jobBadge(j.status), h("div", { class: "t2 num" }, `try ${j.attempts || 0}/${spec.cost === "ai" ? 1 : spec.max_attempts || "?"}`)),
      h("td", { class: "c-dev", title: j.device }, j.device || h("span", { class: "faint" }, "-")),
      h("td", { class: "c-created r nw" }, h("div", null, when(j.created_ms)), h("div", { class: "t2 num" }, ran(j))),
      h("td", { class: "acts" }, acts),
    );
    return WAITING.has(j.status) ? [row, whyRow(j)] : [row];
  });
  box.replaceChildren(h("div", { class: "scroll" }, h("table", { class: "jobs-table" },
    h("thead", null, h("tr", null, h("th", null, "ID"), h("th", null, "Job"), h("th", null, "Status"), h("th", null, "Device"), h("th", { class: "r" }, "Created"), h("th", { class: "r" }, h("span", { class: "faint" }, "Actions")))),
    h("tbody", null, rows))));
}

// The "why" of a waiting job: its own row under the job, as wide as the table (or the card on a
// phone), built empty and hidden with the table and filled in place by renderWhy(), so a changing
// reason or countdown never rebuilds the table or loses a click.
function whyRow(j) {
  return h("tr", { class: "why-row", "data-key": j.id, hidden: true },
    h("td", { class: "c-id" }),
    h("td", { colspan: 5 }, h("details", { class: "why", "data-key": j.id, "data-status": j.status, open: whyOpen.has(j.id), ontoggle: onWhyToggle },
      h("summary", null), h("div", { class: "why-body" }))));
}
const whyPath = (id) => `/v1/jobs/${encodeURIComponent(id)}/why`;

function onWhyToggle(e) {
  const el = e.currentTarget;
  const id = Number(el.dataset.key);
  if (el.open === whyOpen.has(id)) return; // a rebuild restoring its own state
  if (!el.open) { whyOpen.delete(id); return; }
  whyOpen.add(id);
  api("GET", whyPath(id)).then((data) => whyDetail.set(id, { data }), (err) => {
    if (err instanceof AuthError) logout("The coordinator rejected this token. Paste the current one.");
    else whyDetail.set(id, { error: err.message });
  }).then(renderWhy);
}

const WHY_TAG = { queued: "waiting", leased: "starting", paused: "paused" };
const WHY_QUIET = new Set(["last_seen_ms"]); // moves on every heartbeat without a visible change

function renderWhy() {
  const byJob = new Map((state.why || []).map((s) => [s.job, s]));
  const multi = ((state.status && state.status.devices) || []).length > 1;
  const rows = [...$("jobs").querySelectorAll("details.why")];
  const present = new Set(rows.map((el) => Number(el.dataset.key)));
  for (const id of [...whyOpen]) if (!present.has(id)) whyOpen.delete(id);
  for (const id of [...whyDetail.keys()]) if (!present.has(id)) whyDetail.delete(id);
  rows.forEach((el) => {
    const id = Number(el.dataset.key);
    patchWhy(el, byJob.get(id), whyDetail.get(id), multi);
  });
}

function patchWhy(el, summary, detail, multi) {
  const head = summary && summary.headline;
  const row = el.closest("tr");
  row.hidden = !head;
  row.previousElementSibling.classList.toggle("has-why", Boolean(head));
  if (!head) { delete el.dataset.sig; return; }
  const next = JSON.stringify([el.dataset.status, multi, summary, detail], (k, v) => (NOT_SHOWN.has(k) || WHY_QUIET.has(k) ? undefined : v));
  if (el.dataset.sig === next) return;
  el.dataset.sig = next;
  const where = multi && head.device ? ` on ${head.device}` : "";
  // replaceChildren turns null into the text "null", so only real nodes and strings go in.
  const line = h("span", { class: "why-line", title: head.reason.message }, h("span", { class: "tag" }, `${WHY_TAG[el.dataset.status] || "waiting"}${where}:`), " ", head.reason.message,
    summary.more ? h("span", { class: "more" }, ` +${summary.more} more`) : null);
  el.querySelector("summary").replaceChildren(line);
  el.querySelector(".why-body").replaceChildren(...whyBody(summary, detail, el.dataset.key).filter((k) => k != null));
}

function reasonList(reasons) {
  return h("ul", { class: "why-list" }, reasons.map((r) => h("li", null, h("div", null, r.message),
    r.fix ? h("div", { class: "fix" }, h("strong", null, "Fix: "), r.fix) : null)));
}

// The summary line already says the headline, so the body starts with that reason's fix and then
// lists what else holds the job. Devices that could take it come first; the ones it can never
// run on (job's device list, a missing tool, the profile's cost classes) fold into one line.
function whyBody(summary, detail, id) {
  const head = summary.headline.reason;
  const lead = head.fix ? [h("div", { class: "why-fix" }, h("strong", null, "Fix: "), head.fix)] : [];
  if (detail && detail.error) return [h("div", { class: "why-note" }, `Could not load the reasons: ${detail.error}`)];
  if (!detail) return [...lead, summary.more ? h("div", { class: "why-note" }, "Loading the other reasons") : null];
  const e = detail.data;
  const rest = (list, own) => {
    const i = own ? list.findIndex((r) => r.kind === head.kind && r.message === head.message) : -1;
    return i < 0 ? list : list.filter((_, k) => k !== i);
  };
  const out = [...lead];
  const fromDevice = summary.headline.device;
  const wide = rest(e.reasons, !fromDevice);
  if (wide.length) out.push(h("div", { class: "why-h" }, fromDevice ? "On every device" : "Also on every device"), reasonList(wide));
  const never = e.devices.filter((d) => (d.ineligible || []).length);
  const could = e.devices.filter((d) => !(d.ineligible || []).length);
  for (const d of could) {
    const own = rest(d.reasons, fromDevice === d.device);
    if (own.length) out.push(h("div", { class: "why-h" }, `${d.device} (${d.online ? "online" : "offline"})`), reasonList(own));
  }
  const clear = could.filter((d) => !d.reasons.length).map((d) => d.device);
  if (clear.length) out.push(h("div", { class: "why-note" }, `No device-specific blocker: ${clear.join(", ")}`));
  if (never.length) {
    const cause = (d) => (d.ineligible.length > 1 ? `${d.ineligible[0]} and ${d.ineligible.length - 1} more` : d.ineligible[0]);
    out.push(h("div", { class: "why-note" }, `Not eligible: ${never.map((d) => `${d.device} (${cause(d)})`).join(", ")}. \`idlemax why ${id}\` lists the fix for each.`));
  }
  if (!out.length) out.push(h("div", { class: "why-note" }, e.summary));
  return out;
}

// What a schedule is doing: its running job, when it is next due, and whether it is paused.
function scheduleState(v) {
  const d = v.decision || {};
  const parts = [];
  if (d.kind === "running") parts.push(badge(`job #${d.job} ${label(d.status)}`, JOB_TONE[d.status]));
  else if (d.kind === "wait") parts.push(rel(d.until_ms, "due "));
  else if (d.kind === "fire") parts.push(h("span", { class: "num" }, "due now"));
  if (v.paused) parts.push(badge("paused", "warn"));
  return parts.map((p) => h("div", null, p));
}

function lastResult(r) {
  if (!r) return h("span", { class: "faint" }, "-");
  const note = r.error || (r.exit_code ? `exit ${r.exit_code}` : "");
  return [
    jobBadge(r.status),
    r.finished_ms ? h("div", { class: "t2" }, rel(r.finished_ms)) : null,
    note ? h("div", { class: "t2", style: r.error ? "color:var(--bad)" : null }, note) : null,
  ];
}

async function removeSchedule(btn, v) {
  const name = (v.spec && v.spec.name) || "(unnamed)";
  const ok = await confirmBox(`Remove schedule #${v.id}?`, `${name} stops repeating. A job it already queued keeps running.`, "Remove", true);
  if (ok) act(btn, `Removed schedule #${v.id}`, () => api("DELETE", `/v1/schedules/${encodeURIComponent(v.id)}`));
}

function renderSchedules() {
  const box = $("schedules");
  const list = state.schedules;
  if (state.errors.schedules) { box.replaceChildren(h("div", { class: "err" }, state.errors.schedules)); return; }
  if (!list) { box.replaceChildren(h("div", { class: "empty" }, "Loading")); return; }
  if (!list.length) { box.replaceChildren(h("div", { class: "empty" }, "No schedules yet. Repeat a job with `idlemax schedule add <spec.toml> --every 1d`.")); return; }
  const rows = list.map((v) => {
    const spec = v.spec || {};
    const verb = v.paused ? "resume" : "pause";
    const detail = [`every ${v.every}`, spec.cost ? label(spec.cost) : null, spec.ai_account ? `account ${spec.ai_account}` : null].filter(Boolean).join(" · ");
    return h("tr", null,
      h("td", { class: "c-id num mono" }, `#${v.id}`),
      h("td", { class: "c-name" }, h("div", { class: "job-name" }, spec.name || "(unnamed)"), h("div", { class: "t2" }, detail)),
      h("td", { class: "c-state" }, scheduleState(v)),
      h("td", { class: "c-last" }, lastResult(v.last_result)),
      h("td", { class: "acts" },
        h("button", { class: "btn sm", type: "button", onclick: (e) => act(e.currentTarget, `${v.paused ? "Resumed" : "Paused"} schedule #${v.id}`, () => api("POST", `/v1/schedules/${encodeURIComponent(v.id)}/${verb}`)) }, v.paused ? "Resume" : "Pause"),
        h("button", { class: "btn sm danger", type: "button", onclick: (e) => removeSchedule(e.currentTarget, v) }, "Remove")),
    );
  });
  box.replaceChildren(h("div", { class: "scroll" }, h("table", { class: "sched-table" },
    h("thead", null, h("tr", null, h("th", null, "ID"), h("th", null, "Schedule"), h("th", null, "State"), h("th", null, "Last result"), h("th", { class: "r" }, h("span", { class: "faint" }, "Actions")))),
    h("tbody", null, rows))));
}

function renderAdvice() {
  const box = $("advice");
  const recs = state.advice;
  if (state.errors.advice) { box.replaceChildren(h("div", { class: "err" }, state.errors.advice)); return; }
  if (!recs) { box.replaceChildren(h("div", { class: "empty" }, "Loading")); return; }
  const open = recs.filter((r) => r.status === "open");
  const closed = recs.filter((r) => r.status !== "open");
  const items = open.map((r) => adviceItem(r, true));
  if (!open.length) items.push(h("div", { class: "empty" }, "Nothing needs you right now."));
  if (closed.length) {
    items.push(h("div", { class: "adv", style: "padding:8px 14px" },
      h("button", { class: "btn ghost sm", type: "button", onclick: () => { state.showClosed = !state.showClosed; render(); } },
        state.showClosed ? `Hide ${closed.length} closed` : `Show ${closed.length} closed`)));
    if (state.showClosed) items.push(...closed.slice(0, 30).map((r) => adviceItem(r, false)));
  }
  box.replaceChildren(...items);
}

function adviceItem(r, isOpen) {
  const job = r.suggested_job;
  const conf = r.confidence != null ? `${Math.round(Math.max(0, Math.min(1, r.confidence)) * 100)}% confidence` : null;
  return h("article", { class: `adv ${isOpen ? "" : "closed"}` },
    h("div", { class: "adv-h" }, badge(label(r.kind), KIND_TONE[r.kind]), h("span", { class: "title" }, r.title || "(untitled)"),
      isOpen ? null : badge(label(r.status), r.status === "accepted" ? "ok" : "")),
    r.why ? h("p", null, r.why) : null,
    job ? h("div", { class: "sugg" }, h("span", { class: "muted" }, "Queues "), h("strong", null, job.name || "(unnamed)"),
      h("span", { class: "muted" }, ` · ${label(job.cost)}${job.devices && job.devices.length ? ` · on ${job.devices.join(", ")}` : ""}`),
      job.command && job.command.length ? h("div", { class: "mono", style: "margin-top:4px" }, job.command.join(" ")) : null) : null,
    r.evidence && r.evidence.length ? h("details", null, h("summary", null, `Evidence (${r.evidence.length})`), h("ul", null, r.evidence.map((e) => h("li", null, e)))) : null,
    h("div", { class: "meta" }, h("span", null, r.source === "ai" ? "AI advisor" : "Rules"), conf ? h("span", null, conf) : null, when(r.at_ms)),
    isOpen ? h("div", { class: "adv-a" },
      job ? h("button", { class: "btn sm primary", type: "button", onclick: (e) => act(e.currentTarget, (out) => out && out.id ? `Queued job #${out.id}` : "Accepted", () => api("POST", `/v1/advice/${encodeURIComponent(r.id)}/accept`)) }, "Accept and queue") : null,
      h("button", { class: "btn sm", type: "button", onclick: (e) => act(e.currentTarget, "Dismissed", () => api("POST", `/v1/advice/${encodeURIComponent(r.id)}/dismiss`)) }, "Dismiss")) : null,
  );
}

function windowLabel(w) {
  if (!w) return "window";
  if (w.kind === "weekly") return "Weekly";
  if (w.kind === "rolling") return w.minutes % 60 === 0 ? `${w.minutes / 60}-hour` : `${w.minutes}-minute`;
  return label(w.kind);
}

function renderQuota() {
  const box = $("quota");
  const rows = state.quota;
  if (state.errors.quota) { box.replaceChildren(h("div", { class: "err", style: "padding:0" }, state.errors.quota)); return; }
  if (!rows) { box.replaceChildren(h("div", { class: "empty" }, "Loading")); return; }
  if (!rows.length) { box.replaceChildren(h("div", { class: "empty" }, "No AI accounts configured. Add [[quota.accounts]] to config.toml.")); return; }
  const paused = new Set((state.status && state.status.paused_accounts) || []);
  const byAcct = new Map();
  for (const r of rows) {
    if (!byAcct.has(r.account)) byAcct.set(r.account, []);
    byAcct.get(r.account).push(r);
  }
  box.replaceChildren(...[...byAcct].map(([name, wins]) => {
    // spendable and reasons are the account verdict, repeated on every window row.
    const spendable = wins.length > 0 && wins.every((w) => w.spendable);
    const reasons = wins[0].reasons || [];
    const isPaused = paused.has(name);
    return h("div", { class: "acct" },
      h("div", { class: "acct-h" },
        h("span", { class: "name" }, name),
        h("span", { class: "muted", style: "font-size:12px" }, label(wins[0].provider)),
        isPaused ? badge("paused", "warn") : spendable ? badge("spendable", "ok") : badge("held", ""),
        isPaused
          ? h("button", { class: "btn sm", type: "button", onclick: (e) => act(e.currentTarget, `Resumed ${name}`, () => control(scopes.account(name), "resume")) }, "Resume")
          : h("button", { class: "btn sm", type: "button", onclick: (e) => act(e.currentTarget, `Paused ${name}`, () => control(scopes.account(name), "pause")) }, "Pause")),
      wins.filter((w) => w.window).map(quotaWindow),
      reasons.length ? h("ul", { class: "reasons" }, reasons.map((r) => h("li", null, r))) : null);
  }));
}

const WINDOW_STATE = { not_started: "not started", stale: "stale data", reset_pending: "reset, waiting for data", no_data: "no data" };

function quotaWindow(w) {
  const used = w.used_pct;
  const known = used != null && isFinite(used);
  const pct = known ? Math.max(0, Math.min(100, used)) : 0;
  const line = 100 - Math.max(0, Math.min(100, w.reserve_pct || 0));
  const tone = !known ? "" : pct >= 100 ? "bad" : pct >= line ? "warn" : "";
  const bar = h("span", { class: tone });
  bar.style.width = `${pct}%`;
  const mark = h("i", { title: `reserve starts at ${line}%` });
  mark.style.left = `calc(${line}% - 1px)`;
  const stateNote = WINDOW_STATE[w.state];
  return h("div", { class: "win" },
    h("div", { class: "win-h" },
      h("span", null, h("strong", null, windowLabel(w.window)),
        h("span", { class: "muted num" }, ` ${known ? `${Math.round(used)}% used` : "usage unknown"}, reserve ${fmtPct(w.reserve_pct)}`),
        stateNote ? h("span", { class: "faint" }, `, ${stateNote}`) : null),
      w.resets_at_ms ? rel(w.resets_at_ms, "resets ", "muted num") : h("span", { class: "muted num" }, "reset unknown")),
    h("div", { class: "meter", role: "img", "aria-label": known ? `${Math.round(used)} percent used, reserve from ${line} percent` : "usage unknown" }, bar, mark),
  );
}

function renderEvents() {
  const box = $("events");
  const evs = state.events;
  if (state.errors.events) { box.replaceChildren(h("div", { class: "err" }, state.errors.events)); return; }
  if (!evs) { box.replaceChildren(h("div", { class: "empty" }, "Loading")); return; }
  if (!evs.length) { box.replaceChildren(h("div", { class: "empty" }, "No events yet.")); return; }
  const tone = (k) => /failed|killed|offline|error|expired/.test(k) ? "bad" : /parked|paused|guard|quota/.test(k) ? "warn" : /succeeded|online/.test(k) ? "ok" : "";
  box.replaceChildren(...evs.map((e) => h("div", { class: "ev" },
    h("span", { class: "when num mono", title: fmtTime(e.at_ms) }, clock(e.at_ms)),
    h("span", { class: "what" }, badge(label(e.kind), tone(String(e.kind))),
      e.device ? h("span", { class: "muted" }, e.device) : null,
      e.job != null ? h("span", { class: "muted mono" }, `#${e.job}`) : null),
    e.message ? h("span", { class: "msg" }, e.message) : null,
  )));
}

// Login flow.
function logout(msg) {
  tokenStore.set("");
  clearInterval(state.timer);
  Object.assign(state, { status: null, jobs: null, schedules: null, quota: null, advice: null, events: null, why: null, errors: {}, lastOk: 0 });
  whyOpen.clear();
  whyDetail.clear();
  Object.keys(sig).forEach((k) => delete sig[k]);
  $("app").hidden = true;
  $("login").hidden = false;
  const err = $("login-err");
  err.textContent = msg || "";
  err.hidden = !msg;
  $("token-input").value = "";
  $("token-input").focus();
}
function start() {
  $("login").hidden = true;
  $("app").hidden = false;
  render();
  refresh();
  schedule();
}
$("login-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const t = $("token-input").value.trim();
  if (!t) return;
  tokenStore.set(t);
  const btn = e.submitter || null;
  if (btn) btn.disabled = true;
  try {
    await api("GET", "/v1/status");
    start();
  } catch (err) {
    if (err instanceof AuthError) logout("That token was rejected.");
    else { tokenStore.set(""); $("login-err").textContent = `Cannot reach the coordinator: ${err.message}`; $("login-err").hidden = false; }
  } finally {
    if (btn) btn.disabled = false;
  }
});

if (tokenStore.get()) start(); else logout("");
})();
