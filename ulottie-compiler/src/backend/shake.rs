//! Symbol-level tree shaking over the runtime modules.
//!
//! Including a whole module because one of its functions is reachable is not
//! good enough for an AOT compiler: `num.js` alone carries three number
//! formatters and most animations use one. This resolves reachability per
//! top-level declaration instead, so what ships is the transitive closure of
//! what the scene actually binds.
//!
//! Two properties of the runtime sources make this cheap and safe:
//!
//! * top-level names are globally unique across `runtime/**`, so a
//!   declaration graph needs no module qualification (enforced by a test);
//! * references are found by scanning identifiers, which over-approximates —
//!   it can only ever retain too much, never drop something live.
//!
//! The scanner cannot tell an object-literal key from a reference, so a
//! property named after a top-level symbol keeps that symbol alive. Runtime
//! object keys are therefore named so they do not collide; the
//! `a_scene_only_carries_what_it_reaches` test is what catches a slip.
//!
//! The one thing scanning cannot see is a reference that is statically present
//! but dynamically unreachable: `kf.js` names `EASE` on a branch the planner
//! only ever takes when the easing capability is set. Those edges are declared
//! in [`GATED`] and cut when the capability is absent.

use std::collections::{BTreeMap, BTreeSet};

use crate::scene::Caps;

/// One top-level declaration, with the comment block that precedes it.
pub struct Decl {
    pub name: String,
    pub text: String,
    refs: BTreeSet<String>,
}

/// Edges that exist in the source but are only ever taken when a capability is
/// present. Cutting them is what lets a scene with no easing drop the bezier
/// solver even though `kf.js` mentions it.
const GATED: &[(&str, Caps)] = &[
    ("EASE", Caps::EASING),
    // Both halves of the motion-path sampler. These are named here by the
    // symbol, so renaming one silently un-gates it and every keyframed
    // animation starts carrying it again — which is exactly what happened when
    // `spatial` was split into `spBuild`/`spSample`.
    ("spBuild", Caps::SPATIAL),
    ("spSample", Caps::SPATIAL),
    ("spSeg", Caps::SPATIAL),
    ("lerpPath", Caps::PATH_KF),
    ("rectPath", Caps::GEOM_RECT),
    ("ellipsePath", Caps::GEOM_ELLIPSE),
    ("starPath", Caps::GEOM_STAR),
    // The shape ops name the trim helpers on a branch they only take when some
    // shape in the batch carries a trim modifier.
    ("trimTable", Caps::TRIM),
    ("trimApply", Caps::TRIM),
    ("trimCols", Caps::TRIM),
    ("trim", Caps::TRIM),
    // The chain composers, reached only when a binding's wire section carries
    // more than one trim step — which the planner accompanies with this bit.
    // Their `resolve` reference would otherwise drag the keyframe-handle
    // surface into every ordinarily-trimmed animation.
    ("trimChainCols", Caps::TRIM_CHAIN),
    ("trimChainWin", Caps::TRIM_CHAIN),
    ("expand", Caps::TEMPLATES),
    // Every op names `xcol` to pick up the expression-driven bindings in one of
    // its columns, and calls it only when there is an engine to hand them to.
    // Without this gate, one `oOpacity` would drag `resolve` and the whole
    // keyframe-handle surface into an animation that has no expressions at all.
    //
    // This is why each binder spells `x.expr ? xcol(…) : null` rather than
    // letting `xcol` return null for itself: cutting the edge removes the
    // *declaration*, so the guard at the call site is what keeps a module
    // without expressions from calling a name it does not carry. Folding those
    // 32 ternaries away looks like a clean ~380 bytes and breaks every
    // animation that has no expressions.
    ("xcol", Caps::EXPRESSIONS),
    // The record-offset column, decoded by `mount` only when an engine is there
    // to read the table it indexes.
    ("column", Caps::EXPRESSIONS),
    // The expression runtime, cut to what the bodies name. A resolved body
    // reports the symbols it calls exactly, in `Plan::helpers`, and those enter
    // `roots()` directly; these gates cover the ones the runtime names on a
    // branch it only takes for some animations. Without them an animation that
    // only calls `loopOut` still carries comp-space transforms and the
    // arc-length path sampler.
    //
    // `thisPropertyFor` was here too, and it is the reason this list is
    // dangerous. `evalExpr` called it to build every body's third argument —
    // not on a branch, on the only path — so cutting the edge shipped a module
    // that named a declaration it did not carry. `evalExpr` catches what the
    // body throws, so every expression in the animation quietly became its
    // authored constant, and the pixel gates saw a `ReferenceError` per
    // expression as "renders identically". It is a root now, reported by the
    // body that builds the surface. An entry belongs here only if the call site
    // is genuinely guarded, the way `xcol` is.
    ("toComp", Caps::EXPR_COMP),
    ("fromCompToSurface", Caps::EXPR_COMP),
    ("pointOnPath", Caps::EXPR_PATH),
    ("tangentOnPath", Caps::EXPR_PATH),
    ("createPath", Caps::EXPR_PATH),
];

fn is_cut(name: &str, caps: Caps) -> bool {
    GATED
        .iter()
        .any(|(sym, cap)| *sym == name && !caps.contains(*cap))
}

