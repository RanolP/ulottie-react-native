//! Gradient stop resolution. Mirrors `ensureGradient` in driver.js — Lottie
//! stores color + alpha stops separately (positions don't always align), so we
//! sample both lists piecewise-linear at every position present in either.
//!
//! The Lottie packing is:
//!   color stops: `[pos, r, g, b, pos, r, g, b, ...]`  (count = `g.p`)
//!   alpha stops follow: `[pos, a, pos, a, ...]`

use anyhow::{Result, anyhow};
use serde_json::Value as Json;

use super::frame::{Color, GradientStop};

/// A keyframed colour ramp, split into one animated property per `<stop>`.
///
/// SVG cannot add or remove stops between frames, so an animated ramp is only
/// representable while the stop *count* is fixed — which Lottie guarantees:
/// `g.p` is a single number for the whole property, and After Effects will not
/// let a ramp gain a stop mid-animation. What moves is each stop's position
/// and colour, which is exactly `[offset, r, g, b]` per keyframe.
///
/// Alpha stops are the case this does not cover. They sit at positions of
/// their own, independent of the colour stops, so a fixed set of `<stop>`
/// elements cannot carry both once either set starts moving — lottie-web
/// answers that with a second gradient used as a mask. Returning `None` leaves
/// such a ramp reported as unsupported rather than silently resampled.
pub struct AnimatedRamp {
    /// Keyframe times, shared by every stop.
    pub times: Vec<f64>,
    /// Per segment: the outgoing and incoming easing handles, as
    /// `[ox, oy, ix, iy]`. Shared by every stop, like the times.
    pub easing: Vec<[f64; 4]>,
    /// Per stop, per keyframe: `[offset, r, g, b]`.
    pub stops: Vec<Vec<[f64; 4]>>,
    /// Per segment: whether it holds its start value.
    pub holds: Vec<bool>,
}

pub fn animated_ramp(g: &Json) -> Option<AnimatedRamp> {
    let count = g.get("p").and_then(Json::as_u64)? as usize;
    let k = g.get("k")?;
    if k.get("a").and_then(Json::as_u64) != Some(1) {
        return None;
    }
    let frames = k.get("k")?.as_array()?;
    if count == 0 || frames.is_empty() {
        return None;
    }

    let mut times = Vec::with_capacity(frames.len());
    let mut easing = Vec::with_capacity(frames.len());
    let mut holds = Vec::with_capacity(frames.len());
    let mut stops = vec![Vec::with_capacity(frames.len()); count];
    for kf in frames {
        // Lottie's older form leaves the final keyframe a bare terminator with
        // no `s`; there is nothing to interpolate past it, so it is dropped
        // rather than read as zeros.
        let Some(vals) = kf.get("s").and_then(Json::as_array) else {
            continue;
        };
        // Anything beyond the colour stops is the alpha ramp.
        if vals.len() != count * 4 {
            return None;
        }
        times.push(kf.get("t").and_then(Json::as_f64)?);
        // A handle is per-component in the general case; a ramp's components
        // are all eased together in every file AE writes, so the first is
        // taken and a genuinely per-component one would be approximated.
        let h = |name: &str, axis: &str, dflt: f64| {
            kf.get(name)
                .and_then(|e| e.get(axis))
                .map(|v| match v {
                    Json::Array(a) => a.first().and_then(Json::as_f64).unwrap_or(dflt),
                    other => other.as_f64().unwrap_or(dflt),
                })
                .unwrap_or(dflt)
        };
        easing.push([
            h("o", "x", 0.0),
            h("o", "y", 0.0),
            h("i", "x", 1.0),
            h("i", "y", 1.0),
        ]);
        holds.push(kf.get("h").and_then(Json::as_f64).unwrap_or(0.0) != 0.0);
        for i in 0..count {
            let n = |j: usize| vals[i * 4 + j].as_f64().unwrap_or(0.0);
            stops[i].push([n(0), n(1), n(2), n(3)]);
        }
    }
    if times.len() < 2 {
        return None;
    }
    Some(AnimatedRamp {
        times,
        easing,
        stops,
        holds,
    })
}

