// Decode a compiled `--pretty` module's payload back into readable structure.
//
// Layer resolution is the one thing in the compiler whose answer cannot be read
// off the source: a record index is only meaningful against the table the
// planner built, and that table only exists in the stream. This prints it.
//
//   node ulottie-dev-server/tools/probe.mjs <module.js> [--records] [--exprs]

import { readFileSync } from 'node:fs';
import { dec } from '../../ulottie-compiler/runtime/vlq.js';
import { column } from '../../ulottie-compiler/runtime/col.js';
import * as W from '../../ulottie-compiler/runtime/wire.js';

// Everything below reads the stream through the runtime's own decoder and
// header. It still went stale twice — once when the payload stopped being a
// `{d, s}` object and became a bare string, and once when the asset and use
// rows lost their record and scope columns — so prefer `W.*` over a literal.

const [, , file, ...flags] = process.argv;
if (!file) {
  console.error('usage: probe.mjs <module.js> [--records] [--exprs] [--assets]');
  process.exit(1);
}

const src = readFileSync(file, 'utf8');
const m = /^const D = "([0-9a-v]*)";$/m.exec(src) || /const D="([0-9a-v]*)"/.exec(src);
if (!m) throw new Error('no payload found in ' + file);
const S = dec(m[1]);
// Layer and effect names stopped being payload once every reference resolved to
// a slot at compile time; a module that still emits a pool names it `SP`.
const pool = /^const SP = \[([\s\S]*?)\];$/m.exec(src);
const str = pool ? pool[1].split('\n').map((l) => l.trim().replace(/^'|',?$/g, '')).filter(Boolean) : [];

const FIELDS = [['p', W.R_P], ['a', W.R_A], ['sc', W.R_SC], ['r', W.R_R], ['o', W.R_O], ['h', W.R_H]];

/** One record row, with property *offsets* rather than evaluators. */
function readRecords(table) {
  const out = [];
  if (!table) return out;
  for (const row of table) {
    const mask = S[row];
    let o = row + 2;
    const rec = { i: S[row + 1] };
    rec.n = mask & W.R_NAME ? str[S[o++]] : undefined;
    rec.pr = mask & W.R_PARENT ? S[o++] : undefined;
    for (const [key, bit] of FIELDS) rec[key] = mask & bit ? S[o++] : 0;
    rec.ef = mask & W.R_EFFECTS ? S[o++] : 0;
    out.push(rec);
  }
  return out;
}

/** `[tag, id, fallbackOff, layer+1]` when the property at `off` is T_EXPR. */
function exprAt(off) {
  if (!off) return null;
  if ((S[off] & 7) !== 4) return null;
  return { x: S[off + 1], fb: S[off + 2], l: S[off + 3] ? S[off + 3] - 1 : undefined };
}

const docRecs = readRecords(column(S, S[W.H_LAYERS]));

console.log(`records: ${docRecs.length}`);

const assetsOff = S[W.H_ASSETS];
const assets = [];
if (assetsOff) {
  for (let k = 0, n = S[assetsOff]; k < n; k++) {
    const row = assetsOff + 1 + k * W.A_STRIDE;
    assets.push(readRecords(column(S, S[row + W.A_RECORDS])));
  }
}
const usesOff = S[W.H_USES];
const uses = [];
if (usesOff) {
  for (let u = 0, n = S[usesOff]; u < n; u++) {
    const row = usesOff + 1 + u * W.U_STRIDE;
    uses.push({
      asset: S[row],
      elBase: S[row + W.U_EL_BASE],
      slotBase: S[row + W.U_SLOT_BASE],
      parentSlot: S[row + W.U_PARENT],
    });
  }
}
console.log(`assets: ${assets.length} (${assets.map((a) => a.length).join(', ')} records)  uses: ${uses.length}`);

const show = (recs, label) => {
  console.log(`\n--- ${label}`);
  recs.forEach((r, i) => {
    const parts = [`#${i}`, `ind=${r.i}`];
    if (r.n !== undefined) parts.push(`n=${JSON.stringify(r.n)}`);
    if (r.pr !== undefined) parts.push(`pr=${r.pr}`);
    for (const [key] of FIELDS) {
      const e = exprAt(r[key]);
      if (e) parts.push(`${key}=EXPR{x:${e.x},l:${e.l}}`);
    }

    console.log('  ' + parts.join(' '));
  });
};

if (flags.includes('--records')) {
  show(docRecs, 'document');
  assets.forEach((a, k) => show(a, `asset ${k}`));
  for (const u of uses) {
    console.log(`  use asset=${u.asset} elBase=${u.elBase} slotBase=${u.slotBase} parentSlot=${u.parentSlot}`);
  }
}

if (flags.includes('--exprs')) {
  console.log('\n--- expression sites on records');
  const walk = (recs, label) =>
    recs.forEach((r, i) => {
      for (const [key] of FIELDS) {
        const e = exprAt(r[key]);
        if (e) console.log(`  ${label} rec#${i} .${key} → E[${e.x}] owner l=${e.l}`);
      }
    });
  walk(docRecs, 'doc');
  assets.forEach((a, k) => walk(a, `asset${k}`));
}
