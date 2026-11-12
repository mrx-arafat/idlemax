(() => {
"use strict";

const POLL_MS = 5000;
// The report changes slowly and costs the coordinator a scan, so it is asked for less often.
const REPORT_MS = 30000;
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
function badge(text, tone, dot, title) { return h("span", { class: `badge ${tone || ""}`, title }, dot ? h("span", { class: "dot" }) : null, text); }
function jobBadge(status) { return badge(label(status), JOB_TONE[status]); }
function guardText(action) {
  if (!action) return "unknown";
  if (action.kind === "run") return `run, ${action.slots} slot${action.slots === 1 ? "" : "s"} and ${action.mem_mib} MiB free`;
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
  if (!res.ok) { const err = new Error(msg); err.status = res.status; throw err; }
  return data;
}
// A route an older coordinator does not have answers 404: null then hides what it feeds.
const optional = (p) => p.catch((e) => { if (e && e.status === 404) return null; throw e; });
const control = (scope, action, reason) => api("POST", "/v1/control", { scope, action, reason: reason || "dashboard" });
const scopes = {
  global: () => ({ type: "global" }),
  device: (id) => ({ type: "device", id }),
  account: (id) => ({ type: "account", id }),
  job: (id) => ({ type: "job", id }),
};

// State and polling.
// dispatcher, limits and proposals are undefined while loading and null when the route is absent.
const state = { status: null, jobs: null, schedules: null, quota: null, advice: null, events: null, why: null, report: null, reportAt: 0, errors: {}, lastOk: 0, allFailed: false, busy: false, timer: 0, showClosed: false, dispatcher: undefined, limits: undefined, proposals: undefined };
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
  // A route that answered 404 is not asked again until the page reloads, so an older
  // coordinator leaves no stream of failed requests in the console.
  for (const [key, path] of [["dispatcher", "/v1/dispatcher"], ["limits", "/v1/limits"], ["proposals", "/v1/proposals"]]) {
    if (state[key] !== null) calls.push([key, optional(api("GET", path))]);
  }
  if (Date.now() - state.reportAt >= REPORT_MS) calls.push(["report", reportCalls()]);
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
    if (r.status === "fulfilled") { state[key] = r.value; delete state.errors[key]; anyOk = true; if (key === "report") state.reportAt = Date.now(); }
    else state.errors[key] = r.reason && r.reason.message ? r.reason.message : "request failed";
  });
  if (anyOk) state.lastOk = Date.now();
  details.forEach((r, i) => whyDetail.set(openIds[i], r.status === "fulfilled" ? { data: r.value } : { error: r.reason && r.reason.message ? r.reason.message : "request failed" }));
  render();
}

