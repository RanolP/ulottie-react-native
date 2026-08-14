// Browser pixel-diff tests. For each fixture, render lottie-web (reference)
// and ulottie (compiled) at sample frames, screenshot both, and compare with
// odiff via the host-side `odiffCompare` command.
//
// This is the visual ground-truth regression gate. The Rust-side
// `tests/frame_snapshot.rs` is the fast unit-level gate; this catches
// genuine rendering divergence that text snapshots miss.
//
// Prereq: harness/<fixture>.json and harness/<fixture>.js must exist. Run
// `node build.mjs` (or rely on the snapshot.mjs pipeline) first.

import { afterEach, beforeAll, describe, expect, test } from 'vitest';

/** `page.screenshot` is typed as returning a path, but hands back a
 *  `{ path }` descriptor in some builds. Accept either. */
const shotPath = (shot: unknown): string =>
  typeof shot === 'string' ? shot : (shot as { path: string }).path;
import { commands, page } from 'vitest/browser';
import { lottie } from '../demo/src/lottie.js';

// Fixture × sample-frame grid. Sample at canonical anchors so a regression
// in mid-animation is caught even if the endpoints happen to coincide.
const FIXTURES: ReadonlyArray<{ name: string; tolerance?: number }> = [
  { name: 'rectangle' },
  { name: 'ellipse' },
  { name: 'fill' },
  { name: 'trim_path' },
  { name: 'boucing_ball' },
  { name: 'lottie_logo_1' },
  { name: 'starfish' },
  { name: 'ripple' },
  { name: 'precomp_star_circle' },
  { name: 'lights' },
  // Feature fixtures: each is the smallest file in a 93-animation survey that
  // exercises one construct nothing else did, and renders at exactly 0.000%
  // across fifteen frames. See _fixtures/PROVENANCE.md.
  { name: 'gradient_radial' },
  { name: 'image_layer' },
  { name: 'image_embedded' },
  { name: 'mask_subtract' },
  { name: 'matte_alpha' },
  { name: 'matte_luma' },
  // `gradient_animated` is hand-made for `gradient:animated-ramp`: an animated
  // colour ramp without alpha stops, the one ramp shape a fixed set of `<stop>`
  // elements can follow (alpha stops are refused — see `AnimatedGradient`).
  { name: 'gradient_animated' },
  // NOT here: `matte_luma_inv` (tt:4). lottie-web's `getMatte` creates a mask
  // for matte types 1–3 only, so its tt:4 output references a mask that does
  // not exist — and Chrome draws an element with an unresolvable mask
  // reference *unmasked*. Pixel parity with lottie-web is therefore
  // unreachable by construction; like `tp`-without-`td`, this compiler
  // renders what After Effects means instead, and the construct is pinned by
  // structural assertions in `ulottie-compiler/tests/track_matte.rs` plus the
  // Rust-side reference render in `tests/frame_snapshot.rs`.
  { name: 'stroke_under_fill' },
  // Hand-made, for the one shape of expression the other three do not have: a
  // body that reads another layer and nothing else. `ripple`, `starfish` and
  // `lights` all touch `thisProperty`, which is what kept its runtime in their
  // bundles — so a shaken-out expression helper rendered correctly in every
  // fixture and silently returned the authored constant everywhere else.
  { name: 'expression_layer_ref' },
  // The Lottie logo family (`_1` is the original wordmark from above's
  // entry; `_2`/`_3` the lottie-flutter variants), AndroidWave and the
  // multiply blend — all pixel-exact against lottie-web (AndroidWave's
  // merge-paths modifier is dropped by both renderers, and its merged
  // shapes are static, so the allowance is invisible).
  { name: 'lottie_logo_2' },
  { name: 'lottie_logo_3' },
  { name: 'android_wave' },
  { name: 'blend_multiply' },
  { name: 'text_baseline' },
  // NOT here, where lottie-web is the one that is wrong — the Rust-side
  // reference render is their gate:
  //   `matte_luma_inv` (tt:4) and `fireworks`: lottie-web cannot render the
  //   first at all, and its repeater clones the trim into every copy *and*
  //   keeps the layer-level trim, trimming each repeated stroke twice (arc
  //   = e² of the property). AE trims once and repeats; so does this
  //   compiler.
  //   `bodymoovin` renders at 0.05% but carries a merge-paths allowance, so
  //   it stays out of this table.
];

const SAMPLES = [0, 0.25, 0.5, 0.75, 0.99] as const;

// Acceptable pixel-diff ratio. Differences below this pass; above this fail.
// Default starts wide; per-fixture overrides tune as we go.
const DEFAULT_TOLERANCE = 0.005; // 0.5%

const VIEWPORT_W = 320;
const VIEWPORT_H = 320;

