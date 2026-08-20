// Comparison app.
//
// `./compiler.js` picks the backend — the Rust dev server when one is proxied,
// the in-browser wasm build otherwise. Both report the same shape (sizes,
// `plan`, `unsupported`), so nothing here cares which answered.

import { type AnimationItem } from 'lottie-web';
import { Bench, type Task } from 'tinybench';

import { lottie } from './lottie.ts';

import { compile } from './compiler.ts';
import { highlight, langOf } from './pretty.ts';
import type {
  CompileResponse,
  CompileResult,
  ManifestEntry,
  Plan,
  Player,
  SizeEntry,
  Sizes,
  UlottieModule,
} from './types.ts';

/** Every id here is in `index.html`; a missing one is a bug, not a case. */
const $ = <T extends HTMLElement = HTMLElement>(id: string): T => {
  const el = document.getElementById(id);
  if (!el) throw new Error(`#${id} is missing from the page`);
  return el as T;
};

const select = $<HTMLSelectElement>('anim-select');
const scrubber = $<HTMLInputElement>('scrubber');
const frameDisplay = $('frame-display');
const totalFramesEl = $('total-frames');
const fileInput = $<HTMLInputElement>('file-input');
const urlInput = $<HTMLInputElement>('url-input');
const urlLoadBtn = $<HTMLButtonElement>('url-load');
const dropHint = $('drop-hint');
const uploadStatus = $('upload-status');

let currentAnim: AnimationItem | null = null;
let ulottieResult: Player | null = null;
let lastJsBlobUrl: string | null = null;
let totalFrames = 0;
// Kept so the benchmark can mount fresh instances of each player.
let animationData: unknown = null;
let ulottieModule: UlottieModule | null = null;
/** A static animation has no frame loop, so there is no frame time to sample. */
let isStatic = false;

async function loadFromSource(jsonText: string, label?: string) {
  $('lottie-ref').innerHTML = '';
  $('ulottie').innerHTML = '';
  if (currentAnim) currentAnim.destroy();
  if (ulottieResult) ulottieResult.destroy();
  currentAnim = ulottieResult = null;
  if (lastJsBlobUrl) {
    URL.revokeObjectURL(lastJsBlobUrl);
    lastJsBlobUrl = null;
  }
  resetPerf();

  let info: CompileResult;
  try {
    info = await compile(jsonText);
  } catch (e) {
    uploadStatus.textContent = 'Compile failed: ' + ((e as Error).message ?? e);
    renderPlan(null);
    renderSizes(null);
    return;
  }
  renderPlan(info);
  renderSizes(info.sizes, info.plan, info);
  void renderAssetHints(info);
  isStatic = info.plan?.is_static ?? false;

  currentAnim = lottie.loadAnimation({
    container: $('lottie-ref'),
    renderer: 'svg',
    loop: false,
    autoplay: false,
    animationData: (animationData = JSON.parse(jsonText)) as object,
  });
  const anim: AnimationItem = currentAnim;
  anim.addEventListener('DOMLoaded', () => {
    // `totalFrames` is a count; the frames themselves are 0..count-1. Labelling
    // the slider with the count made its last position unreachable.
    totalFrames = Math.round(anim.totalFrames);
    const last = Math.max(0, totalFrames - 1);
    scrubber.max = String(last);
    totalFramesEl.textContent = String(last);
    scrubber.value = '0';
    frameDisplay.textContent = '0';
    anim.goToAndStop(0, true);
  });

  try {
    // Prefer the self-contained embedded module: extern output imports the
    // runtime as a module graph, which resolves against the dev server but not
    // against a blob: URL from the in-browser wasm compiler.
    const src = info.js_embedded_url ?? info.js_url;
    const jsUrl = src + (src.startsWith('blob:') ? '' : `?t=${Date.now()}`);
    if (src.startsWith('blob:')) lastJsBlobUrl = src;
    const mod = (ulottieModule = (await import(/* @vite-ignore */ jsUrl)) as UlottieModule);
    ulottieResult = mod.init($('ulottie'), { autoplay: false });
    ulottieResult.goToFrame(0);
  } catch (e) {
    $('ulottie').textContent = 'Error: ' + (e as Error).message;
  }
  if (label) uploadStatus.textContent = 'Loaded: ' + label;
}

const loadFixture = async (name: string) => {
  const res = await fetch('./_fixtures/' + name + '.json');
  if (!res.ok) return void (uploadStatus.textContent = 'Fetch failed: ' + res.status);
  await loadFromSource(await res.text(), name);
};

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

