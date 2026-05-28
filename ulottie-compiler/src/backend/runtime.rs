//! Embedded runtime build pipeline.
//!
//! Driver.js is a single hand-written file that ships every optional feature
//! (expressions, trim path, gradient) behind compile-time `if (HAS_*)` gates.
//! In dev / extern mode the `HAS_*` consts are `true`, so the runtime works
//! unchanged.
//!
//! In embedded mode this module:
//!   1. Detects which features the animation needs from its data backend
//!      payload + IR.
//!   2. Substitutes the placeholder `HAS_*` const declarations in driver.js
//!      with the actual flag values.
//!   3. Strips the `export` keyword from `run` so the caller can wrap it in
//!      its own `export const init`.
//!   4. Runs the result through the oxc minifier with dead-code elimination,
//!      which folds the now-constant flag expressions, drops the dead
//!      branches, and tree-shakes the unreferenced functions.
//!
//! The minifier is the source of truth for which functions survive. No
//! per-function table, no region markers — just `if (HAS_FOO)` gates that
//! constant-fold away.

use bitflags::bitflags;

use crate::data;
use crate::ir;

const DRIVER_JS: &str = include_str!("../../runtime/driver.js");

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Features: u8 {
        const EXPRESSIONS = 1 << 0;
        const TRIM_PATH   = 1 << 1;
        const GRADIENT    = 1 << 2;
    }
}

/// Inspect the IR module + encoded payload to determine which optional
/// runtime features are required.
pub fn detect_features(module: &ir::Module, payload: &data::Payload) -> Features {
    let mut f = Features::empty();
    if !module.expressions.is_empty() {
        f |= Features::EXPRESSIONS;
    }
    for prop in &payload.p {
        if matches!(prop, data::Property::Expression(_)) {
            f |= Features::EXPRESSIONS;
        }
    }
    for style in &payload.y {
        match style {
            data::Style::TrimPath { .. } => f |= Features::TRIM_PATH,
            data::Style::GradientStroke { .. } | data::Style::GradientFill { .. } => {
                f |= Features::GRADIENT;
            }
            _ => {}
        }
    }
    f
}

/// Prepare the embedded runtime source for the given feature set: substitute
/// the `HAS_*` const placeholders with the resolved flag values and drop
/// `export` from `run` so the embedded caller can wrap it in its own
/// `export const init`. Returned un-minified — the final minifier pass at
/// the end of `format_module` folds the constants, eliminates dead
/// branches, and tree-shakes the unreferenced functions.
///
/// Substitution is done with a literal text replace rather than an AST
/// rewrite because the placeholders are a fixed shape under our control —
/// the JS file's own contract, not arbitrary user code.
pub(crate) fn prepare_embedded(needed: Features) -> String {
    let mut src = DRIVER_JS
        .replace(
            "const HAS_EXPRESSIONS = true;",
            &format!("const HAS_EXPRESSIONS = {};", needed.contains(Features::EXPRESSIONS)),
        )
        .replace(
            "const HAS_TRIM_PATH = true;",
            &format!("const HAS_TRIM_PATH = {};", needed.contains(Features::TRIM_PATH)),
        )
        .replace(
            "const HAS_GRADIENT = true;",
            &format!("const HAS_GRADIENT = {};", needed.contains(Features::GRADIENT)),
        );
    src = src.replace("export function run(", "function run(");
    src
}

/// Build the minified embedded runtime standalone (no data, no init wrapper).
/// Used by `embedded_runtime_size` to price each feature against the
/// all-features-on baseline — the dev server caches the deltas to drive the
/// per-feature byte-cost chips in the matrix.
pub fn build_embedded(needed: Features) -> String {
    let source = prepare_embedded(needed);
    minify(&source).unwrap_or(source)
}

