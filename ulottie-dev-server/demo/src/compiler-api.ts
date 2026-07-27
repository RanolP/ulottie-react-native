// Compile through the Rust dev server. Used when it is there to talk to —
// `vite dev` proxies `/compile` to it (see vite.config.ts).
//
// The response is an rkyv archive, decoded by bindings generated from the
// server's own `contract.rs`. No JSON, and no hand-written response type.

import { ArchivedCompileResponse } from './generated/bindings.ts';
import type { CompileResponse } from './types.ts';

export const ready = Promise.resolve();

export async function compile(jsonText: string): Promise<CompileResponse> {
  const r = await fetch('/compile', { method: 'POST', body: jsonText });
  if (!r.ok) throw new Error(await r.text());
  return ArchivedCompileResponse.decode(new Uint8Array(await r.arrayBuffer()));
}
