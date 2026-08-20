//! End-to-end tests for pre-loadable asset extraction (`--assets` /
//! `CompileOptions::assets`): oversized embedded images become URL
//! references plus a manifest; everything at or below the threshold — and
//! every compile that does not opt in — stays exactly as it was.

use ulottie_compiler::{compile_with, compile_with_output, AssetOptions, CompileOptions};

fn fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("_fixtures/animations")
        .join(name);
    std::fs::read_to_string(path).unwrap()
}

fn extracting(threshold: usize) -> CompileOptions {
    CompileOptions {
        assets: AssetOptions {
            extract: true,
            url_base: "assets/".into(),
            threshold,
        },
        ..Default::default()
    }
}

/// FNV-1a, so the byte-identity regression guard needs no hash dependency.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |h, b| {
        (h ^ *b as u64).wrapping_mul(0x100000001b3)
    })
}

#[test]
fn embedded_images_above_the_threshold_are_extracted() {
    let json = fixture("image_embedded.json");
    // The fixture's image decodes to ~100 bytes, so a tiny threshold makes it
    // "oversized" for the test.
    let out = compile_with_output(&json, &extracting(8)).unwrap();

    assert_eq!(out.assets.len(), 1, "one image asset: {:#?}", out.assets);
    let a = &out.assets[0];
    assert!(a.name.starts_with("img_"), "content-hash name: {}", a.name);
    assert!(a.name.ends_with(".png"));
    assert_eq!(&a.bytes[..8], b"\x89PNG\r\n\x1a\n", "decodes to a PNG");
    assert!(
        !out.module.contains("data:image/png;base64"),
        "the module must not carry the payload any more"
    );
    assert!(
        out.module.contains(&format!("assets/{}", a.name)),
        "the markup references the file by URL"
    );

    let manifest: serde_json::Value = serde_json::from_str(&out.manifest).unwrap();
    let e = &manifest[0];
    assert_eq!(e["file"], a.name);
    assert_eq!(e["url"], format!("assets/{}", a.name));
    assert_eq!(e["mime"], "image/png");
    assert_eq!(e["bytes"], a.bytes.len());
}

#[test]
fn images_below_the_threshold_stay_inline() {
    let json = fixture("image_embedded.json");
    // The whole image (134 base64 chars ≈ 100 bytes) is under this.
    let out = compile_with_output(&json, &extracting(4096)).unwrap();
    assert!(out.assets.is_empty());
    assert_eq!(out.manifest, "[]");
    assert!(out.module.contains("data:image/png;base64"));
}

#[test]
fn default_options_are_byte_identical_to_before_extraction() {
    let json = fixture("image_embedded.json");
    let js = compile_with(&json, &CompileOptions::default()).unwrap();
    // The module as the compiler shipped it before the extraction pass
    // existed (extern, minified), pinned by content hash.
    assert_eq!(
        fnv1a(js.as_bytes()),
        0x88beb7d839858ca0,
        "default-options output changed; extraction must be a no-op when off"
    );
    // And the data URI is still inline, not referenced.
    assert!(js.contains("data:image/png;base64"));
}
