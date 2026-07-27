// JS-side types with no Rust counterpart.
//
// The `/compile` contract is NOT here: it is generated from
// `src/contract.rs` into `./generated/bindings.ts`, types and decoder
// together, so the page cannot drift from the server. Re-exported below so
// callers have one import site.

export type {
  CompileResponse,
  FeatureReport,
  Plan,
  SizeEntry,
  Sizes,
  Unsupported,
} from './generated/bindings.ts';

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
  markup: string;
  init(
    container: HTMLElement,
    options?: { autoplay?: boolean; loop?: boolean; hydrate?: boolean },
  ): Player;
}