/// Build the minified shared runtime — driver.js with all `HAS_*` flags
/// kept `true`, `export function run` preserved, and run through the same
/// minifier as the embedded variant. This is what extern mode ships at
/// `/.output/driver.js`; the size comparison against `lottie.min.js`
/// stays apples-to-apples because both sides are minified.
pub fn build_minified_driver() -> String {
    minify(DRIVER_JS).unwrap_or_else(|| DRIVER_JS.to_string())
}

// ---------------------------------------------------------------------------
// Minifier (oxc) — feature-gated behind `minify`.
// ---------------------------------------------------------------------------

#[cfg(feature = "minify")]
pub(crate) fn minify(source: &str) -> Option<String> {
    use oxc_allocator::Allocator;
    use oxc_codegen::{Codegen, CodegenOptions};
    use oxc_minifier::{Minifier, MinifierOptions};
    use oxc_parser::Parser;
    use oxc_span::SourceType;

    let allocator = Allocator::default();
    let source_type = SourceType::mjs();
    let parsed = Parser::new(&allocator, source, source_type).parse();
    if parsed.panicked || !parsed.errors.is_empty() {
        return None;
    }
    let mut program = parsed.program;

    Minifier::new(MinifierOptions::default()).minify(&allocator, &mut program);

    // `CodegenOptions::minify()` strips whitespace + comments. That's what we
    // want for the embedded payload — the matrix's headline is "self-contained
    // and small", not "self-contained but readable".
    Some(
        Codegen::new()
            .with_options(CodegenOptions::minify())
            .build(&program)
            .code,
    )
}

/// Fallback when the `minify` feature is disabled: emit the gated source
/// as-is. Without DCE the embedded module ships the full driver, but the
/// gates ensure unused branches are at least skipped at runtime.
#[cfg(not(feature = "minify"))]
pub(crate) fn minify(_source: &str) -> Option<String> {
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_present_in_driver() {
        assert!(DRIVER_JS.contains("const HAS_EXPRESSIONS = true;"));
        assert!(DRIVER_JS.contains("const HAS_TRIM_PATH = true;"));
        assert!(DRIVER_JS.contains("const HAS_GRADIENT = true;"));
    }

    #[test]
    fn prepare_substitutes_flags() {
        let src = prepare_embedded(Features::EXPRESSIONS);
        assert!(src.contains("const HAS_EXPRESSIONS = true;"));
        assert!(src.contains("const HAS_TRIM_PATH = false;"));
        assert!(src.contains("const HAS_GRADIENT = false;"));
        assert!(!src.contains("export function run("));
        assert!(src.contains("function run("));
    }

    #[cfg(feature = "minify")]
    #[test]
    fn embedded_strips_unused_features_via_dce() {
        let core = build_embedded(Features::empty());
        // Constants folded, dead branches eliminated, function declarations
        // tree-shaken. The minifier renames some identifiers; we check
        // distinctive substrings that survive renaming.
        assert!(
            !core.contains("computeTrimRange"),
            "trim-path function should be tree-shaken when not needed"
        );
        assert!(
            !core.contains("ensureGradient"),
            "gradient function should be tree-shaken when not needed"
        );
        assert!(
            !core.contains("attachExpressionRuntime"),
            "expression bootstrap should be tree-shaken when not needed"
        );
    }

    #[cfg(feature = "minify")]
    #[test]
    fn embedded_keeps_needed_feature_functions() {
        let trim = build_embedded(Features::TRIM_PATH);
        assert!(trim.contains("trimPath") || trim.contains("computeTrimRange"));

        let grad = build_embedded(Features::GRADIENT);
        assert!(grad.contains("ensureGradient") || grad.contains("Gradient"));

        let exprs = build_embedded(Features::EXPRESSIONS);
        assert!(exprs.contains("attachExpressionRuntime") || exprs.contains("makeThisProperty"));
    }

    #[cfg(feature = "minify")]
    #[test]
    fn embedded_size_shrinks_when_features_stripped() {
        let full = build_embedded(Features::all()).len();
        let core = build_embedded(Features::empty()).len();
        assert!(
            core < full,
            "stripping features must shrink the embedded runtime: \
             full={full} core={core}"
        );
    }
}
