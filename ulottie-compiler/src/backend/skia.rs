//! The `skia-aot` target: a React Native module for @shopify/react-native-skia
//! + react-native-reanimated.
//!
//! Same wire, different write point — again. The payload `D`, the program
//! pair `P0`/`A0` and every DOM-free math module are the web runtime's, byte
//! for byte, and the element handles are the rn target's prop-store records
//! (`mountRn` is target-agnostic and mounts here unchanged). What changes is
//! the output side: instead of a react-native-svg component tree, the baked
//! markup lowers at compile time to a **display-list descriptor** `dl` that
//! `runtime/skia/draw.js` walks imperatively into an `SkPicture`, so a player
//! mounts ONE native `<Canvas>` view regardless of animation size.
//!
//! Every `url(#id)` reference — clip paths, masks, gradients, the
//! matte-inversion color filter — is resolved right here into an inline
//! descriptor; no id registry exists at runtime. The tree is first
//! restructured by the rn passes (slot assignment on the tree exactly as
//! baked, invert-filter lowering, nested-svg flattening), because the
//! flattened form is target-neutral: a viewport becomes a plain rect clip.
//!
//! The `Skia` factory is not importable from a self-contained generated
//! module, so `init(Skia)` takes it as a parameter; the player passes the
//! imported object.
//!
//! Phase 1 implements exactly the reanimated-aot capability set (plus exact
//! `paint-order`, which is free in draw ordering); anything the lowering
//! meets outside that set bails with a named finding — nothing renders as
//! silently-missing content.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail, ensure};

use crate::scene::{Caps, Scene};
use crate::{CompileOptions, data, ir, scene};

use super::rn::{self, Node};
use super::{Report, emit, shake};

/// The Skia runtime modules. `runtime/rn/set.js` is NOT in this set — its
/// role (the `put`/`rput` write points) is taken by `runtime/skia/set.js`,
/// and including both would declare `rput` twice in one flat scope. The
/// other rn modules are write-point-agnostic: they call `put`/`rput` by bare
/// name, which resolves to the Skia pair in the shaken bundle.
const SKIA_MODS: &[emit::Mod] = &[
    emit::Mod {
        name: "skia/set.js",
        src: include_str!("../../runtime/skia/set.js"),
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
    // draw.js must precede core.js: the reanimated babel plugin rewrites
    // 'worklet' function declarations into module-order assignments and
    // captures a worklet's free variables when its factory runs, so
    // `mountSkia`'s reference to `skDraw` is captured as `undefined` unless
    // `skDraw` is assigned first. (On device this surfaced as
    // "undefined is not a function" inside `h.draw` on the UI runtime.)
    emit::Mod {
        name: "skia/draw.js",
        src: include_str!("../../runtime/skia/draw.js"),
    },
    emit::Mod {
        name: "skia/core.js",
        src: include_str!("../../runtime/skia/core.js"),
    },
];

/// The rn whitelist plus the Skia-only capabilities react-native-svg cannot
/// express: animated gradient geometry (`GRADIENT`), keyframed colour ramps
/// (`RAMP`) and animated layer-effect parameters (`FX`). Blend modes, static
/// filters and inverted mattes are markup-only — no capability bit exists for
/// them — so they arrive through the lowering below, not through this set.
fn supported() -> Caps {
    rn::supported() | Caps::GRADIENT | Caps::RAMP | Caps::FX
}

pub fn report(module: &ir::Module, _options: &CompileOptions) -> Result<Option<Report>> {
    if !data::can_encode(module) {
        return Ok(None);
    }
    // No expression engine on this target — same stance as reanimated-aot.
    ensure!(
        module.expressions.is_empty(),
        "the skia-aot target does not support expressions"
    );
    let payload = data::encode(module)?;
    // Fully inlined, never instanced — the display list is emitted from the
    // expanded markup and bindings address document-order slots.
    let scene = scene::plan_with(&payload, false, usize::MAX, false, &[])?;
    let missing = scene.caps - supported();
    ensure!(
        missing.is_empty(),
        "the skia-aot target does not support: {}",
        emit::caps_list(missing)
    );

    let (js, elements) = emit_skia(&scene)?;
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
// Runtime assembly.
// ---------------------------------------------------------------------------

fn skia_declarations() -> Vec<shake::Decl> {
    rn::declarations_with(SKIA_MODS)
}

/// Shake roots for an animated scene: the Skia mount plus the op pairs this
/// scene binds — the rn `roots`, with `mountSkia` as the entry.
fn roots(caps: Caps) -> Vec<&'static str> {
    let mut r = vec!["mountSkia"];
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

/// The reachable runtime, worklet-marked. Unlike rn, a *static* scene still
/// carries a runtime slice here — the draw walker — rooted at
/// `skPrepare`/`skDraw` directly.
fn bundle(roots: &[&str], caps: Caps) -> String {
    let kept = shake::shake(skia_declarations(), roots, caps);
    let mut out = String::with_capacity(16384);
    for d in &kept {
        out.push_str(&rn::workletize(&d.name, &d.text));
    }
    out
}

fn symbols(roots: &[&str], caps: Caps) -> String {
    shake::shake(skia_declarations(), roots, caps)
        .iter()
        .map(|d| d.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

// ---------------------------------------------------------------------------
// Markup → display-list descriptor.
//
// Grammar (all refs already resolved inline):
//   group   { k: 0, s?, m?: [9 row-major], o?, hd?: 1, bm?, clip?, mask?,
//             cf?, fx?, c: [..] }
//   path    { k: 1, s?, m?, o?, hd?: 1, d: '..', eo?: 1, paint }
//   rect    { k: 2, s?, m?, o?, hd?: 1, x, y, w, h, rx?, ry?, paint }
//   ellipse { k: 3, s?, m?, o?, hd?: 1, cx, cy, rx, ry, paint }
//   image   { k: 4, w, h, u: 'base64..' } — an embedded image, decoded at
//           mount; pure markup (nothing about it animates, so no slot)
//   clip    { r: [x, y, w, h] } | { d: '..', eo?: 1 }
//   mask    { luma?: 1, c: [nodes] }
//   cf      { m: [20 floats], r: [x, y, w, h] }   (matte inversion)
//   bm      Skia BlendMode int (mix-blend-mode on a layer group)
//   fx      [stage, ..] — layer effects, applied innermost first; each stage
//           re-draws the running content once per pass:
//     stage [pass, ..]
//     pass  0                            source drawn unchanged (tint base)
//         | { cf: [20] }                 colour-matrix layer (fill / tint t1)
//         | { cf2: [[20], [20]] }        composed matrices, outer∘inner (tint t2)
//         | { sh: 1, sb?, so?, sf?, dx?, dy?, sd?, c: '#hex', fo? }
//                                        drop shadow (slots: blur/offset/flood)
//         | { bl: 1, s?, sx?, sy?, tm }  gaussian blur, tm = Skia TileMode
//   paint   { f?, fo?, sc?, so?, sw?, cap?, join?, ml?, da?: [..], doff?, po?: 1 }
//   f/sc:   '#color' | { rad: 1, s?, cx, cy, r, st, gt?: [9] }
//                    | { lin: 1, s?, x1, y1, x2, y2, st, gt? }
//   st      [[off, '#c'], ..] — a bound (animated) stop carries its slot as a
//           third entry: [off, '#c', slot]
// Slots on gradients, stops and filter primitives link the element handle to
// a live record so the RAMP/GRADIENT/FX op writes land (see skia/set.js).
// ---------------------------------------------------------------------------

/// Tags that define referenced resources: indexed by id up front, resolved
/// inline at their use sites, and never lowered as drawable content.
const DEF_TAGS: &[&str] = &["defs", "clipPath", "mask", "linearGradient", "radialGradient", "filter"];

fn index_defs<'a>(n: &'a Node, map: &mut BTreeMap<&'a str, &'a Node>) {
    if let Some((_, id)) = n.attrs.iter().find(|(k, _)| k == "id")
        && DEF_TAGS.contains(&n.tag.as_str())
    {
        map.insert(id, n);
    }
    for c in &n.children {
        index_defs(c, map);
    }
}

/// Validate a numeric attribute value; emitted verbatim (JS reads `.406`).
fn num<'a>(v: &'a str, what: &str) -> Result<&'a str> {
    ensure!(
        v.parse::<f64>().is_ok(),
        "non-numeric `{what}` value `{v}` in the skia-aot target"
    );
    Ok(v)
}

/// A baked transform as a row-major 3x3 for `canvas.concat`. The compiler
/// only ever writes these two spellings — see `scene::svg`.
fn transform9(v: &str) -> Result<String> {
    if let Some(args) = v.strip_prefix("matrix(").and_then(|r| r.strip_suffix(')')) {
        let p: Vec<&str> = args.split(',').collect();
        ensure!(p.len() == 6, "matrix() with {} arguments", p.len());
        Ok(format!(
            "[{}, {}, {}, {}, {}, {}, 0, 0, 1]",
            p[0], p[2], p[4], p[1], p[3], p[5]
        ))
    } else if let Some(args) = v.strip_prefix("translate(").and_then(|r| r.strip_suffix(')')) {
        let p: Vec<&str> = args.split(',').collect();
        ensure!(p.len() == 2, "translate() with {} arguments", p.len());
        Ok(format!("[1, 0, {}, 0, 1, {}, 0, 0, 1]", p[0], p[1]))
    } else {
        bail!("unsupported transform `{v}` in the skia-aot target")
    }
}

struct Lower<'a> {
    defs: &'a BTreeMap<&'a str, &'a Node>,
    bound: &'a BTreeSet<usize>,
    /// Every slot stamped on an emitted (drawable) node — checked against
    /// `bound` afterwards so a binding into a defs subtree fails loudly.
    slots: BTreeSet<usize>,
}

