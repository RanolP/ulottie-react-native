//! Data-driven backend.
//!
//! Lowers an `ir::Module` to a JS module that exports `init(container)`. In
//! `RuntimeMode::Extern` it imports `run` from a shared `./driver.js`; in
//! `RuntimeMode::Embedded` it inlines a tree-shaken copy of the runtime.
//! Either way the final source is passed through the minifier — so the
//! compiled artifact is what production would ship, and what the size
//! matrix measures.
//!
//! When the IR contains expressions, each unique expression is also emitted
//! as a JS function in an `E[]` array. The driver receives it as `run`'s
//! third argument and dispatches via `Property::Expression { e: id }`.

pub mod emit_expressions;
pub mod runtime;

use anyhow::Result;

use crate::data;
use crate::ir;
use crate::RuntimeMode;

pub fn compile(module: &ir::Module, runtime_mode: RuntimeMode) -> Result<Option<String>> {
    if !data::can_encode(module) {
        return Ok(None);
    }
    let payload = data::encode(module)?;
    Ok(Some(format_module(module, &payload, runtime_mode)?))
}

/// Render the JS source, then minify the whole module in one pass. The
/// single-pass minify is what makes the embedded mode's tree-shaking work
/// (HAS_* consts substituted by `prepare_embedded` fold to literals here,
/// dead branches collapse, unreachable functions are DCE'd) and lets the
/// extern mode ship the same minification level as `lottie.min.js`.
fn format_module(
    module: &ir::Module,
    payload: &data::Payload,
    runtime_mode: RuntimeMode,
) -> Result<String> {
    let json = serde_json::to_string(payload)?;
    let mut src = String::with_capacity(json.len() + 1024);

    match runtime_mode {
        RuntimeMode::Extern => {
            src.push_str("import { run } from './driver.js';\n");
        }
        RuntimeMode::Embedded => {
            let features = runtime::detect_features(module, payload);
            src.push_str(&runtime::prepare_embedded(features));
            src.push('\n');
        }
    }

    src.push_str(&format!("const D = {json};\n"));
    if !module.expressions.is_empty() {
        src.push_str("const E = [\n");
        for expr in module.expressions.iter() {
            emit_expressions::emit_one(&mut src, expr);
        }
        src.push_str("];\n");
        src.push_str("export const init = (container) => run(D, container, E);\n");
    } else {
        src.push_str("export const init = (container) => run(D, container);\n");
    }

    Ok(runtime::minify(&src).unwrap_or(src))
}
