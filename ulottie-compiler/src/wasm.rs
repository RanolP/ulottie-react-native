//! Browser bindings: same `/compile` shape the dev server returns, but run
//! entirely in-process inside the page. The JS-side wraps the returned
//! bytes in Blob URLs, measures gzip with `CompressionStream('gzip')`, and
//! drives the existing comparison matrix unchanged.
//!
//! `CompileRequest` is a `#[wasm_bindgen]` struct so the byte arrays land
//! on the JS side as `Uint8Array` directly — no JSON-of-numbers
//! intermediate, no extra copy.

use js_sys::Uint8Array;
use serde::Serialize as _;
use wasm_bindgen::prelude::*;

use crate::{
    AssetOptions, CompileOptions, EmbeddedFeatures, ExtractedAsset, MarkupMode, RuntimeMode,
    backend, ir, lottie, minified_driver,
};

/// Outcome of compiling one Lottie animation. JS reads the byte arrays via
/// the getters, builds Blob URLs, and runs `CompressionStream` for gzip
/// sizes — same numbers the dev server's `/compile` reports.
#[wasm_bindgen]
pub struct CompileResult {
    compact_json: Vec<u8>,
    compiled_js: Vec<u8>,
    compiled_embedded: Vec<u8>,
    compiled_extracted: Vec<u8>,
    sprite_svg: Vec<u8>,
    // The SSR pair: the baked document, and the self-contained module with no
    // markup of its own that hydrates it.
    compiled_hydrate: Vec<u8>,
    document_svg: Vec<u8>,
    runtime_slice: Vec<u8>,
    // The same artifacts as the compiler writes them before minification —
    // what the demo's viewer shows. Producing them here rather than
    // reconstructing structure in the page keeps the wasm build at parity with
    // the server, and there is nothing for a formatter to get wrong.
    pretty_js: Vec<u8>,
    pretty_embedded: Vec<u8>,
    pretty_extracted: Vec<u8>,
    pretty_hydrate: Vec<u8>,
    pretty_document: Vec<u8>,
    pretty_slice: Vec<u8>,
    pretty_sprite: Vec<u8>,
    driver_min_js: Vec<u8>,
    total_frames: f64,
    name: Option<String>,
    features: EmbeddedFeatures,
    feature_cost_expressions: i64,
    feature_cost_trim_path: i64,
    feature_cost_gradient: i64,
    plan: serde_json::Value,
    unsupported: serde_json::Value,
    // Images extraction pulled out of the markup. The markup references them
    // as `assets/<name>` (the same stable spelling the CLI writes); the JS
    // side mints a Blob URL per file and rewrites the references, which is the
    // in-browser stand-in for a server that writes the files and serves them.
    assets: Vec<ExtractedAsset>,
}

#[wasm_bindgen]
impl CompileResult {
    #[wasm_bindgen(getter, js_name = compactJson)]
    pub fn compact_json(&self) -> Uint8Array {
        bytes(&self.compact_json)
    }

    #[wasm_bindgen(getter, js_name = compiledJs)]
    pub fn compiled_js(&self) -> Uint8Array {
        bytes(&self.compiled_js)
    }

    #[wasm_bindgen(getter, js_name = compiledEmbedded)]
    pub fn compiled_embedded(&self) -> Uint8Array {
        bytes(&self.compiled_embedded)
    }

    #[wasm_bindgen(getter, js_name = compiledExtracted)]
    pub fn compiled_extracted(&self) -> Uint8Array {
        bytes(&self.compiled_extracted)
    }

    #[wasm_bindgen(getter, js_name = spriteSvg)]
    pub fn sprite_svg(&self) -> Uint8Array {
        bytes(&self.sprite_svg)
    }

    /// The self-contained hydration module: no markup, adopts a served
    /// `documentSvg`.
    #[wasm_bindgen(getter, js_name = compiledHydrate)]
    pub fn compiled_hydrate(&self) -> Uint8Array {
        bytes(&self.compiled_hydrate)
    }

    /// The baked document — the SSR response, and what `compiledHydrate`
    /// hydrates.
    #[wasm_bindgen(getter, js_name = documentSvg)]
    pub fn document_svg(&self) -> Uint8Array {
        bytes(&self.document_svg)
    }

    /// Minified source of only the runtime modules this animation imports —
    /// what a bundler ships for it, as opposed to `driverMinJs`, the ceiling.
    #[wasm_bindgen(getter, js_name = runtimeSlice)]
    pub fn runtime_slice(&self) -> Uint8Array {
        bytes(&self.runtime_slice)
    }

