pub mod backend;
pub mod data;
#[cfg(feature = "eval")]
pub mod eval;
pub mod ir;
pub mod lottie;

use anyhow::Result;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RuntimeMode {
    /// Emit `import { run } from './driver.js'` — the runtime is a separate
    /// shared module. Default mode; best when many animations ship together.
    #[default]
    Extern,
    /// Inline a tree-shaken subset of the runtime into the compiled output.
    /// Produces a self-contained JS module with no external dependencies.
    /// Best for single animations or when the shared runtime can't be cached.
    Embedded,
}

#[derive(Debug, Clone, Default)]
pub struct CompileOptions {
    /// How the runtime is delivered: a shared `./driver.js` import (extern)
    /// or inlined into the compiled module (embedded).
    pub runtime_mode: RuntimeMode,
}

/// Compile a Lottie animation JSON string into JS module source code.
pub fn compile(json: &str) -> Result<String> {
    compile_with(json, &CompileOptions::default())
}

pub fn compile_with(json: &str, options: &CompileOptions) -> Result<String> {
    let animation: lottie::Animation = serde_json::from_str(json)?;
    let module = ir::lower(&animation)?;
    backend::compile(&module, options.runtime_mode)?
        .ok_or_else(|| anyhow::anyhow!("fixture uses features the data backend doesn't support"))
}

/// Lower a Lottie JSON straight to the data backend's `Payload`. Useful for
/// the frame evaluator (`eval::render`) which works on the Payload directly,
/// bypassing JS emission. Gated behind the `eval` feature because it only
/// exists to service the evaluator.
#[cfg(feature = "eval")]
pub fn compile_to_payload(json: &str) -> Result<data::Payload> {
    let animation: lottie::Animation = serde_json::from_str(json)?;
    let module = ir::lower(&animation)?;
    if !data::can_encode(&module) {
        anyhow::bail!("fixture uses features the data backend doesn't support yet");
    }
    data::encode(&module)
}

/// Which optional runtime features a Lottie animation requires. The embedded
/// build inlines only the regions whose flags are `true`; the rest are
/// tree-shaken out. Returned by [`analyze_features`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddedFeatures {
    /// Lottie expressions (`makeThisProperty`, path API, expression runtime).
    pub expressions: bool,
    /// `TrimPath` shape modifier (largest single feature region).
    pub trim_path: bool,
    /// Linear / radial gradient fills and strokes.
    pub gradient: bool,
}

/// Inspect a Lottie JSON to determine which optional runtime features the
/// embedded build would include. `Ok(None)` means the data backend can't
/// encode this fixture yet.
pub fn analyze_features(json: &str) -> Result<Option<EmbeddedFeatures>> {
    let animation: lottie::Animation = serde_json::from_str(json)?;
    let module = ir::lower(&animation)?;
    if !data::can_encode(&module) {
        return Ok(None);
    }
    let payload = data::encode(&module)?;
    let f = backend::runtime::detect_features(&module, &payload);
    Ok(Some(EmbeddedFeatures {
        expressions: f.contains(backend::runtime::Features::EXPRESSIONS),
        trim_path: f.contains(backend::runtime::Features::TRIM_PATH),
        gradient: f.contains(backend::runtime::Features::GRADIENT),
    }))
}

/// Minified `driver.js` (full runtime, all features intact, `export`
/// preserved). This is what production deployments ship at
/// `./driver.js` for extern-mode bundles; using it keeps the size
/// comparison against `lottie.min.js` apples-to-apples.
pub fn minified_driver() -> String {
    backend::runtime::build_minified_driver()
}

/// Build the embedded runtime source for an arbitrary feature subset. Used by
/// the dev server to compute per-feature size deltas: by calling this with
/// each feature individually stripped, the UI can show "if you tree-shake
/// this feature you save N bytes".
pub fn embedded_runtime_size(features: EmbeddedFeatures) -> usize {
    let mut f = backend::runtime::Features::empty();
    if features.expressions {
        f |= backend::runtime::Features::EXPRESSIONS;
    }
    if features.trim_path {
        f |= backend::runtime::Features::TRIM_PATH;
    }
    if features.gradient {
        f |= backend::runtime::Features::GRADIENT;
    }
    backend::runtime::build_embedded(f).len()
}
