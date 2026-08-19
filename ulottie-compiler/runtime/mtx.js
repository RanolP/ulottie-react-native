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

/**
 * The layer matrix with lottie-web's `skewFromAxis(-sk, sa)` folded in
 * between scale and rotation. Separate from `mtx` so the skewless majority
 * never pays for the factor.
 */
export function mtxSkew(p, a, s, rot, sk, sa) {
  const th = rot * Math.PI / 180;
  const cs = Math.cos(th), sn = Math.sin(th);
  const t = Math.tan(-sk * Math.PI / 180);
  const ax = sa * Math.PI / 180;
  const c2 = Math.cos(ax), s2 = Math.sin(ax);
  const f0 = 1 + t * s2 * c2, f1 = t * c2 * c2, f2 = -t * s2 * s2, f3 = 1 - t * s2 * c2;
  const g0 = cs * f0 - sn * f2, g1 = sn * f0 + cs * f2;
  const g2 = cs * f1 - sn * f3, g3 = sn * f1 + cs * f3;
  const sx = s[0] / 100, sy = s[1] / 100;
  const m0 = g0 * sx, m1 = g1 * sx, m2 = g2 * sy, m3 = g3 * sy;
  return 'matrix(' + r5(m0) + ',' + r5(m1) + ',' + r5(m2) + ',' + r5(m3) + ','
    + r2(p[0] - (m0 * a[0] + m2 * a[1])) + ','
    + r2(p[1] - (m1 * a[0] + m3 * a[1])) + ')';
}
