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

import { xv, xvv, pvp, mkPath, T_PATH } from '../pv.js';
import { xcol } from '../kf.js';
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
 * The trim triple as columns, plus the arc-length table of every source path
 * the compiler already resolved — those are frame-invariant, so measuring one
 * per frame would be the single most expensive thing a trimmed shape does.
 */
function trimCols(x, TM, P, n, at) {
  const S = x.S;
  const M = new Int32Array(n), M2 = new Int32Array(n), M3 = new Int32Array(n);
  const B = new Array(n);
  for (let i = 0; i < n; i++) {
    const m = TM[i];
    if (!m) continue;
    M[i] = S[m]; M2[i] = S[m + 1]; M3[i] = S[m + 2];
    // A keyframed or expression-driven shape is not a static path.
    if (P && P[i] && (S[P[i]] & 7) === T_PATH) B[i] = trimTable(mkPath(S, P[i]));
  }
  return {
    M, M2, M3, B,
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
  const lo = a < z ? a : z, hi = a < z ? z : a, vis = hi - lo;
  let out = null, hide = false;
  if (vis <= 0) {
    hide = true;
  } else if (vis < 1) {
    out = trimApply(m.B[i] || trimTable(src), lo, hi, xv(x, m.X3, m.M3, i, t, m.C3) / 360);
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
