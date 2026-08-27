//! Frame emulator. Evaluates a compiled `data::Payload` at a given frame and
//! produces a structured `Frame` that mirrors what the runtime would draw.

mod frame;
// Public because the scene planner reuses this math to evaluate static
// geometry, transforms and gradients at compile time — keeping exactly one
// implementation of each behind both the reference renderer and the baker.
pub mod geometry;
pub mod gradient;
pub mod keyframes;
pub mod property;
pub mod transform;
pub mod trim;

pub use frame::{
    BezierPath, Color, FillRule, Frame, FrameComposition, Geometry, GradientKind, GradientStop,
    LayerKind, Paint, RenderedLayer, RenderedPrimitive, RenderedStyle, ShapeTree, Transform2D,
};

fn gradient_kind(gk: u8) -> GradientKind {
    if gk == 2 {
        GradientKind::Radial
    } else {
        GradientKind::Linear
    }
}

fn fill_rule(fr: u8) -> FillRule {
    if fr == 2 {
        FillRule::EvenOdd
    } else {
        FillRule::NonZero
    }
}

use anyhow::{Result, anyhow};

use crate::data::{self, Payload, Shape, ShapeRef, Style};

pub fn render(payload: &Payload, frame: f64) -> Result<Frame> {
    let frame = frame
        .max(payload.c.ip)
        .min((payload.c.op - 1.0).max(payload.c.ip));

    let mut layers = Vec::with_capacity(payload.l.len());
    for layer in &payload.l {
        layers.push(render_layer(payload, layer, frame)?);
    }
    Ok(Frame {
        composition: FrameComposition {
            width: payload.c.w,
            height: payload.c.h,
            frame_rate: payload.c.fr,
            frame,
        },
        layers,
    })
}

fn render_layer(payload: &Payload, layer: &data::Layer, frame: f64) -> Result<RenderedLayer> {
    let visible = frame >= layer.ip && frame < layer.op;
    let name = layer.n.and_then(|i| payload.st.get(i as usize).cloned());
    let xform = transform::eval_layer_transform(layer, frame)?;
    let masks = build_masks(layer.mk.as_deref().unwrap_or(&[]), frame)?;
    let kind = match layer.ty {
        4 => {
            let mut trees = Vec::new();
            if let Some(shapes) = &layer.shapes {
                for sr in shapes {
                    trees.push(render_shape_ref(payload, sr, frame)?);
                }
            }
            LayerKind::Shape(trees)
        }
        3 => LayerKind::Null,
        0 => {
            let refid = layer.rf.as_deref().unwrap_or("");
            let asset = payload
                .a
                .as_ref()
                .and_then(|a| a.get(refid))
                .ok_or_else(|| anyhow!("precomp ref `{}` not found", refid))?;
            let rate = if layer.sr == 0.0 { 1.0 } else { layer.sr };
            let inner_frame = (frame - layer.st.unwrap_or(0.0)) / rate;
            let inner_layers = match asset {
                data::Asset::Precomp { l } => l,
                _ => return Err(anyhow!("layer ty=0 references non-precomp asset")),
            };
            let mut rendered = Vec::with_capacity(inner_layers.len());
            for il in inner_layers {
                rendered.push(render_layer(payload, il, inner_frame)?);
            }
            LayerKind::Precomp(rendered)
        }
        ty => LayerKind::Unsupported(ty),
    };
    Ok(RenderedLayer {
        index: layer.i,
        name,
        layer_type: layer.ty,
        transform: xform.to_matrix(),
        opacity: xform.opacity / 100.0,
        visible,
        kind,
        masks,
    })
}

fn build_masks(masks: &[data::LayerMask], frame: f64) -> Result<Vec<frame::RenderedMask>> {
    let mut out = Vec::with_capacity(masks.len());
    for m in masks {
        let path = property::eval_path(&m.pt, frame)?;
        let opacity = match &m.o {
            Some(p) => property::eval_scalar(p, frame)? / 100.0,
            None => 1.0,
        };
        let mode = m.m.chars().next().unwrap_or('a');
        out.push(frame::RenderedMask {
            mode,
            inverted: m.inv,
            path,
            opacity,
        });
    }
    Ok(out)
}

fn render_shape_ref(payload: &Payload, sr: &ShapeRef, frame: f64) -> Result<ShapeTree> {
    match sr {
        ShapeRef::Prim(prim) => {
            let shape = payload
                .s
                .get(prim.s as usize)
                .ok_or_else(|| anyhow!("shape ref {} out of range", prim.s))?;
            let geometry = build_geometry(shape, frame)?;
            let mut styles = Vec::new();
            for &yid in &prim.y {
                let style = payload
                    .y
                    .get(yid as usize)
                    .ok_or_else(|| anyhow!("style ref {} out of range", yid))?;
                styles.push(build_style(style, frame)?);
            }
            // The whole chain, in application order — a shape under a group
            // trim and a layer trim reports both.
            for &tm in &prim.tm {
                let style = payload
                    .y
                    .get(tm as usize)
                    .ok_or_else(|| anyhow!("trim style ref {} out of range", tm))?;
                styles.push(build_style(style, frame)?);
            }
            Ok(ShapeTree::Primitive(RenderedPrimitive { geometry, styles }))
        }
        ShapeRef::Group(grp) => {
            let xform = transform::eval_group_transform(grp, frame)?;
            let mut children = Vec::new();
            for child in &grp.c {
                children.push(render_shape_ref(payload, child, frame)?);
            }
            Ok(ShapeTree::Group {
                transform: xform.to_matrix(),
                opacity: xform.opacity / 100.0,
                children,
            })
        }
    }
}

