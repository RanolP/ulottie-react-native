//! Lottie AST → μlottie IR lowering.
//!
//! Walks an `Animation` and produces a `Module`. Resolves parent/child layer
//! links into `LayerId`s, lifts raw property JSON into typed `Property<T>`,
//! and registers each expression in the module's `ExprTable`.
//!
//! Lowering is deliberately deterministic and lossless w.r.t. supported
//! features. Anything we don't yet model is captured as `LayerKind::Other` or
//! a raw `serde_json::Value` so that subsequent passes / backends can still
//! reason about it.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use anyhow::Result;

use crate::lottie::{self, Animation, GraphicElement, Keyframe as AstKeyframe};
use crate::lottie::property::Property as AstProperty;

use super::types::*;

// ---------------------------------------------------------------------------
// Public entry
// ---------------------------------------------------------------------------

pub fn lower(anim: &Animation) -> Result<Module> {
    let composition = Composition {
        name: anim.name.clone(),
        width: anim.width,
        height: anim.height,
        frame_rate: anim.frame_rate,
        in_point: anim.in_point,
        out_point: anim.out_point,
        is_3d: anim.ddd.unwrap_or(0) != 0,
    };

    let mut module = Module::new(composition);

    // Assign LayerIds in source order; build an `ind -> LayerId` lookup so we
    // can resolve `parent` references inside the same composition. (Precomps
    // have their own layer space, handled separately when lowering an asset.)
    let mut ctx = LowerContext::default();
    let layers = lower_layers(&mut module, &mut ctx, &anim.layers)?;
    module.layers = layers;

    // Lower assets after top-level layers so that their internal layer ids
    // don't collide with the top-level mapping.
    for asset in &anim.assets {
        let kind = if let Some(asset_layers) = &asset.layers {
            let mut sub_ctx = LowerContext::default();
            let inner = lower_layers(&mut module, &mut sub_ctx, asset_layers)?;
            AssetKind::Precomp { layers: inner }
        } else {
            AssetKind::Image {
                path: asset.path.clone(),
                filename: asset.filename.clone(),
                width: asset.w.unwrap_or(0),
                height: asset.h.unwrap_or(0),
                embedded: asset.e.unwrap_or(0) != 0,
            }
        };
        module.assets.push(Asset {
            id: asset.id.clone(),
            name: asset.name.clone(),
            kind,
        });
    }

    Ok(module)
}

// ---------------------------------------------------------------------------
// Layer lowering
// ---------------------------------------------------------------------------

#[derive(Default)]
struct LowerContext {
    /// `ind` (1-based composition index) → `LayerId` (0-based index into the
    /// flat layers vector for this composition).
    ind_to_id: HashMap<u32, LayerId>,
}

fn lower_layers(
    module: &mut Module,
    ctx: &mut LowerContext,
    layers: &[lottie::Layer],
) -> Result<Vec<Layer>> {
    // First pass: allocate LayerIds and record ind → id mapping.
    for (idx, layer) in layers.iter().enumerate() {
        let id = LayerId(idx as u32);
        if let Some(ind) = layer.ind {
            ctx.ind_to_id.insert(ind, id);
        }
    }

    // Second pass: lower each layer body.
    let mut out = Vec::with_capacity(layers.len());
    for (idx, layer) in layers.iter().enumerate() {
        let id = LayerId(idx as u32);
        out.push(lower_layer(module, ctx, id, layer)?);
    }
    Ok(out)
}

