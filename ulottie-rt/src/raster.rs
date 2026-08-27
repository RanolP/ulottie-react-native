//! tiny-skia rasterization of a decoded RTDL scene.
//!
//! The walk mirrors the web runtime's draw order exactly: a group that needs
//! an offscreen layer (blend, mask, matte filter, effects, opacity < 1)
//! renders its children unclipped into a scratch pixmap sized by the
//! compiler-computed bbox, applies effects → mask → opacity → matte filter in
//! that order, then blits under the inherited clip — clipping after
//! filtering, which is SVG's order. Groups without a layer just pass the
//! (intersected) clip mask down.
//!
//! tiny-skia covers paths, gradients, patterns and masks; the renderer gaps —
//! 4×5 color matrices, gaussian blur (as the W3C three-box approximation),
//! drop shadows — run as the shared pixel loops in [`crate::pixels`], the
//! same code the ThorVG backend uses for the same gaps.

use crate::bounds;
use crate::pixels;
use crate::rtdl::{
    Animation, Blend, Clip, Geom, Group, ImageRef, Mask as RtMask, Node, Paint as RtPaint,
    PaintSource, PathData, Shape, VERB_CLOSE, VERB_CUBIC, VERB_LINE, VERB_MOVE,
};
use alloc::vec::Vec;
use tiny_skia::{
    BlendMode, Color, FillRule, FilterQuality, GradientStop, IntSize, LineCap, LineJoin,
    LinearGradient, Mask, MaskType, Paint, Path, PathBuilder, Pattern, Pixmap, PixmapMut,
    PixmapPaint, Point, RadialGradient, Rect, Shader, SpreadMode, Stroke, StrokeDash, Transform,
};

extern crate alloc;

/// Images premultiplied into tiny-skia pixmaps once, at load time.
/// Parallel to `Animation::images` — a `None` (pixmap allocation failed)
/// must not shift later indices, which [`draw_image`] looks up by wire
/// position.
pub struct ImagePool {
    pixmaps: Vec<Option<Pixmap>>,
}

impl ImagePool {
    pub fn new(anim: &Animation) -> Self {
        let pixmaps = anim
            .images
            .iter()
            .map(|img| {
                let mut pm = Pixmap::new(img.width.max(1), img.height.max(1))?;
                let data = pm.data_mut();
                let n = data.len().min(img.rgba.len());
                data[..n].copy_from_slice(&img.rgba[..n]);
                // RTDL images are straight alpha; tiny-skia wants premultiplied.
                for px in data.chunks_exact_mut(4) {
                    let a = px[3] as u32;
                    px[0] = ((px[0] as u32 * a + 127) / 255) as u8;
                    px[1] = ((px[1] as u32 * a + 127) / 255) as u8;
                    px[2] = ((px[2] as u32 * a + 127) / 255) as u8;
                }
                Some(pm)
            })
            .collect();
        ImagePool { pixmaps }
    }
}

/// Rasterize the scene's current state into `canvas` under `fit` (the
/// design-space → device transform). The caller clears the canvas.
pub fn render(anim: &Animation, images: &ImagePool, canvas: &mut PixmapMut, fit: Transform) {
    if anim.nodes.is_empty() {
        return;
    }
    draw_node(anim, images, 0, canvas, fit, None);
}

fn matrix(m: &Option<[f32; 6]>) -> Transform {
    match m {
        Some([a, b, c, d, e, f]) => Transform::from_row(*a, *b, *c, *d, *e, *f),
        None => Transform::identity(),
    }
}

fn draw_node(
    anim: &Animation,
    images: &ImagePool,
    idx: u32,
    canvas: &mut PixmapMut,
    ctm: Transform,
    clip: Option<&Mask>,
) {
    match &anim.nodes[idx as usize] {
        Node::Group(g) => draw_group(anim, images, g, canvas, ctm, clip),
        Node::Shape(s) => draw_shape(s, anim, canvas, ctm, clip),
        Node::Image(i) => draw_image(i, images, canvas, ctm, clip),
    }
}

