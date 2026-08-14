# Where the fixtures come from

Every file in `animations/` has to be redistributable and has to *render
correctly*, in that order. The second is not a formality: a fixture that passes
while the compiler drops a feature does not prove a capability, it enshrines a
gap. See `worm`, below.

## Sources

**rlottie** — <https://github.com/Samsung/rlottie>, MIT. `example/resource/`.
The repo's `COPYING` enumerates licences for vendored dependencies under
`src/` — freetype, pixman, stb, rapidjson, Skia — and the example resources are
not among them, so they fall under the project's MIT licence.

| fixture | rlottie name | what it is here for |
|---|---|---|
| `gradient_radial` | `waves_.json` | a radial gradient, and a gradient *fill* — `ripple`'s is a linear gradient stroke, which was the whole of the gradient coverage |
| `gradient_animated` | `gradient_animated_background.json` | an animated colour ramp (`gradient:animated-ramp`) — one binding per `<stop>`, and the file that found the ramp reader's legacy-`e` bug: the ramp read raw JSON, so the parse-boundary normalization did not reach it |
| `image_layer` | `image_test.json` | an image layer and an externally-referenced image asset |
| `mask_subtract` | `mask.json` | a subtractive layer mask — `emit_masks`'s `has_subtract` branch, which had never run |
| `matte_alpha` | `emoji_shock.json` | a plain alpha track matte; `lottie_logo_1` is mode 2, so the *uninverted* path had no fixture |
| `stroke_under_fill` | `triib_manage.json` | a stroke Lottie paints *under* its fill, which SVG will not do without `paint-order` |

**Derived** — `matte_luma` and `matte_luma_inv` are `matte_alpha` with the two
`tt` layers set to 3 and 4. No file in any of the four local corpora carries a
luma matte (the flutter corpus's one `tt:3` file also has an inverted mask, a
refused feature), so derived-from-a-real-file beat waiting for a real one.
`matte_luma` pixel-matches lottie-web at 0.000%; `matte_luma_inv` cannot —
lottie-web's `getMatte` builds masks for matte types 1–3 only, so its `tt:4`
output references a mask that does not exist, and Chrome's error recovery for
an unresolvable mask reference draws the element *unmasked*. Like
`tp`-without-`td`, this compiler renders what After Effects means instead;
the structural assertions in `ulottie-compiler/tests/track_matte.rs` and the
Rust reference render in `tests/frame_snapshot.rs` are its gate.

**lottie-flutter** — <https://github.com/xvrh/lottie-flutter> (the sample
gallery at xvrh.github.io/lottie-flutter-web serves this corpus), MIT. The
gallery files carry no per-file licence of their own; they are Lottie's own
demo assets and community samples, redistributed with the corpus.

| fixture | corpus name | what it is here for |
|---|---|---|
| `lottie_logo_2` | `LottieLogo1.json` | the classic Lottie logo — trim-heavy animated strokes. Pixel-exact. (`lottiefiles_lottie_logo_1.json` in the same corpus is byte-identical to this file.) |
| `lottie_logo_3` | `LottieLogo2.json` | the second logo variant. Its nine `o: 0` white solids are what made statically-transparent layers elide (they rendered as `opacity="0"` rects that matched pixels and carried geometry no gate could see). Pixel-exact. |
| `android_wave` | `AndroidWave.json` | the Android wave, `keyframe:hold` and a precomp. `merge-paths` allowed: both renderers drop the modifier and this file's merged shapes are static, so the allowance is invisible — 0.000%. |
| `text_baseline` | `Tests_TextBaseline.json` | **text from embedded glyphs** (`layer:text`, `text:layer`, `text:fonts`, `text:glyphs`): a static text document lowered to glyph shapes at compile time — 0.000%, strict. (Its predecessor, the file that first exercised this path, was retired from the suite as not suitable for the repo.) |
| `fireworks` | `17297-fireworks.json` | **the repeater** (`shape:repeater`): four static repeaters (20 copies, 48°) expand at lowering. NOT a parity fixture: lottie-web's repeater clones the trim into every copy *and* keeps the layer-level trim, so each repeated stroke is trimmed twice — measured, its arc equals e² of the property value (0.328² = 0.108 at frame 20, and the s/e composition matches at every sampled frame). AE trims once and repeats; so does this compiler. The Rust reference render is the gate, like `matte_luma_inv`. |
| `blend_multiply` | `Animation-1700642783167.json` | a layer blend mode (`bm: 1` multiply, emitted as CSS `mix-blend-mode`) — 0.000%, strict. |
| `bodymoovin` | `Tests/bm.json` (≈2 MB; `Tests_bm.json` is byte-identical) | the Bodymovin logo: a `v: 3.1.6` document with legacy **0–255 shape colours** (`checkColors` territory — rescaled at the parse boundary with alpha pinned, see HANDOFF), old property spellings, and a lettermark built from staggered precomps. `merge-paths` allowed and invisible; 0.05% pixel residual. What found the colour-scale bug: the background painted white instead of green, 14.5% wrong. |

