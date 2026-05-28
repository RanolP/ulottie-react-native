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

    #[serde(rename = "tr")]
    Transform {
        #[serde(rename = "nm")]
        name: Option<String>,
        #[serde(rename = "hd", default)]
        hidden: bool,
        p: Property,
        a: Property,
        s: Property,
        r: Property,
        o: Property,
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
        o: Property,
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
        /// Opacity (0-100).
        o: Property,
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
        w: Property,
        /// Opacity (0-100).
        o: Property,
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
    },

    #[serde(rename = "st")]
    Stroke {
        #[serde(rename = "nm")]
        name: Option<String>,
        #[serde(rename = "hd", default)]
        hidden: bool,
        c: Property,
        o: Property,
        w: Property,
        lc: Option<u8>,
        lj: Option<u8>,
        ml: Option<f64>,
        bm: Option<u8>,

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
        s: Property,
        e: Property,
        o: Property,
        m: Option<u8>,
    },

    #[serde(other)]
    Unknown,
}
