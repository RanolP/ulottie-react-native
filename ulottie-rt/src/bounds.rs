//! Conservative geometry bounds, shared between the compiler's bbox pass and
//! the rasterizer's per-shape scratch sizing.
//!
//! Everything here over-approximates on purpose: cubic segments use their
//! control-point hull, strokes pad by the worst joint/cap reach, effects pad
//! by their full 3σ support. A too-large box costs a few scratch pixels; a
//! too-small one clips content.

use crate::rtdl::{Animation, FxPass, Geom, Group, Node, PathData, Shape};

/// `[x0, y0, x1, y1]`, valid only when `x0 <= x1 && y0 <= y1`.
pub type Aabb = [f32; 4];

pub fn union(a: Option<Aabb>, b: Option<Aabb>) -> Option<Aabb> {
    match (a, b) {
        (Some(a), Some(b)) => Some([
            a[0].min(b[0]),
            a[1].min(b[1]),
            a[2].max(b[2]),
            a[3].max(b[3]),
        ]),
        (a, None) => a,
        (None, b) => b,
    }
}

pub fn pad(b: Aabb, px: f32, py: f32) -> Aabb {
    [b[0] - px, b[1] - py, b[2] + px, b[3] + py]
}

/// Map a box through an SVG-order matrix `[a, b, c, d, e, f]` (axis-aligned
/// hull of the four mapped corners).
pub fn map_aabb(m: &[f32; 6], b: Aabb) -> Aabb {
    let mut out: Option<Aabb> = None;
    for (x, y) in [(b[0], b[1]), (b[2], b[1]), (b[0], b[3]), (b[2], b[3])] {
        let px = m[0] * x + m[2] * y + m[4];
        let py = m[1] * x + m[3] * y + m[5];
        out = union(out, Some([px, py, px, py]));
    }
    out.unwrap()
}

/// Control-point hull of a path; `None` when it has no points.
pub fn path_bounds(p: &PathData) -> Option<Aabb> {
    let mut out: Option<Aabb> = None;
    for xy in p.points.chunks_exact(2) {
        out = union(out, Some([xy[0], xy[1], xy[0], xy[1]]));
    }
    out
}

pub fn geom_bounds(g: &Geom) -> Option<Aabb> {
    match g {
        Geom::Path(p) => path_bounds(p),
        Geom::Rect { x, y, w, h, .. } => {
            if *w <= 0.0 || *h <= 0.0 {
                None
            } else {
                Some([*x, *y, x + w, y + h])
            }
        }
        Geom::Ellipse { cx, cy, rx, ry } => {
            if *rx <= 0.0 || *ry <= 0.0 {
                None
            } else {
                Some([cx - rx, cy - ry, cx + rx, cy + ry])
            }
        }
    }
}

/// How far a stroke can reach past its path: half the width, times the miter
/// limit for miter joins, and never less than the square-cap diagonal.
pub fn stroke_pad(s: &Shape) -> f32 {
    if s.paint.stroke.is_none() {
        return 0.0;
    }
    let joint = if s.paint.join == 0 {
        s.paint.miter_limit.max(1.0)
    } else {
        1.0
    };
    s.paint.stroke_width * 0.5 * joint.max(core::f32::consts::SQRT_2)
}

/// How far a group's effect stages can push content past its geometry
/// (per-axis pads, accumulated across stages).
pub fn fx_pad(g: &Group) -> (f32, f32) {
    let mut px = 0.0f32;
    let mut py = 0.0f32;
    for stage in &g.fx {
        for pass in &stage.passes {
            match pass {
                FxPass::Blur { sx, sy, .. } => {
                    px += 3.0 * sx;
                    py += 3.0 * sy;
                }
                FxPass::Shadow {
                    std_dev, dx, dy, ..
                } => {
                    px += 3.0 * std_dev + dx.abs();
                    py += 3.0 * std_dev + dy.abs();
                }
                _ => {}
            }
        }
    }
    (px, py)
}

/// Bounds of everything the node can paint, in its *parent's* space (the
/// node's own matrix applied). Hidden nodes and empty subtrees are `None`.
/// Mask children never enlarge the box — a mask only reveals what the
/// content painted.
pub fn subtree_bounds(anim: &Animation, idx: u32) -> Option<Aabb> {
    match &anim.nodes[idx as usize] {
        Node::Group(g) => {
            if g.hidden {
                return None;
            }
            let inner = group_inner_bounds(anim, g)?;
            Some(match &g.matrix {
                Some(m) => map_aabb(m, inner),
                None => inner,
            })
        }
        Node::Shape(s) => {
            if s.hidden {
                return None;
            }
            let p = stroke_pad(s);
            let b = pad(geom_bounds(&s.geom)?, p, p);
            Some(match &s.matrix {
                Some(m) => map_aabb(m, b),
                None => b,
            })
        }
        Node::Image(i) => Some([0.0, 0.0, i.w, i.h]),
    }
}

/// Bounds of a group's paint in its *inner* space: children union, expanded
/// by effect reach. A matte-inversion group paints its whole filter region
/// instead, so the region is the answer verbatim.
pub fn group_inner_bounds(anim: &Animation, g: &Group) -> Option<Aabb> {
    if let Some(cf) = &g.cf {
        return Some(cf.rect);
    }
    let mut b: Option<Aabb> = None;
    for &c in &g.children {
        b = union(b, subtree_bounds(anim, c));
    }
    let (px, py) = fx_pad(g);
    b.map(|b| pad(b, px, py))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rtdl::{Paint, PaintSource};

    /// The one runnable check: a stroked ellipse under a translate maps to
    /// the expected padded box.
    #[test]
    fn shape_bounds_with_stroke_and_matrix() {
        let anim = Animation {
            nodes: alloc::vec![Node::Shape(Shape {
                slot: None,
                matrix: Some([1.0, 0.0, 0.0, 1.0, 10.0, 0.0]),
                opacity: 1.0,
                hidden: false,
                geom: Geom::Ellipse {
                    cx: 0.0,
                    cy: 0.0,
                    rx: 5.0,
                    ry: 5.0,
                },
                even_odd: false,
                paint: Paint {
                    stroke: Some(PaintSource::Color([0.0, 0.0, 0.0, 1.0])),
                    stroke_width: 2.0,
                    join: 1,
                    ..Paint::default()
                },
            })],
            ..Animation::default()
        };
        let b = subtree_bounds(&anim, 0).unwrap();
        let p = core::f32::consts::SQRT_2; // 1.0 half-width × √2 cap reach
        assert!((b[0] - (10.0 - 5.0 - p)).abs() < 1e-4);
        assert!((b[2] - (10.0 + 5.0 + p)).abs() < 1e-4);
    }
}

extern crate alloc;
