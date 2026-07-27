//! JS minification.
//!
//! The only thing left of what used to be a second runtime + bundler: every
//! animation now goes through the scene planner, so `backend::emit` assembles
//! the runtime and this just shrinks the result.

// ---------------------------------------------------------------------------
// Minifier (oxc) — feature-gated behind `minify`.
// ---------------------------------------------------------------------------

#[cfg(feature = "minify")]
pub fn minify(source: &str) -> Option<String> {
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

    // `Minifier::minify` computes the mangled scoping but does not rewrite the
    // AST — the renamed bindings only materialize if the scoping is handed to
    // the codegen. Dropping it silently shipped every identifier at full
    // length.
    let ret = Minifier::new(MinifierOptions::default()).minify(&allocator, &mut program);

    Some(
        Codegen::new()
            .with_options(CodegenOptions::minify())
            .with_scoping(ret.scoping)
            .build(&program)
            .code,
    )
}

#[cfg(not(feature = "minify"))]
pub fn minify(_source: &str) -> Option<String> {
    None
}
