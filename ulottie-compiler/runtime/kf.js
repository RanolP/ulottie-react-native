// Property resolution and keyframe interpolation.
//
// `resolve` runs once per property at mount and returns the cheapest closure
// that can produce that property's value. Everything the compiler could not
// decide statically is decided here, once — the per-frame path is a direct
// call with no branching on the property's shape.

import { EASE } from './ease.js';
import { spatial } from './spatial.js';
import { lerpPath } from './kfpath.js';

/** Static scalar. */
const konst = (v) => () => v;

/**
 * Turn a wire property into an evaluator.
 *
 * number → static scalar, array → static vector, `.t` → keyframed,
 * `.x` → expression, anything else → static path.
 */
export function resolve(p, ctx, at) {
  if (typeof p === 'number') return konst(p);
  if (p === null || p === undefined) return konst(0);
  if (Array.isArray(p)) return konst(p);
  // `x` is checked before `t`: an expression whose value source is keyframed
  // carries both, and the expression is what produces the value — the
  // keyframes are only what it reads through `value`.
  if (p.x !== undefined) return ctx.expr ? ctx.expr(p, at) : konst(fallbackOf(p, ctx));
  if (p.t) return keyframed(p, ctx);
  return konst(p); // static path {v,i,o,c}
}

function fallbackOf(p, ctx) {
  const f = p.f;
  if (f === undefined) return 0;
  if (typeof f === 'number' || Array.isArray(f)) return f;
  // Keyframed fallback with no expression engine: sample it directly.
  const ev = keyframed(f, ctx);
  return ev(0);
}

/**
 * Build an interpolator for a keyframed property.
 *
 * Values are columnar: `v` is flat with stride `d`. Vector results are written
 * into a scratch array owned by this closure, so a steady-state frame performs
 * zero allocations.
 */
function keyframed(p, ctx) {
  const t = p.t;
  const n = t.length;
  const kind = p.k || 0;          // 0 scalar, 1 vector, 2 path
  const d = p.d || 1;
  const ez = p.z;                 // per-segment easing index
  const hd = p.h;                 // per-segment hold flags
  const easings = ctx.z;
  const to = p.to, ti = p.ti;

  // Segment cursor. Playback is overwhelmingly monotonic, so checking the
  // previous segment first turns the search into two comparisons.
  let cur = 0;

  const seek = (f) => {
    if (f >= t[cur] && f <= t[cur + 1]) return cur;
    let lo = 0, hi = n - 1;
    while (hi - lo > 1) {
      const m = (lo + hi) >> 1;
      if (t[m] <= f) lo = m; else hi = m;
    }
    cur = lo;
    return lo;
  };

  const ease = (i, u) => {
    if (!ez) return u;
    const e = ez[i];
    return e === 0 ? u : EASE(easings[e], u);
  };

  if (kind === 2) {
    const paths = p.v;
    const ends = p.e;
    return (f) => {
      if (f <= t[0]) return paths[0];
      if (f >= t[n - 1]) return paths[n - 1];
      const i = seek(f);
      const span = t[i + 1] - t[i];
      if (span === 0) return paths[i + 1];
      if (hd && hd[i]) return paths[i];
      const u = ease(i, (f - t[i]) / span);
      return lerpPath(paths[i], ends ? ends[i] : paths[i + 1], u);
    };
  }

  const v = p.v;
  const ends = p.e;

  if (kind === 0) {
    return (f) => {
      if (f <= t[0]) return v[0];
      if (f >= t[n - 1]) return v[n - 1];
      const i = seek(f);
      const span = t[i + 1] - t[i];
      if (span === 0) return v[i + 1];
      if (hd && hd[i]) return v[i];
      const u = ease(i, (f - t[i]) / span);
      const a = v[i];
      const b = ends ? ends[i] : v[i + 1];
      return a + (b - a) * u;
    };
  }

  const out = new Array(d);
  // Arc-length tables for spatial segments, built on first visit and kept.
  const tabs = to ? [] : null;
  const slice = (base) => {
    for (let k = 0; k < d; k++) out[k] = v[base + k];
    return out;
  };
  return (f) => {
    if (f <= t[0]) return slice(0);
    if (f >= t[n - 1]) return slice((n - 1) * d);
    const i = seek(f);
    const span = t[i + 1] - t[i];
    if (span === 0) return slice((i + 1) * d);
    if (hd && hd[i]) return slice(i * d);
    const u = ease(i, (f - t[i]) / span);
    const ai = i * d;
    const bi = ends ? i * d : (i + 1) * d;
    const bv = ends || v;
    if (to) {
      const so = i * d;
      if (to[so] || to[so + 1] || ti[so] || ti[so + 1]) {
        return spatial(v, ai, bv, bi, to, ti, so, d, u, out, tabs, i);
      }
    }
    for (let k = 0; k < d; k++) {
      const a = v[ai + k];
      out[k] = a + (bv[bi + k] - a) * u;
    }
    return out;
  };
}
