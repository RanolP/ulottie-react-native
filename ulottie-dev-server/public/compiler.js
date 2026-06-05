// Default (static-deployment) compile backend: spawn a Web Worker that
// owns the wasm-built ulottie compiler. Exports the same { compile,
// ready } interface as the api-mode shim that the dev server serves at
// this URL when running with --mode api; app.js imports `./compiler.js`
// and gets whichever variant the host provides.
//
// The worker measures lottie.min.js itself (constant, baked at vendor
// time) and constructs the full size response — main thread does no
// per-compile bookkeeping beyond id routing.

const worker = new Worker('./compile-worker.js', { type: 'module' });
const pending = new Map();
let nextId = 0;

// The worker can't accept messages until its top-level wasm init
// settles, so it postMessages `{ready: true}` once ready and we gate
// dispatch on this.
export const ready = new Promise((resolve) => {
  worker.addEventListener('message', function onReady(e) {
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
export async function compile(jsonText) {
  await ready;
  return new Promise((resolve, reject) => {
    const id = ++nextId;
    pending.set(id, { resolve, reject });
    worker.postMessage({ id, jsonText });
  });
}
