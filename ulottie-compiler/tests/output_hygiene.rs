//! Output hygiene: an AOT compiler should not emit code the animation cannot
//! reach.
//!
//! The bundler resolves reachability per top-level declaration, so a fixture
//! that never animates a path must not carry the path interpolator and one that
//! never formats a plain coordinate must not carry that formatter. This asserts
//! it on the real emitted module rather than trusting the minifier.

use std::fs;

use ulottie_compiler::backend::shake;
use ulottie_compiler::{compile_with, CompileOptions, RuntimeMode};

mod common;

fn fixture_names() -> Vec<String> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("_fixtures/animations");
    let mut names: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| {
            let p = e.ok()?.path();
            (p.extension()? == "json").then(|| p.file_stem()?.to_str().map(String::from))?
        })
        .collect();
    names.sort();
    names
}

fn source(name: &str) -> String {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("_fixtures/animations");
    fs::read_to_string(dir.join(format!("{name}.json"))).unwrap()
}

fn emitted(name: &str, mode: RuntimeMode) -> String {
    compile_with(
        &source(name),
        &CompileOptions {
            runtime_mode: mode,
            minify: false,
            allow: common::allowances(name),
            ..Default::default()
        },
    )
    .unwrap_or_else(|e| panic!("compile {name}: {e}"))
}

/// Declarations in the emitted module that nothing reaches from its exports.
fn dead_declarations(src: &str) -> Vec<String> {
    let all = shake::declarations(src);
    let names: Vec<String> = all.iter().map(|d| d.name.clone()).collect();
    // `markup` and `init` are the module's exports; everything else has to be
    // reachable from them.
    let live = shake::shake(all, &["markup", "init"], ulottie_compiler::scene::Caps::all());
    let live: Vec<&str> = live.iter().map(|d| d.name.as_str()).collect();
    names
        .into_iter()
        .filter(|n| !live.contains(&n.as_str()))
        .collect()
}

#[test]
fn embedded_output_has_no_unreachable_declarations() {
    let mut problems = Vec::new();
    for name in fixture_names() {
        let src = emitted(&name, RuntimeMode::Embedded);
        let dead = dead_declarations(&src);
        if !dead.is_empty() {
            problems.push(format!("  {name}: {}", dead.join(", ")));
        }
    }
    assert!(
        problems.is_empty(),
        "embedded output carries unreachable declarations:\n{}",
        problems.join("\n")
    );
}

/// The capability gates are what let the shaker cut edges that exist in the
/// source but are never taken. If a gate stops working the symbol comes back,
/// so pin the cases that motivated each gate.
#[test]
fn capability_gates_keep_unused_runtime_out() {
    let ball = emitted("bouncy_ball", RuntimeMode::Embedded);
    for gone in ["lerpPath", "trimTable", "pathD", "rectPath", "starPath", "css"] {
        assert!(
            !ball.contains(&format!("function {gone}")),
            "bouncy_ball animates one transform but still ships `{gone}`"
        );
    }

    let logo = emitted("lottie-logo", RuntimeMode::Embedded);
    assert!(logo.contains("function trimTable"), "lottie-logo trims paths");
    assert!(
        !logo.contains("function bGradient"),
        "lottie-logo has no gradients but ships the gradient binder"
    );
}

/// Extern mode imports exactly the entry points a scene binds, so a bundler
/// sees a normal module graph instead of one aggregate driver.
#[test]
fn extern_output_imports_only_what_it_binds() {
    let ball = emitted("bouncy_ball", RuntimeMode::Extern);
    assert!(ball.contains("import { mount } from './runtime/core.js';"));
    assert!(ball.contains("import { bTransform } from './runtime/ops/tx.js';"));
    assert!(!ball.contains("driver.js"), "extern must not pull an aggregate driver");
    assert!(!ball.contains("bGradient"), "bouncy_ball binds no gradients");
}

/// Reachability is resolved on bare names, so two runtime modules must never
/// declare the same top-level identifier.
#[test]
fn runtime_top_level_names_are_globally_unique() {
    let mut seen: std::collections::HashMap<String, &str> = std::collections::HashMap::new();
    let mut clashes = Vec::new();
    for (module, src) in ulottie_compiler::runtime_modules() {
        for decl in shake::declarations(src) {
            if let Some(prev) = seen.insert(decl.name.clone(), module) {
                clashes.push(format!("  `{}` declared in {prev} and {module}", decl.name));
            }
        }
    }
    assert!(
        clashes.is_empty(),
        "runtime top-level names must be globally unique:\n{}",
        clashes.join("\n")
    );
}
