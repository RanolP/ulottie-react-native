use serde::{Deserialize, Serialize};

use super::keyframes::Keyframe;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AnimatedProperty {
    #[serde(rename = "a")]
    pub animated: Option<u8>,

    /// The keyframes, with Lottie's legacy end-value form already normalized
    /// away — see [`normalize_keyframes`].
    #[serde(rename = "k", deserialize_with = "normalize_keyframes")]
    pub keyframes: Vec<Keyframe>,

    pub ix: Option<u32>,

    /// Lottie expression (After Effects expression pre-transpiled to JS by Bodymovin)
    pub x: Option<String>,
}

/// Fold the legacy keyframe form into the modern one, at the parse boundary.
///
/// Lottie's older encoding puts a segment's *destination* in `e` on the
/// keyframe that starts it, and leaves the final keyframe a bare terminator
/// with no `s`. The modern encoding gives every keyframe a start value and
/// interpolates to the next one. The resolution — the one lottie-web applies
/// as `nextKeyData.s || keyData.e` — is to fill a missing `s` from the
/// previous keyframe's `e`, letting `s` win when both exist, and then `e` is
/// dead. Normalizing here means nothing downstream (IR, wire, runtime) ever
/// carries a second spelling of a segment's end value.
fn normalize_keyframes<'de, D>(d: D) -> Result<Vec<Keyframe>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let mut kfs = Vec::<Keyframe>::deserialize(d)?;
    for i in 1..kfs.len() {
        if kfs[i].start_value.is_none() {
            kfs[i].start_value = kfs[i - 1].end_value.take();
        }
    }
    for kf in &mut kfs {
        kf.end_value = None;
    }
    Ok(kfs)
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
            Property::Split(s) => {
                s.x.is_animated()
                    || s.y.is_animated()
                    || s.z.as_deref().map(Property::is_animated).unwrap_or(false)
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The legacy form's destination (`e` on the segment's first keyframe,
    /// terminator with no `s`) folds into the next keyframe's start value, and
    /// no `e` survives the parse. `next.s` wins when both exist — the
    /// precedence lottie-web applies.
    #[test]
    fn legacy_end_values_fold_into_start_values() {
        let p: AnimatedProperty = serde_json::from_str(
            r#"{"a":1,"k":[
                {"t":0,"s":[0],"e":[2.333],"o":{"x":0.3,"y":0},"i":{"x":0.7,"y":1}},
                {"t":140}
            ]}"#,
        )
        .unwrap();
        assert_eq!(p.keyframes.len(), 2);
        assert_eq!(p.keyframes[0].start_numbers().as_deref(), Some(&[0.0][..]));
        assert_eq!(
            p.keyframes[1].start_numbers().as_deref(),
            Some(&[2.333][..]),
            "the terminator inherits the previous keyframe's `e`"
        );
        assert!(p.keyframes.iter().all(|k| k.end_value.is_none()));
    }

    #[test]
    fn an_explicit_next_start_beats_a_legacy_end() {
        let p: AnimatedProperty = serde_json::from_str(
            r#"{"a":1,"k":[
                {"t":0,"s":[0],"e":[9]},
                {"t":10,"s":[1]}
            ]}"#,
        )
        .unwrap();
        assert_eq!(p.keyframes[1].start_numbers().as_deref(), Some(&[1.0][..]));
        assert!(p.keyframes.iter().all(|k| k.end_value.is_none()));
    }
}