    #[wasm_bindgen(getter, js_name = prettyJs)]
    pub fn pretty_js(&self) -> Uint8Array {
        bytes(&self.pretty_js)
    }

    #[wasm_bindgen(getter, js_name = prettyEmbedded)]
    pub fn pretty_embedded(&self) -> Uint8Array {
        bytes(&self.pretty_embedded)
    }

    #[wasm_bindgen(getter, js_name = prettyExtracted)]
    pub fn pretty_extracted(&self) -> Uint8Array {
        bytes(&self.pretty_extracted)
    }

    #[wasm_bindgen(getter, js_name = prettyHydrate)]
    pub fn pretty_hydrate(&self) -> Uint8Array {
        bytes(&self.pretty_hydrate)
    }

    #[wasm_bindgen(getter, js_name = prettyDocument)]
    pub fn pretty_document(&self) -> Uint8Array {
        bytes(&self.pretty_document)
    }

    #[wasm_bindgen(getter, js_name = prettySlice)]
    pub fn pretty_slice(&self) -> Uint8Array {
        bytes(&self.pretty_slice)
    }

    #[wasm_bindgen(getter, js_name = prettySprite)]
    pub fn pretty_sprite(&self) -> Uint8Array {
        bytes(&self.pretty_sprite)
    }

    #[wasm_bindgen(getter, js_name = driverMinJs)]
    pub fn driver_min_js(&self) -> Uint8Array {
        bytes(&self.driver_min_js)
    }

    /// How many images were extracted from the markup.
    #[wasm_bindgen(getter, js_name = assetCount)]
    pub fn asset_count(&self) -> usize {
        self.assets.len()
    }

    /// Extracted asset `i`'s file name (`img_<hash>.<ext>`), or `undefined`.
    #[wasm_bindgen(js_name = assetName)]
    pub fn asset_name(&self, i: usize) -> Option<String> {
        self.assets.get(i).map(|a| a.name.clone())
    }

    /// Extracted asset `i`'s MIME type, or `undefined`.
    #[wasm_bindgen(js_name = assetMime)]
    pub fn asset_mime(&self, i: usize) -> Option<String> {
        self.assets.get(i).map(|a| a.mime.clone())
    }

    /// Extracted asset `i`'s decoded bytes — the file to serve (as a Blob, here).
    #[wasm_bindgen(js_name = assetBytes)]
    pub fn asset_bytes(&self, i: usize) -> Option<Uint8Array> {
        self.assets.get(i).map(|a| bytes(&a.bytes))
    }

    /// What the AOT stage decided: capabilities, imports, instancing, counts.
    #[wasm_bindgen(getter)]
    pub fn plan(&self) -> JsValue {
        to_object(&self.plan)
    }

    /// Features the backend does not implement, with their visible effect.
    #[wasm_bindgen(getter)]
    pub fn unsupported(&self) -> JsValue {
        to_object(&self.unsupported)
    }

    #[wasm_bindgen(getter, js_name = total_frames)]
    pub fn total_frames(&self) -> f64 {
        self.total_frames
    }

    #[wasm_bindgen(getter)]
    pub fn name(&self) -> Option<String> {
        self.name.clone()
    }

    /// The flattened feature report, matching `contract::FeatureReport` on the
    /// server side — the shape the generated bindings describe, so both
    /// backends hand the page the same object.
    #[wasm_bindgen(getter)]
    pub fn features(&self) -> JsValue {
        to_object(&serde_json::json!({
            "expressions": self.features.expressions,
            "trim_path": self.features.trim_path,
            "gradient": self.features.gradient,
            "expressions_cost": self.feature_cost_expressions,
            "trim_path_cost": self.feature_cost_trim_path,
            "gradient_cost": self.feature_cost_gradient,
        }))
    }
}

