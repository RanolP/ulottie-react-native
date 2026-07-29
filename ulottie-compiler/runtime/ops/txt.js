// Translate-only transform: the compiler proved anchor, scale and rotation are
// constant, so the matrix's linear part is a baked string prefix and each frame
// only has to append two numbers.

import { xvv } from '../pv.js';
import { xcol } from '../kf.js';
import { r2 } from '../num.js';
import { put } from '../set.js';
import { open } from '../batch.js';

export function bTranslate(x, base, eb, sb, ps, at) {
  // Columns: [prefixString, extraX, extraY, position]
  const b = open(x, base, eb, sb, ps, 4);
  const n = b.n;
  // No prefix means the linear part was the identity, which `translate()`
  // spells in five fewer bytes — and it is the common case, so the compiler
  // sends nothing rather than a string saying nothing.
  const P = new Array(n);
  const X = new Float64Array(n), Y = new Float64Array(n);
  for (let i = 0; i < n; i++) {
    P[i] = b.A[0][i] ? x.str[b.A[0][i] - 1] : 'translate(';
    X[i] = b.A[1][i] / 1000;
    Y[i] = b.A[2][i] / 1000;
  }
  return {
    n, E: b.E, G: b.G, L: b.L, P, X, Y, Q: b.A[3],
    XQ: x.expr ? xcol(x, b.A[3], n, at) : null,
    C: new Int32Array(n), V: [0, 0, 0], W: new Array(n),
  };
}

export function oTranslate(x, s) {
  const n = s.n, E = s.E, G = s.G, L = s.L, P = s.P, X = s.X, Y = s.Y, W = s.W;
  const Q = s.Q, XQ = s.XQ, C = s.C, V = s.V, T = x.T, ON = x.ON;
  for (let i = 0; i < n; i++) {
    if (!ON[G[i]]) continue;
    const v = xvv(x, XQ, Q, i, T[L[i]], C, V);
    put(E[i], 'transform', P[i] + r2(v[0] + X[i]) + ',' + r2(v[1] + Y[i]) + ')', W, i);
  }
}
