// Comparison app. The `compile` import resolves to whichever variant
// the host serves at `./compiler.js`: the dev server in --mode api
// overrides it with a tiny fetch-based shim, otherwise the file on
// disk (`public/compiler.js`) is the wasm + worker variant. Same
// interface, no globals, no module-ordering tricks.

import { compile } from './compiler.js';

const select = document.getElementById('anim-select');
const scrubber = document.getElementById('scrubber');
const frameDisplay = document.getElementById('frame-display');
const totalFramesEl = document.getElementById('total-frames');
const fileInput = document.getElementById('file-input');
const dropHint = document.getElementById('drop-hint');
const uploadStatus = document.getElementById('upload-status');

let currentAnim = null;
let ulottieResult = null;
let ulottieModule = null;
let lastJsBlobUrl = null;

async function loadFromSource(jsonText, label) {
  document.getElementById('lottie-ref').innerHTML = '';
  document.getElementById('ulottie').innerHTML = '';
  if (currentAnim) currentAnim.destroy();
  if (ulottieResult) ulottieResult.destroy();
  if (lastJsBlobUrl) {
    URL.revokeObjectURL(lastJsBlobUrl);
    lastJsBlobUrl = null;
  }

  let info;
  try {
    info = await compile(jsonText);
  } catch (e) {
    uploadStatus.textContent = 'Compile failed: ' + (e.message ?? e);
    renderSizes(null);
    return;
  }
  renderSizes(info.sizes);

  currentAnim = lottie.loadAnimation({
    container: document.getElementById('lottie-ref'),
    renderer: 'svg',
    loop: false,
    autoplay: false,
    animationData: JSON.parse(jsonText),
  });

  currentAnim.addEventListener('DOMLoaded', () => {
    const tf = Math.round(currentAnim.totalFrames);
    scrubber.max = tf - 1;
    totalFramesEl.textContent = tf;
    scrubber.value = 0;
    frameDisplay.textContent = 0;
    currentAnim.goToAndStop(0, true);
  });

  try {
    // Dev-server URLs are plain paths; wasm bootstrap returns blob: URLs.
    // Cache-bust path-style URLs so disk-cache misses don't bite; blob
    // URLs are already unique per compile so the suffix is harmless there.
    const jsUrl = info.jsUrl + (info.jsUrl.startsWith('blob:') ? '' : `?t=${Date.now()}`);
    if (info.jsUrl.startsWith('blob:')) lastJsBlobUrl = info.jsUrl;
    ulottieModule = await import(jsUrl);
    ulottieResult = ulottieModule.init(document.getElementById('ulottie'));
    ulottieResult.destroy();
    ulottieResult.goToFrame(0);
  } catch (e) {
    document.getElementById('ulottie').textContent = 'Error: ' + e.message;
  }
  if (label) uploadStatus.textContent = 'Loaded: ' + label;
}

async function loadFixture(name) {
  const res = await fetch('./_fixtures/' + name + '.json');
  if (!res.ok) {
    uploadStatus.textContent = 'Fetch failed: ' + res.status;
    return;
  }
  await loadFromSource(await res.text(), name);
}

async function loadUploaded(file) {
  uploadStatus.textContent = 'Compiling ' + file.name + '…';
  await loadFromSource(await file.text(), file.name);
}

// ----- Size panel rendering -----

const fmtBytes = (n) => {
  if (n < 1024) return n + ' B';
  if (n < 1024 * 1024) return (n / 1024).toFixed(1) + ' KB';
  return (n / 1024 / 1024).toFixed(2) + ' MB';
};
const fmtDelta = (n) =>
  (n >= 0 ? '+' : '−') + fmtBytes(Math.abs(n));
const fmtPct = (delta, base) => {
  if (!base) return '';
  const pct = Math.round((delta / base) * 100);
  const sign = pct > 0 ? '+' : pct < 0 ? '−' : '';
  return ` (${sign}${Math.abs(pct)}%)`;
};
const cellClass = (n) => n > 0 ? 'loss' : n < 0 ? 'gain' : '';
const dCell = (n) => `<td class="${cellClass(n)}">${fmtDelta(n)}</td>`;
const dCellPct = (n, base) =>
  `<td class="${cellClass(n)}">${fmtDelta(n)}${fmtPct(n, base)}</td>`;

function featuresRow(features, costs) {
  const items = [
    ['expressions', features.expressions, costs.expressions],
    ['trim-path',   features.trimPath,    costs.trimPath],
    ['gradient',    features.gradient,    costs.gradient],
  ];
  const chips = items.map(([name, kept, cost]) => {
    const klass = kept ? 'kept' : 'stripped';
    const sign = kept ? '+' : '−';
    return `<span class="chip ${klass}">${name} ${sign}${fmtBytes(Math.max(0, cost))}</span>`;
  }).join('');
  return `
    <tr><td colspan="3">
      <div class="features">
        <span class="label">Tree-shaken</span>${chips}
      </div>
    </td></tr>`;
}

