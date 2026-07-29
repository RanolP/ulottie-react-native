// Layer transform and opacity, read from the expression layer table.
//
// When an animation has expressions the layer's properties already live in the
// record table so `thisLayer.position` can read them. These bindings take the
// record index rather than a second copy of the same keyframes — which is also
// why they are the one op whose inputs stay evaluator objects: a record is the
// handle the engine hands around, and it is shared with whatever else reads it.

import { mtx } from '../mtx.js';
import { r } from '../num.js';
import { put } from '../set.js';
import { record } from '../rec.js';
import { open, runsum } from '../batch.js';

// A record field the compiler elided is one that equals its default, so the
// default lives here rather than costing a wire entry. These must stay in step
// with `flat::RECORD_DEFAULTS` — `o` is 100, so an elided opacity is opaque.
//
// The vectors are module constants rather than fresh literals: `ripple`'s three
// records all elide both anchor and scale, so returning a new array would
// allocate two per binding per frame — 276 on a corpus whose frame path is
// meant to allocate nothing. Nothing mutates them; `mtx` only reads.
const O3 = [0, 0, 0];
const F3 = [100, 100, 100];
const ORIGIN = () => O3;
const FULL = () => F3;
const ZERO = () => 0;
const OPAQUE = () => 100;

/**
 * A record's transform fields as evaluators, `[p, a, sc, r, o]`, with the
 * compiler's elisions filled in.
 *
 * Resolved **once per binding** — at bind time for a batch, in `init` for
 * generated code. Asking the record for its own defaults per frame is four
 * property loads and four branches on every binding, which measured 5% of
 * `ripple`'s frame across its 140 layer bindings.
 */
export function lyFields(rec) {
  return [rec.p || ORIGIN, rec.a || ORIGIN, rec.sc || FULL, rec.r || ZERO, rec.o || OPAQUE];
}

// The record column ships as first differences — an inlined precomp's copies
// address ascending records, which is the one shape deltas suit.

export function bLayerTx(x, base, eb, sb, ps, at) {
  const b = open(x, base, eb, sb, ps, 1);
  const n = b.n;
  const R = runsum(b.A[0], n);
  const P = new Array(n), A = new Array(n), K = new Array(n), Q = new Array(n);
  for (let i = 0; i < n; i++) {
    const f = lyFields(record(x, at, R[i]));
    P[i] = f[0]; A[i] = f[1]; K[i] = f[2]; Q[i] = f[3];
  }
  return { n, E: b.E, G: b.G, L: b.L, P, A, K, Q, W: new Array(n) };
}

export function oLayerTx(x, s) {
  const n = s.n, E = s.E, G = s.G, L = s.L, W = s.W, T = x.T, ON = x.ON;
  const P = s.P, A = s.A, K = s.K, Q = s.Q;
  for (let i = 0; i < n; i++) {
    if (!ON[G[i]]) continue;
    const t = T[L[i]];
    put(E[i], 'transform', mtx(P[i](t), A[i](t), K[i](t), Q[i](t)), W, i);
  }
}

export function bLayerOpacity(x, base, eb, sb, ps, at) {
  const b = open(x, base, eb, sb, ps, 1);
  const n = b.n;
  const R = runsum(b.A[0], n);
  const O = new Array(n);
  for (let i = 0; i < n; i++) O[i] = lyFields(record(x, at, R[i]))[4];
  return { n, E: b.E, G: b.G, L: b.L, O, W: new Array(n) };
}

export function oLayerOpacity(x, s) {
  const n = s.n, E = s.E, G = s.G, L = s.L, O = s.O, W = s.W, T = x.T, ON = x.ON;
  for (let i = 0; i < n; i++) {
    if (!ON[G[i]]) continue;
    put(E[i], 'opacity', r(O[i](T[L[i]]) / 100), W, i);
  }
}
