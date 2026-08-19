//! What the compiler does not implement yet, and finding it before it silently
//! changes how an animation looks.
//!
//! The typed AST is lossy on purpose — an unrecognised shape deserializes to
//! `GraphicElement::Unknown` and a `tt` field simply is not read — so a check
//! written against it cannot see what it dropped. This walks the raw JSON
//! instead, which is the only representation that still has everything.
//!
//! Detected features are rejected by default. A caller that knows an animation
//! degrades acceptably can allow specific ones; nothing is ever dropped
//! silently.

use std::collections::BTreeSet;
use std::fmt;

use serde_json::Value;

/// A Lottie capability this compiler does not implement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Feature {
    TextNoChars,
    TextAnimated,
    TextAnimators,
    TextBox,
    TextStroke,
    TextPath,
    TextGlyphMissing,
    UnknownLayerType,
    TrackMatte,
    BlendMode,
    TimeRemap,
    AutoOrient,
    ThreeD,
    MaskMode,
    MaskInverted,
    Repeater,
    MergePaths,
    RoundedCorners,
    OffsetPath,
    PuckerBloat,
    ZigZag,
    UnknownShape,
    AnimatedGradient,
    ImageAsset,
    LayerEffect,
}

impl Feature {
    /// Stable name used on the command line and in the fixture manifest.
    pub fn name(self) -> &'static str {
        use Feature::*;
        match self {
            TextNoChars => "text-no-chars",
            TextAnimated => "text-animated",
            TextAnimators => "text-animators",
            TextBox => "text-box",
            TextStroke => "text-stroke",
            TextPath => "text-path",
            TextGlyphMissing => "text-glyph-missing",
            UnknownLayerType => "unknown-layer-type",
            TrackMatte => "track-matte",
            BlendMode => "blend-mode",
            TimeRemap => "time-remap",
            AutoOrient => "auto-orient",
            ThreeD => "3d",
            MaskMode => "mask-mode",
            MaskInverted => "mask-inverted",
            Repeater => "repeater",
            MergePaths => "merge-paths",
            RoundedCorners => "rounded-corners",
            OffsetPath => "offset-path",
            PuckerBloat => "pucker-bloat",
            ZigZag => "zig-zag",
            UnknownShape => "unknown-shape",
            AnimatedGradient => "animated-gradient",
            ImageAsset => "image-asset",
            LayerEffect => "layer-effect",
        }
    }

    /// What ignoring it does to the render.
    pub fn effect(self) -> &'static str {
        use Feature::*;
        match self {
            TextNoChars => "the text is not drawn (no glyph outlines)",
            TextAnimated => "the text is drawn as its first keyframe",
            TextAnimators => "the text is drawn without its animators",
            TextBox => "the text is drawn without wrapping to its box",
            TextStroke => "the text is drawn without its stroke",
            TextPath => "the text is drawn on a straight baseline",
            TextGlyphMissing => "the text is not drawn (a character has no outline)",
            UnknownLayerType => "the layer is not drawn",
            TrackMatte => "the masked layer draws unmasked",
            BlendMode => "the layer composites normally",
            TimeRemap => "the time remap is ignored and the layer plays linearly",
            AutoOrient => "the layer keeps its authored rotation",
            ThreeD => "the layer is drawn flat",
            MaskMode => "the mask is treated as Add",
            MaskInverted => "the inversion is approximated by subtracting",
            Repeater => "only the original copy is drawn",
            MergePaths => "the paths are drawn separately",
            RoundedCorners => "corners stay sharp",
            OffsetPath => "the path is not offset",
            PuckerBloat => "the distortion is not applied",
            ZigZag => "the path is not zig-zagged",
            UnknownShape => "the shape is not drawn",
            AnimatedGradient => "the gradient ramp is read as static and may render as NaN",
            ImageAsset => "the image has no source to draw from",
            LayerEffect => "the layer is drawn without the effect",
        }
    }

    pub fn from_name(s: &str) -> Option<Feature> {
        use Feature::*;
        const ALL: &[Feature] = &[
            TextNoChars,
            TextAnimated,
            TextAnimators,
            TextBox,
            TextStroke,
            TextPath,
            TextGlyphMissing,
            UnknownLayerType,
            TrackMatte,
            BlendMode,
            TimeRemap,
            AutoOrient,
            ThreeD,
            MaskMode,
            MaskInverted,
                    Repeater,
            MergePaths,
            RoundedCorners,
            OffsetPath,
            PuckerBloat,
            ZigZag,
            UnknownShape,
            AnimatedGradient,
            ImageAsset,
            LayerEffect,
        ];
        ALL.iter().copied().find(|f| f.name() == s)
    }
}

