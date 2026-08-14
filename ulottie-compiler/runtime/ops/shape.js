// Geometry: build a path for the frame, optionally trim it, write `d`.
//
// Reached only when something about the shape actually moves. A static shape
// under an animated trim still comes through here, but with its source path
// already resolved by the compiler — so its arc-length table is built once.
//
// **One loop per geometry kind, not one loop with a switch.** Which generator a
// shape needs was decided when the shape was planned, so it is the op code: an
// animation that only draws bezier paths ships neither the polystar generator
// nor the branch that skips past it, and each loop below calls exactly one
// generator from exactly one call site.
//
// The trim modifier stays a column rather than doubling the op count. It is
// shared, and a batch that has none never enters it.

import { xv, xvv, pv, pvp, mkPath, T_PATH, T_EXPR } from '../pv.js';
import { xcol, resolve } from '../kf.js';
import { pathD } from '../path.js';
import { rectPath, ellipsePath, starPath } from '../geom.js';
import { trimTable, trimApply } from '../trim.js';
import { put } from '../set.js';
import { open } from '../batch.js';

/** Does any binding in this batch carry a trim? */
function hasTrim(TM, n) {
  for (let i = 0; i < n; i++) if (TM[i]) return true;
  return false;
}

/**
 * The extra steps of one binding's trim chain: property offsets, cursors and
 * — when an engine is present — expression handles, per binding.
 *
 * Its own declaration, cut by `TRIM_CHAIN`: a chain is rare (a group trim
 * nested inside a layer trim), and an ordinarily-trimmed animation should not
 * carry the composing machinery. The call site guards on the wire's own step
 * count, which a module without the capability never exceeds.
 */
function trimChainCols(x, R, n, i, m, at) {
  const S = x.S, k = S[m];
  if (!R) R = { r: new Array(n), c: new Array(n), h: null, w: [0, 0] };
  const r = new Int32Array((k - 1) * 3);
  for (let j = 1; j < k; j++) {
    r[(j - 1) * 3] = S[m + 1 + j * 4];
    r[(j - 1) * 3 + 1] = S[m + 2 + j * 4];
    r[(j - 1) * 3 + 2] = S[m + 3 + j * 4];
  }
  R.r[i] = r;
  R.c[i] = new Int32Array(r.length);
  if (x.expr) {
    for (let q = 0; q < r.length; q++) {
      if (r[q] && (S[r[q]] & 7) === T_EXPR) {
        if (!R.h) R.h = new Array(n);
        (R.h[i] || (R.h[i] = new Array(r.length).fill(null)))[q] = resolve(r[q], x, at);
      }
    }
  }
  return R;
}

/**
 * Fold a chain's later steps over the window `[A, A+L)`, in source fractions.
 *
 * Sequential trims compose exactly in arc-fraction space, since a sub-range
 * of a trimmed path is a sub-range of the original. Every later step works on
 * the *open* result of the first, so its window clamps to `[0,1]`; only the
 * first step's offset can wrap a closed contour, which the final cut resolves.
 */
function trimChainWin(x, R, i, t, A, L) {
  const r = R.r[i], H = R.h && R.h[i], C = R.c[i], w = R.w;
  for (let j = 0; j < r.length; j += 3) {
    const a = (H && H[j] ? H[j](t) : pv(x, r[j], t, C, j)) / 100;
    const z = (H && H[j + 1] ? H[j + 1](t) : pv(x, r[j + 1], t, C, j + 1)) / 100;
    const o = (H && H[j + 2] ? H[j + 2](t) : pv(x, r[j + 2], t, C, j + 2)) / 360;
    let lo = (a < z ? a : z) + o, hi = (a < z ? z : a) + o;
    lo = lo < 0 ? 0 : lo > 1 ? 1 : lo;
    hi = hi < 0 ? 0 : hi > 1 ? 1 : hi;
    A += L * lo;
    L *= hi - lo;
  }
  w[0] = A;
  w[1] = A + L;
  return w;
}

