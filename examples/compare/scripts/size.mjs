// Bundle-size metric: raw Lottie JSON bytes vs the compiled module bytes for
// both AOT targets (reanimated-aot "svg" and skia-aot), raw and gzipped, per
// fixture. Run manually: node scripts/size.mjs
import { createRequire } from 'node:module';
import { readFileSync, readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { gzipSync } from 'node:zlib';
import path from 'node:path';

const require = createRequire(import.meta.url);
const { compileLottie } = require('ulottie-react-native/metro/compile');

const assets = path.join(path.dirname(fileURLToPath(import.meta.url)), '..', 'assets');
const files = readdirSync(assets);
// `.lottie.json` twins compile for the svg target, `.skia.lottie.json` twins
// for skia-aot; skia-only fixtures (no `.lottie.json`) get skia numbers only.
const svgNames = new Set(
  files
    .filter((f) => f.endsWith('.lottie.json') && !f.endsWith('.skia.lottie.json'))
    .map((f) => f.replace(/\.lottie\.json$/, '')),
);
const skiaNames = new Set(
  files.filter((f) => f.endsWith('.skia.lottie.json')).map((f) => f.replace(/\.skia\.lottie\.json$/, '')),
);
const names = [...new Set([...svgNames, ...skiaNames])].sort();

const gz = (buf) => gzipSync(buf, { level: 9 }).length;
const ratio = (a, b) => Math.round((a / b) * 100) / 100;

const rows = names.map((name) => {
  const src = readFileSync(path.join(assets, `${name}.json`));
  const row = { fixture: name, jsonBytes: src.length, jsonGzip: gz(src) };
  if (svgNames.has(name)) {
    const js = compileLottie(src, `${name}.lottie.json`, {
      allow: ['track-matte-inverted'], // lottie_logo_1 needs it (see metro.config.js)
    });
    row.svgBytes = Buffer.byteLength(js, 'utf8');
    row.svgGzip = gz(Buffer.from(js, 'utf8'));
    row.svgRatio = ratio(row.svgBytes, row.jsonBytes);
  }
  if (skiaNames.has(name)) {
    const js = compileLottie(src, `${name}.skia.lottie.json`);
    row.skiaBytes = Buffer.byteLength(js, 'utf8');
    row.skiaGzip = gz(Buffer.from(js, 'utf8'));
    row.skiaRatio = ratio(row.skiaBytes, row.jsonBytes);
  }
  return row;
});

console.log(JSON.stringify(rows, null, 2));
