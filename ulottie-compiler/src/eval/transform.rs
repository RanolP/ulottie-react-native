//! Layer and group transform composition. Mirrors `composeTransform` and the
//! `updateLayer` math in `runtime/driver.js`.
//!
//! The Lottie transform chain is:
//!
//! ```text
//! translate(position) · rotate(rotation) · scale(scale/100) · translate(-anchor)
//! ```
//!
//! Returned as a 2D affine `Transform2D` `[m00 m01 m10 m11 dx dy]`
//! (SVG `matrix(a,b,c,d,e,f)` order — column-major).

use anyhow::Result;

use crate::data::{self, Payload};

use super::frame::Transform2D;
use super::property::{eval_scalar_or, eval_value};

#[derive(Debug, Clone, Copy)]
pub struct TransformSpec {
    pub position: [f64; 2],
    pub anchor: [f64; 2],
    pub scale: [f64; 2],
    pub rotation: f64,
    pub opacity: f64,
}

impl Default for TransformSpec {
    fn default() -> Self {
        Self {
            position: [0.0, 0.0],
            anchor: [0.0, 0.0],
            scale: [100.0, 100.0],
            rotation: 0.0,
            opacity: 100.0,
        }
    }
}

impl TransformSpec {
    pub fn to_matrix(self) -> Transform2D {
        let theta = self.rotation.to_radians();
        let (sin, cos) = theta.sin_cos();
        let sx = self.scale[0] / 100.0;
        let sy = self.scale[1] / 100.0;
        let m00 = cos * sx;
        let m10 = sin * sx;
        let m01 = -sin * sy;
        let m11 = cos * sy;
        let dx = self.position[0] - (m00 * self.anchor[0] + m01 * self.anchor[1]);
        let dy = self.position[1] - (m10 * self.anchor[0] + m11 * self.anchor[1]);
        Transform2D { m: [m00, m10, m01, m11, dx, dy] }
    }
}

/// Read a layer's transform property bundle and produce a `TransformSpec`.
pub fn eval_layer_transform(payload: &Payload, layer: &data::Layer, frame: f64) -> Result<TransformSpec> {
    let position = eval_vec_or(payload, layer.p, frame, [0.0, 0.0])?;
    let anchor = eval_vec_or(payload, layer.a, frame, [0.0, 0.0])?;
    let scale = eval_vec_or(payload, layer.sc, frame, [100.0, 100.0])?;
    let rotation = eval_scalar_or(payload, layer.r, frame, 0.0)?;
    let opacity = eval_scalar_or(payload, layer.o, frame, 100.0)?;
    Ok(TransformSpec { position, anchor, scale, rotation, opacity })
}

/// Same shape as `eval_layer_transform`, applied to a `GroupRef`'s local
/// transform bundle.
pub fn eval_group_transform(payload: &Payload, g: &data::GroupRef, frame: f64) -> Result<TransformSpec> {
    let position = eval_vec_or(payload, g.p, frame, [0.0, 0.0])?;
    let anchor = eval_vec_or(payload, g.a, frame, [0.0, 0.0])?;
    let scale = eval_vec_or(payload, g.sc, frame, [100.0, 100.0])?;
    let rotation = eval_scalar_or(payload, g.r, frame, 0.0)?;
    let opacity = eval_scalar_or(payload, g.o, frame, 100.0)?;
    Ok(TransformSpec { position, anchor, scale, rotation, opacity })
}

// A position/anchor/scale property may be vec2 OR vec3 (when ddd=1). Treat
// any vec≥2 as vec2 by truncation; missing axes use the default.
fn eval_vec_or(
    payload: &Payload,
    id: Option<u32>,
    frame: f64,
    default: [f64; 2],
) -> Result<[f64; 2]> {
    let Some(id) = id else { return Ok(default) };
    match eval_value(payload, id, frame)? {
        crate::data::Value::Scalar(n) => Ok([n, n]),
        crate::data::Value::Vector(v) if v.len() >= 2 => Ok([v[0], v[1]]),
        crate::data::Value::Vector(v) if v.len() == 1 => Ok([v[0], default[1]]),
        other => Err(anyhow::anyhow!("expected vec2 at property {id}, got {:?}", other)),
    }
}
