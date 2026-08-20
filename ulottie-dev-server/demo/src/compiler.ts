// Which compiler compiles.
//
// `vite dev` proxies `/compile` to the Rust dev server, so use that: it is the
// same code path the CLI takes, and it is faster than booting wasm. A built
// demo is a static site with no backend, so it uses the in-browser wasm build
// of the very same compiler crate. Both report the same shape — sizes, `plan`,
// `unsupported` — so nothing downstream cares which answered.
//
// `?compiler=api|wasm` overrides, which is how the wasm path gets exercised
// without a production build.

import type { CompileResult } from './types.ts';

const override = new URLSearchParams(location.search).get('compiler');
const useApi = override ? override === 'api' : import.meta.env.DEV;

/** Which compiler answered. Both extract the same image assets: the dev
 *  server writes the files and serves them, the wasm build hands the bytes
 *  back for the page to mint Blob URLs from. */
export const backend = useApi ? ('api' as const) : ('wasm' as const);

const backendModule = useApi ? import('./compiler-api.ts') : import('./compiler-wasm.ts');

export const ready = backendModule.then((m) => m.ready);

export async function compile(jsonText: string): Promise<CompileResult> {
  return (await backendModule).compile(jsonText);
}
