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
    EmbeddedFeatures, RuntimeMode,
    backend,
    ir,
    lottie,
    minified_driver,
};

/// Outcome of compiling one Lottie animation. JS reads the byte arrays via
/// the getters, builds Blob URLs, and runs `CompressionStream` for gzip
/// sizes — same numbers the dev server's `/compile` reports.
#[wasm_bindgen]
pub struct CompileResult {
    compact_json: Vec<u8>,
    compiled_js: Vec<u8>,
    compiled_embedded: Vec<u8>,
    driver_min_js: Vec<u8>,
    total_frames: f64,
    name: Option<String>,
    features: EmbeddedFeatures,
    feature_cost_expressions: i64,
    feature_cost_trim_path: i64,
    feature_cost_gradient: i64,
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

    #[wasm_bindgen(getter, js_name = driverMinJs)]
    pub fn driver_min_js(&self) -> Uint8Array {
        bytes(&self.driver_min_js)
    }

    #[wasm_bindgen(getter, js_name = totalFrames)]
    pub fn total_frames(&self) -> f64 {
        self.total_frames
    }

    #[wasm_bindgen(getter)]
    pub fn name(&self) -> Option<String> {
        self.name.clone()
    }

    /// `{ included, costRaw }` mirroring `embeddedFeatures` on the dev
    /// server side, so the existing `featuresRow()` renderer works
    /// unchanged.
    #[wasm_bindgen(getter, js_name = embeddedFeatures)]
    pub fn embedded_features(&self) -> JsValue {
        let v = serde_json::json!({
            "included": {
                "expressions": self.features.expressions,
                "trimPath":   self.features.trim_path,
                "gradient":   self.features.gradient,
            },
            "costRaw": {
                "expressions": self.feature_cost_expressions,
                "trimPath":   self.feature_cost_trim_path,
                "gradient":   self.feature_cost_gradient,
            },
        });
        // Default serde-wasm-bindgen serializes serde Maps as JS `Map`,
        // but app.js reads `ef.included.expressions` with dot notation —
        // an Object is what we want.
        let ser = serde_wasm_bindgen::Serializer::new().serialize_maps_as_objects(true);
        v.serialize(&ser).unwrap_or(JsValue::NULL)
    }
}

/// Compile a Lottie JSON source. Returns a [`CompileResult`] populated with
/// the same outputs the dev server's `/compile` endpoint emits — minus the
/// URLs (the JS side mints Blob URLs from the byte arrays).
#[wasm_bindgen(js_name = compileRequest)]
pub fn compile_request(json_text: &str) -> Result<CompileResult, JsError> {
    let animation: lottie::Animation = serde_json::from_str(json_text)
        .map_err(|e| JsError::new(&format!("invalid JSON: {e}")))?;
    let module = ir::lower(&animation)
        .map_err(|e| JsError::new(&format!("IR lowering failed: {e}")))?;

    let (ip, op, name) = (animation.in_point, animation.out_point, animation.name.clone());

    let compiled_js = backend::compile(&module, RuntimeMode::Extern)
        .map_err(|e| JsError::new(&format!("compile extern: {e}")))?
        .ok_or_else(|| JsError::new("data backend can't encode this fixture"))?;
    let compiled_embedded = backend::compile(&module, RuntimeMode::Embedded)
        .map_err(|e| JsError::new(&format!("compile embedded: {e}")))?
        .unwrap_or_default();

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
    let all_on = EmbeddedFeatures { expressions: true, trim_path: true, gradient: true };
    let full = crate::embedded_runtime_size(all_on) as i64;
    let cost = |omitted: EmbeddedFeatures| full - crate::embedded_runtime_size(omitted) as i64;
    let feature_cost_expressions = cost(EmbeddedFeatures { expressions: false, ..all_on });
    let feature_cost_trim_path = cost(EmbeddedFeatures { trim_path: false, ..all_on });
    let feature_cost_gradient = cost(EmbeddedFeatures { gradient: false, ..all_on });

    let features = crate::analyze_features(json_text)
        .ok()
        .flatten()
        .unwrap_or_default();

    Ok(CompileResult {
        compact_json,
        compiled_js: compiled_js.into_bytes(),
        compiled_embedded: compiled_embedded.into_bytes(),
        driver_min_js: driver_min_js.into_bytes(),
        total_frames: (op - ip).max(0.0),
        name,
        features,
        feature_cost_expressions,
        feature_cost_trim_path,
        feature_cost_gradient,
    })
}

fn bytes(v: &[u8]) -> Uint8Array {
    // `from(&[u8])` allocates a fresh Uint8Array in JS land and copies in;
    // ownership remains in Rust so subsequent getter calls observe the
    // same underlying buffer.
    Uint8Array::from(v)
}
