'use strict';
// The pure compile step of the Metro integration: no Metro imports, so it is
// unit-testable (scripts/check.mjs) and reusable outside a bundler.

const { execFileSync } = require('child_process');
const crypto = require('crypto');
const fs = require('fs');
const os = require('os');
const path = require('path');

function isLottieFile(filename) {
  return filename.endsWith('.lottie.json');
}

function resolveCompilerBin() {
  const bin =
    process.env.ULOTTIE_COMPILER_BIN ||
    path.join(__dirname, '..', '..', 'target', 'release', 'ulottie-compiler');
  if (!fs.existsSync(bin)) {
    throw new Error(
      `ulottie: compiler binary not found at ${bin}\n` +
        'Run `cargo build --release -p ulottie-compiler` at the repo root, ' +
        'or point ULOTTIE_COMPILER_BIN at the binary.',
    );
  }
  return bin;
}

/**
 * Compile Lottie JSON source into an AOT ES module.
 *
 * @param {string | Buffer} src the .lottie.json contents
 * @param {string} displayName used in error messages, and to pick the target:
 *   a `*.skia.lottie.json` name compiles as 'skia-aot' regardless of `opts`
 * @param {{ allow?: string[], target?: string }} [opts] `allow` accepts the
 *   compiler's named degradations (`--allow <name>` per entry) — e.g.
 *   'track-matte-inverted'; `target` defaults to 'reanimated-aot'
 * @returns {string} the generated module source
 */
function compileLottie(src, displayName, opts) {
  const bin = resolveCompilerBin();
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'ulottie-'));
  try {
    const input = path.join(dir, 'in.json');
    const output = path.join(dir, 'out.js');
    fs.writeFileSync(input, src);
    const target = /\.skia\.lottie\.json$/.test(displayName || '')
      ? 'skia-aot'
      : (opts && opts.target) || 'reanimated-aot';
    const args = ['--target', target];
    for (const name of (opts && opts.allow) || []) args.push('--allow', name);
    args.push('--output', output, input);
    try {
      execFileSync(bin, args, {
        stdio: ['ignore', 'pipe', 'pipe'],
      });
    } catch (e) {
      const stderr = e.stderr ? e.stderr.toString() : e.message;
      throw new Error(`ulottie: compiling ${displayName || 'lottie json'} failed:\n${stderr}`);
    }
    return reorderWorkletDecls(stubCutSymbols(fs.readFileSync(output, 'utf8')));
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
}

// Mirror of the compiler's gated runtime symbols (ulottie-compiler/src/backend/
// shake.rs, `GATED`). The shake cuts a gated symbol's *declaration* and relies
// on every call site being guarded (`x.expr ? xcol(…) : null`) — sound in a
// lazily-resolving JS engine, but the react-native-worklets babel plugin
// builds each worklet's closure eagerly at module load, so Hermes evaluates
// the cut name and throws `Property 'xcol' doesn't exist` before anything is
// called. Until the reanimated-aot emitter retains (or stubs) these itself,
// declare a throwing stub for any gated name the module references but does
// not declare; the guard means it is never actually called.
const GATED_RUNTIME_SYMBOLS = [
  'EASE',
  'spBuild',
  'spSample',
  'spSeg',
  'lerpPath',
  'rectPath',
  'ellipsePath',
  'starPath',
  'trimTable',
  'trimApply',
  'trimCols',
  'trim',
  'trimChainCols',
  'trimChainWin',
  'expand',
  'xcol',
  'column',
  'toComp',
  'fromCompToSurface',
  'pointOnPath',
  'tangentOnPath',
  'createPath',
];