const fmtBytes = (n: number): string =>
  n < 1024 ? n + ' B'
  : n < 1024 * 1024 ? (n / 1024).toFixed(1) + ' KB'
  : (n / 1024 / 1024).toFixed(2) + ' MB';

/**
 * Signed comparison against the baseline, on one line.
 *
 * Past a halving the factor says more than the percentage — "14× smaller" is
 * legible where "−93%" is not — but only ever one of the two, so every row is
 * the same height.
 */
function vsCell(value: number, base: number): string {
  if (!base) return '<td></td>';
  const pct = ((value - base) / base) * 100;
  if (Math.abs(pct) < 0.05) return '<td>same</td>';
  const cls = pct < 0 ? 'gain' : 'loss';
  const text =
    pct < -50 ? `${(base / value).toFixed(1)}× smaller`
    : `${pct > 0 ? '+' : '−'}${Math.abs(pct).toFixed(0)}%`;
  return `<td class="${cls}">${text}</td>`;
}

// ---------------------------------------------------------------------------
// Plan panel — the AOT decisions behind the numbers
// ---------------------------------------------------------------------------

function renderPlan(info: CompileResponse | null) {
  const el = $('plan');
  const plan = info?.plan;
  if (!plan) {
    el.innerHTML =
      '<div class="note">no plan reported</div>';
    return;
  }

  const badges = [];
  if (plan.is_static) {
    badges.push('<span class="badge static">fully static — no runtime, no frame loop</span>');
  }
  if (plan.generated) {
    badges.push('<span class="badge gen">compiled to code — no interpreter, no payload</span>');
  } else if (!plan.is_static) {
    badges.push('<span class="badge muted">interpreter + payload</span>');
  }
  if (plan.instanced) badges.push('<span class="badge">precomps instanced</span>');
  if (plan.templated) badges.push('<span class="badge">subtrees templated</span>');
  if (!badges.length) badges.push('<span class="badge muted">markup inlined, no instancing</span>');

  const facts = ([
    ['elements', plan.elements],
    ['bindings', plan.bindings],
    ['layer records', plan.records],
  ] as [string, number][])
    .filter(([, v]) => v > 0)
    .map(([k, v]) => `<div class="fact"><b>${v.toLocaleString()}</b>${k}</div>`)
    .join('');

  const caps = plan.caps.length
    ? plan.caps.map((c: string) => `<span class="chip on">${c}</span>`).join('')
    : '<span class="chip">none</span>';

  const mods = plan.modules.length
    ? plan.modules.map((m: string) => `<span class="chip">${m}</span>`).join('')
    : '<span class="chip">none — nothing to import</span>';

  // One chip per distinct finding, carrying what it actually does to the
  // picture — "allowed" is not worth showing here, since the viewer compiles
  // everything and you are looking at the degraded render either way.
  const seen = new Map();
  for (const u of info.unsupported ?? []) {
    seen.set(u.feature, { effect: u.effect, n: (seen.get(u.feature)?.n ?? 0) + 1 });
  }
  const unsupported = seen.size
    ? `<div class="chips"><span class="k">Not implemented</span>${[...seen]
        .map(
          ([feature, { effect, n }]) =>
            `<span class="chip warn">${feature}${n > 1 ? ` ×${n}` : ''} — ${effect}</span>`,
        )
        .join('')}</div>`
    : '';

  el.innerHTML = `
    <div class="badges">${badges.join('')}</div>
    <div class="facts">${facts}</div>
    <div class="chips"><span class="k">Capabilities</span>${caps}</div>
    <div class="chips"><span class="k">Imports</span>${mods}</div>
    ${unsupported}`;
}

// ---------------------------------------------------------------------------
// Size panel — every delivery mode, against the lottie-web baseline
// ---------------------------------------------------------------------------

/** One row: a delivery mode, and the parts whose sizes make up its total. */
type Part = [name: string, size: SizeEntry, url?: string, prettyUrl?: string];

interface Way {
  label: string;
  parts: Part[];
  baseline?: boolean;
  raw: number;
  gz: number;
}

/**
 * Names the self-contained artifact, and says what was shaken out of it — the
 * tree-shaking claim belongs next to the row it explains, not in a footnote.
 */
