//! The `reanimated-aot` target: a React Native module for react-native-svg +
//! react-native-reanimated.
//!
//! Same wire, different write point. The payload `D`, the program pair
//! `P0`/`A0` and every DOM-free math module are the web runtime's, byte for
//! byte — what changes is where a frame's values land. The web runtime writes
//! SVG attributes; here every element is a plain prop-store handle
//! (`runtime/rn/set.js`), and the markup is replaced by a static element-tree
//! descriptor emitted at compile time from the baked scene, so nothing parses
//! XML at runtime.
//!
//! The swap happens at declaration granularity: the RN modules under
//! `runtime/rn/` redeclare exactly the names that touch the DOM (`put`,
//! `oDisplay`, `oRect`, `trim`, `kzero`, plus a DOM-free `mountRn`), and the
//! shaker then works on the substituted set with the same bare-name
//! reachability it uses for the web. Web emission is untouched.
//!
//! Every function that can run per-frame on the UI thread gets `'worklet'` as
//! its first statement, injected here rather than written into the shared
//! sources (a directive in the shared files would be dead weight on the web).
//! The output is deliberately unminified: Metro workletizes the directives
//! first and minifies after, whereas a minifier run here could strip them.
//!
//! What the target refuses (over the web set) is scanned up front by
//! [`crate::support::scan_rn`]; this backend also bails on any capability
//! outside its whitelist, so a scan gap fails the compile instead of shipping
//! a module that throws.

use std::collections::BTreeSet;

use anyhow::{Result, bail, ensure};

use crate::scene::{self, Binding, Caps, Scene};
use crate::{CompileOptions, data, ir};

use super::{Report, emit, shake};

/// The RN runtime modules. Each declaration either *overrides* a web
/// declaration of the same name (the DOM write points) or is new (`mountRn`
/// and the prop-store helpers `rput`/`rnProp`/`rnMatrix`).
const RN_MODS: &[emit::Mod] = &[
    emit::Mod {
        name: "rn/set.js",
        src: include_str!("../../runtime/rn/set.js"),
    },
    emit::Mod {
        name: "rn/display.js",
        src: include_str!("../../runtime/rn/display.js"),
    },
    emit::Mod {
        name: "rn/rect.js",
        src: include_str!("../../runtime/rn/rect.js"),
    },
    emit::Mod {
        name: "rn/shape.js",
        src: include_str!("../../runtime/rn/shape.js"),
    },
    emit::Mod {
        name: "rn/kf.js",
        src: include_str!("../../runtime/rn/kf.js"),
    },
    emit::Mod {
        name: "rn/core.js",
        src: include_str!("../../runtime/rn/core.js"),
    },
];

/// The capabilities this target's v1 implements. Everything else bails below —
/// gradient/ramp rebinding, filters, expressions, templates and instancing
/// have no RN counterpart yet.
pub(crate) fn supported() -> Caps {
    Caps::TRANSFORM
        | Caps::TRANSLATE
        | Caps::OPACITY
        | Caps::DISPLAY
        | Caps::SHAPE
        | Caps::RECT
        | Caps::ELLIPSE
        | Caps::FILL
        | Caps::STROKE
        | Caps::KEYFRAMES
        | Caps::EASING
        | Caps::SPATIAL
        | Caps::PATH_KF
        | Caps::HOLD
        | Caps::GEOM_RECT
        | Caps::GEOM_ELLIPSE
        | Caps::GEOM_STAR
        | Caps::PATH_D
        | Caps::TRIM
        | Caps::TIMELINE
        | Caps::TIME_REMAP
        | Caps::SHAPE_MULTI
        | Caps::TRIM_CHAIN
        | Caps::DASH
        | Caps::TRANSFORM_SKEW
}

pub fn report(module: &ir::Module, _options: &CompileOptions) -> Result<Option<Report>> {
    if !data::can_encode(module) {
        return Ok(None);
    }
    // No expression engine on this target. The scan already refused these by
    // name; this covers a module built through the library with the finding
    // allowed, where "play the keyframes" is not something the RN runtime can
    // be trusted to do yet.
    ensure!(
        module.expressions.is_empty(),
        "the reanimated-aot target does not support expressions"
    );
    let payload = data::encode(module)?;
    // Fully inlined, never instanced: the tree descriptor is emitted from the
    // expanded markup, and the element indices must be the same document-order
    // numbering the bindings address — no templates, no `uses` table.
    let scene = scene::plan_with(&payload, false, usize::MAX, false, &[])?;
    let missing = scene.caps - supported();
    ensure!(
        missing.is_empty(),
        "the reanimated-aot target does not support: {}",
        emit::caps_list(missing)
    );

    let (js, elements) = emit_rn(&scene)?;
    let mut caps: Vec<String> = scene.caps.iter_names().map(|(n, _)| n.to_string()).collect();
    caps.sort_unstable();
    Ok(Some(Report {
        js,
        caps,
        modules: Vec::new(),
        runtime_slice: 0,
        is_static: scene.is_static(),
        instanced: false,
        templated: false,
        elements,
        bindings: scene.data.b.len(),
        records: 0,
        generated: false,
        instance_clocks: false,
        assets: Vec::new(),
    }))
}