function mountContainers(): { ref: HTMLDivElement; ulottie: HTMLDivElement } {
  document.body.innerHTML = '';
  // Side-by-side panels — both square, identical CSS, so renderers can't
  // accidentally be different sizes.
  const wrap = document.createElement('div');
  wrap.style.cssText = `display:flex;gap:0;align-items:flex-start;background:white;padding:0;margin:0;`;
  const ref = document.createElement('div');
  ref.id = 'ref-panel';
  const ulottie = document.createElement('div');
  ulottie.id = 'ulottie-panel';
  for (const el of [ref, ulottie]) {
    el.style.cssText = `width:${VIEWPORT_W}px;height:${VIEWPORT_H}px;overflow:hidden;background:white;`;
  }
  wrap.appendChild(ref);
  wrap.appendChild(ulottie);
  document.body.appendChild(wrap);
  return { ref, ulottie };
}

async function loadFixture(
  name: string,
  refEl: HTMLElement,
  ulottieEl: HTMLElement,
  variant: 'extern' | 'embedded' | 'extracted' | 'instanced' = 'extern',
): Promise<{
  totalFrames: number;
  goToFrame: (f: number) => void;
  destroy: () => void;
}> {
  // Reference panel: lottie-web fetches the source via `path`. The dev
  // server serves registered fixtures at `/.output/<name>.json` (with a
  // fallback to `_fixtures/animations/`).
  const refAnim = lottie.loadAnimation({
    container: refEl,
    renderer: 'svg',
    loop: false,
    autoplay: false,
    path: `/.output/${name}.json`,
    rendererSettings: {
      preserveAspectRatio: 'xMidYMid meet',
    },
  });
  await new Promise<void>((resolve, reject) => {
    refAnim.addEventListener('DOMLoaded', () => resolve());
    refAnim.addEventListener('data_failed', () =>
      reject(new Error('lottie-web failed to load ' + name)),
    );
  });

  // ulottie panel: dynamic import the compiled module. The dev server
  // lazy-compiles `_fixtures/animations/<name>.json` into `.output/<name>.js`
  // before ServeDir hands it over.
  // Extracted mode carries no markup: the elements live in a sprite the page
  // has to have in the document before `init()` runs. Injecting it here is the
  // test's stand-in for a server inlining it into the HTML.
  if (variant === 'extracted') {
    const svg = await fetch(`/.output/${name}.sprite.svg`).then(r => r.text());
    const holder = document.createElement('div');
    holder.innerHTML = svg;
    document.body.appendChild(holder);
  }

  const suffix =
    variant === 'embedded' ? '.embedded.js'
    : variant === 'extracted' ? '.extracted.js'
    : variant === 'instanced' ? '.instanced.js'
    : '.js';
  const ulottieMod = await import(/* @vite-ignore */ `/.output/${name}${suffix}?t=${Date.now()}`);
  const ulottieResult = ulottieMod.init(ulottieEl);
  const totalFrames = Math.round(refAnim.totalFrames || ulottieResult.totalFrames || 0);
  // Both rendered immediately at frame 0; pause both.
  refAnim.goToAndStop(0, true);
  if (ulottieResult.goToFrame) ulottieResult.goToFrame(0);
  return {
    totalFrames,
    goToFrame: (f: number) => {
      refAnim.goToAndStop(f, true);
      if (ulottieResult.goToFrame) ulottieResult.goToFrame(f);
    },
    destroy: () => {
      refAnim.destroy();
      if (ulottieResult.destroy) ulottieResult.destroy();
    },
  };
}

// The extern and embedded builds are different assemblies of the same scene:
// extern imports the runtime as ES modules, embedded inlines a tree-shaken copy.
// Only testing one of them leaves the other free to break — which it did.
describe('embedded build renders', () => {
  beforeAll(async () => {
    await page.viewport(VIEWPORT_W * 2 + 40, VIEWPORT_H + 40);
  });

  for (const fx of FIXTURES) {
    test(`${fx.name}`, { timeout: 30_000 }, async () => {
      const { ref, ulottie } = mountContainers();
      const anim = await loadFixture(fx.name, ref, ulottie, 'embedded');
      try {
        anim.goToFrame(Math.floor(anim.totalFrames * 0.5));
        await new Promise(r => requestAnimationFrame(() => r(undefined)));
        await new Promise(r => requestAnimationFrame(() => r(undefined)));

        const refShot = await page.screenshot({ element: ref, save: true });
        const ulottieShot = await page.screenshot({ element: ulottie, save: true });
        const refPath = shotPath(refShot);
        const ulottiePath = shotPath(ulottieShot);

        const diff = await commands.odiffCompare(refPath, ulottiePath, { antialiasing: true });
        if (!diff.match) {
          const ratio = diff.diffPercentage / 100;
          const tolerance = fx.tolerance ?? DEFAULT_TOLERANCE;
          if (ratio > tolerance) {
            throw new Error(
              `Embedded build of ${fx.name} diverges: ` +
                `${diff.diffPercentage.toFixed(3)}% > ${(tolerance * 100).toFixed(3)}%. ` +
                `diff: ${diff.diffPath}`,
            );
          }
        }
      } finally {
        anim.destroy();
      }
    });
  }
});

