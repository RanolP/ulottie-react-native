//! Wire format for compiled animations.
//!
//! The compiler emits a `Payload` (this module's root type) serialized as a
//! JS object literal, plus an optional `E[]` array of compiled expression
//! functions. The shared `runtime/driver.js` decodes the payload and drives
//! the animation.
//!
//! Wire fields are deliberately short (`p`, `kf`, `v`, `oi`, …) because they
//! survive into the gzipped output; clarity lives in this Rust file via
//! field names. `serde(rename = …)` and `skip_serializing_if` tags handle
//! the encoding.

pub mod encode;

pub use encode::{can_encode, encode};

use std::collections::BTreeMap;

use serde::{Serialize, Serializer};

// ---------------------------------------------------------------------------
// Root
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Default)]
pub struct Payload {
    /// Composition header.
    pub c: Composition,
    /// Top-level layers. Z-order is source order (driver appends in reverse
    /// so that the first listed layer ends up on top in SVG).
    pub l: Vec<Layer>,
    /// Precomp / image assets, keyed by refId. Optional — many animations
    /// don't use precomps. BTreeMap so on-wire key order is deterministic
    /// across runs (snapshot tests rely on this).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub a: Option<BTreeMap<String, Asset>>,
    /// Property table. Layers and shapes reference entries by index.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub p: Vec<Property>,
    /// Shape table.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub s: Vec<Shape>,
    /// Style table (fills, strokes, trim paths, gradients).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub y: Vec<Style>,
    /// String table — for layer names, match-names, asset refIds. Items in
    /// the IR that reference strings by id index into this table.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub st: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Composition {
    pub w: u32,
    pub h: u32,
    pub fr: f64,
    pub ip: f64,
    pub op: f64,
    /// 3D composition flag. Skipped when 0 (the common case).
    #[serde(skip_serializing_if = "is_zero_u8")]
    pub ddd: u8,
}

fn is_zero_u8(v: &u8) -> bool {
    *v == 0
}

// ---------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------

/// Either a scalar number, a vector, or a bezier path. Serializes
/// transparently — `1`, `[1, 2, 3]`, or `{"v": [...], "i": [...], ...}`. The
/// driver discriminates by JSON shape: typeof === number / Array.isArray /
/// object-with-`v`-field.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Scalar(f64),
    Vector(Vec<f64>),
    Path(PathValue),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PathValue {
    /// Vertex coordinates, each `[x, y]`.
    pub v: Vec<[f64; 2]>,
    /// In tangents, paired with `v`. Stored relative to the vertex.
    pub i: Vec<[f64; 2]>,
    /// Out tangents, paired with `v`. Stored relative to the vertex.
    pub o: Vec<[f64; 2]>,
    /// Closed path flag.
    pub c: bool,
}

impl Serialize for Value {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Value::Scalar(n) => s.serialize_f64(*n),
            Value::Vector(v) => v.serialize(s),
            Value::Path(p) => p.serialize(s),
        }
    }
}

impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Value::Scalar(v)
    }
}
impl From<&[f64]> for Value {
    fn from(v: &[f64]) -> Self {
        Value::Vector(v.to_vec())
    }
}
impl From<Vec<f64>> for Value {
    fn from(v: Vec<f64>) -> Self {
        Value::Vector(v)
    }
}
impl From<PathValue> for Value {
    fn from(v: PathValue) -> Self {
        Value::Path(v)
    }
}

// ---------------------------------------------------------------------------
// Property — the most-used variant; dispatched by which field is present.
// ---------------------------------------------------------------------------

/// A property in the wire payload. Decoder uses field presence to discriminate:
/// `k` → static, `kf` → animated, `d` → built-in pattern, `e` → custom JS.
///
/// `untagged` rather than `tag = "kind"` keeps the on-wire form maximally
/// compact (`{"k":[1,2]}` vs `{"kind":"static","k":[1,2]}`).
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum Property {
    Static(StaticProp),
    Animated(AnimatedProp),
    Pattern(PatternProp),
    Expression(ExprProp),
}