fn lower_layer(
    module: &mut Module,
    ctx: &LowerContext,
    id: LayerId,
    src: &lottie::Layer,
) -> Result<Layer> {
    let parent = src
        .parent
        .and_then(|ind| ctx.ind_to_id.get(&ind).copied());

    let kind = match src.ty {
        0 => LayerKind::Precomp {
            asset: src.ref_id.clone().unwrap_or_default(),
            width: src.width.unwrap_or(0),
            height: src.height.unwrap_or(0),
        },
        1 => LayerKind::Solid {
            color: src.sc.clone().unwrap_or_else(|| "#000000".to_string()),
            width: src.sw.unwrap_or(0),
            height: src.sh.unwrap_or(0),
        },
        2 => LayerKind::Image {
            asset: src.ref_id.clone().unwrap_or_default(),
        },
        3 => LayerKind::Null,
        4 => LayerKind::Shape {
            shapes: src
                .shapes
                .as_ref()
                .map(|v| lower_shapes(module, v))
                .transpose()?
                .unwrap_or_default(),
        },
        ty => LayerKind::Other { ty },
    };

    let transform = lower_transform_block(module, &src.ks)?;
    let effects = lower_effects(module, src.ef.as_ref())?;
    let masks = lower_masks(module, src.masks_properties.as_ref())?;

    Ok(Layer {
        id,
        name: src.name.clone(),
        index: src.ind.unwrap_or(0),
        parent,
        kind,
        transform,
        effects,
        in_point: src.ip,
        out_point: src.op,
        stretch: src.sr.unwrap_or(1.0),
        start_time: src.st.unwrap_or(0.0),
        is_3d: src.ddd.unwrap_or(0) != 0,
        auto_orient: src.ao.unwrap_or(0) != 0,
        // The Lottie spec uses `hd: 1` to mean "hidden". On layers the field
        // isn't on our AST; treat as not-hidden by default.
        hidden: false,
        blend_mode: src.bm.unwrap_or(0),
        track_matte: src.tt,
        matte_layer_for_above: src.td.unwrap_or(0) != 0,
        has_mask: src.has_mask.unwrap_or(false),
        masks,
    })
}

