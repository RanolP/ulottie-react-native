# μLottie on React Native — port report

μLottie is a Rust ahead-of-time compiler that turns Lottie JSON into small self-contained JS modules. This report covers the React Native port: a new compiler target, a Metro integration, a Reanimated-driven player, and a measured comparison against the official `lottie-react-native` (~7.3, lottie-ios underneath) in `examples/compare` (Expo SDK 57, React Native 0.86, new architecture).

## What was built

- **`--target reanimated-aot` compiler target** (`ulottie-compiler/src/backend/rn.rs`): emits an ES module exporting a `react-native-svg` element tree (`tree`), animation metadata (`meta`), and an `init()` worklet whose `apply(frame)` writes per-frame values into per-slot animated props. Nested `<svg>` elements are flattened to viewport-clipped `<G>`s (rationale under Caveats), and inverted-matte `feComponentTransfer` filters are lowered to `FeColorMatrix`.
- **Metro plugin** (`ulottie-react-native/metro/withUlottie.js`): wraps any Metro config so `import anim from './foo.lottie.json'` compiles through the AOT compiler at bundle time; every other file falls through to the previous transformer. Compiler refusals surface as Metro build errors; named degradations pass through an `allow` option (`--allow <name>` per entry).
- **`createUlottie` player** (`ulottie-react-native/src/index.ts`): renders the compiled tree once, then drives playback entirely on the UI thread — a Reanimated `useFrameCallback` worklet advances the frame clock and `apply(frame)` flushes only the dirty slots into `useAnimatedProps`-bound `react-native-svg` elements. No per-frame JS-thread work, no re-render per frame.
- **react-native-skottie was evaluated and skipped**: verified broken/stale on current new-architecture React Native, so the comparison baseline is `lottie-react-native` only.

## Rendering parity

Method: the compare app pins each fixture at 0/25/50/75/100% of its frame range for both players, a screenshot sweep (`examples/compare/scripts/capture_parity.sh`) crops both 300×300dp player regions (@3x → 900px), and `examples/compare/scripts/parity_table.mjs` diffs each ulottie crop against the lottie-react-native crop with odiff (antialiasing tolerance on). Gate: ≤1% differing pixels. Full results: `examples/compare/.artifacts/parity_table.json`; per-cell crops and diff images: `examples/compare/.artifacts/parity/` (e.g. `stroke_under_fill_50_diff.png`).

**Result: 59/60 cells pass the ≤1% gate; 1 cell is a documented divergence.**

% pixels differing vs lottie-react-native, per fixture × frame position:

| Fixture | 0% | 25% | 50% | 75% | 100% |
| --- | ---: | ---: | ---: | ---: | ---: |
| boucing_ball | 0 | 0 | 0 | 0 | 0 |
| rectangle | 0 | 0 | 0 | 0 | 0 |
| ellipse | 0 | 0 | 0 | 0 | 0 |
| fill | 0 | 0 | 0 | 0 | 0 |
| trim_path | 0 | 0 | 0 | 0 | 0 |
| android_wave | 0 | 0 | 0 | 0 | 0 |
| precomp_star_circle | 0 | 0.02 | 0.04 | 0 | 0.01 |
| gradient_radial | 0.23 | 0.22 | 0.22 | 0.22 | 0.23 |
| lottie_logo_1 | 0.11 | 0.11 | 0.11 | 0.11 | 0.18 |
| mask_subtract | 0.11 | 0.18 | 0.22 | 0.18 | 0.11 |
| matte_alpha | 0.11 | 0.11 | 0.11 | 0.32 | 0.11 |
| stroke_under_fill | 0.11 | 0.11 | **1.14** | 0.11 | 0.11 |

