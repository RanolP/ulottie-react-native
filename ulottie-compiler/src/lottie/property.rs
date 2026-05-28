use serde::{Deserialize, Serialize};

use super::keyframes::Keyframe;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AnimatedProperty {
    #[serde(rename = "a")]
    pub animated: Option<u8>,

    #[serde(rename = "k")]
    pub keyframes: Vec<Keyframe>,

    pub ix: Option<u32>,

    /// Lottie expression (After Effects expression pre-transpiled to JS by Bodymovin)
    pub x: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StaticProperty {
    #[serde(rename = "a")]
    pub animated: Option<u8>,

    #[serde(rename = "k")]
    pub value: serde_json::Value,

    pub ix: Option<u32>,

    /// Lottie expression (After Effects expression pre-transpiled to JS by Bodymovin)
    pub x: Option<String>,
}

/// A Lottie property that can be either static, animated, or a split-axis
/// composite. Lottie's position property in AE has a "Separate Dimensions"
/// switch that emits `{s: true, x: {...}, y: {...}, z?: {...}}` — each axis
/// becomes its own independent property. We handle that here so the parser
/// doesn't reject fixtures that toggle the switch.
///
/// Variant order matters for untagged enums: try `Split` first (specific
/// `s: true` marker), then `Animated` (`k` is an array of objects), then
/// `Static` (`k` is anything via `serde_json::Value` — would otherwise
/// swallow the others).
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum Property {
    Split(SplitProperty),
    Animated(AnimatedProperty),
    Static(StaticProperty),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SplitProperty {
    /// Separate-dimensions flag. Always `true` for the split form; serde uses
    /// this as a discriminator since the untagged enum picks the first
    /// variant whose fields fit the input.
    pub s: bool,
    pub x: Box<Property>,
    pub y: Box<Property>,
    pub z: Option<Box<Property>>,
}

impl Property {
    pub fn is_animated(&self) -> bool {
        match self {
            Property::Animated(_) => true,
            Property::Split(s) => s.x.is_animated() || s.y.is_animated()
                || s.z.as_deref().map(Property::is_animated).unwrap_or(false),
            _ => false,
        }
    }

    pub fn static_value(&self) -> Option<&serde_json::Value> {
        match self {
            Property::Static(p) => Some(&p.value),
            _ => None,
        }
    }

    pub fn keyframes(&self) -> Option<&[Keyframe]> {
        match self {
            Property::Animated(p) => Some(&p.keyframes),
            _ => None,
        }
    }

    pub fn static_f64(&self) -> Option<f64> {
        self.static_value()?.as_f64()
    }

    pub fn static_array(&self) -> Option<Vec<f64>> {
        let arr = self.static_value()?.as_array()?;
        arr.iter().map(|v| v.as_f64()).collect()
    }

    pub fn expression(&self) -> Option<&str> {
        match self {
            Property::Animated(p) => p.x.as_deref(),
            Property::Static(p) => p.x.as_deref(),
            Property::Split(_) => None,
        }
    }

    pub fn has_expression(&self) -> bool {
        self.expression().is_some()
    }

    pub fn split(&self) -> Option<&SplitProperty> {
        match self {
            Property::Split(s) => Some(s),
            _ => None,
        }
    }
}
