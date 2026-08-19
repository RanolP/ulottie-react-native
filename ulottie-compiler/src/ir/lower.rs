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

use crate::lottie::property::Property as AstProperty;
use crate::lottie::{self, Animation, GraphicElement, Keyframe as AstKeyframe};

use super::types::*;

// ---------------------------------------------------------------------------
// Public entry
// ---------------------------------------------------------------------------

pub fn lower(anim: &Animation) -> Result<Module> {
    // Files older than 4.1.9 write shape colours 0–255; lottie-web's
    // `checkColors` rescales them at load, so the reference render is 0–1.
    // Normalize before anything else sees a value (`bodymoovin`'s bg at
    // `v: 3.1.6` painted white instead of green until this existed).
    let anim = &rescale_legacy_colors(anim);
    let composition = Composition {
        name: anim.name.clone(),
        width: anim.width,
        height: anim.height,
        frame_rate: anim.frame_rate,
        // lottie-web maps player frame 0 to `Math.round(ip)`
        // (`this.firstFrame`), so a fractional in-point starts the clock just
        // *before* a layer authored to begin exactly at it — `loading_indicator`
        // opens on a blank frame there. Rounding here counts frames the same
        // way; layer in/out points stay authored, which is what makes the
        // first-frame gating agree.
        in_point: anim.in_point.round(),
        out_point: anim.out_point,
        is_3d: anim.ddd.unwrap_or(0) != 0,
    };

    let mut module = Module::new(composition);

    // Glyph outlines and font metadata are document-wide (lottie-web's font
    // manager is global), so every composition — root and assets alike —
    // lowers text against the same tables.
    let glyph_table = (
        anim.chars.clone(),
        anim.fonts.as_ref().map(|f| f.list.clone()).unwrap_or_default(),
    );

    // Assign LayerIds in source order; build an `ind -> LayerId` lookup so we
    // can resolve `parent` references inside the same composition. (Precomps
    // have their own layer space, handled separately when lowering an asset.)
    let mut ctx = LowerContext {
        glyphs: glyph_table.clone(),
        ..Default::default()
    };
    let layers = lower_layers(&mut module, &mut ctx, &anim.layers)?;
    module.layers = layers;

    // Lower assets after top-level layers so that their internal layer ids
    // don't collide with the top-level mapping.
    for asset in &anim.assets {
        let kind = if let Some(asset_layers) = &asset.layers {
            let mut sub_ctx = LowerContext {
                glyphs: glyph_table.clone(),
                ..Default::default()
            };
            let inner = lower_layers(&mut module, &mut sub_ctx, asset_layers)?;
            AssetKind::Precomp { layers: inner }
        } else {
            AssetKind::Image {
                path: asset.path.clone(),
                filename: asset.filename.clone(),
                width: asset.w.unwrap_or(0.0),
                height: asset.h.unwrap_or(0.0),
                embedded: asset.e.unwrap_or(0) != 0,
            }
        };
        module.assets.push(Asset {
            id: asset.id.clone(),
            name: asset.name.clone(),
            kind,
        });
    }

    // Fold the expressions before anything downstream sees them. It runs here
    // rather than at each entry point so the module, the document and the
    // reference renderer cannot disagree about which expressions still exist.
    crate::expr::fold_module(&mut module);

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
    /// Document-wide glyph outlines and fonts, for text layers.
    glyphs: (Vec<lottie::GlyphChar>, Vec<lottie::Font>),
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
    let parent = src.parent.and_then(|ind| ctx.ind_to_id.get(&ind).copied());

    let kind = match src.ty {
        0 => LayerKind::Precomp {
            asset: src.ref_id.clone().unwrap_or_default(),
            width: src.width.unwrap_or(0.0),
            height: src.height.unwrap_or(0.0),
        },
        1 => LayerKind::Solid {
            color: src.sc.clone().unwrap_or_else(|| "#000000".to_string()),
            width: src.sw.unwrap_or(0.0),
            height: src.sh.unwrap_or(0.0),
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
        // A text layer lowers to the shapes its glyphs lay out to. The
        // support scan reaches the same verdict through the same call, so an
        // unsupported text layer only reaches here when the degradation was
        // explicitly allowed — and "the text is not drawn" is that
        // degradation.
        5 => {
            let shapes = src.t.as_ref()
                .and_then(|t| lottie::text_shapes(t, &ctx.glyphs.0, &ctx.glyphs.1).ok())
                .unwrap_or_default();
            LayerKind::Shape {
                shapes: lower_shapes(module, &shapes)?,
            }
        }
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
        // Absent bounds mean "the whole composition" — a layer that can never
        // be range-hidden. The sentinels stay finite so they survive the
        // wire's ×1000 integer quantization if they ever reach it.
        in_point: src.ip.unwrap_or(-1e6),
        out_point: src.op.unwrap_or(1e6),
        stretch: src.sr.unwrap_or(1.0),
        start_time: src.st.unwrap_or(0.0),
        time_remap: match &src.tm {
            Some(tm) => Some(lower_prop_scalar(module, tm, 0.0)?),
            None => None,
        },
        is_3d: src.ddd.unwrap_or(0) != 0,
        auto_orient: src.ao.unwrap_or(0) != 0,
        // The Lottie spec uses `hd: 1` to mean "hidden". On layers the field
        // isn't on our AST; treat as not-hidden by default.
        hidden: false,
        blend_mode: src.bm.unwrap_or(0),
        track_matte: src.tt,
        matte_parent: src.tp.and_then(|ind| ctx.ind_to_id.get(&ind).copied()),
        matte_layer_for_above: src.td.unwrap_or(0) != 0,
        has_mask: src.has_mask.unwrap_or(false),
        masks,
    })
}