#[derive(Debug, Clone, Serialize)]
pub struct StaticProp {
    pub k: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct AnimatedProp {
    pub kf: Keyframes,
}

#[derive(Debug, Clone, Serialize)]
pub struct Keyframes {
    /// Times, sorted ascending.
    pub t: Vec<f64>,
    /// Values, one per time. Each may be a scalar or a vector — same shape
    /// across the array. The decoder picks shape from the first entry.
    pub v: Vec<Value>,
    /// Older-Lottie end values (paired with `t[i]` → segment goes to `e[i]`
    /// instead of `v[i+1]`). Omitted when no keyframe uses the older form.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub e: Option<Vec<Option<Value>>>,
    /// Bezier easing handles for each segment. `oi[i]` is the pair
    /// (out_tangent_at_i, in_tangent_at_i+1). Omitted entirely when every
    /// segment is linear.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oi: Option<Vec<EasingPair>>,
    /// Spatial tangents (cubic-bezier in 2D/3D path space). Omitted when all
    /// segments are linear.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<Vec<Vec<f64>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ti: Option<Vec<Vec<f64>>>,
}

/// (out tangent of segment start, in tangent of segment end). Each handle is
/// `(x, y)` ∈ [0,1].
#[derive(Debug, Clone, Serialize)]
pub struct EasingPair {
    pub o: EasingHandle,
    pub i: EasingHandle,
}

#[derive(Debug, Clone, Serialize)]
pub struct EasingHandle {
    pub x: EasingComponent,
    pub y: EasingComponent,
}

#[derive(Debug, Clone)]
pub enum EasingComponent {
    Scalar(f64),
    PerComponent(Vec<f64>),
}

impl Serialize for EasingComponent {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            EasingComponent::Scalar(n) => s.serialize_f64(*n),
            EasingComponent::PerComponent(v) => v.serialize(s),
        }
    }
}

/// Compiled JS expression reference. `e` indexes into the output module's
/// `E[]` array of compiled functions. The fallback represents the underlying
/// value source (static or keyframes); expression bodies that call
/// `thisProperty.valueAtTime` / `velocityAtTime` / `numKeys` / `key` /
/// `nearestKey` need access to it.
#[derive(Debug, Clone, Serialize)]
pub struct ExprProp {
    pub e: u32,
    /// Static fallback value (used when the property's source isn't animated,
    /// or as a hard fallback if the expression throws).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fb: Option<Value>,
    /// Keyframes the underlying property is built from, when animated. The
    /// driver wires this into `thisProperty` so expression bodies can drive
    /// off the property's own animation curve.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kf: Option<Keyframes>,
}

/// Built-in expression pattern. The driver has a dedicated branch per `d`
/// discriminator. `args` is a free-form object whose schema is pattern-
/// specific (kept loose here; the recognizer pass fills it in).
#[derive(Debug, Clone, Serialize)]
pub struct PatternProp {
    pub d: String,
    #[serde(flatten)]
    pub args: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Shape — primitives + container forms. Tagged with `t` (single-char).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "t")]
pub enum Shape {
    /// Group. `it` is a list of shape ids. `tr` is an optional transform
    /// property bundle id (for group-local transforms; rare).
    #[serde(rename = "g")]
    Group {
        it: Vec<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tr: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        nm: Option<u32>,
    },
    /// Rectangle. Property ids for size, position, corner radius.
    #[serde(rename = "r")]
    Rect {
        sz: u32,
        ps: u32,
        rd: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        nm: Option<u32>,
    },
    /// Ellipse.
    #[serde(rename = "e")]
    Ellipse {
        sz: u32,
        ps: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        nm: Option<u32>,
    },
    /// Bezier path. `pt` is the property id holding the path data (static
    /// or animated as a Keyframes-of-paths; the path-keyframe variant comes
    /// in a later phase).
    #[serde(rename = "p")]
    Path {
        pt: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        nm: Option<u32>,
    },
    /// PolyStar (star or polygon).
    #[serde(rename = "s")]
    PolyStar {
        /// Star type: 1 = star, 2 = polygon.
        sy: u8,
        pt: u32, // points (property id)
        ps: u32, // position
        or: u32, // outer radius
        ir: u32, // inner radius (ignored when sy=2)
        rt: u32, // rotation
        #[serde(skip_serializing_if = "Option::is_none")]
        os: Option<u32>, // outer roundness
        #[serde(skip_serializing_if = "Option::is_none")]
        is: Option<u32>, // inner roundness
        #[serde(skip_serializing_if = "Option::is_none")]
        nm: Option<u32>,
    },
}

// ---------------------------------------------------------------------------
// Style — fill, stroke, trim, gradient stroke.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "t")]
pub enum Style {
    #[serde(rename = "fl")]
    Fill {
        c: u32, // color (property id)
        o: u32, // opacity
    },
    #[serde(rename = "st")]
    Stroke {
        c: u32,
        o: u32,
        w: u32,
        /// Line cap: 1=butt, 2=round, 3=square.
        lc: u8,
        /// Line join: 1=miter, 2=round, 3=bevel.
        lj: u8,
        #[serde(skip_serializing_if = "Option::is_none")]
        ml: Option<f64>,
    },
    #[serde(rename = "gs")]
    GradientStroke {
        /// Gradient definition (color stops). Kept loose for now; will be
        /// promoted to a typed variant once we touch gradients.
        g: serde_json::Value,
        w: u32,
        o: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        s: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        e: Option<u32>,
        /// Gradient kind: 1=linear, 2=radial.
        gk: u8,
        lc: u8,
        lj: u8,
        #[serde(skip_serializing_if = "Option::is_none")]
        ml: Option<f64>,
    },
    #[serde(rename = "gf")]
    GradientFill {
        g: serde_json::Value,
        o: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        s: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        e: Option<u32>,
        /// Gradient kind: 1=linear, 2=radial.
        gk: u8,
        /// Fill rule: 1=non-zero, 2=even-odd.
        fr: u8,
    },
    #[serde(rename = "tm")]
    TrimPath {
        s: u32,
        e: u32,
        o: u32,
        /// Multiple-shapes mode: 1=simultaneously, 2=individually.
        m: u8,
    },
}

