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

use crate::scene::Caps;

use super::layers::{Plan, Surface};

/// Which parts of the expression vocabulary a module's bodies actually reach.
///
/// The same scan that trims each body's preamble, applied across the module and
/// turned into capabilities so the shaker can cut the runtime to match. A body
/// that only calls `loopOut` needs the `thisProperty` surface and nothing else
/// — no comp-space transforms, no arc-length path sampler.
///
/// This runs on the *rewritten* bodies, so the word lists have to name what the
/// layer pass emits as well as what After Effects wrote. It is now a backstop
/// rather than the mechanism: a rewritten body reports the symbols it calls
/// exactly, in `Plan::helpers`, and those go straight into the shake roots.
pub fn vocabulary(bodies: &[String]) -> Caps {
    const PROPERTY: &[&str] = &[
        "thisProperty",
        "numKeys",
        "key",
        "nearestKey",
        "valueAtTime",
        "velocityAtTime",
        "loopOut",
    ];
    const COMP: &[&str] = &["thisComp", "toComp", "fromCompToSurface"];
    const PATH: &[&str] = &[
        "createPath",
        "pointOnPath",
        "tangentOnPath",
        "points",
        "inTangents",
        "outTangents",
        "isClosed",
        "lyPath",
        "lyPoints",
        "lyClosed",
    ];
    // Deliberately *not* `free_identifiers`: that one excludes member accesses,
    // because `path.pointOnPath(t)` needs no free binding in the preamble. The
    // runtime still has to provide the method, so this scan counts every
    // mention. Over-retaining costs bytes; under-retaining ships a module that
    // throws only when the expression runs.
    let mut caps = Caps::empty();
    for body in bodies {
        for name in mentions(body) {
            if PROPERTY.contains(&name.as_str()) {
                caps |= Caps::EXPR_PROPERTY;
            }
            if COMP.contains(&name.as_str()) {
                caps |= Caps::EXPR_COMP;
            }
            if PATH.contains(&name.as_str()) {
                caps |= Caps::EXPR_PATH;
            }
        }
    }
    caps
}

/// Every identifier-shaped word in a body, members included.
fn mentions(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    for ch in body.chars() {
        if ch.is_alphanumeric() || ch == '_' || ch == '$' {
            word.push(ch);
        } else if !word.is_empty() {
            out.push(std::mem::take(&mut word));
        }
    }
    if !word.is_empty() {
        out.push(word);
    }
    out
}

/// Helper names bodies call by bare reference, to add as shake roots.
///
/// The rewrite reports the names it introduces via `need()`, but bodies also
/// call utility functions that pass through untouched — `clamp`, `sum`,
/// `createPath`, …. Scanning the shipped bodies for the full vocabulary catches
/// both, uniformly and exactly.
pub fn bare_helpers(bodies: &[String]) -> Vec<&'static str> {
    const HELPERS: &[&str] = &[
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
    let mut found: Vec<&'static str> = Vec::new();
    for body in bodies {
        for name in free_identifiers(body) {
            if let Some(h) = HELPERS.iter().find(|s| **s == name.as_str()) {
                if !found.contains(h) {
                    found.push(h);
                }
            }
        }
    }
    found
}