function selfContainedPart(s: Sizes, generated: boolean): string {
  const f = s.features;
  const shaken = ([
    ['expressions', f.expressions, f.expressions_cost],
    ['trim-path', f.trim_path, f.trim_path_cost],
    ['gradient', f.gradient, f.gradient_cost],
  ] as [string, boolean, number][])
    .filter(([, kept]) => !kept)
    .map(([name, , cost]) => `${name} −${fmtBytes(Math.max(0, cost))}`)
    .join(', ');
  const how = generated
    ? 'one file, compiled to code'
    : 'one file, runtime inlined';
  return shaken ? `${how} — shook out ${shaken}` : how;
}

/**
 * Show one artifact's actual bytes.
 *
 * The table says a compiled module is 495 B; this is what 495 B looks like next
 * to the 298 KB it replaces. The sprite is markup, so it also renders.
 */
let viewerUrls: { raw: string; pretty: string } = { raw: '', pretty: '' };
let viewerRaw = false;

/** Put the panel back to a known state before it shows anything else. */
function resetViewer() {
  const svgBox = $('viewer-svg');
  // Cleared, not just hidden: the markup carries `<symbol id="…">`, and a
  // leftover definition is live in the document for anything that references
  // that id — including the mounted animations on this very page.
  svgBox.innerHTML = '';
  svgBox.hidden = true;
  $('viewer-body').hidden = false;
  const render = $('viewer-render') as HTMLButtonElement;
  render.hidden = true;
  render.textContent = 'rendered';
  ($('viewer-format') as HTMLButtonElement).hidden = false;
  viewerRaw = false;
  ($('viewer-format') as HTMLButtonElement).textContent = 'minified';
}

/** Close the viewer and drop everything it put in the document. */
function closeViewer() {
  resetViewer();
  $('viewer').hidden = true;
  for (const r of $('size-table').querySelectorAll('tr.part')) r.classList.remove('open');
}

/**
 * Fetch and show whichever form is selected.
 *
 * Both come from the compiler: `minified` is what ships, and the other is its
 * own unminified output — the same form the snapshots are reviewed in, not a
 * reformatting of the shipped bytes.
 */
async function paintSource() {
  const body = $('viewer-body');
  const url = viewerRaw ? viewerUrls.raw : viewerUrls.pretty;
  body.textContent = 'loading…';
  let text: string;
  try {
    const res = await fetch(url);
    if (!res.ok) throw new Error(String(res.status));
    text = await res.text();
  } catch (e) {
    body.textContent = `could not load ${url} — ${(e as Error).message}`;
    return;
  }
  // Enough to see the shape of it; laying out a few hundred kilobytes of
  // source is slow and tells you nothing the first screens did not.
  const LIMIT = 60000;
  const clipped = text.length > LIMIT;
  const shown = clipped ? text.slice(0, LIMIT) : text;
  body.innerHTML = await highlight(shown, langOf(viewerUrls.raw));
  if (clipped) {
    body.insertAdjacentHTML(
      'beforeend',
      `<p class="note">… ${fmtBytes(text.length - LIMIT)} more</p>`,
    );
  }
}

/**
 * Show one artifact's actual bytes.
 *
 * The table says a compiled module is 825 B; this is what 825 B looks like next
 * to the 298 KB it replaces.
 */
async function showArtifact(name: string, size: SizeEntry, url: string, prettyUrl: string) {
  resetViewer();
  $('viewer').hidden = false;
  $('viewer-title').textContent = name;
  $('viewer-size').textContent = `${fmtBytes(size.raw)} raw · ${fmtBytes(size.gzipped)} gzipped`;
  viewerUrls = { raw: url, pretty: prettyUrl || url };
  await paintSource();

  if (langOf(url) !== 'xml') return;
  const render = $('viewer-render') as HTMLButtonElement;
  render.hidden = false;
  render.onclick = async () => {
    const svgBox = $('viewer-svg');
    const body = $('viewer-body');
    const showing = !svgBox.hidden;
    svgBox.hidden = showing;
    body.hidden = !showing;
    render.textContent = showing ? 'rendered' : 'source';
    // Formatting applies to the source, not to a picture.
    ($('viewer-format') as HTMLButtonElement).hidden = !showing;
    if (showing) { svgBox.innerHTML = ''; return; }
    const text = await fetch(viewerUrls.raw).then((r) => r.text()).catch(() => '');
    // A sprite is `<symbol>` definitions; nothing draws until something
    // references one, which is also how a page consumes it.
    const id = /<symbol[^>]*\bid="([^"]+)"/.exec(text)?.[1];
    const vb = /<symbol[^>]*\bviewBox="([^"]+)"/.exec(text)?.[1] ?? '0 0 512 512';
    svgBox.innerHTML = id
      ? // The holder is out of flow and zero-sized, never `hidden`: a gradient
        // defined inside a `display:none` subtree is not painted when the
        // `<use>` instance references it (Chromium), and the sprite keeps
        // itself out of the page the same way for the same reason — `ripple`'s
        // gradient-stroked wires vanished from this panel while its plain-
        // filled dots stayed.
        `<div style="position:absolute;width:0;height:0;overflow:hidden">${text}</div>` +
        // The frame is the symbol's own viewBox. Without it an animation whose
        // static geometry sits in one corner — most of them, since bindings
        // write the rest — reads as clipped rather than as a mostly-empty
        // canvas, which is what it is.
        `<svg class="canvas" viewBox="${vb}" preserveAspectRatio="xMidYMid meet">` +
        `<use href="#${id}"/></svg>`
      : text;
    svgBox.insertAdjacentHTML(
      'beforeend',
      '<p class="note">the first frame, on the symbol\'s canvas — this is ' +
        'what renders before any script does, and what the module hydrates</p>',
    );
  };
}

