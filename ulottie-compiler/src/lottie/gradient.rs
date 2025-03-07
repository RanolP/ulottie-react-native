use std::fmt;

use serde::ser::SerializeSeq;

#[derive(Debug)]
pub struct Gradient {
    stops: Vec<GradientStop>,
}

#[derive(Debug)]
pub struct GradientStop {
    r: f32,
    g: f32,
    b: f32,
    a: f32,
}

impl serde::ser::Serialize for Gradient {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut transparent = false;

        let mut seq = serializer.serialize_seq(len)?;
        seq.serialize_element(value)?;
        seq.end()
    }
}

impl<'de> serde::de::Deserialize<'de> for Gradient {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct Visitor;

        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Gradient;

            fn expecting(&self, fmt: &mut fmt::Formatter) -> fmt::Result {}

            fn visit_seq<A>(self, seq: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
            }
        }
    }
}
