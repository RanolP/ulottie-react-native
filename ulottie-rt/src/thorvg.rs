//! ThorVG rasterization of a decoded RTDL scene (feature `thorvg`).
//!
//! Same walk, same draw order as [`crate::raster`]: a group that needs an
//! offscreen layer (blend, mask, matte filter, effects, opacity < 1) renders
//! its children into a scratch buffer sized by the compiler bbox — here via a
//! nested SW canvas — applies effects → mask → opacity → matte filter, then
//! lands in the parent as a raw-picture blit under the inherited clip.
//!
//! Native ThorVG covers paths, rects/ellipses, fills, gradients, strokes with
//! dashes, paint order, clipping (nested scenes, one clipper each) and the
//! scene effects gaussian blur / drop shadow. The pixel-loop gaps — 4×5 color
//! matrices (tint chains), the matte-inversion filter, masks, and any fx
//! stack the scene effects cannot express — run through [`crate::pixels`],
//! the very same code the tiny-skia backend uses, so both backends diverge
//! from lottie-web identically wherever the gap features run.
//!
//! Scene lifetime: everything is rebuilt per frame and destroyed with the
//! canvas. That sidesteps ThorVG's retained-mode sharp edges (fill ownership,
//! in-place gradient mutation not dirty-marking) at a cost that is fine for
//! the small scenes RTDL carries. Leaf paints carry the full device CTM;
//! scenes stay at identity, which also makes the scene-effect sigma scale 1
//! (ThorVG scales sigma by the paint transform's scale).

use crate::bounds;
use crate::pixels;
use crate::rtdl::{
    Animation, Blend, Clip, FxPass, FxStage, Geom, Group, ImageRef, Node, Paint as RtPaint,
    PaintSource, PathData, Shape, VERB_CLOSE, VERB_CUBIC, VERB_LINE, VERB_MOVE,
};
use std::sync::{Mutex, Once};

mod capi {
    //! Hand-written bindings for the ~40 `thorvg_capi.h` (v1.1.1) functions
    //! this renderer calls. Signatures verified against the pinned tarball.
    #![allow(non_camel_case_types, dead_code)]
    use core::ffi::{c_int, c_uint};

    #[repr(C)]
    pub struct _Tvg_Canvas(());
    #[repr(C)]
    pub struct _Tvg_Paint(());
    #[repr(C)]
    pub struct _Tvg_Gradient(());
    pub type Tvg_Canvas = *mut _Tvg_Canvas;
    pub type Tvg_Paint = *mut _Tvg_Paint;
    pub type Tvg_Gradient = *mut _Tvg_Gradient;
    pub type Tvg_Result = c_int;

    /// Colors alpha-premultiplied, word `0xAABBGGRR` — RGBA byte order on
    /// little-endian, i.e. exactly our buffers.
    pub const TVG_COLORSPACE_ABGR8888: c_int = 0;
    pub const TVG_ENGINE_OPTION_DEFAULT: c_int = 1;
    pub const TVG_FILL_RULE_EVEN_ODD: c_int = 1;
    pub const TVG_STROKE_FILL_PAD: c_int = 0;

    /// Row-major 3×3; a point maps as `x' = e11·x + e12·y + e13`.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct Tvg_Matrix {
        pub e11: f32,
        pub e12: f32,
        pub e13: f32,
        pub e21: f32,
        pub e22: f32,
        pub e23: f32,
        pub e31: f32,
        pub e32: f32,
        pub e33: f32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct Tvg_Color_Stop {
        pub offset: f32,
        pub r: u8,
        pub g: u8,
        pub b: u8,
        pub a: u8,
    }

