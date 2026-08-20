// Compile worker: runs the wasm-compiled ulottie pipeline off the main
// thread so the page stays responsive while large fixtures compile.
//
// Protocol: `WorkerRequest` in, `WorkerReply` out. Blob URLs are minted here
// and ride back as plain strings — same-origin blob URLs are visible to the
// main realm without further coordination.
//
// `postMessage` is untyped, so the two shapes are declared here and imported
// by the sender; without that a renamed key fails silently at runtime.

// wasm-pack's output sits in `demo/public/wasm/`, which Vite copies verbatim
// rather than bundling — so it is loaded by URL at runtime. `@vite-ignore`
// stops Vite trying to resolve it at build time.
// wasm-pack's output, installed by the Vite plugin. A normal import: Vite
// bundles the glue and emits the `.wasm` beside it, and wasm-pack ships the
// `.d.ts` so this is typed.
import init, { compileRequest as compileRequestUntyped } from '#wasm';

const compileRequest = compileRequestUntyped as unknown as (json: string) => WasmResult;

import type { CompileResult, Plan, SizeEntry, Unsupported } from './types.ts';

/** wasm-bindgen hands back views over its own memory, copied on each read. */
type Bytes = Uint8Array<ArrayBuffer>;

export interface WorkerRequest {
  id: number;
  jsonText: string;
  /** Measured on the main thread, where the bundled asset is reachable. */
  lottieRuntime: SizeEntry;
}

export type WorkerReply =
  | { ready: true }
  | { id: number; response: CompileResult }
  | { id: number; error: string };

/**
 * What `compileRequest` hands back across the wasm boundary. Byte arrays are
 * getters, so each read copies — pull them out once.
 */
interface WasmResult {
  compactJson: Bytes;
  compiledJs: Bytes;
  compiledEmbedded: Bytes;
  compiledExtracted: Bytes;
  spriteSvg: Bytes;
  // The SSR pair: the baked document, and the self-contained module with no
  // markup that hydrates it.
  compiledHydrate: Bytes;
  documentSvg: Bytes;
  runtimeSlice: Bytes;
  // The compiler's own unminified output, for the viewer.
  prettyJs: Bytes;
  prettyEmbedded: Bytes;
  prettyExtracted: Bytes;
  prettyHydrate: Bytes;
  prettyDocument: Bytes;
  prettySlice: Bytes;
  prettySprite: Bytes;
  driverMinJs: Bytes;
  // Images extraction pulled out of the markup. Each is a file the markup
  // references as `assets/<name>` — in a page there is nowhere to write it,
  // so each becomes a Blob URL and the references are rewritten.
  assetCount: number;
  assetName(i: number): string | undefined;
  assetMime(i: number): string | undefined;
  assetBytes(i: number): Bytes | undefined;
  total_frames: number;
  name: string | null;
  plan: Plan;
  unsupported: Unsupported[];
  features: CompileResult['sizes']['features'];
  free(): void;
}

// Filled in by the caller, which measures the real `lottie-web` bundle rather
// than trusting a constant that goes stale when the dependency is bumped.
let LOTTIE_RUNTIME: SizeEntry = { raw: 0, gzipped: 0 };

await init();

// Module workers with top-level await silently drop messages posted
// before the module finishes evaluating (Chromium/WebKit behavior in
// 2026). Tell the bootstrap we're alive so it gates dispatch on this.
self.postMessage({ ready: true });

self.onmessage = async (e: MessageEvent<WorkerRequest>) => {
  const { id, jsonText, lottieRuntime } = e.data;
  if (lottieRuntime) LOTTIE_RUNTIME = lottieRuntime;
  try {
    const r = compileRequest(jsonText);
    try {
      const response = await buildCompileResponse(r, LOTTIE_RUNTIME);
      self.postMessage({ id, response });
    } finally {
      r.free();
    }
  } catch (err) {
    self.postMessage({ id, error: String((err as Error)?.message ?? err) });
  }
};

async function gzipSize(bytes: Bytes): Promise<number> {
  const cs = new CompressionStream('gzip');
  const stream = new Blob([bytes]).stream().pipeThrough(cs);
  const compressed = new Uint8Array(await new Response(stream).arrayBuffer());
  return compressed.length;
}