fn lower_masks(
    module: &mut Module,
    props: Option<&Vec<lottie::MaskProperty>>,
) -> Result<Vec<LayerMask>> {
    let Some(props) = props else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(props.len());
    for m in props {
        let mode = match m.mode.as_str() {
            "a" => MaskMode::Add,
            "s" => MaskMode::Subtract,
            "i" => MaskMode::Intersect,
            "n" => MaskMode::None,
            _ => MaskMode::Other,
        };
        let shape = lower_prop_path(module, &m.pt, m.cl)?;
        let opacity = match &m.o {
            Some(p) => Some(lower_prop_scalar(module, p, 100.0)?),
            None => None,
        };
        out.push(LayerMask {
            mode,
            inverted: m.inv,
            shape,
            opacity,
        });
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

fn lower_prop_vec2(module: &mut Module, p: &AstProperty, default: Vec2) -> Result<Property<Vec2>> {
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

fn lower_prop_vec3(module: &mut Module, p: &AstProperty, default: Vec3) -> Result<Property<Vec3>> {
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
        z_prop
            .as_ref()
            .map(static_scalar)
            .unwrap_or(Some(default[2])),
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

    // The merged property carries one easing per segment, because that is what
    // the wire and the runtime can express — so the axes' handles can only be
    // kept when they agree. One axis moving is the usual shape of a separated
    // position and the case that is exactly representable: the merged timeline
    // *is* that axis's, handles and holds included. Where several axes move to
    // different curves the merge samples them instead, which is why
    // `interpolate_scalar` has to ease.
    let axes: Vec<&Property<Scalar>> = std::iter::once(&x_prop)
        .chain(std::iter::once(&y_prop))
        .chain(z_prop.iter())
        .filter(|p| static_scalar(p).is_none())
        .collect();
    let shape = axes.first().and_then(|p| frames_of(p));
    let uniform = shape.is_some_and(|f| axes.iter().all(|p| frames_of(p).is_some_and(|g| same_curve(f, g))));
    let source = if uniform { shape } else { None };

    let sampled: Vec<f64> = if uniform {
        times.clone()
    } else {
        // Linear segments between the union's times cannot follow an eased
        // curve, so subdivide them by whole frames — the resolution the
        // animation is authored and played at.
        subdivide(&times)
    };

    let frames: Vec<Keyframe<Vec3>> = sampled
        .iter()
        .map(|&t| {
            let x = eval_scalar_at(&x_prop, t, default[0]);
            let y = eval_scalar_at(&y_prop, t, default[1]);
            let z = z_prop
                .as_ref()
                .map(|p| eval_scalar_at(p, t, default[2]))
                .unwrap_or(default[2]);
            let at = source.and_then(|f| f.frames.iter().find(|k| k.time == t));
            Keyframe {
                time: t,
                value: Some([x, y, z]),
                easing_in: at.and_then(|k| k.easing_in.clone()),
                easing_out: at.and_then(|k| k.easing_out.clone()),
                spatial_in: None,
                spatial_out: None,
                hold: at.is_some_and(|k| k.hold),
            }
        })
        .collect();

    Ok(Property::Animated(Keyframes { frames }))
}

/// Files older than 4.1.9 write shape colours 0–255; newer ones 0–1.
/// Returns true when the document is in the old spelling. This is
/// lottie-web's `checkColors` threshold — `[4, 1, 9]` compared
/// major/minor/patch — *not* "before 4.9": `simple_loader` at `v: 4.6.3`
/// already writes 0–1 and would render black if divided again.
fn uses_legacy_colors(version: &str) -> bool {
    let mut parts = version.split('.');
    let num = |p: Option<&str>| -> Option<u32> {
        p?.trim_end_matches(|c: char| !c.is_ascii_digit())
            .parse()
            .ok()
    };
    let (major, minor, patch) = (
        num(parts.next()),
        num(parts.next()),
        num(parts.next()),
    );
    match (major, minor) {
        // A missing minor cannot be below 1.
        (Some(m), Some(n)) => m < 4 || (m == 4 && n < 1) || (m == 4 && n == 1 && patch.unwrap_or(0) < 9),
        (Some(m), None) => m < 4,
        // Unparseable: assume modern, the safer reading for a value that is
        // already 0–1 in most exports.
        _ => false,
    }
}

/// Rescale 0–255 colours to 0–1 across every shape layer of the document —
/// lottie-web's `checkColors`, run at the parse boundary so no later stage
/// can see the wrong scale. Animated keyframes carry their values in `s`/`e`,
/// which the legacy-`e` normalization has already folded into `s` by now.
///
/// The **alpha channel is pinned to 1**, not divided: lottie-web's own pass
/// divides all four components but only ever reads three — its styles take
/// opacity from the separate `o` property — while this compiler folds colour
/// alpha into paint alpha. `Tests_Rect9` strokes with `[0,0,0,1]` in 0–255
/// spelling would otherwise paint at 1/255 opacity.
fn rescale_legacy_colors(anim: &Animation) -> Animation {
    if !uses_legacy_colors(&anim.version) {
        return anim.clone();
    }
    let mut anim = anim.clone();
    let mut layers = std::mem::take(&mut anim.layers);
    for l in &mut layers {
        rescale_layer_colors(l);
    }
    anim.layers = layers;
    for a in &mut anim.assets {
        if let Some(layers) = &mut a.layers {
            for l in layers {
                rescale_layer_colors(l);
            }
        }
    }
    anim
}

fn rescale_layer_colors(layer: &mut lottie::Layer) {
    let Some(shapes) = &mut layer.shapes else {
        return;
    };
    rescale_shape_colors(shapes);
}

fn rescale_shape_colors(shapes: &mut [GraphicElement]) {
    for s in shapes.iter_mut() {
        match s {
            GraphicElement::Fill { c, .. } | GraphicElement::Stroke { c, .. } => {
                rescale_color_property(c);
            }
            GraphicElement::Group { it, .. } => rescale_shape_colors(it),
            _ => {}
        }
    }
}

/// A Lottie (AST) property: static value or keyframes.
fn rescale_color_property(p: &mut lottie::Property) {
    match p {
        lottie::Property::Static(s) => {
            if let Some(arr) = s.value.as_array_mut() {
                for (i, v) in arr.iter_mut().enumerate() {
                    if let Some(n) = v.as_f64() {
                        // Alpha pinned: see `rescale_legacy_colors`.
                        *v = if i < 3 {
                            serde_json::Value::from(n / 255.0)
                        } else {
                            serde_json::Value::from(1.0)
                        };
                    }
                }
            }
        }
        lottie::Property::Animated(a) => {
            for kf in a.keyframes.iter_mut() {
                rescale_color_kf(&mut kf.start_value);
                rescale_color_kf(&mut kf.end_value);
            }
        }
        _ => {}
    }
}

fn rescale_color_kf(v: &mut Option<serde_json::Value>) {
    if let Some(serde_json::Value::Array(arr)) = v {
        for (i, n) in arr.iter_mut().enumerate() {
            if let Some(x) = n.as_f64() {
                *n = if i < 3 {
                    serde_json::Value::from(x / 255.0)
                } else {
                    serde_json::Value::from(1.0)
                };
            }
        }
    }
}

/// The keyframes behind a scalar property, whether or not an expression wraps
/// it.
fn frames_of(p: &Property<Scalar>) -> Option<&Keyframes<Scalar>> {
    match p {
        Property::Animated(kf)
        | Property::Expression {
            fallback: ValueSource::Animated(kf),
            ..
        } => Some(kf),
        _ => None,
    }
}

/// Do two axes move on the same timeline, with the same handles and holds?
fn same_curve(a: &Keyframes<Scalar>, b: &Keyframes<Scalar>) -> bool {
    a.frames.len() == b.frames.len()
        && a.frames.iter().zip(&b.frames).all(|(p, q)| {
            p.time == q.time
                && p.hold == q.hold
                && p.easing_in == q.easing_in
                && p.easing_out == q.easing_out
        })
}

/// Whole frames between each pair of times, plus the times themselves.
fn subdivide(times: &[f64]) -> Vec<f64> {
    let mut out = Vec::with_capacity(times.len());
    for w in times.windows(2) {
        out.push(w[0]);
        let mut t = w[0].floor() + 1.0;
        while t < w[1] {
            if t > w[0] {
                out.push(t);
            }
            t += 1.0;
        }
    }
    if let Some(&last) = times.last() {
        out.push(last);
    }
    out
}

fn project_vec3_to_vec2(prop: Property<Vec3>) -> Property<Vec2> {
    // `spatial_in` / `spatial_out` are typed `Option<Vec3>` regardless of the
    // outer keyframe value, so we keep them as-is and only project the value
    // down to 2D.
    match prop {
        Property::Static(v) => Property::Static([v[0], v[1]]),
        Property::Animated(kf) => Property::Animated(Keyframes {
            frames: kf.frames.into_iter().map(project_kf_vec3_to_vec2).collect(),
        }),
        Property::Expression { fallback, expr } => Property::Expression {
            fallback: match fallback {
                ValueSource::Static(v) => ValueSource::Static([v[0], v[1]]),
                ValueSource::Animated(kf) => ValueSource::Animated(Keyframes {
                    frames: kf.frames.into_iter().map(project_kf_vec3_to_vec2).collect(),
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
        Property::Expression {
            fallback: ValueSource::Animated(kf),
            ..
        } => kf.frames.iter().map(|f| f.time).collect(),
        _ => Vec::new(),
    }
}

fn eval_scalar_at(p: &Property<Scalar>, t: f64, fallback: Scalar) -> Scalar {
    match p {
        Property::Static(v) => *v,
        Property::Animated(kf) => interpolate_scalar(kf, t, fallback),
        Property::Expression {
            fallback: ValueSource::Static(v),
            ..
        } => *v,
        Property::Expression {
            fallback: ValueSource::Animated(kf),
            ..
        } => interpolate_scalar(kf, t, fallback),
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
        return last.value.unwrap_or(fallback);
    }
    for i in 0..frames.len() - 1 {
        let a = &frames[i];
        let b = &frames[i + 1];
        if t >= a.time && t <= b.time {
            let dt = b.time - a.time;
            if dt == 0.0 {
                return b.value.unwrap_or(fallback);
            }
            let av = a.value.unwrap_or(fallback);
            if a.hold {
                return av;
            }
            let mut u = (t - a.time) / dt;
            // Both handles sit on the keyframe the segment *starts* at — `o`
            // leaving it and `i` arriving at the next — which is the pairing
            // `encode_keyframes_*` reads and the one Lottie writes. Sampling
            // without them reads an eased move as a linear one, which is how
            // `car-8`'s rows slid into place at the wrong times and `car-13`
            // scrolled 40px ahead of itself.
            if let (Some(o), Some(inn)) = (a.easing_out.as_ref(), a.easing_in.as_ref()) {
                let (x1, y1) = handle(o);
                let (x2, y2) = handle(inn);
                u = crate::eval::keyframes::cubic_bezier(u, x1, y1, x2, y2);
            }
            let bv = b.value.unwrap_or(fallback);
            return av + (bv - av) * u;
        }
    }
    fallback
}

/// An easing handle's `(x, y)`. A per-component handle on a scalar axis is a
/// one-element list, so the first component is the whole of it.
fn handle(h: &EasingHandle) -> (f64, f64) {
    let pick = |v: &EasingValue| match v {
        EasingValue::Scalar(n) => *n,
        EasingValue::PerComponent(c) => c.first().copied().unwrap_or(0.0),
    };
    (pick(&h.x), pick(&h.y))
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

/// `legacy_closed` is the pre-4.4.18 element-level closed flag; it applies
/// only where a path value carries no `c` of its own, exactly the way
/// lottie-web's `checkShapes` migration writes it in.
fn lower_prop_path(
    module: &mut Module,
    p: &AstProperty,
    legacy_closed: Option<bool>,
) -> Result<Property<PathData>> {
    let value_source = if p.is_animated() {
        ValueSource::Animated(Keyframes {
            frames: p
                .keyframes()
                .unwrap_or(&[])
                .iter()
                .map(|kf| lower_kf_path(kf, legacy_closed))
                .collect(),
        })
    } else {
        ValueSource::Static(
            parse_path_with(p.static_value(), legacy_closed).unwrap_or_default(),
        )
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
        let id = module
            .expressions
            .insert(build_expression(expr_body.to_string()));
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

fn lower_kf<T: Clone>(src: &AstKeyframe, parse: fn(&[f64]) -> Option<T>) -> Keyframe<T> {
    Keyframe {
        time: src.time,
        value: src.start_numbers().as_deref().and_then(parse),
        easing_in: src.in_tangent.as_ref().map(lower_easing),
        easing_out: src.out_tangent.as_ref().map(lower_easing),
        spatial_in: src.spatial_tangent_in.as_deref().and_then(parse_vec3),
        spatial_out: src.spatial_tangent_to.as_deref().and_then(parse_vec3),
        hold: src.hold == Some(1),
    }
}

fn lower_kf_path(src: &AstKeyframe, legacy_closed: Option<bool>) -> Keyframe<PathData> {
    // Path keyframes wrap the bezier shape in a single-element array, e.g.
    //   s: [{ v: [...], i: [...], o: [...], c: ... }]
    // We extract that object and parse it via the existing path parser. This
    // is what the starfish wink (animated mask path inside the eye precomp)
    // depends on.
    Keyframe {
        time: src.time,
        value: src
            .start_path()
            .and_then(|v| parse_path_with(Some(v), legacy_closed)),
        easing_in: src.in_tangent.as_ref().map(lower_easing),
        easing_out: src.out_tangent.as_ref().map(lower_easing),
        spatial_in: None,
        spatial_out: None,
        hold: src.hold == Some(1),
    }
}

fn parse_path_with(
    v: Option<&serde_json::Value>,
    legacy_closed: Option<bool>,
) -> Option<PathData> {
    let mut path = parse_path_opt(v)?;
    if let Some(c) = legacy_closed
        && !v
            .and_then(|x| x.as_object())
            .is_some_and(|o| o.contains_key("c"))
    {
        path.closed = c;
    }
    Some(path)
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
    // A static repeater expands before anything else sees the list — the
    // copies it produces are ordinary groups. A non-static one reaches
    // `lower_shape` as `Unknown` and drops, which only happens under an
    // explicit allowance (the scan refuses it).
    if let Some(at) = shapes
        .iter()
        .position(|s| matches!(s, GraphicElement::Repeater { .. }))
        && let Some(expanded) = lottie::repeat::expand(shapes, at)
    {
        return lower_shapes(module, &expanded);
    }
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
        GraphicElement::Group {
            name,
            match_name,
            it,
            hidden,
            ..
        } => ShapeNode::Group {
            name: name.clone(),
            match_name: match_name.clone(),
            items: lower_shapes(module, it)?,
            hidden: *hidden,
        },
        GraphicElement::Path {
            name,
            ks,
            hidden,
            d,
            closed,
        } => ShapeNode::Path {
            name: name.clone(),
            ks: lower_prop_path(module, ks, *closed)?,
            direction: ShapeDirection::from_lottie(*d),
            hidden: *hidden,
        },
        GraphicElement::Ellipse {
            name,
            s,
            p,
            hidden,
            d,
        } => ShapeNode::Ellipse {
            name: name.clone(),
            size: lower_prop_vec2(module, s, [0.0, 0.0])?,
            position: lower_prop_vec2(module, p, [0.0, 0.0])?,
            direction: ShapeDirection::from_lottie(*d),
            hidden: *hidden,
        },
        GraphicElement::Rectangle {
            name,
            s,
            p,
            r,
            hidden,
            d,
        } => ShapeNode::Rectangle {
            name: name.clone(),
            size: lower_prop_vec2(module, s, [0.0, 0.0])?,
            position: lower_prop_vec2(module, p, [0.0, 0.0])?,
            radius: lower_prop_scalar(module, r, 0.0)?,
            // lottie-web's rect tests `d === 1 || d === 2` and reverses
            // otherwise — so an *absent* `d` runs counter-clockwise, unlike
            // the ellipse and star, which reverse only on `d === 3`.
            direction: match d {
                Some(1) | Some(2) => ShapeDirection::Normal,
                _ => ShapeDirection::Reversed,
            },
            hidden: *hidden,
        },
        GraphicElement::PolyStar {
            name,
            sy,
            pt,
            p,
            outer_radius,
            ir,
            os,
            inner_roundness,
            r,
            hidden,
            d,
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
            name,
            p,
            a,
            s,
            r,
            o,
            sk,
            sa,
            hidden,
        } => {
            // Old bodymovin omits fields at their default, so each lowers to
            // its AE default when absent.
            let transform = Transform {
                anchor: match a {
                    Some(a) => lower_prop_vec3(module, a, [0.0, 0.0, 0.0])?,
                    None => Property::Static([0.0, 0.0, 0.0]),
                },
                position: match p {
                    Some(p) => lower_prop_vec3(module, p, [0.0, 0.0, 0.0])?,
                    None => Property::Static([0.0, 0.0, 0.0]),
                },
                scale: match s {
                    Some(s) => lower_prop_vec3(module, s, [100.0, 100.0, 100.0])?,
                    None => Property::Static([100.0, 100.0, 100.0]),
                },
                rotation: match r {
                    Some(r) => lower_prop_scalar(module, r, 0.0)?,
                    None => Property::Static(0.0),
                },
                opacity: match o {
                    Some(o) => lower_prop_scalar(module, o, 100.0)?,
                    None => Property::Static(100.0),
                },
                skew: match sk {
                    Some(p) => Some(lower_prop_scalar(module, p, 0.0)?),
                    None => None,
                },
                skew_axis: match sa {
                    Some(p) => Some(lower_prop_scalar(module, p, 0.0)?),
                    None => None,
                },
            };
            ShapeNode::Transform {
                name: name.clone(),
                transform,
                hidden: *hidden,
            }
        }
        GraphicElement::Fill {
            name,
            match_name,
            c,
            o,
            r,
            hidden,
            ..
        } => ShapeNode::Fill {
            name: name.clone(),
            match_name: match_name.clone(),
            color: lower_prop_color(module, c, [0.0, 0.0, 0.0, 1.0])?,
            opacity: match o {
                Some(p) => lower_prop_scalar(module, p, 100.0)?,
                None => Property::Static(100.0),
            },
            rule: match r {
                Some(2) => FillRule::EvenOdd,
                _ => FillRule::NonZero,
            },
            hidden: *hidden,
        },
        GraphicElement::Stroke {
            name,
            match_name,
            c,
            o,
            w,
            lc,
            lj,
            ml,
            d,
            hidden,
            ..
        } => ShapeNode::Stroke {
            name: name.clone(),
            match_name: match_name.clone(),
            color: lower_prop_color(module, c, [0.0, 0.0, 0.0, 1.0])?,
            opacity: match o {
                Some(p) => lower_prop_scalar(module, p, 100.0)?,
                None => Property::Static(100.0),
            },
            width: match w {
                Some(p) => lower_prop_scalar(module, p, 1.0)?,
                None => Property::Static(1.0),
            },
            linecap: LineCap::from_lottie(*lc),
            linejoin: LineJoin::from_lottie(*lj),
            miter_limit: *ml,
            dash: lower_dash(module, d.as_deref())?,
            hidden: *hidden,
        },
        GraphicElement::GradientStroke {
            name,
            g,
            w,
            o,
            s,
            e,
            t,
            lc,
            lj,
            ml,
            d,
            hidden,
        } => ShapeNode::GradientStroke {
            name: name.clone(),
            gradient: GradientDef { raw: g.clone() },
            width: match w {
                Some(p) => lower_prop_scalar(module, p, 1.0)?,
                None => Property::Static(1.0),
            },
            opacity: match o {
                Some(p) => lower_prop_scalar(module, p, 100.0)?,
                None => Property::Static(100.0),
            },
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
            dash: lower_dash(module, d.as_deref())?,
            hidden: *hidden,
        },
        GraphicElement::TrimPath {
            name,
            s,
            e,
            o,
            m,
            hidden,
        } => ShapeNode::TrimPath {
            name: name.clone(),
            start: match s {
                Some(p) => lower_prop_scalar(module, p, 0.0)?,
                None => Property::Static(0.0),
            },
            end: match e {
                Some(p) => lower_prop_scalar(module, p, 100.0)?,
                None => Property::Static(100.0),
            },
            offset: match o {
                Some(p) => lower_prop_scalar(module, p, 0.0)?,
                None => Property::Static(0.0),
            },
            multiple_shapes: match m.unwrap_or(1) {
                2 => TrimMultipleShapes::Individually,
                _ => TrimMultipleShapes::Simultaneously,
            },
            hidden: *hidden,
        },
        GraphicElement::GradientFill {
            name,
            g,
            o,
            s,
            e,
            t,
            r,
            hidden,
        } => ShapeNode::GradientFill {
            name: name.clone(),
            gradient: GradientDef { raw: g.clone() },
            opacity: match o {
                Some(p) => lower_prop_scalar(module, p, 100.0)?,
                None => Property::Static(100.0),
            },
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
        // Only reachable under an explicit allowance: the scan refuses a
        // repeater `expand` cannot take, and lowering drops it — the
        // documented "only the original copy is drawn" degradation.
        GraphicElement::Repeater { .. } => return Ok(None),
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
        let Some(ef_obj) = ef_val.as_object() else {
            continue;
        };
        let name = ef_obj.get("nm").and_then(|v| v.as_str()).map(String::from);
        let match_name = ef_obj.get("mn").and_then(|v| v.as_str()).map(String::from);
        let ty = ef_obj.get("ty").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let index = ef_obj.get("ix").and_then(|v| v.as_u64()).map(|n| n as u32);
        let enabled = ef_obj
            .get("en")
            .and_then(|v| v.as_u64())
            .map(|n| n != 0)
            .unwrap_or(true);
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

/// An effect parameter holding a colour, in After Effects' numbering.
const PARAM_COLOR: u32 = 2;

fn lower_effect_param(module: &mut Module, p: &serde_json::Value) -> Result<Option<EffectParam>> {
    let Some(obj) = p.as_object() else {
        return Ok(None);
    };
    let name = obj.get("nm").and_then(|v| v.as_str()).map(String::from);
    let match_name = obj.get("mn").and_then(|v| v.as_str()).map(String::from);
    let ty = obj.get("ty").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let index = obj.get("ix").and_then(|v| v.as_u64()).map(|n| n as u32);
    let value = match obj.get("v") {
        // A colour parameter is a vector, and `AstProperty` accepts it — so
        // reading it as a scalar succeeded and produced a number that meant
        // nothing. It has to take the raw path or `ADBE Fill` never sees the
        // colour that is the whole of the effect.
        Some(v_obj) if ty == PARAM_COLOR => EffectValue::Other(v_obj.clone()),
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

/// A stroke's dash pattern, in authored order. `n` distinguishes a length
/// (`d`/`g`) from the offset (`o`); lottie-web keeps that order when it joins
/// the lengths into `stroke-dasharray`.
fn lower_dash(
    module: &mut Module,
    d: Option<&[lottie::DashElement]>,
) -> Result<Vec<DashStop>> {
    let Some(items) = d else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(items.len());
    for el in items {
        out.push(DashStop {
            offset: el.n.as_deref() == Some("o"),
            value: lower_prop_scalar(module, &el.v, 0.0)?,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_colors_rescale_below_4_1_9_only() {
        assert!(uses_legacy_colors("3.1.6"));
        assert!(uses_legacy_colors("4.0.0"));
        assert!(uses_legacy_colors("4.1.8"));
        assert!(!uses_legacy_colors("4.1.9"));
        assert!(!uses_legacy_colors("4.6.3"));
        assert!(!uses_legacy_colors("5.5.7"));
        // Unparseable assumes modern.
        assert!(!uses_legacy_colors("latest"));
    }

    #[test]
    fn legacy_rescale_pins_alpha() {
        let anim: Animation = serde_json::from_str(
            r#"{"v":"3.1.6","fr":30,"ip":0,"op":10,"w":10,"h":10,"layers":[
                {"ty":4,"ind":1,"ip":0,"op":10,"st":0,"ks":{},
                 "shapes":[{"ty":"gr","it":[
                    {"ty":"fl","c":{"k":[88,214,112,255]},"o":{"k":100}},
                    {"ty":"tr","p":{"k":[0,0]},"a":{"k":[0,0]},"s":{"k":[100,100]},"r":{"k":0},"o":{"k":100}}
                 ]}]}]}"#,
        )
        .unwrap();
        let out = rescale_legacy_colors(&anim);
        let shapes = out.layers[0].shapes.as_deref().unwrap();
        let GraphicElement::Group { it, .. } = &shapes[0] else {
            panic!("group");
        };
        let fl = it.iter().find_map(|s| match s {
            GraphicElement::Fill { c, .. } => Some(c),
            _ => None,
        }).unwrap();
        let lottie::Property::Static(s) = fl else { panic!("static") };
        let arr = s.value.as_array().unwrap();
        assert!((arr[0].as_f64().unwrap() - 88.0 / 255.0).abs() < 1e-12);
        assert!((arr[1].as_f64().unwrap() - 214.0 / 255.0).abs() < 1e-12);
        // Alpha pinned to 1: lottie-web divides all four but only reads
        // three, and this compiler folds colour alpha into paint alpha.
        assert_eq!(arr[3].as_f64().unwrap(), 1.0);
    }
}
