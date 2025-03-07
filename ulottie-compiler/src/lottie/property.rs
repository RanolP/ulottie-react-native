use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};

#[derive(Serialize_repr, Deserialize_repr, Debug)]
#[repr(u8)]
enum Fixed {
    X = 0,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct FixedProperty {
    #[serde(rename = "a")]
    property_tag: Fixed,
}

#[derive(Serialize_repr, Deserialize_repr, Debug)]
#[repr(u8)]
enum Animated {
    X = 1,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct AnimatedProperty {
    #[serde(rename = "a")]
    property_tag: Animated,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(untagged)]
pub enum Property {
    Fixed(FixedProperty),
    Animated(AnimatedProperty),
}
