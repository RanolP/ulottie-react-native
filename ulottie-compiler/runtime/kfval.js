// Keyframe interpolation over plain arrays.
//
// Generated code unrolls a property's segments into an if-chain, which is the
// fastest thing there is and costs one branch per segment. That trade stops
// paying somewhere around a handful of segments: `ripple` has 230 bindings and
// unrolling all of them produced a 255 KB module against the interpreter's 52.
//
// So above a threshold the compiler emits the columns as literals and calls
// this instead — the same shape `kf.js` reads out of the stream, minus the
// stream. Everything here is a plain array, which is what makes it usable from
// generated code at all.

import { EASE } from './ease.js';
import { lerpPath } from './kfpath.js';
import { spBuild, spSample } from './spatial.js';

/** Keyframe `i` of `k`, written into `out` for vector properties. */
function pick(k, i, out) {
  const d = k.d;
  if (k.kind === 2 || d === 1) return k.v[i];
  for (let c = 0; c < d; c++) out[c] = k.v[i * d + c];
  return out;
}

/**
 * Sample `k` at frame `f`.
 *
 * `k` is `{ t, v, d, kind, z, h, to, ti }` — times, values, dimension, kind,
 * per-segment easing handles (or 0 for linear), per-segment hold flags, and the
 * spatial tangent columns. `out` is the caller's scratch, so a steady-state
 * frame allocates nothing.
 */
export function kfEval(k, f, out) {
  const t = k.t;
  const n = t.length;
  if (f <= t[0]) return pick(k, 0, out);
  if (f >= t[n - 1]) return pick(k, n - 1, out);

  // Playback is overwhelmingly monotonic, so the previous segment is checked
  // before the search — two comparisons in the common case.
  let i = k.c || 0;
  if (!(f >= t[i] && f <= t[i + 1])) {
    let lo = 0, hi = n - 1;
    while (hi - lo > 1) {
      const m = (lo + hi) >> 1;
      if (t[m] <= f) lo = m; else hi = m;
    }
    i = lo;
  }
  k.c = i;

  const span = t[i + 1] - t[i];
  if (span === 0) return pick(k, i + 1, out);
  if (k.h && k.h[i]) return pick(k, i, out);

  let u = (f - t[i]) / span;
  const e = k.z && k.z[i];
  if (e) u = EASE(e, u);

  const v = k.v, d = k.d;
  if (k.kind === 2) return lerpPath(v[i], v[i + 1], u);
  if (d === 1) return v[i] + (v[i + 1] - v[i]) * u;
  const a = i * d, b = (i + 1) * d;
  // Spatial tangents bend the segment and the result is paced by arc length,
  // not interpolated straight. `anim`'s unrolled form has done this all along;
  // a property large enough to arrive here as columns used to lose the
  // tangents silently and travel in a straight line instead — which on
  // `starfish` moved a limb by 10 px at mid-animation, since two of its
  // position keyframes are equal and the whole excursion between them *is* the
  // tangent. Tables are built on a segment's first visit and kept, the same
  // deal the interpreter makes.
  if (k.to) {
    const tabs = k.sp || (k.sp = []);
    let tab = tabs[i];
    if (tab === undefined) {
      const to = k.to, ti = k.ti;
      let bent = false;
      for (let c = 0; c < d; c++) if (to[a + c] || ti[a + c]) { bent = true; break; }
      tab = tabs[i] = bent
        ? spBuild(v.slice(a, b), v.slice(b, b + d), to.slice(a, b), ti.slice(a, b), d)
        : null;
    }
    if (tab) return spSample(tab, u, out);
  }
  for (let c = 0; c < d; c++) {
    const x = v[a + c];
    out[c] = x + (v[b + c] - x) * u;
  }
  return out;
}
