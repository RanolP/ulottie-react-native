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
| `image_layer` | `image_test.json` | an image layer and an externally-referenced image asset |
| `mask_subtract` | `mask.json` | a subtractive layer mask — `emit_masks`'s `has_subtract` branch, which had never run |
| `matte_alpha` | `emoji_shock.json` | a plain alpha track matte; `lottie-logo` is mode 2, so the *uninverted* path had no fixture |

**Original** — the other eleven predate this file.

## How these four were chosen

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

## What the four found immediately

`gradient_radial` failed `output_hygiene` on its first run: a gradient whose
handles never move bakes into the markup completely, but `emit_gradient` set
`Caps::GRADIENT` before knowing that, so every animation with any gradient
shipped `bGradient` and `oGradient` whether or not it could reach them. The
capability is set by the binding now.