// Extracted markup takes a different path to the same DOM: the module clones a
// `<symbol>`'s children into its shell instead of parsing a string. The element
// sequence `querySelectorAll('*')` yields has to come out identical, or every
// binding addresses the wrong node — which a pixel diff catches immediately.
describe('extracted markup renders', () => {
  beforeAll(async () => {
    await page.viewport(VIEWPORT_W * 2 + 40, VIEWPORT_H + 40);
  });

  for (const fx of FIXTURES) {
    test(`${fx.name}`, { timeout: 30_000 }, async () => {
      const { ref, ulottie } = mountContainers();
      const anim = await loadFixture(fx.name, ref, ulottie, 'extracted');
      try {
        anim.goToFrame(Math.floor(anim.totalFrames * 0.5));
        await new Promise(r => requestAnimationFrame(() => r(undefined)));
        await new Promise(r => requestAnimationFrame(() => r(undefined)));

        const refShot = await page.screenshot({ element: ref, save: true });
        const ulottieShot = await page.screenshot({ element: ulottie, save: true });
        const refPath = shotPath(refShot);
        const ulottiePath = shotPath(ulottieShot);

        const diff = await commands.odiffCompare(refPath, ulottiePath, { antialiasing: true });
        if (!diff.match) {
          const ratio = diff.diffPercentage / 100;
          const tolerance = fx.tolerance ?? DEFAULT_TOLERANCE;
          if (ratio > tolerance) {
            throw new Error(
              `Extracted build of ${fx.name} diverges: ` +
                `${diff.diffPercentage.toFixed(3)}% > ${(tolerance * 100).toFixed(3)}%. ` +
                `diff: ${diff.diffPath}`,
            );
          }
        }
      } finally {
        anim.destroy();
      }
    });
  }
});

// Precomp instancing plans an asset once and replays its bindings per use, so
// it takes a completely different path through mount() — one that was never
// visually tested, and had shipped a bug: a fully-instanced animation has no
// document-level bindings at all, and playback was gated on that count, so it
// mounted at frame 0 and froze.
describe('instanced precomps render and animate', () => {
  beforeAll(async () => {
    await page.viewport(VIEWPORT_W * 2 + 40, VIEWPORT_H + 40);
  });

  for (const fx of FIXTURES) {
    test(`${fx.name}`, { timeout: 30_000 }, async () => {
      const { ref, ulottie } = mountContainers();
      const anim = await loadFixture(fx.name, ref, ulottie, 'instanced');
      try {
        anim.goToFrame(Math.floor(anim.totalFrames * 0.5));
        await new Promise(r => requestAnimationFrame(() => r(undefined)));
        await new Promise(r => requestAnimationFrame(() => r(undefined)));

        const refShot = await page.screenshot({ element: ref, save: true });
        const ulottieShot = await page.screenshot({ element: ulottie, save: true });
        const refPath = shotPath(refShot);
        const ulottiePath = shotPath(ulottieShot);

        const diff = await commands.odiffCompare(refPath, ulottiePath, { antialiasing: true });
        if (!diff.match) {
          const ratio = diff.diffPercentage / 100;
          const tolerance = fx.tolerance ?? DEFAULT_TOLERANCE;
          if (ratio > tolerance) {
            throw new Error(
              `Instanced build of ${fx.name} diverges: ` +
                `${diff.diffPercentage.toFixed(3)}% > ${(tolerance * 100).toFixed(3)}%. ` +
                `diff: ${diff.diffPath}`,
            );
          }
        }
      } finally {
        anim.destroy();
      }
    });
  }

  // The frozen-at-frame-0 bug rendered correctly at frame 0 and only showed up
  // in motion, which a single-frame pixel diff cannot see. Assert playback
  // actually starts.
  test('an all-instanced animation autoplays', { timeout: 30_000 }, async () => {
    document.body.innerHTML = '';
    const host = document.createElement('div');
    host.style.cssText = 'width:200px;height:200px';
    document.body.appendChild(host);
    // ripple compiles to zero document-level bindings — all 230 belong to
    // instanced assets — which is exactly the case that used to freeze.
    const mod = await import(/* @vite-ignore */ `/.output/ripple.instanced.js?t=${Date.now()}`);
    const player = mod.init(host, { autoplay: true });
    try {
      expect(player.isPlaying, 'a fully-instanced animation must autoplay').toBe(true);
      const start = player.currentFrame;
      await new Promise(r => setTimeout(r, 250));
      expect(player.currentFrame, 'the frame counter must advance').not.toBe(start);
    } finally {
      player.destroy();
    }
  });
});

