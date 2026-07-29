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