/**
 * The trim chain as columns, plus the arc-length table of every source path
 * the compiler already resolved — those are frame-invariant, so measuring one
 * per frame would be the single most expensive thing a trimmed shape does.
 *
 * A binding's section is `[count, (s, e, o, mode) × count]`, steps in
 * application order. The first step is the common case and stays in the flat
 * `M`/`M2`/`M3` columns; the rare extra steps ride per binding in `R`.
 */
function trimCols(x, TM, P, n, at) {
  const S = x.S;
  const M = new Int32Array(n), M2 = new Int32Array(n), M3 = new Int32Array(n);
  const B = new Array(n);
  let R = null;
  for (let i = 0; i < n; i++) {
    const m = TM[i];
    if (!m) continue;
    M[i] = S[m + 1]; M2[i] = S[m + 2]; M3[i] = S[m + 3];
    if (S[m] > 1) R = trimChainCols(x, R, n, i, m, at);
    // A keyframed or expression-driven shape is not a static path.
    if (P && P[i] && (S[P[i]] & 7) === T_PATH) B[i] = trimTable(mkPath(S, P[i]));
  }
  return {
    M, M2, M3, B, R,
    X: x.expr ? xcol(x, M, n, at) : null,
    X2: x.expr ? xcol(x, M2, n, at) : null,
    X3: x.expr ? xcol(x, M3, n, at) : null,
    C: new Int32Array(n), C2: new Int32Array(n), C3: new Int32Array(n),
    W: new Array(n),
  };
}

/**
 * The outline to draw, or null when the trim range has closed over it.
 *
 * Hiding is the element's own state rather than an attribute, so it is settled
 * here and the caller only has to know whether there is anything left to write.
 *
 * Whether *this* binding has a trim at all is the caller's test — a batch can
 * mix trimmed and untrimmed shapes, and the untrimmed ones should cost one
 * array load rather than a call that reads eleven fields to learn they have
 * nothing to do.
 */
function trim(x, m, i, t, src, el) {
  const a = xv(x, m.X, m.M, i, t, m.C) / 100;
  const z = xv(x, m.X2, m.M2, i, t, m.C2) / 100;
  let lo = a < z ? a : z, hi = a < z ? z : a;
  let off = xv(x, m.X3, m.M3, i, t, m.C3) / 360;
  if (m.R && m.R.r[i]) {
    const w = trimChainWin(x, m.R, i, t, lo + off, hi - lo);
    lo = w[0]; hi = w[1]; off = 0;
  }
  const vis = hi - lo;
  let out = null, hide = false;
  if (vis <= 0) {
    hide = true;
  } else if (vis < 1) {
    out = trimApply(m.B[i] || trimTable(src), lo, hi, off);
    if (out && !out.v.length) hide = true;
  }
  if (hide !== m.W[i]) { m.W[i] = hide; el.style.display = hide ? 'none' : ''; }
  return hide ? null : out || src;
}

/** One reusable outline per binding, so a steady-state frame allocates nothing. */
function outlines(n) {
  const out = new Array(n);
  for (let i = 0; i < n; i++) out[i] = { v: [], i: null, o: null, c: 1 };
  return out;
}

// --- a bezier path property, keyframed or driven by an expression ----------

export function bShape(x, base, eb, sb, ps, at) {
  const b = open(x, base, eb, sb, ps, 2);
  const n = b.n, P = b.A[0];
  return {
    n, E: b.E, G: b.G, L: b.L, P,
    XP: x.expr ? xcol(x, P, n, at) : null,
    TM: hasTrim(b.A[1], n) ? trimCols(x, b.A[1], P, n, at) : null,
    C: new Int32Array(n), Q: new Array(n), W: new Array(n),
  };
}

export function oShape(x, s) {
  const n = s.n, E = s.E, G = s.G, L = s.L, P = s.P, XP = s.XP, TM = s.TM;
  const TB = TM && TM.M, C = s.C, Q = s.Q, W = s.W, T = x.T, ON = x.ON;
  for (let i = 0; i < n; i++) {
    if (!ON[G[i]]) continue;
    const t = T[L[i]], el = E[i];
    let src = XP && XP[i] ? XP[i](t) : pvp(x, P[i], t, C, i, Q);
    if (TB && TB[i]) src = trim(x, TM, i, t, src, el);
    if (src) put(el, 'd', pathD(src), W, i);
  }
}