    unsafe extern "C" {
        pub fn tvg_engine_init(threads: c_uint) -> Tvg_Result;
        pub fn tvg_swcanvas_create(op: c_int) -> Tvg_Canvas;
        pub fn tvg_swcanvas_set_target(
            canvas: Tvg_Canvas,
            buffer: *mut u32,
            stride: u32,
            w: u32,
            h: u32,
            cs: c_int,
        ) -> Tvg_Result;
        pub fn tvg_canvas_add(canvas: Tvg_Canvas, paint: Tvg_Paint) -> Tvg_Result;
        pub fn tvg_canvas_draw(canvas: Tvg_Canvas, clear: bool) -> Tvg_Result;
        pub fn tvg_canvas_sync(canvas: Tvg_Canvas) -> Tvg_Result;
        pub fn tvg_canvas_destroy(canvas: Tvg_Canvas) -> Tvg_Result;

        pub fn tvg_paint_unref(paint: Tvg_Paint, free: bool) -> u16;
        pub fn tvg_paint_set_transform(paint: Tvg_Paint, m: *const Tvg_Matrix) -> Tvg_Result;
        pub fn tvg_paint_set_opacity(paint: Tvg_Paint, opacity: u8) -> Tvg_Result;
        pub fn tvg_paint_set_blend_method(paint: Tvg_Paint, method: c_int) -> Tvg_Result;
        pub fn tvg_paint_set_clip(paint: Tvg_Paint, clipper: Tvg_Paint) -> Tvg_Result;

        pub fn tvg_scene_new() -> Tvg_Paint;
        pub fn tvg_scene_add(scene: Tvg_Paint, paint: Tvg_Paint) -> Tvg_Result;
        pub fn tvg_scene_add_effect_gaussian_blur(
            scene: Tvg_Paint,
            sigma: f64,
            direction: c_int,
            border: c_int,
            quality: c_int,
        ) -> Tvg_Result;
        pub fn tvg_scene_add_effect_drop_shadow(
            scene: Tvg_Paint,
            r: c_int,
            g: c_int,
            b: c_int,
            a: c_int,
            angle: f64,
            distance: f64,
            sigma: f64,
            quality: c_int,
        ) -> Tvg_Result;

        pub fn tvg_shape_new() -> Tvg_Paint;
        pub fn tvg_shape_move_to(paint: Tvg_Paint, x: f32, y: f32) -> Tvg_Result;
        pub fn tvg_shape_line_to(paint: Tvg_Paint, x: f32, y: f32) -> Tvg_Result;
        pub fn tvg_shape_cubic_to(
            paint: Tvg_Paint,
            cx1: f32,
            cy1: f32,
            cx2: f32,
            cy2: f32,
            x: f32,
            y: f32,
        ) -> Tvg_Result;
        pub fn tvg_shape_close(paint: Tvg_Paint) -> Tvg_Result;
        pub fn tvg_shape_append_rect(
            paint: Tvg_Paint,
            x: f32,
            y: f32,
            w: f32,
            h: f32,
            rx: f32,
            ry: f32,
            cw: bool,
        ) -> Tvg_Result;
        pub fn tvg_shape_append_circle(
            paint: Tvg_Paint,
            cx: f32,
            cy: f32,
            rx: f32,
            ry: f32,
            cw: bool,
        ) -> Tvg_Result;
        pub fn tvg_shape_set_fill_color(
            paint: Tvg_Paint,
            r: u8,
            g: u8,
            b: u8,
            a: u8,
        ) -> Tvg_Result;
        pub fn tvg_shape_set_fill_rule(paint: Tvg_Paint, rule: c_int) -> Tvg_Result;
        pub fn tvg_shape_set_paint_order(paint: Tvg_Paint, stroke_first: bool) -> Tvg_Result;
        pub fn tvg_shape_set_gradient(paint: Tvg_Paint, grad: Tvg_Gradient) -> Tvg_Result;
        pub fn tvg_shape_set_stroke_width(paint: Tvg_Paint, width: f32) -> Tvg_Result;
        pub fn tvg_shape_set_stroke_color(
            paint: Tvg_Paint,
            r: u8,
            g: u8,
            b: u8,
            a: u8,
        ) -> Tvg_Result;
        pub fn tvg_shape_set_stroke_gradient(paint: Tvg_Paint, grad: Tvg_Gradient) -> Tvg_Result;
        pub fn tvg_shape_set_stroke_dash(
            paint: Tvg_Paint,
            pattern: *const f32,
            cnt: u32,
            offset: f32,
        ) -> Tvg_Result;
        pub fn tvg_shape_set_stroke_cap(paint: Tvg_Paint, cap: c_int) -> Tvg_Result;
        pub fn tvg_shape_set_stroke_join(paint: Tvg_Paint, join: c_int) -> Tvg_Result;
        pub fn tvg_shape_set_stroke_miterlimit(paint: Tvg_Paint, limit: f32) -> Tvg_Result;

        pub fn tvg_linear_gradient_new() -> Tvg_Gradient;
        pub fn tvg_radial_gradient_new() -> Tvg_Gradient;
        pub fn tvg_linear_gradient_set(
            grad: Tvg_Gradient,
            x1: f32,
            y1: f32,
            x2: f32,
            y2: f32,
        ) -> Tvg_Result;
        pub fn tvg_radial_gradient_set(
            grad: Tvg_Gradient,
            cx: f32,
            cy: f32,
            r: f32,
            fx: f32,
            fy: f32,
            fr: f32,
        ) -> Tvg_Result;
        pub fn tvg_gradient_set_color_stops(
            grad: Tvg_Gradient,
            stops: *const Tvg_Color_Stop,
            cnt: u32,
        ) -> Tvg_Result;
        pub fn tvg_gradient_set_spread(grad: Tvg_Gradient, spread: c_int) -> Tvg_Result;
        pub fn tvg_gradient_set_transform(grad: Tvg_Gradient, m: *const Tvg_Matrix) -> Tvg_Result;

        pub fn tvg_picture_new() -> Tvg_Paint;
        pub fn tvg_picture_load_raw(
            picture: Tvg_Paint,
            data: *const u32,
            w: u32,
            h: u32,
            cs: c_int,
            copy: bool,
        ) -> Tvg_Result;
    }
}

