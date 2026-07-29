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

import type { CompileResponse, Plan, SizeEntry, Unsupported } from './types.ts';

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
  | { id: number; response: CompileResponse }
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
  runtimeSlice: Bytes;
  // The compiler's own unminified output, for the viewer.
  prettyJs: Bytes;
  prettyEmbedded: Bytes;
  prettyExtracted: Bytes;
  prettySlice: Bytes;
  prettySprite: Bytes;
  driverMinJs: Bytes;
  total_frames: number;
  name: string | null;
  plan: Plan;
  unsupported: Unsupported[];
  features: CompileResponse['sizes']['features'];
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

async function buildCompileResponse(r: WasmResult, lottieRuntime: SizeEntry): Promise<CompileResponse> {
  const json = r.compactJson;
  const compiledJs = r.compiledJs;
  const compiledEmbedded = r.compiledEmbedded;
  const compiledExtracted = r.compiledExtracted;
  const sprite = r.spriteSvg;
  const slice = r.runtimeSlice;
  const driver = r.driverMinJs;

  // Extern output imports the runtime as a module graph
  // (`./runtime/core.js`, `./runtime/ops/…`), which a blob: URL has no base to
  // resolve against. The page renders from the embedded module instead — it is
  // self-contained, and it is the artifact the size matrix is about. The extern
  // blob is still handed back so its bytes can be inspected.
  const externUrl = blobUrl(compiledJs, 'application/javascript');
  const embeddedUrl = blobUrl(compiledEmbedded, 'application/javascript');
  const jsonUrlValue = blobUrl(json, 'application/json');
  // The size table names these too, and the viewer shows whatever a row names.
  // The wasm path already has every artifact in hand, so they cost a blob each.
  const extractedUrl = blobUrl(compiledExtracted, 'application/javascript');
  const spriteUrl = blobUrl(sprite, 'image/svg+xml');
  const sliceUrl = blobUrl(slice, 'application/javascript');
  // The compiler's own unminified output, so the viewer shows the same thing
  // the server would. Nothing is reformatted in the page.
  const prettyJsonUrl = blobUrl(indentJson(json), 'application/json');
  const prettyJsUrl = blobUrl(r.prettyJs, 'application/javascript');
  const prettyEmbeddedUrl = blobUrl(r.prettyEmbedded, 'application/javascript');
  const prettyExtractedUrl = blobUrl(r.prettyExtracted, 'application/javascript');
  const prettySliceUrl = blobUrl(r.prettySlice, 'application/javascript');

  const [jsonGz, jsGz, jsEmbeddedGz, jsExtractedGz, spriteGz, sliceGz, ulottieRuntimeGz] =
    await Promise.all([
      gzipSize(json),
      gzipSize(compiledJs),
      gzipSize(compiledEmbedded),
      gzipSize(compiledExtracted),
      gzipSize(sprite),
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
    js_url: externUrl,
    js_embedded_url: embeddedUrl,
    js_extracted_url: extractedUrl,
    sprite_url: spriteUrl,
    slice_url: sliceUrl,
    json_pretty_url: prettyJsonUrl,
    js_pretty_url: prettyJsUrl,
    js_embedded_pretty_url: prettyEmbeddedUrl,
    js_extracted_pretty_url: prettyExtractedUrl,
    sprite_pretty_url: blobUrl(r.prettySprite, 'image/svg+xml'),
    slice_pretty_url: prettySliceUrl,
    plan: r.plan,
    unsupported: r.unsupported,
    sizes: {
      json: { raw: json.length, gzipped: jsonGz },
      js: { raw: compiledJs.length, gzipped: jsGz },
      runtime_slice: { raw: slice.length, gzipped: sliceGz },
      ulottie_runtime: { raw: driver.length, gzipped: ulottieRuntimeGz },
      js_embedded: { raw: compiledEmbedded.length, gzipped: jsEmbeddedGz },
      js_extracted: { raw: compiledExtracted.length, gzipped: jsExtractedGz },
      sprite: { raw: sprite.length, gzipped: spriteGz },
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