pub fn resolve_stops(g: &Json) -> Result<Vec<GradientStop>> {
    let color_count = g
        .get("p")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow!("gradient `g.p` missing"))? as usize;
    let k = g
        .get("k")
        .ok_or_else(|| anyhow!("gradient `g.k` missing"))?;
    // `k` may be a property-shaped object `{a, k:[...]}` (static) or `{a, k:[...]}` animated.
    // For now we only handle static gradients — animated will land later.
    let arr = if let Some(arr) = k.as_array() {
        arr.clone()
    } else if let Some(inner) = k.get("k").and_then(|v| v.as_array()) {
        inner.clone()
    } else {
        return Err(anyhow!("gradient `g.k` is not a number array or {{k:[…]}}"));
    };
    let nums: Vec<f64> = arr.iter().map(|v| v.as_f64().unwrap_or(0.0)).collect();

    let color_stride = 4usize;
    let mut colors: Vec<(f64, Color)> = Vec::with_capacity(color_count);
    for i in 0..color_count {
        let base = i * color_stride;
        if base + 3 >= nums.len() {
            break;
        }
        colors.push((
            nums[base],
            Color {
                r: nums[base + 1],
                g: nums[base + 2],
                b: nums[base + 3],
                a: 1.0,
            },
        ));
    }
    let mut alphas: Vec<(f64, f64)> = Vec::new();
    let mut idx = color_count * color_stride;
    while idx + 1 < nums.len() {
        alphas.push((nums[idx], nums[idx + 1]));
        idx += 2;
    }

    // Union of positions, sorted.
    let mut positions: Vec<f64> = colors
        .iter()
        .map(|s| s.0)
        .chain(alphas.iter().map(|s| s.0))
        .collect();
    positions.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    positions.dedup_by(|a, b| (*a - *b).abs() < f64::EPSILON);

    let mut out = Vec::with_capacity(positions.len());
    for p in positions {
        let mut c = sample_color(&colors, p);
        if !alphas.is_empty() {
            c.a = sample_alpha(&alphas, p);
        }
        out.push(GradientStop {
            offset: p,
            color: c,
        });
    }
    Ok(out)
}

fn sample_color(list: &[(f64, Color)], pos: f64) -> Color {
    if list.is_empty() {
        return Color {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        };
    }
    if pos <= list[0].0 {
        return list[0].1;
    }
    if pos >= list[list.len() - 1].0 {
        return list[list.len() - 1].1;
    }
    for w in list.windows(2) {
        let (pa, a) = (w[0].0, w[0].1);
        let (pb, b) = (w[1].0, w[1].1);
        if pos >= pa && pos <= pb {
            let t = if (pb - pa).abs() < f64::EPSILON {
                0.0
            } else {
                (pos - pa) / (pb - pa)
            };
            return Color {
                r: a.r + (b.r - a.r) * t,
                g: a.g + (b.g - a.g) * t,
                b: a.b + (b.b - a.b) * t,
                a: a.a + (b.a - a.a) * t,
            };
        }
    }
    list[0].1
}

fn sample_alpha(list: &[(f64, f64)], pos: f64) -> f64 {
    if list.is_empty() {
        return 1.0;
    }
    if pos <= list[0].0 {
        return list[0].1;
    }
    if pos >= list[list.len() - 1].0 {
        return list[list.len() - 1].1;
    }
    for w in list.windows(2) {
        let (pa, a) = (w[0].0, w[0].1);
        let (pb, b) = (w[1].0, w[1].1);
        if pos >= pa && pos <= pb {
            let t = if (pb - pa).abs() < f64::EPSILON {
                0.0
            } else {
                (pos - pa) / (pb - pa)
            };
            return a + (b - a) * t;
        }
    }
    1.0
}
