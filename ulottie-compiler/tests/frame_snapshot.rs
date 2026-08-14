//! Frame-snapshot regression tests for the eval module.
//!
//! Each fixture under `_fixtures/animations/*.json` is compiled to a
//! `data::Payload`, evaluated at five sample frames `[0%, 25%, 50%, 75%, 99%]`
//! of `[ip, op)`, and the concatenated Frame trees are snapshotted via insta.
//!
//! Step 1 of H2 only covers static fixtures. As eval grows (keyframes,
//! transforms, gradients, masks, precomps, expressions) more fixtures will
//! land here. The macro emits one test per fixture so the test grid is
//! granular and a single regression doesn't gate the others.
//!
//! Run `cargo nextest run --test frame_snapshot`. Accept changes with
//! `cargo insta review`.

use std::fmt::Write;
use std::fs;

use ulottie_compiler::{compile_to_payload, eval};

fn fixture_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("_fixtures")
        .join("animations")
        .join(format!("{name}.json"))
}

fn snapshot_frames(name: &str) -> String {
    let json = fs::read_to_string(fixture_path(name))
        .unwrap_or_else(|_| panic!("missing fixture: {name}"));
    let payload = compile_to_payload(&json).expect("compile_to_payload");
    let ip = payload.c.ip;
    let op = payload.c.op;
    let span = (op - ip).max(1.0);
    let samples = [0.0f64, 0.25, 0.50, 0.75, 0.99];
    let mut out = String::new();
    for &t in &samples {
        let frame = ip + (span * t).floor();
        match eval::render(&payload, frame) {
            Ok(f) => {
                writeln!(out, "=== sample t={t} frame={frame} ===").unwrap();
                write!(out, "{f}").unwrap();
            }
            Err(e) => {
                writeln!(out, "=== sample t={t} frame={frame} ===").unwrap();
                writeln!(out, "ERROR: {e}").unwrap();
            }
        }
        out.push('\n');
    }
    out
}

macro_rules! frame_snapshot {
    ($name:ident, $fixture:literal) => {
        #[test]
        fn $name() {
            let snap = snapshot_frames($fixture);
            insta::assert_snapshot!($fixture, snap);
        }
    };
}

// Step 1: zero-expression / static fixtures.
frame_snapshot!(frame_rectangle, "rectangle");
frame_snapshot!(frame_ellipse, "ellipse");
frame_snapshot!(frame_fill, "fill");
frame_snapshot!(frame_trim_path, "trim_path");

// Step 2: keyframes + easing + spatial bezier + group/layer transforms.
frame_snapshot!(frame_boucing_ball, "boucing_ball");

// Step 3: opacity propagation + parent-chain transforms.
frame_snapshot!(frame_lottie_logo_1, "lottie_logo_1");

// Step 4: gradients (linear + radial; color+alpha stop merging).
frame_snapshot!(frame_ripple, "ripple");

// Step 6: precomp instances with inner clock offset.
frame_snapshot!(frame_precomp_star_circle, "precomp_star_circle");

// Step 5: masks (add/subtract) with animated path keyframes.
frame_snapshot!(frame_starfish, "starfish");

// Step 7+: expressions via rquickjs ($bm_rt context + callable proxies).
frame_snapshot!(frame_lights, "lights");

// The lottie-flutter logo variants, and AndroidWave (merge-paths allowed —
// both renderers drop the modifier, so parity holds). `lottie_logo_1` is
// the original wordmark (Step 3 above); `_2`/`_3` are the lottie-flutter
// variants.
frame_snapshot!(frame_lottie_logo_2, "lottie_logo_2");
frame_snapshot!(frame_lottie_logo_3, "lottie_logo_3");
frame_snapshot!(frame_android_wave, "android_wave");

// Text lowered from embedded glyphs.
frame_snapshot!(frame_text_baseline, "text_baseline");

// A static repeater, expanded at lowering. NOT a pixel-parity fixture:
// lottie-web clones the trim into every copy *and* keeps the layer-level
// trim, so each repeated stroke is trimmed twice (measured: its arc equals
// e² of the property value) — an artifact, not AE semantics. This compiler
// trims once and repeats, which is what After Effects means; the reference
// render is the gate.
frame_snapshot!(frame_fireworks, "fireworks");

// Layer blend mode (`bm: 1` multiply) as CSS `mix-blend-mode`.
frame_snapshot!(frame_blend_multiply, "blend_multiply");

// A v3.1.6 file: legacy 0–255 shape colours, the old property spellings, and
// precomps on staggered clocks. `merge-paths` allowed (invisible — its merged
// groups are static or style-bucketed); 0.05% pixel residual against
// lottie-web.
frame_snapshot!(frame_bodymoovin, "bodymoovin");

// Luma mattes (plain and inverted) — the inverted one cannot be pixel-diffed
// against lottie-web (see tests/track_matte.rs), so this reference render is
// the only regression gate it has.
frame_snapshot!(frame_matte_luma, "matte_luma");
frame_snapshot!(frame_matte_luma_inv, "matte_luma_inv");

// An animated colour ramp (one binding per <stop>) and an embedded image asset.
frame_snapshot!(frame_gradient_animated, "gradient_animated");
frame_snapshot!(frame_image_embedded, "image_embedded");
