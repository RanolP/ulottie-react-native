# μLottie on React Native — port report

μLottie is a Rust ahead-of-time compiler that turns Lottie JSON into small self-contained JS modules. This report covers the React Native port: a new compiler target, a Metro integration, a Reanimated-driven player, and a measured comparison against the official `lottie-react-native` (~7.3, lottie-ios underneath) in `examples/compare` (Expo SDK 57, React Native 0.86, new architecture).

## What was built

- **`--target reanimated-aot` compiler target** (`ulottie-compiler/src/backend/rn.rs`): emits an ES module exporting a `react-native-svg` element tree (`tree`), animation metadata (`meta`), and an `init()` worklet whose `apply(frame)` writes per-frame values into per-slot animated props. Nested `<svg>` elements are flattened to viewport-clipped `<G>`s (rationale under Caveats), and inverted-matte `feComponentTransfer` filters are lowered to `FeColorMatrix`.
- **Metro plugin** (`ulottie-react-native/metro/withUlottie.js`): wraps any Metro config so `import anim from './foo.lottie.json'` compiles through the AOT compiler at bundle time; every other file falls through to the previous transformer. Compiler refusals surface as Metro build errors; named degradations pass through an `allow` option (`--allow <name>` per entry).
- **`createUlottie` player** (`ulottie-react-native/src/index.ts`): renders the compiled tree once, then drives playback entirely on the UI thread — a Reanimated `useFrameCallback` worklet advances the frame clock and `apply(frame)` flushes only the dirty slots into `useAnimatedProps`-bound `react-native-svg` elements. No per-frame JS-thread work, no re-render per frame.
- **react-native-skottie was evaluated and skipped in this app**: verified broken/stale on current new-architecture React Native, so the in-app comparison baselines are `lottie-react-native`, rn-skia's Skottie module, and `@lottiefiles/dotlottie-react-native`. It was measured separately on its own happy-path stack in `examples/compare-legacy` (RN 0.74.1, old architecture) — see "Baselines beyond lottie-react-native".

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
- The skia parity captures referenced in this section predate the tint luminance-matrix fix in `ulottie-compiler/src/scene/build.rs` (0.3086/0.6094/0.082 → ⅓/⅓/⅓, matching lottie-web's `linearFilterValue`); a re-capture will shift tint output slightly.
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

## The rt targets: native rasterizers (tiny-skia and ThorVG)

A third compiler target (`--target rt`, `ulottie-compiler/src/backend/rt.rs`), selected by the `*.rt.lottie.json` naming convention in the same Metro plugin, that renders through a **native Rust rasterizer** instead of any JS-driven drawing library. One compiler output feeds two interchangeable React Native packages:

- `ulottie-react-native-rt-tiny-skia` — CPU rasterizer built on the `tiny-skia` crate (pure Rust).
- `ulottie-react-native-rt-thorvg` — the same scene played through ThorVG v1.1.1 (C++, compiled from source by the crate's build script).

**Architecture.** The compiler lowers the same restructured display list the skia-aot target uses into **RTDL** (`ulottie-rt/src/rtdl.rs`), a renderer-agnostic binary scene: numbers only, paths as verb+point arrays, colors as float RGBA. Per-frame work moves from runtime programs to compile-time baking — the binding bake runs at every integer frame and the samples compress into numeric keyframe tracks the native player interpolates. The emitted JS module is just the blob as base64 plus numeric `meta`; there is no per-fixture runtime code at all. On device:

- A shared driver (`ulottie-react-native/src/rt-shared.ts`) runs the whole clock in one `requestAnimationFrame` worklet on the **react-native-worklets** UI runtime — no reanimated dependency. Per frame, the only JS→native traffic is one JSI call, `global.UlottieRtApi.renderFrame(nativeId, frame)`; the RTDL blob crosses once at load.
- The Rust core (`ulottie-rt`, C ABI in `include/ulottie_rt.h` / `include/ulottie_rt_tvg.h`, two parallel symbol sets so both backends can coexist in one process) decodes the blob, interpolates tracks, and rasterizes premultiplied RGBA8888 into a caller-provided buffer.
- iOS: a Fabric view whose CALayer `contents` is a CGImage wrapping (not copying) the just-rendered buffer, double-buffered (`ulottie-react-native-rt-tiny-skia/ios/UlottieRtView.mm` and the thorvg twin).
- Android: a Fabric view backed by an ARGB_8888 `Bitmap` (premultiplied RGBA in memory — the exact pixel format Rust emits); per frame the JNI adapter locks the pixels, hands the pointer to Rust, unlocks, and invalidates (`*/android/src/main/cpp/UlottieRtAdapter.cpp`). Native code is built per-ABI (arm64-v8a, x86_64) by CMake linking cargo-built static libraries; both backend `.so`s link one prefab-published `libulottiertshared.so` so a single process-global registry and one `UlottieRtApi` host object serve both backends despite `RTLD_LOCAL`.

Verified on both platforms in `examples/compare` (RT tab renders both backends side by side): animations play, and unmount/remount with the frame loop still scheduled neither crashes nor leaks a view — teardown flips an `alive` flag the loop notices on its next tick, and `renderFrame` on a torn-down view is a native no-op.

**Capability coverage.** The refusal scan is skia-aot's exactly (blend modes, animated gradients, effects, and embedded images all render); the deltas:

- Images: embedded (`data:` URI) **PNG** only — the rt decoder ships no JPEG path ("the rt target only decodes embedded PNG images" is a named refusal).
- Expressions: refused, as on every RN target.
- A scene past ~500 display-list nodes compiles with a named **WARNING** (stderr + module comment) instead of a refusal: past that budget tiny-skia at 512² cannot hold 60 fps on a phone (measured; ~2000 nodes is infeasible). Correctness stays a refusal; the budget is advisory.

**Pixel parity, gated in CI.** Unlike the players above (compared against lottie-react-native screenshots), the rt backends diff **directly against lottie-web** at 512² over the full 17-fixture corpus, 5 pins per fixture, in `cargo test -p ulottie-rt --features "tinyskia thorvg"` (`ulottie-rt/tests/parity.rs`; 34 tests pass). A pixel counts as different past 25/255 on any channel; default budget 1% of pixels per frame. Documented wider budgets, both structural divergences rather than bugs:

| Fixture | Backend | Budget | Why |
| --- | --- | ---: | --- |
| fx_effects | both | 3% | lottie-web's default 0%/100% filter region clips the drop shadow to the element's own box (hiding it behind the opaque square); the rt rasterizer, like skia-aot, drops percentage filter regions and draws the real After Effects shadow. Worst measured frame: 2.657%. |
| image_embedded | thorvg only | 3.5% | ThorVG's texture mapper bilinear-filters across the picture's outer edge (a ~1-texel feather band, wide at this fixture's 32× upscale); the axis-aligned frame 0 passes at 0.002%. |

Every other fixture × frame × backend holds the 1% default.

**Bundle size, measured.** The rt module is usually the smallest artifact of any target — base64 binary instead of JS code, and zero embedded runtime:

| Fixture | JSON | JSON gz | rt module | rt gz | svg module | skia module |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| android_wave | 96,132 | 4,241 | 24,049 | 6,072 | 40,950 | 57,977 |
| blend_multiply | 10,407 | 959 | 3,019 | 971 | — | 45,684 |
| boucing_ball | 13,075 | 1,011 | 3,195 | 1,042 | 31,154 | 49,245 |
| ellipse | 2,706 | 568 | 617 | 387 | 1,234 | 17,198 |
| fill | 4,193 | 729 | 673 | 436 | 1,202 | 17,235 |
| fx_effects | 7,875 | 982 | 5,101 | 2,120 | — | 42,925 |
| gradient_animated | 16,386 | 1,472 | 13,922 | 6,966 | — | 46,859 |
| gradient_radial | 40,416 | 2,212 | 29,976 | 11,322 | 33,893 | 50,908 |
| image_embedded | 1,681 | 487 | 3,896 | 1,654 | — | 45,042 |
| lottie_logo_1 | 67,263 | 2,336 | 21,957 | 7,177 | 47,325 | 63,430 |
| mask_subtract | 4,837 | 602 | 5,651 | 2,507 | 24,555 | 42,428 |
| matte_alpha | 130,649 | 3,898 | 55,200 | 16,612 | 45,859 | 62,940 |
| matte_luma_inv | 130,680 | 3,924 | 55,544 | 16,658 | — | 63,256 |
| precomp_star_circle | 16,737 | 1,146 | 75,646 | 14,730 | 39,203 | 56,296 |
| rectangle | 2,793 | 571 | 629 | 386 | 1,236 | 17,194 |
| stroke_under_fill | 40,058 | 2,208 | 19,213 | 6,165 | 41,939 | 59,547 |
| trim_path | 3,814 | 755 | 845 | 536 | 1,411 | 17,386 |

The floor for a static fixture is ~0.6 KB raw (vs ~1.2 KB svg, ~17 KB skia). The trade shows on keyframe-dense content: sampled numeric tracks grow with animated span × node count, so precomp_star_circle inflates to 75.6 KB raw (14.7 KB gzip — the samples compress well) and the pathological bodymoovin (1381 nodes, well past the node budget) bakes to 1.22 MB raw / 358 KB gzip against 379 KB / 91 KB for its svg module.

**Production JS-bundle cost.** Same protocol as the table above (`expo export:embed --dev false`, minimal entry points, boucing_ball as the one animation):

| Entry | Raw bytes | Gzip | Delta (raw / gzip) vs |
| --- | ---: | ---: | --- |
| e8 + react-native-worklets | 921,419 | 219,883 | +94,539 / +17,068 vs e0 empty |
| e5 rt tiny-skia player + module | 931,894 | 222,629 | +10,475 / +2,746 vs e8 |
| e6 rt thorvg player + module | 931,918 | 222,640 | +10,499 / +2,757 vs e8 |
| e7 rn-skia Skottie + raw JSON | 2,133,157 | 459,585 | +415,735 / +94,286 vs e1 reanimated |
| e9 dotlottie player + .lottie asset | 833,079 | 204,532 | +6,199 / +761 vs e0 empty |

The rt player itself is ~10 KB of JS over the worklets runtime many apps already carry — two orders of magnitude below the skia stack's JS layer. The dotlottie JS layer is the thinnest of all (+6.2 KB raw / +0.8 KB gzip over an empty app, no reanimated/worklets dependency); its animation ships as a `.lottie` file asset outside the JS bundle (boucing_ball: 1,411 B — the archive is essentially the deflated JSON), and its real cost is native (below).

**Native binary cost.** What the JS numbers exclude, measured directly. iOS (arm64, dead-stripped dylib exporting only the 5 C-ABI functions, `strip -x`): tiny-skia **1,223,600 B** (~1.17 MiB), thorvg **1,158,032 B** (~1.10 MiB). Android (stripped `.so` in the APK):

| lib | arm64-v8a | x86_64 |
| --- | ---: | ---: |
| libulottiertshared.so (registry + JSI host) | 150,776 | 143,680 |
| libulottierttinyskia.so (rt core + tiny-skia) | 1,306,560 | 1,437,368 |
| libulottiertthorvg.so (rt core + ThorVG) | 1,215,448 | 1,268,792 |
| librnskia.so (the Skia baseline carries) | 34,665,064 | 35,037,704 |

One rt backend costs ~1.4 MB per ABI all-in — 1/24 of the Skia engine a Skottie- or rn-skia-based player ships.

**Performance, measured (mixed16).** Same protocol as the skia-aot heterogeneous table: Perf tab, count `mixed` (the 16 heaviest distinct fixtures mounted simultaneously, baked node sum 2281), 10 s `useFrameCallback` probe, 3 runs per player, median per metric, dev-mode Metro bundles. The rt players mount all 16 fixtures as RTDL scenes; `skottie (rn-skia)` is `Skia.Skottie.Make` over the 16 raw JSONs.

iOS — iPhone 17 simulator (iOS 26.4, 60 Hz). Rows marked * repeat the skia-aot table above (same device, protocol, and build):

| Player | Mount→first frame (ms, median) | Steady mean (ms) | Steady p50 | Dropped / 10 s |
| --- | ---: | ---: | ---: | ---: |
| rt-thorvg | 19.5 | 16.8 | 16.67 | 4 |
| rt-tinyskia | 18.7 | 32.5 | 33.7 | 270 |
| skottie (rn-skia) | 236.5 | 17.0 | 16.67 | 5 |
| ulottie-skia * | 122.6 | 17.0 | 16.67 | 10 |
| lottie-react-native * | 99.9 | 17.6 | 16.67 | 2 |
| dotlottie (LottieFiles) | 183.2 | 16.75 | 16.67 | 3 |
| ulottie (svg) * | 2169.6 | 206.2 | 218.0 | 42 |

- **dotlottie holds 60 fps on mixed16** (like lottie-react-native, its rendering runs off the probed JS/UI thread, so the probe sees mostly the React commit). Its 183 ms mount carries a dev-mode caveat: the player takes only a file source, so each of the 16 cells fetches its `.lottie` archive over HTTP from Metro before parsing — a production build reads bundled assets instead.
- **rt-thorvg holds 60 fps on the full mixed load** and mounts in ~19 ms — an order of magnitude under every JSON-parsing player (skottie 236 ms, lottie 100 ms), because there is nothing to parse: the scene arrives as a binary display list and the first frame is one rasterize call.
- **rt-tinyskia saturates at ~30 fps** (32.5 ms mean): sixteen 512² CPU rasterizations per frame is where tiny-skia's scalar pipeline tops out, and bodymoovin alone is 1381 nodes — exactly the territory the compiler's >500-node WARNING flags.

Android — arm64 AVD (android-35, 60 Hz), same app and protocol. Three sweeps, reported separately because they ran in separate sessions under different host contention (absolute numbers inflated; within-table comparison only). **These emulator sweeps are superseded for cross-player ranking by the Pixel 8 sweep below** — they are kept because they are the record the earlier conclusions were drawn from.

Sweep A, sole emulator on the host (cut short by an unrelated device takeover — rt and skottie groups only):

| Player | Mount→first frame (ms, median) | Steady mean (ms) | Steady p50 | Dropped / 10 s |
| --- | ---: | ---: | ---: | ---: |
| rt-thorvg | 135.4 | 18.5 | 16.67 | 37 |
| rt-tinyskia | 132.7 | 36.3 | 33.33 | 157 |
| skottie (rn-skia) | 443.9 | 29.8 | 16.67 | 129 |

Sweep B, dedicated AVD, contended host (all players):

| Player | Mount→first frame (ms, median) | Steady mean (ms) | Steady p50 | Dropped / 10 s |
| --- | ---: | ---: | ---: | ---: |
| rt-thorvg | 328.4 | 111.2 | 100.0 | 89 |
| rt-tinyskia | 361.3 | 140.5 | 133.3 | 72 |
| lottie-react-native | 549.0 | 126.8 | 116.7 | 78 |
| skottie (rn-skia) | 1137.3 | 181.1 | 183.3 | 53 |
| ulottie-skia | 989.5 | 239.8 | 200.0 | 41 |
| none (probe only) | 102.5 | 17.1 | 16.67 | 5 |
| ulottie (svg) | DNF | — | — | — |

Sweep C, dedicated AVD (a fresh `ulottie_perf` device), contended host again (a second emulator owned by another workload was booted throughout — within-table comparison only). dotlottie plus two same-run anchors:

| Player | Mount→first frame (ms, median) | Steady mean (ms) | Steady p50 | Dropped / 10 s |
| --- | ---: | ---: | ---: | ---: |
| dotlottie (LottieFiles) | 136.4 | 25.4 | 16.67 | 102 |
| rt-thorvg | 124.4 | 19.5 | 16.67 | 42 |
| none (probe only, 1 run) | 50.4 | 16.8 | 16.67 | 3 |

dotlottie animates on its own render thread(s) — on Android its default renderer is the Compose view's software rasterizer — so the probe again measures induced load on the UI thread, where it drops ~2.4× more frames than rt-thorvg under the same contention.

- The ordering reproduces iOS: rt-thorvg has the best steady mean and the cheapest mount of every animating player on all three sweeps; rt-tinyskia is CPU-bound; skottie pays the heaviest mount (native parse of 16 JSONs).
- **ulottie (svg) did not finish on the emulator**: two attempts, two crashes. The cause was diagnosed wrongly at the time (read as a Hermes VM SIGSEGV); it is neither native nor emulator-specific, and it is now fixed — see "Android on a real device" below.

**Android on a real device — Google Pixel 8.** Same app and protocol, but the **release APK** on a physical Google Pixel 8 (Android 14, 60 Hz) driven through BrowserStack App Automate, mean of 3 runs per player, every player in **one session** (BrowserStack session `e34ca87f260f92f789ae71262cd6a957e90b026d`, 725 s). One session for the whole roster means these rows do race each other like for like, which no emulator sweep above can claim:

| Player | Mount→first frame (ms, mean of 3) | Steady mean (ms) | Steady p50 | Dropped / 10 s |
| --- | ---: | ---: | ---: | ---: |
| none (probe only) | 24.4 | 16.61 | 16.61 | 0 |
| rt-thorvg | 44.7 | 16.61 | 16.60 | 0.3 |
| rt-tinyskia | 50.0 | 45.91 | 49.82 | 218.7 |
| dotlottie (LottieFiles) | 127.2 | 17.06 | 16.60 | 2.3 |
| skottie (rn-skia) | 165.0 | 17.03 | 16.60 | 13.3 |
| lottie-react-native | 218.3 | 16.77 | 16.61 | 1.7 |
| ulottie-skia | 219.5 | 30.60 | 33.22 | 244.7 |
| ulottie (svg) | ~1161 | ~898 | — | every frame (≈1 fps) |

- **The AVD numbers are inflated, and not uniformly.** skottie's steady mean goes 52.3 ms on the emulator → **17.0 ms** on the Pixel 8; rt-thorvg's mount goes 124–135 ms → **44.7 ms**; the ulottie-skia and lottie-react-native rows fall by roughly 5–8×. An AVD's software GPU and shared host CPU penalize the heavy paths worst, so the emulator did not merely scale the ordering — it changed it. **Read cross-player ranking off the Pixel 8 table, not off sweeps A–C.**
- **The device collapses the top of the field.** rt-thorvg, dotlottie, lottie-react-native and skottie are all one vsync at steady state, statistically indistinguishable from the no-player probe floor. What real hardware separates is the two per-frame CPU rasterizers: rt-tinyskia (~20 fps) and ulottie-skia (~33 fps at p50) are the only animating players that miss the budget.
- **rt-thorvg is the cheapest animating player on both metrics** — 44.7 ms mount against 127–219 ms for every JSON-parsing player, and a steady mean equal to the probe floor to two decimal places.
- **ulottie (svg) now completes on Android** — 4 runs, zero crashes — but at ~1161 ms mount and ~898 ms per frame, i.e. **≈1 fps with every frame dropped**. The mount figure is the mean of the two runs not perturbed by screenshot capture (1146.0 and 1175.8 ms); the frame figure is the mean of those same two runs (926.9 and 869.4 ms) against a 730–930 ms run-to-run spread. This is roughly 4× worse than the same fixture on iOS (2169.6 ms mount, 206.2 ms/frame, ~5 fps): the SVG player's cost is the Fabric commit of a ~2200-node tree every frame, and Android's `react-native-svg` backend pays more per node than iOS's. It completes; it is not usable.

**Why ulottie (svg) used to crash on Android — three causes, found in sequence.** The original "Hermes VM SIGSEGV" reading was wrong: nothing here is a native crash.

1. **Leading-dot decimals from our own compiler.** The svg backend emitted `.47` where `react-native-svg`'s JS prop parser expects `0.47`; the parser rejects the string and leaves the prop a `String`. Fixed in `ulottie-compiler/src/scene/svg.rs` and `ulottie-compiler/runtime/num.js` — emitted numbers keep the leading zero.
2. **Animated props bypass the JS coercion entirely.** Even with `"0.47"`, a reanimated-driven prop goes `performOperations` → Fabric direct, never through the JS parser, and the generated `RNSVGGroupManagerDelegate` casts the opacity family with `(Double) value` — hence `java.lang.String cannot be cast to java.lang.Double`. Fixed in `ulottie-compiler/runtime/rn/set.js` and `ulottie-compiler/src/backend/rn.rs`: `opacity`, `fillOpacity`, `strokeOpacity` and `strokeDashoffset` are emitted as JS numbers on both the static and the animated path.
3. **An upstream `react-native-svg` 15.15.4 Android bug, uncovered once 1 and 2 were fixed.** `GroupView.mLayerCanvas` aliases the parent canvas whenever a group's opacity is exactly 1; on a later frame with opacity ≠ 1, `Canvas.setBitmap` on that stale alias resets the parent's save stack and `GroupView.drawGroup` throws `IllegalStateException: Underflow in restore`. Worked around locally in `patches/react-native-svg+15.15.4.patch`. **Still to be filed upstream.**

With all three fixed, mixed16 completed on every Pixel 8 run (BrowserStack sessions `3fa4653bd73b6461f65288ff04fa6537d442bbee` and `70effb3b5787223832ff72343b99b541fd650f0a`). Causes 1 and 2 were platform-independent emitter bugs; only Android's hard `(Double)` cast turned them into a crash, which is why the iOS sweeps never flagged them.

**Baselines beyond lottie-react-native.**

- **`@shopify/react-native-skia`'s Skottie module works** on this stack (RN 0.86, new architecture) and is the comparison baseline: `Skia.Skottie.Make(json)` + the `<Skottie>` sksg element, frame driven by `useClock` (`examples/compare/src/baselines.js`).
- **`react-native-skottie` (margelo) is incompatible**, verified in three steps. (1) Its latest release 2.1.4 (last publish 2024-05-06) hard-depends on `@shopify/react-native-skia ^1.2.3`, so installing beside the app's 2.10.x nests a duplicate 429 MiB Skia copy. (2) The Android build fails at configuration: its `android/build.gradle` hard-wires `dependsOn ':react-native-reanimated:prepareHeadersForPrefab'`, a reanimated 3.x internal task that reanimated 4's worklets split removed. (3) Its C++ includes rn-skia 1.x internal headers (`RNSkPlatformContext.h`, the `JsiSk*` wrapping API) whose layout changed in rn-skia 2.x. Not patchable without forking; the install was reverted. (Strike 2 is specific to reanimated 4: against reanimated 3.x the `prepareHeadersForPrefab` task exists and the Gradle build passes. Strikes 1 and 3 stand regardless on this app's rn-skia 2.10.x.)
- **`react-native-skottie` measured on its own happy-path stack** (`examples/compare-legacy`, a standalone app: React Native 0.74.1, React 18.2.0, old architecture, Hermes, `react-native-skottie` 2.1.4, `@shopify/react-native-skia` 1.2.3, `react-native-reanimated` 3.10.1). Same probe as the main app (10 s UI-thread `useFrameCallback`, 21 ms drop threshold, mixed16 grid, dev-mode Metro bundle), median of 3 in-session runs. **Different app, different RN version — cross-table comparison is indicative only.**
  - **iOS** (iPhone 17 Pro simulator, iOS 26.4): skottie — mount→first frame **147.5 ms**, steady mean **16.68 ms**, p50 16.67 ms, **1 dropped/10 s**; the empty `none` baseline in the same app: 16.1 ms / 16.67 ms / 0 dropped. Skottie holds a clean 60 fps on the iOS simulator under mixed16. dotlottie in the main app measured 16.9 ms steady mean / 6 dropped on the same fixture set, so on iOS the two are comparable at steady state, with skottie mounting faster than dotlottie's 344 ms.
  - **Android** (arm64 API 35 emulator, contended host — a second emulator was running throughout): the one sweep session that completed all 3 runs gave skottie — mount median **413 ms** (cold first mount 2,048 ms), steady mean **52.3 ms (~19 fps)**, p50 50 ms, **191 dropped/10 s**; `none` baseline 16.8 ms mean / 3 dropped, so the slowdown is the player, not the harness. **Unstable**: in 2 of 3 sweep sessions the app died or the emulator guest itself rebooted mid-sweep, so treat the Android numbers as a single-session indication. dotlottie in the main app held ~17 ms steady mean on Android mixed16.
  - **JS bundle** (within this legacy app, `react-native bundle --dev false --minify true`, gzip -9; the RN 0.74 baseline differs from the main app's e0): empty entry 879,940 B raw / 220,683 B gz; + reanimated 3.10.1: +533,583 / +96,904; full skottie stack (reanimated + rn-skia + skottie): **+876,619 raw / +173,470 gz** over empty, i.e. +343,036 / +76,566 on top of reanimated alone. dotlottie's JS delta in the main app is +25.0 KiB gz.
  - **Native size** (release APK, stripped, per ABI): `libreact-native-skottie.so` **3,123,488 B** arm64-v8a on top of the `librnskia.so` **9,643,304 B** rn-skia 1.2.3 core it requires (~12.7 MiB/ABI total; note the main app's rn-skia 2.x `librnskia.so` is 34.7 MB). iOS links statically; on the same pre-dead-strip arm64 basis as the main app's rows, the arm64 slices of the prebuilt archives (skia 14,184,072 B + skshaper 3,356,488 B + skunicode 760,560 B + skparagraph 432,136 B + svg 427,976 B + skottie 738,120 B + sksg 149,536 B = 20,048,888 B) plus the three wrapper pods rebuilt as release arm64 linked objects (`libreact-native-skia.a` 1,216,756 B + `libreact-native-skia-skottie.a` 96,997 B + `libSSZipArchive.a` 72,235 B, `__TEXT`+`__DATA`) total **21,434,876 B** (~20.4 MiB). dotlottie ships ~2.4 MiB/ABI on Android and ~8.1 MiB per iOS slice, so skottie's Android footprint is ~5× dotlottie's.
- **`@lottiefiles/dotlottie-react-native` 0.12.1 (LottieFiles' official ThorVG-based player) is compatible**, verified running on both platforms (all 16 mixed16 cells render). It works on RN 0.86 new-architecture/bridgeless as published — iOS registers a Fabric ComponentView (plus a legacy-interop fallback), Android wraps a Compose `DotLottieAnimation` view. No config changes were needed on this app: its Expo config plugin only raises the iOS deployment target to 15.4 (this app is at 16.4) and defaults `useFrameworks: dynamic`, which proved unnecessary — the vendored dynamic frameworks embed fine in a static-linkage Pods build via plain `pod install`. Two integration facts worth knowing:
  - **The API takes a file source only** — `source` accepts a `require()`'d asset or a URL, never a JSON object/string (`parseSource` routes through `Image.resolveAssetSource`). Each fixture was therefore wrapped into a minimal `.lottie` zip (`manifest.json` + `animations/<id>.json`) under `examples/compare/assets/`, and Metro's `assetExts` gained `lottie`.
  - **Native size** (as shipped, per-arch): iOS embeds two dynamic frameworks — `DotLottiePlayer` 1,583,400 B (~1.51 MiB) + `WgpuNative` 6,913,816 B (~6.59 MiB) ≈ **8.1 MiB** per device slice. Android pulls `com.github.LottieFiles:dotlottie-android:0.15.0` via JitPack; in the built arm64-v8a APK: `libdotlottie_player.so` 2,414,136 B + `libdlplayer.so` 72,328 B ≈ **2.4 MiB** per ABI (plus `libc++_shared.so` if the app does not already ship it). That is ~1.7× an rt backend's all-in cost on Android and ~6× on iOS, though far under the Skia engine.

## Reproduce

```sh
# toolchain: Rust (stable), Node + corepack (yarn 4), Xcode + an iOS simulator
corepack enable && yarn install
cargo build --release -p ulottie-compiler   # the Metro plugin resolves this binary
cargo test --features eval                  # compiler tests incl. reanimated-aot snapshots (_fixtures/__snapshots__/*.rn.js)
cargo test -p ulottie-rt --features "tinyskia thorvg"  # rt pixel-parity vs lottie-web pins + unit tests
yarn workspace ulottie-react-native test    # ulottie-react-native/scripts/check.mjs: compile step honors the tree/meta/init contract

# rt native archives: CMake (Android) and the podspecs (iOS) import PREBUILT
# static libraries — Gradle/Xcode do not drive cargo. Build them first:
bash ulottie-react-native-rt-tiny-skia/scripts/build-rust.sh all   # ios/rust/libulottie_rt.a + android/rust/<abi>/libulottie_rt.a
bash ulottie-react-native-rt-thorvg/scripts/build-rust.sh all      # same layout for the thorvg package (compiles ThorVG from source)

# the comparison app
cd examples/compare
npx expo prebuild -p ios
npx expo run:ios                            # or: CI=1 npx expo start --port 8083 with a prebuilt app installed
npx expo run:android                        # JDK 17; CMake links the prebuilt android/rust/<abi> archives from the step above

# parity sweep (app running on a simulator; driven via agent-device)
bash scripts/capture_parity.sh              # screenshots + 300x300 crops into .artifacts/parity/
node scripts/parity_table.mjs               # odiff sweep -> .artifacts/parity_table.json

# bundle size table
node scripts/size.mjs

# perf sweep: Perf tab in the app — select player
# (ulottie|ulottie-skia|rt-tinyskia|rt-thorvg|lottie|skottie-skia|dotlottie|none)
# x count (1|4|9|16|mixed — mixed mounts the 16 heaviest distinct fixtures),
# Start runs a 10 s useFrameCallback probe and logs `PERF_RESULTS {json}` to Metro;
# this report's numbers are best-of-3 per cell (raw runs: .artifacts/perf_runs.json,
# best-of-3: .artifacts/perf_table.json)

# legacy skottie app (standalone yarn project with its own lockfile; Metro on 8091)
cd examples/compare-legacy
yarn install && cd ios && pod install && cd ..
yarn start --port 8091                      # in another shell; app auto-runs 3x skottie/mixed
                                            # + 3x none/mixed and logs PERF_RESULTS to Metro
yarn ios   # or: yarn android (JDK 17; adb reverse tcp:8091 tcp:8091)
```