use capi::*;

// ------------------------------------------------------------------ helpers

/// Identity in SVG order `[a b c d e f]` ((x,y) → (ax+cy+e, bx+dy+f)).
#[cfg_attr(not(test), allow(dead_code))]
const IDENTITY: [f32; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// `outer ∘ inner` (apply `inner` first) — the same composition as
/// tiny-skia's `pre_concat`.
fn mul(outer: &[f32; 6], inner: &[f32; 6]) -> [f32; 6] {
    [
        outer[0] * inner[0] + outer[2] * inner[1],
        outer[1] * inner[0] + outer[3] * inner[1],
        outer[0] * inner[2] + outer[2] * inner[3],
        outer[1] * inner[2] + outer[3] * inner[3],
        outer[0] * inner[4] + outer[2] * inner[5] + outer[4],
        outer[1] * inner[4] + outer[3] * inner[5] + outer[5],
    ]
}

fn concat(ctm: &[f32; 6], m: &Option<[f32; 6]>) -> [f32; 6] {
    match m {
        Some(m) => mul(ctm, m),
        None => *ctm,
    }
}

fn tvg_matrix(m: &[f32; 6]) -> Tvg_Matrix {
    Tvg_Matrix {
        e11: m[0],
        e12: m[2],
        e13: m[4],
        e21: m[1],
        e22: m[3],
        e23: m[5],
        e31: 0.0,
        e32: 0.0,
        e33: 1.0,
    }
}

/// Per-axis scale factors of the transform's linear part.
fn scale_of(m: &[f32; 6]) -> (f32, f32) {
    (
        (m[0] * m[0] + m[1] * m[1]).sqrt(),
        (m[2] * m[2] + m[3] * m[3]).sqrt(),
    )
}

fn a8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

fn blend_method(b: Option<Blend>) -> i32 {
    match b {
        None => 0,
        Some(Blend::Multiply) => 1,
        Some(Blend::Screen) => 2,
        Some(Blend::Overlay) => 3,
        Some(Blend::Darken) => 4,
        Some(Blend::Lighten) => 5,
        Some(Blend::ColorDodge) => 6,
        Some(Blend::ColorBurn) => 7,
        Some(Blend::HardLight) => 8,
        Some(Blend::SoftLight) => 9,
        Some(Blend::Difference) => 10,
        Some(Blend::Exclusion) => 11,
        Some(Blend::Hue) => 12,
        Some(Blend::Saturation) => 13,
        Some(Blend::Color) => 14,
        Some(Blend::Luminosity) => 15,
    }
}

/// A 4-aligned premultiplied RGBA8888 buffer ThorVG can target and the pixel
/// helpers can walk. `Vec<u8>` alone won't do — `tvg_swcanvas_set_target`
/// takes `uint32_t*`, and a byte vec has no 4-byte alignment guarantee.
struct Surface {
    words: Vec<u32>,
    w: u32,
    h: u32,
}

impl Surface {
    fn new(w: u32, h: u32) -> Surface {
        Surface {
            words: vec![0u32; (w * h) as usize],
            w,
            h,
        }
    }

    fn bytes(&self) -> Vec<u8> {
        // Little-endian: ABGR8888 words are RGBA bytes in memory.
        unsafe {
            core::slice::from_raw_parts(self.words.as_ptr() as *const u8, self.words.len() * 4)
        }
        .to_vec()
    }

    fn set_bytes(&mut self, bytes: &[u8]) {
        assert_eq!(bytes.len(), self.words.len() * 4);
        unsafe {
            core::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                self.words.as_mut_ptr() as *mut u8,
                bytes.len(),
            );
        }
    }
}

// ------------------------------------------------------------------- images

/// Images premultiplied into ABGR8888 word buffers once, at load time —
/// referenced zero-copy by per-frame pictures.
pub struct ThorImages {
    surfaces: Vec<Surface>,
}

