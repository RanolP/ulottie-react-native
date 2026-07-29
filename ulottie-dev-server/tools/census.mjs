// What Lottie features a set of files actually uses.
//
//   node ulottie-dev-server/tools/census.mjs <input.json|dir> [...] [--per-file]
//
// `support::scan` in the compiler answers the narrower question — what is
// *unsupported* — and only for things it already knows to look for. This walks
// the raw document and counts everything, so a feature nobody has thought
// about yet shows up as a row rather than as a rendering difference.

import { readdir, readFile, stat } from 'node:fs/promises';
import * as path from 'node:path';

const SHAPE_TY = {
  gr: 'group', rc: 'rect', el: 'ellipse', sr: 'star/polygon', sh: 'path',
  fl: 'fill', st: 'stroke', gf: 'gradient-fill', gs: 'gradient-stroke',
  tr: 'transform', tm: 'trim', rp: 'repeater', rd: 'rounded-corners',
  mm: 'merge', pb: 'pucker-bloat', tw: 'twist', op: 'offset-path',
  zz: 'zig-zag', no: 'no-op',
};
const LAYER_TY = {
  0: 'precomp', 1: 'solid', 2: 'image', 3: 'null', 4: 'shape',
  5: 'text', 6: 'audio', 13: 'camera',
};

/** Bump `key` in `tally`, remembering one example location. */
const hit = (tally, key, where) => {
  const t = (tally[key] ??= { n: 0, where: [] });
  t.n++;
  if (t.where.length < 3 && where) t.where.push(where);
};

/** True when a Lottie property node is keyframed rather than constant. */
const animated = (p) => !!(p && typeof p === 'object' && p.a === 1);

function walkShapes(items, tally, at) {
  for (const s of items ?? []) {
    if (!s || typeof s !== 'object') continue;
    hit(tally, `shape:${SHAPE_TY[s.ty] ?? s.ty}`, at);
    if (s.ty === 'gr') walkShapes(s.it, tally, at);
    if ((s.ty === 'gf' || s.ty === 'gs')) {
      hit(tally, `gradient:${s.t === 2 ? 'radial' : 'linear'}`, at);
      if (animated(s.g?.k) || (Array.isArray(s.g?.k) && s.g.k.some((k) => k.s && k.t !== undefined))) {
        hit(tally, 'gradient:animated-ramp', at);
      }
      if (s.h !== undefined && s.h?.k !== 0) hit(tally, 'gradient:highlight', at);
    }
    if (s.ty === 'tm' && s.m === 2) hit(tally, 'trim:individually', at);
    if (s.ty === 'st' && s.d?.length) hit(tally, 'stroke:dash', at);
    if (s.ty === 'gs' && s.d?.length) hit(tally, 'stroke:dash', at);
    if (s.ty === 'tr' && (animated(s.sk) || s.sk?.k)) hit(tally, 'transform:skew', at);
    // An expression can sit on any property node, at any depth.
    scanExpr(s, tally, at);
  }
}

/** `x` on any property node is an After Effects expression. */
function scanExpr(node, tally, at, depth = 0) {
  if (!node || typeof node !== 'object' || depth > 6) return;
  if (typeof node.x === 'string' && node.x.length) hit(tally, 'expression', at);
  for (const v of Object.values(node)) {
    if (v && typeof v === 'object') scanExpr(v, tally, at, depth + 1);
  }
}