// ---------------------------------------------------------------------------
// Runtime assembly: web declarations with the DOM write points substituted.
// ---------------------------------------------------------------------------

/// The web runtime's declarations with each same-named RN declaration swapped
/// in place, and the RN-only ones appended. In-place keeps dependency order
/// for the overrides; the appended ones are `function` declarations (hoisted)
/// or leaves, so the end is a correct position for them.
fn rn_declarations() -> Vec<shake::Decl> {
    declarations_with(RN_MODS)
}

/// [`rn_declarations`], parameterized over the override module set so the
/// skia target can run the same substitution with its own modules.
pub(crate) fn declarations_with(mods: &[emit::Mod]) -> Vec<shake::Decl> {
    let mut decls = emit::all_declarations();
    let mut extra = Vec::new();
    for m in mods {
        for d in shake::declarations(m.src) {
            match decls.iter_mut().find(|w| w.name == d.name) {
                Some(slot) => *slot = d,
                None => extra.push(d),
            }
        }
    }
    decls.extend(extra);
    decls
}

/// Shake roots: the RN mount plus the op pairs this scene binds, exactly as
/// the web `roots` computes them (minus the DOM-only entry points).
fn roots(caps: Caps) -> Vec<&'static str> {
    let mut r = vec!["mountRn"];
    if caps.contains(Caps::TIME_REMAP) {
        r.push("resolve");
    }
    for (code, bind, apply, _) in emit::OPS {
        if caps.contains(scene::caps_for_op(code)) {
            r.push(bind);
            r.push(apply);
        }
    }
    r
}

/// The reachable runtime, each `function` declaration marked as a worklet.
fn bundle(caps: Caps) -> String {
    let kept = shake::shake(rn_declarations(), &roots(caps), caps);
    let mut out = String::with_capacity(16384);
    for d in &kept {
        out.push_str(&workletize(&d.name, &d.text));
    }
    out
}

/// Insert `'worklet';` as the first statement of a top-level `function NAME`
/// declaration. Anything else (consts, classes) passes through — the one
/// callable arrow the RN set reaches, `kzero`, carries its directive by hand
/// in `runtime/rn/kf.js`.
///
/// The declaration text starts with the comments `shake::declarations`
/// attached, so the scan looks for the `function NAME` line itself (always at
/// column 0) and takes the first `{` after it — no runtime source puts a brace
/// in a parameter list.
pub(crate) fn workletize(name: &str, text: &str) -> String {
    let needle = format!("function {name}");
    let mut off = 0;
    for line in text.split_inclusive('\n') {
        if line.starts_with(&needle)
            && matches!(line.as_bytes().get(needle.len()), Some(b'(') | Some(b' '))
            && let Some(rel) = text[off..].find('{')
        {
            let b = off + rel;
            return format!("{}{{ 'worklet';{}", &text[..b], &text[b + 1..]);
        }
        off += line.len();
    }
    text.to_string()
}

/// The document program as worklet `function` declarations — the web emitter's
/// `program`, in the form the reanimated Babel plugin can mark.
pub(crate) fn program_rn(k: usize, list: &[Binding]) -> String {
    let ops = scene::program_ops(list);
    if ops.is_empty() {
        return format!(
            "function P{k}() {{ 'worklet'; return 0; }}\nfunction A{k}() {{ 'worklet'; }}\n"
        );
    }
    let mut binds = Vec::with_capacity(ops.len());
    let mut calls = Vec::with_capacity(ops.len());
    for (i, code) in ops.iter().enumerate() {
        let (_, bind, apply, _) = emit::OPS
            .iter()
            .find(|(c, ..)| c == code)
            .expect("every op code has a runtime loop");
        binds.push(format!("{bind}(x, B[{i}], e, l, q, a)"));
        calls.push(format!("{apply}(x, S[{i}]);"));
    }
    format!(
        "function P{k}(x, B, e, l, q, a) {{ 'worklet'; return [{}]; }}\n\
         function A{k}(x, S) {{ 'worklet'; {} }}\n",
        binds.join(", "),
        calls.join(" ")
    )
}