impl ThorImages {
    pub fn new(anim: &Animation) -> Self {
        let surfaces = anim
            .images
            .iter()
            .map(|img| {
                // Same 8192 ceiling as the offscreen-layer sites below: the
                // dims come off the wire, so an absurd declared size must not
                // drive the allocation.
                let (w, h) = (img.width.clamp(1, 8192), img.height.clamp(1, 8192));
                let mut s = Surface::new(w, h);
                let dst = unsafe {
                    core::slice::from_raw_parts_mut(
                        s.words.as_mut_ptr() as *mut u8,
                        s.words.len() * 4,
                    )
                };
                let n = dst.len().min(img.rgba.len());
                dst[..n].copy_from_slice(&img.rgba[..n]);
                // RTDL images are straight alpha; ThorVG's ABGR8888 wants
                // premultiplied.
                for px in dst.chunks_exact_mut(4) {
                    let a = px[3] as u32;
                    px[0] = ((px[0] as u32 * a + 127) / 255) as u8;
                    px[1] = ((px[1] as u32 * a + 127) / 255) as u8;
                    px[2] = ((px[2] as u32 * a + 127) / 255) as u8;
                }
                s
            })
            .collect();
        ThorImages { surfaces }
    }
}

// ------------------------------------------------------------------- render

static ENGINE: Once = Once::new();
/// One render at a time: the engine runs single-threaded (`threads = 0`) and
/// parallel test threads must not interleave canvas lifecycles.
static RENDER_LOCK: Mutex<()> = Mutex::new(());

/// Rasterize the scene's current state into `out` (tightly packed
/// premultiplied RGBA8888, `w`·`h`·4 bytes) under `fit`, the design-space →
/// device transform in SVG order. Clears `out` first.
pub fn render(anim: &Animation, images: &ThorImages, out: &mut [u8], w: u32, h: u32, fit: [f32; 6]) {
    if out.len() < (w * h * 4) as usize || w == 0 || h == 0 {
        return;
    }
    ENGINE.call_once(|| unsafe {
        tvg_engine_init(0);
    });
    let _guard = RENDER_LOCK.lock().unwrap();
    let mut surface = Surface::new(w, h);
    if !anim.nodes.is_empty() {
        unsafe {
            render_tree(anim, images, 0, &fit, &mut surface);
        }
    }
    out[..(w * h * 4) as usize].copy_from_slice(unsafe {
        core::slice::from_raw_parts(surface.words.as_ptr() as *const u8, (w * h * 4) as usize)
    });
}

/// Renders node `idx`'s tree into `surface` through a fresh SW canvas.
/// `effects_on_root` optionally attaches native scene effects to the root
/// scene before drawing (layer fx fast path).
unsafe fn render_tree(
    anim: &Animation,
    images: &ThorImages,
    idx: u32,
    ctm: &[f32; 6],
    surface: &mut Surface,
) {
    unsafe {
        render_nodes(anim, images, &[idx], ctm, surface, None);
    }
}

unsafe fn render_nodes(
    anim: &Animation,
    images: &ThorImages,
    nodes: &[u32],
    ctm: &[f32; 6],
    surface: &mut Surface,
    effects: Option<&[FxStage]>,
) {
    unsafe {
        let canvas = tvg_swcanvas_create(TVG_ENGINE_OPTION_DEFAULT);
        if canvas.is_null() {
            return;
        }
        tvg_swcanvas_set_target(
            canvas,
            surface.words.as_mut_ptr(),
            surface.w,
            surface.w,
            surface.h,
            TVG_COLORSPACE_ABGR8888,
        );
        let root = tvg_scene_new();
        for &n in nodes {
            draw_node(anim, images, n, root, ctm, surface.w, surface.h);
        }
        if let Some(stages) = effects {
            attach_native_fx(root, stages, ctm);
        }
        tvg_canvas_add(canvas, root);
        tvg_canvas_draw(canvas, false);
        tvg_canvas_sync(canvas);
        tvg_canvas_destroy(canvas);
    }
}

unsafe fn draw_node(
    anim: &Animation,
    images: &ThorImages,
    idx: u32,
    parent: Tvg_Paint,
    ctm: &[f32; 6],
    cw: u32,
    ch: u32,
) {
    match &anim.nodes[idx as usize] {
        Node::Group(g) => unsafe { draw_group(anim, images, g, parent, ctm, cw, ch) },
        Node::Shape(s) => unsafe { draw_shape(s, anim, parent, ctm) },
        Node::Image(i) => unsafe { draw_image(i, images, parent, ctm) },
    }
}

/// Every fx stage the native scene effects can express in order: a lone blur
/// or a lone drop shadow. Anything else (tint chains, bare color matrices,
/// mixed stacks) goes through the shared pixel loops instead.
fn fx_native(stages: &[FxStage]) -> bool {
    stages.iter().all(|st| {
        matches!(
            st.passes.as_slice(),
            [FxPass::Blur { .. }] | [FxPass::Shadow { .. }]
        )
    })
}

