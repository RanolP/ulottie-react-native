//! Property evaluation. Handles static + animated; expression and pattern
//! variants land in later H2 steps.

use anyhow::{Result, anyhow};

use crate::data::{self, Payload, Property, Value};

use super::frame::{BezierPath, Color};
use super::keyframes;

/// Resolve a property to a `Value` at `frame`. Single entry point for
/// shape-shape-aware code; downstream helpers narrow to scalar / vec / path.
pub fn eval_value(payload: &Payload, id: u32, frame: f64) -> Result<Value> {
    match lookup(payload, id)? {
        Property::Static(s) => Ok(s.k.clone()),
        Property::Animated(a) => keyframes::interpolate(&a.kf, frame),
        Property::Expression(e) => {
            // Try animated fallback (the underlying property's keyframes) first,
            // then static fallback. Expression evaluation comes in step 7.
            if let Some(kf) = &e.kf {
                return keyframes::interpolate(kf, frame);
            }
            e.fb
                .clone()
                .ok_or_else(|| anyhow!("expression property {id} has no usable fallback"))
        }
        Property::Pattern(_) => Err(anyhow!("pattern property {id} not yet supported (step 10)")),
    }
}

pub fn eval_scalar(payload: &Payload, id: u32, frame: f64) -> Result<f64> {
    match eval_value(payload, id, frame)? {
        Value::Scalar(n) => Ok(n),
        Value::Vector(v) if v.len() == 1 => Ok(v[0]),
        Value::Vector(v) => Err(anyhow!(
            "expected scalar at property {id}, got vec[{}]",
            v.len()
        )),
        Value::Path(_) => Err(anyhow!("expected scalar at property {id}, got path")),
    }
}

pub fn eval_vec2(payload: &Payload, id: u32, frame: f64) -> Result<[f64; 2]> {
    match eval_value(payload, id, frame)? {
        Value::Vector(v) if v.len() >= 2 => Ok([v[0], v[1]]),
        Value::Scalar(n) => Ok([n, n]),
        v => Err(anyhow!("expected vec2 at property {id}, got {:?}", v)),
    }
}

pub fn eval_color(payload: &Payload, id: u32, frame: f64) -> Result<Color> {
    match eval_value(payload, id, frame)? {
        Value::Vector(v) if v.len() >= 3 => Ok(Color {
            r: v[0],
            g: v[1],
            b: v[2],
            a: v.get(3).copied().unwrap_or(1.0),
        }),
        v => Err(anyhow!("expected color vec at property {id}, got {:?}", v)),
    }
}

pub fn eval_path(payload: &Payload, id: u32, frame: f64) -> Result<BezierPath> {
    match eval_value(payload, id, frame)? {
        Value::Path(p) => Ok(BezierPath {
            vertices: p.v,
            in_tangents: p.i,
            out_tangents: p.o,
            closed: p.c,
        }),
        v => Err(anyhow!("expected path at property {id}, got {:?}", v)),
    }
}

/// Optional variant: returns the default when the property id is `None`.
/// Used for layer transform components that default to identity when absent.
pub fn eval_scalar_or(payload: &Payload, id: Option<u32>, frame: f64, default: f64) -> Result<f64> {
    match id {
        Some(i) => eval_scalar(payload, i, frame),
        None => Ok(default),
    }
}

fn lookup(payload: &Payload, id: u32) -> Result<&data::Property> {
    payload
        .p
        .get(id as usize)
        .ok_or_else(|| anyhow!("property id {id} out of range"))
}