impl fmt::Display for Feature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// One occurrence, with enough context to find it in the source.
#[derive(Debug, Clone)]
pub struct Finding {
    pub feature: Feature,
    pub location: String,
}

/// Shape types the backend renders. Anything else is reported.
const KNOWN_SHAPES: &[&str] = &[
    "gr", "sh", "el", "rc", "sr", "tr", "fl", "st", "gf", "gs", "tm",
];

/// Effect types that change the picture — the ones lottie-web builds an SVG
/// filter for, from its `registerEffect` table. Everything else in an `ef` list
/// is a slider or a checkbox that only an expression reads, and reporting those
/// would bury the real findings.
const RENDERING_EFFECTS: &[u64] = &[
    20, // tint
    21, // fill — implemented
    22, // stroke
    23, // tritone
    24, // pro levels
    25, // drop shadow
    28, // matte3
    29, // gaussian blur
    35, // transform
];

/// Walk a Lottie document and report everything the compiler would ignore.
pub fn scan(doc: &Value) -> Vec<Finding> {
    let mut out = Vec::new();
    // Text layers lower against the document's glyph outlines; parse them
    // once so every layer scan sees the same table the lowering will.
    let chars: Vec<crate::lottie::GlyphChar> = doc
        .get("chars")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|c| serde_json::from_value(c.clone()).ok())
                .collect()
        })
        .unwrap_or_default();
    let fonts: Vec<crate::lottie::Font> = doc
        .get("fonts")
        .and_then(|f| f.get("list"))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|f| serde_json::from_value(f.clone()).ok())
                .collect()
        })
        .unwrap_or_default();
    let text_ctx = (chars, fonts);
    if let Some(layers) = doc.get("layers").and_then(Value::as_array) {
        scan_layers(layers, "layers", &mut out, &text_ctx);
    }
    for (i, asset) in doc
        .get("assets")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .enumerate()
    {
        let id = asset.get("id").and_then(Value::as_str).unwrap_or("?");
        let where_ = format!("assets[{i}] `{id}`");
        // An image asset is drawn from `p` — a data URI when `e` is set, a
        // filename to hang off `u` otherwise. Without it there is no source to
        // point `<image>` at, and only then is there nothing to be done.
        if asset.get("layers").is_none()
            && asset.get("p").and_then(Value::as_str).unwrap_or("").is_empty()
        {
            push(&mut out, Feature::ImageAsset, &where_);
        }
        if let Some(layers) = asset.get("layers").and_then(Value::as_array) {
            scan_layers(layers, &where_, &mut out, &text_ctx);
        }
    }
    out.sort_by(|a, b| a.feature.cmp(&b.feature).then(a.location.cmp(&b.location)));
    out
}

fn push(out: &mut Vec<Finding>, feature: Feature, location: &str) {
    out.push(Finding {
        feature,
        location: location.to_string(),
    });
}

/// Whether a Lottie flag is set.
///
/// Lottie is inconsistent about how it spells one: `ddd` and `bm` are numbers,
/// `inv` is a JSON boolean. Reading only `as_f64` meant every boolean flag read
/// as unset, which is why the mask-inversion check below silently never fired.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        other => other.and_then(Value::as_f64).map(|n| n != 0.0).unwrap_or(false),
    }
}

