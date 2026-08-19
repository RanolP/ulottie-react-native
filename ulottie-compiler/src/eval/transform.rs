//! Layer and group transform composition. Mirrors `composeTransform` and the
//! `updateLayer` math in `runtime/driver.js`.

use anyhow::Result;

use crate::data::{self, InlineProp, Value};

use super::frame::Transform2D;
use super::property::{eval_inline, eval_scalar_or};

#[derive(Debug, Clone, Copy)]
pub struct TransformSpec {
    pub position: [f64; 2],
    pub anchor: [f64; 2],
    pub scale: [f64; 2],
    pub rotation: f64,
    /// Skew angle, degrees. lottie-web applies `skewFromAxis(-sk, sa)`
    /// between scale and rotation.
    pub skew: f64,
    /// Skew axis, degrees.
    pub skew_axis: f64,
    pub opacity: f64,
}

impl Default for TransformSpec {
    fn default() -> Self {
        Self {
            position: [0.0, 0.0],
            anchor: [0.0, 0.0],
            skew: 0.0,
            skew_axis: 0.0,
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
        let mut m00 = cos * sx;
        let mut m10 = sin * sx;
        let mut m01 = -sin * sy;
        let mut m11 = cos * sy;
        if self.skew != 0.0 {
            // lottie-web's `skewFromAxis(-sk, sa)`, between scale and
            // rotation. In column convention the factor is
            // `[1 + t·s·c, t·c²; −t·s², 1 − t·s·c]` with `t = tan(−sk)`.
            let t = (-self.skew.to_radians()).tan();
            let (s_, c_) = self.skew_axis.to_radians().sin_cos();
            let f00 = 1.0 + t * s_ * c_;
            let f01 = t * c_ * c_;
            let f10 = -t * s_ * s_;
            let f11 = 1.0 - t * s_ * c_;
            // Linear part becomes R · F · S: the rotation columns computed
            // above already carry S on the right, so fold F between them by
            // recomposing from the pure rotation and pure scale.
            let (r00, r10, r01, r11) = (cos, sin, -sin, cos);
            let g00 = r00 * f00 + r01 * f10;
            let g10 = r10 * f00 + r11 * f10;
            let g01 = r00 * f01 + r01 * f11;
            let g11 = r10 * f01 + r11 * f11;
            m00 = g00 * sx;
            m10 = g10 * sx;
            m01 = g01 * sy;
            m11 = g11 * sy;
        }
        let dx = self.position[0] - (m00 * self.anchor[0] + m01 * self.anchor[1]);
        let dy = self.position[1] - (m10 * self.anchor[0] + m11 * self.anchor[1]);
        Transform2D {
            m: [m00, m10, m01, m11, dx, dy],
        }
    }
}

pub fn eval_layer_transform(layer: &data::Layer, frame: f64) -> Result<TransformSpec> {
    Ok(TransformSpec {
        position: eval_vec_or(layer.p.as_ref(), frame, [0.0, 0.0])?,
        anchor: eval_vec_or(layer.a.as_ref(), frame, [0.0, 0.0])?,
        scale: eval_vec_or(layer.sc.as_ref(), frame, [100.0, 100.0])?,
        rotation: eval_scalar_or(layer.r.as_ref(), frame, 0.0)?,
        skew: eval_scalar_or(layer.sk.as_ref(), frame, 0.0)?,
        skew_axis: eval_scalar_or(layer.sa.as_ref(), frame, 0.0)?,
        opacity: eval_scalar_or(layer.o.as_ref(), frame, 100.0)?,
    })
}

pub fn eval_group_transform(g: &data::GroupRef, frame: f64) -> Result<TransformSpec> {
    Ok(TransformSpec {
        position: eval_vec_or(g.p.as_ref(), frame, [0.0, 0.0])?,
        anchor: eval_vec_or(g.a.as_ref(), frame, [0.0, 0.0])?,
        scale: eval_vec_or(g.sc.as_ref(), frame, [100.0, 100.0])?,
        rotation: eval_scalar_or(g.r.as_ref(), frame, 0.0)?,
        skew: eval_scalar_or(g.sk.as_ref(), frame, 0.0)?,
        skew_axis: eval_scalar_or(g.sa.as_ref(), frame, 0.0)?,
        opacity: eval_scalar_or(g.o.as_ref(), frame, 100.0)?,
    })
}

fn eval_vec_or(prop: Option<&InlineProp>, frame: f64, default: [f64; 2]) -> Result<[f64; 2]> {
    let Some(prop) = prop else { return Ok(default) };
    match eval_inline(prop, frame)? {
        Value::Scalar(n) => Ok([n, n]),
        Value::Vector(v) if v.len() >= 2 => Ok([v[0], v[1]]),
        Value::Vector(v) if v.len() == 1 => Ok([v[0], default[1]]),
        other => Err(anyhow::anyhow!("expected vec2, got {:?}", other)),
    }
}
