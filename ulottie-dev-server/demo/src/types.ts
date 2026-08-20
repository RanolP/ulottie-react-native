// JS-side types with no Rust counterpart.
//
// The `/compile` contract is NOT here: it is generated from
// `src/contract.rs` into `./generated/bindings.ts`, types and decoder
// together, so the page cannot drift from the server. Re-exported below so
// callers have one import site.

import type { CompileResponse } from './generated/bindings.ts';

export type {
  CompileResponse,
  FeatureReport,
  Plan,
  SizeEntry,
  Sizes,
  Unsupported,
} from './generated/bindings.ts';

/** One line of the extraction manifest — what a server turns into 103 Early
 *  Hints / `<link rel=preload as=image>` entries. `url` is the file's served
 *  URL on the dev-server path, and the Blob URL the page minted on the wasm
 *  path, where there is nowhere to write the file. */
export interface ManifestEntry {
  url: string;
  file: string;
  mime: string;
  bytes: number;
}

/** The compile contract, plus the assets the wasm worker minted as Blobs.
 *
 * The generated `CompileResponse` cannot carry them: it mirrors the server's
 * rkyv contract byte for byte. The server path fetches its manifest from
 * `/.output/<id>/assets/manifest.json` instead, so both backends end with the
 * same panel.
 */
export type CompileResult = CompileResponse & { assets?: ManifestEntry[] };

/** What a compiled module's `init` hands back. */
export interface Player {
  readonly totalFrames: number;
  readonly currentFrame: number;
  readonly isPlaying: boolean;
  play(): Player;
  pause(): Player;
  stop(): Player;
  seek(frame: number): Player;
  goToFrame(frame: number): Player;
  destroy(): void;
}

/** A compiled animation module. */
export interface UlottieModule {
  /** The markup the module carries — the document, or only its `<svg>` shell
   *  in extracted mode. Absent from a module compiled with `--no-markup`,
   *  which hydrates a served document and carries none. */
  markup?: string;
  /** `hydrate` adopts the `<svg>` already in the container instead of building
   *  one; a `--no-markup` module always does, and throws if there is none. */
  init(
    container: HTMLElement,
    options?: { autoplay?: boolean; loop?: boolean; hydrate?: boolean },
  ): Player;
}
