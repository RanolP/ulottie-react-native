// Compare any Lottie file's compiled output against lottie-web, frame by frame.
//
//   node ulottie-dev-server/tools/compare.mjs <input.json|dir> [...] [options]
//
// The visual suite in `tests/` is a gate: it asks whether the eleven fixtures
// still pass. This asks the other question — *where* does an arbitrary file
// diverge, and what is different about the DOM there. It takes files from
// anywhere, needs no fixture registration, and reports rather than throws.
//
// Options
//   --frames <n>      sample count across the timeline (default 9)
//   --at <list>       explicit frames: integers, or 0..1 fractions
//   --size <px>       panel size, square (default 400)
//   --variant <v>     extern | embedded | extracted | instanced (default extern)
//   --out <dir>       report directory (default ./.compare)
//   --tolerance <r>   ratio above which a frame is called a failure (0.005)
//   --dom             also diff the two SVG trees at the worst frame
//   --headed          run the browser visibly
//   --keep            keep the report from a previous run instead of clearing
//   --json            print machine-readable results to stdout
//   --quiet           suppress the per-frame table
//
// Writes `<out>/index.html` — every input, every sampled frame, reference and
// candidate and odiff mask side by side, with the structural diff underneath.

import { execFile } from 'node:child_process';
import { createReadStream } from 'node:fs';
import { mkdir, readdir, readFile, rm, stat, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import * as path from 'node:path';
import { promisify } from 'node:util';

import { compare as odiffCompare } from 'odiff-bin';
import { chromium } from 'playwright';

const execFileAsync = promisify(execFile);

const here = import.meta.dirname;
const devServer = path.dirname(here);
const workspace = path.dirname(devServer);
const compilerDir = path.join(workspace, 'ulottie-compiler');
const compilerBin = path.join(workspace, 'target', 'release', 'ulottie-compiler');

// ---------------------------------------------------------------- arguments

function parseArgs(argv) {
  const opts = {
    inputs: [],
    frames: 9,
    at: null,
    size: 400,
    variant: 'extern',
    out: path.join(process.cwd(), '.compare'),
    tolerance: 0.005,
    dom: false,
    headed: false,
    keep: false,
    json: false,
    quiet: false,
  };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    const val = () => argv[++i];
    switch (a) {
      case '--frames': opts.frames = Number(val()); break;
      case '--at': opts.at = val().split(',').map(Number); break;
      case '--size': opts.size = Number(val()); break;
      case '--variant': opts.variant = val(); break;
      case '--out': opts.out = path.resolve(val()); break;
      case '--tolerance': opts.tolerance = Number(val()); break;
      case '--dom': opts.dom = true; break;
      case '--headed': opts.headed = true; break;
      case '--keep': opts.keep = true; break;
      case '--json': opts.json = true; break;
      case '--quiet': opts.quiet = true; break;
      case '-h': case '--help': usage(); process.exit(0);
      default:
        if (a.startsWith('-')) { console.error(`unknown option ${a}`); process.exit(1); }
        opts.inputs.push(a);
    }
  }
  if (!opts.inputs.length) { usage(); process.exit(1); }
  return opts;
}

function usage() {
  console.error(
    'usage: compare.mjs <input.json|dir> [...] [--frames n] [--at 0,0.5,1] [--size px]\n' +
    '                   [--variant extern|embedded|extracted|instanced] [--out dir]\n' +
    '                   [--tolerance r] [--dom] [--headed] [--keep] [--json] [--quiet]',
  );
}

/** Expand directories into their `.json` children; keep files as given. */
async function expand(inputs) {
  const files = [];
  for (const raw of inputs) {
    const p = path.resolve(raw);
    const st = await stat(p).catch(() => null);
    if (!st) throw new Error(`no such file: ${raw}`);
    if (st.isDirectory()) {
      const entries = (await readdir(p)).filter((e) => e.endsWith('.json')).sort(natural);
      files.push(...entries.map((e) => path.join(p, e)));
    } else {
      files.push(p);
    }
  }
  return files;
}

/** `car-2` before `car-10`. Plain sort puts them the other way round. */
const natural = (a, b) =>
  a.localeCompare(b, undefined, { numeric: true, sensitivity: 'base' });

// ---------------------------------------------------------------- compiling

/**
 * Compile one input into `dir` — and if the compiler refuses, again with every
 * feature it named allowed.
 *
 * That refusal is the most useful thing this tool prints, so it is captured
 * rather than fatal: an animation that needs `image-layer` still renders, just
 * without the images, and seeing *that* next to lottie-web's render is the
 * whole point. There is no `--check` flag to ask with; the rejection itself
 * carries the list, which beats reimplementing the scan here and having the
 * two drift.
 */
