#!/usr/bin/env node
// Build the standalone static demo. Produces a `dist/` folder that is
// self-sufficient — drop it on any static host (CDN, GitHub Pages, S3 +
// CloudFront, `python -m http.server`, …) and `compare-all.html` runs
// the full comparison via the in-browser wasm compiler. No backend.
//
// Pipeline:
//   1. wasm-pack build the compiler crate with `--features wasm,eval`.
//   2. Mirror public/ into dist/.
//   3. Drop the freshly built pkg/ into dist/wasm/.
//   4. Copy _fixtures/animations/*.json into dist/_fixtures/.
//
// Run with `yarn workspace ulottie-dev-server build:demo` from the
// workspace root, or `node build-demo.mjs` from this directory.

import { spawn } from 'node:child_process';
import { cp, mkdir, readdir, rm, stat } from 'node:fs/promises';
import * as path from 'node:path';

const __dirname = import.meta.dirname;
const workspaceRoot = path.dirname(__dirname);
const compilerDir = path.join(workspaceRoot, 'ulottie-compiler');
const fixturesSrc = path.join(workspaceRoot, '_fixtures', 'animations');
const publicDir = path.join(__dirname, 'public');
const distDir = path.join(__dirname, 'dist');

async function run(cmd, args, opts = {}) {
  await new Promise((resolve, reject) => {
    const child = spawn(cmd, args, { stdio: 'inherit', ...opts });
    child.on('exit', (code) => {
      if (code === 0) resolve();
      else reject(new Error(`${cmd} ${args.join(' ')} exited ${code}`));
    });
    child.on('error', reject);
  });
}

async function exists(path) {
  try { await stat(path); return true; } catch { return false; }
}

// wasm-pack 0.15.0's `--out-dir` flag maps onto cargo's unstable
// `--artifact-dir`, which the stable toolchain rejects. Build into the
// crate's default pkg/ and move it ourselves.
console.log('→ wasm-pack build');
const pkgDir = join(compilerDir, 'pkg');
await rm(pkgDir, { recursive: true, force: true });
await run(
  'wasm-pack',
  [
    'build', '--release', '--target', 'web',
    '--no-default-features', '--features', 'wasm,eval',
  ],
  { cwd: compilerDir },
);

console.log('→ rsync public/ → dist/');
await rm(distDir, { recursive: true, force: true });
await mkdir(distDir, { recursive: true });
for (const entry of await readdir(publicDir, { withFileTypes: true })) {
  // Skip any stale public/wasm/ — wasm-pack just rebuilt fresh into pkg/.
  if (entry.name === 'wasm') continue;
  const src = join(publicDir, entry.name);
  const dst = join(distDir, entry.name);
  await cp(src, dst, { recursive: true });
}

console.log('→ install wasm-pack output → dist/wasm/');
await cp(pkgDir, join(distDir, 'wasm'), { recursive: true });

console.log('→ copy _fixtures/animations/ → dist/_fixtures/');
const fixturesDst = join(distDir, '_fixtures');
await mkdir(fixturesDst, { recursive: true });
for (const entry of await readdir(fixturesSrc)) {
  if (!entry.endsWith('.json')) continue;
  await cp(join(fixturesSrc, entry), join(fixturesDst, entry));
}

if (!(await exists(join(distDir, 'wasm', 'ulottie_compiler_bg.wasm')))) {
  throw new Error('wasm-pack output missing — check the build log above');
}

console.log('\n✓ demo ready at', distDir);
console.log('  serve with any static host, e.g.:');
console.log(`    cd ${distDir} && python3 -m http.server 8000`);
console.log('    open http://127.0.0.1:8000/compare-all.html');
