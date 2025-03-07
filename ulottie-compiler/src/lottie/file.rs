use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct Animation {
    #[serde(rename = "v")]
    pub version: String,

    #[serde(rename = "nm")]
    pub name: Option<String>,

    #[serde(rename = "w")]
    pub width: u32,

    #[serde(rename = "h")]
    pub height: u32,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Layer {}

#[derive(Serialize, Deserialize, Debug)]
pub struct Asset {
    pub id: String,

    #[serde(rename = "nm")]
    pub name: Option<String>,
}