async function compileOne(input, name, dir, variant) {
  const out = path.join(dir, `${name}.js`);
  const args = [input, '-o', out];
  if (variant === 'embedded') args.push('--embedded');
  if (variant === 'instanced') args.push('--instance-precomps');
  if (variant === 'extracted') args.push('--extract', path.join(dir, `${name}.sprite.svg`),
                                         '--symbol-id', name);

  const run = (extra) =>
    execFileAsync(compilerBin, [...args, ...extra], { maxBuffer: 64 << 20 });

  try {
    await run([]);
    return { ok: true, findings: [], module: out };
  } catch (err) {
    const findings = parseFindings(err.stderr || '');
    if (!findings.length) {
      return { ok: false, findings, error: (err.stderr || err.message || '').trim() };
    }
    const allow = [...new Set(findings.map((f) => f.feature))].join(',');
    try {
      await run(['--allow', allow]);
      return { ok: true, findings, module: out };
    } catch (err2) {
      return { ok: false, findings, error: (err2.stderr || err2.message || '').trim() };
    }
  }
}

/** `  image-layer          layers[3] `car.png`` → `{feature, where}`. */
function parseFindings(stderr) {
  const out = [];
  let inList = false;
  for (const line of stderr.split('\n')) {
    if (/^unsupported Lottie features:/.test(line) || /^Error: unsupported/.test(line)) {
      inList = true;
      continue;
    }
    if (!inList) continue;
    if (/^\s*$/.test(line)) break;                       // the list ends at the blank line
    const m = /^\s{2}(\S+)\s+(.*)$/.exec(line);
    if (m) out.push({ feature: m[1], where: m[2].trim() });
  }
  return out;
}

// ------------------------------------------------------------------ serving

const MIME = {
  '.js': 'text/javascript', '.mjs': 'text/javascript', '.json': 'application/json',
  '.svg': 'image/svg+xml', '.html': 'text/html', '.png': 'image/png',
  '.wasm': 'application/wasm', '.css': 'text/css',
};

/**
 * Serve the scratch dir, the runtime, and lottie-web from one origin.
 *
 * A compiled module imports `./runtime/*.js` relative to itself, and
 * lottie-web has to come from somewhere the page can reach — mounting the
 * three real directories is less machinery than a bundler and keeps the module
 * byte-identical to what the CLI wrote.
 */
function serve(roots) {
  const server = createServer(async (req, res) => {
    const url = new URL(req.url, 'http://localhost');
    const rel = decodeURIComponent(url.pathname).replace(/^\/+/, '');
    for (const [prefix, root] of roots) {
      if (!rel.startsWith(prefix)) continue;
      const file = path.join(root, rel.slice(prefix.length));
      if (!file.startsWith(root)) break;                 // no escaping the mount
      const st = await stat(file).catch(() => null);
      if (!st?.isFile()) continue;
      res.writeHead(200, {
        'content-type': MIME[path.extname(file)] ?? 'application/octet-stream',
        'cache-control': 'no-store',
      });
      createReadStream(file).pipe(res);
      return;
    }
    res.writeHead(404).end('not found');
  });
  return new Promise((resolve) => {
    server.listen(0, '127.0.0.1', () =>
      resolve({ port: server.address().port, close: () => server.close() }),
    );
  });
}

// ------------------------------------------------------------------ the page

/**
 * Two panels, identical geometry, one renderer each.
 *
 * `overflow:hidden` and a fixed square matter: `preserveAspectRatio` resolves
 * against the box, so panels of different sizes would diverge on every pixel
 * for reasons that have nothing to do with the compiler.
 */
const pageHtml = (size) => `<!doctype html>
<meta charset="utf-8">
<title>ulottie compare</title>
<style>
  html,body { margin:0; padding:0; background:#fff; }
  .panel { width:${size}px; height:${size}px; overflow:hidden; background:#fff; }
  #wrap { display:flex; gap:0; align-items:flex-start; }
</style>
<div id="wrap">
  <div class="panel" id="ref"></div>
  <div class="panel" id="cand"></div>
</div>
<script src="/lottie/lottie.min.js"></script>
`;

// Everything below runs in the page. They are arrow functions because
// Playwright serializes them with `toString()`, and method shorthand does not
// survive that — `async load(a) {…}` is not an expression on the far side.

