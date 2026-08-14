//! Wire format for compiled animations.
//!
//! **AOT design**: no property table. Property values are inlined at their
//! reference sites using `InlineProp`, which serializes as a tagged union by
//! field name (`k` for static, `kf` for animated, `e` for expression). The
//! runtime resolves each to a specialized closure at build time — zero
//! variant dispatch per frame.

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
    /// Top-level layers.
    pub l: Vec<Layer>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub a: Option<BTreeMap<String, Asset>>,
    /// Shape table.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub s: Vec<Shape>,
    /// Style table (fills, strokes, trim paths, gradients).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub y: Vec<Style>,
    /// String table.
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
    #[serde(skip_serializing_if = "is_zero_u8")]
    pub ddd: u8,
}

fn is_zero_u8(v: &u8) -> bool {
    *v == 0
}
fn is_false(v: &bool) -> bool {
    !*v
}
fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}
fn is_one_f64(v: &f64) -> bool {
    (*v - 1.0).abs() < f64::EPSILON
}

// ---------------------------------------------------------------------------
// Inline property — the core AOT type. No table lookup, no runtime dispatch.
// ---------------------------------------------------------------------------

/// An inlined property value. Serialized with a single discriminant field:
/// `k` (static), `kf` (animated), or `e` (expression). The runtime checks
/// which field exists ONCE at build time.
///
/// Serializes as e.g. `{"k":[256,256]}`, `{"kf":{...}}`, `{"e":0,"fb":...}`.
#[derive(Debug, Clone)]
pub enum InlineProp {
    /// Static constant value — the common case.
    Static(Value),
    /// Animated keyframes.
    Animated(Keyframes),
    /// Expression reference + optional fallback.
    Expression(ExprProp),
}

impl Default for InlineProp {
    fn default() -> Self {
        InlineProp::Static(Value::Scalar(0.0))
    }
}

impl Serialize for InlineProp {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            InlineProp::Static(v) => {
                use serde::ser::SerializeStruct;
                let mut st = s.serialize_struct("InlineProp", 1)?;
                st.serialize_field("k", v)?;
                st.end()
            }
            InlineProp::Animated(kf) => {
                use serde::ser::SerializeStruct;
                let mut st = s.serialize_struct("InlineProp", 1)?;
                st.serialize_field("kf", kf)?;
                st.end()
            }
            InlineProp::Expression(ep) => ep.serialize(s),
        }
    }
}

// ---------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Scalar(f64),
    Vector(Vec<f64>),
    Path(PathValue),
}

#[derive(Debug, Clone, PartialEq)]
pub struct PathValue {
    pub v: Vec<[f64; 2]>,
    pub i: Vec<[f64; 2]>,
    pub o: Vec<[f64; 2]>,
    pub c: bool,
}

/// Round floats to 4 decimal places for compact wire output.
fn q(n: f64) -> f64 {
    (n * 10000.0).round() / 10000.0
}

impl Serialize for Value {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Value::Scalar(n) => s.serialize_f64(q(*n)),
            Value::Vector(v) => {
                use serde::ser::SerializeSeq;
                let mut seq = s.serialize_seq(Some(v.len()))?;
                for n in v {
                    seq.serialize_element(&q(*n))?;
                }
                seq.end()
            }
            Value::Path(p) => p.serialize(s),
        }
    }
}

impl Serialize for PathValue {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("PathValue", 4)?;
        let qv: Vec<[f64; 2]> = self.v.iter().map(|p| [q(p[0]), q(p[1])]).collect();
        let qi: Vec<[f64; 2]> = self.i.iter().map(|p| [q(p[0]), q(p[1])]).collect();
        let qo: Vec<[f64; 2]> = self.o.iter().map(|p| [q(p[0]), q(p[1])]).collect();
        st.serialize_field("v", &qv)?;
        st.serialize_field("i", &qi)?;
        st.serialize_field("o", &qo)?;
        st.serialize_field("c", &self.c)?;
        st.end()
    }
}

impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Value::Scalar(v)
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
// Keyframes
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Keyframes {
    pub t: Vec<f64>,
    pub v: Vec<Value>,
    pub oi: Option<Vec<EasingPair>>,
    pub to: Option<Vec<Vec<f64>>>,
    pub ti: Option<Vec<Vec<f64>>>,
    /// Per-keyframe hold flags. A held segment keeps its start value until the
    /// next keyframe instead of interpolating.
    pub h: Option<Vec<bool>>,
}

impl Serialize for Keyframes {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut fields = 2;
        if self.oi.is_some() {
            fields += 1;
        }
        if self.to.is_some() {
            fields += 1;
        }
        if self.ti.is_some() {
            fields += 1;
        }
        if self.h.is_some() {
            fields += 1;
        }
        let mut st = s.serialize_struct("Keyframes", fields)?;
        let qt: Vec<f64> = self.t.iter().map(|n| q(*n)).collect();
        st.serialize_field("t", &qt)?;
        st.serialize_field("v", &self.v)?;
        if let Some(oi) = &self.oi {
            st.serialize_field("oi", oi)?;
        }
        if let Some(to) = &self.to {
            let qto: Vec<Vec<f64>> = to
                .iter()
                .map(|v| v.iter().map(|n| q(*n)).collect())
                .collect();
            st.serialize_field("to", &qto)?;
        }
        if let Some(h) = &self.h {
            st.serialize_field("h", h)?;
        }
        if let Some(ti) = &self.ti {
            let qti: Vec<Vec<f64>> = ti
                .iter()
                .map(|v| v.iter().map(|n| q(*n)).collect())
                .collect();
            st.serialize_field("ti", &qti)?;
        }
        st.end()
    }
}

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
            EasingComponent::Scalar(n) => s.serialize_f64(q(*n)),
            EasingComponent::PerComponent(v) => {
                use serde::ser::SerializeSeq;
                let mut seq = s.serialize_seq(Some(v.len()))?;
                for n in v {
                    seq.serialize_element(&q(*n))?;
                }
                seq.end()
            }
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ExprProp {
    pub e: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fb: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kf: Option<Keyframes>,
}

// ---------------------------------------------------------------------------
// Shape — tagged with `t` (single-char). Properties inlined.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "t")]
// Written once at encode and read by reference thereafter; the size skew
// between `Rect` and `PolyStar` is not worth boxing seven fields over.
#[allow(clippy::large_enum_variant)]
pub enum Shape {
    #[serde(rename = "r")]
    Rect {
        sz: InlineProp,
        ps: InlineProp,
        rd: InlineProp,
        #[serde(skip_serializing_if = "Option::is_none")]
        nm: Option<u32>,
    },
    #[serde(rename = "e")]
    Ellipse {
        sz: InlineProp,
        ps: InlineProp,
        #[serde(skip_serializing_if = "Option::is_none")]
        nm: Option<u32>,
    },
    #[serde(rename = "p")]
    Path {
        pt: InlineProp,
        #[serde(skip_serializing_if = "Option::is_none")]
        nm: Option<u32>,
    },
    #[serde(rename = "s")]
    PolyStar {
        sy: u8,
        pt: InlineProp,
        ps: InlineProp,
        or: InlineProp,
        ir: InlineProp,
        rt: InlineProp,
        #[serde(skip_serializing_if = "Option::is_none")]
        os: Option<InlineProp>,
        #[serde(skip_serializing_if = "Option::is_none")]
        is: Option<InlineProp>,
        #[serde(skip_serializing_if = "Option::is_none")]
        nm: Option<u32>,
    },
}