async function buildCompileResponse(r: WasmResult, lottieRuntime: SizeEntry): Promise<CompileResult> {
  const json = r.compactJson;
  const compiledJs = r.compiledJs;
  const compiledEmbedded = r.compiledEmbedded;
  const compiledExtracted = r.compiledExtracted;
  const sprite = r.spriteSvg;
  const compiledHydrate = r.compiledHydrate;
  const documentSvg = r.documentSvg;
  const slice = r.runtimeSlice;
  const driver = r.driverMinJs;

  // Extern output imports the runtime as a module graph
  // (`./runtime/core.js`, `./runtime/ops/…`), which a blob: URL has no base to
  // resolve against. The page renders from the embedded module instead — it is
  // self-contained, and it is the artifact the size matrix is about. The extern
  // blob is still handed back so its bytes can be inspected.
  const jsonUrlValue = blobUrl(json, 'application/json');
  const sliceUrl = blobUrl(slice, 'application/javascript');
  // The compiler's own unminified output, so the viewer shows the same thing
  // the server would. Nothing is reformatted in the page.
  const prettyJsonUrl = blobUrl(indentJson(json), 'application/json');
  const prettySliceUrl = blobUrl(r.prettySlice, 'application/javascript');

  // One Blob URL per extracted image, plus the manifest row for the panel.
  // The rewrite map is applied to every artifact that carries markup, so what
  // mounts in the page points at the blobs the page owns.
  const assetEntries: NonNullable<CompileResult['assets']> = [];
  const rewrites: [from: string, to: string][] = [];
  for (let i = 0; i < r.assetCount; i++) {
    const file = r.assetName(i);
    const mime = r.assetMime(i);
    const b = r.assetBytes(i);
    if (!file || !mime || !b) continue;
    const url = blobUrl(b, mime);
    rewrites.push([`assets/${file}`, url]);
    assetEntries.push({ url, file, mime, bytes: b.length });
  }
  const rewrite = (bytes: Bytes): Bytes => {
    if (!rewrites.length) return bytes;
    let text = new TextDecoder().decode(bytes);
    for (const [from, to] of rewrites) text = text.replaceAll(from, to);
    return new TextEncoder().encode(text);
  };

  const [jsonGz, jsGz, jsEmbeddedGz, jsExtractedGz, spriteGz, hydrateGz, documentGz, sliceGz, ulottieRuntimeGz] =
    await Promise.all([
      gzipSize(json),
      // Measured on the pre-rewrite bytes: the size matrix reports what ships
      // in production (`assets/img_….png`), not this page's blob: URLs.
      gzipSize(compiledJs),
      gzipSize(compiledEmbedded),
      gzipSize(compiledExtracted),
      gzipSize(sprite),
      gzipSize(compiledHydrate),
      gzipSize(documentSvg),
      // A static animation imports nothing; gzipping empty would report the
      // 20-byte header as if it were payload.
      slice.length ? gzipSize(slice) : 0,
      gzipSize(driver),
    ]);

  return {
    id: 'wasm',
    name: r.name ?? null,
    total_frames: r.total_frames,
    json_url: jsonUrlValue,
    js_url: blobUrl(rewrite(compiledJs), 'application/javascript'),
    js_embedded_url: blobUrl(rewrite(compiledEmbedded), 'application/javascript'),
    js_extracted_url: blobUrl(rewrite(compiledExtracted), 'application/javascript'),
    sprite_url: blobUrl(rewrite(sprite), 'image/svg+xml'),
    document_url: blobUrl(rewrite(documentSvg), 'image/svg+xml'),
    js_hydrate_url: blobUrl(compiledHydrate, 'application/javascript'),
    slice_url: sliceUrl,
    json_pretty_url: prettyJsonUrl,
    js_pretty_url: blobUrl(rewrite(r.prettyJs), 'application/javascript'),
    js_embedded_pretty_url: blobUrl(rewrite(r.prettyEmbedded), 'application/javascript'),
    js_extracted_pretty_url: blobUrl(rewrite(r.prettyExtracted), 'application/javascript'),
    sprite_pretty_url: blobUrl(rewrite(r.prettySprite), 'image/svg+xml'),
    document_pretty_url: blobUrl(rewrite(r.prettyDocument), 'image/svg+xml'),
    js_hydrate_pretty_url: blobUrl(r.prettyHydrate, 'application/javascript'),
    slice_pretty_url: prettySliceUrl,
    plan: r.plan,
    unsupported: r.unsupported,
    assets: assetEntries,
    sizes: {
      json: { raw: json.length, gzipped: jsonGz },
      js: { raw: compiledJs.length, gzipped: jsGz },
      runtime_slice: { raw: slice.length, gzipped: sliceGz },
      ulottie_runtime: { raw: driver.length, gzipped: ulottieRuntimeGz },
      js_embedded: { raw: compiledEmbedded.length, gzipped: jsEmbeddedGz },
      js_extracted: { raw: compiledExtracted.length, gzipped: jsExtractedGz },
      sprite: { raw: sprite.length, gzipped: spriteGz },
      document: { raw: documentSvg.length, gzipped: documentGz },
      js_hydrate: { raw: compiledHydrate.length, gzipped: hydrateGz },
      lottie_runtime: lottieRuntime,
      features: r.features,
    },
  };
}

/** Re-serialize the source JSON at two spaces; the server does the same. */
function indentJson(bytes: Bytes): Bytes {
  try {
    const text = new TextDecoder().decode(bytes);
    return new TextEncoder().encode(JSON.stringify(JSON.parse(text), null, 2));
  } catch {
    return bytes;
  }
}

function blobUrl(bytes: Bytes, type: string): string {
  return URL.createObjectURL(new Blob([bytes], { type }));
}
