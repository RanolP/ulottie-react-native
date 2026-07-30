// What Lottie features a set of files actually uses.
//
//   node ulottie-dev-server/tools/census.mjs <input.json|dir> [...] [--per-file]
//   node ulottie-dev-server/tools/census.mjs --coverage
//
// `support::scan` in the compiler answers the narrower question — what is
// *unsupported* — and only for things it already knows to look for. This walks
// the raw document and counts everything, so a feature nobody has thought
// about yet shows up as a row rather than as a rendering difference.
//
// It is also importable: `census(file)` and [`FEATURES`] are what the coverage
// gate in `tests/coverage.spec.ts` reads, so "which Lottie constructs does the
// fixture set exercise" is a number rather than a judgement call.

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

/**
 * Every construct the coverage gate tracks, and what it means.
 *
 * A closed list, deliberately: the point is that adding a row here without a
 * fixture to match fails the build, so "we should test that some day" becomes a
 * line in `_fixtures/coverage.json` with a reason attached instead of a memory.
 *
 * The census emits some keys that are open-ended rather than a feature —
 * `version:`, `fps:`, `effect:<ty>` (After Effects has hundreds), `blend-mode:`
 * — and those are counted but not tracked; see [`OPEN_ENDED`].
 */
export const FEATURES = {
  'layer:precomp': 'a layer instancing a composition',
  'layer:solid': 'a solid-colour layer',
  'layer:image': 'an image layer',
  'layer:null': 'a null layer, transform only',
  'layer:shape': 'a shape layer',
  'layer:text': 'a text layer',
  'layer:audio': 'an audio layer',
  'layer:camera': 'a camera layer',

  'shape:group': 'a shape group',
  'shape:rect': 'a rectangle',
  'shape:ellipse': 'an ellipse',
  'shape:star/polygon': 'a polystar',
  'shape:path': 'a bezier path',
  'shape:fill': 'a solid fill',
  'shape:stroke': 'a solid stroke',
  'shape:gradient-fill': 'a gradient fill',
  'shape:gradient-stroke': 'a gradient stroke',
  'shape:transform': "a group's own transform",
  'shape:trim': 'a trim-path modifier',
  'shape:repeater': 'a repeater modifier',
  'shape:rounded-corners': 'a round-corners modifier',
  'shape:merge': 'a merge-paths modifier',
  'shape:pucker-bloat': 'a pucker/bloat modifier',
  'shape:twist': 'a twist modifier',
  'shape:offset-path': 'an offset-path modifier',
  'shape:zig-zag': 'a zig-zag modifier',
  'shape:no-op': 'a no-op style',

  'gradient:linear': 'a linear gradient',
  'gradient:radial': 'a radial gradient',
  'gradient:animated-ramp': 'a gradient whose stops move',
  'gradient:highlight': 'a radial gradient with a displaced focus',

  'mask:a': 'an additive layer mask',
  'mask:s': 'a subtractive layer mask',
  'mask:i': 'an intersecting layer mask',
  'mask:n': 'a mask set to none',
  'mask:a-inv': 'an inverted additive mask',
  'mask:s-inv': 'an inverted subtractive mask',

  'matte:source': 'a layer marked `td`, drawn only as a matte',
  'matte:mode-1': 'alpha matte',
  'matte:mode-2': 'alpha matte, inverted',
  'matte:mode-3': 'luma matte',
  'matte:mode-4': 'luma matte, inverted',

  'asset:precomp': 'a precomposition asset',
  'asset:image-embedded': 'an image asset carried as a data URI',
  'asset:image-external': 'an image asset referenced by path',

  'transform:skew': 'a skewed transform',
  'transform:3d-rotation': 'separate X/Y/Z rotation',
  '3d-layer': 'a layer flagged 3D',
  'auto-orient': 'a layer that turns to face its motion path',
  'time-remap': "a precomp on its own remapped clock",
  'time-stretch': 'a layer playing at a non-unit rate',

  'keyframe:hold': 'a held (step) keyframe',
  'stroke:dash': 'a dashed stroke',
  'trim:individually': 'trim applied per shape rather than across the group',
  'expression': 'an expression on any property',
  'markers': 'composition markers',
  'text:layer': 'a text layer, seen from the layer side',
  'text:fonts': 'an embedded font list',
  'text:glyphs': 'embedded glyph outlines',
};

/**
 * Keys the census emits that name a value rather than a capability. Counting
 * them is useful; requiring a fixture per After Effects effect id is not.
 */
export const OPEN_ENDED = ['version:', 'fps:', 'effect:', 'blend-mode:'];

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

export async function census(file) {
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

// Importable above this line, a command below it.
if (import.meta.main) await main();

async function main() {
const argv = process.argv.slice(2);
if (argv.includes('--coverage')) {
  await coverage();
  process.exit(0);
}
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
}

/**
 * Where the committed fixtures stand against [`FEATURES`], and what each gap
 * leaves untested. The gate in `tests/coverage.spec.ts` asserts the *shape* of
 * this — that every gap is documented and every documented gap is real; this
 * prints it, which is the part a person wants when deciding what to write next.
 */
async function coverage() {
  const root = path.resolve(path.dirname(new URL(import.meta.url).pathname), '../../_fixtures');
  const dir = path.join(root, 'animations');
  const seen = new Set();
  for (const f of (await readdir(dir)).filter((e) => e.endsWith('.json'))) {
    for (const k of Object.keys(await census(path.join(dir, f)))) seen.add(k);
  }
  const { uncovered } = JSON.parse(await readFile(path.join(root, 'coverage.json'), 'utf8'));
  const all = Object.keys(FEATURES);
  const gap = all.filter((f) => !seen.has(f));

  console.log(`\nfeature coverage: ${all.length - gap.length} of ${all.length} exercised by _fixtures/animations\n`);
  const w = Math.max(...gap.map((f) => f.length));
  for (const kind of ['implemented', 'not implemented', 'rejected']) {
    const rows = gap.filter((f) => (uncovered[f] ?? '').startsWith(kind));
    if (!rows.length) continue;
    console.log(`  ${kind} — ${rows.length}`);
    for (const f of rows) {
      console.log(`    ${f.padEnd(w)}  ${uncovered[f].slice(kind.length + 2)}`);
    }
    console.log();
  }
  const rest = gap.filter((f) => !/^(implemented|not implemented|rejected)/.test(uncovered[f] ?? ''));
  if (rest.length) console.log('  unclassified —', rest.join(', '), '\n');
}