// ---------------------------------------------------------------------------
// Style — properties inlined.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "t")]
// Same deal as `Shape`: encode-time data, one materialization, the gradient
// variants simply carry more.
#[allow(clippy::large_enum_variant)]
pub enum Style {
    #[serde(rename = "fl")]
    Fill { c: InlineProp, o: InlineProp },
    #[serde(rename = "st")]
    Stroke {
        c: InlineProp,
        o: InlineProp,
        w: InlineProp,
        lc: u8,
        lj: u8,
        #[serde(skip_serializing_if = "Option::is_none")]
        ml: Option<f64>,
    },
    #[serde(rename = "gs")]
    GradientStroke {
        g: serde_json::Value,
        w: InlineProp,
        o: InlineProp,
        #[serde(skip_serializing_if = "Option::is_none")]
        s: Option<InlineProp>,
        #[serde(skip_serializing_if = "Option::is_none")]
        e: Option<InlineProp>,
        gk: u8,
        lc: u8,
        lj: u8,
        #[serde(skip_serializing_if = "Option::is_none")]
        ml: Option<f64>,
    },
    #[serde(rename = "gf")]
    GradientFill {
        g: serde_json::Value,
        o: InlineProp,
        #[serde(skip_serializing_if = "Option::is_none")]
        s: Option<InlineProp>,
        #[serde(skip_serializing_if = "Option::is_none")]
        e: Option<InlineProp>,
        gk: u8,
        fr: u8,
    },
    #[serde(rename = "tm")]
    TrimPath {
        s: InlineProp,
        e: InlineProp,
        o: InlineProp,
        m: u8,
    },
}

// ---------------------------------------------------------------------------
// Layer — transform properties inlined.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Default)]
pub struct Layer {
    pub i: u32,
    pub ty: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr: Option<u32>,
    pub ip: f64,
    pub op: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub st: Option<f64>,
    /// Time remap, in seconds. Present only on precomp layers that carry one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tr: Option<InlineProp>,
    /// Track matte mode on *this* layer: 1 alpha, 2 alpha-inverted, 3 luma,
    /// 4 luma-inverted. The matte source is the layer immediately before it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tt: Option<u8>,
    /// Index of the layer that mattes this one, in *this* list — like `pr`, an
    /// array index rather than the layer's `ind`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tp: Option<u32>,
    /// This layer is a matte source: it is not drawn, it masks the next one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub td: Option<u8>,
    /// Blend mode (`bm`), 1–15: multiply, screen, overlay, darken, lighten,
    /// color-dodge, color-burn, hard-light, soft-light, difference,
    /// exclusion, hue, saturation, color, luminosity. Emitted as CSS
    /// `mix-blend-mode` on the layer group, the same spelling lottie-web
    /// writes. 0 (normal) is absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bm: Option<u8>,
    #[serde(skip_serializing_if = "is_one_f64")]
    pub sr: f64,

    // Transform — inlined properties.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p: Option<InlineProp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub a: Option<InlineProp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sc: Option<InlineProp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r: Option<InlineProp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub o: Option<InlineProp>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub shapes: Option<Vec<ShapeRef>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rf: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cl: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sw: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sh: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ef: Option<Vec<Effect>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mk: Option<Vec<LayerMask>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LayerMask {
    pub m: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub inv: bool,
    pub pt: InlineProp,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub o: Option<InlineProp>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Effect {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nm: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mn: Option<String>,
    /// After Effects' effect type. Only expressions used to read effects, and
    /// they look them up by name — the planner needs the type, because that is
    /// what says whether the effect *draws*. lottie-web keys its filter table
    /// on the same number.
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub ty: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ef: Vec<EffectParam>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EffectParam {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nm: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mn: Option<String>,
    #[serde(skip_serializing_if = "is_zero_u32")]
    pub ty: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub v: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p: Option<InlineProp>,
    /// A static colour parameter (`ty: 2`), which is a vector and so cannot
    /// travel in `v`. Only the static case: an animated effect colour is
    /// reported rather than frozen.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub c: Option<[f64; 4]>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum ShapeRef {
    Prim(PrimRef),
    /// Boxed: `GroupRef` is an order of magnitude larger than `PrimRef`, and
    /// a `Vec<ShapeRef>` is cloned per scene plan — the indirection keeps
    /// every `Prim` in it from paying for the groups.
    Group(Box<GroupRef>),
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct PrimRef {
    pub s: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub y: Vec<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tm: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct GroupRef {
    pub c: Vec<ShapeRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p: Option<InlineProp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub a: Option<InlineProp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sc: Option<InlineProp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r: Option<InlineProp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub o: Option<InlineProp>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "t")]
pub enum Asset {
    #[serde(rename = "p")]
    Precomp { l: Vec<Layer> },
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
