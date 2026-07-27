// The static-deployment compile backend: a Web Worker owning the wasm build of
// the compiler crate, off the main thread so a large fixture does not freeze
// the page. Same `{ compile, ready }` interface as `compiler-api.ts`, and the
// worker assembles the same response shape, so callers cannot tell them apart.

import type { WorkerRequest } from './compile-worker.ts';
import type { CompileResponse, SizeEntry } from './types.ts';

const worker = new Worker(new URL('./compile-worker.ts', import.meta.url), { type: 'module' });

/**
 * Size of the `lottie-web` bundle a page using it would ship.
 *
 * Measured at build time from the installed dependency (see `vite.config.ts`),
 * the same file the Rust backend reads — so bumping lottie-web moves the
 * baseline instead of leaving a stale constant. Measuring it at runtime is not
 * an option: fetching a JS file's URL goes back through Vite's transform and
 * returns the instrumented module, which reported 1.56 MB for a 298 KB file.
 */
const lottieRuntime: SizeEntry = __LOTTIE_WEB_SIZE__;

type Slot = {
  resolve: (r: CompileResponse) => void;
  reject: (e: Error) => void;
};
const pending = new Map<number, Slot>();
let nextId = 0;

// The worker can't accept messages until its top-level wasm init
// settles, so it postMessages `{ready: true}` once ready and we gate
// dispatch on this.
export const ready = new Promise<void>((resolve) => {
  worker.addEventListener('message', function onReady(e: MessageEvent) {
    if (e.data?.ready) {
      worker.removeEventListener('message', onReady);
      resolve();
    }
  });
});

worker.addEventListener('message', (e) => {
  if (e.data?.ready) return;
  const { id, response, error } = e.data;
  const slot = pending.get(id);
  if (!slot) return;
  pending.delete(id);
  if (error) slot.reject(new Error(error));
  else slot.resolve(response);
});
worker.addEventListener('error', (e) => {
  for (const slot of pending.values()) slot.reject(new Error(e.message ?? 'worker error'));
  pending.clear();
});

/** Compile a Lottie JSON string. Awaits `ready` internally so callers
 *  don't have to. */
export async function compile(jsonText: string): Promise<CompileResponse> {
  await ready;
  return new Promise<CompileResponse>((resolve, reject) => {
    const id = ++nextId;
    pending.set(id, { resolve, reject });
    worker.postMessage({ id, jsonText, lottieRuntime } satisfies WorkerRequest);
  });
}