// `thisComp.layer()` — by name or by index — resolves within one composition.
// A document that inlines two precomps holds two sets of layers whose names and
// indices both restart, and two layers called `Ball` at index 1 are two
// different layers. Before scoping was wired up every inlined record was placed
// at scope 0 and the last one silently won.
//
// The compiler answers that question now, so the claim it makes is pinned on
// the compiler side (`backend::layers::index_tests`). What is left here is the
// runtime half: `lyLink` giving every record the table and slot the resolved
// spelling indexes through, and the fallback lookup still keying on scope for
// the bodies the pass refused.
//
// After Effects auto-names layers `Shape Layer 1` / `Null 1` per comp and
// numbers them from 1, so this collision is the common case, not an exotic one.
describe('a record knows its own table and slot', () => {
  test('two comps with the same name and index stay distinct', async () => {
    const { makeExpr, lyAt } = await import(
      /* @vite-ignore */ `/.output/runtime/expr.js?t=${Date.now()}`
    );
    // Two comps, each with one layer called 'Ball' at index 1 — the shape a
    // document gets when it inlines two precomps. Nothing looks either up by
    // name any more, so what has to hold is that they are separate records and
    // that `lyAt` reaches the right one from either.
    //
    // Built as the integer stream `mount` would decode, minus the base36 hop:
    // `makeExpr` reads `ctx.S`, so going through the codec here would only test
    // the codec. Layout is `scene/flat.rs` — header, then sections.
    const HEAD = 17;
    const LAYERS = HEAD;          // [count, …rowOffsets, …rows]
    const ROW0 = LAYERS + 3;
    const ROW1 = ROW0 + 3;
    const NAME = 1;               // presence bit for the name field
    const S = new Int32Array(ROW1 + 3);
    S[1] = 60_000;                // frame rate, x1000
    S[12] = LAYERS;
    // The row table is delta-encoded, to keep the offsets small.
    S.set([2, ROW0, ROW1 - ROW0], LAYERS);
    // [mask, compIndex, nameIndex] — same name, same index, different comps.
    S.set([NAME, 1, 0], ROW0);
    S.set([NAME, 1, 0], ROW1);

    // `y` is the record table `mount` decodes out of the stream — this test
    // never calls `mount`, so it hands over the decoded form directly.
    const ctx = { S, str: ['Ball'], fr: 60, y: [ROW0, ROW1], svg: null, z: [], frame: 0 } as unknown as {
      recs: { _t: unknown[]; _i: number }[];
    };
    makeExpr([], ctx);

    // A resolved reference is `lyAt(thisLayer, i)`, which walks the record's own
    // table — so every record has to carry that table and its slot in it.
    const [first, second] = ctx.recs;
    expect(first).not.toBe(second);
    expect(first._t, 'the record knows its table').toBe(ctx.recs);
    expect(first._i).toBe(0);
    expect(second._i).toBe(1);
    expect(lyAt(first, 1), 'a slot resolves against the owner').toBe(second);
    expect(lyAt(second, 0)).toBe(first);
  });
});

// The attribute formatter is the hottest thing the runtime does, so it
// assembles digits from the rounded integer instead of handing a float to
// `toString`. That is only allowed if it is byte-identical to the float
// spelling — a differing digit is a differing picture, and the pixel diff
// would only catch it if the divergence happened to be large.
// The spelling `num.js` produces is a contract, not an implementation detail:
// it is what every snapshot in `_fixtures/__snapshots__/` was blessed against,
// and the dropped leading zero is worth real bytes across a path string. Pin
// the spelling and the per-role precision so a rewrite has to reproduce both.
describe('the number formatter', () => {
  test('spells values the way the snapshots expect', async () => {
    const { r, r5, r2 } = await import(
      /* @vite-ignore */ `/.output/runtime/num.js?t=${Date.now()}`
    );
    // [input, r (3dp), r5 (5dp), r2 (2dp)]
    const cases: [number, string, string, string][] = [
      [0, '0', '0', '0'],
      [-0, '0', '0', '0'],
      [1, '1', '1', '1'],
      // A bare leading zero is dropped, on both signs.
      [0.5, '.5', '.5', '.5'],
      [-0.5, '-.5', '-.5', '-.5'],
      // Trailing zeros never appear, whatever the role's precision.
      [1.5, '1.5', '1.5', '1.5'],
      [0.25, '.25', '.25', '.25'],
      // Each role rounds at its own quantum.
      [1.23456789, '1.235', '1.23457', '1.23'],
      [0.000004, '0', '0', '0'],
      [0.001, '.001', '.001', '0'],
      // Rounding that carries into the whole part.
      [0.9999, '1', '.9999', '1'],
      [-0.9999, '-1', '-.9999', '-1'],
      // Non-finite input must not throw — it reaches an attribute as-is.
      [Infinity, 'Infinity', 'Infinity', 'Infinity'],
      [NaN, 'NaN', 'NaN', 'NaN'],
    ];
    const bad: string[] = [];
    for (const [x, e3, e5, e2] of cases) {
      for (const [fn, want, name] of [[r, e3, 'r'], [r5, e5, 'r5'], [r2, e2, 'r2']] as const) {
        const got = fn(x);
        if (got !== want) bad.push(`${name}(${x}) = "${got}", expected "${want}"`);
      }
    }
    expect(bad, `${bad.length} divergences`).toEqual([]);
  });
});

// Geometry parity, as a backstop for the pixel diff.
//
// odiff runs with `antialiasing: true`, which discounts pixels it judges to be
// antialiasing artifacts. For an animation drawn entirely in hairlines that is
// almost every pixel it looks at, so `ripple` passed the pixel gate while
// rendering a visibly different picture. Comparing the bounding box of all
// drawn geometry, normalised to the SVG viewport, is immune to that: it does
// not care about colour, thickness or edge softness, only about where the
// drawing actually is.
const GEOMETRY_TOLERANCE = 0.02; // 2% of the viewport

