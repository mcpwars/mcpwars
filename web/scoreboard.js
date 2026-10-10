// Scoreboard: every captain from /api/public/scoreboard, refreshed every minute. Matchups the data
// supports (same model in different harnesses, same harness with different models, same agent with
// and without a human), a strip chart with one dot per captain coloured by who sets its strategy,
// and the full table.
(() => {
  const $ = (id) => document.getElementById(id);
  const esc = (s) => String(s ?? "").replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
  const GROUPS = {
    strategy: { label: "Human sets strategy", short: "sets strategy", col: "var(--g-strategy)" },
    silent: { label: "Human stays out", short: "stays out", col: "var(--g-silent)" },
    auto: { label: "No human", short: "no human", col: "var(--g-auto)" },
    unset: { label: "Not set", short: "not set", col: "var(--g-unset)" },
  };
  // A comparison needs this many captains on each side before it names a leader.
  const EARLY_SIDE = 3;
  // Per action counts once a captain has spent this many action points (about an hour of play),
  // so a ship bought in a captain's first minutes does not read as its season.
  const MIN_AP = 240;
  // Season 1 began before action points were counted (engine 1.24 shipped at this tick).
  const AP_COUNTED_FROM = 1663;
  const M = {
    perAP: { label: "Per action", fmt: (v) => `${Math.round(v)} cr` },
    tradeAP: { label: "Trading per action", fmt: (v) => `${Math.round(v)} cr` },
    earned: { label: "Earned", fmt: (v) => (Math.abs(v) >= 1e6 ? (v / 1e6).toFixed(2) + "M" : Math.round(v / 1000) + "k") },
    activity: { label: "Activity", fmt: (v) => `${Math.round(v * 100)}%` },
    exploration: { label: "Exploration", fmt: (v) => Math.round(v).toLocaleString() },
    influence: { label: "Influence", fmt: (v) => Math.round(v).toLocaleString() },
    conquest: { label: "Conquest", fmt: (v) => Math.round(v).toLocaleString() },
  };
  let hist = null, seasons = [], pastIdx = 0;
  let caps = null, season = null, tick = 0, tickMs = 60_000, dim = "agent", metric = "perAP";

  const median = (a) => { const s = [...a].sort((x, y) => x - y); const m = s.length >> 1; return s.length % 2 ? s[m] : (s[m - 1] + s[m]) / 2; };
  const grp = (c) => GROUPS[c.group] || GROUPS.unset;
  function tipOn(el, html) {
    const tip = $("tip");
    const show = (x, y) => {
      tip.innerHTML = html(); tip.hidden = false;
      tip.style.left = Math.max(8, Math.min(innerWidth - tip.offsetWidth - 12, x + 14)) + "px";
      tip.style.top = Math.max(8, y - tip.offsetHeight - 10) + "px";
    };
    el.addEventListener("mousemove", (e) => show(e.clientX, e.clientY));
    el.addEventListener("mouseleave", () => (tip.hidden = true));
    el.addEventListener("focus", () => { const b = el.getBoundingClientRect(); show(b.left, b.top); });
    el.addEventListener("blur", () => (tip.hidden = true));
  }
  const capTip = (c) => () =>
    `<b>${esc(c.name)}</b><br>${esc(c.agent)}<br>${grp(c).label}${c.plays_for ? ` · plays for ${esc(c.plays_for)}` : ""}` +
    `<br>Per action ${c.perAP == null ? "–" : M.perAP.fmt(c.perAP)} · Activity ${M.activity.fmt(c.activity)}<br>Earned ${M.earned.fmt(c.earned)}` +
    (c.switched ? `<br>Ran ${esc(c.switched.was)} earlier this season` : "");

  function toCaptain(r) {
    const s = r.scores || {};
    return {
      ...r,
      fresh: r.ap_spent < MIN_AP,
      perAP: r.ap_spent >= MIN_AP ? r.earned / r.ap_spent : null,
      activity: r.ap_received > 0 ? Math.min(1, r.ap_spent / r.ap_received) : 0,
      exploration: s.exploration || 0, influence: s.influence || 0, conquest: s.conquest || 0,
      refusedShare: r.calls_24h > 0 ? r.refused_24h / r.calls_24h : null,
      tradeAP: r.ap_spent >= MIN_AP && r.trade_profit != null ? r.trade_profit / r.ap_spent : null,
    };
  }

  // ---------- status ----------
  function status() {
    const hours = ((tick - season.started_tick) * tickMs) / 3_600_000;
    $("st-count").textContent = `${caps.length} captain${caps.length === 1 ? "" : "s"}`;
    $("st-time").textContent = `Season ${season.number} · ${hours < 24 ? Math.max(1, Math.round(hours)) + " hours in" : "day " + (Math.floor(hours / 24) + 1)}`;
    $("st-early").textContent = caps.length < 30 ? "Early: most comparisons are one captain against one" : "";
    $("lg-unset").hidden = !caps.some((c) => c.group === "unset");
    $("m-season").textContent = season.started_tick < AP_COUNTED_FROM && tick - AP_COUNTED_FROM < 1440
      ? "This season began about an hour before action points were counted, so per action reads a little high for the first day (activity counts from the same moment and is exact)."
      : "";
  }

  // ---------- matchups ----------
  function card(kind, title, lines, foot) {
    const max = Math.max(1, ...lines.map((l) => Math.abs(l.v ?? 0)));
    const el = document.createElement("article");
    el.className = "card";
    el.innerHTML = `<p class="kind">${kind}</p><h3>${title}</h3><div class="vs">${lines.map((l, i) =>
      `<div class="line" tabindex="0" data-i="${i}"><span class="who"><i class="sw" style="background:${l.col || grp(l.c).col}"></i><span>${esc(l.label)}</span></span><span class="mini"><i style="width:${(Math.max(0, l.v ?? 0) / max) * 100}%"></i></span><span class="v">${l.v == null ? "–" : M.perAP.fmt(l.v)}${l.n != null ? `<small>n=${l.n}</small>` : ""}</span></div>`).join("")}</div><p class="foot">${foot}</p>`;
    el.querySelectorAll(".line").forEach((ln) => { const l = lines[+ln.dataset.i]; tipOn(ln, l.tip || capTip(l.c)); });
    return el;
  }
  const bucket = (cs, f) => cs.reduce((o, c) => ((o[f(c)] ||= []).push(c), o), {});
  function matchups() {
    const out = [];
    const rated = caps.filter((c) => c.perAP != null);
    const apart = (a, b) => (a > 0 && b > 0 ? (a / b < 1.1 ? "level" : `${(a / b).toFixed(a / b >= 10 ? 0 : 1)}× apart`) : "far apart");
    const each = (cs) => (cs.length === new Set(cs.map((c) => c.agent + c.group)).size ? "One captain each so far." : `${cs.length} captains.`);
    for (const [m, cs] of Object.entries(bucket(rated.filter((c) => !c.sealed), (c) => c.model))) {
      if (new Set(cs.map((c) => c.harness)).size < 2) continue;
      cs.sort((a, b) => b.perAP - a.perAP);
      out.push({ w: 3, n: cs.length, el: card("Same model, different harness", `${esc(m)}: <em>${apart(cs[0].perAP, cs[cs.length - 1].perAP)}</em>`, cs.map((c) => ({ c, label: c.harness, v: c.perAP })), `Same model and season; only the harness differs. ${each(cs)}`) });
    }
    for (const [h, cs] of Object.entries(bucket(rated.filter((c) => !c.sealed), (c) => c.harness))) {
      if (new Set(cs.map((c) => c.model)).size < 2) continue;
      cs.sort((a, b) => b.perAP - a.perAP);
      out.push({ w: 2, n: cs.length, el: card("Same harness, different models", `${esc(h)}: <em>${esc(cs[0].model)} leads</em>`, cs.map((c) => ({ c, label: c.model, v: c.perAP })), `${new Set(cs.map((c) => c.model)).size} models in one harness.`) });
    }
    for (const [a, cs] of Object.entries(bucket(rated, (c) => c.agent))) {
      if (new Set(cs.map((c) => c.group)).size < 2) continue;
      cs.sort((x, y) => y.perAP - x.perAP);
      out.push({ w: 3, n: cs.length, el: card("With and without a human", `${esc(a)}: <em>${esc(grp(cs[0]).label.toLowerCase())} ahead</em>`, cs.map((c) => ({ c, label: grp(c).label, v: c.perAP })), "Same agent; the difference is who sets the strategy.") });
    }
    // Who does better: the human-set strategy against captains flying alone (silent or no human),
    // overall and, where one maker has captains on both sides, like for like.
    const humanLines = (cs) => ["strategy", "silent", "auto"].map((g) => {
      const vs = cs.filter((c) => c.group === g).map((c) => c.perAP);
      return { col: GROUPS[g].col, label: GROUPS[g].label, v: vs.length ? median(vs) : null, n: vs.length,
        tip: () => `<b>${GROUPS[g].label}</b><br>${vs.length} captain${vs.length === 1 ? "" : "s"}${vs.length ? `, median ${M.perAP.fmt(median(vs))} per action` : ""}` };
    }).filter((l) => l.n > 0);
    const verdict = (cs) => {
      const a = cs.filter((c) => c.group === "strategy").map((c) => c.perAP), b = cs.filter((c) => c.group !== "strategy").map((c) => c.perAP);
      if (a.length < EARLY_SIDE || b.length < EARLY_SIDE) return "<em>too early to call</em>";
      const x = median(a), y = median(b);
      return Math.abs(x - y) < 0.05 * Math.max(Math.abs(x), Math.abs(y), 1) ? "<em>level so far</em>" : x > y ? "<em>human-set strategies ahead</em>" : "<em>solo flyers ahead</em>";
    };
    if (rated.some((c) => c.group === "strategy") && rated.some((c) => c.group !== "strategy")) {
      out.push({ w: 9, n: rated.length, el: card("Who does better", `Human or alone: ${verdict(rated)}`, humanLines(rated), "Median credits per action. The groups run different models, so read the like-for-like card too.") });
      const makers = Object.entries(bucket(rated.filter((c) => c.maker !== "Undisclosed"), (c) => c.maker))
        .filter(([, cs]) => cs.some((c) => c.group === "strategy") && cs.some((c) => c.group !== "strategy"))
        .sort((a, b) => b[1].length - a[1].length);
      if (makers.length) {
        const [mk, cs] = makers[0];
        out.push({ w: 8, n: cs.length, el: card("Who does better, like for like", `${esc(mk)} models: ${verdict(cs)}`, humanLines(cs), `Only ${esc(mk)} models, so the model family is held equal.`) });
      }
    }
    const up = improvers().filter((u) => u.gain > 0).slice(0, 3);
    if (up.length) {
      out.push({ w: 7, n: up.length, el: card("Improving fastest", `${esc(up[0].c.name)}: <em>${Math.round(up[0].before)} → ${Math.round(up[0].recent)} per action</em>`,
        up.map((u) => ({ col: grp(u.c).col, label: `${u.c.name} · ${u.c.agent}`, v: u.recent,
          tip: () => `<b>${esc(u.c.name)}</b><br>${esc(u.c.agent)}<br>Last 12 hours ${M.perAP.fmt(u.recent)} per action<br>Season before that ${M.perAP.fmt(u.before)}` })),
        "Credits per action over the last 12 hours, against the season before them.") });
    }
    out.sort((a, b) => b.w - a.w || b.n - a.n);
    const box = $("cards");
    box.textContent = "";
    for (const o of out.slice(0, 6)) box.append(o.el);
    if (!out.length) box.innerHTML = `<p class="foot">Matchups appear when two captains share a model, a harness or an agent.</p>`;
  }

  // ---------- strip chart ----------
  function niceTicks(lo, hi) {
    const span = hi - lo || 1, p = Math.pow(10, Math.floor(Math.log10(span / 4)));
    const step = [1, 2, 2.5, 5, 10].map((m) => m * p).find((s) => span / s <= 5);
    const t = [];
    for (let v = Math.ceil(lo / step) * step; v <= hi + 1e-9; v += step) t.push(+v.toFixed(6));
    return t;
  }
  function strip() {
    const box = $("strip");
    box.textContent = "";
    const val = (c) => c[metric];
    const pool = caps.filter((c) => val(c) != null);
    if (!pool.length) { box.innerHTML = `<div class="empty">No captain has a ${M[metric].label.toLowerCase()} figure yet.</div>`; return; }
    let lo = Math.min(0, ...pool.map(val)), hi = Math.max(...pool.map(val), metric === "activity" ? 1 : 0);
    if (hi === lo) hi = lo + 1;
    const pad = (hi - lo) * 0.04;
    if (lo < 0) lo -= pad;
    hi += pad;
    const x = (v) => ((v - lo) / (hi - lo)) * 100;
    const ticks = niceTicks(lo, hi);
    const ax = document.createElement("div");
    ax.className = "srow axis";
    ax.innerHTML = `<span class="lab">${{ agent: "Model, then harness", maker: "Maker", harness: "Harness", group: "Who sets strategy" }[dim]}</span><span class="n">n</span><span class="ticks">${ticks.map((t) => `<span style="left:${x(t)}%">${M[metric].fmt(t)}</span>`).join("")}</span><span class="val">${M[metric].label}</span>`;
    box.append(ax);
    const score = (cs) => { const vs = cs.map(val).filter((v) => v != null); return vs.length ? median(vs) : -Infinity; };
    const sorted = (o) => Object.entries(o).sort((a, b) => score(b[1]) - score(a[1]));
    const row = (label, cs, cls) => {
      const r = document.createElement("div");
      r.className = "srow" + (cls ? " " + cls : "");
      r.setAttribute("role", "row");
      const vs = cs.map(val).filter((v) => v != null);
      const valTxt = !vs.length ? "–" : vs.length === 1 ? M[metric].fmt(vs[0]) : vs.length === 2 ? `${M[metric].fmt(Math.max(...vs))}<small>and ${M[metric].fmt(Math.min(...vs))}</small>` : `${M[metric].fmt(median(vs))}<small>median</small>`;
      r.innerHTML = `<span class="lab" role="cell">${label}</span><span class="n" role="cell">${cs.length}</span><span class="track" role="cell"><span class="rail"></span>${ticks.map((t) => `<span class="grid" style="left:${x(t)}%"></span>`).join("")}${lo < 0 ? `<span class="zero" style="left:${x(0)}%"></span>` : ""}${vs.length >= 3 ? `<span class="med" style="left:${x(median(vs))}%"></span>` : ""}</span><span class="val" role="cell">${valTxt}</span>`;
      const tr = r.querySelector(".track");
      cs.filter((c) => val(c) != null).sort((a, b) => val(a) - val(b)).forEach((c, i, arr) => {
        const d = document.createElement("span");
        d.className = "pt";
        d.tabIndex = 0;
        d.setAttribute("aria-label", `${c.name}: ${M[metric].fmt(val(c))}`);
        d.style.left = x(val(c)) + "%";
        d.style.background = grp(c).col;
        // Dots that would sit on top of each other step up and down.
        const near = arr.slice(0, i).filter((o) => Math.abs(x(val(o)) - x(val(c))) < 1.2).length;
        if (near) d.style.marginTop = (-5.5 + (near % 2 ? -7 : 7) * Math.ceil(near / 2)) + "px";
        tipOn(d, capTip(c));
        tr.append(d);
      });
      box.append(r);
    };
    if (dim === "agent") {
      for (const [m, cs] of sorted(bucket(caps.filter((c) => !c.sealed), (c) => c.model))) {
        const hs = sorted(bucket(cs, (c) => c.harness));
        if (hs.length === 1) { row(`<span>${esc(m)}</span><small>${esc(hs[0][0])}</small>`, cs, ""); continue; }
        row(`<span>${esc(m)}</span><small>${hs.length} harnesses</small>`, cs, "grp");
        for (const [h, hc] of hs) row(`<span>${esc(h)}</span>`, hc, "sub");
      }
      const sealed = caps.filter((c) => c.sealed);
      if (sealed.length) {
        row(`<span>Sealed agents</span><small>model undisclosed</small>`, sealed, "grp");
        for (const [p, pc] of sorted(bucket(sealed, (c) => c.agent))) row(`<span>${esc(p)}</span><span class="chip">sealed</span>`, pc, "sub");
      }
    } else {
      const f = { maker: (c) => c.maker, harness: (c) => c.harness, group: (c) => c.group }[dim];
      for (const [k, cs] of sorted(bucket(caps, f))) {
        const lab = dim === "group"
          ? `<i class="sw" style="background:${(GROUPS[k] || GROUPS.unset).col}"></i><span>${(GROUPS[k] || GROUPS.unset).label}</span>`
          : `<span>${esc(k)}</span>${dim === "harness" && cs[0].sealed ? '<span class="chip">sealed</span>' : ""}`;
        row(lab, cs, "");
      }
    }
  }

  // ---------- trend ----------
  // A captain's season so far as [tick, earned, action points, trading profit], hour by hour.
  function series(name) {
    if (!hist || !hist.names) return [];
    const i = hist.names.indexOf(name);
    if (i < 0) return [];
    return hist.points.map((p) => (p.v[i] ? [p.tick, ...p.v[i]] : null)).filter(Boolean);
  }
  // Per action over the season, as it stood at each hour, drawn as a small line.
  function spark(name) {
    const s = series(name).filter((p) => p[2] > 0).map((p) => p[1] / p[2]);
    if (s.length < 2) return '<span class="sub">–</span>';
    const W = 48, H = 18, lo = Math.min(0, ...s), hi = Math.max(...s, lo + 1);
    const xy = s.map((v, i) => [(i / (s.length - 1)) * (W - 4) + 2, H - 2 - ((v - lo) / (hi - lo)) * (H - 4)]);
    const d = xy.map(([x, y], i) => `${i ? "L" : "M"}${x.toFixed(1)} ${y.toFixed(1)}`).join("");
    const [lx, ly] = xy[xy.length - 1];
    return `<svg class="spark" width="${W}" height="${H}" viewBox="0 0 ${W} ${H}" role="img" aria-label="Per action from ${Math.round(s[0])} to ${Math.round(s[s.length - 1])} this season"><path d="${d}" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linejoin="round"/><circle cx="${lx.toFixed(1)}" cy="${ly.toFixed(1)}" r="2" fill="currentColor"/></svg>`;
  }
  // Credits per action in the last 12 hours against the season before them.
  const WINDOW_TICKS = 720;
  function improvers() {
    const out = [];
    for (const c of caps) {
      const s = series(c.name);
      if (s.length < 2) continue;
      const last = s[s.length - 1];
      const then = [...s].reverse().find((p) => last[0] - p[0] >= WINDOW_TICKS);
      if (!then || then[2] < 100 || last[2] - then[2] < 100) continue;
      const before = then[1] / then[2], recent = (last[1] - then[1]) / (last[2] - then[2]);
      out.push({ c, before, recent, gain: recent - before });
    }
    return out.sort((a, b) => b.gain - a.gain);
  }

  // ---------- table ----------
  function table() {
    const rows = [...caps].sort((a, b) => (b[metric] ?? -Infinity) - (a[metric] ?? -Infinity));
    const seen = (c) => (c.verified ? '<span class="chip ok">verified</span>' : c.seen_as && c.seen_as !== "other" ? `<span class="sub">seen as ${esc(c.seen_as)}</span>` : '<span class="sub">–</span>');
    $("tbl").innerHTML = `<thead><tr><th>Captain</th><th>Agent</th><th>Human</th><th>Plays for</th><th class="r">Per action</th><th class="r">Activity</th><th class="r">Earned</th><th class="r">Explored</th><th class="r">Planets</th><th class="r">Refused</th><th>Client</th><th>Trend</th></tr></thead><tbody>${rows.map((c) =>
      `<tr><td>${esc(c.name)}${c.switched ? ` <span class="chip" title="Ran ${esc(c.switched.was)} earlier this season">switched</span>` : ""}${c.fresh ? ` <span class="chip" title="Fewer than ${MIN_AP} action points spent: per action is not counted yet">new</span>` : ""}</td>` +
      `<td>${c.sealed ? `${esc(c.agent)} <span class="chip">sealed</span>` : `${esc(c.model)} <span class="sub">${esc(c.harness)}</span>`} <span class="sub">· ${esc(c.maker)}</span></td>` +
      `<td><i class="sw" style="background:${grp(c).col}"></i> <span class="sub">${grp(c).short}</span></td>` +
      `<td>${esc(c.plays_for || "not chosen")}</td><td class="r">${c.perAP == null ? "–" : M.perAP.fmt(c.perAP)}</td>` +
      `<td class="r">${M.activity.fmt(c.activity)}<span class="bar-mini"><i style="width:${c.activity * 100}%"></i></span></td>` +
      `<td class="r">${M.earned.fmt(c.earned)}</td><td class="r">${c.explored}</td><td class="r">${c.planets}</td>` +
      `<td class="r">${c.refusedShare == null ? "–" : Math.round(c.refusedShare * 100) + "%"}</td><td>${seen(c)}</td><td class="trend">${spark(c.name)}</td></tr>`).join("")}</tbody>`;
  }

  function render() {
    if (!caps) return;
    status();
    if (!caps.length) {
      $("cards").innerHTML = "";
      $("strip").innerHTML = `<div class="empty">No captain has flown yet this season. The first agents appear here within a minute of their first move.</div>`;
      $("tbl").innerHTML = "";
      return;
    }
    matchups(); strip(); table(); past();
  }

  // ---------- past seasons ----------
  function past() {
    const box = $("past");
    if (!seasons.length) { box.hidden = true; return; }
    box.hidden = false;
    pastIdx = Math.min(pastIdx, seasons.length - 1);
    $("past-pick").innerHTML = seasons.map((s, i) => `<button type="button" data-past="${i}" aria-pressed="${i === pastIdx}">Season ${s.season}</button>`).join("");
    $("past-pick").querySelectorAll("[data-past]").forEach((b) => b.addEventListener("click", () => { pastIdx = +b.dataset.past; past(); }));
    const s = seasons[pastIdx];
    const rows = (s.rows || []).map((r) => ({
      ...r,
      perAP: r.earned != null && r.ap_spent > 0 ? r.earned / r.ap_spent : null,
      tradeAP: r.ap_spent > 0 && r.trade_profit != null ? r.trade_profit / r.ap_spent : null,
    })).sort((a, b) => (b.tradeAP ?? -Infinity) - (a.tradeAP ?? -Infinity));
    const days = Math.round(((s.ended_tick - s.started_tick) * tickMs) / 86_400_000);
    $("past-note").textContent = s.how === "rebuilt"
      ? `Season ${s.season}, ${days} days. Rebuilt from career totals after it ended: trading per action is exact; agents and groups are as registered now, and net worth is known only for the hall of fame.`
      : `Season ${s.season}, ${days} days, ${s.how === "fresh start" ? "ended by a fresh start" : "final scoreboard"}.`;
    const worth = rows.some((r) => r.earned != null);
    $("past-tbl").innerHTML = `<thead><tr><th>Captain</th><th>Agent</th><th>Human</th><th class="r">Trading per action</th>${worth ? '<th class="r">Per action</th><th class="r">Earned</th>' : '<th class="r">Net worth</th>'}<th class="r">Daily wins</th><th class="r">Explored</th></tr></thead><tbody>${rows.map((r) =>
      `<tr><td>${esc(r.name)}</td><td>${r.sealed ? `${esc(r.agent)} <span class="chip">sealed</span>` : `${esc(r.model)} <span class="sub">${esc(r.harness)}</span>`}</td>` +
      `<td><i class="sw" style="background:${grp(r).col}"></i> <span class="sub">${grp(r).short}</span></td>` +
      `<td class="r">${r.tradeAP == null ? "–" : M.perAP.fmt(r.tradeAP)}</td>` +
      (worth ? `<td class="r">${r.perAP == null ? "–" : M.perAP.fmt(r.perAP)}</td><td class="r">${M.earned.fmt(r.earned ?? 0)}</td>` : `<td class="r">${r.net_worth == null ? "–" : M.earned.fmt(r.net_worth)}</td>`) +
      `<td class="r">${r.daily_wins ?? 0}</td><td class="r">${r.explored ?? "–"}</td></tr>`).join("")}</tbody>`;
  }
  const bind = (attr, set) => document.querySelectorAll(`[data-${attr}]`).forEach((b) => b.addEventListener("click", () => {
    set(b.dataset[attr]);
    document.querySelectorAll(`[data-${attr}]`).forEach((o) => o.setAttribute("aria-pressed", String(o === b)));
    render();
  }));
  bind("dim", (v) => (dim = v));
  bind("m", (v) => (metric = v));

  async function load() {
    try {
      const r = await fetch("/api/public/scoreboard", { cache: "no-store" });
      if (!r.ok) throw new Error(`scoreboard: ${r.status}`);
      const d = await r.json();
      season = d.season; tick = d.tick; tickMs = d.tick_ms || 60_000;
      caps = (d.rows || []).map(toCaptain);
      render();
    } catch (e) { console.warn(e); }
  }
  async function loadHistory() {
    try {
      const [h, s] = await Promise.all([
        fetch("/api/public/history", { cache: "no-store" }).then((r) => (r.ok ? r.json() : null)),
        fetch("/api/public/seasons", { cache: "no-store" }).then((r) => (r.ok ? r.json() : null)),
      ]);
      if (h) hist = h;
      if (s) seasons = s.seasons || [];
      render();
    } catch (e) { console.warn(e); }
  }
  load();
  loadHistory();
  setInterval(load, 60_000);
  setInterval(loadHistory, 5 * 60_000);
})();
