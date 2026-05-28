use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct BezierShape {
    #[serde(rename = "c", default)]
    pub closed: bool,

    #[serde(rename = "v")]
    pub vertices: Vec<Vec<f64>>,

    #[serde(rename = "i")]
    pub in_tangents: Vec<Vec<f64>>,

    #[serde(rename = "o")]
    pub out_tangents: Vec<Vec<f64>>,
}
