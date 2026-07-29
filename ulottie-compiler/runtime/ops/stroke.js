import { xv, xvv, vscratch } from '../pv.js';
import { xcol } from '../kf.js';
import { css } from '../css.js';
import { r } from '../num.js';
import { put } from '../set.js';
import { open } from '../batch.js';

export function bStroke(x, base, eb, sb, ps, at) {
  const b = open(x, base, eb, sb, ps, 3);
  const n = b.n;
  return {
    n, E: b.E, G: b.G, L: b.L, K: b.A[0], O: b.A[1], Q: b.A[2],
    XK: x.expr ? xcol(x, b.A[0], n, at) : null,
    XO: x.expr ? xcol(x, b.A[1], n, at) : null,
    XQ: x.expr ? xcol(x, b.A[2], n, at) : null,
    CK: new Int32Array(n), CO: new Int32Array(n), CQ: new Int32Array(n),
    V: vscratch(x, b.A[0], n),
    W: new Array(n), W2: new Array(n),
  };
}

export function oStroke(x, s) {
  const n = s.n, E = s.E, G = s.G, L = s.L, K = s.K, T = x.T, ON = x.ON;
  const O = s.O, Q = s.Q, XK = s.XK, XO = s.XO, XQ = s.XQ;
  const CK = s.CK, CO = s.CO, CQ = s.CQ, V = s.V, W = s.W, W2 = s.W2;
  for (let i = 0; i < n; i++) {
    if (!ON[G[i]]) continue;
    const t = T[L[i]], el = E[i];
    const o = xv(x, XO, O, i, t, CO);
    put(el, K[i] ? 'stroke' : 'stroke-opacity',
      K[i] ? css(xvv(x, XK, K, i, t, CK, V[i]), o) : r(o / 100), W, i);
    put(el, 'stroke-width', r(xv(x, XQ, Q, i, t, CQ)), W2, i);
  }
}