impl<'a> Lower<'a> {
    fn lookup(&self, v: &str, what: &str) -> Result<&'a Node> {
        let id = v
            .strip_prefix("url(#")
            .and_then(|r| r.strip_suffix(')'))
            .ok_or_else(|| anyhow::anyhow!("unsupported {what} value `{v}`"))?;
        self.defs
            .get(id)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("unresolved reference `{v}`"))
    }

    /// `clip-path="url(#id)"` → an inline clip descriptor. A path clip may
    /// be *animated* (a lottie layer mask keyframes its bezier): its slot
    /// rides along as `s`, the runtime links the element handle to the clip
    /// record, and — exactly like a drawable path — a `d` animated from
    /// frame `ip` is not baked, so `d` is optional.
    fn clip_js(&mut self, v: &str) -> Result<String> {
        let def = self.lookup(v, "clip-path")?;
        ensure!(def.tag == "clipPath", "`{v}` does not name a <clipPath>");
        for (k, val) in &def.attrs {
            match k.as_str() {
                "id" => {}
                "clipPathUnits" if val == "userSpaceOnUse" => {}
                other => bail!("the skia-aot target cannot lower <clipPath {other}>"),
            }
        }
        ensure!(
            def.children.len() == 1,
            "the skia-aot target only lowers single-shape clip paths"
        );
        let c = &def.children[0];
        match c.tag.as_str() {
            "rect" => {
                let mut x = "0";
                let mut y = "0";
                let (mut w, mut h) = (None, None);
                for (k, val) in &c.attrs {
                    match k.as_str() {
                        "x" => x = num(val, "x")?,
                        "y" => y = num(val, "y")?,
                        "width" => w = Some(num(val, "width")?),
                        "height" => h = Some(num(val, "height")?),
                        other => bail!("the skia-aot target cannot lower a clip <rect {other}>"),
                    }
                }
                let (Some(w), Some(h)) = (w, h) else {
                    bail!("clip <rect> without width/height");
                };
                Ok(format!("{{ r: [{x}, {y}, {w}, {h}] }}"))
            }
            "path" => {
                let mut parts = Vec::new();
                if let Some(s) = c.slot {
                    self.slots.insert(s);
                    if self.bound.contains(&s) {
                        parts.push(format!("s: {s}"));
                    }
                }
                for (k, val) in &c.attrs {
                    match k.as_str() {
                        "d" => parts.push(format!("d: {}", emit::js_string(val))),
                        "clip-rule" | "fill-rule" => {
                            if fill_rule_eo(val)? {
                                parts.push("eo: 1".to_string());
                            }
                        }
                        other => bail!("the skia-aot target cannot lower a clip <path {other}>"),
                    }
                }
                ensure!(!parts.is_empty(), "clip <path> without d");
                Ok(format!("{{ {} }}", parts.join(", ")))
            }
            other => bail!("the skia-aot target cannot lower a <{other}> clip shape"),
        }
    }

    /// `fill="url(#id)"` / `stroke="url(#id)"` → an inline gradient.
    ///
    /// A bound gradient (animated geometry, GRADIENT op) rides its slot as
    /// `s`; a bound stop (keyframed ramp, RAMP op) rides its slot as the
    /// third entry of its `st` triple. An animated-from-frame-`ip` attribute
    /// is not baked — same as shape geometry — so bound coordinates and stop
    /// attributes default (0 / black) until the first `apply` writes them.
    fn gradient_js(&mut self, v: &str) -> Result<String> {
        let def = self.lookup(v, "paint")?;
        let radial = match def.tag.as_str() {
            "radialGradient" => true,
            "linearGradient" => false,
            other => bail!("`{v}` names a <{other}>, not a gradient"),
        };
        let bound_self = def.slot.map(|s| self.bound.contains(&s)).unwrap_or(false);
        if let Some(s) = def.slot {
            self.slots.insert(s);
        }
        let mut coords: BTreeMap<&str, &str> = BTreeMap::new();
        let mut gt = None;
        let mut units = false;
        for (k, val) in &def.attrs {
            match k.as_str() {
                "id" => {}
                "gradientUnits" if val == "userSpaceOnUse" => units = true,
                "gradientTransform" => gt = Some(transform9(val)?),
                "cx" | "cy" | "r" if radial => {
                    coords.insert(k, num(val, k)?);
                }
                "x1" | "y1" | "x2" | "y2" if !radial => {
                    coords.insert(k, num(val, k)?);
                }
                other => bail!("the skia-aot target cannot lower <{} {other}>", def.tag),
            }
        }
        ensure!(units, "<{}> without gradientUnits=\"userSpaceOnUse\"", def.tag);
        let mut stops = Vec::new();
        for s in &def.children {
            ensure!(s.tag == "stop", "unexpected <{}> inside a gradient", s.tag);
            let bound_stop = s.slot.map(|s| self.bound.contains(&s)).unwrap_or(false);
            if let Some(s) = s.slot {
                self.slots.insert(s);
            }
            let (mut off, mut color) = (None, None);
            for (k, val) in &s.attrs {
                match k.as_str() {
                    "offset" => off = Some(num(val, "offset")?),
                    "stop-color" => color = Some(val.as_str()),
                    other => bail!("the skia-aot target cannot lower <stop {other}>"),
                }
            }
            let (off, color) = if bound_stop {
                (off.unwrap_or("0"), color.unwrap_or("#000"))
            } else {
                match (off, color) {
                    (Some(o), Some(c)) => (o, c),
                    _ => bail!("<stop> without offset/stop-color"),
                }
            };
            let slot = match s.slot {
                Some(s) if bound_stop => format!(", {s}"),
                _ => String::new(),
            };
            stops.push(format!("[{off}, {}{slot}]", emit::js_string(color)));
        }
        let at = |k: &str| -> Result<&str> {
            match coords.get(k).copied() {
                Some(v) => Ok(v),
                // The GRADIENT op writes every coordinate on its first apply.
                None if bound_self => Ok("0"),
                None => bail!("<{}> without `{k}`", def.tag),
            }
        };
        let s = match def.slot {
            Some(s) if bound_self => format!("s: {s}, "),
            _ => String::new(),
        };
        let head = if radial {
            format!("rad: 1, {s}cx: {}, cy: {}, r: {}", at("cx")?, at("cy")?, at("r")?)
        } else {
            format!(
                "lin: 1, {s}x1: {}, y1: {}, x2: {}, y2: {}",
                at("x1")?,
                at("y1")?,
                at("x2")?,
                at("y2")?
            )
        };
        let gt = gt.map(|m| format!(", gt: {m}")).unwrap_or_default();
        Ok(format!("{{ {head}, st: [{}]{gt} }}", stops.join(", ")))
    }

    /// `mask="url(#id)"` → `{ luma?: 1, c: [..] }`. The mask *region*
    /// (x/y/width/height) is dropped: the compiler either omits it (the SVG
    /// default region pads the consumer's bounding box, a no-op for content
    /// inside it) or writes the full viewport at origin — a nonzero origin
    /// would clip and is refused.
    fn mask_js(&mut self, v: &str, indent: usize) -> Result<String> {
        let def = self.lookup(v, "mask")?;
        ensure!(def.tag == "mask", "`{v}` does not name a <mask>");
        let mut luma = true;
        for (k, val) in &def.attrs {
            match (k.as_str(), val.as_str()) {
                ("id", _) => {}
                ("mask-type" | "maskType", "alpha") => luma = false,
                ("mask-type" | "maskType", "luminance") => luma = true,
                ("maskUnits", "userSpaceOnUse") => {}
                ("x", "0") | ("y", "0") => {}
                ("width" | "height", val) => {
                    num(val, k)?;
                }
                (other, val) => {
                    bail!("the skia-aot target cannot lower <mask {other}=\"{val}\">")
                }
            }
        }
        let mut out = String::new();
        out.push_str(if luma { "{ luma: 1, c: [\n" } else { "{ c: [\n" });
        self.list_js(&def.children, indent + 1, &mut out)?;
        out.push_str(&format!("{}] }}", "  ".repeat(indent)));
        Ok(out)
    }

    /// `filter="url(#id)"` → `cf: { m, r }` for the matte-inversion colour
    /// matrix (the one `filterUnits="userSpaceOnUse"` filter the compiler
    /// emits, already lowered to <feColorMatrix> by `lower_invert_filters`),
    /// or `fx: [stage, ..]` for a layer-effect chain.
    fn filter_js(&mut self, v: &str) -> Result<String> {
        let def = self.lookup(v, "filter")?;
        ensure!(def.tag == "filter", "`{v}` does not name a <filter>");
        if let Some(s) = def.slot {
            self.slots.insert(s);
        }
        if attr(def, "filterUnits") == Some("userSpaceOnUse") {
            return self.invert_filter_js(def);
        }
        self.fx_js(def)
    }

    fn invert_filter_js(&mut self, def: &'a Node) -> Result<String> {
        let mut r: BTreeMap<&str, &str> = BTreeMap::new();
        for (k, val) in &def.attrs {
            match k.as_str() {
                "id" => {}
                "filterUnits" if val == "userSpaceOnUse" => {}
                "x" | "y" | "width" | "height" => {
                    r.insert(k, num(val, k)?);
                }
                other => bail!("the skia-aot target cannot lower <filter {other}>"),
            }
        }
        let at = |k: &str| -> Result<&str> {
            r.get(k)
                .copied()
                .ok_or_else(|| anyhow::anyhow!("<filter> without `{k}`"))
        };
        ensure!(
            def.children.len() == 1 && def.children[0].tag == "feColorMatrix",
            "the skia-aot target only lowers a single <feColorMatrix> filter"
        );
        let (m, cif) = self.fe_matrix(&def.children[0])?;
        ensure!(cif.is_none(), "unexpected color-interpolation-filters on a matte inversion");
        Ok(format!(
            "cf: {{ m: [{m}], r: [{}, {}, {}, {}] }}",
            at("x")?,
            at("y")?,
            at("width")?,
            at("height")?
        ))
    }

    /// A layer-effect <filter> — the exact primitive chains
    /// `scene::build::emit_effects` writes (transcriptions of lottie-web's
    /// SVGFillFilter / SVGTintFilter / SVGDropShadowEffect /
    /// SVGGaussianBlurEffect) — as sequential `fx` stages. The percentage
    /// filter regions (shadow's `0%/100%` self-box clip, blur's widened
    /// `-100%/300%`) are objectBoundingBox-relative and are NOT reproduced:
    /// Skia layers are unclipped, so a shadow reaching past the element's
    /// own box draws instead of clipping — a documented divergence from
    /// lottie-web's quirk.
    ///
    /// The tint chain's `color-interpolation-filters="linearRGB"` is the
    /// discriminator that identifies its luminance matrix, and is then
    /// dropped: Skia has no per-filter colorspace, so that matrix runs in
    /// sRGB — the second documented divergence (see REPORT.md's skia-aot
    /// section). Visually close, not bit-equal.
    fn fx_js(&mut self, def: &'a Node) -> Result<String> {
        for (k, val) in &def.attrs {
            match k.as_str() {
                "id" => {}
                "x" | "y" | "width" | "height" if val.ends_with('%') => {}
                other => bail!("the skia-aot target cannot lower <filter {other}>"),
            }
        }
        let c = &def.children;
        let mut stages: Vec<String> = Vec::new();
        let mut i = 0;
        while i < c.len() {
            match c[i].tag.as_str() {
                "feColorMatrix" => {
                    let (m, cif) = self.fe_matrix(&c[i])?;
                    if cif.as_deref() == Some("linearRGB") {
                        // Tint: luma (t1), ramp (t2 = ramp∘luma), then
                        // feMerge(source, t1, t2) — three passes stacking
                        // over the untouched source.
                        ensure!(
                            i + 2 < c.len(),
                            "truncated tint chain in the skia-aot target"
                        );
                        let (m2, cif2) = self.fe_matrix(&c[i + 1])?;
                        ensure!(
                            cif2.as_deref() == Some("sRGB"),
                            "tint ramp matrix without sRGB interpolation"
                        );
                        self.fe_merge(&c[i + 2], 3)?;
                        stages.push(format!(
                            "[0, {{ cf: [{m}] }}, {{ cf2: [[{m2}], [{m}]] }}]"
                        ));
                        i += 3;
                    } else {
                        ensure!(
                            cif.as_deref() == Some("sRGB"),
                            "effect colour matrix without sRGB interpolation"
                        );
                        stages.push(format!("[{{ cf: [{m}] }}]"));
                        i += 1;
                    }
                }
                "feGaussianBlur" if attr(&c[i], "in") == Some("SourceAlpha") => {
                    ensure!(
                        i + 4 < c.len(),
                        "truncated drop-shadow chain in the skia-aot target"
                    );
                    stages.push(self.fe_shadow(&c[i], &c[i + 1], &c[i + 2], &c[i + 3], &c[i + 4])?);
                    i += 5;
                }
                "feGaussianBlur" => {
                    stages.push(self.fe_blur(&c[i])?);
                    i += 1;
                }
                other => bail!("the skia-aot target cannot lower <{other}>"),
            }
        }
        ensure!(!stages.is_empty(), "<filter> with no primitives");
        Ok(format!("fx: [{}]", stages.join(", ")))
    }

    /// One <feColorMatrix type="matrix">: its 20 values (validated, joined)
    /// and its color-interpolation-filters, if any. `result` names are
    /// bookkeeping for the SVG chain and drop away — the Skia stages chain
    /// positionally.
    fn fe_matrix(&mut self, fe: &'a Node) -> Result<(String, Option<&'a str>)> {
        ensure!(fe.tag == "feColorMatrix", "expected <feColorMatrix>, found <{}>", fe.tag);
        if let Some(s) = fe.slot {
            self.slots.insert(s);
        }
        let mut values = None;
        let mut cif = None;
        for (k, val) in &fe.attrs {
            match k.as_str() {
                "type" if val == "matrix" => {}
                "color-interpolation-filters" => cif = Some(val.as_str()),
                "values" => values = Some(val),
                "result" => {}
                other => bail!("the skia-aot target cannot lower <feColorMatrix {other}>"),
            }
        }
        let Some(values) = values else {
            bail!("<feColorMatrix> without values")
        };
        let m: Vec<&str> = values.split_ascii_whitespace().collect();
        ensure!(m.len() == 20, "color matrix with {} values", m.len());
        for v in &m {
            num(v, "values")?;
        }
        Ok((m.join(", "), cif))
    }

    /// A <feMerge> with exactly `n` <feMergeNode> children. The inputs it
    /// names are implied by the stage's pass order.
    fn fe_merge(&mut self, fe: &'a Node, n: usize) -> Result<()> {
        ensure!(fe.tag == "feMerge", "expected <feMerge>, found <{}>", fe.tag);
        if let Some(s) = fe.slot {
            self.slots.insert(s);
        }
        ensure!(
            fe.children.len() == n,
            "<feMerge> with {} inputs where {n} were expected",
            fe.children.len()
        );
        for c in &fe.children {
            ensure!(c.tag == "feMergeNode", "unexpected <{}> in <feMerge>", c.tag);
            if let Some(s) = c.slot {
                self.slots.insert(s);
            }
        }
        Ok(())
    }

    /// The five-primitive drop-shadow chain → one `MakeDropShadow` pass.
    /// `sd`/`dx`/`dy`/`fo` are absent when the FX ops write them (the
    /// primitive's slot rides as `sb`/`so`/`sf`).
    fn fe_shadow(
        &mut self,
        blur: &'a Node,
        off: &'a Node,
        flood: &'a Node,
        comp: &'a Node,
        merge: &'a Node,
    ) -> Result<String> {
        let mut parts = vec!["sh: 1".to_string()];
        if let Some(s) = self.prim_slot(blur) {
            parts.push(format!("sb: {s}"));
        }
        for (k, val) in &blur.attrs {
            match k.as_str() {
                "in" | "result" => {}
                "stdDeviation" => parts.push(format!("sd: {}", num(val, k)?)),
                other => bail!("the skia-aot target cannot lower <feGaussianBlur {other}>"),
            }
        }
        ensure!(off.tag == "feOffset", "expected <feOffset>, found <{}>", off.tag);
        if let Some(s) = self.prim_slot(off) {
            parts.push(format!("so: {s}"));
        }
        for (k, val) in &off.attrs {
            match k.as_str() {
                "in" | "result" => {}
                "dx" | "dy" => parts.push(format!("{k}: {}", num(val, k)?)),
                other => bail!("the skia-aot target cannot lower <feOffset {other}>"),
            }
        }
        ensure!(flood.tag == "feFlood", "expected <feFlood>, found <{}>", flood.tag);
        if let Some(s) = self.prim_slot(flood) {
            parts.push(format!("sf: {s}"));
        }
        let mut color = None;
        for (k, val) in &flood.attrs {
            match k.as_str() {
                "result" => {}
                "flood-color" => color = Some(val),
                "flood-opacity" => parts.push(format!("fo: {}", num(val, k)?)),
                other => bail!("the skia-aot target cannot lower <feFlood {other}>"),
            }
        }
        let Some(color) = color else {
            bail!("<feFlood> without flood-color")
        };
        parts.push(format!("c: {}", emit::js_string(color)));
        ensure!(comp.tag == "feComposite", "expected <feComposite>, found <{}>", comp.tag);
        if let Some(s) = comp.slot {
            self.slots.insert(s);
        }
        for (k, val) in &comp.attrs {
            match k.as_str() {
                "in" | "in2" | "result" => {}
                "operator" if val == "in" => {}
                other => bail!("the skia-aot target cannot lower <feComposite {other}>"),
            }
        }
        self.fe_merge(merge, 2)?;
        Ok(format!("[{{ {} }}]", parts.join(", ")))
    }

    /// A lone <feGaussianBlur> → one `MakeBlur` pass. `edgeMode` maps to a
    /// Skia `TileMode`: `wrap` → Repeat (1), `duplicate` → Clamp (0).
    fn fe_blur(&mut self, fe: &'a Node) -> Result<String> {
        let mut parts = vec!["bl: 1".to_string()];
        if let Some(s) = self.prim_slot(fe) {
            parts.push(format!("s: {s}"));
        }
        let mut tm = None;
        for (k, val) in &fe.attrs {
            match k.as_str() {
                "result" => {}
                "stdDeviation" => {
                    let p: Vec<&str> = val.split(' ').collect();
                    ensure!(p.len() == 2, "stdDeviation `{val}` is not an x/y pair");
                    parts.push(format!(
                        "sx: {}, sy: {}",
                        num(p[0], k)?,
                        num(p[1], k)?
                    ));
                }
                "edgeMode" => {
                    tm = Some(match val.as_str() {
                        "wrap" => 1,
                        "duplicate" => 0,
                        other => bail!("unsupported edgeMode `{other}`"),
                    });
                }
                other => bail!("the skia-aot target cannot lower <feGaussianBlur {other}>"),
            }
        }
        let Some(tm) = tm else {
            bail!("<feGaussianBlur> without edgeMode")
        };
        parts.push(format!("tm: {tm}"));
        Ok(format!("[{{ {} }}]", parts.join(", ")))
    }

    /// Register a filter primitive's slot; return it when a binding targets
    /// it (an animated effect parameter).
    fn prim_slot(&mut self, n: &Node) -> Option<usize> {
        let s = n.slot?;
        self.slots.insert(s);
        self.bound.contains(&s).then_some(s)
    }

    /// Children of a drawable, defs skipped (already indexed and resolved).
    fn list_js(&mut self, children: &[Node], indent: usize, out: &mut String) -> Result<()> {
        let drawn: Vec<&Node> = children
            .iter()
            .filter(|c| !DEF_TAGS.contains(&c.tag.as_str()))
            .collect();
        for (i, c) in drawn.iter().enumerate() {
            self.node_js(c, indent, out)?;
            out.push_str(if i + 1 < drawn.len() { ",\n" } else { "\n" });
        }
        Ok(())
    }

    fn node_js(&mut self, n: &Node, indent: usize, out: &mut String) -> Result<()> {
        if let Some(s) = n.slot {
            self.slots.insert(s);
        }
        let pad = "  ".repeat(indent);
        match n.tag.as_str() {
            "g" => {
                out.push_str(&pad);
                out.push_str("{ k: 0");
                if let Some(s) = n.slot
                    && self.bound.contains(&s)
                {
                    out.push_str(&format!(", s: {s}"));
                }
                for (k, v) in &n.attrs {
                    match k.as_str() {
                        "transform" => out.push_str(&format!(", m: {}", transform9(v)?)),
                        "opacity" => out.push_str(&format!(", o: {}", num(v, "opacity")?)),
                        "display" if v == "none" => out.push_str(", hd: 1"),
                        "style" => {
                            for decl in v.split(';').filter(|d| !d.is_empty()) {
                                match decl {
                                    // The viewport clip already rides the
                                    // flattened group's clip-path.
                                    "overflow:hidden" => {}
                                    "display:none" => out.push_str(", hd: 1"),
                                    other => match other.strip_prefix("mix-blend-mode:") {
                                        Some(kw) => out
                                            .push_str(&format!(", bm: {}", blend_mode(kw)?)),
                                        None => bail!(
                                            "unsupported style `{other}` in the skia-aot target"
                                        ),
                                    },
                                }
                            }
                        }
                        "clip-path" => out.push_str(&format!(", clip: {}", self.clip_js(v)?)),
                        "mask" => {
                            let m = self.mask_js(v, indent)?;
                            out.push_str(&format!(", mask: {m}"));
                        }
                        "filter" => {
                            let f = self.filter_js(v)?;
                            out.push_str(&format!(", {f}"));
                        }
                        other => bail!("the skia-aot target cannot lower <g {other}>"),
                    }
                }
                if n.children.iter().all(|c| DEF_TAGS.contains(&c.tag.as_str())) {
                    out.push_str(", c: [] }");
                } else {
                    out.push_str(", c: [\n");
                    self.list_js(&n.children, indent + 1, out)?;
                    out.push_str(&format!("{pad}] }}"));
                }
                Ok(())
            }
            "path" | "rect" | "ellipse" => self.shape_js(n, &pad, out),
            "image" => self.image_js(n, &pad, out),
            other => bail!("the skia-aot target cannot lower <{other}>"),
        }
    }

    /// An `<image>` layer — `scene::build_image` emits exactly four
    /// attributes, all static, so the descriptor is the decoded-at-mount
    /// payload alone. Only an embedded `data:*;base64,` source lowers: an
    /// external URL has no loader inside a self-contained worklet module.
    /// The base64 payload is validated to its alphabet so it embeds in a
    /// single-quoted JS string verbatim.
    fn image_js(&mut self, n: &Node, pad: &str, out: &mut String) -> Result<()> {
        ensure!(n.children.is_empty(), "unexpected children inside <image>");
        let (mut w, mut h, mut u) = (None, None, None);
        for (key, v) in &n.attrs {
            match key.as_str() {
                "width" => w = Some(num(v, "width")?),
                "height" => h = Some(num(v, "height")?),
                // lottie-web's image fit; with the box at the asset's
                // natural size the runtime's center-crop reproduces it.
                "preserveAspectRatio" => {
                    ensure!(
                        v == "xMidYMid slice",
                        "unsupported preserveAspectRatio `{v}` in the skia-aot target"
                    );
                }
                "href" => {
                    let b64 = v
                        .strip_prefix("data:")
                        .and_then(|rest| rest.split_once(";base64,"))
                        .map(|(_, b)| b);
                    match b64 {
                        Some(b)
                            if !b.is_empty()
                                && b.bytes().all(|c| {
                                    c.is_ascii_alphanumeric() || matches!(c, b'+' | b'/' | b'=')
                                }) =>
                        {
                            u = Some(b);
                        }
                        _ => bail!(
                            "the skia-aot target cannot draw a non-embedded image source"
                        ),
                    }
                }
                other => bail!("the skia-aot target cannot lower <image {other}>"),
            }
        }
        let (Some(w), Some(h), Some(u)) = (w, h, u) else {
            bail!("<image> without width/height/href");
        };
        out.push_str(&format!("{pad}{{ k: 4, w: {w}, h: {h}, u: '{u}' }}"));
        Ok(())
    }

    fn shape_js(&mut self, n: &Node, pad: &str, out: &mut String) -> Result<()> {
        ensure!(
            n.children.is_empty(),
            "unexpected children inside <{}>",
            n.tag
        );
        let k = match n.tag.as_str() {
            "path" => 1,
            "rect" => 2,
            _ => 3,
        };
        let mut head: Vec<String> = vec![format!("k: {k}")];
        if let Some(s) = n.slot
            && self.bound.contains(&s)
        {
            head.push(format!("s: {s}"));
        }
        // Geometry defaults per SVG (x/y/cx/cy default 0).
        let mut geo: BTreeMap<&str, String> = BTreeMap::new();
        let mut paint: Vec<String> = Vec::new();
        let mut fill: Option<String> = None; // None = default black
        let mut eo = false;
        for (key, v) in &n.attrs {
            match key.as_str() {
                "transform" => head.push(format!("m: {}", transform9(v)?)),
                "opacity" => head.push(format!("o: {}", num(v, "opacity")?)),
                "display" if v == "none" => head.push("hd: 1".to_string()),
                "style" => {
                    for decl in v.split(';').filter(|d| !d.is_empty()) {
                        match decl {
                            "display:none" => head.push("hd: 1".to_string()),
                            other => {
                                bail!("unsupported style `{other}` in the skia-aot target")
                            }
                        }
                    }
                }
                "d" if k == 1 => head.push(format!("d: {}", emit::js_string(v))),
                "fill-rule" | "clip-rule" => eo = fill_rule_eo(v)?,
                "x" | "y" if k == 2 => {
                    geo.insert(key, num(v, key)?.to_string());
                }
                "width" if k == 2 => {
                    geo.insert("w", num(v, key)?.to_string());
                }
                "height" if k == 2 => {
                    geo.insert("h", num(v, key)?.to_string());
                }
                "rx" | "ry" if k >= 2 => {
                    geo.insert(key, num(v, key)?.to_string());
                }
                "cx" | "cy" if k == 3 => {
                    geo.insert(key, num(v, key)?.to_string());
                }
                "fill" => {
                    fill = Some(if v == "none" {
                        String::new()
                    } else if v.starts_with("url(") {
                        format!("f: {}", self.gradient_js(v)?)
                    } else {
                        format!("f: {}", emit::js_string(v))
                    });
                }
                "fill-opacity" => paint.push(format!("fo: {}", num(v, key)?)),
                "stroke" => {
                    if v != "none" {
                        paint.push(if v.starts_with("url(") {
                            format!("sc: {}", self.gradient_js(v)?)
                        } else {
                            format!("sc: {}", emit::js_string(v))
                        });
                    }
                }
                "stroke-opacity" => paint.push(format!("so: {}", num(v, key)?)),
                "stroke-width" => paint.push(format!("sw: {}", num(v, key)?)),
                "stroke-linecap" => match v.as_str() {
                    "butt" => {}
                    "round" => paint.push("cap: 1".to_string()),
                    "square" => paint.push("cap: 2".to_string()),
                    other => bail!("unsupported stroke-linecap `{other}`"),
                },
                "stroke-linejoin" => match v.as_str() {
                    "miter" => {}
                    "round" => paint.push("join: 1".to_string()),
                    "bevel" => paint.push("join: 2".to_string()),
                    other => bail!("unsupported stroke-linejoin `{other}`"),
                },
                "stroke-miterlimit" => paint.push(format!("ml: {}", num(v, key)?)),
                "stroke-dasharray" => {
                    let nums: Result<Vec<&str>> =
                        v.split(' ').map(|p| num(p, "stroke-dasharray")).collect();
                    paint.push(format!("da: [{}]", nums?.join(", ")));
                }
                "stroke-dashoffset" => paint.push(format!("doff: {}", num(v, key)?)),
                "paint-order" => {
                    ensure!(v == "stroke", "unsupported paint-order `{v}`");
                    paint.push("po: 1".to_string());
                }
                other => bail!("the skia-aot target cannot lower <{} {other}>", n.tag),
            }
        }
        if eo {
            head.push("eo: 1".to_string());
        }
        // Geometry attributes are optional: an attribute animated from frame
        // `ip` is not baked (the first `apply` writes it), and the SVG
        // defaults are 0 — a zero-extent shape draws nothing until then,
        // matching the web/rn behavior of an attribute-less element.
        match k {
            1 => {}
            2 => {
                for key in ["x", "y", "w", "h"] {
                    geo.entry(key).or_insert_with(|| "0".to_string());
                }
            }
            _ => {
                for key in ["cx", "cy", "rx", "ry"] {
                    geo.entry(key).or_insert_with(|| "0".to_string());
                }
            }
        }
        for (key, v) in &geo {
            head.push(format!("{key}: {v}"));
        }
        // SVG paints black when `fill` is absent; `fill="none"` omits `f`.
        match fill {
            None => paint.insert(0, "f: '#000'".to_string()),
            Some(f) if f.is_empty() => {}
            Some(f) => paint.insert(0, f),
        }
        out.push_str(pad);
        out.push_str(&format!(
            "{{ {}, paint: {{ {} }} }}",
            head.join(", "),
            paint.join(", ")
        ));
        Ok(())
    }
}

