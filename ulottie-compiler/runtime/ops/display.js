// Layer in/out point. Only emitted for layers whose span is narrower than the
// composition's.

import { open } from '../batch.js';

export function bDisplay(x, base, eb, sb, ps) {
  const b = open(x, base, eb, sb, ps, 2);
  const n = b.n;
  const O = new Float64Array(n), Q = new Float64Array(n);
  for (let i = 0; i < n; i++) { O[i] = b.A[0][i] / 1000; Q[i] = b.A[1][i] / 1000; }
  return { n, E: b.E, G: b.G, L: b.L, O, Q, W: new Array(n) };
}

export function oDisplay(x, s) {
  const n = s.n, E = s.E, G = s.G, L = s.L, O = s.O, Q = s.Q, W = s.W;
  const T = x.T, ON = x.ON;
  for (let i = 0; i < n; i++) {
    if (!ON[G[i]]) continue;
    const t = T[L[i]];
    const v = t >= O[i] && t < Q[i];
    if (v !== W[i]) { W[i] = v; E[i].style.display = v ? '' : 'none'; }
  }
}
