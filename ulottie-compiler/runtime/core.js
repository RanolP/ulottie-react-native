// μLottie runtime — mount and playback.
//
// The compiler hands over three things:
//   M — SVG markup with every frame-invariant value already baked in;
//   D — a table of what actually varies;
//   B — a sparse array of binder factories, generated to hold exactly the ops
//       this animation uses so the rest is never bundled.
//
// mount() parses M once, resolves each entry of D.b into a closure, and from
// then on a frame is a flat loop over those closures. Nothing walks a tree,
// nothing re-reads the payload, and nothing branches on a property's shape.

let seq = 0;

export function mount(M, D, B, container, opt, ext) {
  opt = opt || {};
  // Normalise once — the optional capabilities are read from four places.
  ext = ext || {};
  // Two mounts of the same module must not share `<mask>`/gradient ids.
  const sfx = D.u ? '-' + seq++ : '';
  // Extracted markup: M is the bare `<svg>` shell and the elements come from a
  // sprite, so the suffix is applied to the built DOM instead of the string.
  const src = ext.s;
  const html = sfx && !src ? M.split('--u').join(sfx) : M;

  let svg;
  if (opt.hydrate) {
    svg = container.querySelector('svg');
  } else {
    container.innerHTML = html;
    svg = container.firstElementChild;
    if (src) src(svg, sfx);
  }

  // Optional capabilities arrive as functions rather than imports: core.js
  // naming them would pull them into every animation through the module graph.
  //   ext.s — fill the shell from an external sprite (applied above)
  //   ext.t — expand factored-out subtrees, before anything is indexed
  //   ext.x — build the expression engine
  //   ext.r — resolve a time-remap property to an evaluator
  if (ext.t) ext.t(svg, D.m);

  const binds = D.b || [];
  const n = binds.length;
  const ctx = { D, svg, z: D.z, frame: 0 };
  if (ext.x) ctx.expr = ext.x(ctx);

  // The two index columns ship as first differences — see `Deltas` in
  // scene/mod.rs. Decoding is a running sum, and it has to stay out of `D`:
  // the payload is module-scoped, so mounting twice would decode twice.
  //
  // `op::LAYER_TX` (10) and `op::LAYER_OP` (11) are the only ops whose first
  // argument is an index, and they are the two highest op codes — so `> 9`
  // identifies them. A Rust test pins that invariant, because adding a higher
  // op would silently misdecode here.

  // One flat list of updaters: the document's own bindings, then each precomp
  // instance replaying its asset's bindings with that instance's offsets.
  const U = [];
  const insts = D.n;
  // Timeline slot per updater. `D.l` covers the document's own bindings and is
  // omitted when they all run on the composition clock — but an instance
  // contributes its own slot regardless, so the array still has to exist, with
  // a zero per document binding to keep it index-aligned with `U`. Without
  // this, a fully-instanced animation ran every precomp on the raw frame and
  // lost its per-instance offsets entirely.
  let run = 0;
  const S = D.l ? D.l.map((v) => (run += v)) : insts ? new Array(n).fill(0) : null;
  if (n || insts) {
    const els = svg.querySelectorAll('*');
    let e = 0, q = 0;
    for (let i = 0; i < n; i++) {
      const b = binds[i];
      e += b[1];
      U.push(B[b[0]](els[e], b, ctx, 0, b[0] > 9 ? (q += b[2]) : 0));
    }
    // [asset, elementBase, recordBase, slotBase, parentSlot, scope]
    for (const u of insts || []) {
      const a = D.q[u[0]];
      const at = { asset: u[0], recBase: u[2] };
      const bs = a.b || [];
      // An asset's columns are relative to the asset, so they restart here.
      let ae = 0, aq = 0, al = 0;
      for (let i = 0; i < bs.length; i++) {
        const b = bs[i];
        ae += b[1];
        U.push(B[b[0]](els[u[1] + ae], b, ctx, at, b[0] > 9 ? (aq += b[2]) : 0));
        if (S) {
          const local = a.l ? (al += a.l[i]) : 0;
          S.push(local ? u[3] + local : u[4]);
        }
      }
    }
  }

  // Precomp clocks. `tl[i] = [parentSlot, offset, loopIp, loopOp]`; slot 0 is
  // the composition clock, so slot i+1 is described by tl[i].
  const tl = D.t;
  const slots = S;
  const T = tl ? new Float64Array(tl.length + 1) : null;
  // A precomp with time remap takes its clock from a property of the parent's
  // time rather than from `parent - offset`. `D.rm` is parallel to `D.t`, with
  // 0 where a slot has no remap.
  const rm = D.rm && ext.r && D.rm.map((p) => (p ? ext.r(p, ctx) : 0));

  // Visibility gates: a binding that lives inside a layer which is off at the
  // current frame is skipped outright, so a scene of staggered layers costs
  // only what is actually on screen.
  const gates = D.k;
  const gateOf = D.g;
  const gateOn = gates ? new Uint8Array(gates.length) : null;

  const fr = D.f;
  const ip = D.i || 0;
  const op = D.o;
  const span = op - ip || 1;

  function apply(f) {
    ctx.frame = f;
    if (T) {
      T[0] = f;
      for (let i = 0; i < tl.length; i++) {
        const e = tl[i];
        // Named `remap`, not `r`: reachability is resolved on bare names
        // across the whole runtime, and a local `r` reads as a reference to
        // num.js's coordinate formatter — which then ships with every module.
        const remap = rm && rm[i];
        if (remap) {
          // Lottie stores the remap in seconds; the timeline is in frames.
          T[i + 1] = remap(T[e[0]]) * fr;
          continue;
        }
        let x = T[e[0]] - e[1];
        const p = e[3] - e[2];
        if (p > 0 && x >= e[3]) x = e[2] + ((x - e[2]) % p);
        T[i + 1] = x;
      }
    }
    const total = U.length;
    if (gateOn) {
      for (let i = 0; i < gates.length; i++) {
        const g = gates[i];
        gateOn[i] = f >= g[0] && f < g[1] ? 1 : 0;
      }
      for (let i = 0; i < total; i++) {
        const g = i < n ? gateOf[i] : 0;
        if (g && !gateOn[g - 1]) continue;
        U[i](slots ? T[slots[i]] : f);
      }
    } else if (slots) {
      for (let i = 0; i < total; i++) U[i](T[slots[i]]);
    } else {
      for (let i = 0; i < total; i++) U[i](f);
    }
  }

  // Is there anything to animate? `n` counts only the document's OWN bindings,
  // and a fully-instanced precomp animation has none — every binding belongs to
  // an asset replayed per instance. Gating playback on `n` left those mounted
  // at frame 0 and frozen.
  const live = U.length;

  let raf = 0;
  let prev = 0;
  let frame = ip;
  let dir = 1;
  let rate = 1;
  let loops = 0;
  let loop = opt.loop === undefined ? true : opt.loop;
  const subs = {};

  const fire = (name, arg) => {
    const l = subs[name];
    if (l) for (let i = 0; i < l.length; i++) l[i](arg);
  };

  function tick(ts) {
    raf = requestAnimationFrame(tick);
    const dt = prev ? (ts - prev) / 1000 : 0;
    prev = ts;
    frame += dt * fr * rate * dir;
    if (frame >= op || frame < ip) {
      if (loop === true || loops + 1 < loop) {
        loops++;
        frame = frame >= op
          ? ip + ((frame - ip) % span)
          : op - ((ip - frame) % span);
        fire('loop', loops);
      } else {
        frame = dir > 0 ? op - 1e-4 : ip;
        halt();
        apply(frame);
        fire('complete');
        return;
      }
    }
    apply(frame);
    fire('frame', frame);
  }

  function halt() {
    if (raf) cancelAnimationFrame(raf);
    raf = 0;
    prev = 0;
  }

  const player = {
    svg,
    markup: html,
    totalFrames: span,
    frameRate: fr,
    duration: span / fr,
    get currentFrame() { return frame; },
    get isPlaying() { return !!raf; },
    get loop() { return loop; },
    set loop(v) { loop = v; loops = 0; },
    get speed() { return rate; },
    set speed(v) { rate = v; },
    get direction() { return dir; },
    set direction(v) { dir = v < 0 ? -1 : 1; },
    play() {
      if (!raf && live) { prev = 0; raf = requestAnimationFrame(tick); }
      return player;
    },
    pause() { halt(); return player; },
    stop() { halt(); frame = ip; apply(ip); return player; },
    seek(f) { frame = f; apply(f); return player; },
    goToFrame(f) { halt(); frame = f; apply(f); return player; },
    goToAndStop(f) { return player.goToFrame(f); },
    goToAndPlay(f) { frame = f; apply(f); return player.play(); },
    on(name, fn) { (subs[name] || (subs[name] = [])).push(fn); return player; },
    off(name, fn) {
      const l = subs[name];
      if (l) { const i = l.indexOf(fn); if (i >= 0) l.splice(i, 1); }
      return player;
    },
    destroy() {
      halt();
      if (!opt.hydrate) container.innerHTML = '';
    },
  };

  apply(ip);
  // `autoplay` defaults to 'auto': play, unless the OS asks for reduced
  // motion. `true` forces playback, `false` mounts paused.
  const auto = opt.autoplay;
  if (live && auto !== false && (auto === true || !reduced())) player.play();
  return player;
}

function reduced() {
  return typeof matchMedia === 'function'
    && matchMedia('(prefers-reduced-motion: reduce)').matches;
}
