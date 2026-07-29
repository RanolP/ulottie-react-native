// Layer transform and opacity, read from the expression layer table.
//
// When an animation has expressions the layer's properties already live in the
// record table so `thisLayer.position` can read them. These binders take the
// record index rather than a second copy of the same keyframes.

import { r5, r2, r } from '../num.js';
import { attr } from '../set.js';
import { record } from '../rec.js';

// A record field the compiler elided is one that equals its default, so the
// default lives here rather than costing a wire entry. These must stay in step
// with `flat::RECORD_DEFAULTS`.
const ORIGIN = () => [0, 0, 0];
const FULL = () => [100, 100, 100];
const ZERO = () => 0;
const OPAQUE = () => 100;

export function bLayerTx(el, S, a, ctx, at, ri) {
  return layerTx(el, record(ctx, at, ri));
}

/**
 * Build a layer's transform updater from its record.
 *
 * Nothing here folds — every input is a runtime handle — so generated code
 * calls this rather than inlining it. `ripple` has 140 of these bindings, and
 * inlining them cost 84 KB against roughly one kilobyte of calls.
 */
export function layerTx(el, rec) {
  // The record's fields are already evaluators; a missing one means the
  // compiler elided a property equal to its default.
  const p = rec.p || ORIGIN;
  const an = rec.a || ORIGIN;
  const s = rec.sc || FULL;
  const rot = rec.r || ZERO;
  const set = attr(el, 'transform');
  return (f) => {
    const pv = p(f), av = an(f), sv = s(f);
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

export function bLayerOpacity(el, S, a, ctx, at, ri) {
  return layerOp(el, record(ctx, at, ri));
}

/** The same, for a layer's opacity. */
export function layerOp(el, rec) {
  const o = rec.o || OPAQUE;
  const set = attr(el, 'opacity');
  return (f) => set(r(o(f) / 100));
}
