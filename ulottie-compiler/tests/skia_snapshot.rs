//! Snapshot tests for the `skia-aot` target — the mirror of
//! `tests/rn_snapshot.rs` over the same fixture set.
//!
//! Each MVP fixture compiles to a React Native Skia module and is snapshotted
//! as `_fixtures/__snapshots__/<name>.skia.js`. A mismatched or missing
//! snapshot fails; `ULOTTIE_BLESS=1` is the only thing that writes one.

use std::fs;

use ulottie_compiler::support::Feature;
use ulottie_compiler::{compile_with, CompileOptions, Target};

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
        .join(format!("{name}.skia.js"))
}

fn compile_skia(name: &str, allow: &[Feature]) -> String {
    let json = fs::read_to_string(fixture_path(name))
        .unwrap_or_else(|_| panic!("missing fixture: {name}"));
    compile_with(
        &json,
        &CompileOptions {
            target: Target::SkiaAot,
            allow: allow.iter().copied().collect(),
            ..Default::default()
        },
    )
    .unwrap_or_else(|e| panic!("{name}: {e:#}"))
}

/// The module must be DOM-free, resolve every reference at compile time, and
/// carry the Skia export surface (`dl`/`meta`/`init`).
fn check_hygiene(name: &str, js: &str) {
    for export in ["export const dl", "export const meta", "export const init"] {
        assert!(js.contains(export), "{name}: missing `{export}`");
    }
    assert!(js.contains("'worklet'"), "{name}: no worklet directive");
    let code = strip_line_comments(js);
    for forbidden in [
        "setAttribute",
        "innerHTML",
        "querySelector",
        "document.",
        ".style.",
        "requestAnimationFrame",
        "matchMedia",
        "createElementNS",
        // Every url(#id) reference must have been resolved inline.
        "url(#",
    ] {
        assert!(
            !code.contains(forbidden),
            "{name}: `{forbidden}` leaked into the Skia module"
        );
    }
    // Bracket balance on the comment-stripped code, skipping string contents.
    let (mut paren, mut brace, mut bracket) = (0i64, 0i64, 0i64);
    let bytes = code.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' | b'"' | b'`' => {
                let q = bytes[i];
                i += 1;
                while i < bytes.len() && bytes[i] != q {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
            }
            b'(' => paren += 1,
            b')' => paren -= 1,
            b'{' => brace += 1,
            b'}' => brace -= 1,
            b'[' => bracket += 1,
            b']' => bracket -= 1,
            _ => {}
        }
        i += 1;
    }
    assert_eq!((paren, brace, bracket), (0, 0, 0), "{name}: unbalanced brackets");
}

/// Remove `// …` and `/* … */` comments, tracking string state — same as the
/// rn snapshot's strip (the runtime comments legitimately mention DOM APIs).
fn strip_line_comments(js: &str) -> String {
    let bytes = js.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            q @ (b'\'' | b'"' | b'`') => {
                out.push(bytes[i]);
                i += 1;
                while i < bytes.len() && bytes[i] != q {
                    out.push(bytes[i]);
                    if bytes[i] == b'\\' && i + 1 < bytes.len() {
                        out.push(bytes[i + 1]);
                        i += 1;
                    }
                    i += 1;
                }
                if i < bytes.len() {
                    out.push(bytes[i]);
                }
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
                continue;
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8(out).expect("comment strip is byte-preserving outside comments")
}

fn assert_snapshot(name: &str, allow: &[Feature]) {
    let js = compile_skia(name, allow);
    check_hygiene(name, &js);
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

macro_rules! skia_snapshot {
    ($name:ident, $fixture:literal $(, allow: [$($f:expr),* $(,)?])?) => {
        #[test]
        fn $name() {
            assert_snapshot($fixture, &[$($($f),*)?]);
        }
    };
}

skia_snapshot!(skia_boucing_ball, "boucing_ball");
skia_snapshot!(skia_rectangle, "rectangle");
skia_snapshot!(skia_ellipse, "ellipse");
skia_snapshot!(skia_fill, "fill");
skia_snapshot!(skia_trim_path, "trim_path");
skia_snapshot!(skia_android_wave, "android_wave");
skia_snapshot!(skia_precomp_star_circle, "precomp_star_circle");
skia_snapshot!(skia_gradient_radial, "gradient_radial");
// Inverted mattes compile without the allow-gate since phase 2: the
// inversion is a colour-matrix layer paint, exact in Skia.
skia_snapshot!(skia_lottie_logo_1, "lottie_logo_1");
skia_snapshot!(skia_mask_subtract, "mask_subtract");
skia_snapshot!(skia_matte_alpha, "matte_alpha");
skia_snapshot!(skia_stroke_under_fill, "stroke_under_fill");
skia_snapshot!(skia_bodymoovin, "bodymoovin");
skia_snapshot!(skia_lottie_logo_2, "lottie_logo_2");
skia_snapshot!(skia_lottie_logo_3, "lottie_logo_3");
skia_snapshot!(skia_fireworks, "fireworks");
skia_snapshot!(skia_matte_luma, "matte_luma");
// Phase 2 — the Skia-only capability set react-native-svg refuses.
skia_snapshot!(skia_blend_multiply, "blend_multiply");
skia_snapshot!(skia_gradient_animated, "gradient_animated");
skia_snapshot!(skia_matte_luma_inv, "matte_luma_inv");
skia_snapshot!(skia_fx_effects, "fx_effects");
// Phase 3 — an embedded image layer, decoded at mount and drawn with
// drawImageRect. External image sources stay a named refusal.
skia_snapshot!(skia_image_embedded, "image_embedded");

/// The refusal half of the image story: a *sourced but external* asset must
/// name itself as an `image-asset` finding instead of compiling.
#[test]
fn skia_image_external_refuses() {
    let json = fs::read_to_string(fixture_path("image_layer")).unwrap();
    let err = compile_with(
        &json,
        &CompileOptions { target: Target::SkiaAot, ..Default::default() },
    )
    .expect_err("an external image source must refuse");
    let msg = format!("{err:#}");
    assert!(msg.contains("image-asset"), "unexpected error: {msg}");
}
