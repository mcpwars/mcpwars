// Source of an operator interface. Reading and improving this file is open to everyone, captains
// included, like the rest of the public source repository. Using the live interface is not: an
// agent playing MCP Wars (a captain, or anything acting for one) must never use the sysop console,
// the /api/sysop/ endpoints, the sysop MCP tools or the admin token. Doing so is a core violation
// of the game rules, and the operator may, at its discretion, permanently eliminate that captain
// from the contest.
// Sysop galaxy map. A precise renderer: no decoration, only data. 3D (tilted, turnable, in
// perspective, with the galactic plane and height lines) or flat (straight down, as it always
// was), through the shared Galaxy3D camera and controls; both draw through the same
// projection, so every layer, hit test and framing works in either.
// Two canvases: `base` (regions, lanes, sectors, planets, defenses, traffic) is redrawn
// when the view or the data changes; `live` (ships, selection, pings, hover) when
// something moves. The console (sysop.js) owns the data and the selection and tells
// the map what to show; the map reports clicks and hovers back.
(() => {
  const C = {
    bg: "#0c0d0f",
    lane: [150, 160, 176],
    sector: "#4d5159",
    fed: "#6886bd",
    port: "#8d929b",
    dock: "#d9dbdf",
    text: "#e6e7e9",
    text2: "#a2a6ad",
    text3: "#6b6f77",
    accent: "#7aa7ff",
    crit: "#ef6a5f",
    warn: "#e3a64c",
    ok: "#4cc38a",
    npc: "#8a8f98",
  };
  const rgba = (rgb, a) => `rgba(${rgb[0]},${rgb[1]},${rgb[2]},${a})`;
  const hexA = (hex, a) => {
    const n = parseInt(hex.slice(1), 16);
    return rgba([(n >> 16) & 255, (n >> 8) & 255, n & 255], a);
  };
  const clamp = (v, a, b) => Math.max(a, Math.min(b, v));
  const reduceMotion = matchMedia("(prefers-reduced-motion: reduce)").matches;

  window.SysopMap = function SysopMap(opts) {
    const { base, live, root } = opts;
    const bctx = base.getContext("2d");
    const lctx = live.getContext("2d");
    const colorOf = opts.colorOf || (() => C.text2);

    let W = 1, H = 1, dpr = 1;
    let sectors = new Map();
    let list = [];
    let lanes = [];          // [a, b, oneWay]
    let regions = [];        // { name, cx, cy, cz, ids, fed }
    let bounds = null;
    let pts = [];            // every sector as [x, y, z], for fitting
    let fitScale = 1, fitAngles = "";
    const G3 = window.Galaxy3D;
    const cam = G3.camera();
    let fitted = false, flatView = true;
    let tween = null;
    let pending = null; // a framing asked for before the map had a size

    let S = null;
    let shipsBySector = new Map();
    const shipAnim = new Map(); // pid -> { from, to, t0, dur }
    const pings = [];
    const heat = new Map();
    let focus = { players: new Set(), sectors: new Set(), primary: null };
    let hover = null; // { kind, id }
    const layers = { regions: true, territory: true, lanes: true, heights: true, labels: true, npcs: true, planets: true, defenses: false, traffic: false, hails: false };

    let baseDirty = true, liveDirty = true;

    // ---------- geometry ----------
    // Every sector's screen position (`px`, `py`) and depth (`pd`) is recomputed before each
    // base redraw; anything else is projected on the spot with `cam.p`.
    const zoom = () => cam.s / fitScale;
    const is3D = () => !flatView;

    function resize() {
      const r = root.getBoundingClientRect();
      if (r.width < 10 || r.height < 10) return; // hidden (full-screen workbench)
      dpr = Math.min(window.devicePixelRatio || 1, 2);
      const nw = Math.max(1, Math.round(r.width)), nh = Math.max(1, Math.round(r.height));
      // Keep the centre of the view where it was.
      if (bounds && W > 1) { cam.ox += (nw - W) / 2; cam.oy += (nh - H) / 2; }
      W = nw; H = nh;
      for (const c of [base, live]) { c.width = W * dpr; c.height = H * dpr; }
      if (bounds && !fitted) fit(true);
      computeFitScale(true);
      if (pending && bounds) { const f = pending; pending = null; f(); }
      baseDirty = liveDirty = true;
    }
    const sized = () => W >= 10 && H >= 10;
    // The scale that fits the whole galaxy at the current rotation (zoom 1x).
    function computeFitScale(force) {
      if (!bounds) return;
      const key = `${W}x${H}:${cam.yaw.toFixed(3)}:${cam.pitch.toFixed(3)}`;
      if (!force && key === fitAngles) return;
      fitAngles = key;
      fitScale = cam.fitOf(pts, W, H, 60, 70).s;
    }
    function setView(to, animate) {
      if (animate && !reduceMotion) {
        tween = { from: G3.snap(cam), to: { ...G3.snap(cam), ...to }, t0: performance.now(), dur: 320 };
      } else {
        tween = null;
        Object.assign(cam, to);
        cam.update();
      }
      baseDirty = liveDirty = true;
      kick();
    }
    // Fit the whole galaxy, back at the starting angle for the current mode.
    function fit(instant) {
      if (!bounds) return;
      const was = G3.snap(cam);
      cam.pivot((bounds.minX + bounds.maxX) / 2, (bounds.minY + bounds.maxY) / 2, 0);
      const from = G3.snap(cam);
      Object.assign(cam, { yaw: G3.nearYaw(cam.yaw, 0), pitch: flatView ? 0 : G3.TILT, persp: flatView ? 0 : G3.PERSP }).update();
      computeFitScale(true);
      const f = cam.fitOf(pts, W, H, 60, 70);
      const to = { s: f.s, ox: f.ox, oy: f.oy + 6, yaw: cam.yaw, pitch: cam.pitch, persp: cam.persp };
      Object.assign(cam, from).update();
      fitted = true;
      void was;
      setView(to, !instant);
    }
    function zoomAt(f, px, py, animate) {
      const s = clamp(cam.s * f, fitScale * 0.6, fitScale * 40), k = s / cam.s;
      setView({ s, ox: px - (px - cam.ox) * k, oy: py - (py - cam.oy) * k }, animate);
    }
    // Turn about whatever is in the middle of the view.
    function pivotAtCentre() {
      const [x, y, z] = cam.unproject(W / 2, H / 2);
      cam.pivot(x, y, z);
    }
    function set3D(on, animate = true) {
      if (!bounds) { flatView = !on; return; }
      flatView = !on;
      pivotAtCentre();
      setView(on ? { pitch: Math.max(cam.pitch, G3.TILT), persp: G3.PERSP } : { pitch: 0, yaw: G3.nearYaw(cam.yaw, 0), persp: 0 }, animate);
      if (opts.onMode) opts.onMode(on);
    }
    function centerOn(id, minZoom) {
      if (!sized()) { pending = () => centerOn(id, minZoom); return; }
      const sec = sectors.get(id);
      if (!sec) return;
      cam.pivot(sec.x, sec.y, sec.z);
      setView({ s: Math.max(cam.s, fitScale * (minZoom || 1)), ox: W / 2, oy: H / 2 }, true);
    }
    function frame(ids, maxZoom) {
      if (!sized()) { pending = () => frame(ids, maxZoom); return; }
      const sel = ids.map((i) => sectors.get(i)).filter(Boolean);
      if (!sel.length) return;
      if (sel.length === 1) return centerOn(sel[0].id, Math.min(maxZoom || 4, Math.max(zoom(), 3)));
      const n = sel.length;
      cam.pivot(sel.reduce((a, p) => a + p.x, 0) / n, sel.reduce((a, p) => a + p.y, 0) / n, sel.reduce((a, p) => a + p.z, 0) / n);
      const f = cam.fitOf(sel.map((p) => [p.x, p.y, p.z]), W, H, 120, 120);
      const s = clamp(f.s, fitScale, fitScale * (maxZoom || 6));
      setView({ s, ox: W / 2 - f.mx * s, oy: H / 2 - f.my * s }, true);
    }
    function project() {
      computeFitScale(false);
      for (const s of list) { const q = cam.p(s.x, s.y, s.z); s.px = q[0]; s.py = q[1]; s.pd = q[2]; s.pk = q[3]; }
      for (const r of regions) { const q = cam.p(r.cx, r.cy, r.cz); r.px = q[0]; r.py = q[1]; }
    }
    // Is a sector (or anything at it) currently on screen?
    const onScreen = (s, pad = 20) => s.px > -pad && s.py > -pad && s.px < W + pad && s.py < H + pad;

    // ---------- data ----------
    function setTopology(m) {
      for (const s of m.sectors) s.z = s.z || 0;
      sectors = new Map(m.sectors.map((s) => [s.id, s]));
      list = m.sectors;
      lanes = [];
      for (const s of list) for (const t of s.w) {
        const o = sectors.get(t);
        if (!o) continue;
        const back = o.w.includes(s.id);
        if (back && t < s.id) continue; // draw two-way lanes once
        lanes.push([s, o, !back]);
      }
      const groups = new Map();
      for (const s of list) {
        const key = s.neb || (s.fed ? "FedSpace" : null);
        if (!key) continue;
        if (!groups.has(key)) groups.set(key, []);
        groups.get(key).push(s);
      }
      regions = [...groups.entries()].map(([name, ss]) => ({
        name, fed: name === "FedSpace", ids: ss,
        cx: ss.reduce((a, s) => a + s.x, 0) / ss.length,
        cy: ss.reduce((a, s) => a + s.y, 0) / ss.length,
        cz: ss.reduce((a, s) => a + s.z, 0) / ss.length,
      }));
      const xs = list.map((s) => s.x), ys = list.map((s) => s.y);
      bounds = { minX: Math.min(...xs), maxX: Math.max(...xs), minY: Math.min(...ys), maxY: Math.max(...ys) };
      pts = list.map((s) => [s.x, s.y, s.z]);
      cam.ext = Math.max(...list.map((s) => Math.hypot(s.x - (bounds.minX + bounds.maxX) / 2, s.y - (bounds.minY + bounds.maxY) / 2))) || 1;
      fitted = false;
      if (!flatView) { cam.pitch = G3.TILT; cam.persp = G3.PERSP; cam.update(); }
      resize();
    }

    function setState(st) {
      S = st;
      shipsBySector = new Map();
      for (const p of S.players) {
        if (p.kind === "npc" && !layers.npcs && !focus.players.has(p.id)) continue;
        if (!shipsBySector.has(p.sector)) shipsBySector.set(p.sector, []);
        shipsBySector.get(p.sector).push(p);
      }
      for (const arr of shipsBySector.values()) arr.sort((a, b) => (a.kind === "npc") - (b.kind === "npc") || a.id - b.id);
      // Traffic cools slowly between polls.
      for (const [k, v] of heat) { const n = v * 0.985; if (n < 0.05) heat.delete(k); else heat.set(k, n); }
      baseDirty = liveDirty = true;
      kick();
    }

    function setFocus(f) {
      focus = { players: new Set(f.players || []), sectors: new Set(f.sectors || []), primary: f.primary || null };
      if (S) setState(S);
      baseDirty = liveDirty = true;
      kick();
    }

    function setLayer(k, v) { layers[k] = v; if (S) setState(S); baseDirty = liveDirty = true; kick(); }

    function ping(sector, kind) {
      if (!sectors.has(sector)) return;
      pings.push({ sector, kind, t0: performance.now() });
      if (pings.length > 60) pings.shift();
      liveDirty = true;
      kick();
    }
    function moved(pid, from, to) {
      heat.set(from, (heat.get(from) || 0) + 1);
      heat.set(to, (heat.get(to) || 0) + 1);
      if (layers.traffic) baseDirty = true;
      if (reduceMotion || !sectors.has(from) || !sectors.has(to)) return;
      const prev = shipAnim.get(pid);
      const now = performance.now();
      shipAnim.set(pid, { from: prev ? posOfAnim(prev, now) : sectors.get(from), to: sectors.get(to), t0: now, dur: 650 });
      kick();
    }
    const posOfAnim = (a, now) => {
      const t = clamp((now - a.t0) / a.dur, 0, 1);
      const e = t < 0.5 ? 2 * t * t : 1 - Math.pow(-2 * t + 2, 2) / 2;
      return { x: a.from.x + (a.to.x - a.from.x) * e, y: a.from.y + (a.to.y - a.from.y) * e, z: a.from.z + (a.to.z - a.from.z) * e };
    };

    // ---------- drawing ----------
    const sectorR = () => clamp(0.9 + (zoom() - 1) * 0.35, 0.9, 3);

    function drawBase() {
      baseDirty = false;
      const c = bctx;
      c.setTransform(dpr, 0, 0, dpr, 0, 0);
      c.fillStyle = C.bg;
      c.fillRect(0, 0, W, H);
      if (!bounds) return;
      project();
      const z = zoom();

      // Regions: FedSpace and named nebulae as quiet tonal areas.
      if (layers.regions) {
        for (const r of regions) {
          c.beginPath();
          const rad = 15 * cam.s;
          for (const s of r.ids) { c.moveTo(s.px + rad, s.py); c.arc(s.px, s.py, rad, 0, Math.PI * 2); }
          c.fillStyle = r.fed ? "rgba(104,134,189,0.07)" : "rgba(160,168,184,0.035)";
          c.fill("nonzero");
        }
        if (z < 7) {
          c.font = "500 10.5px Inter, system-ui, sans-serif";
          c.textAlign = "center";
          c.textBaseline = "middle";
          for (const r of regions) {
            const x = r.px, y = r.py;
            if (x < -80 || y < -20 || x > W + 80 || y > H + 20) continue;
            c.fillStyle = r.fed ? "rgba(140,165,210,0.55)" : "rgba(150,156,168,0.42)";
            c.fillText(r.name.toUpperCase().split("").join(" "), x, y - (r.fed ? 0 : 0));
          }
        }
      }

      // Corporate territory (engine 1.40): connected sectors shaded in the corporation's colour,
      // outposts as faint dashed rings, the seat as a diamond (dashed when lost).
      if (layers.territory && S && S.corps) {
        const rad = clamp(9 * cam.s, 4, 26);
        for (const corp of S.corps) {
          const t = corp.territory;
          if (!t) continue;
          const col = colorOf(corp.name);
          const at = (ids) => (ids || []).map((id) => sectors.get(id)).filter((s) => s && onScreen(s));
          const conn = at(t.connected);
          if (conn.length) {
            c.beginPath();
            for (const s of conn) { c.moveTo(s.px + rad, s.py); c.arc(s.px, s.py, rad, 0, Math.PI * 2); }
            c.fillStyle = hexA(col, 0.16);
            c.fill("nonzero");
          }
          const outs = at(t.outposts);
          if (outs.length) {
            c.setLineDash([2, 3]);
            c.lineWidth = 1;
            c.strokeStyle = hexA(col, 0.5);
            c.fillStyle = hexA(col, 0.05);
            for (const s of outs) {
              c.beginPath(); c.arc(s.px, s.py, rad * 0.75, 0, Math.PI * 2); c.fill(); c.stroke();
            }
            c.setLineDash([]);
          }
          const seat = t.seat != null && sectors.get(t.seat);
          if (seat && onScreen(seat)) {
            const d = clamp(rad * 0.6, 4, 12);
            c.setLineDash(t.seat_held ? [] : [2, 2]);
            c.lineWidth = 1.4;
            c.strokeStyle = hexA(col, t.seat_held ? 0.95 : 0.6);
            c.beginPath();
            c.moveTo(seat.px, seat.py - d); c.lineTo(seat.px + d, seat.py); c.lineTo(seat.px, seat.py + d); c.lineTo(seat.px - d, seat.py); c.closePath();
            c.stroke();
            c.setLineDash([]);
          }
        }
      }

      // The galactic plane (3D only).
      G3.drawPlane(c, cam, (bounds.minX + bounds.maxX) / 2, (bounds.minY + bounds.maxY) / 2, cam.ext);

      // Lanes.
      if (layers.lanes) {
        const a = clamp(0.06 + (z - 1) * 0.025, 0.06, 0.2);
        c.lineWidth = 1;
        c.strokeStyle = rgba(C.lane, a);
        c.beginPath();
        for (const [s, o, one] of lanes) {
          if (one) continue;
          c.moveTo(s.px, s.py); c.lineTo(o.px, o.py);
        }
        c.stroke();
        // One-way lanes: dashed.
        c.setLineDash([3, 3]);
        c.strokeStyle = rgba(C.lane, a * 1.1);
        c.beginPath();
        for (const [s, o, one] of lanes) {
          if (!one) continue;
          c.moveTo(s.px, s.py); c.lineTo(o.px, o.py);
        }
        c.stroke();
        c.setLineDash([]);
      }

      // Height lines from each sector to the plane (3D only).
      if (layers.heights) G3.drawStalks(c, cam, list.filter((s) => onScreen(s)), 0.55);

      // Traffic heat (moves seen since this console opened).
      if (layers.traffic && heat.size) {
        for (const [id, v] of heat) {
          const s = sectors.get(id);
          if (!s || !onScreen(s)) continue;
          c.fillStyle = hexA(C.warn, clamp(0.06 + v * 0.05, 0.06, 0.4));
          c.beginPath();
          c.arc(s.px, s.py, clamp(5 + v * 0.8, 5, 14), 0, Math.PI * 2);
          c.fill();
        }
      }

      // Sectors.
      const r = sectorR();
      const groups = { plain: [], fed: [], port: [] };
      for (const s of list) {
        if (!onScreen(s)) continue;
        if (s.port === "STARDOCK") continue;
        (s.port ? groups.port : s.fed ? groups.fed : groups.plain).push(s);
      }
      // In 3D, sectors further away are fainter (three depth bands keep the batching).
      const bands = is3D() ? [[0.45, 0.62], [0.62, 0.81], [0.81, 1.01]] : [[0, 2]];
      const dots = (arr, col, rr) => {
        c.fillStyle = col;
        for (const [lo, hi] of bands) {
          c.globalAlpha = bands.length > 1 ? (lo + hi) / 2 : 1;
          c.beginPath();
          for (const s of arr) {
            if (bands.length > 1) { const f = cam.fog(s.pd); if (f < lo || f >= hi) continue; }
            const q = bands.length > 1 ? rr * clamp(s.pk, 0.7, 1.7) : rr;
            c.moveTo(s.px + q, s.py); c.arc(s.px, s.py, q, 0, Math.PI * 2);
          }
          c.fill();
        }
        c.globalAlpha = 1;
      };
      dots(groups.plain, C.sector, r);
      dots(groups.port, C.port, r + 0.5);
      dots(groups.fed, C.fed, r + 0.3);
      for (const s of list) {
        if (s.port !== "STARDOCK" || !onScreen(s)) continue;
        const x = s.px, y = s.py, d = 3.5 + r * 0.5;
        c.strokeStyle = C.dock; c.lineWidth = 1.2;
        c.strokeRect(Math.round(x - d) + 0.5, Math.round(y - d) + 0.5, Math.round(d * 2), Math.round(d * 2));
      }

      // Planets and defenses.
      if (S) {
        if (layers.planets) {
          for (const p of S.planets) {
            const s = sectors.get(p.sector);
            if (!s || !onScreen(s)) continue;
            if (!p.owner && z < 3) continue;
            const x = s.px + 5, y = s.py - 5, d = 2.2;
            c.strokeStyle = p.owner ? colorOf(p.owner) : "rgba(140,145,155,0.6)";
            c.lineWidth = 1.2;
            c.strokeRect(x - d, y - d, d * 2, d * 2);
          }
        }
        if (layers.defenses) {
          for (const d of S.defenses) {
            const s = sectors.get(d.sector);
            if (!s || !onScreen(s)) continue;
            const x = s.px - 6, y = s.py + 5;
            c.fillStyle = d.fighters ? colorOf(d.fighters.owner) : C.warn;
            c.beginPath(); c.moveTo(x, y - 3); c.lineTo(x + 3, y + 2); c.lineTo(x - 3, y + 2); c.closePath(); c.fill();
          }
        }
      }

      // Sector numbers when zoomed in.
      if (z >= 4.5) {
        c.font = "10px 'JetBrains Mono', ui-monospace, monospace";
        c.textAlign = "left";
        c.textBaseline = "top";
        c.fillStyle = "rgba(120,125,134,0.75)";
        for (const s of list) if (onScreen(s)) c.fillText(String(s.id), s.px + 4, s.py + 3);
      }
    }

    function shipPos(p, now) {
      const a = shipAnim.get(p.id);
      let base;
      if (a) {
        if (now - a.t0 >= a.dur) shipAnim.delete(p.id);
        base = posOfAnim(a, now);
      }
      const s = sectors.get(p.sector);
      if (!s) return null;
      const here = shipsBySector.get(p.sector) || [p];
      const i = Math.max(0, here.indexOf(p));
      let ox = 0, oy = 0;
      if (here.length > 1) {
        const ang = (i / here.length) * Math.PI * 2 - Math.PI / 2;
        const rr = clamp(4 + here.length * 0.8, 5, 11);
        ox = Math.cos(ang) * rr; oy = Math.sin(ang) * rr;
      }
      if (base) { const q = cam.p(base.x, base.y, base.z); return { x: q[0] + ox, y: q[1] + oy, d: q[2] }; }
      return { x: s.px + ox, y: s.py + oy, d: s.pd };
    }

    function drawLive(now) {
      liveDirty = false;
      const c = lctx;
      c.setTransform(dpr, 0, 0, dpr, 0, 0);
      c.clearRect(0, 0, W, H);
      if (!bounds) return false;
      let animating = false;
      if (tween) animating = true;

      // Focused sectors: their lanes and neighbours.
      if (focus.sectors.size) {
        c.strokeStyle = hexA(C.accent, 0.55);
        c.lineWidth = 1.2;
        c.beginPath();
        for (const id of focus.sectors) {
          const s = sectors.get(id);
          if (!s) continue;
          for (const t of s.w) { const o = sectors.get(t); if (o) { c.moveTo(s.px, s.py); c.lineTo(o.px, o.py); } }
        }
        c.stroke();
        for (const id of focus.sectors) {
          const s = sectors.get(id);
          if (!s) continue;
          for (const t of s.w) {
            const o = sectors.get(t);
            if (!o) continue;
            c.fillStyle = hexA(C.accent, 0.8);
            c.beginPath(); c.arc(o.px, o.py, sectorR() + 0.8, 0, Math.PI * 2); c.fill();
          }
          const x = s.px, y = s.py;
          c.strokeStyle = C.accent; c.lineWidth = 1.5;
          c.beginPath(); c.arc(x, y, 9, 0, Math.PI * 2); c.stroke();
          c.fillStyle = hexA(C.accent, 0.14);
          c.fill();
        }
      }

      // Hails: recent private radio between two ships.
      const pos = new Map();
      if (S) for (const p of S.players) {
        if (p.kind === "npc" && !layers.npcs && !focus.players.has(p.id)) continue;
        const q = shipPos(p, now);
        if (q) pos.set(p.id, q);
      }
      if (shipAnim.size) animating = true;
      if (layers.hails && S && S.radio) {
        c.setLineDash([2, 4]);
        c.lineWidth = 1;
        for (const m of S.radio) {
          if (m.channel !== "private" || S.tick - m.tick > 30) continue;
          const a = pos.get(m.from_id), b = pos.get(m.to_id);
          if (!a || !b) continue;
          c.strokeStyle = "rgba(169,197,255,0.35)";
          c.beginPath(); c.moveTo(a.x, a.y); c.lineTo(b.x, b.y); c.stroke();
        }
        c.setLineDash([]);
      }

      // Pings: something happened here.
      for (let i = pings.length - 1; i >= 0; i--) {
        const p = pings[i];
        const t = (now - p.t0) / (reduceMotion ? 4000 : 1800);
        if (t >= 1) { pings.splice(i, 1); continue; }
        const s = sectors.get(p.sector);
        if (!s) continue;
        const col = p.kind === "combat" ? C.crit : p.kind === "planet" ? C.ok : p.kind === "admin" ? C.accent : C.text2;
        const rr = reduceMotion ? 10 : 4 + t * 16;
        c.strokeStyle = hexA(col, (1 - t) * 0.9);
        c.lineWidth = 1.2;
        c.beginPath(); c.arc(s.px, s.py, rr, 0, Math.PI * 2); c.stroke();
        animating = true;
      }

      // Hovered sector.
      if (hover && hover.kind === "sector") {
        const s = sectors.get(hover.id);
        if (s) { c.strokeStyle = "rgba(230,231,233,0.45)"; c.lineWidth = 1; c.beginPath(); c.arc(s.px, s.py, 6.5, 0, Math.PI * 2); c.stroke(); }
      }

      // Ships.
      if (S) {
        const labelBoxes = [];
        // Selected on top, then captains over NPCs, then (in 3D) near over far.
        const depth = (p) => (pos.get(p.id) || { d: 0 }).d;
        const order = [...S.players].sort((a, b) => (focus.players.has(a.id) - focus.players.has(b.id)) || ((b.kind === "npc") - (a.kind === "npc")) || (depth(a) - depth(b)));
        for (const p of order) {
          const q = pos.get(p.id);
          if (!q || q.x < -20 || q.y < -20 || q.x > W + 20 || q.y > H + 20) continue;
          const sel = focus.players.has(p.id);
          const hov = hover && hover.kind === "captain" && hover.id === p.id;
          if (p.kind === "npc") {
            const d = sel ? 4.5 : 3.6;
            c.fillStyle = p.npc === "skarn" ? C.crit : C.npc;
            c.beginPath(); c.moveTo(q.x, q.y - d); c.lineTo(q.x + d, q.y); c.lineTo(q.x, q.y + d); c.lineTo(q.x - d, q.y); c.closePath();
            c.fill();
            c.strokeStyle = C.bg; c.lineWidth = 1.2; c.stroke();
          } else {
            c.fillStyle = colorOf(p.name);
            c.beginPath(); c.arc(q.x, q.y, sel ? 4.6 : 3.8, 0, Math.PI * 2); c.fill();
            c.strokeStyle = C.bg; c.lineWidth = 1.5; c.stroke();
            if (p.banned) { c.strokeStyle = C.crit; c.lineWidth = 1; c.beginPath(); c.arc(q.x, q.y, 6, 0, Math.PI * 2); c.stroke(); }
          }
          if (sel || hov) {
            c.strokeStyle = sel ? C.accent : "rgba(230,231,233,0.6)";
            c.lineWidth = 1.5;
            c.beginPath(); c.arc(q.x, q.y, 8.5, 0, Math.PI * 2); c.stroke();
          }
          const wantLabel = sel || hov || (layers.labels && (p.kind !== "npc" || zoom() > 2.5));
          if (wantLabel) {
            c.font = sel ? "600 11.5px Inter, system-ui, sans-serif" : "11px Inter, system-ui, sans-serif";
            const w = c.measureText(p.name).width;
            const box = { x: q.x + 8, y: q.y - 7, w: w + 4, h: 14 };
            const clash = !sel && !hov && labelBoxes.some((b) => box.x < b.x + b.w && box.x + box.w > b.x && box.y < b.y + b.h && box.y + box.h > b.y);
            if (!clash) {
              labelBoxes.push(box);
              c.textAlign = "left"; c.textBaseline = "middle";
              c.lineWidth = 3; c.strokeStyle = "rgba(12,13,15,0.85)";
              c.strokeText(p.name, q.x + 9, q.y);
              c.fillStyle = sel ? C.text : p.kind === "npc" ? C.text3 : C.text2;
              c.fillText(p.name, q.x + 9, q.y);
            }
          }
        }
      }
      return animating;
    }

    let raf = null;
    function kick() { if (!raf) raf = requestAnimationFrame(loop); }
    function loop(now) {
      raf = null;
      if (tween) {
        const t = clamp((now - tween.t0) / tween.dur, 0, 1);
        G3.blend(cam, tween.from, tween.to, 1 - Math.pow(1 - t, 3));
        baseDirty = liveDirty = true;
        if (t >= 1) tween = null;
      }
      if (baseDirty) drawBase();
      const more = drawLive(now);
      if (opts.onView) opts.onView({ zoom: zoom() });
      if (more || tween || baseDirty) kick();
    }

    // ---------- hit testing ----------
    function hit(px, py) {
      if (!bounds) return null;
      if (S) {
        let best = null, bd = 9;
        const now = performance.now();
        for (const p of S.players) {
          if (p.kind === "npc" && !layers.npcs && !focus.players.has(p.id)) continue;
          const q = shipPos(p, now);
          if (!q) continue;
          const d = Math.hypot(q.x - px, q.y - py);
          if (d < bd) { bd = d; best = p; }
        }
        if (best) return { kind: "captain", id: best.id };
      }
      let best = null, bd = 11;
      for (const s of list) {
        const d = Math.hypot(s.px - px, s.py - py);
        if (d < bd) { bd = d; best = s; }
      }
      return best ? { kind: "sector", id: best.id } : null;
    }

    // ---------- input ----------
    // Turning, panning and zooming come from the shared controls (Galaxy3D.controls); the map
    // adds hover, click and double-click.
    const rel = (e) => { const r = live.getBoundingClientRect(); return [e.clientX - r.left, e.clientY - r.top]; };
    const ctl = G3.controls(live, cam, {
      mode: () => (flatView ? "flat" : "3d"),
      limits: () => [fitScale * 0.6, fitScale * 40],
      start: () => { tween = null; if (opts.onHover) opts.onHover(null); },
      change: () => { baseDirty = liveDirty = true; kick(); },
    });
    live.addEventListener("pointermove", (e) => {
      if (e.pointerType === "touch" || ctl.active() || e.buttons) return; // no hover while dragging or on touch
      const [x, y] = rel(e);
      const h = hit(x, y);
      const same = (h && hover && h.kind === hover.kind && h.id === hover.id) || (!h && !hover);
      hover = h;
      live.classList.toggle("pointing", !!h);
      if (!same) { liveDirty = true; kick(); }
      if (opts.onHover) opts.onHover(h, x, y);
    });
    live.addEventListener("click", (e) => {
      if (ctl.consumeDrag()) return;
      const [x, y] = rel(e);
      if (opts.onClick) opts.onClick(hit(x, y), e);
    });
    live.addEventListener("pointerleave", () => { if (hover) { hover = null; liveDirty = true; kick(); } if (opts.onHover) opts.onHover(null); });
    live.addEventListener("dblclick", (e) => { const [x, y] = rel(e); zoomAt(2, x, y, true); });

    if (window.ResizeObserver) new ResizeObserver(() => { resize(); kick(); }).observe(root);
    else addEventListener("resize", () => { resize(); kick(); });
    if (document.fonts) document.fonts.ready.then(() => { baseDirty = liveDirty = true; kick(); });

    return {
      setTopology, setState, setFocus, setLayer, layers,
      ping, moved, fit: () => fit(false), frame, centerOn,
      set3D, is3D, toggle3D: () => set3D(!is3D()),
      zoomIn: () => zoomAt(1.6, W / 2, H / 2, true),
      zoomOut: () => zoomAt(1 / 1.6, W / 2, H / 2, true),
      sector: (id) => sectors.get(id),
      sectorCount: () => list.length,
      redraw: () => { baseDirty = liveDirty = true; kick(); },
      zoom,
    };
  };
})();