unsafe fn attach_native_fx(scene: Tvg_Paint, stages: &[FxStage], ctm: &[f32; 6]) {
    let (sx, sy) = scale_of(ctm);
    for st in stages {
        match &st.passes[..] {
            [FxPass::Blur {
                sx: bx, sy: by, wrap, ..
            }] => {
                let (bx, by) = (bx * sx, by * sy);
                let border = if *wrap { 1 } else { 0 };
                unsafe {
                    if (bx - by).abs() < 1e-3 {
                        if bx > 0.0 {
                            tvg_scene_add_effect_gaussian_blur(scene, bx as f64, 0, border, 100);
                        }
                    } else {
                        if bx > 0.0 {
                            tvg_scene_add_effect_gaussian_blur(scene, bx as f64, 1, border, 100);
                        }
                        if by > 0.0 {
                            tvg_scene_add_effect_gaussian_blur(scene, by as f64, 2, border, 100);
                        }
                    }
                }
            }
            [FxPass::Shadow {
                std_dev,
                dx,
                dy,
                color,
                flood_opacity,
                ..
            }] => {
                // Device-space offset, then back into ThorVG's polar form:
                // it computes offset = (d·cos(r), −d·sin(r)) with
                // r = deg2rad(90 − angle), y-down.
                let odx = ctm[0] * dx + ctm[2] * dy;
                let ody = ctm[1] * dx + ctm[3] * dy;
                let r = (-ody).atan2(odx);
                let angle = 90.0 - r.to_degrees();
                let distance = (odx * odx + ody * ody).sqrt();
                unsafe {
                    tvg_scene_add_effect_drop_shadow(
                        scene,
                        a8(color[0]) as i32,
                        a8(color[1]) as i32,
                        a8(color[2]) as i32,
                        a8(color[3] * flood_opacity) as i32,
                        angle as f64,
                        distance as f64,
                        (std_dev * sx) as f64,
                        100,
                    );
                }
            }
            _ => {}
        }
    }
}

unsafe fn draw_group(
    anim: &Animation,
    images: &ThorImages,
    g: &Group,
    parent: Tvg_Paint,
    ctm: &[f32; 6],
    cw: u32,
    ch: u32,
) {
    if g.hidden || g.opacity <= 0.0 {
        return;
    }
    let ctm = concat(ctm, &g.matrix);

    // Own clip → a nested scene holding the clipper; nesting intersects with
    // every inherited clip above (one clipper per paint in ThorVG).
    let container = match &g.clip {
        Some(c) => {
            let Some(clipper) = (unsafe { clip_shape(c, &ctm) }) else {
                return; // clip resolves to nothing — nothing can draw
            };
            unsafe {
                let s = tvg_scene_new();
                tvg_paint_set_clip(s, clipper);
                tvg_scene_add(parent, s);
                s
            }
        }
        None => parent,
    };

    let needs_layer = g.opacity < 1.0
        || g.blend.is_some()
        || g.mask.is_some()
        || g.cf.is_some()
        || !g.fx.is_empty();
    if !needs_layer {
        for &c in &g.children {
            unsafe { draw_node(anim, images, c, container, &ctm, cw, ch) };
        }
        return;
    }

    // Offscreen layer, sized by the compiler's bbox mapped to device space —
    // the same arithmetic as the tiny-skia walk.
    let Some(bbox) = g.bbox else { return };
    let dev = bounds::map_aabb(&ctm, bbox);
    let x0 = dev[0].floor().max(0.0) as i32;
    let y0 = dev[1].floor().max(0.0) as i32;
    let x1 = (dev[2].ceil() as i32).min(cw as i32);
    let y1 = (dev[3].ceil() as i32).min(ch as i32);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let (w, h) = ((x1 - x0) as u32, (y1 - y0) as u32);
    let mut scratch = Surface::new(w, h);
    let inner = mul(&[1.0, 0.0, 0.0, 1.0, -(x0 as f32), -(y0 as f32)], &ctm);

    let native = !g.fx.is_empty() && fx_native(&g.fx);
    unsafe {
        render_nodes(
            anim,
            images,
            &g.children,
            &inner,
            &mut scratch,
            if native { Some(&g.fx) } else { None },
        );
    }

    // Pixel post-passes on the scratch, in the same order as raster.rs.
    let needs_pixels = (!native && !g.fx.is_empty()) || g.mask.is_some() || g.cf.is_some();
    let mut blit_opacity = g.opacity;
    if needs_pixels {
        let mut data = scratch.bytes();
        if !native {
            let (sx, sy) = scale_of(&ctm);
            for stage in &g.fx {
                data = pixels::apply_stage(data, w, h, stage, sx, sy, &ctm);
            }
        }
        if let Some(m) = &g.mask {
            let mut matte = Surface::new(w, h);
            unsafe { render_nodes(anim, images, &m.children, &inner, &mut matte, None) };
            pixels::mask_apply(&mut data, &matte.bytes(), m.luma);
        }
        if let Some(cf) = &g.cf {
            pixels::matte_invert(&mut data, cf, blit_opacity);
            blit_opacity = 1.0;
        }
        scratch.set_bytes(&data);
    }

    unsafe {
        blit_surface(
            &scratch,
            container,
            x0 as f32,
            y0 as f32,
            blit_opacity,
            blend_method(g.blend),
        );
    }
}