// --- one element, several path properties -----------------------------------
//
// lottie-web gives every style ONE element and writes each shape it paints
// into that element, so contours sharing a style share a fill rule and their
// windings interact — that is how holes and merged paths render. The compiler
// buckets same-style siblings onto one element; this concatenates their
// outlines per frame. The property list is the one argument, a section
// `[count, prop offset…]` referenced by offset.

export function bShapeMulti(x, base, eb, sb, ps, at) {
  const b = open(x, base, eb, sb, ps, 1);
  const n = b.n, S = x.S, A0 = b.A[0];
  // Each binding's argument is a section `[count, prop offset…]` — the count
  // rides in the section because a list carries no length of its own.
  const N = new Int32Array(n);
  const B = new Int32Array(n + 1);
  let total = 0;
  for (let i = 0; i < n; i++) total += N[i] = S[A0[i]];
  for (let i = 0; i <= n; i++) B[i] = i === 0 ? 0 : B[i - 1] + N[i - 1];
  const P = new Int32Array(total);
  for (let i = 0, k = 0; i < n; i++) {
    for (let j = 0; j < N[i]; j++, k++) P[k] = S[A0[i] + 1 + j];
  }
  return {
    n, E: b.E, G: b.G, L: b.L, N, B, P,
    C: new Int32Array(total), Q: new Array(total), W: new Array(n),
  };
}

export function oShapeMulti(x, s) {
  const n = s.n, E = s.E, G = s.G, L = s.L, B = s.B, P = s.P;
  const C = s.C, Q = s.Q, W = s.W, T = x.T, ON = x.ON;
  for (let i = 0; i < n; i++) {
    if (!ON[G[i]]) continue;
    const t = T[L[i]];
    let d = '';
    for (let k = B[i], end = B[i + 1]; k < end; k++) {
      const src = pvp(x, P[k], t, C, k, Q);
      if (src) d += pathD(src);
    }
    put(E[i], 'd', d, W, i);
  }
}

// --- generated outlines ----------------------------------------------------

export function bShapeRect(x, base, eb, sb, ps, at) {
  const b = open(x, base, eb, sb, ps, 4);
  const n = b.n;
  return {
    n, E: b.E, G: b.G, L: b.L, Z: b.A[0], P: b.A[1], R: b.A[2],
    XZ: x.expr ? xcol(x, b.A[0], n, at) : null,
    XP: x.expr ? xcol(x, b.A[1], n, at) : null,
    XR: x.expr ? xcol(x, b.A[2], n, at) : null,
    TM: hasTrim(b.A[3], n) ? trimCols(x, b.A[3], null, n, at) : null,
    CZ: new Int32Array(n), CP: new Int32Array(n), CR: new Int32Array(n),
    VZ: [0, 0, 0], VP: [0, 0, 0], O: outlines(n), W: new Array(n),
  };
}

export function oShapeRect(x, s) {
  const n = s.n, E = s.E, G = s.G, L = s.L, TM = s.TM, TB = TM && TM.M;
  const T = x.T, ON = x.ON;
  const Z = s.Z, P = s.P, R = s.R, XZ = s.XZ, XP = s.XP, XR = s.XR;
  const CZ = s.CZ, CP = s.CP, CR = s.CR, VZ = s.VZ, VP = s.VP, O = s.O, W = s.W;
  for (let i = 0; i < n; i++) {
    if (!ON[G[i]]) continue;
    const t = T[L[i]], el = E[i];
    const z = xvv(x, XZ, Z, i, t, CZ, VZ);
    const p = xvv(x, XP, P, i, t, CP, VP);
    let src = rectPath(O[i], p[0], p[1], z[0], z[1], xv(x, XR, R, i, t, CR));
    if (TB && TB[i]) src = trim(x, TM, i, t, src, el);
    if (src) put(el, 'd', pathD(src), W, i);
  }
}