function walkLayers(layers, tally, scope) {
  for (const l of layers ?? []) {
    const at = `${scope}[${l.ind ?? '?'}]${l.nm ? ` ${l.nm}` : ''}`;
    hit(tally, `layer:${LAYER_TY[l.ty] ?? l.ty}`, at);
    if (l.tt !== undefined) hit(tally, `matte:mode-${l.tt}`, at);
    if (l.td !== undefined) hit(tally, 'matte:source', at);
    if (l.hasMask && l.masksProperties?.length) {
      for (const m of l.masksProperties) hit(tally, `mask:${m.mode ?? 'a'}${m.inv ? '-inv' : ''}`, at);
    }
    if (l.bm) hit(tally, `blend-mode:${l.bm}`, at);
    if (l.tm !== undefined) hit(tally, 'time-remap', at);
    if (l.sr !== undefined && l.sr !== 1) hit(tally, 'time-stretch', at);
    if (l.ao === 1) hit(tally, 'auto-orient', at);
    if (l.ddd === 1) hit(tally, '3d-layer', at);
    if (l.ef?.length) for (const e of l.ef) hit(tally, `effect:${e.ty}`, at);
    if (l.ks?.sk && (l.ks.sk.k || animated(l.ks.sk))) hit(tally, 'transform:skew', at);
    if (Array.isArray(l.ks?.r?.k) && l.ddd === 1) hit(tally, 'transform:3d-rotation', at);
    if (l.ks?.rx || l.ks?.ry || l.ks?.rz) hit(tally, 'transform:3d-rotation', at);
    if (l.ty === 4) walkShapes(l.shapes, tally, at);
    if (l.ty === 5) hit(tally, 'text:layer', at);
    scanExpr(l.ks, tally, at);
    // Hold keyframes (`h:1`) change interpolation, not shape, and are easy to
    // drop silently — worth counting on their own.
    for (const p of Object.values(l.ks ?? {})) {
      if (Array.isArray(p?.k) && p.k.some((kf) => kf && kf.h === 1)) hit(tally, 'keyframe:hold', at);
    }
  }
}

async function census(file) {
  const doc = JSON.parse(await readFile(file, 'utf8'));
  const tally = {};
  hit(tally, `version:${doc.v ?? '?'}`);
  if (doc.fr) hit(tally, `fps:${doc.fr}`);
  walkLayers(doc.layers, tally, 'root');
  for (const [i, a] of (doc.assets ?? []).entries()) {
    if (a.layers) {
      hit(tally, 'asset:precomp', `assets[${i}] ${a.id}`);
      walkLayers(a.layers, tally, `assets[${i}]`);
    } else if (a.p || a.u || a.e !== undefined) {
      hit(tally, a.e === 1 ? 'asset:image-embedded' : 'asset:image-external', `assets[${i}] ${a.p ?? a.id}`);
    }
  }
  if (doc.chars?.length) hit(tally, 'text:glyphs');
  if (doc.fonts?.list?.length) hit(tally, 'text:fonts');
  if (doc.markers?.length) hit(tally, 'markers');
  return tally;
}

const natural = (a, b) => a.localeCompare(b, undefined, { numeric: true });

async function expand(inputs) {
  const files = [];
  for (const raw of inputs) {
    const p = path.resolve(raw);
    const st = await stat(p);
    if (st.isDirectory()) {
      files.push(...(await readdir(p)).filter((e) => e.endsWith('.json')).sort(natural)
        .map((e) => path.join(p, e)));
    } else files.push(p);
  }
  return files;
}

const argv = process.argv.slice(2);
const perFile = argv.includes('--per-file');
const files = await expand(argv.filter((a) => !a.startsWith('--')));
if (!files.length) {
  console.error('usage: census.mjs <input.json|dir> [...] [--per-file]');
  process.exit(1);
}

const all = new Map();
for (const f of files) {
  const name = path.basename(f, '.json');
  const t = await census(f);
  all.set(name, t);
  if (perFile) {
    console.log(`\n=== ${name}`);
    for (const [k, v] of Object.entries(t).sort()) {
      console.log(`  ${k.padEnd(30)} ${String(v.n).padStart(4)}  ${v.where.join(' · ')}`);
    }
  }
}

// The cross-tab is the useful view when deciding what to build next: a feature
// one file needs is a curiosity, a feature six need is the next commit.
const keys = [...new Set([...all.values()].flatMap((t) => Object.keys(t)))].sort();
const names = [...all.keys()];
const w = Math.max(...keys.map((k) => k.length));
console.log('\n' + ' '.repeat(w) + '  ' + names.map((n) => n.replace(/^car-/, '').padStart(4)).join(''));
for (const k of keys) {
  const row = names.map((n) => (all.get(n)[k] ? String(all.get(n)[k].n).padStart(4) : '   ·')).join('');
  console.log(k.padEnd(w) + '  ' + row);
}