/// Adds `surface` to `container` as a raw picture at (`x0`, `y0`) with
/// opacity and blend — the layer blit. `copy = true`: the scratch dies with
/// this stack frame, the picture lives until the parent canvas draws.
unsafe fn blit_surface(
    surface: &Surface,
    container: Tvg_Paint,
    x0: f32,
    y0: f32,
    opacity: f32,
    blend: i32,
) {
    unsafe {
        let pic = tvg_picture_new();
        tvg_picture_load_raw(
            pic,
            surface.words.as_ptr(),
            surface.w,
            surface.h,
            TVG_COLORSPACE_ABGR8888,
            true,
        );
        tvg_paint_set_transform(pic, &tvg_matrix(&[1.0, 0.0, 0.0, 1.0, x0, y0]));
        tvg_paint_set_opacity(pic, a8(opacity));
        if blend != 0 {
            tvg_paint_set_blend_method(pic, blend);
        }
        tvg_scene_add(container, pic);
    }
}

// ---------------------------------------------------------------- clipping

/// Builds the clipper shape for a clip, device transform applied. `None`
/// means the clip resolves to nothing (e.g. an animated path currently
/// empty) — the caller must skip the subtree.
unsafe fn clip_shape(clip: &Clip, ctm: &[f32; 6]) -> Option<Tvg_Paint> {
    unsafe {
        let s = tvg_shape_new();
        match clip {
            Clip::Rect([x, y, w, h]) => {
                if *w <= 0.0 || *h <= 0.0 {
                    tvg_paint_unref(s, true);
                    return None;
                }
                tvg_shape_append_rect(s, *x, *y, *w, *h, 0.0, 0.0, true);
            }
            Clip::Path { path, even_odd, .. } => {
                if !append_path(s, path) {
                    tvg_paint_unref(s, true);
                    return None;
                }
                if *even_odd {
                    tvg_shape_set_fill_rule(s, TVG_FILL_RULE_EVEN_ODD);
                }
            }
        }
        tvg_paint_set_transform(s, &tvg_matrix(ctm));
        tvg_shape_set_fill_color(s, 255, 255, 255, 255);
        Some(s)
    }
}

// ------------------------------------------------------------------ shapes

/// Appends RTDL path data; false when the path is empty or malformed.
unsafe fn append_path(s: Tvg_Paint, p: &PathData) -> bool {
    if p.verbs.is_empty() {
        return false;
    }
    let pts = &p.points;
    let mut i = 0usize;
    unsafe {
        for &v in &p.verbs {
            match v {
                VERB_MOVE => {
                    tvg_shape_move_to(s, pts[i], pts[i + 1]);
                    i += 2;
                }
                VERB_LINE => {
                    tvg_shape_line_to(s, pts[i], pts[i + 1]);
                    i += 2;
                }
                VERB_CUBIC => {
                    tvg_shape_cubic_to(
                        s,
                        pts[i],
                        pts[i + 1],
                        pts[i + 2],
                        pts[i + 3],
                        pts[i + 4],
                        pts[i + 5],
                    );
                    i += 6;
                }
                VERB_CLOSE => {
                    tvg_shape_close(s);
                }
                _ => return false,
            }
        }
    }
    true
}

/// Appends the geometry; false when it is degenerate (nothing to draw).
unsafe fn append_geom(s: Tvg_Paint, g: &Geom) -> bool {
    unsafe {
        match g {
            Geom::Path(p) => append_path(s, p),
            Geom::Rect { x, y, w, h, rx, ry } => {
                if *w <= 0.0 || *h <= 0.0 {
                    return false;
                }
                let rx = rx.min(w / 2.0).max(0.0);
                let ry = ry.min(h / 2.0).max(0.0);
                let (rx, ry) = if rx <= 0.0 || ry <= 0.0 {
                    (0.0, 0.0)
                } else {
                    (rx, ry)
                };
                tvg_shape_append_rect(s, *x, *y, *w, *h, rx, ry, true);
                true
            }
            Geom::Ellipse { cx, cy, rx, ry } => {
                if *rx <= 0.0 || *ry <= 0.0 {
                    return false;
                }
                tvg_shape_append_circle(s, *cx, *cy, *rx, *ry, true);
                true
            }
        }
    }
}

