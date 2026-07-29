// Animated gradient geometry. The stops themselves were resolved and written
// into the markup at compile time; only the start/end handles move.

import { xvv } from '../pv.js';
import { xcol } from '../kf.js';
import { r } from '../num.js';
import { put } from '../set.js';
import { open } from '../batch.js';

export function bGradient(x, base, eb, sb, ps, at) {
  const b = open(x, base, eb, sb, ps, 3);
  const n = b.n;
  return {
    n, E: b.E, G: b.G, L: b.L, K: b.A[0], P: b.A[1], Q: b.A[2],
    XP: x.expr ? xcol(x, b.A[1], n, at) : null,
    XQ: x.expr ? xcol(x, b.A[2], n, at) : null,
    CP: new Int32Array(n), CQ: new Int32Array(n),
    VP: [0, 0, 0], VQ: [0, 0, 0],
    W: new Array(n), W2: new Array(n), W3: new Array(n), W4: new Array(n),
  };
}

export function oGradient(x, s) {
  const n = s.n, E = s.E, G = s.G, L = s.L, K = s.K, T = x.T, ON = x.ON;
  const P = s.P, Q = s.Q, XP = s.XP, XQ = s.XQ, CP = s.CP, CQ = s.CQ;
  const VP = s.VP, VQ = s.VQ, W = s.W, W2 = s.W2, W3 = s.W3, W4 = s.W4;
  for (let i = 0; i < n; i++) {
    if (!ON[G[i]]) continue;
    const t = T[L[i]], el = E[i], radial = K[i] === 2;
    const a = xvv(x, XP, P, i, t, CP, VP);
    const c = xvv(x, XQ, Q, i, t, CQ, VQ);
    put(el, radial ? 'cx' : 'x1', r(a[0]), W, i);
    put(el, radial ? 'cy' : 'y1', r(a[1]), W2, i);
    if (radial) {
      put(el, 'r', r(Math.hypot(c[0] - a[0], c[1] - a[1])), W3, i);
    } else {
      put(el, 'x2', r(c[0]), W3, i);
      put(el, 'y2', r(c[1]), W4, i);
    }
  }
}
