//! Compile each unique IR expression into a JS function that the driver's
//! `evalProp` dispatches to.
//!
//! The Bodymovin-transpiled expression body uses a fixed set of names —
//! `value`, `time`, `thisLayer`, `thisProperty`, `effect`, `sum`, `sub`,
//! `mul`, `div`, `clamp`, `thisComp`, `radiansToDegrees`, `degreesToRadians`,
//! `numKeys`, `nearestKey`, `key`, `valueAtTime`, `velocityAtTime`,
//! `loopOut`, `createPath`. We surface all of them in the wrapper's scope
//! so the body can run as-is, then `return $bm_rt;`.

use crate::ir;

pub fn emit_one(out: &mut String, expr: &ir::Expression) {
    out.push_str("  function(value, thisLayer, thisProperty, frame, ctx) {\n");
    out.push_str("    const { thisComp, sum, sub, mul, div, clamp, radiansToDegrees, degreesToRadians, createPath, pointOnPath, tangentOnPath } = ctx;\n");
    out.push_str("    const time = frame / ctx.frameRate;\n");
    out.push_str("    const effect = (n) => (thisLayer ? thisLayer.effect(n) : (() => 0));\n");
    // Bare `fromCompToSurface(...)` calls in AE expressions resolve to the
    // current layer's transform inverse — make it available as a free name
    // here. Lights wire path uses this to map a null layer's comp position
    // back into the wire layer's own space.
    out.push_str("    const fromCompToSurface = (pt) => (thisLayer ? thisLayer.fromCompToSurface(pt) : pt);\n");
    // Use the same property API surface lottie-web exposes. If `thisProperty`
    // is null (e.g. expressions on a static property), provide safe stubs.
    out.push_str("    const numKeys = thisProperty?.numKeys ?? 0;\n");
    out.push_str("    const nearestKey = thisProperty?.nearestKey ? thisProperty.nearestKey.bind(thisProperty) : ((t) => ({ index: 1, time: 0 }));\n");
    out.push_str("    const key = thisProperty?.key ? thisProperty.key.bind(thisProperty) : ((n) => ({ time: 0, value: 0, index: n }));\n");
    out.push_str("    const valueAtTime = thisProperty?.valueAtTime ? thisProperty.valueAtTime.bind(thisProperty) : ((t) => 0);\n");
    out.push_str("    const velocityAtTime = thisProperty?.velocityAtTime ? thisProperty.velocityAtTime.bind(thisProperty) : ((t) => 0);\n");
    // AE exposes `loopOut` as a free function (equivalent to
    // `thisProperty.loopOut(...)`). The ripple's traceNull Progress relies on
    // this form. Stubs to a no-op when no property is in scope.
    out.push_str("    const loopOut = thisProperty?.loopOut ? thisProperty.loopOut.bind(thisProperty) : ((mode, n) => value);\n");
    out.push_str("    var $bm_rt;\n");
    // Body. Indent it for readability.
    for line in expr.body.lines() {
        out.push_str("    ");
        out.push_str(line);
        out.push('\n');
    }
    // Some Bodymovin bodies already `return $bm_rt;` themselves; ours adds it
    // unconditionally. Duplicate `return` is harmless because the inner one
    // wins (this is JS semantics).
    out.push_str("    return $bm_rt;\n");
    out.push_str("  },\n");
}
