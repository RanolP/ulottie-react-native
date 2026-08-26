// μLottie RN runtime — mount, without a document.
//
// The web `mount` parses markup, adopts DOM nodes and hands the ops
// `svg.querySelectorAll('*')`. Here the compiler already emitted the element
// tree as a static descriptor (`tree` in the generated module), so mounting
// is pure data: decode the stream, widen the easing/clock/gate tables exactly
// as core.js does, and hand the program an `els` array of plain prop-store
// handles in the same document order `querySelectorAll` would have produced
// (root `<svg>` excluded — it stays static, animated writes go to inner
// elements only).
//
//   D — the payload, one integer stream;
//   P/A — the emitted bind/apply program pair;
//   N — the element count (handles are dense; most never receive a write);
//   ext.p — the string pool; ext.r — the time-remap resolver, when used.
//
// `apply(f)` runs one frame: clocks, gates, one program pass. Changed
// elements collect in `dirty`; the consumer drains it (clearing each `d`)
// and pushes `p` into the matching react-native-svg elements.

import { dec } from '../vlq.js';
import { H_FR, H_IP, H_OP, H_EASINGS, H_TIMELINES, H_GATES, H_PROGRAM, H_REMAPS } from '../wire.js';
import { INV } from '../scale.js';

export function mountRn(D, P, A, N, ext) {
  ext = ext || {};
  // The immutable half — the payload decode, the widened easing handles and
  // the arc-length table cache — is identical for every instance of one
  // module, so it is built once per JS runtime and shared; init() then only
  // creates per-instance state (clocks, gates, element handles, bind state).
  // The cache must live on `globalThis`: worklet closures capture module
  // scope BY COPY at definition time, so a module-level `let` mutated on the
  // UI runtime would not persist across init() calls. The payload string is
  // its own cache key — it names the animation content exactly, and every
  // call passes the same captured string object, so the engine's cached
  // string hash makes the lookup cheap.
  const g = globalThis;
  const cache = g.__ulottie || (g.__ulottie = {});
  let sh = cache[D];
  if (!sh) {
    const dS = dec(D);

    // Easing handles, widened once — same as core.js.
    const ez = dS[H_EASINGS];
    const easings = [];
    if (ez) {
      for (let i = 0, n = dS[ez]; i < n; i++) {
        const at = ez + 1 + i * 4;
        easings.push([dS[at] / 1000, dS[at + 1] / 1000, dS[at + 2] / 1000, dS[at + 3] / 1000]);
      }
    }

    // `sp` holds spatial arc-length tables, keyed by tangent-column offset —
    // deterministic from the stream alone (see `pvv`), so instances share it.
    sh = cache[D] = { S: dS, z: easings, sp: new Map() };
  }
  const S = sh.S;
  const str = ext.p || [];

  const fr = S[H_FR] / 1000;
  const ip = S[H_IP] / 1000;
  const op = S[H_OP] / 1000;

  // Precomp clocks — see core.js for the row layout.
  const tl = S[H_TIMELINES];
  const nTl = tl ? S[tl] : 0;
  const tScale = nTl ? INV[S[tl + 1]] : 1;
  const tRows = tl + 2;
  const T = new Float64Array(nTl + 1);

  // Visibility gates — gate 0 pinned on, as on the web.
  const gt = S[H_GATES];
  const nGates = gt ? S[gt] : 0;
  const gScale = nGates ? INV[S[gt + 1]] : 1;
  const gRows = gt + 2;
  const ON = new Uint8Array(nGates + 1);
  ON[0] = 1;

  // Element handles: slot index, dynamic props, dirty flag, shared queue.
  // `x.els[i]` replaces the web's NodeList — same indexing, plain records.
  const dirty = [];
  const els = new Array(N);
  for (let i = 0; i < N; i++) els[i] = { i, p: {}, d: 0, q: dirty };

  const ctx = {
    S, str, els, z: sh.z,
    fr, frame: 0, T, ON, sp: sh.sp, expr: null, y: null,
  };

  // Time remap — the column is parallel to the timeline table.
  const rmc = S[H_REMAPS];
  const rm = rmc && ext.r
    ? Array.from({ length: S[rmc] }, (_, i) => (S[rmc + 1 + i] ? ext.r(S[rmc + 1 + i], ctx) : 0))
    : null;

  // One program: precomp instancing is off in this target, so every binding
  // lives in the document program and there is no uses/assets table to walk.
  const prog = S[H_PROGRAM];
  const st = prog ? P(ctx, S.subarray(prog + 1, prog + 1 + S[prog]), 0, 0, 0, 0) : null;

  function apply(f) {
    ctx.frame = f;
    T[0] = f;
    for (let i = 0; i < nTl; i++) {
      const e = tRows + i * 5;
      const remap = rm && rm[i];
      if (remap) {
        // Lottie stores the remap in seconds; the timeline is in frames.
        T[i + 1] = remap(T[S[e]]) * fr;
        continue;
      }
      T[i + 1] = (T[S[e]] - S[e + 1] * tScale) / (S[e + 2] / 1000);
    }
    for (let i = 0; i < nGates; i++) {
      const g = gRows + i * 2;
      ON[i + 1] = f >= S[g] * gScale && f < S[g + 1] * gScale ? 1 : 0;
    }
    if (st) A(ctx, st);
  }

  return { els, dirty, apply, fr, ip, op };
}