// An animated mask has to keep animating.
//
// `starfish`'s eye is a precomp whose clock is a time remap, and the eyelid is
// an animated mask inside it. Every gate above missed it losing that: the mask
// only moves for ~18 frames either side of t≈0.09 and t≈0.57, and `SAMPLES`
// steps over both; the geometry check reads `getBoundingClientRect`, which is
// blind to clipping; and the pixel check never looked at those frames. So the
// starfish stopped winking and nothing said a word.
//
// This asks the narrow question the others cannot: does the mask's `d` take
// more than one value over the animation? It is deliberately not a comparison
// against lottie-web — a frozen mask is wrong on its own terms.
describe('an animated mask keeps animating', () => {
  test('starfish', { timeout: 60_000 }, async () => {
    const { ref, ulottie } = mountContainers();
    const anim = await loadFixture('starfish', ref, ulottie, 'embedded');
    try {
      const seen = new Set<string>();
      for (let f = 0; f <= anim.totalFrames; f += 4) {
        anim.goToFrame(f);
        await new Promise(r => requestAnimationFrame(() => r(undefined)));
        // Either holder: an additive, opaque, non-inverted mask compiles to a
        // `<clipPath>`, the way lottie-web's `MaskElement` picks between the
        // two. Which one it is says nothing about whether the shape moves.
        for (const p of ulottie.querySelectorAll('mask path, clipPath path')) {
          seen.add(p.getAttribute('d') ?? '');
        }
      }
      expect(
        seen.size,
        `starfish's eyelid mask never moves — it took one shape across the whole animation`,
      ).toBeGreaterThan(1);
    } finally {
      anim.destroy();
    }
  });
});

describe('geometry parity vs lottie-web', () => {
  beforeAll(async () => {
    await page.viewport(VIEWPORT_W * 2 + 40, VIEWPORT_H + 40);
  });

  /** Bounding box of every drawn shape, as a fraction of the SVG viewport. */
  function drawnBox(host: HTMLElement) {
    const svg = host.querySelector('svg');
    if (!svg) return null;
    const s = svg.getBoundingClientRect();
    let x0 = Infinity, y0 = Infinity, x1 = -Infinity, y1 = -Infinity, n = 0;
    for (const el of svg.querySelectorAll('path,rect,ellipse,circle,line,polygon')) {
      const b = el.getBoundingClientRect();
      // `getBoundingClientRect` is a *fill* box: it does not include the
      // stroke. For an open, nearly straight contour — what a trim leaves of
      // a swoosh — that box is a subpixel sliver at the mercy of its slope,
      // unrelated to where the stroke actually paints (± half the stroke
      // width beyond it). A box thinner than the element's own stroke
      // under-measures its mark on both renderers' spellings (`fill="none"`
      // here, `fill-opacity="0"` in lottie-web), so both are skipped and the
      // pixel gate owns them.
      const sw = parseFloat(el.getAttribute('stroke-width') ?? '0');
      if (sw > 0 && (b.width < sw || b.height < sw)) continue;
      // `getBoundingClientRect` cannot see clipping, and both renderers clip
      // to the viewport — but they carry *different* amounts of geometry
      // parked outside it (`lottie_logo_2`'s mid-flight swoosh leaves its
      // trim's empty tails at different places, invisible either way). What
      // renders is the intersection with the SVG's own box, so that is what
      // this measures; a box that vanishes under the clamp drew nothing.
      const l = Math.max(b.left, s.left);
      const r = Math.min(b.right, s.right);
      const t = Math.max(b.top, s.top);
      const bm = Math.min(b.bottom, s.bottom);
      // A box one pixel thin in either dimension paints at most an
      // antialiasing hint — a trimmed stroke's subpixel tail (`lottie_logo_2`
      // keeps one crossing the viewport edge that lottie-web trims away) —
      // and the pixel gate owns those. Its *position* was never well measured
      // by a box anyway.
      if (r - l < 1 || bm - t < 1) continue;
      n++;
      x0 = Math.min(x0, l); y0 = Math.min(y0, t);
      x1 = Math.max(x1, r); y1 = Math.max(y1, bm);
    }
    if (!n || !s.width || !s.height) return null;
    return {
      x: (x0 - s.left) / s.width,
      y: (y0 - s.top) / s.height,
      w: (x1 - x0) / s.width,
      h: (y1 - y0) / s.height,
    };
  }

  for (const fx of FIXTURES) {
    test(`${fx.name}`, { timeout: 30_000 }, async () => {
      const { ref, ulottie } = mountContainers();
      const anim = await loadFixture(fx.name, ref, ulottie);
      try {
        anim.goToFrame(Math.floor(anim.totalFrames * 0.5));
        await new Promise(r => requestAnimationFrame(() => r(undefined)));
        await new Promise(r => requestAnimationFrame(() => r(undefined)));

        const a = drawnBox(ref);
        const b = drawnBox(ulottie);
        // A fixture that draws nothing at the midpoint has nothing to compare.
        if (!a || !b) return;

        const off = (Object.keys(a) as (keyof typeof a)[])
          .filter(k => Math.abs(a[k] - b[k]) > GEOMETRY_TOLERANCE)
          .map(k => `${k}: ${a[k].toFixed(3)} vs ${b[k].toFixed(3)}`);

        expect(off, `${fx.name} geometry diverges — ${off.join(', ')}`).toEqual([]);
      } finally {
        anim.destroy();
      }
    });
  }

  // The same check against the build that actually ships, at every sample
  // frame rather than only the midpoint.
  //
  // `--embedded` is code-generated for the three expression fixtures: the
  // record table and the expression handles are emitted as JS literals instead
  // of decoded from the stream, so it is a genuinely different assembly of the
  // layer references, not a repackaging of the extern one. Until this existed
  // it was pixel-checked at t=0.5 and geometry-checked not at all, which is
  // exactly the shape of hole a mis-resolved layer handle hides in: `lyAt` /
  // `lyRel` off by one slot moves a layer by a constant, and a constant offset
  // on hairline geometry is what odiff's antialiasing pass discounts. Geometry
  // is blind to colour and edge softness and sees position, so it catches what
  // the pixel gate cannot — and sweeping the frames catches the ones that only
  // diverge once something has had time to interpolate.
  //
  // It earned its keep immediately: `starfish` was 10 px out at t=0.5 in the
  // shipped build and exact in the extern one, because `codegen`'s columnar
  // keyframe form dropped spatial tangents while its unrolled form kept them.
  // Two of that limb's position keyframes are equal, so the whole excursion
  // between them *was* the tangent — a straight line between two identical
  // points does not move at all.
  //
  for (const fx of FIXTURES) {
    test(`${fx.name} (embedded, across the animation)`, { timeout: 60_000 }, async () => {
      const { ref, ulottie } = mountContainers();
      const anim = await loadFixture(fx.name, ref, ulottie, 'embedded');
      try {
        const off: string[] = [];
        for (const s of SAMPLES) {
          anim.goToFrame(Math.floor(anim.totalFrames * s));
          await new Promise(r => requestAnimationFrame(() => r(undefined)));
          await new Promise(r => requestAnimationFrame(() => r(undefined)));

          const a = drawnBox(ref);
          const b = drawnBox(ulottie);
          // Nothing drawn at this frame is nothing to compare, not a failure.
          if (!a || !b) continue;
          for (const k of Object.keys(a) as (keyof typeof a)[]) {
            if (Math.abs(a[k] - b[k]) > GEOMETRY_TOLERANCE) {
              off.push(`t=${s} ${k}: ${a[k].toFixed(3)} vs ${b[k].toFixed(3)}`);
            }
          }
        }

        expect(off, `${fx.name} embedded geometry diverges — ${off.join(', ')}`).toEqual([]);
      } finally {
        anim.destroy();
      }
    });
  }
});

