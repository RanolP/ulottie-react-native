//! Shape geometry generators — matches `rectToPath`, `ellipseToPath`,
//! `polystarToPath` in `runtime/driver.js`. Output is the same structured
//! `{v, i, o, c}` form so the downstream Display lowering is shared.

use super::frame::BezierPath;

/// Magic kappa for approximating a quarter-circle with a cubic bezier.
const KAPPA: f64 = 0.5522847498307933;

pub fn rect_to_path(center: [f64; 2], size: [f64; 2], radius: f64) -> BezierPath {
    let (cx, cy) = (center[0], center[1]);
    let (hw, hh) = (size[0] / 2.0, size[1] / 2.0);
    let l = cx - hw;
    let t = cy - hh;
    let r = cx + hw;
    let b = cy + hh;
    if radius < 1e-3 {
        return BezierPath {
            vertices: vec![[r, t], [r, b], [l, b], [l, t]],
            in_tangents: vec![[0.0, 0.0]; 4],
            out_tangents: vec![[0.0, 0.0]; 4],
            closed: true,
        };
    }
    let rr = radius.min(hw).min(hh);
    let k = rr * KAPPA;
    BezierPath {
        vertices: vec![
            [r, t + rr],         // 0: top-right corner start
            [r, b - rr],         // 1: bottom-right corner start
            [r - rr, b],         // 2: bottom-right corner end
            [l + rr, b],         // 3: bottom-left corner start
            [l, b - rr],         // 4: bottom-left corner end
            [l, t + rr],         // 5: top-left corner start
            [l + rr, t],         // 6: top-left corner end
            [r - rr, t],         // 7: top-right corner end
        ],
        in_tangents: vec![
            [0.0, 0.0],
            [0.0, 0.0],
            [k, 0.0],
            [0.0, 0.0],
            [0.0, 0.0],
            [0.0, 0.0],
            [-k, 0.0],
            [0.0, 0.0],
        ],
        out_tangents: vec![
            [0.0, 0.0],
            [0.0, k],
            [0.0, 0.0],
            [0.0, 0.0],
            [0.0, -k],
            [0.0, 0.0],
            [0.0, 0.0],
            [k, 0.0],
        ],
        closed: true,
    }
}

pub fn ellipse_to_path(center: [f64; 2], size: [f64; 2]) -> BezierPath {
    let (cx, cy) = (center[0], center[1]);
    let (rx, ry) = (size[0] / 2.0, size[1] / 2.0);
    let kx = rx * KAPPA;
    let ky = ry * KAPPA;
    BezierPath {
        vertices: vec![
            [cx, cy - ry], // 0: top
            [cx + rx, cy], // 1: right
            [cx, cy + ry], // 2: bottom
            [cx - rx, cy], // 3: left
        ],
        in_tangents: vec![
            [-kx, 0.0],
            [0.0, -ky],
            [kx, 0.0],
            [0.0, ky],
        ],
        out_tangents: vec![
            [kx, 0.0],
            [0.0, ky],
            [-kx, 0.0],
            [0.0, -ky],
        ],
        closed: true,
    }
}

/// `sy = 1` → star (alternates outer/inner radii). `sy = 2` → polygon (outer
/// only). `rotation` is in degrees, measured from the positive Y axis (Lottie
/// convention: 0° points up).
pub fn polystar_to_path(
    sy: u8,
    center: [f64; 2],
    points: f64,
    outer_radius: f64,
    inner_radius: f64,
    rotation: f64,
) -> BezierPath {
    let p = points.round() as i32;
    if p < 3 {
        return BezierPath {
            vertices: vec![],
            in_tangents: vec![],
            out_tangents: vec![],
            closed: true,
        };
    }
    let is_star = sy == 1;
    let total = if is_star { 2 * p } else { p };
    let rot_rad = rotation.to_radians();
    let mut vertices = Vec::with_capacity(total as usize);
    for i in 0..total {
        let t = (i as f64) / (total as f64) * std::f64::consts::TAU + rot_rad - std::f64::consts::FRAC_PI_2;
        let r = if is_star && i % 2 == 1 { inner_radius } else { outer_radius };
        vertices.push([center[0] + r * t.cos(), center[1] + r * t.sin()]);
    }
    let n = vertices.len();
    BezierPath {
        vertices,
        in_tangents: vec![[0.0, 0.0]; n],
        out_tangents: vec![[0.0, 0.0]; n],
        closed: true,
    }
}
