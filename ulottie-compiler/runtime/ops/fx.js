// Animated layer-effect parameters.
//
// Each op is one small attribute write on a filter primitive; the filter
// graph itself is markup the compiler already built. Reached only when an
// effect parameter actually animates.

import { xv } from '../pv.js';
import { xcol } from '../kf.js';
import { put } from '../set.js';
import { open } from '../batch.js';

/** Gaussian blur `stdDeviation`: sigma × 0.3, one axis zeroed by the tag. */
export function bFxBlur(x, base, eb, sb, ps, at) {
  const b = open(x, base, eb, sb, ps, 2);
  const n = b.n;
  return {
    n, E: b.E, G: b.G, L: b.L, P: b.A[0], D: b.A[1],
    XP: x.expr ? xcol(x, b.A[0], n, at) : null,
    C: new Int32Array(n), W: new Array(n),
  };
}

export function oFxBlur(x, s) {
  const n = s.n, E = s.E, G = s.G, L = s.L, P = s.P, D = s.D, XP = s.XP;
  const C = s.C, W = s.W, T = x.T, ON = x.ON;
  for (let i = 0; i < n; i++) {
    if (!ON[G[i]]) continue;
    const t = T[L[i]];
    const sg = xv(x, XP, P, i, t, C) * 0.3;
    const d = D[i];
    put(E[i], 'stdDeviation', (d === 3 ? 0 : sg) + ' ' + (d === 2 ? 0 : sg), W, i);
  }
}

/** A scaled scalar attribute — the factor is the op's own, not wire data:
 * shadow softness ÷ 4, flood opacity ÷ 255, exactly `SVGDropShadowEffect`. */
export function bFxStd(x, base, eb, sb, ps, at) {
  const b = open(x, base, eb, sb, ps, 1);
  const n = b.n;
  return {
    n, E: b.E, G: b.G, L: b.L, P: b.A[0],
    XP: x.expr ? xcol(x, b.A[0], n, at) : null,
    C: new Int32Array(n), W: new Array(n),
  };
}

export function oFxStd(x, s) {
  fxScaled(x, s, 'stdDeviation', 4);
}

export function bFxFloodO(x, base, eb, sb, ps, at) {
  return bFxStd(x, base, eb, sb, ps, at);
}

export function oFxFloodO(x, s) {
  fxScaled(x, s, 'flood-opacity', 255);
}

function fxScaled(x, s, attr, div) {
  const n = s.n, E = s.E, G = s.G, L = s.L, P = s.P, XP = s.XP;
  const C = s.C, W = s.W, T = x.T, ON = x.ON;
  for (let i = 0; i < n; i++) {
    if (!ON[G[i]]) continue;
    const t = T[L[i]];
    put(E[i], attr, '' + xv(x, XP, P, i, t, C) / div, W, i);
  }
}

/** A drop shadow's offset: `dx`/`dy` from direction (degrees) and distance. */
export function bFxOffset(x, base, eb, sb, ps, at) {
  const b = open(x, base, eb, sb, ps, 2);
  const n = b.n;
  return {
    n, E: b.E, G: b.G, L: b.L, A: b.A[0], P: b.A[1],
    XA: x.expr ? xcol(x, b.A[0], n, at) : null,
    XP: x.expr ? xcol(x, b.A[1], n, at) : null,
    CA: new Int32Array(n), CP: new Int32Array(n),
    W: new Array(n), W2: new Array(n),
  };
}

export function oFxOffset(x, s) {
  const n = s.n, E = s.E, G = s.G, L = s.L, A = s.A, P = s.P;
  const XA = s.XA, XP = s.XP, CA = s.CA, CP = s.CP, W = s.W, W2 = s.W2;
  const T = x.T, ON = x.ON;
  for (let i = 0; i < n; i++) {
    if (!ON[G[i]]) continue;
    const t = T[L[i]], el = E[i];
    const rad = (xv(x, XA, A, i, t, CA) - 90) * Math.PI / 180;
    const d = xv(x, XP, P, i, t, CP);
    put(el, 'dx', '' + d * Math.cos(rad), W, i);
    put(el, 'dy', '' + d * Math.sin(rad), W2, i);
  }
}