function renderSizes(s: Sizes | null, plan?: Plan, urls?: CompileResponse) {
  const tbody = $('size-table').querySelector('tbody')!;
  // Every source change lands here, and the open artifact belonged to the
  // previous one. Leaving it up is not only stale: a rendered sprite puts a
  // live `<symbol id="…">` in the document, and the next animation's mount
  // would resolve `url(#…)` against whatever is still defined.
  closeViewer();
  if (!s) {
    tbody.innerHTML = '<tr><td colspan="4">no data</td></tr>';
    $('size-note').textContent = '';
    return;
  }

  // The runtime slice is what a bundler ships for THIS animation — not the
  // whole runtime, which nothing loads.
  const slice = s.runtime_slice ?? s.ulottie_runtime;

  // Every entry is `parts`, and the total is their sum, in both columns. The
  // previous version put a gzipped breakdown under a row showing raw and
  // gzipped side by side, which read as if the numbers should reconcile and
  // they could not.
  const sum = (parts: Part[], k: keyof SizeEntry) =>
    parts.reduce((n, [, v]) => n + v[k], 0);
  const way = (label: string, parts: Part[], baseline = false): Way => ({
    label,
    parts,
    baseline,
    raw: sum(parts, 'raw'),
    gz: sum(parts, 'gzipped'),
  });

  const baseline = way('lottie-web', [
    ['Lottie JSON', s.json, urls?.json_url, urls?.json_pretty_url],
    ['lottie.min.js', s.lottie_runtime],
  ], true);
  const shared = way('ulottie — shared runtime', [
    ['compiled module', s.js, urls?.js_url, urls?.js_pretty_url],
    ['runtime it imports', slice, urls?.slice_url, urls?.slice_pretty_url],
  ]);
  // Extracted is a variant of the split, so it reads next to it and lists its
  // parts in the same order: same module, same runtime slice, and then the one
  // thing that differs — the markup moved out into a sprite.
  const extracted =
    s.js_extracted && s.sprite
      ? way('ulottie — markup extracted', [
          ['compiled module', s.js_extracted, urls?.js_extracted_url, urls?.js_extracted_pretty_url],
          ['runtime it imports', slice, urls?.slice_url, urls?.slice_pretty_url],
          ['SVG sprite', s.sprite, urls?.sprite_url, urls?.sprite_pretty_url],
        ])
      : null;
  // Self-contained last: it is the one that shares nothing, so it ends the
  // progression rather than interrupting the two that do.
  const self = way('ulottie — self-contained', [
    [
      selfContainedPart(s, !!plan?.generated),
      s.js_embedded,
      urls?.js_embedded_url,
      urls?.js_embedded_pretty_url,
    ],
  ]);

  // Server-rendered: the document goes out inside the HTML — no request of
  // its own, but bytes all the same — and the module that hydrates it carries
  // no markup. The one delivery where the picture is never downloaded twice.
  const ssr =
    s.document && s.js_hydrate
      ? way('ulottie — server-rendered, then hydrated', [
          ['baked document, in the HTML', s.document, urls?.document_url, urls?.document_pretty_url],
          ['hydration module (no markup)', s.js_hydrate, urls?.js_hydrate_url, urls?.js_hydrate_pretty_url],
        ])
      : null;

  const ways: Way[] = [
    baseline,
    shared,
    ...(extracted ? [extracted] : []),
    self,
    ...(ssr ? [ssr] : []),
  ];

  const base = ways.find((w) => w.baseline)!;
  const best = Math.min(...ways.filter((w) => !w.baseline).map((w) => w.gz));

  tbody.innerHTML = ways
    .map((w) => {
      const cls = ['total', w.baseline ? 'base' : '', !w.baseline && w.gz === best ? 'best' : '']
        .filter(Boolean)
        .join(' ');
      const total =
        `<tr class="${cls}"><td>${w.label}</td><td>${fmtBytes(w.raw)}</td>` +
        `<td>${fmtBytes(w.gz)}</td>${w.baseline ? '<td>baseline</td>' : vsCell(w.gz, base.gz)}</tr>`;
      // A sole part has the same numbers as its total, so it contributes only
      // its name — repeating the figures reads as an error.
      const sole = w.parts.length === 1;
      const parts = w.parts
        .map(([name, v, url, prettyUrl]) => {
          const cells = sole
            ? '<td></td><td></td>'
            : `<td>${fmtBytes(v.raw)}</td><td>${fmtBytes(v.gzipped)}</td>`;
          const attrs = url
            ? ` class="part viewable" data-url="${url}" data-name="${name}"` +
              ` data-pretty="${prettyUrl ?? url}"` +
              ` data-raw="${v.raw}" data-gz="${v.gzipped}"`
            : ' class="part"';
          return `<tr${attrs}><td>${name}</td>${cells}<td></td></tr>`;
        })
        .join('');
      return total + parts;
    })
    .join('');

  ($('viewer-format') as HTMLButtonElement).onclick = () => {
    viewerRaw = !viewerRaw;
    ($('viewer-format') as HTMLButtonElement).textContent = viewerRaw ? 'unminified' : 'minified';
    void paintSource();
  };
  $('viewer-close').onclick = closeViewer;
  for (const row of tbody.querySelectorAll<HTMLElement>('tr.viewable')) {
    row.addEventListener('click', () => {
      for (const r of tbody.querySelectorAll('tr.part')) r.classList.remove('open');
      row.classList.add('open');
      void showArtifact(
        row.dataset.name!,
        { raw: Number(row.dataset.raw), gzipped: Number(row.dataset.gz) },
        row.dataset.url!,
        row.dataset.pretty!,
      );
    });
  }

  // One line, and only what the table cannot say for itself. The
  // tree-shaking detail lives in the breakdown, where the row it explains is.
  const notes: string[] = [];
  if (self.gz > shared.gz) {
    // Two files gzip independently; one big file has a single 32 KiB DEFLATE
    // window, so past that the split genuinely wins. Surprising enough to say.
    notes.push(
      `Self-contained is ${fmtBytes(self.gz - shared.gz)} larger gzipped than the split ` +
        `despite being ${fmtBytes(shared.raw - self.raw)} smaller raw — one ` +
        `${fmtBytes(self.raw)} stream outruns DEFLATE's 32 KiB window.`,
    );
  } else if (plan?.generated) {
    notes.push(
      'Self-contained is the smallest way to ship one animation, and compiling ' +
        'to code widens that: there is no interpreter and no payload left to ' +
        'inline. It is still one copy per animation — a second one is smaller ' +
        'again on the shared runtime, which amortises across the page.',
    );
  } else {
    notes.push('The runtime is shared: a second animation adds only its module.');
  }
  if (plan && !plan.generated && !plan.is_static) {
    notes.push(
      'This one kept the interpreter — either the generator cannot express it, ' +
        'or unrolling it came out larger. The compiler builds both and keeps ' +
        'the smaller.',
    );
  }
  $('size-note').textContent = notes.join(' ');
}

