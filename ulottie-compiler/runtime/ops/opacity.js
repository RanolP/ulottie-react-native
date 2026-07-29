import { xv } from '../pv.js';
import { xcol } from '../kf.js';
import { r } from '../num.js';
import { put } from '../set.js';
import { open } from '../batch.js';

export function bOpacity(x, base, eb, sb, ps, at) {
  const b = open(x, base, eb, sb, ps, 1);
  return {
    n: b.n, E: b.E, G: b.G, L: b.L, O: b.A[0],
    X: x.expr ? xcol(x, b.A[0], b.n, at) : null,
    C: new Int32Array(b.n), W: new Array(b.n),
  };
}

export function oOpacity(x, s) {
  const n = s.n, E = s.E, G = s.G, L = s.L, O = s.O, X = s.X, C = s.C, W = s.W;
  const T = x.T, ON = x.ON;
  for (let i = 0; i < n; i++) {
    if (!ON[G[i]]) continue;
    put(E[i], 'opacity', r(xv(x, X, O, i, T[L[i]], C) / 100), W, i);
  }
}
