//! Snapshot tests for the `reanimated-aot` target.
//!
//! Each MVP fixture compiles to a React Native module and is snapshotted as
//! `_fixtures/__snapshots__/<name>.rn.js` — the same directory the web
//! snapshots live in, so the vitest syntax check in
//! `ulottie-dev-server/tests/output.spec.ts` picks the files up automatically.
//!
//! A mismatched or missing snapshot fails; `ULOTTIE_BLESS=1` is the only thing
//! that writes one. Auto-writing a missing snapshot would let a deleted file
//! heal itself green, which is the opposite of what a snapshot test is for.

use std::fs;

use ulottie_compiler::support::Feature;
use ulottie_compiler::{compile_with, CompileOptions, Target};

fn fixture_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("_fixtures")
        .join("animations")
        .join(format!("{name}.json"))
}

fn snapshot_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("_fixtures")
        .join("__snapshots__")
        .join(format!("{name}.rn.js"))
}

fn compile_rn(name: &str, allow: &[Feature]) -> String {
    let json = fs::read_to_string(fixture_path(name))
        .unwrap_or_else(|_| panic!("missing fixture: {name}"));
    compile_with(
        &json,
        &CompileOptions {
            target: Target::ReanimatedAot,
            allow: allow.iter().copied().collect(),
            ..Default::default()
        },
    )
    .unwrap_or_else(|e| panic!("{name}: {e:#}"))
}

/// The module must be DOM-free and carry the RN export surface. Balanced
/// brackets catch the class of emitter bugs (an unclosed template, a stray
/// brace from the worklet injection) that a substring check misses.
fn check_hygiene(name: &str, js: &str) {
    for export in ["export const tree", "export const meta", "export const init"] {
        assert!(js.contains(export), "{name}: missing `{export}`");
    }
    assert!(js.contains("'worklet'"), "{name}: no worklet directive");
    // The runtime's comments legitimately *mention* DOM APIs ("without a
    // document.", "the order querySelectorAll would have produced"), so both
    // scans run on the code with line comments removed. The strip is
    // string-aware: `//` inside a string (the VLQ payload can hold slashes)
    // is data, and a prose apostrophe inside a comment is not a quote.
    let code = strip_line_comments(js);
    for forbidden in [
        "setAttribute",
        "innerHTML",
        "querySelector",
        "document.",
        ".style.",
        "requestAnimationFrame",
        "matchMedia",
        "createElementNS",
    ] {
        assert!(
            !code.contains(forbidden),
            "{name}: DOM leaked into the RN module: `{forbidden}`"
        );
    }
    // Bracket balance on the comment-stripped code, skipping string contents.
    let (mut paren, mut brace, mut bracket) = (0i64, 0i64, 0i64);
    let bytes = code.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' | b'"' | b'`' => {
                let q = bytes[i];
                i += 1;
                while i < bytes.len() && bytes[i] != q {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
            }
            b'(' => paren += 1,
            b')' => paren -= 1,
            b'{' => brace += 1,
            b'}' => brace -= 1,
            b'[' => bracket += 1,
            b']' => bracket -= 1,
            _ => {}
        }
        i += 1;
    }
    assert_eq!((paren, brace, bracket), (0, 0, 0), "{name}: unbalanced brackets");
}

