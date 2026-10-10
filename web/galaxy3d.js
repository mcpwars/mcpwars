// Galaxy3D: the orbit camera, controls and 3D cues shared by the home page, /watch and the
// sysop console. Sectors have x, y and z (height above the galactic plane, same units). The
// camera turns the galaxy about a pivot: `yaw` spins it in its plane, `pitch` tilts it (0 is
// straight down, the flat map; about 1.4 is nearly edge on). `persp` adds perspective (0 is
// orthographic, which the flat map uses so it stays exactly the old map).
(() => {
  const PITCH_MAX = 1.45;
  const clamp = (v, a, b) => Math.max(a, Math.min(b, v));

  function camera(init) {
    const c = Object.assign({ yaw: 0, pitch: 0, s: 1, ox: 0, oy: 0, cx: 0, cy: 0, cz: 0, persp: 0, ext: 1 }, init || {});
    let cy_ = 1, sy_ = 0, cp = 1, sp = 0;
    // Call after changing yaw or pitch.
    c.update = () => {
      c.pitch = clamp(c.pitch, 0, PITCH_MAX);
      cy_ = Math.cos(c.yaw); sy_ = Math.sin(c.yaw); cp = Math.cos(c.pitch); sp = Math.sin(c.pitch);
      return c;
    };
    // World to screen: [x, y, depth (towards the viewer), scale factor].
    c.p = (x, y, z) => {
      const dx = x - c.cx, dy = y - c.cy, dz = (z || 0) - c.cz;
      const rx = dx * cy_ - dy * sy_, ry = dx * sy_ + dy * cy_;
      const ty = ry * cp - dz * sp, d = ry * sp + dz * cp;
      let k = 1;
      if (c.persp > 0) { const f = c.ext * c.persp; k = f / Math.max(f * 0.3, f - d); }
      return [c.ox + rx * c.s * k, c.oy + ty * c.s * k, d, k];
    };
    // Screen to world, on the plane through the pivot parallel to the galaxy (perspective ignored).
    c.unproject = (px, py) => {
      const rx = (px - c.ox) / c.s, ty = (py - c.oy) / c.s;
      const ry = ty / Math.max(0.05, cp);
      return [c.cx + rx * cy_ + ry * sy_, c.cy - rx * sy_ + ry * cy_, c.cz];
    };
    // Move the pivot to a world point, keeping that point where it is on screen.
    c.pivot = (x, y, z) => {
      const [px, py] = c.p(x, y, z);
      c.cx = x; c.cy = y; c.cz = z || 0; c.ox = px; c.oy = py;
      return c;
    };
    c.zoomAt = (f, px, py) => { c.s *= f; c.ox = px - (px - c.ox) * f; c.oy = py - (py - c.oy) * f; return c; };
    c.orbit = (dyaw, dpitch) => { c.yaw += dyaw; c.pitch += dpitch; return c.update(); };
    // Scale and offsets that fit world points [[x, y, z], ...] around the current pivot into a
    // w x h box with the given padding, at the current rotation (`mx`, `my`: the middle of the
    // points relative to the pivot, in world units on screen). Does not change the camera.
    c.fitOf = (pts, w, h, padX, padY) => {
      let x0 = Infinity, x1 = -Infinity, y0 = Infinity, y1 = -Infinity;
      const s0 = c.s, ox0 = c.ox, oy0 = c.oy;
      c.s = 1; c.ox = 0; c.oy = 0;
      for (const q of pts) {
        const [x, y] = c.p(q[0], q[1], q[2]);
        if (x < x0) x0 = x; if (x > x1) x1 = x; if (y < y0) y0 = y; if (y > y1) y1 = y;
      }
      c.s = s0; c.ox = ox0; c.oy = oy0;
      if (!isFinite(x0)) return { s: c.s, ox: c.ox, oy: c.oy, mx: 0, my: 0 };
      const s = Math.min((w - padX) / (x1 - x0 || 1), (h - padY) / (y1 - y0 || 1));
      const mx = (x0 + x1) / 2, my = (y0 + y1) / 2;
      return { s, ox: w / 2 - mx * s, oy: h / 2 - my * s, mx, my };
    };
    // Depth cue in [lo, 1]: far things fade, near ones stay bright. Flat map: always 1.
    c.fog = (d, lo = 0.4) => (c.pitch < 0.02 ? 1 : clamp(lo + (1 - lo) * (0.5 + d / (2 * c.ext)), lo, 1));
    // How much of the 3D dressing (plane grid, height lines) to show: none flat, all once tilted.
    c.tilt = () => clamp(c.pitch / 0.35, 0, 1);
    return c.update();
  }

  // The camera state a tween moves between; the pivot stays put while it runs.
  const KEYS = ["s", "ox", "oy", "yaw", "pitch", "persp"];
  const snap = (c) => Object.fromEntries(KEYS.map((k) => [k, c[k]]));
  const blend = (c, a, b, e) => { for (const k of KEYS) c[k] = a[k] + (b[k] - a[k]) * e; c.update(); };
  // The equivalent of `yaw` closest to `to`, so a tween never spins the long way round.
  const nearYaw = (yaw, to) => to + Math.round((yaw - to) / (2 * Math.PI)) * 2 * Math.PI;

  // Mouse, touch and keyboard for one canvas. Drag turns the galaxy (on the flat map it pans);
  // right-drag or shift-drag pans; the wheel or a pinch zooms (two fingers also pan); a flick
  // keeps it turning for a moment. Arrows turn (shift+arrows pan), + and - zoom.
  // h: { mode() -> "3d" | "flat", limits() -> [min s, max s], change(), start()?, wheel: "always" | "ctrl" }
  function controls(el, cam, h) {
    const pts = new Map();
    let g = null, pinch = null, vel = null, raf = 0, dragged = false, lastMove = 0;
    const rel = (e) => { const r = el.getBoundingClientRect(); return [e.clientX - r.left, e.clientY - r.top]; };
    const is3D = () => h.mode() === "3d";
    const zoomBy = (f, x, y) => { const [lo, hi] = h.limits(); cam.zoomAt(clamp(cam.s * f, lo, hi) / cam.s, x, y); };
    const pivotCentre = () => { const p = cam.unproject(el.clientWidth / 2, el.clientHeight / 2); cam.pivot(p[0], p[1], p[2]); };
    const stop = () => { if (raf) cancelAnimationFrame(raf); raf = 0; vel = null; };
    function coast() {
      raf = 0;
      if (!vel) return;
      cam.orbit(vel[0], vel[1]);
      vel = [vel[0] * 0.93, vel[1] * 0.9];
      h.change();
      if (Math.abs(vel[0]) + Math.abs(vel[1]) < 0.0004) vel = null;
      else raf = requestAnimationFrame(coast);
    }
    const begin = () => { if (h.start) h.start(); };

    el.addEventListener("contextmenu", (e) => e.preventDefault());
    el.addEventListener("pointerdown", (e) => {
      if (e.button !== 0 && e.button !== 2) return;
      stop();
      const [x, y] = rel(e);
      pts.set(e.pointerId, [x, y]);
      try { el.setPointerCapture(e.pointerId); } catch (_) { /* not capturable */ }
      if (pts.size === 2) {
        const [a, b] = [...pts.values()];
        pinch = { d: Math.hypot(a[0] - b[0], a[1] - b[1]) || 1, s: cam.s, mx: (a[0] + b[0]) / 2, my: (a[1] + b[1]) / 2 };
        g = null; dragged = true; begin();
        return;
      }
      const pan = e.button === 2 || e.shiftKey || !is3D();
      g = { x, y, lx: x, ly: y, pan, moved: false };
      dragged = false;
    });
    el.addEventListener("pointermove", (e) => {
      if (!pts.has(e.pointerId)) return;
      const [x, y] = rel(e);
      pts.set(e.pointerId, [x, y]);
      if (pinch && pts.size >= 2) {
        const [a, b] = [...pts.values()];
        const d = Math.hypot(a[0] - b[0], a[1] - b[1]), mx = (a[0] + b[0]) / 2, my = (a[1] + b[1]) / 2;
        cam.ox += mx - pinch.mx; cam.oy += my - pinch.my; pinch.mx = mx; pinch.my = my;
        zoomBy((pinch.s * d) / pinch.d / cam.s, mx, my);
        h.change();
        return;
      }
      if (!g) return;
      if (!g.moved) {
        if (Math.hypot(x - g.x, y - g.y) <= 4) return;
        g.moved = true; dragged = true; el.classList.add("dragging"); begin();
        if (!g.pan) pivotCentre();
      }
      const dx = x - g.lx, dy = y - g.ly;
      g.lx = x; g.ly = y;
      if (g.pan) { cam.ox += dx; cam.oy += dy; }
      else { vel = [dx * 0.0065, -dy * 0.005]; cam.orbit(vel[0], vel[1]); lastMove = performance.now(); }
      h.change();
    });
    const lift = (e) => {
      pts.delete(e.pointerId);
      if (pts.size < 2) pinch = null;
      if (pts.size) return;
      el.classList.remove("dragging");
      const flick = g && g.moved && !g.pan && vel && performance.now() - lastMove < 60;
      g = null;
      if (flick) raf = requestAnimationFrame(coast); else vel = null;
    };
    el.addEventListener("pointerup", lift);
    el.addEventListener("pointercancel", lift);
    el.addEventListener("wheel", (e) => {
      if (h.wheel === "ctrl" && !e.ctrlKey) return; // let the page scroll
      e.preventDefault();
      stop(); begin();
      const [x, y] = rel(e);
      zoomBy(Math.exp(-e.deltaY * (e.ctrlKey ? 0.01 : 0.0018)), x, y);
      h.change();
    }, { passive: false });
    el.addEventListener("keydown", (e) => {
      const dir = { ArrowLeft: [-1, 0], ArrowRight: [1, 0], ArrowUp: [0, -1], ArrowDown: [0, 1] }[e.key];
      if (dir) {
        e.preventDefault(); stop(); begin();
        if (is3D() && !e.shiftKey) { pivotCentre(); cam.orbit(dir[0] * 0.12, -dir[1] * 0.08); }
        else { cam.ox -= dir[0] * 60; cam.oy -= dir[1] * 60; }
        h.change();
      } else if (e.key === "+" || e.key === "=" || e.key === "-" || e.key === "_") {
        e.preventDefault(); stop(); begin();
        zoomBy(e.key === "+" || e.key === "=" ? 1.5 : 1 / 1.5, el.clientWidth / 2, el.clientHeight / 2);
        h.change();
      }
    });
    return {
      // A drag or pinch is under way (pages skip hover while it is).
      active: () => !!(g && g.moved) || !!pinch,
      // Was the last press a drag rather than a click? Reading it resets it.
      consumeDrag: () => { const d = dragged; dragged = false; return d; },
      stop,
    };
  }

  // The galactic plane: faint rings and spokes at height 0 around (gx, gy), out to radius r.
  function drawPlane(c, cam, gx, gy, r, rgb = "150,165,200") {
    const a = cam.tilt();
    if (a <= 0) return;
    c.lineWidth = 1;
    for (let i = 1; i <= 4; i++) {
      c.strokeStyle = `rgba(${rgb},${(i === 4 ? 0.13 : 0.07) * a})`;
      c.beginPath();
      for (let k = 0; k <= 96; k++) {
        const t = (k / 96) * 2 * Math.PI, [x, y] = cam.p(gx + Math.cos(t) * r * i / 4, gy + Math.sin(t) * r * i / 4, 0);
        if (k) c.lineTo(x, y); else c.moveTo(x, y);
      }
      c.stroke();
    }
    c.strokeStyle = `rgba(${rgb},${0.05 * a})`;
    c.beginPath();
    for (let k = 0; k < 12; k++) {
      const t = (k / 12) * 2 * Math.PI;
      const [x0, y0] = cam.p(gx + Math.cos(t) * r * 0.25, gy + Math.sin(t) * r * 0.25, 0);
      const [x1, y1] = cam.p(gx + Math.cos(t) * r, gy + Math.sin(t) * r, 0);
      c.moveTo(x0, y0); c.lineTo(x1, y1);
    }
    c.stroke();
  }

  // Height lines: from each point down (or up) to its foot on the galactic plane, brighter above
  // the plane than below it, with a dot at the foot. `items` have x, y, z and screen px, py.
  function drawStalks(c, cam, items, strength = 1) {
    const a = cam.tilt() * strength;
    if (a <= 0) return;
    c.lineWidth = 1;
    for (const above of [true, false]) {
      c.strokeStyle = above ? `rgba(150,175,220,${0.16 * a})` : `rgba(130,130,150,${0.1 * a})`;
      c.fillStyle = above ? `rgba(150,175,220,${0.3 * a})` : `rgba(130,130,150,${0.22 * a})`;
      c.beginPath();
      const feet = [];
      for (const s of items) {
        if (Math.abs(s.z) < 3 || (s.z > 0) !== above) continue;
        const f = cam.p(s.x, s.y, 0);
        c.moveTo(s.px, s.py); c.lineTo(f[0], f[1]);
        feet.push(f);
      }
      c.stroke();
      c.beginPath();
      for (const f of feet) c.rect(f[0] - 0.6, f[1] - 0.6, 1.2, 1.2);
      c.fill();
    }
  }

  window.Galaxy3D = { camera, controls, drawPlane, drawStalks, snap, blend, nearYaw, PITCH_MAX, TILT: 1.0, PERSP: 3.6 };
})();
