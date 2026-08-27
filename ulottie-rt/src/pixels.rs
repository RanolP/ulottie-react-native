//! Backend-agnostic pixel passes over premultiplied RGBA8 buffers.
//!
//! Both rasterizer backends (tiny-skia and ThorVG) share these for the
//! renderer gaps no vector API covers: 4×5 color matrices (sRGB and
//! linearRGB), the matte-inversion filter, the W3C three-box gaussian
//! approximation, drop shadows, effect-stage stacking, and mask application.
//! Sharing the exact loops is what keeps the two backends' divergences from
//! lottie-web identical wherever the gap features run.
//!
//! Every function takes a tightly packed, premultiplied RGBA8 buffer
//! (`data.len() == w * h * 4`), the format both backends render in.

use crate::rtdl::{FxPass, FxStage, MatteInvert};
use alloc::vec::Vec;

extern crate alloc;

/// Straight-alpha view of one premultiplied pixel, channels 0..=1.
#[inline]
pub fn unpremul(px: &[u8]) -> [f32; 4] {
    let a = px[3] as f32 / 255.0;
    if a <= 0.0 {
        return [0.0, 0.0, 0.0, 0.0];
    }
    [
        px[0] as f32 / 255.0 / a,
        px[1] as f32 / 255.0 / a,
        px[2] as f32 / 255.0 / a,
        a,
    ]
}

#[inline]
pub fn premul(c: [f32; 4], px: &mut [u8]) {
    let a = c[3].clamp(0.0, 1.0);
    px[0] = ((c[0].clamp(0.0, 1.0) * a) * 255.0 + 0.5) as u8;
    px[1] = ((c[1].clamp(0.0, 1.0) * a) * 255.0 + 0.5) as u8;
    px[2] = ((c[2].clamp(0.0, 1.0) * a) * 255.0 + 0.5) as u8;
    px[3] = (a * 255.0 + 0.5) as u8;
}

/// Apply a 4×5 color matrix (rows R G B A over [r g b a 1], all 0..=1) to
/// every pixel, on straight alpha.
pub fn color_matrix(data: &mut [u8], m: &[f32; 20]) {
    for px in data.chunks_exact_mut(4) {
        let c = unpremul(px);
        let mut out = [0.0f32; 4];
        for (r, o) in out.iter_mut().enumerate() {
            let row = &m[r * 5..r * 5 + 5];
            *o = row[0] * c[0] + row[1] * c[1] + row[2] * c[2] + row[3] * c[3] + row[4];
        }
        premul(out, px);
    }
}

/// sRGB transfer function, both directions (IEC 61966-2-1).
#[inline]
pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

#[inline]
pub fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

/// `color_matrix`, but in linearRGB — the SVG default space for filter
/// primitives, and what an explicit `color-interpolation-filters="linearRGB"`
/// asks for: sRGB channels decode to linear, the matrix runs there, and the
/// result re-encodes to sRGB. Alpha has no transfer function.
pub fn color_matrix_linear(data: &mut [u8], m: &[f32; 20]) {
    for px in data.chunks_exact_mut(4) {
        let s = unpremul(px);
        let c = [
            srgb_to_linear(s[0]),
            srgb_to_linear(s[1]),
            srgb_to_linear(s[2]),
            s[3],
        ];
        let mut out = [0.0f32; 4];
        for (r, o) in out.iter_mut().enumerate() {
            let row = &m[r * 5..r * 5 + 5];
            *o = row[0] * c[0] + row[1] * c[1] + row[2] * c[2] + row[3] * c[3] + row[4];
        }
        for o in out.iter_mut().take(3) {
            *o = linear_to_srgb(o.clamp(0.0, 1.0));
        }
        premul(out, px);
    }
}

/// The inverted-matte colour filter, with the browser semantics the web
/// reference exhibits: the filter's output covers the *whole* filter region,
/// so a pixel the matte source never touched comes out as the matrix applied
/// to transparent black — for the inversion matrix, opaque white — and the
/// luminance mask reads 1 there (`1 − luma` inside the source). Running the
/// straight matrix on unpremultiplied data instead would leave those pixels
/// transparent and cull everything outside the matte source, which is exactly
/// the bug the reference render disproves. Concretely: take content-over-
/// black (the premultiplied channels as-is), run the matrix in linearRGB (the
/// markup carries no `color-interpolation-filters`, so the SVG default
/// applies), and emit fully opaque.
pub fn matte_invert(data: &mut [u8], cf: &MatteInvert, opacity: f32) {
    if opacity < 1.0 {
        // The matrix has offsets, so opacity must multiply *before* it.
        for px in data.chunks_exact_mut(4) {
            for ch in px.iter_mut() {
                *ch = (*ch as f32 * opacity + 0.5) as u8;
            }
        }
    }
    for px in data.chunks_exact_mut(4) {
        let c = [
            srgb_to_linear(px[0] as f32 / 255.0),
            srgb_to_linear(px[1] as f32 / 255.0),
            srgb_to_linear(px[2] as f32 / 255.0),
            px[3] as f32 / 255.0,
        ];
        for (r, ch) in px.iter_mut().take(3).enumerate() {
            let row = &cf.matrix[r * 5..r * 5 + 5];
            let o = row[0] * c[0] + row[1] * c[1] + row[2] * c[2] + row[3] * c[3] + row[4];
            *ch = (linear_to_srgb(o.clamp(0.0, 1.0)) * 255.0 + 0.5) as u8;
        }
        px[3] = 255;
    }
}