**Original** — `expression_layer_ref` and `image_embedded` are hand-made.

| fixture | what it is here for |
|---|---|
| `expression_layer_ref` | an expression that reads another layer and touches nothing else — `thisComp.layer('Fader').transform.opacity` on a static property |
| `image_embedded` | an embedded image asset (`asset:image-embedded`): a 16×16 two-colour PNG as a data URI, per the rule that a hand-made file with a 100-byte image exercises the same code as a 139 KB one for a thousandth of the repo |

`ripple`, `starfish` and `lights` all have expressions, and all three name
`thisProperty`, so all three carried the `thisProperty` runtime. Nothing
exercised the *other* shake: expressions on, that surface off. A production
animation hit it and every expression in it silently returned its authored
constant — see the `thisPropertyFor` note in `backend/shake.rs`. The Follower's
opacity here is the Fader's, so a body that stops evaluating leaves a blue disc
on screen through the whole fade.

## How the first four were chosen

All 93 rlottie examples were compiled and diffed against lottie-web
(`tools/compare.mjs`). 42 render at **exactly 0.000%** with no `--allow`. Those
42 were then set-covered against the gaps in `coverage.json`, smallest file
winning ties, and each survivor re-checked at fifteen frames *and* inspected to
confirm the feature is present in **both** DOMs.

That last step is not paranoia. `worm.json` renders at 0.000% across twelve
frames and covers `stroke:dash` — and lottie-web writes `stroke-dasharray=" 10"`
where this compiler writes nothing at all. The dash is dropped, the picture
scores perfect, and adopting it would have recorded a missing feature as a
passing test. It is left out, and `stroke:dash` stays in `coverage.json`.

Two gaps were reachable only at a price not worth paying: `asset:image-embedded`
and `mask:a-inv` exist in clean rlottie files, but the smallest is 139 KB —
an embedded asset *is* a base64 PNG, so the fixture, the module and the snapshot
are all that size. A hand-made file with a 100-byte image would exercise the
same code for a thousandth of the repo.

## What they found immediately

`gradient_radial` failed `output_hygiene` on its first run: a gradient whose
handles never move bakes into the markup completely, but `emit_gradient` set
`Caps::GRADIENT` before knowing that, so every animation with any gradient
shipped `bGradient` and `oGradient` whether or not it could reach them. The
capability is set by the binding now.

`stroke_under_fill` came the other way round — from a bug rather than to catch
one. A production animation rendered its avatars with the white ring eating
fifteen units into the disc it was meant to surround, because `it` order
`el fl st tr` puts the stroke *below* the fill and SVG paints fill-then-stroke
whatever the source said. `paint-order="stroke"` is that ordering in one
attribute. Nothing in twelve fixtures had the construct; `triib_manage` is the
smallest of three files in ninety-three that does, and it renders at 0.000%.