fn draw_group(
    anim: &Animation,
    images: &ImagePool,
    g: &Group,
    canvas: &mut PixmapMut,
    ctm: Transform,
    clip: Option<&Mask>,
) {
    if g.hidden || g.opacity <= 0.0 {
        return;
    }
    let ctm = ctm.pre_concat(matrix(&g.matrix));

    // Inherited ∩ own clip, as a canvas-sized device mask.
    let merged: Option<Mask> = match &g.clip {
        Some(c) => match clip_mask(c, canvas.width(), canvas.height(), ctm, clip) {
            Some(m) => Some(m),
            None => return, // clip resolves to nothing — nothing can draw
        },
        None => None,
    };
    let clip = merged.as_ref().or(clip);

    let needs_layer = g.opacity < 1.0
        || g.blend.is_some()
        || g.mask.is_some()
        || g.cf.is_some()
        || !g.fx.is_empty();
    if !needs_layer {
        for &c in &g.children {
            draw_node(anim, images, c, canvas, ctm, clip);
        }
        return;
    }

    // Offscreen layer, sized by the compiler's bbox mapped to device space.
    let Some(bbox) = g.bbox else { return };
    let dev = bounds::map_aabb(&svg_of(ctm), bbox);
    let x0 = dev[0].floor().max(0.0) as i32;
    let y0 = dev[1].floor().max(0.0) as i32;
    let x1 = (dev[2].ceil() as i32).min(canvas.width() as i32);
    let y1 = (dev[3].ceil() as i32).min(canvas.height() as i32);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let (w, h) = ((x1 - x0) as u32, (y1 - y0) as u32);
    let Some(mut scratch) = Pixmap::new(w, h) else {
        return;
    };
    let inner = Transform::from_translate(-(x0 as f32), -(y0 as f32)).pre_concat(ctm);
    {
        let mut sm = scratch.as_mut();
        for &c in &g.children {
            draw_node(anim, images, c, &mut sm, inner, None);
        }
    }

    // Scale user-space effect radii/offsets into device pixels.
    let (sx, sy) = scale_of(ctm);
    if !g.fx.is_empty() {
        let mut data = scratch.take();
        for stage in &g.fx {
            data = pixels::apply_stage(data, w, h, stage, sx, sy, &svg_of(ctm));
        }
        scratch = Pixmap::from_vec(data, IntSize::from_wh(w, h).unwrap()).unwrap();
    }

    if let Some(m) = &g.mask {
        apply_rt_mask(anim, images, m, &mut scratch, inner);
    }

    let mut blit_opacity = g.opacity;
    if let Some(cf) = &g.cf {
        pixels::matte_invert(scratch.data_mut(), cf, blit_opacity);
        blit_opacity = 1.0;
    }

    canvas.draw_pixmap(
        x0,
        y0,
        scratch.as_ref(),
        &PixmapPaint {
            opacity: blit_opacity,
            blend_mode: blend_mode(g.blend),
            quality: FilterQuality::Nearest,
        },
        Transform::identity(),
        clip,
    );
}

/// SVG-order coefficients back out of a Transform (for aabb mapping).
fn svg_of(t: Transform) -> [f32; 6] {
    [t.sx, t.ky, t.kx, t.sy, t.tx, t.ty]
}

/// Per-axis scale factors of the transform's linear part.
fn scale_of(t: Transform) -> (f32, f32) {
    (
        (t.sx * t.sx + t.ky * t.ky).sqrt(),
        (t.kx * t.kx + t.sy * t.sy).sqrt(),
    )
}

fn blend_mode(b: Option<Blend>) -> BlendMode {
    match b {
        None => BlendMode::SourceOver,
        Some(Blend::Multiply) => BlendMode::Multiply,
        Some(Blend::Screen) => BlendMode::Screen,
        Some(Blend::Overlay) => BlendMode::Overlay,
        Some(Blend::Darken) => BlendMode::Darken,
        Some(Blend::Lighten) => BlendMode::Lighten,
        Some(Blend::ColorDodge) => BlendMode::ColorDodge,
        Some(Blend::ColorBurn) => BlendMode::ColorBurn,
        Some(Blend::HardLight) => BlendMode::HardLight,
        Some(Blend::SoftLight) => BlendMode::SoftLight,
        Some(Blend::Difference) => BlendMode::Difference,
        Some(Blend::Exclusion) => BlendMode::Exclusion,
        Some(Blend::Hue) => BlendMode::Hue,
        Some(Blend::Saturation) => BlendMode::Saturation,
        Some(Blend::Color) => BlendMode::Color,
        Some(Blend::Luminosity) => BlendMode::Luminosity,
    }
}

// ---------------------------------------------------------------- clipping

