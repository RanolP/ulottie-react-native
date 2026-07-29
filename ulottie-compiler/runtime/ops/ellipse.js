import { xvv } from '../pv.js';
import { xcol } from '../kf.js';
import { r } from '../num.js';
import { put } from '../set.js';
import { open } from '../batch.js';

export function bEllipse(x, base, eb, sb, ps, at) {
  const b = open(x, base, eb, sb, ps, 2);
  const n = b.n;
  return {
    n, E: b.E, G: b.G, L: b.L, Z: b.A[0], P: b.A[1],
    XZ: x.expr ? xcol(x, b.A[0], n, at) : null,
    XP: x.expr ? xcol(x, b.A[1], n, at) : null,
    CZ: new Int32Array(n), CP: new Int32Array(n),
    VZ: [0, 0, 0], VP: [0, 0, 0],
    W: new Array(n), W2: new Array(n), W3: new Array(n), W4: new Array(n),
  };
}

export function oEllipse(x, s) {
  const n = s.n, E = s.E, G = s.G, L = s.L, T = x.T, ON = x.ON;
  const Z = s.Z, P = s.P, XZ = s.XZ, XP = s.XP, CZ = s.CZ, CP = s.CP;
  const VZ = s.VZ, VP = s.VP, W = s.W, W2 = s.W2, W3 = s.W3, W4 = s.W4;
  for (let i = 0; i < n; i++) {
    if (!ON[G[i]]) continue;
    const t = T[L[i]], el = E[i];
    const z = xvv(x, XZ, Z, i, t, CZ, VZ);
    const p = xvv(x, XP, P, i, t, CP, VP);
    put(el, 'cx', r(p[0]), W, i);
    put(el, 'cy', r(p[1]), W2, i);
    put(el, 'rx', r(z[0] / 2), W3, i);
    put(el, 'ry', r(z[1] / 2), W4, i);
  }
}
