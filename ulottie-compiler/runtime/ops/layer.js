// Layer transform and opacity, read from the expression layer table.
//
// When an animation has expressions the layer's properties already live in
// `D.y` so `thisLayer.position` can read them. These binders take the record
// index rather than a second copy of the same keyframes.

import { resolve } from '../kf.js';
import { r5, r2, r } from '../num.js';
import { attr } from '../set.js';
import { record } from '../rec.js';

export function bLayerTx(el, b, ctx, at, ri) {
  const rec = record(ctx, at, ri);
  const p = resolve(rec.p ?? [0, 0, 0], ctx);
  const a = resolve(rec.a ?? [0, 0, 0], ctx);
  const s = resolve(rec.sc ?? [100, 100, 100], ctx);
  const rot = resolve(rec.r ?? 0, ctx);
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

export function bLayerOpacity(el, b, ctx, at, ri) {
  const o = resolve(record(ctx, at, ri).o ?? 100, ctx, at);
  const set = attr(el, 'opacity');
  return (f) => set(r(o(f) / 100));
}