- **stroke_under_fill @50% (1.14%)** — the one divergence, and it is a playback-semantics difference, not a rendering bug: 50% of `op=177` pins the fractional frame 88.5. lottie-ios quantizes a paused fractional frame to a whole frame; ulottie renders the exact fractional frame, matching lottie-web (the web pixel oracle diffs 0.000% here). On this fast-moving segment the half frame of motion shows as a uniform offset of the whole artwork. Every integer-frame pin of the same fixture sits at the 0.11% baseline.
- **Capture baseline**: the recurring ~0.11% cells trace to capture and antialiasing noise on complex edges, not to systematic renderer differences — the same fixtures diff at 0 at other frames or hold a constant floor independent of motion.

## Performance

Measured in `examples/compare` on the iPhone 17 simulator (iOS 26.4, locked 60 Hz vsync), boucing_ball fixture, 10 s `useFrameCallback` UI-thread probe, best of 3 runs per cell. "baseline" mounts no player and measures the probe itself. All perf numbers here and in the skia-aot section are dev-mode Metro bundles (Hermes, `dev=true`) on a simulator — treat them as relative comparisons, not production absolutes.

| Player | Instances | Mount→first frame (ms) | Mean | p50 | p95 | p99 | Max | Dropped |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| baseline | 1 | 16.4 | 16.67 | 16.67 | 16.67 | 16.67 | 16.67 | 0 |
| baseline | 16 | 16.1 | 16.67 | 16.67 | 16.67 | 16.67 | 16.67 | 0 |
| ulottie | 1 | 16.1 | 16.67 | 16.67 | 16.67 | 16.67 | 16.67 | 0 |
| ulottie | 4 | 32.9 | 16.67 | 16.67 | 16.67 | 16.67 | 16.67 | 0 |
| ulottie | 9 | 50.0 | 16.69 | 16.67 | 16.67 | 16.67 | 36.67 | 1 |
| ulottie | 16 | 66.8 | 16.67 | 16.67 | 16.67 | 16.67 | 16.67 | 0 |
| lottie-react-native | 1 | 16.7 | 16.67 | 16.67 | 16.67 | 16.67 | 16.67 | 0 |
| lottie-react-native | 4 | 16.7 | 16.67 | 16.67 | 16.67 | 16.67 | 16.67 | 0 |
| lottie-react-native | 9 | 16.7 | 16.67 | 16.67 | 16.67 | 16.67 | 16.67 | 0 |
| lottie-react-native | 16 | 16.8 | 16.67 | 16.67 | 16.67 | 16.67 | 16.67 | 0 |
| ulottie-skia | 1 | 16.7 | 16.67 | 16.67 | 16.67 | 16.67 | 16.67 | 0 |
| ulottie-skia | 4 | 32.3 | 16.67 | 16.67 | 16.67 | 16.67 | 16.67 | 0 |
| ulottie-skia | 9 | 51.3 | 16.68 | 16.67 | 16.67 | 16.67 | 27.18 | 1 |
| ulottie-skia | 16 | 71.7 | 16.76 | 16.67 | 16.67 | 16.67 | 44.23 | 4 |

