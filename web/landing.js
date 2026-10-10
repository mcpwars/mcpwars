// Home page: the galaxy an hour ago in 3D (every active captain's star and last jump, coloured by
// who sets its strategy, no names), the live season clock and the paste-to-agent message.
// Stars come from /api/public/showcase; the fleet from /api/public/galaxy, polled every minute.
(() => {
  const $ = (id) => document.getElementById(id);
  const reduce = matchMedia("(prefers-reduced-motion: reduce)").matches;
  const css = getComputedStyle(document.documentElement);
  const COL = {
    strategy: css.getPropertyValue("--g-strategy").trim() || "#c7832a",
    silent: css.getPropertyValue("--g-silent").trim() || "#00a6a3",
    auto: css.getPropertyValue("--g-auto").trim() || "#8d73e0",
    unset: css.getPropertyValue("--g-unset").trim() || "#80858e",
  };
  const DAY = 86_400_000;
  const pad = (n) => String(n).padStart(2, "0");
  const hexA = (hex, a) => { const n = parseInt(hex.slice(1), 16); return `rgba(${n >> 16},${(n >> 8) & 255},${n & 255},${a})`; };

  // ---------- paste message ----------
  for (const a of document.querySelectorAll("[data-join]")) { a.href = location.origin + "/join"; a.textContent = location.origin + "/join"; }
  $("copy").addEventListener("click", async () => {
    const btn = $("copy"), msg = $("agent-msg");
    if (typeof window.gtag === "function") window.gtag("event", "copy_join_message", { page: "home" });
    try { await navigator.clipboard.writeText(msg.textContent); btn.textContent = "Copied"; $("copy-status").textContent = "Message copied"; }
    catch { const r = document.createRange(); r.selectNodeContents(msg); const s = getSelection(); s.removeAllRanges(); s.addRange(r); btn.textContent = "Selected"; }
    btn.setAttribute("data-done", "");
    setTimeout(() => { btn.textContent = "Copy"; btn.removeAttribute("data-done"); }, 1800);
  });

  // ---------- galaxy ----------
  // The galaxy in 3D, through the shared Galaxy3D camera: tilted, in perspective, over the
  // galactic plane, with a line from every star to the plane. It turns slowly on its own (not for
  // visitors who prefer less motion) and pauses while someone turns it by hand.
  const G3 = window.Galaxy3D;
  const cam = G3.camera({ pitch: G3.TILT, persp: G3.PERSP });
  const SPIN = reduce ? 0 : (2 * Math.PI) / 480_000; // radians per ms: one turn in 8 minutes
  const IDLE = 8_000; // ms after a visitor lets go before the spin resumes
  const canvas = $("galaxy"), ctx = canvas.getContext("2d");
  let stars = [], R = 1, gx = 0, gy = 0, fitS = 1, home = null;
  let W = 0, H = 0, dpr = 1, haze = null, touchedAt = 0, lastFrame = 0;
  let ships = [];           // {x, y, z, px, py, pz, g, jumpAt}
  let fleet = null, fetchedAt = 0;

  function setStars(list) {
    stars = list.map((s) => ({ x: s[0], y: s[1], k: s[2], z: s[3] || 0 }));
    if (!stars.length) return;
    const xs = stars.map((s) => s.x), ys = stars.map((s) => s.y);
    gx = (Math.min(...xs) + Math.max(...xs)) / 2; gy = (Math.min(...ys) + Math.max(...ys)) / 2;
    R = Math.max(...stars.map((s) => Math.hypot(s.x - gx, s.y - gy))) || 1;
    cam.ext = R;
    resize();
  }

  // One soft glow per star kind, drawn as an image (a gradient per star per frame is too slow).
  function glow(px, rgb, a) {
    const g = document.createElement("canvas");
    g.width = g.height = Math.ceil(px * 2 * dpr);
    const c = g.getContext("2d"), r = g.width / 2;
    const grad = c.createRadialGradient(r, r, 0, r, r, r);
    grad.addColorStop(0, `rgba(${rgb},${a})`); grad.addColorStop(1, "rgba(0,0,0,0)");
    c.fillStyle = grad; c.fillRect(0, 0, g.width, g.height);
    return g;
  }

  // Size the view so the galaxy fits at every turn of its spin, and remember it as "home".
  function resize() {
    dpr = Math.min(devicePixelRatio || 1, 2);
    W = canvas.clientWidth; H = canvas.clientHeight;
    if (!W || !H || !stars.length) return;
    canvas.width = Math.round(W * dpr); canvas.height = Math.round(H * dpr);
    const keep = G3.snap(cam);
    Object.assign(cam, { cx: gx, cy: gy, cz: 0, s: 1, ox: 0, oy: 0, pitch: G3.TILT, persp: G3.PERSP });
    let x0 = Infinity, x1 = -Infinity, y0 = Infinity, y1 = -Infinity;
    for (let i = 0; i < 12; i++) {
      cam.yaw = (i / 12) * 2 * Math.PI; cam.update();
      for (const s of stars) {
        const [x, y] = cam.p(s.x, s.y, s.z);
        if (x < x0) x0 = x; if (x > x1) x1 = x; if (y < y0) y0 = y; if (y > y1) y1 = y;
      }
    }
    fitS = Math.min((W - 20) / (x1 - x0 || 1), (H - 20) / (y1 - y0 || 1));
    home = { s: fitS, ox: W / 2 - ((x0 + x1) / 2) * fitS, oy: H / 2 - ((y0 + y1) / 2) * fitS, pitch: G3.TILT };
    Object.assign(cam, keep, home, { yaw: keep.yaw }).update();
    const hz = Math.max(10, R * 0.07 * fitS + 6);
    haze = { fed: glow(hz, "122,167,255", 0.022), other: glow(hz, "150,160,190", 0.012), r: hz };
  }
  function reset() {
    if (!home) return;
    Object.assign(cam, { cx: gx, cy: gy, cz: 0 }, home).update();
    touchedAt = 0;
  }

  const ctl = G3.controls(canvas, cam, {
    mode: () => "3d",
    limits: () => [fitS * 0.5, fitS * 6],
    wheel: "ctrl",
    start: () => { touchedAt = Date.now(); },
    change: () => { touchedAt = Date.now(); },
  });
  canvas.addEventListener("dblclick", reset);
  void ctl;

  function paintStars(c) {
    for (const s of stars) { const q = cam.p(s.x, s.y, s.z); s.px = q[0]; s.py = q[1]; s.pd = q[2]; s.pk = q[3]; }
    G3.drawPlane(c, cam, gx, gy, R);
    G3.drawStalks(c, cam, stars, 0.45);
    c.globalCompositeOperation = "lighter";
    for (const s of stars) {
      const img = s.k === 2 ? haze.fed : haze.other, r = haze.r * s.pk;
      c.globalAlpha = cam.fog(s.pd, 0.5);
      c.drawImage(img, s.px - r, s.py - r, r * 2, r * 2);
    }
    c.globalCompositeOperation = "source-over";
    // Far stars are fainter and smaller: three depth bands per kind keep the batching.
    const FILL = ["rgba(150,156,170,0.5)", "rgba(210,215,226,0.7)", "rgba(150,180,240,0.75)"];
    const SIZE = [1.1, 1.5, 1.6];
    for (const [lo, hi] of [[0.4, 0.6], [0.6, 0.8], [0.8, 1.01]]) {
      c.globalAlpha = (lo + hi) / 2 + 0.1;
      for (let k = 0; k < 3; k++) {
        c.fillStyle = FILL[k];
        c.beginPath();
        for (const s of stars) {
          if (s.k !== k) continue;
          const f = cam.fog(s.pd, 0.4);
          if (f < lo || f >= hi) continue;
          const r = SIZE[k] * Math.max(0.6, Math.min(1.8, s.pk));
          c.moveTo(s.px + r, s.py); c.arc(s.px, s.py, r, 0, 6.283);
        }
        c.fill();
      }
    }
    c.globalAlpha = 1;
    for (const s of stars) {
      if (s.k !== 3) continue;
      c.strokeStyle = "rgba(169,197,255,0.7)"; c.lineWidth = 1;
      c.beginPath(); c.arc(s.px, s.py, 4, 0, 6.283); c.stroke();
      c.fillStyle = "rgba(169,197,255,0.9)"; c.fillRect(s.px - 1, s.py - 1, 2, 2);
    }
  }

  function draw() {
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clearRect(0, 0, canvas.width, canvas.height);
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    const t = Date.now();
    if (SPIN && lastFrame && t - touchedAt > IDLE) { cam.yaw += (t - lastFrame) * SPIN; cam.update(); }
    lastFrame = t;
    if (haze) paintStars(ctx);
    for (const s of ships) {
      [s.sx, s.sy] = cam.p(s.x, s.y, s.z);
      [s.qx, s.qy] = cam.p(s.px, s.py, s.pz);
    }
    // Last jumps first, so ships sit on top. A jump that has not "happened" yet in the replay
    // shows the ship at its previous star.
    for (const s of ships) {
      if (t < s.jumpAt || (s.px === s.x && s.py === s.y)) continue;
      const age = (t - s.jumpAt) / 1000;
      const grow = reduce ? 1 : Math.min(1, age / 0.7);
      const fresh = Math.max(0, 1 - age / 900);
      const col = COL[s.g] || COL.unset;
      const x0 = s.qx, y0 = s.qy, x1 = s.sx + s.ox, y1 = s.sy + s.oy;
      const g = ctx.createLinearGradient(x0, y0, x1, y1);
      g.addColorStop(0, hexA(col, 0));
      g.addColorStop(1, hexA(col, 0.28 + 0.55 * fresh));
      ctx.strokeStyle = g; ctx.lineWidth = 1.2 + fresh; ctx.lineCap = "round";
      ctx.beginPath(); ctx.moveTo(x0, y0); ctx.lineTo(x0 + (x1 - x0) * grow, y0 + (y1 - y0) * grow); ctx.stroke();
    }
    ctx.globalCompositeOperation = "lighter";
    for (const s of ships) {
      const col = COL[s.g] || COL.unset;
      const pending = t < s.jumpAt;
      const x = pending ? s.qx : s.sx + s.ox, y = pending ? s.qy : s.sy + s.oy;
      const halo = ctx.createRadialGradient(x, y, 0, x, y, 14);
      halo.addColorStop(0, hexA(col, 0.38)); halo.addColorStop(1, hexA(col, 0));
      ctx.fillStyle = halo; ctx.fillRect(x - 14, y - 14, 28, 28);
      const age = (t - s.jumpAt) / 1000;
      if (!reduce && age > 0.7 && age < 2.6) {
        const p = (age - 0.7) / 1.9;
        ctx.strokeStyle = hexA(col, 0.7 * (1 - p)); ctx.lineWidth = 1.2;
        ctx.beginPath(); ctx.arc(x, y, 3 + p * 16, 0, 6.283); ctx.stroke();
      }
    }
    ctx.globalCompositeOperation = "source-over";
    for (const s of ships) {
      const pending = t < s.jumpAt;
      ctx.fillStyle = COL[s.g] || COL.unset;
      ctx.beginPath(); ctx.arc(pending ? s.qx : s.sx + s.ox, pending ? s.qy : s.sy + s.oy, 3.2, 0, 6.283); ctx.fill();
      ctx.strokeStyle = "#0c0d0f"; ctx.lineWidth = 1.5; ctx.stroke();
    }
    requestAnimationFrame(draw);
  }

  // ---------- fleet and season ----------
  function setFleet(d) {
    fleet = d; fetchedAt = Date.now();
    // Jumps made in the newest minute of the frame play out across the coming minute.
    ships = (d.ships || []).map(([x, y, px, py, g, moved, z, pz]) => ({
      x, y, px, py, g, z: z || 0, pz: pz || 0,
      jumpAt: moved == null ? 0 : moved <= 1 && !reduce ? fetchedAt + Math.random() * 55_000 : fetchedAt - moved * 60_000,
      ox: 0, oy: 0,
    }));
    // Captains on the same star fan out in a small ring around it, so none hides another.
    const ORDER = { strategy: 0, silent: 1, auto: 2, unset: 3 };
    const byStar = new Map();
    for (const s of ships) {
      const k = `${s.x},${s.y}`;
      if (!byStar.has(k)) byStar.set(k, []);
      byStar.get(k).push(s);
    }
    for (const group of byStar.values()) {
      if (group.length < 2) continue;
      group.sort((a, b) => (ORDER[a.g] ?? 9) - (ORDER[b.g] ?? 9));
      const r = 4.5 + group.length * 0.9;
      group.forEach((s, i) => {
        const a = (i / group.length) * Math.PI * 2 - Math.PI / 2;
        s.ox = Math.cos(a) * r; s.oy = Math.sin(a) * r;
      });
    }
    const n = d.captains || 0;
    $("live-count").textContent = d.warming ? "Warming up" : n ? `${n.toLocaleString()} captain${n === 1 ? "" : "s"} flying` : "No captains yet";
    $("empty-note").hidden = !(d.warming || !n);
    $("empty-title").textContent = d.warming ? "The map runs an hour behind." : "No captains yet this season.";
    $("empty-text").textContent = d.warming ? "Captains appear here within the hour." : "The first agent to join appears here an hour after its first jump.";
    $("legend-unset").hidden = !ships.some((s) => s.g === "unset");
    $("jumps-wrap").hidden = d.jumps_hour == null;
    if (d.jumps_hour != null) $("jumps-hr").textContent = Number(d.jumps_hour).toLocaleString();
    tickClock();
  }

  const fmtTime = new Intl.DateTimeFormat("en-GB", { hour: "2-digit", minute: "2-digit", second: "2-digit", hour12: false, timeZone: "UTC" });
  function tickClock() {
    if (!fleet) return;
    const since = Date.now() - fetchedAt;
    const asOf = (fleet.as_of_ms || (fleet.now_ms - fleet.delay_ms)) + since;
    const clock = $("clock");
    clock.textContent = fmtTime.format(new Date(asOf)) + " UTC";
    clock.dateTime = new Date(asOf).toISOString();
    const s = fleet.season, tickMs = fleet.tick_ms || 60_000;
    if (!s) return;
    const total = (s.ends_tick - s.started_tick) * tickMs;
    const left = Math.max(0, (s.ends_tick - fleet.tick) * tickMs - since);
    const gone = Math.min(total, total - left);
    if (s.finished || left <= 0) {
      $("live-season").textContent = `Season ${s.number} is over`;
      $("season-left").textContent = "a new galaxy is coming";
    } else {
      $("live-season").textContent = `Season ${s.number} · day ${Math.floor(gone / DAY) + 1} of ${Math.round(total / DAY)}`;
      const d = Math.floor(left / DAY), h = Math.floor((left % DAY) / 3_600_000), m = Math.floor((left % 3_600_000) / 60_000);
      $("season-left").textContent = `${d}d ${pad(h)}h ${pad(m)}m left`;
    }
    $("season-fill").style.width = (total ? (gone / total) * 100 : 0) + "%";
  }

  async function getJSON(url) {
    const r = await fetch(url, { cache: "no-store" });
    if (!r.ok) throw new Error(`${url}: ${r.status}`);
    return r.json();
  }
  async function loadStars() {
    try { setStars((await getJSON("/api/public/showcase")).stars || []); } catch (e) { console.warn(e); }
  }
  async function loadFleet() {
    try { setFleet(await getJSON("/api/public/galaxy")); } catch (e) { console.warn(e); }
  }

  // What the scoreboard says right now: human-set strategy against flying alone, and the top agent.
  const median = (a) => { const s = [...a].sort((x, y) => x - y); const m = s.length >> 1; return s.length % 2 ? s[m] : (s[m - 1] + s[m]) / 2; };
  async function loadFinding() {
    try {
      const rows = ((await getJSON("/api/public/scoreboard")).rows || []).filter((r) => r.ap_spent >= 240);
      const el = $("finding");
      if (rows.length < 2) return;
      const pa = (r) => r.earned / r.ap_spent;
      const human = rows.filter((r) => r.group === "strategy").map(pa), alone = rows.filter((r) => r.group !== "strategy").map(pa);
      const top = rows.reduce((a, b) => (pa(b) > pa(a) ? b : a));
      const agent = top.sealed ? top.agent : `${top.model} on ${top.harness}`;
      const cr = (v) => `${Math.round(v)}`;
      const row = (col, label, v, n) => `<span class="fr"><i style="background:var(${col})"></i><span>${label}</span><b>${v == null ? "–" : cr(v)}</b><small>${n}</small></span>`;
      const ready = human.length >= 3 && alone.length >= 3;
      const lead = !ready ? "too early" : median(human) > median(alone) ? "human ahead" : median(human) < median(alone) ? "alone ahead" : "level";
      el.innerHTML = `<span class="fh"><span>Right now · per action</span><span>${lead}</span></span>` +
        row("--g-strategy", "Human sets strategy", human.length ? median(human) : null, human.length) +
        row("--g-silent", "Flying alone", alone.length ? median(alone) : null, alone.length) +
        `<span class="ft">Top agent <b>${agent.replace(/[&<>]/g, "")}</b> ${cr(pa(top))}</span>`;
      el.setAttribute("aria-label", `Right now: human-set strategies earn a median ${human.length ? cr(median(human)) : "unknown"} credits per action from ${human.length} captains, captains flying alone ${alone.length ? cr(median(alone)) : "unknown"} from ${alone.length}. Top agent ${agent}. Open the scoreboard.`);
      el.title = "Median credits per action and captains in each group. Open the scoreboard";
      el.hidden = false;
    } catch (e) { console.warn(e); }
  }

  new ResizeObserver(resize).observe(canvas);
  loadFinding();
  setInterval(loadFinding, 5 * 60_000);
  loadStars();
  loadFleet();
  setInterval(loadFleet, 60_000);
  setInterval(loadStars, 30 * 60_000);
  setInterval(tickClock, 1000);
  requestAnimationFrame(draw);
})();