fn attr<'n>(n: &'n Node, key: &str) -> Option<&'n str> {
    n.attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

/// A CSS `mix-blend-mode` keyword (the 15 the web emitter writes for Lottie
/// `bm` 1–15) → the Skia `BlendMode` enum value.
fn blend_mode(kw: &str) -> Result<u32> {
    Ok(match kw {
        "multiply" => 24,
        "screen" => 14,
        "overlay" => 15,
        "darken" => 16,
        "lighten" => 17,
        "color-dodge" => 18,
        "color-burn" => 19,
        "hard-light" => 20,
        "soft-light" => 21,
        "difference" => 22,
        "exclusion" => 23,
        "hue" => 25,
        "saturation" => 26,
        "color" => 27,
        "luminosity" => 28,
        other => bail!("unsupported mix-blend-mode `{other}`"),
    })
}

fn fill_rule_eo(v: &str) -> Result<bool> {
    match v {
        "evenodd" => Ok(true),
        "nonzero" => Ok(false),
        other => bail!("unsupported fill rule `{other}`"),
    }
}

/// The restructured markup → the display-list literal plus the element count.
/// Factored off `emit_skia` so tests can lower markup directly.
fn dl_from_markup(markup: &str, bound: &BTreeSet<usize>) -> Result<(String, usize)> {
    let mut root = rn::parse_markup(markup)?;
    ensure!(root.tag == "svg", "the document root is <{}>", root.tag);
    let mut count = 0usize;
    rn::assign_slots(&mut root, true, &mut count);
    rn::lower_invert_filters(&mut root)?;
    rn::flatten_nested_svgs(&mut root)?;

    // Root sanity: the draw walker assumes the design space starts at the
    // origin (the player's fit matrix and the viewport clips do too).
    for (k, v) in &root.attrs {
        match k.as_str() {
            "viewBox" => ensure!(
                v.starts_with("0 0 "),
                "viewBox `{v}` does not start at the origin"
            ),
            "width" | "height" => {}
            "preserveAspectRatio" => ensure!(
                v == "xMidYMid meet",
                "unsupported preserveAspectRatio `{v}` (the player fits xMidYMid meet)"
            ),
            "xmlns" => {}
            "style" => {
                for decl in v.split(';').filter(|d| !d.is_empty()) {
                    ensure!(decl == "overflow:hidden", "unsupported root style `{decl}`");
                }
            }
            other => bail!("the skia-aot target cannot lower <svg {other}>"),
        }
    }

    let mut defs = BTreeMap::new();
    index_defs(&root, &mut defs);
    let mut lower = Lower {
        defs: &defs,
        bound,
        slots: BTreeSet::new(),
    };
    let mut dl = String::new();
    dl.push_str("{ k: 0, c: [\n");
    lower.list_js(&root.children, 1, &mut dl)?;
    dl.push_str("] }");
    for b in bound {
        ensure!(
            lower.slots.contains(b),
            "binding addresses element {b}, which is not a drawable node (defs subtree or past the tree)"
        );
    }
    Ok((dl, count))
}