// A sprite is a real `.svg` file, so it has to be well-formed XML — unlike
// inlined markup, which the page parses with the lenient HTML parser. This is
// not theoretical: the id placeholder used to be U+0001, which `innerHTML`
// accepts and `DOMParser` rejects outright, so the mode would have shipped
// files no browser could load as an image.
describe('the sprite is a valid standalone SVG', () => {
  for (const fx of FIXTURES) {
    test(`${fx.name}`, { timeout: 30_000 }, async () => {
      const text = await fetch(`/.output/${fx.name}.sprite.svg`).then(r => r.text());
      const doc = new DOMParser().parseFromString(text, 'image/svg+xml');
      const err = doc.querySelector('parsererror');
      expect(err?.textContent ?? '').toBe('');
      expect(doc.documentElement.namespaceURI).toBe('http://www.w3.org/2000/svg');
      expect(doc.querySelector(`symbol[id="${fx.name}"]`)).toBeTruthy();
    });
  }
});

// Fixtures whose first frame depends on an expression. The compiler cannot
// evaluate one ahead of time — that needs the expression engine, which is
// JavaScript — so those properties bake to their fallback and the static
// picture is an approximation. Everything else has to be exact.
//
// Pinned rather than skipped: if one of these starts matching, the expression
// compiler landed and the entry should go.
const EXPRESSION_DRIVEN = new Set(['lights', 'ripple', 'starfish']);

// The sprite has to be a finished picture *before* any script runs: it is what
// an SSR response paints, what `<noscript>` falls back to, and what an
// `<img src="…svg">` shows. The planner writes only frame-invariant values into
// markup, so without a bake a layer with an animated transform has no
// `transform` at all and lands at the origin — `bouncy_ball` rendered
// off-centre and `lights` rendered as one bulb on top of another.
//
// `player()` calls `apply(ip)` before scheduling anything, so a module mounted
// with `autoplay: false` *is* the composition's first frame. That makes it the
// oracle: every attribute the runtime writes on mount has to already be in the
// sprite, with the same value.
/**
 * One element as a comparable shape.
 *
 * Attribute *order* is not meaningful and neither is serialization, so this
 * compares maps rather than markup — `innerHTML` differs between a tree the XML
 * parser produced and one built by the runtime, for reasons (explicit `xmlns`,
 * attribute order) that have nothing to do with what is drawn.
 *
 * Per-mount id suffixes are meant to differ, and so is anything referencing
 * one. `display` is read off `style` because the runtime sets the property
 * rather than the attribute.
 */