fn scan_layers(
    layers: &[Value],
    where_: &str,
    out: &mut Vec<Finding>,
    text: &(Vec<crate::lottie::GlyphChar>, Vec<crate::lottie::Font>),
) {
    for (i, l) in layers.iter().enumerate() {
        let name = l.get("nm").and_then(Value::as_str).unwrap_or("");
        let at = if name.is_empty() {
            format!("{where_}[{i}]")
        } else {
            format!("{where_}[{i}] `{name}`")
        };

        match l.get("ty").and_then(Value::as_u64) {
            Some(0) | Some(1) | Some(2) | Some(3) | Some(4) => {}
            // Audio (9) has nothing to draw; not drawing it is the correct
            // render, not a degradation.
            Some(9) => {}
            // Text layers are implemented for a static document against
            // embedded glyph outlines; the same `text_shapes` call decides
            // support here and in the lowering, so the two cannot disagree.
            Some(5) => {
                let t: Option<crate::lottie::TextData> = l
                    .get("t")
                    .and_then(|t| serde_json::from_value(t.clone()).ok());
                match t {
                    Some(t) => {
                        if let Err(refusal) =
                            crate::lottie::text_shapes(&t, &text.0, &text.1)
                        {
                            let feature = match refusal {
                                crate::lottie::TextRefusal::NoChars => Feature::TextNoChars,
                                crate::lottie::TextRefusal::AnimatedDocument => {
                                    Feature::TextAnimated
                                }
                                crate::lottie::TextRefusal::Animators => Feature::TextAnimators,
                                crate::lottie::TextRefusal::Box => Feature::TextBox,
                                crate::lottie::TextRefusal::Stroke => Feature::TextStroke,
                                crate::lottie::TextRefusal::Path => Feature::TextPath,
                                crate::lottie::TextRefusal::GlyphMissing(_) => {
                                    Feature::TextGlyphMissing
                                }
                            };
                            push(out, feature, &at);
                        }
                    }
                    None => push(out, Feature::TextNoChars, &at),
                }
            }
            _ => push(out, Feature::UnknownLayerType, &at),
        }

        // Track mattes are implemented for the four AE modes (alpha, alpha
        // inverted, luma, luma inverted). `tt: 0` is "no matte", written
        // explicitly by some exporters; anything past 4 would be read and
        // dropped, so it still counts as unsupported.
        if let Some(tt) = l.get("tt").and_then(Value::as_u64)
            && tt > 4 {
                push(out, Feature::TrackMatte, &at);
            }
        // Blend modes 1–15 are implemented (CSS `mix-blend-mode`, the same
        // keywords lottie-web writes); `bm: 0` is normal and exports write it
        // explicitly. Anything else on `bm` is read and dropped, which is
        // what the refusal is for.
        // A blend mode past 15 has no CSS keyword; lottie-web's
        // `blendModeEnums[mode] || ''` composites it normally, and so does
        // the emitter here.
        // `sr` is not scanned: lottie-web applies time-stretch only at the
        // precomp boundary (`renderedFrame = num / sr` in `CompElement`) —
        // bodymovin exports every other layer's keyframes already stretched
        // into composition time. The precomp case is implemented (the clock
        // row divides by its rate); everywhere else the field is inert.
        if truthy(l.get("ao")) {
            push(out, Feature::AutoOrient, &at);
        }
        if truthy(l.get("ddd")) {
            push(out, Feature::ThreeD, &at);
        }
        // Time remap is implemented, but only where Lottie defines it: on a
        // precomp layer, whose clock it replaces. Anywhere else it would be
        // read and dropped, which is exactly what this scan exists to catch.
        if l.get("tm").is_some() && l.get("ty").and_then(Value::as_u64) != Some(0) {
            push(out, Feature::TimeRemap, &at);
        }

        if let Some(ks) = l.get("ks") {
            if ks.get("rx").is_some() || ks.get("ry").is_some() || ks.get("rz").is_some() {
                push(out, Feature::ThreeD, &at);
            }
        }

        for m in l
            .get("masksProperties")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[])
        {
            // The combination now mirrors lottie-web's `MaskElement`: `a`
            // adds, `s` subtracts, `i` intersects, `n` draws nothing, and
            // every other mode paints white the way lottie-web's untested
            // branch does — so only a mode outside that set is worth a word.
            // Inversion is the composition rect plus the contour in one `d`,
            // which needs the contour at compile time: only an *animated*
            // inverted path is still a refusal. Mask opacity rides the
            // colourless fill op.
            match m.get("mode").and_then(Value::as_str) {
                Some("a") | Some("s") | Some("i") | Some("n") | Some("f") | None => {}
                Some(_) => push(out, Feature::MaskMode, &at),
            }
            if truthy(m.get("inv"))
                && m
                    .get("pt")
                    .and_then(|p| p.get("a"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    == 1
            {
                push(out, Feature::MaskInverted, &at);
            }
        }

        // Effects that draw. Most of an `ef` list is inert — sliders and
        // checkboxes an expression reads — and reporting those would bury the
        // list in noise, so this asks only about the types lottie-web hands to
        // a filter. `ADBE Fill` is implemented; the rest change the picture and
        // were being dropped without a word.
        for e in l
            .get("ef")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[])
        {
            let Some(ty) = e.get("ty").and_then(Value::as_u64) else {
                continue;
            };
            // 20 tint, 21 fill, 25 drop shadow and 29 gaussian blur are
            // implemented; anything lottie-web never registered is skipped
            // the way the reference skips it.
            if !RENDERING_EFFECTS.contains(&ty) || matches!(ty, 20 | 21 | 25 | 29) {
                continue;
            }
            let nm = e.get("nm").and_then(Value::as_str).unwrap_or("");
            push(out, Feature::LayerEffect, &format!("{at} effect `{nm}`"));
        }

        if let Some(shapes) = l.get("shapes").and_then(Value::as_array) {
            scan_shapes(shapes, &at, out);
        }
    }
}

