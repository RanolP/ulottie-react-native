// Property **handles**, for the two consumers that cannot read a column.
//
// The ops evaluate their properties directly — see `pv.js`, where a property is
// a run of integers and a frame is one call with no closure in it. Two things
// still need a property as a *function*: the expression engine, which hangs
// `thisProperty` off it and hands it around, and a precomp's time remap, which
// the clock table calls per frame.
//
// So this is the same evaluation, wrapped once. It carries no interpolator of
// its own — a second implementation is a second thing to keep in step, and the
// difference would only ever show up as a rendering divergence between an
// animation with expressions and one without.

import { pv, pvv, pvp, mkPath, T_SCALAR, T_EXPR, T_VECTOR, T_PATH, T_ANIM, T_KIND_PATH } from './pv.js';
import { INV } from './scale.js';

/** An absent property. */
const kzero = () => 0;

/** A cursor for the readers that never seek — a constant has no segments. */
const KC = new Int32Array(1);

/**
 * Turn a wire property into an evaluator.
 *
 * `off` is an offset into `ctx.S`; zero means the property is absent, which is
 * unambiguous because offset zero is a guard slot the encoder never fills.
 */
export function resolve(off, ctx, at) {
  if (!off) return kzero;
  const S = ctx.S;
  const head = S[off];
  const tag = head & 7;
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
    return ctx.expr ? ctx.expr(h, at) : () => (h.src ? h.src(0) : 0);
  }
  // A property that cannot change is read once, here, not decoded on every
  // frame. `ripple` reaches 92 of these a frame through the record table alone,
  // and re-deriving a constant is the one thing a handle is free not to do.
  if (tag === T_SCALAR) {
    const v = S[off + 1] * INV[(head >> 3) & 3];
    return () => v;
  }
  if (tag === T_VECTOR) {
    const v = pvv(ctx, off, 0, KC, 0, new Array(S[off + 1]));
    return () => v;
  }
  const c = new Int32Array(1);
  if (tag === T_PATH || (tag === T_ANIM && ((head >> 11) & 3) === T_KIND_PATH)) {
    const m = [];
    const ev = (f) => pvp(ctx, off, f, c, 0, m);
    // Expressions that rewrite a shape read its geometry off `thisProperty`.
    if (ctx.expr && tag === T_PATH) ev.pathv = (m[0] = mkPath(S, off));
    return ev;
  }
  const d = ((head >> 13) & 3) + 1;
  let ev;
  if (d === 1) {
    ev = (f) => pv(ctx, off, f, c, 0);
  } else {
    const out = new Array(d).fill(0);
    ev = (f) => pvv(ctx, off, f, c, 0, out);
  }
  // `numKeys`/`key`/`nearestKey` walk the raw keyframes rather than sampling
  // them, so the columns come along — but only when there is an engine to ask.
  if (ctx.expr && tag === T_ANIM) ev.kf = keyframes(S, off);
  return ev;
}

/**
 * Expression handles for one binding column, or null when it holds none.
 *
 * An op resolves this once at mount and then tests it per binding, so an
 * animation whose expressions drive two of its two hundred bindings pays for
 * two closures rather than for a uniform indirection.
 */
export function xcol(x, col, n, at) {
  const S = x.S;
  let out = null;
  for (let i = 0; i < n; i++) {
    const off = col[i];
    if (off && (S[off] & 7) === T_EXPR) {
      if (!out) out = new Array(n).fill(null);
      out[i] = resolve(off, x, at);
    }
  }
  return out;
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
  if (a.kind === T_KIND_PATH) {
    for (let i = 0; i < a.n; i++) v.push(mkPath(S, S[a.v + i]));
  } else {
    for (let i = 0; i < a.n * a.d; i++) v.push(S[a.v + i] * a.iv);
  }
  return { t, v, d: a.d, kind: a.kind };
}

/**
 * Where a keyframed property's columns start, for the expression API.
 *
 * `numKeys`, `key()` and `nearestKey()` need to walk the raw keyframes rather
 * than sample them.
 */
function animInfo(S, off) {
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
