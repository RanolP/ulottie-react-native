use serde::{Deserialize, Serialize};

use super::graphic::GraphicElement;
use super::property::Property;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Animation {
    #[serde(rename = "v")]
    pub version: String,

    #[serde(rename = "nm")]
    pub name: Option<String>,

    #[serde(rename = "w")]
    pub width: u32,

    #[serde(rename = "h")]
    pub height: u32,

    #[serde(rename = "fr")]
    pub frame_rate: f64,

    #[serde(rename = "ip")]
    pub in_point: f64,

    #[serde(rename = "op")]
    pub out_point: f64,

    #[serde(default)]
    pub ddd: Option<u8>,

    pub layers: Vec<Layer>,

    #[serde(default)]
    pub assets: Vec<Asset>,

    pub meta: Option<serde_json::Value>,

    #[serde(default)]
    pub markers: Option<serde_json::Value>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Asset {
    pub id: String,

    #[serde(rename = "nm")]
    pub name: Option<String>,

    /// Precomp assets have nested layers
    #[serde(default)]
    pub layers: Option<Vec<Layer>>,

    /// Image assets
    #[serde(rename = "u")]
    pub path: Option<String>,
    #[serde(rename = "p")]
    pub filename: Option<String>,
    pub w: Option<u32>,
    pub h: Option<u32>,
    pub e: Option<u8>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TransformBlock {
    pub o: Option<Property>,
    pub r: Option<Property>,
    pub p: Option<Property>,
    pub a: Option<Property>,
    pub s: Option<Property>,
    pub sk: Option<Property>,
    pub sa: Option<Property>,
}

impl TransformBlock {
    pub fn from_properties(
        p: &Property,
        a: &Property,
        s: &Property,
        r: &Property,
        o: &Property,
    ) -> Self {
        Self {
            p: Some(p.clone()),
            a: Some(a.clone()),
            s: Some(s.clone()),
            r: Some(r.clone()),
            o: Some(o.clone()),
            sk: None,
            sa: None,
        }
    }

    pub fn opacity(&self) -> &Property {
        static DEFAULT_OPACITY: std::sync::LazyLock<Property> =
            std::sync::LazyLock::new(|| serde_json::from_str(r#"{"a":0,"k":100}"#).unwrap());
        self.o.as_ref().unwrap_or(&DEFAULT_OPACITY)
    }
    pub fn rotation(&self) -> &Property {
        static DEFAULT: std::sync::LazyLock<Property> =
            std::sync::LazyLock::new(|| serde_json::from_str(r#"{"a":0,"k":0}"#).unwrap());
        self.r.as_ref().unwrap_or(&DEFAULT)
    }
    pub fn position(&self) -> &Property {
        static DEFAULT: std::sync::LazyLock<Property> =
            std::sync::LazyLock::new(|| serde_json::from_str(r#"{"a":0,"k":[0,0,0]}"#).unwrap());
        self.p.as_ref().unwrap_or(&DEFAULT)
    }
    pub fn anchor(&self) -> &Property {
        static DEFAULT: std::sync::LazyLock<Property> =
            std::sync::LazyLock::new(|| serde_json::from_str(r#"{"a":0,"k":[0,0,0]}"#).unwrap());
        self.a.as_ref().unwrap_or(&DEFAULT)
    }
    pub fn scale(&self) -> &Property {
        static DEFAULT: std::sync::LazyLock<Property> = std::sync::LazyLock::new(|| {
            serde_json::from_str(r#"{"a":0,"k":[100,100,100]}"#).unwrap()
        });
        self.s.as_ref().unwrap_or(&DEFAULT)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Layer {
    pub ddd: Option<u8>,
    pub ind: Option<u32>,
    pub ty: u32,

    #[serde(rename = "nm")]
    pub name: Option<String>,

    pub sr: Option<f64>,
    pub ks: TransformBlock,
    pub ao: Option<u8>,

    pub shapes: Option<Vec<GraphicElement>>,

    // Solid layer (ty=1) fields
    pub sw: Option<u32>,
    pub sh: Option<u32>,
    pub sc: Option<String>,

    // Precomp layer (ty=0) fields
    #[serde(rename = "refId")]
    pub ref_id: Option<String>,
    /// Precomp width (reuses w for precomp, sw for solid)
    #[serde(rename = "w")]
    pub width: Option<u32>,
    /// Precomp height
    #[serde(rename = "h")]
    pub height: Option<u32>,

    // Parent-child linking
    pub parent: Option<u32>,

    pub ip: f64,
    pub op: f64,
    pub st: Option<f64>,
    pub bm: Option<u8>,

    #[serde(rename = "hasMask")]
    pub has_mask: Option<bool>,
    /// Per-layer SVG masks, parsed by the layer-mask renderer. Each entry has
    /// a mode (a/s/i/d/f/l), an inv flag, and a `pt` property whose value is
    /// a bezier path (possibly animated).
    #[serde(rename = "masksProperties")]
    pub masks_properties: Option<Vec<MaskProperty>>,
    pub td: Option<u8>,
    pub tt: Option<u8>,
    /// Time remap (precomp layers): the composition's own time, in **seconds**,
    /// as a function of the parent's time. Multiply by the frame rate to get
    /// the inner frame.
    pub tm: Option<Property>,
    pub ef: Option<serde_json::Value>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MaskProperty {
    /// Mask mode: "a" (add), "s" (subtract), "i" (intersect), "d" (difference),
    /// "f" (darken), "l" (lighten).
    pub mode: String,
    /// Inverted mask flag.
    #[serde(default)]
    pub inv: bool,
    /// The mask shape itself — a path Property (animated or static bezier).
    pub pt: Property,
    /// Mask opacity (optional, defaults to 100).
    pub o: Option<Property>,
}
