use serde::{Deserialize, Serialize};

use super::constants::ShapeDirection;

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "ty")]
pub enum GraphicElement {
    // Shapes
    Ellipse {
        #[serde(rename = "nm")]
        name: Option<String>,
        #[serde(rename = "hd", default)]
        hidden: bool,
        #[serde(rename = "d")]
        direction: ShapeDirection,
    },
    Path {
        #[serde(rename = "nm")]
        name: Option<String>,
        #[serde(rename = "hd", default)]
        hidden: bool,
        #[serde(rename = "d")]
        direction: ShapeDirection,
    },
    Rectangle {
        #[serde(rename = "nm")]
        name: Option<String>,
        #[serde(rename = "hd", default)]
        hidden: bool,
        #[serde(rename = "d")]
        direction: ShapeDirection,
    },
    PolyStar {
        #[serde(rename = "nm")]
        name: Option<String>,
        #[serde(rename = "hd", default)]
        hidden: bool,
        #[serde(rename = "d")]
        direction: ShapeDirection,
    },

    // Grouping
    Group {
        #[serde(rename = "nm")]
        name: Option<String>,
        #[serde(rename = "hd", default)]
        hidden: bool,
    },
    Transform {
        #[serde(rename = "nm")]
        name: Option<String>,
        #[serde(rename = "hd", default)]
        hidden: bool,
    },

    // Style
    Fill {
        #[serde(rename = "nm")]
        name: Option<String>,
        #[serde(rename = "hd", default)]
        hidden: bool,
    },
    Gradient {
        #[serde(rename = "nm")]
        name: Option<String>,
        #[serde(rename = "hd", default)]
        hidden: bool,
    },
    GradientStroke {
        #[serde(rename = "nm")]
        name: Option<String>,
        #[serde(rename = "hd", default)]
        hidden: bool,
    },
    Stroke {
        #[serde(rename = "nm")]
        name: Option<String>,
        #[serde(rename = "hd", default)]
        hidden: bool,
    },

    // Modifiers
    TrimPath {
        #[serde(rename = "nm")]
        name: Option<String>,
        #[serde(rename = "hd", default)]
        hidden: bool,
    },
}