// ---------------------------------------------------------------------------
// Extracted assets — the preload story of the first-load panel
// ---------------------------------------------------------------------------

/**
 * One line of the extraction manifest: what a server would turn into 103
 * Early Hints or `<link rel="preload" as="image">` entries, so the images are
 * already arriving while the module is still being parsed.
 */

/**
 * Show the images extraction pulled out of the markup, and actually issue the
 * preload hints the manifest implies — the panel then demonstrates the claim
 * rather than describing it.
 *
 * Both compilers extract. The dev server writes the files and serves a
 * manifest at a URL; the wasm build hands the bytes back on the response,
 * already minted as Blob URLs — so the panel reads the same shape either way.
 */
async function renderAssetHints(info: CompileResult) {
  const el = $('assets-hints');
  let entries: ManifestEntry[];
  let source: string;
  if (info.assets) {
    entries = info.assets;
    source = 'Blob URLs minted by the in-browser compiler';
  } else {
    const url = `/.output/${info.id}/assets/manifest.json`;
    try {
      const res = await fetch(url);
      if (!res.ok) throw new Error(String(res.status));
      entries = (await res.json()) as ManifestEntry[];
      source = `<a href="${url}">manifest.json</a>`;
    } catch {
      el.innerHTML = '';
      return;
    }
  }
  if (!entries.length) {
    el.innerHTML = '';
    return;
  }
  // The hints themselves — exactly what the manifest is for. Removed and
  // re-added per animation so a previous one's preloads do not linger.
  for (const l of document.head.querySelectorAll('link[data-ulottie-preload]'))
    l.remove();
  for (const e of entries) {
    const link = document.createElement('link');
    link.rel = 'preload';
    link.as = 'image';
    link.href = e.url;
    link.setAttribute('data-ulottie-preload', '');
    document.head.appendChild(link);
  }
  const total = entries.reduce((n, e) => n + e.bytes, 0);
  el.innerHTML =
    `<div class="chips"><span class="k">Extracted images</span>` +
    entries
      .map(
        (e) =>
          `<a class="chip" href="${e.url}">${e.file}</a>` +
          `<span class="chip">${e.mime} · ${fmtBytes(e.bytes)}</span>`,
      )
      .join('') +
    `</div>` +
    `<div class="note">${entries.length} image(s), ${fmtBytes(total)}, preloaded via ` +
      `<code>&lt;link rel="preload" as="image"&gt;</code> from ${source} — they ` +
      `load ahead of the module instead of inside it.</div>`;
}

