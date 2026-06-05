// Compile worker: runs the wasm-compiled ulottie pipeline off the main
// thread so the page stays responsive while large fixtures compile.
//
// Protocol: one message in `{id, jsonText, lottieRuntime}`, one message
// out `{id, response}` on success or `{id, error}` on failure. Blob URLs
// are minted here and ride back as plain strings — same-origin blob URLs
// are visible to the main realm without further coordination.

import init, { compileRequest } from './wasm/ulottie_compiler.js';

// lottie.min.js is a vendored static asset — same content every load,
// so the size never needs to be measured at runtime. The dev server
// computes it from disk on every /compile in api mode; this constant
// keeps the static-deploy matrix consistent. Refresh after updating
// the vendored bundle:
//
//   public$ wc -c lottie.min.js && gzip -c lottie.min.js | wc -c
const LOTTIE_RUNTIME = { raw: 305704, gzipped: 76665 };

await init();

// Module workers with top-level await silently drop messages posted
// before the module finishes evaluating (Chromium/WebKit behavior in
// 2026). Tell the bootstrap we're alive so it gates dispatch on this.
self.postMessage({ ready: true });

self.onmessage = async (e) => {
  const { id, jsonText } = e.data;
  try {
    const r = compileRequest(jsonText);
    try {
      const response = await buildCompileResponse(r, LOTTIE_RUNTIME);
      self.postMessage({ id, response });
    } finally {
      r.free();
    }
  } catch (err) {
    self.postMessage({ id, error: String(err?.message ?? err) });
  }
};

async function gzipSize(bytes) {
  const cs = new CompressionStream('gzip');
  const stream = new Blob([bytes]).stream().pipeThrough(cs);
  const compressed = new Uint8Array(await new Response(stream).arrayBuffer());
  return compressed.length;
}

async function buildCompileResponse(r, lottieRuntime) {
  const json = r.compactJson;
  const compiledJs = r.compiledJs;
  const compiledEmbedded = r.compiledEmbedded;
  const driver = r.driverMinJs;

  // Patch the extern compiled JS's `import"./driver.js"` literal to a
  // Blob URL of the minified driver so dynamic `import(jsUrl)` on the
  // main thread resolves the runtime.
  const driverUrl = blobUrl(driver, 'application/javascript');
  const patchedExtern = new TextEncoder().encode(
    new TextDecoder().decode(compiledJs)
      .replace('"./driver.js"', JSON.stringify(driverUrl))
  );

  const externUrl = blobUrl(patchedExtern, 'application/javascript');
  const embeddedUrl = blobUrl(compiledEmbedded, 'application/javascript');
  const jsonUrl = blobUrl(json, 'application/json');

  const [jsonGz, jsGz, jsEmbeddedGz, ulottieRuntimeGz] = await Promise.all([
    gzipSize(json),
    gzipSize(compiledJs),
    gzipSize(compiledEmbedded),
    gzipSize(driver),
  ]);

  return {
    id: 'wasm',
    name: r.name ?? null,
    totalFrames: r.totalFrames,
    jsonUrl,
    jsUrl: externUrl,
    jsEmbeddedUrl: embeddedUrl,
    sizes: {
      json: { raw: json.length, gzipped: jsonGz },
      js: { raw: compiledJs.length, gzipped: jsGz },
      ulottieRuntime: { raw: driver.length, gzipped: ulottieRuntimeGz },
      jsEmbedded: { raw: compiledEmbedded.length, gzipped: jsEmbeddedGz },
      lottieRuntime,
      embeddedFeatures: r.embeddedFeatures,
    },
  };
}

function blobUrl(bytes, type) {
  return URL.createObjectURL(new Blob([bytes], { type }));
}