fn clip_mask(
    clip: &Clip,
    w: u32,
    h: u32,
    ctm: Transform,
    inherited: Option<&Mask>,
) -> Option<Mask> {
    let (path, rule) = match clip {
        Clip::Rect([x, y, cw, ch]) => {
            let r = Rect::from_xywh(*x, *y, *cw, *ch)?;
            (PathBuilder::from_rect(r), FillRule::Winding)
        }
        Clip::Path {
            path, even_odd, ..
        } => {
            let p = build_path(path)?; // empty animated clip = clip everything
            (
                p,
                if *even_odd {
                    FillRule::EvenOdd
                } else {
                    FillRule::Winding
                },
            )
        }
    };
    match inherited {
        Some(m) => {
            let mut m = m.clone();
            m.intersect_path(&path, rule, true, ctm);
            Some(m)
        }
        None => {
            let mut m = Mask::new(w, h)?;
            m.fill_path(&path, rule, true, ctm);
            Some(m)
        }
    }
}

// ------------------------------------------------------------------ shapes

fn build_path(p: &PathData) -> Option<Path> {
    let mut b = PathBuilder::new();
    let mut i = 0usize;
    let pts = &p.points;
    for &v in &p.verbs {
        match v {
            VERB_MOVE => {
                b.move_to(pts[i], pts[i + 1]);
                i += 2;
            }
            VERB_LINE => {
                b.line_to(pts[i], pts[i + 1]);
                i += 2;
            }
            VERB_CUBIC => {
                b.cubic_to(pts[i], pts[i + 1], pts[i + 2], pts[i + 3], pts[i + 4], pts[i + 5]);
                i += 6;
            }
            VERB_CLOSE => b.close(),
            _ => return None,
        }
    }
    b.finish()
}

/// Cubic circle-arc constant.
const KAPPA: f32 = 0.552_284_8;

fn geom_path(g: &Geom) -> Option<Path> {
    match g {
        Geom::Path(p) => build_path(p),
        Geom::Rect { x, y, w, h, rx, ry } => {
            if *w <= 0.0 || *h <= 0.0 {
                return None;
            }
            let rx = rx.min(w / 2.0).max(0.0);
            let ry = ry.min(h / 2.0).max(0.0);
            if rx <= 0.0 || ry <= 0.0 {
                return Some(PathBuilder::from_rect(Rect::from_xywh(*x, *y, *w, *h)?));
            }
            let (kx, ky) = (rx * KAPPA, ry * KAPPA);
            let (x1, y1) = (x + w, y + h);
            let mut b = PathBuilder::new();
            b.move_to(x + rx, *y);
            b.line_to(x1 - rx, *y);
            b.cubic_to(x1 - rx + kx, *y, x1, y + ry - ky, x1, y + ry);
            b.line_to(x1, y1 - ry);
            b.cubic_to(x1, y1 - ry + ky, x1 - rx + kx, y1, x1 - rx, y1);
            b.line_to(x + rx, y1);
            b.cubic_to(x + rx - kx, y1, *x, y1 - ry + ky, *x, y1 - ry);
            b.line_to(*x, y + ry);
            b.cubic_to(*x, y + ry - ky, x + rx - kx, *y, x + rx, *y);
            b.close();
            b.finish()
        }
        Geom::Ellipse { cx, cy, rx, ry } => {
            if *rx <= 0.0 || *ry <= 0.0 {
                return None;
            }
            PathBuilder::from_oval(Rect::from_xywh(cx - rx, cy - ry, rx * 2.0, ry * 2.0)?)
        }
    }
}

fn shader(source: &PaintSource, alpha: f32, anim: &Animation) -> Option<Shader<'static>> {
    match source {
        PaintSource::Color([r, g, b, a]) => Some(Shader::SolidColor(Color::from_rgba(
            r.clamp(0.0, 1.0),
            g.clamp(0.0, 1.0),
            b.clamp(0.0, 1.0),
            (a * alpha).clamp(0.0, 1.0),
        )?)),
        PaintSource::Gradient(gi) => {
            let g = anim.gradients.get(*gi as usize)?;
            let stops: Vec<GradientStop> = g
                .stops
                .iter()
                .map(|s| {
                    GradientStop::new(
                        s.offset.clamp(0.0, 1.0),
                        Color::from_rgba(
                            s.color[0].clamp(0.0, 1.0),
                            s.color[1].clamp(0.0, 1.0),
                            s.color[2].clamp(0.0, 1.0),
                            (s.color[3] * alpha).clamp(0.0, 1.0),
                        )
                        .unwrap_or_else(|| Color::from_rgba8(0, 0, 0, 255)),
                    )
                })
                .collect();
            let t = matrix(&g.transform);
            if g.radial {
                let c = Point::from_xy(g.coords[0], g.coords[1]);
                RadialGradient::new(c, 0.0, c, g.coords[2].max(1e-6), stops, SpreadMode::Pad, t)
            } else {
                LinearGradient::new(
                    Point::from_xy(g.coords[0], g.coords[1]),
                    Point::from_xy(g.coords[2], g.coords[3]),
                    stops,
                    SpreadMode::Pad,
                    t,
                )
            }
        }
    }
}