// ---------------------------------------------------------------------------
// Layer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Default)]
pub struct Layer {
    /// Composition index (1-based, matches Lottie's `ind`).
    pub i: u32,
    /// Layer type: 0=precomp, 1=solid, 2=image, 3=null, 4=shape.
    pub ty: u32,
    /// Display name → string-table id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n: Option<u32>,
    /// Parent layer's id in the enclosing layers vector. Resolved by lower().
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr: Option<u32>,
    pub ip: f64,
    pub op: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub st: Option<f64>,
    /// Time-stretch (Lottie `sr`); skipped when 1.0.
    #[serde(skip_serializing_if = "is_one_f64")]
    pub sr: f64,

    // Transform properties — each is an id into `p[]`. Optional so unused
    // ones cost zero bytes on the wire.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub a: Option<u32>,
    /// "sc" instead of "s" so it doesn't collide with shape arrays in
    /// hand-reading and so that future tooling can identify it unambiguously.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sc: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub o: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sk: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sa: Option<u32>,

    // Body — only the field for this layer's type is set.
    /// Shape-layer content: a flat list of (shape, style) pairs. Each entry
    /// references one Shape id and (optionally) one Style id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shapes: Option<Vec<ShapeRef>>,
    /// Precomp reference (refId; resolved via `Payload::a`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rf: Option<String>,
    /// Solid color (e.g. "#abcdef").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cl: Option<String>,
    /// Solid width / height.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sw: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sh: Option<u32>,

    /// Effects — expression bodies look these up via
    /// `thisLayer.effect('name-or-matchname')('param-name-or-matchname')`. Most
    /// fixtures only use slider-style scalars (e.g. ADBE Slider Control,
    /// Pseudo/ADBE Trace Path); each parameter carries either a constant `v`
    /// (most common) or a property id `p` into the property table.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ef: Option<Vec<Effect>>,

    /// Per-layer SVG masks. Each entry has a mode (`a`=add, `s`=subtract),
    /// an `inv` flag, and a property id pointing at the bezier path that
    /// defines the mask shape. The driver builds a `<mask>` element with
    /// the path filled white (or the inverse) and applies it to the layer's
    /// outer group.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mk: Option<Vec<LayerMask>>,
}

/// One mask in the wire format. `m` is the mode (single char), `inv` is true
/// when the mask is inverted, `pt` is the property id for the bezier path
/// shape (static or animated path).
#[derive(Debug, Clone, Serialize)]
pub struct LayerMask {
    pub m: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub inv: bool,
    pub pt: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub o: Option<u32>,
}

fn is_false(v: &bool) -> bool {
    !*v
}

/// One effect entry, indexed in expression bodies by `nm` or `mn`. Parameters
/// nest the same way; each parameter has an `nm`/`mn` plus a value source.
#[derive(Debug, Clone, Serialize)]
pub struct Effect {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nm: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mn: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ef: Vec<EffectParam>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EffectParam {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nm: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mn: Option<String>,
    /// Lottie parameter type — drives how the driver interprets `v`. ty=10
    /// means `v` is a 1-based layer index (ADBE Layer Control); the driver
    /// resolves it to a layer proxy. ty=7 is a checkbox/boolean (driver
    /// returns `v` directly). Skipped when 0 (the common slider scalar).
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub ty: u32,
    /// Constant scalar — preferred when the parameter doesn't animate. The
    /// driver returns this directly without touching the property table.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub v: Option<f64>,
    /// Property id (for animated or expression-driven parameters).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p: Option<u32>,
}

fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

fn is_one_f64(v: &f64) -> bool {
    (*v - 1.0).abs() < f64::EPSILON
}

/// One node in a layer's draw tree. Either a Primitive (a single shape with
/// optional style and trim) or a Group (a transform wrapping nested children).
///
/// Serialized untagged: Primitives have `s`, Groups have `c`. The driver
/// discriminates by field presence.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum ShapeRef {
    Prim(PrimRef),
    Group(GroupRef),
}

