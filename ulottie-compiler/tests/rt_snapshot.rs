//! Snapshot tests for the `rt` target — the mirror of `tests/skia_snapshot.rs`
//! over the same fixture set.
//!
//! Each fixture compiles to an RTDL module and is snapshotted as
//! `_fixtures/__snapshots__/<name>.rt.js`. A mismatched or missing snapshot
//! fails; `ULOTTIE_BLESS=1` is the only thing that writes one. On top of the
//! byte snapshot, every module's base64 blob must round-trip through
//! `ulottie_rt::rtdl::decode` — the exact structs the device runtime uses.

use std::fs;

use base64::Engine;
use ulottie_compiler::{CompileOptions, Target, compile_with};

fn fixture_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("_fixtures")
        .join("animations")
        .join(format!("{name}.json"))
}

fn snapshot_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("_fixtures")
        .join("__snapshots__")
        .join(format!("{name}.rt.js"))
}

fn compile_rt(name: &str) -> String {
    let json = fs::read_to_string(fixture_path(name))
        .unwrap_or_else(|_| panic!("missing fixture: {name}"));
    compile_with(
        &json,
        &CompileOptions {
            target: Target::Rt,
            ..Default::default()
        },
    )
    .unwrap_or_else(|e| panic!("{name}: {e:#}"))
}

/// The module carries the rt export surface, and its blob decodes with the
/// device runtime's own decoder into a scene with a root and at least one
/// drawable.
fn check_module(name: &str, js: &str) {
    for export in [
        "export const rtdl",
        "export const meta",
        "export const init",
    ] {
        assert!(js.contains(export), "{name}: missing `{export}`");
    }
    let b64 = js
        .split("export const rtdl = '")
        .nth(1)
        .and_then(|r| r.split('\'').next())
        .unwrap_or_else(|| panic!("{name}: no rtdl blob"));
    let blob = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .unwrap_or_else(|e| panic!("{name}: bad base64: {e}"));
    let anim = ulottie_rt::rtdl::decode(&blob)
        .unwrap_or_else(|e| panic!("{name}: RTDL decode failed: {e}"));
    assert!(anim.nodes.len() > 1, "{name}: no drawables beyond the root");
    assert!(
        anim.width > 0.0 && anim.height > 0.0,
        "{name}: no design size"
    );
    assert!(anim.op > anim.ip, "{name}: empty frame span");
    // Every layer-forcing group either has its bbox stamped or is known to
    // never draw — a missing bbox on a drawing layered group would silently
    // skip it at raster time.
    for node in &anim.nodes {
        let ulottie_rt::rtdl::Node::Group(g) = node else {
            continue;
        };
        let layered = g.blend.is_some() || g.mask.is_some() || g.cf.is_some() || !g.fx.is_empty();
        if layered && !g.children.is_empty() {
            assert!(
                g.bbox.is_some(),
                "{name}: layer-forcing group with children but no bbox"
            );
        }
    }
}

fn assert_snapshot(name: &str) {
    let js = compile_rt(name);
    check_module(name, &js);
    let path = snapshot_path(name);
    let bless = std::env::var_os("ULOTTIE_BLESS").is_some();
    match fs::read_to_string(&path) {
        Ok(existing) if existing == js => {}
        Ok(_) | Err(_) if bless => fs::write(&path, &js).unwrap(),
        Ok(existing) => {
            let at = existing
                .bytes()
                .zip(js.bytes())
                .position(|(a, b)| a != b)
                .unwrap_or(existing.len().min(js.len()));
            panic!(
                "{name}: snapshot mismatch at byte {at} ({}). \
                 Run with ULOTTIE_BLESS=1 to accept.",
                path.display()
            );
        }
        Err(e) => panic!(
            "{name}: no snapshot at {} ({e}). \
             Run with ULOTTIE_BLESS=1 to create it.",
            path.display()
        ),
    }
}

macro_rules! rt_snapshot {
    ($name:ident, $fixture:literal) => {
        #[test]
        fn $name() {
            assert_snapshot($fixture);
        }
    };
}

rt_snapshot!(rt_boucing_ball, "boucing_ball");
rt_snapshot!(rt_rectangle, "rectangle");
rt_snapshot!(rt_ellipse, "ellipse");
rt_snapshot!(rt_fill, "fill");
rt_snapshot!(rt_trim_path, "trim_path");
rt_snapshot!(rt_android_wave, "android_wave");
rt_snapshot!(rt_precomp_star_circle, "precomp_star_circle");
rt_snapshot!(rt_gradient_radial, "gradient_radial");
rt_snapshot!(rt_lottie_logo_1, "lottie_logo_1");
rt_snapshot!(rt_mask_subtract, "mask_subtract");
rt_snapshot!(rt_matte_alpha, "matte_alpha");
rt_snapshot!(rt_stroke_under_fill, "stroke_under_fill");
rt_snapshot!(rt_blend_multiply, "blend_multiply");
rt_snapshot!(rt_gradient_animated, "gradient_animated");
rt_snapshot!(rt_matte_luma_inv, "matte_luma_inv");
rt_snapshot!(rt_fx_effects, "fx_effects");
rt_snapshot!(rt_image_embedded, "image_embedded");

/// The refusal half of the image story, same as skia-aot: a *sourced but
/// external* asset must name itself as an `image-asset` finding.
#[test]
fn rt_image_external_refuses() {
    let json = fs::read_to_string(fixture_path("image_layer")).unwrap();
    let err = compile_with(
        &json,
        &CompileOptions {
            target: Target::Rt,
            ..Default::default()
        },
    )
    .expect_err("an external image source must refuse");
    let msg = format!("{err:#}");
    assert!(msg.contains("image-asset"), "unexpected error: {msg}");
}

/// Expressions are a named refusal on the rt target.
#[test]
fn rt_expressions_refuse() {
    let json = fs::read_to_string(fixture_path("expression_layer_ref")).unwrap();
    let err = compile_with(
        &json,
        &CompileOptions {
            target: Target::Rt,
            ..Default::default()
        },
    )
    .expect_err("expressions must refuse");
    let msg = format!("{err:#}");
    assert!(msg.contains("expression"), "unexpected error: {msg}");
}
