// Property evaluation, read straight off the stream.
//
// A property is a run of integers at an offset in the single `Int32Array` the
// payload decodes to. These functions read it at the frame they are asked for:
// there is no closure per property, no object, and no copy of the keyframe
// data. The only state a property needs is its segment cursor, which lives in
// an `Int32Array` the caller indexes by binding.
//
// **One function per result shape, not one closure per property shape.** That
// is what makes an op's frame loop monomorphic: `pv` is a single call target,
// so V8 can inline it into the loop, where twelve closure shapes reached
// through one `U[i](f)` could only ever be a generic call.
//
// The three shapes are the three things a binding can consume — a number, a
// vector written into the caller's scratch, and a path object.

import { EASE } from './ease.js';
import { spSeg, spSample } from './spatial.js';
import { lerpPath } from './kfpath.js';
import { INV, P10 } from './scale.js';

/// Property tags, in the low three bits of a property's first word. Kept in
/// sync with `scene/flat.rs`. `T_ANIM` is what the readers fall through to.
export const T_SCALAR = 0;
export const T_VECTOR = 1;
export const T_PATH = 2;
export const T_ANIM = 3;
export const T_EXPR = 4;

/// An `Anim`'s value kind, in bits 11–12. Path keyframes hold offsets to pooled
/// path properties where the others hold numbers.
export const T_KIND_PATH = 2;

/// `Anim` header flags, above the tag and the two shifts.
const F_END = 1;
const F_EASE = 2;
const F_HOLD = 4;
const F_SPATIAL = 8;

/**
 * The segment containing `ft`, remembered per binding.
 *
 * Playback is overwhelmingly monotonic, so checking the cursor's own segment
 * first turns the search into two comparisons. The cursor is only ever a hint,
 * which is why two bindings sharing a hash-consed property may share one.
 */
function pseg(S, t, n, ft, c, i) {
  const k = c[i];
  if (ft >= S[t + k] && ft <= S[t + k + 1]) return k;
  let lo = 0, hi = n - 1;
  while (hi - lo > 1) {
    const m = (lo + hi) >> 1;
    if (S[t + m] <= ft) lo = m; else hi = m;
  }
  c[i] = lo;
  return lo;
}

/** Segment easing. Handle 0 is linear and never reaches the solver. */
function pease(z, S, ez, k, u) {
  if (!ez) return u;
  const h = S[ez + k];
  return h === 0 ? u : EASE(z[h], u);
}

// The optional columns follow the values in a fixed order — ends, easing,
// holds, then the two spatial tangent columns — so each reader walks a running
// offset through the ones it needs. Three functions that each recomputed the
// whole staircase from the header meant the shared prefix was derived two or
// three times per property per frame, and adding a column meant editing all
// three in step.

/**
 * A scalar property at frame `f`.
 *
 * `off` of zero means the property is absent, which is unambiguous because
 * offset zero is the header and no property can live there.
 */
export function pv(x, off, f, c, i) {
  if (!off) return 0;
  const S = x.S;
  const head = S[off];
  const tag = head & 7;
  if (tag === T_SCALAR) return S[off + 1] * INV[(head >> 3) & 3];
  // A one-component vector in a scalar slot. The planner classifies by the
  // dimension it asked for, not by what Lottie stored, so a scalar property
  // written as `[x]` arrives here. The closure form this replaced handed the
  // array back and let JS coerce it; reading component zero is the same value
  // and does not depend on that coercion.
  if (tag === T_VECTOR) return S[off + 2] * INV[(head >> 3) & 3];
  // An expression with no engine to run it is its own value source. With an
  // engine, the whole column is resolved ahead of the loop and never lands here.
  if (tag === T_EXPR) return pv(x, S[off + 2], f, c, i);
  const n = S[off + 1];
  const iv = INV[(head >> 5) & 3];
  const t = off + 2;
  const v = t + n;
  const ft = f * P10[(head >> 3) & 3];
  if (ft <= S[t]) return S[v] * iv;
  if (ft >= S[t + n - 1]) return S[v + n - 1] * iv;
  const k = pseg(S, t, n, ft, c, i);
  const span = S[t + k + 1] - S[t + k];
  if (span === 0) return S[v + k + 1] * iv;
  const fl = (head >> 7) & 15;
  let nx = v + n + (fl & F_END ? n : 0);
  const ez = fl & F_EASE ? nx : 0;
  if (ez) nx += n - 1;
  if (fl & F_HOLD && S[nx + k]) return S[v + k] * iv;
  const u = pease(x.z, S, ez, k, (ft - S[t + k]) / span);
  const a = S[v + k];
  const b = fl & F_END ? S[v + n + k] : S[v + k + 1];
  return (a + (b - a) * u) * iv;
}

