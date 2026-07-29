//! Property evaluation for the frame emulator.
//!
//! Works with the inlined property format — no table lookup.

use anyhow::{Result, anyhow};

use crate::data::{InlineProp, Value};

use super::frame::{BezierPath, Color};
use super::keyframes;

/// Evaluate an inline property at `frame`.
pub fn eval_inline(prop: &InlineProp, frame: f64) -> Result<Value> {
    match prop {
        InlineProp::Static(v) => Ok(v.clone()),
        InlineProp::Animated(kf) => keyframes::interpolate(kf, frame),
        InlineProp::Expression(e) => {
            if let Some(kf) = &e.kf {
                return keyframes::interpolate(kf, frame);
            }
            e.fb.clone()
                .ok_or_else(|| anyhow!("expression property has no fallback"))
        }
    }
}

pub fn eval_scalar(prop: &InlineProp, frame: f64) -> Result<f64> {
    match eval_inline(prop, frame)? {
        Value::Scalar(n) => Ok(n),
        Value::Vector(v) if v.len() == 1 => Ok(v[0]),
        v => Err(anyhow!("expected scalar, got {:?}", v)),
    }
}

pub fn eval_vec2(prop: &InlineProp, frame: f64) -> Result<[f64; 2]> {
    match eval_inline(prop, frame)? {
        Value::Vector(v) if v.len() >= 2 => Ok([v[0], v[1]]),
        Value::Scalar(n) => Ok([n, n]),
        v => Err(anyhow!("expected vec2, got {:?}", v)),
    }
}

pub fn eval_color(prop: &InlineProp, frame: f64) -> Result<Color> {
    match eval_inline(prop, frame)? {
        Value::Vector(v) if v.len() >= 3 => Ok(Color {
            r: v[0],
            g: v[1],
            b: v[2],
            a: v.get(3).copied().unwrap_or(1.0),
        }),
        v => Err(anyhow!("expected color vec, got {:?}", v)),
    }
}

pub fn eval_path(prop: &InlineProp, frame: f64) -> Result<BezierPath> {
    match eval_inline(prop, frame)? {
        Value::Path(p) => Ok(BezierPath {
            vertices: p.v,
            in_tangents: p.i,
            out_tangents: p.o,
            closed: p.c,
        }),
        v => Err(anyhow!("expected path, got {:?}", v)),
    }
}

/// Optional variant: returns the default when the property is `None`.
pub fn eval_scalar_or(prop: Option<&InlineProp>, frame: f64, default: f64) -> Result<f64> {
    match prop {
        Some(p) => eval_scalar(p, frame),
        None => Ok(default),
    }
}