const shape = (el: Element) => {
  const attrs: Record<string, string> = {};
  for (const at of el.attributes) {
    if (at.name === 'id' || at.name === 'style' || at.name === 'xmlns') continue;
    if (at.value.includes('url(#') || at.value.includes('--u')) continue;
    attrs[at.name] = at.value;
  }
  attrs['@display'] = (el as SVGElement).style?.display ?? '';
  return { tag: el.tagName, attrs };
};

/** Element-wise attribute differences between two rendered trees. */
const compare = (a: ReturnType<typeof shape>[], b: ReturnType<typeof shape>[]) => {
  const off: string[] = [];
  for (const [i, x] of a.entries()) {
    const y = b[i];
    if (!y) { off.push(`[${i}] <${x.tag}> missing`); continue; }
    if (x.tag !== y.tag) { off.push(`[${i}] ${x.tag} vs ${y.tag}`); continue; }
    for (const k of new Set([...Object.keys(x.attrs), ...Object.keys(y.attrs)])) {
      if (x.attrs[k] === y.attrs[k]) continue;
      // A transform is the same value through two roundings — the bake folds
      // it in f64, the runtime descales integers — and a tie in the last
      // digit (`-605.965`) can round either way. One unit either side is
      // below anything a pixel can see.
      if (k === 'transform') {
        const xs = x.attrs[k].match(/-?\d+(\.\d+)?(e-?\d+)?/g) ?? [];
        const ys = y.attrs[k].match(/-?\d+(\.\d+)?(e-?\d+)?/g) ?? [];
        if (xs.length === ys.length
          && xs.every((v, j) => Math.abs(parseFloat(v) - parseFloat(ys[j])) <= 0.011)) {
          continue;
        }
      }
      off.push(`[${i}] <${x.tag}> ${k}: ${x.attrs[k]} vs ${y.attrs[k]}`);
    }
  }
  return off;
};

describe('the sprite is the first frame, with no script', () => {
  for (const fx of FIXTURES) {
    test(`${fx.name}`, { timeout: 30_000 }, async () => {
      const text = await fetch(`/.output/${fx.name}.sprite.svg`).then(r => r.text());
      const symbol = new DOMParser()
        .parseFromString(text, 'image/svg+xml')
        .querySelector(`symbol[id="${fx.name}"]`);
      expect(symbol, `no <symbol id="${fx.name}"> in the sprite`).toBeTruthy();
      const baked = [...symbol!.querySelectorAll('*')].map(shape);

      document.body.innerHTML = '';
      const host = document.createElement('div');
      document.body.appendChild(host);
      const mod = await import(
        /* @vite-ignore */ `/.output/${fx.name}.js?t=${Date.now()}`
      );
      const anim = mod.init(host, { autoplay: false, loop: false });
      try {
        const live = [...host.querySelector('svg')!.querySelectorAll('*')].map(shape);

        // The runtime addresses elements by document-order index, so this is
        // also what makes hydrating the sprite legal at all.
        expect(baked.length, `${fx.name}: element count`).toBe(live.length);

        const off = compare(baked, live);

        if (EXPRESSION_DRIVEN.has(fx.name)) {
          expect(
            off.length,
            `${fx.name} now bakes exactly — drop it from EXPRESSION_DRIVEN`,
          ).toBeGreaterThan(0);
          return;
        }
        expect(off, `${fx.name} static markup differs from frame ${0}`).toEqual([]);
      } finally {
        anim.destroy();
      }
    });
  }
});

// The other half of the SSR flow: markup arrives already painted, and the
// module adopts it instead of replacing it (`init(el, { hydrate: true })`).
//
// This is what the bake has to not break. Bindings address elements by
// document-order index, so adding attributes is safe and adding *elements*
// would not be — and nothing here would fail loudly if that changed, it would
// just start writing the right values onto the wrong nodes.
describe('a served first frame hydrates', () => {
  for (const fx of FIXTURES) {
    test(`${fx.name}`, { timeout: 30_000 }, async () => {
      const text = await fetch(`/.output/${fx.name}.sprite.svg`).then(r => r.text());
      const symbol = new DOMParser()
        .parseFromString(text, 'image/svg+xml')
        .querySelector(`symbol[id="${fx.name}"]`)!;

      document.body.innerHTML = '';
      // Stand in for the server's response: the symbol's children in a shell.
      const served = document.createElement('div');
      served.innerHTML =
        `<svg viewBox="${symbol.getAttribute('viewBox')}" width="100%" height="100%"` +
        ` preserveAspectRatio="xMidYMid meet" style="overflow:hidden">${symbol.innerHTML}</svg>`;
      document.body.appendChild(served);
      const before = served.querySelector('svg')!.querySelectorAll('*').length;

      const fresh = document.createElement('div');
      document.body.appendChild(fresh);

      const mod = await import(
        /* @vite-ignore */ `/.output/${fx.name}.js?t=${Date.now()}`
      );
      const a = mod.init(served, { hydrate: true, autoplay: false, loop: false });
      const b = mod.init(fresh, { autoplay: false, loop: false });
      try {
        // Adopted, not re-rendered: the same nodes are still there.
        expect(served.querySelector('svg')!.querySelectorAll('*').length).toBe(before);

        // Mid-animation, not frame 0: an off-by-one in element indexing would
        // still look right at the frame the markup was baked at.
        const mid = Math.floor(b.totalFrames / 2);
        a.goToAndStop(mid);
        b.goToAndStop(mid);
        const shot = (host: HTMLElement) =>
          [...host.querySelector('svg')!.querySelectorAll('*')].map(shape);
        expect(compare(shot(served), shot(fresh)), `${fx.name} hydrated`).toEqual([]);
      } finally {
        a.destroy();
        b.destroy();
      }
    });
  }
});