/// Builds a ThorVG gradient from an RTDL one, folding `alpha` into the stop
/// alphas (ThorVG has no per-fill opacity). Pad spread, like the SVG output.
unsafe fn build_gradient(gi: u32, alpha: f32, anim: &Animation) -> Option<Tvg_Gradient> {
    let g = anim.gradients.get(gi as usize)?;
    unsafe {
        let grad = if g.radial {
            let grad = tvg_radial_gradient_new();
            let (cx, cy) = (g.coords[0], g.coords[1]);
            tvg_radial_gradient_set(grad, cx, cy, g.coords[2].max(1e-6), cx, cy, 0.0);
            grad
        } else {
            let grad = tvg_linear_gradient_new();
            tvg_linear_gradient_set(grad, g.coords[0], g.coords[1], g.coords[2], g.coords[3]);
            grad
        };
        let stops: Vec<Tvg_Color_Stop> = g
            .stops
            .iter()
            .map(|st| Tvg_Color_Stop {
                offset: st.offset.clamp(0.0, 1.0),
                r: a8(st.color[0]),
                g: a8(st.color[1]),
                b: a8(st.color[2]),
                a: a8(st.color[3] * alpha),
            })
            .collect();
        tvg_gradient_set_color_stops(grad, stops.as_ptr(), stops.len() as u32);
        tvg_gradient_set_spread(grad, TVG_STROKE_FILL_PAD);
        if let Some(t) = &g.transform {
            tvg_gradient_set_transform(grad, &tvg_matrix(t));
        }
        Some(grad)
    }
}

unsafe fn set_fill(s: Tvg_Paint, source: &PaintSource, alpha: f32, anim: &Animation) {
    unsafe {
        match source {
            PaintSource::Color([r, g, b, a]) => {
                tvg_shape_set_fill_color(s, a8(*r), a8(*g), a8(*b), a8(a * alpha));
            }
            PaintSource::Gradient(gi) => {
                if let Some(grad) = build_gradient(*gi, alpha, anim) {
                    tvg_shape_set_gradient(s, grad);
                }
            }
        }
    }
}

unsafe fn set_stroke(s: Tvg_Paint, p: &RtPaint, source: &PaintSource, alpha: f32, anim: &Animation) {
    unsafe {
        tvg_shape_set_stroke_width(s, p.stroke_width);
        tvg_shape_set_stroke_cap(s, p.cap.min(2) as i32);
        tvg_shape_set_stroke_join(s, p.join.min(2) as i32);
        tvg_shape_set_stroke_miterlimit(s, p.miter_limit);
        if let Some(d) = &p.dash {
            // SVG repeats an odd-length list doubled; keep the array even.
            let mut arr = d.array.clone();
            if arr.len() % 2 == 1 {
                arr.extend_from_slice(&d.array);
            }
            if arr.iter().any(|v| *v > 0.0) {
                tvg_shape_set_stroke_dash(s, arr.as_ptr(), arr.len() as u32, d.offset);
            }
        }
        match source {
            PaintSource::Color([r, g, b, a]) => {
                tvg_shape_set_stroke_color(s, a8(*r), a8(*g), a8(*b), a8(a * alpha));
            }
            PaintSource::Gradient(gi) => {
                if let Some(grad) = build_gradient(*gi, alpha, anim) {
                    tvg_shape_set_stroke_gradient(s, grad);
                }
            }
        }
    }
}

/// Builds one shape paint with `fold` multiplied into both paint alphas.
unsafe fn shape_paint(s: &Shape, fold: f32, anim: &Animation, ctm: &[f32; 6]) -> Option<Tvg_Paint> {
    unsafe {
        let sh = tvg_shape_new();
        if !append_geom(sh, &s.geom) {
            tvg_paint_unref(sh, true);
            return None;
        }
        if s.even_odd {
            tvg_shape_set_fill_rule(sh, TVG_FILL_RULE_EVEN_ODD);
        }
        if let Some(f) = &s.paint.fill {
            set_fill(sh, f, s.paint.fill_opacity * fold, anim);
        }
        if let Some(st) = &s.paint.stroke {
            set_stroke(sh, &s.paint, st, s.paint.stroke_opacity * fold, anim);
        }
        tvg_shape_set_paint_order(sh, s.paint.stroke_first);
        tvg_paint_set_transform(sh, &tvg_matrix(ctm));
        Some(sh)
    }
}

unsafe fn draw_shape(s: &Shape, anim: &Animation, parent: Tvg_Paint, ctm: &[f32; 6]) {
    if s.hidden || s.opacity <= 0.0 {
        return;
    }
    let ctm = concat(ctm, &s.matrix);
    let both = s.paint.fill.is_some() && s.paint.stroke.is_some();
    if s.opacity < 1.0 && both {
        // Fill and stroke overlap under shape opacity: group-opacity
        // semantics need a composed layer (ThorVG folds paint opacity into
        // fill and stroke separately). Rasterize offscreen and blit — the
        // same fallback the tiny-skia walk uses.
        unsafe { draw_shape_layered(s, anim, parent, &ctm) };
        return;
    }
    if let Some(sh) = unsafe { shape_paint(s, s.opacity, anim, &ctm) } {
        unsafe { tvg_scene_add(parent, sh) };
    }
}

