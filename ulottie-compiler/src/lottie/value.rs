use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct Vector1(f32);

#[derive(Serialize, Deserialize, Debug)]
pub struct Vector2(f32, f32);

#[derive(Serialize, Deserialize, Debug)]
pub struct Vector3(f32, f32, f32);

#[derive(Serialize, Deserialize, Debug)]
#[serde(untagged)]
pub enum Vector {
    Vector1(f32),
    Vector2(f32, f32),
    Vector3(f32, f32, f32),
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(untagged)]
pub enum Color {
    RGB(u8, u8, u8),
    Hex(String),
}

#[derive(Serialize, Deserialize, Debug)]
pub struct BazierShape {
    #[serde(rename = "c")]
    pub closed: bool,

    #[serde(rename = "v")]
    pub vertices: Vec<Vector2>,

    #[serde(rename = "i")]
    pub in_tangents: Vec<Vector2>,

    #[serde(rename = "o")]
    pub out_tangents: Vec<Vector2>,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(untagged)]
pub enum Value {
    Vector(Vector),
    Color(Color),
    BazierShape(BazierShape),
}
