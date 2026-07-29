// Layer records, as the expression engine sees them.
//
// The engine used to address everything by stream offset — a record was an
// offset, a property was an offset, and reading one meant knowing the wire
// layout. That works for the interpreter and is useless to a generated module,
// which has no stream at all.
//
// So the boundary is objects and **handles** instead. A record is a plain
// object; each of its properties is an evaluator function carrying whatever
// metadata the engine needs hung off it (`kf` for the keyframe surface, `pathv`
// for geometry, `x`/`src`/`l` for an expression). The interpreter builds them
// by reading the stream once at mount, which is where it already read most of
// this; a generated module emits the same shapes directly.
//
// A precomp's records are stored once on its asset but materialized **per
// instantiation**, because each instance runs on its own clock and a shared
// evaluator would drag its keyframe cursor between them on every frame.

import { resolve } from './kf.js';
import { R_NAME, R_PARENT, R_P, R_A, R_SC, R_R, R_O, R_H, R_EFFECTS } from './wire.js';

/** Fields in mask-bit order, after `name` and `parent`. */
const FIELDS = [['p', R_P], ['a', R_A], ['sc', R_SC], ['r', R_R], ['o', R_O], ['h', R_H]];

/**
 * Materialize one record table. `table` is the decoded row-offset column.
 *
 * Rows are variable-length and their fields are stored in mask-bit order, so
 * this walks the mask once rather than counting set bits per field.
 *
 * `at` is the instantiation these records belong to, undefined for the
 * document's own. It has to reach `resolve`: a property stored on a record
 * inside a precomp names its owning layer with an index local to that precomp,
 * and without `at` the engine read that index against the *document*'s table —
 * silently landing on a real but wrong layer.
 */
export function records(ctx, table, at) {
  const S = ctx.S;
  const out = [];
  if (!table) return out;
  for (const row of table) {
    const mask = S[row];
    let o = row + 2;
    const rec = { i: S[row + 1] };
    // The name itself, not its pool index: the engine is shared with
    // generated modules, which have no pool.
    rec.n = mask & R_NAME ? ctx.str[S[o++]] : undefined;
    rec.pr = mask & R_PARENT ? S[o++] : undefined;
    for (const [key, bit] of FIELDS) {
      rec[key] = mask & bit ? resolve(S[o++], ctx, at) : null;
    }
    rec.ef = mask & R_EFFECTS ? effects(ctx, S[o++], at) : null;
    out.push(rec);
  }
  lyLink(out);
  return out;
}

/**
 * Give each record its own table and position in it.
 *
 * Everything a layer reference resolves to is one of these two: a sibling by
 * index, or the parent by `pr` — both local to the table the record already
 * lives in. Stamping them here means a reference needs no map, no global index
 * space, and no separate placement pass.
 */
export function lyLink(recs) {
  for (let i = 0; i < recs.length; i++) {
    recs[i]._t = recs;
    recs[i]._i = i;
  }
}

/**
 * `[count, (name, mn, paramCount, (name, mn, ty, value, prop) × n) × count]`.
 *
 * Names are pool indices biased by one, so 0 can mean "unnamed". A parameter
 * carries either a literal — the low bit of its value slot says so — or a
 * property handle.
 */
function effects(ctx, off, at) {
  const S = ctx.S, str = ctx.str;
  const name = (i) => (i ? str[i - 1] : null);
  const out = [];
  let c = off + 1;
  for (let i = 0, n = S[off]; i < n; i++) {
    const np = S[c + 2];
    const params = c + 3;
    const ps = [];
    for (let k = 0; k < np; k++) {
      const q = params + k * 5;
      const ty = S[q + 2];
      const v = S[q + 3];
      ps.push({
        nm: name(S[q]),
        mn: name(S[q + 1]),
        ty,
        // A layer-control parameter holds an index, so it is not scaled.
        v: v & 1 ? (ty === 10 ? v >> 1 : (v >> 1) / 1000) : undefined,
        p: S[q + 4] ? resolve(S[q + 4], ctx, at) : null,
      });
    }
    out.push({ nm: name(S[c]), mn: name(S[c + 1]), ef: ps });
    c = params + np * 5;
  }
  return out;
}

/** The record a binding names, within the document's table or an asset's. */
export function record(ctx, at, i) {
  return (at ? at.recs : ctx.recs)[i];
}
