// μLottie runtime — mount and playback.
//
// The compiler hands over three things:
//   M — SVG markup with every frame-invariant value already baked in;
//   D — the payload: one integer stream and the strings that could not become
//       integers;
//   B — a sparse array of binder factories, generated to hold exactly the ops
//       this animation uses so the rest is never bundled.
//
// The payload decodes to a single `Int32Array`. Everything in it — bindings,
// layer records, clocks, gates, properties — is a run of integers at an offset,
// and every reference is that offset. mount() parses M once, walks the binding
// section, and resolves each row into a closure. From then on a frame is a flat
// loop over those closures: nothing walks a tree, nothing re-reads the payload,
// and nothing branches on a property's shape.

import { dec } from './vlq.js';
import { H_FR, H_IP, H_OP, H_FLAGS, H_EASINGS, H_TIMELINES, H_GATES, H_SLOTS, H_BIND_GATE, H_BINDINGS, H_LAYERS, H_ASSETS, H_USES, H_REMAPS } from './wire.js';
import { column } from './col.js';
import { INV } from './scale.js';

let seq = 0;

export function mount(M, D, B, container, opt, ext) {
  opt = opt || {};
  // Normalise once — the optional capabilities are read from four places.
  ext = ext || {};
  const S = dec(D);
  //   ext.p — the string pool, when anything in the scene still needs one
  const str = ext.p || [];

  // Two mounts of the same module must not share `<mask>`/gradient ids.
  const sfx = S[H_FLAGS] & 1 ? '-' + seq++ : '';
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
  // The templates are the module's own strings and it closes over them, so
  // nothing here has to find them on the wire.
  if (ext.t) ext.t(svg);

  const fr = S[H_FR] / 1000;
  const ip = S[H_IP] / 1000;
  const op = S[H_OP] / 1000;

  // Easing handles are the one table the hot loop wants as floats: the bezier
  // solver reads all four per non-linear segment. There are only a handful, so
  // they are widened once here rather than divided on every frame.
  const ez = S[H_EASINGS];
  const easings = [];
  if (ez) {
    for (let i = 0, n = S[ez]; i < n; i++) {
      // Not `r`: reachability is resolved on bare names across the whole
      // runtime, so a local `r` reads as a reference to num.js's coordinate
      // formatter and ships it with every module. See the note in num.js.
      const at = ez + 1 + i * 4;
      easings.push([S[at] / 1000, S[at + 1] / 1000, S[at + 2] / 1000, S[at + 3] / 1000]);
    }
  }

  const ctx = { S, str, svg, z: easings, fr, frame: 0, y: column(S, S[H_LAYERS], true) };
  if (ext.x) ctx.expr = ext.x(ctx);

  const bind = S[H_BINDINGS];
  const n = bind ? S[bind] : 0;

  // One flat list of updaters: the document's own bindings, then each precomp
  // instance replaying its asset's bindings with that instance's offsets.
  const U = [];
  const uses = S[H_USES];
  const nUses = uses ? S[uses] : 0;
  // Timeline slot per updater. The slot column covers the document's own
  // bindings and is omitted when they all run on the composition clock — but an
  // instance contributes its own slot regardless, so the array still has to
  // exist, with a zero per document binding to keep it index-aligned with `U`.
  // Without this, a fully-instanced animation ran every precomp on the raw
  // frame and lost its per-instance offsets entirely.
  const S_ = column(S, S[H_SLOTS], true);
  const slots = S_ || (nUses ? new Array(n).fill(0) : null);

  if (n || nUses) {
    const els = svg.querySelectorAll('*');
    // `op::LAYER_TX` (10) and `op::LAYER_OP` (11) are the only ops whose first
    // argument is a record index, and they are the two highest op codes — so
    // `> 9` identifies them. Both index columns ship as first differences, so
    // decoding is a running sum, and it has to stay out of the payload: the
    // stream is module-scoped, and mounting twice would decode twice.
    let e = 0, q = 0, c = bind + 1;
    for (let i = 0; i < n; i++) {
      const len = S[c], code = S[c + 1];
      e += S[c + 2];
      const args = c + 3;
      U.push(B[code](els[e], S, args, ctx, 0, code > 9 ? (q += S[args]) : 0));
      c += 3 + len;
    }
    const assets = S[H_ASSETS];
    for (let u = 0; u < nUses; u++) {
      // [asset, elementBase, recordBase, slotBase, parentSlot, scope]
      const row = uses + 1 + u * 6;
      const a = assets + 1 + S[row] * 5;
      // The expression engine already built this instantiation, records and
      // all — `at.recs` is its own materialized record set, so its keyframe
      // cursors stay separate from every other instance's — and its record
      // properties captured *that* object. Reusing it keeps the two halves
      // from drifting into two `at`s for one instance. With no engine there
      // are no records to find, and `at` is only ever carried: `resolve` looks
      // at it in its expression branch and nowhere else.
      const at = (ctx.byUse && ctx.byUse[u]) || {};
      const ab = S[a + 1];
      const an = ab ? S[ab] : 0;
      const al = column(S, S[a + 2], true);
      // An asset's columns are relative to the asset, so they restart here.
      let ae = 0, aq = 0, ac = ab + 1;
      for (let i = 0; i < an; i++) {
        const len = S[ac], code = S[ac + 1];
        ae += S[ac + 2];
        const args = ac + 3;
        U.push(B[code](els[S[row + 1] + ae], S, args, ctx, at, code > 9 ? (aq += S[args]) : 0));
        ac += 3 + len;
        if (slots) {
          const local = al ? al[i] : 0;
          slots.push(local ? S[row + 3] + local : S[row + 4]);
        }
      }
    }
  }

  // Precomp clocks. Each row is `[parentSlot, offset, loopIp, loopOp]`; slot 0
  // is the composition clock, so slot i+1 is described by row i.
  const tl = S[H_TIMELINES];
  const nTl = tl ? S[tl] : 0;
  // Both tables carry the scale their frame numbers were written at.
  const tScale = nTl ? INV[S[tl + 1]] : 1;
  const tRows = tl + 2;
  const T = nTl ? new Float64Array(nTl + 1) : null;
  // A precomp with time remap takes its clock from a property of the parent's
  // time rather than from `parent - offset`. The remap column is parallel to
  // the timeline table, with 0 where a slot has no remap.
  const rmc = S[H_REMAPS];
  const rm = rmc && ext.r
    ? Array.from({ length: S[rmc] }, (_, i) => (S[rmc + 1 + i] ? ext.r(S[rmc + 1 + i], ctx) : 0))
    : null;

  // Visibility gates: a binding that lives inside a layer which is off at the
  // current frame is skipped outright, so a scene of staggered layers costs
  // only what is actually on screen.
  const gt = S[H_GATES];
  const nGates = gt ? S[gt] : 0;
  const gScale = nGates ? INV[S[gt + 1]] : 1;
  const gRows = gt + 2;
  // Gated on the per-binding column, not on the gate table: a table with no
  // binding pointing into it gates nothing, and the frame loop would index a
  // column that was never written.
  const gateOf = column(S, S[H_BIND_GATE], false);
  const gateOn = nGates && gateOf ? new Uint8Array(nGates) : null;

  const span = op - ip || 1;

  function apply(f) {
    ctx.frame = f;
    if (T) {
      T[0] = f;
      for (let i = 0; i < nTl; i++) {
        const e = tRows + i * 4;
        // Named `remap`, not `r`: reachability is resolved on bare names
        // across the whole runtime, and a local `r` reads as a reference to
        // num.js's coordinate formatter — which then ships with every module.
        const remap = rm && rm[i];
        if (remap) {
          // Lottie stores the remap in seconds; the timeline is in frames.
          T[i + 1] = remap(T[S[e]]) * fr;
          continue;
        }
        let x = T[S[e]] - S[e + 1] * tScale;
        const lo = S[e + 2] * tScale, hi = S[e + 3] * tScale;
        const p = hi - lo;
        if (p > 0 && x >= hi) x = lo + ((x - lo) % p);
        T[i + 1] = x;
      }
    }
    const total = U.length;
    if (gateOn) {
      for (let i = 0; i < nGates; i++) {
        const g = gRows + i * 2;
        gateOn[i] = f >= S[g] * gScale && f < S[g + 1] * gScale ? 1 : 0;
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

  const api = {
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
      return api;
    },
    pause() { halt(); return api; },
    stop() { halt(); frame = ip; apply(ip); return api; },
    seek(f) { frame = f; apply(f); return api; },
    goToFrame(f) { halt(); frame = f; apply(f); return api; },
    goToAndStop(f) { return api.goToFrame(f); },
    goToAndPlay(f) { frame = f; apply(f); return api.play(); },
    on(name, fn) { (subs[name] || (subs[name] = [])).push(fn); return api; },
    off(name, fn) {
      const l = subs[name];
      if (l) { const i = l.indexOf(fn); if (i >= 0) l.splice(i, 1); }
      return api;
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
  if (live && auto !== false && (auto === true || !reduced())) api.play();
  return api;
}


function reduced() {
  return typeof matchMedia === 'function'
    && matchMedia('(prefers-reduced-motion: reduce)').matches;
}
