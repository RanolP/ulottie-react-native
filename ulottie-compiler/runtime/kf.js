// Property resolution and keyframe interpolation.
//
// A property is a run of integers at an offset in `ctx.S`, the single
// `Int32Array` the payload decodes to. `resolve` runs once per property at
// mount and returns the cheapest closure that can produce its value; the
// per-frame path then indexes that array directly.
//
// Nothing is rebuilt at mount: there is no object per property, no array per
// column, and no copy of the keyframe data. The interpolator closes over four
// integer offsets into the shared buffer and reads through them.

import { EASE } from './ease.js';
import { spSeg, spSample } from './spatial.js';
import { lerpPath } from './kfpath.js';
import { INV } from './scale.js';

/// Property tags, in the low three bits of a property's first word. Kept in
/// sync with `scene/flat.rs`.
const T_SCALAR = 0;
const T_VECTOR = 1;
const T_PATH = 2;
const T_EXPR = 4;
// T_ANIM (3) needs no constant — it is what `resolve` falls through to.

/// `Anim` header flags, above the tag and the two shifts.
const F_END = 1;
const F_EASE = 2;
const F_HOLD = 4;
const F_SPATIAL = 8;

/** Static value. */
const konst = (v) => () => v;

/**
 * Turn a wire property into an evaluator.
 *
 * `off` is an offset into `ctx.S`; zero means the property is absent, which is
 * unambiguous because offset zero is a guard slot the encoder never fills.
 */
export function resolve(off, ctx, at) {
  if (!off) return konst(0);
  const S = ctx.S;
  const head = S[off];
  const tag = head & 7;
  if (tag === T_SCALAR) return konst(S[off + 1] * INV[(head >> 3) & 3]);
  if (tag === T_VECTOR) return konst(vector(S, off));
  if (tag === T_PATH) {
    const p = mkPath(S, off);
    const ev = konst(p);
    // Expressions that rewrite a shape read its geometry off `thisProperty`.
    if (ctx.expr) ev.pathv = p;
    return ev;
  }
  // Expressions are checked before keyframes because a property driven by an
  // expression carries both: the keyframes are only what it reads as `value`.
  if (tag === T_EXPR) {
    // A handle, not an offset: the engine never learns where this came from,
    // which is what lets a generated module hand it the same thing.
    const h = {
      x: S[off + 1],
      src: S[off + 2] ? resolve(S[off + 2], ctx) : null,
      l: S[off + 3] ? S[off + 3] - 1 : undefined,
    };
    return ctx.expr ? ctx.expr(h, at) : konst(h.src ? h.src(0) : 0);
  }
  const ev = keyframed(off, ctx);
  // `numKeys`/`key`/`nearestKey` walk the raw keyframes rather than sampling
  // them, so the columns come along — but only when there is an engine to ask.
  if (ctx.expr) ev.kf = keyframes(S, off);
  return ev;
}

/**
 * A keyframed property's columns as plain arrays, for the `thisProperty`
 * surface. Materialized only for animations that have expressions, since
 * nothing else can observe them.
 */
function keyframes(S, off) {
  const a = animInfo(S, off);
  const t = new Array(a.n);
  for (let i = 0; i < a.n; i++) t[i] = S[a.t + i] * a.ts;
  const v = [];
  if (a.kind === 2) {
    for (let i = 0; i < a.n; i++) v.push(mkPath(S, S[a.v + i]));
  } else {
    for (let i = 0; i < a.n * a.d; i++) v.push(S[a.v + i] * a.iv);
  }
  return { t, v, d: a.d, kind: a.kind };
}

/** `[tag|shift<<3, count, v…]` → a plain array, built once. */
function vector(S, off) {
  const iv = INV[(S[off] >> 3) & 3];
  const n = S[off + 1];
  const out = new Array(n);
  for (let k = 0; k < n; k++) out[k] = S[off + 2 + k] * iv;
  return out;
}

/**
 * `[tag|shift<<3|closed<<5|curved<<6, points, x,y…, (i…), (o…)]` → `{v,i,o,c}`.
 *
 * Materialized once per property. Geometry is the one place where an object is
 * the working representation rather than the wire one — `pathD`, `trimTable`
 * and `lerpPath` all walk it — so it is built here and never again.
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
 * Where a keyframed property's columns start, for the expression API.
 *
 * `numKeys`, `key()` and `nearestKey()` need to walk the raw keyframes rather
 * than sample them. This is the one place outside this module that has to know
 * the layout, so it is handed over rather than duplicated.
 */
export function animInfo(S, off) {
  const head = S[off];
  const n = S[off + 1];
  return {
    kind: (head >> 11) & 3,
    d: ((head >> 13) & 3) + 1,
    n,
    /** Times column, already descaled by `ts`. */
    t: off + 2,
    ts: INV[(head >> 3) & 3],
    /** Values column. */
    v: off + 2 + n,
    iv: INV[(head >> 5) & 3],
  };
}