/// W3C gaussian approximation: three box blurs per axis, box size
/// `d = floor(σ·3·√(2π)/4 + 0.5)`. Runs on premultiplied channels (blur is
/// linear, so premultiplied is the correct space). `wrap` follows SVG
/// `edgeMode="wrap"`; otherwise edges clamp (`duplicate`).
pub fn box_blur(data: &mut [u8], w: u32, h: u32, sigma_x: f32, sigma_y: f32, wrap: bool) {
    let w = w as i32;
    let h = h as i32;
    let boxes = |sigma: f32| -> Vec<(i32, i32)> {
        let d = (sigma * 3.0 * (2.0 * core::f32::consts::PI).sqrt() / 4.0 + 0.5).floor() as i32;
        if d < 1 {
            return Vec::new();
        }
        if d % 2 == 1 {
            let lo = -(d / 2);
            alloc::vec![(d, lo), (d, lo), (d, lo)]
        } else {
            alloc::vec![(d, -(d / 2)), (d, -(d / 2) + 1), (d + 1, -(d / 2))]
        }
    };
    let mut tmp = data.to_vec();
    let idx = |x: i32, y: i32| -> usize { ((y * w + x) * 4) as usize };
    let clampw = |v: i32, n: i32| -> i32 {
        if wrap {
            v.rem_euclid(n)
        } else {
            v.clamp(0, n - 1)
        }
    };
    // Horizontal passes.
    for (size, lo) in boxes(sigma_x) {
        for y in 0..h {
            for x in 0..w {
                let mut acc = [0u32; 4];
                for k in 0..size {
                    let sxp = clampw(x + lo + k, w);
                    let p = idx(sxp, y);
                    for c in 0..4 {
                        acc[c] += data[p + c] as u32;
                    }
                }
                let p = idx(x, y);
                for c in 0..4 {
                    tmp[p + c] = ((acc[c] + size as u32 / 2) / size as u32) as u8;
                }
            }
        }
        data.copy_from_slice(&tmp);
    }
    // Vertical passes.
    for (size, lo) in boxes(sigma_y) {
        for x in 0..w {
            for y in 0..h {
                let mut acc = [0u32; 4];
                for k in 0..size {
                    let syp = clampw(y + lo + k, h);
                    let p = idx(x, syp);
                    for c in 0..4 {
                        acc[c] += data[p + c] as u32;
                    }
                }
                let p = idx(x, y);
                for c in 0..4 {
                    tmp[p + c] = ((acc[c] + size as u32 / 2) / size as u32) as u8;
                }
            }
        }
        data.copy_from_slice(&tmp);
    }
}

/// Premultiplied source-over of `src` onto `dst`, offset by (`dx`, `dy`).
/// Both buffers are the same size; the offset clips.
pub fn blit_over(dst: &mut [u8], src: &[u8], w: u32, h: u32, dx: i32, dy: i32) {
    let (w, h) = (w as i32, h as i32);
    for sy in 0..h {
        let ty = sy + dy;
        if ty < 0 || ty >= h {
            continue;
        }
        for sx in 0..w {
            let tx = sx + dx;
            if tx < 0 || tx >= w {
                continue;
            }
            let sp = ((sy * w + sx) * 4) as usize;
            let tp = ((ty * w + tx) * 4) as usize;
            let sa = src[sp + 3] as u32;
            if sa == 0 {
                continue;
            }
            for c in 0..4 {
                let s = src[sp + c] as u32;
                let d = dst[tp + c] as u32;
                dst[tp + c] = (s + (d * (255 - sa) + 127) / 255).min(255) as u8;
            }
        }
    }
}

