//! Output hygiene: an AOT compiler should not emit code the animation cannot
//! reach.
//!
//! The bundler resolves reachability per top-level declaration, so a fixture
//! that never animates a path must not carry the path interpolator and one that
//! never formats a plain coordinate must not carry that formatter. This asserts
//! it on the real emitted module rather than trusting the minifier.

use std::fs;

use ulottie_compiler::backend::shake;
use ulottie_compiler::{CompileOptions, MarkupMode, RuntimeMode, compile_with};

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
    emitted_as(name, mode, MarkupMode::Inline)
}

fn emitted_as(name: &str, mode: RuntimeMode, markup: MarkupMode) -> String {
    compile_as(name, mode, markup, false)
}

fn compile_as(name: &str, mode: RuntimeMode, markup: MarkupMode, minify: bool) -> String {
    compile_with(
        &source(name),
        &CompileOptions {
            runtime_mode: mode,
            markup,
            minify,
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
    let live = shake::shake(
        all,
        &["markup", "init"],
        ulottie_compiler::scene::Caps::all(),
    );
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
/// The module that hydrates a served document (`--no-markup`) carries nothing
/// the page already has: no markup, no template table (the served document is
/// the expanded tree), no sprite wiring — and, as shipped, is smaller than the
/// inline module for every fixture in both runtime modes. (Measured minified:
/// unminified, the id walker's comments outweigh a small document.) It still
/// has to be free of unreachable declarations, and it has to declare the one
/// thing it adds: the id walker, whenever the scene defines ids, because each
/// adopted mount rewrites the served tree's marker for itself.
#[test]
fn a_no_markup_module_carries_no_markup() {
    let mut problems = Vec::new();
    for name in fixture_names() {
        for mode in [RuntimeMode::Extern, RuntimeMode::Embedded] {
            let inline = emitted_as(&name, mode, MarkupMode::Inline);
            let served = emitted_as(&name, mode, MarkupMode::None);
            let tag = format!("{name} ({mode:?})");
            // Markup, however it is spelled: a `viewBox` (the document's root)
            // or the module-level `const M` (at column 0 — the runtime has
            // locals of that name). The one `<svg>` a served module may say
            // is in its "nothing to hydrate" message.
            for needle in [
                "viewBox",
                "\nconst M =",
                "\nconst M=",
                "TPL",
                "spriteSymbol",
                "fromSprite",
            ] {
                if served.contains(needle) {
                    problems.push(format!("  {tag}: carries `{}`", needle.trim()));
                }
            }
            if served.contains("export const markup") {
                problems.push(format!("  {tag}: exports `markup` it does not have"));
            }
            let shipped = |markup| compile_as(&name, mode, markup, true).len();
            let (inline_min, served_min) = (shipped(MarkupMode::Inline), shipped(MarkupMode::None));
            if served_min >= inline_min {
                problems.push(format!(
                    "  {tag}: {served_min} B minified is not smaller than the inline module's {inline_min} B"
                ));
            }
            let defines_ids = inline.contains("--u");
            if mode == RuntimeMode::Extern && defines_ids != served.contains("suffixIds") {
                problems.push(format!(
                    "  {tag}: defines ids = {defines_ids}, imports the id walker = {}",
                    !defines_ids
                ));
            }
            if mode == RuntimeMode::Extern && served.contains("tpl.js") {
                problems.push(format!(
                    "  {tag}: imports template expansion for an expanded document"
                ));
            }
            if mode == RuntimeMode::Embedded {
                let dead = dead_declarations(&served);
                if !dead.is_empty() {
                    problems.push(format!("  {tag}: unreachable {}", dead.join(", ")));
                }
            }
        }
    }
    assert!(
        problems.is_empty(),
        "no-markup modules carry what the page already has:\n{}",
        problems.join("\n")
    );
}

/// The served document keeps the per-mount id marker in place. The hydrating
/// module rewrites it per adopted mount, which is what keeps two served copies
/// of one animation from pointing into each other's gradients and masks;
/// resolved to a fixed suffix here, every served copy would be identical and
/// nothing downstream could tell them apart again.
#[test]
fn the_document_keeps_its_id_marker() {
    // `ripple` defines gradients, `starfish` masks, `lottie_logo_1` a matte.
    for name in ["ripple", "starfish", "lottie_logo_1"] {
        let doc = ulottie_compiler::compile_document(&source(name))
            .unwrap_or_else(|e| panic!("document {name}: {e}"));
        let refs: Vec<&str> = doc
            .match_indices("url(#")
            .map(|(i, _)| &doc[i..doc[i..].find(')').map_or(doc.len(), |e| i + e)])
            .collect();
        assert!(
            !refs.is_empty(),
            "{name}: expected id references in the document"
        );
        for r in &refs {
            assert!(
                r.ends_with("--u"),
                "{name}: `{r})` lost its per-mount marker"
            );
        }
        assert!(
            doc.contains("--u\""),
            "{name}: no id definition carries the marker"
        );
    }
}

#[test]
fn capability_gates_keep_unused_runtime_out() {
    let ball = emitted("boucing_ball", RuntimeMode::Embedded);
    for gone in [
        "lerpPath",
        "trimTable",
        "pathD",
        "rectPath",
        "starPath",
        "css",
    ] {
        assert!(
            !ball.contains(&format!("function {gone}")),
            "boucing_ball animates one transform but still ships `{gone}`"
        );
    }

    let logo = emitted("lottie_logo_1", RuntimeMode::Embedded);
    assert!(
        logo.contains("function trimTable"),
        "lottie_logo_1 trims paths"
    );
    assert!(
        !logo.contains("function oGradient"),
        "lottie_logo_1 has no gradients but ships the gradient binder"
    );
}

/// Extern mode imports exactly the entry points a scene binds, so a bundler
/// sees a normal module graph instead of one aggregate driver.
#[test]
fn extern_output_imports_only_what_it_binds() {
    let ball = emitted("boucing_ball", RuntimeMode::Extern);
    assert!(ball.contains("import { mount } from './runtime/core.js';"));
    assert!(ball.contains("import { bTransform, oTransform } from './runtime/ops/tx.js';"));
    assert!(
        !ball.contains("driver.js"),
        "extern must not pull an aggregate driver"
    );
    assert!(
        !ball.contains("oGradient"),
        "boucing_ball binds no gradients"
    );
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

/// Every free name an expression body calls must be something the module
/// actually declares.
///
/// The bodies are text the shaker cannot follow: it resolves references between
/// runtime declarations, and `E[]` is not one of them. What keeps the two in
/// step is the layer pass reporting the helpers it wrote, which then go in as
/// shake roots. Getting that wrong ships a module that throws the first time an
/// expression runs, at a frame no pixel test may sample — so it is checked here
/// rather than left to the corpus to notice.
#[test]
fn expression_bodies_only_call_what_the_module_declares() {
    for name in fixture_names() {
        let src = emitted(&name, RuntimeMode::Embedded);
        let Some(table) = src.split_once("const E = [") else {
            continue;
        };
        let table = &table.1[..table
            .1
            .find("\n];")
            .expect("expression table is terminated")];

        // Everything the module declares, plus what a body legitimately binds
        // for itself: the function's own parameters, and the names the preamble
        // introduces (all of which are `const`/`var` lines inside the body).
        let mut known: std::collections::BTreeSet<String> = shake::declarations(&src)
            .iter()
            .map(|d| d.name.clone())
            .collect();
        known.extend(
            [
                "value",
                "thisLayer",
                // The property handle. `thisProperty` is no longer a parameter:
                // building the view is what pulls `thisPropertyFor` in, so the
                // preamble does it — and a `const` line is already known.
                "$p",
                "frame",
                "ctx",
                "Math",
                "Array",
                "console",
            ]
            .map(String::from),
        );
        for line in table.lines() {
            let t = line.trim_start();
            // `catch (e$$4)` binds a name too, and Bodymovin emits one.
            if let Some(rest) = t.split_once("catch (")
                && let Some((binding, _)) = rest.1.split_once(')')
            {
                known.insert(binding.trim().to_string());
            }
            // Arrow parameters: the preamble stubs are all arrows, and
            // `((mode, n) => value)` binds two names in passing.
            for (at, _) in t.match_indices("=>") {
                let before = t[..at].trim_end();
                if let Some(open) = before.rfind('(') {
                    for w in before[open..]
                        .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
                    {
                        if !w.is_empty() {
                            known.insert(w.to_string());
                        }
                    }
                }
            }
            // Anywhere in the line, not just at its start: `for (var i = 0; …)`
            // declares `i` mid-statement, and Bodymovin writes whole bodies on
            // one line.
            for kw in ["const ", "var ", "let ", "function "] {
                for (at, _) in t.match_indices(kw) {
                    let rest = &t[at + kw.len()..];
                    // `const { a, b } = ctx;` and `const x = …` alike.
                    let head = rest.split(['=', ';']).next().unwrap_or("");
                    for w in head.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$')) {
                        if !w.is_empty() {
                            known.insert(w.to_string());
                        }
                    }
                }
            }
        }

        for word in free_names(table) {
            assert!(
                known.contains(&word),
                "{name}: expression body calls `{word}`, which the module does not declare"
            );
        }
    }
}

/// Identifier-shaped words that are neither member accesses, string contents,
/// object keys, nor labels. Only ever drops candidates, so a miss here is a
/// weaker assertion rather than a false failure.
fn free_names(src: &str) -> std::collections::BTreeSet<String> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = std::collections::BTreeSet::new();
    let mut quote: Option<char> = None;
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if let Some(q) = quote {
            if c == '\\' {
                i += 2;
                continue;
            }
            if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if c == '\'' || c == '"' || c == '`' {
            quote = Some(c);
            i += 1;
            continue;
        }
        if c.is_alphabetic() || c == '_' || c == '$' {
            let start = i;
            while i < chars.len()
                && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '$')
            {
                i += 1;
            }
            let after = chars[i..].iter().find(|c| !c.is_whitespace());
            let is_member = start > 0 && chars[start - 1] == '.';
            // `{ key: …}` and `case x:` — a colon after the word means it is
            // not a reference to a binding.
            let is_key = after == Some(&':');
            if !is_member && !is_key {
                let w: String = chars[start..i].iter().collect();
                // Keywords are not references.
                const KW: &[&str] = &[
                    "var",
                    "let",
                    "const",
                    "function",
                    "return",
                    "if",
                    "else",
                    "for",
                    "while",
                    "try",
                    "catch",
                    "finally",
                    "new",
                    "typeof",
                    "in",
                    "of",
                    "true",
                    "false",
                    "null",
                    "undefined",
                    "this",
                    "throw",
                    "switch",
                    "case",
                    "break",
                    "continue",
                    "do",
                    "delete",
                    "void",
                    "instanceof",
                ];
                if !KW.contains(&w.as_str()) {
                    out.insert(w);
                }
            }
            continue;
        }
        i += 1;
    }
    out
}

/// Every fixture's expressions resolve.
///
/// There is no fallback: a layer reference the compiler cannot resolve fails
/// the compile. So the gate is simply that the corpus still compiles — a
/// fixture appearing here is the layer pass having lost ground, and
/// `ULOTTIE_WHY=1` names the construct that defeated it.
#[test]
fn every_fixture_resolves_its_layer_references() {
    for name in fixture_names() {
        ulottie_compiler::compile_report(
            &source(&name),
            &CompileOptions {
                allow: common::allowances(&name),
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("compile {name}: {e}"));
    }
}

/// A generated module must carry every spatial tangent the payload does.
///
/// `codegen` has two forms for a keyframed property: an unrolled if-chain, and
/// — above `UNROLL_MAX` segments, or whenever the expression engine needs a
/// handle to hang keyframes off — a columnar literal sampled by `kfEval`. The
/// unrolled form has always emitted tangents; the columnar one silently did
/// not, so a motion path became a straight line. It is invisible in the source:
/// the property still animates, just along the wrong curve, and on `starfish`
/// it moved a limb 10 px at mid-animation while the extern build — reading the
/// same tangents off the stream — was exact.
#[test]
fn a_generated_module_keeps_its_spatial_motion_paths() {
    for name in fixture_names() {
        let report = ulottie_compiler::compile_report(
            &source(&name),
            &CompileOptions {
                allow: common::allowances(&name),
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("compile {name}: {e}"));
        if !report.generated || !report.caps.iter().any(|c| c == "SPATIAL") {
            continue;
        }
        let js = emitted(&name, RuntimeMode::Embedded);
        // Either form is fine — `spBuild` is what both reach for, and a scene
        // whose only spatial segments are all-zero tangents needs neither.
        assert!(
            js.contains("spBuild") || js.contains("spSample"),
            "{name}: generated with SPATIAL but no arc-length sampler in the output"
        );
    }
}