/**
 * Build an interpolator for a keyframed property.
 *
 * ```text
 * [tag | tShift<<3 | vShift<<5 | flags<<7 | kind<<11 | (dim-1)<<13, count,
 *  t…, v…, (e…), (ez…), (h…), (to… ti…)]
 * ```
 *
 * Every column is a slice of the shared `Int32Array`, addressed by an offset
 * held in the closure. Vector results are written into one scratch array, so a
 * steady-state frame allocates nothing.
 */
function keyframed(off, ctx) {
  const S = ctx.S;
  const head = S[off];
  const kind = (head >> 11) & 3;
  const d = ((head >> 13) & 3) + 1;
  const n = S[off + 1];
  const flags = (head >> 7) & 15;

  // Times stay in their own scale and the frame is scaled to match, so the hot
  // comparisons are against integers and no time is ever descaled.
  const ts = INV[(head >> 3) & 3];
  const tscale = 1 / ts;
  const iv = INV[(head >> 5) & 3];

  const t = off + 2;                 // times
  const vlen = kind === 2 ? n : n * d;
  const v = t + n;                   // values
  const e = flags & F_END ? v + vlen : 0;
  let next = v + vlen + (e ? vlen : 0);
  const ez = flags & F_EASE ? next : 0;
  if (ez) next += n - 1;
  const hd = flags & F_HOLD ? next : 0;
  if (hd) next += n - 1;
  const to = flags & F_SPATIAL ? next : 0;
  // Spatial tangents are per *segment*, not per keyframe, so each column is
  // `(n-1)*d` — one shorter than the value column. Reading `ti` at `n*d` put
  // every in-tangent one keyframe late, which bent `starfish`'s motion paths
  // by a couple of percent: visible to the geometry check, invisible to the
  // pixel diff.
  const ti = to ? to + (n - 1) * d : 0;

  const easings = ctx.z;
  const last = t + n - 1;

  // Segment cursor. Playback is overwhelmingly monotonic, so checking the
  // previous segment first turns the search into two comparisons.
  let cur = 0;

  const seek = (f) => {
    if (f >= S[t + cur] && f <= S[t + cur + 1]) return cur;
    let lo = 0, hi = n - 1;
    while (hi - lo > 1) {
      const m = (lo + hi) >> 1;
      if (S[t + m] <= f) lo = m; else hi = m;
    }
    cur = lo;
    return lo;
  };

  const ease = (i, u) => {
    if (!ez) return u;
    const k = S[ez + i];
    return k === 0 ? u : EASE(easings[k], u);
  };

  if (kind === 2) {
    // Path keyframes hold offsets to pooled path properties. They are constant,
    // so each is materialized once here rather than per frame.
    const paths = new Array(n);
    for (let i = 0; i < n; i++) paths[i] = mkPath(S, S[v + i]);
    const ends = e ? new Array(n) : null;
    if (ends) for (let i = 0; i < n; i++) ends[i] = mkPath(S, S[e + i]);
    return (f) => {
      const ft = f * tscale;
      if (ft <= S[t]) return paths[0];
      if (ft >= S[last]) return paths[n - 1];
      const i = seek(ft);
      const span = S[t + i + 1] - S[t + i];
      if (span === 0) return paths[i + 1];
      if (hd && S[hd + i]) return paths[i];
      const u = ease(i, (ft - S[t + i]) / span);
      return lerpPath(paths[i], ends ? ends[i] : paths[i + 1], u);
    };
  }

  if (kind === 0) {
    return (f) => {
      const ft = f * tscale;
      if (ft <= S[t]) return S[v] * iv;
      if (ft >= S[last]) return S[v + n - 1] * iv;
      const i = seek(ft);
      const span = S[t + i + 1] - S[t + i];
      if (span === 0) return S[v + i + 1] * iv;
      if (hd && S[hd + i]) return S[v + i] * iv;
      const u = ease(i, (ft - S[t + i]) / span);
      const a = S[v + i];
      const b = e ? S[e + i] : S[v + i + 1];
      return (a + (b - a) * u) * iv;
    };
  }

  const out = new Array(d);
  // Arc-length tables for spatial segments, built on first visit and kept.
  const tabs = to ? [] : null;
  const slice = (base) => {
    for (let k = 0; k < d; k++) out[k] = S[base + k] * iv;
    return out;
  };
  return (f) => {
    const ft = f * tscale;
    if (ft <= S[t]) return slice(v);
    if (ft >= S[last]) return slice(v + (n - 1) * d);
    const i = seek(ft);
    const span = S[t + i + 1] - S[t + i];
    if (span === 0) return slice(v + (i + 1) * d);
    if (hd && S[hd + i]) return slice(v + i * d);
    const u = ease(i, (ft - S[t + i]) / span);
    const ai = v + i * d;
    const bi = e ? e + i * d : v + (i + 1) * d;
    if (to) {
      const so = to + i * d, si = ti + i * d;
      if (S[so] || S[so + 1] || S[si] || S[si + 1]) {
        // Built on the segment's first visit and kept; the per-frame path is
        // only `spSample`.
        return spSample(tabs[i] || (tabs[i] = spSeg(S, ai, bi, so, si, d, iv)), u, out);
      }
    }
    for (let k = 0; k < d; k++) {
      const a = S[ai + k];
      out[k] = (a + (S[bi + k] - a) * u) * iv;
    }
    return out;
  };
}
