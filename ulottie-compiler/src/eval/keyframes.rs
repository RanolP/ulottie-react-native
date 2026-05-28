//! Keyframe interpolation. Mirrors `interpolateKf` + `lerpValue` in
//! `runtime/driver.js`.
//!
//! Supports:
//! - scalar / vector lerp with optional cubic-bezier easing handles
//! - spatial bezier (cubic 2D/3D motion path via `to`/`ti` tangents and
//!   arc-length parameterization)
//! - bezier path lerp (per-vertex; closure flips at t > 0.5)
//!
//! Easing handle parsing: each segment carries a pair (out-of-start,
//! in-of-end). `x` is the *input* time fraction; `y` is the *output* value
//! fraction. Both can be scalars or per-component vectors; we read the first
//! component as a representative scalar.

use anyhow::{Result, anyhow};

use crate::data::{EasingComponent, EasingHandle, EasingPair, Keyframes, PathValue, Value};

/// Number of arc-length samples per spatial bezier segment. Matches driver.js.
const SPATIAL_SAMPLES: usize = 200;
/// Newton-Raphson iterations for solving bezier_x(t) = u.
const NEWTON_ITERS: usize = 8;

/// Interpolate `kf` at `frame` (in composition frame units).
pub fn interpolate(kf: &Keyframes, frame: f64) -> Result<Value> {
    let n = kf.t.len();
    if n == 0 {
        return Err(anyhow!("empty keyframes"));
    }
    if frame <= kf.t[0] {
        return Ok(first_valid(kf));
    }
    if frame >= kf.t[n - 1] {
        return Ok(last_valid(kf));
    }
    let i = find_segment(&kf.t, frame);
    let t0 = kf.t[i];
    let t1 = kf.t[i + 1];
    let u_lin = if t1 > t0 { (frame - t0) / (t1 - t0) } else { 0.0 };
    // Start value. Lottie's "hold-last" pattern sometimes leaves `v[i]` as
    // an empty vector when this keyframe only exists to mark the end of the
    // previous segment — in that case fall back to `e[i-1]` or `v[i-1]`.
    let v0_owned = resolve_start(kf, i);
    let v0: &Value = &v0_owned;
    // End value: prefer Lottie's older `e[i]` if present, else next keyframe.
    let v1_owned = kf
        .e
        .as_ref()
        .and_then(|e| e.get(i).and_then(|x| x.clone()))
        .unwrap_or_else(|| resolve_start(kf, i + 1));
    let v1: &Value = &v1_owned;

    // Easing: scalar bezier on the time axis.
    let u = match &kf.oi {
        Some(oi) if i < oi.len() => apply_easing(u_lin, &oi[i]),
        _ => u_lin,
    };

    // Spatial bezier (cubic motion path in 2D/3D). Only when both `to` and
    // `ti` exist AND both endpoints are vectors of the same dimension.
    if let (Some(to_arr), Some(ti_arr)) = (&kf.to, &kf.ti) {
        if let (Value::Vector(a), Value::Vector(b)) = (v0, v1) {
            if a.len() == b.len() && i < to_arr.len() && i < ti_arr.len() {
                let to = &to_arr[i];
                let ti = &ti_arr[i];
                if to.len() == a.len() && ti.len() == a.len() {
                    return Ok(Value::Vector(spatial_bezier(a, b, to, ti, u)));
                }
            }
        }
    }

    Ok(lerp_value(v0, v1, u))
}

fn find_segment(t: &[f64], frame: f64) -> usize {
    // Linear scan — keyframe counts per property rarely exceed ~30.
    let mut i = 0;
    while i + 1 < t.len() && t[i + 1] <= frame {
        i += 1;
    }
    i
}