// ---------------------------------------------------------------------------
// Module assembly.
// ---------------------------------------------------------------------------

fn emit_skia(scene: &Scene) -> Result<(String, usize)> {
    debug_assert!(scene.data.assets.is_empty(), "skia scenes are never instanced");
    debug_assert!(scene.data.uses.is_empty(), "skia scenes are never instanced");
    debug_assert!(scene.data.tpl.is_empty(), "skia scenes are fully inlined");

    let bound: BTreeSet<usize> = scene.data.b.iter().map(|b| b.el_index as usize).collect();
    let (dl, count) = dl_from_markup(&scene.markup, &bound)?;

    // `meta` wants the design size in numbers; the baked width/height may be
    // "100%", so non-numeric values fall back to the viewBox extent (always
    // written from the design size). Same fallback as the rn target.
    let root = rn::parse_markup(&scene.markup)?;
    let attr = |name: &str| {
        root.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };
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

    let n = |v: f64| crate::scene::svg::n(v);
    let mut src = String::with_capacity(scene.markup.len() * 2 + 4096);
    src.push_str(&format!(
        "// Generated by ulottie — skia-aot target (@shopify/react-native-skia + react-native-reanimated).\n\
         // Deliberately unminified: Metro's reanimated plugin reads the 'worklet' directives\n\
         // before its own minifier runs; a minifier here could strip them.\n\
         // caps: {}\n\n",
        emit::caps_list(scene.caps),
    ));

    if scene.is_static() {
        // Static still draws: the display list is recorded once and never
        // re-applied, so only the prepare/draw half of the runtime ships.
        let r = ["skPrepare", "skDraw"];
        src.push_str(&format!("// runtime symbols: {}\n", symbols(&r, scene.caps)));
        src.push_str(&bundle(&r, scene.caps));
        src.push('\n');
    } else {
        src.push_str(&format!(
            "// runtime symbols: {}\n",
            symbols(&roots(scene.caps), scene.caps)
        ));
        src.push_str(&bundle(&roots(scene.caps), scene.caps));
        src.push_str(&rn::program_rn(0, &scene.data.b));
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

    src.push_str("export const dl =\n");
    src.push_str(&dl);
    src.push_str(";\n\n");
    src.push_str(&format!(
        "export const meta = {{ fr: {}, ip: {}, op: {}, width: {width}, height: {height} }};\n",
        n(scene.data.fr),
        n(scene.data.ip),
        n(scene.data.op),
    ));

    if scene.is_static() {
        src.push_str(&format!(
            "export const init = (Sk) => {{ 'worklet'; const x = skPrepare(dl, [], Sk); return {{ els: [], dirty: [], apply: function () {{ 'worklet'; }}, draw: function (c) {{ skDraw(c, x, Sk); }}, fr: {}, ip: {}, op: {} }}; }};\n",
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
            "null".to_string()
        } else {
            format!("{{ {} }}", ext.join(", "))
        };
        src.push_str(&format!(
            "export const init = (Sk) => {{ 'worklet'; return mountSkia(D, P0, A0, {count}, {ext}, Sk, dl); }};\n"
        ));
    }
    Ok((src, count))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transforms_become_row_major_matrices() {
        assert_eq!(
            transform9("matrix(.5,0,0,.5,10,-2.5)").unwrap(),
            "[.5, 0, 10, 0, .5, -2.5, 0, 0, 1]"
        );
        assert_eq!(
            transform9("translate(4,-8)").unwrap(),
            "[1, 0, 4, 0, 1, -8, 0, 0, 1]"
        );
        assert!(transform9("rotate(45)").is_err());
    }

    #[test]
    fn the_skia_names_do_not_collide_with_web_declarations() {
        let web: BTreeSet<String> = emit::all_declarations()
            .into_iter()
            .map(|d| d.name)
            .collect();
        // The substituted set must be a flat scope of unique names.
        let mut names = BTreeSet::new();
        for d in skia_declarations() {
            assert!(names.insert(d.name.clone()), "duplicate declaration `{}`", d.name);
        }
        // Overrides replace a web declaration; additions must be new names.
        for name in ["put", "oDisplay", "oRect", "trim", "kzero"] {
            assert!(web.contains(name), "override `{name}` has no web original");
        }
        for name in [
            "rput", "skSet", "skMatrix", "skNums", "skGeom", "skPaints", "skGrad",
            "skOwn", "skFxPaint", "skGradRec", "skFx",
            "skPrepare", "skDraw", "skShape", "skGeomDraw", "mountRn", "mountSkia",
        ] {
            assert!(!web.contains(name), "`{name}` collides with a web name");
            assert!(names.contains(name), "`{name}` is not declared");
        }
    }

    #[test]
    fn the_lowering_resolves_refs_inline() {
        let markup = r##"<svg viewBox="0 0 10 10" width="100%" height="100%" preserveAspectRatio="xMidYMid meet"><defs><clipPath id="c0"><rect width="10" height="10"/></clipPath><radialGradient id="g0" gradientUnits="userSpaceOnUse" cx="1" cy="2" r="3"><stop offset="0" stop-color="#fff"/><stop offset="1" stop-color="#000"/></radialGradient></defs><g clip-path="url(#c0)"><path d="M0 0L1 1" fill="url(#g0)" stroke="#f00" stroke-width="2" paint-order="stroke"/></g></svg>"##;
        // Slots number every descendant in document order, defs included:
        // defs 0, clipPath 1, rect 2, gradient 3, stops 4/5, g 6, path 7.
        let (dl, count) = dl_from_markup(markup, &BTreeSet::from([7usize])).unwrap();
        assert_eq!(count, 8);
        assert!(dl.contains("clip: { r: [0, 0, 10, 10] }"), "{dl}");
        assert!(dl.contains("rad: 1, cx: 1, cy: 2, r: 3"), "{dl}");
        assert!(dl.contains("st: [[0, '#fff'], [1, '#000']]"), "{dl}");
        assert!(dl.contains("po: 1"), "{dl}");
        assert!(dl.contains("s: 7"), "{dl}");
        // No url() survives lowering.
        assert!(!dl.contains("url("), "{dl}");
    }

    #[test]
    fn unmapped_constructs_bail_with_named_findings() {
        let circle = r#"<svg viewBox="0 0 10 10"><circle cx="1" cy="1" r="1"/></svg>"#;
        let err = dl_from_markup(circle, &BTreeSet::new()).unwrap_err().to_string();
        assert!(err.contains("<circle>"), "{err}");

        let bound_in_defs = r#"<svg viewBox="0 0 10 10"><defs><clipPath id="c0"><rect width="1" height="1"/></clipPath></defs><g clip-path="url(#c0)"><path d="M0 0"/></g></svg>"#;
        // Slot 1 is the clipPath — not a drawable node.
        let err = dl_from_markup(bound_in_defs, &BTreeSet::from([1usize]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("not a drawable"), "{err}");
    }

    #[test]
    fn a_shape_without_fill_paints_svg_default_black() {
        let markup = r##"<svg viewBox="0 0 10 10"><path d="M0 0"/><rect x="1" y="1" width="2" height="2" fill="none" stroke="#00f"/></svg>"##;
        let (dl, _) = dl_from_markup(markup, &BTreeSet::new()).unwrap();
        assert!(dl.contains("paint: { f: '#000' }"), "{dl}");
        assert!(dl.contains("paint: { sc: '#00f' }"), "{dl}");
    }
}