- **Steady state is a tie — on this small fixture.** Every player at every instance count holds 60 fps with at most a few dropped frames in ~600, including 16 concurrent looping instances. The heterogeneous 16-fixture grid in the skia-aot section breaks the tie: the svg player collapses to ~5 fps there while skia and lottie hold 60. On a vsync-locked simulator this means "both under 16.67 ms per frame", not "equal cost".
- **Mount latency is the one difference.** ulottie's mount→first-frame grows roughly one vsync per ~5 instances (16 → 67 ms best-of-3 from 1 to 16), because each instance runs its `init()` worklet on the UI thread at mount. lottie-react-native's ~17 ms is partly a blind spot: its lottie-ios view initializes off the measured thread (its worst single run was 100 ms at 1 instance, vs ulottie's 33 ms).
- **Mount optimization, measured.** Two changes landed after the first sweep: an eager `runOnUI` init warm-up at mount (`ulottie-react-native/src/index.ts`) and a once-per-runtime shared cache for the payload decode, easing tables, and arc-length tables, keyed by payload string on the UI runtime's `globalThis` (`ulottie-compiler/runtime/rn/core.js`). Result of the re-sweep (ulottie × {1,4,9,16} × 3): best-of-3 unchanged within noise (16.7 / 33.1 / 49.5 / 66.7 ms), worst-of-3 at ×16 improved 123 → 69 ms. Conclusion: shared-payload decode is not the dominant mount cost — the remaining latency sits in per-instance program bind state plus the Fabric commit of the SVG node tree. Parity spot-check after the change: new crops pixel-identical to the pre-optimization crops. The cache trades memory for the tail win: one decoded payload per animation persists for the runtime's lifetime.

## Bundle size

`examples/compare/scripts/size.mjs` compiles each fixture through the Metro plugin's compile step and compares module bytes against the raw Lottie JSON bytes:

| Fixture | JSON bytes | Compiled module bytes | Module / JSON |
| --- | ---: | ---: | ---: |
| android_wave | 96,132 | 39,847 | 0.41 |
| boucing_ball | 13,075 | 30,051 | 2.30 |
| ellipse | 2,706 | 1,234 | 0.46 |
| fill | 4,193 | 1,202 | 0.29 |
| gradient_radial | 40,416 | 32,790 | 0.81 |
| lottie_logo_1 | 67,263 | 46,222 | 0.69 |
| mask_subtract | 4,837 | 23,452 | 4.85 |
| matte_alpha | 130,649 | 44,756 | 0.34 |
| precomp_star_circle | 16,737 | 38,100 | 2.28 |
| rectangle | 2,793 | 1,236 | 0.44 |
| stroke_under_fill | 40,058 | 40,836 | 1.02 |
| trim_path | 3,814 | 1,411 | 0.37 |

Honest framing of that comparison:

- `lottie-react-native` ships the raw JSON **plus** the lottie-ios native library in the app binary; the ulottie module is the whole payload — self-contained JS with no native rendering dependency beyond `react-native-svg`/`react-native-reanimated`, which many apps already carry.
- Large or keyframe-dense fixtures compress well (matte_alpha 0.34×, android_wave 0.41×) because the compiler precomputes and dedupes what the JSON spells out per keyframe.
- Small fixtures with animation-heavy content inflate (mask_subtract 4.85×, boucing_ball 2.30×) because the fixed runtime worklet code (frame clock, slot flush, interpolators) dominates a tiny document. That runtime is emitted per module today; sharing it across modules would amortize it.

## Caveats & known limitations

- **v1 capability subset.** The reanimated-aot target refuses, at compile time with named findings: layer-effect filters (tint/fill/drop-shadow/blur — the web target lowers these to SVG filters, `react-native-svg` has no dependable counterpart), animated gradients (per-stop rebinding), image assets (no `<image>` element in the RN tree), blend modes (no `mix-blend-mode`), and expressions. Inverted track mattes (`tt: 2`/`tt: 4`) compile behind `--allow track-matte-inverted` — the inverting `feComponentTransfer` is lowered to `FeColorMatrix` (the stubbed `FeComponentTransfer` would render the matte blank), but SVG filters inside masks are the least-trodden `react-native-svg` path, so the finding stays allow-gated rather than silently waved through.
- **`paint-order` is not supported by `react-native-svg`**, so a stroke-below-fill style pair degrades to default paint order (stroke over fill); `stroke_under_fill` exercises this.
- **The pixel reference differs from the web target's.** ulottie's web output is gated against lottie-web; `lottie-react-native` renders through lottie-ios, a different renderer with its own semantics (e.g. the fractional-frame quantization above). Divergences between the two references are documented, not chased.
- **Nested `<Svg>` flattening is deliberate.** `react-native-svg` mounts a native `RNSVGSvgView` per `<Svg>`, and that breaks the emitted tree twice: each view keeps its own brush/mask/filter registry, so `url(#id)` inside a nested `<Svg>` never resolves to the root `<Defs>`; and the iOS blend path (`RNSVGRenderable renderTo`, root-caused in `RNSVGRenderable.mm`) sizes its offscreen mask buffers from the nested view's rect in parent user units while rendering with the full device CTM, so an up-scaling outer viewBox paints every masked subtree empty. Flattening to one root `<Svg>` with viewport-clipped `<G>`s leaves one native view with correctly sized buffers and one document-global registry.

## The skia-aot target

A second RN target (`--target skia-aot`, `ulottie-compiler/src/backend/skia.rs`), selected by the `*.skia.lottie.json` naming convention in the same Metro plugin, that renders through `@shopify/react-native-skia` instead of `react-native-svg`.

**Architecture.** The wire stays the same — payload, program pair, math modules, and the rn target's element-handle records are reused byte for byte — but the output side replaces the SVG component tree with a **display-list descriptor** (`dl`) baked at compile time. Every `url(#id)` reference (clip paths, masks, gradients, the matte-inversion color filter) resolves inline; no id registry exists at runtime. At mount, `runtime/skia/draw.js` turns the descriptor into live records (SkPath, SkPaint pairs, gradient shaders, layer paints, decoded images), and each frame `apply(frame)` writes dirty values into those records before a recursive walk re-records ONE native `<Canvas>` — one native view per player regardless of animation size, versus one native node per SVG element.

**Capability coverage.** Everything reanimated-aot renders, plus the constructs `react-native-svg` refuses or degrades:

| Capability | reanimated-aot (svg) | skia-aot | Fixture |
| --- | --- | --- | --- |
| `paint-order: stroke` | degrades (stroke over fill) | exact (stroke draw issues first) | stroke_under_fill |
| Blend modes | refused | `paint.setBlendMode` on a layer restore | blend_multiply |
| Animated gradients / color ramps | refused | shader rebuilt on GRADIENT/RAMP writes | gradient_animated |
| Inverted track mattes | allow-gated degradation | exact — inverting color-matrix layer paint, no gate | matte_luma_inv, lottie_logo_1 |
| Layer effects (tint/fill/drop-shadow/blur) | refused | `ColorFilter.MakeMatrix`/`MakeCompose` chains, `ImageFilter.MakeBlur`/`MakeDropShadow` | fx_effects |
| Embedded (`data:` URI) images | refused | decoded once at mount, `drawImageRect` with lottie-web's center-crop fit | image_embedded |

Still refused, with named findings: external image sources (no loader inside a self-contained worklet module) and expressions (no engine on either RN target).

**Documented deviations from the web target.**

- The tint effect's luminance matrix is specified in linearRGB (`color-interpolation-filters`); Skia has no per-filter colorspace, so it runs in sRGB. Visually close, not bit-equal.
- SVG filter regions on effects are ignored: the drop shadow's `0%/100%` region (a lottie-web self-box clip quirk) and the blur's widened region have no Skia counterpart on a layer filter, and neither is needed for correct output.
- An effect stage re-draws its input once per pass (SVG's primitive chain as nested `saveLayer`s), so tint costs three content redraws. Correctness first; a flattened single-pass form is an optimization left open.

**Performance, measured.** The `ulottie-skia` rows in the Performance table above come from the same protocol (boucing_ball, 10 s probe, best of 3). Steady state ties the other players at 60 fps through ×16. Mount→first-frame ramps like the svg player's (16.7 / 32.3 / 51.3 / 71.7 ms best-of-3 at 1/4/9/16) — the one-native-view-per-player design did **not** flatten the homogeneous ramp. A timing probe inside `ulottie-react-native/src/skia.ts` attributes it: at ×16, all 16 `init()` calls together cost ~11 ms on the UI thread (10.6 ms for the first instance's payload decode, ~0.03 ms each for the 15 cache hits) and all 16 first picture records together ~2.7 ms — so the ramp is not worklet work but per-instance `Canvas` native-view creation, its Fabric commit, and the per-mount `runOnUI` scheduling round-trips.

**Heterogeneous mount, measured.** The homogeneous grid repeats one tiny fixture; this cell mounts the 16 heaviest *distinct* fixtures simultaneously (Perf tab, count `mixed`), ranked by baked display-tree node count: bodymoovin (1381), lottie_logo_3 (207), fireworks (157), lottie_logo_2 (121), matte_luma_inv (45), android_wave (41), lottie_logo_1 (36), fx_effects (33), matte_alpha (33), matte_luma (33), precomp_star_circle (27), gradient_radial (26), stroke_under_fill (15), gradient_animated (10), blend_multiply (8), mask_subtract (8) — node sum **2281**. The svg player mounts the 12 of these it can compile (fx_effects, gradient_animated, blend_multiply, matte_luma_inv are skia-only in the app registry; their cells stay empty), node sum **2185**. lottie-react-native mounts the 16 raw JSONs. 3 runs per player, median (dev-mode Metro bundle):

| Player | Distinct fixtures | Node sum | Mount→first frame (ms, median) | Steady mean (ms) | Steady p50 | Dropped / 10 s |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| ulottie (svg) | 12 | 2185 | 2169.6 | 206.2 | 218.0 | 42 |
| ulottie-skia | 16 | 2281 | 122.6 | 17.0 | 16.67 | 10 |
| lottie-react-native | 16 | 2281 | 99.9 | 17.6 | 16.67 | 2 |

- **The svg player is the outlier, and not only at mount.** With ~2200 nodes on screen it takes ~2.2 s to first frame and then *stays* at ~5 fps (mean 206 ms/frame): per-frame shared-value writes fan out across thousands of `react-native-svg` props and every frame re-commits the huge native tree. The homogeneous ×16 table hid this because 16 × boucing_ball is only ~100 nodes.
- **skia holds 60 fps on the same content** (17.0 ms mean, p50 at vsync) with a ~123 ms mount — the display-list design pays off exactly where the SVG tree collapses. Its worst run (first after load: 147 ms mount, 87 dropped) reflects cold module require of 16 fixtures.
- **lottie-react-native's 99.9 ms median hides a 1316.7 ms cold first run** (native JSON parse of all 16 documents); steady state is clean, but as before part of its init runs off the measured thread.

**Bundle size, measured.** Same fixtures as the table above, module bytes (and gzip) per target:

| Fixture | JSON | JSON gz | svg module | svg gz | skia module | skia gz |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| android_wave | 96,132 | 4,241 | 40,950 | 14,533 | 57,648 | 20,112 |
| blend_multiply | 10,407 | 959 | — | — | 45,355 | 16,630 |
| boucing_ball | 13,075 | 1,011 | 31,154 | 12,087 | 48,916 | 17,680 |
| ellipse | 2,706 | 568 | 1,234 | 629 | 16,869 | 6,152 |
| fill | 4,193 | 729 | 1,202 | 648 | 16,906 | 6,201 |
| fx_effects | 7,875 | 982 | — | — | 42,257 | 15,135 |
| gradient_animated | 16,386 | 1,472 | — | — | 46,530 | 17,060 |
| gradient_radial | 40,416 | 2,212 | 33,893 | 12,471 | 50,579 | 18,029 |
| image_embedded | 1,681 | 487 | — | — | 44,713 | 16,211 |
| lottie_logo_1 | 67,263 | 2,336 | 47,325 | 16,574 | 63,101 | 22,062 |
| mask_subtract | 4,837 | 602 | 24,555 | 9,922 | 42,099 | 15,492 |
| matte_alpha | 130,649 | 3,898 | 45,859 | 16,225 | 62,611 | 21,696 |
| matte_luma_inv | 130,680 | 3,924 | — | — | 62,927 | 21,732 |
| precomp_star_circle | 16,737 | 1,146 | 39,203 | 13,262 | 55,967 | 18,759 |
| rectangle | 2,793 | 571 | 1,236 | 624 | 16,865 | 6,150 |
| stroke_under_fill | 40,058 | 2,208 | 41,939 | 15,335 | 59,218 | 20,864 |
| trim_path | 3,814 | 755 | 1,411 | 752 | 17,057 | 6,287 |

A skia module costs a near-constant ~15.5–17.8 KB raw (~5.5 KB gzip) over its svg twin — the skia runtime (draw-record construction, effects, blend, image handling) is embedded per module today, on top of the shared-runtime inflation already noted for the svg target. The static-fixture floor is ~16.9 KB raw / 6.2 KB gzip (skia) vs ~1.2 KB (svg). Sharing the runtime across modules would collapse both floors.

**Production app-bundle cost per player stack.** `npx expo export:embed --dev false` over five minimal entry points (`examples/compare/bench/e0..e4`), Hermes-targeted minified JS, boucing_ball as the one animation. Deltas attribute shared dependencies: reanimated is measured as its own baseline because both ulottie targets require it while many apps already ship it.

| Entry | Raw bytes | Gzip | Delta (raw / gzip) vs |
| --- | ---: | ---: | --- |
| e0 empty (View only) | 826,880 | 203,771 | — |
| e1 + reanimated | 1,717,422 | 367,280 | +890,542 / +163,509 vs e0 |
| e2 lottie-react-native + JSON | 833,968 | 205,817 | +7,088 / +2,046 vs e0 |
| e3 ulottie svg + module | 1,865,666 | 401,396 | +148,244 / +34,116 vs e1 |
| e4 ulottie skia + module | 2,177,617 | 476,668 | +460,195 / +109,388 vs e1 |

JS-bundle numbers only: lottie-react-native's +7 KB excludes the lottie-ios native library, and the skia stack's +460 KB (mostly `@shopify/react-native-skia`'s JS layer) excludes the multi-megabyte Skia native binary — native-binary deltas are an app-size question `export:embed` cannot answer.

**Parity sweep: pending.** The skia rows are not yet in the parity table; the capture scripts (`bash scripts/capture_parity.sh`, `node scripts/parity_table.mjs`) predate the skia player and need a `_s` crop pass before the 17-fixture × 5-pin sweep can run.

## Reproduce

```sh
# toolchain: Rust (stable), Node + corepack (yarn 4), Xcode + an iOS simulator
corepack enable && yarn install
cargo build --release -p ulottie-compiler   # the Metro plugin resolves this binary
cargo test --features eval                  # compiler tests incl. reanimated-aot snapshots (_fixtures/__snapshots__/*.rn.js)
yarn workspace ulottie-react-native test    # ulottie-react-native/scripts/check.mjs: compile step honors the tree/meta/init contract

# the comparison app
cd examples/compare
npx expo prebuild -p ios
npx expo run:ios                            # or: CI=1 npx expo start --port 8083 with a prebuilt app installed

# parity sweep (app running on a simulator; driven via agent-device)
bash scripts/capture_parity.sh              # screenshots + 300x300 crops into .artifacts/parity/
node scripts/parity_table.mjs               # odiff sweep -> .artifacts/parity_table.json

# bundle size table
node scripts/size.mjs

# perf sweep: Perf tab in the app — select player (ulottie|ulottie-skia|lottie|none)
# x count (1|4|9|16|mixed — mixed mounts the 16 heaviest distinct fixtures),
# Start runs a 10 s useFrameCallback probe and logs `PERF_RESULTS {json}` to Metro;
# this report's numbers are best-of-3 per cell (raw runs: .artifacts/perf_runs.json,
# best-of-3: .artifacts/perf_table.json)
```
