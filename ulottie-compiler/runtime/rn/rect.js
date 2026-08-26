// The web `oRect` writes the shared radius through `el.setAttribute` — the one
// op that bypasses `put` because two attributes take one guarded value. This
// twin routes that pair through the RN prop store; everything else is the web
// body verbatim (`bRect` is DOM-free and stays shared).

import { xv, xvv } from '../pv.js';
import { r } from '../num.js';
import { put } from './set.js';
import { rput } from './set.js';

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
      // guard has to cover both writes, exactly as on the web.
      const v = r(Math.min(rad, z[0] / 2, z[1] / 2));
      if (v !== W5[i]) {
        W5[i] = v;
        rput(el, 'rx', v);
        rput(el, 'ry', v);
      }
    }
  }
}