export function bShapeEllipse(x, base, eb, sb, ps, at) {
  const b = open(x, base, eb, sb, ps, 3);
  const n = b.n;
  return {
    n, E: b.E, G: b.G, L: b.L, Z: b.A[0], P: b.A[1],
    XZ: x.expr ? xcol(x, b.A[0], n, at) : null,
    XP: x.expr ? xcol(x, b.A[1], n, at) : null,
    TM: hasTrim(b.A[2], n) ? trimCols(x, b.A[2], null, n, at) : null,
    CZ: new Int32Array(n), CP: new Int32Array(n),
    VZ: [0, 0, 0], VP: [0, 0, 0], O: outlines(n), W: new Array(n),
  };
}

export function oShapeEllipse(x, s) {
  const n = s.n, E = s.E, G = s.G, L = s.L, TM = s.TM, TB = TM && TM.M;
  const T = x.T, ON = x.ON;
  const Z = s.Z, P = s.P, XZ = s.XZ, XP = s.XP, CZ = s.CZ, CP = s.CP;
  const VZ = s.VZ, VP = s.VP, O = s.O, W = s.W;
  for (let i = 0; i < n; i++) {
    if (!ON[G[i]]) continue;
    const t = T[L[i]], el = E[i];
    const z = xvv(x, XZ, Z, i, t, CZ, VZ);
    const p = xvv(x, XP, P, i, t, CP, VP);
    let src = ellipsePath(O[i], p[0], p[1], z[0] / 2, z[1] / 2);
    if (TB && TB[i]) src = trim(x, TM, i, t, src, el);
    if (src) put(el, 'd', pathD(src), W, i);
  }
}

export function bShapeStar(x, base, eb, sb, ps, at) {
  const b = open(x, base, eb, sb, ps, 7);
  const n = b.n;
  return {
    n, E: b.E, G: b.G, L: b.L,
    Y: b.A[0], K: b.A[1], P: b.A[2], Z: b.A[3], I: b.A[4], R: b.A[5],
    XK: x.expr ? xcol(x, b.A[1], n, at) : null,
    XP: x.expr ? xcol(x, b.A[2], n, at) : null,
    XZ: x.expr ? xcol(x, b.A[3], n, at) : null,
    XI: x.expr ? xcol(x, b.A[4], n, at) : null,
    XR: x.expr ? xcol(x, b.A[5], n, at) : null,
    TM: hasTrim(b.A[6], n) ? trimCols(x, b.A[6], null, n, at) : null,
    CK: new Int32Array(n), CP: new Int32Array(n), CZ: new Int32Array(n),
    CI: new Int32Array(n), CR: new Int32Array(n),
    VP: [0, 0, 0], O: outlines(n), W: new Array(n),
  };
}

export function oShapeStar(x, s) {
  const n = s.n, E = s.E, G = s.G, L = s.L, Y = s.Y, TM = s.TM, TB = TM && TM.M;
  const T = x.T, ON = x.ON;
  const K = s.K, P = s.P, Z = s.Z, I = s.I, R = s.R;
  const XK = s.XK, XP = s.XP, XZ = s.XZ, XI = s.XI, XR = s.XR;
  const CK = s.CK, CP = s.CP, CZ = s.CZ, CI = s.CI, CR = s.CR;
  const VP = s.VP, O = s.O, W = s.W;
  for (let i = 0; i < n; i++) {
    if (!ON[G[i]]) continue;
    const t = T[L[i]], el = E[i];
    const p = xvv(x, XP, P, i, t, CP, VP);
    let src = starPath(O[i], Y[i], xv(x, XK, K, i, t, CK), p[0], p[1],
      xv(x, XZ, Z, i, t, CZ), xv(x, XI, I, i, t, CI), xv(x, XR, R, i, t, CR));
    if (TB && TB[i]) src = trim(x, TM, i, t, src, el);
    if (src) put(el, 'd', pathD(src), W, i);
  }
}