fn scan_shapes(shapes: &[Value], where_: &str, out: &mut Vec<Finding>) {
    for (i, s) in shapes.iter().enumerate() {
        let ty = s.get("ty").and_then(Value::as_str).unwrap_or("");
        if !KNOWN_SHAPES.contains(&ty) {
            let feature = match ty {
                // A static repeater expands at lowering; the same `expand`
                // decides here, so the gate and the compiler agree. Parsing
                // the slice only when an `rp` is present keeps the common
                // walk on raw JSON.
                "rp" => {
                    let parsed: Option<Vec<crate::lottie::GraphicElement>> =
                        serde_json::from_value(serde_json::Value::Array(shapes.to_vec())).ok();
                    let expandable = parsed
                        .as_ref()
                        .and_then(|items| crate::lottie::repeat::expand(items, i))
                        .is_some();
                    if expandable {
                        continue;
                    }
                    Feature::Repeater
                }
                // Merge mode 1 is plain concatenation into one composite
                // path — which is already how shapes render: every contour a
                // style paints lands in that style's one element, so their
                // windings interact exactly as the composite's would. The
                // boolean modes (2 add, 3 subtract, 4 intersect, 5 exclude)
                // change geometry and stay refusals — lottie-web does not
                // render them either.
                "mm" if s.get("mm").and_then(Value::as_u64).unwrap_or(1) == 1 => continue,
                "mm" => Feature::MergePaths,
                "rd" => Feature::RoundedCorners,
                "op" => Feature::OffsetPath,
                "pb" => Feature::PuckerBloat,
                "zz" => Feature::ZigZag,
                _ => Feature::UnknownShape,
            };
            push(out, feature, &format!("{where_} shape `{ty}`"));
        }
        // A keyframed colour ramp is planned as one binding per `<stop>`.
        // What that cannot represent is a ramp that also carries alpha stops:
        // those sit at positions of their own, so one set of stop elements
        // cannot follow both once either set moves. `animated_ramp` is the
        // same test, and answering it here from the raw JSON keeps the
        // rejection honest about which ramps are actually covered.
        if let Some(g) = s.get("g")
            && g.get("k").and_then(|k| k.get("a")).and_then(Value::as_u64) == Some(1)
            && crate::eval::gradient::animated_ramp(g).is_none()
        {
            push(out, Feature::AnimatedGradient, where_);
        }
        if let Some(items) = s.get("it").and_then(Value::as_array) {
            scan_shapes(items, where_, out);
        }
    }
}