fn build_geometry(shape: &Shape, frame: f64) -> Result<Geometry> {
    match shape {
        Shape::Rect { sz, ps, rd, rv, .. } => {
            let size = property::eval_vec2(sz, frame)?;
            let pos = property::eval_vec2(ps, frame)?;
            let r = property::eval_scalar(rd, frame)?;
            Ok(Geometry::Path(geometry::rect_to_path(
                pos,
                size,
                r,
                *rv != 0,
            )))
        }
        Shape::Ellipse { sz, ps, rv, .. } => {
            let size = property::eval_vec2(sz, frame)?;
            let pos = property::eval_vec2(ps, frame)?;
            Ok(Geometry::Path(geometry::ellipse_to_path(
                pos,
                size,
                *rv != 0,
            )))
        }
        Shape::Path { pt, .. } => {
            let path = property::eval_path(pt, frame)?;
            Ok(Geometry::Path(path))
        }
        Shape::PolyStar {
            sy,
            pt,
            ps,
            or,
            ir,
            rt,
            os,
            is,
            rv,
            ..
        } => {
            let points = property::eval_scalar(pt, frame)?;
            let pos = property::eval_vec2(ps, frame)?;
            let outer = property::eval_scalar(or, frame)?;
            let inner = property::eval_scalar(ir, frame)?;
            let rot = property::eval_scalar(rt, frame)?;
            let osr = match os {
                Some(p) => property::eval_scalar(p, frame)?,
                None => 0.0,
            };
            let isr = match is {
                Some(p) => property::eval_scalar(p, frame)?,
                None => 0.0,
            };
            Ok(Geometry::Path(geometry::polystar_to_path(
                *sy,
                pos,
                points,
                outer,
                inner,
                rot,
                osr,
                isr,
                *rv != 0,
            )))
        }
    }
}

fn build_style(style: &Style, frame: f64) -> Result<RenderedStyle> {
    match style {
        Style::Fill { c, o, fr } => {
            let color = property::eval_color(c, frame)?;
            let opacity = property::eval_scalar(o, frame)?;
            let rule = if *fr == 2 {
                FillRule::EvenOdd
            } else {
                FillRule::NonZero
            };
            Ok(RenderedStyle::Paint(Paint::Solid {
                color,
                opacity,
                rule,
            }))
        }
        Style::Stroke {
            c,
            o,
            w,
            lc,
            lj,
            ml,
            dl,
            dof,
        } => {
            let color = property::eval_color(c, frame)?;
            let opacity = property::eval_scalar(o, frame)?;
            let width = property::eval_scalar(w, frame)?;
            Ok(RenderedStyle::Stroke {
                color,
                opacity,
                width,
                linecap: *lc,
                linejoin: *lj,
                miter_limit: *ml,
                dash: eval_dash(dl, dof, frame)?,
            })
        }
        Style::TrimPath { s, e, o, m } => {
            let start = property::eval_scalar(s, frame)?;
            let end = property::eval_scalar(e, frame)?;
            let offset = property::eval_scalar(o, frame)?;
            Ok(RenderedStyle::TrimPath {
                start,
                end,
                offset,
                mode: *m,
            })
        }
        Style::GradientFill { g, o, s, e, gk, fr } => {
            let stops = gradient::resolve_stops(g)?;
            let opacity = property::eval_scalar(o, frame)?;
            let start = match s {
                Some(p) => property::eval_vec2(p, frame)?,
                None => [0.0, 0.0],
            };
            let end = match e {
                Some(p) => property::eval_vec2(p, frame)?,
                None => [0.0, 0.0],
            };
            Ok(RenderedStyle::Paint(Paint::Gradient {
                kind: gradient_kind(*gk),
                rule: fill_rule(*fr),
                stops,
                start,
                end,
                opacity,
            }))
        }
        Style::GradientStroke {
            g,
            w,
            o,
            s,
            e,
            gk,
            lc,
            lj,
            ml,
            dl,
            dof,
        } => {
            let stops = gradient::resolve_stops(g)?;
            let opacity = property::eval_scalar(o, frame)?;
            let width = property::eval_scalar(w, frame)?;
            let start = match s {
                Some(p) => property::eval_vec2(p, frame)?,
                None => [0.0, 0.0],
            };
            let end = match e {
                Some(p) => property::eval_vec2(p, frame)?,
                None => [0.0, 0.0],
            };
            Ok(RenderedStyle::GradientStroke {
                kind: gradient_kind(*gk),
                stops,
                start,
                end,
                opacity,
                width,
                linecap: *lc,
                linejoin: *lj,
                miter_limit: *ml,
                dash: eval_dash(dl, dof, frame)?,
            })
        }
    }
}

/// A dash pattern at one frame: the lengths in draw order plus the offset,
/// `None` for a solid stroke.
fn eval_dash(
    dl: &[crate::data::InlineProp],
    dof: &Option<crate::data::InlineProp>,
    frame: f64,
) -> Result<Option<frame::RenderedDash>> {
    if dl.is_empty() {
        return Ok(None);
    }
    let mut lengths = Vec::with_capacity(dl.len());
    for p in dl {
        lengths.push(property::eval_scalar(p, frame)?);
    }
    let offset = match dof {
        Some(p) => property::eval_scalar(p, frame)?,
        None => 0.0,
    };
    Ok(Some(frame::RenderedDash { lengths, offset }))
}