/** Load both renderers and report the frame count they agree on. */
const pageLoad = async ({ name, src, mod, sprite }) => {
    const w = window;
    w.__cleanup?.();

    document.getElementById('ref').innerHTML = '';
    document.getElementById('cand').innerHTML = '';
    document.querySelectorAll('.sprite-holder').forEach((el) => el.remove());

    if (sprite) {
      const holder = document.createElement('div');
      holder.className = 'sprite-holder';
      holder.style.cssText = 'position:absolute;width:0;height:0;overflow:hidden';
      holder.innerHTML = await fetch(sprite).then((r) => r.text());
      document.body.appendChild(holder);
    }

    const refAnim = w.lottie.loadAnimation({
      container: document.getElementById('ref'),
      renderer: 'svg',
      loop: false,
      autoplay: false,
      path: src,
      rendererSettings: { preserveAspectRatio: 'xMidYMid meet' },
    });
    await new Promise((resolve, reject) => {
      refAnim.addEventListener('DOMLoaded', () => resolve());
      refAnim.addEventListener('data_failed', () => reject(new Error('lottie-web: ' + name)));
      setTimeout(() => reject(new Error('lottie-web timed out: ' + name)), 20000);
    });

    const errors = [];
    const onError = (e) => errors.push(String(e.message ?? e.reason ?? e));
    w.addEventListener('error', onError);
    w.addEventListener('unhandledrejection', onError);

    let cand = null;
    let mountError = null;
    try {
      const m = await import(mod + '?t=' + Date.now());
      cand = m.init(document.getElementById('cand'), { autoplay: false });
    } catch (err) {
      mountError = String(err && err.stack ? err.stack : err);
    }

    refAnim.goToAndStop(0, true);
    cand?.goToFrame?.(0);

    w.__cleanup = () => {
      w.removeEventListener('error', onError);
      w.removeEventListener('unhandledrejection', onError);
      refAnim.destroy();
      cand?.destroy?.();
    };
    w.__anim = { refAnim, cand, errors };

    return {
      totalFrames: Math.round(refAnim.totalFrames || 0),
      candFrames: Math.round(cand?.totalFrames ?? 0),
      frameRate: refAnim.frameRate ?? 0,
      mountError,
      errors,
    };
};

/** Drive both to `f` and settle. Two rAFs: one to run, one to paint. */
const pageSeek = async (f) => {
  const { refAnim, cand } = window.__anim;
  refAnim.goToAndStop(f, true);
  cand?.goToFrame?.(f);
  await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
  return window.__anim.errors.slice();
};

/** Both SVG trees, serialized, for the structural diff. */
const pageDump = () => {
  const get = (id) => document.querySelector('#' + id + ' svg')?.outerHTML ?? '';
  return { ref: get('ref'), cand: get('cand') };
};

// --------------------------------------------------------------- structural

/**
 * A shallow, order-preserving summary of an SVG tree: one line per element,
 * indented by depth, carrying the attributes that decide what is drawn.
 *
 * lottie-web wraps everything in its own `<g>` scaffolding and ulottie does
 * not, so a literal tree diff is all noise. Reducing both to *drawing*
 * elements — the leaves that actually paint, with their resolved geometry and
 * paint attributes — leaves two sequences that should correspond one to one,
 * and the first place they stop corresponding is the bug.
 */
const PAINTED = new Set(['path', 'rect', 'circle', 'ellipse', 'polygon', 'polyline',
                         'line', 'image', 'text', 'use']);
const KEEP = ['d', 'points', 'x', 'y', 'width', 'height', 'cx', 'cy', 'r', 'rx', 'ry',
              'fill', 'fill-rule', 'stroke', 'stroke-width',
              'stroke-linecap', 'stroke-linejoin', 'stroke-dasharray',
              'stroke-dashoffset', 'opacity', 'transform', 'href', 'xlink:href',
              'preserveAspectRatio', 'mask', 'clip-path', 'filter', 'display',
              'visibility'];

// A `<path>` inside one of these is a clip or a matte, not a mark. lottie-web
// clips every layer to the composition and every precomp to its own bounds, so
// counting those as drawing put four phantom rectangles in front of every
// comparison and knocked the two sequences out of step from element zero.
const NONRENDERING = new Set(['defs', 'clipPath', 'mask', 'symbol', 'pattern', 'marker']);

function outline(svgText) {
  // No DOM here, and pulling in a parser for a summary is more dependency than
  // the summary is worth. Tags are matched textually, which is sound for
  // serialized SVG: it has no script or CDATA to confuse the scan.
  const lines = [];
  const tagRe = /<([a-zA-Z][\w:-]*)((?:\s+[^>]*?)?)(\/?)>|<\/([a-zA-Z][\w:-]*)>/g;
  const stack = [];
  let ctm = IDENT;
  let hidden = 0;
  // Effective opacity down the tree. An element under `opacity="0"` is not on
  // the screen, and counting it makes the diff argue about marks nobody can
  // see — car-4 staggers four masked layers and fades the three that are not
  // its turn, so half of every comparison was invisible geometry.
  let alpha = 1;
  let m;
  while ((m = tagRe.exec(svgText))) {
    const [, open, attrs, selfClose, close] = m;
    if (close) {
      const popped = stack.pop();
      if (popped) {
        ctm = popped.ctm;
        alpha = popped.alpha;
        if (NONRENDERING.has(popped.tag)) hidden--;
      }
      continue;
    }
    const own = /\stransform="([^"]*)"/.exec(attrs);
    const here = own ? mul(ctm, parseTransform(own[1])) : ctm;
    const op = /\sopacity="([^"]*)"/.exec(attrs);
    const gone = /\sdisplay="none"/.test(attrs) || /\svisibility="hidden"/.test(attrs);
    const here_a = gone ? 0 : alpha * (op ? Number(op[1]) : 1);

    if (!hidden && here_a > 0 && PAINTED.has(open)) {
      const raw = {};
      const kept = [];
      for (const key of KEEP) {
        const av = new RegExp(`\\s${key.replace(':', '\\:')}="([^"]*)"`).exec(attrs);
        if (!av) continue;
        raw[key] = av[1];
        if (key !== 'd' && key !== 'points' && key !== 'transform') {
          kept.push(`${key}=${normalize(key, av[1])}`);
        }
      }
      const box = bbox(open, raw);
      // The box in the composition's own coordinates, not the element's. Two
      // renderers nest their groups differently and hang the transform at
      // different depths, so a local box says nothing about where the mark
      // lands; the product of the chain does, and it is what the eye compares.
      const abs = box ? apply(here, box) : null;
      if (abs) kept.unshift(`at=${abs.map(round).join()}`);
      lines.push({ depth: stack.length, tag: open, attrs: kept, box: abs, painted: true });
    }
    if (!selfClose) {
      stack.push({ tag: open, ctm, alpha });
      ctm = here;
      alpha = here_a;
      if (NONRENDERING.has(open)) hidden++;
    }
  }
  return lines;
}