/**
 * A vector property at frame `f`, written into `out`.
 *
 * `out` is the column's scratch, so a steady-state frame allocates nothing. One
 * per column is enough because a binding never holds two values from the same
 * column at once — except for paint, where `css` asks the buffer itself whether
 * the colour carries an alpha channel and so needs one sized per binding. See
 * [`vscratch`].
 */
export function pvv(x, off, f, c, i, out) {
  const S = x.S;
  if (!off) {
    for (let k = 0; k < out.length; k++) out[k] = 0;
    return out;
  }
  const head = S[off];
  const tag = head & 7;
  if (tag === T_VECTOR) {
    const iv = INV[(head >> 3) & 3];
    for (let k = 0, m = S[off + 1]; k < m; k++) out[k] = S[off + 2 + k] * iv;
    return out;
  }
  if (tag === T_SCALAR) {
    out[0] = S[off + 1] * INV[(head >> 3) & 3];
    return out;
  }
  if (tag === T_EXPR) return pvv(x, S[off + 2], f, c, i, out);
  const n = S[off + 1];
  const d = ((head >> 13) & 3) + 1;
  const iv = INV[(head >> 5) & 3];
  const t = off + 2;
  const v = t + n;
  const vlen = n * d;
  const ft = f * P10[(head >> 3) & 3];
  if (ft <= S[t]) return pslice(S, v, d, iv, out);
  if (ft >= S[t + n - 1]) return pslice(S, v + (n - 1) * d, d, iv, out);
  const k = pseg(S, t, n, ft, c, i);
  const span = S[t + k + 1] - S[t + k];
  if (span === 0) return pslice(S, v + (k + 1) * d, d, iv, out);
  const fl = (head >> 7) & 15;
  let nx = v + vlen + (fl & F_END ? vlen : 0);
  const ez = fl & F_EASE ? nx : 0;
  if (ez) nx += n - 1;
  if (fl & F_HOLD) {
    if (S[nx + k]) return pslice(S, v + k * d, d, iv, out);
    nx += n - 1;
  }
  const u = pease(x.z, S, ez, k, (ft - S[t + k]) / span);
  const ai = v + k * d;
  const bi = fl & F_END ? v + vlen + k * d : v + (k + 1) * d;
  if (fl & F_SPATIAL) {
    const to = nx;
    // Both tangent columns are per *segment*, so each is `(n-1)*d` — one
    // keyframe shorter than the values. Reading `ti` at `n*d` puts every
    // in-tangent one keyframe late, which bends a motion path by a couple of
    // percent: visible to the geometry check, invisible to the pixel diff.
    const so = to + k * d, si = to + (n - 1) * d + k * d;
    if (S[so] || S[so + 1] || S[si] || S[si + 1]) {
      // Arc-length tables are built on a segment's first visit and kept. Keyed
      // by the tangent column's own offset, which names one (property,
      // segment) uniquely — so two bindings sharing a hash-consed property
      // share the table, which depends on nothing else.
      const sp = x.sp || (x.sp = new Map());
      let tab = sp.get(so);
      if (tab === undefined) sp.set(so, tab = spSeg(S, ai, bi, so, si, d, iv));
      return spSample(tab, u, out);
    }
  }
  for (let q = 0; q < d; q++) {
    const a = S[ai + q];
    out[q] = (a + (S[bi + q] - a) * u) * iv;
  }
  return out;
}

/** One keyframe's components, descaled into `out`. */
function pslice(S, base, d, iv, out) {
  for (let k = 0; k < d; k++) out[k] = S[base + k] * iv;
  return out;
}