// ---------------------------------------------------------------------------
// Runtime panel — measured here, not quoted
// ---------------------------------------------------------------------------

const perfBody = () => $('perf-table').querySelector('tbody')!;

const resetPerf = () => {
  perfBody().innerHTML = '<tr><td colspan="4">press Measure</td></tr>';
};

/**
 * Count attribute writes while `fn` runs. Both players move pictures by
 * writing attributes (and inline styles, which is how each toggles
 * visibility), so this is the renderer-independent measure of work done.
 */
function countWrites(fn: () => void): number {
  const proto = Element.prototype;
  const styleProto = CSSStyleDeclaration.prototype;
  const realSet = proto.setAttribute;
  const realStyle = styleProto.setProperty;
  let n = 0;
  proto.setAttribute = function (...a) { n++; return realSet.apply(this, a); };
  styleProto.setProperty = function (...a) { n++; return realStyle.apply(this, a); };
  try { fn(); } finally {
    proto.setAttribute = realSet;
    styleProto.setProperty = realStyle;
  }
  return n;
}

/**
 * Force the browser to act on the attribute writes just made.
 *
 * Without this the timer sees the write and the invalidation walk and stops:
 * style and layout are lazy. Measured on ulottie, forcing them adds 38% of the
 * per-frame cost on `lottie_logo_1`, 41% on `lights` and 48% on `ripple` — so
 * without the flush the table would be describing attribute writes, not
 * rendering, and would omit roughly half the work. It omits it asymmetrically
 * too: ulottie's script is the cheaper half, so leaving layout out flatters it.
 * Paint and raster happen off the main thread afterwards and are still outside
 * what any synchronous measurement can see.
 */
const flush = () => document.body.offsetHeight;

const fmtMs = (ms: number): string => (ms < 0.01 ? ms.toFixed(4) : ms < 1 ? ms.toFixed(3) : ms.toFixed(2));

/**
 * `mean ms ± rme%` — tinybench reports the relative margin of error.
 *
 * `result` is a discriminated union and the statistics only exist on the
 * states that have them, so a task that errored or never ran reports a dash
 * rather than `NaN`.
 */
function stat(task: Task): { ms: number; cell: string } {
  const r = task.result;
  if (!r || !('latency' in r)) return { ms: NaN, cell: '—' };
  const { mean, rme, samplesCount } = r.latency;
  return {
    ms: mean,
    cell: `${fmtMs(mean)} ms<span class="sub">±${rme.toFixed(1)}% · ${samplesCount} samples</span>`,
  };
}

function ratio(refMs: number, ulMs: number): { cell: string; good: boolean | null } {
  if (!isFinite(refMs) || !isFinite(ulMs) || ulMs <= 0) return { cell: '—', good: null };
  const x = refMs / ulMs;
  return {
    cell: x >= 1 ? `${x.toFixed(1)}× faster` : `${(1 / x).toFixed(1)}× slower`,
    good: x >= 1,
  };
}

