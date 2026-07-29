// μLottie runtime — mount.
//
// The compiler hands over three things:
//   M — SVG markup with every frame-invariant value already baked in;
//   D — the payload, one integer stream;
//   P — bind: a function the compiler wrote that binds one batch per op and
//       returns their state records;
//   A — apply: the matching function that calls one op per batch, by name.
//
// There is no binder table and no op code on the wire. Which op a binding is
// was only ever a compile-time fact, so it is spent at compile time: the module
// says `oTranslate(x, S[0])` where it used to say `B[code](...)`, and the shaker
// sees a direct reference instead of a table entry.
//
// Neither emitted function closes over anything, and no op returns a callback —
// state is data, and every runtime primitive is a direct call. The one closure
// in a mounted animation is `apply` below, which is what `player` is handed.
//
// What mount does is everything that is not the animation — parse the markup,
// decode the stream, resolve the easing, clock and gate tables — and then hand
// the program a context. A frame is `apply`: update the clocks, update the
// gates, and run each program once.

import { dec } from './vlq.js';
import { H_FR, H_IP, H_OP, H_FLAGS, H_EASINGS, H_TIMELINES, H_GATES, H_PROGRAM, H_LAYERS, H_ASSETS, H_USES, H_REMAPS, A_STRIDE, A_PROGRAM, U_STRIDE, U_EL_BASE, U_SLOT_BASE, U_PARENT } from './wire.js';
import { column } from './col.js';
import { INV } from './scale.js';
import { player } from './play.js';

let seq = 0;

export function mount(M, D, P, A, container, opt, ext) {
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
  //   ext.a / ext.b — the asset programs, when precomps are instanced
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

  // Precomp clocks. Each row is `[parentSlot, offset, loopIp, loopOp]`; slot 0
  // is the composition clock, so slot i+1 is described by row i — which is why
  // `T` is one longer than the table and why a binding with no clock of its own
  // reads `T[0]` rather than branching on whether it has one.
  const tl = S[H_TIMELINES];
  const nTl = tl ? S[tl] : 0;
  // Both tables carry the scale their frame numbers were written at.
  const tScale = nTl ? INV[S[tl + 1]] : 1;
  const tRows = tl + 2;
  const T = new Float64Array(nTl + 1);

  // Visibility gates: a binding that lives inside a layer which is off at the
  // current frame is skipped outright, so a scene of staggered layers costs
  // only what is actually on screen. Gate 0 is pinned on, which is what lets
  // an ungated binding read `ON[0]` instead of testing whether it has a gate.
  const gt = S[H_GATES];
  const nGates = gt ? S[gt] : 0;
  const gScale = nGates ? INV[S[gt + 1]] : 1;
  const gRows = gt + 2;
  const ON = new Uint8Array(nGates + 1);
  ON[0] = 1;

  // Declared in one place and never extended: every op reads `x.S` and `x.z`
  // per property, and a field added after the first frame would change this
  // object's shape underneath them.
  const ctx = {
    S, str, svg, els: svg.querySelectorAll('*'), z: easings,
    fr, frame: 0, T, ON, sp: null, expr: null,
    // Declared either way — the ops read `x.S` off this object on every
    // property, and two mounts on one page whose contexts differ in shape would
    // make every one of those reads polymorphic. Only the engine reads it, so
    // the column is decoded only when there is one.
    y: ext.x ? column(S, S[H_LAYERS]) : null,
  };
  if (ext.x) ctx.expr = ext.x(ctx);

  // A precomp with time remap takes its clock from a property of the parent's
  // time rather than from `parent - offset`. The remap column is parallel to
  // the timeline table, with 0 where a slot has no remap.
  const rmc = S[H_REMAPS];
  const rm = rmc && ext.r
    ? Array.from({ length: S[rmc] }, (_, i) => (S[rmc + 1 + i] ? ext.r(S[rmc + 1 + i], ctx) : 0))
    : null;

  // One program for the document, then one per precomp instantiation replaying
  // its asset's with that instance's bases applied. `St` holds each one's bound
  // state and `Ap` the function that runs it — data and code, kept apart.
  const St = [], Ap = [];
  const prog = S[H_PROGRAM];
  if (prog) {
    St.push(P(ctx, S.subarray(prog + 1, prog + 1 + S[prog]), 0, 0, 0, 0));
    Ap.push(A);
  }
  const uses = S[H_USES];
  const assets = S[H_ASSETS];
  for (let u = 0, nu = uses ? S[uses] : 0; u < nu; u++) {
    const row = uses + 1 + u * U_STRIDE;
    const ap = S[assets + 1 + S[row] * A_STRIDE + A_PROGRAM];
    // The expression engine already built this instantiation, records and all
    // — `at.recs` is its own materialized record set, so its keyframe cursors
    // stay separate from every other instance's. Reusing it keeps the two
    // halves from drifting into two `at`s for one instance.
    const at = (ctx.byUse && ctx.byUse[u]) || 0;
    St.push(ext.a[S[row]](ctx, S.subarray(ap + 1, ap + 1 + S[ap]),
      S[row + U_EL_BASE], S[row + U_SLOT_BASE], S[row + U_PARENT], at));
    Ap.push(ext.b[S[row]]);
  }
  const nA = Ap.length;

  function apply(f) {
    ctx.frame = f;
    T[0] = f;
    for (let i = 0; i < nTl; i++) {
      const e = tRows + i * 4;
      // Named `remap`, not `r`: reachability is resolved on bare names across
      // the whole runtime, and a local `r` reads as a reference to num.js's
      // coordinate formatter — which then ships with every module.
      const remap = rm && rm[i];
      if (remap) {
        // Lottie stores the remap in seconds; the timeline is in frames.
        T[i + 1] = remap(T[S[e]]) * fr;
        continue;
      }
      let v = T[S[e]] - S[e + 1] * tScale;
      const lo = S[e + 2] * tScale, hi = S[e + 3] * tScale;
      const span = hi - lo;
      if (span > 0 && v >= hi) v = lo + ((v - lo) % span);
      T[i + 1] = v;
    }
    for (let i = 0; i < nGates; i++) {
      const g = gRows + i * 2;
      ON[i + 1] = f >= S[g] * gScale && f < S[g + 1] * gScale ? 1 : 0;
    }
    for (let i = 0; i < nA; i++) Ap[i](ctx, St[i]);
  }

  return player(container, svg, html, apply, fr, ip, op, opt);
}