/**
 * A geometry property at frame `f`.
 *
 * Path objects are the one place where an object is the working representation
 * rather than the wire one — `pathD`, `trimTable` and `lerpPath` all walk one —
 * so they are materialized once into `m`, the caller's per-binding cache, and
 * never again.
 */
export function pvp(x, off, f, c, i, m) {
  if (!off) return null;
  const S = x.S;
  const head = S[off];
  const tag = head & 7;
  if (tag === T_PATH) return m[i] || (m[i] = mkPath(S, off));
  if (tag === T_EXPR) return pvp(x, S[off + 2], f, c, i, m);
  const n = S[off + 1];
  const t = off + 2;
  const v = t + n;
  const fl = (head >> 7) & 15;
  // The keyframes hold offsets to pooled path properties. They are constant, so
  // the whole column is materialized on this binding's first frame — end values
  // included, which is why the cache is `2n` long when the property carries them.
  let keys = m[i];
  if (keys === undefined) {
    keys = m[i] = new Array(fl & F_END ? n * 2 : n);
    for (let k = 0; k < keys.length; k++) keys[k] = mkPath(S, S[v + k]);
  }
  const ft = f * P10[(head >> 3) & 3];
  if (ft <= S[t]) return keys[0];
  if (ft >= S[t + n - 1]) return keys[n - 1];
  const k = pseg(S, t, n, ft, c, i);
  const span = S[t + k + 1] - S[t + k];
  if (span === 0) return keys[k + 1];
  let nx = v + n + (fl & F_END ? n : 0);
  const ez = fl & F_EASE ? nx : 0;
  if (ez) nx += n - 1;
  if (fl & F_HOLD && S[nx + k]) return keys[k];
  const u = pease(x.z, S, ez, k, (ft - S[t + k]) / span);
  return lerpPath(keys[k], fl & F_END ? keys[n + k] : keys[k + 1], u);
}

/**
 * `[tag|shift<<3|closed<<5|curved<<6, points, x,y…, (i…), (o…)]` → `{v,i,o,c}`.
 */
export function mkPath(S, off) {
  const head = S[off];
  const iv = INV[(head >> 3) & 3];
  const n = S[off + 1] * 2;
  const base = off + 2;
  const v = new Array(n);
  for (let k = 0; k < n; k++) v[k] = S[base + k] * iv;
  let i = null, o = null;
  if (head & 64) {
    i = new Array(n);
    o = new Array(n);
    for (let k = 0; k < n; k++) {
      i[k] = S[base + n + k] * iv;
      o[k] = S[base + n + n + k] * iv;
    }
  }
  return { v, i, o, c: (head >> 5) & 1 };
}

/**
 * A scalar column's value, taking the expression column ahead of the wire.
 *
 * `X` is null for the columns nothing drives by expression, which is nearly all
 * of them, so the test costs one compare against a loop-invariant.
 */
export function xv(x, X, C, i, f, c) {
  return X && X[i] ? X[i](f) : pv(x, C[i], f, c, i);
}

/** The same for a vector column, into the column's scratch. */
export function xvv(x, X, C, i, f, c, out) {
  return X && X[i] ? X[i](f) : pvv(x, C[i], f, c, i, out);
}

/**
 * One scratch buffer per binding in a vector column, sized to that binding's
 * own property.
 *
 * Sizing it per property rather than per column is what lets `css` keep asking
 * `c.length > 3` for an alpha channel: a shared four-slot buffer would answer
 * yes for a three-component colour and paint it with whatever the previous
 * binding left behind.
 */
export function vscratch(x, col, n) {
  const S = x.S;
  const out = new Array(n);
  for (let i = 0; i < n; i++) out[i] = new Array(pdim(S, col[i])).fill(0);
  return out;
}

/** A property's component count, without evaluating it. */
function pdim(S, off) {
  if (!off) return 2;
  const head = S[off];
  const tag = head & 7;
  if (tag === T_VECTOR) return S[off + 1];
  if (tag === T_EXPR) return pdim(S, S[off + 2]);
  if (tag === T_SCALAR || tag === T_PATH) return 1;
  return ((head >> 13) & 3) + 1;
}