/// Primitive shape ref. `s` indexes into `Payload::s`; `y` and `tm` into
/// `Payload::y`.
///
/// `y` is a *stack* of style ids — fills and strokes can apply to the same
/// primitive (Lottie shapes commonly carry both), and Lottie style siblings
/// at the same group level all apply, painted in source order.
#[derive(Debug, Clone, Serialize, Default)]
pub struct PrimRef {
    pub s: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub y: Vec<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tm: Option<u32>,
}

/// Group ref. Contains nested children and an optional group-local transform.
/// The driver wraps the children in a `<g transform="...">` evaluated per frame.
#[derive(Debug, Clone, Serialize, Default)]
pub struct GroupRef {
    pub c: Vec<ShapeRef>,
    /// Group-local transform: 5 optional property ids ([position, anchor,
    /// scale, rotation, opacity]). Any missing field defaults to identity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub a: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sc: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub o: Option<u32>,
}

// ---------------------------------------------------------------------------
// Asset (precomp / image)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "t")]
pub enum Asset {
    /// Precomp: a nested composition that can be instantiated by precomp
    /// layers via `Layer::rf`.
    #[serde(rename = "p")]
    Precomp { l: Vec<Layer> },
    /// Image asset. `e` is the embedded-data flag (1 = data URI), `u`/`p` are
    /// path + filename.
    #[serde(rename = "i")]
    Image {
        #[serde(skip_serializing_if = "Option::is_none")]
        u: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        p: Option<String>,
        w: u32,
        h: u32,
        #[serde(skip_serializing_if = "is_zero_u8")]
        e: u8,
    },
}

// ---------------------------------------------------------------------------
// Tests — round-trip a hand-crafted Payload to JSON and back, sanity-check
// the wire format.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip a minimal payload and confirm the JSON shape is what we
    /// expect (compact, no extra keys for default-valued fields).
    #[test]
    fn empty_payload_serializes_compactly() {
        let p = Payload {
            c: Composition { w: 100, h: 100, fr: 60.0, ip: 0.0, op: 30.0, ddd: 0 },
            l: vec![],
            ..Default::default()
        };
        let s = serde_json::to_string(&p).unwrap();
        // No "a", no "p", no "s", no "y", no "st", no "ddd" should be present.
        assert!(!s.contains("\"a\""), "got: {s}");
        assert!(!s.contains("\"ddd\""), "got: {s}");
        assert!(!s.contains("\"st\""), "got: {s}");
        assert!(s.contains("\"c\""));
    }

    /// Property variants must be discriminable by field presence (untagged).
    #[test]
    fn property_variants_serialize_without_kind_tag() {
        let stat = Property::Static(StaticProp { k: Value::Scalar(100.0) });
        let s = serde_json::to_string(&stat).unwrap();
        assert_eq!(s, r#"{"k":100.0}"#);

        let anim = Property::Animated(AnimatedProp {
            kf: Keyframes {
                t: vec![0.0, 30.0],
                v: vec![Value::Scalar(0.0), Value::Scalar(100.0)],
                e: None, oi: None, to: None, ti: None,
            },
        });
        let s = serde_json::to_string(&anim).unwrap();
        assert!(s.contains(r#""kf""#));
        assert!(!s.contains(r#""kind""#));
        // No empty oi/to/ti fields.
        assert!(!s.contains(r#""oi""#));
    }

    /// A hand-crafted "rectangle.json" equivalent payload. This is the
    /// reference the smoke test will use.
    #[test]
    fn rectangle_payload_round_trips() {
        let p = Payload {
            c: Composition { w: 512, h: 512, fr: 60.0, ip: 0.0, op: 180.0, ddd: 0 },
            l: vec![Layer {
                i: 1,
                ty: 4,
                ip: 0.0,
                op: 180.0,
                sr: 1.0,
                shapes: Some(vec![ShapeRef::Prim(PrimRef { s: 0, y: vec![0], tm: None })]),
                ..Default::default()
            }],
            p: vec![
                Property::Static(StaticProp { k: Value::Vector(vec![256.0, 256.0]) }), // 0: size
                Property::Static(StaticProp { k: Value::Vector(vec![0.0, 0.0]) }),     // 1: pos
                Property::Static(StaticProp { k: Value::Scalar(0.0) }),                // 2: radius
                Property::Static(StaticProp { k: Value::Vector(vec![1.0, 0.98, 0.28, 1.0]) }), // 3: color
                Property::Static(StaticProp { k: Value::Scalar(100.0) }),              // 4: opacity
                Property::Static(StaticProp { k: Value::Scalar(30.0) }),               // 5: stroke width
            ],
            s: vec![Shape::Rect { sz: 0, ps: 1, rd: 2, nm: None }],
            y: vec![Style::Stroke {
                c: 3, o: 4, w: 5, lc: 2, lj: 2, ml: None,
            }],
            ..Default::default()
        };
        let _ = serde_json::to_string(&p).expect("serialize round-trip");
    }
}