/// Resolve the "real" value at keyframe `i`. Lottie sometimes stores empty
/// vectors at hold-last keyframes — fall back through `e[i-1]` then `v[i-1]`
/// to recover something meaningful. Mirrors driver.js behavior.
fn resolve_start(kf: &Keyframes, i: usize) -> Value {
    if i >= kf.v.len() {
        return last_valid(kf);
    }
    if !is_empty(&kf.v[i]) {
        return kf.v[i].clone();
    }
    if i > 0 {
        if let Some(e) = kf
            .e
            .as_ref()
            .and_then(|e| e.get(i - 1).and_then(|x| x.clone()))
        {
            if !is_empty(&e) {
                return e;
            }
        }
        if !is_empty(&kf.v[i - 1]) {
            return kf.v[i - 1].clone();
        }
    }
    kf.v[i].clone()
}

fn first_valid(kf: &Keyframes) -> Value {
    for v in &kf.v {
        if !is_empty(v) {
            return v.clone();
        }
    }
    kf.v[0].clone()
}

fn last_valid(kf: &Keyframes) -> Value {
    // Walk backwards through v[], skipping holds-with-empty-vector. If the
    // last non-empty value lives in e[i-1], that wins (the segment that ended
    // *at* the last keyframe).
    for i in (0..kf.v.len()).rev() {
        if !is_empty(&kf.v[i]) {
            return kf.v[i].clone();
        }
        if i > 0 {
            if let Some(e) = kf
                .e
                .as_ref()
                .and_then(|e| e.get(i - 1).and_then(|x| x.clone()))
            {
                if !is_empty(&e) {
                    return e;
                }
            }
        }
    }
    kf.v[kf.v.len() - 1].clone()
}

fn is_empty(v: &Value) -> bool {
    matches!(v, Value::Vector(x) if x.is_empty())
}

// ---------------------------------------------------------------------------
// Easing
// ---------------------------------------------------------------------------

fn apply_easing(u: f64, pair: &EasingPair) -> f64 {
    let (x1, y1) = scalar_handle(&pair.o);
    let (x2, y2) = scalar_handle(&pair.i);
    cubic_bezier(u, x1, y1, x2, y2)
}

fn scalar_handle(h: &EasingHandle) -> (f64, f64) {
    let x = match &h.x {
        EasingComponent::Scalar(n) => *n,
        EasingComponent::PerComponent(v) => v.first().copied().unwrap_or(0.0),
    };
    let y = match &h.y {
        EasingComponent::Scalar(n) => *n,
        EasingComponent::PerComponent(v) => v.first().copied().unwrap_or(0.0),
    };
    (x, y)
}

/// Find t such that bezier_x(t) = u, then return bezier_y(t). The bezier
/// has control points (0,0), (x1,y1), (x2,y2), (1,1). Solves via
/// Newton-Raphson seeded at u, with eight iterations. Matches `cubicBezier`
/// in driver.js exactly.
fn cubic_bezier(u: f64, x1: f64, y1: f64, x2: f64, y2: f64) -> f64 {
    fn b(p1: f64, p2: f64, t: f64) -> f64 {
        let it = 1.0 - t;
        3.0 * it * it * t * p1 + 3.0 * it * t * t * p2 + t * t * t
    }
    fn db(p1: f64, p2: f64, t: f64) -> f64 {
        let it = 1.0 - t;
        3.0 * it * it * p1 + 6.0 * it * t * (p2 - p1) + 3.0 * t * t * (1.0 - p2)
    }
    let mut t = u;
    for _ in 0..NEWTON_ITERS {
        let x = b(x1, x2, t);
        let dx = db(x1, x2, t);
        if dx.abs() < 1e-12 {
            break;
        }
        t -= (x - u) / dx;
        t = t.clamp(0.0, 1.0);
    }
    b(y1, y2, t)
}

// ---------------------------------------------------------------------------
// Value lerp
// ---------------------------------------------------------------------------

