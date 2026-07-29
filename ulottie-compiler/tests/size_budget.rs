//! Output-size budgets.
//!
//! The whole point of the AOT stage is that output stays small, so "small" has
//! to be asserted, not just observed. Budgets are raw bytes of the
//! self-contained (embedded) module — compressor-independent, so they don't
//! shift when a gzip implementation changes.
//!
//! When a budget fails, the message prints the actual size. Lowering a budget
//! after a genuine win is expected and welcome; raising one should require a
//! reason in the commit message.

use std::fs;

use ulottie_compiler::{CompileOptions, RuntimeMode, compile_with};

mod common;

fn fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("_fixtures/animations")
        .join(format!("{name}.json"));
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn embedded(name: &str) -> String {
    compile_with(
        &fixture(name),
        &CompileOptions {
            runtime_mode: RuntimeMode::Embedded,
            allow: common::allowances(name),
            ..Default::default()
        },
    )
    .unwrap_or_else(|e| panic!("compile {name}: {e}"))
}

/// `(fixture, raw byte budget)`. Headroom is ~10% over the measured size.
///
/// Raised 2026-07-28 for `bouncy_ball`, `boucing-ball` and `lottie-logo`:
/// implementing time remap put a branch in `mount`, which every embedded module
/// inlines whether or not it remaps anything (~85 B), and track mattes give
/// `lottie-logo` a `<mask>` and an inversion `<filter>` it previously did not
/// draw at all. Both are new output for new capability, not drift.
const BUDGETS: &[(&str, usize)] = &[
    // Fully static: markup plus an inert player, no runtime at all.
    ("rectangle", 700),
    ("ellipse", 700),
    ("fill", 700),
    ("trim_path", 1_300),
    // One or two animated properties.
    //
    // Every fixture below except `ripple` is code-generated: no payload, no
    // binder table, no interpreter. `ripple` is the counter-example and the
    // reason the compiler builds both and keeps the smaller — 230 bindings
    // unroll to 151 KB against the interpreter's 52.
    ("bouncy_ball", 2_900),
    ("boucing-ball", 4_400),
    // `lottie-logo` moves little because two thirds of it is baked markup,
    // which the generator does not touch.
    ("precomp_star_circle", 8_800),
    ("lottie-logo", 14_000),
    ("starfish", 22_500),
    ("lights", 20_500),
    // ripple instances one precomp 46 times and the planner expands every
    // instance into the markup, so its element count — not its animation —
    // sets the size. Bringing this down needs precomp templating: emit the
    // subtree once and clone it at mount, before element indexing.
    ("ripple", 100_000),
];

#[test]
fn embedded_output_stays_within_budget() {
    let mut over = Vec::new();
    for (name, budget) in BUDGETS {
        let size = embedded(name).len();
        if size > *budget {
            over.push(format!("  {name}: {size} B > budget {budget} B"));
        }
    }
    assert!(
        over.is_empty(),
        "embedded output exceeded budget:\n{}",
        over.join("\n")
    );
}

/// Animations with no time-varying property must not carry a runtime at all —
/// no import, no data table, no animation-frame loop.
#[test]
fn static_animations_ship_no_runtime() {
    for name in ["rectangle", "ellipse", "fill", "trim_path"] {
        let js = embedded(name);
        assert!(!js.contains("import"), "{name} should be self-contained");
        assert!(
            !js.contains("requestAnimationFrame"),
            "{name} is fully static but still schedules frames"
        );
        assert!(js.contains("<svg"), "{name} should carry baked markup");
        assert!(
            js.contains("export const markup"),
            "{name} should export markup for SSR"
        );
    }
}

/// Capability gating is the mechanism that keeps the bundles small; if a
/// feature leaks into an animation that never uses it, sizes creep back.
#[test]
fn unused_capabilities_are_not_bundled() {
    let ball = embedded("bouncy_ball");
    assert!(
        !ball.contains("trimTable"),
        "bouncy_ball pulled in trim support"
    );
    assert!(
        !ball.contains("radialGradient"),
        "bouncy_ball pulled in gradient support"
    );

    let logo = embedded("lottie-logo");
    assert!(
        !logo.contains("radialGradient"),
        "lottie-logo pulled in gradient support"
    );
}