// ---------------------------------------------------------------------------
// Markup → element-tree descriptor.
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub(crate) struct Node {
    pub(crate) tag: String,
    pub(crate) attrs: Vec<(String, String)>,
    pub(crate) children: Vec<Node>,
    /// Document-order index (root excluded) — the same numbering
    /// `svg.querySelectorAll('*')` gives the web runtime, assigned by
    /// [`assign_slots`] *before* any tree restructuring so bindings keep
    /// addressing the right element after nested svgs flatten to groups.
    pub(crate) slot: Option<usize>,
}

/// Parse the compiler's own baked markup. This is not an XML parser: generated
/// markup has no text nodes, no comments, no entities, no escaped quotes and
/// double-quoted attributes only, so anything else here is a compiler bug and
/// fails loudly.
pub(crate) fn parse_markup(src: &str) -> Result<Node> {
    let b = src.as_bytes();
    let mut pos = 0;
    let node = parse_element(b, &mut pos)?;
    ensure!(
        b[pos..].iter().all(u8::is_ascii_whitespace),
        "trailing content after the root element"
    );
    Ok(node)
}

fn parse_element(b: &[u8], pos: &mut usize) -> Result<Node> {
    ensure!(b.get(*pos) == Some(&b'<'), "expected '<' at byte {pos}");
    *pos += 1;
    let start = *pos;
    while b.get(*pos).is_some_and(u8::is_ascii_alphanumeric) {
        *pos += 1;
    }
    let tag = std::str::from_utf8(&b[start..*pos])?.to_string();
    ensure!(!tag.is_empty(), "empty tag name at byte {start}");

    let mut attrs = Vec::new();
    loop {
        while b.get(*pos).is_some_and(u8::is_ascii_whitespace) {
            *pos += 1;
        }
        match b.get(*pos) {
            Some(b'/') => {
                ensure!(b.get(*pos + 1) == Some(&b'>'), "expected '/>' in <{tag}>");
                *pos += 2;
                return Ok(Node {
                    tag,
                    attrs,
                    children: Vec::new(),
                    slot: None,
                });
            }
            Some(b'>') => {
                *pos += 1;
                break;
            }
            Some(_) => {
                let s = *pos;
                while b
                    .get(*pos)
                    .is_some_and(|c| *c != b'=' && !c.is_ascii_whitespace())
                {
                    *pos += 1;
                }
                let name = std::str::from_utf8(&b[s..*pos])?.to_string();
                ensure!(
                    b.get(*pos) == Some(&b'=') && b.get(*pos + 1) == Some(&b'"'),
                    "attribute `{name}` in <{tag}> is not double-quoted"
                );
                *pos += 2;
                let vs = *pos;
                while b.get(*pos).is_some_and(|c| *c != b'"') {
                    *pos += 1;
                }
                ensure!(b.get(*pos).is_some(), "unterminated attribute in <{tag}>");
                let value = std::str::from_utf8(&b[vs..*pos])?.to_string();
                *pos += 1;
                attrs.push((name, value));
            }
            None => bail!("unterminated element <{tag}>"),
        }
    }

    let mut children = Vec::new();
    loop {
        match (b.get(*pos), b.get(*pos + 1)) {
            (Some(b'<'), Some(b'/')) => {
                *pos += 2;
                let s = *pos;
                while b.get(*pos).is_some_and(|c| *c != b'>') {
                    *pos += 1;
                }
                ensure!(
                    &b[s..*pos] == tag.as_bytes(),
                    "mismatched close tag for <{tag}>"
                );
                *pos += 1;
                return Ok(Node {
                    tag,
                    attrs,
                    children,
                    slot: None,
                });
            }
            (Some(b'<'), _) => children.push(parse_element(b, pos)?),
            (Some(_), _) => bail!("unexpected text content inside <{tag}>"),
            (None, _) => bail!("unterminated element <{tag}>"),
        }
    }
}

/// The react-native-svg component for one SVG tag. Refusing an unmapped tag
/// here is the tree-level twin of the capability whitelist: nothing renders as
/// silently-missing markup.
fn component(tag: &str) -> Result<&'static str> {
    Ok(match tag {
        "svg" => "Svg",
        "g" => "G",
        "path" => "Path",
        "rect" => "Rect",
        "ellipse" => "Ellipse",
        "circle" => "Circle",
        "line" => "Line",
        "defs" => "Defs",
        "use" => "Use",
        "mask" => "Mask",
        "clipPath" => "ClipPath",
        "linearGradient" => "LinearGradient",
        "radialGradient" => "RadialGradient",
        "stop" => "Stop",
        "filter" => "Filter",
        // `feComponentTransfer`/`feFunc*` never reach here: the only shape the
        // compiler emits (the matte-inversion table) is lowered to
        // `feColorMatrix` by `lower_invert_filters`, because react-native-svg
        // stubs FeComponentTransfer out while FeColorMatrix is implemented
        // natively on both platforms.
        "feColorMatrix" => "FeColorMatrix",
        other => bail!("the reanimated-aot target has no react-native-svg component for <{other}>"),
    })
}

