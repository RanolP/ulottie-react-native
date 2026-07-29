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
    ImageLayer,
    TextLayer,
    UnknownLayerType,
    TrackMatte,
    BlendMode,
    TimeStretch,
    TimeRemap,
    AutoOrient,
    ThreeD,
    Skew,
    MaskMode,
    MaskOpacity,
    Repeater,
    MergePaths,
    RoundedCorners,
    OffsetPath,
    PuckerBloat,
    ZigZag,
    UnknownShape,
    ReversedDirection,
    EvenOddFill,
    AnimatedGradient,
    ImageAsset,
}

impl Feature {
    /// Stable name used on the command line and in the fixture manifest.
    pub fn name(self) -> &'static str {
        use Feature::*;
        match self {
            ImageLayer => "image-layer",
            TextLayer => "text-layer",
            UnknownLayerType => "unknown-layer-type",
            TrackMatte => "track-matte",
            BlendMode => "blend-mode",
            TimeStretch => "time-stretch",
            TimeRemap => "time-remap",
            AutoOrient => "auto-orient",
            ThreeD => "3d",
            Skew => "skew",
            MaskMode => "mask-mode",
            MaskOpacity => "mask-opacity",
            Repeater => "repeater",
            MergePaths => "merge-paths",
            RoundedCorners => "rounded-corners",
            OffsetPath => "offset-path",
            PuckerBloat => "pucker-bloat",
            ZigZag => "zig-zag",
            UnknownShape => "unknown-shape",
            ReversedDirection => "reversed-direction",
            EvenOddFill => "even-odd-fill",
            AnimatedGradient => "animated-gradient",
            ImageAsset => "image-asset",
        }
    }

    /// What ignoring it does to the render.
    pub fn effect(self) -> &'static str {
        use Feature::*;
        match self {
            ImageLayer => "the image is not drawn",
            TextLayer => "the text is not drawn",
            UnknownLayerType => "the layer is not drawn",
            TrackMatte => "the masked layer draws unmasked",
            BlendMode => "the layer composites normally",
            TimeStretch => "the layer plays at composition speed",
            TimeRemap => "the time remap is ignored and the layer plays linearly",
            AutoOrient => "the layer keeps its authored rotation",
            ThreeD => "the layer is drawn flat",
            Skew => "the skew is not applied",
            MaskMode => "the mask is treated as Add",
            MaskOpacity => "the mask is fully opaque",
            Repeater => "only the original copy is drawn",
            MergePaths => "the paths are drawn separately",
            RoundedCorners => "corners stay sharp",
            OffsetPath => "the path is not offset",
            PuckerBloat => "the distortion is not applied",
            ZigZag => "the path is not zig-zagged",
            UnknownShape => "the shape is not drawn",
            ReversedDirection => "winding is unchanged, which can alter holes",
            EvenOddFill => "the fill uses the non-zero rule",
            AnimatedGradient => "the gradient ramp is read as static and may render as NaN",
            ImageAsset => "the image is not drawn",
        }
    }

    pub fn from_name(s: &str) -> Option<Feature> {
        use Feature::*;
        const ALL: &[Feature] = &[
            ImageLayer,
            TextLayer,
            UnknownLayerType,
            TrackMatte,
            BlendMode,
            TimeStretch,
            TimeRemap,
            AutoOrient,
            ThreeD,
            Skew,
            MaskMode,
            MaskOpacity,
            Repeater,
            MergePaths,
            RoundedCorners,
            OffsetPath,
            PuckerBloat,
            ZigZag,
            UnknownShape,
            ReversedDirection,
            EvenOddFill,
            AnimatedGradient,
            ImageAsset,
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

/// Walk a Lottie document and report everything the compiler would ignore.
pub fn scan(doc: &Value) -> Vec<Finding> {
    let mut out = Vec::new();
    if let Some(layers) = doc.get("layers").and_then(Value::as_array) {
        scan_layers(layers, "layers", &mut out);
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
        if asset.get("p").is_some() || asset.get("u").is_some() {
            push(&mut out, Feature::ImageAsset, &where_);
        }
        if let Some(layers) = asset.get("layers").and_then(Value::as_array) {
            scan_layers(layers, &where_, &mut out);
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

fn truthy(v: Option<&Value>) -> bool {
    v.and_then(Value::as_f64).map(|n| n != 0.0).unwrap_or(false)
}

fn scan_layers(layers: &[Value], where_: &str, out: &mut Vec<Finding>) {
    for (i, l) in layers.iter().enumerate() {
        let name = l.get("nm").and_then(Value::as_str).unwrap_or("");
        let at = if name.is_empty() {
            format!("{where_}[{i}]")
        } else {
            format!("{where_}[{i}] `{name}`")
        };

        match l.get("ty").and_then(Value::as_u64) {
            Some(0) | Some(1) | Some(3) | Some(4) => {}
            Some(2) => push(out, Feature::ImageLayer, &at),
            Some(5) => push(out, Feature::TextLayer, &at),
            _ => push(out, Feature::UnknownLayerType, &at),
        }

        // Track mattes are implemented for the four AE modes (alpha, alpha
        // inverted, luma, luma inverted). Anything outside that range would be
        // read and dropped, so it still counts as unsupported.
        if let Some(tt) = l.get("tt").and_then(Value::as_u64) {
            if !(1..=4).contains(&tt) {
                push(out, Feature::TrackMatte, &at);
            }
        }
        if truthy(l.get("bm")) {
            push(out, Feature::BlendMode, &at);
        }
        if l.get("sr")
            .and_then(Value::as_f64)
            .map(|s| s != 1.0)
            .unwrap_or(false)
        {
            push(out, Feature::TimeStretch, &at);
        }
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
            // A skew property that is present but zero is not a skew.
            for key in ["sk", "sa"] {
                if let Some(p) = ks.get(key) {
                    if property_is_nonzero(p) {
                        push(out, Feature::Skew, &at);
                        break;
                    }
                }
            }
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
            match m.get("mode").and_then(Value::as_str) {
                Some("a") | Some("s") | Some("n") | None => {}
                Some(_) => push(out, Feature::MaskMode, &at),
            }
            if let Some(o) = m.get("o") {
                if property_differs_from(o, 100.0) {
                    push(out, Feature::MaskOpacity, &at);
                }
            }
        }

        if let Some(shapes) = l.get("shapes").and_then(Value::as_array) {
            scan_shapes(shapes, &at, out);
        }
    }
}

fn scan_shapes(shapes: &[Value], where_: &str, out: &mut Vec<Finding>) {
    for s in shapes {
        let ty = s.get("ty").and_then(Value::as_str).unwrap_or("");
        if !KNOWN_SHAPES.contains(&ty) {
            let feature = match ty {
                "rp" => Feature::Repeater,
                "mm" => Feature::MergePaths,
                "rd" => Feature::RoundedCorners,
                "op" => Feature::OffsetPath,
                "pb" => Feature::PuckerBloat,
                "zz" => Feature::ZigZag,
                _ => Feature::UnknownShape,
            };
            push(out, feature, &format!("{where_} shape `{ty}`"));
        }
        if s.get("d").and_then(Value::as_u64) == Some(3) {
            push(out, Feature::ReversedDirection, where_);
        }
        if ty == "fl" && s.get("r").and_then(Value::as_u64) == Some(2) {
            push(out, Feature::EvenOddFill, where_);
        }
        if s.get("g")
            .and_then(|g| g.get("k"))
            .and_then(|k| k.get("a"))
            .and_then(Value::as_u64)
            == Some(1)
        {
            push(out, Feature::AnimatedGradient, where_);
        }
        if let Some(items) = s.get("it").and_then(Value::as_array) {
            scan_shapes(items, where_, out);
        }
    }
}

/// A static property whose value is not zero, or any animated one.
fn property_is_nonzero(p: &Value) -> bool {
    if p.get("a").and_then(Value::as_u64) == Some(1) {
        return true;
    }
    match p.get("k") {
        Some(Value::Number(n)) => n.as_f64().map(|v| v != 0.0).unwrap_or(false),
        Some(Value::Array(a)) => a
            .iter()
            .any(|v| v.as_f64().map(|x| x != 0.0).unwrap_or(false)),
        _ => false,
    }
}

fn property_differs_from(p: &Value, expect: f64) -> bool {
    if p.get("a").and_then(Value::as_u64) == Some(1) {
        return true;
    }
    match p.get("k") {
        Some(Value::Number(n)) => n.as_f64().map(|v| v != expect).unwrap_or(false),
        Some(Value::Array(a)) => a
            .first()
            .and_then(Value::as_f64)
            .map(|v| v != expect)
            .unwrap_or(false),
        _ => false,
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
        let doc = json!({ "layers": [{ "ty": 4, "nm": "O", "bm": 3, "ks": {} }] });
        let found = scan(&doc);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].feature, Feature::BlendMode);
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
    fn a_zero_skew_is_not_a_skew() {
        let doc = json!({ "layers": [{ "ty": 4, "ks": { "sk": { "a": 0, "k": 0 } } }] });
        assert!(scan(&doc).is_empty());
        let doc = json!({ "layers": [{ "ty": 4, "ks": { "sk": { "a": 0, "k": 12 } } }] });
        assert_eq!(scan(&doc)[0].feature, Feature::Skew);
    }

    #[test]
    fn allowing_a_feature_stops_it_blocking() {
        let doc = json!({ "layers": [{ "ty": 4, "bm": 3, "ks": {} }] });
        let found = scan(&doc);
        assert!(reject(&found, &BTreeSet::new()).is_some());
        let allow = BTreeSet::from([Feature::BlendMode]);
        assert!(reject(&found, &allow).is_none());
    }

    #[test]
    fn the_error_says_what_ignoring_it_would_do() {
        let doc = json!({ "layers": [{ "ty": 4, "bm": 3, "ks": {} }] });
        let msg = reject(&scan(&doc), &BTreeSet::new()).unwrap();
        assert!(msg.contains("blend-mode"), "{msg}");
        assert!(msg.contains("normal"), "{msg}");
        assert!(msg.contains("--allow blend-mode"), "{msg}");
    }
}