// --- 2×3 affine, as `[a, b, c, d, e, f]` (the SVG matrix() order).

const IDENT = [1, 0, 0, 1, 0, 0];

const mul = (p, q) => [
  p[0] * q[0] + p[2] * q[1],
  p[1] * q[0] + p[3] * q[1],
  p[0] * q[2] + p[2] * q[3],
  p[1] * q[2] + p[3] * q[3],
  p[0] * q[4] + p[2] * q[5] + p[4],
  p[1] * q[4] + p[3] * q[5] + p[5],
];

/** The four transformed corners' axis-aligned box. */
function apply(m, [x0, y0, x1, y1]) {
  const pt = (x, y) => [m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]];
  const cs = [pt(x0, y0), pt(x1, y0), pt(x0, y1), pt(x1, y1)];
  return [
    Math.min(...cs.map((c) => c[0])), Math.min(...cs.map((c) => c[1])),
    Math.max(...cs.map((c) => c[0])), Math.max(...cs.map((c) => c[1])),
  ];
}

/** `translate(2,3) rotate(30) matrix(…)` → one matrix. */
function parseTransform(s) {
  let m = IDENT;
  const re = /([a-zA-Z]+)\s*\(([^)]*)\)/g;
  let t;
  while ((t = re.exec(s))) {
    const n = t[2].split(/[\s,]+/).filter(Boolean).map(Number);
    const rad = (d) => (d * Math.PI) / 180;
    switch (t[1]) {
      case 'matrix': m = mul(m, n.slice(0, 6)); break;
      case 'translate': m = mul(m, [1, 0, 0, 1, n[0] || 0, n[1] || 0]); break;
      case 'scale': m = mul(m, [n[0] ?? 1, 0, 0, n[1] ?? n[0] ?? 1, 0, 0]); break;
      case 'rotate': {
        const [c, si] = [Math.cos(rad(n[0] || 0)), Math.sin(rad(n[0] || 0))];
        let r = [c, si, -si, c, 0, 0];
        if (n.length > 1) {
          r = mul(mul([1, 0, 0, 1, n[1], n[2]], r), [1, 0, 0, 1, -n[1], -n[2]]);
        }
        m = mul(m, r);
        break;
      }
      case 'skewX': m = mul(m, [1, 0, Math.tan(rad(n[0] || 0)), 1, 0, 0]); break;
      case 'skewY': m = mul(m, [1, Math.tan(rad(n[0] || 0)), 0, 1, 0, 0]); break;
    }
  }
  return m;
}

/**
 * Put an attribute into a form the two renderers can be compared in.
 *
 * They agree on the picture and disagree on everything about how to write it
 * down: lottie-web emits ` M0,45 C24.85300064086914,45 …` and full-precision
 * `rgb(255,111,15)`, the compiler emits `M0,45C24.853,45…` and `#ff6f0f`.
 * Without this every attribute of every element reads as a difference and the
 * diff says nothing.
 */
function normalize(key, raw) {
  let v = raw.trim();
  if (key === 'href' || key === 'xlink:href') {
    // An embedded image is a hundred kilobytes of base64 whose first eighty
    // characters are the PNG header, so truncating it makes every image in a
    // sequence look like the same one. A digest of the whole thing does not.
    return v.startsWith('data:') ? `${v.slice(0, v.indexOf(';'))}#${digest(v)}` : v;
  }
  if (key === 'width' || key === 'height') return String(parseFloat(v));
  if (key === 'transform') {
    v = v.replace(/(-?\d*\.?\d+(?:e[-+]?\d+)?)/gi, (n) => round(Number(n)))
         .replace(/\s+/g, '');
  } else if (/^(fill|stroke)$/.test(key)) {
    v = color(v);
  }
  return trunc(v);
}

const round = (n) => String(Math.round(n * 100) / 100);

/** FNV-1a, base36. Enough to tell two images apart in a report. */
function digest(s) {
  let h = 0x811c9dc5;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 0x01000193) >>> 0;
  }
  return h.toString(36);
}

