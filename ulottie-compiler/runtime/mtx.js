// The layer matrix.
//
// `translate(p) rotate(r) scale(s) translate(-a)` is emitted as a single
// `matrix()` — one attribute, one parse, and the same CTM the browser would
// have derived from the transform list anyway. It is also the only form that
// can express skew, flattened 3D and auto-orient later on.
//
// One implementation, three callers: the transform op, the layer-record op, and
// the single-binding form generated code links against.

import { r5, r2 } from './num.js';

export function mtx(p, a, s, rot) {
  const th = rot * Math.PI / 180;
  const cs = Math.cos(th), sn = Math.sin(th);
  const sx = s[0] / 100, sy = s[1] / 100;
  const m0 = cs * sx, m1 = sn * sx, m2 = -sn * sy, m3 = cs * sy;
  return 'matrix(' + r5(m0) + ',' + r5(m1) + ',' + r5(m2) + ',' + r5(m3) + ','
    + r2(p[0] - (m0 * a[0] + m2 * a[1])) + ','
    + r2(p[1] - (m1 * a[0] + m3 * a[1])) + ')';
}
