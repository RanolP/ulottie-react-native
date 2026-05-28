use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GradientStop {
    pub offset: f64,
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

#[derive(Debug, Clone)]
pub struct Gradient {
    pub stops: Vec<GradientStop>,
}