// The worklets babel plugin rewrites every top-level `function` declaration
// into a factory assignment plus an eager `__closure = {…}` object at the
// declaration's source position — hoisting is gone, so a worklet that
// captures a function declared LATER in the file captures `undefined` (the
// generated `pvv` captured `pslice` this way and threw `undefined is not a
// function` on the UI thread). Plain JS never notices because function
// declarations hoist, which also makes reordering them semantically free.
// So: lift every top-level function declaration out, topologically sort by
// who-references-whom, and re-insert the group after all other statements
// (consts they capture) and before the exports (which capture them).
function reorderWorkletDecls(js) {
  const lines = js.split('\n');
  const fns = []; // { name, start, end } inclusive line ranges
  for (let i = 0; i < lines.length; i++) {
    const m = /^function ([A-Za-z_$][\w$]*)\s*\(/.exec(lines[i]);
    if (!m) continue;
    let end = i;
    if (!/\}\s*;?\s*$/.test(lines[i]) || lines[i].indexOf('{') === -1) {
      while (end < lines.length && !/^\}/.test(lines[end])) end++;
    } else if (!balanced(lines[i])) {
      while (end < lines.length && !/^\}/.test(lines[end])) end++;
    }
    fns.push({ name: m[1], start: i, end });
    i = end;
  }
  if (fns.length === 0) return js;

  const bodies = new Map(
    fns.map((f) => [f.name, lines.slice(f.start, f.end + 1).join('\n')]),
  );
  const names = new Set(bodies.keys());
  const deps = new Map();
  for (const [name, body] of bodies) {
    const d = new Set();
    for (const other of names) {
      if (other !== name && new RegExp(`(?<![.\\w$])${other}\\b`).test(body)) d.add(other);
    }
    deps.set(name, d);
  }
  // Kahn's algorithm, original order as tie-break; cycle members (mutual
  // recursion — ordering cannot fix those anyway) keep their original order.
  const order = [];
  const placed = new Set();
  let progress = true;
  while (progress) {
    progress = false;
    for (const f of fns) {
      if (placed.has(f.name)) continue;
      if ([...deps.get(f.name)].every((d) => placed.has(d))) {
        order.push(f);
        placed.add(f.name);
        progress = true;
      }
    }
  }
  for (const f of fns) if (!placed.has(f.name)) order.push(f);

  const isFnLine = new Array(lines.length).fill(false);
  for (const f of fns) for (let i = f.start; i <= f.end; i++) isFnLine[i] = true;
  const rest = [];
  let insertAt = -1;
  for (let i = 0; i < lines.length; i++) {
    if (isFnLine[i]) continue;
    if (insertAt === -1 && /^export /.test(lines[i])) insertAt = rest.length;
    rest.push(lines[i]);
  }
  const fnText = order.map((f) => bodies.get(f.name));
  if (insertAt === -1) return rest.concat(fnText).join('\n');
  return rest
    .slice(0, insertAt)
    .concat(fnText, rest.slice(insertAt))
    .join('\n');
}

function balanced(line) {
  let n = 0;
  for (const ch of line) {
    if (ch === '{') n++;
    else if (ch === '}') n--;
  }
  return n === 0;
}

function stubCutSymbols(js) {
  const stubs = GATED_RUNTIME_SYMBOLS.filter(
    (name) =>
      !new RegExp(`function ${name}\\s*\\(`).test(js) &&
      // A bare call — `xcol(`, not `.trim(` or a declaration.
      new RegExp(`(?<![.\\w$])${name}\\s*\\(`).test(js),
  ).map(
    (name) =>
      `function ${name}() { 'worklet'; throw new Error('ulottie: ${name} was tree-shaken out and must never be called'); }`,
  );
  if (stubs.length === 0) return js;
  return `${js}\n// Stubs for tree-shaken gated runtime symbols (see metro/compile.js).\n${stubs.join('\n')}\n`;
}

function sha256File(p) {
  return crypto.createHash('sha256').update(fs.readFileSync(p)).digest('hex');
}

/**
 * Cache-key fragment covering the transformer sources, the compiler binary,
 * and the compile options, so rebuilding the Rust compiler or changing the
 * allow list busts Metro's transform cache.
 */
function cacheKeyFragment(opts) {
  return [
    sha256File(__filename),
    sha256File(path.join(__dirname, 'transform-worker.js')),
    sha256File(resolveCompilerBin()),
    JSON.stringify((opts && opts.allow) || []),
    (opts && opts.target) || 'reanimated-aot',
  ].join('$');
}

module.exports = { isLottieFile, resolveCompilerBin, compileLottie, cacheKeyFragment };