fn lerp_value(a: &Value, b: &Value, t: f64) -> Value {
    match (a, b) {
        (Value::Scalar(x), Value::Scalar(y)) => Value::Scalar(x + (y - x) * t),
        (Value::Vector(x), Value::Vector(y)) => {
            let n = x.len().min(y.len());
            Value::Vector((0..n).map(|i| x[i] + (y[i] - x[i]) * t).collect())
        }
        (Value::Path(x), Value::Path(y)) => Value::Path(lerp_path(x, y, t)),
        // Mixed shape — fall back to "a" before mid-segment, "b" after.
        _ => {
            if t < 0.5 {
                a.clone()
            } else {
                b.clone()
            }
        }
    }
}

fn lerp_path(a: &PathValue, b: &PathValue, t: f64) -> PathValue {
    let n = a.v.len().min(b.v.len());
    let lerp = |a: f64, b: f64| a + (b - a) * t;
    let mut v = Vec::with_capacity(n);
    let mut i = Vec::with_capacity(n);
    let mut o = Vec::with_capacity(n);
    for k in 0..n {
        v.push([lerp(a.v[k][0], b.v[k][0]), lerp(a.v[k][1], b.v[k][1])]);
        i.push([lerp(a.i[k][0], b.i[k][0]), lerp(a.i[k][1], b.i[k][1])]);
        o.push([lerp(a.o[k][0], b.o[k][0]), lerp(a.o[k][1], b.o[k][1])]);
    }
    // Closure flips at t > 0.5 (matches driver.js).
    let c = if t < 0.5 { a.c } else { b.c };
    PathValue { v, i, o, c }
}

// ---------------------------------------------------------------------------
// Spatial bezier
// ---------------------------------------------------------------------------

/// Cubic bezier motion path from `a` to `b` with control points
/// `a + to` and `b + ti`. `u` is the (already-eased) time fraction.
/// Output is parameterized by arc length so motion looks even-paced across
/// the curve — matches `lerpValue` spatial branch in driver.js.
fn spatial_bezier(a: &[f64], b: &[f64], to: &[f64], ti: &[f64], u: f64) -> Vec<f64> {
    let n = a.len();
    let c1: Vec<f64> = (0..n).map(|i| a[i] + to[i]).collect();
    let c2: Vec<f64> = (0..n).map(|i| b[i] + ti[i]).collect();
    // Pre-sample arc lengths.
    let mut samples = Vec::with_capacity(SPATIAL_SAMPLES + 1);
    let mut cumul = vec![0.0_f64; SPATIAL_SAMPLES + 1];
    let mut prev: Option<Vec<f64>> = None;
    for i in 0..=SPATIAL_SAMPLES {
        let t = i as f64 / SPATIAL_SAMPLES as f64;
        let p = cubic_at(a, &c1, &c2, b, t);
        if let Some(prv) = &prev {
            let d: f64 = (0..n).map(|k| (p[k] - prv[k]).powi(2)).sum::<f64>().sqrt();
            cumul[i] = cumul[i - 1] + d;
        }
        samples.push(p.clone());
        prev = Some(p);
    }
    let total = cumul[SPATIAL_SAMPLES];
    if total <= f64::EPSILON {
        return a.to_vec();
    }
    let target = u * total;
    // Binary search for the segment whose cumulative length brackets target.
    let mut lo = 0usize;
    let mut hi = SPATIAL_SAMPLES;
    while lo + 1 < hi {
        let mid = (lo + hi) / 2;
        if cumul[mid] <= target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let span = cumul[hi] - cumul[lo];
    let frac = if span > 0.0 { (target - cumul[lo]) / span } else { 0.0 };
    (0..n)
        .map(|k| samples[lo][k] + (samples[hi][k] - samples[lo][k]) * frac)
        .collect()
}

fn cubic_at(p0: &[f64], p1: &[f64], p2: &[f64], p3: &[f64], t: f64) -> Vec<f64> {
    let it = 1.0 - t;
    let b0 = it * it * it;
    let b1 = 3.0 * it * it * t;
    let b2 = 3.0 * it * t * t;
    let b3 = t * t * t;
    (0..p0.len())
        .map(|k| b0 * p0[k] + b1 * p1[k] + b2 * p2[k] + b3 * p3[k])
        .collect()
}