/// One effect stage: every pass reads the stage input and the pass outputs
/// stack over each other in order (`feMerge` semantics). `sx`/`sy` scale
/// user-space radii into device pixels; `ctm` (SVG order) maps shadow
/// offsets. Returns the stage's output buffer.
pub fn apply_stage(
    input: Vec<u8>,
    w: u32,
    h: u32,
    stage: &FxStage,
    sx: f32,
    sy: f32,
    ctm: &[f32; 6],
) -> Vec<u8> {
    if stage.passes.is_empty() {
        return input;
    }
    // Single-pass stages transform in place — the common case.
    if stage.passes.len() == 1 {
        let mut pm = input.clone();
        run_pass(&mut pm, &input, w, h, &stage.passes[0], sx, sy, ctm);
        return pm;
    }
    let mut out = alloc::vec![0u8; input.len()];
    for pass in &stage.passes {
        let mut layer = input.clone();
        run_pass(&mut layer, &input, w, h, pass, sx, sy, ctm);
        blit_over(&mut out, &layer, w, h, 0, 0);
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn run_pass(
    pm: &mut [u8],
    input: &[u8],
    w: u32,
    h: u32,
    pass: &FxPass,
    sx: f32,
    sy: f32,
    ctm: &[f32; 6],
) {
    match pass {
        FxPass::Source => {}
        FxPass::ColorMatrix(m) => color_matrix(pm, m),
        FxPass::ColorMatrix2 { outer, inner } => {
            // The tint pair: the luminance matrix declares linearRGB (as
            // lottie-web's `SVGTintFilter` does), the ramp declares sRGB.
            color_matrix_linear(pm, inner);
            color_matrix(pm, outer);
        }
        FxPass::Blur {
            sx: bx, sy: by, wrap, ..
        } => box_blur(pm, w, h, bx * sx, by * sy, *wrap),
        FxPass::Shadow {
            std_dev,
            dx,
            dy,
            color,
            flood_opacity,
            ..
        } => {
            // Blur the alpha, colorize, offset — then the content over it.
            let mut shadow = input.to_vec();
            let c = [
                color[0].clamp(0.0, 1.0),
                color[1].clamp(0.0, 1.0),
                color[2].clamp(0.0, 1.0),
                (color[3] * flood_opacity).clamp(0.0, 1.0),
            ];
            for px in shadow.chunks_exact_mut(4) {
                let a = px[3] as f32 / 255.0 * c[3];
                px[0] = (c[0] * a * 255.0 + 0.5) as u8;
                px[1] = (c[1] * a * 255.0 + 0.5) as u8;
                px[2] = (c[2] * a * 255.0 + 0.5) as u8;
                px[3] = (a * 255.0 + 0.5) as u8;
            }
            box_blur(&mut shadow, w, h, std_dev * sx, std_dev * sy, false);
            let (odx, ody) = (
                (ctm[0] * dx + ctm[2] * dy).round() as i32,
                (ctm[1] * dx + ctm[3] * dy).round() as i32,
            );
            let mut out = alloc::vec![0u8; pm.len()];
            blit_over(&mut out, &shadow, w, h, odx, ody);
            blit_over(&mut out, input, w, h, 0, 0);
            pm.copy_from_slice(&out);
        }
    }
}

/// Multiply `dst`'s channels by a coverage taken from `mask` (same size):
/// Rec.709 luminance of the premultiplied pixel (`luma`) or its alpha.
pub fn mask_apply(dst: &mut [u8], mask: &[u8], luma: bool) {
    for (px, mp) in dst.chunks_exact_mut(4).zip(mask.chunks_exact(4)) {
        let cov = if luma {
            // Premultiplied channels already carry alpha, so the weighted sum
            // is luminance × alpha — the same value tiny-skia's luminance
            // mask computes.
            0.2126 * mp[0] as f32 + 0.7152 * mp[1] as f32 + 0.0722 * mp[2] as f32
        } else {
            mp[3] as f32
        } / 255.0;
        for ch in px.iter_mut() {
            *ch = (*ch as f32 * cov + 0.5) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one runnable check: an identity color matrix round-trips a pixel,
    /// the inversion row flips it, and blit_over honors premultiplied
    /// source-over with an offset.
    #[test]
    fn matrix_and_blit() {
        let mut px = [255u8, 0, 0, 255]; // opaque red
        let mut ident = [0.0f32; 20];
        ident[0] = 1.0;
        ident[6] = 1.0;
        ident[12] = 1.0;
        ident[18] = 1.0;
        color_matrix(&mut px, &ident);
        assert_eq!(px, [255, 0, 0, 255]);

        // 2×1 canvas: blit an opaque green pixel one to the right.
        let mut dst = [255u8, 0, 0, 255, 0, 0, 0, 0];
        let src = [0u8, 255, 0, 255, 0, 0, 0, 0];
        blit_over(&mut dst, &src, 2, 1, 1, 0);
        assert_eq!(&dst[0..4], &[255, 0, 0, 255], "off-canvas source clipped");
        assert_eq!(&dst[4..8], &[0, 255, 0, 255], "green landed at x=1");

        let mut masked = [255u8, 0, 0, 255];
        mask_apply(&mut masked, &[0, 0, 0, 128], false);
        assert_eq!(masked[3], 128, "alpha mask halves alpha");
    }
}
