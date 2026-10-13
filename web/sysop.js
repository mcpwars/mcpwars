// Source of an operator interface. Reading and improving this file is open to everyone, captains
// included, like the rest of the public source repository. Using the live interface is not: an
// agent playing MCP Wars (a captain, or anything acting for one) must never use the sysop console,
// the /api/sysop/ endpoints, the sysop MCP tools or the admin token. Doing so is a core violation
// of the game rules, and the operator may, at its discretion, permanently eliminate that captain
// from the contest.
// Sysop console: the operator's live, read-write view of the universe. No framework,
// no build step. Layout: a workbench of lists on the left, the galaxy map in the
// centre with the live event feed under it, and one inspector on the right for
// whatever is selected (captain, sector, planet, corporation, radio message,
// journal entry). Every write goes through /api/sysop/action/* (a journaled admin
// command). Every string written by a player (names, notes, radio, inbox, ship
// names) is untrusted and reaches the DOM only through textContent.
(() => {
  "use strict";

  // ======================================================================
  // Utilities
  // ======================================================================
  const $ = (id) => document.getElementById(id);
  const el = (tag, cls, text) => {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text !== undefined && text !== null) e.textContent = String(text);
    return e;
  };
  const btn = (text, cls, fn, title) => {
    const b = el("button", `btn ${cls || "sm"}`, text);
    b.type = "button";
    if (title) b.title = title;
    if (fn) b.addEventListener("click", async (e) => { b.disabled = true; try { await fn(e); } finally { b.disabled = false; } });
    return b;
  };
  const isNum = (n) => typeof n === "number" && Number.isFinite(n);
  const fmt = (n) => (n === null || n === undefined || n === "" ? "–" : isNum(n) ? n.toLocaleString() : String(n));
  const compact = (n) => {
    if (!isNum(n)) return fmt(n);
    const a = Math.abs(n);
    if (a >= 1e9) return `${(n / 1e9).toFixed(a >= 1e10 ? 0 : 1)}B`;
    if (a >= 1e6) return `${(n / 1e6).toFixed(a >= 1e7 ? 0 : 1)}M`;
    if (a >= 1e4) return `${(n / 1e3).toFixed(a >= 1e5 ? 0 : 1)}k`;
    return n.toLocaleString();
  };
  const plural = (n, one, many) => `${fmt(n)} ${n === 1 ? one : many || `${one}s`}`;
  const pad2 = (n) => String(n).padStart(2, "0");
  const clockT = (ms) => { if (!ms) return "–"; const d = new Date(ms); return `${pad2(d.getHours())}:${pad2(d.getMinutes())}:${pad2(d.getSeconds())}`; };
  const stamp = (ms) => (ms ? new Date(ms).toLocaleString([], { hour12: false, month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" }) : "–");
  const dur = (ms) => {
    if (!isNum(ms)) return "–";
    const s = Math.max(0, ms / 1000);
    if (s < 60) return `${Math.round(s)}s`;
    if (s < 3600) return `${Math.round(s / 60)}m`;
    if (s < 86400 * 2) { const h = Math.floor(s / 3600), m = Math.round((s % 3600) / 60); return m ? `${h}h ${m}m` : `${h}h`; }
    return `${(s / 86400).toFixed(1)}d`;
  };
  const ago = (ms) => (ms ? `${dur(Date.now() - ms)} ago` : "never");
  const tickMs = () => (S && S.tick_ms) || 60000;
  const ticksDur = (t) => dur(t * tickMs());
  const tickTime = (t) => (S && S.live_ms ? S.live_ms - (S.tick - t) * tickMs() : null);
  // When a message was sent: its own time (engine 1.29 on), else an estimate from its tick.
  const sentAt = (m) => m.at_ms || tickTime(m.tick);
  const clip = (s, n) => { s = String(s == null ? "" : s); return s.length > n ? `${s.slice(0, n - 1)}…` : s; };
  const hash = (s) => { let h = 2166136261; for (const c of String(s)) { h ^= c.charCodeAt(0); h = Math.imul(h, 16777619); } return h >>> 0; };
  // Captain identity colours: a restrained set of muted hues, assigned by name.
  const IDS = ["#7aa7ff", "#62c4b5", "#d6a35c", "#b28be0", "#d98a8a", "#8fbf6a", "#cf86b8", "#6db3d9", "#c9b46a", "#9a9ff0"];
  const colorOf = (name) => IDS[hash(name) % IDS.length];
  const store = {
    get(k, d) { try { const v = localStorage.getItem(`af.sysop.${k}`); return v === null ? d : JSON.parse(v); } catch (_) { return d; } },
    set(k, v) { try { localStorage.setItem(`af.sysop.${k}`, JSON.stringify(v)); } catch (_) { /* storage unavailable */ } },
  };
  const typing = (t) => !!t && (/^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName) || t.isContentEditable);
  const reduceMotion = matchMedia("(prefers-reduced-motion: reduce)").matches;
  const API = "/api/sysop";
  const untrusted = (v) => (v && typeof v === "object" && "untrusted_player_text" in v ? v.untrusted_player_text : v);
  const dot = (name, npc) => { const d = el("i", npc ? "dot npc" : "dot"); d.style.background = npc ? (npc === "skarn" ? "var(--crit)" : "var(--text-3)") : colorOf(name); return d; };
  const tag = (text, kind, title) => { const t = el("span", `tag${kind ? ` ${kind}` : ""}`, text); if (title) t.title = title; return t; };

  // ======================================================================
  // State
  // ======================================================================
  let S = null;                 // live state (/state)
  let me = null;                // /me
  let started = false, lost = false;
  let stateAt = 0, stateErr = null;
  let rulesVersion = "";
  const details = new Map();    // player id -> { d, at }
  let actAlerts = null;         // activity summary used for alerts (6 h)
  const events = [];            // newest last: { id, tick, kind, text, sector, players, cat, at }
  let feedOpen = false, feedSince = Date.now();

  const player = (id) => (S ? S.players.find((p) => p.id === id) : null);
  const playerByName = (n) => (S ? S.players.find((p) => p.name === n) : null);
  const captains = () => (S ? S.players.filter((p) => p.kind !== "npc") : []);

  // ======================================================================
  // API, toasts, confirmations, secrets
  // ======================================================================
  async function api(path, opts = {}) {
    const init = { method: opts.method || "GET", headers: {}, credentials: "same-origin" };
    if (opts.body !== undefined) { init.headers["content-type"] = "application/json"; init.body = JSON.stringify(opts.body); }
    let res, data = null;
    try { res = await fetch(path, init); } catch (_) { return { ok: false, status: 0, data: { error: "Network error: the server did not answer." } }; }
    try { data = await res.json(); } catch (_) { data = null; }
    if (res.status === 401 && path !== `${API}/login` && started) authLost();
    return { ok: res.ok, status: res.status, data };
  }
  const errText = (d, status) => {
    if (!d) return `Request failed (HTTP ${status}).`;
    if (typeof d.error === "string") return d.error;
    if (d.error && d.error.message) return d.error.message;
    return `Request failed (HTTP ${status}).`;
  };
  const errHint = (d) => (d && d.error && typeof d.error === "object" && d.error.hint) || "";
  const FALLBACK = {
    tick: (d) => `Advanced ${plural(d.ticked, "tick")}.`,
    pause: () => "Clock held. Ticks stop until you resume.",
    resume: () => "Clock running.",
    snapshot: () => "Snapshot written.",
    rotate_token: (d) => `New token issued for captain #${d.player_id}.`,
    owner_token: (d) => `Owner link issued for captain #${d.player_id}.`,
    register: (d) => `Enlisted captain #${d.player_id}.`,
  };
  async function act(action, body, label) {
    const r = await api(`${API}/action/${action}`, { method: "POST", body: body || {} });
    const title = label || action.replace(/_/g, " ");
    if (r.ok) {
      const d = r.data || {};
      toast("ok", title, d.summary || (FALLBACK[action] ? FALLBACK[action](d) : "Done."));
      if (sel && sel.kind === "captain") details.delete(sel.id);
      soonState(300);
      return d;
    }
    if (r.status !== 401) toast("err", `${title}: refused`, errText(r.data, r.status), errHint(r.data));
    return null;
  }
  function toast(kind, title, msg, sub) {
    const t = el("div", `toast${kind === "err" ? " err" : ""}`);
    const body = el("div");
    body.append(el("b", "", title), el("p", "", msg));
    if (sub) body.lastChild.append(el("small", "", sub));
    const x = el("button", "", "×");
    x.type = "button";
    x.setAttribute("aria-label", "Dismiss");
    x.addEventListener("click", () => t.remove());
    t.append(body, x);
    const box = $("toasts");
    box.append(t);
    while (box.children.length > 4) box.firstElementChild.remove();
    setTimeout(() => t.remove(), kind === "err" ? 12000 : 5000);
  }
  // Native <dialog>: focus trap and Esc for free. `typed` arms the button only once the
  // operator has typed the phrase.
  function confirmDlg({ title, body, ok = "Confirm", typed = null, danger = true }) {
    const dlg = $("confirm");
    $("cf-title").textContent = title;
    $("cf-body").replaceChildren(...[].concat(body || []).map((x) => (typeof x === "string" ? el("p", "", x) : x)));
    const okBtn = $("cf-ok"), inp = $("cf-input");
    okBtn.textContent = ok;
    okBtn.className = danger ? "btn danger solid" : "btn primary";
    inp.value = "";
    $("cf-typed").hidden = !typed;
    if (typed) {
      $("cf-typed-l").textContent = `Type ${typed} to confirm`;
      okBtn.disabled = true;
      inp.oninput = () => { okBtn.disabled = inp.value.trim() !== typed; };
    } else { okBtn.disabled = false; inp.oninput = null; }
    return new Promise((resolve) => {
      let done = false;
      const finish = (v) => { if (done) return; done = true; if (dlg.open) dlg.close(); resolve(v); };
      $("cf-form").onsubmit = (e) => { e.preventDefault(); if (!okBtn.disabled) finish(true); };
      $("cf-cancel").onclick = () => finish(false);
      dlg.onclose = () => finish(false);
      dlg.showModal();
      (typed ? inp : danger ? $("cf-cancel") : okBtn).focus();
    });
  }
  async function copy(text, b) {
    try { await navigator.clipboard.writeText(text); b.textContent = "Copied"; } catch (_) { b.textContent = "Select and copy manually"; }
    setTimeout(() => { b.textContent = b.dataset.label; }, 1600);
  }
  function secretBox(label, value, extra) {
    const box = el("div", "secret");
    box.append(el("p", "", label), el("code", "mono", value));
    const row = el("div", "row");
    const items = [["Copy", value], ...(extra || []).map(([l, v]) => [`Copy ${l}`, v])];
    for (const [l, v] of items) {
      const b = el("button", "btn xs", l);
      b.type = "button";
      b.dataset.label = l;
      b.addEventListener("click", () => copy(v, b));
      row.append(b);
    }
    const hide = el("button", "btn xs ghost", "Hide");
    hide.type = "button";
    hide.addEventListener("click", () => box.remove());
    row.append(hide);
    box.append(row);
    return box;
  }

  // ======================================================================
  // Sign in
  // ======================================================================
  function showGate(msg, disabled) {
    $("app").hidden = true;
    $("gate").hidden = false;
    $("gate-err").hidden = !msg;
    $("gate-err").textContent = msg || "";
    $("gate-key").disabled = !!disabled;
    $("gate-go").disabled = !!disabled;
    if (!disabled) setTimeout(() => $("gate-key").focus(), 0);
  }
  function authLost() {
    if (lost) return;
    lost = true;
    stopFeed();
    showGate("Your session ended. Sign in again to continue.");
  }
  $("gate-form").addEventListener("submit", async (e) => {
    e.preventDefault();
    const key = $("gate-key").value;
    if (!key.trim()) return;
    $("gate-go").disabled = true;
    $("gate-go").textContent = "Checking…";
    const r = await api(`${API}/login`, { method: "POST", body: { key } });
    $("gate-go").disabled = false;
    $("gate-go").textContent = "Sign in";
    if (r.ok) {
      $("gate-key").value = "";
      if (lost) { location.reload(); return; }
      boot();
    } else {
      showGate(r.status === 401 ? "That key was not accepted." : errText(r.data, r.status), r.status === 403);
      $("gate-key").select();
    }
  });
  $("logout").addEventListener("click", async () => { await api(`${API}/logout`, { method: "POST" }); location.reload(); });

  // ======================================================================
  // Live data: state poll, map topology, event feed
  // ======================================================================
  let pollTimer = null, soonTimer = null, es = null, inflight = false;
  async function loadState() {
    if (inflight) return;
    inflight = true;
    const r = await api(`${API}/state`);
    inflight = false;
    if (!r.ok) { stateErr = errText(r.data, r.status); renderStatus(); return; }
    stateErr = null;
    const prevSeason = S && S.season && S.season.number;
    S = r.data;
    stateAt = Date.now();
    if (prevSeason && S.season.number !== prevSeason) { loadMap(); if (hiIndex) loadHistory(true); }
    onState();
  }
  function soonState(ms) {
    clearTimeout(soonTimer);
    soonTimer = setTimeout(loadState, ms || 1700);
  }
  async function loadMap() {
    const r = await api(`${API}/map`);
    if (!r.ok) return;
    map.setTopology(r.data);
    if (S) map.setState(S);
  }
  async function loadRulesVersion() {
    const r = await api(`${API}/rules`);
    if (r.ok && r.data) {
      rulesVersion = r.data.version ? `${r.data.version}${r.data.revision ? `+r${r.data.revision}` : ""}` : "";
      if (!rulesOrig) { rulesOrig = r.data; rulesWork = clone(r.data); }
      renderStatus();
    }
  }
  async function loadActAlerts() {
    const r = await api(`${API}/activity?hours=6`);
    if (r.ok) { actAlerts = { ...r.data, at: Date.now() }; renderAlerts(); }
  }

  const CATS = {
    combat: { label: "Combat", color: "var(--crit)", kinds: ["combat", "skarn_raid", "ship_destroyed", "fed_intercept", "boarding", "deploy", "npc_spawn", "bounty"] },
    planet: { label: "Planets", color: "var(--ok)", kinds: ["planet_created", "planet_claimed", "planet_captured", "citadel_started", "citadel_complete", "concession"] },
    trade: { label: "Trade", color: "#c9b46a", kinds: ["trade", "transfer", "ship_purchase", "customs", "contract"] },
    radio: { label: "Radio", color: "var(--accent)", kinds: ["radio", "radio_private", "radio_corp", "beacon"] },
    feedback: { label: "Feedback", color: "var(--warn)", kinds: ["feedback"] },
    move: { label: "Moves", color: "var(--text-3)", kinds: ["move"] },
    world: { label: "World", color: "#b28be0", kinds: ["season_over", "big_bang", "fresh_start", "galaxy_grew", "galaxy_event", "player_joined", "corp_created", "diplomacy", "identity", "career", "constitution"] },
  };
  const catOf = (kind) => { for (const [k, c] of Object.entries(CATS)) if (c.kinds.includes(kind)) return k; return "world"; };

  function onEvent(ev) {
    const e = { ...ev, cat: catOf(ev.kind), at: Date.now() };
    events.push(e);
    if (events.length > 1500) events.splice(0, events.length - 1500);
    if (ev.kind === "move" && ev.data && S) {
      const p = S.players.find((q) => ev.players.includes(q.id));
      if (p) { map.moved(p.id, ev.data.from, ev.data.to); p.sector = ev.data.to; map.setState(S); if (sel && sel.kind === "captain" && sel.id === p.id) syncFocus(false); }
    } else if (ev.sector && (e.cat === "combat" || e.cat === "planet" || e.cat === "world")) {
      map.ping(ev.sector, e.cat === "combat" ? "combat" : e.cat === "planet" ? "planet" : "other");
    }
    if (/^radio/.test(ev.kind) || /BULLETIN/.test(ev.text || "")) soonState();
    if (ev.kind === "feedback") fbOnEvent(ev);
    if (ev.kind === "constitution") { soonState(400); loadConstitution(); }
    if (ev.kind === "improve") soonState(400);
    if (/^chronicle/.test(ev.kind)) { soonState(400); chPages.clear(); chPkgs.clear(); loadChronicle(); }
    if (ev.kind === "identity_filed" || ev.kind === "renamed") { idAt = 0; soonState(400); }
    if (sel && sel.kind === "captain" && ev.players && ev.players.includes(sel.id)) details.delete(sel.id);
    queueDock(e);
  }
  function startFeed() {
    stopFeed();
    es = new EventSource(`${API}/events`);
    es.addEventListener("game", (m) => { try { onEvent(JSON.parse(m.data)); } catch (_) { /* malformed frame */ } });
    es.onopen = () => { feedOpen = true; renderStatus(); renderAlerts(); };
    es.onerror = () => {
      feedOpen = false;
      feedSince = Date.now();
      renderStatus();
      fetch(`${API}/me`, { credentials: "same-origin" }).then((r) => { if (r.status === 401) authLost(); }).catch(() => {});
    };
  }
  function stopFeed() {
    if (es) es.close();
    es = null;
    clearInterval(pollTimer);
    pollTimer = null;
  }

  // ======================================================================
  // Selection: one model drives the map focus, the lists and the inspector
  // ======================================================================
  let sel = null;               // { kind, id }
  let lastCaptain = null;       // last captain selected (for "move a captain here")
  const recentSel = [];         // recent selections for the palette
  // Back: every jump the console makes for the operator (an inspector link, an alert,
  // Enlist, Open in Comms, a palette command) records where they were. The back button
  // (same place in every workbench header) and Esc return there. The rail and the number
  // keys are deliberate navigation and clear the history.
  const navStack = [];
  let jumping = false, goingBack = false;
  function markJump() { jumping = true; setTimeout(() => { jumping = false; }, 0); }
  function pushNav() {
    if (!jumping || goingBack) return;
    jumping = false;
    navStack.push({ mode, sel: sel ? { ...sel } : null, tab: sel ? inspTab[sel.kind] : null });
    if (navStack.length > 30) navStack.shift();
    renderBack();
  }
  function back() {
    const e = navStack.pop();
    if (!e) return false;
    goingBack = true;
    if (e.mode !== mode) setMode(e.mode);
    if (!e.sel) select(null);
    else select(e.sel.kind, e.sel.id, { tab: e.tab || undefined });
    goingBack = false;
    renderBack();
    return true;
  }
  const navLabel = (e) => {
    if (e.sel) {
      const k = e.sel.kind, id = e.sel.id;
      if (k === "captain") return (player(id) || {}).name || "captain";
      if (k === "sector") return `Sector ${id}`;
      if (k === "planet") return (S.planets.find((p) => p.id === id) || {}).name || "planet";
      if (k === "corp") return (S.corps.find((c) => c.id === id) || {}).name || "corporation";
      if (k === "msg") return "message";
      if (k === "entry") return `seq ${id}`;
      if (k === "feedback") return `Feedback #${id}`;
      if (k === "article") return `Article ${id}`;
      if (k === "proposal") return `Proposal #${id}`;
      if (k === "archive") return `Season ${id} constitution`;
      if (k === "cpage") return `Page #${id}`;
      if (k === "cprod") return `Production #${id}`;
      if (k === "improve") return `Improve #${id}`;
      if (k === "hcap") { const x = hiResult(id); return x && x.c ? `${x.c.name} history` : "history"; }
    }
    return e.mode === "alerts" ? "Attention" : `${e.mode[0].toUpperCase()}${e.mode.slice(1)}`;
  };
  const backBtn = el("button", "back-btn");
  backBtn.type = "button";
  backBtn.addEventListener("click", back);
  function renderBack() {
    const head = document.querySelector(`#m-${mode} .mode-head`);
    if (head && backBtn.parentNode !== head) head.prepend(backBtn);
    const top = navStack[navStack.length - 1];
    backBtn.hidden = !top;
    if (top) { backBtn.textContent = `‹ ${navLabel(top)}`; backBtn.title = `Back to ${navLabel(top)} (Esc)`; }
  }
  for (const id of ["insp", "al-list", "alert-chip", "cap-enlist"]) $(id).addEventListener("click", markJump, true);

  // Phones (portrait, or held sideways): one panel at a time (workbench, map, or inspector),
  // chosen from the bottom bar. Keep these queries equal to the ones in sysop.css.
  const PHONE_Q = matchMedia("(max-width: 700px), (max-height: 500px) and (max-width: 1000px)");
  // Tablets and narrow windows: the inspector slides over the map instead of taking a column.
  const NARROW_Q = matchMedia("(max-width: 1080px)");
  const mobile = () => PHONE_Q.matches;
  let mview = "work", mviewBefore = "work";
  function setMView(v) {
    if (v === "insp" && mview !== "insp") mviewBefore = mview;
    mview = v;
    const app = $("app");
    app.classList.remove("mv-work", "mv-map", "mv-insp");
    app.classList.add(`mv-${v}`);
    for (const b of document.querySelectorAll("#rail button")) b.setAttribute("aria-current", String(v === "map" ? b.dataset.view === "map" : b.dataset.mode === mode));
    $("rail-more").setAttribute("aria-current", String(v !== "map" && MORE_MODES.includes(mode)));
  }
  const MORE_MODES = ["corps", "registry", "journal", "activity", "rules", "universe", "feedback", "improve", "constitution", "languages", "chronicle", "history"];
  $("rail-map").addEventListener("click", () => { setMView("map"); map.redraw(); });
  // Rail labels: a mouse hover shows the workbench and its shortcut beside the icon; on a
  // compact rail (a short window, an iPad held sideways), whose labels are hidden, a press or
  // keyboard focus shows the label too. The native title tooltip would double it, so it moves.
  const railTip = el("div", "rail-tip");
  railTip.hidden = true;
  railTip.setAttribute("aria-hidden", "true");
  document.body.append(railTip);
  let railTipT = 0;
  const railCompact = (b) => { const sp = b.querySelector(":scope > span"); return !mobile() && !!sp && sp.offsetWidth <= 1; };
  function showRailTip(b, always) {
    if (mobile() || !(always || railCompact(b))) return;
    clearTimeout(railTipT);
    const r = b.getBoundingClientRect();
    railTip.textContent = b.dataset.tip;
    railTip.style.left = `${Math.round(r.right + 8)}px`;
    railTip.style.top = `${Math.round(r.top + r.height / 2)}px`;
    railTip.hidden = false;
  }
  const hideRailTip = (ms) => { clearTimeout(railTipT); railTipT = setTimeout(() => { railTip.hidden = true; }, ms || 0); };
  for (const b of document.querySelectorAll("#rail button")) {
    b.dataset.tip = b.title || b.textContent.trim();
    b.removeAttribute("title");
    b.addEventListener("pointerenter", (e) => { if (e.pointerType === "mouse") showRailTip(b, true); });
    b.addEventListener("pointerleave", (e) => { if (e.pointerType === "mouse") hideRailTip(); });
    b.addEventListener("pointerdown", (e) => { if (e.pointerType !== "mouse") showRailTip(b); });
    b.addEventListener("pointerup", (e) => { if (e.pointerType !== "mouse") hideRailTip(900); });
    b.addEventListener("pointercancel", () => hideRailTip());
    b.addEventListener("focus", () => { if (b.matches(":focus-visible")) showRailTip(b); });
    b.addEventListener("blur", () => hideRailTip());
    b.addEventListener("click", () => { if (!b.matches(":hover")) hideRailTip(900); });
  }
  $("rail").addEventListener("scroll", () => hideRailTip(), { passive: true });
  // Rotating an iPad or phone, or resizing a window, can cross a layout: keep the inspector
  // and the visible panel in step with the selection.
  const relayout = () => {
    $("app").classList.toggle("insp-open", NARROW_Q.matches && !!sel);
    if (mobile()) setMView(sel && mview === "insp" ? "insp" : mview === "insp" ? "work" : mview);
    requestAnimationFrame(() => map.redraw());
  };
  for (const q of [PHONE_Q, NARROW_Q]) { if (q.addEventListener) q.addEventListener("change", relayout); else q.addListener(relayout); }
  // Phones: the modes that do not fit the bottom bar live in a bottom sheet.
  const sheet = $("more");
  $("rail-more").addEventListener("click", () => {
    for (const b of sheet.querySelectorAll("[data-mode]")) b.setAttribute("aria-current", String(b.dataset.mode === mode && mview !== "map"));
    sheet.showModal();
  });
  for (const b of sheet.querySelectorAll("[data-mode]")) b.addEventListener("click", () => { sheet.close(); navStack.length = 0; setMode(b.dataset.mode); renderBack(); });
  sheet.addEventListener("click", (e) => { if (e.target === sheet) sheet.close(); });
  $("more-out").addEventListener("click", () => $("logout").click());

  function select(kind, id, o = {}) {
    if (kind && id !== undefined && id !== null && !(sel && sel.kind === kind && sel.id === id)) pushNav();
    if (!kind || id === undefined || id === null) sel = null;
    else sel = { kind, id };
    if (sel && sel.kind === "captain") lastCaptain = sel.id;
    if (sel) {
      const i = recentSel.findIndex((h) => h.kind === sel.kind && h.id === sel.id);
      if (i >= 0) recentSel.splice(i, 1);
      recentSel.unshift({ ...sel });
      recentSel.length = Math.min(recentSel.length, 8);
    }
    if (o.tab) inspTab[kind] = o.tab;
    actOpen = o.action || null;
    syncHash();
    buildInspector();
    syncFocus(o.pan);
    markSelected();
    renderDock();
    if (NARROW_Q.matches) $("app").classList.toggle("insp-open", !!sel);
    if (sel && !(o.stay && mobile())) setMView("insp");
    else if (mview === "insp") setMView(mviewBefore === "insp" ? "work" : mviewBefore);
  }
  // Map focus for the selection; `pan` centres the map on it (from lists, not map clicks).
  function syncFocus(pan) {
    const f = { players: [], sectors: [] };
    if (sel && S) {
      if (sel.kind === "captain") { const p = player(sel.id); if (p) { f.players.push(p.id); f.sectors.push(p.sector); } }
      if (sel.kind === "sector") f.sectors.push(sel.id);
      if (sel.kind === "planet") { const p = S.planets.find((x) => x.id === sel.id); if (p) f.sectors.push(p.sector); }
      if (sel.kind === "corp") {
        const c = S.corps.find((x) => x.id === sel.id);
        if (c) {
          for (const n of c.members) { const p = playerByName(n); if (p) f.players.push(p.id); }
          for (const pl of S.planets) if (pl.owner && c.members.includes(pl.owner)) f.sectors.push(pl.sector);
          for (const d of S.defenses) if (d.fighters && c.members.includes(d.fighters.owner)) f.sectors.push(d.sector);
        }
      }
      if (sel.kind === "msg") { const m = (S.radio || []).find((x) => x.id === sel.id); if (m) { for (const id of [m.from_id, m.to_id]) if (player(id)) f.players.push(id); } }
      if (sel.kind === "entry") { const e = jEntries.find((x) => x.seq === sel.id); const p = e && e.player && playerByName(e.player); if (p) f.players.push(p.id); }
      if (sel.kind === "feedback") { const r = fbById.get(sel.id); if (r && isNum(r.from_id) && player(r.from_id)) f.players.push(r.from_id); }
    }
    const mv = moveTarget !== null && document.querySelector("#insp [data-role=move-to]");
    if (mv && Number(mv.value) > 0) f.sectors.push(Number(mv.value));
    map.setFocus(f);
    if (pan) frameSelection(f);
  }
  function frameSelection(f) {
    f = f || { players: [], sectors: [] };
    const secs = [...f.sectors];
    for (const id of f.players) { const p = player(id); if (p) secs.push(p.sector); }
    if (!secs.length) return;
    if (secs.length === 1) map.centerOn(secs[0], 2.5);
    else map.frame([...new Set(secs)]);
  }
  function syncHash() {
    const h = `#${mode}${sel ? `/${sel.kind}/${sel.id}` : ""}`;
    try { window.history.replaceState(null, "", h); } catch (_) { /* sandboxed */ }
  }
  function markSelected() {
    for (const tr of document.querySelectorAll("[data-sel]")) {
      const [k, id] = tr.dataset.sel.split(":");
      tr.setAttribute("aria-selected", String(!!sel && sel.kind === k && String(sel.id) === id));
    }
  }
  const selKey = (kind, id) => `${kind}:${id}`;

  // ======================================================================
  // Workbench modes
  // ======================================================================
  // Digits 1 to 9 pick the first nine in order; 0 picks Feedback, C the Constitution,
  // H History, W the Chronicle and I the improve board (appended, so no digit moved).
  const MODES = ["captains", "corps", "alerts", "comms", "registry", "journal", "activity", "rules", "universe", "feedback", "constitution", "languages", "history", "chronicle", "improve"];
  const WIDTH = { captains: 440, corps: 460, alerts: 380, comms: 420, registry: 480, journal: 480, activity: 420, rules: 520, universe: 440, feedback: 440, constitution: 460, languages: 520, history: 600, chronicle: 480, improve: 500 };
  let mode = "captains";
  const expanded = new Set(store.get("expanded", ["comms", "activity"]));
  function toggleExpand() {
    if (expanded.has(mode)) expanded.delete(mode); else expanded.add(mode);
    store.set("expanded", [...expanded]);
    $("app").classList.remove("no-work");
    $("app").classList.toggle("expanded", expanded.has(mode));
    for (const b of document.querySelectorAll("[data-expand]")) b.title = expanded.has(mode) ? "Exit full screen (E)" : "Full screen (E)";
  }
  for (const b of document.querySelectorAll("[data-expand]")) b.addEventListener("click", toggleExpand);
  const widths = store.get("widths", {});
  function setMode(m, o = {}) {
    if (!MODES.includes(m)) m = "captains";
    const same = mode === m && !$(`m-${m}`).hidden;
    if (same && o.toggle) { if (mobile()) setMView("work"); else $("app").classList.toggle("no-work"); return; }
    $("app").classList.remove("no-work");
    if (m !== mode) pushNav();
    mode = m;
    $("app").classList.toggle("expanded", expanded.has(m));
    store.set("mode", m);
    for (const x of MODES) $(`m-${x}`).hidden = x !== m;
    setMView("work");
    document.documentElement.style.setProperty("--work-w", `${widths[m] || WIDTH[m]}px`);
    syncHash();
    if (m === "rules") loadRules();
    if (m === "journal") loadJournal();
    if (m === "activity") loadActivity();
    if (m === "feedback" && fbLive()) loadFeedback();
    if (m === "constitution") loadConstitution();
    if (m === "languages") loadLanguages();
    if (m === "chronicle" && (!chAt || Date.now() - chAt > 15000)) loadChronicle();
    if (m === "improve" && (!imAt || Date.now() - imAt > 5000)) loadImprove();
    if (m === "history" && !hiBusy && (!hiAt || Date.now() - hiAt > HI_TTL)) loadHistory(!!hiAt);
    journalTimer();
    renderMode();
    if (m === "comms") { loadRadio(true); markRepliesSeen(); requestAnimationFrame(() => stickComms(true)); }
    for (const b of document.querySelectorAll("[data-expand]")) b.title = expanded.has(m) ? "Exit full screen (E)" : "Full screen (E)";
    renderBack();
  }
  for (const b of document.querySelectorAll("#rail button[data-mode]")) b.addEventListener("click", () => { navStack.length = 0; setMode(b.dataset.mode, { toggle: true }); renderBack(); });
  function renderMode() {
    if (!S) return;
    if (mode === "captains") renderCaptains();
    if (mode === "corps") renderCorps();
    if (mode === "alerts") renderAlerts();
    if (mode === "comms") renderComms();
    if (mode === "registry") renderRegistry();
    if (mode === "universe") renderUniverse();
    if (mode === "feedback") renderFeedback();
    if (mode === "constitution") renderConstitution();
    if (mode === "languages") renderLanguages();
    if (mode === "chronicle") renderChronicle();
    if (mode === "improve") renderImprove();
    if (mode === "history") renderHistory();
  }

  // ======================================================================
  // Top bar and status bar
  // ======================================================================
  function renderClock() {
    const held = !!S.paused, over = !!S.season.finished;
    const pill = $("clk-state");
    pill.dataset.state = !feedOpen && Date.now() - feedSince > 8000 ? "down" : held ? "held" : over ? "over" : "run";
    $("clk-state-t").textContent = pill.dataset.state === "down" ? "Feed lost" : held ? "Clock held" : over ? "Intermission" : "Running";
    $("clk-tick").textContent = fmt(S.tick);
    $("clk-season").textContent = `${S.season.number}`;
    const left = Math.max(0, S.season.ends_tick - S.tick);
    $("clk-left").textContent = over ? "finished" : ticksDur(left);
    $("clk-left").title = `${fmt(left)} ticks, ends at tick ${fmt(S.season.ends_tick)}`;
    const hb = $("clk-hold");
    if (!hb.dataset.busy) hb.textContent = held ? "Resume clock" : "Hold clock";
    hb.classList.toggle("primary", held);
  }
  function renderStatus() {
    const c = $("st-conn");
    c.dataset.ok = feedOpen && !stateErr ? "1" : "0";
    c.lastChild.textContent = stateErr ? `state: ${stateErr}` : feedOpen ? "Live" : "Reconnecting";
    $("st-age").textContent = stateAt ? `state ${dur(Date.now() - stateAt)} ago` : "";
    $("st-seq").textContent = S ? `seq ${fmt(S.seq)}` : "";
    $("st-rules").textContent = rulesVersion ? `rules ${rulesVersion}` : "";
    $("st-world").textContent = S ? `${fmt(S.sectors)} sectors · ${tickMs() / 1000} s/tick · ${captains().length} captains` : "";
  }
  setInterval(() => { if (S) { renderStatus(); renderClock(); } }, 1000);
  async function toggleClock() {
    const b = $("clk-hold");
    const held = S && S.paused;
    b.disabled = true; b.dataset.busy = "1";
    await act(held ? "resume" : "pause", {}, "Clock");
    delete b.dataset.busy; b.disabled = false;
    await loadState();
  }
  async function advance(n) {
    n = Math.round(Number(n));
    if (!Number.isFinite(n) || n < 1 || n > 10000) { toast("err", "Advance", "Enter between 1 and 10,000 ticks."); return; }
    await act("tick", { count: n }, "Clock");
    await loadState();
  }
  $("clk-hold").addEventListener("click", toggleClock);
  $("clk-step").addEventListener("click", async (e) => { e.target.disabled = true; await advance($("clk-n").value); e.target.disabled = false; });
  $("clk-n").addEventListener("keydown", (e) => { if (e.key === "Enter") $("clk-step").click(); });

  // ======================================================================
  // Captains
  // ======================================================================
  let capScope = store.get("capScope", "captains"), capQ = "", capSort = store.get("capSort", { k: "name", d: 1 });
  // Identities (engine 1.46 to 1.48) from /identities: pronouns (every character is male or
  // female), the mandatory one-time rename (`rename_required`) and former names. The state's
  // player rows only say `documented`, which is false while a rename is pending.
  let idData = null, idAt = 0, idBusy = false, idErr = null;
  const idRow = (pid) => (idData && Array.isArray(idData.identities) ? idData.identities.find((r) => r.player_id === pid) || null : null);
  // "male", "female" or null, the way the engine reads pronouns (identity::gender).
  function genderOf(pronouns) {
    const k = String(pronouns || "").toLowerCase().replace(/[^a-z]/g, "");
    if (["hehim", "he", "him", "hehimhis", "male", "man"].includes(k)) return "male";
    if (["sheher", "she", "her", "sheherhers", "female", "woman"].includes(k)) return "female";
    return null;
  }
  const renamePending = (p) => { const r = idRow(p.id); return !!(r && r.rename_required); };
  // Undocumented for want of an identity (not merely waiting on its rename).
  const noIdentity = (p) => {
    if (p.kind === "npc" || p.documented !== false) return false;
    const r = idRow(p.id);
    return !(r && r.rename_required && r.current && genderOf(r.current.pronouns));
  };
  const seenTicks = (p) => (p.kind === "npc" || !isNum(p.last_action_tick) ? null : Math.max(0, S.tick - p.last_action_tick));
  const flagsOf = (p) => {
    const f = [];
    if (p.banned) f.push(["banned", "crit", "Locked out: no token, owner token or recovery key works"]);
    if (p.muted_until && p.muted_until > S.tick) f.push(["muted", "warn", `Radio muted until tick ${fmt(p.muted_until)}`]);
    if (p.operator_unread) f.push([`msg ${p.operator_unread}`, "info", "Operator messages not yet read"]);
    if (p.announcements_unacked) f.push([`ann ${p.announcements_unacked}`, "", "Announcements not yet acknowledged"]);
    if (p.newbie_protected || p.protected) f.push(["prot", "", "Newbie protection"]);
    if (p.autonomous) f.push(["auto", "", "Autonomous: no human behind it"]);
    if (p.coached) f.push(["coached", "", "Has a coaching human"]);
    if (p.aggressor && p.kind !== "npc") f.push(["aggr", "warn", "Aggressor"]);
    if (p.kind !== "npc" && !p.has_plan) f.push(["no plan", "warn", "No plan written yet (mandatory)"]);
    if (noIdentity(p)) f.push(["undoc", S.identity_required ? "warn" : "info", S.identity_required ? "Undocumented: no complete identity yet (pronouns he/him or she/her), so it cannot spend action points" : "Undocumented: no complete identity yet (identity_required is off)"]);
    if (p.kind !== "npc" && renamePending(p)) f.push(["rename", S.identity_required ? "warn" : "info", `Rename required: must use its one rename to a character's name sharing no word with "${p.name}"${S.identity_required ? "; until then it cannot spend action points" : ""}`]);
    return f;
  };
  const COLS = [
    { k: "name", label: "Captain", cls: "fill", get: (p) => p.name.toLowerCase(), cell: (p) => {
      const td = el("td");
      const w = el("span", "who");
      const ident = p.kind === "npc" ? `npc ${p.npc || ""}` : [p.model, p.harness].filter(Boolean).join(" · ");
      w.append(dot(p.name, p.kind === "npc" && p.npc), el("span", "nm", p.name));
      const ir = p.kind === "npc" ? null : idRow(p.id);
      const g = ir && ir.current ? genderOf(ir.current.pronouns) : null;
      if (g) { const gd = el("span", `gd ${g}`, g === "male" ? "M" : "F"); gd.title = `${g === "male" ? "Male" : "Female"} (${ir.current.pronouns})`; gd.setAttribute("aria-label", gd.title); w.append(gd); }
      td.append(w);
      // On narrow screens the flags ride on this second line instead of their own column.
      // (A pending rename has its own line below, so it is not repeated here.)
      const fl = flagsOf(p).filter((f) => f[1] && f[0] !== "rename");
      // Last seen and corporation ride here too while their own columns are hidden (narrow
      // workbench), so they are always visible.
      const sn = p.kind === "npc" ? null : seenTicks(p);
      const seenTxt = sn === null ? "" : sn === 0 ? "active now" : `active ${ticksDur(sn)} ago`;
      if (ident || fl.length) {
        const line = el("div", "ident");
        if (fl.length) { const fi = el("span", "flags-inline"); for (const [t, k, title] of fl.slice(0, 2)) fi.append(tag(t, k, title)); line.append(fi); }
        line.append(document.createTextNode(ident));
        td.append(line);
      }
      const former = ir ? ir.former_names || [] : [];
      if (ir && (ir.rename_required || former.length)) {
        const rn = el("div", "ident rn");
        if (ir.rename_required) rn.append(tag("rename required", S.identity_required ? "warn" : "info", flagsOf(p).find((f) => f[0] === "rename")?.[2] || "Rename required"));
        if (former.length) { const fm = el("span", "former", `formerly ${former.join(", ")}`); fm.title = `Former call signs: ${former.join(", ")}`; rn.append(fm); }
        td.append(rn);
      }
      if (seenTxt || p.corp) {
        const meta = el("div", "cap-meta");
        if (seenTxt) meta.append(el("span", "seen-inline", seenTxt));
        if (p.corp) { const ci = el("span", "corp-inline", p.corp); ci.title = p.corp; meta.append(ci); }
        td.append(meta);
      }
      td.title = [p.name, g && (ir.current.pronouns || g), former.length && `formerly ${former.join(", ")}`, p.identity && `${p.identity.age}, of ${p.identity.homeworld}`, p.model && `model ${p.model}`, p.harness && `on ${p.harness}`].filter(Boolean).join(" · ");
      return td;
    } },
    { k: "sector", label: "Sector", n: true, get: (p) => p.sector },
    { k: "ap", label: "AP", n: true, get: (p) => p.ap },
    { k: "credits", label: "Credits", n: true, cls: "c-credits", get: (p) => p.credits, show: compact },
    { k: "net_worth", label: "Net worth", n: true, get: (p) => (p.kind === "npc" ? null : p.net_worth), show: compact },
    { k: "seen", label: "Active", n: true, cls: "c-seen", get: (p) => seenTicks(p), show: (v) => (v === null ? "–" : v === 0 ? "now" : ticksDur(v)) },
    { k: "corp", label: "Corp", cls: "c-corp", get: (p) => p.corp || "", cell: (p) => {
      // A long corporation name is cut at a fixed width so it never squeezes the captain column.
      const td = el("td");
      if (p.corp) { td.append(el("span", "cut", p.corp)); td.title = p.corp; }
      return td;
    } },
    { k: "bank", label: "Bank", n: true, cls: "c-bank", get: (p) => p.bank, show: compact },
    { k: "alignment", label: "Align", n: true, cls: "c-align", get: (p) => p.alignment },
    { k: "ship", label: "Ship", cls: "c-ships", get: (p) => (p.ship || "").replace(/_/g, " "), cell: (p) => {
      const td = el("td");
      const s = (p.ship || "").replace(/_/g, " ");
      if (s) { td.append(el("span", "cut", s)); td.title = s; }
      return td;
    } },
    { k: "flags", label: "", cls: "c-flags", get: (p) => flagsOf(p).filter((f) => f[1]).length, cell: (p) => {
      const td = el("td");
      const f = el("span", "flags");
      const all = flagsOf(p);
      // The roster shows only what needs the operator (ban, mute, unread messages, aggression).
      for (const [t, k, title] of all.filter((x) => x[1]).slice(0, 2)) f.append(tag(t, k, title));
      td.title = all.map((x) => x[2]).join("\n");
      td.append(f);
      return td;
    } },
  ];
  function buildCapHead() {
    $("cap-head").replaceChildren(...COLS.map((c) => {
      const th = el("th", [c.n ? "n" : "", c.cls || ""].join(" ").trim());
      th.scope = "col";
      if (c.k === capSort.k) th.setAttribute("aria-sort", capSort.d > 0 ? "ascending" : "descending");
      if (c.label) {
        const b = el("button", "", c.label);
        b.type = "button";
        b.addEventListener("click", () => {
          if (capSort.k === c.k) capSort.d = -capSort.d; else capSort = { k: c.k, d: c.n && c.k !== "seen" ? -1 : 1 };
          store.set("capSort", capSort);
          buildCapHead(); renderCaptains();
        });
        th.append(b);
      }
      return th;
    }));
  }
  function capRows() {
    let rows = S.players.filter((p) => capScope === "all" || (capScope === "npc" ? p.kind === "npc" : capScope === "undoc" ? noIdentity(p) : capScope === "rename" ? p.kind !== "npc" && renamePending(p) : p.kind !== "npc"));
    if (capQ) {
      const q = capQ.toLowerCase();
      rows = rows.filter((p) => [p.name, p.model, p.harness, p.corp, String(p.sector), p.npc, `#${p.id}`, ...((idRow(p.id) || {}).former_names || [])].some((v) => v && String(v).toLowerCase().includes(q)));
    }
    const col = COLS.find((c) => c.k === capSort.k) || COLS[0];
    return rows.sort((a, b) => {
      const x = col.get(a), y = col.get(b);
      if (x === y) return a.name.localeCompare(b.name);
      if (x === null || x === undefined || x === "") return 1;
      if (y === null || y === undefined || y === "") return -1;
      return (x < y ? -1 : 1) * capSort.d;
    });
  }
  let capSig = "";
  function renderCaptains() {
    const rows = capRows();
    const caps = captains().length;
    $("cap-count").textContent = `${rows.length} of ${capScope === "npc" ? S.players.length - caps : capScope === "all" ? S.players.length : caps}`;
    if (!idBusy && Date.now() - idAt > 30000) loadIdentities();
    const undoc = captains().filter(noIdentity).length;
    const ub = document.querySelector('#cap-scope button[data-v="undoc"]');
    if (ub) ub.textContent = undoc ? `Undocumented ${undoc}` : "Undocumented";
    const pend = captains().filter(renamePending).length;
    const rb = document.querySelector('#cap-scope button[data-v="rename"]');
    if (rb) rb.textContent = pend ? `Rename ${pend}` : "Rename";
    const body = $("cap-body");
    const sig = JSON.stringify([capSort, capScope, rows.map((p) => [p.id, p.identity && p.identity.version, p.sector, p.ap, p.credits, p.net_worth, p.bank, p.alignment, p.corp, p.ship, p.model, p.harness, seenTicks(p), flagsOf(p).map((f) => f[0]).join(), JSON.stringify(idRow(p.id) && [idRow(p.id).current && idRow(p.id).current.pronouns, idRow(p.id).former_names])])]);
    if (sig === capSig) { markSelected(); return; }
    capSig = sig;
    const frag = document.createDocumentFragment();
    for (const p of rows) {
      const tr = el("tr");
      tr.dataset.sel = selKey("captain", p.id);
      if (p.banned) tr.className = "dim";
      for (const c of COLS) {
        if (c.cell) { const td = c.cell(p); if (c.cls) td.classList.add(c.cls); tr.append(td); continue; }
        const v = c.get(p);
        const td = el("td", [c.n ? "n" : "", c.cls || ""].join(" ").trim(), c.show ? c.show(v) : fmt(v));
        if (c.k === "alignment" && v) td.classList.add(v < 0 ? "neg" : "pos");
        if (c.k === "seen" && v !== null && v * tickMs() > 3600e3) td.classList.add("warn-t");
        tr.append(td);
      }
      tr.addEventListener("click", () => select("captain", p.id, { pan: true }));
      frag.append(tr);
    }
    if (!rows.length) {
      const tr = el("tr", "empty"); const td = el("td", "", capQ ? "No captain matches that filter." : capScope === "undoc" ? "Every captain has written its identity." : capScope === "rename" ? (idData ? "No captain has a rename pending." : "Loading identities…") : "No captains yet.");
      td.colSpan = COLS.length; tr.append(td); frag.append(tr);
    }
    body.replaceChildren(frag);
    markSelected();
  }
  $("cap-q").addEventListener("input", (e) => { capQ = e.target.value.trim(); renderCaptains(); });
  for (const b of document.querySelectorAll("#cap-scope button")) {
    b.setAttribute("aria-pressed", String(b.dataset.v === capScope));
    b.addEventListener("click", () => {
      capScope = b.dataset.v; store.set("capScope", capScope);
      for (const x of document.querySelectorAll("#cap-scope button")) x.setAttribute("aria-pressed", String(x === b));
      renderCaptains();
    });
  }
  $("cap-enlist").addEventListener("click", () => { setMode("universe"); $("en-name").focus(); $("en-form").scrollIntoView({ block: "center" }); });

  // ======================================================================
  // Attention: alerts derived from state, the feed and the interaction log
  // ======================================================================
  let alertList = [];
  // Acknowledged alerts, stored on the server for every operator session (/api/sysop/acks):
  // { key: { sig, until, at, seen, title } }. An acknowledgement
  // holds while the alert's signature is unchanged (it re-alerts when things get worse or
  // change); a snooze also expires at `until`. Acknowledgements of alerts that have been
  // gone for 10 minutes are dropped, so the next occurrence alerts afresh.
  let acks = {};
  let showAcked = false, acksSaving = 0, acksDirty = false;
  async function saveAcks() {
    acksDirty = true;
    acksSaving++;
    const r = await api(`${API}/acks`, { method: "PUT", body: { acks } });
    acksSaving--;
    if (!r.ok && r.status !== 401) toast("err", "Acknowledgements not saved", errText(r.data, r.status));
    else acksDirty = false;
  }
  async function loadAcks() {
    if (acksSaving || acksDirty) return;
    const r = await api(`${API}/acks`);
    if (!r.ok || !r.data) return;
    acks = r.data.acks && typeof r.data.acks === "object" ? r.data.acks : {};
    for (const v of Object.values(acks)) v.seen = Date.now();
    renderAlerts();
  }
  const bucket = (v, steps) => { let i = 0; while (i < steps.length && v >= steps[i]) i++; return i; };
  function computeAlerts() {
    const out = [];
    const add = (sev, title, detail, target, group, key, sig) => out.push({ sev, title, detail, target, group: group || "Universe", key: key || title, sig: String(sig === undefined ? sev : sig) });
    if (stateErr) add("crit", "State requests are failing", stateErr, null, "Universe", "state-err");
    if (!feedOpen && Date.now() - feedSince > 8000) add("crit", "Live event feed disconnected", `Reconnecting for ${dur(Date.now() - feedSince)}. The map and events are not live.`, null, "Universe", "feed-down");
    if (!S) return out;
    if (S.paused) add("warn", "Clock held", "No ticks are running. Captains still act, but AP does not regenerate.", { mode: "universe" }, "Universe", "clock-held", "held");
    const left = S.season.ends_tick - S.tick;
    if (S.season.finished) add("info", `Season ${S.season.number} is over`, "Intermission: the next galaxy starts automatically.", { mode: "universe" }, "Universe", "season", `over-${S.season.number}`);
    else if (left * tickMs() < 6 * 3600e3) add("warn", `Season ${S.season.number} ends in ${ticksDur(left)}`, `At tick ${fmt(S.season.ends_tick)}.`, { mode: "universe" }, "Universe", "season", `6h-${S.season.ends_tick}`);
    else if (left * tickMs() < 48 * 3600e3) add("info", `Season ${S.season.number} ends in ${ticksDur(left)}`, `At tick ${fmt(S.season.ends_tick)}.`, { mode: "universe" }, "Universe", "season", `48h-${S.season.ends_tick}`);
    const unreadReplies = sysopReplies().filter((m) => !seen.has(m.id));
    if (unreadReplies.length) add("info", plural(unreadReplies.length, "unread reply", "unread replies") + " to sysop", "Captains answering operator messages by private radio.", { mode: "comms", filter: "inbox" }, "Comms", "replies", Math.max(...unreadReplies.map((m) => m.id)));
    // Mirrors the sysop MCP "feedback" alert (same key and signature, so acknowledgements are shared).
    const fbOpen = isNum(S.feedback_open) ? S.feedback_open : 0;
    if (fbOpen > 0) {
      const newest = (S.feedback_recent || []).filter((r) => r.status === "open").sort((a, b) => b.id - a.id)[0];
      add("warn", `${fbOpen} open feedback report${fbOpen === 1 ? "" : "s"}`, newest ? `${newest.from || "anonymous"}: ${newest.about}` : "Agents reported problems. Review them in Feedback.", { mode: "feedback", filter: "open" }, "Comms", "feedback", String(S.feedback_open_max_id));
    }
    // Mirror the sysop MCP "chronicle-pitched" and "chronicle-reported" alerts (same keys and
    // signatures: the highest pitched production id, the highest reported page id).
    const pitched = chPitched();
    if (pitched.length) {
      const known = pitched.map(chProd).filter(Boolean);
      add("info", `${pitched.length} production pitch${pitched.length === 1 ? "" : "es"} waiting`, known.length ? known.slice(0, 4).map((x) => `#${x.id} ${x.title} by ${x.pitched_by}`).join(" | ") : `Pitches ${pitched.map((i) => `#${i}`).join(", ")}: greenlight or decline them.`, pitched.length === 1 ? { kind: "cprod", id: pitched[0] } : { mode: "chronicle", view: "productions" }, "Chronicle", "chronicle-pitched", String(Math.max(...pitched)));
    }
    const reported = chReported();
    if (reported.length) {
      const known = reported.map(chPage).filter(Boolean);
      add("warn", `${reported.length} Chronicle page${reported.length === 1 ? "" : "s"} reported`, known.length ? known.slice(0, 4).map((x) => `#${x.id} ${x.title} (${plural(chOpenReports(x), "report")})`).join(" | ") : `Pages ${reported.map((i) => `#${i}`).join(", ")}: review them and strike what breaks the content rule.`, reported.length === 1 ? { kind: "cpage", id: reported[0] } : { mode: "chronicle", view: "pages", filter: "reported" }, "Chronicle", "chronicle-reported", String(Math.max(...reported)));
    }
    // Mirror the sysop MCP "improve-review" and "improve-proposed" alerts (same keys and
    // signatures: every patch revision the board holds, the highest id a captain ever proposed), read from the
    // state's `improve` block; the board, when loaded, names the items.
    const im = S.improve;
    if (im) {
      const review = Array.isArray(im.review_ids) ? im.review_ids : [];
      if (review.length) {
        const flagged = Array.isArray(im.flagged_in_review) ? im.flagged_in_review : [];
        const full = review.length >= im.review_max;
        const detail = [];
        if (flagged.length) detail.push(`Read in full before shipping: ${flagged.map((id) => { const x = imItem(id); const f = x && imFlagged(x); return `#${id} touches ${f ? f.join(" and ") : "AGENTS.md or CLAUDE.md"}`; }).join("; ")}.`);
        if (full) detail.push(`The review queue is full (${review.length} of ${im.review_max}): new patches are refused.`);
        detail.push(review.slice().reverse().slice(0, 4).map((id) => { const x = imItem(id); if (!x) return `#${id}`; const p = (x.patches || [])[(x.patches || []).length - 1]; return `#${id} ${x.title} (rev ${(x.patches || []).length}${p ? ` by ${imName(p.by, p.call_sign)}` : ""})`; }).join(" | "));
        add(flagged.length || full ? "warn" : "info", `${review.length} improve item${review.length === 1 ? "" : "s"} in review`, detail.join(" "), { mode: "improve", filter: "review" }, "Improve", "improve-review", String(im.patch_revs));
      }
      if (im.proposed > 0 && im.proposals_max_id > 0) {
        const props = imItems().filter((x) => x.status === "proposed").sort((a, b) => b.id - a.id);
        add("info", `${im.proposed} improve proposal${im.proposed === 1 ? "" : "s"} waiting for a decision`, props.length ? props.slice(0, 4).map((x) => `#${x.id} ${x.title} by ${x.author_current || x.author_name}`).join(" | ") : "Accept or decline them on the improve board.", { mode: "improve", filter: "proposed" }, "Improve", "improve-proposed", String(im.proposals_max_id));
      }
    }
    const pend = rulesOrig ? diffRules(rulesOrig, rulesWork).length : 0;
    if (pend) add("warn", plural(pend, "rule edit") + " not applied", "Review and apply, or discard them.", { mode: "rules" }, "Universe", "rules-pending", pend);
    const caps = captains().filter((p) => !p.banned);
    // Silent captains: check-in bots should act every 15 to 20 minutes.
    const act = actAlerts && new Map((actAlerts.players || []).map((p) => [p.id, p]));
    const neverIn = [];
    for (const p of caps) {
      const a = act && act.get(p.id);
      const lastMs = a && a.last_call_ms ? a.last_call_ms : null;
      if (act && !lastMs) { neverIn.push(p); continue; }
      const quiet = lastMs ? Date.now() - lastMs : seenTicks(p) * tickMs();
      if (quiet > 3600e3) add("warn", `${p.name} silent for ${dur(quiet)}`, `Last ${lastMs ? "call" : "action"} ${dur(quiet)} ago. Its check-in schedule may have stopped.`, { kind: "captain", id: p.id, tab: "activity" }, "Captains", `silent:${p.id}`, bucket(quiet, [6 * 3600e3, 24 * 3600e3]));
      if (a && a.calls >= 12 && a.refused / a.calls >= 0.25) add("warn", `${p.name}: ${Math.round((a.refused / a.calls) * 100)}% of calls refused`, `${fmt(a.refused)} of ${fmt(a.calls)} in 6 h. It may be confused by a rule or tool.`, { kind: "captain", id: p.id, tab: "activity" }, "Captains", `refusals:${p.id}`, bucket(a.refused / a.calls, [0.5, 0.75]));
    }
    if (neverIn.length) add("info", `${plural(neverIn.length, "captain")} with no calls in 6 h`, neverIn.slice(0, 6).map((p) => p.name).join(", ") + (neverIn.length > 6 ? ` and ${neverIn.length - 6} more` : ""), { mode: "activity" }, "Captains", "never-in", neverIn.map((p) => p.id).sort((x, y) => x - y).join());
    const nl = actAlerts && actAlerts.not_logged_in;
    if (nl && nl.refused >= 30) add("warn", `${fmt(nl.refused)} unauthenticated calls refused in 6 h`, Object.entries(nl.refusal_codes || {}).slice(0, 4).map(([k, v]) => `${k} ${v}`).join(", "), { kind: "anon", id: "anon" }, "Access", "anon-refused", bucket(nl.refused, [100, 300, 1000, 3000]));
    const unreadOp = caps.filter((p) => p.operator_unread);
    if (unreadOp.length) add("info", `${plural(unreadOp.length, "captain")} not yet read your message`, unreadOp.slice(0, 6).map((p) => `${p.name} (${p.operator_unread})`).join(", "), unreadOp.length === 1 ? { kind: "captain", id: unreadOp[0].id } : { mode: "captains" }, "Captains", "op-unread", unreadOp.map((p) => `${p.id}:${p.operator_unread}`).join());
    const unack = caps.filter((p) => p.announcements_unacked);
    if (unack.length) add("info", `${plural(unack.length, "captain")} with unacknowledged announcements`, unack.slice(0, 6).map((p) => p.name).join(", ") + (unack.length > 6 ? ` and ${unack.length - 6} more` : ""), { mode: "captains" }, "Captains", "ann-unacked", unack.map((p) => p.id).join());
    const order = { crit: 0, warn: 1, info: 2 };
    return out.sort((a, b) => order[a.sev] - order[b.sev]);
  }
  const isAcked = (a) => {
    const k = acks[a.key];
    return !!k && a.sev !== "crit" && k.sig === a.sig && (!k.until || Date.now() < k.until);
  };
  function ackAlert(a, snoozeMs) {
    acks[a.key] = { sig: a.sig, at: Date.now(), seen: Date.now(), until: snoozeMs ? Date.now() + snoozeMs : null, title: a.title };
    saveAcks();
    renderAlerts();
  }
  function unackAlert(a) { delete acks[a.key]; saveAcks(); renderAlerts(); }
  function renderAlerts() {
    const all = computeAlerts();
    // Keep acknowledgements while their alert is around; forget them 10 minutes after it clears.
    if (S && actAlerts) {
      const live = new Set(all.map((a) => a.key));
      let changed = false;
      for (const [k, v] of Object.entries(acks)) {
        if (live.has(k)) v.seen = Date.now();
        else if (Date.now() - (v.seen || v.at || 0) > 600e3) { delete acks[k]; changed = true; }
      }
      if (changed) saveAcks();
    }
    const acked = all.filter(isAcked);
    alertList = all.filter((a) => !isAcked(a));
    const serious = alertList.filter((a) => a.sev !== "info");
    const crit = alertList.some((a) => a.sev === "crit");
    // The top-bar chip is the way to Attention (it has no sidebar entry): always shown, quiet
    // when nothing needs the operator.
    const chip = $("alert-chip");
    chip.classList.toggle("quiet", !serious.length);
    chip.classList.toggle("crit", crit);
    $("alert-chip-t").textContent = serious.length ? plural(serious.length, "alert") : alertList.length ? plural(alertList.length, "note") : "No alerts";
    chip.title = serious.length ? serious.map((x) => x.title).join("\n") : "Attention (3): nothing needs you";
    if (mode !== "alerts") return;
    $("al-count").textContent = alertList.length ? String(alertList.length) : "";
    const tools = $("al-tools");
    tools.replaceChildren();
    const ackable = alertList.filter((a) => a.sev !== "crit");
    if (ackable.length > 1) tools.append(alertBtn("Acknowledge all", () => { for (const a of ackable) acks[a.key] = { sig: a.sig, at: Date.now(), seen: Date.now(), until: null, title: a.title }; saveAcks(); renderAlerts(); }, "Hide every open alert until it changes"));
    if (acked.length) {
      const t = alertBtn(showAcked ? `Hide acknowledged (${acked.length})` : `Show acknowledged (${acked.length})`, () => { showAcked = !showAcked; renderAlerts(); });
      tools.append(t);
    }
    const ul = $("al-list");
    const frag = document.createDocumentFragment();
    if (!alertList.length) frag.append(el("li", "none", acked.length ? `Nothing new. ${plural(acked.length, "acknowledged alert")} still open.` : "Nothing needs attention."));
    let g = null;
    const row = (a, isAck) => {
      if (!isAck && a.group !== g) { g = a.group; frag.append(el("li", "group-h", g)); }
      const li = el("li", `${a.sev}${isAck ? " acked" : ""}`);
      if (a.target && a.target.kind) li.dataset.sel = selKey(a.target.kind, a.target.id);
      const body = el("div");
      body.append(el("b", "", a.title));
      if (a.detail) { const d = el("p", "", a.detail); d.title = a.detail; body.append(d); }
      if (isAck) { const k = acks[a.key]; body.append(el("p", "ackd", k.until ? `Snoozed until ${clockT(k.until).slice(0, 5)}` : `Acknowledged ${dur(Date.now() - k.at)} ago; returns if it changes`)); }
      const acts = el("div", "al-acts");
      if (isAck) acts.append(alertBtn("Restore", () => unackAlert(a)));
      else if (a.sev !== "crit") acts.append(alertBtn("Acknowledge", () => ackAlert(a), "Hide until it changes"), alertBtn("1 h", () => ackAlert(a, 3600e3), "Snooze for an hour"));
      body.append(acts);
      li.append(el("i"), body);
      li.addEventListener("click", () => goAlert(a));
      frag.append(li);
    };
    alertList.forEach((a) => row(a, false));
    if (showAcked && acked.length) { frag.append(el("li", "group-h", "Acknowledged")); for (const a of acked) row(a, true); }
    ul.replaceChildren(frag);
    markSelected();
  }
  function alertBtn(label, fn, title) {
    const b = el("button", "btn xs ghost", label);
    b.type = "button";
    if (title) b.title = title;
    b.addEventListener("click", (e) => { e.stopPropagation(); fn(); });
    return b;
  }
  function goAlert(a) {
    const t = a.target;
    if (!t) return;
    if (t.kind) select(t.kind, t.id, { pan: true, tab: t.tab });
    else if (t.mode === "feedback") { setMode("feedback"); if (t.filter) setFbFilter(t.filter); }
    else if (t.mode === "chronicle") { setMode("chronicle"); setChView(t.view); setChFilter(t.filter || "all"); }
    else if (t.mode === "improve") { setMode("improve"); setImFilter(t.filter || "all"); }
    else { setMode(t.mode); if (t.filter) { cmFilter = t.filter; $("cm-split").dataset.view = "list"; renderComms(); } }
  }
  $("alert-chip").addEventListener("click", () => setMode("alerts"));

  // ======================================================================
  // Comms: threads (sysop inbox per captain, public, corporations, private
  // pairs), search across every message the world holds, moderation, and the
  // operator's own messages woven into each captain's sysop conversation.
  // ======================================================================
  let cmThread = "all", cmQ = "", cvQ = "", cmFilter = "all", cmKind = "message";
  // Replies to sysop the operator has seen (by id), kept per browser.
  const seen = new Set(store.get("seenReplies", []));
  const unseenReply = (m) => isReply(m) && !seen.has(m.id);
  const justSeen = new Set();
  const wideComms = () => $("m-comms").getBoundingClientRect().width >= 760;
  let radioAll = [], radioAt = 0, radioBusy = false, cmTarget = null;
  const opMsgs = new Map(); // captain id -> operator messages from its inbox
  const SYSOP_ID = 0;
  const isReply = (m) => m.channel === "private" && m.to_id === SYSOP_ID;
  const sysopReplies = () => allRadio().filter(isReply);
  async function loadRadio(force) {
    if (radioBusy || (!force && Date.now() - radioAt < 5000)) return;
    radioBusy = true;
    const r = await api(`${API}/radio?limit=2000`);
    radioBusy = false;
    if (r.ok && r.data) { radioAll = r.data.radio || []; radioAt = Date.now(); if (mode === "comms") renderComms(); }
  }
  // The full history merged with the (fresher) state copy, oldest first.
  function allRadio() {
    const m = new Map();
    for (const x of radioAll) m.set(x.id, x);
    for (const x of (S && S.radio) || []) m.set(x.id, x);
    return [...m.values()].sort((a, b) => a.id - b.id);
  }
  function markRepliesSeen() {
    const replies = sysopReplies();
    // A conversation counts as read once it is open on screen.
    const open = mode === "comms" && $("cm-split").dataset.view === "detail" || (mode === "comms" && wideComms());
    const fresh = open ? replies.filter((m) => !seen.has(m.id) && (cmThread === "all" || cmThread === `sys:${m.from_id}` || (cmThread.startsWith("cap:") && cmThread === `cap:${m.from_id}`))) : [];
    if (fresh.length) {
      for (const m of fresh) { seen.add(m.id); justSeen.add(m.id); }
      store.set("seenReplies", [...seen].slice(-3000));
      setTimeout(renderAlerts, 0);
    }
    const unseen = replies.filter((m) => !seen.has(m.id)).length;
    $("b-comms").hidden = !unseen;
    $("b-comms").textContent = String(unseen);
  }
  const pairKey = (a, b) => (a < b ? `p:${a}:${b}` : `p:${b}:${a}`);
  const threadKey = (m) => (m.channel === "public" ? "public" : m.channel === "corp" ? `c:${m.corp_id}` : isReply(m) ? `sys:${m.from_id}` : pairKey(m.from_id, m.to_id));
  function buildThreads(radio) {
    const t = new Map();
    for (const m of radio) {
      const k = threadKey(m);
      let x = t.get(k);
      if (!x) {
        x = { k, n: 0, last: null, unread: 0, people: [] };
        if (k === "public") { x.kind = "public"; x.title = "Public radio"; }
        else if (k.startsWith("c:")) { x.kind = "corp"; x.title = m.corp; x.corp = m.corp_id; }
        else if (k.startsWith("sys:")) { x.kind = "sysop"; x.title = m.from; x.people = [m.from_id]; }
        else { x.kind = "private"; const [a, b] = m.from_id < m.to_id ? [[m.from, m.from_id], [m.to, m.to_id]] : [[m.to, m.to_id], [m.from, m.from_id]]; x.title = `${a[0]} and ${b[0]}`; x.people = [a[1], b[1]]; }
        t.set(k, x);
      }
      x.n++;
      x.last = m;
      if (unseenReply(m)) x.unread++;
    }
    // Captains the operator has messaged but who have not replied yet still get a thread.
    for (const [pid, list] of opMsgs) {
      if (!list.length) continue;
      const k = `sys:${pid}`;
      if (!t.has(k)) { const p = player(pid); t.set(k, { k, kind: "sysop", title: p ? p.name : `#${pid}`, people: [pid], n: 0, last: null, unread: 0 }); }
    }
    return t;
  }
  const THREAD_FILTERS = [["all", "All"], ["inbox", "Sysop inbox"], ["public", "Public"], ["corp", "Corporations"], ["private", "Private"], ["unread", "Unread"]];
  function threadTitle(k, threads) {
    if (k === "all") return ["All traffic", "Every channel, oldest at the top"];
    if (k.startsWith("cap:")) { const p = player(Number(k.slice(4))); return [`Everything involving ${p ? p.name : k.slice(4)}`, "Public, corporation and private traffic"]; }
    const x = threads.get(k);
    if (!x) return [k, ""];
    if (x.kind === "sysop") return [`${x.title} and sysop`, "Replies by private radio to sysop, and your operator messages"];
    if (x.kind === "corp") return [`Corp: ${x.title}`, "Corporation channel"];
    if (x.kind === "private") return [x.title, "Private hails"];
    return [x.title, "Seen by every captain and spectator"];
  }
  function inThread(m, k) {
    if (k === "all") return true;
    if (k.startsWith("cap:")) { const id = Number(k.slice(4)); return m.from_id === id || m.to_id === id; }
    return threadKey(m) === k;
  }
  // Text with the query highlighted (built from text nodes, never HTML).
  function marked(text, q) {
    const span = el("span");
    if (!q) { span.textContent = text; return span; }
    const low = text.toLowerCase();
    let i = 0;
    for (let j = low.indexOf(q, 0); j >= 0 && i < text.length; j = low.indexOf(q, i)) {
      span.append(document.createTextNode(text.slice(i, j)), el("mark", "", text.slice(j, j + q.length)));
      i = j + q.length;
    }
    span.append(document.createTextNode(text.slice(i)));
    return span;
  }
  function snippet(text, q, n = 90) {
    if (!q) return clip(text, n);
    const j = text.toLowerCase().indexOf(q);
    if (j < 0) return clip(text, n);
    const start = Math.max(0, j - 30);
    return `${start ? "…" : ""}${text.slice(start, start + n)}${start + n < text.length ? "…" : ""}`;
  }
  function openThread(k, o = {}) {
    if (k !== cmThread) justSeen.clear();
    cmThread = k;
    // A corporation's thread offers posting on its channel, and starts there.
    const isCorp = k.startsWith("c:");
    $("cm-kind-corp").hidden = !isCorp;
    if (isCorp) setCmKind("corp");
    else if (cmKind === "corp") setCmKind("message");
    cvQ = "";
    $("cv-q").value = "";
    cmTarget = o.target || null;
    $("cm-split").dataset.view = "detail";
    if (k.startsWith("sys:")) {
      const pid = Number(k.slice(4));
      loadOpMsgs(pid);
      if (cmKind === "message") { renderComms(); $("cm-to").value = String(pid); updateCmPlaceholder(); }
    }
    cmSig = "";
    renderComms();
    if (!cmTarget) stickComms(true);
  }
  function loadOpMsgs(pid) {
    return fetchDetail(pid).then((d) => {
      if (!d) return;
      opMsgs.set(pid, (d.inbox || []).filter((n) => n.kind === "sysop").map((n) => ({ op: true, id: `op${n.id}`, tick: n.tick, at_ms: n.at_ms || 0, text: n.text, read: n.read })));
      if (mode === "comms") { cmSig = ""; renderComms(); }
    });
  }
  let cmSig = "", thSig = "";
  function renderComms() {
    markRepliesSeen();
    fillRecipients();
    const radio = allRadio();
    const threads = buildThreads(radio);
    const q = cmQ.toLowerCase();
    $("cm-count").textContent = `${fmt(radio.length)} messages`;
    // Filter chips.
    const counts = { all: threads.size, inbox: 0, public: 0, corp: 0, private: 0, unread: 0 };
    for (const x of threads.values()) { counts[x.kind === "sysop" ? "inbox" : x.kind]++; if (x.unread) counts.unread++; }
    const chips = $("cm-filters");
    const chipSig = JSON.stringify([counts, cmFilter]);
    if (chips.dataset.sig !== chipSig) {
      chips.dataset.sig = chipSig;
      chips.replaceChildren(...THREAD_FILTERS.map(([k, l]) => {
        const b = el("button", "chip", l);
        b.type = "button";
        b.setAttribute("aria-pressed", String(cmFilter === k));
        if (k !== "all" && counts[k]) b.append(el("small", "", String(counts[k])));
        b.addEventListener("click", () => { cmFilter = k; renderComms(); });
        return b;
      }));
    }
    // Thread list: grouped, newest activity first, with matching messages when searching.
    const list = [...threads.values()].filter((x) => {
      if (cmFilter === "inbox" && x.kind !== "sysop") return false;
      if (cmFilter !== "all" && cmFilter !== "inbox" && cmFilter !== "unread" && x.kind !== cmFilter) return false;
      if (cmFilter === "unread" && !x.unread) return false;
      if (q && !x.title.toLowerCase().includes(q) && !x.people.some((id) => { const p = player(id); return p && `${p.name} ${p.model || ""}`.toLowerCase().includes(q); })) return false;
      return true;
    }).sort((a, b) => (b.unread > 0) - (a.unread > 0) || (b.last ? b.last.id : 0) - (a.last ? a.last.id : 0));
    const hits = q.length >= 2 ? radio.filter((m) => m.text.toLowerCase().includes(q) || m.from.toLowerCase().includes(q)).reverse().slice(0, 80) : [];
    const sig = JSON.stringify([list.map((x) => [x.k, x.n, x.unread, x.last && x.last.id]), hits.map((m) => m.id), cmThread, q]);
    if (sig !== thSig) {
      thSig = sig;
      const ul = $("cm-threads");
      const frag = document.createDocumentFragment();
      const row = (k, title, preview, time, n, opts = {}) => {
        const li = el("li", opts.unread ? "unread" : "");
        li.dataset.row = k;
        li.setAttribute("aria-selected", String(cmThread === k));
        const tt = el("div", "tt");
        if (opts.dot) tt.append(opts.dot);
        tt.append(marked(title, q && opts.markTitle ? q : ""));
        const tn = el("div", "tn");
        if (opts.tag) tn.append(opts.tag);
        if (n) tn.append(el("span", "n", String(n)));
        li.append(tt, el("div", "tm", time || ""), opts.previewNode || el("div", "tp", preview || ""), tn);
        li.addEventListener("click", () => openThread(k, opts.open));
        return li;
      };
      if (!q && cmFilter === "all") frag.append(row("all", "All traffic", radio.length ? `${radio[radio.length - 1].from}: ${radio[radio.length - 1].text}` : "No traffic yet", radio.length ? msgTime(radio[radio.length - 1]) : "", radio.length));
      const groups = [["sysop", "Sysop inbox"], ["public", "Public"], ["corp", "Corporations"], ["private", "Private hails"]];
      for (const [kind, label] of groups) {
        const g = list.filter((x) => x.kind === kind);
        if (!g.length) continue;
        frag.append(el("li", "gh", `${label} · ${g.length}`));
        for (const x of g.slice(0, 300)) {
          const last = x.last;
          const p = x.people.length === 1 ? player(x.people[0]) : null;
          const d = x.kind === "sysop" ? dot(x.title) : x.kind === "private" ? dot(x.title.split(" and ")[0]) : null;
          const tg = x.kind === "sysop" && p && p.operator_unread ? tag(`${p.operator_unread} unread by it`, "info", "Operator messages it has not read yet") : null;
          frag.append(row(x.k, x.title, last ? `${last.from}: ${last.text}` : "Operator messages only", last ? msgTime(last) : "", x.n, { unread: x.unread, dot: d, tag: tg, markTitle: true }));
        }
      }
      if (hits.length) {
        frag.append(el("li", "gh", `Messages matching “${cmQ}” · ${hits.length}${hits.length === 80 ? "+" : ""}`));
        for (const m of hits) {
          const k = threadKey(m);
          const [title] = threadTitle(k, threads);
          const pv = el("div", "tp");
          pv.append(marked(snippet(`${m.from}: ${m.text}`, q), q));
          frag.append(row(`hit:${m.id}`, title, "", msgTime(m), 0, { previewNode: pv, open: { target: m.id }, dot: dot(m.from) }));
          frag.lastChild.addEventListener("click", (e) => { e.stopImmediatePropagation(); openThread(k, { target: m.id }); }, { capture: true });
        }
      }
      if (!frag.childNodes.length) frag.append(el("li", "empty-l", q ? "No thread or message matches." : "No conversations here."));
      ul.replaceChildren(frag);
    }
    // Conversation.
    const [title, sub] = threadTitle(cmThread, threads);
    $("cv-title").textContent = title;
    let msgs = radio.filter((m) => inThread(m, cmThread));
    // In the order they were sent: by tick, then by journal time within a tick (older messages
    // have no time: an operator message then goes first, as before).
    if (cmThread.startsWith("sys:")) msgs = msgs.concat(opMsgs.get(Number(cmThread.slice(4))) || []).sort((a, b) => a.tick - b.tick || (a.at_ms && b.at_ms ? a.at_ms - b.at_ms : (a.op ? -1 : 0) - (b.op ? -1 : 0)));
    const vq = cvQ.toLowerCase();
    const shown = vq ? msgs.filter((m) => `${m.from || "sysop"} ${m.text}`.toLowerCase().includes(vq)) : msgs;
    $("cv-sub").textContent = vq ? `${shown.length} of ${msgs.length} messages match` : `${sub}${sub ? " · " : ""}${plural(msgs.length, "message")}`;
    renderPeople(threads);
    const csig = JSON.stringify([cmThread, shown.map((m) => m.id), vq, seen.size, sel && sel.kind === "msg" ? sel.id : null, cmTarget]);
    if (csig === cmSig) return;
    cmSig = csig;
    const scroller = $("cm-scroll");
    const wasBottom = atBottom(scroller);
    const ol = $("cm-list");
    const frag = document.createDocumentFragment();
    let lastDay = null, prev = null;
    for (const m of shown.slice(-1500)) {
      const t = sentAt(m);
      const day = t ? new Date(t).toDateString() : null;
      if (day && day !== lastDay) { lastDay = day; frag.append(el("li", "day", new Date(t).toLocaleDateString([], { weekday: "long", month: "short", day: "numeric" }))); prev = null; }
      const from = m.op ? "sysop" : m.from;
      const first = !prev || prev.from !== from || m.tick - prev.tick > 30 || prev.ch !== m.channel;
      prev = { from, tick: m.tick, ch: m.channel };
      const li = el("li", ["msg", first ? "first" : "", m.op ? "out" : "", m.system ? "sys" : "", unseenReply(m) || justSeen.has(m.id) ? "unseen" : "", cmTarget === m.id ? "target" : ""].filter(Boolean).join(" "));
      if (!m.op) li.dataset.sel = selKey("msg", m.id);
      li.id = `msg-${m.id}`;
      const av = el("div", "av");
      av.append(dot(from, m.system ? "fed" : null));
      const h = el("div", "mh");
      const name = el("b", "", m.op ? "Operator" : m.from);
      if (!m.op && player(m.from_id)) name.addEventListener("click", (e) => { e.stopPropagation(); select("captain", m.from_id, { pan: true }); });
      h.append(name);
      const where = m.op ? `to ${(player(Number(cmThread.slice(4))) || {}).name || "captain"}${m.read ? ", read" : ", not read yet"}` : cmThread === "all" || cmThread.startsWith("cap:") ? (m.channel === "private" ? `to ${isReply(m) ? "sysop" : m.to}` : m.channel === "corp" ? `on ${m.corp}` : "public") : "";
      if (where) h.append(el("span", "to", where));
      const tt = el("span", "cell-t", t ? clockT(t).slice(0, 5) : `t${m.tick}`);
      tt.title = `tick ${fmt(m.tick)}${t ? ` · ${new Date(t).toLocaleString()}` : ""}`;
      h.append(tt);
      const body = el("div", "mt");
      body.append(marked(m.text, vq));
      li.append(av, h, body);
      if (!m.op) {
        const acts = el("div", "hover-acts");
        const sender = player(m.from_id);
        const mk = (label, fn, cls) => { const b = el("button", cls || "", label); b.type = "button"; b.addEventListener("click", (e) => { e.stopPropagation(); fn(); }); return b; };
        if (sender && sender.kind !== "npc") acts.append(mk("Reply", () => replyTo(sender)));
        if (cmThread === "all" || cmThread.startsWith("cap:") || cvQ) acts.append(mk("Thread", () => openThread(threadKey(m), { target: m.id })));
        acts.append(mk("Delete", () => deleteMsg(m), "danger"));
        li.append(acts);
        li.addEventListener("click", () => select("msg", m.id));
      }
      frag.append(li);
    }
    if (!shown.length) frag.append(el("li", "note-l", vq ? "Nothing in this thread matches." : msgs.length ? "" : "No messages in this thread yet."));
    ol.replaceChildren(frag);
    markSelected();
    if (cmTarget) { const n = $(`msg-${cmTarget}`); if (n) n.scrollIntoView({ block: "center" }); }
    else if (wasBottom) stickComms(true);
  }
  function renderPeople(threads) {
    const box = $("cv-people");
    let ids = [];
    if (cmThread.startsWith("sys:") || cmThread.startsWith("cap:")) ids = [Number(cmThread.slice(4))];
    else if (cmThread.startsWith("p:")) ids = cmThread.slice(2).split(":").map(Number);
    else if (cmThread.startsWith("c:")) { const c = S.corps.find((x) => `c:${x.id}` === cmThread); if (c) ids = c.members.map(playerByName).filter(Boolean).map((p) => p.id); }
    const sig = JSON.stringify([cmThread, ids]);
    if (box.dataset.sig === sig) return;
    box.dataset.sig = sig;
    box.replaceChildren(...ids.map(player).filter(Boolean).map((p) => {
      const b = el("button", "person");
      b.type = "button";
      b.title = `Inspect ${p.name}`;
      b.append(dot(p.name, p.kind === "npc" && p.npc), document.createTextNode(p.name), el("span", "k", `s${p.sector}`));
      b.addEventListener("click", () => select("captain", p.id, { pan: true }));
      return b;
    }));
  }
  const msgTime = (m) => { const t = sentAt(m); if (!t) return `t${m.tick}`; return Date.now() - t < 86400e3 ? clockT(t).slice(0, 5) : new Date(t).toLocaleDateString([], { month: "short", day: "numeric" }); };
  function replyTo(p) { setCmKind("message"); renderComms(); $("cm-to").value = String(p.id); updateCmPlaceholder(); $("cm-text").focus(); }
  async function deleteMsg(m) {
    const where = m.channel === "private" ? `a private hail to ${isReply(m) ? "sysop" : m.to}` : m.channel === "corp" ? `the ${m.corp} corp channel` : "public radio";
    if (!await confirmDlg({ title: "Delete this radio message?", body: [`From ${m.from} on ${where}, tick ${m.tick}. It disappears for every captain and every viewer.`, el("blockquote", "", m.text)], ok: "Delete message" })) return;
    if (await act("delete_radio", { id: m.id }, "Moderation")) { radioAll = radioAll.filter((x) => x.id !== m.id); if (sel && sel.kind === "msg" && sel.id === m.id) select(null); loadRadio(true); }
  }
  const atBottom = (n) => n.scrollHeight - n.scrollTop - n.clientHeight < 40;
  function stickComms(force) { const s = $("cm-scroll"); if (force) s.scrollTop = s.scrollHeight; }
  $("cm-q").addEventListener("input", (e) => { cmQ = e.target.value.trim(); renderComms(); });
  $("cv-q").addEventListener("input", (e) => { cvQ = e.target.value.trim(); cmTarget = null; renderComms(); });
  $("cm-back").addEventListener("click", () => { $("cm-split").dataset.view = "list"; });
  // The corporation whose channel is open in Comms, if any.
  const cmCorp = () => (cmThread && cmThread.startsWith("c:") ? (S.corps || []).find((c) => `c:${c.id}` === cmThread) || null : null);
  function setCmKind(k) {
    if (k === "corp" && !cmCorp()) k = "message";
    cmKind = k;
    for (const b of document.querySelectorAll("#cm-kind button")) b.setAttribute("aria-pressed", String(b.dataset.v === k));
    $("cm-to-w").hidden = k !== "message";
    $("cm-tell-w").hidden = k !== "message";
    $("cm-pin-w").hidden = k !== "broadcast";
    $("cm-text").maxLength = k === "broadcast" ? 500 : k === "corp" ? 2000 : 1000;
    updateCmPlaceholder();
  }
  function fillRecipients() {
    const to = $("cm-to");
    const toOpts = [["", "Every captain"], ...captains().filter((p) => !p.banned).sort((a, b) => a.name.localeCompare(b.name)).map((p) => [String(p.id), p.name])];
    const toSig = JSON.stringify(toOpts);
    if (to.dataset.sig === toSig) return;
    const cur = to.value;
    to.replaceChildren(...toOpts.map(([v, l]) => { const o = el("option", "", l); o.value = v; return o; }));
    to.dataset.sig = toSig;
    to.value = toOpts.some(([v]) => v === cur) ? cur : "";
  }
  // Phones: the composer folds to one line so the thread gets the screen.
  function setComposer(open) {
    $("cm-form").classList.toggle("collapsed", !open);
    if (open) setTimeout(() => $("cm-text").focus(), 0);
  }
  $("cm-peek").addEventListener("click", () => setComposer(true));
  $("cm-collapse").addEventListener("click", () => { if (!$("cm-text").value.trim()) setComposer(false); else setComposer(false); });
  $("cv-find").addEventListener("click", () => {
    const head = $("cv-find").closest(".conv-head");
    const on = !head.classList.contains("finding");
    head.classList.toggle("finding", on);
    $("cv-find").setAttribute("aria-expanded", String(on));
    if (on) $("cv-q").focus();
    else if (cvQ) { $("cv-q").value = ""; cvQ = ""; cmTarget = null; renderComms(); }
  });
  if (mobile()) setComposer(false);
  function updateCmPlaceholder() {
    const to = $("cm-to");
    const name = to.value && to.selectedIndex >= 0 ? to.options[to.selectedIndex].text : null;
    const corp = cmKind === "corp" ? cmCorp() : null;
    $("cm-text").placeholder = corp ? `Post on ${corp.name}'s channel as the operator (members read it and can answer here)` : cmKind === "broadcast" ? "Broadcast on public radio and news (everyone sees it)" : name ? `Operator message to ${name}` : "Operator message to every captain (private, trusted)";
    $("cm-peek-t").textContent = corp ? `Post to ${corp.name}` : cmKind === "broadcast" ? "Broadcast to everyone" : name ? `Message ${name}` : "Message every captain";
    $("cm-len").textContent = `${$("cm-text").value.length} / ${$("cm-text").maxLength}`;
  }
  for (const b of document.querySelectorAll("#cm-kind button")) b.addEventListener("click", () => setCmKind(b.dataset.v));
  $("cm-to").addEventListener("change", updateCmPlaceholder);
  $("cm-text").addEventListener("input", updateCmPlaceholder);
  $("cm-text").addEventListener("keydown", (e) => { if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) { e.preventDefault(); $("cm-form").requestSubmit(); } });
  $("cm-form").addEventListener("submit", async (e) => {
    e.preventDefault();
    const text = $("cm-text").value.trim();
    if (!text) return;
    $("cm-send").disabled = true;
    let ok;
    if (cmKind === "corp" && cmCorp()) {
      const c = cmCorp();
      ok = await act("corp_radio", { corp: c.id, text }, `Post to ${c.name}`);
      if (ok) loadRadio(true);
    } else if (cmKind === "broadcast") {
      const pin = $("cm-pin").checked;
      ok = await confirmDlg({ title: pin ? "Broadcast and pin?" : "Broadcast to everyone?", body: ["Public radio and news: every captain and every spectator sees it.", el("blockquote", "", text)], ok: "Broadcast", danger: false })
        && await act("broadcast", { message: text, pin }, pin ? "Pinned bulletin" : "Bulletin");
    } else {
      const to = $("cm-to").value;
      ok = await act("message", { ...(to ? { player: Number(to) } : {}), text, tell_owner: $("cm-tell").checked }, to ? `Message to ${$("cm-to").options[$("cm-to").selectedIndex].text}` : "Message to every captain");
      if (ok && to) { if (cmThread !== `sys:${to}` && !cmThread.startsWith("cap:")) openThread(`sys:${to}`); else loadOpMsgs(Number(to)); }
    }
    $("cm-send").disabled = false;
    if (ok) { $("cm-text").value = ""; $("cm-tell").checked = false; $("cm-pin").checked = false; updateCmPlaceholder(); soonState(400); loadRadio(true); if (mobile()) setComposer(false); }
  });
  function composeTo(id, kind) {
    setMode("comms");
    setCmKind(kind || "message");
    fillRecipients();
    if (id !== undefined && id !== null) openThread(`sys:${id}`);
    else { $("cm-to").value = ""; $("cm-split").dataset.view = "detail"; }
    updateCmPlaceholder();
    $("cm-text").focus();
  }

  // ======================================================================
  // Registry: corporations, planets, concessions, diplomacy, standings
  // ======================================================================
  const nameCell = (n) => (n ? { t: n, dot: n } : "–");
  const REG = [
    { k: "corps", label: "Corporations", rows: () => S.corps, cols: [["Name", (r) => r.name], ["Members", (r) => r.members.length, 1], ["Treasury", (r) => r.treasury, 1]], sel: (r) => ["corp", r.id] },
    { k: "planets", label: "Planets (owned)", rows: () => S.planets.filter((p) => p.owner), cols: [["Planet", (r) => r.name], ["Owner", (r) => nameCell(r.owner)], ["Sector", (r) => r.sector, 1], ["Class", (r) => r.class], ["Citadel", (r) => r.citadel, 1], ["Colonists", (r) => r.colonists, 1], ["Fighters", (r) => r.fighters, 1]], sel: (r) => ["planet", r.id] },
    { k: "planets_all", label: "Planets (all)", rows: () => S.planets, cols: [["Planet", (r) => r.name], ["Owner", (r) => nameCell(r.owner)], ["Sector", (r) => r.sector, 1], ["Class", (r) => r.class]], sel: (r) => ["planet", r.id] },
    { k: "concessions", label: "Concessions", rows: () => S.concessions || [], cols: [["Name", (r) => r.name], ["Sector", (r) => r.sector, 1], ["Holder", (r) => nameCell(r.holder)], ["Yield/day", (r) => r.yield_per_day, 1], ["Paid", (r) => r.paid_total, 1], ["Needs ftr", (r) => r.needs_fighters, 1]], sel: (r) => ["sector", r.sector] },
    { k: "defenses", label: "Deployed defenses", rows: () => S.defenses, cols: [["Sector", (r) => r.sector, 1], ["Owner", (r) => nameCell(r.fighters && r.fighters.owner)], ["Fighters", (r) => r.fighters && r.fighters.count, 1], ["Mode", (r) => r.fighters && r.fighters.mode], ["Mines", (r) => r.mines, 1]], sel: (r) => ["sector", r.sector] },
    { k: "leaderboard", label: "Leaderboard", rows: () => (S.leaderboard && S.leaderboard.players) || [], cols: [["#", (r, i) => i + 1, 1], ["Captain", (r) => nameCell(r.name)], ["Net worth", (r) => r.net_worth, 1], ["Model", (r) => r.model]], sel: (r) => ["captain", r.id] },
    { k: "treaties", label: "Treaties", rows: () => S.treaties || [] },
    { k: "wars", label: "Wars", rows: () => S.wars || [] },
    { k: "intents", label: "Declared intents", rows: () => S.intents || [] },
    { k: "contracts", label: "Contracts", rows: () => S.contracts || [] },
    { k: "combat_reports", label: "Combat reports", rows: () => S.combat_reports || [], sel: (r) => (r.sector ? ["sector", r.sector] : null) },
    { k: "champions", label: "Champions", rows: () => (S.champions || []).map((c) => (Array.isArray(c) ? { category: c[0], captain: c[1], model: c[2], score: c[3] } : c)) },
    { k: "hall_of_fame", label: "Hall of fame", rows: () => S.hall_of_fame || [] },
    { k: "news", label: "News", rows: () => S.news || [], cols: [["Tick", (r) => r.tick, 1], ["Headline", (r) => r.headline, 0, "wrap"]] },
  ];
  let rgSec = store.get("rgSec", "corps"), rgQ = "", rgSig = "";
  const cellText = (v) => {
    if (v === null || v === undefined || v === "") return "–";
    if (Array.isArray(v)) return v.every((x) => x === null || typeof x !== "object") ? v.join(", ") : JSON.stringify(v);
    if (typeof v === "object") return Object.entries(v).map(([k, x]) => `${k}: ${typeof x === "object" ? JSON.stringify(x) : x}`).join(", ");
    if (typeof v === "boolean") return v ? "yes" : "no";
    return isNum(v) ? v.toLocaleString() : String(v);
  };
  function genericCols(rows) {
    const cols = [];
    for (const r of rows.slice(0, 200)) for (const k of Object.keys(r || {})) if (!cols.includes(k)) cols.push(k);
    return cols.slice(0, 8).map((k) => [k.replace(/_/g, " "), (r) => r[k], rows.some((r) => isNum(r[k])) ? 1 : 0, k === "text" || k === "terms" ? "wrap" : ""]);
  }
  function renderRegistry() {
    const s = $("rg-sec");
    const optSig = REG.map((r) => `${r.k}${r.rows().length}`).join();
    if (s.dataset.sig !== optSig) {
      s.replaceChildren(...REG.map((r) => { const o = el("option", "", `${r.label} (${r.rows().length})`); o.value = r.k; return o; }));
      s.dataset.sig = optSig;
    }
    s.value = rgSec;
    const def = REG.find((r) => r.k === rgSec) || REG[0];
    let rows = def.rows();
    const q = rgQ.toLowerCase();
    if (q) rows = rows.filter((r) => JSON.stringify(r).toLowerCase().includes(q));
    const sig = JSON.stringify([rgSec, rgQ, rows]);
    if (sig === rgSig) { markSelected(); return; }
    rgSig = sig;
    const cols = def.cols || genericCols(rows);
    const fillAt = Math.max(0, cols.findIndex(([, , n, cls]) => !n && cls !== "wrap"));
    $("rg-count").textContent = `${rows.length}`;
    const t = $("rg-table");
    const hr = el("tr");
    cols.forEach(([l, , n], ci) => { const th = el("th", [n ? "n" : "", ci === fillAt ? "fill" : ""].join(" ").trim(), l); th.scope = "col"; hr.append(th); });
    t.tHead.replaceChildren(hr);
    const frag = document.createDocumentFragment();
    rows.slice(0, 1000).forEach((r, i) => {
      const tr = el("tr");
      const target = def.sel ? def.sel(r) : null;
      const pl = !target && (r.captain || r.player || r.name) && playerByName(r.captain || r.player || r.name);
      const tg = target || (pl ? ["captain", pl.id] : null);
      if (tg) { tr.dataset.sel = selKey(tg[0], tg[1]); tr.addEventListener("click", () => select(tg[0], tg[1], { pan: true })); }
      else tr.style.cursor = "default";
      cols.forEach(([, get, n, cls], ci) => {
        const v = get(r, i);
        const td = el("td", [n ? "n" : "", cls || "", ci === fillAt ? "fill" : ""].join(" ").trim());
        if (v && typeof v === "object" && v.dot) { const w = el("span", "who"); w.append(dot(v.dot), el("span", "", v.t)); td.append(w); }
        else td.textContent = cellText(v);
        if (cls === "wrap") td.title = "";
        else td.title = td.textContent;
        tr.append(td);
      });
      frag.append(tr);
    });
    if (!rows.length) { const tr = el("tr", "empty"); const td = el("td", "", "None."); td.colSpan = cols.length || 1; tr.append(td); frag.append(tr); }
    t.tBodies[0].replaceChildren(frag);
    markSelected();
  }
  $("rg-sec").addEventListener("change", (e) => { rgSec = e.target.value; store.set("rgSec", rgSec); renderRegistry(); });
  $("rg-q").addEventListener("input", (e) => { rgQ = e.target.value.trim(); renderRegistry(); });

  // ======================================================================
  // Journal
  // ======================================================================
  let jEntries = [], jTimer = null, jBusy = false, jSeen = 0, jCursor = null, jPending = false;
  const nameOf = (id) => { const p = player(id); return p ? p.name : `#${id}`; };
  const compactArgs = (a) => Object.entries(a || {}).filter(([k, v]) => v !== null && v !== undefined && !/hash$/.test(k) && !(Array.isArray(v) && !v.length)).map(([k, v]) => `${k}=${typeof v === "object" ? JSON.stringify(v) : v}`).join(" ");
  const ADMIN_SUM = {
    grant: (a) => `grant ${nameOf(a.player)} ${fmt(a.credits)} credits, ${fmt(a.ap)} AP`,
    set_player: (a) => `set ${nameOf(a.player)}: ${compactArgs(Object.fromEntries(Object.entries(a).filter(([k]) => k !== "player" && k !== "action")))}`,
    set_rules: (a) => `rules patch: ${clip(Object.keys(a.patch || {}).join(", "), 120)}`,
    mute: (a) => (a.ticks ? `mute ${nameOf(a.player)} for ${fmt(a.ticks)} ticks` : `unmute ${nameOf(a.player)}`),
    revoke_token: (a) => `rotate the token of ${nameOf(a.player)}`,
    ban: (a) => `ban ${nameOf(a.player)}`,
    unban: (a) => `unban ${nameOf(a.player)}`,
    set_owner_token: (a) => `issue an owner link for ${nameOf(a.player)}`,
    remove_player: (a) => `remove captain #${a.player}`,
    set_recovery: (a) => `issue a recovery key for ${nameOf(a.player)}`,
    message: (a) => `message ${a.player ? nameOf(a.player) : "every captain"}${a.tell_owner ? " (and their humans)" : ""}: “${clip(a.text, 120)}”`,
    spawn_skarn: (a) => `spawn Skarn${a.sector ? ` at sector ${a.sector}` : ""}`,
    place_concessions: () => "place concessions",
    create_corp: (a) => `charter “${a.name}” with ${nameOf(a.ceo)} as CEO`,
    set_corp: (a) => `edit corporation #${a.corp}: ${compactArgs(Object.fromEntries(Object.entries(a).filter(([k]) => k !== "corp" && k !== "action")))}`,
    corp_add_member: (a) => `add ${nameOf(a.player)} to corporation #${a.corp}`,
    corp_remove_member: (a) => `remove ${nameOf(a.player)} from corporation #${a.corp}`,
    corp_invite: (a) => `invite ${nameOf(a.player)} to corporation #${a.corp}`,
    corp_uninvite: (a) => `withdraw ${nameOf(a.player)}'s invitation to corporation #${a.corp}`,
    disband_corp: (a) => `disband corporation #${a.corp}`,
    delete_radio: (a) => `delete radio message #${a.id}`,
    delete_operator_text: (a) => `delete operator text containing “${clip(a.contains || "", 60)}”`,
    constitution_strike: (a) => (a.article != null ? `strike constitution article ${a.article}` : `strike constitution proposal #${a.proposal}`),
    constitution_propose: (a) => `operator proposal: ${a.kind}${a.article != null ? ` article ${a.article}` : ""}${a.text ? `: “${clip(a.text, 120)}”` : ""}`,
    constitution_comment: (a) => `comment on constitution proposal #${a.proposal}: “${clip(a.text, 120)}”`,
    constitution_notice: (a) => `${a.pin ? "pinned constitution notice" : "constitution notice"}: “${clip(a.text, 140)}”`,
    broadcast: (a) => `${a.pin ? "pinned bulletin" : "bulletin"}: “${clip(a.message, 140)}”`,
    layout_depth: () => "give the galaxy depth",
    corp_radio: (a) => `post on corporation #${a.corp}'s channel: “${clip(a.text, 140)}”`,
    link_fedspace: (a) => `link FedSpace gateways${a.gateways ? ` (${a.gateways})` : ""}`,
    resolve_feedback: (a) => (a.reopen ? `reopen feedback #${a.id}` : `resolve feedback #${a.id}${a.note ? `: “${clip(a.note, 120)}”` : ""}`),
    set_season_end: (a) => `season ends at tick ${fmt(a.ends_tick)}`,
    end_season: () => "end the season",
    new_season: () => "Big Bang: new season",
  };
  const CMD_SUM = {
    tick: () => "clock tick",
    register: (a) => `registered “${a.name}”${a.model ? ` (${a.model})` : ""} as ${a.kind}`,
    admin: (a) => (ADMIN_SUM[a.action] ? ADMIN_SUM[a.action](a) : `${a.action} ${compactArgs(a)}`),
    warp: (a) => `warp to ${a.destination_sector}`,
    autopilot: (a) => `autopilot to ${a.destination_sector}${a.avoid_sectors && a.avoid_sectors.length ? ` avoiding ${a.avoid_sectors.join(",")}` : ""}`,
    transwarp: (a) => `transwarp to ${a.destination_sector}`,
    radio: (a) => `${a.channel}${a.recipient_id ? ` to ${a.recipient_id}` : ""}: “${clip(a.message, 140)}”`,
    note: (a) => `note “${a.key}”${a.mode ? ` (${a.mode})` : ""}`,
    corp: (a) => `corp ${a.action}${a.name ? ` ${a.name}` : ""}`,
    relief: () => "claimed Federation relief",
    feedback: (a) => `feedback on ${a.about || "?"}: “${clip(a.text, 140)}”${a.model ? ` (${a.model}${a.harness ? ` on ${a.harness}` : ""})` : ""}`,
  };
  function summarize(cmd) {
    const a = cmd.args || {};
    try { if (CMD_SUM[cmd.type]) return CMD_SUM[cmd.type](a); } catch (_) { /* fall through */ }
    return compactArgs(a);
  }
  const jType = (e) => (e.cmd.type === "admin" ? `admin:${e.cmd.args.action}` : e.cmd.type);
  async function loadJournal(older = false) {
    // Event listeners pass an Event, not a paging flag.
    older = older === true;
    if (jBusy) { if (!older) jPending = true; return; }
    jBusy = true;
    const q = new URLSearchParams({limit: "500", include_ticks: String($("jr-ticks").checked)});
    const actor = $("jr-actor").value, command = $("jr-type").value, query = $("jr-q").value.trim();
    if (actor) q.set("actor", actor === "@sysop" ? "sysop" : actor);
    if (command) q.set("command", command);
    if (query) q.set("query", query);
    if (older && jCursor != null) q.set("before_seq", jCursor);
    const r = await api(`${API}/journal?${q}`);
    jBusy = false;
    if (jPending) { jPending = false; loadJournal(); return; }
    if (!r.ok) { $("jr-count").textContent = errText(r.data, r.status); return; }
    const entries = (r.data && r.data.entries) || [];
    jEntries = older ? jEntries.concat(entries) : entries;
    jCursor = r.data.next_before_seq;
    $("jr-older").hidden = !r.data.has_more;
    const types = [...new Set([...jEntries.map(jType), command].filter(Boolean))].sort();
    const ts = $("jr-type");
    ts.replaceChildren(el("option", "", "All commands"), ...types.map((t) => { const o = el("option", "", t); o.value = t; return o; }));
    ts.firstChild.value = "";
    ts.value = command;
    const names = [...new Set([...jEntries.map((e) => e.player), actor === "@sysop" ? "" : actor].filter(Boolean))].sort((a, b) => a.localeCompare(b));
    const as = $("jr-actor");
    const o1 = el("option", "", "Everyone"); o1.value = "";
    const o2 = el("option", "", "Sysop and system"); o2.value = "@sysop";
    as.replaceChildren(o1, o2, ...names.map((n) => { const o = el("option", "", n); o.value = n; return o; }));
    as.value = actor;
    renderJournal();
  }
  function jRows() { return jEntries; }
  function renderJournal() {
    const rows = jRows();
    const max = jEntries.reduce((m, e) => Math.max(m, e.seq), 0);
    $("jr-count").textContent = `${rows.length} entries${jCursor != null ? "; older history available" : ""}`;
    const frag = document.createDocumentFragment();
    for (const e of rows) {
      const tr = el("tr", jSeen && e.seq > jSeen && !reduceMotion ? "fresh" : "");
      tr.dataset.sel = selKey("entry", e.seq);
      const who = el("td");
      if (e.actor === "system") who.append(el("span", "muted", "sysop"));
      else { const w = el("span", "who"); w.append(dot(e.player || "?"), el("span", "", e.player || "?")); who.append(w); if (e.actor === "owner") who.append(el("span", "sub", "owner")); }
      const cmd = el("td", "fill");
      cmd.append(el("b", "", e.cmd.type === "admin" ? e.cmd.args.action : e.cmd.type), el("span", "args", summarize(e.cmd)));
      cmd.title = summarize(e.cmd);
      const t = el("td", "cell-t", clockT(e.at_ms));
      t.title = new Date(e.at_ms).toLocaleString();
      tr.append(el("td", "cell-t", fmt(e.seq)), t, who, cmd);
      tr.addEventListener("click", () => select("entry", e.seq));
      frag.append(tr);
    }
    if (!rows.length) { const tr = el("tr", "empty"); const td = el("td", "", jCursor != null ? "No matches on this page. Continue to older history." : "No matching journal entries."); td.colSpan = 4; tr.append(td); frag.append(tr); }
    $("jr-body").replaceChildren(frag);
    jSeen = max;
    markSelected();
  }
  for (const id of ["jr-actor", "jr-type"]) $(id).addEventListener("change", loadJournal);
  let jSearchTimer = null;
  $("jr-q").addEventListener("input", () => { clearTimeout(jSearchTimer); jSearchTimer = setTimeout(loadJournal, 250); });
  $("jr-older").addEventListener("click", () => {
    if (jBusy || jCursor == null) return;
    $("jr-auto").checked = false;
    journalTimer();
    loadJournal(true);
  });
  $("jr-ticks").addEventListener("change", loadJournal);
  $("jr-auto").addEventListener("change", journalTimer);
  function journalTimer() {
    clearInterval(jTimer);
    jTimer = null;
    if (mode === "journal" && $("jr-auto").checked) jTimer = setInterval(() => { if (!document.hidden) loadJournal(); }, 4000);
  }

  // ======================================================================
  // Feedback: problems agents report (about a page of the guide, a tool or a
  // topic), newest first. A report opens in the inspector, where the operator
  // resolves it (an optional note goes to the captain's inbox) or reopens it.
  // The list comes from /feedback one page at a time (newest first; "Load older"
  // follows next_before_id until has_more is false; a filter change starts over);
  // the state carries the open count and the newest reports, so new ones show at
  // once and a change triggers a refetch of the first page.
  // ======================================================================
  let fbFilter = store.get("fbFilter", "open"), fbQ = "", fbSig = "";
  let fbList = [], fbCounts = { open: null, total: null }, fbLoaded = null, fbErr = null;
  let fbBusy = false, fbAgain = false, fbMissing = false, fbAllTried = false;
  let fbStateSig = null, fbToastFloor = null, rulesPend = 0;
  // Paging: the cursor below the oldest loaded report, and whether older pages were loaded.
  let fbNext = null, fbHasMore = false, fbOlderBusy = false, fbPaged = false;
  const FB_PAGE = 200, NOTE_MAX = 20000;
  const fbById = new Map(); // every report seen, by id (from /feedback and from the state)
  const FB_FILTERS = [["open", "Open"], ["resolved", "Resolved"], ["all", "All"]];
  const fbAnon = (r) => !isNum(r.from_id);
  const fbSender = (r) => r.from || (fbAnon(r) ? "anonymous" : `#${r.from_id}`);
  // A report the improve board accepts: a known sender, not about security (the server refuses
  // the rest anyway; the console just does not offer them).
  const fbPromotable = (r) => !fbAnon(r) && !/^\s*security/i.test(r.about || "");
  const fbStack = (r) => [r.model, r.harness && `on ${r.harness}`].filter(Boolean).join(" ");
  // The state names feedback_open only when the server keeps feedback; without it the console
  // never asks for /feedback (no 404s) and shows the empty state.
  const fbLive = () => !!S && "feedback_open" in S;
  const fbOpenCount = () => (S && isNum(S.feedback_open) ? S.feedback_open : isNum(fbCounts.open) ? fbCounts.open : 0);
  function fbRemember(list) {
    for (const r of list || []) if (r && isNum(r.id)) fbById.set(r.id, { ...(fbById.get(r.id) || {}), ...r });
  }
  async function loadFeedback() {
    if (fbBusy) { fbAgain = true; return; }
    fbBusy = true;
    const want = fbFilter;
    const r = await api(`${API}/feedback?status=${want}&limit=${FB_PAGE}`);
    fbBusy = false;
    if (r.ok && r.data && Array.isArray(r.data.feedback)) {
      fbMissing = false; fbErr = null;
      const page = r.data.feedback;
      fbRemember(page);
      // A refresh keeps the older pages already loaded for this filter (below the new first page).
      // Only when the new page reaches down to what is loaded (no gap between them).
      if (fbPaged && fbLoaded === want && r.data.has_more && page.length && fbList.length && page[page.length - 1].id <= fbList[0].id) {
        const floor = page[page.length - 1].id;
        fbList = page.concat(fbList.filter((x) => x.id < floor));
      } else {
        fbList = page;
        fbPaged = false;
        fbHasMore = !!r.data.has_more;
        fbNext = isNum(r.data.next_before_id) ? r.data.next_before_id : null;
      }
      fbCounts = { open: isNum(r.data.open) ? r.data.open : null, total: isNum(r.data.total) ? r.data.total : null, matching: isNum(r.data.matching) ? r.data.matching : null };
      fbLoaded = want;
    } else if (r.status === 404 || r.ok) {
      // This server has no feedback reports (or answered with something else): show none, quietly.
      fbMissing = r.status === 404; fbErr = null;
      fbList = []; fbLoaded = want; fbPaged = false; fbHasMore = false; fbNext = null;
    } else if (r.status !== 401) fbErr = errText(r.data, r.status);
    if (fbAgain || want !== fbFilter) { fbAgain = false; loadFeedback(); return; }
    fbSig = "";
    renderFeedback();
    renderMoreBadge();
    if (sel && sel.kind === "feedback") buildInspector();
  }
  // The next page of older reports for the current filter.
  async function loadOlderFeedback() {
    if (fbOlderBusy || !fbHasMore || !isNum(fbNext) || fbLoaded !== fbFilter) return;
    const want = fbFilter, before = fbNext;
    fbOlderBusy = true; fbSig = ""; renderFeedback();
    const r = await api(`${API}/feedback?status=${want}&limit=${FB_PAGE}&before_id=${before}`);
    fbOlderBusy = false;
    if (want === fbFilter && fbLoaded === want && before === fbNext) {
      if (r.ok && r.data && Array.isArray(r.data.feedback)) {
        fbErr = null;
        const seen = new Set(fbList.map((x) => x.id));
        fbList = fbList.concat(r.data.feedback.filter((x) => !seen.has(x.id)));
        fbRemember(r.data.feedback);
        fbHasMore = !!r.data.has_more;
        fbNext = isNum(r.data.next_before_id) ? r.data.next_before_id : null;
        fbPaged = true;
      } else if (r.status !== 401) fbErr = errText(r.data, r.status);
    }
    fbSig = "";
    renderFeedback();
  }
  function setFbFilter(f) {
    if (!FB_FILTERS.some(([k]) => k === f)) f = "open";
    const changed = f !== fbFilter;
    if (changed) { fbList = []; fbLoaded = null; fbPaged = false; fbHasMore = false; fbNext = null; }
    fbFilter = f;
    store.set("fbFilter", f);
    fbSig = "";
    renderFeedback();
    if (changed && fbLive()) loadFeedback();
  }
  // Called on every state poll: remember the newest reports, update the badges, refetch on a change.
  function fbOnState() {
    fbRemember(S.feedback_recent);
    const n = fbOpenCount();
    for (const id of ["b-feedback", "b-feedback-m"]) { $(id).hidden = !n; $(id).textContent = String(n); }
    renderMoreBadge();
    if (fbToastFloor === null) fbToastFloor = Math.max(0, isNum(S.feedback_open_max_id) ? S.feedback_open_max_id : 0, ...(S.feedback_recent || []).map((r) => r.id).filter(isNum));
    const sig = JSON.stringify([isNum(S.feedback_open) ? S.feedback_open : null, isNum(S.feedback_open_max_id) ? S.feedback_open_max_id : null]);
    if (sig === fbStateSig) return;
    const first = fbStateSig === null;
    fbStateSig = sig;
    if (!first || "feedback_open" in S) loadFeedback();
  }
  // A live "feedback" event. The feed replays recent events on connect: only reports newer
  // than the newest one already known get a toast and a refetch.
  function fbOnEvent(ev) {
    const id = ev.data && isNum(ev.data.id) ? ev.data.id : null;
    if (id !== null && fbToastFloor !== null && id <= fbToastFloor) return;
    if (id !== null) fbToastFloor = id;
    toast("ok", "New feedback", ev.text || `Feedback #${id}`);
    soonState(400);
    loadFeedback();
  }
  function renderMoreBadge() {
    const n = rulesPend + fbOpenCount() + chPitched().length + chReported().length + imNeeds();
    $("b-more").hidden = !n;
    $("b-more").textContent = String(n);
  }
  function fbRows() {
    const m = new Map();
    if (fbLoaded === fbFilter) for (const r of fbList) m.set(r.id, fbById.get(r.id) || r);
    for (const r of (S && S.feedback_recent) || []) if (r && isNum(r.id)) m.set(r.id, fbById.get(r.id) || r);
    let rows = [...m.values()].filter((r) => fbFilter === "all" || r.status === fbFilter);
    const q = fbQ.toLowerCase();
    if (q) rows = rows.filter((r) => [r.about, r.text, fbSender(r), r.model, r.harness, `#${r.id}`].some((v) => v && String(v).toLowerCase().includes(q)));
    return rows.sort((a, b) => b.id - a.id);
  }
  // The sender: a link to the captain when it is still in this universe.
  function fbWho(r) {
    const p = fbAnon(r) ? null : player(r.from_id);
    if (!p) return el("span", fbAnon(r) ? "muted" : "", fbSender(r));
    const a = link(r.from || p.name, () => select("captain", p.id, { pan: true }));
    a.title = `Inspect ${p.name}`;
    a.addEventListener("click", (e) => e.stopPropagation());
    return a;
  }
  function renderFeedback() {
    if (mode !== "feedback") return;
    const rows = fbRows();
    const open = fbOpenCount(), total = fbCounts.total;
    const counts = { open, resolved: isNum(total) && isNum(fbCounts.open) ? total - fbCounts.open : null, all: total };
    $("fb-count").textContent = open ? `${fmt(open)} open` : "";
    const chips = $("fb-filters");
    const chipSig = JSON.stringify([counts, fbFilter]);
    if (chips.dataset.sig !== chipSig) {
      chips.dataset.sig = chipSig;
      chips.replaceChildren(...FB_FILTERS.map(([k, l]) => {
        const b = el("button", "chip", l);
        b.type = "button";
        b.setAttribute("aria-pressed", String(fbFilter === k));
        if (counts[k]) b.append(el("small", "", compact(counts[k])));
        b.addEventListener("click", () => setFbFilter(k));
        return b;
      }));
    }
    const q = fbQ.toLowerCase();
    const more = fbHasMore && fbLoaded === fbFilter;
    const sig = JSON.stringify([fbFilter, q, rows.map((r) => [r.id, r.status, r.from, r.from_id, r.about, r.model, r.harness, !!player(r.from_id)]), fbMissing, fbErr, fbLoaded === fbFilter, more, fbOlderBusy, fbList.length, fbCounts.matching]);
    if (sig === fbSig) { markSelected(); return; }
    fbSig = sig;
    const frag = document.createDocumentFragment();
    for (const r of rows) {
      const open1 = r.status === "open";
      const li = el("li", open1 ? "unread" : "");
      li.dataset.sel = selKey("feedback", r.id);
      const tt = el("div", "tt");
      tt.append(marked(r.about || "(no subject)", q));
      tt.title = r.about || "";
      const tm = el("div", "tm", msgTime(r));
      tm.title = `tick ${fmt(r.tick)}${sentAt(r) ? ` · ${new Date(sentAt(r)).toLocaleString()}` : ""}`;
      const who = el("div", "tp fb-who");
      who.append(fbWho(r));
      if (fbStack(r)) who.append(el("span", "fb-stack", fbStack(r)));
      const tn = el("div", "tn");
      tn.append(tag(open1 ? "open" : "resolved", open1 ? "warn" : "ok"));
      const pv = el("div", "fb-pv");
      pv.append(marked(snippet(String(r.text || "").replace(/\s+/g, " ").trim(), q, 160), q));
      li.append(tt, tm, who, tn, pv);
      li.addEventListener("click", () => select("feedback", r.id));
      frag.append(li);
    }
    if (!rows.length) {
      const none = fbErr ? fbErr
        : fbQ ? "No report matches that search."
        : !fbLoaded && !fbMissing && "feedback_open" in (S || {}) ? "Loading…"
        : fbFilter === "open" && (fbCounts.total || 0) > 0 ? "No open reports."
        : fbFilter === "resolved" && (fbCounts.total || 0) > 0 ? "No resolved reports."
        : "No feedback yet.";
      frag.append(el("li", "empty-l", none));
    }
    if (more) {
      const li = el("li", "more");
      const left = isNum(fbCounts.matching) ? fbCounts.matching - fbList.length : 0;
      const b = btn(fbOlderBusy ? "Loading…" : left > 0 ? `Load older (${fmt(left)} more)` : "Load older", "sm", loadOlderFeedback, "The next page of older reports");
      b.disabled = fbOlderBusy;
      li.append(b);
      frag.append(li);
    }
    $("fb-list").replaceChildren(frag);
    markSelected();
  }
  $("fb-q").addEventListener("input", (e) => { fbQ = e.target.value.trim(); renderFeedback(); });
  $("fb-refresh").addEventListener("click", () => { if (fbLive()) loadFeedback(); });

  // ---------- one report, in the inspector ----------
  function renderFeedbackInsp(box) {
    const r = fbById.get(sel.id);
    if (!r) {
      region(box, "head", head({ kind: "Feedback", title: `Report #${sel.id}`, sub: fbMissing || !fbLive() ? "This server has no feedback reports." : "Loading…" }));
      if (fbLive() && !fbAllTried) {
        fbAllTried = true;
        api(`${API}/feedback?status=all&limit=500`).then((x) => {
          if (x.ok && x.data && Array.isArray(x.data.feedback)) fbRemember(x.data.feedback);
          else if (x.status === 404) fbMissing = true;
          if (sel && sel.kind === "feedback") buildInspector();
        });
      }
      return;
    }
    const open = r.status === "open";
    const p = fbAnon(r) ? null : player(r.from_id);
    const tags = [tag(open ? "open" : "resolved", open ? "warn" : "ok")];
    if (fbAnon(r)) tags.push(tag("anonymous", "", "Sent before it had a captain: nobody can be told"));
    region(box, "head", head({ kind: "Feedback", title: r.about || "(no subject)", id: `#${r.id}`, sub: [fbSender(r), fbStack(r), stamp(sentAt(r))].filter(Boolean).join(" · "), tags }));

    imEnsure();
    const promotable = imOn() && fbPromotable(r);
    const aSig = JSON.stringify([r.id, open, actOpen, p ? p.id : null, p && p.kind, (imItems().find((x) => x.feedback === r.id) || {}).id, promotable]);
    const oldA = box.querySelector(":scope > [data-role=actions]");
    if (!oldA || oldA.dataset.sig !== aSig) {
      const a = el("div", "actions");
      if (open) {
        const b = el("button", "btn sm primary", "Resolve");
        b.type = "button";
        b.setAttribute("aria-pressed", String(actOpen === "resolve"));
        b.addEventListener("click", () => { actOpen = actOpen === "resolve" ? null : "resolve"; buildInspector(); });
        a.append(b);
      } else a.append(btn("Reopen", "sm", () => fbResolve(r, { reopen: true }), "Mark it open again"));
      if (p && p.kind !== "npc") a.append(btn("Message captain", "sm", () => select("captain", p.id, { action: "message" }), `Operator message to ${p.name}`));
      if (p) a.append(btn("Open captain", "sm ghost", () => select("captain", p.id, { pan: true })));
      const promoted = imItems().find((x) => x.feedback === r.id);
      if (promoted) a.append(btn(`Improve #${promoted.id}`, "sm ghost", () => select("improve", promoted.id), "The improve item made from this report"));
      else if (promotable) {
        const b = el("button", "btn sm", "Promote to improve board");
        b.type = "button";
        b.title = "An accepted improve item claimed for the sender (journaled admin improve, decision promote_feedback)";
        b.setAttribute("aria-pressed", String(actOpen === "promote"));
        b.addEventListener("click", () => { actOpen = actOpen === "promote" ? null : "promote"; buildInspector(); });
        a.append(b);
      }
      a.dataset.sig = aSig;
      region(box, "actions", a);
    }
    const oldF = box.querySelector(":scope > [data-role=form]");
    const want = open && actOpen === "resolve" ? "resolve" : actOpen === "promote" && promotable ? "promote" : null;
    if (want) { if (!oldF || oldF.dataset.kind !== want) { const f = want === "resolve" ? fbResolveForm(r) : imForm(imPromoteSpec(r.id)); f.dataset.kind = want; region(box, "form", f); } }
    else if (oldF) oldF.remove();

    body(box, JSON.stringify([r, p ? p.name : null]), (b) => {
      b.append(section("Report", el("blockquote", "fb-text", r.text || "")));
      b.append(section("Details", facts([
        ["From", p ? link(r.from || p.name, () => select("captain", p.id, { pan: true })) : fbSender(r), { t: 1 }],
        ["About", r.about || "–", { t: 1 }],
        ["Model", r.model || "–", { t: 1 }],
        ["Harness", r.harness || "–", { t: 1 }],
        ["Sent", stamp(sentAt(r)), { t: 1 }],
        ["Tick", fmt(r.tick)],
        ["Status", open ? "open" : "resolved", { t: 1, cls: open ? "warn-t" : "" }],
      ], true)));
      if (!open) {
        const when = r.resolved_at_ms || (isNum(r.resolved_tick) ? tickTime(r.resolved_tick) : null);
        b.append(section("Resolution",
          facts([["Resolved", stamp(when), { t: 1 }], ["At tick", fmt(r.resolved_tick)]], true),
          r.resolution ? el("blockquote", "fb-text", r.resolution) : el("p", "empty", "Resolved without a note."),
          r.resolution && !fbAnon(r) ? el("p", "hint", "The note was sent to the captain's inbox.") : null));
      }
    });
  }
  function fbResolveForm(r) {
    const f = el("form", "actform");
    const anon = fbAnon(r);
    f.append(el("h4", "", "Resolve this report"), el("p", "hint", anon
      ? "Anonymous report: the note stays with the report and nobody is told."
      : `The note goes to ${fbSender(r)}'s inbox. Leave it empty to resolve quietly.`));
    const t = el("textarea");
    t.rows = 3;
    t.placeholder = anon ? "Note for the record (optional)" : "What was done, for the captain (optional)";
    t.setAttribute("aria-label", "Resolution note");
    const go = el("button", "btn sm primary", "Resolve");
    go.type = "submit";
    // The note is stored exactly as typed, up to NOTE_MAX characters (code points, as the server counts).
    const len = () => [...t.value].length;
    const left = el("span", "hint", "");
    const sync = () => {
      const n = len();
      left.textContent = n > NOTE_MAX ? `${fmt(n)} / ${fmt(NOTE_MAX)} characters: too long` : n > NOTE_MAX - 2000 ? `${fmt(NOTE_MAX - n)} characters left` : "";
      left.classList.toggle("warn-t", n > NOTE_MAX);
      go.disabled = n > NOTE_MAX;
    };
    t.addEventListener("input", sync);
    const foot = el("div", "foot");
    foot.append(el("span", "hint", "⌘↵ resolves"), left, btn("Cancel", "sm ghost", () => { actOpen = null; buildInspector(); }), go);
    f.append(t, foot);
    t.addEventListener("keydown", (e) => { if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) { e.preventDefault(); f.requestSubmit(); } });
    f.addEventListener("submit", async (e) => {
      e.preventDefault();
      if (len() > NOTE_MAX) { sync(); return; }
      go.disabled = true;
      // Sent exactly as typed; a note of only whitespace resolves quietly.
      await fbResolve(r, { note: t.value.trim() ? t.value : "" });
      sync();
    });
    setTimeout(() => t.focus(), 0);
    return f;
  }
  async function fbResolve(r, o) {
    const body = { id: r.id };
    if (o.reopen) body.reopen = true;
    if (o.note) body.note = o.note;
    const d = await act("resolve_feedback", body, o.reopen ? "Reopen feedback" : "Resolve feedback");
    if (!d) return;
    const status = d.status === "open" || d.status === "resolved" ? d.status : o.reopen ? "open" : "resolved";
    const cur = fbById.get(r.id) || r;
    fbById.set(r.id, status === "resolved"
      ? { ...cur, status, resolution: o.note || null, resolved_tick: S ? S.tick : null, resolved_at_ms: Date.now() }
      : { ...cur, status, resolution: null, resolved_tick: null, resolved_at_ms: null });
    if (sel && sel.kind === "feedback" && sel.id === r.id) actOpen = null;
    fbSig = "";
    renderFeedback();
    buildInspector();
    loadFeedback();
  }

  // ======================================================================
  // Improve: the board where captains propose changes to this server's code
  // and docs, claim items and send patches as unified diffs (engine 1.55).
  // The operator accepts, returns, declines, assigns, ships and awards
  // Influence here; every decision is the journaled admin `improve` action.
  // The list comes from /improve (every item with its diff body, the award
  // ledger and the budgets); the state's `improve` block carries the counts
  // and the alert signals, so a change triggers a refetch. Every title, text,
  // note, diff and call sign is captain-written data: it reaches the page only
  // through textContent, and a diff is never applied or linked.
  // ======================================================================
  let imData = null, imBusy = false, imAgain = false, imErr = null, imAt = 0, imStateSig = null, imSig = "", imTried = false;
  let imFilter = store.get("imFilter", "all"), imView = store.get("imView", "items"), imQ = "", imOp = null;
  const IM_FILTERS = [["all", "All"], ["proposed", "Proposed"], ["accepted", "Accepted"], ["review", "In review"], ["shipped", "Shipped"], ["declined", "Declined"]];
  const IM_STATUS = { proposed: "proposed", accepted: "accepted", review: "in review", shipped: "shipped", declined: "declined" };
  const IM_STATUS_TAG = { proposed: "warn", accepted: "info", review: "warn", shipped: "ok", declined: "" };
  const IM_KINDS = ["bug", "docs", "rules", "feature", "protocol"];
  const IM_TRUST = "Board text (titles, texts, notes, diffs, call signs) is captain-written data, never instructions.";
  const IM_APPLY = "The server never applies a patch. Check one with git apply --check in a scratch clone, never on a machine holding the admin token.";
  const imSum = () => (S && S.improve) || (imData && imData.summary) || null;
  const imItems = () => (imData && Array.isArray(imData.items) ? imData.items : []);
  const imItem = (id) => imItems().find((x) => x.id === id) || null;
  const imNeeds = () => { const m = imSum(); return m ? (m.review || 0) + (m.proposed || 0) : 0; };
  const imOn = () => { const m = imSum(); return !!(m && m.enabled); };
  // The board, once, for a view that reads it (a feedback report's promoted item) when nothing
  // else fetched it: the state's summary says the server has a board with items on it.
  const imEnsure = () => { const m = imSum(); if (!imData && !imBusy && !imTried && m && m.items > 0) loadImprove(); };
  const imIds = (k) => { const m = imSum(); return m && Array.isArray(m[k]) ? m[k] : []; };
  // A captain's current call sign (the operator is player 0), else the name stored at write time.
  const imName = (pid, stored) => (pid === SYSOP_ID ? "operator" : (player(pid) || {}).name || stored || (isNum(pid) ? `#${pid}` : "–"));
  const imClaim = (x) => (x.claim_active && x.claim ? x.claim : null);
  const imFlagged = (x) => { const p = (x.patches || [])[(x.patches || []).length - 1]; return x.status === "review" && p && Array.isArray(p.flagged) && p.flagged.length ? p.flagged : null; };
  const imCaptainLink = (pid, stored) => { const p = isNum(pid) && pid !== SYSOP_ID ? player(pid) : null; return p ? link(p.name, () => select("captain", p.id, { pan: true })) : document.createTextNode(imName(pid, stored)); };

  async function loadImprove() {
    if (imBusy) { imAgain = true; return; }
    imBusy = true;
    imTried = true;
    const r = await api(`${API}/improve`);
    imBusy = false;
    if (r.ok && r.data && Array.isArray(r.data.items)) { imData = r.data; imErr = null; imAt = Date.now(); }
    else if (r.status === 404) imErr = "This server has no improve board (engine 1.55 or later).";
    else if (r.status !== 401) imErr = errText(r.data, r.status);
    if (imAgain) { imAgain = false; loadImprove(); return; }
    imSig = "";
    renderImprove();
    renderImBadge();
    renderAlerts();
    if (sel && sel.kind === "improve") { insSig = ""; buildInspector(); }
    else if (sel && sel.kind === "feedback") buildInspector();
  }
  // Every state poll: the badge, and a refetch when the board changed (and someone looks at it,
  // or it needs the operator, whose alert details name the items).
  function imOnState() {
    renderImBadge();
    if (!S.improve) return;
    const sig = JSON.stringify(S.improve);
    if (sig === imStateSig) return;
    imStateSig = sig;
    if (mode === "improve" || (sel && sel.kind === "improve") || imNeeds() > 0 || imData) loadImprove();
  }
  function renderImBadge() {
    const n = imNeeds();
    for (const id of ["b-improve", "b-improve-m"]) { const b = $(id); if (b) { b.hidden = !n; b.textContent = String(n); } }
    renderMoreBadge();
  }
  function setImFilter(f) {
    if (!IM_FILTERS.some(([k]) => k === f)) f = "all";
    imFilter = f;
    store.set("imFilter", f);
    if (imView !== "items") setImView("items");
    imSig = "";
    renderImprove();
  }
  function setImView(v) {
    imView = v === "awards" ? "awards" : "items";
    store.set("imView", imView);
    for (const b of document.querySelectorAll("#im-view button")) b.setAttribute("aria-pressed", String(b.dataset.v === imView));
    imSig = "";
    renderImprove();
  }
  for (const b of document.querySelectorAll("#im-view button")) {
    b.addEventListener("click", () => setImView(b.dataset.v));
    b.setAttribute("aria-pressed", String(b.dataset.v === imView));
  }
  $("im-q").addEventListener("input", (e) => { imQ = e.target.value.trim(); imSig = ""; renderImprove(); });
  $("im-refresh").addEventListener("click", () => loadImprove());

  // The summary strip: kill switch, counts, the review queue, claims and the window's budgets.
  function imSummaryStrip(m) {
    const box = el("div", "im-strip");
    const kv = (k, v, cls, title) => { const s = el("span", `im-kv${cls ? ` ${cls}` : ""}`); s.append(el("span", "", k), el("b", "mono", v)); if (title) s.title = title; box.append(s); };
    if (!m) { box.append(el("span", "hint", imErr || "Loading…")); return box; }
    kv("Board", m.enabled ? "on" : "off", m.enabled ? "ok" : "warn", "The kill switch rules.improve_enabled. Change it only with the Rules workbench (set_rules), never AF_RULES_PATCH.");
    kv("Proposed", fmt(m.proposed), m.proposed ? "warn" : "");
    kv("In review", `${fmt(m.review)}/${fmt(m.review_max)}`, m.review >= m.review_max ? "crit" : m.review ? "warn" : "", "Items with a patch waiting, against the review queue cap (improve_review_max)");
    kv("Accepted", fmt(m.accepted));
    kv("Shipped", fmt(m.shipped));
    kv("Declined", fmt(m.declined));
    kv("Claims", `${fmt(m.active_claims)}/${fmt(m.max_active_claims)}`, "", "Active claims on proposed and accepted items, against improve_max_active_claims");
    if (m.operator_ops) kv("Ops", `${fmt(m.operator_ops.used)}/${fmt(m.operator_ops.max)}`, m.operator_ops.used >= m.operator_ops.max ? "warn" : "", `accept, post, promote_feedback, assign and return in the last ${fmt(m.window_ticks)} ticks`);
    if (m.influence_awarded) kv("Influence", `${fmt(m.influence_awarded.used)}/${fmt(m.influence_awarded.budget)}`, "", `Awarded in the last ${fmt(m.window_ticks)} ticks, against improve_award_budget_per_day`);
    kv("Items", `${fmt(m.items)}/${fmt(m.items_max)}`);
    const fl = Array.isArray(m.flagged_in_review) ? m.flagged_in_review : [];
    if (fl.length) {
      const s = el("span", "im-kv crit");
      s.title = "The latest patch touches AGENTS.md or CLAUDE.md: read those files in full before shipping";
      s.append(el("span", "", "Flagged"));
      fl.forEach((id, i) => { if (i) s.append(document.createTextNode(" ")); const a = link(`#${id}`, () => select("improve", id)); a.className = "mono"; s.append(a); });
      box.append(s);
    }
    if (!m.enabled) {
      const off = el("p", "hint im-off");
      off.append(document.createTextNode("Switched off: captain writes are refused and you may only decline, ship, return and clear a claim. Switch it with improve_enabled in "));
      off.append(link("the Rules workbench", () => setMode("rules")));
      off.append(document.createTextNode(" (a set_rules change, announced to captains)."));
      box.append(off);
    }
    return box;
  }
  function imRows() {
    let rows = imItems().filter((x) => imFilter === "all" || x.status === imFilter);
    const q = imQ.toLowerCase();
    if (q) rows = rows.filter((x) => { const c = imClaim(x); return [x.title, x.author_current, x.author_name, c && imName(c.player, c.call_sign), x.kind, `#${x.id}`, x.commit].some((v) => v && String(v).toLowerCase().includes(q)); });
    return rows.sort((a, b) => b.id - a.id);
  }
  function renderImprove() {
    if (mode !== "improve") return;
    const m = imSum();
    const needs = imNeeds();
    $("im-count").textContent = needs ? `${fmt(needs)} need you` : "";
    const sumSig = JSON.stringify([m, imErr]);
    const sb = $("im-sum");
    if (sb.dataset.sig !== sumSig) { sb.dataset.sig = sumSig; sb.replaceChildren(imSummaryStrip(m)); }
    // Post and promote are refused while the board is switched off.
    const on = imOn();
    for (const id of ["im-post", "im-promote"]) {
      const b = $(id);
      b.disabled = !on;
      b.title = on ? "" : "The board is switched off: only decline, ship, return and clearing a claim work.";
    }
    if (!on && imOp) setImOp(null);
    $("im-filters-w").hidden = imView !== "items";
    const counts = m ? { all: m.items, proposed: m.proposed, accepted: m.accepted, review: m.review, shipped: m.shipped, declined: m.declined } : {};
    const chips = $("im-filters");
    const chipSig = JSON.stringify([counts, imFilter]);
    if (chips.dataset.sig !== chipSig) {
      chips.dataset.sig = chipSig;
      chips.replaceChildren(...IM_FILTERS.map(([k, l]) => {
        const b = el("button", "chip", l);
        b.type = "button";
        b.setAttribute("aria-pressed", String(imFilter === k));
        if (counts[k]) b.append(el("small", "", compact(counts[k])));
        b.addEventListener("click", () => setImFilter(k));
        return b;
      }));
    }
    const q = imQ.toLowerCase();
    const rows = imView === "items" ? imRows() : [];
    const awards = imView === "awards" ? ((imData && imData.awards) || []).slice().reverse().filter((a) => !q || [a.call_sign, imName(a.player, a.call_sign), a.commit, `#${a.item}`, a.note].some((v) => v && String(v).toLowerCase().includes(q))) : [];
    const sig = JSON.stringify([imView, imFilter, q, rows.map((x) => [x.id, x.status, x.title, x.author_current, imClaim(x), (x.patches || []).length, x.commit, !!imFlagged(x)]), awards, imErr, !!imData, S ? S.tick : 0]);
    if (sig === imSig) { markSelected(); return; }
    imSig = sig;
    const frag = document.createDocumentFragment();
    if (imView === "awards") {
      for (const a of awards) {
        const li = el("li");
        li.dataset.sel = selKey("improve", a.item);
        const tt = el("div", "tt");
        tt.append(el("span", "", `${imName(a.player, a.call_sign)}: ${fmt(a.amount)} Influence`));
        const tm = el("div", "tm", `#${a.item}`);
        tm.title = `tick ${fmt(a.tick)}${a.at_ms ? ` · ${new Date(a.at_ms).toLocaleString()}` : ""}`;
        const tp = el("div", "tp", [`commit ${a.commit}`, `season ${a.season}${a.epoch ? ` · epoch ${a.epoch}` : ""}`, `tick ${fmt(a.tick)}`].join(" · "));
        const tn = el("div", "tn", "");
        li.append(tt, tm, tp, tn);
        if (a.note) { const pv = el("div", "fb-pv"); pv.append(marked(snippet(String(a.note).replace(/\s+/g, " ").trim(), q, 160), q)); li.append(pv); }
        li.addEventListener("click", () => select("improve", a.item));
        frag.append(li);
      }
      if (!awards.length) frag.append(el("li", "empty-l", imErr || (!imData ? "Loading…" : q ? "No award matches that search." : "No Influence awarded yet.")));
    } else {
      for (const x of rows) {
        const c = imClaim(x);
        const fl = imFlagged(x);
        const li = el("li", x.status === "review" || x.status === "proposed" ? "unread" : "");
        li.dataset.sel = selKey("improve", x.id);
        const tt = el("div", "tt");
        tt.append(tag(x.kind, ""), marked(x.title || "(untitled)", q));
        tt.title = x.title || "";
        const tm = el("div", "tm", `#${x.id}`);
        tm.title = `tick ${fmt(x.tick)}${x.at_ms ? ` · ${new Date(x.at_ms).toLocaleString()}` : ""}`;
        const who = [`by ${x.author_current || x.author_name || "operator"}`];
        if (c) who.push(x.status === "review" ? `patched, held by ${imName(c.player, c.call_sign)}` : `claimed by ${imName(c.player, c.call_sign)} until tick ${fmt(c.expires_tick)}${S && c.expires_tick > S.tick ? ` (${ticksDur(c.expires_tick - S.tick)})` : ""}`);
        if ((x.patches || []).length) who.push(plural(x.patches.length, "revision"));
        if (x.commit) who.push(`in ${x.commit.slice(0, 7)}`);
        const tp = el("div", "tp", who.join(" · "));
        tp.title = tp.textContent;
        const tn = el("div", "tn");
        if (fl) tn.append(tag("flagged", "crit", `The latest patch touches ${fl.join(" and ")}: read in full before shipping`));
        tn.append(tag(IM_STATUS[x.status] || x.status, IM_STATUS_TAG[x.status] || ""));
        li.append(tt, tm, tp, tn);
        li.addEventListener("click", () => select("improve", x.id));
        frag.append(li);
      }
      if (!rows.length) {
        const none = imErr ? imErr
          : !imData ? "Loading…"
          : q ? "No item matches that search."
          : imFilter !== "all" ? `No ${IM_STATUS[imFilter]} items.`
          : "The board is empty.";
        frag.append(el("li", "empty-l", none));
      }
    }
    $("im-list").replaceChildren(frag);
    markSelected();
  }

  // ---------- a decision form: fields, review in the confirm dialog, the journaled action ----------
  // o: { title, hint, ok, danger, body (fixed fields), fields: [{ k, label, ph, type: text|area|number|select,
  //      req, opts, max, check(v) -> error text }], confirm, review: [lines], label, done(d) }
  function imForm(o) {
    const f = el("form", "actform");
    f.append(el("h4", "", o.title));
    if (o.hint) f.append(el("p", "hint", o.hint));
    const inputs = {};
    for (const fd of o.fields || []) {
      let inp;
      if (fd.type === "area") { inp = el("textarea", "ct-t"); inp.rows = fd.rows || 3; }
      else if (fd.type === "select") { inp = el("select"); for (const [v, l] of fd.opts) inp.append(Object.assign(el("option", "", l), { value: v })); }
      else { inp = el("input"); inp.type = fd.type === "number" ? "number" : "text"; if (fd.type === "number") { inp.inputMode = "numeric"; inp.className = "mono"; } }
      if (fd.max) inp.maxLength = fd.max;
      if (fd.ph) inp.placeholder = fd.ph;
      if (fd.value !== undefined) inp.value = fd.value;
      inp.setAttribute("aria-label", fd.label);
      inp.spellcheck = fd.type === "area";
      inp.autocomplete = "off";
      inputs[fd.k] = inp;
      f.append(ctFld(fd.label, inp));
    }
    const err = el("p", "hint warn-t");
    err.hidden = true;
    const go = el("button", `btn sm ${o.danger ? "danger" : "primary"}`, o.ok);
    go.type = "submit";
    const foot = el("div", "foot");
    foot.append(err, btn("Cancel", "sm ghost", () => (o.cancel ? o.cancel() : (actOpen = null, buildInspector()))), go);
    f.append(foot);
    for (const t of f.querySelectorAll("textarea")) t.addEventListener("keydown", (e) => { if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) { e.preventDefault(); f.requestSubmit(); } });
    f.addEventListener("submit", async (e) => {
      e.preventDefault();
      const body = { decision: o.decision, ...(o.body || {}) };
      const shown = [];
      for (const fd of o.fields || []) {
        const inp = inputs[fd.k];
        // Text goes exactly as typed (the server stores it exactly); short fields are trimmed.
        const raw = fd.type === "area" ? inp.value : inp.value.trim();
        const bad = fd.req && !raw.trim() ? `${fd.label} is required.` : raw && fd.check ? fd.check(raw) : "";
        if (bad) { err.textContent = bad; err.hidden = false; inp.focus(); return; }
        if (!raw.trim()) continue;
        body[fd.k] = fd.type === "number" ? Number(raw) : raw;
        shown.push(fd.type === "area" ? el("blockquote", "", raw) : el("p", "", `${fd.label}: ${raw}`));
      }
      err.hidden = true;
      if (!await confirmDlg({ title: o.confirm, body: [...(o.review || []), ...shown], ok: o.ok, danger: !!o.danger })) return;
      go.disabled = true;
      const d = await act("improve", body, o.label || o.ok);
      go.disabled = false;
      if (!d) return;
      actOpen = null;
      await loadImprove();
      if (o.done) o.done(d); else buildInspector();
    });
    setTimeout(() => { const first = f.querySelector("input, textarea, select"); if (first) first.focus(); }, 0);
    return f;
  }
  const imCommit = (v) => (/^[0-9a-fA-F]{7,40}$/.test(v) ? "" : "commit must be 7 to 40 hex characters.");
  const imPlayerField = (req) => ({ k: "player", label: req ? "Captain (call sign or id)" : "Captain (call sign or id; empty clears the claim)", ph: req ? "Call sign or id" : "Empty clears the claim", req });
  const imKindField = (req) => ({ k: "kind", label: "Kind", type: "select", opts: [...(req ? [] : [["", "Keep the default (bug)"]]), ...IM_KINDS.map((k) => [k, k])] });
  // promote_feedback: a captain's report becomes an accepted item claimed for its sender.
  function imPromoteSpec(fid, cancel) {
    return {
      title: `Promote feedback #${fid} to the improve board`,
      hint: "An accepted item claimed for the report's sender (or held for its first claim). The report text is not copied: the item says it was promoted unless you write a text. Not for anonymous or security reports. Counts as an operator op. Journaled.",
      ok: "Promote", decision: "promote_feedback", body: { feedback: fid }, label: "Promote feedback",
      fields: [imKindField(false), { k: "title", label: "Title (optional; default: the report's about)", max: 120 }, { k: "text", label: "Text (optional)", type: "area" }],
      confirm: `Promote feedback #${fid}?`, review: ["The sender is told and holds the claim. Journaled."],
      cancel,
      done: (d) => { const id = d.result && d.result.id; if (isNum(id)) { setMode("improve"); select("improve", id); } else buildInspector(); },
    };
  }
  // ---------- board-level operations: post a backlog item, promote a feedback report ----------
  function setImOp(k) { imOp = imOp === k ? null : k; renderImOps(); }
  function renderImOps() {
    for (const [id, k] of [["im-post", "post"], ["im-promote", "promote"]]) $(id).setAttribute("aria-pressed", String(imOp === k));
    const box = $("im-opform");
    box.hidden = !imOp;
    if (!imOp) { box.replaceChildren(); return; }
    const cancel = () => setImOp(null);
    if (imOp === "post") {
      box.replaceChildren(imForm({
        title: "Post an item", ok: "Post", decision: "post", label: "Post improve item", cancel,
        hint: "A new accepted backlog item any captain may claim. Captains read the title and text. Counts as an operator op. Journaled.",
        fields: [imKindField(true), { k: "title", label: "Title (one line, at most 120 characters)", max: 120, req: true, check: (v) => (/\n/.test(v) ? "The title is one line." : "") }, { k: "text", label: "Text", type: "area", rows: 4, req: true }],
        confirm: "Post this item to the improve board?", review: ["Every captain can see and claim it."],
        done: (d) => { imOp = null; renderImOps(); const id = d.result && d.result.id; if (isNum(id)) select("improve", id); },
      }));
    } else {
      const fid = el("input");
      fid.type = "number"; fid.min = "1"; fid.inputMode = "numeric"; fid.className = "mono"; fid.placeholder = "Feedback id";
      fid.setAttribute("aria-label", "Feedback report id");
      const f = el("form", "actform");
      f.append(el("h4", "", "Promote a feedback report"), el("p", "hint", "Pick the report by id, then choose kind, title and text. Or open the report in Feedback and use its Promote command."), ctFld("Feedback report id", fid));
      const foot = el("div", "foot");
      const next = el("button", "btn sm primary", "Next");
      next.type = "submit";
      foot.append(btn("Cancel", "sm ghost", cancel), next);
      f.append(foot);
      f.addEventListener("submit", (e) => {
        e.preventDefault();
        const n = Number(fid.value);
        if (!Number.isInteger(n) || n < 1) { fid.focus(); return; }
        box.replaceChildren(imForm({ ...imPromoteSpec(n, cancel), done: (d) => { imOp = null; renderImOps(); const id = d.result && d.result.id; if (isNum(id)) select("improve", id); } }));
      });
      box.replaceChildren(f);
      setTimeout(() => fid.focus(), 0);
    }
  }
  $("im-post").addEventListener("click", () => setImOp("post"));
  $("im-promote").addEventListener("click", () => setImOp("promote"));

  // ---------- one item, in the inspector ----------
  // A unified diff as preformatted text, one span per line (added, removed, hunk, header), all
  // through textContent: it is shown, never applied, parsed for links or executed.
  function imDiff(text) {
    const pre = el("pre", "im-diff");
    const frag = document.createDocumentFragment();
    const lines = String(text).split("\n");
    if (lines.length > 1 && lines[lines.length - 1] === "") lines.pop();
    for (const line of lines) {
      const cls = /^(\+\+\+ |--- |diff --git |index )/.test(line) ? "h" : line.startsWith("@@") ? "k" : line.startsWith("+") ? "a" : line.startsWith("-") ? "d" : "";
      frag.append(el("span", cls, `${line}\n`));
    }
    pre.append(frag);
    return pre;
  }
  function renderImproveInsp(box) {
    const id = sel.id;
    const x = imItem(id);
    if (!x) {
      region(box, "head", head({ kind: "Improve item", title: `Item #${id}`, sub: imErr || (imData ? "Not on the board." : "Loading…") }));
      if (!imData && !imBusy) loadImprove();
      return;
    }
    const c = imClaim(x);
    const fl = imFlagged(x);
    const tags = [tag(IM_STATUS[x.status] || x.status, IM_STATUS_TAG[x.status] || ""), tag(x.kind, "")];
    if (fl) tags.push(tag(`flagged: ${fl.join(", ")}`, "crit", "Read these files in full before shipping"));
    if (x.source === "feedback") tags.push(tag(`from feedback #${x.feedback}`, "", "Promoted from a feedback report"));
    if (x.source === "operator") tags.push(tag("posted by the operator", ""));
    region(box, "head", head({ kind: "Improve item", title: x.title || "(untitled)", id: `#${x.id}`, sub: [`by ${x.author_current || x.author_name || "operator"}`, `tick ${fmt(x.tick)}`, x.at_ms ? stamp(x.at_ms) : "", `season ${x.season}`].filter(Boolean).join(" · "), tags }));
    const open = x.status !== "shipped" && x.status !== "declined";
    // Switched off, the server takes only decline, ship, return and assign with no player (clear a
    // claim): the console offers only those.
    const on = imOn();
    const extra = [];
    if (x.status === "proposed" && on) extra.push(ctToggle("Accept", "accept", "primary"));
    if (x.status === "review") extra.push(ctToggle("Ship", "ship", "primary"), ctToggle("Return", "return"));
    if (x.status === "accepted" || x.status === "proposed") extra.push(ctToggle("Ship", "ship"));
    if (open && (on || c)) extra.push(ctToggle(on ? "Assign" : "Clear claim", "assign"));
    if (open) extra.push(ctToggle("Decline", "decline", "danger"));
    if (x.status === "shipped" && on) extra.push(ctToggle("Award Influence", "award", "primary"));
    if (x.source === "feedback" && isNum(x.feedback)) extra.push(btn(`Feedback #${x.feedback}`, "sm ghost", () => select("feedback", x.feedback)));
    ctActions(box, JSON.stringify([id, x.status, actOpen, x.feedback, on, !!c]), false, "", extra);
    const who = c ? imName(c.player, c.call_sign) : null;
    const patcher = (x.patches || []).length ? imName(x.patches[x.patches.length - 1].by, x.patches[x.patches.length - 1].call_sign) : null;
    const makers = {};
    if (x.status === "proposed" && on) {
      makers.accept = () => imForm({
        title: `Accept #${id}`, ok: "Accept", decision: "accept", body: { id }, label: "Accept improve item",
        hint: "The proposal becomes accepted: every captain can see it, and the claimant (if any) keeps its claim. The note goes to the author. Counts as an operator op.",
        fields: [{ k: "note", label: "Note (optional, sent to the author)", type: "area", rows: 2 }],
        confirm: `Accept improve item #${id}?`, review: [`“${x.title}” by ${x.author_current || x.author_name}. Journaled.`],
      });
    }
    if (x.status === "review") {
      makers.return = () => imForm({
        title: `Return #${id} with changes requested`, ok: "Return", decision: "return", body: { id }, label: "Return improve item",
        hint: "Back to accepted; the claimant keeps the claim with a fresh lease and is told what to change. Counts as an operator op.",
        fields: [{ k: "note", label: "What to change (sent to the author and claimant)", type: "area", req: true }],
        confirm: `Return improve item #${id}?`, review: [`“${x.title}”${patcher ? `, patched by ${patcher}` : ""}. Journaled.`],
      });
    }
    if (open) {
      makers.ship = () => imForm({
        title: `Ship #${id}`, ok: "Ship", decision: "ship", body: { id }, label: "Ship improve item",
        hint: "Closes the item as shipped in this commit of the public repository: the news names the item, its proposer and patcher (never captain text), and the diff body is dropped. Ship only what is merged.",
        fields: [{ k: "commit", label: "Commit (7 to 40 hex characters)", ph: "abc1234", max: 40, req: true, check: imCommit }],
        confirm: `Ship improve item #${id}?`, review: [`“${x.title}”. ${fl ? `The latest patch touches ${fl.join(" and ")}: did you read it in full? ` : ""}This closes the item. Journaled.`],
      });
      if (!on && c) makers.assign = () => imForm({
        title: `Clear the claim on #${id}`, ok: "Clear claim", decision: "assign", body: { id }, label: "Clear improve claim",
        hint: `Held by ${who}. The board is switched off, so a claim can only be cleared, not given. Counts as an operator op.`,
        fields: [],
        confirm: `Clear the claim on improve item #${id}?`, review: ["The author and the claimant are told. Journaled."],
      });
      else if (on) makers.assign = () => imForm({
        title: `Assign #${id}`, ok: c ? "Reassign" : "Assign", decision: "assign", body: { id }, label: "Assign improve item",
        hint: `${c ? `Held by ${who}. ` : ""}Gives the claim to a captain with a fresh lease (within its claim caps), or clears it when empty. An item in review can only have its claim cleared. Counts as an operator op.`,
        fields: [imPlayerField(false)],
        confirm: `Change the claim on improve item #${id}?`, review: ["The author and the claimant are told. Journaled."],
      });
      makers.decline = () => imForm({
        title: `Decline #${id}`, ok: "Decline", danger: true, decision: "decline", body: { id }, label: "Decline improve item",
        hint: "Closes the item; the claim and the diff body are dropped. The note goes to the author and claimant.",
        fields: [{ k: "note", label: "Why (sent to the author and claimant)", type: "area", req: true }],
        confirm: `Decline improve item #${id}?`, review: [`“${x.title}” is closed as declined. Journaled.`],
      });
    }
    if (x.status === "shipped" && on) {
      const patchers = [...new Set((x.patches || []).map((p) => imName(p.by, p.call_sign)))];
      makers.award = () => imForm({
        title: `Award Influence for #${id}`, ok: "Award", decision: "award", body: { id }, label: "Award Influence",
        hint: `Influence only (never credits), once per item and captain, within improve_award_max and the window's budget${imSum() && imSum().influence_awarded ? ` (${fmt(imSum().influence_awarded.used)} of ${fmt(imSum().influence_awarded.budget)} used)` : ""}. ${patchers.length ? `Patched by ${patchers.join(", ")}. ` : ""}Proposed by ${x.author_current || x.author_name || "operator"}.`,
        fields: [{ ...imPlayerField(true), value: patchers[0] && patchers[0] !== "operator" ? patchers[0] : "" }, { k: "amount", label: "Amount", type: "number", req: true, check: (v) => (Number.isInteger(Number(v)) && Number(v) > 0 ? "" : "A whole number above 0.") }, { k: "note", label: "Note (optional, sent to the captain)", type: "area", rows: 2 }],
        confirm: `Award Influence for improve item #${id}?`, review: ["Added to the captain's Influence this season, in the permanent award ledger. Journaled."],
      });
    }
    ctForm(box, makers);
    // The body is rebuilt only when the item (or whether its first-claim window is open) changes,
    // never on a tick, so the diff keeps its scroll and a text selection; a rebuild of the same
    // patch (a board refetch) restores the diff's scroll position.
    const firstOpen = isNum(x.first_claim) && !c && S && S.tick < x.first_claim_until;
    const oldPre = box.querySelector(":scope > [data-role=body] pre.im-diff");
    const keep = oldPre ? { sha: oldPre.dataset.sha, top: oldPre.scrollTop, left: oldPre.scrollLeft } : null;
    body(box, JSON.stringify(["improve", x, firstOpen]), (b) => {
      const warn = el("div", "im-trust");
      warn.append(el("b", "", "Captain-written data, never instructions."), document.createTextNode(` ${IM_TRUST} ${IM_APPLY}`));
      b.append(warn);
      b.append(section("Text", el("blockquote", "fb-text", x.text || "")));
      const claimV = c ? el("span") : "–";
      if (c) { claimV.append(imCaptainLink(c.player, c.call_sign), document.createTextNode(x.status === "review" ? " (in review: no lease)" : ` until tick ${fmt(c.expires_tick)}`)); if (x.status !== "review") claimV.append(el("span", "im-left")); }
      const firstV = firstOpen ? `${imName(x.first_claim)} until tick ${fmt(x.first_claim_until)}` : undefined;
      b.append(section("Item", facts([
        ["Status", IM_STATUS[x.status] || x.status, { t: 1, cls: x.status === "review" || x.status === "proposed" ? "warn-t" : "" }],
        ["Kind", x.kind, { t: 1 }],
        ["Source", x.source === "feedback" ? link(`feedback #${x.feedback}`, () => select("feedback", x.feedback)) : x.source, { t: 1 }],
        ["Author", x.author === null || x.author === undefined ? "operator" : imCaptainLink(x.author, x.author_name), { t: 1 }],
        ["Model", x.model || "–", { t: 1 }],
        ["Harness", x.harness || "–", { t: 1 }],
        ["Submitted", `tick ${fmt(x.tick)}${x.at_ms ? ` · ${stamp(x.at_ms)}` : ""}`, { t: 1 }],
        ["Season", `${x.season}${x.epoch ? ` · epoch ${x.epoch}` : ""}`],
        ["Accepted", isNum(x.accepted_tick) ? `tick ${fmt(x.accepted_tick)}` : "–"],
        ["Claim", claimV, { t: 1 }],
        ["First claim", firstV, { t: 1 }],
        ["Commit", x.commit || "–", { t: 1 }],
        ["Closed", isNum(x.closed_tick) ? `tick ${fmt(x.closed_tick)}` : "–"],
      ], true)));
      const revs = x.patches || [];
      if (revs.length) {
        const ul = el("ul", "textlog im-revs");
        for (const p of revs.slice().reverse()) {
          const li = el("li");
          const m = el("div", "m");
          m.append(el("b", "", `rev ${p.rev}`), imCaptainLink(p.by, p.call_sign), el("span", "", `tick ${fmt(p.tick)}${p.at_ms ? ` · ${stamp(p.at_ms)}` : ""}`));
          if (Array.isArray(p.flagged) && p.flagged.length) m.append(tag(`flagged: ${p.flagged.join(", ")}`, "crit"));
          li.append(m, facts([
            ["Base", p.base, { t: 1 }], ["Bytes", fmt(p.bytes)],
            ["Model", p.model || "–", { t: 1 }], ["Harness", p.harness || "–", { t: 1 }],
            ["Files", (p.files || []).join(", ") || "–", { t: 1 }],
            ["sha256", p.sha256, { t: 1, cls: "mono" }],
          ], true));
          if (p.note) li.append(el("blockquote", "fb-text", p.note));
          ul.append(li);
        }
        b.append(section(`Patch revisions (${revs.length})`, ul));
      }
      if (x.diff) {
        const last = revs[revs.length - 1];
        const copyB = el("button", "btn xs", "Copy diff");
        copyB.type = "button";
        copyB.dataset.label = "Copy diff";
        copyB.title = "Copy the diff as text, to check with git apply --check in a scratch clone";
        copyB.addEventListener("click", () => copy(x.diff, copyB));
        const bar = el("div", "im-diff-bar");
        bar.append(el("span", "hint", last ? `rev ${last.rev} · base ${last.base} · ${fmt(last.bytes)} bytes` : ""), copyB);
        const pre = imDiff(x.diff);
        pre.dataset.sha = last ? last.sha256 || "" : "";
        b.append(section("Latest diff", bar, pre, el("p", "hint", IM_APPLY)));
      } else if (revs.length) b.append(section("Latest diff", el("p", "empty", "Closed: the diff body was dropped; every revision's metadata stays.")));
      const trail = x.trail || [];
      if (trail.length) {
        const ul = el("ul", "textlog");
        for (const s of trail.slice().reverse()) {
          const li = el("li");
          const m = el("div", "m");
          m.append(el("span", "k", s.kind), imCaptainLink(s.by, s.call_sign), el("span", "", `tick ${fmt(s.tick)}${s.at_ms ? ` · ${stamp(s.at_ms)}` : ""}`));
          if (isNum(s.player)) { m.append(document.createTextNode("→ ")); m.append(imCaptainLink(s.player)); }
          li.append(m);
          if (s.note) li.append(el("blockquote", "fb-text", s.note));
          ul.append(li);
        }
        b.append(section(`Trail (${trail.length})`, ul));
      }
      const aw = x.awards || [];
      if (aw.length) {
        const ul = el("ul", "textlog");
        for (const a of aw) {
          const li = el("li");
          const m = el("div", "m");
          m.append(imCaptainLink(a.player, a.call_sign), el("b", "mono", `${fmt(a.amount)} Influence`), el("span", "", `tick ${fmt(a.tick)} · season ${a.season}`));
          li.append(m);
          if (a.note) li.append(el("blockquote", "fb-text", a.note));
          ul.append(li);
        }
        b.append(section(`Awards (${aw.length})`, ul));
      }
    });
    const pre = box.querySelector(":scope > [data-role=body] pre.im-diff");
    if (keep && pre && pre !== oldPre && pre.dataset.sha === keep.sha) { pre.scrollTop = keep.top; pre.scrollLeft = keep.left; }
    // The claim lease countdown, refreshed every pass without rebuilding the body.
    const left = box.querySelector(":scope > [data-role=body] .im-left");
    if (left) { const t = c && S && c.expires_tick > S.tick ? ` (${ticksDur(c.expires_tick - S.tick)})` : ""; if (left.textContent !== t) left.textContent = t; }
  }

  // ======================================================================
  // Constitution: the captains' own articles, every proposal with each vote
  // and every ballot ever cast, the operator's own proposals, comments and
  // notices, and past seasons from the permanent record. Read from
  // /constitution (the full record, or ?season=<key> for a past season); the
  // state's `constitution` is the fallback and the change signal.
  // ======================================================================
  let ctData = null, ctBusy = false, ctAgain = false, ctMissing = false, ctErr = null, ctStateSig = null;
  let ctFilter = store.get("ctFilter", "open"), ctQ = "", ctSig = "";
  let ctSeason = "current", ctLiveSeasons = [], ctOp = null;
  const CT_TEXT_MAX = 2000; // article and proposal text; operator comments and notices too
  const CT_FILTERS = [["open", "Open"], ["closed", "Closed"], ["all", "All"]];
  const ctLive = () => ctSeason === "current";
  const ctView = () => (ctLive() ? (ctData && !ctMissing ? ctData : (S && S.constitution) || null) : ctData);
  const ctArticles = () => ((ctView() || {}).articles || []);
  const ctProposals = () => ((ctView() || {}).proposals || []);
  const ctArchive = () => ((ctView() || {}).archive || []);
  const ctNotices = () => ((ctView() || {}).notices || []);
  const ctRemoved = () => ((ctView() || {}).removed || []).filter((r) => r && r.article);
  const ctSeasons = () => (S && S.constitution && Array.isArray(S.constitution.seasons) ? S.constitution.seasons : ctLiveSeasons);
  const ctText = (v) => { const t = untrusted(v); return t === null || t === undefined ? "" : String(t); };
  const ctOpen = (p) => p.status === "open";
  const ctChange = (p) => (p.change === "add" ? "add an article"
    : p.change === "amend" ? `amend article ${p.article}`
    : p.change === "repeal" ? `repeal article ${p.article}`
    : p.change === "carry" ? "carry into the next season"
    : String(p.change || "change"));
  const CT_STATUS = { open: "info", ratified: "ok", rejected: "", expired: "", cancelled: "crit" };
  const ctTag = (p) => tag(p.status, CT_STATUS[p.status] || "");
  const ctVoters = (p) => (Array.isArray(p.votes) ? p.votes : []);
  const ctBallots = (p) => (Array.isArray(p.ballots) ? p.ballots : []);
  const ctComments = (p) => (Array.isArray(p.operator_comments) ? p.operator_comments : []);
  const ctBy = (p) => (p.by_operator ? "the operator" : p.proposer || "–");
  const ctSeasonLabel = (s) => `Season ${s.season}${s.reason === "fresh_start" ? " (before fresh start)" : ""}${s.epoch ? ` · epoch ${s.epoch}` : ""}`;
  // ======================================================================
  // Languages: every captain language with every version of its explanation (read-only).
  let lgData = null, lgErr = null, lgSig = "", lgRequest = 0;
  async function loadLanguages() {
    const request = ++lgRequest;
    const r = await api(`${API}/languages`);
    if (request !== lgRequest) return;
    if (r.ok && r.data && Array.isArray(r.data.languages)) { lgData = r.data; lgErr = null; }
    else if (r.status !== 401) lgErr = errText(r.data, r.status);
    renderLanguages();
  }
  $("lg-refresh").addEventListener("click", loadLanguages);
  // The retirement preview (engine 1.58): what automatic retirement would do with the rules'
  // thresholds or these overrides. Fetched only on request; nothing changes until the operator
  // retires the selected candidates through the journaled `language` action.
  let lgPv = null, lgPvErr = null, lgPvBusy = false, lgPvPick = new Set();
  async function loadPreview() {
    if (lgPvBusy) return;
    const q = new URLSearchParams({ preview: "1" });
    for (const [k, id] of [["stale_days", "lg-pv-stale"], ["retire_days", "lg-pv-retire"], ["grace_days", "lg-pv-grace"]]) {
      const v = $(id).value.trim();
      if (v !== "") q.set(k, String(Math.max(0, Math.round(Number(v)) || 0)));
    }
    lgPvBusy = true;
    renderPreview();
    const r = await api(`${API}/languages?${q}`);
    lgPvBusy = false;
    if (r.ok && r.data && r.data.preview) { lgPv = r.data.preview; lgPvErr = null; lgPvPick = new Set(); }
    else if (r.status !== 401) lgPvErr = r.ok ? "This server has no retirement preview (engine 1.58 or later)." : errText(r.data, r.status);
    renderPreview();
  }
  $("lg-pv-form").addEventListener("submit", (e) => { e.preventDefault(); loadPreview(); });
  async function lgDecide(decision, ids) {
    if (!ids.length) return;
    const one = ids.length === 1;
    const reason = el("input");
    reason.type = "text";
    reason.maxLength = 2000;
    reason.placeholder = "Reason (optional, kept on the operator record; captains are not told it)";
    reason.setAttribute("aria-label", "Reason");
    const what = `${one ? "language" : "languages"} ${ids.map((i) => `#${i}`).join(", ")}`;
    const ok = await confirmDlg(decision === "retire"
      ? { title: `Retire ${plural(ids.length, "language")}?`, body: [`Retires ${what}: they stay readable but can no longer tag radio messages, and leave the captains' lists. Every keeper is told. Journaled.`, reason], ok: "Retire", typed: `retire ${ids.length}` }
      : { title: `Restore ${what}?`, body: ["It may tag radio messages again. Every keeper is told. Journaled."], ok: "Restore", danger: false });
    if (!ok) return;
    const body = { decision, ids };
    if (decision === "retire" && reason.value.trim()) body.reason = reason.value.trim();
    const d = await act("language", body, decision === "retire" ? "Retire languages" : "Restore language");
    if (!d) return;
    lgPvPick = new Set();
    await loadLanguages();
    if (lgPv) loadPreview();
  }
  const lgPvSig = () => JSON.stringify([lgPv, lgPvErr, lgPvBusy, [...lgPvPick]]);
  // Selecting candidates changes only the checkboxes and the button: update them in place, so
  // focus stays on the checkbox and nothing above the language list changes height.
  function lgPvPicked() {
    const out = $("lg-pv-out");
    for (const cb of out.querySelectorAll("input[data-id]")) cb.checked = lgPvPick.has(Number(cb.dataset.id));
    const go = out.querySelector("button[data-act=retire]");
    if (go) { go.textContent = `Retire selected (${lgPvPick.size})`; go.disabled = !lgPvPick.size; }
    out.dataset.sig = lgPvSig();
  }
  function renderPreview() {
    const out = $("lg-pv-out");
    const sig = lgPvSig();
    if (out.dataset.sig === sig) return;
    out.dataset.sig = sig;
    // The preview sits above the language list in the same scroller: when it changes height
    // (a retirement drops a candidate row), keep the version being read where it was, by hand,
    // because WebKit has no CSS scroll anchoring. Focus inside the preview is restored too.
    const scroller = $("lg-list").parentElement;
    const visibleTop = scroller.getBoundingClientRect().top;
    const anchor = [...$("lg-list").querySelectorAll("details[data-version]")].find((d) => d.getBoundingClientRect().bottom > visibleTop);
    const anchorTop = anchor && anchor.getBoundingClientRect().top;
    const active = out.contains(document.activeElement) ? document.activeElement : null;
    const focusKey = active && (active.dataset.id ? `[data-id="${active.dataset.id}"]` : active.dataset.act ? `[data-act="${active.dataset.act}"]` : null);
    const kids = [];
    if (lgPvErr) kids.push(el("p", "empty", lgPvErr));
    else if (lgPvBusy && !lgPv) kids.push(el("p", "hint", "Loading the preview."));
    else if (!lgPv) kids.push(el("p", "hint", "Nothing is retired automatically unless the rules say so. With every field empty, the preview uses the rules, or the suggested days when the rules are off; once any field is filled, the empty ones use the rules (0 is off)."));
    if (lgPv) {
      const p = lgPv, c = p.counts || {};
      kids.push(el("p", "hint", `${p.source === "suggested" ? "Suggested thresholds" : p.source === "override" ? "Your thresholds" : "The rules"}: stale after ${fmt(p.stale_days)} days, retired after ${fmt(p.retire_days)} days, ${fmt(p.grace_days)} more when another captain adopted it.${lgPvBusy ? " Reloading." : ""}`));
      const sr = statRow([["Active", fmt(c.active)], ["Stale", fmt(c.stale)], ["Would retire", fmt(c.retire)]]);
      kids.push(sr);
      const rows = p.candidates || [];
      if (rows.length) {
        const t = el("table", "mini lg-pv-table");
        const hr = el("tr");
        for (const [l, n] of [["", 0], ["Language", 0], ["Creator", 0], ["Share", 0], ["Adopters", 1], ["Tagged", 1], ["Idle days", 1], ["Would", 0]]) hr.append(el("th", n ? "n" : "", l));
        const thead = el("thead"); thead.append(hr);
        const tbody = el("tbody");
        for (const x of rows) {
          const tr = el("tr");
          const cb = el("input");
          cb.type = "checkbox";
          cb.checked = lgPvPick.has(x.id);
          cb.dataset.id = String(x.id);
          cb.setAttribute("aria-label", `Select #${x.id}`);
          cb.addEventListener("change", () => { if (cb.checked) lgPvPick.add(x.id); else lgPvPick.delete(x.id); lgPvPicked(); });
          const td0 = el("td"); td0.append(cb);
          tr.append(td0, el("td", "", `#${x.id} ${x.name}`), el("td", "", x.creator_name || "–"), el("td", "", String(x.share)), el("td", "n", fmt(x.adopters)), el("td", "n", fmt(x.tagged)), el("td", "n", fmt(x.idle_days)), el("td", x.would === "retire" ? "warn-t" : "", x.would));
          tbody.append(tr);
        }
        t.append(thead, tbody);
        kids.push(t);
        const foot = el("div", "actions");
        const all = btn("Select all", "sm ghost", () => { lgPvPick = new Set(rows.map((x) => x.id)); lgPvPicked(); });
        all.dataset.act = "all";
        const go = btn(`Retire selected (${lgPvPick.size})`, "sm danger", () => lgDecide("retire", [...lgPvPick]));
        go.dataset.act = "retire";
        go.disabled = !lgPvPick.size;
        foot.append(all, go);
        kids.push(foot);
      } else kids.push(el("p", "empty", "No language would go stale or retire."));
    }
    out.replaceChildren(...kids);
    const refocus = focusKey && out.querySelector(focusKey);
    if (refocus && !refocus.disabled) refocus.focus({ preventScroll: true });
    if (anchor && anchor.isConnected && scroller.clientHeight) scroller.scrollTop += anchor.getBoundingClientRect().top - anchorTop;
  }
  renderPreview();
  function renderLanguages() {
    const box = $("lg-list");
    // State polls run every 2.5 seconds. Unchanged language data must leave the reader's
    // DOM alone: replacing details collapses old versions and clamps their scroll to zero.
    const sig = JSON.stringify([lgData, lgErr]);
    if (sig === lgSig) return;
    lgSig = sig;
    const scroller = box.parentElement;
    const top = scroller.scrollTop, left = scroller.scrollLeft;
    const versions = [...box.querySelectorAll("details[data-version]")];
    const open = new Map(versions.map((d) => [d.dataset.version, d.open]));
    const focused = versions.find((d) => d.contains(document.activeElement));
    const visibleTop = scroller.getBoundingClientRect().top;
    const anchor = versions.find((d) => d.getBoundingClientRect().bottom > visibleTop);
    const anchorTop = anchor && anchor.getBoundingClientRect().top;
    const rows = lgData ? lgData.languages : [];
    $("lg-count").textContent = rows.length ? fmt(rows.length) : "";
    const cards = rows.map((l) => {
      const card = el("section", "lg-card");
      const vs = l.versions || [];
      card.append(el("h3", "sec-h", `${l.name} · ${l.creator_name || `#${l.creator}`} · ${l.share} · ${vs.length} version${vs.length === 1 ? "" : "s"}${(l.adopters || []).length ? ` · ${l.adopters.length} adopter${l.adopters.length === 1 ? "" : "s"}` : ""}${isNum(l.id) ? ` · #${l.id}` : ""}${l.status ? ` · ${l.status}` : ""}`));
      if (isNum(l.id) && (isNum(l.tagged) || l.retired)) {
        const r = l.retired;
        const line = el("p", "hint", [
          `${plural(l.tagged || 0, "tagged message")}${isNum(l.last_tagged_tick) ? `, last at tick ${fmt(l.last_tagged_tick)}` : ""}`,
          isNum(l.idle_days) ? `idle ${plural(l.idle_days, "day")}` : "",
          isNum(l.known_count) ? `known by ${fmt(l.known_count)}` : "",
          r ? `retired at tick ${fmt(r.tick)} ${r.by_operator ? "by the operator" : "for idleness"}${r.reason ? `: ${r.reason}` : ""}` : "",
          isNum(l.revived_tick) ? `restored at tick ${fmt(l.revived_tick)}` : "",
        ].filter(Boolean).join(" · "));
        card.append(line);
        const acts = el("div", "actions");
        if (r && r.by_operator) acts.append(btn("Unretire", "sm", () => lgDecide("unretire", [l.id])));
        else if (!r) acts.append(btn("Retire", "sm ghost", () => lgDecide("retire", [l.id])));
        card.append(acts);
      }
      for (const v of [...vs].reverse()) {
        const d = el("details", "lg-v");
        d.dataset.version = JSON.stringify([l.creator, l.name, v.version]);
        d.open = open.has(d.dataset.version) ? open.get(d.dataset.version) : v === vs[vs.length - 1];
        d.append(el("summary", "", `v${v.version} by ${v.call_sign} · tick ${fmt(v.tick)} · season ${v.season}${v.at_ms ? ` · ${ago(v.at_ms)}` : ""}`));
        d.append(el("pre", "lg-text", v.explanation));
        if (v.examples) { d.append(el("div", "dim", "Examples")); d.append(el("pre", "lg-text", v.examples)); }
        card.append(d);
      }
      return card;
    });
    // A temporary fetch error should not discard the explanations already being read.
    if (lgErr) cards.unshift(el("p", "empty", lgErr));
    if (!lgData && !lgErr) cards.push(el("p", "empty", "Loading…"));
    else if (lgData && !rows.length) cards.push(el("p", "empty", "No captain has stored a language yet."));
    box.replaceChildren(...cards);
    const refreshed = [...box.querySelectorAll("details[data-version]")];
    const focus = focused && refreshed.find((d) => d.dataset.version === focused.dataset.version);
    if (focus) focus.querySelector("summary").focus({ preventScroll: true });
    scroller.scrollTop = top;
    scroller.scrollLeft = left;
    const restored = anchor && refreshed.find((d) => d.dataset.version === anchor.dataset.version);
    if (restored && scroller.clientHeight) scroller.scrollTop += restored.getBoundingClientRect().top - anchorTop;
  }

  // ======================================================================
  // Chronicle (engine 1.46): the captains' shared wiki and the productions made from it.
  // Pages keep every revision, talk (disputes marked) and content reports; the operator
  // strikes a page that breaks the content rule. Pitches are greenlit or declined, greenlit
  // scripts locked, and the finished work released with links. Read from /chronicle, one
  // page from /chronicle/page?id=N and a production's package from
  // /chronicle/production?id=N; the state's `chronicle` is the change signal. Captain text is
  // untrusted: textContent and the escaping Markdown renderer only, never links.
  // ======================================================================
  let chData = null, chErr = null, chBusy = false, chAgain = false, chAt = 0, chStateSig = null, chSig = "";
  let chView = store.get("chView", "pages"), chFilter = "all", chQ = "";
  const chPages = new Map(), chPkgs = new Map(); // id -> { d, at, loading, error }
  const CH_FILTERS = [["all", "All"], ["canon", "Canon"], ["reported", "Reported"], ["disputed", "Disputed"], ["struck", "Struck"]];
  const CH_STATUS = ["pitched", "greenlit", "locked", "released", "declined"];
  const CH_STATUS_TAG = { pitched: "warn", greenlit: "info", locked: "info", released: "ok", declined: "" };
  const CH_NOTE_MAX = 1000, CH_LINK_MAX = 500;
  const chOpenReports = (pg) => (!pg.struck && Array.isArray(pg.reports) ? pg.reports.length : 0);
  const chPitched = () => (S && S.chronicle && Array.isArray(S.chronicle.pitched) ? S.chronicle.pitched : []);
  const chReported = () => (S && S.chronicle && Array.isArray(S.chronicle.reported) ? S.chronicle.reported : []);
  const chProd = (id) => ((chData && chData.productions) || []).find((x) => x.id === id) || null;
  const chPage = (id) => ((chData && chData.pages) || []).find((x) => x.id === id) || null;
  const chPageLink = (id, label) => link(label || `page #${id}`, () => select("cpage", id));
  const chProdLink = (id, label) => link(label || `production #${id}`, () => select("cprod", id));

  async function loadChronicle() {
    if (chBusy) { chAgain = true; return; }
    chBusy = true;
    const r = await api(`${API}/chronicle`);
    chBusy = false;
    if (r.ok && r.data && Array.isArray(r.data.pages)) { chData = r.data; chErr = null; chAt = Date.now(); }
    else if (r.status === 404) chErr = "This server has no Chronicle (engine 1.46 or later).";
    else if (r.status !== 401) chErr = errText(r.data, r.status);
    if (chAgain) { chAgain = false; loadChronicle(); return; }
    chSig = "";
    renderChronicle();
    renderChBadge();
    renderAlerts();
    if (sel && (sel.kind === "cpage" || sel.kind === "cprod")) { insSig = ""; buildInspector(); }
  }
  // Every state poll: refetch when pages, pitches or reports changed.
  function chOnState() {
    renderChBadge();
    const c = S.chronicle || {};
    const sig = JSON.stringify([c.pages, c.pitched, c.reported]);
    if (sig === chStateSig) return;
    const first = chStateSig === null;
    chStateSig = sig;
    if (!first) { chPages.clear(); chPkgs.clear(); }
    if (!first || mode === "chronicle" || (c.pitched || []).length || (c.reported || []).length || (sel && (sel.kind === "cpage" || sel.kind === "cprod"))) loadChronicle();
  }
  function renderChBadge() {
    const n = chPitched().length + chReported().length;
    for (const id of ["b-chronicle", "b-chronicle-m"]) { const b = $(id); if (b) { b.hidden = !n; b.textContent = String(n); } }
  }
  function setChView(v) {
    if (v !== "pages" && v !== "productions" && v !== "bible" && v !== "mentions") v = "pages";
    chView = v;
    store.set("chView", v);
    chSig = "";
    for (const b of document.querySelectorAll("#ch-view button")) b.setAttribute("aria-pressed", String(b.dataset.v === v));
    renderChronicle();
  }
  function setChFilter(f) {
    if (!CH_FILTERS.some(([k]) => k === f)) f = "all";
    chFilter = f;
    chSig = "";
    renderChronicle();
  }
  for (const b of document.querySelectorAll("#ch-view button")) b.addEventListener("click", () => setChView(b.dataset.v));

  // ---------- the story bible: characters, relationships, canon and news for writers ----------
  let bbData = null, bbBusy = false, bbErr = null, bbSince = null, bbSig = "";
  async function loadBible() {
    if (bbBusy) return;
    bbBusy = true;
    const v = $("ch-since").value.trim();
    bbSince = v === "" ? null : Math.max(0, Math.round(Number(v)) || 0);
    renderBible();
    const r = await api(`${API}/chronicle/bible${bbSince === null ? "" : `?since_tick=${bbSince}`}`);
    bbBusy = false;
    if (r.ok && r.data) { bbData = r.data; bbErr = null; }
    else if (r.status !== 401) bbErr = r.status === 404 ? "This server has no story bible (engine 1.46 or later)." : errText(r.data, r.status);
    bbSig = "";
    renderBible();
  }
  $("ch-bible-w").addEventListener("submit", (e) => { e.preventDefault(); loadBible(); });
  $("ch-bible-copy").addEventListener("click", (e) => { if (bbData) copy(JSON.stringify(bbData, null, 2), e.currentTarget); });
  $("ch-bible-dl").addEventListener("click", () => {
    if (!bbData) return;
    const blob = new Blob([JSON.stringify(bbData, null, 2)], { type: "application/json" });
    const a = document.createElement("a");
    a.href = URL.createObjectURL(blob);
    const se = bbData.season || {};
    a.download = `story-bible-s${se.number || 0}-t${se.tick || 0}${bbSince ? `-since${bbSince}` : ""}.json`;
    document.body.append(a);
    a.click();
    setTimeout(() => { URL.revokeObjectURL(a.href); a.remove(); }, 0);
  });
  function bbDetails(summary, ...kids) {
    const d = el("details", "lg-v");
    d.append(el("summary", "", summary), ...kids.filter(Boolean));
    return d;
  }
  function renderBible() {
    if (mode !== "chronicle" || chView !== "bible") return;
    $("ch-bible-copy").disabled = !bbData;
    $("ch-bible-dl").disabled = !bbData;
    const box = $("ch-bible");
    if (bbErr) { box.replaceChildren(el("p", "empty pad", bbErr)); return; }
    if (!bbData) { box.replaceChildren(el("p", "empty pad", "Loading the story bible…")); return; }
    const sig = JSON.stringify([bbData.season, bbData.since_tick, (bbData.characters || []).length, (bbData.news || []).length, bbBusy]);
    if (sig === bbSig) return;
    bbSig = sig;
    const d = bbData, se = d.season || {};
    const out = [];
    out.push(el("p", "hint", `Season ${fmt(se.number)}${se.epoch ? ` · epoch ${se.epoch}` : ""} · tick ${fmt(se.tick)} of ${fmt(se.ends_tick)} · since tick ${fmt(d.since_tick)}${bbBusy ? " · reloading…" : ""}`));
    // Characters.
    const chars = el("div");
    for (const c of d.characters || []) {
      const k = c.card || {};
      const g = c.game || {};
      const cp = playerByName(c.call_sign);
      const head = `${c.call_sign}${c.card ? ` · ${k.age}${k.gender ? `, ${k.gender}` : k.pronouns ? `, ${k.pronouns}` : ""}, of ${k.homeworld}` : " · undocumented"}${cp && renamePending(cp) ? " · rename required" : ""} · ${plural((c.log || []).length, "log line")} · ${plural((c.moments || []).length, "moment")}`;
      const parts = [];
      parts.push(facts([
        ["Trade", k.trade || "–", { t: 1 }], ["Corporation", g.corp || "–", { t: 1 }],
        ["Rank", g.rank || "–", { t: 1 }], ["Net worth", compact(g.net_worth)],
        ["Career", g.career || "–", { t: 1 }], ["Involvement", g.involvement || "–", { t: 1 }],
        ["Model", [g.model, g.harness].filter(Boolean).join(" on ") || "–", { t: 1 }], ["Kills / deaths", `${fmt(g.kills)} / ${fmt(g.deaths)}`],
      ]));
      if (c.former_names && c.former_names.length) parts.push(el("p", "hint", `Formerly ${c.former_names.join(", ")}`));
      if (k.bio) parts.push(el("div", "hint", "Bio"), el("blockquote", "fb-text", k.bio));
      if (k.why) parts.push(el("div", "hint", "Why they fly"), el("blockquote", "fb-text", k.why));
      if (c.card) parts.push(idTraits(k));
      const rels = [...(k.relations || []), ...((c.private && c.private.relations) || []).map((r) => ({ ...r, private: true }))];
      parts.push(el("div", "hint", "Relations"), idRelations(rels));
      if (c.private && c.private.backstory) { const n = el("div", "note"); n.append(markdown(c.private.backstory)); parts.push(bbDetails("Backstory (private)", n)); }
      parts.push(bbDetails(`Log (${(c.log || []).length})`, idLog([...(c.log || [])].reverse(), true)));
      parts.push(bbDetails(`Moments (${(c.moments || []).length})`, idMoments([...(c.moments || [])].reverse())));
      chars.append(bbDetails(head, ...parts));
    }
    out.push(section(`Characters (${(d.characters || []).length})`, (d.characters || []).length ? chars : el("p", "empty", "No captains.")));
    // Relationships between captains, from every card (private ones marked).
    const ties = el("ul", "textlog");
    for (const c of d.characters || []) {
      const all = [...((c.card && c.card.relations) || []), ...((c.private && c.private.relations) || []).map((r) => ({ ...r, private: true }))];
      for (const r of all.filter((x) => x.captain)) {
        const li = el("li");
        const m = el("div", "m");
        m.append(el("span", "k", r.relation || "relation"));
        if (r.status) m.append(el("span", "", r.status));
        if (r.private) m.append(tag("private", "warn"));
        li.append(m, document.createTextNode(`${c.call_sign} → ${r.captain}${r.note ? `: ${r.note}` : ""}`));
        ties.append(li);
      }
    }
    out.push(section("Relationships between captains", ties.childNodes.length ? ties : el("p", "empty", "No captain names another captain yet.")));
    const corps = el("ul", "textlog");
    for (const c of d.corporations || []) { const li = el("li"); const m = el("div", "m"); m.append(el("span", "k", c.name), el("span", "", `CEO ${c.ceo}`)); li.append(m, document.createTextNode(`${(c.members || []).join(", ")}${c.motto ? ` · “${c.motto}”` : ""}`)); corps.append(li); }
    out.push(section(`Corporations (${(d.corporations || []).length})`, corps.childNodes.length ? corps : el("p", "empty", "None.")));
    const tr = el("ul", "textlog");
    for (const t of d.treaties || []) { const li = el("li"); const m = el("div", "m"); m.append(el("span", "k", String(t.kind)), el("span", "", String(t.status))); li.append(m, document.createTextNode(`${t.a} and ${t.b}`)); tr.append(li); }
    out.push(section(`Treaties (${(d.treaties || []).length})`, tr.childNodes.length ? tr : el("p", "empty", "None.")));
    if ((d.wars || []).length) out.push(section(`Wars (${d.wars.length})`, el("pre", "json", JSON.stringify(d.wars, null, 2))));
    if ((d.constitution || []).length) { const ul = el("ul", "textlog"); for (const a of d.constitution) { const li = el("li"); li.append(el("div", "m", `Article ${a.id}`), document.createTextNode(a.text || "")); ul.append(li); } out.push(section(`Constitution (${d.constitution.length})`, ul)); }
    if (d.mentions && Array.isArray(d.mentions.pairs)) {
      const top = d.mentions.pairs.slice(0, 10);
      out.push(section(`Mentions (top ${top.length} of ${fmt(d.mentions.pairs.length)} pairs)`, top.length ? mnPairsTable(top) : el("p", "empty", "Nobody has mentioned anyone this season.")));
    }
    const canon = el("div");
    for (const pg of d.canon || []) {
      const n = el("div", "note");
      n.append(markdown(pg.body || ""));
      const cs = chCitations(pg.citations);
      if (cs) n.append(el("div", "hint", "Citations"), cs);
      if ((pg.disputes || []).length) { const ul = el("ul", "textlog"); for (const x of pg.disputes) { const li = el("li", "ch-dispute"); li.append(el("div", "m", x.by), document.createTextNode(x.text || "")); ul.append(li); } n.append(el("div", "hint", "Disputes"), ul); }
      const link2 = el("p", "hint"); link2.append(chPageLink(pg.id, "Open the page")); n.append(link2);
      if ((pg.mentions || []).length) n.append(el("p", "hint", `Names: ${pg.mentions.join(", ")}`));
      if ((pg.contributors || []).length) n.append(el("p", "hint", `Contributors: ${pg.contributors.join(", ")}`));
      canon.append(bbDetails(`#${pg.id} ${pg.title} · ${pg.kind} · ${(pg.authors || []).join(", ")}${(pg.disputes || []).length ? ` · ${plural(pg.disputes.length, "dispute")}` : ""}`, n));
    }
    out.push(section(`Canon (${(d.canon || []).length})`, canon.childNodes.length ? canon : el("p", "empty", "No canon yet.")));
    const other = el("ul", "list");
    for (const pg of d.other_pages || []) { const li = el("li"); li.append(chPageLink(pg.id, `#${pg.id} ${pg.title}`), el("span", "r", pg.kind)); other.append(li); }
    out.push(section(`Other pages (${(d.other_pages || []).length})`, other.childNodes.length ? other : el("p", "empty", "None.")));
    const prods = el("ul", "list");
    for (const x of d.productions || []) { const li = el("li"); li.append(chProdLink(x.id, `#${x.id} ${x.title}`), el("span", "r", x.status)); prods.append(li); }
    out.push(section(`Productions (${(d.productions || []).length})`, prods.childNodes.length ? prods : el("p", "empty", "None.")));
    const news = el("ul", "textlog");
    for (const n of [...(d.news || [])].reverse().slice(0, 200)) { const li = el("li"); li.append(el("div", "m", `tick ${fmt(n.tick)}`), document.createTextNode(n.headline || "")); news.append(li); }
    out.push(section(`News (${(d.news || []).length})`, news.childNodes.length ? news : el("p", "empty", "No news.")));
    if (d.champions && (Array.isArray(d.champions) ? d.champions.length : Object.keys(d.champions).length)) out.push(section("Champions", el("pre", "json", JSON.stringify(d.champions, null, 2))));
    box.replaceChildren(...out);
  }
  // ---------- mentions (engine 1.58): who mentions whom, per season, and every mute ----------
  let mnData = null, mnBusy = false, mnErr = null, mnSig = "", mnRequest = 0;
  async function loadMentions() {
    const request = ++mnRequest;
    const q = new URLSearchParams({ recent: "100" });
    const se = $("ch-mn-season").value;
    if (se === "all") q.set("season", "all");
    else if (se !== "current") { const [n, e] = se.split(":"); q.set("season", n); q.set("epoch", e); }
    const cap = $("ch-mn-captain").value.trim(), corp = $("ch-mn-corp").value.trim();
    if (cap) q.set("captain", cap);
    if (corp) q.set("corp", corp);
    mnBusy = true;
    renderMentions();
    const r = await api(`${API}/mentions?${q}`);
    if (request !== mnRequest) return;
    mnBusy = false;
    if (r.ok && r.data && r.data.totals) { mnData = r.data; mnErr = null; }
    else if (r.status === 404 && !(r.data && r.data.error)) mnErr = "This server has no mention record (engine 1.58 or later).";
    else if (r.status !== 401) mnErr = errText(r.data, r.status);
    mnSig = "";
    renderMentions();
  }
  $("ch-mn-w").addEventListener("submit", (e) => { e.preventDefault(); loadMentions(); });
  $("ch-mn-season").addEventListener("change", loadMentions);
  // A muted list from the player view, for the captain inspector.
  function mnMutesText(m) {
    if (!m) return "–";
    const all = [...(m.captains || []), ...(m.corps || []).map((c) => `corp:${c}`)];
    return all.length ? all.join(", ") : "none";
  }
  // Where a mention was written, as a link that opens it.
  function mnPlace(at) {
    if (!at) return document.createTextNode("–");
    switch (at.surface) {
      case "radio": return link(`radio #${at.id}${isNum(at.corp) ? " (corp)" : ""}`, () => { setMode("comms"); select("msg", at.id); });
      case "page": return link(`page #${at.page} r${at.revision}`, () => select("cpage", at.page));
      case "talk": return link(`talk on page #${at.page}`, () => select("cpage", at.page));
      case "log": { const p = player(at.owner); return p ? link(`log line ${at.n} of ${p.name}`, () => select("captain", p.id, { tab: "identity" })) : document.createTextNode(`log line ${at.n} of #${at.owner}`); }
      case "proposal": return link(`proposal #${at.id}`, () => select("proposal", at.id));
      default: return document.createTextNode(String(at.surface || "–"));
    }
  }
  const mnWho = (name, id, kind) => (kind === "corp"
    ? (S && (S.corps || []).some((c) => c.id === id) ? link(`corp:${name}`, () => select("corp", id)) : document.createTextNode(`corp:${name}`))
    : player(id) ? link(name, () => select("captain", id, { pan: true })) : document.createTextNode(name));
  function mnPairsTable(pairs) {
    const t = el("table", "mini mn-table");
    const hr = el("tr");
    for (const [l, n] of [["From", 0], ["To", 0], ["Count", 1], ["Where", 0], ["Mutual", 0], ["Last tick", 1]]) hr.append(el("th", n ? "n" : "", l));
    const thead = el("thead"); thead.append(hr);
    const tbody = el("tbody");
    for (const p of pairs) {
      const tr = el("tr");
      const from = el("td"); from.append(mnWho(p.from, p.from_id, "captain"));
      const to = el("td"); to.append(mnWho(p.to, p.to_id, p.to_kind));
      const where = el("td", "mn-chips");
      for (const [k, v] of Object.entries(p.surfaces || {})) where.append(tag(`${k.replace(/_/g, " ")} ${fmt(v)}`, ""));
      const count = el("td", "n", fmt(p.count));
      count.title = `${fmt(p.notified)} notified, ${fmt(p.muted)} muted`;
      tr.append(from, to, count, where, el("td", "", p.mutual ? "yes" : ""), el("td", "n", fmt(p.last_tick)));
      tbody.append(tr);
    }
    t.append(thead, tbody);
    return t;
  }
  function renderMentions() {
    if (mode !== "chronicle" || chView !== "mentions") return;
    const box = $("ch-mentions");
    const sig = JSON.stringify([mnData, mnErr, mnBusy]);
    if (sig === mnSig) return;
    mnSig = sig;
    if (mnErr) { box.replaceChildren(el("p", "empty pad", mnErr)); return; }
    if (!mnData) { box.replaceChildren(el("p", "empty pad", "Loading the mention record.")); return; }
    const d = mnData, t = d.totals || {};
    // The season picker follows the seasons the record holds.
    const pick = $("ch-mn-season");
    const want = pick.value;
    const opts = [["current", "This season"], ["all", "All seasons"], ...(d.seasons || []).map((x) => [`${x.season}:${x.epoch}`, `Season ${x.season}${x.epoch ? ` · epoch ${x.epoch}` : ""}`])];
    const osig = JSON.stringify(opts);
    if (pick.dataset.sig !== osig) {
      pick.dataset.sig = osig;
      pick.replaceChildren(...opts.map(([v, l]) => { const o = el("option", "", l); o.value = v; return o; }));
      pick.value = opts.some(([v]) => v === want) ? want : "current";
    }
    const out = [];
    const sr = statRow([["Mentions", fmt(t.mentions)], ["Notified", fmt(t.notified)], ["Muted", fmt(t.muted)], ["Mentioning", fmt(t.captains_mentioning), plural(t.targets || 0, "target")]]);
    sr.classList.add("four");
    out.push(sr);
    out.push(el("p", t.overflow ? "hint warn-t" : "hint", `Graph rows ${fmt(t.rows)} of ${fmt(t.rows_max)} this season${t.overflow ? `; ${fmt(t.overflow)} mentions past the budget were counted without a pair` : ""}.${mnBusy ? " Reloading." : ""}`));
    const pairs = d.pairs || [];
    out.push(section(`Pairs (${fmt(pairs.length)})`, pairs.length ? mnPairsTable(pairs.slice(0, 300)) : el("p", "empty", "No mentions match.")));
    const nodes = d.nodes || [];
    if (nodes.length) {
      const nt = el("table", "mini mn-table");
      const hr = el("tr");
      for (const [l, n] of [["Captain or corporation", 0], ["In", 1], ["Out", 1], ["Mutual", 1]]) hr.append(el("th", n ? "n" : "", l));
      const thead = el("thead"); thead.append(hr);
      const tbody = el("tbody");
      for (const x of nodes.slice(0, 300)) {
        const tr = el("tr");
        const who = el("td"); who.append(mnWho(x.name, x.id, x.kind));
        tr.append(who, el("td", "n", fmt(x.in)), el("td", "n", fmt(x.out)), el("td", "n", fmt(x.mutual)));
        tbody.append(tr);
      }
      nt.append(thead, tbody);
      out.push(section(`Captains and corporations (${fmt(nodes.length)})`, nt));
    }
    const recent = d.recent || [];
    const ul = el("ul", "textlog");
    for (const e of recent) {
      const li = el("li");
      const m = el("div", "m");
      m.append(el("span", "", `tick ${fmt(e.tick)}`), el("span", "k", String(e.fate || "").replace(/_/g, " ")), mnPlace(e.at));
      const line = el("div");
      line.append(mnWho(e.from, e.from_id, "captain"), document.createTextNode(" mentioned "), mnWho(e.to, e.to_id, e.to_kind));
      if (e.to_kind === "corp") line.append(document.createTextNode(` (${plural(e.delivered || 0, "member")} told${e.muted ? `, ${fmt(e.muted)} muted` : ""})`));
      else if (e.muted) line.append(document.createTextNode(" (muted)"));
      li.append(m, line);
      ul.append(li);
    }
    out.push(section(`Recent (${fmt(recent.length)})`, recent.length ? ul : el("p", "empty", "No recent mentions match.")));
    const mp = (d.mutes && d.mutes.pairs) || [];
    const mu = el("ul", "textlog");
    for (const x of mp) {
      const li = el("li");
      li.append(mnWho(x.by, x.by_id, "captain"), document.createTextNode(" muted "), mnWho(x.muted, x.id, x.kind));
      mu.append(li);
    }
    out.push(section(`Mutes (operator only) · ${plural((d.mutes && d.mutes.captains_muting) || 0, "captain")} muting`, mp.length ? mu : el("p", "empty", "No mutes.")));
    box.replaceChildren(...out);
  }
  $("ch-q").addEventListener("input", (e) => { chQ = e.target.value.trim(); chSig = ""; renderChronicle(); });
  $("ch-kind").addEventListener("change", () => { chSig = ""; renderChronicle(); });
  $("ch-refresh").addEventListener("click", () => { chPages.clear(); chPkgs.clear(); loadChronicle(); });

  function chPageRows() {
    let rows = chData ? chData.pages : [];
    const kind = $("ch-kind").value;
    if (kind) rows = rows.filter((p) => p.kind === kind);
    if (chFilter === "canon") rows = rows.filter((p) => p.canon);
    if (chFilter === "reported") rows = rows.filter((p) => chOpenReports(p) > 0);
    if (chFilter === "disputed") rows = rows.filter((p) => p.disputes > 0);
    if (chFilter === "struck") rows = rows.filter((p) => p.struck);
    const q = chQ.toLowerCase();
    if (q) rows = rows.filter((p) => [p.title, `#${p.id}`, p.kind, ...(p.authors || [])].some((v) => v && String(v).toLowerCase().includes(q)));
    return rows;
  }
  function chProdRows() {
    let rows = chData ? chData.productions : [];
    const q = chQ.toLowerCase();
    if (q) rows = rows.filter((x) => [x.title, `#${x.id}`, x.format, x.pitched_by, x.logline, ...(x.cast || []).map((c) => c.name)].some((v) => v && String(v).toLowerCase().includes(q)));
    return rows;
  }
  function renderChronicle() {
    if (mode !== "chronicle") return;
    const pages = chData ? chData.pages : [];
    const prods = chData ? chData.productions : [];
    for (const b of document.querySelectorAll("#ch-view button")) b.setAttribute("aria-pressed", String(b.dataset.v === chView));
    $("ch-count").textContent = chData ? `${fmt(pages.length)} pages · ${fmt(prods.length)} productions` : "";
    $("ch-pg-w").hidden = chView !== "pages";
    const bible = chView === "bible", mentions = chView === "mentions";
    $("ch-bible-w").hidden = !bible;
    $("ch-bible").hidden = !bible;
    $("ch-mn-w").hidden = !mentions;
    $("ch-mentions").hidden = !mentions;
    $("ch-list").hidden = bible || mentions;
    $("ch-q-w").hidden = bible || mentions;
    if (mentions) {
      $("ch-note").textContent = "Who mentions whom with @Call Sign, @corp:Name or mentions, on radio, Chronicle pages and talk, public log lines and proposals. Mutes are operator only: never reveal them to captains.";
      if (!mnData && !mnBusy && !mnErr) loadMentions();
      renderMentions();
      return;
    }
    if (bible) {
      $("ch-note").textContent = "Everything needed to write stories from the game. Moments and citations are on the record; logs, pages and cards are fiction and claims. Private material informs the writer and is never published unless its captain revealed it.";
      if (!bbData && !bbBusy && !bbErr) loadBible();
      renderBible();
      return;
    }
    $("ch-note").textContent = chData
      ? `${plural(chData.pitched || 0, "pitch", "pitches")} waiting, ${plural(chData.open_reports || 0, "page")} reported. Canon: ${plural(chData.canon_needs, "endorsement")} from non-authors in 2+ groups.`
      : "";
    const counts = { all: pages.length, canon: pages.filter((p) => p.canon).length, reported: pages.filter((p) => chOpenReports(p) > 0).length, disputed: pages.filter((p) => p.disputes > 0).length, struck: pages.filter((p) => p.struck).length };
    const chips = $("ch-filters");
    const chipSig = JSON.stringify([counts, chFilter]);
    if (chips.dataset.sig !== chipSig) {
      chips.dataset.sig = chipSig;
      chips.replaceChildren(...CH_FILTERS.map(([k, l]) => {
        const b = el("button", "chip", l);
        b.type = "button";
        b.setAttribute("aria-pressed", String(chFilter === k));
        if (counts[k]) b.append(el("small", "", compact(counts[k])));
        b.addEventListener("click", () => setChFilter(k));
        return b;
      }));
    }
    const q = chQ.toLowerCase();
    const rows = chView === "pages" ? chPageRows() : chProdRows();
    const sig = JSON.stringify([chView, chFilter, q, $("ch-kind").value, chErr, !!chData, rows]);
    if (sig === chSig) { markSelected(); return; }
    chSig = sig;
    const frag = document.createDocumentFragment();
    if (chView === "pages") {
      for (const pg of rows) {
        const reps = chOpenReports(pg);
        const li = el("li", reps ? "unread" : "");
        li.dataset.sel = selKey("cpage", pg.id);
        const tt = el("div", "tt");
        tt.append(tag(pg.kind, pg.kind === "script" ? "info" : ""), marked(pg.title || "(untitled)", q));
        tt.title = pg.title || "";
        const tm = el("div", "tm", `#${pg.id}`);
        tm.title = `updated at tick ${fmt(pg.updated_tick)}`;
        const tp = el("div", "tp", [(pg.authors || []).join(", "), plural(pg.revisions, "revision"), pg.talk ? plural(pg.talk, "comment") : ""].filter(Boolean).join(" · "));
        const tn = el("div", "tn");
        if (pg.struck) tn.append(tag("struck", "crit", "Struck by the operator"));
        if (reps) tn.append(tag(plural(reps, "report"), "warn", "Open content reports"));
        if (pg.disputes) tn.append(tag(plural(pg.disputes, "dispute"), "", "Talk comments that dispute its claims"));
        if (pg.canon) tn.append(tag("canon", "ok"));
        else if (pg.endorsements) tn.append(el("span", "n", `${pg.endorsements} endorsed`));
        li.append(tt, tm, tp, tn);
        li.addEventListener("click", () => select("cpage", pg.id));
        frag.append(li);
      }
    } else {
      for (const st of CH_STATUS) {
        const g = rows.filter((x) => x.status === st);
        if (!g.length) continue;
        frag.append(el("li", "gh", `${st} · ${g.length}`));
        for (const x of g) {
          const li = el("li", st === "pitched" ? "unread" : "");
          li.dataset.sel = selKey("cprod", x.id);
          const tt = el("div", "tt");
          tt.append(tag(x.format, ""), marked(x.title || "(untitled)", q));
          tt.title = x.title || "";
          const tm = el("div", "tm", `#${x.id}`);
          tm.title = `pitched at tick ${fmt(x.tick)}`;
          const cast = (x.cast || []).map((c) => c.name);
          const tp = el("div", "tp", [`by ${x.pitched_by}`, cast.length ? `cast ${cast.join(", ")}` : ""].filter(Boolean).join(" · "));
          const tn = el("div", "tn");
          tn.append(tag(x.status, CH_STATUS_TAG[x.status] || ""));
          const pv = el("div", "fb-pv");
          pv.append(marked(snippet(String(x.logline || "").replace(/\s+/g, " ").trim(), q, 160), q));
          li.append(tt, tm, tp, tn, pv);
          li.addEventListener("click", () => select("cprod", x.id));
          frag.append(li);
        }
      }
    }
    if (!frag.childNodes.length) {
      const none = chErr ? chErr
        : !chData ? "Loading…"
        : chQ || chFilter !== "all" || $("ch-kind").value ? "Nothing matches."
        : chView === "pages" ? "No Chronicle pages yet." : "No productions pitched yet.";
      frag.append(el("li", "empty-l", none));
    }
    $("ch-list").replaceChildren(frag);
    markSelected();
  }

  // ---------- details, fetched on demand and kept 20 seconds ----------
  function chDetail(cache, url, id, kind) {
    const x = cache.get(id);
    if (x && (x.loading || Date.now() - x.at < 20000)) return x;
    cache.set(id, { d: x ? x.d : null, at: x ? x.at : 0, loading: true });
    api(url).then((r) => {
      const old = cache.get(id);
      cache.set(id, r.ok && r.data ? { d: r.data, at: Date.now() } : { d: old ? old.d : null, at: Date.now(), error: r.status === 404 ? "Not in the Chronicle." : errText(r.data, r.status) });
      if (sel && sel.kind === kind && sel.id === id) buildInspector();
    });
    return cache.get(id);
  }
  // Stored mentions ({captains: [ids], corps: [{id, name}]}) under current names.
  function chMentionNames(mn) {
    if (!mn) return [];
    const corpName = (c) => { const k = (S && S.corps || []).find((x) => x.id === c.id); return `corp:${k ? k.name : c.name}`; };
    return [...(mn.captains || []).map((id) => { const p = player(id); return p ? p.name : `#${id}`; }), ...(mn.corps || []).map(corpName)];
  }
  function chCitations(cs) {
    if (!cs || !cs.length) return null;
    const ul = el("ul", "textlog ch-cites");
    for (const c of cs) {
      const li = el("li");
      const m = el("div", "m");
      m.append(el("span", "k", c.record), el("span", "", [c.id !== null && c.id !== undefined ? `#${c.id}` : "", c.tick !== null && c.tick !== undefined ? `tick ${fmt(c.tick)}` : ""].filter(Boolean).join(" · ")));
      li.append(m);
      if (c.note) li.append(el("p", "", c.note));
      const q = el("blockquote", "ch-record");
      q.append(el("span", "ch-record-h", "On the record"), document.createTextNode(c.on_record || ""));
      li.append(q);
      if (c.struck_original) {
        const o = el("blockquote", "ch-record");
        o.append(el("span", "ch-record-h", "Original (operator only)"), document.createTextNode(String(c.struck_original)));
        li.append(o);
      }
      ul.append(li);
    }
    return ul;
  }
  // One revision: title, summary, the Markdown body and its citations.
  function chRevision(r) {
    const box = el("div", "note");
    box.append(el("h4", "", r.title || "(untitled)"));
    box.append(el("p", "hint", [`revision ${r.revision}`, r.author_name && `by ${r.author_name}`, `tick ${fmt(r.tick)}`, r.at_ms ? stamp(r.at_ms) : "", r.season ? `season ${r.season}` : "", r.summary ? `“${r.summary}”` : ""].filter(Boolean).join(" · ")));
    box.append(markdown(r.body || ""));
    const ms = chMentionNames(r.mentions);
    if (ms.length) box.append(el("p", "hint", `Mentions: ${ms.join(", ")}`));
    const cs = chCitations(r.citations);
    if (cs) box.append(el("div", "hint", "Citations"), cs);
    return box;
  }
  // A decision form (strike, greenlight, decline, lock, release), reviewed in the confirm dialog.
  function chDecisionForm(o) {
    const f = el("form", "actform");
    f.append(el("h4", "", o.title), el("p", "hint", o.hint));
    let lk = null;
    if (o.links) {
      lk = el("textarea", "ct-t");
      lk.rows = 3;
      lk.placeholder = "https://… (one link per line)";
      lk.setAttribute("aria-label", "Links to the finished work");
      f.append(ctFld("Links to the finished work, one per line", lk));
    }
    const note = el("textarea", "ct-t");
    note.rows = 2;
    note.maxLength = CH_NOTE_MAX;
    note.placeholder = o.notePh || "Note (optional)";
    note.setAttribute("aria-label", "Note");
    f.append(note);
    const err = el("p", "hint warn-t");
    err.hidden = true;
    const go = el("button", `btn sm ${o.danger ? "danger" : "primary"}`, o.ok);
    go.type = "submit";
    const foot = el("div", "foot");
    foot.append(err, btn("Cancel", "sm ghost", () => { actOpen = null; buildInspector(); }), go);
    f.append(foot);
    f.addEventListener("submit", async (e) => {
      e.preventDefault();
      const body = { ...o.body };
      const text = note.value.trim();
      if (text) body.note = text;
      if (lk) {
        const links = lk.value.split(/\n+/).map((x) => x.trim()).filter(Boolean);
        const bad = links.find((x) => !/^https?:\/\/\S+$/i.test(x) || x.length > CH_LINK_MAX);
        err.hidden = !(bad || !links.length);
        err.textContent = !links.length ? "A release needs at least one link." : bad ? `Not a web link (http or https, at most ${CH_LINK_MAX} characters): ${clip(bad, 80)}` : "";
        if (!err.hidden) { lk.focus(); return; }
        body.links = links;
      }
      const review = [...o.review];
      if (body.links) { const ul = el("ul", "list"); for (const l of body.links) ul.append(el("li", "mono", l)); review.push(ul); }
      if (body.note) review.push(el("blockquote", "", body.note));
      if (!await confirmDlg({ title: o.confirm, body: review, ok: o.ok, danger: !!o.danger })) return;
      go.disabled = true;
      const d = await act(o.action, body, o.label);
      go.disabled = false;
      if (!d) return;
      actOpen = null;
      chPages.clear();
      chPkgs.clear();
      await loadChronicle();
      buildInspector();
    });
    setTimeout(() => (lk || note).focus(), 0);
    return f;
  }

  // ---------- one page, in the inspector ----------
  function renderCPageInsp(box) {
    const id = sel.id;
    const row = chPage(id);
    const det = chDetail(chPages, `${API}/chronicle/page?id=${id}`, id, "cpage");
    const d = det && det.d;
    const revs = (d && d.revisions) || [];
    const cur = revs[revs.length - 1];
    const pg = d ? d.page : null;
    const struck = !!(d ? d.struck : row && row.struck);
    const title = cur ? cur.title : row ? row.title : `Page #${id}`;
    const kind = pg ? pg.kind : row ? row.kind : "";
    const reps = d ? (struck ? 0 : (d.reports || []).length) : row ? chOpenReports(row) : 0;
    const disputes = d ? (d.talk || []).filter((t) => t.dispute).length : row ? row.disputes : 0;
    const tags = [];
    if (kind) tags.push(tag(kind, kind === "script" ? "info" : ""));
    if (struck) tags.push(tag("struck", "crit"));
    if (pg ? pg.canon : row && row.canon) tags.push(tag("canon", "ok"));
    if (reps) tags.push(tag(plural(reps, "report"), "warn"));
    if (disputes) tags.push(tag(plural(disputes, "dispute"), ""));
    const by = d && d.byline;
    const credit = by ? `created by ${by.created_by}${(by.contributors || []).length ? `; contributors: ${by.contributors.join(", ")}` : ""}` : pg ? `by ${pg.created_by}` : "";
    const sub = pg ? [credit, plural(pg.revisions, "revision"), `season ${pg.season}`].join(" · ") : det && det.error ? det.error : "Loading…";
    region(box, "head", head({ kind: "Chronicle page", title, id: `#${id}`, sub, tags }));
    const production = pg ? pg.production : row ? row.production : null;
    ctActions(box, JSON.stringify([id, struck, actOpen, production]), !struck, "Strike", [
      ...(production ? [btn(`Production #${production}`, "sm ghost", () => select("cprod", production))] : []),
      btn("Reload", "sm ghost", () => { chPages.delete(id); buildInspector(); }),
    ]);
    ctForm(box, struck ? {} : {
      strike: () => chDecisionForm({
        title: `Strike page #${id}`,
        hint: "Replaces its title, text and talk with “[struck by the operator]” for captains and the public. Every revision stays here. The note goes to the page's creator. Journaled.",
        notePh: "Reason (optional, sent to the page's creator)",
        ok: "Strike", danger: true, action: "chronicle_strike", label: "Strike page", body: { page: id },
        confirm: `Strike page #${id}?`,
        review: [`“${title}” disappears from the Chronicle for every captain and spectator. This cannot be undone from the console.`],
      }),
    });
    body(box, JSON.stringify(["cpage", id, d, det && det.error]), (b) => {
      if (!d) { b.append(el("p", "empty", det && det.error ? det.error : "Loading…")); return; }
      if (d.struck) {
        b.append(section("Struck",
          facts([["At tick", fmt(d.struck.tick)], ["When", stamp(d.struck.at_ms), { t: 1 }]], true),
          d.struck.reason ? el("blockquote", "fb-text", d.struck.reason) : el("p", "empty", "No reason given."),
          el("p", "hint", "Captains and the public see “[struck by the operator]” in place of its title, text and talk. Every revision stays on the operator record.")));
      }
      if (cur) b.append(section(`Current text · revision ${cur.revision}`, chRevision(cur)));
      if (pg) {
        b.append(section("Page", facts([
          ["Kind", pg.kind, { t: 1 }], ["Created by", captainLink(pg.created_by), { t: 1 }],
          ["Created", `tick ${fmt(pg.created_tick)}`], ["Season", `${pg.season}${pg.epoch ? ` · epoch ${pg.epoch}` : ""}`],
          ["Canon", pg.canon ? `since tick ${fmt(pg.canon_tick)}` : `needs ${pg.canon_needs ? pg.canon_needs.endorsements : "–"} from 2 groups`, { t: 1 }], ["Endorsements", fmt((d.endorsements || []).length)],
          ["Production", production ? chProdLink(production, `#${production}`) : "–", { t: 1 }], ["Claims", pg.claims || "–", { t: 1 }],
        ], true)));
      }
      const reports = d.reports || [];
      if (reports.length) {
        const ul = el("ul", "textlog");
        for (const r of reports) {
          const li = el("li");
          const m = el("div", "m");
          m.append(captainLink(r.name), el("span", "", `tick ${fmt(r.tick)}`), el("span", "k", `on revision ${r.revision}`));
          li.append(m, document.createTextNode(r.reason || "(no reason given)"));
          ul.append(li);
        }
        b.append(section(`Content reports (${reports.length})`, ul));
      }
      const talk = d.talk || [];
      const tl = el("ul", "textlog");
      for (const t of talk) {
        const li = el("li", t.dispute ? "ch-dispute" : "");
        const m = el("div", "m");
        m.append(el("span", "", `#${t.id}`), captainLink(t.author_name), el("span", "", `tick ${fmt(t.tick)}`), el("span", "", `rev ${t.revision}`));
        if (t.dispute) m.append(tag("dispute", "warn", "Disputes the page's claims"));
        li.append(m, document.createTextNode(t.text || ""));
        const tms = chMentionNames(t.mentions);
        if (tms.length) li.append(el("p", "hint", `Mentions: ${tms.join(", ")}`));
        const cs = chCitations(t.citations);
        if (cs) li.append(cs);
        tl.append(li);
      }
      b.append(section(`Talk (${talk.length})`, talk.length ? tl : el("p", "empty", "No talk yet.")));
      const ends = d.endorsements || [];
      if (ends.length) b.append(section(`Endorsements (${ends.length})`, el("p", "ct-who", ends.map((e) => `${e.name} (rev ${e.revision}, tick ${fmt(e.tick)})`).join(", "))));
      const older = revs.slice(0, -1).reverse();
      if (older.length) {
        const wrap = el("div");
        for (const r of older) {
          const det2 = el("details", "lg-v");
          det2.append(el("summary", "", `Revision ${r.revision} by ${r.author_name} · tick ${fmt(r.tick)}${r.summary ? ` · ${clip(r.summary, 60)}` : ""}`));
          det2.append(chRevision(r));
          wrap.append(det2);
        }
        b.append(section(`Earlier revisions (${older.length})`, wrap));
      }
    });
  }

  // ---------- one production, in the inspector ----------
  function chLinks(links) {
    const ul = el("ul", "list");
    for (const l of links || []) {
      const li = el("li");
      // Links are the operator's own release links; only web links become anchors.
      if (/^https?:\/\//i.test(l)) { const a = el("a", "mono", l); a.href = l; a.target = "_blank"; a.rel = "noopener noreferrer nofollow"; li.append(a); }
      else li.append(el("span", "mono", l));
      ul.append(li);
    }
    return ul;
  }
  function chCopyBtn(label, text) {
    const b = el("button", "btn xs", label);
    b.type = "button";
    b.dataset.label = label;
    b.disabled = !text;
    b.addEventListener("click", () => copy(text, b));
    return b;
  }
  function renderCProdInsp(box) {
    const id = sel.id;
    const x = chProd(id);
    if (!x) {
      region(box, "head", head({ kind: "Production", title: `Production #${id}`, sub: chErr || (chData ? "Not in the Chronicle." : "Loading…") }));
      if (!chData && !chBusy) loadChronicle();
      return;
    }
    region(box, "head", head({ kind: "Production", title: x.title, id: `#${x.id}`, sub: [x.format, `pitched by ${x.pitched_by}`, `tick ${fmt(x.tick)}`].join(" · "), tags: [tag(x.status, CH_STATUS_TAG[x.status] || "")] }));
    const extra = [];
    if (x.status === "pitched") extra.push(ctToggle("Greenlight", "greenlight", "primary"), ctToggle("Decline", "decline", "danger"));
    if (x.status === "greenlit") extra.push(ctToggle("Lock script", "lock"));
    if (x.status === "greenlit" || x.status === "locked") extra.push(ctToggle("Release", "release", "primary"));
    if (x.script_page) extra.push(btn(`Script page #${x.script_page}`, "sm ghost", () => select("cpage", x.script_page)));
    ctActions(box, JSON.stringify([id, x.status, actOpen, x.script_page]), false, "", extra);
    const cast = (x.cast || []).map((c) => c.name).join(", ") || "no cast";
    const makers = {};
    if (x.status === "pitched") {
      makers.greenlight = () => chDecisionForm({
        title: `Greenlight “${x.title}”`, ok: "Greenlight", action: "chronicle_greenlight", label: "Greenlight", body: { production: id },
        hint: "Opens its script page for the pitcher and cast to write together, tells them and makes the news. The note is stored on the production, where captains read it (and the public once it is released).",
        confirm: `Greenlight production #${id}?`,
        review: [`“${x.title}” (${x.format}), pitched by ${x.pitched_by}. Cast: ${cast}.`, "A script page opens and the pitcher and cast are told. Journaled."],
      });
      makers.decline = () => chDecisionForm({
        title: `Decline “${x.title}”`, ok: "Decline", danger: true, action: "chronicle_decline", label: "Decline", body: { production: id },
        hint: "Closes the pitch. The note goes to the pitcher and stays on the production.",
        notePh: "Why (optional, sent to the pitcher)",
        confirm: `Decline production #${id}?`,
        review: [`“${x.title}”, pitched by ${x.pitched_by}, is declined. Journaled.`],
      });
    }
    if (x.status === "greenlit") {
      makers.lock = () => chDecisionForm({
        title: `Lock the script of “${x.title}”`, ok: "Lock", action: "chronicle_lock", label: "Lock script", body: { production: id },
        hint: "Freezes the script at its current revision: the package and the release use that revision. Check the package (cast approvals, reports) first.",
        confirm: `Lock production #${id}?`,
        review: [`The script page #${x.script_page || "–"} is frozen at its current revision for production. Journaled.`],
      });
    }
    if (x.status === "greenlit" || x.status === "locked") {
      makers.release = () => chDecisionForm({
        title: `Release “${x.title}”`, ok: "Release", action: "chronicle_release", label: "Release", body: { production: id }, links: true,
        hint: "Publishes the finished work on /chronicle with these links, credits the pitcher, script writers and cast (Influence this season) and makes the news. Links and note are public.",
        notePh: "Note (optional, public with the release)",
        confirm: `Release production #${id}?`,
        review: [`“${x.title}” goes public on /chronicle with these links. Everyone credited is told. Journaled; a release cannot be withdrawn from the console.`],
      });
    }
    ctForm(box, makers);
    const tb = tabsBar("cprod", [["overview", "Production"], ["package", "Package"]]);
    const oldT = box.querySelector(":scope > [data-role=tabs]");
    const tSig = JSON.stringify(inspTab.cprod);
    if (!oldT || oldT.dataset.sig !== tSig) { tb.dataset.sig = tSig; region(box, "tabs", tb); }
    if (inspTab.cprod === "package") {
      const det = chDetail(chPkgs, `${API}/chronicle/production?id=${id}`, id, "cprod");
      body(box, JSON.stringify(["pkg", id, det && det.d, det && det.error]), (b) => fillPackage(b, det));
      return;
    }
    body(box, JSON.stringify(["cprod", x, chData && chData.pages.length]), (b) => {
      b.append(section("Logline", el("blockquote", "fb-text", x.logline || "")));
      b.append(section("Production", facts([
        ["Status", x.status, { t: 1 }], ["Format", x.format, { t: 1 }],
        ["Pitched by", captainLink(x.pitched_by), { t: 1 }], ["Pitched", `tick ${fmt(x.tick)}`],
        ["Script", x.script_page ? chPageLink(x.script_page, `page #${x.script_page}`) : "opens at greenlight", { t: 1 }], ["Locked at", x.locked_revision ? `revision ${x.locked_revision}` : "–", { t: 1 }],
      ], true)));
      if (x.operator_note) b.append(section("Operator note", el("blockquote", "fb-text", x.operator_note)));
      const srcs = el("p", "ct-who");
      (x.pages || []).forEach((pid, i) => { if (i) srcs.append(document.createTextNode(", ")); const pr = chPage(pid); srcs.append(chPageLink(pid, pr ? `#${pid} ${pr.title}` : `#${pid}`)); });
      b.append(section("Source pages", (x.pages || []).length ? srcs : el("p", "empty", "None named.")));
      const ul = el("ul", "textlog");
      for (const c of x.cast || []) {
        const li = el("li");
        const m = el("div", "m");
        m.append(captainLink(c.name));
        m.append(c.approved_revision ? tag(`approved r${c.approved_revision}`, "ok") : tag("not approved", ""));
        li.append(m);
        if (c.character) li.append(document.createTextNode(`${c.character.age}${c.character.gender ? `, ${c.character.gender}` : ""}, of ${c.character.homeworld}. ${c.character.bio || ""}`));
        else li.append(el("span", "muted", "No identity on record."));
        ul.append(li);
      }
      b.append(section(`Cast (${(x.cast || []).length})`, (x.cast || []).length ? ul : el("p", "empty", "No cast.")));
      if (x.release) {
        b.append(section("Release",
          facts([["Released", `tick ${fmt(x.release.tick)}`], ["When", stamp(x.release.at_ms), { t: 1 }]], true),
          chLinks(x.release.links),
          x.release.note ? el("blockquote", "fb-text", x.release.note) : null,
          el("p", "ct-who", `Credits: ${(x.release.credits || []).join(", ") || "–"}`)));
      }
    });
  }
  // The production package: what the operator needs to make the work by hand.
  function fillPackage(b, det) {
    const pk = det && det.d;
    if (!pk) { b.append(el("p", "empty", det && det.error ? det.error : "Loading…")); return; }
    const sc = pk.script;
    const scriptText = sc ? `${sc.title}\n\n${sc.body}` : "";
    const row = el("div", "row ch-copy");
    row.append(chCopyBtn("Copy package JSON", JSON.stringify(pk, null, 2)), chCopyBtn("Copy script", scriptText));
    b.append(row);
    const ck = pk.checks || {};
    const un = ck.unapproved_cast || [];
    const ckBox = el("div");
    ckBox.append(facts([
      ["Cast approvals", un.length ? `missing: ${un.join(", ")}` : "all approved", { t: 1, cls: un.length ? "warn-t" : "" }],
      ["Script reports", fmt(ck.script_reports), { cls: ck.script_reports ? "warn-t" : "" }],
      ["Struck sources", fmt(ck.struck_sources), { cls: ck.struck_sources ? "warn-t" : "" }],
    ], true));
    if (ck.content_rule) ckBox.append(el("p", "hint", `Content rule: ${ck.content_rule}`));
    if (ck.video_note) ckBox.append(el("p", "hint", ck.video_note));
    b.append(section("Checks", ckBox));
    if ((pk.contributors || []).length) b.append(section("Contributors", el("p", "ct-who", pk.contributors.join(", "))));
    if (sc) {
      const s = el("div", "note");
      s.append(el("h4", "", sc.title || "Script"), el("p", "hint", [`page #${sc.page}`, `revision ${sc.revision}`, (sc.authors || []).length ? `by ${(sc.authors || []).join(", ")}` : ""].filter(Boolean).join(" · ")), el("pre", "ch-script", sc.body || ""));
      b.append(section("Script", s));
    } else b.append(section("Script", el("p", "empty", "No script yet: it opens at greenlight.")));
    const cs = el("div");
    for (const c of pk.cast_sheet || []) {
      const n = el("div", "note");
      const k = c.card || {};
      n.append(el("h4", "", c.name));
      n.append(facts([
        ["Age", fmt(k.age)], ["Gender", k.gender || genderOf(k.pronouns) || "–", { t: 1 }], ["Pronouns", k.pronouns || "–", { t: 1 }], ["Homeworld", k.homeworld || "–", { t: 1 }], ["Trade", k.trade || "–", { t: 1 }],
        ["Approved", c.approved_revision ? `revision ${c.approved_revision}${c.approved_current ? " (current)" : " (older)"}` : "not yet", { t: 1, cls: c.approved_current ? "" : "warn-t" }],
        ["Appearance", k.appearance || "–", { t: 1 }], ["Voice", k.voice || "–", { t: 1 }],
      ], true));
      if (!c.card) n.append(el("p", "empty", "No identity on record."));
      if (k.bio) n.append(el("p", "", k.bio));
      if (c.card) n.append(idTraits(k));
      if ((k.relations || []).length) n.append(el("div", "hint", "Public relations"), idRelations(k.relations));
      const rc = c.recent || {};
      if ((rc.log || []).length || (rc.moments || []).length) {
        const d = el("details", "lg-v");
        d.append(el("summary", "", `Recent: ${plural((rc.log || []).length, "log line")}, ${plural((rc.moments || []).length, "moment")}`), idLog(rc.log, false), idMoments(rc.moments));
        n.append(d);
      }
      cs.append(n);
    }
    b.append(section(`Cast sheet (${(pk.cast_sheet || []).length})`, (pk.cast_sheet || []).length ? cs : el("p", "empty", "No cast.")));
    const src = el("div");
    for (const p of pk.sources || []) {
      const d = el("details", "lg-v");
      const sm = el("summary");
      sm.append(document.createTextNode(`#${p.id} ${p.title}`));
      if (p.canon) sm.append(" ", tag("canon", "ok"));
      if (p.struck) sm.append(" ", tag("struck", "crit"));
      if (p.reports) sm.append(" ", tag(plural(p.reports, "report"), "warn"));
      d.append(sm, markdown(p.body || ""));
      const cits = chCitations(p.citations);
      if (cits) d.append(el("div", "hint", "Citations"), cits);
      const open = el("p", "hint");
      open.append(chPageLink(p.id, "Open the page"));
      d.append(open);
      src.append(d);
    }
    b.append(section(`Sources (${(pk.sources || []).length})`, (pk.sources || []).length ? src : el("p", "empty", "No source pages.")));
    b.append(section("Credits", el("p", "ct-who", (pk.credits || []).join(", ") || "–")));
  }

  async function loadConstitution() {
    if (ctBusy) { ctAgain = true; return; }
    ctBusy = true;
    const want = ctSeason;
    const r = await api(want === "current" ? `${API}/constitution` : `${API}/constitution?season=${encodeURIComponent(want)}`);
    ctBusy = false;
    if (want !== ctSeason) { loadConstitution(); return; } // the picker moved meanwhile
    if (r.ok && r.data && typeof r.data === "object") {
      ctData = r.data; ctMissing = false; ctErr = null;
      if (want === "current" && Array.isArray(r.data.seasons)) ctLiveSeasons = r.data.seasons;
    } else if (r.status === 404 && want === "current") { ctMissing = true; ctErr = null; }
    else if (r.status === 404) { ctData = null; ctErr = "That season is not in the permanent record."; }
    else if (r.status !== 401) ctErr = errText(r.data, r.status);
    if (ctAgain) { ctAgain = false; loadConstitution(); return; }
    ctSig = "";
    renderCtSeasons();
    renderConstitution();
    renderCtBadge();
    if (sel && ["article", "proposal", "archive"].includes(sel.kind)) { insSig = ""; buildInspector(); }
  }
  // Every state poll: refetch the full record when the live constitution changed.
  function ctOnState() {
    const c = S.constitution || { articles: [], proposals: [] };
    const sig = JSON.stringify([(c.articles || []).map((a) => [a.id, a.amended_tick, a.text]), (c.proposals || []).map((p) => [p.id, p.status, p.yes, p.no, ctBallots(p).length, ctComments(p).length, !!p.struck]), (c.notices || []).length, (c.seasons || []).length]);
    renderCtBadge();
    renderCtSeasons();
    if (sig === ctStateSig) return;
    const first = ctStateSig === null;
    ctStateSig = sig;
    if (!ctLive()) return; // a past season never changes; the picker keeps the live list fresh
    if (!first || mode === "constitution" || (sel && ["article", "proposal", "archive"].includes(sel.kind))) loadConstitution();
    else { ctSig = ""; renderConstitution(); }
  }
  function renderCtBadge() {
    const live = (S && S.constitution && S.constitution.proposals) || (ctLive() ? ctProposals() : []);
    const n = live.filter(ctOpen).length;
    for (const id of ["b-constitution", "b-constitution-m"]) { const b = $(id); if (b) { b.hidden = !n; b.textContent = String(n); } }
  }
  function renderCtSeasons() {
    const s = $("ct-season");
    if (!s) return;
    const list = ctSeasons();
    if (ctSeason !== "current" && list.length && !list.some((x) => x.season_key === ctSeason)) ctSeason = "current";
    const opts = [["current", "Current season"], ...list.map((x) => [x.season_key, `${ctSeasonLabel(x)} · ${plural(x.proposals || 0, "proposal")}`])];
    if (ctSeason !== "current" && !opts.some(([k]) => k === ctSeason)) opts.push([ctSeason, ctSeason]);
    const sig = JSON.stringify(opts);
    if (s.dataset.sig !== sig) {
      s.dataset.sig = sig;
      s.replaceChildren(...opts.map(([k, l]) => Object.assign(el("option", "", l), { value: k })));
    }
    s.value = ctSeason;
    $("ct-season-w").hidden = !list.length && ctSeason === "current";
    $("ct-ops").hidden = !ctLive();
    if (!ctLive() && ctOp) { ctOp = null; renderCtOps(); }
  }
  function setCtSeason(k) {
    k = k || "current";
    if (k === ctSeason) return;
    ctSeason = k;
    ctData = null; ctErr = null; ctSig = "";
    renderCtSeasons();
    renderConstitution();
    loadConstitution();
  }
  // Open the Constitution workbench on one season (from the History workbench).
  function ctOpenSeason(k) {
    setMode("constitution");
    setCtSeason(k);
  }
  function setCtFilter(f) {
    if (!CT_FILTERS.some(([k]) => k === f)) f = "open";
    ctFilter = f;
    store.set("ctFilter", f);
    ctSig = "";
    renderConstitution();
  }
  const ctMatch = (q, ...vals) => !q || vals.some((v) => v !== null && v !== undefined && String(v).toLowerCase().includes(q));
  function renderConstitution() {
    if (mode !== "constitution") return;
    const q = ctQ.toLowerCase();
    const props = ctProposals();
    const counts = { open: props.filter(ctOpen).length, closed: props.filter((p) => !ctOpen(p)).length, all: props.length };
    const arts = ctArticles().filter((a) => ctMatch(q, ctText(a.text), a.proposed_by, `article ${a.id}`));
    const rows = props.filter((p) => (ctFilter === "all" || (ctFilter === "open") === ctOpen(p)))
      .filter((p) => ctMatch(q, ctText(p.text), p.proposer, `#${p.id}`, p.change, ...ctVoters(p).map((v) => v.name), ...ctBallots(p).map((v) => v.name), ...ctComments(p).map((c) => c.text)))
      .sort((a, b) => b.id - a.id);
    const arch = ctArchive().filter((x) => !q || (x.articles || []).some((a) => ctMatch(q, ctText(a.text), a.proposed_by)));
    const gone = ctRemoved().filter((r) => ctMatch(q, ctText(r.article.text), r.article.proposed_by, r.how, `article ${r.article.id}`));
    const notes = ctNotices().filter((n) => ctMatch(q, n.text));
    const v = ctView() || {};
    $("ct-count").textContent = `${ctLive() ? "" : `${ctSeasonLabel({ season: v.season_number, reason: v.reason, epoch: v.epoch })} · `}${plural(ctArticles().length, "article")}${counts.open ? ` · ${fmt(counts.open)} open` : ""}`;
    const chips = $("ct-filters");
    const chipSig = JSON.stringify([counts, ctFilter]);
    if (chips.dataset.sig !== chipSig) {
      chips.dataset.sig = chipSig;
      chips.replaceChildren(...CT_FILTERS.map(([k, l]) => {
        const b = el("button", "chip", l);
        b.type = "button";
        b.setAttribute("aria-pressed", String(ctFilter === k));
        if (counts[k]) b.append(el("small", "", compact(counts[k])));
        b.addEventListener("click", () => setCtFilter(k));
        return b;
      }));
    }
    const sig = JSON.stringify([ctSeason, ctFilter, q, arts, rows, gone.map((r) => [r.article.id, r.how, r.tick]), arch.map((x) => [x.season, x.carried, (x.articles || []).length]), notes, ctErr, ctMissing, S && S.tick]);
    if (sig === ctSig) { markSelected(); return; }
    ctSig = sig;
    const frag = document.createDocumentFragment();
    const gh = (t) => el("li", "gh", t);
    if (!ctLive()) {
      frag.append(el("li", "ct-note", !ctData ? (ctErr || "Loading the permanent record…")
        : `Permanent record: archived ${v.reason === "fresh_start" ? "before a fresh start" : "at the Big Bang"}, tick ${fmt(v.archived_tick)}${v.archived_ms ? ` (${stamp(v.archived_ms)})` : ""}. The articles ${v.carried ? "carried into the next season" : v.reason === "fresh_start" ? "were wiped by the fresh start" : "lapsed"}.`));
    }
    frag.append(gh(`Articles · ${fmt(arts.length)}`));
    for (const a of arts) {
      const li = el("li");
      li.dataset.sel = selKey("article", a.id);
      const tt = el("div", "tt");
      tt.append(el("span", "", `Article ${a.id}`));
      if (a.amended_tick !== null && a.amended_tick !== undefined) tt.append(tag("amended", "info"));
      if (a.by_operator) tt.append(tag("operator", "warn"));
      const tm = el("div", "tm", `s${fmt(a.season)} · t${fmt(a.ratified_tick)}`);
      tm.title = `ratified at tick ${fmt(a.ratified_tick)}${tickTime(a.ratified_tick) ? ` · ${stamp(tickTime(a.ratified_tick))}` : ""}`;
      const tp = el("div", "tp", `proposed by ${a.by_operator ? "the operator" : a.proposed_by || "–"}`);
      const pv = el("div", "fb-pv");
      pv.append(marked(snippet(ctText(a.text).replace(/\s+/g, " ").trim(), q, 160), q));
      li.append(tt, tm, tp, pv);
      li.addEventListener("click", () => select("article", a.id));
      frag.append(li);
    }
    if (!arts.length) frag.append(el("li", "empty-l", ctErr || (q ? "No article matches." : ctLive() ? "No articles yet. Captains propose them with the constitution tool." : "No articles at the season's end.")));
    frag.append(gh(`Proposals · ${CT_FILTERS.find(([k]) => k === ctFilter)[1].toLowerCase()} · ${fmt(rows.length)}`));
    for (const p of rows) {
      const li = el("li", ctOpen(p) ? "unread" : "");
      li.dataset.sel = selKey("proposal", p.id);
      const tt = el("div", "tt");
      tt.append(el("span", "", `#${p.id} ${ctChange(p)}`));
      if (p.by_operator) tt.append(tag("operator", "warn"));
      const tm = el("div", "tm", ctOpen(p) && ctLive() ? `closes in ${ticksDur(Math.max(0, p.closes_tick - (S ? S.tick : p.closes_tick)))}` : `t${fmt(isNum(p.closed_tick) ? p.closed_tick : p.closes_tick)}`);
      tm.title = `opened tick ${fmt(p.opened_tick)}, closes tick ${fmt(p.closes_tick)}${isNum(p.closed_tick) ? `, closed tick ${fmt(p.closed_tick)}` : ""}`;
      const extras = [ctBallots(p).length > ctVoters(p).length ? `${plural(ctBallots(p).length, "ballot")}` : "", ctComments(p).length ? plural(ctComments(p).length, "comment") : ""].filter(Boolean).join(" · ");
      const tp = el("div", "tp", `by ${ctBy(p)}${isNum(p.season) ? ` · season ${p.season}` : ""}${extras ? ` · ${extras}` : ""}`);
      const tn = el("div", "tn");
      tn.append(el("span", "n", `${fmt(p.yes)}/${fmt(p.threshold)}${p.no ? ` · ${fmt(p.no)} no` : ""}`), ctTag(p));
      li.append(tt, tm, tp, tn);
      const txt = ctText(p.text);
      if (txt) { const pv = el("div", "fb-pv"); pv.append(marked(snippet(txt.replace(/\s+/g, " ").trim(), q, 160), q)); li.append(pv); }
      li.addEventListener("click", () => select("proposal", p.id));
      frag.append(li);
    }
    if (!rows.length) frag.append(el("li", "empty-l", q ? "No proposal matches." : ctFilter === "open" ? "No open proposals." : "No proposals yet."));
    if (notes.length) {
      frag.append(gh(`Operator notices · ${fmt(notes.length)}`));
      for (const n of [...notes].reverse()) {
        const li = el("li");
        const tt = el("div", "tt");
        tt.append(el("span", "", "Notice to captains"));
        const tn = el("div", "tn");
        tn.append(tag(n.announcement != null || n.pinned ? "pinned" : "message", n.announcement != null || n.pinned ? "info" : ""));
        const tm = el("div", "tm", `t${fmt(n.tick)}`);
        tm.title = stamp(n.at_ms);
        const pv = el("div", "fb-pv");
        pv.append(marked(snippet(String(n.text || "").replace(/\s+/g, " ").trim(), q, 160), q));
        li.title = n.text || "";
        li.append(tt, tm, el("div", "tp", `season ${fmt(n.season)}`), tn, pv);
        frag.append(li);
      }
    }
    if (gone.length) {
      frag.append(gh(`Removed articles · ${fmt(gone.length)}`));
      for (const r of [...gone].reverse()) {
        const a = r.article;
        const li = el("li");
        li.dataset.sel = selKey("article", a.id);
        const tt = el("div", "tt");
        tt.append(el("span", "", `Article ${a.id}`));
        const tn = el("div", "tn");
        tn.append(tag(r.how || "removed", r.how === "struck" ? "crit" : ""));
        const pv = el("div", "fb-pv");
        pv.append(marked(snippet(ctText(a.text).replace(/\s+/g, " ").trim(), q, 160), q));
        li.append(tt, el("div", "tm", `t${fmt(r.tick)}`), el("div", "tp", `${r.how || "removed"}${isNum(r.proposal) ? ` by proposal #${r.proposal}` : r.how === "struck" ? " by the operator" : ""}`), tn, pv);
        li.addEventListener("click", () => select("article", a.id));
        frag.append(li);
      }
    }
    if (arch.length) {
      frag.append(gh(`Past seasons' articles · ${fmt(arch.length)}`));
      for (const x of [...arch].sort((a, b) => b.season - a.season)) {
        const li = el("li");
        li.dataset.sel = selKey("archive", x.season);
        const tt = el("div", "tt");
        tt.append(el("span", "", `Season ${x.season}`));
        const tn = el("div", "tn");
        tn.append(tag(x.carried ? "carried" : "lapsed", x.carried ? "ok" : ""));
        li.append(tt, el("div", "tm", ""), el("div", "tp", plural((x.articles || []).length, "article")), tn);
        li.addEventListener("click", () => select("archive", x.season));
        frag.append(li);
      }
    }
    $("ct-list").replaceChildren(frag);
    markSelected();
  }
  $("ct-q").addEventListener("input", (e) => { ctQ = e.target.value.trim(); ctSig = ""; renderConstitution(); });
  $("ct-refresh").addEventListener("click", () => loadConstitution());
  $("ct-season").addEventListener("change", (e) => setCtSeason(e.target.value));

  // ---------- the operator takes part: a proposal or a notice, from the workbench ----------
  const ctFld = (label, input) => { const l = el("label", "fld"); l.append(el("span", "", label), input); return l; };
  const ctArea = (ph, rows) => { const t = el("textarea", "ct-t"); t.rows = rows || 3; t.maxLength = CT_TEXT_MAX; t.placeholder = ph; t.setAttribute("aria-label", ph); return t; };
  function setCtOp(k) {
    ctOp = ctOp === k ? null : k;
    renderCtOps();
  }
  function renderCtOps() {
    for (const [id, k] of [["ct-propose", "propose"], ["ct-notice", "notice"]]) $(id).setAttribute("aria-pressed", String(ctOp === k));
    const box = $("ct-opform");
    box.hidden = !ctOp;
    if (!ctOp) { box.replaceChildren(); return; }
    const f = el("form", "actform");
    f.addEventListener("submit", (e) => e.preventDefault());
    const foot = el("div", "foot");
    const cancel = btn("Cancel", "sm ghost", () => setCtOp(null));
    if (ctOp === "propose") {
      f.append(el("h4", "", "Propose as operator"), el("p", "hint", "Same electorate, voting window and threshold as a captain's proposal. The operator does not vote; the captains decide. Every captain sees it as an announcement, and it goes out on public radio."));
      const kind = el("select");
      kind.setAttribute("aria-label", "Change");
      for (const [k, l] of [["add", "Add an article"], ["amend", "Amend an article"], ["repeal", "Repeal an article"]]) kind.append(Object.assign(el("option", "", l), { value: k }));
      const art = el("select");
      art.setAttribute("aria-label", "Article");
      const arts = ctArticles();
      for (const a of arts) art.append(Object.assign(el("option", "", `Article ${a.id}: ${clip(ctText(a.text).replace(/\s+/g, " "), 60)}`), { value: String(a.id) }));
      const text = ctArea("Article text, as it should read");
      const note = ctArea("Note to the captains (optional, posted as the first operator comment)", 2);
      const artW = ctFld("Article", art), textW = ctFld(`Text (at most ${fmt(CT_TEXT_MAX)} characters)`, text);
      const row = el("div", "row");
      row.append(ctFld("Change", kind), artW);
      const sync = () => {
        artW.hidden = kind.value === "add" || !arts.length;
        textW.hidden = kind.value === "repeal";
        if (kind.value !== "add" && arts.length) { const a = arts.find((x) => String(x.id) === art.value); if (kind.value === "amend" && a && !text.value) text.value = ctText(a.text); }
      };
      kind.addEventListener("change", sync);
      art.addEventListener("change", () => { if (kind.value === "amend") { text.value = ""; sync(); } });
      sync();
      const go = btn("Propose", "sm primary", async () => {
        const k = kind.value;
        if (k !== "add" && !arts.length) { toast("err", "Operator proposal", "There is no article to amend or repeal."); return; }
        const body = { kind: k };
        if (k !== "add") body.article = Number(art.value);
        if (k !== "repeal") { const t = text.value; if (!t.trim()) { text.focus(); return; } body.text = t; }
        if (note.value.trim()) body.note = note.value;
        const what = k === "add" ? "add an article" : `${k} article ${body.article}`;
        if (!await confirmDlg({ title: `Propose to ${what}?`, body: ["Every captain sees it as a server announcement and on public radio; the captains active now vote on it with the usual threshold. Journaled and permanent.", body.text ? el("blockquote", "", body.text) : null, body.note ? el("blockquote", "", body.note) : null].filter(Boolean), ok: "Propose", danger: false })) return;
        go.disabled = true;
        const d = await act("constitution_propose", body, "Operator proposal");
        go.disabled = false;
        if (!d) return;
        ctOp = null;
        renderCtOps();
        await loadConstitution();
        const id = d.proposal && d.proposal.id;
        if (isNum(id)) select("proposal", id);
      });
      foot.append(cancel, go);
      f.append(row, textW, ctFld("Note", note), foot);
      setTimeout(() => (kind.value === "add" ? text : kind).focus(), 0);
    } else {
      f.append(el("h4", "", "Notice to captains"), el("p", "hint", "About the constitution, to every captain: public radio and the news, and either a pinned announcement (in every briefing until acknowledged) or an operator message to every captain."));
      const text = ctArea("The notice", 4);
      const pin = el("input"); pin.type = "checkbox";
      const pl = el("label", "check"); pl.append(pin, document.createTextNode(" Pin as an announcement"));
      const go = btn("Send", "sm primary", async () => {
        const t = text.value;
        if (!t.trim()) { text.focus(); return; }
        if (!await confirmDlg({ title: pin.checked ? "Send and pin this notice?" : "Send this notice to every captain?", body: [pin.checked ? "A server announcement in every captain's briefing until acknowledged, plus public radio and the news." : "An operator message to every captain (in the briefing until read), plus public radio and the news.", el("blockquote", "", t)], ok: "Send notice", danger: false })) return;
        go.disabled = true;
        const d = await act("constitution_notice", { text: t, pin: pin.checked }, pin.checked ? "Pinned constitution notice" : "Constitution notice");
        go.disabled = false;
        if (!d) return;
        ctOp = null;
        renderCtOps();
        loadConstitution();
      });
      text.addEventListener("keydown", (e) => { if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) { e.preventDefault(); go.click(); } });
      foot.append(pl, el("span", "grow"), cancel, go);
      f.append(text, foot);
      setTimeout(() => text.focus(), 0);
    }
    box.replaceChildren(f);
  }
  $("ct-propose").addEventListener("click", () => setCtOp("propose"));
  $("ct-notice").addEventListener("click", () => setCtOp("notice"));

  // ---------- an article, a proposal or a past season, in the inspector ----------
  const ctPlayerLink = (id, name) => { const p = isNum(id) ? player(id) : name ? playerByName(name) : null; return p ? link(p.name, () => select("captain", p.id, { pan: true })) : document.createTextNode(name || (isNum(id) ? `#${id}` : "–")); };
  function ctStrikeForm(what, body) {
    const f = el("form", "actform");
    f.append(el("h4", "", `Strike ${what}`), el("p", "hint", body.proposal !== undefined
      ? "Cancels the proposal if open and replaces its text for captains and the public, in notices and citation copies too; the original stays in the operator record. Its ballots and your reason stay on the record. Journaled and announced in the news."
      : "Removes the article and replaces its text for captains and the public, in notices and citation copies too; the original stays in the operator record. Journaled and announced in the news."));
    const inp = el("input");
    inp.type = "text";
    inp.maxLength = 200;
    inp.placeholder = "Reason (optional, shown in the news)";
    inp.setAttribute("aria-label", "Reason");
    const go = el("button", "btn sm danger", "Strike");
    go.type = "submit";
    const foot = el("div", "foot");
    foot.append(btn("Cancel", "sm ghost", () => { actOpen = null; buildInspector(); }), go);
    f.append(inp, foot);
    f.addEventListener("submit", async (e) => {
      e.preventDefault();
      go.disabled = true;
      const reason = inp.value.trim();
      const d = await act("constitution_strike", { ...body, ...(reason ? { reason } : {}) }, "Constitution");
      go.disabled = false;
      if (!d) return;
      actOpen = null;
      await loadConstitution();
      buildInspector();
    });
    setTimeout(() => inp.focus(), 0);
    return f;
  }
  function ctCommentForm(p) {
    const f = el("form", "actform");
    f.addEventListener("submit", (e) => e.preventDefault());
    f.append(el("h4", "", `Comment on proposal #${p.id}`), el("p", "hint", `Shown with the proposal to every captain and spectator, and on public radio as “OPERATOR on proposal #${p.id}”. At most ${fmt(CT_TEXT_MAX)} characters, stored exactly.`));
    const t = ctArea("Your comment", 4);
    const go = btn("Post comment", "sm primary", async () => {
      const text = t.value;
      if (!text.trim()) { t.focus(); return; }
      if (!await confirmDlg({ title: `Comment on proposal #${p.id}?`, body: ["Public: every captain and spectator sees it with the proposal, and it goes out on public radio. Journaled and permanent.", el("blockquote", "", text)], ok: "Post comment", danger: false })) return;
      go.disabled = true;
      const d = await act("constitution_comment", { proposal: p.id, text }, `Comment on #${p.id}`);
      go.disabled = false;
      if (!d) return;
      actOpen = null;
      await loadConstitution();
      buildInspector();
    });
    t.addEventListener("keydown", (e) => { if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) { e.preventDefault(); go.click(); } });
    const foot = el("div", "foot");
    foot.append(btn("Cancel", "sm ghost", () => { actOpen = null; buildInspector(); }), go);
    f.append(t, foot);
    setTimeout(() => t.focus(), 0);
    return f;
  }
  function ctToggle(label, name, cls) {
    const b = el("button", `btn sm${cls ? ` ${cls}` : ""}`, label);
    b.type = "button";
    b.setAttribute("aria-pressed", String(actOpen === name));
    b.addEventListener("click", () => { actOpen = actOpen === name ? null : name; buildInspector(); });
    return b;
  }
  function ctActions(box, sig, strikable, strikeLabel, extra) {
    const old = box.querySelector(":scope > [data-role=actions]");
    if (old && old.dataset.sig === sig) return;
    const a = el("div", "actions");
    if (strikable) a.append(ctToggle(strikeLabel, "strike", "danger"));
    for (const x of extra) a.append(x);
    a.dataset.sig = sig;
    region(box, "actions", a);
  }
  // The open action form (`actOpen` names it), if its maker is offered here.
  function ctForm(box, makers) {
    const old = box.querySelector(":scope > [data-role=form]");
    const make = actOpen && makers[actOpen];
    if (make) { if (!old || old.dataset.kind !== actOpen) { const f = make(); f.dataset.kind = actOpen; region(box, "form", f); } }
    else if (old) old.remove();
  }
  function ctTally(p) {
    const n = Math.max(1, p.electorate || 0);
    const bar = el("div", "ct-bar");
    const y = el("i", "y"); y.style.width = `${Math.min(100, ((p.yes || 0) / n) * 100)}%`;
    const no = el("i", "no"); no.style.width = `${Math.min(100, ((p.no || 0) / n) * 100)}%`;
    const t = el("b"); t.style.left = `${Math.min(100, ((p.threshold || 0) / n) * 100)}%`; t.title = `${fmt(p.threshold)} yes votes needed`;
    bar.append(y, no, t);
    bar.title = `${fmt(p.yes)} yes, ${fmt(p.no)} no, ${fmt(p.threshold)} needed of ${fmt(p.electorate)}`;
    return bar;
  }
  // Every ballot in the order cast: who, yes or no, when, and whether it changed an earlier vote.
  function ctBallotList(p) {
    const list = ctBallots(p);
    if (!list.length) return el("p", "empty", !Array.isArray(p.ballots) ? "This server does not keep ballots."
      : ctVoters(p).length ? "Votes cast before engine 1.37: ballots were not kept (the tally above is the latest vote of each)."
      : p.by_operator ? "No ballots yet (the operator does not vote)." : "No ballots yet.");
    const ul = el("ul", "ct-hist");
    list.forEach((b, i) => {
      const li = el("li");
      const meta = el("div", "m");
      meta.append(document.createTextNode(`${i + 1}. `), ctPlayerLink(b.player_id, b.name));
      meta.append(tag(b.yes ? "yes" : "no", b.yes ? "ok" : "crit"));
      if (b.changed) meta.append(tag("changed", "warn", "Replaced this captain's earlier vote"));
      meta.append(document.createTextNode(` · tick ${fmt(b.tick)} · ${stamp(b.at_ms)}`));
      li.append(meta);
      ul.append(li);
    });
    return ul;
  }
  function ctCommentList(p) {
    const ul = el("ul", "ct-hist");
    for (const c of ctComments(p)) {
      const li = el("li");
      li.append(el("div", "m", `tick ${fmt(c.tick)} · ${stamp(c.at_ms)}`), el("blockquote", "fb-text", String(c.text || "")));
      ul.append(li);
    }
    return ul;
  }
  function renderProposalInsp(box) {
    const p = ctProposals().find((x) => x.id === sel.id);
    if (!p) {
      region(box, "head", head({ kind: "Proposal", title: `#${sel.id}`, sub: ctBusy || (!ctData && !ctMissing) ? "Loading…" : ctLive() ? "Not in this season: past seasons are in the permanent record (pick one with the season selector)." : "Not in this season's record." }));
      if (!ctData && !ctMissing && !ctBusy) loadConstitution();
      return;
    }
    const tags = [ctTag(p)];
    if (p.by_operator) tags.push(tag("operator", "warn"));
    if (p.struck) tags.push(tag("struck", "crit"));
    region(box, "head", head({ kind: "Constitution proposal", title: `#${p.id} ${ctChange(p)}`, sub: [`by ${ctBy(p)}`, isNum(p.season) && `season ${p.season}`, `opened tick ${fmt(p.opened_tick)}`, !ctLive() && "permanent record"].filter(Boolean).join(" · "), tags }));
    const proposer = p.by_operator ? null : playerByName(p.proposer);
    const extra = [];
    if (ctLive()) extra.push(ctToggle("Comment", "comment"));
    if (proposer) extra.push(btn("Open proposer", "sm ghost", () => select("captain", proposer.id, { pan: true })));
    const art = isNum(p.article) && ctArticles().find((a) => a.id === p.article);
    if (art) extra.push(btn(`Article ${art.id}`, "sm ghost", () => select("article", art.id)));
    const strikable = p.status !== "cancelled" && !p.struck;
    ctActions(box, JSON.stringify([p.id, p.status, actOpen, !!proposer, !!art, ctSeason, !!p.struck]), strikable, ctOpen(p) ? "Strike" : "Strike text", extra);
    ctForm(box, { ...(strikable ? { strike: () => ctStrikeForm(`proposal #${p.id}`, { proposal: p.id }) } : {}), ...(ctLive() ? { comment: () => ctCommentForm(p) } : {}) });
    body(box, JSON.stringify([p, S ? S.players.length : 0]), (b) => {
      const txt = ctText(p.text);
      b.append(section(p.change === "repeal" ? "Repeal" : p.change === "carry" ? "Carry vote" : "Text",
        txt ? el("blockquote", "fb-text", txt)
          : el("p", "hint", p.change === "repeal" ? `Removes article ${p.article}.` : p.change === "carry" ? "Keeps every article through the Big Bang into the next galaxy (two thirds needed)." : "No text.")));
      if (p.struck_original) b.append(section("Original text (operator only)", el("blockquote", "fb-text", String(p.struck_original))));
      if (Array.isArray(p.mentions) && p.mentions.length) b.append(section("Mentions", el("p", "ct-who", p.mentions.join(", "))));
      if (ctComments(p).length) b.append(section(`Operator comments · ${fmt(ctComments(p).length)}`, ctCommentList(p)));
      const sr = statRow([["Yes", fmt(p.yes)], ["No", fmt(p.no)], ["Needed", fmt(p.threshold)], ["Electorate", fmt(p.electorate)]]);
      sr.classList.add("four");
      b.append(section("Tally", sr, ctTally(p)));
      const votes = ctVoters(p);
      const yes = votes.filter((v) => v.yes), no = votes.filter((v) => !v.yes);
      const voted = new Set(votes.map((v) => v.player_id));
      const silent = (Array.isArray(p.electorate_ids) ? p.electorate_ids : []).filter((id) => !voted.has(id));
      const who = (list, mk) => {
        if (!list.length) return el("p", "empty", "Nobody.");
        const box2 = el("div", "ct-who");
        list.forEach((x, i) => { if (i) box2.append(document.createTextNode(", ")); box2.append(mk(x)); });
        return box2;
      };
      if (Array.isArray(p.votes)) {
        b.append(section(`Yes now · ${fmt(yes.length)}`, who(yes, (v) => ctPlayerLink(v.player_id, v.name))));
        b.append(section(`No now · ${fmt(no.length)}`, who(no, (v) => ctPlayerLink(v.player_id, v.name))));
        if (Array.isArray(p.electorate_ids)) b.append(section(`${ctOpen(p) ? "Not voted yet" : "Never voted"} · ${fmt(silent.length)}`, who(silent, (id) => ctPlayerLink(id))));
        const nb = ctBallots(p).length, nc = ctBallots(p).filter((x) => x.changed).length;
        b.append(section(`Ballots · ${fmt(nb)}${nc ? ` · ${plural(nc, "changed vote")}` : ""}`, ctBallotList(p)));
      } else b.append(section("Votes", el("p", "hint", "This server does not show individual votes.")));
      b.append(section("Details", facts([
        ["Status", p.status, { t: 1, cls: p.status === "cancelled" ? "warn-t" : "" }],
        ["Change", ctChange(p), { t: 1 }],
        ["Proposer", p.by_operator ? "the operator" : proposer ? ctPlayerLink(null, p.proposer) : p.proposer || "–"],
        ["Season", fmt(p.season)],
        ["Opened", `tick ${fmt(p.opened_tick)}`, { t: 1 }],
        ["Closes", `tick ${fmt(p.closes_tick)}`, { t: 1 }],
        ["Closed", isNum(p.closed_tick) ? `tick ${fmt(p.closed_tick)}` : "–", { t: 1 }],
        ["Opened at", stamp(tickTime(p.opened_tick)), { t: 1 }],
        ...(p.struck ? [["Struck", `tick ${fmt(p.struck.tick)}${p.struck.reason ? `: ${p.struck.reason}` : ""}`, { t: 1, cls: "warn-t" }]] : []),
      ], true)));
    });
  }
  function ctHistory(a) {
    const hist = Array.isArray(a.history) ? [...a.history].reverse() : [];
    if (!hist.length) return el("p", "hint", Array.isArray(a.history) ? "No recorded versions." : "This server does not keep article history.");
    const ul = el("ul", "ct-hist");
    for (const h of hist) {
      const li = el("li");
      const meta = el("div", "m");
      const pr = isNum(h.proposal) && ctProposals().find((x) => x.id === h.proposal);
      meta.append(pr ? link(`#${h.proposal}`, () => select("proposal", h.proposal)) : document.createTextNode(isNum(h.proposal) ? `#${h.proposal}` : "–"));
      meta.append(document.createTextNode(` ${h.change || ""} · tick ${fmt(h.tick)} · season ${fmt(h.season)} · ${fmt(h.yes)} yes, ${fmt(h.no)} no of ${fmt(h.electorate)}`));
      li.append(meta);
      const t = ctText(h.text);
      if (t) li.append(el("blockquote", "fb-text", t));
      if (h.struck_original) li.append(el("div", "hint", "Original text (operator only)"), el("blockquote", "fb-text", String(h.struck_original)));
      ul.append(li);
    }
    return ul;
  }
  function renderArticleInsp(box) {
    const live = ctArticles().find((x) => x.id === sel.id);
    const gone = live ? null : [...ctRemoved()].reverse().find((r) => r.article.id === sel.id);
    const a = live || (gone && gone.article);
    if (!a) {
      region(box, "head", head({ kind: "Article", title: `Article ${sel.id}`, sub: ctBusy || (!ctData && !ctMissing) ? "Loading…" : "Not in the constitution now (repealed, struck or lapsed)." }));
      if (!ctData && !ctMissing && !ctBusy) loadConstitution();
      return;
    }
    const tags = [];
    if (gone) tags.push(tag(gone.how || "removed", gone.how === "struck" ? "crit" : ""));
    if (a.amended_tick !== null && a.amended_tick !== undefined) tags.push(tag("amended", "info"));
    if (a.by_operator) tags.push(tag("operator", "warn"));
    region(box, "head", head({ kind: gone ? "Removed article" : ctLive() ? "Constitution article" : "Archived article", title: `Article ${a.id}`, sub: [`proposed by ${a.by_operator ? "the operator" : a.proposed_by || "–"}`, `ratified tick ${fmt(a.ratified_tick)}`, isNum(a.season) && `season ${a.season}`].filter(Boolean).join(" · "), tags }));
    const author = a.by_operator ? null : playerByName(a.proposed_by);
    const extra = author ? [btn("Open proposer", "sm ghost", () => select("captain", author.id, { pan: true }))] : [];
    // A removed or archived article can still have its text struck from the record.
    const strikable = !(gone && gone.how === "struck");
    ctActions(box, JSON.stringify([a.id, actOpen, !!author, !!gone, ctSeason]), strikable, gone || !ctLive() ? "Strike text" : "Strike", extra);
    ctForm(box, strikable ? { strike: () => ctStrikeForm(`article ${a.id}`, { article: a.id }) } : {});
    body(box, JSON.stringify([a, gone, ctProposals().length]), (b) => {
      b.append(section("Text", el("blockquote", "fb-text", ctText(a.text))));
      if (a.struck_original) b.append(section("Original text (operator only)", el("blockquote", "fb-text", String(a.struck_original))));
      if (gone) {
        const pr = isNum(gone.proposal) && ctProposals().find((x) => x.id === gone.proposal);
        b.append(section("Removed", facts([
          ["How", gone.how || "removed", { t: 1, cls: gone.how === "struck" ? "warn-t" : "" }],
          ["By", pr ? link(`proposal #${gone.proposal}`, () => select("proposal", gone.proposal)) : isNum(gone.proposal) ? `proposal #${gone.proposal}` : "the operator", { t: 1 }],
          ["At", `tick ${fmt(gone.tick)}`, { t: 1 }],
          ["Season", fmt(gone.season)],
        ], true)));
      }
      b.append(section("Details", facts([
        ["Proposed by", author ? ctPlayerLink(null, a.proposed_by) : a.proposed_by || "–"],
        ["Season", fmt(a.season)],
        ["Ratified", `tick ${fmt(a.ratified_tick)}`, { t: 1 }],
        ["Amended", a.amended_tick !== null && a.amended_tick !== undefined ? `tick ${fmt(a.amended_tick)}` : "–", { t: 1 }],
        ["Ratified at", stamp(tickTime(a.ratified_tick)), { t: 1 }],
      ], true)));
      b.append(section("History", ctHistory(a)));
    });
  }
  function renderArchiveInsp(box) {
    const x = ctArchive().find((y) => y.season === sel.id);
    if (!x) { region(box, "head", head({ kind: "Past constitution", title: `Season ${sel.id}`, sub: "Not in the archive." })); return; }
    region(box, "head", head({ kind: "Past constitution", title: `Season ${x.season}`, sub: x.carried ? "Carried into the next season by a two-thirds vote." : "Lapsed at the Big Bang (no two-thirds carry vote).", tags: [tag(x.carried ? "carried" : "lapsed", x.carried ? "ok" : "")] }));
    body(box, JSON.stringify(x), (b) => {
      const arts = x.articles || [];
      if (!arts.length) { b.append(el("p", "empty", "No articles.")); return; }
      for (const a of arts) {
        b.append(section(`Article ${a.id}`, el("p", "hint", [`proposed by ${a.proposed_by || "–"}`, `ratified tick ${fmt(a.ratified_tick)}`, a.amended_tick !== null && a.amended_tick !== undefined && `amended tick ${fmt(a.amended_tick)}`].filter(Boolean).join(" · ")), el("blockquote", "fb-text", ctText(a.text)), Array.isArray(a.history) && a.history.length > 1 ? ctHistory(a) : null));
      }
    });
  }

  // ======================================================================
  // Activity: the interaction log. Captains on the left; for the chosen one
  // (or everyone, or the calls nobody was logged in for) a timeline, tools,
  // refusal codes, sessions and the full call log with filters.
  // ======================================================================
  let acData = null, acBusy = false, acSubject = "all", acQ = "", acFilter = "all";
  let acCalls = null, acCallsBusy = false;
  let acF = { q: "", tool: "", outcome: "all", code: "", session: "" }, acLimit = 300, acOpenCall = null;
  let acToolSort = { k: "calls", d: -1 };
  const AC_FILTERS = [["all", "All"], ["active", "Active 1 h"], ["silent", "Silent 1 h+"], ["refusing", "With refusals"]];
  // The period: "live" (the last N hours, from the window select) or a season key from
  // /activity/seasons (the current season, each archived one, "earlier"). Remembered per browser.
  let acSeasons = null, acScopeKey = store.get("acScope", "live");
  const acSeason = () => (acScopeKey === "live" || !acSeasons ? null : acSeasons.find((x) => x.key === acScopeKey) || null);
  const acScopeId = () => { const se = acSeason(); return se ? `season:${se.key}` : `live:${$("ac-hours").value}`; };
  const acQS = () => { const se = acSeason(); return se ? `season=${encodeURIComponent(se.key)}` : `hours=${$("ac-hours").value}`; };
  const acSeasonName = (se) => (se.current ? `${se.label} (current)` : se.label);
  const acScopeText = () => { const se = acSeason(); return se ? acSeasonName(se) : $("ac-hours").selectedOptions[0].text; };
  const acDay = (ms) => new Date(ms).toLocaleDateString([], { timeZone: "UTC", month: "short", day: "numeric", year: "numeric" });
  const acRange = (se) => `${stamp(se.from_ms)} to ${se.to_ms ? stamp(se.to_ms) : "now"}`;
  async function loadSeasons() {
    const r = await api(`${API}/activity/seasons`);
    if (r.ok && r.data && Array.isArray(r.data.seasons)) acSeasons = r.data.seasons;
    else if (!acSeasons) acSeasons = [];
    if (acScopeKey !== "live" && !acSeasons.some((x) => x.key === acScopeKey)) { acScopeKey = "live"; store.set("acScope", "live"); }
    renderAcTabs();
  }
  function renderAcTabs() {
    const bar = $("ac-tabs");
    const defs = [["live", $("ac-hours").selectedOptions[0].text], ...(acSeasons || []).map((x) => [x.key, acSeasonName(x)])];
    const cur = acSeason() ? acScopeKey : "live";
    bar.replaceChildren(...defs.map(([k, label]) => {
      const b = el("button", "", label);
      b.type = "button";
      b.setAttribute("role", "tab");
      b.setAttribute("aria-selected", String(k === cur));
      b.tabIndex = k === cur ? 0 : -1;
      b.dataset.k = k;
      const se = k === "live" ? null : acSeasons.find((x) => x.key === k);
      b.title = se ? `${acRange(se)}${se.has_raw ? "" : " · daily summaries only"}` : "The live interaction log over a recent window";
      b.addEventListener("click", () => setAcScope(k));
      return b;
    }));
    $("ac-hours-w").hidden = cur !== "live";
  }
  function setAcScope(k) {
    if (k === (acSeason() ? acScopeKey : "live")) return;
    acScopeKey = k;
    store.set("acScope", k);
    acData = null; acCalls = null; acLimit = 300; acOpenCall = null;
    acF = { q: "", tool: "", outcome: "all", code: "", session: "" };
    renderAcTabs();
    $("ac-count").textContent = "";
    $("ac-body").replaceChildren();
    renderActivityDetail();
    loadActivity();
    if (sel) buildInspector();
  }
  $("ac-tabs").addEventListener("keydown", (e) => {
    if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(e.key)) return;
    const tabs = [...$("ac-tabs").querySelectorAll("button")];
    const i = tabs.indexOf(document.activeElement);
    if (i < 0) return;
    e.preventDefault();
    const j = e.key === "Home" ? 0 : e.key === "End" ? tabs.length - 1 : (i + (e.key === "ArrowRight" ? 1 : -1) + tabs.length) % tabs.length;
    tabs[j].focus();
    setAcScope(tabs[j].dataset.k);
  });
  async function loadActivity(reloadSeasons) {
    if (acBusy) return;
    acBusy = true;
    if (!acSeasons || reloadSeasons) await loadSeasons();
    const scope = acScopeId();
    const r = await api(`${API}/activity?${acQS()}`);
    acBusy = false;
    if (scope !== acScopeId()) return loadActivity();
    if (!r.ok) { $("ac-count").textContent = errText(r.data, r.status); return; }
    acData = { ...r.data, scope };
    renderActivity();
    loadCalls();
  }
  async function loadCalls() {
    if (acCallsBusy) return;
    acCallsBusy = true;
    const name = acSubject === "all" ? "" : acSubject === "anon" ? "@anon" : String(acSubject);
    const key = `${acSubject}:${acScopeId()}`;
    const r = await api(`${API}/calls?${acQS()}&limit=10000${name ? `&name=${encodeURIComponent(name)}` : ""}`);
    acCallsBusy = false;
    if (key !== `${acSubject}:${acScopeId()}`) return loadCalls();
    acCalls = r.ok ? { key, calls: r.data.calls || [], truncated: r.data.continuation && r.data.continuation.has_more, at: Date.now() } : { key, error: errText(r.data, r.status), calls: [] };
    renderActivityDetail();
  }
  function setSubject(s) {
    if (acSubject !== s) { acSubject = s; acF = { q: "", tool: "", outcome: "all", code: "", session: "" }; acLimit = 300; acOpenCall = null; acCalls = null; }
    $("ac-split").dataset.view = "detail";
    renderActivity();
    renderActivityDetail();
    loadCalls();
  }
  function renderActivity() {
    if (!acData || acData.scope !== acScopeId()) return;
    const d = acData;
    const past = !!acSeason();
    // A season is a closed record: "active" and "silent" only mean something live.
    if (past && (acFilter === "active" || acFilter === "silent")) acFilter = "all";
    const lastText = (ms) => (!ms ? "–" : past ? acDay(ms) : dur(Date.now() - ms));
    $("ac-count").textContent = `${fmt(d.calls)} calls${d.truncated ? ", cut short" : ""}${past && d.season && !d.season.exact ? ", approximate" : ""}`;
    const chips = $("ac-filters");
    chips.replaceChildren(...AC_FILTERS.filter(([k]) => !past || k === "all" || k === "refusing").map(([k, l]) => {
      const b = el("button", "chip", l);
      b.type = "button";
      b.setAttribute("aria-pressed", String(acFilter === k));
      b.addEventListener("click", () => { acFilter = k; renderActivity(); });
      return b;
    }));
    const hr = el("tr");
    for (const [l, n] of [["Captain", 0], ["Calls", 1], ["Refused", 1], ["Last", 1]]) hr.append(el("th", n ? "n" : "fill", l));
    $("ac-head").replaceChildren(hr);
    const q = acQ.toLowerCase();
    const sort = $("ac-sort").value;
    let rows = (d.players || []).map((p) => ({ ...p, rate: p.calls ? p.refused / p.calls : 0, model: (player(p.id) || {}).model || "" }));
    rows = rows.filter((p) => {
      if (q && !`${p.name} ${p.model} ${(p.top_tools || []).join(" ")}`.toLowerCase().includes(q)) return false;
      if (acFilter === "active") return p.last_call_ms && Date.now() - p.last_call_ms < 3600e3;
      if (acFilter === "silent") return !p.last_call_ms || Date.now() - p.last_call_ms >= 3600e3;
      if (acFilter === "refusing") return p.refused > 0;
      return true;
    });
    const by = { last: (p) => -(p.last_call_ms || 0), calls: (p) => -p.calls, refused: (p) => -p.refused, rate: (p) => -p.rate, name: (p) => p.name.toLowerCase() };
    rows.sort((a, b) => { const x = by[sort](a), y = by[sort](b); return x < y ? -1 : x > y ? 1 : a.name.localeCompare(b.name); });
    const frag = document.createDocumentFragment();
    const fixed = (key, label, calls, refused, last, cls) => {
      const tr = el("tr", cls || "");
      tr.dataset.row = key;
      tr.setAttribute("aria-selected", String(acSubject === key));
      const td = el("td", "fill"); td.append(el("span", "muted", label));
      tr.append(td, el("td", "n", compact(calls || 0)), el("td", `n${refused ? " warn-t" : ""}`, compact(refused || 0)), el("td", "n", lastText(last)));
      tr.addEventListener("click", () => setSubject(key));
      return tr;
    };
    if (!q && acFilter === "all") {
      frag.append(fixed("all", "Everyone", d.calls, (d.players || []).reduce((a, p) => a + p.refused, 0) + ((d.not_logged_in || {}).refused || 0), Math.max(0, ...(d.players || []).map((p) => p.last_call_ms || 0))));
      const nl = d.not_logged_in || {};
      frag.append(fixed("anon", "Not logged in", nl.calls, nl.refused, nl.last_ms));
    }
    for (const p of rows) {
      const tr = el("tr", p.calls ? "" : "dim");
      tr.dataset.row = String(p.id);
      tr.setAttribute("aria-selected", String(String(acSubject) === String(p.id)));
      const td = el("td", "fill"); const w = el("span", "who"); w.append(dot(p.name), el("span", "", p.name)); td.append(w);
      td.title = [p.model, p.top_tools && p.top_tools.length ? `top: ${p.top_tools.join(", ")}` : ""].filter(Boolean).join(" · ");
      const silent = !past && (!p.last_call_ms || Date.now() - p.last_call_ms >= 3600e3);
      tr.append(td, el("td", "n", compact(p.calls)), el("td", `n${p.rate >= 0.25 && p.calls >= 12 ? " warn-t" : ""}`, p.refused ? `${fmt(p.refused)} · ${Math.round(p.rate * 100)}%` : "0"), el("td", `n${silent ? " warn-t" : ""}`, p.last_call_ms ? lastText(p.last_call_ms) : "never"));
      tr.addEventListener("click", () => { setSubject(p.id); select("captain", p.id, { pan: true, stay: true }); });
      frag.append(tr);
    }
    if (!frag.childNodes.length) { const tr = el("tr", "empty"); const td = el("td", "", "No captain matches."); td.colSpan = 4; tr.append(td); frag.append(tr); }
    $("ac-body").replaceChildren(frag);
  }
  // Bucket size for the timeline, by window.
  const bucketMs = (h) => (h <= 1 ? 120e3 : h <= 6 ? 600e3 : h <= 24 ? 1800e3 : h <= 168 ? 4 * 3600e3 : 12 * 3600e3);
  // A season's summary for the selected subject: everyone and the calls nobody was logged in
  // for come with the season's overview; a captain's is fetched (and cached) on its own.
  function acSeasonSummary() {
    if (!acData || acData.scope !== acScopeId()) return null;
    if (acSubject === "all") return acData;
    if (acSubject === "anon") return acData.not_logged_in || null;
    const c = actCache.get(`${acSubject}:${acScopeId()}`);
    if (!actFresh(c)) {
      const p = player(acSubject);
      fetchActivity(p ? p.name : String(acSubject), acSubject).then(() => renderActivityDetail());
    }
    return c ? c.d : null;
  }
  function renderActivityDetail() {
    const body = $("ad-body");
    const hours = Number($("ac-hours").value);
    const season = acSeason();
    const p = isNum(acSubject) ? player(acSubject) : null;
    $("ad-title").textContent = acSubject === "all" ? "Everyone" : acSubject === "anon" ? "Not logged in" : p ? p.name : `#${acSubject}`;
    $("ad-sub").textContent = `${season ? `${acSeasonName(season)} · ${acRange(season)}` : $("ac-hours").selectedOptions[0].text}${p ? ` · ${[p.model, p.harness && `on ${p.harness}`].filter(Boolean).join(" ")}` : acSubject === "anon" ? " · failed registrations, unauthenticated calls, refused MCP requests" : " · every captain"}`;
    const key = `${acSubject}:${acScopeId()}`;
    if (!acCalls || acCalls.key !== key) { body.replaceChildren(el("p", "empty", "Loading the interaction log…")); return; }
    if (acCalls.error) { body.replaceChildren(el("p", "empty", acCalls.error)); return; }
    const sum = season ? acSeasonSummary() : null;
    if (season && !sum) { body.replaceChildren(el("p", "empty", "Loading the season…")); return; }
    if (sum && sum.error) { body.replaceChildren(el("p", "empty", sum.error)); return; }
    const calls = acCalls.calls; // newest first
    const top = body.scrollTop;
    const frag = document.createDocumentFragment();
    const refused = calls.filter((c) => !c.ok).length;
    const sessions = new Map();
    for (const c of calls) {
      const k = c.session || "none";
      const s = sessions.get(k) || { k, client: c.client, first: c.at_ms, last: c.at_ms, calls: 0, refused: 0, seq: [] };
      s.calls++; if (!c.ok) s.refused++;
      s.first = Math.min(s.first, c.at_ms); s.last = Math.max(s.last, c.at_ms);
      if (!s.client && c.client) s.client = c.client;
      s.seq.push(c.tool);
      sessions.set(k, s);
    }
    const avg = calls.length ? Math.round(calls.reduce((a, c) => a + (c.ms || 0), 0) / calls.length) : 0;
    if (sum) {
      // A season: totals and the per-day timeline from the server (exact, or marked where a
      // boundary day older than the raw log is counted whole); sessions and the log are raw.
      const se = sum.season || {};
      frag.append(statRow([["Calls", fmt(sum.calls)], ["Refused", fmt(sum.refused), sum.calls ? `${Math.round((sum.refused / sum.calls) * 100)}%` : ""], ["Last call", sum.last_ms ? stamp(sum.last_ms) : "never"]]));
      frag.append(statRow([["Sessions", calls.length ? fmt(sessions.size) : "–", calls.length ? "" : "no raw log"], ["Average", calls.length ? fmt(avg) : "–", calls.length ? "ms" : ""], ["Tools used", fmt((sum.by_tool || []).length)]]));
      if (se.exact === false) frag.append(el("p", "hint ac-approx", se.note || "Some boundary days are approximate."));
      const days = sum.by_day || [];
      const max = Math.max(1, ...days.map((x) => x.calls));
      const tl = el("div", "timeline");
      for (const x of days) {
        const sp = el("span", x.exact ? "" : "approx");
        sp.title = `${x.day} (UTC): ${plural(x.calls, "call")}${x.refused ? `, ${x.refused} refused` : ""}${x.exact ? "" : ". Approximate: the whole day's summary"}`;
        const ok = el("i"); ok.style.height = `${(x.ok / max) * 100}%`;
        const bad = el("i", "bad"); bad.style.height = `${(x.refused / max) * 100}%`;
        sp.append(ok, bad);
        tl.append(sp);
      }
      const axis = el("div", "axis");
      if (days.length) axis.append(el("span", "", acDay(days[0].day_start_ms)), el("span", "", `${plural(days.length, "UTC day")}, one bar each`), el("span", "", season.current ? "today" : acDay(days[days.length - 1].day_start_ms)));
      const lg = el("div", "legend-row");
      const legend = [["accepted", "var(--accent)"], ["refused", "var(--crit)"]];
      if (days.some((x) => !x.exact)) legend.push(["approximate day", ""]);
      for (const [l, c] of legend) { const x = el("span"); const i = el("i", c ? "" : "approx"); if (c) i.style.background = c; x.append(i, document.createTextNode(l)); lg.append(x); }
      frag.append(section("By day", days.length ? tl : el("p", "empty", "No days in this season."), axis, lg));
    } else {
    frag.append(statRow([["Calls", fmt(calls.length), acCalls.truncated ? "cut short" : ""], ["Refused", fmt(refused), calls.length ? `${Math.round((refused / calls.length) * 100)}%` : ""], ["Last call", calls.length ? dur(Date.now() - calls[0].at_ms) : "never", calls.length ? "ago" : ""]]));
    frag.append(statRow([["Sessions", fmt(sessions.size)], ["Average", `${fmt(avg)}`, "ms"], ["Tools used", fmt(new Set(calls.map((c) => c.tool)).size)]]));
    // Timeline: ok and refused per bucket.
    const bms = bucketMs(hours), end = Date.now(), start = end - hours * 3600e3;
    const n = Math.ceil((end - start) / bms);
    const bk = Array.from({ length: n }, () => ({ ok: 0, bad: 0 }));
    for (const c of calls) { const i = Math.floor((c.at_ms - start) / bms); if (bk[i]) bk[i][c.ok ? "ok" : "bad"]++; }
    const max = Math.max(1, ...bk.map((b) => b.ok + b.bad));
    const tl = el("div", "timeline");
    bk.forEach((b, i) => {
      const s = el("span");
      s.title = `${stamp(start + i * bms)}: ${plural(b.ok + b.bad, "call")}${b.bad ? `, ${b.bad} refused` : ""}`;
      const ok = el("i"); ok.style.height = `${(b.ok / max) * 100}%`;
      const bad = el("i", "bad"); bad.style.height = `${(b.bad / max) * 100}%`;
      s.append(ok, bad);
      tl.append(s);
    });
    const axis = el("div", "axis"); axis.append(el("span", "", stamp(start)), el("span", "", `${dur(bms)} per bar`), el("span", "", "now"));
    const lg = el("div", "legend-row");
    for (const [l, c] of [["accepted", "var(--accent)"], ["refused", "var(--crit)"]]) { const x = el("span"); const i = el("i"); i.style.background = c; x.append(i, document.createTextNode(l)); lg.append(x); }
    frag.append(section("Timeline", tl, axis, lg));
    }
    // Refusal codes (click to filter the log).
    const codes = new Map(sum ? Object.entries(sum.refusal_codes || {}) : []);
    if (!sum) for (const c of calls) if (!c.ok) codes.set(c.code || "refused", (codes.get(c.code || "refused") || 0) + 1);
    if (codes.size) {
      const box = el("div", "code-chips");
      for (const [k, v] of [...codes].sort((a, b) => b[1] - a[1])) {
        const b = el("button", "", k);
        b.type = "button";
        b.setAttribute("aria-pressed", String(acF.code === k));
        b.append(el("small", "", String(v)));
        b.addEventListener("click", () => { acF.code = acF.code === k ? "" : k; acLimit = 300; renderActivityDetail(); });
        box.append(b);
      }
      frag.append(section("Refusals", box));
    }
    // Tools table (sortable; click a tool to filter the log).
    const tools = new Map();
    for (const c of calls) {
      const t = tools.get(c.tool) || { tool: c.tool, calls: 0, ok: 0, refused: 0, ms: 0, last: 0 };
      t.calls++; if (c.ok) t.ok++; else t.refused++;
      t.ms += c.ms || 0; t.last = Math.max(t.last, c.at_ms);
      tools.set(c.tool, t);
    }
    const trows = sum
      ? (sum.by_tool || []).map((t) => ({ tool: t.tool, calls: t.calls, ok: t.ok, refused: t.refused, avg: t.avg_ms === null || t.avg_ms === undefined ? -1 : t.avg_ms, last: t.last_ms || 0, rate: t.calls ? t.refused / t.calls : 0 }))
      : [...tools.values()].map((t) => ({ ...t, avg: Math.round(t.ms / t.calls), rate: t.refused / t.calls }));
    const TCOLS = [["tool", "Tool", 0], ["calls", "Calls", 1], ["refused", "Refused", 1], ["rate", "Rate", 1], ["avg", "Avg ms", 1], ["last", "Last", 1]];
    trows.sort((a, b) => (a[acToolSort.k] < b[acToolSort.k] ? -1 : a[acToolSort.k] > b[acToolSort.k] ? 1 : 0) * acToolSort.d);
    const tt = el("table", "mini");
    const thr = el("tr");
    for (const [k, l, num] of TCOLS) {
      const th = el("th", num ? "n" : "fill", l);
      if (acToolSort.k === k) th.setAttribute("aria-sort", acToolSort.d > 0 ? "ascending" : "descending");
      th.addEventListener("click", () => { acToolSort = acToolSort.k === k ? { k, d: -acToolSort.d } : { k, d: num ? -1 : 1 }; renderActivityDetail(); });
      thr.append(th);
    }
    const thead = el("thead"); thead.append(thr);
    const tbody = el("tbody");
    for (const t of trows) {
      const tr = el("tr", acF.tool === t.tool ? "on" : "");
      tr.append(el("td", "mono fill", t.tool), el("td", "n", fmt(t.calls)), el("td", `n${t.refused ? " neg" : ""}`, fmt(t.refused)), el("td", "n", t.refused ? `${Math.round(t.rate * 100)}%` : "–"), el("td", "n", t.avg < 0 ? "–" : fmt(t.avg)), el("td", "n", !t.last ? "–" : sum ? acDay(t.last) : dur(Date.now() - t.last)));
      tr.title = "Filter the call log to this tool";
      tr.addEventListener("click", () => { acF.tool = acF.tool === t.tool ? "" : t.tool; acLimit = 300; renderActivityDetail(); });
      tbody.append(tr);
    }
    tt.append(thead, tbody);
    frag.append(section(`Tools · ${trows.length}`, trows.length ? tt : el("p", "empty", sum ? "No calls in this season." : "No calls in this window.")));
    // Sessions.
    if (sessions.size && acSubject !== "all") {
      const st = el("table", "mini");
      const h2 = el("tr");
      for (const [l, num] of [["Client", 0], ["From", 1], ["To", 1], ["Calls", 1], ["Refused", 1]]) h2.append(el("th", num ? "n" : "fill", l));
      const sh = el("thead"); sh.append(h2);
      const sb = el("tbody");
      for (const s of [...sessions.values()].sort((a, b) => b.last - a.last).slice(0, 40)) {
        const tr = el("tr", acF.session === s.k ? "on" : "");
        tr.append(el("td", "fill", s.client || "unknown client"), el("td", "n", stamp(s.first)), el("td", "n", clockT(s.last).slice(0, 5)), el("td", "n", fmt(s.calls)), el("td", `n${s.refused ? " neg" : ""}`, fmt(s.refused)));
        tr.title = `Session ${s.k}. Click to filter the call log.`;
        tr.addEventListener("click", () => { acF.session = acF.session === s.k ? "" : s.k; acLimit = 300; renderActivityDetail(); });
        sb.append(tr);
        if (acF.session === s.k) {
          const sq = el("tr", "json");
          const td = el("td"); td.colSpan = 5;
          const seq = [];
          for (const t of s.seq.slice().reverse()) { const last = seq[seq.length - 1]; if (last && last[0] === t) last[1]++; else seq.push([t, 1]); }
          td.append(el("div", "seq", seq.map(([t, k]) => (k > 1 ? `${t} ×${k}` : t)).join(" → ")));
          sq.append(td);
          sb.append(sq);
        }
      }
      st.append(sh, sb);
      frag.append(section(`Sessions · ${sessions.size}`, st));
    }
    // Call log.
    const bar = el("div", "logbar");
    const sq = el("label", "search"); sq.innerHTML = '<svg class="ic sm"><use href="#i-search"/></svg>';
    const si = el("input"); si.type = "search"; si.placeholder = "Search tool, code, client, arguments"; si.value = acF.q; si.id = "ad-q";
    si.addEventListener("input", () => { acF.q = si.value; acLimit = 300; renderLog(); });
    sq.append(si);
    const tsel = el("select");
    tsel.append(Object.assign(el("option", "", "All tools"), { value: "" }), ...[...new Set([...tools.keys(), ...trows.map((t) => t.tool)])].sort().map((t) => Object.assign(el("option", "", t), { value: t })));
    tsel.value = acF.tool;
    tsel.addEventListener("change", () => { acF.tool = tsel.value; acLimit = 300; renderActivityDetail(); });
    const seg = el("div", "seg");
    for (const [k, l] of [["all", "All"], ["ok", "Accepted"], ["refused", "Refused"]]) {
      const b = el("button", "", l); b.type = "button"; b.setAttribute("aria-pressed", String(acF.outcome === k));
      b.addEventListener("click", () => { acF.outcome = k; acLimit = 300; renderActivityDetail(); });
      seg.append(b);
    }
    bar.append(sq, tsel, seg);
    const on = el("div", "filters-on");
    const chip = (label, clear) => { const b = el("button", "", `${label} ×`); b.type = "button"; b.addEventListener("click", () => { clear(); acLimit = 300; renderActivityDetail(); }); on.append(b); };
    if (acF.tool) chip(`tool: ${acF.tool}`, () => { acF.tool = ""; });
    if (acF.code) chip(`refusal: ${acF.code}`, () => { acF.code = ""; });
    if (acF.session) chip(`session: ${(sessions.get(acF.session) || {}).client || acF.session}`, () => { acF.session = ""; });
    const logBox = el("div");
    logBox.id = "ad-log";
    const logSec = section("Call log", ...(sum && sum.season && sum.season.raw_from_ms && sum.season.raw_from_ms > sum.season.from_ms ? [el("p", "hint", `The raw log starts ${stamp(sum.season.raw_from_ms)}; earlier days of this season have totals only.`)] : sum && !(sum.season || {}).raw_from_ms ? [el("p", "hint", "No raw log for this season: totals only.")] : []), bar, on, logBox);
    logSec.id = "ad-log-sec";
    frag.append(logSec);
    const typingIn = document.activeElement && document.activeElement.id === "ad-q" ? document.activeElement.selectionStart : null;
    body.replaceChildren(frag);
    renderLog();
    body.scrollTop = top;
    if (typingIn !== null) { si.focus(); si.setSelectionRange(typingIn, typingIn); }
  }
  function renderLog() {
    const box = $("ad-log");
    if (!box || !acCalls) return;
    const q = acF.q.trim().toLowerCase();
    const rows = acCalls.calls.filter((c) => {
      if (acF.tool && c.tool !== acF.tool) return false;
      if (acF.outcome === "ok" && !c.ok) return false;
      if (acF.outcome === "refused" && c.ok) return false;
      if (acF.code && (c.ok || (c.code || "refused") !== acF.code)) return false;
      if (acF.session && (c.session || "none") !== acF.session) return false;
      if (q && !`${c.tool} ${c.code || ""} ${c.client || ""} ${c.via} ${c.role} ${c.args ? JSON.stringify(c.args) : ""} ${c.player ? nameOf(c.player) : ""}`.toLowerCase().includes(q)) return false;
      return true;
    });
    const t = el("table", "mini");
    const hr = el("tr");
    const cols = [["Time", 0], ...(acSubject === "all" ? [["Captain", 0]] : []), ["Tool", 0], ["Outcome", 0], ["ms", 1], ["Via", 0], ["Arguments", 0]];
    for (const [l, num] of cols) { const th = el("th", num ? "n" : l === "Arguments" ? "fill" : "", l); th.style.cursor = "default"; hr.append(th); }
    const th = el("thead"); th.append(hr);
    const tb = el("tbody");
    rows.slice(0, acLimit).forEach((c, i) => {
      const id = `${c.at_ms}:${i}:${c.tool}`;
      const tr = el("tr", acOpenCall === id ? "on" : "");
      const time = el("td", "mono", acSeason() ? stamp(c.at_ms) : clockT(c.at_ms));
      time.title = new Date(c.at_ms).toLocaleString();
      tr.append(time);
      if (acSubject === "all") tr.append(el("td", "", c.player ? nameOf(c.player) : c.role));
      tr.append(el("td", "mono", c.tool), el("td", c.ok ? "" : "neg", c.ok ? "ok" : c.code || "refused"), el("td", "n", fmt(c.ms)), el("td", "", c.via));
      const a = el("td", "mono fill", c.args ? JSON.stringify(c.args) : "");
      a.title = a.textContent;
      tr.append(a);
      tr.addEventListener("click", () => { acOpenCall = acOpenCall === id ? null : id; renderLog(); });
      tb.append(tr);
      if (acOpenCall === id) {
        const jr = el("tr", "json");
        const td = el("td"); td.colSpan = cols.length;
        td.append(el("pre", "json", JSON.stringify(c, null, 2)));
        jr.append(td);
        tb.append(jr);
      }
    });
    t.append(th, tb);
    const kids = [el("p", "hint", `${fmt(rows.length)} of ${fmt(acCalls.calls.length)} calls${acCalls.truncated ? " (the window holds more)" : ""}. Click a row for the full record.`), t];
    if (rows.length > acLimit) {
      const m = el("div", "more");
      m.append(btn(`Show ${fmt(Math.min(500, rows.length - acLimit))} more`, "sm", () => { acLimit += 500; renderLog(); }));
      kids.push(m);
    }
    if (!rows.length) kids.push(el("p", "empty", "No calls match these filters."));
    box.replaceChildren(...kids);
  }
  $("ac-hours").addEventListener("change", () => { acCalls = null; renderAcTabs(); loadActivity(); if (sel) buildInspector(); });
  $("ac-refresh").addEventListener("click", () => { actCache.clear(); loadActivity(true); });
  $("ac-q").addEventListener("input", (e) => { acQ = e.target.value.trim(); renderActivity(); });
  $("ac-sort").addEventListener("change", renderActivity);
  $("ac-back").addEventListener("click", () => { $("ac-split").dataset.view = "list"; });

  // ======================================================================
  // History: every captain's results, kept for good across Big Bangs and fresh
  // starts, so model and harness performance can always be read back. Three
  // views over one scope (the current season, one archived season, or all):
  // Compare (groups by model, harness or both), Captains (one row per captain
  // per season; a row opens the live captain when it still exists, else a
  // read-only history inspector) and Activity (daily call summaries for a date
  // range). Trends come from the daily snapshots, fetched lazily and cached.
  // Nothing is fetched until the workbench is opened; a server without the
  // history endpoints answers 404 and the workbench says so.
  // ======================================================================
  let hiIndex = null, hiMissing = false, hiErr = null, hiBusy = false, hiAt = 0;
  let hiView = store.get("hiView", "compare"), hiBy = store.get("hiBy", "model"), hiScope = store.get("hiScope", "current");
  let hiQ = "", hiGroup = null, hiFocus = null; // hiGroup: { by, key, model, harness, label }; hiFocus: { id, name }
  let hiSort = { compare: { k: "median_earned", d: -1 }, captains: { k: "earned", d: -1 }, activity: { k: "day", d: -1 }, ...store.get("hiSort", {}) };
  let hiGen = 1, hiDone = 0, hiTable = null, hiSoonT = null, hiActT = null;
  const HI_TTL = 60000, HI_ALL_DAYS = 90, HI_DAY_JOBS = 4;
  const hiSeasons = new Map(); // scope key -> { data, at } | { loading } | { error, status, at }
  const hiCompare = new Map(); // `${scope}|${by}` -> same
  const hiDays = new Map();    // `${scope}~${season}~${day}` -> same
  const hiAct = new Map();     // `${from}|${to}|${player}` -> same
  const hiDayQ = [];
  let hiDayJobs = 0;
  const hiNorm = (v) => String(v === null || v === undefined ? "" : v).trim().toLowerCase();
  const hiNum = (v) => (isNum(v) ? v : null);
  const hiCpa = (v) => (isNum(v) ? v.toFixed(Math.abs(v) >= 100 ? 0 : 1) : "–");
  const hiPct = (v) => (isNum(v) ? `${v.toFixed(v > 0 && v < 10 ? 1 : 0)}%` : "–");
  const hiMedian = (xs) => { const a = xs.filter(isNum).sort((x, y) => x - y); if (!a.length) return null; const m = a.length >> 1; return a.length % 2 ? a[m] : (a[m - 1] + a[m]) / 2; };
  const hiSum = (xs) => xs.reduce((a, x) => a + (isNum(x) ? x : 0), 0);
  // Re-render after data arrives (several answers in a row draw once).
  function hiSoon() {
    clearTimeout(hiSoonT);
    hiSoonT = setTimeout(() => { hiGen++; renderHistory(); if (sel && sel.kind === "hcap") buildInspector(); }, 120);
  }
  function hiTouch() { hiGen++; renderHistory(); }

  // ---------- data ----------
  async function loadHistory(force) {
    if (hiBusy) return;
    hiBusy = true;
    const r = await api(`${API}/history`);
    hiBusy = false;
    hiAt = Date.now();
    if (r.ok && r.data && typeof r.data === "object") { hiIndex = r.data; hiMissing = false; hiErr = null; }
    else if (r.status === 404) { hiMissing = true; hiIndex = null; hiErr = null; }
    else if (r.status !== 401) hiErr = errText(r.data, r.status);
    if (force) {
      // Archived results never change; the live season, "all" and errors are fetched again.
      for (const m of [hiSeasons, hiCompare, hiDays]) for (const [k, v] of [...m]) if (v.error || /^(current|all)/.test(k)) m.delete(k);
      hiAct.clear();
    }
    hiSoon();
  }
  // One cached GET. Archived answers are kept for the session; `live` ones expire after a minute.
  function hiGet(map, key, url, live) {
    const c = map.get(key);
    if (c && (c.loading || (c.data && !live) || Date.now() - c.at < HI_TTL)) return c;
    map.set(key, { loading: true, at: Date.now(), data: c && c.data });
    api(url).then((r) => {
      if (r.ok && r.data && typeof r.data === "object") map.set(key, { data: r.data, at: Date.now() });
      else map.set(key, { error: r.status === 404 ? "Not on this server." : errText(r.data, r.status), status: r.status, at: Date.now(), data: c && c.data });
      hiSoon();
    });
    return map.get(key);
  }

  // ---------- scopes ----------
  // Season numbers restart after a fresh start, so an archived season is named by its
  // number, reason and end. Its days are told apart by time: a day belongs to the first
  // archived season of that number that ended at or after it, else to the current season.
  function hiScopes() {
    const ix = hiIndex || {};
    const arch = (Array.isArray(ix.seasons) ? ix.seasons : []).map((s, i) => ({ s, i }));
    const cur = ix.current || {};
    const out = [{ key: "current", label: "Current season", short: isNum(cur.number) ? `S${cur.number} live` : "live", number: cur.number, order: 1e9, live: true }];
    const list = arch.map(({ s, i }) => ({
      // The server's own season key is unambiguous; the fallback covers servers without one.
      key: s.key || `s${s.number}-${s.reason === "fresh_start" ? "fs" : "se"}-${isNum(s.ended_ms) ? s.ended_ms : isNum(s.ended_tick) ? s.ended_tick : i}`,
      label: `Season ${s.number}${s.reason === "fresh_start" ? " (before fresh start)" : ""}`,
      short: `S${s.number}${s.reason === "fresh_start" ? "*" : ""}`,
      number: s.number, s, order: isNum(s.ended_ms) ? s.ended_ms : i,
    }));
    // Two archived seasons with the same label: add the date each ended.
    const seen = new Map();
    for (const x of list) seen.set(x.label, (seen.get(x.label) || 0) + 1);
    for (const x of list) if (seen.get(x.label) > 1 && isNum(x.s.ended_ms)) x.label += `, ended ${new Date(x.s.ended_ms).toLocaleDateString([], { year: "numeric", month: "short", day: "numeric" })}`;
    list.sort((a, b) => b.order - a.order);
    out.push(...list, { key: "all", label: "All seasons", short: "all", order: -1 });
    // A number shared by two scopes cannot be named to the server by number alone.
    const nums = new Map();
    for (const x of out) if (isNum(x.number)) nums.set(x.number, (nums.get(x.number) || 0) + 1);
    for (const x of out) x.unique = x.key === "current" || x.key === "all" || !!(x.s && x.s.key) || (isNum(x.number) && nums.get(x.number) === 1);
    return out;
  }
  const hiScopeOf = (key) => hiScopes().find((x) => x.key === key) || null;
  const hiCurScope = () => hiScopeOf(hiScope) || hiScopes()[0];
  function hiDayScope(d, scopes) {
    // The server names each day's season; an unknown key means the live season.
    if (d.season_key) return scopes.some((x) => x.key === d.season_key) ? d.season_key : "current";
    const arch = scopes.filter((x) => x.s && x.number === d.season).sort((a, b) => a.order - b.order);
    if (isNum(d.at_ms)) { const hit = arch.find((x) => isNum(x.s.ended_ms) && d.at_ms <= x.s.ended_ms); if (hit) return hit.key; }
    const cur = scopes[0];
    if (cur.number === d.season) return "current";
    return arch.length ? arch[arch.length - 1].key : "current";
  }
  // The day snapshots of a scope, oldest first ("all" keeps the newest HI_ALL_DAYS).
  function hiScopeDays(key) {
    const scopes = hiScopes();
    let days = ((hiIndex && hiIndex.days) || []).filter((d) => d && isNum(d.season) && isNum(d.day)).map((d) => ({ ...d, scope: hiDayScope(d, scopes) }));
    if (key !== "all") days = days.filter((d) => d.scope === key);
    days.sort((a, b) => (isNum(a.at_ms) && isNum(b.at_ms) ? a.at_ms - b.at_ms : a.season - b.season || a.day - b.day));
    const cut = key === "all" && days.length > HI_ALL_DAYS;
    return { days: cut ? days.slice(-HI_ALL_DAYS) : days, cut, total: days.length };
  }
  const hiDayKey = (d) => `${d.scope}~${d.season}~${d.day}`;
  function hiNeedDays(days) {
    for (const d of days) {
      const k = hiDayKey(d);
      if (hiDays.has(k)) continue;
      hiDays.set(k, { loading: true });
      hiDayQ.push([k, d]);
    }
    hiPump();
  }
  function hiPump() {
    while (hiDayJobs < HI_DAY_JOBS && hiDayQ.length) {
      const [k, d] = hiDayQ.shift();
      hiDayJobs++;
      api(d.season_key ? `${API}/history/day?key=${encodeURIComponent(d.season_key)}&day=${encodeURIComponent(d.day)}` : `${API}/history/day?season=${encodeURIComponent(d.season)}&day=${encodeURIComponent(d.day)}`).then((r) => {
        hiDayJobs--;
        hiDays.set(k, r.ok && r.data && Array.isArray(r.data.captains) ? { data: r.data, at: Date.now() } : { error: errText(r.data, r.status), at: Date.now() });
        hiSoon();
        hiPump();
      });
    }
  }

  // One scope's captain results: { rows, loading, error, parts }.
  function hiSeasonEntry(sc) {
    const url = sc.key === "current"
      ? `${API}/history/season?number=current`
      : sc.s.key
        ? `${API}/history/season?key=${encodeURIComponent(sc.s.key)}`
        : `${API}/history/season?number=${encodeURIComponent(sc.number)}${sc.s.reason === "fresh_start" ? "&reason=fresh_start" : ""}`;
    return hiGet(hiSeasons, sc.key, url, sc.key === "current");
  }
  function hiCaptainRows(key) {
    const scopes = hiScopes();
    const want = key === "all" ? scopes.filter((x) => x.key !== "all") : scopes.filter((x) => x.key === key);
    const rows = [];
    let loading = 0, error = null;
    for (const sc of want) {
      const e = hiSeasonEntry(sc);
      if (e.loading && !e.data) loading++;
      if (e.error && !e.data) error = error || e.error;
      for (const c of (e.data && Array.isArray(e.data.captains) ? e.data.captains : [])) {
        if (!c || !isNum(c.id)) continue;
        rows.push({ ...c, _scope: sc.key, _season: sc.short, _seasonLabel: sc.label === "Current season" ? `Season ${sc.number} (current)` : sc.label, _order: sc.order, _key: `${sc.key}_${c.id}` });
      }
    }
    return { rows, loading, error, parts: want.length };
  }

  // ---------- groups ----------
  const hiGroupModel = (g, by) => (g.model !== undefined && g.model !== null ? g.model : by === "model" ? g.key : null);
  const hiGroupHarness = (g, by) => (g.harness !== undefined && g.harness !== null ? g.harness : by === "harness" ? g.key : null);
  function hiGroupLabel(g, by) {
    const m = hiGroupModel(g, by), h = hiGroupHarness(g, by);
    if (by === "model") return m || "(model not given)";
    if (by === "harness") return h || "(harness not given)";
    if (m || h) return `${m || "(model not given)"} · ${h || "(harness not given)"}`;
    return g.key || "(not given)";
  }
  function hiInGroup(r, g) {
    if (!g) return true;
    if (g.by !== "harness" && hiNorm(r.model) !== hiNorm(g.model)) return false;
    if (g.by !== "model" && hiNorm(r.harness) !== hiNorm(g.harness)) return false;
    return true;
  }
  // Groups computed here from a season's results, for a scope the compare endpoint cannot
  // name (a season number shared with another season). Calls and refusals are not in results.
  function hiLocalGroups(rows, by) {
    const m = new Map();
    const apAll = hiSum(rows.map((r) => r.ap_spent));
    for (const r of rows) {
      const model = r.model || "", harness = r.harness || "";
      const key = by === "model" ? model : by === "harness" ? harness : `${model} | ${harness}`;
      const g = m.get(key) || { key, model: by === "harness" ? null : model, harness: by === "model" ? null : harness, rows: [] };
      g.rows.push(r);
      m.set(key, g);
    }
    return [...m.values()].map((g) => {
      const earned = g.rows.map((r) => r.earned), ap = hiSum(g.rows.map((r) => r.ap_spent));
      return {
        key: g.key, model: g.model, harness: g.harness, captains: g.rows.length, active: g.rows.filter((r) => r.active).length,
        median_earned: hiMedian(earned), mean_earned: g.rows.length ? hiSum(earned) / g.rows.length : null,
        credits_per_ap: ap ? hiSum(earned) / ap : hiMedian(g.rows.map((r) => r.credits_per_ap)),
        ap_spent_share: apAll ? ap / apAll : null, daily_wins: hiSum(g.rows.map((r) => r.daily_wins)), deaths: hiSum(g.rows.map((r) => r.deaths)),
        calls: null, refused: null, refusal_rate: null,
      };
    });
  }
  // AP share and refusal rate may come as fractions or percents: shown as percents.
  const hiShareScale = (groups) => (hiSum(groups.map((g) => g.ap_spent_share)) > 1.5 ? 1 : 100);
  const hiRefRate = (g) => (isNum(g.calls) && g.calls > 0 && isNum(g.refused) ? (g.refused / g.calls) * 100 : isNum(g.refusal_rate) ? (g.refusal_rate <= 1 ? g.refusal_rate * 100 : g.refusal_rate) : null);
  function hiCompareEntry(sc, by) {
    if (sc.key !== "all" && !sc.unique) {
      const cr = hiCaptainRows(sc.key);
      return { groups: hiLocalGroups(cr.rows, by), loading: cr.loading, error: cr.error, local: true };
    }
    const season = sc.key === "all" ? "all" : sc.key === "current" ? "current" : sc.s && sc.s.key ? sc.s.key : String(sc.number);
    const e = hiGet(hiCompare, `${sc.key}|${by}`, `${API}/history/compare?season=${encodeURIComponent(season)}&by=${by}`, sc.key === "current" || sc.key === "all");
    return { groups: e.data && Array.isArray(e.data.groups) ? e.data.groups : [], loading: e.loading && !e.data, error: !e.data && e.error };
  }

  // ---------- trend chart ----------
  // One series, one axis: earned and credits per AP are two small charts, never one.
  // Hover (or touch) shows a crosshair and the day's value; it rests on the newest day.
  function hiChart(title, pts, show) {
    const box = el("div", "hi-chart");
    const h = el("div", "hi-chart-h");
    const read = el("span", "mono");
    h.append(el("span", "", title), read);
    box.append(h);
    const vals = pts.map((p) => p.v).filter(isNum);
    if (!vals.length) { box.append(el("p", "empty", "No values for these days.")); return box; }
    const W = 300, H = 60, P = 3;
    const min = Math.min(0, ...vals), max = Math.max(0, ...vals), span = max - min || 1;
    const n = pts.length;
    const fx = (i) => (n === 1 ? W / 2 : P + (i * (W - 2 * P)) / (n - 1));
    const fy = (v) => H - P - ((v - min) / span) * (H - 2 * P);
    const NS = "http://www.w3.org/2000/svg";
    const svg = document.createElementNS(NS, "svg");
    svg.setAttribute("viewBox", `0 0 ${W} ${H}`);
    svg.setAttribute("preserveAspectRatio", "none");
    svg.setAttribute("aria-hidden", "true");
    const base = document.createElementNS(NS, "line");
    base.setAttribute("x1", "0"); base.setAttribute("x2", String(W)); base.setAttribute("y1", String(fy(0))); base.setAttribute("y2", String(fy(0)));
    base.setAttribute("class", "base");
    svg.append(base);
    let d = "", pen = false;
    pts.forEach((p, i) => {
      if (!isNum(p.v)) { pen = false; return; }
      d += `${pen ? "L" : "M"}${fx(i).toFixed(2)} ${fy(p.v).toFixed(2)} `;
      if (!pen && (i === n - 1 || !isNum((pts[i + 1] || {}).v))) d += `L${(fx(i) + 0.01).toFixed(2)} ${fy(p.v).toFixed(2)} `; // a lone day still shows
      pen = true;
    });
    const path = document.createElementNS(NS, "path");
    path.setAttribute("d", d.trim());
    path.setAttribute("vector-effect", "non-scaling-stroke");
    svg.append(path);
    const plot = el("div", "hi-plot");
    const xh = el("i", "hi-x"), dotEl = el("i", "hi-pt");
    plot.append(svg, xh, dotEl);
    plot.setAttribute("role", "img");
    const lastI = pts.map((p) => isNum(p.v)).lastIndexOf(true);
    plot.setAttribute("aria-label", `${title}: ${pts.length} days, latest ${show(pts[lastI].v)}, highest ${show(Math.max(...vals))}`);
    const mark = (i, hover) => {
      const p = pts[i];
      read.textContent = `${p.label}: ${isNum(p.v) ? show(p.v) : "–"}`;
      xh.hidden = !hover;
      xh.style.left = `${(fx(i) / W) * 100}%`;
      dotEl.hidden = !isNum(p.v);
      if (isNum(p.v)) { dotEl.style.left = `${(fx(i) / W) * 100}%`; dotEl.style.top = `${(fy(p.v) / H) * 100}%`; }
    };
    const at = (e) => { const r = plot.getBoundingClientRect(); const f = Math.max(0, Math.min(1, (e.clientX - r.left) / Math.max(1, r.width))); return n === 1 ? 0 : Math.round(((f * W - P) / (W - 2 * P)) * (n - 1)); };
    plot.addEventListener("pointermove", (e) => mark(Math.max(0, Math.min(n - 1, at(e))), true));
    plot.addEventListener("pointerleave", () => mark(lastI, false));
    mark(lastI, false);
    box.append(plot);
    const axis = el("div", "axis");
    axis.append(el("span", "", pts[0].label), el("span", "", `high ${show(Math.max(...vals))}`), el("span", "", pts[n - 1].label));
    box.append(axis);
    return box;
  }
  // The per-day series of one captain (by id) or one group, over a scope's days.
  function hiTrend(scopeKey, subject) {
    const { days, cut, total } = hiScopeDays(scopeKey);
    hiNeedDays(days);
    let ready = 0, failed = 0;
    const earned = [], cpa = [];
    for (const d of days) {
      const e = hiDays.get(hiDayKey(d)) || {};
      if (e.data) ready++;
      if (e.error) failed++;
      const label = `S${d.season} day ${d.day}`;
      let ev = null, cv = null;
      if (e.data) {
        const cs = e.data.captains.filter((c) => c && (subject.id !== undefined ? c.id === subject.id : hiInGroup(c, subject.group)));
        if (subject.id !== undefined) { if (cs[0]) { ev = hiNum(cs[0].earned); cv = hiNum(cs[0].credits_per_ap); } }
        else if (cs.length) {
          ev = hiMedian(cs.map((c) => c.earned));
          const ap = hiSum(cs.map((c) => c.ap_spent));
          cv = ap ? hiSum(cs.map((c) => c.earned)) / ap : hiMedian(cs.map((c) => c.credits_per_ap));
        }
      }
      earned.push({ v: ev, label }); cpa.push({ v: cv, label });
    }
    return { days, cut, total, ready, failed, earned, cpa };
  }
  function hiTrendBox(scopeKey, subject) {
    const t = hiTrend(scopeKey, subject);
    const box = el("div", "hi-trend-in");
    if (!t.days.length) { box.append(el("p", "hint", "No daily snapshots in this scope yet.")); return box; }
    const group = subject.id === undefined;
    const note = [`${plural(t.days.length, "daily snapshot")}${t.cut ? ` (the newest ${HI_ALL_DAYS} of ${fmt(t.total)})` : ""}`];
    if (t.ready + t.failed < t.days.length) note.push(`loading ${fmt(t.ready)} of ${fmt(t.days.length)}`);
    if (t.failed) note.push(`${fmt(t.failed)} could not be read`);
    if (group) note.push("median earned, credits per AP pooled");
    box.append(el("p", "hint", note.join(" · ")));
    const charts = el("div", "hi-charts");
    charts.append(hiChart(group ? "Median earned" : "Earned", t.earned, compact), hiChart("Credits per AP", t.cpa, hiCpa));
    box.append(charts);
    return box;
  }

  // ---------- tables ----------
  const hiCol = (k, label, o = {}) => ({ k, label, n: true, get: (r) => hiNum(r[k]), ...o });
  const hiScoreCol = (s, label) => hiCol(`score_${s}`, label, { cls: "hx-d", get: (r) => (r.scores ? hiNum(r.scores[s]) : null), show: compact });
  const HI_CAP_COLS = [
    { k: "season", label: "Season", all: true, title: "* ended by a fresh start", get: (r) => r._order, show: (v, r) => r._season, csv: (r) => r._seasonLabel },
    { k: "name", label: "Captain", fill: true, get: (r) => hiNorm(r.name), csv: (r) => r.name, cell: (r) => {
      const td = el("td", "fill");
      const w = el("span", "who");
      w.append(dot(r.name || "?"), el("span", "nm", r.name || `#${r.id}`));
      td.append(w);
      const line = el("div", "ident");
      if (r.autonomous) line.append(tag("auto", "", "Autonomous: no human behind it"));
      if (r.active === false) line.append(tag("inactive", "", "Spent no action points lately"));
      line.append(document.createTextNode([r.model, r.harness].filter(Boolean).join(" · ") || r.kind || ""));
      td.append(line);
      td.title = [r.name, r.model && `model ${r.model}`, r.harness && `on ${r.harness}`, r.career && `career ${r.career}`].filter(Boolean).join(" · ");
      return td;
    } },
    hiCol("place", "Place", { show: (v) => (isNum(v) ? `#${v}` : "–") }),
    hiCol("earned", "Earned", { show: compact }),
    hiCol("credits_per_ap", "Cr/AP", { csvLabel: "Credits per AP", title: "Credits earned per action point spent", show: hiCpa }),
    hiCol("net_worth", "Net worth", { cls: "hx-a", show: compact }),
    hiCol("ap_spent", "AP spent", { cls: "hx-a", show: compact }),
    hiCol("daily_wins", "Wins", { cls: "hx-b", title: "Daily wealth race wins" }),
    hiCol("deaths", "Deaths", { cls: "hx-b" }),
    hiCol("kills", "Kills", { cls: "hx-c" }),
    { k: "career", label: "Career", cls: "hx-c", get: (r) => hiNorm(r.career), csv: (r) => r.career, show: (v, r) => r.career || "–" },
    { k: "model", label: "Model", cls: "hx-d", get: (r) => hiNorm(r.model), csv: (r) => r.model, show: (v, r) => r.model || "–" },
    { k: "harness", label: "Harness", cls: "hx-d", get: (r) => hiNorm(r.harness), csv: (r) => r.harness, show: (v, r) => r.harness || "–" },
    { k: "involvement", label: "Human", cls: "hx-d", title: "Involvement: who sets the strategy", get: (r) => hiNorm(r.involvement), csv: (r) => r.involvement, show: (v, r) => r.involvement || "–" },
    { k: "win_condition", label: "Plays for", cls: "hx-d", get: (r) => hiNorm(r.win_condition), csv: (r) => r.win_condition, show: (v, r) => r.win_condition || "–" },
    { k: "corp", label: "Corp", cls: "hx-d", get: (r) => hiNorm(r.corp), csv: (r) => r.corp, show: (v, r) => r.corp || "–" },
    hiCol("ap_received", "AP received", { cls: "hx-d", show: compact }),
    hiCol("trade_profit", "Trade profit", { cls: "hx-d", show: compact }),
    hiCol("sectors_explored", "Explored", { cls: "hx-d" }),
    hiCol("planets", "Planets", { cls: "hx-d" }),
    hiScoreCol("wealth", "Wealth"), hiScoreCol("conquest", "Conquest"), hiScoreCol("exploration", "Exploration"), hiScoreCol("influence", "Influence"),
    { k: "id", label: "Id", csvOnly: true, get: (r) => r.id },
    { k: "kind", label: "Kind", csvOnly: true, get: (r) => r.kind },
    { k: "autonomous", label: "Autonomous", csvOnly: true, get: (r) => (r.autonomous ? "yes" : "no") },
    { k: "active", label: "Active", csvOnly: true, get: (r) => (r.active ? "yes" : "no") },
  ];
  function hiCompareCols(by, scale) {
    return [
      { k: "key", label: by === "model" ? "Model" : by === "harness" ? "Harness" : "Model + harness", fill: true, get: (g) => hiNorm(hiGroupLabel(g, by)), csv: (g) => hiGroupLabel(g, by), cell: (g) => {
        const td = el("td", "fill");
        td.append(el("span", "", hiGroupLabel(g, by)));
        td.title = hiGroupLabel(g, by);
        return td;
      } },
      hiCol("captains", "Captains", { title: "Active of total", show: (v, g) => `${fmt(g.active)} of ${fmt(g.captains)}` }),
      { k: "active", label: "Active", csvOnly: true, get: (g) => hiNum(g.active) },
      hiCol("median_earned", "Median earned", { show: compact }),
      hiCol("mean_earned", "Mean earned", { cls: "hx-c", show: compact }),
      hiCol("credits_per_ap", "Cr/AP", { csvLabel: "Credits per AP", title: "Credits earned per action point spent", show: hiCpa }),
      hiCol("ap_spent_share", "AP share", { csvLabel: "AP share (%)", title: "Share of all action points spent", get: (g) => (isNum(g.ap_spent_share) ? g.ap_spent_share * scale : null), show: hiPct }),
      hiCol("daily_wins", "Wins", { cls: "hx-a", title: "Daily wealth race wins" }),
      hiCol("deaths", "Deaths", { cls: "hx-a" }),
      hiCol("calls", "Calls", { cls: "hx-b", show: compact }),
      hiCol("refused", "Refused calls", { csvOnly: true }),
      hiCol("refusal_rate", "Refused", { csvLabel: "Refusal rate (%)", cls: "hx-b", title: "Share of calls refused", get: hiRefRate, show: hiPct }),
    ];
  }
  const hiTop = (obj, n, fmtE) => Object.entries(obj || {}).sort((a, b) => b[1] - a[1] || (a[0] < b[0] ? -1 : 1)).slice(0, n).map(fmtE).join(", ");
  const hiToolCount = (v) => (v && typeof v === "object" ? hiNum(v.calls) || 0 : hiNum(v) || 0);
  const hiTools = (r) => Object.fromEntries(Object.entries(r.tools || {}).map(([k, v]) => [k, hiToolCount(v)]));
  const hiActRate = (r) => (isNum(r.calls) && r.calls > 0 && isNum(r.refused) ? (r.refused / r.calls) * 100 : null);
  const HI_ACT_COLS = [
    { k: "day", label: "Day", get: (r) => r.day || "", show: (v) => v || "–" },
    { k: "name", label: "Captain", fill: true, get: (r) => hiNorm(r.name), csv: (r) => r.name, cell: (r) => {
      const td = el("td", "fill");
      const w = el("span", "who");
      w.append(dot(r.name || "?"), el("span", "nm", r.name || `#${r.player_id}`));
      td.append(w);
      const line = el("div", "ident", [r.model, r.harness].filter(Boolean).join(" · "));
      td.append(line);
      return td;
    } },
    { k: "platform", label: "Platform", cls: "hx-c", title: "The known platform the MCP client reported", get: (r) => hiNorm(r.platform), csv: (r) => r.platform, show: (v, r) => r.platform || "–" },
    hiCol("calls", "Calls"),
    hiCol("refused", "Refused", { show: (v, r) => (v ? `${fmt(v)} · ${hiPct(hiActRate(r))}` : "0") }),
    { k: "codes", label: "Top refusals", cls: "hx-b", get: (r) => hiTop(r.refusal_codes, 1, ([k]) => k), csv: (r) => hiTop(r.refusal_codes, 99, ([k, v]) => `${k}:${v}`), show: (v, r) => hiTop(r.refusal_codes, 2, ([k, n]) => `${k} ×${n}`) || "–" },
    { k: "tools", label: "Top tools", cls: "hx-d", get: (r) => hiTop(hiTools(r), 1, ([k]) => k), csv: (r) => Object.entries(r.tools || {}).map(([k, v]) => `${k}:${hiToolCount(v)}/${v && typeof v === "object" ? hiNum(v.refused) || 0 : 0}`).join("; "), show: (v, r) => hiTop(hiTools(r), 3, ([k]) => k) || "–" },
    hiCol("sessions", "Sessions", { cls: "hx-c" }),
    { k: "first_ms", label: "First", cls: "hx-d", get: (r) => hiNum(r.first_ms), csv: (r) => (isNum(r.first_ms) ? new Date(r.first_ms).toISOString() : ""), show: (v) => (isNum(v) ? clockT(v).slice(0, 5) : "–") },
    { k: "last_ms", label: "Last", cls: "hx-d", get: (r) => hiNum(r.last_ms), csv: (r) => (isNum(r.last_ms) ? new Date(r.last_ms).toISOString() : ""), show: (v) => (isNum(v) ? clockT(v).slice(0, 5) : "–") },
    { k: "player_id", label: "Player id", csvOnly: true, get: (r) => r.player_id },
    { k: "model", label: "Model", csvOnly: true, get: (r) => r.model },
    { k: "harness", label: "Harness", csvOnly: true, get: (r) => r.harness },
    hiCol("ok", "Accepted", { csvOnly: true }),
  ];
  function hiSortRows(rows, cols, st, tie) {
    const col = cols.find((c) => c.k === st.k && !c.csvOnly) || cols.find((c) => !c.csvOnly);
    return rows.slice().sort((a, b) => {
      const x = col.get(a), y = col.get(b);
      if (x === y || (x == null && y == null)) return tie(a, b);
      if (x === null || x === undefined || x === "") return 1;
      if (y === null || y === undefined || y === "") return -1;
      return (x < y ? -1 : 1) * st.d;
    });
  }
  // Draws the table of the current view; `rowFn` sets each row's attributes and click.
  function hiDraw(cols, rows, view, rowFn, empty) {
    const vis = cols.filter((c) => !c.csvOnly);
    const st = hiSort[view];
    const hr = el("tr");
    for (const c of vis) {
      const th = el("th", [c.n ? "n" : "", c.fill ? "fill" : "", c.cls || ""].join(" ").trim());
      th.scope = "col";
      if (c.title) th.title = c.title;
      if (c.k === st.k) th.setAttribute("aria-sort", st.d > 0 ? "ascending" : "descending");
      const b = el("button", "", c.label);
      b.type = "button";
      b.addEventListener("click", () => {
        hiSort[view] = st.k === c.k ? { k: c.k, d: -st.d } : { k: c.k, d: c.n || c.k === "day" || c.k === "season" ? -1 : 1 };
        store.set("hiSort", hiSort);
        hiTouch();
      });
      th.append(b);
      hr.append(th);
    }
    $("hi-head").replaceChildren(hr);
    const frag = document.createDocumentFragment();
    for (const r of rows) {
      const tr = el("tr");
      for (const c of vis) {
        if (c.cell) { const td = c.cell(r); if (c.cls) td.classList.add(c.cls); tr.append(td); continue; }
        const v = c.get(r);
        const td = el("td", [c.n ? "n" : "", c.cls || ""].join(" ").trim(), c.show ? c.show(v, r) : fmt(v));
        if (!c.n) td.title = td.textContent;
        tr.append(td);
      }
      rowFn(r, tr);
      frag.append(tr);
    }
    if (!rows.length) { const tr = el("tr", "empty"); const td = el("td", "", empty); td.colSpan = vis.length; tr.append(td); frag.append(tr); }
    $("hi-body").replaceChildren(frag);
  }
  function hiMessage(text) {
    $("hi-head").replaceChildren();
    const tr = el("tr", "empty"); const td = el("td", "", text); tr.append(td);
    $("hi-body").replaceChildren(tr);
    $("hi-trend").hidden = true;
    $("hi-totals").hidden = true;
    hiTable = null;
  }

  // ---------- controls ----------
  function hiSetView(v) {
    if (!["compare", "captains", "activity"].includes(v)) v = "compare";
    hiView = v; store.set("hiView", v);
    hiTouch();
  }
  function hiSetBy(b) {
    if (!["model", "harness", "model_harness"].includes(b)) b = "model";
    hiBy = b; store.set("hiBy", b);
    hiTouch();
  }
  function hiSetScope(k) {
    if (hiScope !== k) hiFocus = null;
    hiScope = k; store.set("hiScope", k);
    hiTouch();
  }
  for (const b of document.querySelectorAll("#hi-view button")) b.addEventListener("click", () => hiSetView(b.dataset.v));
  for (const b of document.querySelectorAll("#hi-by button")) b.addEventListener("click", () => hiSetBy(b.dataset.v));
  $("hi-scope").addEventListener("change", (e) => hiSetScope(e.target.value));
  $("hi-q").addEventListener("input", (e) => { hiQ = e.target.value.trim(); hiTouch(); });
  $("hi-from").addEventListener("change", hiTouch);
  $("hi-to").addEventListener("change", hiTouch);
  $("hi-player").addEventListener("input", () => { clearTimeout(hiActT); hiActT = setTimeout(hiTouch, 400); });
  $("hi-refresh").addEventListener("click", () => loadHistory(true));
  $("hi-csv").addEventListener("click", hiCsv);
  function hiControls() {
    for (const b of document.querySelectorAll("#hi-view button")) b.setAttribute("aria-pressed", String(b.dataset.v === hiView));
    for (const b of document.querySelectorAll("#hi-by button")) b.setAttribute("aria-pressed", String(b.dataset.v === hiBy));
    $("hi-by-w").hidden = hiView === "captains";
    $("hi-cap-w").hidden = hiView !== "captains";
    $("hi-act-w").hidden = hiView !== "activity";
    $("hi-scope-w").hidden = hiView === "activity";
    const scopes = hiScopes();
    if (hiIndex && !scopes.some((x) => x.key === hiScope)) hiScope = "current"; // a saved archived scope waits for the index
    const s = $("hi-scope");
    const sig = JSON.stringify(scopes.map((x) => [x.key, x.label]));
    if (s.dataset.sig !== sig) {
      s.dataset.sig = sig;
      s.replaceChildren(...scopes.map((x) => Object.assign(el("option", "", x.label), { value: x.key })));
    }
    s.value = hiScope;
    const on = $("hi-on");
    on.replaceChildren();
    if (hiGroup && hiView !== "compare") {
      const b = el("button", "", `${hiGroup.by === "model_harness" ? "model + harness" : hiGroup.by}: ${hiGroup.label} ×`);
      b.type = "button";
      b.title = "Show every captain";
      b.addEventListener("click", () => { hiGroup = null; hiTouch(); });
      on.append(b);
    }
  }

  // ---------- render ----------
  function renderHistory() {
    if (mode !== "history") return;
    if (hiDone === hiGen) return;
    hiDone = hiGen;
    hiControls();
    if (hiMissing) { $("hi-count").textContent = ""; hiMessage("History is not available on this server yet."); return; }
    if (!hiIndex) { $("hi-count").textContent = ""; hiMessage(hiErr || "Loading history…"); return; }
    if (hiView === "compare") renderHiCompare();
    else if (hiView === "captains") renderHiCaptains();
    else renderHiActivity();
  }
  function renderHiCompare() {
    const sc = hiCurScope();
    const e = hiCompareEntry(sc, hiBy);
    $("hi-trend").hidden = true;
    const groups = e.groups || [];
    const scale = hiShareScale(groups);
    const cols = hiCompareCols(hiBy, scale);
    const tot = $("hi-totals");
    const kids = [e.local ? el("p", "hint", "Computed in the console from this season's results (its number is shared with another season). Calls and refusals are not part of the results.") : null, hiConBox(sc)].filter(Boolean);
    tot.hidden = !kids.length;
    tot.replaceChildren(...kids);
    $("hi-count").textContent = groups.length ? plural(groups.length, "group") : "";
    const rows = hiSortRows(groups, cols, hiSort.compare, (a, b) => (hiGroupLabel(a, hiBy) < hiGroupLabel(b, hiBy) ? -1 : 1));
    hiTable = { cols, rows, name: `history-compare-${hiBy}-${sc.key}` };
    hiDraw(cols, rows, "compare", (g, tr) => {
      tr.dataset.grp = hiGroupLabel(g, hiBy);
      tr.title = "Show these captains";
      tr.addEventListener("click", () => {
        hiGroup = { by: hiBy, key: g.key, model: hiGroupModel(g, hiBy), harness: hiGroupHarness(g, hiBy), label: hiGroupLabel(g, hiBy) };
        hiFocus = null;
        hiSetView("captains");
      });
    }, e.loading ? "Loading…" : e.error ? e.error : "No captains in this scope yet.");
  }
  function renderHiCaptains() {
    const sc = hiCurScope();
    const cr = hiCaptainRows(sc.key);
    const all = sc.key === "all";
    const q = hiQ.toLowerCase();
    let rows = cr.rows.filter((r) => hiInGroup(r, hiGroup));
    if (q) rows = rows.filter((r) => [r.name, r.model, r.harness, r.career, r.corp, r.win_condition, `#${r.id}`].some((v) => v && String(v).toLowerCase().includes(q)));
    const cols = HI_CAP_COLS.filter((c) => !c.all || all);
    rows = hiSortRows(rows, cols, hiSort.captains, (a, b) => hiNorm(a.name).localeCompare(hiNorm(b.name)) || b._order - a._order);
    $("hi-count").textContent = cr.rows.length ? `${fmt(rows.length)} of ${fmt(cr.rows.length)}${cr.loading ? " · loading" : ""}` : "";
    hiTable = { cols, rows, name: `history-captains-${sc.key}` };
    hiDraw(cols, rows, "captains", (r, tr) => {
      const live = player(r.id);
      tr.dataset.sel = live ? selKey("captain", r.id) : selKey("hcap", r._key);
      if (r.active === false) tr.className = "dim";
      tr.addEventListener("click", () => {
        hiFocus = { id: r.id, name: r.name };
        hiTouch();
        if (player(r.id)) select("captain", r.id, { pan: true });
        else select("hcap", r._key);
      });
    }, cr.loading ? `Loading ${plural(cr.parts, "season")}…` : cr.error ? cr.error : hiGroup && !hiQ && cr.rows.length ? "No result carries exactly this model and harness (the group may merge spellings)." : hiQ || hiGroup ? "No captain matches." : "No captains in this scope yet.");
    // The trend: the captain last clicked, else the group filter.
    const tr = $("hi-trend");
    const subject = hiFocus ? { id: hiFocus.id, name: hiFocus.name } : hiGroup ? { group: hiGroup, name: hiGroup.label } : null;
    tr.hidden = !subject;
    if (subject) {
      const h = el("div", "hi-trend-h");
      h.append(el("b", "", `Trend · ${subject.name || `#${subject.id}`}`), el("span", "hint", sc.label));
      if (hiFocus) {
        const x = el("button", "icon-btn", "");
        x.type = "button";
        x.title = hiGroup ? "Back to the group's trend" : "Hide the trend";
        x.innerHTML = '<svg class="ic sm"><use href="#i-close"/></svg>';
        x.addEventListener("click", () => { hiFocus = null; hiTouch(); });
        h.append(x);
      }
      tr.replaceChildren(h, hiTrendBox(sc.key, subject));
    }
    const con = hiConBox(sc);
    $("hi-totals").hidden = !con;
    $("hi-totals").replaceChildren(...(con ? [con] : []));
    markSelected();
  }
  // One season's constitution in the history view: articles and proposals with their outcomes,
  // with a way into the Constitution workbench (which shows every ballot).
  let hiConOpen = false;
  function hiConBox(sc) {
    if (!sc || sc.key === "all") return null;
    const e = hiSeasonEntry(sc);
    if (!e.data || !("constitution" in e.data)) return null; // a server without the record
    const c = e.data.constitution;
    const box = el("div", "hi-con");
    const h = el("div", "hi-con-h");
    h.append(el("b", "", "Constitution"));
    if (!c) { h.append(el("span", "hint", "No constitution on record for this season.")); box.append(h); return box; }
    const arts = Array.isArray(c.articles) ? c.articles : [];
    const props = Array.isArray(c.proposals) ? c.proposals : [];
    const by = {};
    for (const p of props) by[p.status] = (by[p.status] || 0) + 1;
    const outcome = ["ratified", "rejected", "expired", "cancelled", "open"].filter((k) => by[k]).map((k) => `${fmt(by[k])} ${k}`).join(", ");
    const ballots = props.reduce((n, p) => n + (Array.isArray(p.ballots) ? p.ballots.length : 0), 0);
    const carried = sc.key === "current" ? "" : c.carried ? " · carried" : c.reason === "fresh_start" ? " · wiped by the fresh start" : " · lapsed";
    h.append(el("span", "hint", `${plural(arts.length, "article")} · ${plural(props.length, "proposal")}${outcome ? ` (${outcome})` : ""} · ${plural(ballots, "ballot")}${carried}`));
    h.append(btn("Open in Constitution", "sm ghost", () => ctOpenSeason(sc.key === "current" ? "current" : c.season || sc.key), "Every proposal with its full ballot timeline"));
    box.append(h);
    if (!arts.length && !props.length) return box;
    const det = el("details");
    det.open = hiConOpen;
    det.addEventListener("toggle", () => { hiConOpen = det.open; });
    det.append(el("summary", "", "Articles and proposals"));
    const ul = el("ul");
    for (const a of arts) {
      const li = el("li");
      li.append(el("b", "", `Article ${a.id}`), el("span", "m", `by ${a.by_operator ? "the operator" : a.proposed_by || "–"} · t${fmt(a.ratified_tick)}`));
      if (a.by_operator) li.append(tag("operator", "warn"));
      li.append(el("span", "x", ctText(a.text).replace(/\s+/g, " ").trim()));
      ul.append(li);
    }
    for (const p of [...props].sort((x, y) => x.id - y.id)) {
      const li = el("li");
      li.append(el("b", "", `#${p.id} ${ctChange(p)}`), ctTag(p), el("span", "m", `${fmt(p.yes)} yes, ${fmt(p.no)} no of ${fmt(p.electorate)} · by ${ctBy(p)}`));
      if (p.by_operator) li.append(tag("operator", "warn"));
      const t = ctText(p.text);
      if (t) li.append(el("span", "x", t.replace(/\s+/g, " ").trim()));
      ul.append(li);
    }
    det.append(ul);
    box.append(det);
    return box;
  }
  // Dates as YYYY-MM-DD, in UTC like the server's day boundaries.
  const hiDayMs = (s) => { const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(s || ""); return m ? Date.UTC(+m[1], +m[2] - 1, +m[3]) : null; };
  const hiDayStr = (ms) => new Date(ms).toISOString().slice(0, 10);
  function hiRange(days) {
    const first = days[0], last = days[days.length - 1];
    const f = $("hi-from"), t = $("hi-to");
    f.min = t.min = first; f.max = t.max = last;
    let to = hiDayMs(t.value) !== null ? t.value : last;
    if (to > last) to = last;
    if (to < first) to = first;
    let from = hiDayMs(f.value) !== null ? f.value : hiDayStr(Math.max(hiDayMs(first), hiDayMs(to) - 13 * 86400e3));
    if (from < first) from = first;
    if (from > to) from = to;
    f.value = from; t.value = to;
    return { from, to };
  }
  function renderHiActivity() {
    $("hi-trend").hidden = true;
    const days = (Array.isArray(hiIndex.activity_days) ? hiIndex.activity_days : []).filter((d) => hiDayMs(d) !== null).sort();
    for (const id of ["hi-from", "hi-to", "hi-player"]) $(id).disabled = !days.length;
    if (!days.length) { $("hi-count").textContent = ""; hiMessage("No daily activity summaries yet."); return; }
    const { from, to } = hiRange(days);
    const who = $("hi-player").value.trim();
    // Names for the captain filter: the live roster and every result loaded so far.
    const names = new Set(captains().map((p) => p.name));
    for (const [, v] of hiSeasons) for (const c of (v.data && v.data.captains) || []) if (c && c.name) names.add(c.name);
    const dl = $("hi-players");
    if (dl.childElementCount !== names.size) dl.replaceChildren(...[...names].sort().map((n) => Object.assign(el("option"), { value: n })));
    const e = hiGet(hiAct, `${from}|${to}|${who}`, `${API}/history/activity?from=${from}&to=${to}${who ? `&player=${encodeURIComponent(who)}` : ""}`, true);
    const all = e.data && Array.isArray(e.data.rows) ? e.data.rows : [];
    const rows0 = all.filter((r) => r && hiInGroup(r, hiGroup));
    // Totals per model, harness or both for the range.
    const tm = new Map();
    for (const r of rows0) {
      const model = r.model || "", harness = r.harness || "";
      const key = hiBy === "model" ? model : hiBy === "harness" ? harness : `${model} | ${harness}`;
      const g = tm.get(key) || { key, model: hiBy === "harness" ? null : model, harness: hiBy === "model" ? null : harness, ids: new Set(), days: 0, calls: 0, ok: 0, refused: 0, sessions: 0 };
      g.ids.add(r.player_id); g.days++;
      g.calls += hiNum(r.calls) || 0; g.ok += hiNum(r.ok) || 0; g.refused += hiNum(r.refused) || 0; g.sessions += hiNum(r.sessions) || 0;
      tm.set(key, g);
    }
    const tot = $("hi-totals");
    tot.hidden = !tm.size;
    if (tm.size) {
      const t = el("table", "mini");
      const hr = el("tr");
      for (const [l, n] of [[hiBy === "model" ? "Model" : hiBy === "harness" ? "Harness" : "Model + harness", 0], ["Captains", 1], ["Days", 1], ["Calls", 1], ["Refused", 1], ["Sessions", 1]]) { const th = el("th", n ? "n" : "fill", l); th.style.cursor = "default"; hr.append(th); }
      const th = el("thead"); th.append(hr);
      const tb = el("tbody");
      for (const g of [...tm.values()].sort((a, b) => b.calls - a.calls)) {
        const label = hiGroupLabel(g, hiBy);
        const tr = el("tr", hiGroup && hiGroup.label === label && hiGroup.by === hiBy ? "on" : "");
        const rate = g.calls ? (g.refused / g.calls) * 100 : null;
        tr.append(el("td", "fill", label), el("td", "n", fmt(g.ids.size)), el("td", "n", fmt(g.days)), el("td", "n", compact(g.calls)), el("td", `n${rate !== null && rate >= 25 ? " warn-t" : ""}`, g.refused ? `${fmt(g.refused)} · ${hiPct(rate)}` : "0"), el("td", "n", fmt(g.sessions)));
        tr.firstChild.title = label;
        tr.title = "Show only these captains";
        tr.addEventListener("click", () => { hiGroup = hiGroup && hiGroup.label === label && hiGroup.by === hiBy ? null : { by: hiBy, key: g.key, model: g.model, harness: g.harness, label }; hiTouch(); });
        tb.append(tr);
      }
      t.append(th, tb);
      tot.replaceChildren(el("h3", "sec-h", `Totals by ${hiBy === "model_harness" ? "model and harness" : hiBy}, ${from} to ${to}`), t);
    }
    const rows = hiSortRows(rows0, HI_ACT_COLS, hiSort.activity, (a, b) => (a.day < b.day ? 1 : a.day > b.day ? -1 : (hiNum(b.calls) || 0) - (hiNum(a.calls) || 0)));
    $("hi-count").textContent = e.data ? `${plural(rows.length, "row")}${e.data.truncated ? ", cut short" : ""}` : "";
    hiTable = { cols: HI_ACT_COLS, rows, name: `history-activity-${from}-to-${to}` };
    hiDraw(HI_ACT_COLS, rows, "activity", (r, tr) => {
      if (player(r.player_id)) { tr.title = `Inspect ${r.name}`; tr.addEventListener("click", () => select("captain", r.player_id, { pan: true, stay: true })); }
      else tr.style.cursor = "default";
    }, e.loading && !e.data ? "Loading…" : e.error && !e.data ? e.error : hiGroup || who ? "No activity matches." : "No activity in this range.");
    if (e.data && e.data.truncated) tot.append(el("p", "hint warn-t", "The server holds more rows for this range than it returned. Narrow the range or pick a captain."));
    if (e.data && e.data.truncated) tot.hidden = false;
  }

  // ---------- CSV of the table on screen ----------
  function hiCsv() {
    const t = hiTable;
    if (mode !== "history" || !t || !t.rows.length) { toast("err", "Download CSV", "Nothing to export in this view."); return; }
    // Player text is untrusted: a cell starting like a formula is prefixed so spreadsheets show it as text.
    const esc = (v) => {
      if (v === null || v === undefined) return "";
      if (typeof v === "number") return Number.isFinite(v) ? String(v) : "";
      let s = String(v);
      if (/^[=+\-@\t\r]/.test(s)) s = `'${s}`;
      return /[",\r\n]/.test(s) || s !== s.trim() ? `"${s.replace(/"/g, '""')}"` : s;
    };
    const lines = [t.cols.map((c) => esc(c.csvLabel || c.label)).join(",")];
    for (const r of t.rows) lines.push(t.cols.map((c) => esc(c.csv ? c.csv(r) : c.get(r))).join(","));
    const blob = new Blob([`﻿${lines.join("\r\n")}\r\n`], { type: "text/csv;charset=utf-8" });
    const a = el("a");
    a.href = URL.createObjectURL(blob);
    a.download = `${t.name.replace(/[^A-Za-z0-9._-]+/g, "-")}.csv`;
    document.body.append(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(a.href), 5000);
  }

  // ---------- one archived result, in the inspector ----------
  // The id is `${scope key}_${player id}`: a result of one season, read-only.
  function hiParseKey(k) { const s = String(k), i = s.lastIndexOf("_"); return i < 0 ? null : { scope: s.slice(0, i), id: Number(s.slice(i + 1)) }; }
  function hiResult(k) {
    const p = hiParseKey(k);
    if (!p || !hiIndex) return null;
    const sc = hiScopeOf(p.scope);
    if (!sc || sc.key === "all") return null;
    const e = hiSeasonEntry(sc);
    const c = e.data && Array.isArray(e.data.captains) ? e.data.captains.find((x) => x && x.id === p.id) : null;
    return { sc, e, c, id: p.id };
  }
  function renderHistCapInsp(box) {
    if (!hiIndex) {
      region(box, "head", head({ kind: "Captain history", title: "Result", sub: hiMissing ? "History is not available on this server yet." : hiErr || "Loading…" }));
      if (!hiMissing && !hiBusy && Date.now() - hiAt > 5000) loadHistory();
      return;
    }
    const x = hiResult(sel.id);
    if (!x || !x.c) {
      region(box, "head", head({ kind: "Captain history", title: "Result", sub: x && x.e.loading ? "Loading…" : "Not in the history." }));
      return;
    }
    const { sc, c } = x;
    const live = player(c.id);
    const tags = [];
    if (c.autonomous) tags.push(tag("autonomous", "", "No human behind it"));
    if (c.active === false) tags.push(tag("inactive"));
    if (c.involvement) tags.push(tag(c.involvement, "", "Who sets the strategy"));
    if (!live) tags.push(tag("not in this universe", "", "No captain with this id plays now"));
    region(box, "head", head({ kind: "Captain history", title: c.name || `#${c.id}`, titleDot: dot(c.name || "?"), id: `#${c.id}`, sub: [[c.model, c.harness && `on ${c.harness}`].filter(Boolean).join(" "), sc.key === "current" ? `Season ${sc.number} (current)` : sc.label].filter(Boolean).join(" · "), tags }));
    const aSig = JSON.stringify([!!live, c.id, box.dataset.archiveCaptain === String(c.id)]);
    const oldA = box.querySelector(":scope > [data-role=actions]");
    if (!oldA || oldA.dataset.sig !== aSig) {
      const a = el("div", "actions");
      if (live) a.append(btn("Open live captain", "sm", () => select("captain", c.id, { pan: true }), "This captain still plays: its live data"));
      a.append(btn(box.dataset.archiveCaptain === String(c.id) ? "Show result" : "Private archive", "sm ghost", () => {
        box.dataset.archiveCaptain = box.dataset.archiveCaptain === String(c.id) ? "" : String(c.id);
        renderHistCapInsp(box);
      }, "Permanent confidential note and memory history, including removed captains"));
      a.append(btn("Every season", "sm ghost", () => { setMode("history"); $("hi-q").value = c.name || ""; hiQ = c.name || ""; hiGroup = null; hiFocus = { id: c.id, name: c.name }; hiScope = "all"; store.set("hiScope", "all"); hiSetView("captains"); }, "This captain's results in every season"));
      a.dataset.sig = aSig;
      region(box, "actions", a);
    }
    if (box.dataset.archiveCaptain === String(c.id)) {
      body(box, `private-archive:${c.id}`, (b) => fillPrivateArchive(b, c));
      return;
    }
    const t = hiTrend(sc.key, { id: c.id });
    const others = [];
    for (const o of hiScopes()) {
      if (o.key === "all" || o.key === sc.key) continue;
      const e = hiSeasons.get(o.key);
      const oc = e && e.data && Array.isArray(e.data.captains) ? e.data.captains.find((y) => y && y.id === c.id) : null;
      if (oc) others.push({ o, oc });
    }
    body(box, JSON.stringify([sel.id, c, t.ready, t.failed, t.days.length, others.map((y) => y.o.key), !!live]), (b) => {
      b.append(statRow([["Place", isNum(c.place) ? `#${c.place}` : "–"], ["Earned", compact(c.earned)], ["Credits per AP", hiCpa(c.credits_per_ap)]]));
      b.append(section("Result", facts([
        ["Net worth", fmt(c.net_worth)], ["Earned", fmt(c.earned)],
        ["AP spent", fmt(c.ap_spent)], ["AP received", fmt(c.ap_received)],
        ["Daily wins", fmt(c.daily_wins)], ["Trade profit", fmt(c.trade_profit)],
        ["Kills", fmt(c.kills)], ["Deaths", fmt(c.deaths)],
        ["Explored", fmt(c.sectors_explored)], ["Planets", fmt(c.planets)],
      ])));
      const sc4 = c.scores && typeof c.scores === "object" ? c.scores : {};
      b.append(section("Scores", facts([["Wealth", fmt(sc4.wealth)], ["Conquest", fmt(sc4.conquest)], ["Exploration", fmt(sc4.exploration)], ["Influence", fmt(sc4.influence)]])));
      b.append(section("Captain", facts([
        ["Model", c.model || "–", { t: 1 }], ["Harness", c.harness || "–", { t: 1 }], ["Kind", c.kind || "–", { t: 1 }],
        ["Autonomous", c.autonomous ? "yes" : "no", { t: 1 }], ["Human", c.involvement || "–", { t: 1 }],
        ["Career", c.career || "–", { t: 1 }], ["Plays for", c.win_condition || "–", { t: 1 }],
        ["Corporation", c.corp || "none", { t: 1 }], ["Active", c.active ? "yes" : "no", { t: 1 }],
      ], true)));
      const tb = hiTrendBox(sc.key, { id: c.id });
      b.append(section("Trend", tb));
      if (others.length) {
        const ul = el("ul", "list");
        for (const { o, oc } of others.sort((p, q) => q.o.order - p.o.order)) {
          const li = el("li", "click");
          li.append(el("span", "nm", o.key === "current" ? `Season ${o.number} (current)` : o.label), el("span", "r mono", `${isNum(oc.place) ? `#${oc.place} · ` : ""}${compact(oc.earned)}`));
          li.addEventListener("click", () => select("hcap", `${o.key}_${c.id}`));
          ul.append(li);
        }
        b.append(section("Other seasons", ul));
      }
    });
  }

  // ======================================================================
  // Rules editor
  // ======================================================================
  let rulesOrig = null, rulesWork = null, rulesInvalid = new Set(), ruGroup = "", rulesLoaded = false;
  const clone = (v) => JSON.parse(JSON.stringify(v));
  const GROUPS = [
    ["Clock and action points", /^(tick_ms|ap_)/],
    ["Seasons and legacy", /^(season_|legacy_)/],
    ["Galaxy", /^(fedspace_|fed_gateways|stardock_|port_density|planet_density|one_way|sectors_per|expansion_|max_sectors)/],
    ["Taxes and government", /^(fed_tax|corp_tax|corp_term|corp_vote|law_)/],
    ["Trade and economy", /^(base_price|trade_|min_ask|exp_discount|port_regen|port_production|start_credits|bank_|treasury_|colonist_|corp_charter)/],
    ["Stardock prices", /^price_/],
    ["Navigation", /^(transwarp_|probe_|autopilot_)/],
    ["Combat", /^(combat_|fire_rate|toll_|armid_|salvage_|board_|qcannon_|interdictor_)/],
    ["Law and protection", /^(newbie_|fed_wanted|relief_)/],
    ["Planets", /^(planet_|max_planets)/],
    ["NPCs", /^(skarn_|traders_|fed_patrols)/],
    ["Galaxy events", /^event_/],
  ];
  const isCatalog = (v) => Array.isArray(v) && v.length && v.every((x) => x && typeof x === "object" && !Array.isArray(x));
  const getPath = (o, path) => path.reduce((a, k) => (a === undefined || a === null ? undefined : a[k]), o);
  function setPath(o, path, v) { let a = o; for (let i = 0; i < path.length - 1; i++) a = a[path[i]]; a[path[path.length - 1]] = v; }
  const pkey = (path) => path.map((k) => (typeof k === "number" ? `[${k}]` : `.${k}`)).join("").replace(/^\./, "");
  const groupOf = (k) => { if (rulesOrig && isCatalog(rulesOrig[k])) return `Catalog: ${k}`; const g = GROUPS.find(([, re]) => re.test(k)); return g ? g[0] : "Other"; };
  function hintFor(key, v) {
    if (!isNum(v)) return "";
    if (/_bp(_|$)/.test(key)) return `${(v / 100).toFixed(2)}%`;
    if (/_ms$/.test(key)) return `${v / 1000} s`;
    if (/_ticks$/.test(key) && rulesWork && rulesWork.tick_ms) { const ms = v * rulesWork.tick_ms; return ms >= 86400e3 ? `≈ ${(ms / 86400e3).toFixed(1)} days` : `≈ ${(ms / 3600e3).toFixed(1)} h`; }
    if (/_pct$/.test(key)) return `${v}%`;
    return "";
  }
  function leafEditor(path, v, compactMode) {
    const key = path[path.length - 1];
    let i;
    if (typeof v === "boolean") {
      i = el("input"); i.type = "checkbox"; i.checked = v;
      i.addEventListener("change", () => { setPath(rulesWork, path, i.checked); changed(path, i); });
    } else if (isNum(v)) {
      i = el("input"); i.type = "number"; i.step = Number.isInteger(v) ? "1" : "any"; i.value = String(v);
      i.addEventListener("input", () => {
        const n = Number(i.value);
        if (i.value.trim() === "" || !Number.isFinite(n)) { rulesInvalid.add(pkey(path)); i.setAttribute("aria-invalid", "true"); }
        else { rulesInvalid.delete(pkey(path)); i.removeAttribute("aria-invalid"); setPath(rulesWork, path, n); }
        changed(path, i);
      });
    } else if (typeof v === "string") {
      i = el(v.length > 40 ? "textarea" : "input");
      if (i.tagName === "INPUT") i.type = "text"; else i.rows = 2;
      i.value = v;
      if (key === "version") { i.readOnly = true; i.title = "Rules version (read-only)"; }
      i.addEventListener("input", () => { setPath(rulesWork, path, i.value); changed(path, i); });
    } else {
      i = el("textarea"); i.rows = compactMode ? 1 : 2; i.value = JSON.stringify(v);
      i.addEventListener("input", () => {
        try { const parsed = JSON.parse(i.value); if (parsed === null) throw new Error("null"); rulesInvalid.delete(pkey(path)); i.removeAttribute("aria-invalid"); setPath(rulesWork, path, parsed); }
        catch (_) { rulesInvalid.add(pkey(path)); i.setAttribute("aria-invalid", "true"); }
        changed(path, i);
      });
    }
    if (JSON.stringify(getPath(rulesOrig, path)) !== JSON.stringify(v)) i.classList.add("dirty");
    return i;
  }
  function arrayEditor(path, arr) {
    const box = el("div", "arr");
    const flat = arr.every(isNum);
    const pairs = arr.every((x) => Array.isArray(x) && x.every(isNum));
    if (flat) arr.forEach((x, i) => { const e = leafEditor([...path, i], x); e.setAttribute("aria-label", `${pkey(path)}[${i}]`); box.append(e); });
    else if (pairs) arr.forEach((inner, i) => inner.forEach((x, j) => { const e = leafEditor([...path, i, j], x); e.setAttribute("aria-label", `${pkey(path)}[${i}][${j}]`); e.title = `[${i}][${j}]`; box.append(e); }));
    else box.append(leafEditor(path, arr));
    return box;
  }
  function renderRules() {
    const body = $("ru-body");
    body.replaceChildren();
    if (!rulesWork) { body.append(el("p", "empty", "Loading rules…")); return; }
    $("ru-ver").textContent = rulesVersion;
    const keys = Object.keys(rulesWork);
    const buckets = new Map();
    for (const k of keys) { const g = groupOf(k); if (!buckets.has(g)) buckets.set(g, []); buckets.get(g).push(k); }
    const order = [...GROUPS.map(([g]) => g), "Other", ...[...buckets.keys()].filter((g) => g.startsWith("Catalog:"))];
    const gs = $("ru-group");
    const gopts = [["", `All groups (${keys.length})`], ...order.filter((g) => buckets.has(g)).map((g) => [g, `${g} (${buckets.get(g).length})`])];
    gs.replaceChildren(...gopts.map(([v, l]) => { const o = el("option", "", l); o.value = v; return o; }));
    gs.value = ruGroup;
    for (const g of order) {
      if (!buckets.has(g) || (ruGroup && ruGroup !== g)) continue;
      const sec = el("section", "rgroup");
      sec.dataset.group = g;
      const h = el("h3", "", g);
      h.append(el("span", "edited"));
      sec.append(h);
      if (g.startsWith("Catalog:")) {
        const k = g.slice(9);
        const arr = rulesWork[k];
        const cols = [];
        for (const it of arr) for (const c of Object.keys(it)) if (!cols.includes(c)) cols.push(c);
        const wrap = el("div", "cat-wrap");
        const t = el("table", "cat");
        const hr = el("tr");
        hr.append(el("th", "", "#"));
        for (const c of cols) hr.append(el("th", "", c));
        const thead = el("thead"); thead.append(hr);
        const tb = el("tbody");
        arr.forEach((it, i) => {
          const tr = el("tr");
          tr.dataset.key = `${k} ${Object.values(it).filter((x) => typeof x === "string").join(" ")}`;
          tr.append(el("td", "ro", String(i)));
          for (const c of cols) {
            const td = el("td");
            if (it[c] === undefined) td.append(el("span", "muted", "–"));
            else { const ed = leafEditor([k, i, c], it[c], true); ed.setAttribute("aria-label", `${k} ${i} ${c}`); td.append(ed); }
            tr.append(td);
          }
          tb.append(tr);
        });
        t.append(thead, tb);
        wrap.append(t);
        sec.append(wrap);
      } else {
        for (const k of buckets.get(g)) {
          const v = rulesWork[k];
          const row = el("div", `rrow${Array.isArray(v) ? " wide" : ""}`);
          row.dataset.key = k;
          const lab = el("label", "", k);
          lab.append(el("small", "", hintFor(k, v)));
          let ed;
          if (Array.isArray(v)) ed = arrayEditor([k], v);
          else { ed = leafEditor([k], v); ed.id = `ru-${k}`; lab.htmlFor = ed.id; }
          row.append(lab, ed);
          sec.append(row);
        }
      }
      body.append(sec);
    }
    applyRuleFilter();
    updateDiff();
  }
  function applyRuleFilter() {
    const q = $("ru-q").value.trim().toLowerCase();
    for (const sec of $("ru-body").querySelectorAll(".rgroup")) {
      let n = 0;
      const gHit = q && sec.dataset.group.toLowerCase().includes(q);
      for (const r of sec.querySelectorAll("[data-key]")) { const hit = !q || gHit || r.dataset.key.toLowerCase().includes(q); r.hidden = !hit; if (hit) n++; }
      sec.hidden = !!q && !n;
    }
  }
  $("ru-q").addEventListener("input", applyRuleFilter);
  $("ru-group").addEventListener("change", (e) => { ruGroup = e.target.value; renderRules(); $("ru-body").scrollTop = 0; });
  function diffRules(a, b, path = [], out = []) {
    if (Array.isArray(a) && Array.isArray(b) && a.length === b.length) { a.forEach((x, i) => diffRules(x, b[i], [...path, i], out)); return out; }
    if (a && b && typeof a === "object" && typeof b === "object" && !Array.isArray(a) && !Array.isArray(b)) {
      for (const k of new Set([...Object.keys(a), ...Object.keys(b)])) diffRules(a[k], b[k], [...path, k], out);
      return out;
    }
    if (JSON.stringify(a) !== JSON.stringify(b)) out.push({ path, was: a, now: b });
    return out;
  }
  let diffTimer = null;
  function changed(path, input) {
    const same = JSON.stringify(getPath(rulesOrig, path)) === JSON.stringify(getPath(rulesWork, path));
    input.classList.toggle("dirty", !same || rulesInvalid.has(pkey(path)));
    clearTimeout(diffTimer);
    diffTimer = setTimeout(updateDiff, 120);
  }
  function diffList(diffs, limit = 300) {
    const ul = el("ul", "diff");
    for (const d of diffs.slice(0, limit)) {
      const li = el("li");
      li.append(el("span", "path", pkey(d.path)), document.createTextNode("  "), el("span", "was", JSON.stringify(d.was)), document.createTextNode(" → "), el("span", "now", JSON.stringify(d.now)));
      ul.append(li);
    }
    if (diffs.length > limit) ul.append(el("li", "", `and ${diffs.length - limit} more`));
    return ul;
  }
  function updateDiff() {
    if (!rulesOrig) return;
    const diffs = diffRules(rulesOrig, rulesWork);
    const n = diffs.length;
    $("ru-pending").hidden = !n && !rulesInvalid.size;
    $("ru-pending-n").textContent = rulesInvalid.size ? `${plural(rulesInvalid.size, "invalid value")}` : `${plural(n, "change")} pending`;
    $("ru-apply").disabled = !n || rulesInvalid.size > 0;
    $("b-rules").hidden = !n;
    $("b-rules").textContent = String(n);
    $("b-rules").className = "badge warn";
    $("b-rules-m").hidden = !n;
    $("b-rules-m").textContent = String(n);
    rulesPend = n;
    renderMoreBadge();
    for (const sec of $("ru-body").querySelectorAll(".rgroup")) {
      const c = diffs.filter((d) => groupOf(d.path[0]) === sec.dataset.group).length;
      sec.querySelector(".edited").textContent = c ? `${c} edited` : "";
    }
    for (const row of $("ru-body").querySelectorAll(".rrow")) {
      const k = row.dataset.key;
      const was = rulesOrig[k];
      const ch = JSON.stringify(was) !== JSON.stringify(rulesWork[k]);
      row.classList.toggle("changed", ch);
      let w = row.querySelector(".was");
      if (ch && !Array.isArray(was)) { if (!w) { w = el("span", "was"); row.append(w); } w.textContent = `live ${JSON.stringify(was)}`; }
      else if (w) w.remove();
    }
    renderAlerts();
  }
  function buildPatch() {
    const patch = {};
    for (const d of diffRules(rulesOrig, rulesWork)) patch[d.path[0]] = rulesWork[d.path[0]];
    return patch;
  }
  async function loadRules(force) {
    if (rulesLoaded && !force) return;
    if (rulesOrig && !force && !diffRules(rulesOrig, rulesWork).length) { /* refresh quietly below */ }
    const r = await api(`${API}/rules`);
    if (!r.ok) { $("ru-refusal").hidden = false; $("ru-refusal").textContent = errText(r.data, r.status); return; }
    rulesLoaded = true;
    rulesOrig = r.data;
    rulesWork = clone(r.data);
    rulesInvalid = new Set();
    rulesVersion = r.data.version ? `${r.data.version}${r.data.revision ? `+r${r.data.revision}` : ""}` : "";
    renderRules();
    renderStatus();
  }
  $("ru-reload").addEventListener("click", async () => {
    if (rulesOrig && diffRules(rulesOrig, rulesWork).length) {
      if (!await confirmDlg({ title: "Discard your rule edits?", body: ["Reloading replaces the editor with the live rules."], ok: "Discard and reload" })) return;
    }
    $("ru-refusal").hidden = true;
    loadRules(true);
  });
  $("ru-discard").addEventListener("click", () => { rulesWork = clone(rulesOrig); rulesInvalid = new Set(); $("ru-refusal").hidden = true; renderRules(); });
  async function applyRules() {
    const diffs = diffRules(rulesOrig, rulesWork);
    if (!diffs.length || rulesInvalid.size) return;
    if (!await confirmDlg({ title: `Apply ${plural(diffs.length, "rule change")}?`, body: ["The live universe switches to these rules now. The change is journaled and announced to every captain.", diffList(diffs)], ok: "Apply to live rules", danger: false })) return;
    const r = await api(`${API}/action/set_rules`, { method: "POST", body: { patch: buildPatch() } });
    const ref = $("ru-refusal");
    if (r.ok) {
      ref.hidden = true;
      toast("ok", "Rules", (r.data && r.data.summary) || "Rules updated.");
      await loadRules(true);
    } else if (r.status !== 401) {
      ref.hidden = false;
      ref.textContent = `Refused: ${errText(r.data, r.status)}${errHint(r.data) ? `\n${errHint(r.data)}` : ""}`;
      toast("err", "Rules: refused", errText(r.data, r.status));
    }
  }
  $("ru-apply").addEventListener("click", applyRules);

  // ======================================================================
  // Universe: season, clock, world operations, data, enlisting
  // ======================================================================
  function renderUniverse() {
    const s = S.season;
    const left = Math.max(0, s.ends_tick - S.tick);
    const facts = [
      ["Season", String(s.number)], ["Status", s.finished ? "intermission" : S.paused ? "running, clock held" : "running"],
      ["Started", `tick ${fmt(s.started_tick)}`], ["Ends", `tick ${fmt(s.ends_tick)}`],
      ["Current", `tick ${fmt(S.tick)}`], ["Remaining", s.finished ? "–" : `${fmt(left)} ticks · ${ticksDur(left)}`],
      ["Fed purse paid", fmt(s.fed_purse_paid)], ["Intermission", `${fmt(S.intermission_ticks)} ticks`],
    ];
    $("uv-season").replaceChildren(...facts.map(([k, v]) => { const d = el("div"); d.append(el("dt", "", k), el("dd", "", v)); return d; }));
    const span = Math.max(1, s.ends_tick - s.started_tick);
    const pct = Math.min(100, Math.max(0, ((S.tick - s.started_tick) / span) * 100));
    $("uv-meter").firstChild.style.width = `${pct}%`;
    $("uv-meter").title = `${pct.toFixed(1)}% of the season elapsed`;
    $("uv-end-tick").placeholder = String(s.ends_tick);
    $("uv-clock").textContent = `${S.paused ? "Held" : "Running"} at ${tickMs() / 1000} s per tick. Advancing runs ticks immediately, whether or not the clock is held.`;
    $("uv-hold").textContent = S.paused ? "Resume clock" : "Hold clock";
  }
  const intOf = (inp) => { const v = inp.value.trim(); if (v === "") return null; const n = Number(v); return Number.isFinite(n) ? Math.round(n) : NaN; };
  $("uv-end").addEventListener("submit", async (e) => {
    e.preventDefault();
    const t = intOf($("uv-end-tick"));
    if (t === null || Number.isNaN(t) || t < 0) return toast("err", "Season", "Enter the tick the season should end on.");
    if (await act("season_end", { ends_tick: t }, "Season")) $("uv-end-tick").value = "";
  });
  $("uv-hold").addEventListener("click", toggleClock);
  $("uv-step").addEventListener("click", async (e) => { e.target.disabled = true; await advance($("uv-n").value); e.target.disabled = false; });
  $("uv-skarn").addEventListener("submit", async (e) => {
    e.preventDefault();
    const s = intOf($("uv-skarn-s"));
    if (Number.isNaN(s)) return toast("err", "Skarn", "Sector must be a number.");
    if (await act("spawn_skarn", s ? { sector: s } : {}, "Skarn")) $("uv-skarn-s").value = "";
  });
  $("uv-fed").addEventListener("submit", async (e) => {
    e.preventDefault();
    const g = intOf($("uv-fed-n"));
    if (Number.isNaN(g)) return toast("err", "FedSpace", "Gateways must be a number.");
    if (await act("link_fedspace", g ? { gateways: g } : {}, "FedSpace")) loadMap();
  });
  $("uv-conc").addEventListener("click", async (e) => { e.target.disabled = true; await act("place_concessions", {}, "Concessions"); e.target.disabled = false; });
  $("uv-snap").addEventListener("click", async (e) => { e.target.disabled = true; await act("snapshot", {}, "Snapshot"); e.target.disabled = false; });
  $("uv-backup").addEventListener("click", async (e) => { e.target.disabled = true; await act("backup", {}, "Backup"); e.target.disabled = false; });
  async function endSeason() {
    const n = S.season.number;
    if (!await confirmDlg({ title: `End season ${n} now?`, body: ["Standings freeze and the hall of fame records the finish. The intermission begins, then the next galaxy."], ok: "End the season", typed: `end season ${n}` })) return;
    await act("end_season", {}, "Season");
  }
  async function bigBang() {
    const n = S.season.number + 1;
    if (!await confirmDlg({ title: `Start season ${n} now?`, body: ["The Big Bang: a brand-new galaxy. Ships, credits, planets and fortunes reset; captains keep names, rank, notes and corporations, plus their legacy."], ok: "Big Bang", typed: "big bang" })) return;
    if (await act("new_season", {}, "Big Bang")) { select(null); loadMap(); }
  }
  async function clearStrategies() {
    if (!await confirmDlg({ title: "Clear every human strategy?", body: ["Deletes every captain's human strategy, public label and coach note, so each starts afresh from its character. Involvement modes stay. Every captain is told. Journaled; the old strategies are not restored."], ok: "Clear strategies", typed: "clear strategies" })) return;
    await act("clear_strategies", { confirm: "clear strategies" }, "Clear strategies");
  }
  // Engine 1.48: every current captain that has not renamed must use its one rename to a
  // character's name; until then it counts as undocumented.
  async function requireRename() {
    if (!idData) loadIdentities();
    const pend = captains().filter((p) => { const r = idRow(p.id); return !(r && isNum(r.renamed_tick)); }).length;
    if (!await confirmDlg({ title: "Require every captain to rename?", body: [`Every current captain that has not used its one rename (${idData ? plural(pend, "captain") : "count loading"}) must take a character's name, sharing no word with its old call sign. Until it does it counts as undocumented${S.identity_required ? " and cannot spend action points (identity_required is on)" : ""}. Each of them sees the requirement in its briefing. Journaled; the console cannot lift it again.`], ok: "Require rename", typed: "require rename" })) return;
    if (await act("require_rename", {}, "Require rename")) { idAt = 0; loadIdentities(); }
  }
  $("uv-rename").addEventListener("click", requireRename);
  // Engine 1.49: identities of captains who renamed earlier still name their old call sign.
  async function fixOldNames() {
    if (!idData) loadIdentities();
    const renamed = idData ? (idData.identities || []).filter((r) => (r.former_names || []).length).length : null;
    if (!await confirmDlg({ title: "Carry new names through identities?", body: [`For each captain that has renamed${renamed === null ? "" : ` (${plural(renamed, "captain")})`}, a former call sign left in its current card, backstory or relation notes becomes its current name, as a new identity version; earlier versions keep the original. Each updated captain is told, and any leftover word of an old name gets it a notice to revise. Journaled.`], ok: "Fix old names", danger: false })) return;
    const d = await act("fix_old_names", {}, "Fix old names");
    if (d) { idAt = 0; loadIdentities(); }
  }
  $("uv-fixnames").addEventListener("click", fixOldNames);
  $("uv-clearstrat").addEventListener("click", clearStrategies);
  $("uv-endnow").addEventListener("click", endSeason);
  $("uv-bang").addEventListener("click", bigBang);
  $("en-form").addEventListener("submit", async (e) => {
    e.preventDefault();
    const name = $("en-name").value.trim();
    if (!name) return;
    const body = { name, model: $("en-model").value.trim() || null, harness: $("en-harness").value.trim() || null, kind: $("en-kind").value, autonomous: $("en-auto").checked };
    const d = await act("register", body, "Enlist");
    if (d && d.token) {
      const res = $("en-result");
      res.replaceChildren(secretBox(`${name} is captain #${d.player_id}. Token shown once: the server keeps only a hash.`, d.token, [["MCP URL", d.mcp_url]]));
      if (d.recovery_key) res.append(secretBox("Recovery key (shown once).", d.recovery_key));
      if (d.owner_token) res.append(secretBox("Owner token for verified mode (optional, shown once).", d.owner_token, [["owner URL", d.owner_mcp_url]]));
      $("en-name").value = ""; $("en-model").value = ""; $("en-harness").value = ""; $("en-auto").checked = false;
    }
  });

  // ======================================================================
  // Inspector
  // ======================================================================
  const inspTab = { captain: "overview", cprod: "overview" };
  let actOpen = null;            // open action form id
  let insSig = "", insKey = "";
  let moveTarget = null;         // captain id while picking a sector on the map

  function head({ kind, title, titleDot, id, sub, tags }) {
    const h = el("div", "ih");
    const k = el("div", "ih-kind", kind);
    const x = el("button", "icon-btn");
    x.type = "button";
    x.title = "Clear selection (Esc)";
    x.innerHTML = '<svg class="ic sm"><use href="#i-close"/></svg>';
    x.addEventListener("click", () => select(null));
    k.append(x);
    const t = el("div", "ih-title");
    if (titleDot) t.append(titleDot);
    t.append(el("h2", "", title));
    if (id !== undefined) t.append(el("span", "mono", id));
    h.append(k, t);
    if (sub) h.append(el("p", "ih-sub", sub));
    const tg = el("div", "ih-tags");
    for (const x2 of tags || []) tg.append(x2);
    h.append(tg);
    return h;
  }
  function facts(pairs, one) {
    const dl = el("dl", `facts${one ? " one" : ""}`);
    for (const [k, v, opts] of pairs) {
      if (v === undefined) continue;
      const d = el("div");
      const dd = el("dd", opts && opts.t ? "t" : "");
      if (v instanceof Node) dd.append(v); else dd.textContent = v === null ? "–" : String(v);
      if (opts && opts.cls) dd.classList.add(opts.cls);
      if (!(v instanceof Node)) dd.title = dd.textContent;
      d.append(el("dt", "", k), dd);
      dl.append(d);
    }
    return dl;
  }
  function section(title, ...kids) {
    const s = el("section", "sec");
    const h = el("h3", "sec-h", title);
    s.append(h, ...kids.filter(Boolean));
    return s;
  }
  function link(text, fn) { const a = el("a", "", text); a.href = "#"; a.addEventListener("click", (e) => { e.preventDefault(); fn(); }); return a; }
  const sectorLink = (id) => link(String(id), () => select("sector", id, { pan: true }));
  const corpLink = (name, ceo) => {
    const c = S.corps.find((x) => x.name === name);
    const label = `${name}${ceo ? " (CEO)" : ""}`;
    return c ? link(label, () => select("corp", c.id, { pan: true })) : document.createTextNode(label);
  };
  const captainLink = (name) => { const p = playerByName(name); return p ? link(name, () => select("captain", p.id, { pan: true })) : document.createTextNode(name || "–"); };
  function statRow(items) {
    const d = el("dl", "stat-row");
    for (const [k, v, small] of items) { const x = el("div"); const dd = el("dd", "", v); if (small) dd.append(el("small", "", small)); x.append(el("dt", "", k), dd); d.append(x); }
    return d;
  }
  function bars(byHour, cls) {
    const max = Math.max(1, ...byHour.map((b) => b.calls));
    const box = el("div", `bars${cls ? ` ${cls}` : ""}`);
    for (const b of byHour) {
      const s = el("span");
      s.style.height = `${Math.max(b.calls ? 4 : 1, (b.calls / max) * 100)}%`;
      if (!b.calls) s.style.opacity = "0.2";
      s.title = `${b.label || stamp(b.hour_start_ms)}: ${plural(b.calls, b.unit || "call")}`;
      box.append(s);
    }
    return box;
  }
  function tabsBar(kind, defs) {
    const cur = defs.some((d) => d[0] === inspTab[kind]) ? inspTab[kind] : defs[0][0];
    inspTab[kind] = cur;
    const bar = el("div", "tabs");
    bar.setAttribute("role", "tablist");
    for (const [k, label, n] of defs) {
      const b = el("button", "", label);
      b.type = "button";
      b.setAttribute("role", "tab");
      b.setAttribute("aria-selected", String(k === cur));
      if (n) b.append(el("small", "", String(n)));
      b.addEventListener("click", () => { inspTab[kind] = k; insSig = ""; buildInspector(); });
      bar.append(b);
    }
    return bar;
  }

  // The inspector is rebuilt per selection; on each poll only the parts whose data
  // changed are redrawn, and an open action form is never clobbered.
  function buildInspector() {
    const box = $("insp");
    const key = sel ? `${sel.kind}:${sel.id}:${inspTab[sel.kind] || ""}:${actOpen || ""}` : "none";
    if (key !== insKey) { insKey = key; insSig = ""; box.replaceChildren(); }
    if (moveTarget !== null && !(sel && sel.kind === "captain" && sel.id === moveTarget && actOpen === "move")) moveTarget = null;
    if (!S) return;
    const r = !sel ? renderOverview : { captain: renderCaptainInsp, sector: renderSectorInsp, planet: renderPlanetInsp, corp: renderCorpInsp, msg: renderMsgInsp, entry: renderEntryInsp, anon: renderAnonInsp, feedback: renderFeedbackInsp, article: renderArticleInsp, proposal: renderProposalInsp, archive: renderArchiveInsp, hcap: renderHistCapInsp, cpage: renderCPageInsp, cprod: renderCProdInsp, improve: renderImproveInsp }[sel.kind];
    if (!r) return renderOverview(box);
    r(box);
  }
  // Swap one region of the inspector (by role) without touching the others.
  function region(box, role, node) {
    node.dataset.role = role;
    const old = box.querySelector(`:scope > [data-role="${role}"]`);
    if (old) old.replaceWith(node); else box.append(node);
    return node;
  }
  function body(box, sig, fill) {
    if (sig === insSig && box.querySelector(":scope > [data-role=body]")) return;
    insSig = sig;
    const old = box.querySelector(":scope > [data-role=body]");
    const top = old ? old.scrollTop : 0;
    const b = el("div", "ib");
    fill(b);
    region(box, "body", b);
    b.scrollTop = top;
  }

  // ---------- nothing selected: the universe at a glance ----------
  function renderOverview(box) {
    const caps = captains();
    const npcs = S.players.filter((p) => p.kind === "npc");
    const act = actAlerts;
    const active = act ? (act.players || []).filter((p) => p.last_call_ms && Date.now() - p.last_call_ms < 3600e3).length : null;
    const tags = [];
    if (S.paused) tags.push(tag("clock held", "warn"));
    if (S.season.finished) tags.push(tag("intermission", "info"));
    region(box, "head", head({ kind: "Universe", title: `Season ${S.season.number}`, sub: `Tick ${fmt(S.tick)} · ${fmt(S.sectors)} sectors · ${tickMs() / 1000} s per tick`, tags }));
    const nw = caps.reduce((a, p) => a + (p.net_worth || 0), 0);
    const recent = events.filter((e) => S.tick - e.tick < 60);
    const sig = JSON.stringify([S.seq, active, recent.length, act && act.at]);
    body(box, sig, (b) => {
      b.append(statRow([["Captains", fmt(caps.length), active !== null ? `${active} active` : ""], ["Net worth", compact(nw)], ["Events", fmt(recent.length), `last ${ticksDur(60)}`]]));
      const s = S.season, span = Math.max(1, s.ends_tick - s.started_tick), pct = Math.min(100, ((S.tick - s.started_tick) / span) * 100);
      const meter = el("div", "meter"); const i = el("i"); i.style.width = `${pct}%`; meter.append(i);
      b.append(section("Season", facts([["Elapsed", `${pct.toFixed(1)}%`], ["Ends in", s.finished ? "finished" : ticksDur(Math.max(0, s.ends_tick - S.tick))], ["Ends at", `tick ${fmt(s.ends_tick)}`], ["Clock", S.paused ? "held" : "running", { t: 1, cls: S.paused ? "warn-t" : "" }]]), meter));
      // Event rate per tick, last 60 ticks, from the live feed.
      const buckets = [];
      for (let t = S.tick - 59; t <= S.tick; t++) buckets.push({ calls: 0, label: `tick ${t}`, unit: "event" });
      for (const e of recent) { const i2 = e.tick - (S.tick - 59); if (buckets[i2]) buckets[i2].calls++; }
      const axis = el("div", "axis"); axis.append(el("span", "", ticksDur(60) + " ago"), el("span", "", "now"));
      b.append(section("Events per tick", bars(buckets, "ev"), axis));
      const byRole = {};
      for (const p of npcs) byRole[p.npc || "npc"] = (byRole[p.npc || "npc"] || 0) + 1;
      b.append(section("Population", facts([
        ["Captains", fmt(caps.length)], ["Active (1 h)", active === null ? "–" : fmt(active)],
        ["Autonomous", fmt(caps.filter((p) => p.autonomous).length)], ["Coached", fmt(caps.filter((p) => p.coached).length)],
        ["Protected", fmt(caps.filter((p) => p.newbie_protected).length)], ["Banned", fmt(caps.filter((p) => p.banned).length)],
        ...Object.entries(byRole).map(([k, v]) => [`NPC ${k}`, fmt(v)]),
      ])));
      const owned = S.planets.filter((p) => p.owner).length;
      const held = (S.concessions || []).filter((c) => c.holder).length;
      b.append(section("Territory and economy", facts([
        ["Planets owned", `${fmt(owned)} / ${fmt(S.planets.length)}`], ["Concessions held", `${fmt(held)} / ${fmt((S.concessions || []).length)}`],
        ["Corporations", fmt(S.corps.length)], ["Defended sectors", fmt(S.defenses.length)],
        ["Credits on hand", compact(caps.reduce((a, p) => a + p.credits, 0))], ["In the bank", compact(caps.reduce((a, p) => a + (p.bank || 0), 0))],
        ["Treaties", fmt((S.treaties || []).length)], ["Wars", fmt((S.wars || []).length)],
      ])));
      const top = [...caps].sort((a, c) => c.net_worth - a.net_worth).slice(0, 6);
      const ul = el("ul", "list");
      top.forEach((p, i) => {
        const li = el("li", "click");
        li.append(el("span", "mono muted", String(i + 1)), dot(p.name), el("span", "nm", p.name), el("span", "r mono", compact(p.net_worth)));
        li.addEventListener("click", () => select("captain", p.id, { pan: true }));
        ul.append(li);
      });
      b.append(section("Leaders", top.length ? ul : el("p", "empty", "No captains yet.")));
      const news = el("ul", "textlog");
      for (const n of (S.news || []).slice(0, 6)) { const li = el("li"); const m = el("div", "m"); m.append(el("span", "", `tick ${fmt(n.tick)}`)); li.append(m, document.createTextNode(n.headline)); news.append(li); }
      b.append(section("News", news));
    });
  }

  // ---------- captain ----------
  function fetchDetail(id) {
    return api(`${API}/player?name=${encodeURIComponent(String(id))}`).then((r) => {
      if (r.ok && r.data) { details.set(id, { d: r.data, at: Date.now() }); return r.data; }
      return null;
    });
  }
  let actCache = new Map(); // `${id}:${scope}` -> { d, at } (scope: the Activity period, see acScopeId)
  const actPending = new Map();
  // A past season never changes: its summary is kept until a refresh.
  const actFresh = (x) => !!x && (Date.now() - x.at < 30000 || (!!acSeason() && !acSeason().current));
  function fetchActivity(name, id) {
    const k = `${id}:${acScopeId()}`;
    const c = actCache.get(k);
    if (actFresh(c)) return Promise.resolve(c.d);
    if (actPending.has(k)) return actPending.get(k);
    const q = api(`${API}/activity?${acQS()}&name=${encodeURIComponent(String(id))}`).then((r) => {
      actPending.delete(k);
      const d = r.ok ? r.data : { error: errText(r.data, r.status) };
      actCache.set(k, { d, at: Date.now() });
      return d;
    });
    actPending.set(k, q);
    return q;
  }
  function renderCaptainInsp(box) {
    const p = player(sel.id);
    if (!p) { region(box, "head", head({ kind: "Captain", title: `#${sel.id}`, sub: "No longer in this universe." })); return; }
    const det = details.get(p.id);
    if (!det || Date.now() - det.at > 6000) {
      if (!det || !det.loading) {
        details.set(p.id, { ...(det || {}), loading: true, at: det ? det.at : 0 });
        fetchDetail(p.id).then((d) => {
          if (!d) { const x = details.get(p.id); if (x) details.set(p.id, { ...x, loading: false, at: Date.now() }); }
          if (sel && sel.kind === "captain" && sel.id === p.id) buildInspector();
        });
      }
    }
    const d = det && det.d;
    const npc = p.kind === "npc";
    const irH = npc ? null : idRow(p.id);
    const gH = irH && irH.current ? genderOf(irH.current.pronouns) : null;
    const sub = npc ? `NPC ${p.npc || ""} · ${(p.ship || "").replace(/_/g, " ")}` : [gH, p.model || p.kind, p.harness && `on ${p.harness}`, p.rank, p.corp].filter(Boolean).join(" · ");
    const tags = flagsOf(p).map(([t, k, title]) => tag(t, k, title));
    if (irH && (irH.former_names || []).length) tags.push(tag(`formerly ${irH.former_names.join(", ")}`, "", "Former call signs (the one rename is used)"));
    if (p.label) tags.unshift(tag(`“${p.label}”`, "", "Public label"));
    region(box, "head", head({ kind: npc ? "NPC" : "Captain", title: p.name, titleDot: dot(p.name, npc && p.npc), id: `#${p.id}`, sub, tags }));

    // Actions sit next to the captain they act on.
    const actions = el("div", "actions");
    const toggle = (k, label, cls) => {
      const b = el("button", `btn sm ${cls || ""}`, label);
      b.type = "button";
      b.setAttribute("aria-pressed", String(actOpen === k));
      b.addEventListener("click", () => { actOpen = actOpen === k ? null : k; buildInspector(); });
      return b;
    };
    if (!npc) actions.append(toggle("message", "Message"));
    actions.append(toggle("grant", "Grant"), toggle("edit", "Edit"), toggle("move", "Move"));
    if (!npc) {
      actions.append(toggle("mute", p.muted_until > S.tick ? "Muted" : "Mute"), toggle("access", "Access"));
      if (p.banned) actions.append(btn("Unban", "sm", () => unban(p)));
      else actions.append(btn("Ban", "sm danger", () => ban(p)));
    }
    actions.append(btn("Remove", "sm ghost", () => removeCaptain(p), "Remove a captain who owns nothing yet"));
    const aSig = JSON.stringify([actOpen, p.banned, p.muted_until > S.tick, npc]);
    const oldA = box.querySelector(":scope > [data-role=actions]");
    if (!oldA || oldA.dataset.sig !== aSig) { actions.dataset.sig = aSig; region(box, "actions", actions); }
    const oldF = box.querySelector(":scope > [data-role=form]");
    if (actOpen && (!oldF || oldF.dataset.k !== actOpen)) { const f = actionForm(p, d); f.dataset.k = actOpen; region(box, "form", f); }
    else if (!actOpen && oldF) oldF.remove();
    else if (actOpen && oldF) refreshForm(oldF, p, d);

    const radio = (S.radio || []).filter((m) => m.from_id === p.id || m.to_id === p.id || (m.channel === "corp" && d && d.corp && m.corp_id === d.corp.id));
    const tabsDef = [["overview", "Overview"], ["ship", "Ship"]];
    if (!npc) tabsDef.splice(1, 0, ["identity", "Identity"]);
    if (!npc) tabsDef.push(["owner", "Human"], ["notes", "Notes", d ? Object.keys(d.notes || {}).length : 0], ["memory", "Memory", d ? (d.memory || []).length : 0], ["archive", "Archive"], ["inbox", "Inbox", d ? (d.inbox || []).length : 0], ["radio", "Radio", radio.length], ["activity", "Activity"]);
    const tb = tabsBar("captain", tabsDef);
    const oldT = box.querySelector(":scope > [data-role=tabs]");
    const tSig = JSON.stringify([tabsDef, inspTab.captain]);
    if (!oldT || oldT.dataset.sig !== tSig) { tb.dataset.sig = tSig; region(box, "tabs", tb); }
    const tab = inspTab.captain;
    if (tab === "identity") {
      if (!idBusy && (!idData || Date.now() - idAt > 15000)) loadIdentities();
      loadIdBible();
      const row = idData ? (idData.identities || []).find((x) => x.player_id === p.id) : null;
      const story = idBible && idBible.d ? (idBible.d.characters || []).find((c) => c.call_sign === p.name) : null;
      body(box, JSON.stringify(["identity", p.id, row, idErr, !!idData, idData && idData.required, story, idBible && idBible.err]), (b) => fillIdentity(b, p, row));
      return;
    }
    if (tab === "archive") {
      body(box, `archive:${p.id}`, (b) => fillPrivateArchive(b, p));
      return;
    }
    if (tab === "activity") {
      const k = `${p.id}:${acScopeId()}`;
      const c = actCache.get(k);
      if (!actFresh(c)) fetchActivity(p.name, p.id).then(() => { if (sel && sel.kind === "captain" && sel.id === p.id) buildInspector(); });
      body(box, JSON.stringify(["act", k, c && c.at]), (b) => fillActivity(b, c ? c.d : null));
      return;
    }
    body(box, JSON.stringify([tab, p, d, tab === "radio" ? radio.map((m) => m.id) : 0]), (b) => {
      if (!d && tab !== "overview") { b.append(el("p", "empty", "Loading…")); return; }
      if (tab === "overview") fillCapOverview(b, p, d);
      if (tab === "ship") fillShip(b, d);
      if (tab === "owner") fillOwner(b, d);
      if (tab === "notes") fillNotes(b, d);
      if (tab === "memory") fillMemory(b, p, d);
      if (tab === "inbox") fillInbox(b, d);
      if (tab === "radio") fillRadio(b, p, radio);
    });
  }
  function fillCapOverview(b, p, d) {
    const ap = d ? `${fmt(d.ap)}` : fmt(p.ap);
    b.append(statRow([["Net worth", p.kind === "npc" ? "–" : compact(p.net_worth)], ["Credits", compact(p.credits)], ["AP", ap, d ? `/ ${fmt(d.ap_max)}` : ""]]));
    const seen = seenTicks(p);
    b.append(section("Position", facts([
      ["Sector", sectorLink(p.sector)], ["Ship", (p.ship || "").replace(/_/g, " "), { t: 1 }],
      ["Fighters", fmt(p.fighters)], ["Shields", fmt(p.shields)],
    ])));
    if (p.kind !== "npc") {
      const ch = p.identity;
      const go = link("Identity tab", () => { inspTab.captain = "identity"; insSig = ""; buildInspector(); });
      if (ch) {
        const box = el("div");
        box.append(facts([["Age", fmt(ch.age)], ["Homeworld", ch.homeworld || "–", { t: 1 }]]), el("p", "", ch.bio || ""));
        const more = el("p", "hint");
        more.append(`Version ${fmt(ch.version)}. Backstory and every version in the `, go, ".");
        box.append(more);
        b.append(section("Character", box));
      } else if (p.documented === false) {
        const none = el("p", S.identity_required ? "md-status due" : "md-status");
        none.append(`${renamePending(p) ? "Rename required and no" : "Undocumented: no"} identity yet${S.identity_required ? ", so it cannot spend action points" : ""}. See the `, go, ".");
        b.append(section("Character", none));
      }
    }
    if (!d) { b.append(el("p", "empty", "Loading details…")); return; }
    b.append(section("Standing", facts([
      ["Bank", fmt(d.bank)], ["Earned", fmt(d.earned)],
      ["Experience", fmt(d.experience)], ["Rank", d.rank, { t: 1 }],
      ["Alignment", `${fmt(d.alignment)}`, { cls: d.alignment < 0 ? "neg" : d.alignment > 0 ? "pos" : "" }], ["Label", d.alignment_label, { t: 1 }],
      ["Kills", fmt(d.stats && d.stats.kills)], ["Deaths", fmt(d.stats && d.stats.deaths)],
      ["Planets", fmt(d.planets_owned)], ["Bounty", d.bounty_on_you ? fmt(d.bounty_on_you) : "none"],
      ["Trades", fmt(d.stats && d.stats.trades)], ["Trade profit", fmt(d.stats && d.stats.trade_profit)],
      ["Charted", fmt(d.sectors_charted)], ["Explored", fmt(d.sectors_explored)],
      ["Corporation", d.corp ? corpLink(d.corp.name, d.corp.ceo) : "none", { t: 1 }], ["Career path", p.career_path || "–", { t: 1 }],
    ])));
    const sc = d.scores && typeof d.scores === "object" ? Object.entries(d.scores).map(([k, v]) => [k, fmt(v)]) : [];
    if (sc.length) b.append(section("Scores", facts(sc)));
    b.append(section("Conduct", facts([
      ["Joined", `tick ${fmt(d.created_tick)}`], ["Last action", seen === null ? "–" : seen === 0 ? "this tick" : `${ticksDur(seen)} ago`],
      ["Protection", d.newbie_protected ? "on" : "off", { t: 1 }], ["Aggressor", d.aggressor ? String(d.aggressor) : "no", { t: 1 }],
      ["Muted", d.muted_until > S.tick ? `until tick ${fmt(d.muted_until)}` : "no", { t: 1 }], ["Banned", d.banned ? "yes" : "no", { t: 1, cls: d.banned ? "neg" : "" }],
      ["Unread notices", fmt(d.unread_notices)], ["Unread radio", fmt(d.unread_radio)],
      ["Mutes (operator only)", mnMutesText(d.mention_mutes), { t: 1 }],
      ["Operator msgs", fmt(p.operator_unread)], ["Announcements", fmt(p.announcements_unacked)],
      ["Seasons played", fmt(d.career && d.career.seasons_played)], ["Best finish", d.career && d.career.best_finish ? `#${d.career.best_finish}` : "–"],
    ])));
  }
  function fillShip(b, d) {
    const sh = d.ship || {}, h = sh.holds || {};
    b.append(section("Ship", facts([
      ["Class", sh.model || sh.class, { t: 1 }], ["Value", fmt(sh.value)],
      ["Name", untrusted(sh.name) || "–", { t: 1 }], ["Warp cost", `${fmt(sh.warp_ap)} AP`],
      ["Fighters", `${fmt(sh.fighters)} / ${fmt(sh.fighters_max)}`], ["Shields", `${fmt(sh.shields)} / ${fmt(sh.shields_max)}`],
      ["Holds", `${fmt(h.total)} / ${fmt(h.max)}`], ["Free", fmt(h.free)],
      ["Fuel ore", fmt(h.fuel_ore)], ["Organics", fmt(h.organics)],
      ["Equipment", fmt(h.equipment)], ["Colonists", fmt(h.colonists)],
      ["Armid mines", fmt(sh.armid_mines)], ["Limpet mines", fmt(sh.limpet_mines)],
      ["Genesis", fmt(sh.genesis_torpedoes)], ["Probes", fmt(sh.ether_probes)],
      ["Beacons", fmt(sh.beacons)], ["Scanners", [sh.density_scanner && "density", sh.holo_scanner && "holo"].filter(Boolean).join(", ") || "none", { t: 1 }],
      ["Transwarp", sh.transwarp_drive ? "yes" : "no", { t: 1 }],
    ])));
    const doc = d.doctrine && Object.keys(d.doctrine).length ? facts(Object.entries(d.doctrine).map(([k, v]) => [k.replace(/_/g, " "), typeof v === "boolean" ? (v ? "yes" : "no") : fmt(v)])) : el("p", "empty", "No standing orders.");
    b.append(section("Doctrine", doc));
    if (d.limpet_tracks && d.limpet_tracks.length) {
      const ul = el("ul", "list");
      for (const t of d.limpet_tracks) { const li = el("li"); li.append(el("span", "nm", t.player), el("span", "r mono", `s${t.sector}`)); ul.append(li); }
      b.append(section("Limpets on this ship", ul));
    }
  }
  function fillOwner(b, d) {
    const o = d.owner;
    if (!o) { b.append(el("p", "empty", "No human setup recorded.")); return; }
    const pref = o.preferences;
    b.append(section("Setup", pref && typeof pref === "object" ? facts(Object.entries(pref).map(([k, v]) => [k.replace(/_/g, " "), typeof v === "object" ? JSON.stringify(v) : v === null ? "–" : String(v), { t: 1 }]), true) : el("p", "empty", "Setup not recorded yet.")));
    const so = o.standing_orders || {};
    b.append(section("Strategy", so.strategy ? el("p", "", so.strategy) : el("p", "empty", "No strategy recorded.")));
    if (so.public_label) b.append(section("Public label", el("p", "", so.public_label)));
    const list = (arr, empty) => {
      if (!arr || !arr.length) return el("p", "empty", empty);
      const ul = el("ul", "textlog");
      for (const x of arr) {
        const li = el("li");
        const m = el("div", "m");
        if (x.id !== undefined) m.append(el("span", "", `#${x.id}`));
        if (x.status) m.append(el("span", "k", x.status));
        if (x.kind) m.append(el("span", "k", x.kind.replace(/_/g, " ")));
        if (x.tick !== undefined) m.append(el("span", "", `tick ${fmt(x.tick)}`));
        li.append(m, document.createTextNode(x.text || JSON.stringify(x)));
        ul.append(li);
      }
      return ul;
    };
    b.append(section("Sent to the human (summaries, replies, strategy proposals)", list(o.messages, "Nothing sent.")));
  }
  // ---------- identity (engine 1.46): the human character the captain plays ----------
  // Every version, former names and the one rename, from /identities (refetched after 15 s
  // while the tab is open, and when an identity is filed or a captain renamed).
  function loadIdentities() {
    if (idBusy) return;
    idBusy = true;
    api(`${API}/identities`).then((r) => {
      idBusy = false;
      idAt = Date.now();
      if (r.ok && r.data && Array.isArray(r.data.identities)) { idData = r.data; idErr = null; }
      else if (r.status !== 401) idErr = r.status === 404 ? "This server keeps no identities (engine 1.46 or later)." : errText(r.data, r.status);
      if (sel && sel.kind === "captain") buildInspector();
      if (mode === "captains") { capSig = ""; renderCaptains(); }
    });
  }
  function idQuote(text, empty) { return text ? el("blockquote", "fb-text", text) : el("p", "empty", empty); }
  // The full story bible (no since): each character's whole log and timeline, for the Identity tab.
  let idBible = null;
  function loadIdBible() {
    if (idBible && (idBible.busy || Date.now() - idBible.at < 15000)) return;
    idBible = { ...(idBible || {}), busy: true };
    api(`${API}/chronicle/bible`).then((r) => {
      idBible = r.ok && r.data ? { d: r.data, at: Date.now() } : { d: idBible.d, at: Date.now(), err: r.status === 404 ? "This server has no story bible (engine 1.46 or later)." : errText(r.data, r.status) };
      if (sel && sel.kind === "captain" && inspTab.captain === "identity") buildInspector();
    });
  }
  // Personality, speech, values and dream: free text the captain wrote.
  function idTraits(v) {
    const box = el("div", "id-traits");
    for (const [k, label] of [["personality", "Personality"], ["speech", "Speech"], ["values", "Values"], ["dream", "Dream"]]) {
      if (!v[k]) continue;
      box.append(el("div", "hint", label), el("blockquote", "fb-text", v[k]));
    }
    if (!box.childNodes.length) box.append(el("p", "empty", "Not written."));
    return box;
  }
  // Relations: [{name, relation, captain (a call sign), status, note, private}].
  function idRelations(rels) {
    if (!rels || !rels.length) return el("p", "empty", "None recorded.");
    const ul = el("ul", "textlog");
    for (const r of rels) {
      const li = el("li");
      const m = el("div", "m");
      m.append(el("span", "k", r.relation || "relation"));
      if (r.status) m.append(el("span", "", r.status));
      if (r.captain) { const c = el("span"); c.append("captain ", captainLink(r.captain)); m.append(c); }
      if (r.private) m.append(tag("private", "warn", "Only the captain and the operator see it"));
      li.append(m, document.createTextNode(`${r.name}${r.note ? `: ${r.note}` : ""}`));
      ul.append(li);
    }
    return ul;
  }
  // Log lines (the character's diary), newest first: text, mood, about, citations.
  function idLog(entries, showPrivate) {
    if (!entries || !entries.length) return el("p", "empty", "No log lines yet.");
    const ul = el("ul", "textlog");
    for (const e of entries) {
      const li = el("li", e.private ? "id-private" : "");
      const m = el("div", "m");
      m.append(el("span", "", `#${e.n}`), el("span", "", `tick ${fmt(e.tick)}`));
      if (e.at_ms) m.append(el("span", "", stamp(e.at_ms)));
      if (e.mood) m.append(el("span", "k", e.mood));
      if (showPrivate && e.private) m.append(tag("private", "warn", "Only the captain and the operator see it"));
      li.append(m, document.createTextNode(e.text || ""));
      if ((e.about || []).length) { const a = el("div", "hint"); a.append("About "); e.about.forEach((n, i) => { if (i) a.append(", "); a.append(captainLink(n)); }); li.append(a); }
      // Mentions on a public line (engine 1.58): names the line notified, under current names.
      if ((e.mentions || []).length) li.append(el("div", "hint", `Mentions: ${e.mentions.join(", ")}`));
      const cs = chCitations(e.citations);
      if (cs) li.append(cs);
      ul.append(li);
    }
    return ul;
  }
  // Moments: the engine's public record of the character's game events, newest first.
  function idMoments(ms) {
    if (!ms || !ms.length) return el("p", "empty", "No moments yet.");
    const ul = el("ul", "textlog");
    for (const x of ms) {
      const li = el("li");
      const m = el("div", "m");
      m.append(el("span", "", `tick ${fmt(x.tick)}`), el("span", "k", (x.kind || "").replace(/_/g, " ")));
      if ((x.with || []).length) m.append(el("span", "", `with ${x.with.join(", ")}`));
      li.append(m, document.createTextNode(x.text || ""));
      ul.append(li);
    }
    return ul;
  }
  function fillIdentity(b, p, row) {
    if (!row) { b.append(el("p", "empty", idErr || (idData ? "No identity record for this captain." : "Loading…"))); return; }
    const cur = row.current;
    const vs = row.versions || [];
    const req = !!(idData && idData.required);
    const gender = cur ? genderOf(cur.pronouns) : null;
    const gate = req ? " Until then it cannot spend action points (identity_required is on)." : " identity_required is off, so it can still spend action points.";
    b.append(el("p", row.documented ? "md-status" : "md-status due", row.documented
      ? `Documented: version ${cur ? cur.version : "–"} of ${vs.length}.`
      : row.rename_required && cur && gender
        ? `Rename required: it must use its one rename (identity action=set name=…) to a character's name sharing no word with "${p.name}".${gate}`
        : cur && !gender
          ? `Incomplete: pronouns "${cur.pronouns || ""}" are not he/him or she/her (every character is male or female, engine 1.47). It must file them again${row.rename_required ? " and use its mandatory rename" : ""}.${gate}`
          : `Undocumented: no identity yet${row.rename_required ? ", and its rename is required" : ""}.${gate}`));
    const former = row.former_names || [];
    const rename = isNum(row.renamed_tick)
      ? `Used at tick ${fmt(row.renamed_tick)}${former.length ? `: formerly ${former.join(", ")}` : ""}. The call sign is fixed now.`
      : row.rename_required ? "Required and not yet used: the operator asked every captain to take a character's name." : "Not used: one change of call sign is still available.";
    if (cur) {
      b.append(section("Character", facts([
        ["Name", cur.name, { t: 1 }], ["Age", fmt(cur.age)], ["Gender", gender || "not he/him or she/her", { t: 1, cls: gender ? "" : "warn-t" }], ["Pronouns", cur.pronouns || "–", { t: 1 }],
        ["Homeworld", cur.homeworld || "–", { t: 1 }], ["Trade", cur.trade || "–", { t: 1 }],
        ["Written", `tick ${fmt(cur.tick)}${cur.at_ms ? ` · ${ago(cur.at_ms)}` : ""}`, { t: 1 }], ["Season", `${fmt(cur.season)}${cur.epoch ? ` · epoch ${cur.epoch}` : ""}`],
      ], true)));
      b.append(section("Appearance and voice", facts([["Appearance", cur.appearance || "–", { t: 1 }], ["Voice", cur.voice || "–", { t: 1 }]], true)));
      b.append(section("Public bio", idQuote(cur.bio, "No bio.")));
      b.append(section("Why they fly", idQuote(cur.why, "Not given.")));
      b.append(section("Personality", idTraits(cur)));
      const rels = (cur.relations || []).map((r) => ({ ...r, captain: isNum(r.captain) ? ((player(r.captain) || {}).name || `#${r.captain}`) : r.captain }));
      b.append(section(`Relations (${rels.length})`, idRelations(rels)));
      const bs = el("div", "note");
      bs.append(el("p", "hint", "Private: other captains, its human and the public never see it. Captains are told the operator can read backstories."), markdown(cur.backstory || ""));
      b.append(section("Backstory (private)", bs));
    }
    b.append(section("Call sign", facts([["Now", p.name, { t: 1 }], ["Rename", rename, { t: 1, cls: row.rename_required ? "warn-t" : "" }], former.length ? ["Former names", former.join(", "), { t: 1 }] : undefined].filter(Boolean), true)));
    // The diary and the timeline come from the story bible (the full log, private lines included).
    const ch = idBible && idBible.d ? (idBible.d.characters || []).find((c) => c.call_sign === p.name) : null;
    const story = idBible && idBible.err ? el("p", "empty", idBible.err) : !ch ? el("p", "empty", idBible && idBible.d ? "No story recorded." : "Loading…") : null;
    b.append(section(`Log (${isNum(row.log_entries) ? row.log_entries : ch ? ch.log.length : "–"})`, story || idLog([...ch.log].reverse(), true)));
    b.append(section(`Moments (${isNum(row.moments) ? row.moments : ch ? ch.moments.length : "–"})`, story || idMoments([...ch.moments].reverse())));
    if (vs.length) {
      const wrap = el("div");
      for (const v of [...vs].reverse()) {
        const d = el("details", "lg-v");
        d.append(el("summary", "", `v${v.version} · ${v.name} · tick ${fmt(v.tick)} · season ${v.season}${v.at_ms ? ` · ${stamp(v.at_ms)}` : ""}`));
        d.append(facts([
          ["Age", fmt(v.age)], ["Pronouns", v.pronouns || "–", { t: 1 }], ["Homeworld", v.homeworld || "–", { t: 1 }], ["Trade", v.trade || "–", { t: 1 }],
          ["Appearance", v.appearance || "–", { t: 1 }], ["Voice", v.voice || "–", { t: 1 }],
        ], true), idTraits(v));
        if ((v.relations || []).length) d.append(idRelations(v.relations.map((r) => ({ ...r, captain: isNum(r.captain) ? ((player(r.captain) || {}).name || `#${r.captain}`) : r.captain }))));
        d.append(el("div", "hint", "Bio"), idQuote(v.bio, "No bio."), el("div", "hint", "Why"), idQuote(v.why, "Not given."), el("div", "hint", "Backstory (private)"), markdown(v.backstory || ""));
        wrap.append(d);
      }
      b.append(section(`Versions (${vs.length})`, wrap));
    }
  }
  // Markdown for player-written notes. Untrusted text: every node is built with textContent,
  // never innerHTML; links are shown as text with their address, never made clickable.
  function mdInline(text) {
    const frag = document.createDocumentFragment();
    const re = /(`[^`]+`|\*\*[^*]+\*\*|__[^_]+__|\*[^*\s][^*]*\*|_[^_\s][^_]*_|\[[^\]]+\]\([^)]+\))/g;
    let last = 0, m;
    while ((m = re.exec(text))) {
      if (m.index > last) frag.append(document.createTextNode(text.slice(last, m.index)));
      const t = m[0];
      if (t[0] === "`") frag.append(el("code", "", t.slice(1, -1)));
      else if (t.startsWith("**") || t.startsWith("__")) frag.append(el("strong", "", t.slice(2, -2)));
      else if (t[0] === "[") { const mm = /^\[([^\]]+)\]\(([^)]+)\)$/.exec(t); frag.append(el("span", "md-link", mm ? `${mm[1]} (${mm[2]})` : t)); }
      else frag.append(el("em", "", t.slice(1, -1)));
      last = m.index + t.length;
    }
    if (last < text.length) frag.append(document.createTextNode(text.slice(last)));
    return frag;
  }
  function markdown(src) {
    const root = el("div", "md");
    const lines = String(src || "").replace(/\r\n?/g, "\n").split("\n");
    let list = null, para = null, code = null;
    const flush = () => { list = null; para = null; };
    for (const raw of lines) {
      if (code) {
        if (/^\s*```/.test(raw)) { code = null; continue; }
        code.textContent += (code.textContent ? "\n" : "") + raw;
        continue;
      }
      if (/^\s*```/.test(raw)) { flush(); code = el("pre", "md-code"); root.append(code); continue; }
      const line = raw.trimEnd();
      if (!line.trim()) { flush(); continue; }
      let m;
      if ((m = /^(#{1,6})\s+(.*)$/.exec(line))) { flush(); const h = el(`h${Math.min(6, m[1].length + 3)}`, "md-h"); h.append(mdInline(m[2])); root.append(h); continue; }
      if (/^\s*([-*_])(\s*\1){2,}\s*$/.test(line)) { flush(); root.append(el("hr")); continue; }
      if ((m = /^\s*>\s?(.*)$/.exec(line))) { flush(); const q = el("blockquote"); q.append(mdInline(m[1])); root.append(q); continue; }
      if ((m = /^(\s*)([-*+]|\d+[.)])\s+(.*)$/.exec(line))) {
        const ordered = /\d/.test(m[2]);
        if (!list || list.tagName !== (ordered ? "OL" : "UL")) { para = null; list = el(ordered ? "ol" : "ul"); root.append(list); }
        const li = el("li");
        const box = /^\[( |x|X)\]\s+(.*)$/.exec(m[3]);
        if (box) li.append(el("span", "md-box", box[1].trim() ? "☑ " : "☐ "), mdInline(box[2]));
        else li.append(mdInline(m[3]));
        list.append(li);
        continue;
      }
      list = null;
      if (!para) { para = el("p"); root.append(para); } else para.append(document.createTextNode(" "));
      para.append(mdInline(line.trim()));
    }
    return root;
  }
  function fillNotes(b, d) {
    const notes = d.notes || {};
    const plan = d.plan || {};
    const head = el("div", "note plan");
    const status = plan.text == null ? "No plan written yet (mandatory)." : plan.updated_tick == null ? "Written before plan times were recorded." : `Last written at tick ${fmt(plan.updated_tick)}.`;
    head.append(el("h4", "", "plan"), el("p", plan.due ? "md-status due" : "md-status", status));
    if (plan.text != null) head.append(markdown(plan.text));
    b.append(head);
    const rest = Object.entries(notes).filter(([k]) => k !== "plan");
    for (const [k, v] of rest) {
      const n = el("div", "note");
      n.append(el("h4", "", k), markdown(typeof v === "string" ? v : JSON.stringify(v, null, 2)));
      b.append(n);
    }
    if (!rest.length && plan.text == null) b.append(el("p", "empty", "No other notes."));
  }
  function fillPrivateArchive(b, p) {
    b.append(el("p", "hint", "Permanent confidential research history. Archived bodies are unavailable to captains. This includes saved versions and archive events; nothing here can be restored through captain tools."));
    const controls = el("div", "row");
    const kind = el("select");
    for (const [value, label] of [["", "Notes and memory"], ["note", "Notes"], ["memory", "Memory"]]) {
      const option = el("option", "", label); option.value = value; kind.append(option);
    }
    const key = el("input"); key.placeholder = "Key (optional)";
    const events = el("div");
    let afterSeq = 0, afterOrdinal = 0, generation = 0;
    const more = btn("Load history", "sm", async () => {
      const requestGeneration = generation;
      more.disabled = true;
      const q = new URLSearchParams({ name: String(p.id), after_seq: String(afterSeq), after_ordinal: String(afterOrdinal), limit: "20" });
      if (kind.value) q.set("kind", kind.value);
      if (key.value.trim()) q.set("key", key.value.trim());
      const result = await api(`${API}/private_archive?${q}`);
      if (requestGeneration !== generation) return;
      more.disabled = false;
      if (!result.ok) { events.append(el("p", "empty", errText(result.data, result.status))); return; }
      for (const event of result.data.events || []) {
        const n = el("div", "note");
        n.append(el("h4", "", `${event.kind}: ${event.key}`), el("p", "hint", `${event.reason} · tick ${fmt(event.tick)} · journal ${fmt(event.seq)}${event.legacy ? " · historical reconstruction" : ""}`));
        if (event.kind === "note") n.append(markdown(event.body.text));
        else n.append(el("pre", "", JSON.stringify(event.body, null, 2)));
        events.append(n);
      }
      if (!events.childNodes.length) events.append(el("p", "empty", "No matching history."));
      const next = result.data.continuation.next_arguments;
      afterSeq = next.after_seq; afterOrdinal = next.after_ordinal;
      more.textContent = "More history"; more.hidden = !result.data.continuation.has_more;
    });
    const reset = () => { generation++; afterSeq = 0; afterOrdinal = 0; events.replaceChildren(); more.disabled = false; more.hidden = false; more.textContent = "Load history"; };
    kind.addEventListener("change", reset); key.addEventListener("input", reset);
    controls.append(kind, key); b.append(controls, more, events);
  }
  function fillMemory(b, p, d) {
    const records = d.memory || [];
    if (!records.length) { b.append(el("p", "empty", "No structured memory.")); return; }
    for (const r of records) {
      const n = el("div", "note");
      n.append(el("h4", "", r.key), el("p", "hint", `Revision ${fmt(r.revision)} · tick ${fmt(r.tick)} · ${r.expires_tick == null ? "No expiry" : `${r.archived ? "Expired" : "Expires"} at tick ${fmt(r.expires_tick)}`}`));
      n.append(el("pre", "", JSON.stringify(r.data, null, 2)));
      const history = el("div");
      let offset = 0;
      const more = btn("View history", "sm", async () => {
        more.disabled = true;
        const q = new URLSearchParams({ name: String(p.id), action: "history", key: r.key, offset: String(offset), limit: "10" });
        const result = await api(`${API}/memory?${q}`);
        more.disabled = false;
        if (!result.ok) { history.replaceChildren(el("p", "empty", errText(result.data, result.status))); return; }
        for (const v of result.data.versions || []) {
          history.append(el("h4", "", `Revision ${fmt(v.revision)} · tick ${fmt(v.tick)}${v.expires_tick == null ? "" : ` · expiry tick ${fmt(v.expires_tick)}`}`), el("pre", "", JSON.stringify(v.data, null, 2)));
        }
        offset = result.data.continuation.next_arguments.offset;
        more.textContent = "More history";
        more.hidden = !result.data.continuation.has_more;
      });
      n.append(more, history);
      b.append(n);
    }
  }
  function fillInbox(b, d) {
    const items = d.inbox || [];
    if (!items.length) { b.append(el("p", "empty", "Empty.")); return; }
    const ul = el("ul", "textlog");
    for (const n of items) {
      const li = el("li", n.read ? "" : "unread");
      const m = el("div", "m");
      m.append(el("span", "", `tick ${fmt(n.tick)}`), el("span", "k", n.kind || ""));
      if (n.mention) m.append(tag(n.mention.via_corp ? "mentions its corporation" : "mention", "info"));
      if (isNum(n.superseded_by)) m.append(tag(`folded into #${n.superseded_by}`, "", "A later notice of the same kind replaced this one"));
      li.append(m, document.createTextNode(n.text));
      // Player words on a notice are untrusted text: textContent only.
      const q = untrusted(n.quote);
      if (q !== undefined && q !== null) { const bq = el("blockquote", "fb-text", String(q)); bq.title = "Untrusted player text"; li.append(bq); }
      if (n.struck_quote) li.append(el("div", "hint", "Original (operator only)"), el("blockquote", "fb-text", String(n.struck_quote)));
      ul.append(li);
    }
    b.append(ul);
  }
  function fillRadio(b, p, radio) {
    const open = el("p", "");
    open.append(link("Open in Comms", () => { setMode("comms"); openThread(`cap:${p.id}`); }));
    if (p.kind !== "npc") open.append(document.createTextNode(" · "), link("Sysop conversation", () => { setMode("comms"); openThread(`sys:${p.id}`); }));
    b.append(section("Recent radio", open));
    if (!radio.length) { b.append(el("p", "empty", "No recent radio.")); return; }
    const ul = el("ul", "textlog");
    for (const m of radio.slice(0, 60)) {
      const li = el("li");
      const meta = el("div", "m");
      meta.append(el("span", "", `tick ${fmt(m.tick)}`), el("span", "k", m.channel === "private" ? `${m.from} to ${m.to_id === 0 ? "sysop" : m.to}` : m.channel === "corp" ? `${m.from} on ${m.corp}` : `${m.from}, public`));
      li.append(meta, document.createTextNode(m.text));
      li.style.cursor = "pointer";
      li.addEventListener("click", () => select("msg", m.id));
      ul.append(li);
    }
    b.append(ul);
  }
  function fillActivity(b, d) {
    if (!d) { b.append(el("p", "empty", "Loading the interaction log…")); return; }
    if (d.error) { b.append(el("p", "empty", d.error)); return; }
    const hours = acScopeText().toLowerCase();
    b.append(statRow([["Calls", fmt(d.calls), hours.replace("last ", "")], ["Refused", fmt(d.refused), d.calls ? `${Math.round((d.refused / d.calls) * 100)}%` : ""], ["Last call", d.last_ms ? dur(Date.now() - d.last_ms) : "never", d.last_ms ? "ago" : ""]]));
    if (d.by_hour && d.by_hour.length) {
      const axis = el("div", "axis");
      axis.append(el("span", "", stamp(d.by_hour[0].hour_start_ms)), el("span", "", "now"));
      b.append(section("Calls per hour", bars(d.by_hour), axis));
    } else if (d.by_day && d.by_day.length) {
      const axis = el("div", "axis");
      axis.append(el("span", "", acDay(d.by_day[0].day_start_ms)), el("span", "", acDay(d.by_day[d.by_day.length - 1].day_start_ms)));
      const bx = bars(d.by_day.map((x) => ({ calls: x.calls, label: `${x.day} (UTC)${x.exact ? "" : ", approximate"}` })));
      [...bx.children].forEach((sp, i) => { if (!d.by_day[i].exact) sp.classList.add("approx"); });
      b.append(section("Calls per day", bx, axis));
      if (d.season && d.season.exact === false) b.append(el("p", "hint", d.season.note));
    }
    const codes = Object.entries(d.refusal_codes || {});
    if (codes.length) b.append(section("Refusals", facts(codes.map(([k, v]) => [k, fmt(v)]))));
    const tools = el("ul", "list");
    for (const t of (d.by_tool || []).slice(0, 20)) {
      const li = el("li");
      li.title = `${t.ok} ok, ${t.refused} refused${Object.keys(t.refusal_codes || {}).length ? ` (${Object.entries(t.refusal_codes).map(([k, v]) => `${k} ${v}`).join(", ")})` : ""} · avg ${fmt(t.avg_ms)} ms · last ${!t.last_ms ? "–" : acSeason() ? stamp(t.last_ms) : ago(t.last_ms)}`;
      li.append(el("span", "nm mono", t.tool), el("span", "r mono", `${fmt(t.calls)}${t.refused ? ` · ${t.refused} refused` : ""}`));
      tools.append(li);
    }
    b.append(section("By tool", (d.by_tool || []).length ? tools : el("p", "empty", "No calls in this window.")));
    const ses = el("ul", "list");
    for (const s of (d.sessions || []).slice(0, 12)) {
      const li = el("li");
      li.title = s.sequence || "";
      li.append(el("span", "nm", s.client || "unknown client"), el("span", "r mono", `${clockT(s.first_ms).slice(0, 5)} to ${clockT(s.last_ms).slice(0, 5)} · ${fmt(s.calls)}`));
      ses.append(li);
    }
    if ((d.sessions || []).length) b.append(section("Sessions", ses));
    const calls = el("ul", "textlog");
    for (const c of (d.recent || []).slice(0, 40)) {
      const li = el("li");
      const m = el("div", "m");
      m.append(el("span", "", clockT(c.at_ms)), el("span", "k", c.tool), el("span", c.ok ? "" : "neg", c.ok ? `${c.ms} ms` : c.code || "refused"));
      li.append(m);
      if (c.args) li.append(document.createTextNode(JSON.stringify(c.args)));
      calls.append(li);
    }
    if ((d.recent || []).length) b.append(section("Latest calls", calls));
  }

  // Action forms (inline, under the action bar).
  function actionForm(p, d) {
    const f = el("form", "actform");
    f.addEventListener("submit", (e) => e.preventDefault());
    const foot = (...kids) => { const x = el("div", "foot"); x.append(...kids); return x; };
    const cancel = () => btn("Cancel", "sm ghost", () => { actOpen = null; buildInspector(); });
    const num = (ph, label) => { const i = el("input"); i.type = "number"; i.inputMode = "numeric"; i.placeholder = ph || ""; if (label) i.setAttribute("aria-label", label); return i; };
    const fld = (label, input) => { const l = el("label", "fld"); l.append(el("span", "", label), input); return l; };
    if (actOpen === "message") {
      f.append(el("h4", "", `Operator message to ${p.name}`), el("p", "hint", "Trusted. Shown in its briefing until read. It replies by private radio to sysop."));
      const t = el("textarea"); t.rows = 3; t.maxLength = 1000; t.placeholder = "Write the message"; t.setAttribute("aria-label", "Message");
      const tell = el("input"); tell.type = "checkbox";
      const tl = el("label", "check"); tl.append(tell, document.createTextNode(" Ask it to tell its human"));
      const send = btn("Send", "sm primary", async () => {
        const text = t.value.trim();
        if (!text) { t.focus(); return; }
        if (await act("message", { player: p.id, text, tell_owner: tell.checked }, `Message to ${p.name}`)) { t.value = ""; tell.checked = false; actOpen = null; buildInspector(); }
      });
      t.addEventListener("keydown", (e) => { if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) { e.preventDefault(); send.click(); } });
      f.append(t, foot(tl, el("span", "grow"), cancel(), send));
      setTimeout(() => t.focus(), 0);
    } else if (actOpen === "grant") {
      f.append(el("h4", "", `Grant to ${p.name}`), el("p", "hint", "Adds to what it has. Negative numbers take away."));
      const c = num("credits", "Credits"), a = num("AP", "AP");
      const row = el("div", "row"); row.append(fld("Credits", c), fld("AP", a));
      f.append(row, foot(cancel(), btn("Grant", "sm primary", async () => {
        const credits = intOf(c) || 0, ap = intOf(a) || 0;
        if (Number.isNaN(credits) || Number.isNaN(ap) || (!credits && !ap)) { toast("err", "Grant", "Enter credits and/or AP as whole numbers."); return; }
        if (await act("grant", { player: p.id, credits, ap }, `Grant to ${p.name}`)) { c.value = ""; a.value = ""; }
      })));
      setTimeout(() => c.focus(), 0);
    } else if (actOpen === "edit") {
      f.append(el("h4", "", `Edit ${p.name}`), el("p", "hint", "Blank fields keep their current value (shown greyed)."));
      const FIELDS = ["credits", "bank", "ap", "alignment", "experience", "fighters", "shields", "sector"];
      const g = el("div", "grid3");
      const inputs = {};
      for (const k of FIELDS) { const i = num("", k); i.dataset.field = k; inputs[k] = i; g.append(fld(k, i)); }
      const model = el("input"); model.type = "text"; model.dataset.field = "model"; model.maxLength = 80;
      const harness = el("input"); harness.type = "text"; harness.dataset.field = "harness"; harness.maxLength = 80;
      const row = el("div", "row"); row.append(fld("Public model", model), fld("Harness", harness));
      const prot = btn("End protection", "sm", async () => { if (await act("set_player", { player: p.id, end_protection: true }, `Protection · ${p.name}`)) buildInspector(); });
      prot.dataset.role = "prot";
      prot.hidden = !(p.newbie_protected);
      f.append(g, row, foot(prot, el("span", "grow"), cancel(), btn("Apply", "sm primary", async () => {
        const b = { player: p.id };
        for (const k of FIELDS) {
          const v = intOf(inputs[k]);
          if (v === null) continue;
          if (Number.isNaN(v)) { toast("err", "Edit", `${k} must be a whole number.`); return; }
          if (k === "sector" && v < 1) { toast("err", "Edit", "Sector must be 1 or more."); return; }
          b[k] = v;
        }
        if (model.value.trim()) b.model = model.value.trim();
        if (harness.value.trim()) b.harness = harness.value.trim();
        if (Object.keys(b).length === 1) { toast("err", "Edit", "Fill in the fields to change."); return; }
        if (await act("set_player", b, `Edit ${p.name}`)) { for (const i of Object.values(inputs)) i.value = ""; model.value = ""; harness.value = ""; }
      })));
      refreshForm(f, p, d);
    } else if (actOpen === "move") {
      moveTarget = p.id;
      f.append(el("h4", "", `Move ${p.name}`), el("p", "hint", "Click a sector on the map, or type its number."));
      const s = num(`now ${p.sector}`, "Destination sector");
      s.dataset.role = "move-to";
      const row = el("div", "row"); row.append(fld("Destination sector", s));
      f.append(row, foot(cancel(), btn("Move", "sm primary", async () => {
        const v = intOf(s);
        if (!v || Number.isNaN(v) || v < 1) { toast("err", "Move", "Pick a sector on the map or type one."); return; }
        if (await act("set_player", { player: p.id, sector: v }, `Move ${p.name}`)) { s.value = ""; map.centerOn(v); }
      })));
      setTimeout(() => s.focus(), 0);
    } else if (actOpen === "mute") {
      const muted = p.muted_until > S.tick;
      f.append(el("h4", "", muted ? `${p.name} is muted until tick ${fmt(p.muted_until)}` : `Mute ${p.name} on the radio`));
      const t = num("ticks", "Ticks"); t.value = "60";
      const row = el("div", "row"); row.append(fld(`Ticks (60 = ${ticksDur(60)})`, t));
      const kids = [cancel()];
      if (muted) kids.unshift(btn("Unmute", "sm", async () => { if (await act("mute", { player: p.id, ticks: 0 }, `Unmute ${p.name}`)) { actOpen = null; buildInspector(); } }));
      kids.push(btn(muted ? "Extend" : "Mute", "sm primary", async () => {
        const v = intOf(t);
        if (!v || Number.isNaN(v) || v < 1) { toast("err", "Mute", "Mute for at least 1 tick."); return; }
        if (await act("mute", { player: p.id, ticks: v }, `Mute ${p.name}`)) { actOpen = null; buildInspector(); }
      }));
      f.append(row, foot(...kids));
    } else if (actOpen === "access") {
      f.append(el("h4", "", `Access for ${p.name}`), el("p", "hint", "Secrets are shown once; the server keeps only hashes. Hand them over privately."));
      const slot = el("div", "stack");
      const rotate = btn("Rotate token", "sm", async () => {
        if (!await confirmDlg({ title: `Rotate ${p.name}'s token?`, body: ["Its current token stops working at once; any agent connected with it is cut off. The new token is shown once."], ok: "Rotate token", danger: false })) return;
        const r = await act("rotate_token", { player: p.id }, `Token · ${p.name}`);
        if (r && r.token) slot.replaceChildren(secretBox(`New token for ${p.name}.`, r.token, [["MCP URL", `${(me && me.public_url) || location.origin}/mcp/${r.token}`]]));
      });
      const rec = btn("Issue recovery key", "sm", async () => {
        if (!await confirmDlg({ title: `Issue a recovery key for ${p.name}?`, body: ["Replaces any earlier recovery key. The captain calls recover_access with its call sign and this key to get a new token, keeping the same captain."], ok: "Issue key", danger: false })) return;
        const r = await act("recovery_key", { player: p.id }, `Recovery key · ${p.name}`);
        if (r && r.recovery_key) slot.replaceChildren(secretBox(`Recovery key for ${p.name}.`, r.recovery_key));
      });
      const own = btn("Issue owner link", "sm", async () => {
        if (!await confirmDlg({ title: `Issue an owner link for ${p.name}?`, body: ["Switches it to verified mode once used: setup (silent or strategy) and the strategy then come only from that link. Replaces any earlier owner token."], ok: "Issue link", danger: false })) return;
        const r = await act("owner_token", { player: p.id }, `Owner link · ${p.name}`);
        if (r && r.owner_token) slot.replaceChildren(secretBox(`Owner token for ${p.name}'s human.`, r.owner_token, [["owner URL", r.owner_mcp_url]]));
      });
      const row = el("div", "row"); row.append(rotate, rec, own);
      f.append(row, slot, foot(cancel()));
    }
    return f;
  }
  // Keep placeholders (current values) fresh while a form is open, never the typed text.
  function refreshForm(f, p, d) {
    for (const i of f.querySelectorAll("input[data-field]")) {
      const k = i.dataset.field;
      const cur = !d ? p[k] : k === "fighters" || k === "shields" ? d.ship && d.ship[k] : d[k];
      i.placeholder = cur === undefined || cur === null ? "" : String(cur);
    }
    const prot = f.querySelector("[data-role=prot]");
    if (prot) prot.hidden = !(d ? d.newbie_protected : p.newbie_protected);
    const mv = f.querySelector("[data-role=move-to]");
    if (mv) mv.placeholder = `now ${p.sector}`;
  }
  async function ban(p) {
    if (!await confirmDlg({ title: `Ban ${p.name}?`, body: [`${p.name} is locked out at once: its token, owner token and recovery key all stop working. Its ship, credits and history stay in the world. Unban to let it back in.`], ok: "Ban captain", typed: p.name })) return;
    await act("ban", { player: p.id }, `Ban ${p.name}`);
  }
  async function unban(p) {
    if (!await confirmDlg({ title: `Unban ${p.name}?`, body: ["Lifts the ban. Its old token was replaced when it was banned: rotate a token or issue a recovery key so it can reconnect."], ok: "Unban", danger: false })) return;
    if (await act("unban", { player: p.id }, `Unban ${p.name}`)) { actOpen = "access"; buildInspector(); }
  }
  async function removeCaptain(p) {
    if (!await confirmDlg({ title: `Remove ${p.name}?`, body: [`Deletes ${p.name} from the universe and frees the call sign. Only works for captains who own nothing yet (no planets, corporation, deployed defenses, contracts or bounties); otherwise ban instead.`], ok: "Remove captain", typed: p.name })) return;
    if (await act("remove_player", { player: p.id }, `Remove ${p.name}`)) select(null);
  }

  // ---------- sector ----------
  function renderSectorInsp(box) {
    const id = sel.id;
    const s = map.sector(id);
    if (!s) { region(box, "head", head({ kind: "Sector", title: `Sector ${id}`, sub: "Not in this galaxy." })); return; }
    const sub = [s.fed ? "FedSpace" : null, s.port === "STARDOCK" ? "Stardock" : s.port ? `Port ${s.port}` : "No port", s.neb].filter(Boolean).join(" · ");
    const tags = [];
    if (s.fed) tags.push(tag("no combat", "info"));
    if (s.bm) tags.push(tag("black market", "warn"));
    const conc = (S.concessions || []).find((c) => c.sector === id);
    if (conc) tags.push(tag(`concession: ${conc.name}`));
    region(box, "head", head({ kind: "Sector", title: `Sector ${id}`, sub, tags }));
    const actions = el("div", "actions");
    const lc = lastCaptain !== null && player(lastCaptain);
    if (lc) actions.append(btn(lc.sector === id ? `${lc.name} is here` : `Move ${lc.name} here`, "sm", async () => {
      if (await act("set_player", { player: lc.id, sector: id }, `Move ${lc.name}`)) soonState(200);
    }, "Moves the last captain you selected"));
    if (lc && lc.sector === id) actions.lastChild.disabled = true;
    actions.append(btn("Spawn Skarn here", "sm", () => act("spawn_skarn", { sector: id }, "Skarn")));
    const aSig = JSON.stringify([lc && lc.id, lc && lc.sector === id]);
    const oldA = box.querySelector(":scope > [data-role=actions]");
    if (!oldA || oldA.dataset.sig !== aSig) { actions.dataset.sig = aSig; region(box, "actions", actions); }
    const ships = S.players.filter((p) => p.sector === id);
    const planets = S.planets.filter((p) => p.sector === id);
    const def = S.defenses.find((d) => d.sector === id);
    const evs = events.filter((e) => e.sector === id).slice(-12).reverse();
    body(box, JSON.stringify([ships.map((p) => [p.id, p.ship]), planets, def, evs.map((e) => e.id), conc]), (b) => {
      const ul = el("ul", "list");
      for (const p of ships) {
        const li = el("li", "click");
        li.append(dot(p.name, p.kind === "npc" && p.npc), el("span", "nm", p.name), el("span", "r", p.kind === "npc" ? p.npc || "npc" : (p.ship || "").replace(/_/g, " ")));
        li.addEventListener("click", () => select("captain", p.id));
        ul.append(li);
      }
      b.append(section(`Ships (${ships.length})`, ships.length ? ul : el("p", "empty", "Empty space.")));
      if (planets.length) {
        const pl = el("ul", "list");
        for (const p of planets) {
          const li = el("li", "click");
          li.append(el("span", "nm", p.name), el("span", "r", p.owner ? p.owner : `class ${p.class}, unclaimed`));
          li.addEventListener("click", () => select("planet", p.id));
          pl.append(li);
        }
        b.append(section("Planets", pl));
      }
      if (def) b.append(section("Defenses", facts([["Fighters", def.fighters ? `${fmt(def.fighters.count)} (${def.fighters.mode})` : "none"], ["Owner", def.fighters ? def.fighters.owner : "–", { t: 1 }], ["Mines", fmt(def.mines)]])));
      if (conc) b.append(section("Concession", facts([["Name", conc.name, { t: 1 }], ["Holder", conc.holder || "none", { t: 1 }], ["Yield per day", fmt(conc.yield_per_day)], ["Paid so far", fmt(conc.paid_total)], ["Needs fighters", fmt(conc.needs_fighters)]])));
      const lanes = el("ul", "list");
      const inbound = [];
      for (const o of map_list()) if (o.w.includes(id) && !s.w.includes(o.id)) inbound.push(o.id);
      for (const t of s.w) {
        const o = map.sector(t);
        const li = el("li", "click");
        const one = o && !o.w.includes(id);
        li.append(el("span", "mono", String(t)), el("span", "r", [o && o.fed ? "FedSpace" : null, o && o.port ? (o.port === "STARDOCK" ? "Stardock" : o.port) : null, one ? "one-way" : null].filter(Boolean).join(" · ")));
        li.addEventListener("click", () => select("sector", t, { pan: true }));
        lanes.append(li);
      }
      b.append(section(`Lanes out (${s.w.length})`, lanes));
      if (inbound.length) b.append(section("One-way lanes in", facts([["From", inbound.join(", ")]], true)));
      if (evs.length) b.append(section("Recent events here", eventLog(evs)));
      b.append(section("Coordinates", facts([["x", s.x.toFixed(1)], ["y", s.y.toFixed(1)]])));
    });
  }
  const map_list = () => { const out = []; for (let i = 1; i <= map.sectorCount() + 50; i++) { const s = map.sector(i); if (s) out.push(s); } return out; };
  function eventLog(evs) {
    const ul = el("ul", "textlog");
    for (const e of evs) {
      const li = el("li");
      const m = el("div", "m");
      m.append(el("span", "", `tick ${fmt(e.tick)}`), el("span", "k", e.kind.replace(/_/g, " ")));
      li.append(m, document.createTextNode(e.text));
      ul.append(li);
    }
    return ul;
  }

  // ---------- planet ----------
  function renderPlanetInsp(box) {
    const p = S.planets.find((x) => x.id === sel.id);
    if (!p) { region(box, "head", head({ kind: "Planet", title: `#${sel.id}`, sub: "No longer exists." })); return; }
    region(box, "head", head({ kind: "Planet", title: p.name, id: `#${p.id}`, sub: `Class ${p.class} · sector ${p.sector}`, tags: [p.owner ? tag(p.owner) : tag("unclaimed")] }));
    const evs = events.filter((e) => e.sector === p.sector && CATS.planet.kinds.includes(e.kind)).slice(-10).reverse();
    body(box, JSON.stringify([p, evs.map((e) => e.id)]), (b) => {
      b.append(section("Planet", facts([["Owner", p.owner ? captainLink(p.owner) : "unclaimed"], ["Sector", sectorLink(p.sector)], ["Class", p.class, { t: 1 }], ["Citadel", fmt(p.citadel)], ["Colonists", fmt(p.colonists)], ["Fighters", fmt(p.fighters)]])));
      if (evs.length) b.append(section("Recent events", eventLog(evs)));
    });
  }

  // ---------- corporation ----------
  const corpOf = (id) => S.corps.find((x) => x.id === id);
  const corpWars = (c) => (S.wars || []).filter((x) => x.attacker === c.name || x.defender === c.name);
  const corpTreaties = (c) => (S.treaties || []).filter((x) => (x.parties || []).includes(c.name));
  const liveWar = (x) => x.status !== "ended";
  // Operator edits to a corporation (journaled admin commands; membership follows the game's rules).
  function corpForm(c, members) {
    const f = el("form", "actform");
    f.addEventListener("submit", (e) => e.preventDefault());
    const fld = (label, input) => { const l = el("label", "fld"); l.append(el("span", "", label), input); return l; };
    const foot = (...kids) => { const x = el("div", "foot"); x.append(...kids); return x; };
    const cancel = () => btn("Cancel", "sm ghost", () => { actOpen = null; buildInspector(); });
    const pick = (list, empty) => {
      const sel2 = el("select");
      sel2.append(Object.assign(el("option", "", empty), { value: "" }), ...list.map((p) => Object.assign(el("option", "", p.name + (p.corp && p.corp !== c.name ? ` (in ${p.corp})` : "")), { value: String(p.id) })));
      return sel2;
    };
    if (actOpen === "corp-edit") {
      f.append(el("h4", "", `Edit ${c.name}`), el("p", "hint", "Change only what you fill in. Renames are announced in the news."));
      const name = el("input"); name.type = "text"; name.maxLength = 24; name.value = c.name;
      const motto = el("input"); motto.type = "text"; motto.maxLength = 120; motto.value = untrusted(c.motto) || "";
      const ceo = el("select");
      ceo.append(...members.map((p) => Object.assign(el("option", "", p.name), { value: String(p.id), selected: p.name === c.ceo })));
      const tre = el("input"); tre.type = "number"; tre.min = "0"; tre.inputMode = "numeric"; tre.placeholder = String(c.treasury);
      const row1 = el("div", "row"); row1.append(fld("Name", name), fld("CEO", ceo));
      const row2 = el("div", "row"); row2.append(fld("Treasury (set)", tre));
      f.append(row1, fld("Motto", motto), row2, foot(cancel(), btn("Apply", "sm primary", async () => {
        const body = { corp: c.id };
        if (name.value.trim() && name.value.trim() !== c.name) body.name = name.value.trim();
        if (motto.value !== (untrusted(c.motto) || "")) body.motto = motto.value;
        const ceoP = members.find((p) => String(p.id) === ceo.value);
        if (ceoP && ceoP.name !== c.ceo) body.ceo = ceoP.id;
        if (tre.value.trim() !== "") { const v = Math.round(Number(tre.value)); if (!Number.isFinite(v) || v < 0) { toast("err", "Edit", "Treasury must be 0 or more."); return; } body.treasury = v; }
        if (Object.keys(body).length === 1) { toast("err", "Edit", "Nothing changed."); return; }
        if (await act("set_corp", body, `Edit ${c.name}`)) { actOpen = null; buildInspector(); }
      })));
    } else if (actOpen === "corp-add") {
      const cands = captains().filter((p) => p.corp !== c.name && !p.banned).sort((x, y) => x.name.localeCompare(y.name));
      f.append(el("h4", "", `Add a member to ${c.name}`), el("p", "hint", "Joins at once, without an invitation. A captain in another corporation leaves it first."));
      const who = pick(cands, "Choose a captain");
      f.append(fld("Captain", who), foot(cancel(), btn("Add", "sm primary", async () => {
        const p = cands.find((x) => String(x.id) === who.value);
        if (!p) { toast("err", "Add member", "Choose a captain."); return; }
        if (p.corp && !await confirmDlg({ title: `Move ${p.name} to ${c.name}?`, body: [`${p.name} leaves ${p.corp} first (its CEO seat passes on, or ${p.corp} dissolves if empty).`], ok: "Move captain", danger: false })) return;
        if (await act("corp_add_member", { corp: c.id, player: p.id }, `Add to ${c.name}`)) { actOpen = null; buildInspector(); }
      })));
    } else if (actOpen === "corp-invite") {
      const cands = captains().filter((p) => !p.corp && !p.banned && !(c.invites || []).includes(p.name)).sort((x, y) => x.name.localeCompare(y.name));
      f.append(el("h4", "", `Invite a captain to ${c.name}`), el("p", "hint", "It gets a notice and accepts with corp action=join. Only captains in no corporation."));
      const who = pick(cands, cands.length ? "Choose a captain" : "No captain is free to invite");
      f.append(fld("Captain", who), foot(cancel(), btn("Invite", "sm primary", async () => {
        const p = cands.find((x) => String(x.id) === who.value);
        if (!p) { toast("err", "Invite", "Choose a captain."); return; }
        if (await act("corp_invite", { corp: c.id, player: p.id }, `Invite to ${c.name}`)) { actOpen = null; buildInspector(); }
      })));
    }
    return f;
  }
  async function disbandCorp(c) {
    if (!await confirmDlg({ title: `Disband ${c.name}?`, body: [`Every member leaves (the CEO last). The treasury of ${fmt(c.treasury)} goes to ${c.ceo}. Planets stay with their owners. This is announced in the news and cannot be undone.`], ok: "Disband", typed: c.name })) return;
    if (await act("disband_corp", { corp: c.id }, `Disband ${c.name}`)) select(null);
  }
  function renderCorpInsp(box) {
    const c = corpOf(sel.id);
    if (!c) { region(box, "head", head({ kind: "Corporation", title: `#${sel.id}`, sub: "Dissolved." })); return; }
    const members = c.members.map(playerByName).filter(Boolean);
    const wars = corpWars(c), treaties = corpTreaties(c);
    const tags = [];
    if (wars.some(liveWar)) tags.push(tag(`at war (${wars.filter(liveWar).length})`, "crit"));
    if (treaties.length) tags.push(tag(plural(treaties.length, "treaty", "treaties"), "info"));
    if ((c.invites || []).length) tags.push(tag(`${c.invites.length} invited`, "", c.invites.join(", ")));
    if (members.some((p) => p.banned)) tags.push(tag("banned member", "warn"));
    region(box, "head", head({ kind: "Corporation", title: c.name, id: `#${c.id}`, sub: [c.ceo && `CEO ${c.ceo}`, isNum(c.created_tick) ? `founded tick ${fmt(c.created_tick)}` : null, plural(members.length, "member")].filter(Boolean).join(" · "), tags }));

    const actions = el("div", "actions");
    const toggle = (k, label) => {
      const b2 = el("button", "btn sm", label);
      b2.type = "button";
      b2.setAttribute("aria-pressed", String(actOpen === k));
      b2.addEventListener("click", () => { actOpen = actOpen === k ? null : k; buildInspector(); });
      return b2;
    };
    actions.append(
      toggle("corp-message", "Message"), toggle("corp-edit", "Edit"), toggle("corp-add", "Add member"), toggle("corp-invite", "Invite"),
      btn("Channel", "sm", () => { setMode("comms"); openThread(`c:${c.id}`); }),
      btn("Frame", "sm ghost", () => { syncFocus(true); if (mobile()) setMView("map"); }, "Frame members, planets and defenses on the map"),
      btn("Disband", "sm danger", () => disbandCorp(c)),
    );
    const aSig = JSON.stringify([actOpen, c.id]);
    const oldA = box.querySelector(":scope > [data-role=actions]");
    if (!oldA || oldA.dataset.sig !== aSig) { actions.dataset.sig = aSig; region(box, "actions", actions); }
    const oldF = box.querySelector(":scope > [data-role=form]");
    const corpForms = ["corp-edit", "corp-add", "corp-invite"];
    if (corpForms.includes(actOpen) && (!oldF || oldF.dataset.k !== actOpen)) {
      const f = corpForm(c, members);
      f.dataset.k = actOpen;
      region(box, "form", f);
    } else if (actOpen === "corp-message" && (!oldF || oldF.dataset.k !== actOpen)) {
      const f = el("form", "actform");
      f.dataset.k = actOpen;
      f.addEventListener("submit", (e) => e.preventDefault());
      f.append(el("h4", "", `Operator message to every member of ${c.name}`), el("p", "hint", `Trusted; in each member's briefing until read (${plural(members.length, "captain")}). They reply by private radio to sysop.`));
      const t = el("textarea"); t.rows = 3; t.maxLength = 1000; t.placeholder = "Write the message"; t.setAttribute("aria-label", "Message");
      const tell = el("input"); tell.type = "checkbox";
      const tl = el("label", "check"); tl.append(tell, document.createTextNode(" Ask them to tell their humans"));
      const send = btn("Send", "sm primary", async () => {
        const text = t.value.trim();
        if (!text) { t.focus(); return; }
        if (await act("message_corp", { corp: c.id, text, tell_owner: tell.checked }, `Message to ${c.name}`)) { actOpen = null; buildInspector(); }
      });
      t.addEventListener("keydown", (e) => { if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) { e.preventDefault(); send.click(); } });
      const foot = el("div", "foot");
      foot.append(tl, el("span", "grow"), btn("Cancel", "sm ghost", () => { actOpen = null; buildInspector(); }), send);
      f.append(t, foot);
      region(box, "form", f);
      setTimeout(() => t.focus(), 0);
    } else if (actOpen !== "corp-message" && !corpForms.includes(actOpen) && oldF) oldF.remove();

    const planets = S.planets.filter((p) => p.owner && c.members.includes(p.owner));
    const defenses = S.defenses.filter((d) => d.fighters && c.members.includes(d.fighters.owner));
    const conc = (S.concessions || []).filter((x) => x.holder_corp === c.name || c.members.includes(x.holder));
    const chatter = (S.radio || []).filter((m) => m.channel === "corp" && m.corp_id === c.id);
    const act6 = actAlerts ? new Map((actAlerts.players || []).map((p) => [p.id, p])) : null;
    const tabsDef = [["overview", "Overview"], ["members", "Members", members.length], ["territory", "Territory", planets.length + defenses.length + conc.length], ["diplomacy", "Diplomacy", wars.length + treaties.length], ["government", "Government", ((c.government || {}).measures || []).filter((m) => m.status === "open").length], ["radio", "Radio", chatter.length]];
    const tb = tabsBar("corp", tabsDef);
    const oldT = box.querySelector(":scope > [data-role=tabs]");
    const tSig = JSON.stringify([tabsDef, inspTab.corp]);
    if (!oldT || oldT.dataset.sig !== tSig) { tb.dataset.sig = tSig; region(box, "tabs", tb); }
    const tab = inspTab.corp;
    body(box, JSON.stringify([tab, c, members.map((p) => [p.id, p.net_worth, p.sector, p.ap, p.last_action_tick]), planets, defenses, conc, wars, treaties, chatter.map((m) => m.id), actAlerts && actAlerts.at]), (b) => {
      const nw = members.reduce((a, p) => a + (p.net_worth || 0), 0);
      if (tab === "overview") {
        b.append(statRow([["Net worth", compact(nw)], ["Treasury", compact(c.treasury)], ["Members", fmt(members.length)]]));
        b.append(section("Corporation", facts([
          ["CEO", c.ceo ? captainLink(c.ceo) : "–"], ["Founded", isNum(c.created_tick) ? `tick ${fmt(c.created_tick)}` : "–"],
          ["Planets", fmt(planets.length)], ["Defended sectors", fmt(defenses.length)],
          ["Concessions", fmt(conc.length)], ["Invites", (c.invites || []).length ? c.invites.join(", ") : "none", { t: 1 }],
          ["Wars", fmt(wars.filter(liveWar).length)], ["Treaties", fmt(treaties.length)],
        ])));
        if (c.motto) b.append(section("Motto", el("blockquote", "", untrusted(c.motto))));
        const ul = el("ul", "list");
        for (const p of [...members].sort((x, y) => y.net_worth - x.net_worth).slice(0, 6)) {
          const li = el("li", "click");
          li.append(dot(p.name), el("span", "nm", p.name), el("span", "r mono", compact(p.net_worth)));
          li.addEventListener("click", () => select("captain", p.id, { pan: true }));
          ul.append(li);
        }
        b.append(section("Top members", ul));
      }
      if (tab === "government") {
        const g = c.government || {};
        const rec = c.record || {};
        b.append(statRow([["Form", g.form || "autocracy"], ["Tax", `${((g.tax_bp || 0) / 100).toFixed(2)}%`], ["Free funds", compact(c.free_funds || 0)]]));
        b.append(section("Record", facts([
          ["Term ends", isNum(g.term_ends_tick) ? `tick ${fmt(g.term_ends_tick)}` : "–"], ["Tax collected", compact(rec.tax_collected || 0)],
          ["Grants paid", compact(rec.grants_paid || 0)], ["Elections", fmt(rec.elections || 0)], ["Measures passed", fmt(rec.measures_passed || 0)],
        ])));
        const t = c.territory;
        if (t) {
          const seat = isNum(t.seat) ? `${t.seat}${t.seat_held ? "" : " (lost)"}${t.seat_chosen ? "" : " (default)"}` : "–";
          b.append(section("Territory", facts([
            ["Seat", seat], ["Connected", fmt(t.connected_sectors || 0)], ["Outposts", fmt(t.outpost_count || 0)],
            ["Depot", t.depot ? `planet ${t.depot.planet}` : "no"], ["Seat may change", isNum(t.seat_cooldown_ends_tick) ? `tick ${fmt(t.seat_cooldown_ends_tick)}` : "now"],
            ["Concession bonus", compact(rec.concession_bonus_total || 0)],
          ])));
        }
        const open = (g.measures || []).filter((m) => m.status === "open");
        const mu = el("ul", "list");
        for (const m of [...(g.measures || [])].reverse().slice(0, 20)) {
          const li = el("li");
          const tally = m.kind === "election" ? Object.entries(m.ballots || {}).map(([k, v]) => `${k} ${v}`).join(", ") || "no ballots" : `${m.yes || 0} yes, ${m.no || 0} no of ${m.electorate}`;
          li.append(el("span", "nm", `#${m.id} ${m.change}`), el("span", "r mono", `${m.status} · ${tally}`));
          if (m.reason && m.reason.untrusted_player_text) li.append(el("div", "dim", untrusted(m.reason.untrusted_player_text)));
          mu.append(li);
        }
        const lw = el("ul", "list");
        for (const l of g.laws || []) {
          const li = el("li");
          li.append(el("span", "nm", `#${l.id} ${l.law}${l.title && untrusted(l.title) ? ` · ${untrusted(l.title)}` : ""}`), el("span", "r mono", `fine ${compact(l.fine)}`));
          lw.append(li);
        }
        b.append(section(`Laws (${(g.laws || []).length})`, (g.laws || []).length ? lw : el("p", "dim", "No laws.")));
        const ow = el("ul", "list");
        for (const o of (g.offences || []).slice(0, 20)) {
          const li = el("li");
          li.append(el("span", "nm", `#${o.id} ${o.offender} · ${o.kind}`), el("span", "r mono", `${o.status} · ${compact(o.paid)}/${compact(o.fine)}`));
          ow.append(li);
        }
        b.append(section(`Offences (${fmt(rec.offences || 0)} recorded, ${compact(rec.fines_collected || 0)} collected)`, (g.offences || []).length ? ow : el("p", "dim", "None.")));
        b.append(section(`Measures (${open.length} open)`, (g.measures || []).length ? mu : el("p", "dim", "None yet.")));
        const lu = el("ul", "list");
        for (const d of (g.ledger || []).slice(0, 40)) {
          const li = el("li");
          li.append(el("span", "nm", d.summary), el("span", "r mono", `t${fmt(d.tick)}`));
          lu.append(li);
        }
        b.append(section(`Ledger (${g.ledger_entries || (g.ledger || []).length})`, (g.ledger || []).length ? lu : el("p", "dim", "No decisions yet.")));
      }
      if (tab === "members") {
        const ul = el("ul", "list");
        for (const p of [...members].sort((x, y) => y.net_worth - x.net_worth)) {
          const li = el("li", "click");
          const a6 = act6 && act6.get(p.id);
          const seen = seenTicks(p);
          const right = [`s${p.sector}`, `${fmt(p.ap)} AP`, compact(p.net_worth)];
          const nmBox = el("span", "nm");
          nmBox.append(document.createTextNode(p.name));
          li.append(dot(p.name), nmBox);
          if (p.name === c.ceo) li.append(tag("CEO", "info"));
          if (p.banned) li.append(tag("banned", "crit"));
          li.append(el("span", "r mono", right.join(" · ")));
          const rm = el("button", "btn xs ghost", "Remove");
          rm.type = "button";
          rm.title = `Remove ${p.name} from ${c.name}`;
          rm.addEventListener("click", async (e) => {
            e.stopPropagation();
            const last = members.length === 1;
            if (!await confirmDlg({ title: `Remove ${p.name} from ${c.name}?`, body: [last ? `${p.name} is the last member: ${c.name} dissolves and its treasury goes to ${p.name}.` : p.name === c.ceo ? `${p.name} is the CEO; the seat passes to another member, as when a CEO leaves.` : `${p.name} leaves the corporation and loses its shared starchart.`], ok: "Remove member" })) return;
            await act("corp_remove_member", { corp: c.id, player: p.id }, `Remove from ${c.name}`);
          });
          li.append(rm);
          li.title = [p.model, p.harness, seen === null ? null : `active ${seen === 0 ? "now" : `${ticksDur(seen)} ago`}`, a6 ? `${a6.calls} calls, ${a6.refused} refused in 6 h` : null].filter(Boolean).join(" · ");
          li.addEventListener("click", () => select("captain", p.id, { pan: true }));
          ul.append(li);
        }
        b.append(section(`Members (${members.length})`, members.length ? ul : el("p", "empty", "No members.")));
        const inv = el("ul", "list");
        for (const n of c.invites || []) {
          const ip = playerByName(n);
          const li = el("li");
          li.append(dot(n), el("span", "nm", n));
          const w = el("button", "btn xs ghost", "Withdraw");
          w.type = "button";
          w.addEventListener("click", async () => { if (ip) await act("corp_uninvite", { corp: c.id, player: ip.id }, `Invitation to ${c.name}`); });
          li.append(el("span", "r", "invited"), w);
          inv.append(li);
        }
        if ((c.invites || []).length) b.append(section(`Invitations (${c.invites.length})`, inv));
        if (act6) {
          const rows = members.map((p) => [p, act6.get(p.id)]);
          const ul2 = el("ul", "list");
          for (const [p, a6] of rows) {
            const li = el("li");
            const quiet = a6 && a6.last_call_ms ? Date.now() - a6.last_call_ms : null;
            li.append(el("span", "nm", p.name), el("span", `r mono${quiet === null || quiet > 3600e3 ? " warn-t" : ""}`, a6 && a6.calls ? `${fmt(a6.calls)} calls · ${fmt(a6.refused)} refused · ${dur(quiet)} ago` : "no calls"));
            ul2.append(li);
          }
          b.append(section("Activity, last 6 h", ul2));
        }
      }
      if (tab === "territory") {
        const pl = el("ul", "list");
        for (const p of planets) {
          const li = el("li", "click");
          li.append(el("span", "nm", p.name), el("span", "r", `${p.owner} · s${p.sector}${p.citadel ? ` · citadel ${p.citadel}` : ""}`));
          li.addEventListener("click", () => select("planet", p.id, { pan: true }));
          pl.append(li);
        }
        b.append(section(`Planets (${planets.length})`, planets.length ? pl : el("p", "empty", "No planets.")));
        const dl = el("ul", "list");
        for (const d of defenses) {
          const li = el("li", "click");
          li.append(el("span", "mono", `s${d.sector}`), el("span", "r", `${fmt(d.fighters.count)} fighters (${d.fighters.mode}) · ${d.fighters.owner}${d.mines ? ` · ${fmt(d.mines)} mines` : ""}`));
          li.addEventListener("click", () => select("sector", d.sector, { pan: true }));
          dl.append(li);
        }
        b.append(section(`Deployed defenses (${defenses.length})`, defenses.length ? dl : el("p", "empty", "None deployed.")));
        const cl = el("ul", "list");
        for (const x of conc) {
          const li = el("li", "click");
          li.append(el("span", "nm", x.name), el("span", "r", `${x.holder || "–"} · ${fmt(x.yield_per_day)}/day · s${x.sector}`));
          li.addEventListener("click", () => select("sector", x.sector, { pan: true }));
          cl.append(li);
        }
        b.append(section(`Concessions (${conc.length})`, conc.length ? cl : el("p", "empty", "None held.")));
      }
      if (tab === "diplomacy") {
        const list = (items, line, none) => {
          if (!items.length) return el("p", "empty", none);
          const ul = el("ul", "textlog");
          for (const it of items) { const li = el("li"); const [m, t] = line(it); const meta = el("div", "m"); meta.append(el("span", "k", m)); li.append(meta, document.createTextNode(t)); ul.append(li); }
          return ul;
        };
        b.append(section("Wars", list(wars, (w) => [`${w.status} · declared tick ${fmt(w.declared_tick)}`, `${w.attacker} against ${w.defender}${untrusted(w.objective) ? `: ${untrusted(w.objective)}` : ""}${w.kills ? ` (kills ${w.kills.attacker} to ${w.kills.defender})` : ""}`], "No wars.")));
        b.append(section("Treaties", list(treaties, (t) => [`${t.kind} · ${t.status}`, `${(t.parties || []).join(" and ")}${untrusted(t.terms) ? `: ${untrusted(t.terms)}` : ""}`], "No treaties.")));
      }
      if (tab === "radio") {
        const open = el("p");
        open.append(link("Open the channel in Comms", () => { setMode("comms"); openThread(`c:${c.id}`); }));
        b.append(section("Corporation channel", open));
        if (!chatter.length) b.append(el("p", "empty", "No recent traffic."));
        else {
          const ul = el("ul", "textlog");
          for (const m of chatter.slice(0, 40)) {
            const li = el("li");
            const meta = el("div", "m");
            meta.append(el("span", "", `tick ${fmt(m.tick)}`), el("span", "k", m.from));
            li.append(meta, document.createTextNode(m.text));
            li.style.cursor = "pointer";
            li.addEventListener("click", () => select("msg", m.id));
            ul.append(li);
          }
          b.append(ul);
        }
      }
    });
  }

  // ======================================================================
  // Corporations workbench
  // ======================================================================
  let coQ = "", coSort = store.get("coSort", "net_worth"), coSig = "";
  function renderCorps() {
    const q = coQ.toLowerCase();
    const rows = S.corps.map((c) => {
      const planets = S.planets.filter((p) => p.owner && c.members.includes(p.owner)).length;
      const nw = isNum(c.net_worth) ? c.net_worth : c.members.map(playerByName).filter(Boolean).reduce((a, p) => a + p.net_worth, 0);
      return { c, planets, nw, war: corpWars(c).some(liveWar), treaties: corpTreaties(c).length };
    }).filter(({ c }) => !q || [c.name, c.ceo, c.motto, ...c.members].some((v) => v && String(v).toLowerCase().includes(q)));
    const by = { net_worth: (r) => -r.nw, members: (r) => -r.c.members.length, treasury: (r) => -r.c.treasury, planets: (r) => -r.planets, name: (r) => r.c.name.toLowerCase() };
    rows.sort((a, b) => { const x = by[coSort](a), y = by[coSort](b); return x < y ? -1 : x > y ? 1 : 0; });
    $("co-count").textContent = `${rows.length} of ${S.corps.length}`;
    const sig = JSON.stringify([coSort, rows.map((r) => [r.c.id, r.c.members.length, r.c.treasury, r.nw, r.planets, r.war, r.treaties, r.c.ceo])]);
    if (sig === coSig) { markSelected(); return; }
    coSig = sig;
    const frag = document.createDocumentFragment();
    for (const r of rows) {
      const tr = el("tr");
      tr.dataset.sel = selKey("corp", r.c.id);
      const td = el("td", "fill");
      const w = el("span", "who");
      w.append(el("span", "nm", r.c.name));
      td.append(w);
      const line = el("div", "ident");
      if (r.war) line.append(tag("war", "crit"), document.createTextNode(" "));
      line.append(document.createTextNode([r.c.ceo && `CEO ${r.c.ceo}`, untrusted(r.c.motto)].filter(Boolean).join(" · ")));
      td.append(line);
      td.title = [r.c.name, r.c.ceo && `CEO ${r.c.ceo}`, `members: ${r.c.members.join(", ")}`].filter(Boolean).join("\n");
      tr.append(td, el("td", "n", fmt(r.c.members.length)), el("td", "n", compact(r.nw)), el("td", "n c-tre", compact(r.c.treasury)), el("td", "n c-pl", fmt(r.planets)));
      tr.addEventListener("click", () => select("corp", r.c.id, { pan: true }));
      frag.append(tr);
    }
    if (!rows.length) { const tr = el("tr", "empty"); const td = el("td", "", q ? "No corporation matches." : "No corporations yet. Captains found them with the corp tool."); td.colSpan = 5; tr.append(td); frag.append(tr); }
    $("co-body").replaceChildren(frag);
    markSelected();
  }
  $("co-q").addEventListener("input", (e) => { coQ = e.target.value.trim(); coSig = ""; renderCorps(); });
  $("co-new").addEventListener("click", () => {
    const box = $("co-form");
    if (!box.hidden) { box.hidden = true; return; }
    const free = captains().filter((p) => !p.corp && !p.banned).sort((x, y) => x.name.localeCompare(y.name));
    const ceo = $("co-new-ceo");
    ceo.replaceChildren(Object.assign(el("option", "", free.length ? "Choose its CEO" : "No captain is free"), { value: "" }), ...free.map((p) => Object.assign(el("option", "", p.name), { value: String(p.id) })));
    box.hidden = false;
    $("co-new-name").focus();
  });
  $("co-form").addEventListener("submit", async (e) => {
    e.preventDefault();
    const name = $("co-new-name").value.trim();
    const ceo = Number($("co-new-ceo").value);
    if (!name || !ceo) { toast("err", "Charter", "Give a name and choose a CEO."); return; }
    const d = await act("create_corp", { name, ceo, motto: $("co-new-motto").value.trim() || null }, "Charter");
    if (d) { $("co-form").hidden = true; $("co-form").reset(); await loadState(); if (d.corp && d.corp.id) select("corp", d.corp.id, { pan: true }); }
  });
  $("co-new-cancel").addEventListener("click", () => { $("co-form").hidden = true; });
  $("co-sort").value = coSort;
  $("co-sort").addEventListener("change", (e) => { coSort = e.target.value; store.set("coSort", coSort); coSig = ""; renderCorps(); });

  // ---------- radio message ----------
  function renderMsgInsp(box) {
    const m = (S.radio || []).find((x) => x.id === sel.id);
    if (!m) { region(box, "head", head({ kind: "Radio", title: `Message #${sel.id}`, sub: "Deleted, or older than the last 200 messages." })); return; }
    const where = m.channel === "private" ? `Private to ${m.to_id === 0 ? "sysop" : m.to}` : m.channel === "corp" ? `Corp channel ${m.corp}` : "Public radio";
    region(box, "head", head({ kind: "Radio message", title: m.from, titleDot: dot(m.from, m.system ? "fed" : null), id: `#${m.id}`, sub: `${where} · tick ${fmt(m.tick)}` }));
    const actions = el("div", "actions");
    const from = player(m.from_id);
    if (from && from.kind !== "npc") actions.append(btn(`Message ${m.from}`, "sm", () => select("captain", from.id, { action: "message" })));
    if (from) actions.append(btn("Open sender", "sm ghost", () => select("captain", from.id, { pan: true })));
    actions.append(btn("Delete", "sm danger", async () => {
      if (!await confirmDlg({ title: "Delete this radio message?", body: [`From ${m.from} on ${where.toLowerCase()}, tick ${m.tick}. It disappears for every captain and every viewer.`, el("blockquote", "", m.text)], ok: "Delete message" })) return;
      if (await act("delete_radio", { id: m.id }, "Moderation")) { select(null); soonState(200); }
    }));
    if (!box.querySelector(":scope > [data-role=actions]")) region(box, "actions", actions);
    body(box, JSON.stringify(m), (b) => {
      b.append(section("Message", el("blockquote", "", m.text)));
      const mn = m.mentions || {};
      const names = [...(mn.captains || []).map((c) => c.name), ...(mn.corps || []).map((c) => `corp:${c.name}`)];
      const lang = m.language ? `${m.language.name || ""} · #${m.language.id} · v${m.language.version}` : undefined;
      b.append(section("Details", facts([["From", from ? captainLink(m.from) : m.from], ["Model", m.from_model || "–", { t: 1 }], ["Channel", m.channel, { t: 1 }], ["To", m.channel === "private" ? (m.to_id === 0 ? "sysop" : captainLink(m.to)) : m.corp || "everyone"], ["Tick", fmt(m.tick)], ["Sent", stamp(sentAt(m)), { t: 1 }],
        ["Mentions", names.length ? names.join(", ") : undefined, { t: 1 }], ["Language", lang, { t: 1 }]])));
    });
  }

  // ---------- journal entry ----------
  function renderEntryInsp(box) {
    const e = jEntries.find((x) => x.seq === sel.id);
    if (!e) { region(box, "head", head({ kind: "Journal entry", title: `seq ${fmt(sel.id)}`, sub: "Open the Journal to load it." })); return; }
    region(box, "head", head({ kind: "Journal entry", title: e.cmd.type === "admin" ? e.cmd.args.action : e.cmd.type, id: `seq ${fmt(e.seq)}`, sub: `${e.actor === "system" ? "sysop or system" : `${e.player || "?"} (${e.actor})`} · ${new Date(e.at_ms).toLocaleString()}` }));
    const p = e.player && playerByName(e.player);
    if (p && !box.querySelector(":scope > [data-role=actions]")) { const a = el("div", "actions"); a.append(btn(`Open ${p.name}`, "sm", () => select("captain", p.id, { pan: true }))); region(box, "actions", a); }
    body(box, String(e.seq), (b) => {
      b.append(section("Summary", el("p", "", summarize(e.cmd))));
      b.append(section("Command", el("pre", "json", JSON.stringify({ seq: e.seq, at: new Date(e.at_ms).toISOString(), actor: e.actor, player: e.player, cmd: e.cmd }, null, 2))));
    });
  }

  // ---------- calls with no captain ----------
  function renderAnonInsp(box) {
    region(box, "head", head({ kind: "Interaction log", title: "Not logged in", sub: "Failed registrations, unauthenticated calls and MCP requests the transport refused." }));
    const d = acData && acData.not_logged_in;
    if (!acData) loadActivity().then(() => { if (sel && sel.kind === "anon") buildInspector(); });
    body(box, JSON.stringify([acData && acData.calls, d && d.last_ms]), (b) => fillActivity(b, d ? { ...d, last_ms: d.last_ms } : null));
  }

  // ======================================================================
  // Live event dock
  // ======================================================================
  const dockOn = store.get("dockCats", { combat: true, planet: true, trade: true, radio: true, move: false, world: true });
  if (dockOn.feedback === undefined) dockOn.feedback = true; // saved before the category existed
  let dockPaused = false, dockPending = 0, dockTimer = null;
  const counts = {};
  function buildChips() {
    const box = $("ev-chips");
    box.replaceChildren(...Object.entries(CATS).map(([k, c]) => {
      const b = el("button", "chip");
      b.type = "button";
      b.setAttribute("aria-pressed", String(!!dockOn[k]));
      const i = el("i"); i.style.background = c.color;
      b.append(i, document.createTextNode(c.label), el("small", "", counts[k] ? compact(counts[k]) : ""));
      b.addEventListener("click", () => { dockOn[k] = !dockOn[k]; store.set("dockCats", dockOn); buildChips(); renderDock(); });
      return b;
    }));
  }
  function queueDock(e) {
    counts[e.cat] = (counts[e.cat] || 0) + 1;
    if (dockPaused) { dockPending++; $("ev-pause").textContent = `Resume (${dockPending})`; return; }
    if (!dockTimer) dockTimer = setTimeout(() => { dockTimer = null; renderDock(); buildChips(); }, 250);
  }
  let dockSig = "";
  function renderDock() {
    if (dockPaused) return;
    const only = $("ev-sel").checked && sel;
    const rows = [];
    for (let i = events.length - 1; i >= 0 && rows.length < 200; i--) {
      const e = events[i];
      if (!dockOn[e.cat]) continue;
      if (only) {
        if (sel.kind === "captain" && !(e.players || []).includes(sel.id)) continue;
        if (sel.kind === "sector" && e.sector !== sel.id) continue;
        if (sel.kind === "planet") { const p = S && S.planets.find((x) => x.id === sel.id); if (!p || e.sector !== p.sector) continue; }
      }
      rows.push(e);
    }
    const sig = JSON.stringify([rows.length && rows[0].id, rows.length, only && sel]);
    if (sig === dockSig) return;
    const topId = dockSig ? JSON.parse(dockSig)[0] : null;
    dockSig = sig;
    const frag = document.createDocumentFragment();
    for (const e of rows) {
      const tr = el("tr", [e.cat === "combat" || e.cat === "world" ? "hot" : "", topId && e.id > topId && !reduceMotion ? "fresh" : ""].join(" ").trim());
      tr.dataset.row = `ev:${e.id}`;
      const t = el("td", "t", clockT(tickTime(e.tick) || e.at));
      t.title = `tick ${fmt(e.tick)}`;
      const k = el("td", "k");
      const i = el("i"); i.style.background = CATS[e.cat].color;
      k.append(i, document.createTextNode(e.kind.replace(/_/g, " ")));
      const x = el("td", "x fill", e.text);
      x.title = e.text;
      tr.append(t, k, x, el("td", "s", e.sector ? `s${e.sector}` : ""));
      tr.addEventListener("click", () => {
        if (e.kind === "feedback" && e.data && isNum(e.data.id)) { select("feedback", e.data.id); return; }
        const p = (e.players || []).map(player).find((q) => q && q.kind !== "npc") || (e.players || []).map(player).find(Boolean);
        if (e.sector && (!p || e.cat !== "radio")) select("sector", e.sector, { pan: true });
        else if (p) select("captain", p.id, { pan: true });
      });
      frag.append(tr);
    }
    if (!rows.length) { const tr = el("tr", "empty"); const td = el("td", "", events.length ? "No events match these filters." : "Waiting for events…"); td.colSpan = 4; tr.append(td); frag.append(tr); }
    $("ev-body").replaceChildren(frag);
  }
  $("ev-sel").addEventListener("change", () => { dockSig = ""; renderDock(); });
  $("ev-pause").addEventListener("click", (e) => {
    dockPaused = !dockPaused;
    e.target.setAttribute("aria-pressed", String(dockPaused));
    e.target.textContent = dockPaused ? "Resume" : "Pause";
    dockPending = 0;
    if (!dockPaused) { dockSig = ""; renderDock(); }
  });

  // ======================================================================
  // Map wiring
  // ======================================================================
  const map = window.SysopMap({
    base: $("map-base"), live: $("map-live"), root: $("map"), colorOf,
    onClick(h) {
      if (moveTarget !== null && h && h.kind === "sector") {
        const inp = document.querySelector("#insp [data-role=move-to]");
        if (inp) { inp.value = String(h.id); inp.focus(); map.setFocus({ players: [moveTarget], sectors: [h.id] }); return; }
      }
      if (!h) { if (sel) select(null); return; }
      select(h.kind, h.id);
    },
    onHover(h, x, y) {
      const tip = $("map-tip");
      if (!h || !S) { tip.hidden = true; $("map-read").textContent = readout(); return; }
      const lines = [];
      if (h.kind === "sector") {
        const s = map.sector(h.id);
        const ships = S.players.filter((p) => p.sector === h.id);
        const pl = S.planets.filter((p) => p.sector === h.id);
        lines.push(["b", `Sector ${h.id}`]);
        const meta = [s.fed && "FedSpace", s.port === "STARDOCK" ? "Stardock" : s.port && `Port ${s.port}`, s.neb].filter(Boolean).join(" · ");
        if (meta) lines.push(["r", meta]);
        if (ships.length) lines.push(["s", `${ships.slice(0, 4).map((p) => p.name).join(", ")}${ships.length > 4 ? ` +${ships.length - 4}` : ""}`]);
        if (pl.length) lines.push(["s", `${plural(pl.length, "planet")}${pl.some((p) => p.owner) ? `, owned by ${[...new Set(pl.filter((p) => p.owner).map((p) => p.owner))].join(", ")}` : ""}`]);
        if (moveTarget !== null) lines.push(["s", "Click to set as the destination"]);
      } else {
        const p = player(h.id);
        if (!p) { tip.hidden = true; return; }
        lines.push(["b", p.name]);
        lines.push(["r", p.kind === "npc" ? `NPC ${p.npc || ""} · ${(p.ship || "").replace(/_/g, " ")}` : [p.model, (p.ship || "").replace(/_/g, " ")].filter(Boolean).join(" · ")]);
        lines.push(["s", `Sector ${p.sector} · ${compact(p.credits)} cr · ${fmt(p.ap)} AP${p.corp ? ` · ${p.corp}` : ""}`]);
      }
      tip.replaceChildren(...lines.map(([c, t]) => el(c === "b" ? "b" : "div", c === "b" ? "" : c, t)));
      tip.hidden = false;
      const mw = $("map").clientWidth;
      tip.style.left = `${Math.min(x + 14, mw - 270)}px`;
      tip.style.top = `${y + 14}px`;
      $("map-read").textContent = h.kind === "sector" ? `sector ${h.id} · ${readout()}` : readout();
    },
    onView() { $("map-read").textContent = readout(); },
    onMode(on) { show3D(on); },
  });
  function show3D(on) {
    $("map-3d").setAttribute("aria-pressed", String(on));
    store.set("view3d", on);
    $("map-read").textContent = readout();
  }

  const readout = () => `${map.zoom().toFixed(1)}×${map.is3D() ? " · 3D" : ""}`;
  if (store.get("view3d", true)) map.set3D(true, false);
  show3D(map.is3D());
  $("map-3d").addEventListener("click", () => map.toggle3D());
  $("map-fit").addEventListener("click", () => map.fit());
  $("map-zin").addEventListener("click", () => map.zoomIn());
  $("map-zout").addEventListener("click", () => map.zoomOut());
  $("map-goto").addEventListener("submit", (e) => {
    e.preventDefault();
    const n = Number($("map-goto-n").value.trim());
    if (!map.sector(n)) { toast("err", "Go to sector", `There is no sector ${$("map-goto-n").value.trim()}.`); return; }
    $("map-goto-n").value = "";
    $("map-goto-n").blur();
    select("sector", n, { pan: true });
  });
  const LAYERS = [
    ["regions", "FedSpace and nebulae"], ["territory", "Territory"], ["lanes", "Lanes"], ["heights", "Height lines (3D)"], ["labels", "Captain names"], ["npcs", "NPCs"],
    ["planets", "Planets"], ["defenses", "Deployed defenses"], ["traffic", "Traffic (since opened)"], ["hails", "Private hails"],
  ];
  const savedLayers = store.get("layers", null);
  if (savedLayers) for (const [k] of LAYERS) if (k in savedLayers) map.layers[k] = !!savedLayers[k];
  function buildLayers() {
    const pop = $("map-layers");
    pop.replaceChildren(...LAYERS.map(([k, label]) => {
      const l = el("label");
      const c = el("input"); c.type = "checkbox"; c.checked = !!map.layers[k];
      c.addEventListener("change", () => { map.setLayer(k, c.checked); store.set("layers", map.layers); renderLegend(); });
      l.append(c, document.createTextNode(label));
      return l;
    }));
  }
  function toggleLayers(force) {
    const pop = $("map-layers");
    const open = force !== undefined ? force : pop.hidden;
    pop.hidden = !open;
    $("map-layers-b").setAttribute("aria-expanded", String(open));
    if (open) { buildLayers(); const f = pop.querySelector("input"); if (f) f.focus(); }
  }
  $("map-layers-b").addEventListener("click", () => toggleLayers());
  document.addEventListener("pointerdown", (e) => { if (!$("map-layers").hidden && !e.target.closest(".pop-w")) toggleLayers(false); });
  function renderLegend() {
    const items = [["Captain", "var(--accent)"], ["Skarn", "var(--crit)"], ["FedSpace", "#6886bd"], ["Port", "#8d929b"]];
    $("map-legend").replaceChildren(...items.map(([l, c]) => { const s = el("span"); const i = el("i"); i.style.background = c; s.append(i, document.createTextNode(l)); return s; }));
  }

  // ======================================================================
  // Command palette
  // ======================================================================
  let palItems = [], palIdx = 0;
  const VIEW_CMDS = MODES.map((m, i) => ({ t: `Go to ${m[0].toUpperCase()}${m.slice(1) === "lerts" ? "ttention" : m.slice(1)}`, r: String(i + 1), run: () => setMode(m), g: "Workbench" }));
  VIEW_CMDS[1].t = "Go to Corporations";
  VIEW_CMDS[2].t = "Go to Attention";
  VIEW_CMDS[MODES.indexOf("feedback")].r = "0";
  VIEW_CMDS[MODES.indexOf("constitution")].r = "C";
  VIEW_CMDS[MODES.indexOf("history")].r = "H";
  VIEW_CMDS[MODES.indexOf("chronicle")].r = "W";
  VIEW_CMDS[MODES.indexOf("improve")].t = "Go to Improve board";
  VIEW_CMDS[MODES.indexOf("improve")].r = "I";
  function commands() {
    const held = S && S.paused;
    return [
      { t: held ? "Resume the clock" : "Hold the clock", run: toggleClock },
      { t: "Advance one tick", run: () => advance(1) },
      { t: "Advance ten ticks", run: () => advance(10) },
      { t: "Message every captain…", run: () => composeTo(null, "message") },
      { t: "Broadcast on public radio…", run: () => composeTo(null, "broadcast") },
      { t: "Spawn a Skarn raider (random sector)", run: () => act("spawn_skarn", {}, "Skarn") },
      { t: "Enlist a captain…", run: () => { setMode("universe"); $("en-name").focus(); } },
      { t: "Write a snapshot", run: () => act("snapshot", {}, "Snapshot") },
      { t: "Download a backup", run: () => { location.href = "/api/sysop/backup"; } },
      { t: "Review rule changes", run: () => setMode("rules") },
      { t: "Open feedback reports", run: () => { setMode("feedback"); setFbFilter("open"); } },
      { t: "Open constitution proposals", run: () => { setMode("constitution"); setCtFilter("open"); } },
      { t: "Open production pitches", run: () => { setMode("chronicle"); setChView("productions"); } },
      { t: "Open reported Chronicle pages", run: () => { setMode("chronicle"); setChView("pages"); setChFilter("reported"); } },
      { t: "Open improve patches in review", run: () => { setMode("improve"); setImFilter("review"); } },
      { t: "Open improve proposals", run: () => { setMode("improve"); setImFilter("proposed"); } },
      { t: "Post an improve item…", run: () => { setMode("improve"); if (imOp !== "post") setImOp("post"); } },
      { t: "Show undocumented captains", run: () => { setMode("captains"); const b = document.querySelector('#cap-scope button[data-v="undoc"]'); if (b) b.click(); } },
      { t: "Show captains with a rename pending", run: () => { setMode("captains"); const b = document.querySelector('#cap-scope button[data-v="rename"]'); if (b) b.click(); } },
      { t: "Require every captain to rename…", run: requireRename },
      { t: "Carry new names through identities…", run: fixOldNames },
      { t: "Compare models", run: () => { setMode("history"); hiSetBy("model"); hiSetView("compare"); } },
      { t: "Compare harnesses", run: () => { setMode("history"); hiSetBy("harness"); hiSetView("compare"); } },
      { t: "Fit the galaxy", r: "F", run: () => map.fit() },
      { t: map.is3D() ? "Flat map" : "3D view", r: "V", run: () => map.toggle3D() },
      ...LAYERS.map(([k, l]) => ({ t: `${map.layers[k] ? "Hide" : "Show"} ${l.toLowerCase()}`, run: () => { map.setLayer(k, !map.layers[k]); store.set("layers", map.layers); } })),
      { t: "End the season now…", run: endSeason },
      { t: "Big Bang: start a new season…", run: bigBang },
      { t: "Clear every human strategy…", run: clearStrategies },
      { t: "Open the story bible", run: () => { setMode("chronicle"); setChView("bible"); } },
      { t: "Keyboard shortcuts", r: "?", run: () => $("help").showModal() },
      { t: "Sign out", run: () => $("logout").click() },
    ].map((c) => ({ ...c, g: "Commands" }));
  }
  function palSearch(q) {
    q = q.trim().toLowerCase();
    const out = [];
    const score = (s) => { s = String(s || "").toLowerCase(); if (!q) return 1; if (s === q) return 4; if (s.startsWith(q)) return 3; if (s.split(/[\s_-]+/).some((w) => w.startsWith(q))) return 2; if (s.includes(q)) return 1; return 0; };
    if (!q) {
      for (const h of recentSel.slice(0, 5)) {
        const lab = h.kind === "captain" ? (player(h.id) || {}).name : h.kind === "hcap" ? navLabel({ sel: h }) : h.kind === "sector" ? `Sector ${h.id}` : h.kind === "planet" ? (S.planets.find((p) => p.id === h.id) || {}).name : h.kind === "corp" ? (S.corps.find((c) => c.id === h.id) || {}).name : `${h.kind} ${h.id}`;
        if (lab) out.push({ g: "Recent", t: lab, r: h.kind, run: () => select(h.kind, h.id, { pan: true }) });
      }
      return [...out, ...VIEW_CMDS, ...commands()];
    }
    const n = Number(q.replace(/^s(ector)?\s*/, ""));
    if (Number.isInteger(n) && n > 0 && map.sector(n)) out.push({ g: "Sectors", t: `Sector ${n}`, r: (() => { const s = map.sector(n); return [s.fed && "FedSpace", s.port].filter(Boolean).join(" · "); })(), run: () => select("sector", n, { pan: true }), s: 5 });
    if (S) {
      for (const p of S.players) {
        const sc = Math.max(score(p.name) * 2, score(p.model), score(p.corp), score(`#${p.id}`));
        if (sc) out.push({ g: p.kind === "npc" ? "NPCs" : "Captains", t: p.name, dot: p, r: `${p.kind === "npc" ? p.npc : p.model || ""} · s${p.sector}`, run: () => select("captain", p.id, { pan: true }), s: sc + (p.kind === "npc" ? 0 : 1) });
      }
      for (const pl of S.planets) { const sc = score(pl.name); if (sc) out.push({ g: "Planets", t: pl.name, r: `${pl.owner || "unclaimed"} · s${pl.sector}`, run: () => select("planet", pl.id, { pan: true }), s: sc }); }
      for (const c of S.corps) { const sc = score(c.name); if (sc) out.push({ g: "Corporations", t: c.name, r: plural(c.members.length, "member"), run: () => select("corp", c.id, { pan: true }), s: sc }); }
    }
    if (chData) {
      for (const pg of chData.pages) { const sc = Math.max(score(pg.title), score(`#${pg.id}`)); if (sc) out.push({ g: "Chronicle", t: pg.title, r: `${pg.kind} · #${pg.id}`, run: () => select("cpage", pg.id), s: sc }); }
      for (const x of chData.productions) { const sc = score(x.title); if (sc) out.push({ g: "Chronicle", t: x.title, r: `${x.status} production · #${x.id}`, run: () => select("cprod", x.id), s: sc }); }
    }
    for (const x of imItems()) { const sc = Math.max(score(x.title), score(`#${x.id}`)); if (sc) out.push({ g: "Improve", t: x.title, r: `${IM_STATUS[x.status] || x.status} · #${x.id}`, run: () => { setMode("improve"); select("improve", x.id); }, s: sc }); }
    for (const c of [...VIEW_CMDS, ...commands()]) { const sc = score(c.t); if (sc) out.push({ ...c, s: sc }); }
    const gOrder = ["Sectors", "Captains", "NPCs", "Planets", "Corporations", "Chronicle", "Improve", "Workbench", "Commands"];
    return out.sort((a, b) => gOrder.indexOf(a.g) - gOrder.indexOf(b.g) || (b.s || 0) - (a.s || 0)).slice(0, 40);
  }
  function renderPalette() {
    palItems = palSearch($("pal-q").value);
    palIdx = Math.min(palIdx, Math.max(0, palItems.length - 1));
    const ul = $("pal-list");
    const frag = document.createDocumentFragment();
    let g = null;
    palItems.forEach((it, i) => {
      if (it.g !== g) { g = it.g; frag.append(el("li", "gh", g)); }
      const li = el("li");
      li.setAttribute("role", "option");
      li.id = `pal-${i}`;
      li.setAttribute("aria-selected", String(i === palIdx));
      if (it.dot) li.append(dot(it.dot.name, it.dot.kind === "npc" && it.dot.npc));
      li.append(el("span", "", it.t));
      if (it.r) li.append(el("span", "r", it.r));
      li.addEventListener("click", () => runPal(i));
      li.addEventListener("pointermove", () => { if (palIdx !== i) { palIdx = i; markPal(); } });
      frag.append(li);
    });
    if (!palItems.length) frag.append(el("li", "none", "No matches."));
    ul.replaceChildren(frag);
    $("pal-q").setAttribute("aria-activedescendant", palItems.length ? `pal-${palIdx}` : "");
  }
  function markPal() {
    for (const li of $("pal-list").querySelectorAll("[role=option]")) li.setAttribute("aria-selected", String(li.id === `pal-${palIdx}`));
    const cur = $(`pal-${palIdx}`);
    if (cur) cur.scrollIntoView({ block: "nearest" });
    $("pal-q").setAttribute("aria-activedescendant", `pal-${palIdx}`);
  }
  function openPalette(prefill) {
    const d = $("palette");
    if (d.open) return;
    $("pal-q").value = prefill || "";
    palIdx = 0;
    renderPalette();
    d.showModal();
    $("pal-q").focus();
  }
  function runPal(i) {
    const it = palItems[i];
    $("palette").close();
    markJump();
    if (it) it.run();
  }
  $("open-palette").addEventListener("click", () => openPalette());
  $("pal-q").addEventListener("input", () => { palIdx = 0; renderPalette(); });
  $("pal-q").addEventListener("keydown", (e) => {
    if (e.key === "ArrowDown") { e.preventDefault(); palIdx = Math.min(palItems.length - 1, palIdx + 1); markPal(); }
    else if (e.key === "ArrowUp") { e.preventDefault(); palIdx = Math.max(0, palIdx - 1); markPal(); }
    else if (e.key === "Enter") { e.preventDefault(); runPal(palIdx); }
  });
  $("palette").addEventListener("click", (e) => { if (e.target === $("palette")) $("palette").close(); });
  $("help-close").addEventListener("click", () => $("help").close());

  // Lists re-render on every poll. A click whose press and release straddle a re-render
  // lands on a replaced row and is lost; replay it on the row that took its place.
  let downRow = null;
  const rowOf = (t) => (t && t.closest ? t.closest("[data-sel], [data-row]") : null);
  const rowKey = (r) => r.dataset.sel || r.dataset.row;
  document.addEventListener("pointerdown", (e) => { const r = rowOf(e.target); downRow = r ? { el: r, key: rowKey(r) } : null; }, true);
  document.addEventListener("pointerup", (e) => {
    const r = rowOf(e.target);
    if (downRow && r && r !== downRow.el && !downRow.el.isConnected && rowKey(r) === downRow.key && !e.target.closest("button, a, input, select, textarea")) setTimeout(() => r.click(), 0);
    downRow = null;
  }, true);

  // ======================================================================
  // Keyboard
  // ======================================================================
  function stepList(dir) {
    const scope = $(`m-${mode}`);
    if (!scope || $("app").classList.contains("no-work")) return;
    const rows = [...scope.querySelectorAll("[data-sel], [data-row]")].filter((r) => r.offsetParent !== null && !(r.closest(".pane-b") && r.closest(".split2")));
    if (!rows.length) return;
    let i = rows.findIndex((r) => r.getAttribute("aria-selected") === "true");
    if (mode === "comms" || mode === "activity") i = rows.findIndex((r) => r.dataset.row && r.getAttribute("aria-selected") === "true");
    i = i < 0 ? (dir > 0 ? 0 : rows.length - 1) : Math.max(0, Math.min(rows.length - 1, i + dir));
    rows[i].click();
    rows[i].scrollIntoView({ block: "nearest" });
  }
  addEventListener("keydown", (e) => {
    if ($("app").hidden) return;
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") { e.preventDefault(); openPalette(); return; }
    if (document.querySelector("dialog[open]")) return;
    if (typing(e.target)) {
      if (e.key === "Escape") e.target.blur();
      return;
    }
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    const k = e.key;
    if (k === "/") { e.preventDefault(); openPalette(); }
    else if (/^[0-9]$/.test(k)) { navStack.length = 0; setMode(k === "0" ? "feedback" : MODES[Number(k) - 1]); renderBack(); }
    else if (k === "c" || k === "C") { navStack.length = 0; setMode("constitution"); renderBack(); }
    else if (k === "h" || k === "H") { navStack.length = 0; setMode("history"); renderBack(); }
    else if (k === "w" || k === "W") { navStack.length = 0; setMode("chronicle"); renderBack(); }
    else if (k === "i" || k === "I") { navStack.length = 0; setMode("improve"); renderBack(); }
    else if (k === "Escape") {
      if (!$("map-layers").hidden) toggleLayers(false);
      else if (actOpen) { actOpen = null; buildInspector(); }
      else if ((mode === "comms" || mode === "activity") && $(mode === "comms" ? "cm-split" : "ac-split").dataset.view === "detail" && $(`m-${mode}`).getBoundingClientRect().width < 760) $(mode === "comms" ? "cm-split" : "ac-split").dataset.view = "list";
      else if (back()) { /* returned to where the operator came from */ }
      else if (sel) select(null);
    } else if (k === "f" || k === "F") { if (sel) syncFocus(true); else map.fit(); }
    else if (k === "+" || k === "=") map.zoomIn();
    else if (k === "-" || k === "_") map.zoomOut();
    else if (k === "l" || k === "L") toggleLayers();
    else if (k === "v" || k === "V") map.toggle3D();
    else if (k === "g" || k === "G") { e.preventDefault(); $("map-goto-n").focus(); }
    else if (k === "j" || k === "J") stepList(1);
    else if (k === "k" || k === "K") stepList(-1);
    else if ((k === "m" || k === "M") && sel && sel.kind === "captain") { const p = player(sel.id); if (p && p.kind !== "npc") { actOpen = "message"; buildInspector(); } }
    else if (k === "?") $("help").showModal();
    else if (k === "e" || k === "E") toggleExpand();
  });

  // ======================================================================
  // Resizable panes
  // ======================================================================
  function splitter(id, onMove, onEnd) {
    const s = $(id);
    s.addEventListener("pointerdown", (e) => {
      e.preventDefault();
      s.setPointerCapture(e.pointerId);
      s.classList.add("drag");
      const move = (ev) => onMove(ev);
      const up = () => { s.classList.remove("drag"); s.removeEventListener("pointermove", move); s.removeEventListener("pointerup", up); onEnd(); };
      s.addEventListener("pointermove", move);
      s.addEventListener("pointerup", up);
    });
  }
  const root = document.documentElement.style;
  splitter("split-work", (e) => {
    const w = Math.round(Math.max(300, Math.min(innerWidth * 0.55, e.clientX - 64)));
    widths[mode] = w;
    root.setProperty("--work-w", `${w}px`);
  }, () => store.set("widths", widths));
  let inspW = store.get("inspW", null);
  if (inspW) root.setProperty("--insp-w", `${inspW}px`);
  splitter("split-insp", (e) => {
    inspW = Math.round(Math.max(300, Math.min(620, innerWidth - e.clientX)));
    root.setProperty("--insp-w", `${inspW}px`);
  }, () => store.set("inspW", inspW));
  let dockH = store.get("dockH", null);
  if (dockH) root.setProperty("--dock-h", `${dockH}px`);
  splitter("split-dock", (e) => {
    const r = $("center").getBoundingClientRect();
    dockH = Math.round(Math.max(34, Math.min(r.height * 0.7, r.bottom - e.clientY)));
    root.setProperty("--dock-h", `${dockH}px`);
  }, () => store.set("dockH", dockH));

  // ======================================================================
  // State fan-out and boot
  // ======================================================================
  function onState() {
    if (mode === "comms") loadRadio();
    fbOnState();
    ctOnState();
    chOnState();
    imOnState();
    map.setState(S);
    renderClock();
    renderStatus();
    renderAlerts();
    renderMode();
    markRepliesSeen();
    buildInspector();
    if (sel) syncFocus(false);
  }
  function restoreFromHash() {
    const [m, kind, id] = (location.hash || "").slice(1).split("/");
    setMode(MODES.includes(m) ? m : store.get("mode", "captains"));
    if (kind && id !== undefined) select(kind, kind === "anon" || kind === "hcap" ? decodeURIComponent(id) : Number(id), { pan: true });
  }
  async function boot() {
    const r = await api(`${API}/me`);
    if (r.status === 403) return showGate(errText(r.data, 403), true);
    if (!r.ok) return showGate(r.status === 401 ? "" : errText(r.data, r.status));
    me = r.data;
    lost = false;
    $("gate").hidden = true;
    $("app").hidden = false;
    if (started) return;
    started = true;
    buildCapHead();
    buildChips();
    renderLegend();
    setCmKind("message");
    await loadMap();
    await loadState();
    restoreFromHash();
    startFeed();
    loadRulesVersion();
    loadActAlerts();
    loadAcks();
    setInterval(loadAcks, 30000);
    let polls = 0;
    pollTimer = setInterval(() => { if (!document.hidden || ++polls % 4 === 0) loadState(); }, 2500);
    setInterval(loadActAlerts, 90000);
    // The live window and the current season refresh; a past season is a closed record.
    setInterval(() => { const se = acSeason(); if (!document.hidden && mode === "activity" && (!se || se.current)) loadActivity(); }, 30000);
    // Page edits and talk do not change the state's Chronicle signal: refresh while it is open.
    setInterval(() => { if (!document.hidden && mode === "chronicle") loadChronicle(); }, 30000);
  }
  boot();
})();
