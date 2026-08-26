#!/usr/bin/env node
// Runnable check: the pure compile step of the Metro transformer produces a
// valid ES module honoring the reanimated-aot contract (tree/meta/init).
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import { createRequire } from 'node:module';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(here, '..', '..');
const require = createRequire(import.meta.url);
const { compileLottie, isLottieFile, resolveCompilerBin } = require('../metro/compile.js');

// 1. Locate the compiler binary; build it if missing.
try {
  resolveCompilerBin();
} catch {
  console.log('compiler binary missing — running `cargo build --release -p ulottie-compiler`');
  const r = spawnSync('cargo', ['build', '--release', '-p', 'ulottie-compiler'], {
    cwd: repoRoot,
    stdio: 'inherit',
  });
  if (r.status !== 0) {
    console.error('cargo build failed');
    process.exit(1);
  }
}

// 2. Interception predicate + pure compile step.
assert.equal(isLottieFile('assets/ball.lottie.json'), true);
assert.equal(isLottieFile('assets/data.json'), false);
const fixture = path.join(repoRoot, '_fixtures', 'animations', 'boucing_ball.json');
const js = compileLottie(fs.readFileSync(fixture), 'boucing_ball.lottie.json');

assert.match(js, /'worklet'/, "generated module carries 'worklet' directives");

// 3. Valid ES module with exactly the contract's three exports.
const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'ulottie-check-'));
let mod;
try {
  const modPath = path.join(tmp, 'boucing_ball.mjs');
  fs.writeFileSync(modPath, js);
  mod = await import(pathToFileURL(modPath).href);
} finally {
  fs.rmSync(tmp, { recursive: true, force: true });
}

assert.deepEqual(Object.keys(mod).sort(), ['init', 'meta', 'tree']);
assert.equal(mod.tree.type, 'Svg');
assert.ok(mod.meta.fr > 0, 'meta.fr > 0');
assert.equal(typeof mod.init, 'function');

// 4. The runtime half of the contract: init() hands back appliable handles.
const h = mod.init();
assert.equal(typeof h.apply, 'function');
assert.ok(Array.isArray(h.dirty));
h.apply(h.ip);
for (const el of h.dirty) {
  assert.equal(typeof el.i, 'number');
  assert.equal(typeof el.p, 'object');
  el.d = 0;
}
h.dirty.length = 0;

// 5. The shared-init split: a second init() gets fresh per-instance state
// while the once-per-runtime cache holds exactly one entry for the payload.
const h2 = mod.init();
assert.notEqual(h2.els, h.els, 'per-instance element handles are fresh');
h2.apply(h2.ip);
assert.equal(
  Object.keys(globalThis.__ulottie || {}).length,
  1,
  'one shared cache entry per module payload',
);

console.log('ok: compile step + module contract (tree/meta/init) verified on boucing_ball');

// 6. The skia-aot target, selected by the `*.skia.lottie.json` naming
// convention: same fixture, dl/meta/init contract, and a draw() that issues
// at least one geometry draw against a mock Skia factory + canvas.
const skiaJs = compileLottie(fs.readFileSync(fixture), 'boucing_ball.skia.lottie.json');
assert.match(skiaJs, /'worklet'/, "skia module carries 'worklet' directives");

let skiaMod;
const tmp2 = fs.mkdtempSync(path.join(os.tmpdir(), 'ulottie-check-'));
try {
  const modPath = path.join(tmp2, 'boucing_ball.skia.mjs');
  fs.writeFileSync(modPath, skiaJs);
  skiaMod = await import(pathToFileURL(modPath).href);
} finally {
  fs.rmSync(tmp2, { recursive: true, force: true });
}

assert.deepEqual(Object.keys(skiaMod).sort(), ['dl', 'init', 'meta']);
assert.ok(skiaMod.meta.fr > 0, 'skia meta.fr > 0');
assert.equal(typeof skiaMod.init, 'function');

// A structural mock of the RN Skia factory: enough surface for skPrepare /
// skPaints / skSet, no rendering.
function mockPaint() {
  const p = { getAlphaf: () => 1 };
  for (const m of [
    'setAlphaf', 'setAntiAlias', 'setBlendMode', 'setColor', 'setColorFilter',
    'setImageFilter', 'setPathEffect', 'setShader', 'setStrokeCap',
    'setStrokeJoin', 'setStrokeMiter', 'setStrokeWidth', 'setStyle',
  ]) {
    p[m] = () => {};
  }
  return p;
}
const mockSkia = {
  Paint: mockPaint,
  // The runtime mutates the returned color (`col[3] = fo` in skFxPaint), so
  // the mock must hand back a real Float32Array like RN Skia does.
  Color: () => new Float32Array(4),
  Point: (x, y) => ({ x, y }),
  Matrix: (m) => m || [1, 0, 0, 0, 1, 0, 0, 0, 1],
  XYWHRect: (x, y, width, height) => ({ x, y, width, height }),
  Path: { MakeFromSVGString: (d) => ({ d, setFillType: () => {} }) },
  ColorFilter: { MakeMatrix: (m) => ({ m }), MakeCompose: (o, i) => ({ o, i }) },
  ImageFilter: {
    MakeBlur: () => ({}),
    MakeDropShadow: () => ({}),
  },
  Data: { fromBase64: () => ({}) },
  Image: { MakeImageFromEncoded: () => ({ width: () => 16, height: () => 16 }) },
  PathEffect: { MakeDash: (iv, phase) => ({ iv, phase }) },
  Shader: {
    MakeLinearGradient: () => ({}),
    MakeRadialGradient: () => ({}),
  },
};