/// Split a module's source into top-level declarations.
///
/// Runtime modules are formatted so that every top-level declaration starts in
/// column 0 and everything belonging to it is indented (or is its closing
/// brace). Anything else in column 0 is a comment, a blank line, or module
/// syntax.
pub fn declarations(src: &str) -> Vec<Decl> {
    let mut out: Vec<Decl> = Vec::new();
    let mut pending = String::new();
    let mut current: Option<(String, String)> = None;

    let flush = |cur: Option<(String, String)>, out: &mut Vec<Decl>| {
        if let Some((name, text)) = cur {
            let refs = identifiers(&text, &name);
            out.push(Decl { name, text, refs });
        }
    };

    for line in src.lines() {
        if let Some(name) = declaration_name(line) {
            flush(current.take(), &mut out);
            let mut text = std::mem::take(&mut pending);
            text.push_str(strip_export(line));
            text.push('\n');
            current = Some((name, text));
            continue;
        }
        let trimmed = line.trim_start();
        if trimmed.starts_with("import ") || trimmed.starts_with("export {") {
            continue;
        }
        match &mut current {
            Some((_, text)) => {
                text.push_str(line);
                text.push('\n');
            }
            // Before the first declaration: file header comments and blanks.
            None => {
                if trimmed.is_empty()
                    || trimmed.starts_with("//")
                    || trimmed.starts_with('*')
                    || trimmed.starts_with("/*")
                {
                    pending.push_str(line);
                    pending.push('\n');
                } else {
                    debug_assert!(
                        false,
                        "runtime modules may only hold declarations at top level: {line}"
                    );
                }
            }
        }
    }
    flush(current, &mut out);

    // A trailing comment block belongs to nothing; drop it.
    out
}

/// `export function foo(` / `const foo =` → `foo`, when in column 0.
fn declaration_name(line: &str) -> Option<String> {
    if line.starts_with(char::is_whitespace) {
        return None;
    }
    let rest = strip_export(line);
    let rest = rest
        .strip_prefix("function ")
        .or_else(|| rest.strip_prefix("const "))
        .or_else(|| rest.strip_prefix("let "))
        .or_else(|| rest.strip_prefix("var "))
        .or_else(|| rest.strip_prefix("class "))?;
    let name: String = rest
        .trim_start()
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
        .collect();
    if name.is_empty() { None } else { Some(name) }
}

fn strip_export(line: &str) -> &str {
    line.strip_prefix("export ").unwrap_or(line)
}

/// Identifiers referenced by a declaration's body. Whole-line comments are
/// skipped so a doc comment naming a symbol does not keep it alive.
fn identifiers(text: &str, own: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in text.lines() {
        let t = line.trim_start();
        if t.starts_with("//") || t.starts_with('*') || t.starts_with("/*") {
            continue;
        }
        let mut word = String::new();
        // A word directly preceded by `.` is a property, not a reference to a
        // top-level binding — `ext.r` must not pull in num.js's `r`. Nothing
        // else distinguishes them, and getting it wrong ships a whole module
        // to every animation.
        let mut member = false;
        let mut prev = '\0';
        for ch in line.chars() {
            if ch.is_alphanumeric() || ch == '_' || ch == '$' {
                if word.is_empty() {
                    member = prev == '.';
                }
                word.push(ch);
            } else if !word.is_empty() {
                let w = std::mem::take(&mut word);
                if !member && w != own && !w.starts_with(|c: char| c.is_ascii_digit()) {
                    out.insert(w);
                }
            }
            prev = ch;
        }
        if !word.is_empty() && !member && word != own {
            out.insert(word);
        }
    }
    out
}

/// Retain the declarations reachable from `roots`, in their original order.
pub fn shake(decls: Vec<Decl>, roots: &[&str], caps: Caps) -> Vec<Decl> {
    let index: BTreeMap<&str, usize> = decls
        .iter()
        .enumerate()
        .map(|(i, d)| (d.name.as_str(), i))
        .collect();

    let mut live = vec![false; decls.len()];
    let mut stack: Vec<usize> = roots.iter().filter_map(|r| index.get(r).copied()).collect();
    for i in &stack {
        live[*i] = true;
    }
    while let Some(i) = stack.pop() {
        for r in &decls[i].refs {
            if is_cut(r, caps) {
                continue;
            }
            if let Some(&j) = index.get(r.as_str())
                && !live[j] {
                    live[j] = true;
                    stack.push(j);
                }
        }
    }

    decls
        .into_iter()
        .zip(live)
        .filter_map(|(d, keep)| keep.then_some(d))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "\
// header
export function a(x) {
  return b(x);
}

/** doc for b */
function b(x) {
  return x + 1;
}

export function unused() {
  return b(0);
}
";

    #[test]
    fn declarations_split_on_column_zero() {
        let d = declarations(SRC);
        assert_eq!(
            d.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(),
            ["a", "b", "unused"]
        );
        assert!(d[0].text.contains("return b(x);"));
        // The `export` keyword is dropped; the body is intact.
        assert!(d[0].text.starts_with("// header\nfunction a(x) {"));
    }

    #[test]
    fn unreachable_declarations_are_dropped() {
        let kept = shake(declarations(SRC), &["a"], Caps::empty());
        assert_eq!(
            kept.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
    }

    #[test]
    fn a_comment_mentioning_a_symbol_does_not_keep_it_alive() {
        let src = "\
export function a() {\n  // calls b() one day\n  return 1;\n}\nfunction b() { return 2; }\n";
        let kept = shake(declarations(src), &["a"], Caps::empty());
        assert_eq!(
            kept.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(),
            ["a"]
        );
    }

    #[test]
    fn gated_edges_are_cut_when_the_capability_is_absent() {
        let src = "\
export function keyframed() {\n  return EASE(1);\n}\nfunction EASE(x) { return x; }\n";
        let without = shake(declarations(src), &["keyframed"], Caps::empty());
        assert_eq!(without.len(), 1, "EASE must be cut without the easing cap");
        let with = shake(declarations(src), &["keyframed"], Caps::EASING);
        assert_eq!(with.len(), 2, "EASE must survive when easing is used");
    }
}
