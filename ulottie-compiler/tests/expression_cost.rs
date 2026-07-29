//! What the expression machinery costs, per fixture.
//!
//! Not a gate — it prints. Run with `--nocapture` when deciding whether an AOT
//! expression stage is worth building, and to see whether one that lands has
//! actually removed what it was supposed to.
//!
//! Three separate costs, because they come off at different times:
//!
//! * the **runtime slice** an expression animation drags in that an otherwise
//!   identical one would not — the engine, the record decoder, and the layer
//!   accessors the bodies name;
//! * the **expression bodies** the module carries as JavaScript source;
//! * the **string table**, which used to exist almost entirely so a body could
//!   say `thisComp.layer('wire')` and `effect('Trace Path')`. Those are slots
//!   now, so what is left of it is what nothing resolved — run with

use std::fs;
use std::path::Path;

mod common;

/// Capability names that exist only to serve expressions.
const EXPR_CAPS: &[&str] =
    &["EXPRESSIONS", "EXPR_PROPERTY", "EXPR_COMP", "EXPR_PATH"];

#[test]
fn expression_machinery_cost() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../_fixtures/animations");
    let mut names: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| {
            let p = e.ok()?.path();
            (p.extension()? == "json").then(|| p.file_stem()?.to_str().map(String::from))?
        })
        .collect();
    names.sort();

    println!(
        "\n{:<20} {:>8} {:>8} {:>8} {:>8} {:>9} {:>7}  {}",
        "fixture", "module", "engine", "bodies", "strings", "overhead", "records", "strings are"
    );

    for name in names {
        let json = fs::read_to_string(dir.join(format!("{name}.json"))).unwrap();
        let opts = ulottie_compiler::CompileOptions {
            allow: common::allowances(&name),
            ..Default::default()
        };
        let Ok(report) = ulottie_compiler::compile_report(&json, &opts) else { continue };
        if !report.caps.iter().any(|c| c == "EXPRESSIONS") {
            continue;
        }

        // The slice this animation ships, against the slice it would ship if
        // nothing reached for an expression. The difference is the engine.
        //
        // The first half is the module's own figure rather than one derived
        // from its capabilities: the layer helpers a rewritten body calls are
        // shake roots, not capabilities, so measuring both sides by caps
        // reported `lyAt`, `lyPos` and the space walks as costing nothing.
        let without: Vec<String> = report
            .caps
            .iter()
            .filter(|c| !EXPR_CAPS.contains(&c.as_str()))
            .cloned()
            .collect();
        let engine =
            report.runtime_slice - ulottie_compiler::runtime_slice(&without).len();

        // Both measured on the shipped bytes. The bodies are real JavaScript
        // and the minifier does shrink them, so taking them off the unminified
        // build would overstate what an AOT stage removes.
        let bodies = bracketed(&report.js, "=[function(");
        let strings = bracketed(&report.js, "s:[`");

        // Names an expression looks up at runtime, in a module that already
        // knows what they resolve to. Templates also live in this table, so it
        // is only *mostly* expression overhead — hence the sample.
        let pretty = ulottie_compiler::compile_report(
            &json,
            &ulottie_compiler::CompileOptions { minify: false, ..opts.clone() },
        )
        .map(|r| r.js)
        .unwrap_or_default();
        let names = string_table(&pretty);
        let sample: Vec<String> = names
            .iter()
            .filter(|s| !s.starts_with('<'))
            .take(3)
            .map(|s| format!("{s:?}"))
            .collect();

        // The markers are shapes in minified output, so they are exactly the
        // kind of thing that stops matching quietly. A zero here is the
        // measurement breaking, not the overhead going away.
        assert!(engine > 0, "{name}: no engine in the slice, but EXPRESSIONS is set");
        assert!(bodies > 0, "{name}: expression bodies not found — `bracketed` marker is stale");
        // A string table can legitimately be gone now: once every lookup in
        // every body is an index, there is nothing left to name. So this only
        // holds the marker to account when the unminified build shows one.
        assert!(
            strings > 0 || names.is_empty(),
            "{name}: {} strings in the unminified build but none found in the shipped one \
             — `bracketed` marker is stale",
            names.len(),
        );

        let overhead = engine + bodies + strings;
        println!(
            "{:<20} {:>8} {:>8} {:>8} {:>8} {:>9} {:>7}  {}",
            name,
            report.js.len(),
            engine,
            bodies,
            strings,
            overhead,
            report.records,
            sample.join(", "),
        );
    }
    println!();
}

/// Byte length of the `[…]` literal `marker` opens, brackets balanced.
///
/// Reads the minified module, so the marker is a shape rather than a name —
/// the minifier renames every binding. A marker that stops matching prints 0,
/// which is the intended failure: a silently wrong number would be worse.
fn bracketed(src: &str, marker: &str) -> usize {
    let Some(start) = src.find(marker) else { return 0 };
    let open = src[start..].find('[').map(|i| start + i).unwrap();
    let mut depth = 0usize;
    for (i, c) in src[open..].char_indices() {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
    }
    0
}

/// The interned strings the payload carries.
fn string_table(pretty: &str) -> Vec<String> {
    let Some(i) = pretty.find("\"s\": [") else { return Vec::new() };
    let rest = &pretty[i + 5..];
    let Some(j) = rest.find("\n  ]") else { return Vec::new() };
    serde_json::from_str::<Vec<String>>(&rest[..j + 4].trim()).unwrap_or_default()
}
