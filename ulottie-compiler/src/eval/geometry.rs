//! Shape geometry generators — exact transcriptions of lottie-web's
//! `RectShapeProperty`, `EllShapeProperty` and `StarShapeProperty`, including
//! its `roundCorner = 0.5519` (not the true circle kappa 0.55228…): the goal
//! is to match the reference renderer digit for digit, not the platonic arc.
//!
//! Direction is a construction fact, not a modifier: a reversed shape is the
//! same contour traversed the other way, which flips its winding — invisible
//! alone, decisive against another contour in the same element (holes) and
//! for where a trim or a dash starts. lottie-web writes each reversed form
//! out by hand; every one of them is exactly the cyclic reversal of its
//! normal form (verified corner by corner against 5.12.2), so one `reverse`
//! covers rect and ellipse, and the polystar folds its `dir` into the loop
//! the way the original does.
//!
//! The rect's reversed branch is also its *absent-`d`* branch: lottie-web
//! tests `d === 1 || d === 2` and reverses otherwise, so a rect with no `d`
//! runs counter-clockwise. The lowering encodes that rule, not this file.

use super::frame::BezierPath;

/// lottie-web's `roundCorner`.
const ROUND: f64 = 0.5519;

/// The same contour traversed the other way: vertex 0 stays first, the rest
/// reverse, and each vertex's in/out tangents swap roles.
fn reverse(p: BezierPath) -> BezierPath {
    let n = p.vertices.len();
    if n == 0 {
        return p;
    }
    let src = |k: usize| (n - k) % n;
    BezierPath {
        vertices: (0..n).map(|k| p.vertices[src(k)]).collect(),
        in_tangents: (0..n).map(|k| p.out_tangents[src(k)]).collect(),
        out_tangents: (0..n).map(|k| p.in_tangents[src(k)]).collect(),
        closed: p.closed,
    }
}

pub fn rect_to_path(center: [f64; 2], size: [f64; 2], radius: f64, reversed: bool) -> BezierPath {
    let (cx, cy) = (center[0], center[1]);
    let (hw, hh) = (size[0] / 2.0, size[1] / 2.0);
    let l = cx - hw;
    let t = cy - hh;
    let r = cx + hw;
    let b = cy + hh;
    let path = if radius < 1e-3 {
        BezierPath {
            vertices: vec![[r, t], [r, b], [l, b], [l, t]],
            in_tangents: vec![[0.0, 0.0]; 4],
            out_tangents: vec![[0.0, 0.0]; 4],
            closed: true,
        }
    } else {
        let rr = radius.min(hw).min(hh);
        let k = rr * ROUND;
        BezierPath {
            vertices: vec![
                [r, t + rr],
                [r, b - rr],
                [r - rr, b],
                [l + rr, b],
                [l, b - rr],
                [l, t + rr],
                [l + rr, t],
                [r - rr, t],
            ],
            in_tangents: vec![
                [0.0, -k],
                [0.0, 0.0],
                [k, 0.0],
                [0.0, 0.0],
                [0.0, k],
                [0.0, 0.0],
                [-k, 0.0],
                [0.0, 0.0],
            ],
            out_tangents: vec![
                [0.0, 0.0],
                [0.0, k],
                [0.0, 0.0],
                [-k, 0.0],
                [0.0, 0.0],
                [0.0, -k],
                [0.0, 0.0],
                [k, 0.0],
            ],
            closed: true,
        }
    };
    if reversed { reverse(path) } else { path }
}

pub fn ellipse_to_path(center: [f64; 2], size: [f64; 2], reversed: bool) -> BezierPath {
    let (cx, cy) = (center[0], center[1]);
    let (rx, ry) = (size[0] / 2.0, size[1] / 2.0);
    let kx = rx * ROUND;
    let ky = ry * ROUND;
    let path = BezierPath {
        vertices: vec![
            [cx, cy - ry], // 0: top
            [cx + rx, cy], // 1: right
            [cx, cy + ry], // 2: bottom
            [cx - rx, cy], // 3: left
        ],
        in_tangents: vec![[-kx, 0.0], [0.0, -ky], [kx, 0.0], [0.0, ky]],
        out_tangents: vec![[kx, 0.0], [0.0, ky], [-kx, 0.0], [0.0, -ky]],
        closed: true,
    };
    if reversed { reverse(path) } else { path }
}

/// `sy = 1` → star (alternates outer/inner radii). `sy = 2` → polygon (outer
/// only). `rotation` is in degrees from the positive Y axis; `outer_round` /
/// `inner_round` are the `os`/`is` percentages.
#[allow(clippy::too_many_arguments)]
pub fn polystar_to_path(
    sy: u8,
    center: [f64; 2],
    points: f64,
    outer_radius: f64,
    inner_radius: f64,
    rotation: f64,
    outer_round: f64,
    inner_round: f64,
    reversed: bool,
) -> BezierPath {
    let p = points.floor() as i64;
    if p < 3 {
        return BezierPath {
            vertices: vec![],
            in_tangents: vec![],
            out_tangents: vec![],
            closed: true,
        };
    }
    let is_star = sy == 1;
    let total = if is_star { 2 * p } else { p } as usize;
    let dir = if reversed { -1.0 } else { 1.0 };
    let step = std::f64::consts::TAU / total as f64;
    // Perimeter share per segment; the polygon's quarters where the star
    // halves — lottie-web's `numPts * 2` against `numPts * 4`.
    let (long_seg, short_seg) = if is_star {
        let d = (total * 2) as f64;
        (
            std::f64::consts::TAU * outer_radius / d,
            std::f64::consts::TAU * inner_radius / d,
        )
    } else {
        let d = (total * 4) as f64;
        let s = std::f64::consts::TAU * outer_radius / d;
        (s, s)
    };
    let mut ang = rotation.to_radians() - std::f64::consts::FRAC_PI_2;
    let mut vertices = Vec::with_capacity(total);
    let mut in_t = Vec::with_capacity(total);
    let mut out_t = Vec::with_capacity(total);
    let mut long_flag = true;
    for _ in 0..total {
        let (rad, roundness, seg) = if is_star && !long_flag {
            (inner_radius, inner_round / 100.0, short_seg)
        } else {
            (outer_radius, outer_round / 100.0, long_seg)
        };
        let x = rad * ang.cos();
        let y = rad * ang.sin();
        let len = x.hypot(y);
        let (ox, oy) = if len == 0.0 { (0.0, 0.0) } else { (y / len, -x / len) };
        let s = seg * roundness * dir;
        vertices.push([center[0] + x, center[1] + y]);
        out_t.push([-ox * s, -oy * s]);
        in_t.push([ox * s, oy * s]);
        long_flag = !long_flag;
        ang += step * dir;
    }
    BezierPath {
        vertices,
        in_tangents: in_t,
        out_tangents: out_t,
        closed: true,
    }
}