fn lower_masks(
    module: &mut Module,
    props: Option<&Vec<lottie::MaskProperty>>,
) -> Result<Vec<LayerMask>> {
    let Some(props) = props else { return Ok(Vec::new()); };
    let mut out = Vec::with_capacity(props.len());
    for m in props {
        let mode = match m.mode.as_str() {
            "a" => MaskMode::Add,
            "s" => MaskMode::Subtract,
            _ => MaskMode::Other,
        };
        let shape = lower_prop_path(module, &m.pt)?;
        let opacity = match &m.o {
            Some(p) => Some(lower_prop_scalar(module, p, 100.0)?),
            None => None,
        };
        out.push(LayerMask { mode, inverted: m.inv, shape, opacity });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Transform lowering
// ---------------------------------------------------------------------------

fn lower_transform_block(module: &mut Module, ks: &lottie::TransformBlock) -> Result<Transform> {
    Ok(Transform {
        anchor: lower_prop_vec3(module, ks.anchor(), [0.0, 0.0, 0.0])?,
        position: lower_prop_vec3(module, ks.position(), [0.0, 0.0, 0.0])?,
        scale: lower_prop_vec3(module, ks.scale(), [100.0, 100.0, 100.0])?,
        rotation: lower_prop_scalar(module, ks.rotation(), 0.0)?,
        opacity: lower_prop_scalar(module, ks.opacity(), 100.0)?,
        skew: match &ks.sk {
            Some(p) => Some(lower_prop_scalar(module, p, 0.0)?),
            None => None,
        },
        skew_axis: match &ks.sa {
            Some(p) => Some(lower_prop_scalar(module, p, 0.0)?),
            None => None,
        },
    })
}

// ---------------------------------------------------------------------------
// Property lowering — generic over T via per-type entry points
// ---------------------------------------------------------------------------

fn lower_prop_scalar(
    module: &mut Module,
    p: &AstProperty,
    default: Scalar,
) -> Result<Property<Scalar>> {
    let value_source = if p.is_animated() {
        ValueSource::Animated(Keyframes {
            frames: p
                .keyframes()
                .unwrap_or(&[])
                .iter()
                .map(|k| lower_kf(k, parse_scalar))
                .collect(),
        })
    } else {
        ValueSource::Static(parse_scalar_opt(p.static_value()).unwrap_or(default))
    };
    Ok(wrap_with_expr(module, p, value_source))
}

fn lower_prop_vec2(
    module: &mut Module,
    p: &AstProperty,
    default: Vec2,
) -> Result<Property<Vec2>> {
    if let Some(split) = p.split() {
        return lower_split_to_vec(module, split, [default[0], default[1], 0.0])
            .map(project_vec3_to_vec2);
    }
    let value_source = if p.is_animated() {
        ValueSource::Animated(Keyframes {
            frames: p
                .keyframes()
                .unwrap_or(&[])
                .iter()
                .map(|k| lower_kf(k, parse_vec2))
                .collect(),
        })
    } else {
        ValueSource::Static(parse_vec2_opt(p.static_value()).unwrap_or(default))
    };
    Ok(wrap_with_expr(module, p, value_source))
}

fn lower_prop_vec3(
    module: &mut Module,
    p: &AstProperty,
    default: Vec3,
) -> Result<Property<Vec3>> {
    if let Some(split) = p.split() {
        return lower_split_to_vec(module, split, default);
    }
    let value_source = if p.is_animated() {
        ValueSource::Animated(Keyframes {
            frames: p
                .keyframes()
                .unwrap_or(&[])
                .iter()
                .map(|k| lower_kf(k, parse_vec3))
                .collect(),
        })
    } else {
        ValueSource::Static(parse_vec3_opt(p.static_value()).unwrap_or(default))
    };
    Ok(wrap_with_expr(module, p, value_source))
}

/// Collapse a separated-dimensions property `{s: true, x, y, z?}` into a
/// single Vec3 property. Static-on-both-axes collapses to Static; otherwise
/// we merge the per-axis keyframe times into a unified timeline and sample
/// each axis at every shared time. This is what AE / lottie-web produce
/// when rendering a Separate-Dimensions position.
fn lower_split_to_vec(
    module: &mut Module,
    split: &lottie::property::SplitProperty,
    default: Vec3,
) -> Result<Property<Vec3>> {
    let x_prop = lower_prop_scalar(module, &split.x, default[0])?;
    let y_prop = lower_prop_scalar(module, &split.y, default[1])?;
    let z_prop = if let Some(z) = split.z.as_deref() {
        Some(lower_prop_scalar(module, z, default[2])?)
    } else {
        None
    };

    // Fast path: every axis is static — emit a static Vec3 directly.
    if let (Some(xs), Some(ys), Some(zs)) = (
        static_scalar(&x_prop),
        static_scalar(&y_prop),
        z_prop.as_ref().map(static_scalar).unwrap_or(Some(default[2])),
    ) {
        return Ok(Property::Static([xs, ys, zs]));
    }

    // General path: collect every keyframe time used by any axis, evaluate
    // each axis at those times, build a single Vec3 keyframe set.
    let mut times: Vec<f64> = collect_times(&x_prop)
        .into_iter()
        .chain(collect_times(&y_prop))
        .chain(z_prop.as_ref().map(collect_times).unwrap_or_default())
        .collect();
    times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    times.dedup();
    if times.is_empty() {
        times.push(0.0);
    }

    let frames: Vec<Keyframe<Vec3>> = times
        .iter()
        .map(|&t| {
            let x = eval_scalar_at(&x_prop, t, default[0]);
            let y = eval_scalar_at(&y_prop, t, default[1]);
            let z = z_prop
                .as_ref()
                .map(|p| eval_scalar_at(p, t, default[2]))
                .unwrap_or(default[2]);
            Keyframe {
                time: t,
                value: Some([x, y, z]),
                end_value: None,
                easing_in: None,
                easing_out: None,
                spatial_in: None,
                spatial_out: None,
                hold: false,
            }
        })
        .collect();

    Ok(Property::Animated(Keyframes { frames }))
}

fn project_vec3_to_vec2(prop: Property<Vec3>) -> Property<Vec2> {
    // `spatial_in` / `spatial_out` are typed `Option<Vec3>` regardless of the
    // outer keyframe value, so we keep them as-is and only project the
    // value / end_value down to 2D.
    match prop {
        Property::Static(v) => Property::Static([v[0], v[1]]),
        Property::Animated(kf) => Property::Animated(Keyframes {
            frames: kf.frames.into_iter().map(project_kf_vec3_to_vec2).collect(),
        }),
        Property::Expression { fallback, expr } => Property::Expression {
            fallback: match fallback {
                ValueSource::Static(v) => ValueSource::Static([v[0], v[1]]),
                ValueSource::Animated(kf) => ValueSource::Animated(Keyframes {
                    frames: kf
                        .frames
                        .into_iter()
                        .map(project_kf_vec3_to_vec2)
                        .collect(),
                }),
            },
            expr,
        },
    }
}

fn project_kf_vec3_to_vec2(f: Keyframe<Vec3>) -> Keyframe<Vec2> {
    Keyframe {
        time: f.time,
        value: f.value.map(|v| [v[0], v[1]]),
        end_value: f.end_value.map(|v| [v[0], v[1]]),
        easing_in: f.easing_in,
        easing_out: f.easing_out,
        spatial_in: f.spatial_in,
        spatial_out: f.spatial_out,
        hold: f.hold,
    }
}

fn static_scalar(p: &Property<Scalar>) -> Option<Scalar> {
    match p {
        Property::Static(v) => Some(*v),
        _ => None,
    }
}

fn collect_times(p: &Property<Scalar>) -> Vec<f64> {
    match p {
        Property::Animated(kf) => kf.frames.iter().map(|f| f.time).collect(),
        Property::Expression { fallback: ValueSource::Animated(kf), .. } => {
            kf.frames.iter().map(|f| f.time).collect()
        }
        _ => Vec::new(),
    }
}

fn eval_scalar_at(p: &Property<Scalar>, t: f64, fallback: Scalar) -> Scalar {
    match p {
        Property::Static(v) => *v,
        Property::Animated(kf) => interpolate_scalar(kf, t, fallback),
        Property::Expression { fallback: ValueSource::Static(v), .. } => *v,
        Property::Expression { fallback: ValueSource::Animated(kf), .. } => {
            interpolate_scalar(kf, t, fallback)
        }
    }
}

fn interpolate_scalar(kf: &Keyframes<Scalar>, t: f64, fallback: Scalar) -> Scalar {
    let frames = &kf.frames;
    if frames.is_empty() {
        return fallback;
    }
    if t <= frames[0].time {
        return frames[0].value.unwrap_or(fallback);
    }
    let last = frames.last().unwrap();
    if t >= last.time {
        return last
            .end_value
            .or(last.value)
            .unwrap_or(fallback);
    }
    for i in 0..frames.len() - 1 {
        let a = &frames[i];
        let b = &frames[i + 1];
        if t >= a.time && t <= b.time {
            let dt = b.time - a.time;
            if dt == 0.0 {
                return b.value.unwrap_or(fallback);
            }
            let progress = (t - a.time) / dt;
            let av = a.value.unwrap_or(fallback);
            let bv = a.end_value.or(b.value).unwrap_or(fallback);
            return av + (bv - av) * progress;
        }
    }
    fallback
}

fn lower_prop_color(
    module: &mut Module,
    p: &AstProperty,
    default: Color,
) -> Result<Property<Color>> {
    let value_source = if p.is_animated() {
        ValueSource::Animated(Keyframes {
            frames: p
                .keyframes()
                .unwrap_or(&[])
                .iter()
                .map(|k| lower_kf(k, parse_color))
                .collect(),
        })
    } else {
        ValueSource::Static(parse_color_opt(p.static_value()).unwrap_or(default))
    };
    Ok(wrap_with_expr(module, p, value_source))
}

fn lower_prop_path(
    module: &mut Module,
    p: &AstProperty,
) -> Result<Property<PathData>> {
    let value_source = if p.is_animated() {
        ValueSource::Animated(Keyframes {
            frames: p
                .keyframes()
                .unwrap_or(&[])
                .iter()
                .map(lower_kf_path)
                .collect(),
        })
    } else {
        ValueSource::Static(parse_path_opt(p.static_value()).unwrap_or_default())
    };
    Ok(wrap_with_expr(module, p, value_source))
}

/// Adapter for properties that already have a ValueSource. Attaches an
/// expression id if the AST property carries an `x` field.
fn wrap_with_expr<T: Clone>(
    module: &mut Module,
    src: &AstProperty,
    value_source: ValueSource<T>,
) -> Property<T> {
    if let Some(expr_body) = src.expression() {
        let id = module.expressions.insert(build_expression(expr_body.to_string()));
        Property::Expression {
            fallback: value_source,
            expr: id,
        }
    } else {
        match value_source {
            ValueSource::Static(v) => Property::Static(v),
            ValueSource::Animated(kf) => Property::Animated(kf),
        }
    }
}

// ---------------------------------------------------------------------------
// Keyframe lowering. The values we extract depend on T; pass the parser in.
// ---------------------------------------------------------------------------

fn lower_kf<T: Clone>(
    src: &AstKeyframe,
    parse: fn(&[f64]) -> Option<T>,
) -> Keyframe<T> {
    Keyframe {
        time: src.time,
        value: src.start_numbers().as_deref().and_then(parse),
        end_value: src.end_numbers().as_deref().and_then(parse),
        easing_in: src.in_tangent.as_ref().map(lower_easing),
        easing_out: src.out_tangent.as_ref().map(lower_easing),
        spatial_in: src
            .spatial_tangent_in
            .as_deref()
            .and_then(parse_vec3),
        spatial_out: src
            .spatial_tangent_to
            .as_deref()
            .and_then(parse_vec3),
        // Hold keyframes: Lottie uses a separate field (`h: 1`); not currently
        // in our AST. Default to false; can be reintroduced in the AST later.
        hold: false,
    }
}

fn lower_kf_path(src: &AstKeyframe) -> Keyframe<PathData> {
    // Path keyframes wrap the bezier shape in a single-element array, e.g.
    //   s: [{ v: [...], i: [...], o: [...], c: ... }]
    // We extract that object and parse it via the existing path parser. This
    // is what the starfish wink (animated mask path inside the eye precomp)
    // depends on.
    Keyframe {
        time: src.time,
        value: src.start_path().and_then(parse_path_value),
        end_value: src.end_path().and_then(parse_path_value),
        easing_in: src.in_tangent.as_ref().map(lower_easing),
        easing_out: src.out_tangent.as_ref().map(lower_easing),
        spatial_in: None,
        spatial_out: None,
        hold: false,
    }
}

fn parse_path_value(v: &serde_json::Value) -> Option<PathData> {
    parse_path_opt(Some(v))
}

fn lower_easing(src: &lottie::keyframes::EasingHandle) -> EasingHandle {
    EasingHandle {
        x: lower_easing_value(&src.x),
        y: lower_easing_value(&src.y),
    }
}

fn lower_easing_value(src: &lottie::keyframes::EasingValue) -> EasingValue {
    match src {
        lottie::keyframes::EasingValue::Scalar(s) => EasingValue::Scalar(*s),
        lottie::keyframes::EasingValue::PerComponent(v) => EasingValue::PerComponent(v.clone()),
    }
}

// ---------------------------------------------------------------------------
// Value parsers — Lottie stores all keyframe values as Vec<f64>; we widen.
// ---------------------------------------------------------------------------

fn parse_scalar(arr: &[f64]) -> Option<Scalar> {
    arr.first().copied()
}

fn parse_scalar_opt(v: Option<&serde_json::Value>) -> Option<Scalar> {
    let v = v?;
    if let Some(n) = v.as_f64() {
        Some(n)
    } else if let Some(arr) = v.as_array() {
        arr.first().and_then(|x| x.as_f64())
    } else {
        None
    }
}

fn parse_vec2(arr: &[f64]) -> Option<Vec2> {
    if arr.len() < 2 {
        None
    } else {
        Some([arr[0], arr[1]])
    }
}

fn parse_vec2_opt(v: Option<&serde_json::Value>) -> Option<Vec2> {
    let arr = v?.as_array()?;
    if arr.len() < 2 {
        return None;
    }
    Some([arr[0].as_f64()?, arr[1].as_f64()?])
}

fn parse_vec3(arr: &[f64]) -> Option<Vec3> {
    match arr.len() {
        2 => Some([arr[0], arr[1], 0.0]),
        n if n >= 3 => Some([arr[0], arr[1], arr[2]]),
        _ => None,
    }
}

fn parse_vec3_opt(v: Option<&serde_json::Value>) -> Option<Vec3> {
    let arr = v?.as_array()?;
    match arr.len() {
        2 => Some([arr[0].as_f64()?, arr[1].as_f64()?, 0.0]),
        n if n >= 3 => Some([arr[0].as_f64()?, arr[1].as_f64()?, arr[2].as_f64()?]),
        _ => None,
    }
}

fn parse_color(arr: &[f64]) -> Option<Color> {
    match arr.len() {
        3 => Some([arr[0], arr[1], arr[2], 1.0]),
        n if n >= 4 => Some([arr[0], arr[1], arr[2], arr[3]]),
        _ => None,
    }
}

fn parse_color_opt(v: Option<&serde_json::Value>) -> Option<Color> {
    let arr = v?.as_array()?;
    match arr.len() {
        3 => Some([arr[0].as_f64()?, arr[1].as_f64()?, arr[2].as_f64()?, 1.0]),
        n if n >= 4 => Some([
            arr[0].as_f64()?,
            arr[1].as_f64()?,
            arr[2].as_f64()?,
            arr[3].as_f64()?,
        ]),
        _ => None,
    }
}

fn parse_path_opt(v: Option<&serde_json::Value>) -> Option<PathData> {
    let v = v?;
    let obj = v.as_object()?;
    let read_pts = |key: &str| -> Vec<Vec2> {
        obj.get(key)
            .and_then(|x| x.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|pt| {
                        let p = pt.as_array()?;
                        Some([p.first()?.as_f64()?, p.get(1)?.as_f64()?])
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    Some(PathData {
        vertices: read_pts("v"),
        in_tangents: read_pts("i"),
        out_tangents: read_pts("o"),
        closed: obj.get("c").and_then(|c| c.as_bool()).unwrap_or(false),
    })
}


// ---------------------------------------------------------------------------
// Shape lowering
// ---------------------------------------------------------------------------

fn lower_shapes(module: &mut Module, shapes: &[GraphicElement]) -> Result<Vec<ShapeNode>> {
    let mut out = Vec::with_capacity(shapes.len());
    for s in shapes {
        if let Some(node) = lower_shape(module, s)? {
            out.push(node);
        }
    }
    Ok(out)
}

fn lower_shape(module: &mut Module, src: &GraphicElement) -> Result<Option<ShapeNode>> {
    Ok(Some(match src {
        GraphicElement::Group { name, match_name, it, hidden, .. } => ShapeNode::Group {
            name: name.clone(),
            match_name: match_name.clone(),
            items: lower_shapes(module, it)?,
            hidden: *hidden,
        },
        GraphicElement::Path { name, ks, hidden, d } => ShapeNode::Path {
            name: name.clone(),
            ks: lower_prop_path(module, ks)?,
            direction: ShapeDirection::from_lottie(*d),
            hidden: *hidden,
        },
        GraphicElement::Ellipse { name, s, p, hidden, d } => ShapeNode::Ellipse {
            name: name.clone(),
            size: lower_prop_vec2(module, s, [0.0, 0.0])?,
            position: lower_prop_vec2(module, p, [0.0, 0.0])?,
            direction: ShapeDirection::from_lottie(*d),
            hidden: *hidden,
        },
        GraphicElement::Rectangle { name, s, p, r, hidden, d } => ShapeNode::Rectangle {
            name: name.clone(),
            size: lower_prop_vec2(module, s, [0.0, 0.0])?,
            position: lower_prop_vec2(module, p, [0.0, 0.0])?,
            radius: lower_prop_scalar(module, r, 0.0)?,
            direction: ShapeDirection::from_lottie(*d),
            hidden: *hidden,
        },
        GraphicElement::PolyStar {
            name, sy, pt, p, outer_radius, ir, os, inner_roundness, r, hidden, d,
        } => ShapeNode::PolyStar {
            name: name.clone(),
            kind: match sy.unwrap_or(1) {
                2 => PolyStarKind::Polygon,
                _ => PolyStarKind::Star,
            },
            points: lower_prop_scalar(module, pt, 5.0)?,
            position: lower_prop_vec2(module, p, [0.0, 0.0])?,
            rotation: lower_prop_scalar(module, r, 0.0)?,
            outer_radius: lower_prop_scalar(module, outer_radius, 50.0)?,
            inner_radius: match ir {
                Some(p) => Some(lower_prop_scalar(module, p, 25.0)?),
                None => None,
            },
            outer_roundness: match os {
                Some(p) => Some(lower_prop_scalar(module, p, 0.0)?),
                None => None,
            },
            inner_roundness: match inner_roundness {
                Some(p) => Some(lower_prop_scalar(module, p, 0.0)?),
                None => None,
            },
            direction: ShapeDirection::from_lottie(*d),
            hidden: *hidden,
        },
        GraphicElement::Transform {
            name, p, a, s, r, o, sk, sa, hidden,
        } => {
            let transform = Transform {
                anchor: lower_prop_vec3(module, a, [0.0, 0.0, 0.0])?,
                position: lower_prop_vec3(module, p, [0.0, 0.0, 0.0])?,
                scale: lower_prop_vec3(module, s, [100.0, 100.0, 100.0])?,
                rotation: lower_prop_scalar(module, r, 0.0)?,
                opacity: lower_prop_scalar(module, o, 100.0)?,
                skew: match sk {
                    Some(p) => Some(lower_prop_scalar(module, p, 0.0)?),
                    None => None,
                },
                skew_axis: match sa {
                    Some(p) => Some(lower_prop_scalar(module, p, 0.0)?),
                    None => None,
                },
            };
            ShapeNode::Transform { name: name.clone(), transform, hidden: *hidden }
        }
        GraphicElement::Fill { name, match_name, c, o, hidden, .. } => ShapeNode::Fill {
            name: name.clone(),
            match_name: match_name.clone(),
            color: lower_prop_color(module, c, [0.0, 0.0, 0.0, 1.0])?,
            opacity: lower_prop_scalar(module, o, 100.0)?,
            rule: FillRule::NonZero,
            hidden: *hidden,
        },
        GraphicElement::Stroke {
            name, match_name, c, o, w, lc, lj, ml, hidden, ..
        } => ShapeNode::Stroke {
            name: name.clone(),
            match_name: match_name.clone(),
            color: lower_prop_color(module, c, [0.0, 0.0, 0.0, 1.0])?,
            opacity: lower_prop_scalar(module, o, 100.0)?,
            width: lower_prop_scalar(module, w, 1.0)?,
            linecap: LineCap::from_lottie(*lc),
            linejoin: LineJoin::from_lottie(*lj),
            miter_limit: *ml,
            hidden: *hidden,
        },
        GraphicElement::GradientStroke {
            name, g, w, o, s, e, t, lc, lj, ml, hidden,
        } => ShapeNode::GradientStroke {
            name: name.clone(),
            gradient: GradientDef { raw: g.clone() },
            width: lower_prop_scalar(module, w, 1.0)?,
            opacity: lower_prop_scalar(module, o, 100.0)?,
            start: match s {
                Some(p) => Some(lower_prop_vec2(module, p, [0.0, 0.0])?),
                None => None,
            },
            end: match e {
                Some(p) => Some(lower_prop_vec2(module, p, [0.0, 0.0])?),
                None => None,
            },
            kind: match t.unwrap_or(1) {
                2 => GradientKind::Radial,
                _ => GradientKind::Linear,
            },
            linecap: LineCap::from_lottie(*lc),
            linejoin: LineJoin::from_lottie(*lj),
            miter_limit: *ml,
            hidden: *hidden,
        },
        GraphicElement::TrimPath { name, s, e, o, m, hidden } => ShapeNode::TrimPath {
            name: name.clone(),
            start: lower_prop_scalar(module, s, 0.0)?,
            end: lower_prop_scalar(module, e, 100.0)?,
            offset: lower_prop_scalar(module, o, 0.0)?,
            multiple_shapes: match m.unwrap_or(1) {
                2 => TrimMultipleShapes::Individually,
                _ => TrimMultipleShapes::Simultaneously,
            },
            hidden: *hidden,
        },
        GraphicElement::GradientFill {
            name, g, o, s, e, t, r, hidden,
        } => ShapeNode::GradientFill {
            name: name.clone(),
            gradient: GradientDef { raw: g.clone() },
            opacity: lower_prop_scalar(module, o, 100.0)?,
            start: match s {
                Some(p) => Some(lower_prop_vec2(module, p, [0.0, 0.0])?),
                None => None,
            },
            end: match e {
                Some(p) => Some(lower_prop_vec2(module, p, [0.0, 0.0])?),
                None => None,
            },
            kind: match t.unwrap_or(1) {
                2 => GradientKind::Radial,
                _ => GradientKind::Linear,
            },
            rule: match r.unwrap_or(1) {
                2 => FillRule::EvenOdd,
                _ => FillRule::NonZero,
            },
            hidden: *hidden,
        },
        GraphicElement::Unknown => return Ok(None),
    }))
}

// ---------------------------------------------------------------------------
// Effects
// ---------------------------------------------------------------------------

fn lower_effects(module: &mut Module, ef: Option<&serde_json::Value>) -> Result<Vec<Effect>> {
    let Some(arr) = ef.and_then(|v| v.as_array()) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(arr.len());
    for ef_val in arr {
        let Some(ef_obj) = ef_val.as_object() else { continue };
        let name = ef_obj.get("nm").and_then(|v| v.as_str()).map(String::from);
        let match_name = ef_obj.get("mn").and_then(|v| v.as_str()).map(String::from);
        let ty = ef_obj.get("ty").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let index = ef_obj.get("ix").and_then(|v| v.as_u64()).map(|n| n as u32);
        let enabled = ef_obj.get("en").and_then(|v| v.as_u64()).map(|n| n != 0).unwrap_or(true);
        let parameters = ef_obj
            .get("ef")
            .and_then(|v| v.as_array())
            .map(|params| {
                params
                    .iter()
                    .filter_map(|p| lower_effect_param(module, p).transpose())
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();
        out.push(Effect {
            name,
            match_name,
            ty,
            index,
            enabled,
            parameters,
        });
    }
    Ok(out)
}

fn lower_effect_param(module: &mut Module, p: &serde_json::Value) -> Result<Option<EffectParam>> {
    let Some(obj) = p.as_object() else { return Ok(None) };
    let name = obj.get("nm").and_then(|v| v.as_str()).map(String::from);
    let match_name = obj.get("mn").and_then(|v| v.as_str()).map(String::from);
    let ty = obj.get("ty").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let index = obj.get("ix").and_then(|v| v.as_u64()).map(|n| n as u32);
    let value = match obj.get("v") {
        Some(v_obj) => {
            // Try to interpret as a scalar Property; otherwise stash raw.
            if let Ok(prop) = serde_json::from_value::<AstProperty>(v_obj.clone()) {
                EffectValue::Scalar(lower_prop_scalar(module, &prop, 0.0)?)
            } else {
                EffectValue::Other(v_obj.clone())
            }
        }
        None => EffectValue::Other(serde_json::Value::Null),
    };
    Ok(Some(EffectParam {
        name,
        match_name,
        ty,
        index,
        value,
    }))
}

// ---------------------------------------------------------------------------
// Expression helpers
// ---------------------------------------------------------------------------

fn build_expression(body: String) -> Expression {
    let canonical = canonicalize_expression(&body);
    let mut hasher = DefaultHasher::new();
    canonical.hash(&mut hasher);
    let canonical_hash = hasher.finish();

    let uses_value = body.contains("value");
    let uses_this_property = body.contains("thisProperty");
    let uses_loop_out = body.contains("loopOut");

    Expression {
        // Filled by ExprTable::insert. Sentinel value here.
        id: ExprId(u32::MAX),
        body,
        canonical_hash,
        used_apis: ApiSet::empty(),
        uses_value,
        uses_this_property,
        uses_loop_out,
        references_layers: Vec::new(),
        references_effects: Vec::new(),
    }
}

/// Strip insignificant whitespace so that two textually-identical expressions
/// hash to the same value regardless of indentation jitter.
fn canonicalize_expression(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut last_was_space = false;
    for c in body.chars() {
        if c.is_whitespace() {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
        } else {
            out.push(c);
            last_was_space = false;
        }
    }
    out
}