/**
 * The box an element's coordinates span — its identity, for alignment.
 *
 * Comparing `d` as text does not work and cannot be made to: lottie-web writes
 * every segment as a cubic, the compiler writes `L` where a segment is
 * straight, and the compiler emits `<rect rx="18">` where lottie-web emits the
 * rounded rectangle as a path. Those are the same picture written three ways.
 * The box they occupy is the same number either way, so that is what the two
 * sequences are matched on — and a real geometry bug moves it.
 */
function bbox(tag, attrs) {
  // `1536px` as readily as `1536`: lottie-web writes an `<image>`'s size with
  // units and the compiler writes it bare.
  const num = (k) => parseFloat(attrs[k] ?? '');
  if (tag === 'rect' || tag === 'image') {
    const [x, y, w, h] = [num('x') || 0, num('y') || 0, num('width'), num('height')];
    return [x, y, x + w, y + h];
  }
  if (tag === 'ellipse' || tag === 'circle') {
    const [cx, cy] = [num('cx') || 0, num('cy') || 0];
    const rx = tag === 'circle' ? num('r') : num('rx');
    const ry = tag === 'circle' ? num('r') : num('ry');
    return [cx - rx, cy - ry, cx + rx, cy + ry];
  }
  const data = attrs.d ?? attrs.points;
  if (!data) return null;
  // Lottie output is M/L/C/Z only, so every number is a coordinate and they
  // alternate x, y. Control points are included, which overstates a curve's
  // box — identically on both sides, which is all this needs.
  const ns = (data.match(/-?\d*\.?\d+(?:e[-+]?\d+)?/gi) ?? []).map(Number);
  if (ns.length < 2) return null;
  let [x0, y0, x1, y1] = [Infinity, Infinity, -Infinity, -Infinity];
  for (let i = 0; i + 1 < ns.length; i += 2) {
    x0 = Math.min(x0, ns[i]); x1 = Math.max(x1, ns[i]);
    y0 = Math.min(y0, ns[i + 1]); y1 = Math.max(y1, ns[i + 1]);
  }
  return [x0, y0, x1, y1];
}

/** `rgb(255,111,15)` / `#f60` / `rgba(…)` → one canonical `#rrggbb[aa]`. */
function color(v) {
  const m = /^rgba?\(([^)]*)\)$/i.exec(v);
  if (m) {
    const parts = m[1].split(',').map((s) => Number(s.trim()));
    const hex = parts.slice(0, 3).map((n) => Math.round(n).toString(16).padStart(2, '0')).join('');
    const a = parts[3];
    return '#' + hex + (a === undefined || a >= 1 ? '' : Math.round(a * 255).toString(16).padStart(2, '0'));
  }
  const s = /^#([0-9a-f]{3,4})$/i.exec(v);
  if (s) return '#' + [...s[1]].map((c) => c + c).join('');
  return v.toLowerCase();
}

const trunc = (s) => (s.length > 90 ? s.slice(0, 87) + '…' : s);

/**
 * Line up the two painted sequences and report where they part company.
 *
 * Counts first, because "37 painted elements against 41" localizes a missing
 * feature faster than any attribute diff. Then an alignment rather than a
 * positional walk: one element the compiler failed to emit shifts every
 * element after it, and comparing index to index turns a single missing shape
 * into a page of differences that are all the same bug.
 */
function structuralDiff(refSvg, candSvg, limit = 24) {
  const a = outline(refSvg);
  const b = outline(candSvg);
  const rows = [];
  if (a.length !== b.length) {
    rows.push({ kind: 'count', ref: `${a.length} painted`, cand: `${b.length} painted` });
  }
  const byTag = (list) => {
    const t = {};
    for (const l of list) t[l.tag] = (t[l.tag] ?? 0) + 1;
    return t;
  };
  const ta = byTag(a), tb = byTag(b);
  for (const tag of [...new Set([...Object.keys(ta), ...Object.keys(tb)])].sort()) {
    if ((ta[tag] ?? 0) !== (tb[tag] ?? 0)) {
      rows.push({ kind: 'tag', tag, ref: String(ta[tag] ?? 0), cand: String(tb[tag] ?? 0) });
    }
  }

  for (const op of align(a, b)) {
    if (rows.length >= limit) break;
    if (op.kind === 'same') continue;
    rows.push({
      kind: op.kind === 'change' ? 'attr' : op.kind,
      at: op.ai ?? op.bi,
      ref: op.a ? render(op.a) : '—',
      cand: op.b ? render(op.b) : '—',
    });
  }
  return rows;
}

const render = (l) => `${l.tag} ${l.attrs.join(' ')}`;

/**
 * Identity of an element for alignment: what it *is*, not how it is painted.
 *
 * Geometry survives a colour bug and a colour survives a geometry bug, so
 * matching on both would refuse to pair up the very elements worth pairing.
 * The tag is left out for the same reason — a rounded rectangle is a `<rect>`
 * on one side and a `<path>` on the other, and those two should pair.
 */
const key = (l) => {
  // Where there is a source, it is part of what the element *is*: a sequence
  // of frames drawn at one spot is one box and many pictures.
  const src = l.attrs.find((s) => /^(href|xlink:href)=/.test(s));
  const box = l.box ? l.box.map(round).join() : l.tag;
  return src ? `${box}|${src.slice(src.indexOf('#'))}` : box;
};