const sh = skiaMod.init(mockSkia);
assert.equal(typeof sh.apply, 'function');
assert.equal(typeof sh.draw, 'function');
sh.apply(sh.ip);
for (const el of sh.dirty) el.d = 0;
sh.dirty.length = 0;

let draws = 0;
const counting = () => { draws++; };
const mockCanvas = {
  save: () => {},
  restore: () => {},
  saveLayer: () => {},
  concat: () => {},
  clipPath: () => {},
  clipRect: () => {},
  drawPath: counting,
  drawRect: counting,
  drawRRect: counting,
  drawOval: counting,
  drawImageRect: counting,
};
sh.draw(mockCanvas);
assert.ok(draws >= 1, 'skia draw() issues at least one geometry draw');

console.log('ok: skia-aot compile step + module contract (dl/meta/init) verified on boucing_ball');

// 7. Skia-only capabilities — the constructs react-native-svg refuses:
// phase 2's blend/gradient/matte/effects plus phase 3's embedded image.
// Each fixture compiles without an allow-gate, applies its first frame, and
// drains through draw() against the same mock without throwing.
for (const name of [
  'blend_multiply', 'gradient_animated', 'matte_luma_inv', 'fx_effects', 'image_embedded',
]) {
  const src = fs.readFileSync(path.join(repoRoot, '_fixtures', 'animations', `${name}.json`));
  const modJs = compileLottie(src, `${name}.skia.lottie.json`);
  const tmpN = fs.mkdtempSync(path.join(os.tmpdir(), 'ulottie-check-'));
  let m;
  try {
    const modPath = path.join(tmpN, `${name}.skia.mjs`);
    fs.writeFileSync(modPath, modJs);
    m = await import(pathToFileURL(modPath).href);
  } finally {
    fs.rmSync(tmpN, { recursive: true, force: true });
  }
  const H = m.init(mockSkia);
  H.apply(H.ip);
  for (const el of H.dirty) el.d = 0;
  H.dirty.length = 0;
  draws = 0;
  H.draw(mockCanvas);
  assert.ok(draws >= 1, `${name}: skia draw() issues at least one geometry draw`);
}

console.log(
  'ok: skia-only caps (blend mode, animated gradient, inverted matte, layer effects, embedded image) drain',
);

// 8. No forward references between generated top-level worklet functions.
// The worklets babel plugin rewrites `function f() { 'worklet'; ... }` into a
// factory ASSIGNMENT that eagerly captures f's free variables at module
// evaluation, so a worklet referencing one defined later captures `undefined`
// on the UI runtime (crashes there only — plain Node resolves names at call
// time, which is why the sections above cannot catch it).
for (const [name, js] of [
  ['boucing_ball.rn', compileLottie(fs.readFileSync(fixture), 'boucing_ball.lottie.json')],
  ['boucing_ball.skia', compileLottie(fs.readFileSync(fixture), 'boucing_ball.skia.lottie.json')],
  ['fx_effects.skia', compileLottie(
    fs.readFileSync(path.join(repoRoot, '_fixtures', 'animations', 'fx_effects.json')),
    'fx_effects.skia.lottie.json',
  )],
]) {
  const lines = js.split('\n');
  const defs = new Map();
  lines.forEach((l, i) => {
    const m = l.match(/^function ([A-Za-z_$][\w$]*)\(/);
    if (m) defs.set(m[1], i);
  });
  for (const [fn, def] of defs) {
    const re = new RegExp(`\\b${fn}\\b`);
    for (let i = 0; i < def; i++) {
      const l = lines[i].trim();
      if (l.startsWith('//') || l.startsWith('*') || l.startsWith('/*')) continue;
      assert.ok(
        !re.test(lines[i]),
        `${name}: worklet ${fn} (line ${def + 1}) is referenced at line ${i + 1} before its assignment`,
      );
    }
  }
}
console.log('ok: generated modules order worklet functions definition-before-reference');