fn stroke_of(p: &RtPaint) -> Stroke {
    Stroke {
        width: p.stroke_width,
        miter_limit: p.miter_limit,
        line_cap: match p.cap {
            1 => LineCap::Round,
            2 => LineCap::Square,
            _ => LineCap::Butt,
        },
        line_join: match p.join {
            1 => LineJoin::Round,
            2 => LineJoin::Bevel,
            _ => LineJoin::Miter,
        },
        dash: p.dash.as_ref().and_then(|d| {
            // SVG repeats an odd-length list doubled; tiny-skia wants even.
            let mut arr = d.array.clone();
            if arr.len() % 2 == 1 {
                arr.extend_from_slice(&d.array);
            }
            StrokeDash::new(arr, d.offset)
        }),
    }
}

fn draw_shape(
    s: &Shape,
    anim: &Animation,
    canvas: &mut PixmapMut,
    ctm: Transform,
    clip: Option<&Mask>,
) {
    if s.hidden || s.opacity <= 0.0 {
        return;
    }
    let ctm = ctm.pre_concat(matrix(&s.matrix));
    let Some(path) = geom_path(&s.geom) else {
        return;
    };
    let rule = if s.even_odd {
        FillRule::EvenOdd
    } else {
        FillRule::Winding
    };
    let both = s.paint.fill.is_some() && s.paint.stroke.is_some();
    if s.opacity < 1.0 && both {
        // Fill and stroke overlap: group-opacity semantics need one layer.
        draw_shape_layered(s, &path, rule, anim, canvas, ctm, clip);
        return;
    }
    // Single paint (or full opacity): fold shape opacity into paint alpha.
    let o = s.opacity;
    let mut ops: [Option<(&PaintSource, f32, bool)>; 2] = [
        s.paint
            .fill
            .as_ref()
            .map(|f| (f, s.paint.fill_opacity * o, false)),
        s.paint
            .stroke
            .as_ref()
            .map(|st| (st, s.paint.stroke_opacity * o, true)),
    ];
    if s.paint.stroke_first {
        ops.swap(0, 1);
    }
    for op in ops.into_iter().flatten() {
        let (source, alpha, is_stroke) = op;
        let Some(sh) = shader(source, alpha, anim) else {
            continue;
        };
        let paint = Paint {
            shader: sh,
            anti_alias: true,
            ..Paint::default()
        };
        if is_stroke {
            canvas.stroke_path(&path, &paint, &stroke_of(&s.paint), ctm, clip);
        } else {
            canvas.fill_path(&path, &paint, rule, ctm, clip);
        }
    }
}

fn draw_shape_layered(
    s: &Shape,
    path: &Path,
    rule: FillRule,
    anim: &Animation,
    canvas: &mut PixmapMut,
    ctm: Transform,
    clip: Option<&Mask>,
) {
    let pad = bounds::stroke_pad(s);
    let Some(b) = bounds::geom_bounds(&s.geom) else {
        return;
    };
    let dev = bounds::map_aabb(&svg_of(ctm), bounds::pad(b, pad, pad));
    let x0 = (dev[0].floor() - 1.0).max(0.0) as i32;
    let y0 = (dev[1].floor() - 1.0).max(0.0) as i32;
    let x1 = ((dev[2].ceil() + 1.0) as i32).min(canvas.width() as i32);
    let y1 = ((dev[3].ceil() + 1.0) as i32).min(canvas.height() as i32);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let Some(mut scratch) = Pixmap::new((x1 - x0) as u32, (y1 - y0) as u32) else {
        return;
    };
    let inner = Transform::from_translate(-(x0 as f32), -(y0 as f32)).pre_concat(ctm);
    {
        let mut sm = scratch.as_mut();
        let mut draw = |source: &PaintSource, alpha: f32, is_stroke: bool| {
            let Some(sh) = shader(source, alpha, anim) else {
                return;
            };
            let paint = Paint {
                shader: sh,
                anti_alias: true,
                ..Paint::default()
            };
            if is_stroke {
                sm.stroke_path(path, &paint, &stroke_of(&s.paint), inner, None);
            } else {
                sm.fill_path(path, &paint, rule, inner, None);
            }
        };
        if s.paint.stroke_first {
            draw(s.paint.stroke.as_ref().unwrap(), s.paint.stroke_opacity, true);
            draw(s.paint.fill.as_ref().unwrap(), s.paint.fill_opacity, false);
        } else {
            draw(s.paint.fill.as_ref().unwrap(), s.paint.fill_opacity, false);
            draw(s.paint.stroke.as_ref().unwrap(), s.paint.stroke_opacity, true);
        }
    }
    canvas.draw_pixmap(
        x0,
        y0,
        scratch.as_ref(),
        &PixmapPaint {
            opacity: s.opacity,
            blend_mode: BlendMode::SourceOver,
            quality: FilterQuality::Nearest,
        },
        Transform::identity(),
        clip,
    );
}