/// Findings not covered by `allow`, formatted as a build error.
pub fn reject(findings: &[Finding], allow: &BTreeSet<Feature>) -> Option<String> {
    let blocking: Vec<&Finding> = findings
        .iter()
        .filter(|f| !allow.contains(&f.feature))
        .collect();
    if blocking.is_empty() {
        return None;
    }
    let mut seen: BTreeSet<Feature> = BTreeSet::new();
    let mut msg = String::from("unsupported Lottie features:\n");
    for f in &blocking {
        msg.push_str(&format!("  {:<20} {}\n", f.feature.name(), f.location));
        seen.insert(f.feature);
    }
    msg.push_str("\nIgnoring them would change the render:\n");
    for f in &seen {
        msg.push_str(&format!("  {:<20} {}\n", f.name(), f.effect()));
    }
    let names: Vec<&str> = seen.iter().map(|f| f.name()).collect();
    msg.push_str(&format!(
        "\nImplement them, or accept the degradation explicitly with\n  --allow {}\n",
        names.join(",")
    ));
    Some(msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_plain_shape_layer_is_clean() {
        let doc = json!({
            "layers": [{ "ty": 4, "ks": {}, "shapes": [{ "ty": "gr", "it": [{ "ty": "sh" }] }] }]
        });
        assert!(scan(&doc).is_empty());
    }

    #[test]
    fn a_finding_is_reported_with_its_layer() {
        let doc = json!({ "layers": [{ "ty": 4, "nm": "O", "ddd": 1, "ks": {} }] });
        let found = scan(&doc);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].feature, Feature::ThreeD);
        assert!(found[0].location.contains("`O`"), "{}", found[0].location);
    }

    #[test]
    fn the_four_implemented_matte_modes_are_not_reported() {
        for tt in 1..=4 {
            let doc = json!({ "layers": [
                { "ty": 4, "td": 1, "ks": {} },
                { "ty": 4, "tt": tt, "ks": {} },
            ] });
            assert!(scan(&doc).is_empty(), "tt={tt} should be supported");
        }
        // A mode outside the range would be read and dropped.
        let doc = json!({ "layers": [{ "ty": 4, "tt": 7, "ks": {} }] });
        assert_eq!(scan(&doc)[0].feature, Feature::TrackMatte);
    }

    #[test]
    fn time_remap_is_supported_on_a_precomp_and_nowhere_else() {
        let remap = json!({ "a": 0, "k": 1.0 });
        let doc = json!({ "layers": [{ "ty": 0, "refId": "c", "tm": remap, "ks": {} }] });
        assert!(scan(&doc).is_empty(), "precomp time remap is implemented");
        let doc = json!({ "layers": [{ "ty": 4, "tm": remap, "ks": {} }] });
        assert_eq!(scan(&doc)[0].feature, Feature::TimeRemap);
    }

    #[test]
    fn a_repeater_is_named_rather_than_lumped_in_with_unknowns() {
        let doc = json!({
            "layers": [{ "ty": 4, "ks": {}, "shapes": [{ "ty": "gr", "it": [{ "ty": "rp" }] }] }]
        });
        assert_eq!(scan(&doc)[0].feature, Feature::Repeater);
    }

    #[test]
    fn allowing_a_feature_stops_it_blocking() {
        let doc = json!({ "layers": [{ "ty": 4, "ddd": 1, "ks": {} }] });
        let found = scan(&doc);
        assert!(reject(&found, &BTreeSet::new()).is_some());
        let allow = BTreeSet::from([Feature::ThreeD]);
        assert!(reject(&found, &allow).is_none());
    }

    #[test]
    fn the_error_says_what_ignoring_it_would_do() {
        let doc = json!({ "layers": [{ "ty": 4, "ddd": 1, "ks": {} }] });
        let msg = reject(&scan(&doc), &BTreeSet::new()).unwrap();
        assert!(msg.contains("3d"), "{msg}");
        assert!(msg.contains("flat"), "{msg}");
        assert!(msg.contains("--allow 3d"), "{msg}");
    }
}