// Today so far (from this browser's midnight) and the last 7 days, as `idlemax report` shows them.
function reportCalls() {
  const midnight = new Date();
  midnight.setHours(0, 0, 0, 0);
  const since = Math.min(midnight.getTime(), Date.now() - 1);
  return Promise.all([api("GET", `/v1/report?since_ms=${since}&label=today`), api("GET", "/v1/report?days=7")]);
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
  const focusables = () => [...box.querySelectorAll("button, select, summary, input")];
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
  section("report", [state.report, e.report], "report", renderReport);
  section("devices", [devs, s && s.global_paused, e.status], "devices", renderDevices);
  section("schedules", [state.schedules, e.schedules], "schedules", renderSchedules);
  section("jobs", [state.jobs, e.jobs, $("job-filter").value], "jobs", renderJobs);
  section("advice", [state.advice, e.advice, state.showClosed], "advice", renderAdvice);
  section("quota", [state.quota, s && s.paused_accounts, e.quota], "quota", renderQuota);
  section("events", [state.events, e.events], "events", renderEvents);
  renderDispatcherCards();
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

// Same wording as the verdict line the coordinator writes: seconds, minutes, hours, then days.
function fmtSpan(ms) {
  ms = Math.max(0, ms);
  if (ms < 60000) return `${Math.round(ms / 1000)} s`;
  if (ms < 3600000) return `${Math.round(ms / 60000)} min`;
  if (ms < 2 * 86400000) return `${(ms / 3600000).toFixed(1)} h`;
  return `${(ms / 86400000).toFixed(1)} days`;
}

function reportLines(r) {
  const j = r.jobs, g = r.guard, i = r.idle, q = r.quota;
  const lines = [`Jobs: ${j.ran} ran, ${j.succeeded} succeeded, ${j.failed} failed, ${j.killed} killed, ${j.cancelled} cancelled; ${j.job_ms > 0 && j.job_ms < 30000 ? "<1" : Math.round(j.job_ms / 60000)} job-min`];
  if (g.tracked_ms > 0) {
    const by = (g.held_by || []).slice(0, 4).map((x) => `${x.label} ${fmtSpan(x.ms)}`).join(", ");
    lines.push(g.held_ms > 0 ? `Held: ${fmtSpan(g.held_ms)} of ${fmtSpan(g.tracked_ms)} tracked: ${by}` : `Held: never, in ${fmtSpan(g.tracked_ms)} tracked`);
    if (g.offline_ms > 0) lines.push(`Asleep or offline: ${fmtSpan(g.offline_ms)}`);
    lines.push(`Idle: jobs running ${fmtSpan(i.used_ms)}, queue empty ${fmtSpan(i.queue_empty_ms)}, work waiting ${fmtSpan(i.work_waiting_ms)}${i.quota_waiting_ms > 0 ? `, AI work waiting for quota ${fmtSpan(i.quota_waiting_ms)}` : ""}`);
    if ((i.stuck_jobs || []).length) lines.push(`Queued, and no device can run: ${i.stuck_jobs.join(", ")}`);
    const l = i.longest;
    lines.push(l ? `Longest idle stretch: ${fmtSpan(l.end_ms - l.start_ms)} from ${fmtTime(l.start_ms)} to ${fmtTime(l.end_ms)}: ${l.label}` : "Longest idle stretch: none");
  }
  if (!q.configured) lines.push("AI quota: no AI accounts are configured");
  else if (!(q.expired || []).length) lines.push("AI quota: no window has reset in this period");
  else {
    const unused = q.expired.reduce((sum, c) => sum + c.unused_pct, 0) / q.expired.length;
    const kept = q.counted_since_ms > r.since_ms ? ` (only the last ${fmtSpan(r.until_ms - q.counted_since_ms)} of quota history is kept)` : "";
    lines.push(`AI quota: ${q.expired.length} window${q.expired.length === 1 ? "" : "s"} expired, ${unused.toFixed(0)}% unused on average${kept}`);
  }
  return lines;
}

function renderReport() {
  const reports = state.report;
  if (!reports) {
    $("report").replaceChildren(h("p", { class: "muted" }, state.errors.report ? `The report is unavailable: ${state.errors.report}` : "Loading"));
    return;
  }
  $("report").replaceChildren(...reports.map((r) => h("div", { class: "rep" },
    h("div", { class: `rep-v${r.jobs.ran ? "" : " warn"}` }, r.verdict),
    h("ul", { class: "rep-l" }, reportLines(r).map((t) => h("li", null, t))))));
}

function stat(k, v, title) { return h("div", { class: "stat", title: title || null }, h("div", { class: "k" }, k), h("div", { class: "v num" }, v)); }

function renderDevices() {
  const box = $("devices");
  const s = state.status;
  if (!s) { box.replaceChildren(state.errors.status ? h("div", { class: "err" }, state.errors.status) : h("div", { class: "empty" }, "Loading")); return; }
  const devs = (s.devices || []).slice().sort((a, b) => (b.online - a.online) || String(a.info.id).localeCompare(String(b.info.id)));
  if (!devs.length) { box.replaceChildren(h("div", { class: "empty" }, "No device has sent a heartbeat yet. Start `idlemax agent` on a device.")); return; }
  box.replaceChildren(h("div", { class: "devices" }, devs.map((d) => deviceCard(d, Boolean(s.global_paused), s.accounts))));
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

// The OS and the profile.
function osAndProfile(info) {
  const os = label(info.os);
  const profile = label(info.profile);
  return [os, profile].filter(Boolean).join(" · ");
}

// How much of the device's allowance is in use and why it is not using more, as the coordinator
// worked it out (`capacity` in GET /v1/status). An older coordinator sends none, and an offline
// device already says so in its header.
const CAPACITY_TONE = { full: "ok", starting: "ok", idle: "info", waiting: "info", too_big: "warn", cannot_run: "warn", held: "warn" };
function fmtMib(m) { return m >= 1024 ? `${(m / 1024).toFixed(1)} GiB` : `${m} MiB`; }
function capacityMeter(name, used, allowed, text) {
  const pct = allowed > 0 ? Math.max(0, Math.min(100, (used / allowed) * 100)) : 0;
  const bar = h("span");
  bar.style.width = `${pct}%`;
  return h("div", { class: "cap" },
    h("div", { class: "cap-h" }, h("span", { class: "muted" }, name), h("span", { class: "num" }, text)),
    h("div", { class: "meter", role: "img", "aria-label": `${name}: ${text}` }, bar));
}
function capacityBlock(c) {
  if (!c || c.state === "offline") return null;
  const slots = c.slots_allowed != null ? `${c.slots_used} / ${c.slots_allowed}` : `${c.slots_used}`;
  const ram = c.mem_allowed_mib != null ? `${fmtMib(c.mem_used_mib)} / ${fmtMib(c.mem_allowed_mib)}` : fmtMib(c.mem_used_mib);
  // The badge already says "held", so the reason drops its own "held: " prefix.
  const why = c.state === "held" ? c.why.replace(/^held: /, "") : c.why;
  return h("div", { class: "capacity" },
    h("div", { class: "caps" },
      capacityMeter("Slots", c.slots_used, c.slots_allowed, slots),
      capacityMeter("RAM", c.mem_used_mib, c.mem_allowed_mib, ram)),
    h("div", { class: "cap-why" }, badge(label(c.state), CAPACITY_TONE[c.state]), h("span", null, why)));
}

// The AI accounts this device may spend: whether idlemax may spend each now, and the quota left
// before its window resets (a separate question: a shut account can have plenty left).
function deviceQuota(info, accounts) {
  const mine = (accounts || []).filter((a) => (info.ai_accounts || []).includes(a.account));
  if (!mine.length) return null;
  return h("div", { class: "dev-jobs" }, mine.map((a) => h("div", { class: "row" },
    a.paused ? badge("paused", "warn") : a.spendable ? badge("spendable", "ok") : badge("held", ""),
    h("span", { class: "t", title: a.why || null }, a.account),
    h("span", { class: "faint num", style: "margin-left:auto" },
      a.headroom_pct != null ? `${fmtPct(a.headroom_pct)} left` : "left unknown",
      a.resets_at_ms ? [" · ", rel(a.resets_at_ms, "resets ", "num")] : null))));
}

function deviceCard(d, byAll, accounts) {
  const info = d.info || {};
  const m = d.metrics || null;
  const g = d.guard || null;
  const kind = g && g.action ? g.action.kind : null;
  const badges = [
    d.online ? badge("online", "ok", true) : badge(rel(d.last_seen_ms, "offline, seen ", ""), "bad", true),
    d.paused ? badge(byAll ? "paused (all)" : "paused", "warn") : null,
    // The agent reports one flag for the PAUSE file and for `idlemax local pause`, so name neither.
    d.local_paused ? badge("paused on device", "warn", false, "Paused on the device itself, by `idlemax local pause` or the PAUSE file in $IDLEMAX_HOME. Run `idlemax local resume` there and delete PAUSE.") : null,
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
    capacityBlock(d.capacity),
    g ? h("div", { class: "guard" }, h("span", { class: "muted" }, "Guard"), badge(guardText(g.action), GUARD_TONE[kind]), g.user_active ? badge("user active", "") : null) : null,
    g && g.reasons && g.reasons.length ? h("ul", { class: "reasons" }, g.reasons.map((r) => h("li", null, r))) : null,
    stats.length ? h("div", { class: "stats" }, stats) : h("div", { class: "muted", style: "margin-top:10px;font-size:12.5px" }, "No metrics yet."),
    deviceQuota(info, accounts),
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

// Dispatcher (GET /v1/dispatcher): the mode switch, the workers, what would launch next, and
// the cards beside them. Every card is hidden when the coordinator has no dispatcher view.
const LINE_TONE = { run: "ok", hold: "", shed: "warn", emergency: "bad" };
const MODES = ["off", "propose", "auto"];
const DISP_CARDS = ["card-dispatcher", "card-needs", "card-review", "card-budget", "card-projects", "card-limits"];
// Unsaved limit text by key, and the last reply of each limit, kept across rebuilds.
const limitDraft = new Map();
const limitReply = new Map();

function renderDispatcherCards() {
  const v = state.dispatcher;
  // Hidden while loading and when the coordinator has no dispatcher view; an error shows.
  const absent = v === null || (v === undefined && !state.errors.dispatcher);
  DISP_CARDS.forEach((id) => { $(id).hidden = absent; });
  $("card-rules").hidden = absent || state.proposals === null;
  if (absent) return;
  const e = state.errors;
  section("dispatcher", [v, e.dispatcher], "dispatcher", renderDispatcher);
  section("needs", [v && v.needs_you, e.dispatcher], "needs", renderNeeds);
  section("review", [v && v.review, e.dispatcher], "review", renderReview);
  section("budget", [v && v.budgets, e.dispatcher], "budget", renderBudget);
  section("projects", [v && v.projects, v && v.devices, e.dispatcher], "projects", renderProjects);
  section("limits", [state.limits, v && v.devices, e.limits, [...limitReply]], "limits", renderLimits);
  section("rules", [state.proposals, e.proposals], "rules", renderRules);
}

// Loading, an error, or the view itself.
function dispReady(box) {
  if (state.errors.dispatcher) { box.replaceChildren(h("div", { class: "err" }, state.errors.dispatcher)); return null; }
  if (!state.dispatcher) { box.replaceChildren(h("div", { class: "empty" }, "Loading")); return null; }
  return state.dispatcher;
}

async function setMode(btn, mode) {
  if (mode === "auto" && !await confirmBox("Switch to auto?", "idlemax then launches workers by itself, inside the fence you set: projects on, the limits and each account's budget.", "Switch to auto")) return;
  act(btn, `Dispatcher mode: ${mode}`, () => api("POST", "/v1/dispatcher/mode", { mode }));
}

// A coordinator without the worker stop route still stops: stop_all ends every running job,
// workers included, and holds new ones until Resume.
async function stopWorkers(btn) {
  if (await confirmBox("Stop every worker now?", "Each worker's turn is cut and its state saved. They continue only after Start.", "Stop workers", true)) {
    act(btn, (out) => out === "stop_all" ? "No worker stop on this coordinator: stopped all jobs instead (Resume continues them)" : "Workers stopped",
      () => api("POST", "/v1/workers/stop", { scope: scopes.global() })
        .catch((e) => { if (e && e.status === 404) return control(scopes.global(), "stop_all").then(() => "stop_all"); throw e; }));
  }
}

async function startWorkers(btn) {
  if (await confirmBox("Start the workers again?", "The stopped workers continue where they were, once memory and quota allow.", "Start")) {
    act(btn, "Workers started", () => api("POST", "/v1/workers/start").catch((e) => {
      if (e && e.status === 404) throw new Error("this coordinator has no worker start yet; Resume continues stopped jobs");
      throw e;
    }));
  }
}

function modeSwitch(mode) {
  return h("div", { class: "seg", role: "radiogroup", "aria-label": "Dispatcher mode" }, MODES.map((m) => h("button", {
    class: `btn sm${m === mode ? " primary" : ""}`, type: "button", role: "radio", "aria-checked": String(m === mode),
    onclick: (e) => { if (m !== mode) setMode(e.currentTarget, m); },
  }, label(m))));
}

function workerTable(rows, withWhy) {
  return h("div", { class: "scroll" }, h("table", { class: "disp-table" },
    h("thead", null, h("tr", null, h("th", null, "State"), h("th", null, "Task"), h("th", null, "Model"), h("th", { class: "r" }, "Started"))),
    h("tbody", null, rows.map((w) => h("tr", null,
      h("td", { class: "c-state" }, badge(w.label, w.state === "stopped" ? "warn" : w.state === "needs_you" ? "warn" : "info")),
      h("td", { class: "c-task" }, h("div", { class: "job-name" }, `${w.project}: ${w.task}`), h("div", { class: "t2" }, w.summary),
        w.branch ? h("div", { class: "t2 mono" }, w.branch) : null,
        w.unsandboxed ? h("div", { class: "t2" }, badge("no sandbox", "warn"), " ", w.unsandboxed) : null,
        withWhy && w.why ? h("details", { class: "why" }, h("summary", null, "why"), h("div", { class: "why-body" }, w.why)) : null),
      h("td", { class: "c-model nw" }, h("div", null, label(w.provider)), h("div", { class: "t2" }, w.model)),
      h("td", { class: "c-start r nw" }, rel(w.start_ms)))))));
}

function renderDispatcher() {
  const box = $("dispatcher");
  const v = dispReady(box);
  if (!v) return;
  const stopped = v.workers.some((w) => w.state === "stopped");
  const devices = v.devices.map((d) => h("div", { class: "row" },
    badge(d.state, LINE_TONE[d.state]), h("strong", null, d.id),
    h("span", { class: "t" }, d.held ? `held: ${d.held}` : `${d.open_slots} open slot${d.open_slots === 1 ? "" : "s"}: ${d.reason}`)));
  const planned = v.planned.map((p) => h("div", { class: "row" },
    h("span", { class: "t" }, `${p.project}: ${p.title}`),
    h("span", { class: "faint" }, `${label(p.provider)} ${p.account} ${p.model} on ${p.device}`),
    p.reason ? h("details", { class: "why" }, h("summary", null, "why"), h("div", { class: "why-body" }, p.reason)) : null));
  const unjudged = v.ready_unjudged.map((t) => h("div", { class: "row" },
    badge("not judged by the orchestrator", ""), h("span", { class: "t" }, `${t.project}: ${t.title}`)));
  box.replaceChildren(...[
    h("div", { class: "disp-top" }, modeSwitch(v.mode),
      h("button", { class: "btn sm danger", type: "button", onclick: (e) => stopWorkers(e.currentTarget) }, "Stop workers"),
      stopped ? h("button", { class: "btn sm primary", type: "button", onclick: (e) => startWorkers(e.currentTarget) }, "Start") : null),
    devices.length ? h("div", { class: "disp-list" }, devices) : null,
    h("div", { class: "why-h" }, "Workers"),
    v.workers.length ? workerTable(v.workers, true) : h("div", { class: "empty" }, "No worker is running."),
    h("div", { class: "why-h" }, "Would launch next"),
    planned.length ? h("div", { class: "disp-list" }, planned) : h("div", { class: "muted" }, "Nothing would launch now."),
    unjudged.length ? h("div", { class: "disp-list" }, unjudged) : null,
    h("div", { class: "faint", style: "margin-top:8px;font-size:12.5px" }, v.picked_by),
  ].filter(Boolean)); // replaceChildren would write null as the text "null"
}

const NEED_TONE = { task: "warn", account: "bad", device: "bad" };
function renderNeeds() {
  const box = $("needs");
  const v = dispReady(box);
  if (!v) return;
  if (!v.needs_you.length) { box.replaceChildren(h("div", { class: "empty" }, "Nothing needs you.")); return; }
  box.replaceChildren(h("div", { class: "disp-list" }, v.needs_you.map((n) => h("div", { class: "row" },
    badge(label(n.kind), NEED_TONE[n.kind]), h("strong", null, n.subject), h("span", { class: "t" }, n.reason)))));
}

function renderReview() {
  const box = $("review");
  const v = dispReady(box);
  if (!v) return;
  // What waits for the owner: the runs a project's `waiting` counts (clean, needs_you). A
  // rejected run keeps its branch but waits for nobody; `idlemax review` still lists it.
  const waiting = v.review.filter((w) => w.state === "clean" || w.state === "needs_you");
  const note = h("div", { class: "faint", style: "margin-top:8px;font-size:12.5px" }, "Keep or discard with idlemax review keep <run> or idlemax review discard <run>.");
  box.replaceChildren(waiting.length ? workerTable(waiting, false) : h("div", { class: "empty" }, "Nothing waits for review."), note);
}

// One bar: what was spent of what may be spent, with the reason as its title.
function budgetBar(day, big) {
  const of = day.spendable_pct;
  const spent = day.spent_pct || 0;
  const pct = of > 0 ? Math.max(0, Math.min(100, (spent / of) * 100)) : spent > 0 ? 100 : 0;
  const bar = h("span", { class: pct >= 100 ? "warn" : "" });
  bar.style.width = `${pct}%`;
  const text = `spent ${fmtPct(spent, 1)}${of != null ? ` of ${fmtPct(of, 1)}` : ""}`;
  return h("div", { class: big ? "win" : "win day", title: day.why || null },
    h("div", { class: "win-h" }, h("span", { class: "num" }, text),
      big ? (day.day_end_ms ? rel(day.day_end_ms, "day ends ", "muted num") : null) : h("span", { class: "faint num" }, clock(day.day_start_ms))),
    h("div", { class: "meter", role: "img", "aria-label": text }, bar));
}

function renderBudget() {
  const box = $("budget");
  const v = dispReady(box);
  if (!v) return;
  if (!v.budgets.length) { box.replaceChildren(h("div", { class: "empty" }, "No AI account is configured.")); return; }
  box.replaceChildren(...v.budgets.map((b) => {
    const [today, ...earlier] = b.days;
    return h("div", { class: "acct" },
      h("div", { class: "acct-h" }, h("span", { class: "name" }, b.account), h("span", { class: "muted", style: "font-size:12px" }, label(b.unit === "daily_cap_runs" ? "daily cap" : b.provider))),
      today ? budgetBar(today, true) : null,
      today && today.why ? h("div", { class: "t2" }, today.why) : null,
      earlier.length ? h("div", { class: "days" }, earlier.map((d) => budgetBar(d, false))) : null);
  }));
}

async function switchProject(btn, p) {
  const on = !p.enabled;
  if (on && !await confirmBox(`Switch ${p.name} on?`, "The dispatcher may then plan and launch workers on its leftover tasks.", "Switch on")) return;
  act(btn, `${p.name} switched ${on ? "on" : "off"}`, () => api("POST", `/v1/projects/${encodeURIComponent(p.id)}/${on ? "on" : "off"}`));
}

async function switchDevice(btn, d) {
  const on = !d.enabled;
  if (on && !await confirmBox(`Switch ${d.id} on?`, "Workers may then run on this device.", "Switch on")) return;
  act(btn, `${d.id} switched ${on ? "on" : "off"}`, () => api("POST", `/v1/devices/${encodeURIComponent(d.id)}/${on ? "on" : "off"}`));
}

function toggle(on, name, onclick) {
  return h("button", { class: `btn sm${on ? " primary" : ""}`, type: "button", role: "switch", "aria-checked": String(on), "aria-label": `${name} ${on ? "on" : "off"}`, onclick }, on ? "on" : "off");
}

function renderProjects() {
  const box = $("projects");
  const v = dispReady(box);
  if (!v) return;
  // Drawn whenever the coordinator knows the switch: a lone Mac starts off and needs it (D19).
  const devices = v.devices.filter((d) => d.enabled != null);
  const rows = [
    ...devices.map((d) => h("div", { class: "row" }, toggle(d.enabled, d.id, (e) => switchDevice(e.currentTarget, d)), h("strong", null, d.id), h("span", { class: "faint" }, "device"))),
    ...v.projects.map((p) => h("div", { class: "row" }, toggle(p.enabled, p.name, (e) => switchProject(e.currentTarget, p)),
      h("strong", null, p.name), h("span", { class: "faint" }, p.device),
      h("span", { class: "faint num", style: "margin-left:auto" }, `${p.waiting}/${p.review_cap} waiting for review`))),
  ];
  box.replaceChildren(rows.length ? h("div", { class: "disp-list" }, rows) : h("div", { class: "empty" }, "No project yet. Add one with idlemax project add <path>."));
}

function replyText(r) {
  const rep = r.report || {};
  if ((rep.restart_coordinator || []).length) return "restart the coordinator";
  if ((rep.restart_agent || []).length) return "restart the agent";
  return r.changed ? "applied now" : "no change";
}

async function saveLimit(btn, key, value) {
  const out = await act(btn, `${key} saved`, () => api("POST", "/v1/limits", { key, value }));
  // A refused value keeps what the owner typed; the toast says why.
  if (!out) return;
  limitDraft.delete(key);
  limitReply.set(key, replyText(out));
}

function limitRow(s) {
  const id = `limit-${s.key.replace(/[^a-z0-9]/gi, "-")}`;
  const input = h("input", { id, class: "num", type: "text", inputmode: "decimal", value: limitDraft.has(s.key) ? limitDraft.get(s.key) : String(s.value ?? "") });
  input.addEventListener("input", () => limitDraft.set(s.key, input.value));
  return h("div", { class: "limit" },
    h("label", { for: id, class: "mono" }, s.key),
    h("div", { class: "limit-c" }, input,
      h("span", { class: "faint num" }, `now ${s.value}, default ${s.default}`),
      h("button", { class: "btn sm", type: "button", onclick: (e) => saveLimit(e.currentTarget, s.key, input.value.trim()) }, "Save"),
      h("button", { class: "btn sm ghost", type: "button", onclick: (e) => saveLimit(e.currentTarget, s.key, null) }, "default")),
    limitReply.has(s.key) ? h("div", { class: "t2" }, limitReply.get(s.key)) : null);
}

function renderLimits() {
  const box = $("limits");
  if (state.errors.limits) { box.replaceChildren(h("div", { class: "err" }, state.errors.limits)); return; }
  if (state.limits === undefined) { box.replaceChildren(h("div", { class: "empty" }, "Loading")); return; }
  const own = state.limits ? state.limits.map(limitRow) : [h("div", { class: "muted" }, "This coordinator cannot change limits.")];
  // Whose config.toml the inputs change: only the coordinator's own machine (A23).
  const whose = state.dispatcher && state.dispatcher.limits_device;
  $("h-limits").textContent = whose ? `Limits of ${whose}` : "Limits";
  const others = ((state.dispatcher && state.dispatcher.devices) || []).filter((d) => (d.limits || []).length).map((d) =>
    h("div", { class: "acct" }, h("div", { class: "acct-h" }, h("span", { class: "name" }, d.id), badge("read-only", "")),
      h("div", { class: "disp-list" }, d.limits.map((s) => h("div", { class: "row" }, h("span", { class: "mono" }, s.key), h("span", { class: "num", style: "margin-left:auto" }, String(s.value))))),
      h("div", { class: "t2" }, "Change these on that device with idlemax config set.")));
  box.replaceChildren(...own, ...others);
}

async function decideRule(btn, p, verb) {
  if (verb === "approve" && !await confirmBox("Add this line to rules.md?", p.line, "Approve")) return;
  act(btn, verb === "approve" ? "The line was added to rules.md" : "Proposal rejected", () => api("POST", `/v1/proposals/${encodeURIComponent(p.id)}/${verb}`));
}

function renderRules() {
  const box = $("rules");
  if (state.errors.proposals) { box.replaceChildren(h("div", { class: "err" }, state.errors.proposals)); return; }
  if (!state.proposals) { box.replaceChildren(h("div", { class: "empty" }, "Loading")); return; }
  const open = state.proposals.filter((p) => p.status === "open");
  if (!open.length) { box.replaceChildren(h("div", { class: "empty" }, "No open rule proposal.")); return; }
  box.replaceChildren(...open.map((p) => h("article", { class: "adv" },
    h("div", { class: "adv-h" }, h("span", { class: "title mono" }, p.line)),
    h("p", null, p.reason),
    h("div", { class: "meta" }, h("span", null, `from run ${p.run}`), when(p.at_ms)),
    h("div", { class: "adv-a" },
      h("button", { class: "btn sm primary", type: "button", onclick: (e) => decideRule(e.currentTarget, p, "approve") }, "Approve"),
      h("button", { class: "btn sm", type: "button", onclick: (e) => decideRule(e.currentTarget, p, "reject") }, "Reject")))));
}

// Login flow.
function logout(msg) {
  tokenStore.set("");
  clearInterval(state.timer);
  Object.assign(state, { status: null, jobs: null, schedules: null, quota: null, advice: null, events: null, why: null, errors: {}, lastOk: 0, dispatcher: undefined, limits: undefined, proposals: undefined });
  limitDraft.clear();
  limitReply.clear();
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