function renderSizes(s) {
  const tbody = document.querySelector('#size-table tbody');
  if (!s) {
    tbody.innerHTML =
      '<tr class="measuring"><td colspan="3">no data</td></tr>';
    return;
  }
  const lottieTotal = {
    raw: s.json.raw + s.lottieRuntime.raw,
    gz: s.json.gzipped + s.lottieRuntime.gzipped,
  };
  const ulottieTotal = {
    raw: s.js.raw + s.ulottieRuntime.raw,
    gz: s.js.gzipped + s.ulottieRuntime.gzipped,
  };
  const delta = {
    raw: ulottieTotal.raw - lottieTotal.raw,
    gz: ulottieTotal.gz - lottieTotal.gz,
  };
  const dPayload = {
    raw: s.js.raw - s.json.raw,
    gz: s.js.gzipped - s.json.gzipped,
  };
  const embeddedDelta = {
    raw: s.jsEmbedded.raw - lottieTotal.raw,
    gz: s.jsEmbedded.gzipped - lottieTotal.gz,
  };
  const ef = s.embeddedFeatures;
  tbody.innerHTML = `
    <tr class="pipeline"><td colspan="3">Lottie pipeline</td></tr>
    <tr><td>Lottie JSON</td><td>${fmtBytes(s.json.raw)}</td><td>${fmtBytes(s.json.gzipped)}</td></tr>
    <tr><td>lottie-web runtime (lottie.min.js)</td><td>${fmtBytes(s.lottieRuntime.raw)}</td><td>${fmtBytes(s.lottieRuntime.gzipped)}</td></tr>
    <tr class="subtotal"><td>= Lottie total</td><td>${fmtBytes(lottieTotal.raw)}</td><td>${fmtBytes(lottieTotal.gz)}</td></tr>

    <tr class="pipeline"><td colspan="3">ulottie pipeline (extern, shared runtime)</td></tr>
    <tr><td>Compiled JS</td><td>${fmtBytes(s.js.raw)}</td><td>${fmtBytes(s.js.gzipped)}</td></tr>
    <tr><td>ulottie runtime (driver.min.js)</td><td>${fmtBytes(s.ulottieRuntime.raw)}</td><td>${fmtBytes(s.ulottieRuntime.gzipped)}</td></tr>
    <tr class="subtotal"><td>= ulottie total</td><td>${fmtBytes(ulottieTotal.raw)}</td><td>${fmtBytes(ulottieTotal.gz)}</td></tr>
    <tr class="delta"><td>Δ vs Lottie</td>${dCellPct(delta.raw, lottieTotal.raw)}${dCellPct(delta.gz, lottieTotal.gz)}</tr>

    <tr class="pipeline"><td colspan="3">ulottie pipeline (embedded, tree-shaken &amp; minified)</td></tr>
    ${featuresRow(ef.included, ef.costRaw)}
    <tr class="subtotal"><td>= embedded total</td><td>${fmtBytes(s.jsEmbedded.raw)}</td><td>${fmtBytes(s.jsEmbedded.gzipped)}</td></tr>
    <tr class="delta headline"><td>Δ vs Lottie</td>${dCellPct(embeddedDelta.raw, lottieTotal.raw)}${dCellPct(embeddedDelta.gz, lottieTotal.gz)}</tr>

    <tr class="delta"><td>Δ amortized payload (JS − JSON)</td>${dCell(dPayload.raw)}${dCell(dPayload.gz)}</tr>
  `;
}

scrubber.addEventListener('input', () => {
  const frame = parseInt(scrubber.value);
  frameDisplay.textContent = frame;
  if (currentAnim) currentAnim.goToAndStop(frame, true);
  if (ulottieResult && ulottieResult.goToFrame) ulottieResult.goToFrame(frame);
});

select.addEventListener('change', () => loadFixture(select.value));
fileInput.addEventListener('change', (e) => {
  const file = e.target.files?.[0];
  if (file) loadUploaded(file);
});

document.addEventListener('dragover', (e) => {
  if (e.dataTransfer?.types?.includes('Files')) {
    e.preventDefault();
    dropHint.classList.add('active');
  }
});
document.addEventListener('dragleave', (e) => {
  if (e.target === document || e.target === document.body) {
    dropHint.classList.remove('active');
  }
});
document.addEventListener('drop', (e) => {
  e.preventDefault();
  dropHint.classList.remove('active');
  const file = e.dataTransfer?.files?.[0];
  if (file && file.name.endsWith('.json')) loadUploaded(file);
});

// Fire-and-forget — compile() awaits the bootstrap-installed impl
// internally, so we don't care what state the page is in here.
loadFixture(select.value);
