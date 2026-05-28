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
frame_snapshot!(frame_bouncy_ball, "bouncy_ball");
frame_snapshot!(frame_boucing_ball, "boucing-ball");

// Step 3: opacity propagation + parent-chain transforms.
frame_snapshot!(frame_lottie_logo, "lottie-logo");

// Step 4: gradients (linear + radial; color+alpha stop merging).
frame_snapshot!(frame_ripple, "ripple");

// Step 6: precomp instances with inner clock offset.
frame_snapshot!(frame_precomp_star_circle, "precomp_star_circle");

// Step 5: masks (add/subtract) with animated path keyframes.
frame_snapshot!(frame_starfish, "starfish");

// Step 7+: expressions via rquickjs ($bm_rt context + callable proxies).
frame_snapshot!(frame_lights, "lights");
