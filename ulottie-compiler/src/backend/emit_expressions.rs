//! Compile each unique IR expression into a JS function the runtime dispatches
//! to.
//!
//! A Bodymovin-transpiled body runs against a fixed vocabulary — `value`,
//! `time`, `thisLayer`, `effect`, `sum`, `clamp`, `loopOut`, … — which used to
//! be introduced wholesale in front of every body. Most expressions touch two
//! or three of those names, and the minifier cannot drop the rest: the
//! `thisProperty?.key` reads are property accesses, which it must assume have
//! side effects.
//!
//! So the preamble is emitted per expression, from what the body references.

use crate::ir;

/// Names destructured from `ctx`.
const CTX_NAMES: &[&str] = &[
    "thisComp",
    "sum",
    "sub",
    "mul",
    "div",
    "clamp",
    "radiansToDegrees",
    "degreesToRadians",
    "createPath",
    "pointOnPath",
    "tangentOnPath",
];

pub fn emit_one(out: &mut String, expr: &ir::Expression) {
    let used = free_identifiers(&expr.body);
    let uses = |n: &str| used.iter().any(|u| u == n);

    out.push_str("  function(value, thisLayer, thisProperty, frame, ctx) {\n");

    let ctx_used: Vec<&str> = CTX_NAMES.iter().copied().filter(|n| uses(n)).collect();
    if !ctx_used.is_empty() {
        out.push_str(&format!(
            "    const {{ {} }} = ctx;\n",
            ctx_used.join(", ")
        ));
    }
    if uses("time") {
        out.push_str("    const time = frame / ctx.frameRate;\n");
    }
    if uses("effect") {
        out.push_str("    const effect = (n) => (thisLayer ? thisLayer.effect(n) : (() => 0));\n");
    }
    // A bare `fromCompToSurface(...)` in an AE expression means the current
    // layer's inverse transform.
    if uses("fromCompToSurface") {
        out.push_str(
            "    const fromCompToSurface = (pt) => (thisLayer ? thisLayer.fromCompToSurface(pt) : pt);\n",
        );
    }
    // The `thisProperty` surface. Each is stubbed when the property has no
    // keyframes, which is why they cannot simply be read off `thisProperty`.
    if uses("numKeys") {
        out.push_str("    const numKeys = thisProperty?.numKeys ?? 0;\n");
    }
    if uses("nearestKey") {
        out.push_str("    const nearestKey = thisProperty?.nearestKey ? thisProperty.nearestKey.bind(thisProperty) : ((t) => ({ index: 1, time: 0 }));\n");
    }
    if uses("key") {
        out.push_str("    const key = thisProperty?.key ? thisProperty.key.bind(thisProperty) : ((n) => ({ time: 0, value: 0, index: n }));\n");
    }
    if uses("valueAtTime") {
        out.push_str("    const valueAtTime = thisProperty?.valueAtTime ? thisProperty.valueAtTime.bind(thisProperty) : ((t) => 0);\n");
    }
    if uses("velocityAtTime") {
        out.push_str("    const velocityAtTime = thisProperty?.velocityAtTime ? thisProperty.velocityAtTime.bind(thisProperty) : ((t) => 0);\n");
    }
    // AE exposes `loopOut` as a free function equivalent to
    // `thisProperty.loopOut(...)`.
    if uses("loopOut") {
        out.push_str("    const loopOut = thisProperty?.loopOut ? thisProperty.loopOut.bind(thisProperty) : ((mode, n) => value);\n");
    }

    // Bodymovin bodies usually declare `$bm_rt` themselves; only add one when
    // the body does not, rather than emitting a redundant redeclaration.
    if !declares_bm_rt(&expr.body) {
        out.push_str("    var $bm_rt;\n");
    }

    for line in expr.body.lines() {
        out.push_str("    ");
        out.push_str(line);
        out.push('\n');
    }
    // Some bodies already `return $bm_rt;`; a duplicate is harmless because the
    // inner one wins.
    out.push_str("    return $bm_rt;\n");
    out.push_str("  },\n");
}

fn declares_bm_rt(body: &str) -> bool {
    body.lines().any(|l| {
        let t = l.trim_start();
        t.starts_with("var $bm_rt") || t.starts_with("let $bm_rt") || t.starts_with("const $bm_rt")
    })
}

/// Identifiers the body uses as free names.
///
/// String literals are skipped (AE property paths are full of words) and so are
/// member accesses: `path.pointOnPath(t)` needs no `pointOnPath` binding. Both
/// filters only ever remove candidates, and a missed candidate would drop a
/// binding the body needs — so the member-access check deliberately looks only
/// at the character immediately before the name, which cannot misfire on a
/// genuine free reference.
fn free_identifiers(body: &str) -> Vec<String> {
    let bytes: Vec<char> = body.chars().collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0usize;
    let mut quote: Option<char> = None;

    while i < bytes.len() {
        let c = bytes[i];
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
            while i < bytes.len()
                && (bytes[i].is_alphanumeric() || bytes[i] == '_' || bytes[i] == '$')
            {
                i += 1;
            }
            // Preceded by `.` → a member name, not a binding this scope owns.
            let preceded_by_dot = start > 0 && bytes[start - 1] == '.';
            if !preceded_by_dot {
                out.push(bytes[start..i].iter().collect());
            }
            continue;
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expr(body: &str) -> ir::Expression {
        ir::Expression {
            id: ir::ExprId(0),
            body: body.to_string(),
            canonical_hash: 0,
            used_apis: ir::ApiSet::empty(),
            uses_value: false,
            uses_this_property: false,
            uses_loop_out: false,
            references_layers: Vec::new(),
            references_effects: Vec::new(),
        }
    }

    fn emit(body: &str) -> String {
        let mut s = String::new();
        emit_one(&mut s, &expr(body));
        s
    }

    #[test]
    fn only_the_names_a_body_uses_are_bound() {
        let out = emit("$bm_rt = clamp(value, 0, 100);");
        assert!(out.contains("const { clamp } = ctx;"), "{out}");
        assert!(!out.contains("thisComp"), "{out}");
        assert!(!out.contains("valueAtTime"), "{out}");
        assert!(!out.contains("const time"), "{out}");
    }

    #[test]
    fn a_member_call_does_not_pull_in_the_free_binding() {
        // `pointOnPath` here is a method on a path object, not the ctx helper,
        // so the body keeps the call but no binding is introduced for it.
        let out = emit("$bm_rt = p.pointOnPath(0.5);");
        assert!(out.contains("p.pointOnPath(0.5)"), "{out}");
        assert!(!out.contains("= ctx;"), "no ctx destructure expected: {out}");
    }

    #[test]
    fn words_inside_string_literals_are_not_references() {
        let out = emit("$bm_rt = thisLayer.effect('Pseudo/Trace time key')('x');");
        assert!(!out.contains("const time"), "{out}");
        assert!(!out.contains("const key"), "{out}");
    }

    #[test]
    fn the_property_surface_is_bound_when_used() {
        let out = emit("$bm_rt = loopOut('cycle');");
        assert!(out.contains("const loopOut"), "{out}");
        assert!(!out.contains("const numKeys"), "{out}");
    }

    #[test]
    fn a_body_that_declares_its_own_result_slot_gets_no_second_one() {
        let out = emit("var $bm_rt;\n$bm_rt = 1;");
        assert_eq!(out.matches("var $bm_rt;").count(), 1, "{out}");
    }
}