/** Longest-common-subsequence alignment over element identity. */
function align(a, b) {
  const n = a.length, m = b.length;
  // Quadratic, but these are tens to low hundreds of elements per frame.
  const ka = a.map(key), kb = b.map(key);
  const lcs = Array.from({ length: n + 1 }, () => new Int32Array(m + 1));
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      lcs[i][j] = ka[i] === kb[j] ? lcs[i + 1][j + 1] + 1
        : Math.max(lcs[i + 1][j], lcs[i][j + 1]);
    }
  }
  const ops = [];
  let i = 0, j = 0;
  while (i < n && j < m) {
    if (ka[i] === kb[j]) {
      const same = render(a[i]) === render(b[j]);
      ops.push({ kind: same ? 'same' : 'change', a: a[i], b: b[j], ai: i, bi: j });
      i++; j++;
    } else if (lcs[i + 1][j] >= lcs[i][j + 1]) {
      ops.push({ kind: 'missing', a: a[i], ai: i });          // in ref, not in ours
      i++;
    } else {
      ops.push({ kind: 'extra', b: b[j], bi: j });            // in ours, not in ref
      j++;
    }
  }
  for (; i < n; i++) ops.push({ kind: 'missing', a: a[i], ai: i });
  for (; j < m; j++) ops.push({ kind: 'extra', b: b[j], bi: j });
  return ops;
}

// ------------------------------------------------------------------- report

