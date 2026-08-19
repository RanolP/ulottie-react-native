// Stroke dash pattern: `stroke-dasharray` and `stroke-dashoffset`.
//
// Reached only when some length or the offset animates — a static pattern is
// baked into the markup. The section is `[count, length…, offset]`; the
// string is the same raw, space-joined form lottie-web's `DashProperty`
// writes.

import { xv } from '../pv.js';
import { xcol } from '../kf.js';
import { put } from '../set.js';
import { open } from '../batch.js';

export function bDash(x, base, eb, sb, ps, at) {
  const b = open(x, base, eb, sb, ps, 1);
  const n = b.n, S = x.S, A0 = b.A[0];
  // Per binding: its length-property offsets and the offset property.
  const N = new Int32Array(n);
  const B = new Int32Array(n + 1);
  let total = 0;
  for (let i = 0; i < n; i++) total += N[i] = S[A0[i]];
  for (let i = 0; i <= n; i++) B[i] = i === 0 ? 0 : B[i - 1] + N[i - 1];
  const P = new Int32Array(total);
  const O = new Int32Array(n);
  for (let i = 0, k = 0; i < n; i++) {
    for (let j = 0; j < N[i]; j++, k++) P[k] = S[A0[i] + 1 + j];
    O[i] = S[A0[i] + 1 + N[i]];
  }
  return {
    n, E: b.E, G: b.G, L: b.L, B, P, O,
    XP: x.expr ? xcol(x, P, total, at) : null,
    XO: x.expr ? xcol(x, O, n, at) : null,
    C: new Int32Array(total), CO: new Int32Array(n),
    W: new Array(n), WO: new Array(n),
  };
}

export function oDash(x, s) {
  const n = s.n, E = s.E, G = s.G, L = s.L, B = s.B, P = s.P, O = s.O;
  const XP = s.XP, XO = s.XO, C = s.C, CO = s.CO, W = s.W, WO = s.WO;
  const T = x.T, ON = x.ON;
  for (let i = 0; i < n; i++) {
    if (!ON[G[i]]) continue;
    const t = T[L[i]], el = E[i];
    let str = '';
    for (let k = B[i], end = B[i + 1]; k < end; k++) {
      str += (k > B[i] ? ' ' : '') + xv(x, XP, P, k, t, C);
    }
    put(el, 'stroke-dasharray', str, W, i);
    put(el, 'stroke-dashoffset', '' + xv(x, XO, O, i, t, CO), WO, i);
  }
}