/**
 * A hard ceiling on any one bench.
 *
 * `time` is tinybench's *minimum* duration, not a maximum: it keeps sampling
 * until that much wall clock has passed, so a task costing ~0.0001 ms runs
 * millions of iterations to fill it. An abort signal is the only actual bound,
 * and partial statistics survive it.
 */
const CEILING_MS = 2_000;

/** An offscreen host, so benchmarking never disturbs the visible panels. */
function scratchHost() {
  const el = document.createElement('div');
  el.style.cssText = 'position:absolute;left:-9999px;top:0;width:300px;height:300px';
  document.body.appendChild(el);
  return el;
}

async function measure() {
  const ref = currentAnim;
  const ul = ulottieResult;
  const mod = ulottieModule;
  if (!ref || !ul || !totalFrames) return;
  const frames = Math.min(totalFrames, 120);

  // --- per frame -----------------------------------------------------------
  // One iteration is one frame, so tinybench's statistics describe exactly the
  // quantity the table reports. The previous estimator timed a whole sweep
  // against a `performance.now()` clamped to 100 µs, which made short sweeps
  // land on multiples of 0.1 ms — for the smallest fixtures it reported zero.
  // Skipped when the animation is static: `goToFrame` is an inert stub, so
  // there is nothing to time, and sampling it burns seconds to report noise.
  let refFrame: ReturnType<typeof stat> | undefined;
  let ulFrame: ReturnType<typeof stat> | undefined;
  if (!isStatic) {
    const frameBench = new Bench({
      time: 400,
      warmupTime: 150,
      signal: AbortSignal.timeout(CEILING_MS),
    });
    let a = 0;
    let b = 0;
    frameBench.add('lottie-web', () => {
      ref.goToAndStop(a++ % frames, true);
      flush();
    });
    frameBench.add('ulottie', () => {
      ul.goToFrame(b++ % frames);
      flush();
    });
    await frameBench.run();
    [refFrame, ulFrame] = frameBench.tasks.map(stat);
  }

  // --- mount ---------------------------------------------------------------
  // Where an AOT compiler actually wins: ulottie parses a baked string and
  // wires closures, lottie-web builds a scene graph. Per instance, so a page
  // with many animations multiplies it.
  const mountBench = new Bench({
    time: 400,
    warmupTime: 100,
    signal: AbortSignal.timeout(CEILING_MS),
  });
  if (animationData) {
    mountBench.add('lottie-web', () => {
      const host = scratchHost();
      const probe = lottie.loadAnimation({
        container: host,
        renderer: 'svg',
        loop: false,
        autoplay: false,
        animationData: animationData as object,
      });
      probe.destroy();
      host.remove();
    });
  }
  if (mod) {
    mountBench.add('ulottie', () => {
      const host = scratchHost();
      mod.init(host, { autoplay: false }).destroy();
      host.remove();
    });
  }
  await mountBench.run();
  const [refMount, ulMount] = mountBench.tasks.map(stat);

  // --- DOM writes ----------------------------------------------------------
  const refW = countWrites(() => { for (let i = 0; i < frames; i++) ref.goToAndStop(i, true); }) / frames;
  const ulW = countWrites(() => { for (let i = 0; i < frames; i++) ul.goToFrame(i); }) / frames;
  // Compare at the precision shown, so two columns reading 181.7 do not
  // report "+0%" off a difference in the fourth decimal.
  const pct = refW === 0 ? 0 : ((ulW - refW) / refW) * 100;
  const writeDelta: Cmp =
    Math.abs(pct) < 0.5
      ? { cell: 'same', good: null }
      : { cell: `${pct < 0 ? '−' : '+'}${Math.abs(pct).toFixed(0)}%`, good: pct < 0 };

  // `good: null` is neither a win nor a loss — "same", or a row that does not
  // apply. Colouring those green would claim an improvement that is not there.
  type Cmp = { cell: string; good: boolean | null };
  const row = (label: string, sub: string, lw: string, ulc: string, cmp: Cmp) => {
    const cls = cmp.good === null ? '' : cmp.good ? 'gain' : 'loss';
    return (
      `<tr><td>${label}<span class="sub">${sub}</span></td><td>${lw}</td><td>${ulc}</td>` +
      `<td class="${cls}">${cmp.cell}</td></tr>`
    );
  };

  perfBody().innerHTML =
    row('mount', 'per instance, one-off', refMount?.cell ?? '—', ulMount?.cell ?? '—',
        ratio(refMount?.ms ?? NaN, ulMount?.ms ?? NaN)) +
    (isStatic
      ? row('frame', 'no frame loop — nothing varies over time', '—', '—', {
          cell: 'not applicable',
          good: null,
        })
      : row(
          'frame',
          `seek + style &amp; layout, cycling ${frames} frames`,
          refFrame?.cell ?? '—',
          ulFrame?.cell ?? '—',
          ratio(refFrame?.ms ?? NaN, ulFrame?.ms ?? NaN),
        )) +
    row('DOM writes / frame', 'attribute + inline-style sets',
        refW.toFixed(1), ulW.toFixed(1), writeDelta);

  // Benchmarking left both players mid-sweep.
  const f = parseInt(scrubber.value, 10) || 0;
  ref.goToAndStop(f, true);
  ul.goToFrame(f);
}

