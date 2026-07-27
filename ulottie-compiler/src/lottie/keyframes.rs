use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum EasingValue {
    Scalar(f64),
    PerComponent(Vec<f64>),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct EasingHandle {
    pub x: EasingValue,
    pub y: EasingValue,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Keyframe {
    #[serde(rename = "t")]
    pub time: f64,

    /// Raw start value — may be a flat number array (scalar/vector kf) or an
    /// array containing a single bezier-path object (path kf). Path kfs use
    /// `[{v: [...], i: [...], o: [...], c: ...}]`.
    #[serde(rename = "s")]
    pub start_value: Option<serde_json::Value>,

    /// End value (older Lottie format). When present, the segment goes
    /// from `s` to `e` (instead of from this kf's `s` to next kf's `s`).
    #[serde(rename = "e")]
    pub end_value: Option<serde_json::Value>,

    #[serde(rename = "i")]
    pub in_tangent: Option<EasingHandle>,

    #[serde(rename = "o")]
    pub out_tangent: Option<EasingHandle>,

    #[serde(rename = "to")]
    pub spatial_tangent_to: Option<Vec<f64>>,

    #[serde(rename = "ti")]
    pub spatial_tangent_in: Option<Vec<f64>>,

    /// Hold (step) keyframe: the value stays put until the next keyframe
    /// instead of interpolating toward it.
    #[serde(rename = "h", default)]
    pub hold: Option<u8>,
}

impl Keyframe {
    /// Try to interpret `start_value` as a flat number array (for scalar /
    /// vector keyframes).
    pub fn start_numbers(&self) -> Option<Vec<f64>> {
        value_as_numbers(self.start_value.as_ref())
    }

    pub fn end_numbers(&self) -> Option<Vec<f64>> {
        value_as_numbers(self.end_value.as_ref())
    }

    /// Try to interpret `start_value` as a bezier-path object. Path keyframes
    /// wrap the path in a single-element array, so we unwrap one level if
    /// present.
    pub fn start_path(&self) -> Option<&serde_json::Value> {
        value_as_path(self.start_value.as_ref())
    }

    pub fn end_path(&self) -> Option<&serde_json::Value> {
        value_as_path(self.end_value.as_ref())
    }
}

fn value_as_numbers(v: Option<&serde_json::Value>) -> Option<Vec<f64>> {
    let v = v?;
    let arr = v.as_array()?;
    let mut out = Vec::with_capacity(arr.len());
    for x in arr {
        out.push(x.as_f64()?);
    }
    Some(out)
}

fn value_as_path(v: Option<&serde_json::Value>) -> Option<&serde_json::Value> {
    let v = v?;
    // Path keyframes wrap the path in a single-element array.
    if let Some(arr) = v.as_array() {
        if let Some(first) = arr.first() {
            if first.is_object() {
                return Some(first);
            }
        }
    }
    if v.is_object() {
        return Some(v);
    }
    None
}