// ------------------------------------------------------------------ images

fn draw_image(
    i: &ImageRef,
    images: &ImagePool,
    canvas: &mut PixmapMut,
    ctm: Transform,
    clip: Option<&Mask>,
) {
    let Some(pm) = images.pixmaps.get(i.image as usize).and_then(Option::as_ref) else {
        return;
    };
    let (iw, ih) = (pm.width() as f32, pm.height() as f32);
    if iw <= 0.0 || ih <= 0.0 || i.w <= 0.0 || i.h <= 0.0 {
        return;
    }
    // lottie-web's `xMidYMid slice`: cover the layer box, center the crop.
    let s = (i.w / iw).max(i.h / ih);
    let tx = (i.w - iw * s) / 2.0;
    let ty = (i.h - ih * s) / 2.0;
    let pattern = Pattern::new(
        pm.as_ref(),
        SpreadMode::Pad,
        FilterQuality::Bilinear,
        1.0,
        Transform::from_row(s, 0.0, 0.0, s, tx, ty),
    );
    let Some(rect) = Rect::from_xywh(0.0, 0.0, i.w, i.h) else {
        return;
    };
    let paint = Paint {
        shader: pattern,
        anti_alias: true,
        ..Paint::default()
    };
    canvas.fill_rect(rect, &paint, ctm, clip);
}

// ------------------------------------------------------------------ mattes

fn apply_rt_mask(
    anim: &Animation,
    images: &ImagePool,
    m: &RtMask,
    scratch: &mut Pixmap,
    inner: Transform,
) {
    let Some(mut matte) = Pixmap::new(scratch.width(), scratch.height()) else {
        return;
    };
    {
        let mut mm = matte.as_mut();
        for &c in &m.children {
            draw_node(anim, images, c, &mut mm, inner, None);
        }
    }
    let ty = if m.luma {
        MaskType::Luminance
    } else {
        MaskType::Alpha
    };
    let mask = Mask::from_pixmap(matte.as_ref(), ty);
    scratch.as_mut().apply_mask(&mask);
}

// The pixel-loop filters (color matrices, matte inversion, box blur, shadow,
// stage stacking) live in `crate::pixels`, shared with the ThorVG backend.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rtdl::{Group, Paint as P, PaintSource};

    /// The one runnable check: a red circle in a half-opacity group lands on
    /// the canvas at half alpha, centered, and outside stays clear.
    #[test]
    fn draws_layered_circle() {
        let anim = Animation {
            width: 32.0,
            height: 32.0,
            fr: 30.0,
            ip: 0.0,
            op: 1.0,
            nodes: alloc::vec![
                Node::Group(Group {
                    opacity: 0.5,
                    bbox: Some([0.0, 0.0, 32.0, 32.0]),
                    children: alloc::vec![1],
                    ..Group::default()
                }),
                Node::Shape(Shape {
                    slot: None,
                    matrix: None,
                    opacity: 1.0,
                    hidden: false,
                    geom: Geom::Ellipse {
                        cx: 16.0,
                        cy: 16.0,
                        rx: 10.0,
                        ry: 10.0,
                    },
                    even_odd: false,
                    paint: P {
                        fill: Some(PaintSource::Color([1.0, 0.0, 0.0, 1.0])),
                        ..P::default()
                    },
                }),
            ],
            ..Animation::default()
        };
        let images = ImagePool::new(&anim);
        let mut pm = Pixmap::new(32, 32).unwrap();
        render(&anim, &images, &mut pm.as_mut(), Transform::identity());
        let center = pm.pixel(16, 16).unwrap();
        assert!(center.alpha() > 120 && center.alpha() < 135, "half alpha");
        assert!(center.red() > 120, "premultiplied red at half alpha");
        assert_eq!(pm.pixel(1, 1).unwrap().alpha(), 0, "outside stays clear");
    }
}