// ---------------------------------------------------------------------------
// Wiring
// ---------------------------------------------------------------------------

scrubber.addEventListener('input', () => {
  const frame = parseInt(scrubber.value, 10);
  frameDisplay.textContent = String(frame);
  if (currentAnim) currentAnim.goToAndStop(frame, true);
  if (ulottieResult?.goToFrame) ulottieResult.goToFrame(frame);
});

// Totals are the story; the parts are there when you want to check the sum.
const detailBtn = $<HTMLButtonElement>('size-detail');
detailBtn.addEventListener('click', () => {
  const on = $('size-table').classList.toggle('detail');
  detailBtn.setAttribute('aria-expanded', String(on));
  detailBtn.textContent = on ? 'hide breakdown' : 'breakdown';
});

// Chrome clamps `performance.now()` to 100 µs unless the document is
// cross-origin isolated, where it relaxes to 5 µs. Which one applies changes
// how much precision the table deserves, so say so rather than leave it
// implicit — see `ISOLATION` in vite.config.ts for the headers.
$('clock').textContent = self.crossOriginIsolated
  ? 'Cross-origin isolated, so the clock reports 5 µs granularity.'
  : 'Not cross-origin isolated: the clock is clamped to 100 µs, so short tasks are coarse.';

const measureBtn = $<HTMLButtonElement>('measure');
measureBtn.addEventListener('click', () => {
  const btn = measureBtn;
  btn.disabled = true;
  btn.textContent = 'Measuring…';
  // Yield so the button repaints before the main thread is monopolised.
  requestAnimationFrame(() =>
    setTimeout(() => {
      try { measure(); } finally {
        btn.disabled = false;
        btn.textContent = 'Measure again';
      }
    }, 0),
  );
});

select.addEventListener('change', () => {
  // Going back to a demo clears any custom source, so the two views do not
  // disagree about what is loaded.
  fileInput.value = '';
  urlInput.value = '';
  void loadFixture(select.value);
});

fileInput.addEventListener('change', () => {
  const file = fileInput.files?.[0];
  if (file) {
    // Loading a file resets the URL — only one custom source at a time.
    urlInput.value = '';
    void loadUploaded(file);
  }
});

urlLoadBtn.addEventListener('click', () => void loadFromUrl(urlInput.value));
// Enter in the URL field submits explicitly, matching the button.
urlInput.addEventListener('keydown', (e) => {
  if (e.key === 'Enter') {
    e.preventDefault();
    void loadFromUrl(urlInput.value);
  }
});

async function loadUploaded(file: File) {
  uploadStatus.textContent = 'Compiling ' + file.name + '…';
  await loadFromSource(await file.text(), file.name);
}

async function loadFromUrl(url: string) {
  url = url.trim();
  if (!url) return;
  // Loading a URL resets the file — only one custom source at a time.
  fileInput.value = '';
  uploadStatus.textContent = 'Fetching ' + url + '…';
  try {
    const res = await fetch(url);
    if (!res.ok) throw new Error('HTTP ' + res.status);
    await loadFromSource(await res.text(), url);
  } catch (e) {
    uploadStatus.textContent = 'Fetch failed: ' + ((e as Error).message ?? e) +
      ' — the host must allow cross-origin reads (CORS).';
  }
}

document.addEventListener('dragover', (e) => {
  if (e.dataTransfer?.types?.includes('Files')) {
    e.preventDefault();
    dropHint.classList.add('active');
  }
});
document.addEventListener('dragleave', (e) => {
  if (e.target === document || e.target === document.body) dropHint.classList.remove('active');
});
document.addEventListener('drop', (e) => {
  e.preventDefault();
  dropHint.classList.remove('active');
  const file = e.dataTransfer?.files?.[0];
  if (file && file.name.endsWith('.json')) loadUploaded(file);
});

loadFixture(select.value);
