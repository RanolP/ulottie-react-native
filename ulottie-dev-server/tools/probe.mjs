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

const [, , file, ...flags] = process.argv;
if (!file) {
  console.error('usage: probe.mjs <module.js> [--records] [--exprs] [--assets]');
  process.exit(1);
}

const src = readFileSync(file, 'utf8');
const m = src.match(/const D = ([\s\S]*?);\n\n(?:const E|export const)/) ||
  src.match(/const D=(\{[\s\S]*?\});\n/);
if (!m) throw new Error('no payload found in ' + file);
const D = JSON.parse(m[1]);
const S = dec(D.d);
const str = D.s || [];

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

const docTable = column(S, S[W.H_LAYERS], true);
const docRecs = readRecords(docTable);
const scopes = column(S, S[W.H_SCOPES], true);

console.log(`records: ${docRecs.length}  scopes column: ${scopes ? scopes.length : 'absent'}`);

const assetsOff = S[W.H_ASSETS];
const assets = [];
if (assetsOff) {
  for (let k = 0, n = S[assetsOff]; k < n; k++) {
    const row = assetsOff + 1 + k * 5;
    assets.push(readRecords(column(S, S[row + 4], true)));
  }
}
const usesOff = S[W.H_USES];
const uses = [];
if (usesOff) {
  for (let u = 0, n = S[usesOff]; u < n; u++) {
    const row = usesOff + 1 + u * 6;
    uses.push({ asset: S[row], elBase: S[row + 1], recBase: S[row + 2], slotBase: S[row + 3], parentSlot: S[row + 4], scope: S[row + 5] });
  }
}
console.log(`assets: ${assets.length} (${assets.map((a) => a.length).join(', ')} records)  uses: ${uses.length}`);

const show = (recs, label, scopeOf) => {
  console.log(`\n--- ${label}`);
  recs.forEach((r, i) => {
    const parts = [`#${i}`, `ind=${r.i}`];
    if (r.n !== undefined) parts.push(`n=${JSON.stringify(r.n)}`);
    if (r.pr !== undefined) parts.push(`pr=${r.pr}`);
    for (const [key] of FIELDS) {
      const e = exprAt(r[key]);
      if (e) parts.push(`${key}=EXPR{x:${e.x},l:${e.l}}`);
    }
    if (scopeOf) parts.push(`scope=${scopeOf(i)}`);
    console.log('  ' + parts.join(' '));
  });
};

if (flags.includes('--records')) {
  show(docRecs, 'document', (i) => (scopes ? scopes[i] : 0));
  assets.forEach((a, k) => show(a, `asset ${k}`, null));
  for (const u of uses) console.log(`  use asset=${u.asset} recBase=${u.recBase} scope=${u.scope}`);
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