// `url(#…)` resolves document-wide, so two mounts of one module must not share
// a gradient or mask id. Inline mode substitutes the marker in the string;
// extracted mode has no string to substitute and rewrites the built DOM
// instead — a separate code path, and the one that would fail silently by
// painting the second mount with the first one's gradient.
describe('two mounts do not share generated ids', () => {
  // `ripple` defines 92 marked ids, `starfish` 2.
  for (const name of ['ripple', 'starfish']) {
    for (const variant of ['extern', 'extracted'] as const) {
      test(`${name} (${variant})`, { timeout: 30_000 }, async () => {
        document.body.innerHTML = '';
        if (variant === 'extracted') {
          const svg = await fetch(`/.output/${name}.sprite.svg`).then(r => r.text());
          const holder = document.createElement('div');
          holder.innerHTML = svg;
          document.body.appendChild(holder);
        }
        const suffix = variant === 'extracted' ? '.extracted.js' : '.js';
        const mod = await import(
          /* @vite-ignore */ `/.output/${name}${suffix}?t=${Date.now()}`
        );

        const ids = (host: HTMLElement) =>
          [...host.querySelectorAll('[id]')].map(e => e.id);
        const mounts = [0, 1].map(() => {
          const host = document.createElement('div');
          document.body.appendChild(host);
          return { host, player: mod.init(host) };
        });
        try {
          const [a, b] = mounts.map(m => ids(m.host));
          expect(a.length, 'the fixture should define ids').toBeGreaterThan(0);
          expect(b.length).toBe(a.length);
          const shared = a.filter(id => b.includes(id));
          expect(shared, `ids reused across mounts: ${shared.slice(0, 5)}`).toEqual([]);

          // An id is only useful if something points at it: check the
          // references moved with it, which is the failure case that renders
          // wrong instead of throwing.
          for (const m of mounts) {
            const own = new Set(ids(m.host));
            const refs = [...m.host.querySelectorAll('*')]
              .flatMap(e => [...e.attributes])
              .flatMap(at => [...at.value.matchAll(/url\(#([^)]+)\)/g)].map(x => x[1]));
            expect(refs.length).toBeGreaterThan(0);
            const dangling = refs.filter(r => !own.has(r));
            expect(dangling, `references outside this mount: ${dangling.slice(0, 5)}`).toEqual([]);
          }
        } finally {
          for (const m of mounts) m.player.destroy?.();
        }
      });
    }
  }
});

describe('visual parity vs lottie-web', () => {
  beforeAll(async () => {
    await page.viewport(VIEWPORT_W * 2 + 40, VIEWPORT_H + 40);
  });

  for (const fx of FIXTURES) {
    describe(fx.name, () => {
      let anim: Awaited<ReturnType<typeof loadFixture>> | undefined;

      afterEach(() => {
        if (anim) {
          anim.destroy();
          anim = undefined;
        }
      });

      for (const t of SAMPLES) {
        test(`frame at t=${t}`, { timeout: 30_000 }, async () => {
          const { ref, ulottie } = mountContainers();
          anim = await loadFixture(fx.name, ref, ulottie);
          const frame = Math.floor(anim.totalFrames * t);
          anim.goToFrame(frame);

          // Let the browser render the new frame before screenshotting.
          await new Promise(r => requestAnimationFrame(() => r(undefined)));
          await new Promise(r => requestAnimationFrame(() => r(undefined)));

          const refShot = await page.screenshot({ element: ref, save: true });
          const ulottieShot = await page.screenshot({ element: ulottie, save: true });
          const refPath = shotPath(refShot);
          const ulottiePath = shotPath(ulottieShot);

          const diff = await commands.odiffCompare(refPath, ulottiePath, {
            antialiasing: true,
          });
          if (!diff.match) {
            const ratio = diff.diffPercentage / 100;
            const tolerance = fx.tolerance ?? DEFAULT_TOLERANCE;
            expect.soft(ratio).toBeLessThanOrEqual(tolerance);
            // If the soft check passed (within tolerance) but odiff still
            // reported "no match", that means absolute mismatch was within
            // tolerance. Anything past tolerance is a hard fail.
            if (ratio > tolerance) {
              throw new Error(
                `Visual mismatch on ${fx.name} frame=${frame} (t=${t}): ` +
                  `${diff.diffPercentage.toFixed(3)}% > ${(tolerance * 100).toFixed(3)}%. ` +
                  `diff: ${diff.diffPath}`,
              );
            }
          }
        });
      }
    });
  }
});