const esc = (s) => String(s).replace(/[&<>]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;' }[c]));

function reportHtml(results, opts) {
  const badge = (r) =>
    r.error ? `<span class="b bad">error</span>`
    : r.worst > opts.tolerance ? `<span class="b bad">${pct(r.worst)}</span>`
    : `<span class="b ok">${pct(r.worst)}</span>`;

  const findingRows = (r) =>
    !r.findings.length ? ''
    : `<details class="findings"><summary>${r.findings.length} unsupported feature${
        r.findings.length > 1 ? 's' : ''} — ${
        [...new Set(r.findings.map((f) => f.feature))].map(esc).join(', ')}</summary>
       <table>${r.findings.map((f) =>
         `<tr><td class="feat">${esc(f.feature)}</td><td>${esc(f.where)}</td></tr>`).join('')}
       </table></details>`;

  const frameCards = (r) => r.frames.map((f) => `
    <figure class="frame${f.ratio > opts.tolerance ? ' fail' : ''}">
      <figcaption>frame ${f.frame} <b>${pct(f.ratio)}</b></figcaption>
      <div class="shots">
        <div><img src="${f.refPng}" loading="lazy"><span>lottie-web</span></div>
        <div><img src="${f.candPng}" loading="lazy"><span>ulottie</span></div>
        <div>${f.diffPng ? `<img src="${f.diffPng}" loading="lazy">` : '<div class="nodiff">identical</div>'}<span>diff</span></div>
      </div>
    </figure>`).join('');

  const domBlock = (r) => !r.dom?.length ? '' : `
    <details class="dom" open><summary>structural diff at frame ${r.domFrame}</summary>
      <table class="domtable">
        <tr><th></th><th>lottie-web</th><th>ulottie</th></tr>
        ${r.dom.map((d) => `<tr class="r-${d.kind}">
          <td class="k">${
            d.kind === 'tag' ? esc(d.tag)
            : d.kind === 'count' ? 'count'
            : `${d.kind === 'missing' ? '−' : d.kind === 'extra' ? '+' : '~'}#${d.at}`}</td>
          <td>${esc(d.ref)}</td><td>${esc(d.cand)}</td></tr>`).join('')}
      </table></details>`;

  return `<!doctype html>
<meta charset="utf-8"><title>ulottie compare</title>
<style>
  :root { color-scheme: light dark; }
  body { font: 13px/1.5 ui-monospace, SFMono-Regular, Menlo, monospace; margin: 0; padding: 24px;
         background: Canvas; color: CanvasText; }
  h1 { font-size: 15px; margin: 0 0 4px; }
  .sub { opacity: .6; margin: 0 0 20px; }
  section { border: 1px solid color-mix(in srgb, CanvasText 18%, transparent);
            border-radius: 8px; margin-bottom: 16px; overflow: hidden; }
  section > header { display: flex; gap: 12px; align-items: baseline; padding: 10px 14px;
                     background: color-mix(in srgb, CanvasText 5%, transparent); }
  section > header h2 { font-size: 13px; margin: 0; }
  .b { padding: 1px 7px; border-radius: 99px; font-size: 11px; }
  .ok { background: color-mix(in srgb, #16a34a 25%, transparent); }
  .bad { background: color-mix(in srgb, #dc2626 30%, transparent); }
  .meta { opacity: .55; font-size: 11px; }
  .body { padding: 12px 14px; }
  .frames { display: flex; flex-wrap: wrap; gap: 14px; }
  figure { margin: 0; }
  figcaption { font-size: 11px; opacity: .7; margin-bottom: 4px; }
  .frame.fail figcaption b { color: #dc2626; }
  .shots { display: flex; gap: 4px; }
  .shots > div { display: flex; flex-direction: column; align-items: center; gap: 2px; }
  .shots span { font-size: 9px; opacity: .45; }
  .shots img, .nodiff { width: 150px; height: 150px; object-fit: contain;
       border: 1px solid color-mix(in srgb, CanvasText 15%, transparent);
       background: #fff; image-rendering: pixelated; }
  .nodiff { display: grid; place-items: center; font-size: 10px; opacity: .4; }
  details { margin-top: 10px; }
  summary { cursor: pointer; opacity: .75; }
  table { border-collapse: collapse; font-size: 11px; margin-top: 6px; width: 100%; }
  td, th { border: 1px solid color-mix(in srgb, CanvasText 15%, transparent);
           padding: 2px 6px; text-align: left; vertical-align: top;
           word-break: break-all; }
  th { opacity: .6; font-weight: normal; }
  .feat { white-space: nowrap; color: #d97706; }
  .k { white-space: nowrap; opacity: .6; }
  .r-missing { background: color-mix(in srgb, #dc2626 12%, transparent); }
  .r-extra   { background: color-mix(in srgb, #2563eb 12%, transparent); }
  .r-count, .r-tag { background: color-mix(in srgb, #d97706 12%, transparent); }
  .err { color: #dc2626; white-space: pre-wrap; }
</style>
<h1>ulottie vs lottie-web</h1>
<p class="sub">${results.length} animation${results.length > 1 ? 's' : ''} ·
  ${opts.variant} build · ${opts.size}px panels · tolerance ${pct(opts.tolerance)}</p>
${results.map((r) => `
<section>
  <header>
    <h2>${esc(r.name)}</h2> ${badge(r)}
    <span class="meta">${r.totalFrames} frames · ${esc(path.relative(process.cwd(), r.input))}</span>
  </header>
  <div class="body">
    ${r.error ? `<div class="err">${esc(r.error)}</div>` : ''}
    ${findingRows(r)}
    <div class="frames">${frameCards(r)}</div>
    ${domBlock(r)}
  </div>
</section>`).join('')}
`;
}

const pct = (r) => (r * 100).toFixed(3) + '%';

// --------------------------------------------------------------------- main

async function main() {
  const opts = parseArgs(process.argv.slice(2));
  const inputs = await expand(opts.inputs);

  if (!(await stat(compilerBin).catch(() => null))) {
    process.stderr.write('building the compiler…\n');
    await execFileAsync('cargo', ['build', '--release', '-p', 'ulottie-compiler', '-q'],
                        { cwd: workspace });
  }

  if (!opts.keep) await rm(opts.out, { recursive: true, force: true });
  const shotsDir = path.join(opts.out, 'shots');
  const buildDir = path.join(opts.out, 'build');
  await mkdir(shotsDir, { recursive: true });
  await mkdir(buildDir, { recursive: true });

  // The compiled module imports `./runtime/*` — relative to itself, so it
  // resolves under `build/`. Mounting the real directory there keeps the
  // module byte-identical to what the CLI wrote; rewriting its imports would
  // mean testing something else.
  const srv = await serve([
    ['lottie/', path.join(workspace, 'node_modules', 'lottie-web', 'build', 'player')],
    ['build/runtime/', path.join(compilerDir, 'runtime')],
    ['build/', buildDir],
  ]);
  const origin = `http://127.0.0.1:${srv.port}`;

  const browser = await chromium.launch({ headless: !opts.headed });
  const page = await browser.newPage({
    viewport: { width: opts.size * 2 + 40, height: opts.size + 40 },
    deviceScaleFactor: 1,
  });
  await page.route('**/index.html', (route) =>
    route.fulfill({ contentType: 'text/html', body: pageHtml(opts.size) }));
  await page.goto(origin + '/index.html');

  const results = [];
  for (const input of inputs) {
    const name = path.basename(input, '.json');
    process.stderr.write(`\n${name}\n`);

    const built = await compileOne(input, name, buildDir, opts.variant);
    const rec = {
      name, input, findings: built.findings, frames: [], worst: 0,
      error: built.ok ? null : built.error, totalFrames: 0, dom: null, domFrame: null,
    };
    results.push(rec);
    if (!built.ok) { process.stderr.write(`  compile failed\n`); continue; }

    // Copying the source next to the module keeps every fetch same-origin and
    // means a file from anywhere on disk needs no extra mount.
    await writeFile(path.join(buildDir, `${name}.json`), await readFile(input));

    let loaded;
    try {
      loaded = await page.evaluate(pageLoad, {
        name,
        src: `${origin}/build/${name}.json`,
        mod: `${origin}/build/${name}.js`,
        sprite: opts.variant === 'extracted' ? `${origin}/build/${name}.sprite.svg` : null,
      });
    } catch (err) {
      rec.error = String(err.message ?? err);
      process.stderr.write(`  load failed: ${rec.error}\n`);
      continue;
    }
    rec.totalFrames = loaded.totalFrames;
    if (loaded.mountError) {
      rec.error = loaded.mountError;
      process.stderr.write(`  mount failed: ${loaded.mountError.split('\n')[0]}\n`);
    }

    const frames = pickFrames(loaded.totalFrames, opts);
    const refEl = page.locator('#ref');
    const candEl = page.locator('#cand');

    for (const f of frames) {
      const errs = await page.evaluate(pageSeek, f);
      if (errs.length && !rec.error) rec.error = errs.join('\n');

      const refPng = path.join(shotsDir, `${name}-${f}-ref.png`);
      const candPng = path.join(shotsDir, `${name}-${f}-cand.png`);
      const diffPng = path.join(shotsDir, `${name}-${f}-diff.png`);
      await refEl.screenshot({ path: refPng });
      await candEl.screenshot({ path: candPng });

      const res = await odiffCompare(refPng, candPng, diffPng, {
        antialiasing: true, threshold: 0.1,
      });
      const ratio = res.match ? 0 : (res.diffPercentage ?? 100) / 100;
      rec.frames.push({
        frame: f, ratio,
        refPng: path.relative(opts.out, refPng),
        candPng: path.relative(opts.out, candPng),
        diffPng: res.match ? null : path.relative(opts.out, diffPng),
      });
      rec.worst = Math.max(rec.worst, ratio);
    }

    if (opts.dom && rec.frames.length) {
      const worst = rec.frames.reduce((a, b) => (b.ratio > a.ratio ? b : a));
      rec.domFrame = worst.frame;
      await page.evaluate(pageSeek, worst.frame);
      const { ref, cand } = await page.evaluate(pageDump);
      rec.dom = structuralDiff(ref, cand);
      await writeFile(path.join(opts.out, `${name}.ref.svg`), ref);
      await writeFile(path.join(opts.out, `${name}.cand.svg`), cand);
    }

    if (!opts.quiet) printTable(rec, opts);
    if (opts.dom && rec.dom?.length) printDom(rec);
  }

  await browser.close();
  srv.close();

  await writeFile(path.join(opts.out, 'index.html'), reportHtml(results, opts));

  if (opts.json) {
    process.stdout.write(JSON.stringify(results.map((r) => ({
      name: r.name, worst: r.worst, error: r.error,
      findings: r.findings, frames: r.frames.map((f) => ({ frame: f.frame, ratio: f.ratio })),
    })), null, 2) + '\n');
  }

  process.stderr.write(`\nreport: ${path.join(opts.out, 'index.html')}\n`);
  summarize(results, opts);
}

/** Sample frames: explicit list, or `frames` evenly spaced over the timeline. */
function pickFrames(total, opts) {
  if (!total) return [0];
  if (opts.at) {
    return [...new Set(opts.at.map((v) =>
      Math.max(0, Math.min(total - 1, v > 0 && v < 1 ? Math.round(v * (total - 1)) : Math.round(v)))))];
  }
  const n = Math.max(1, opts.frames);
  // `total - 1` rather than `total`: lottie-web clamps at the last frame, and
  // sampling past it would compare two clamped renders and call it agreement.
  return [...new Set(Array.from({ length: n }, (_, i) =>
    Math.round((i / Math.max(1, n - 1)) * (total - 1))))];
}

function printTable(rec, opts) {
  for (const f of rec.frames) {
    const flag = f.ratio > opts.tolerance ? '✗' : ' ';
    process.stderr.write(`  ${flag} frame ${String(f.frame).padStart(4)}  ${pct(f.ratio).padStart(9)}\n`);
  }
}

/** The structural diff, for a terminal rather than the report page. */
function printDom(rec) {
  process.stderr.write(`  ── structure @ frame ${rec.domFrame}\n`);
  for (const d of rec.dom) {
    if (d.kind === 'count') {
      process.stderr.write(`     count  lottie-web ${d.ref}, ulottie ${d.cand}\n`);
    } else if (d.kind === 'tag') {
      process.stderr.write(`     <${d.tag}>  lottie-web ${d.ref}, ulottie ${d.cand}\n`);
    } else if (d.kind === 'missing') {
      process.stderr.write(`     −#${d.at} ${d.ref}\n`);
    } else if (d.kind === 'extra') {
      process.stderr.write(`     +#${d.at} ${d.cand}\n`);
    } else {
      process.stderr.write(`     ~#${d.at} ref  ${d.ref}\n              ours ${d.cand}\n`);
    }
  }
}

function summarize(results, opts) {
  const w = Math.max(...results.map((r) => r.name.length));
  process.stderr.write('\n');
  for (const r of results) {
    const state = r.error ? 'ERROR'
      : r.worst > opts.tolerance ? `DIFF ${pct(r.worst)}`
      : `ok   ${pct(r.worst)}`;
    const feats = r.findings.length
      ? `  [${[...new Set(r.findings.map((f) => f.feature))].join(' ')}]` : '';
    process.stderr.write(`  ${r.name.padEnd(w)}  ${state}${feats}\n`);
  }
}

await main();
