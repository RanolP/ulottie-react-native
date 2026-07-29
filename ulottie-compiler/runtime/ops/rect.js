import { xv, xvv } from '../pv.js';
import { xcol } from '../kf.js';
import { r } from '../num.js';
import { put } from '../set.js';
import { open } from '../batch.js';

export function bRect(x, base, eb, sb, ps, at) {
  const b = open(x, base, eb, sb, ps, 3);
  const n = b.n;
  return {
    n, E: b.E, G: b.G, L: b.L, Z: b.A[0], P: b.A[1], R: b.A[2],
    XZ: x.expr ? xcol(x, b.A[0], n, at) : null,
    XP: x.expr ? xcol(x, b.A[1], n, at) : null,
    XR: x.expr ? xcol(x, b.A[2], n, at) : null,
    CZ: new Int32Array(n), CP: new Int32Array(n), CR: new Int32Array(n),
    VZ: [0, 0, 0], VP: [0, 0, 0],
    W: new Array(n), W2: new Array(n), W3: new Array(n),
    W4: new Array(n), W5: new Array(n),
  };
}

export function oRect(x, s) {
  const n = s.n, E = s.E, G = s.G, L = s.L, T = x.T, ON = x.ON;
  const Z = s.Z, P = s.P, R = s.R, XZ = s.XZ, XP = s.XP, XR = s.XR;
  const CZ = s.CZ, CP = s.CP, CR = s.CR, VZ = s.VZ, VP = s.VP;
  const W = s.W, W2 = s.W2, W3 = s.W3, W4 = s.W4, W5 = s.W5;
  for (let i = 0; i < n; i++) {
    if (!ON[G[i]]) continue;
    const t = T[L[i]], el = E[i];
    const z = xvv(x, XZ, Z, i, t, CZ, VZ);
    const p = xvv(x, XP, P, i, t, CP, VP);
    put(el, 'x', r(p[0] - z[0] / 2), W, i);
    put(el, 'y', r(p[1] - z[1] / 2), W2, i);
    put(el, 'width', r(z[0]), W3, i);
    put(el, 'height', r(z[1]), W4, i);
    const rad = xv(x, XR, R, i, t, CR);
    if (rad > 0) {
      // One slot for both radii — they always take the same value — but the
      // guard has to cover both writes. Writing `ry` outside it invalidated
      // style and layout on every frame, which is the one thing `put` is for.
      const v = r(Math.min(rad, z[0] / 2, z[1] / 2));
      if (v !== W5[i]) {
        W5[i] = v;
        el.setAttribute('rx', v);
        el.setAttribute('ry', v);
      }
    }
  }
}