/// Emit one body, with the preamble its (already rewritten) text needs.
///
/// `thisLayer` is the layer *record*. A body the layer pass resolved reads it
/// through free functions; one it cannot resolve fails the compile, which is
/// the old proxy and delegates to those same functions.
///
/// Returns whether the body ended up naming `thisPropertyFor`, so the caller
/// can add it to the shake roots. That is the whole gate: `evalExpr` hands the
/// body the property *handle*, and the surface is built here or not at all, so
/// "the emitted text calls it" and "the declaration ships" are the same fact
/// rather than two lists that have to agree.
pub fn emit_one(dst: &mut String, body: &str, plan: &Plan) -> bool {
    let used = free_identifiers(body);
    let uses = |n: &str| used.iter().any(|u| u == n);

    // Built before it is written, because whether any of it reads
    // `thisProperty` is what decides if the view is materialized — and the
    // binding for that has to come out ahead of the reads.
    let mut pre = String::new();
    let out = &mut pre;

    if uses("time") {
        out.push_str("    const time = frame / ctx.frameRate;\n");
    }
    // The division, not a baked decimal: the frame rate is already a float on
    // the wire and round-tripping it through a literal would not always land
    // on the same bits.
    if plan.frame_duration {
        out.push_str("    const frameDuration = 1 / ctx.frameRate;\n");
    }
    // The `thisProperty` surface.
    //
    // `thisProperty` is always an object — `evalExpr` builds one for every call
    // — and none of its methods reads `this`, so neither the `?.` nor a
    // `.bind()` was ever load-bearing. The bind was also *per evaluation*: the
    // preamble is inside the body, so every frame allocated one bound function
    // per accessor.
    //
    // Which accessors exist is decided by the value source, which the planner
    // resolved long ago — see [`Surface`]. A body applied to a path property
    // gets the stub written out; one applied to anything else reads the method
    // directly. The `||` survives only where the uses disagree, which is the
    // same rule the layer pass folds by.
    let keys = plan.surface.map(Surface::has_keys);
    let mut accessor = |name: &str, stub: &str| {
        if !uses(name) {
            return;
        }
        out.push_str(&match keys {
            Some(true) => format!("    const {name} = thisProperty.{name};\n"),
            Some(false) => format!("    const {name} = {stub};\n"),
            None => format!("    const {name} = thisProperty.{name} || ({stub});\n"),
        });
    };
    accessor("nearestKey", "(t) => ({ index: 1, time: 0 })");
    accessor("key", "(n) => ({ time: 0, value: 0, index: n })");
    accessor("valueAtTime", "(t) => 0");
    accessor("velocityAtTime", "(t) => 0");
    // AE exposes `loopOut` as a free function equivalent to
    // `thisProperty.loopOut(...)`.
    accessor("loopOut", "(mode, n) => value");
    // `numKeys` is defined on every shape, and is zero on all but the keyed
    // one — so it is a literal whenever the uses agree it is not keyed.
    if uses("numKeys") {
        out.push_str(match plan.surface {
            Some(Surface::Keyed) | None => "    const numKeys = thisProperty.numKeys;\n",
            Some(_) => "    const numKeys = 0;\n",
        });
    }

    // The third parameter is the property handle. A body that wants the
    // `thisProperty` surface — its own reads, or an accessor the preamble
    // folded off it — binds it here; one that does not never names the builder,
    // and the shaker drops it along with the three views it constructs.
    let surface = uses("thisProperty") || pre.contains("thisProperty");
    dst.push_str("  function(value, thisLayer, $p, frame, ctx) {\n");
    if surface {
        dst.push_str("    const thisProperty = thisPropertyFor(ctx, $p);\n");
    }
    dst.push_str(&pre);

    // Bodymovin bodies usually declare `$bm_rt` themselves; only add one when
    // the body does not, rather than emitting a redundant redeclaration.
    if !declares_bm_rt(body) {
        dst.push_str("    var $bm_rt;\n");
    }

    for line in body.lines() {
        dst.push_str("    ");
        dst.push_str(line);
        dst.push('\n');
    }
    // Some bodies already `return $bm_rt;`; a duplicate is harmless because the
    // inner one wins.
    dst.push_str("    return $bm_rt;\n");
    dst.push_str("  },\n");
    surface
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

    /// A resolved body — the only kind there is.
    fn resolved() -> Plan {
        Plan {
            body: String::new(),
            helpers: Default::default(),
            frame_duration: false,
            // Keyed is what the corpus is; the other two are covered below.
            surface: Some(Surface::Keyed),
        }
    }

    fn emit_with(body: &str, surface: Option<Surface>) -> String {
        let mut out = String::new();
        emit_one(
            &mut out,
            body,
            &Plan {
                surface,
                ..resolved()
            },
        );
        out
    }

    /// The accessors are read straight off `thisProperty` when every property
    /// using the body agrees it has them, replaced by the stub when they all
    /// agree it does not, and probed only when they disagree.
    ///
    /// Only the first of the three occurs in the corpus, so the other two would
    /// otherwise ship unexercised.
    #[test]
    fn the_property_surface_folds_to_what_the_uses_agree_on() {
        let body = "$bm_rt = key(nearestKey(time).index);";

        let keyed = emit_with(body, Some(Surface::Keyed));
        assert!(keyed.contains("const key = thisProperty.key;"), "{keyed}");
        assert!(
            !keyed.contains("||"),
            "a settled surface needs no probe: {keyed}"
        );

        // A path property has the geometry accessors and none of these.
        let path = emit_with(body, Some(Surface::Path));
        assert!(
            path.contains("const key = (n) => ({ time: 0, value: 0, index: n });"),
            "{path}"
        );
        assert!(!path.contains("thisProperty.key"), "{path}");

        // Deduplicated across properties that disagree: keep the probe.
        let mixed = emit_with(body, None);
        assert!(
            mixed.contains("const key = thisProperty.key || ((n) =>"),
            "{mixed}"
        );
    }

    /// `numKeys` is defined on all three shapes, and zero on all but the keyed
    /// one — so it is a literal wherever the uses agree it is not keyed.
    #[test]
    fn num_keys_folds_to_a_literal_off_the_keyed_surface() {
        let body = "$bm_rt = numKeys;";
        assert!(
            emit_with(body, Some(Surface::Keyed)).contains("const numKeys = thisProperty.numKeys;")
        );
        assert!(emit_with(body, Some(Surface::Stub)).contains("const numKeys = 0;"));
        assert!(emit_with(body, Some(Surface::Path)).contains("const numKeys = 0;"));
        assert!(emit_with(body, None).contains("const numKeys = thisProperty.numKeys;"));
    }

    /// Nothing in the preamble may probe or rebind: `thisProperty` is always an
    /// object, and none of its methods reads `this`. The bind was also once per
    /// *evaluation*, since the preamble lives inside the body.
    #[test]
    fn the_preamble_neither_probes_nor_binds() {
        let body = "$bm_rt = loopOut('cycle') + valueAtTime(0) + velocityAtTime(0) + numKeys;";
        for surface in [
            Some(Surface::Keyed),
            Some(Surface::Stub),
            Some(Surface::Path),
            None,
        ] {
            let out = emit_with(body, surface);
            assert!(!out.contains(".bind("), "{surface:?}: {out}");
            assert!(!out.contains("thisProperty?."), "{surface:?}: {out}");
        }
    }

    fn emit(body: &str) -> String {
        let mut s = String::new();
        emit_one(&mut s, body, &resolved());
        s
    }

    #[test]
    fn a_body_calls_utils_by_bare_name_not_via_ctx() {
        let out = emit("$bm_rt = clamp(value, 0, 100);");
        assert!(out.contains("clamp(value, 0, 100)"), "{out}");
        assert!(
            !out.contains("ctx."),
            "clamp is a bare top-level call, not a ctx property: {out}"
        );
    }

    #[test]
    fn a_member_call_does_not_pull_in_the_free_binding() {
        // `pointOnPath` here is a method on a path object, not the ctx helper,
        // so the body keeps the call but no binding is introduced for it.
        let out = emit("$bm_rt = p.pointOnPath(0.5);");
        assert!(out.contains("p.pointOnPath(0.5)"), "{out}");
        assert!(
            !out.contains("= ctx;"),
            "no ctx destructure expected: {out}"
        );
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

    #[test]
    fn a_resolved_body_gets_no_lookup_preamble() {
        // What the layer pass produces: `thisLayer` stays the record, and the
        // helpers are called by bare name. Nothing is bound in front of it.
        let out = emit("$bm_rt = lyPos(lyAt(thisLayer, 8), frame);");
        assert!(!out.contains("thisComp"), "{out}");
    }

    #[test]
    fn frame_duration_is_a_division_not_a_baked_decimal() {
        let mut s = String::new();
        emit_one(
            &mut s,
            "$bm_rt = frameDuration;",
            &Plan {
                frame_duration: true,
                ..resolved()
            },
        );
        assert!(
            s.contains("const frameDuration = 1 / ctx.frameRate;"),
            "{s}"
        );
    }
}
