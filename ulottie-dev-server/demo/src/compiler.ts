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

import type { CompileResponse } from './types.ts';

const override = new URLSearchParams(location.search).get('compiler');
const useApi = override ? override === 'api' : import.meta.env.DEV;

const backend = useApi ? import('./compiler-api.ts') : import('./compiler-wasm.ts');

export const ready = backend.then((m) => m.ready);

export async function compile(jsonText: string): Promise<CompileResponse> {
  return (await backend).compile(jsonText);
}
