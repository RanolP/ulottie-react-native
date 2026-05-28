//! Frame emulator. Evaluates a compiled `data::Payload` at a given frame and
//! produces a structured `Frame` that mirrors what `runtime/driver.js` would
//! draw — but without a browser, SVG, or DOM.
//!
//! Snapshots of `Frame` (via its `Display` impl) act as the fast regression
//! gate for the compiler. The visual pixel-diff harness is the ground-truth
//! gate; this is the unit-testable, hermetic, sub-second alternative.
//!
//! Status: step 1 — static properties + shape primitives + solid fills. No
//! keyframes, no transforms (identity), no expressions, no precomps. The
//! module grows in lock-step with the H2 ordering in the plan.

mod frame;
mod geometry;
mod gradient;
mod keyframes;
mod property;
mod transform;

pub use frame::{
    BezierPath, Color, FillRule, Frame, FrameComposition, Geometry, GradientKind,
    GradientStop, LayerKind, Paint, RenderedLayer, RenderedPrimitive, RenderedStyle,
    ShapeTree, Transform2D,
};

fn gradient_kind(gk: u8) -> GradientKind {
    if gk == 2 { GradientKind::Radial } else { GradientKind::Linear }
}

fn fill_rule(fr: u8) -> FillRule {
    if fr == 2 { FillRule::EvenOdd } else { FillRule::NonZero }
}

use anyhow::{Result, anyhow};

use crate::data::{self, Payload, Shape, ShapeRef, Style};

/// Evaluate `payload` at `frame` (in composition frame units; usually equal
/// to a sample from `[ip, op)`). The frame number is clamped to that range.
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
    let xform = transform::eval_layer_transform(payload, layer, frame)?;
    let masks = build_masks(payload, layer.mk.as_deref().unwrap_or(&[]), frame)?;
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
            // Precomp instance — look up the asset's layers, render them with
            // an inner clock offset by `layer.st` (start time of the instance
            // within its outer comp). Visibility still gates by outer
            // [ip, op).
            let refid = layer.rf.as_deref().unwrap_or("");
            let asset = payload
                .a
                .as_ref()
                .and_then(|a| a.get(refid))
                .ok_or_else(|| anyhow!("precomp ref `{}` not found in assets", refid))?;
            let inner_frame = frame - layer.st.unwrap_or(0.0);
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

fn build_masks(
    payload: &Payload,
    masks: &[data::LayerMask],
    frame: f64,
) -> Result<Vec<frame::RenderedMask>> {
    let mut out = Vec::with_capacity(masks.len());
    for m in masks {
        let path = property::eval_path(payload, m.pt, frame)?;
        let opacity = match m.o {
            Some(id) => property::eval_scalar(payload, id, frame)? / 100.0,
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
            let geometry = build_geometry(payload, shape, frame)?;
            let mut styles = Vec::new();
            for &yid in &prim.y {
                let style = payload
                    .y
                    .get(yid as usize)
                    .ok_or_else(|| anyhow!("style ref {} out of range", yid))?;
                styles.push(build_style(payload, style, frame)?);
            }
            if let Some(tm) = prim.tm {
                let style = payload
                    .y
                    .get(tm as usize)
                    .ok_or_else(|| anyhow!("trim style ref {} out of range", tm))?;
                styles.push(build_style(payload, style, frame)?);
            }
            Ok(ShapeTree::Primitive(RenderedPrimitive { geometry, styles }))
        }
        ShapeRef::Group(grp) => {
            let xform = transform::eval_group_transform(payload, grp, frame)?;
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

fn build_geometry(payload: &Payload, shape: &Shape, frame: f64) -> Result<Geometry> {
    match shape {
        Shape::Rect { sz, ps, rd, .. } => {
            let size = property::eval_vec2(payload, *sz, frame)?;
            let pos = property::eval_vec2(payload, *ps, frame)?;
            let r = property::eval_scalar(payload, *rd, frame)?;
            Ok(Geometry::Path(geometry::rect_to_path(pos, size, r)))
        }
        Shape::Ellipse { sz, ps, .. } => {
            let size = property::eval_vec2(payload, *sz, frame)?;
            let pos = property::eval_vec2(payload, *ps, frame)?;
            Ok(Geometry::Path(geometry::ellipse_to_path(pos, size)))
        }
        Shape::Path { pt, .. } => {
            let path = property::eval_path(payload, *pt, frame)?;
            Ok(Geometry::Path(path))
        }
        Shape::PolyStar {
            sy, pt, ps, or, ir, rt, ..
        } => {
            let points = property::eval_scalar(payload, *pt, frame)?;
            let pos = property::eval_vec2(payload, *ps, frame)?;
            let outer = property::eval_scalar(payload, *or, frame)?;
            let inner = property::eval_scalar(payload, *ir, frame)?;
            let rot = property::eval_scalar(payload, *rt, frame)?;
            Ok(Geometry::Path(geometry::polystar_to_path(
                *sy, pos, points, outer, inner, rot,
            )))
        }
        Shape::Group { .. } => Err(anyhow!("Shape::Group is handled at ShapeRef level")),
    }
}

fn build_style(payload: &Payload, style: &Style, frame: f64) -> Result<RenderedStyle> {
    match style {
        Style::Fill { c, o } => {
            let color = property::eval_color(payload, *c, frame)?;
            let opacity = property::eval_scalar(payload, *o, frame)?;
            Ok(RenderedStyle::Paint(Paint::Solid { color, opacity }))
        }
        Style::Stroke { c, o, w, lc, lj, ml } => {
            let color = property::eval_color(payload, *c, frame)?;
            let opacity = property::eval_scalar(payload, *o, frame)?;
            let width = property::eval_scalar(payload, *w, frame)?;
            Ok(RenderedStyle::Stroke {
                color,
                opacity,
                width,
                linecap: *lc,
                linejoin: *lj,
                miter_limit: *ml,
            })
        }
        Style::TrimPath { s, e, o, m } => {
            let start = property::eval_scalar(payload, *s, frame)?;
            let end = property::eval_scalar(payload, *e, frame)?;
            let offset = property::eval_scalar(payload, *o, frame)?;
            Ok(RenderedStyle::TrimPath {
                start,
                end,
                offset,
                mode: *m,
            })
        }
        Style::GradientFill { g, o, s, e, gk, fr } => {
            let stops = gradient::resolve_stops(g)?;
            let opacity = property::eval_scalar(payload, *o, frame)?;
            let start = match s {
                Some(id) => property::eval_vec2(payload, *id, frame)?,
                None => [0.0, 0.0],
            };
            let end = match e {
                Some(id) => property::eval_vec2(payload, *id, frame)?,
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
            g, w, o, s, e, gk, lc, lj, ml,
        } => {
            let stops = gradient::resolve_stops(g)?;
            let opacity = property::eval_scalar(payload, *o, frame)?;
            let width = property::eval_scalar(payload, *w, frame)?;
            let start = match s {
                Some(id) => property::eval_vec2(payload, *id, frame)?,
                None => [0.0, 0.0],
            };
            let end = match e {
                Some(id) => property::eval_vec2(payload, *id, frame)?,
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
            })
        }
    }
}
