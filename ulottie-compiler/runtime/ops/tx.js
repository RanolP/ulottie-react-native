// Full transform: position, anchor, scale and rotation may each vary.

import { xv, xvv } from '../pv.js';
import { xcol } from '../kf.js';
import { mtx } from '../mtx.js';
import { put } from '../set.js';
import { open } from '../batch.js';

export function bTransform(x, base, eb, sb, ps, at) {
  const b = open(x, base, eb, sb, ps, 4);
  const n = b.n;
  return {
    n, E: b.E, G: b.G, L: b.L,
    P: b.A[0], A: b.A[1], K: b.A[2], R: b.A[3],
    XP: x.expr ? xcol(x, b.A[0], n, at) : null,
    XA: x.expr ? xcol(x, b.A[1], n, at) : null,
    XK: x.expr ? xcol(x, b.A[2], n, at) : null,
    XR: x.expr ? xcol(x, b.A[3], n, at) : null,
    CP: new Int32Array(n), CA: new Int32Array(n),
    CK: new Int32Array(n), CR: new Int32Array(n),
    // One scratch per column, not per binding: a binding never holds two values
    // from the same column at once, and every consumer here reads only the
    // components its own property has.
    VP: [0, 0, 0], VA: [0, 0, 0], VK: [0, 0, 0],
    W: new Array(n),
  };
}

export function oTransform(x, s) {
  // Every field is hoisted before the loop, not read inside it. This is the
  // closure's capture list written out: the loop body must see locals, or each
  // iteration pays a property load per column.
  const n = s.n, E = s.E, G = s.G, L = s.L, W = s.W, T = x.T, ON = x.ON;
  const P = s.P, A = s.A, K = s.K, R = s.R;
  const XP = s.XP, XA = s.XA, XK = s.XK, XR = s.XR;
  const CP = s.CP, CA = s.CA, CK = s.CK, CR = s.CR;
  const VP = s.VP, VA = s.VA, VK = s.VK;
  for (let i = 0; i < n; i++) {
    if (!ON[G[i]]) continue;
    const t = T[L[i]];
    put(E[i], 'transform', mtx(
      xvv(x, XP, P, i, t, CP, VP),
      xvv(x, XA, A, i, t, CA, VA),
      xvv(x, XK, K, i, t, CK, VK),
      xv(x, XR, R, i, t, CR)), W, i);
  }
}
