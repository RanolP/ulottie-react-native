#!/usr/bin/env node
// Diff every captured parity cell against the lottie-react-native crop with
// odiff and write .artifacts/parity_table.json. Two players per cell: `svg`
// (reanimated-aot, `_u` crop) and `skia` (skia-aot, `_s` crop); skia-only
// fixtures have no svg player, so no svg row. Gate: <=1% differing pixels
// with antialiasing tolerance.
import { execFileSync } from 'child_process';
import fs from 'fs';
import path from 'path';
import { fileURLToPath } from 'url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../..');
const dir = path.join(root, 'examples/compare/.artifacts/parity');
const odiff = path.join(root, 'node_modules/odiff-bin/bin/odiff');
const fixtures = [
  'boucing_ball', 'rectangle', 'ellipse', 'fill', 'trim_path', 'android_wave',
  'precomp_star_circle', 'gradient_radial', 'lottie_logo_1', 'mask_subtract',
  'matte_alpha', 'stroke_under_fill',
  // Skia-only — the svg target refuses these, so they diff skia vs lottie only.
  'blend_multiply', 'gradient_animated', 'matte_luma_inv', 'fx_effects',
  'image_embedded',
];
const skiaOnly = new Set([
  'blend_multiply', 'gradient_animated', 'matte_luma_inv', 'fx_effects',
  'image_embedded',
]);
const pcts = [0, 25, 50, 75, 100];

// Cells whose gap traces to lottie-ios vs lottie-web semantics, not to a
// ulottie bug. Key: `${fixture}:${frame}` — both ulottie players render the
// exact fractional frame, so a documented cell covers svg and skia alike.
const documented = {
  // 50% of op=177 pins frame 88.5. ulottie renders the exact fractional
  // frame (lottie-web playback semantics; the web oracle diffs 0.000% here),
  // while lottie-ios quantizes a paused fractional frame to a whole frame —
  // on this fast-moving segment (~1.8 user units per half frame) the half
  // frame of motion shows as a uniform offset of the whole artwork
  // (stroke_under_fill_50_diff.png). At every integer-frame pin the two
  // renderers agree at baseline.
  'stroke_under_fill:50': 'lottie-ios rounds paused fractional frames; ulottie renders exact frame 88.5',
};

const rows = [];
function diffCell(fixture, frame, player, crop, diffName) {
  const a = path.join(dir, `${fixture}_${frame}_${crop}.png`);
  const l = path.join(dir, `${fixture}_${frame}_l.png`);
  const d = path.join(dir, diffName);
  if (!fs.existsSync(a) || !fs.existsSync(l)) {
    rows.push({ fixture, frame, player, diffPct: null, verdict: 'fail', note: 'capture missing' });
    return;
  }
  let out = '';
  try {
    out = execFileSync(odiff, ['--aa', '--parsable-stdout', a, l, d], { encoding: 'utf8' });
  } catch (e) {
    if (e.status === 22) out = e.stdout; // differences found
    else {
      rows.push({ fixture, frame, player, diffPct: null, verdict: 'fail', note: `odiff exit ${e.status}: ${e.stderr}` });
      return;
    }
  }
  // Identical images print a bare "0"; differences print "count;pct".
  const parts = out.trim().split(';');
  const diffPct = parts.length > 1 ? Number(parts[1]) : 0;
  const doc = documented[`${fixture}:${frame}`];
  const verdict = diffPct <= 1 ? 'pass' : doc ? 'documented-divergence' : 'fail';
  rows.push({ fixture, frame, player, diffPct, verdict, note: (verdict !== 'pass' && doc) || '' });
}

for (const fixture of fixtures) {
  for (const frame of pcts) {
    if (!skiaOnly.has(fixture)) {
      diffCell(fixture, frame, 'svg', 'u', `${fixture}_${frame}_diff.png`);
    }
    diffCell(fixture, frame, 'skia', 's', `${fixture}_${frame}_sdiff.png`);
  }
}
fs.writeFileSync(path.join(dir, '..', 'parity_table.json'), JSON.stringify(rows, null, 2) + '\n');
for (const r of rows) console.log(`${r.fixture}\t${r.frame}\t${r.player}\t${r.diffPct}\t${r.verdict}\t${r.note}`);