/// Compile a Lottie JSON source. Returns a [`CompileResult`] populated with
/// the same outputs the dev server's `/compile` endpoint emits — minus the
/// URLs (the JS side mints Blob URLs from the byte arrays).
#[wasm_bindgen(js_name = compileRequest)]
pub fn compile_request(json_text: &str) -> Result<CompileResult, JsError> {
    let animation: lottie::Animation =
        serde_json::from_str(json_text).map_err(|e| JsError::new(&format!("invalid JSON: {e}")))?;
    let module =
        ir::lower(&animation).map_err(|e| JsError::new(&format!("IR lowering failed: {e}")))?;

    let (ip, op, name) = (
        animation.in_point,
        animation.out_point,
        animation.name.clone(),
    );

    // Everything the source uses that the backend does not implement. Allowed
    // wholesale here: the page is a viewer, and refusing to show a degraded
    // render would be less useful than showing it next to the warning.
    let found = crate::unsupported(json_text).unwrap_or_default();
    let allow: std::collections::BTreeSet<_> = found.iter().map(|f| f.feature).collect();

    // Same delivery shape as the dev server: oversized embedded images come
    // out of the markup as `assets/<name>` references, handed back as bytes
    // for the page to mint Blob URLs from. There is nowhere to write files in
    // a page — rewriting the reference is the whole difference.
    let assets_on = || AssetOptions {
        extract: true,
        ..Default::default()
    };
    let mut assets: Vec<ExtractedAsset> = Vec::new();
    let mut keep = |more: Vec<ExtractedAsset>| {
        for a in more {
            if !assets.iter().any(|x| x.name == a.name) {
                assets.push(a);
            }
        }
    };

    let opts = |m| CompileOptions {
        runtime_mode: m,
        allow: allow.clone(),
        assets: assets_on(),
        ..Default::default()
    };
    let report = backend::report(&module, &opts(RuntimeMode::Extern))
        .map_err(|e| JsError::new(&format!("compile extern: {e}")))?
        .ok_or_else(|| JsError::new("data backend can't encode this fixture"))?;
    keep(report.assets.clone());
    let compiled_js = report.js.clone();
    let embedded_rep = backend::report(&module, &opts(RuntimeMode::Embedded))
        .map_err(|e| JsError::new(&format!("compile embedded: {e}")))?
        .ok_or_else(|| JsError::new("data backend can't encode this fixture"))?;
    keep(embedded_rep.assets.clone());
    let compiled_embedded = embedded_rep.js;

    // Extracted delivery: the module carries only the `<svg>` shell and the
    // markup ships as a sprite the page inlines or preloads.
    let extracted_opts = CompileOptions {
        markup: MarkupMode::Extracted("anim".into()),
        allow: allow.clone(),
        assets: assets_on(),
        ..Default::default()
    };
    let extracted_rep = backend::report(&module, &extracted_opts)
        .map_err(|e| JsError::new(&format!("compile extracted: {e}")))?
        .ok_or_else(|| JsError::new("data backend can't encode this fixture"))?;
    keep(extracted_rep.assets.clone());
    let compiled_extracted = extracted_rep.js;
    let sprite_out = crate::compile_symbol_with(json_text, "anim", &extracted_opts)
        .map_err(|e| JsError::new(&format!("compile sprite: {e}")))?;
    keep(sprite_out.assets);
    let sprite_svg = crate::sprite(&[sprite_out.svg]);

    // The SSR pair. The document is what a server renders into the HTML; the
    // hydration module is the self-contained build with no markup in it.
    let hydrate_opts = CompileOptions {
        runtime_mode: RuntimeMode::Embedded,
        markup: MarkupMode::None,
        allow: allow.clone(),
        assets: assets_on(),
        ..Default::default()
    };
    let hydrate_rep = backend::report(&module, &hydrate_opts)
        .map_err(|e| JsError::new(&format!("compile hydrate: {e}")))?
        .ok_or_else(|| JsError::new("data backend can't encode this fixture"))?;
    keep(hydrate_rep.assets.clone());
    let compiled_hydrate = hydrate_rep.js;
    let document_out = crate::compile_document_with(json_text, &opts(RuntimeMode::Extern))
        .map_err(|e| JsError::new(&format!("compile document: {e}")))?;
    keep(document_out.assets);
    let document_svg = document_out.svg;

    let runtime_slice = if report.is_static {
        String::new()
    } else {
        crate::runtime_slice(&report.caps)
    };

    let unmin = |m, markup: MarkupMode| CompileOptions {
        runtime_mode: m,
        markup,
        allow: allow.clone(),
        minify: false,
        assets: assets_on(),
        ..Default::default()
    };
    let pretty_js = backend::compile(&module, &unmin(RuntimeMode::Extern, MarkupMode::Inline))
        .unwrap_or_default()
        .unwrap_or_default();
    let pretty_embedded =
        backend::compile(&module, &unmin(RuntimeMode::Embedded, MarkupMode::Inline))
            .unwrap_or_default()
            .unwrap_or_default();
    let pretty_extracted = backend::compile(
        &module,
        &unmin(RuntimeMode::Extern, MarkupMode::Extracted("anim".into())),
    )
    .unwrap_or_default()
    .unwrap_or_default();
    let pretty_hydrate = backend::compile(&module, &unmin(RuntimeMode::Embedded, MarkupMode::None))
        .unwrap_or_default()
        .unwrap_or_default();
    let pretty_document = crate::markup_pretty(&document_svg);
    let pretty_sprite = crate::markup_pretty(&sprite_svg);
    let pretty_slice = if report.is_static {
        String::new()
    } else {
        crate::runtime_slice_pretty(&report.caps)
    };

    let plan = serde_json::json!({
        "caps": report.caps,
        "modules": report.modules,
        "isStatic": report.is_static,
        "instanced": report.instanced,
        "templated": report.templated,
        "elements": report.elements,
        "bindings": report.bindings,
        "records": report.records,
    });
    let unsupported = serde_json::Value::Array(
        found
            .iter()
            .map(|f| {
                serde_json::json!({
                    "feature": f.feature.name(),
                    "effect": f.feature.effect(),
                    // The viewer allows everything, so anything listed here is
                    // a degradation you are currently looking at.
                    "allowed": true,
                })
            })
            .collect(),
    );

    // Compact JSON: re-stringify without whitespace so the matrix's
    // "Lottie JSON" size reflects what production would ship, same rule
    // the dev server applies.
    let compact_json = serde_json::to_vec(
        &serde_json::from_str::<serde_json::Value>(json_text)
            .map_err(|e| JsError::new(&format!("re-parse JSON: {e}")))?,
    )
    .map_err(|e| JsError::new(&format!("re-serialize JSON: {e}")))?;

    let driver_min_js = minified_driver();

    // Per-feature byte cost: identical formula to the dev server's
    // `feature_costs()` cache, recomputed each request because in wasm the
    // page is single-shot — no point caching across compiles.
    let all_on = EmbeddedFeatures {
        expressions: true,
        trim_path: true,
        gradient: true,
    };
    let full = crate::embedded_runtime_size(all_on) as i64;
    let cost = |omitted: EmbeddedFeatures| full - crate::embedded_runtime_size(omitted) as i64;
    let feature_cost_expressions = cost(EmbeddedFeatures {
        expressions: false,
        ..all_on
    });
    let feature_cost_trim_path = cost(EmbeddedFeatures {
        trim_path: false,
        ..all_on
    });
    let feature_cost_gradient = cost(EmbeddedFeatures {
        gradient: false,
        ..all_on
    });

    let features = crate::analyze_features(json_text)
        .ok()
        .flatten()
        .unwrap_or_default();

    Ok(CompileResult {
        compact_json,
        compiled_js: compiled_js.into_bytes(),
        compiled_embedded: compiled_embedded.into_bytes(),
        compiled_extracted: compiled_extracted.into_bytes(),
        sprite_svg: sprite_svg.into_bytes(),
        compiled_hydrate: compiled_hydrate.into_bytes(),
        document_svg: document_svg.into_bytes(),
        runtime_slice: runtime_slice.into_bytes(),
        pretty_js: pretty_js.into_bytes(),
        pretty_embedded: pretty_embedded.into_bytes(),
        pretty_extracted: pretty_extracted.into_bytes(),
        pretty_hydrate: pretty_hydrate.into_bytes(),
        pretty_document: pretty_document.into_bytes(),
        pretty_slice: pretty_slice.into_bytes(),
        pretty_sprite: pretty_sprite.into_bytes(),
        driver_min_js: driver_min_js.into_bytes(),
        total_frames: (op - ip).max(0.0),
        name,
        features,
        feature_cost_expressions,
        feature_cost_trim_path,
        feature_cost_gradient,
        plan,
        unsupported,
        assets,
    })
}

/// serde-wasm-bindgen serializes maps as JS `Map` by default; the page reads
/// these with dot notation, so they have to come across as plain objects.
fn to_object(v: &serde_json::Value) -> JsValue {
    let ser = serde_wasm_bindgen::Serializer::new().serialize_maps_as_objects(true);
    v.serialize(&ser).unwrap_or(JsValue::NULL)
}

fn bytes(v: &[u8]) -> Uint8Array {
    // `from(&[u8])` allocates a fresh Uint8Array in JS land and copies in;
    // ownership remains in Rust so subsequent getter calls observe the
    // same underlying buffer.
    Uint8Array::from(v)
}
