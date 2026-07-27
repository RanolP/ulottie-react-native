// Full transform binding: position, anchor, scale and rotation may each vary.
//
// The composition `translate(p) rotate(r) scale(s) translate(-a)` is emitted as
// a single `matrix()` — one attribute, one parse, and the same CTM the browser
// would have derived from the transform list anyway. It is also the only form
// that can express skew, flattened 3D and auto-orient later on.

import { resolve } from '../kf.js';
import { r5, r2 } from '../num.js';
import { attr } from '../set.js';

export function bTransform(el, b, ctx, at) {
  const p = resolve(b[2], ctx, at);
  const a = resolve(b[3], ctx, at);
  const s = resolve(b[4], ctx, at);
  const rot = resolve(b[5], ctx, at);
  const set = attr(el, 'transform');
  return (f) => {
    const pv = p(f), av = a(f), sv = s(f);
    const th = rot(f) * Math.PI / 180;
    const cs = Math.cos(th), sn = Math.sin(th);
    const sx = sv[0] / 100, sy = sv[1] / 100;
    const m0 = cs * sx, m1 = sn * sx, m2 = -sn * sy, m3 = cs * sy;
    set(
      'matrix(' + r5(m0) + ',' + r5(m1) + ',' + r5(m2) + ',' + r5(m3) + ','
      + r2(pv[0] - (m0 * av[0] + m2 * av[1])) + ','
      + r2(pv[1] - (m1 * av[0] + m3 * av[1])) + ')');
  };
}