unsafe fn draw_shape_layered(s: &Shape, anim: &Animation, parent: Tvg_Paint, ctm: &[f32; 6]) {
    let pad = bounds::stroke_pad(s);
    let Some(b) = bounds::geom_bounds(&s.geom) else {
        return;
    };
    let dev = bounds::map_aabb(ctm, bounds::pad(b, pad, pad));
    let x0 = dev[0].floor() - 1.0;
    let y0 = dev[1].floor() - 1.0;
    let w = (dev[2].ceil() + 1.0 - x0) as i32;
    let h = (dev[3].ceil() + 1.0 - y0) as i32;
    if w <= 0 || h <= 0 || w > 8192 || h > 8192 {
        return;
    }
    let mut scratch = Surface::new(w as u32, h as u32);
    let inner = mul(&[1.0, 0.0, 0.0, 1.0, -x0, -y0], ctm);
    unsafe {
        let canvas = tvg_swcanvas_create(TVG_ENGINE_OPTION_DEFAULT);
        if canvas.is_null() {
            return;
        }
        tvg_swcanvas_set_target(
            canvas,
            scratch.words.as_mut_ptr(),
            scratch.w,
            scratch.w,
            scratch.h,
            TVG_COLORSPACE_ABGR8888,
        );
        if let Some(sh) = shape_paint(s, 1.0, anim, &inner) {
            tvg_canvas_add(canvas, sh);
            tvg_canvas_draw(canvas, false);
            tvg_canvas_sync(canvas);
        }
        tvg_canvas_destroy(canvas);
        blit_surface(&scratch, parent, x0, y0, s.opacity, 0);
    }
}

// ------------------------------------------------------------------ images

unsafe fn draw_image(i: &ImageRef, images: &ThorImages, parent: Tvg_Paint, ctm: &[f32; 6]) {
    let Some(surface) = images.surfaces.get(i.image as usize) else {
        return;
    };
    let (iw, ih) = (surface.w as f32, surface.h as f32);
    if iw <= 0.0 || ih <= 0.0 || i.w <= 0.0 || i.h <= 0.0 {
        return;
    }
    // lottie-web's `xMidYMid slice`: cover the layer box, center the crop —
    // and clip the overflow to the box.
    let sc = (i.w / iw).max(i.h / ih);
    let tx = (i.w - iw * sc) / 2.0;
    let ty = (i.h - ih * sc) / 2.0;
    unsafe {
        let pic = tvg_picture_new();
        // Zero-copy: the pool outlives every per-frame canvas.
        tvg_picture_load_raw(
            pic,
            surface.words.as_ptr(),
            surface.w,
            surface.h,
            TVG_COLORSPACE_ABGR8888,
            false,
        );
        tvg_paint_set_transform(pic, &tvg_matrix(&mul(ctm, &[sc, 0.0, 0.0, sc, tx, ty])));
        let clip = tvg_shape_new();
        tvg_shape_append_rect(clip, 0.0, 0.0, i.w, i.h, 0.0, 0.0, true);
        tvg_paint_set_transform(clip, &tvg_matrix(ctm));
        tvg_paint_set_clip(pic, clip);
        tvg_scene_add(parent, pic);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rtdl::{Paint as P, Shape};

    /// The one runnable check: ThorVG draws a red circle in a half-opacity
    /// group — center lands at half alpha, outside stays clear (same scene as
    /// the tiny-skia backend's smoke test, same assertions).
    #[test]
    fn draws_layered_circle() {
        let anim = Animation {
            width: 32.0,
            height: 32.0,
            fr: 30.0,
            ip: 0.0,
            op: 1.0,
            nodes: vec![
                Node::Group(Group {
                    opacity: 0.5,
                    bbox: Some([0.0, 0.0, 32.0, 32.0]),
                    children: vec![1],
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
        let images = ThorImages::new(&anim);
        let mut buf = vec![0u8; 32 * 32 * 4];
        render(&anim, &images, &mut buf, 32, 32, IDENTITY);
        let center = &buf[(16 * 32 + 16) * 4..(16 * 32 + 16) * 4 + 4];
        assert!(
            center[3] > 115 && center[3] < 140,
            "half alpha, got {}",
            center[3]
        );
        assert!(center[0] > 110, "premultiplied red at half alpha");
        assert_eq!(buf[(1 * 32 + 1) * 4 + 3], 0, "outside stays clear");
    }
}
