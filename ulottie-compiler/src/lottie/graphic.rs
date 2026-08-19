use serde::{Deserialize, Serialize};

use super::property::Property;

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "ty")]
pub enum GraphicElement {
    // Shapes
    #[serde(rename = "el")]
    Ellipse {
        #[serde(rename = "nm")]
        name: Option<String>,
        #[serde(rename = "hd", default)]
        hidden: bool,
        d: Option<u8>,
        s: Property,
        p: Property,
    },

    #[serde(rename = "sh")]
    Path {
        #[serde(rename = "nm")]
        name: Option<String>,
        #[serde(rename = "hd", default)]
        hidden: bool,
        d: Option<u8>,
        ks: Property,
        /// Pre-4.4.18 bodymovin puts the closed flag here, on the element,
        /// with no `c` inside the path values — lottie-web's `checkShapes`
        /// migrates it in at load, and so does the lowering.
        closed: Option<bool>,
    },

    #[serde(rename = "rc")]
    Rectangle {
        #[serde(rename = "nm")]
        name: Option<String>,
        #[serde(rename = "hd", default)]
        hidden: bool,
        d: Option<u8>,
        s: Property,
        p: Property,
        r: Property,
    },

    #[serde(rename = "sr")]
    PolyStar {
        #[serde(rename = "nm")]
        name: Option<String>,
        #[serde(rename = "hd", default)]
        hidden: bool,
        d: Option<u8>,
        /// Star type: 1=star, 2=polygon
        sy: Option<u8>,
        /// Number of points
        pt: Property,
        /// Position
        p: Property,
        /// Outer radius
        #[serde(rename = "or")]
        outer_radius: Property,
        /// Inner radius (star only)
        ir: Option<Property>,
        /// Outer roundness
        os: Option<Property>,
        /// Inner roundness
        #[serde(rename = "is")]
        inner_roundness: Option<Property>,
        /// Rotation
        r: Property,
    },

    // Grouping
    #[serde(rename = "gr")]
    Group {
        #[serde(rename = "nm")]
        name: Option<String>,
        #[serde(rename = "hd", default)]
        hidden: bool,
        it: Vec<GraphicElement>,
        np: Option<u32>,
        cix: Option<u32>,
        bm: Option<u8>,
        ix: Option<u32>,

        #[serde(rename = "mn")]
        match_name: Option<String>,
    },

    // Every field is optional: old bodymovin omits what is at its default,
    // down to `{"ty":"tr","nm":"Transform"}` with nothing in it at all
    // (`Tests_catrim_converted`, `sticker`).
    #[serde(rename = "tr")]
    Transform {
        #[serde(rename = "nm")]
        name: Option<String>,
        #[serde(rename = "hd", default)]
        hidden: bool,
        p: Option<Property>,
        a: Option<Property>,
        s: Option<Property>,
        r: Option<Property>,
        o: Option<Property>,
        sk: Option<Property>,
        sa: Option<Property>,
    },

    // Style
    #[serde(rename = "fl")]
    Fill {
        #[serde(rename = "nm")]
        name: Option<String>,
        #[serde(rename = "hd", default)]
        hidden: bool,
        c: Property,
        o: Option<Property>,
        r: Option<u8>,
        bm: Option<u8>,

        #[serde(rename = "mn")]
        match_name: Option<String>,
    },

    #[serde(rename = "gf")]
    GradientFill {
        #[serde(rename = "nm")]
        name: Option<String>,
        #[serde(rename = "hd", default)]
        hidden: bool,
        /// Gradient definition (with stops in `g.k`).
        g: Option<serde_json::Value>,
        /// Opacity (0-100). A Telegram-sticker export omits it.
        o: Option<Property>,
        /// Gradient start point.
        s: Option<Property>,
        /// Gradient end point.
        e: Option<Property>,
        /// Gradient type: 1=linear, 2=radial.
        t: Option<u8>,
        /// Fill rule: 1=non-zero, 2=even-odd.
        r: Option<u8>,
    },

    #[serde(rename = "gs")]
    GradientStroke {
        #[serde(rename = "nm")]
        name: Option<String>,
        #[serde(rename = "hd", default)]
        hidden: bool,
        /// Gradient definition (with stops in `g.k`).
        g: Option<serde_json::Value>,
        /// Stroke width.
        w: Option<Property>,
        /// Opacity (0-100).
        o: Option<Property>,
        /// Gradient start point.
        s: Option<Property>,
        /// Gradient end point.
        e: Option<Property>,
        /// Gradient type: 1=linear, 2=radial.
        t: Option<u8>,
        /// Line cap (1=butt, 2=round, 3=square).
        lc: Option<u8>,
        /// Line join.
        lj: Option<u8>,
        /// Miter limit.
        ml: Option<f64>,
        /// Dash pattern: `d`/`g` lengths in order, `o` the offset.
        d: Option<Vec<DashElement>>,
    },

    #[serde(rename = "st")]
    Stroke {
        #[serde(rename = "nm")]
        name: Option<String>,
        #[serde(rename = "hd", default)]
        hidden: bool,
        c: Property,
        o: Option<Property>,
        w: Option<Property>,
        lc: Option<u8>,
        lj: Option<u8>,
        ml: Option<f64>,
        bm: Option<u8>,
        /// Dash pattern: `d`/`g` lengths in order, `o` the offset.
        d: Option<Vec<DashElement>>,

        #[serde(rename = "mn")]
        match_name: Option<String>,
    },

    // Modifiers
    #[serde(rename = "tm")]
    TrimPath {
        #[serde(rename = "nm")]
        name: Option<String>,
        #[serde(rename = "hd", default)]
        hidden: bool,
        s: Option<Property>,
        e: Option<Property>,
        o: Option<Property>,
        m: Option<u8>,
    },

    #[serde(rename = "rp")]
    Repeater {
        #[serde(rename = "nm")]
        name: Option<String>,
        #[serde(rename = "hd", default)]
        hidden: bool,
        /// Copy count.
        c: Property,
        /// Copy offset (how many applications the first copy starts at).
        o: Property,
        /// Order: 1 sequential, 2 simultaneous.
        m: Option<u8>,
        tr: RepeatTransform,
    },

    #[serde(other)]
    Unknown,
}

/// One entry of a stroke's dash pattern: `n` is `"d"` (dash) or `"g"` (gap)
/// for a length in draw order, `"o"` for the offset.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DashElement {
    pub n: Option<String>,
    pub v: Property,
}

/// The transform a repeater applies per copy — a layer transform plus the
/// per-copy opacity ramp `so`/`eo`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RepeatTransform {
    #[serde(rename = "ty")]
    pub ty: Option<String>,
    #[serde(rename = "nm")]
    pub name: Option<String>,
    pub p: Option<Property>,
    pub a: Option<Property>,
    pub s: Option<Property>,
    pub r: Option<Property>,
    /// Start opacity of the copy ramp, percent.
    pub so: Option<Property>,
    /// End opacity of the copy ramp, percent.
    pub eo: Option<Property>,
}