/// Remove `// …` and `/* … */` comments, tracking string state so a slash
/// inside a string stays and comment prose (with its apostrophes) goes.
fn strip_line_comments(js: &str) -> String {
    let bytes = js.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            q @ (b'\'' | b'"' | b'`') => {
                out.push(bytes[i]);
                i += 1;
                while i < bytes.len() && bytes[i] != q {
                    out.push(bytes[i]);
                    if bytes[i] == b'\\' && i + 1 < bytes.len() {
                        out.push(bytes[i + 1]);
                        i += 1;
                    }
                    i += 1;
                }
                if i < bytes.len() {
                    out.push(bytes[i]);
                }
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
                continue;
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8(out).expect("comment strip is byte-preserving outside comments")
}

fn assert_snapshot(name: &str, allow: &[Feature]) {
    let js = compile_rn(name, allow);
    check_hygiene(name, &js);
    let path = snapshot_path(name);
    let bless = std::env::var_os("ULOTTIE_BLESS").is_some();
    match fs::read_to_string(&path) {
        Ok(existing) if existing == js => {}
        Ok(_) | Err(_) if bless => fs::write(&path, &js).unwrap(),
        Ok(existing) => {
            let at = existing
                .bytes()
                .zip(js.bytes())
                .position(|(a, b)| a != b)
                .unwrap_or(existing.len().min(js.len()));
            panic!(
                "{name}: snapshot mismatch at byte {at} ({}). \
                 Run with ULOTTIE_BLESS=1 to accept.",
                path.display()
            );
        }
        Err(e) => panic!(
            "{name}: no snapshot at {} ({e}). \
             Run with ULOTTIE_BLESS=1 to create it.",
            path.display()
        ),
    }
}

macro_rules! rn_snapshot {
    ($name:ident, $fixture:literal $(, allow: [$($f:expr),* $(,)?])?) => {
        #[test]
        fn $name() {
            assert_snapshot($fixture, &[$($($f),*)?]);
        }
    };
}

rn_snapshot!(rn_boucing_ball, "boucing_ball");
rn_snapshot!(rn_rectangle, "rectangle");
rn_snapshot!(rn_ellipse, "ellipse");
rn_snapshot!(rn_fill, "fill");
rn_snapshot!(rn_trim_path, "trim_path");
rn_snapshot!(rn_android_wave, "android_wave");
rn_snapshot!(rn_precomp_star_circle, "precomp_star_circle");
rn_snapshot!(rn_gradient_radial, "gradient_radial");
// `lottie_logo_1` carries one `tt: 2` layer — an inverted alpha matte, which
// the RN target refuses (see `Feature::TrackMatteInverted`). It stays in the
// MVP set with the degradation accepted explicitly, which is exactly what the
// `--allow` escape hatch is for: the matte still masks, it just does not
// invert.
rn_snapshot!(rn_lottie_logo_1, "lottie_logo_1", allow: [Feature::TrackMatteInverted]);
rn_snapshot!(rn_mask_subtract, "mask_subtract");
rn_snapshot!(rn_matte_alpha, "matte_alpha");
rn_snapshot!(rn_stroke_under_fill, "stroke_under_fill");
// Beyond the MVP set: these compile under the RN target today, so they pin the
// whitelist entries the twelve above never exercise (dashes, rects, luma
// mattes, skewed transforms, time remapping).
rn_snapshot!(rn_bodymoovin, "bodymoovin");
rn_snapshot!(rn_lottie_logo_2, "lottie_logo_2");
rn_snapshot!(rn_lottie_logo_3, "lottie_logo_3");
rn_snapshot!(rn_fireworks, "fireworks");
rn_snapshot!(rn_matte_luma, "matte_luma");

// ---------------------------------------------------------------------------
// The RN runtime forks
// ---------------------------------------------------------------------------

/// Every op the RN runtime forks, as (web original, RN fork, function name).
///
/// Three ops in `runtime/ops/` write the DOM directly instead of going through
/// `put`, so `runtime/rn/` carries a hand-copied twin of each with that one
/// write routed to the prop store. Nothing but this test stops the two from
/// drifting when the web original is edited.
const FORKS: &[(&str, &str, &str)] = &[
    ("ops/rect.js", "rn/rect.js", "oRect"),
    ("ops/display.js", "rn/display.js", "oDisplay"),
    ("ops/shape.js", "rn/shape.js", "trim"),
];

#[test]
fn rn_runtime_forks_match_their_web_originals() {
    let runtime = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("runtime");
    for (web, rn, func) in FORKS {
        let read = |rel: &str| {
            fs::read_to_string(runtime.join(rel))
                .unwrap_or_else(|e| panic!("runtime/{rel}: {e}"))
        };
        let a = normalize_writes(&extract_fn(&read(web), func, web));
        let b = normalize_writes(&extract_fn(&read(rn), func, rn));
        assert_eq!(
            a, b,
            "runtime/{rn} has drifted from runtime/{web} (`{func}`). The fork may \
             differ only in how it writes — `el.setAttribute(n, v)` and \
             `el.style.display = v` on the web become `rput(el, n, v)` there — so \
             port the rest of the edit across."
        );
    }
}

/// The body of `function <name>(…) { … }`, braces balanced, comments removed.
fn extract_fn(src: &str, name: &str, file: &str) -> String {
    let code = strip_line_comments(src);
    let needle = format!("function {name}(");
    let start = code
        .find(&needle)
        .unwrap_or_else(|| panic!("runtime/{file}: no `{needle}`"));
    let open = code[start..]
        .find('{')
        .unwrap_or_else(|| panic!("runtime/{file}: `{name}` has no body"))
        + start;
    let bytes = code.as_bytes();
    let mut depth = 0i32;
    for i in open..bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return code[start..=i].to_string();
                }
            }
            _ => {}
        }
    }
    panic!("runtime/{file}: `{name}` body never closes");
}

/// Collapse the intended DOM-write → prop-write substitution, so the two forks
/// become byte-identical wherever they are meant to be.
///
/// All three write forms fold into one canonical `WRITE(target, name, value)`:
/// `el.setAttribute(n, v)` and `rput(el, n, v)` already have that shape, and
/// `el.style.display = v` gains the implicit `'display'` name. Whitespace
/// collapses too, because the fork indents the same code under a comment of a
/// different length.
fn normalize_writes(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let bytes = src.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if src[i..].starts_with("rput(") {
            out.push_str("WRITE(");
            i += "rput(".len();
            continue;
        }
        if src[i..].starts_with(".setAttribute(") {
            let target = take_target(&mut out);
            out.push_str(&format!("WRITE({target}, "));
            i += ".setAttribute(".len();
            continue;
        }
        if src[i..].starts_with(".style.display = ") {
            let target = take_target(&mut out);
            let rest = &src[i + ".style.display = ".len()..];
            let end = rest.find(';').expect("a display assignment ends in `;`");
            out.push_str(&format!("WRITE({target}, 'display', {})", &rest[..end]));
            i += ".style.display = ".len() + end;
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    // One space between tokens, so indentation and line breaks stop mattering.
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Pull the just-emitted target expression (`el`, `E[i]`) back off the output,
/// so it can be re-emitted as the first argument of `WRITE`.
fn take_target(out: &mut String) -> String {
    let keep = out
        .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$' || c == '[' || c == ']'))
        .map(|p| p + 1)
        .unwrap_or(0);
    out.split_off(keep)
}