/// `fill-opacity` → `fillOpacity`, matching react-native-svg prop names (and
/// the runtime's `rnProp`).
fn camel(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut up = false;
    for c in name.chars() {
        if c == '-' {
            up = true;
        } else if up {
            out.extend(c.to_uppercase());
            up = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// A baked transform value as a ColumnMajorTransformMatrix array. A transform
/// *string* throws on Fabric iOS, so both static props (here) and per-frame
/// writes (`rnMatrix` in `runtime/rn/set.js`) emit the 6-number array. The
/// compiler only ever writes these two spellings — see `scene::svg`.
fn transform_array(v: &str) -> Result<String> {
    if let Some(args) = v.strip_prefix("matrix(").and_then(|r| r.strip_suffix(')')) {
        let parts: Vec<&str> = args.split(',').collect();
        ensure!(parts.len() == 6, "matrix() with {} arguments", parts.len());
        Ok(format!("[{}]", parts.join(", ")))
    } else if let Some(args) = v.strip_prefix("translate(").and_then(|r| r.strip_suffix(')')) {
        let parts: Vec<&str> = args.split(',').collect();
        ensure!(parts.len() == 2, "translate() with {} arguments", parts.len());
        Ok(format!("[1, 0, 0, 1, {}, {}]", parts[0], parts[1]))
    } else {
        bail!("unsupported transform `{v}` in the reanimated-aot target")
    }
}

/// Whether react-native-svg's native side reads this prop as a raw `Double`.
///
/// The generated `RNSVG*ManagerDelegate.setProperty` casts these four with
/// `((Double) value).floatValue()`, so a `String` there throws
/// `ClassCastException: String cannot be cast to Double`. The static props
/// here reach the native side through rn-svg's JS components, which coerce —
/// but they share both the name set and the value with the per-frame writes
/// (`rnNumeric` in `runtime/rn/set.js`), which do not, so the two agree on the
/// emitted type rather than differing by accident.
fn numeric_prop(prop: &str) -> bool {
    matches!(
        prop,
        "opacity" | "fillOpacity" | "strokeOpacity" | "strokeDashoffset"
    )
}

/// One node's static props as JS object entries. Everything the runtime may
/// later overwrite is still emitted — these are the baked first-frame values,
/// so the tree renders exactly frame `ip` before any `apply` runs.
fn static_props(node: &Node) -> Result<Vec<String>> {
    let mut parts = Vec::new();
    for (name, value) in &node.attrs {
        if name == "xmlns" || name.starts_with("xmlns:") {
            continue;
        }
        ensure!(
            !name.contains(':'),
            "namespaced attribute `{name}` in <{}>",
            node.tag
        );
        match name.as_str() {
            "style" => {
                for decl in value.split(';').filter(|d| !d.is_empty()) {
                    match decl {
                        // An `<svg>` clips to its viewport in react-native-svg
                        // already; a precomp frame's clip rides its clipPath.
                        "overflow:hidden" => {}
                        "display:none" => parts.push("display: 'none'".to_string()),
                        other => bail!(
                            "unsupported style `{other}` in the reanimated-aot target (blend modes and filters are refused up front)"
                        ),
                    }
                }
            }
            "transform" | "gradientTransform" => {
                parts.push(format!("{}: {}", camel(name), transform_array(value)?));
            }
            _ => {
                // `--u` is the web target's per-mount id-uniquing marker;
                // react-native-svg scopes ids per <Svg> root, so the marker is
                // resolved away instead of carried.
                let v = value.replace("--u", "");
                let prop = camel(name);
                // A percentage (`opacity="50%"`) or any other non-plain
                // spelling stays a string for the JS component to parse.
                let num = numeric_prop(&prop)
                    && v.parse::<f64>().is_ok_and(|x| x.is_finite() && format!("{x}") == v);
                if num {
                    parts.push(format!("{prop}: {v}"));
                } else {
                    parts.push(format!("{prop}: {}", emit::js_string(&v)));
                }
            }
        }
    }
    Ok(parts)
}

/// Assign document-order slot indices — the same numbering
/// `svg.querySelectorAll('*')` gives the web runtime: every descendant in
/// document order, root excluded (the root `<Svg>` stays static; animated
/// writes only ever land on inner elements). Runs on the tree exactly as
/// baked, before any RN-only restructuring, so the indices stay the ones the
/// bindings address.
pub(crate) fn assign_slots(node: &mut Node, root: bool, next: &mut usize) {
    if !root {
        node.slot = Some(*next);
        *next += 1;
    }
    for c in &mut node.children {
        assign_slots(c, false, next);
    }
}

/// Lower the matte-inversion filter to `<feColorMatrix>`.
///
/// The compiler's only `<feComponentTransfer>` producer is
/// `scene::build::invert_filter`: `feFuncA` (alpha matte) or
/// `feFuncR/G/B` (luma matte), each `type="table" tableValues="1 0"`.
/// react-native-svg stubs FeComponentTransfer/FeFunc* out (they render null,
/// so the whole filtered subtree paints nothing), but FeColorMatrix is
/// implemented natively — and a color matrix expresses the same inversion:
/// `c' = -c + 1` on exactly the inverted channels.
pub(crate) fn lower_invert_filters(node: &mut Node) -> Result<()> {
    for c in &mut node.children {
        lower_invert_filters(c)?;
    }
    for c in &mut node.children {
        if c.tag != "feComponentTransfer" {
            continue;
        }
        let mut funcs = BTreeSet::new();
        for f in &c.children {
            let table = f.attrs.iter().any(|(k, v)| k == "type" && v == "table")
                && f.attrs
                    .iter()
                    .any(|(k, v)| k == "tableValues" && v == "1 0");
            ensure!(
                table,
                "the reanimated-aot target only lowers the invert-table <{}>",
                f.tag
            );
            funcs.insert(f.tag.clone());
        }
        let values = if funcs == BTreeSet::from(["feFuncA".to_string()]) {
            // Keep RGB, invert alpha.
            "1 0 0 0 0 0 1 0 0 0 0 0 1 0 0 0 0 0 -1 1"
        } else if funcs
            == BTreeSet::from([
                "feFuncR".to_string(),
                "feFuncG".to_string(),
                "feFuncB".to_string(),
            ])
        {
            // Invert RGB, keep alpha.
            "-1 0 0 0 1 0 -1 0 0 1 0 0 -1 0 1 0 0 0 1 0"
        } else {
            bail!("unrecognized <feComponentTransfer> channel set {funcs:?}")
        };
        c.tag = "feColorMatrix".to_string();
        c.attrs = vec![
            ("type".to_string(), "matrix".to_string()),
            ("values".to_string(), values.to_string()),
        ];
        c.children.clear();
    }
    Ok(())
}

/// Flatten every nested `<svg>` into a viewport-clipped `<g>`.
///
/// react-native-svg mounts a native `RNSVGSvgView` for *every* `<Svg>`
/// element, which breaks the emitted tree twice over:
///
/// - references: each view keeps its own brush/mask/filter registry, so a
///   `url(#id)` inside a nested `<Svg>` never resolves to a root `<Defs>`
///   (web references are document-global and this cannot happen there);
/// - masks/filters: the iOS blend path (`RNSVGRenderable renderTo`) sizes its
///   offscreen buffers from the *nested* view's rect — measured in the
///   parent's user units — while rendering with the full device CTM, so
///   whenever the outer viewBox up-scales (design units smaller than
///   on-screen points) the content lands outside the buffer and every masked
///   subtree paints empty.
///
/// A `<g clip-path>` with a viewport-sized `<rect>` is exactly what the
/// nested svg contributed visually (its viewport clip), and it leaves ONE
/// native view whose buffers are sized from real device bounds and whose
/// single registry resolves every reference. Runs after [`assign_slots`], so
/// the `<g>` keeps the svg's slot (bindings only ever write props a `<g>`
/// carries identically, like `opacity`).
pub(crate) fn flatten_nested_svgs(root: &mut Node) -> Result<()> {
    let mut clips: Vec<(String, String)> = Vec::new();
    flatten_walk(root, true, &mut clips)?;
    if clips.is_empty() {
        return Ok(());
    }
    let clip_defs = clips
        .iter()
        .enumerate()
        .map(|(i, (w, h))| Node {
            tag: "clipPath".to_string(),
            attrs: vec![("id".to_string(), format!("vp{i}"))],
            children: vec![Node {
                tag: "rect".to_string(),
                attrs: vec![
                    ("width".to_string(), w.clone()),
                    ("height".to_string(), h.clone()),
                ],
                children: Vec::new(),
                slot: None,
            }],
            slot: None,
        })
        .collect();
    // First, not last: the svg view clears its clip-path registry on every
    // paint and refills it in document order, so a defs that trails its
    // consumers would leave them unclipped on that whole pass.
    root.children.insert(
        0,
        Node {
            tag: "defs".to_string(),
            attrs: Vec::new(),
            children: clip_defs,
            slot: None,
        },
    );
    Ok(())
}

fn flatten_walk(node: &mut Node, root: bool, clips: &mut Vec<(String, String)>) -> Result<()> {
    if !root && node.tag == "svg" {
        let mut width = None;
        let mut height = None;
        let mut kept = Vec::new();
        for (k, v) in node.attrs.drain(..) {
            match k.as_str() {
                "width" => width = Some(v),
                "height" => height = Some(v),
                // Props a <g> carries identically (`style` only ever holds
                // `overflow:hidden`/`display:none` here — see `static_props`).
                // A precomp frame's own `clip-path` rides along; the viewport
                // clip then wraps the children instead (one element carries
                // one clip).
                "opacity" | "display" | "style" | "clip-path" => kept.push((k, v)),
                other => bail!(
                    "nested <svg> attribute `{other}` has no <g> equivalent in the reanimated-aot target"
                ),
            }
        }
        let (Some(w), Some(h)) = (width, height) else {
            bail!("nested <svg> without width/height");
        };
        let pair = (w, h);
        let idx = clips.iter().position(|c| *c == pair).unwrap_or_else(|| {
            clips.push(pair.clone());
            clips.len() - 1
        });
        node.tag = "g".to_string();
        let vp = ("clip-path".to_string(), format!("url(#vp{idx})"));
        if kept.iter().any(|(k, _)| k == "clip-path") {
            // The svg already carries its precomp clip; the viewport clip
            // wraps the children on a fresh (slotless) inner <g> so both
            // apply — matching the svg's viewport-then-clip stacking.
            let children = std::mem::take(&mut node.children);
            node.children = vec![Node {
                tag: "g".to_string(),
                attrs: vec![vp],
                children,
                slot: None,
            }];
        } else {
            kept.push(vp);
        }
        node.attrs = kept;
    }
    for c in &mut node.children {
        flatten_walk(c, false, clips)?;
    }
    Ok(())
}

/// Print the tree descriptor from the slots [`assign_slots`] stamped.
fn tree_js(
    node: &Node,
    root: bool,
    bound: &BTreeSet<usize>,
    indent: usize,
    out: &mut String,
) -> Result<()> {
    let pad = "  ".repeat(indent);
    out.push_str(&pad);
    out.push_str(&format!("{{ type: '{}'", component(&node.tag)?));
    if !root
        && let Some(s) = node.slot
        && bound.contains(&s)
    {
        out.push_str(&format!(", slot: {s}"));
    }
    out.push_str(&format!(
        ", staticProps: {{ {} }}",
        static_props(node)?.join(", ")
    ));
    if node.children.is_empty() {
        out.push_str(" }");
    } else {
        out.push_str(", children: [\n");
        for (i, c) in node.children.iter().enumerate() {
            tree_js(c, false, bound, indent + 1, out)?;
            out.push_str(if i + 1 < node.children.len() { ",\n" } else { "\n" });
        }
        out.push_str(&format!("{pad}] }}"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Module assembly.
// ---------------------------------------------------------------------------

fn emit_rn(scene: &Scene) -> Result<(String, usize)> {
    // The plan above forces all three off; a scene that still carries them
    // would emit a tree the bindings do not address.
    debug_assert!(scene.data.assets.is_empty(), "RN scenes are never instanced");
    debug_assert!(scene.data.uses.is_empty(), "RN scenes are never instanced");
    debug_assert!(scene.data.tpl.is_empty(), "RN scenes are fully inlined");

    let mut root = parse_markup(&scene.markup)?;
    ensure!(root.tag == "svg", "the document root is <{}>", root.tag);

    let bound: BTreeSet<usize> = scene.data.b.iter().map(|b| b.el_index as usize).collect();
    let mut count = 0usize;
    assign_slots(&mut root, true, &mut count);
    lower_invert_filters(&mut root)?;
    flatten_nested_svgs(&mut root)?;
    let mut tree = String::new();
    tree_js(&root, true, &bound, 0, &mut tree)?;
    if let Some(max) = bound.iter().next_back() {
        ensure!(
            *max < count,
            "binding addresses element {max} but the tree has {count}"
        );
    }

    let n = |v: f64| crate::scene::svg::n(v);
    let attr = |name: &str| {
        root.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };
    // `meta` wants the design size in numbers. The baked root's width/height
    // mirror the source document, which may say "100%" — a length JS cannot
    // hold as a number — so anything non-numeric falls back to the viewBox
    // extent, which the compiler always writes from the design size.
    let view: Vec<&str> = attr("viewBox").unwrap_or("").split(' ').collect();
    fn numeric(v: Option<&str>) -> Option<&str> {
        v.filter(|s| s.parse::<f64>().is_ok())
    }
    let width = numeric(attr("width"))
        .or_else(|| numeric(view.get(2).copied()))
        .unwrap_or("0");
    let height = numeric(attr("height"))
        .or_else(|| numeric(view.get(3).copied()))
        .unwrap_or("0");

    let mut src = String::with_capacity(scene.markup.len() * 2 + 4096);
    src.push_str(&format!(
        "// Generated by ulottie — reanimated-aot target (react-native-svg + react-native-reanimated).\n\
         // Deliberately unminified: Metro's reanimated plugin reads the 'worklet' directives\n\
         // before its own minifier runs; a minifier here could strip them.\n\
         // caps: {}\n\n",
        emit::caps_list(scene.caps),
    ));

    if scene.is_static() {
        src.push_str(
            "// Fully static after compilation: the tree descriptor is the whole picture.\n\
             // `init` returns an inert instance so consumers keep one code path.\n\n",
        );
    } else {
        src.push_str(&format!(
            "// runtime symbols: {}\n",
            shake::shake(rn_declarations(), &roots(scene.caps), scene.caps)
                .iter()
                .map(|d| d.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
        src.push_str(&bundle(scene.caps));
        src.push_str(&program_rn(0, &scene.data.b));
        src.push('\n');
        src.push_str(&format!("const D = {};\n", serde_json::to_string(&scene.data)?));
        if !scene.data.strings.is_empty() {
            let items: Vec<String> = scene
                .data
                .strings
                .iter()
                .map(|m| emit::js_string(m))
                .collect();
            src.push_str(&format!("const SP = [{}];\n", items.join(", ")));
        }
        src.push('\n');
    }

    src.push_str("export const tree =\n");
    src.push_str(&tree);
    src.push_str(";\n\n");
    src.push_str(&format!(
        "export const meta = {{ fr: {}, ip: {}, op: {}, width: {width}, height: {height} }};\n",
        n(scene.data.fr),
        n(scene.data.ip),
        n(scene.data.op),
    ));

    if scene.is_static() {
        src.push_str(&format!(
            "export const init = () => {{ 'worklet'; return {{ els: [], dirty: [], apply: function () {{ 'worklet'; }}, fr: {}, ip: {}, op: {} }}; }};\n",
            n(scene.data.fr),
            n(scene.data.ip),
            n(scene.data.op),
        ));
    } else {
        let mut ext = Vec::new();
        if !scene.data.strings.is_empty() {
            ext.push("p: SP");
        }
        if scene.caps.contains(Caps::TIME_REMAP) {
            ext.push("r: resolve");
        }
        let ext = if ext.is_empty() {
            String::new()
        } else {
            format!(", {{ {} }}", ext.join(", "))
        };
        src.push_str(&format!(
            "export const init = () => {{ 'worklet'; return mountRn(D, P0, A0, {count}{ext}); }};\n"
        ));
    }
    Ok((src, count))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worklet_lands_after_the_body_brace_not_in_the_comment() {
        let text = "// says { things } in a comment\nfunction put(el, v) {\n  el.p = v;\n}\n";
        let out = workletize("put", text);
        assert!(
            out.starts_with("// says { things } in a comment\nfunction put(el, v) { 'worklet';"),
            "{out}"
        );
        // A const passes through untouched.
        let arrow = "const kzero = () => 0;\n";
        assert_eq!(workletize("kzero", arrow), arrow);
    }

    #[test]
    fn transforms_become_matrix_arrays_never_strings() {
        assert_eq!(
            transform_array("matrix(.5,0,0,.5,10,-2.5)").unwrap(),
            "[.5, 0, 0, .5, 10, -2.5]"
        );
        assert_eq!(
            transform_array("translate(4,-8)").unwrap(),
            "[1, 0, 0, 1, 4, -8]"
        );
        assert!(transform_array("rotate(45)").is_err());
    }

    #[test]
    fn attrs_map_to_react_native_svg_props() {
        assert_eq!(camel("fill-opacity"), "fillOpacity");
        assert_eq!(camel("stroke-dasharray"), "strokeDasharray");
        assert_eq!(camel("mask-type"), "maskType");
        assert_eq!(camel("viewBox"), "viewBox");
    }

    #[test]
    fn the_rn_names_do_not_collide_with_web_declarations() {
        // Overrides must exist on the web side (they replace, not shadow);
        // additions must not (two declarations of one name in a flat scope).
        let web: BTreeSet<String> = emit::all_declarations()
            .into_iter()
            .map(|d| d.name)
            .collect();
        for name in ["put", "oDisplay", "oRect", "trim", "kzero"] {
            assert!(web.contains(name), "override `{name}` has no web original");
        }
        for name in ["rput", "rnProp", "rnMatrix", "mountRn"] {
            assert!(!web.contains(name), "`{name}` collides with a web name");
        }
    }

    #[test]
    fn the_parser_reads_the_compilers_own_markup_shape() {
        let node = parse_markup(
            r#"<svg viewBox="0 0 10 10"><g transform="translate(1,2)"><path d="M0 0"/></g></svg>"#,
        )
        .unwrap();
        assert_eq!(node.tag, "svg");
        assert_eq!(node.children.len(), 1);
        assert_eq!(node.children[0].children[0].tag, "path");
        assert!(parse_markup("<svg>text</svg>").is_err());
    }

    #[test]
    fn slots_number_descendants_in_document_order_root_excluded() {
        let mut node =
            parse_markup(r#"<svg><g><path d="M0 0"/></g><rect width="1" height="1"/></svg>"#)
                .unwrap();
        let bound = BTreeSet::from([1usize, 2]);
        let mut count = 0;
        assign_slots(&mut node, true, &mut count);
        let mut out = String::new();
        tree_js(&node, true, &bound, 0, &mut out).unwrap();
        assert_eq!(count, 3);
        // g is slot 0 (unbound, so unlisted); path slot 1; rect slot 2.
        assert!(!out.contains("slot: 0"), "{out}");
        assert!(out.contains("slot: 1"), "{out}");
        assert!(out.contains("slot: 2"), "{out}");
    }

    #[test]
    fn nested_svgs_flatten_to_viewport_clipped_groups() {
        // Two nested svgs (one deep, sharing a size with the shallow one, one
        // distinct) flatten to <g clip-path>; slots stamped before the pass
        // ride along; the shared viewport size dedupes to one clipPath.
        let mut root = parse_markup(
            r#"<svg viewBox="0 0 10 10"><svg width="10" height="10" opacity="0"><svg width="5" height="5"><rect fill="url(#g0)"/></svg><svg width="10" height="10"/></svg><defs><linearGradient id="g0"/></defs></svg>"#,
        )
        .unwrap();
        let mut count = 0;
        assign_slots(&mut root, true, &mut count);
        flatten_nested_svgs(&mut root).unwrap();
        let outer = &root.children[1];
        assert_eq!(outer.tag, "g");
        assert_eq!(outer.slot, Some(0));
        assert!(
            outer.attrs.contains(&("opacity".to_string(), "0".to_string())),
            "carried props survive: {:?}",
            outer.attrs
        );
        assert!(outer.attrs.contains(&("clip-path".to_string(), "url(#vp0)".to_string())));
        assert!(outer.children[0]
            .attrs
            .contains(&("clip-path".to_string(), "url(#vp1)".to_string())));
        assert!(outer.children[1]
            .attrs
            .contains(&("clip-path".to_string(), "url(#vp0)".to_string())));
        // The defs prepended at the root holds one clipPath per distinct
        // viewport size (first, so the per-paint clip registry fills before
        // any consumer renders).
        let defs = &root.children[0];
        assert_eq!(defs.tag, "defs");
        let vps: Vec<&str> = defs
            .children
            .iter()
            .filter(|c| c.tag == "clipPath")
            .map(|c| c.attrs[0].1.as_str())
            .collect();
        assert_eq!(vps, ["vp0", "vp1"]);
        assert_eq!(defs.children[1].children[0].tag, "rect");
        assert_eq!(defs.children[1].children[0].attrs[0].1, "5");
        // No <Svg> below the root survives the pass.
        fn any_svg(n: &Node) -> bool {
            n.children.iter().any(|c| c.tag == "svg" || any_svg(c))
        }
        assert!(!any_svg(&root));
    }

    #[test]
    fn a_nested_svg_attribute_a_group_cannot_carry_is_refused() {
        let mut root = parse_markup(
            r#"<svg><svg width="10" height="10" viewBox="0 0 5 5"><rect width="1" height="1"/></svg></svg>"#,
        )
        .unwrap();
        let err = flatten_nested_svgs(&mut root).unwrap_err().to_string();
        assert!(err.contains("viewBox"), "{err}");
    }
}
