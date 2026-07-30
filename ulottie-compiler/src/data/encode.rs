//! Lower an `ir::Module` to a wire-format `Payload` with inlined properties.
//!
//! No property table — each property value is placed directly at its
//! reference site. Static values are literals; animated values carry their
//! keyframes inline; expressions carry their ref + fallback.

use anyhow::Result;

use crate::data::*;
use crate::ir;

pub fn can_encode(m: &ir::Module) -> bool {
    for asset in &m.assets {
        if let ir::AssetKind::Precomp { layers } = &asset.kind {
            for layer in layers {
                if !layer_supported(layer) {
                    return false;
                }
            }
        }
    }
    for layer in &m.layers {
        if !layer_supported(layer) {
            return false;
        }
    }
    true
}

fn layer_supported(layer: &ir::Layer) -> bool {
    let supported = match &layer.kind {
        ir::LayerKind::Shape { shapes } => shapes_supported(shapes),
        ir::LayerKind::Null
        | ir::LayerKind::Solid { .. }
        | ir::LayerKind::Precomp { .. }
        | ir::LayerKind::Image { .. } => true,
        ir::LayerKind::Other { .. } => false,
    };
    if !supported && std::env::var("ULOTTIE_DEBUG_BACKEND").is_ok() {
        eprintln!(
            "data-backend bail: layer ind={} ty={:?}",
            layer.index,
            std::mem::discriminant(&layer.kind)
        );
    }
    supported
}

fn shapes_supported(shapes: &[ir::ShapeNode]) -> bool {
    for s in shapes {
        match s {
            ir::ShapeNode::Group { items, .. } => {
                if !shapes_supported(items) {
                    return false;
                }
            }
            _ => {}
        }
    }
    true
}

/// Whether a group transform does nothing, so the group itself can go.
///
/// Every component has to be **statically** identity. An animated one is
/// identity only at whatever frames it passes through, and reading it as its
/// default — which is what `unwrap_or` did on a `None` from `static_*` — quietly
/// deleted the group and the animation with it. `lf30_t0igqye8` rotates a
/// banknote about its own anchor: position equals anchor, scale is 100, opacity
/// is 100, and the one thing that moves is the rotation, which read as 0. The
/// group was flattened and the notes never turned.
fn transform_is_identity(t: &ir::Transform) -> bool {
    let (Some(position), Some(anchor)) = (static_vec3(&t.position), static_vec3(&t.anchor)) else {
        return false;
    };
    if (position[0] - anchor[0]).abs() > 1e-3
        || (position[1] - anchor[1]).abs() > 1e-3
        || (position[2] - anchor[2]).abs() > 1e-3
    {
        return false;
    }
    let Some(scale) = static_vec3(&t.scale) else {
        return false;
    };
    if (scale[0] - 100.0).abs() > 1e-3 || (scale[1] - 100.0).abs() > 1e-3 {
        return false;
    }
    let Some(rotation) = static_scalar(&t.rotation) else {
        return false;
    };
    if rotation.abs() > 1e-3 {
        return false;
    }
    let Some(opacity) = static_scalar(&t.opacity) else {
        return false;
    };
    if (opacity - 100.0).abs() > 1e-3 {
        return false;
    }
    let skew_ok = t
        .skew
        .as_ref()
        .is_none_or(|p| static_scalar(p) == Some(0.0));
    let skew_axis_ok = t
        .skew_axis
        .as_ref()
        .is_none_or(|p| static_scalar(p) == Some(0.0));
    skew_ok && skew_axis_ok
}

/// An effect parameter's value when it is a static colour: `{"a":0,"k":[r,g,b,a]}`.
/// Anything animated returns `None` and the parameter stays out of the payload,
/// which is what keeps the planner from freezing a colour that moves.
fn static_color(raw: &serde_json::Value) -> Option<[f64; 4]> {
    let obj = raw.as_object()?;
    if obj.get("a").and_then(|a| a.as_f64()).unwrap_or(0.0) != 0.0 {
        return None;
    }
    let k = obj.get("k")?.as_array()?;
    if k.len() < 3 {
        return None;
    }
    Some([
        k[0].as_f64()?,
        k[1].as_f64()?,
        k[2].as_f64()?,
        k.get(3).and_then(|v| v.as_f64()).unwrap_or(1.0),
    ])
}

/// The style list one item sees: its own group's, then everything inherited.
///
/// Both halves are in `it` order, and the group's own come first because a
/// style declared closer to the geometry paints over one declared further out —
/// lottie-web appends the inner group's element after the outer style's.
fn styles_at(own: &[u32], inherited: &[u32]) -> Vec<u32> {
    let mut v = Vec::with_capacity(own.len() + inherited.len());
    v.extend_from_slice(own);
    v.extend_from_slice(inherited);
    v
}

fn static_scalar(p: &ir::Property<f64>) -> Option<f64> {
    match p {
        ir::Property::Static(v) => Some(*v),
        _ => None,
    }
}
fn static_vec3(p: &ir::Property<ir::Vec3>) -> Option<ir::Vec3> {
    match p {
        ir::Property::Static(v) => Some(*v),
        _ => None,
    }
}

// ---------------------------------------------------------------------------

pub fn encode(m: &ir::Module) -> Result<Payload> {
    let mut enc = Encoder::new();
    enc.payload.c = Composition {
        w: m.composition.width,
        h: m.composition.height,
        fr: m.composition.frame_rate,
        ip: m.composition.in_point,
        op: m.composition.out_point,
        ddd: if m.composition.is_3d { 1 } else { 0 },
    };

    if !m.assets.is_empty() {
        let mut assets = std::collections::BTreeMap::new();
        for asset in &m.assets {
            match &asset.kind {
                ir::AssetKind::Precomp { layers } => {
                    let mut inner = Vec::with_capacity(layers.len());
                    for l in layers {
                        inner.push(enc.encode_layer(l)?);
                    }
                    assets.insert(asset.id.clone(), Asset::Precomp { l: inner });
                }
                ir::AssetKind::Image {
                    path,
                    filename,
                    width,
                    height,
                    embedded,
                } => {
                    assets.insert(
                        asset.id.clone(),
                        Asset::Image {
                            u: path.clone(),
                            p: filename.clone(),
                            w: *width,
                            h: *height,
                            e: if *embedded { 1 } else { 0 },
                        },
                    );
                }
            }
        }
        if !assets.is_empty() {
            enc.payload.a = Some(assets);
        }
    }

    for layer in &m.layers {
        let l = enc.encode_layer(layer)?;
        enc.payload.l.push(l);
    }
    Ok(enc.payload)
}

struct Encoder {
    payload: Payload,
    str_cache: std::collections::HashMap<String, u32>,
}

impl Encoder {
    fn new() -> Self {
        Self {
            payload: Payload::default(),
            str_cache: std::collections::HashMap::new(),
        }
    }

    fn intern_string(&mut self, s: &str) -> u32 {
        if let Some(&id) = self.str_cache.get(s) {
            return id;
        }
        let id = self.payload.st.len() as u32;
        self.payload.st.push(s.to_string());
        self.str_cache.insert(s.to_string(), id);
        id
    }

    fn encode_layer(&mut self, layer: &ir::Layer) -> Result<Layer> {
        let mut out = Layer {
            i: layer.index,
            ty: layer_ty_num(&layer.kind),
            ip: layer.in_point,
            op: layer.out_point,
            sr: layer.stretch,
            st: if layer.start_time == 0.0 {
                None
            } else {
                Some(layer.start_time)
            },
            p: Some(self.inline_vec3(&layer.transform.position)?),
            a: Some(self.inline_vec3(&layer.transform.anchor)?),
            sc: Some(self.inline_vec3(&layer.transform.scale)?),
            r: Some(self.inline_scalar(&layer.transform.rotation)?),
            o: Some(self.inline_scalar(&layer.transform.opacity)?),
            ..Default::default()
        };

        if let Some(tr) = &layer.time_remap {
            out.tr = Some(self.inline_scalar(tr)?);
        }
        out.tt = layer.track_matte;
        out.tp = layer.matte_parent.map(|id| id.0);
        if layer.matte_layer_for_above {
            out.td = Some(1);
        }
        if let Some(parent) = layer.parent {
            out.pr = Some(parent.0);
        }
        if let Some(name) = &layer.name {
            out.n = Some(self.intern_string(name));
        }

        if !layer.masks.is_empty() {
            let mut masks = Vec::with_capacity(layer.masks.len());
            for m in &layer.masks {
                let mode = match m.mode {
                    ir::MaskMode::Add => "a",
                    ir::MaskMode::Subtract => "s",
                    ir::MaskMode::Other => "a",
                };
                let pt = self.inline_path(&m.shape)?;
                let o = match &m.opacity {
                    Some(prop) => Some(self.inline_scalar(prop)?),
                    None => None,
                };
                masks.push(LayerMask {
                    m: mode.to_string(),
                    inv: m.inverted,
                    pt,
                    o,
                });
            }
            out.mk = Some(masks);
        }

        if !layer.effects.is_empty() {
            let mut effects = Vec::with_capacity(layer.effects.len());
            for e in &layer.effects {
                let mut params = Vec::with_capacity(e.parameters.len());
                for p in &e.parameters {
                    let (v, pid, c) = match &p.value {
                        ir::EffectValue::Scalar(ir::Property::Static(s)) => (Some(*s), None, None),
                        ir::EffectValue::Scalar(prop) => {
                            (None, Some(self.inline_scalar(prop)?), None)
                        }
                        // A colour is a vector, so it never parsed as a scalar
                        // property and used to be dropped here. `ADBE Fill`
                        // *is* its colour, so the static case comes through.
                        ir::EffectValue::Other(raw) => match static_color(raw) {
                            Some(c) => (None, None, Some(c)),
                            None => continue,
                        },
                    };
                    params.push(EffectParam {
                        nm: p.name.clone(),
                        mn: p.match_name.clone(),
                        ty: p.ty,
                        v,
                        p: pid,
                        c,
                    });
                }
                effects.push(Effect {
                    nm: e.name.clone(),
                    mn: e.match_name.clone(),
                    ty: e.ty,
                    ef: params,
                });
            }
            if !effects.is_empty() {
                out.ef = Some(effects);
            }
        }

        match &layer.kind {
            ir::LayerKind::Shape { shapes } => {
                let mut shape_refs = Vec::new();
                self.encode_shape_tree(shapes, &mut shape_refs)?;
                if !shape_refs.is_empty() {
                    out.shapes = Some(shape_refs);
                }
            }
            ir::LayerKind::Solid {
                color,
                width,
                height,
            } => {
                out.cl = Some(color.clone());
                out.sw = Some(*width);
                out.sh = Some(*height);
            }
            ir::LayerKind::Precomp {
                asset,
                width,
                height,
            } => {
                out.rf = Some(asset.clone());
                if *width != 0 {
                    out.sw = Some(*width);
                }
                if *height != 0 {
                    out.sh = Some(*height);
                }
            }
            // An image layer is a reference and nothing else: its size comes
            // from the asset, not the layer.
            ir::LayerKind::Image { asset } => {
                out.rf = Some(asset.clone());
            }
            _ => {}
        }

        Ok(out)
    }

    fn encode_shape_tree(
        &mut self,
        shapes: &[ir::ShapeNode],
        out: &mut Vec<ShapeRef>,
    ) -> Result<()> {
        self.encode_shape_tree_with(shapes, out, &[], None)
    }

    /// Encode one group's item list.
    ///
    /// **The walk runs backwards, and that is the whole contract.** Lottie
    /// authors `it` top-first, and lottie-web's `searchShapes` iterates it from
    /// the end appending as it goes — so `it[0]` is appended last and paints on
    /// top. Two rules fall out of that one, and both come out wrong if the list
    /// is read forwards:
    ///
    /// * geometry is emitted in reverse `it` order;
    /// * a fill, stroke or trim reaches only the items *above* it, which are
    ///   exactly the ones this walk has not passed yet.
    ///
    /// The style list itself is kept in `it` order, the group's own ahead of
    /// anything inherited, because `emit_styles` resolves an attribute to the
    /// first style carrying it and the topmost is the one that shows.
    fn encode_shape_tree_with(
        &mut self,
        shapes: &[ir::ShapeNode],
        out: &mut Vec<ShapeRef>,
        inherited_styles: &[u32],
        inherited_trim: Option<u32>,
    ) -> Result<()> {
        // The transform applies to the group as a whole wherever it sits in the
        // list, so it is the one item that has to be known before the walk.
        let group_transform = shapes.iter().rev().find_map(|s| match s {
            ir::ShapeNode::Transform { transform, .. } => Some(transform),
            _ => None,
        });
        let emit_into_group = group_transform.is_some_and(|t| !transform_is_identity(t));
        let mut emitted: Vec<ShapeRef> = Vec::new();
        let target = if emit_into_group {
            &mut emitted
        } else {
            &mut *out
        };

        let mut own_styles: Vec<u32> = Vec::new();
        let mut current_trim: Option<u32> = inherited_trim;

        for s in shapes.iter().rev() {
            match s {
                ir::ShapeNode::Fill { color, opacity, .. } => {
                    let c = self.inline_color(color)?;
                    let o = self.inline_scalar(opacity)?;
                    let id = self.payload.y.len() as u32;
                    self.payload.y.push(Style::Fill { c, o });
                    own_styles.insert(0, id);
                }
                ir::ShapeNode::Stroke {
                    color,
                    opacity,
                    width,
                    linecap,
                    linejoin,
                    miter_limit,
                    ..
                } => {
                    let c = self.inline_color(color)?;
                    let o = self.inline_scalar(opacity)?;
                    let w = self.inline_scalar(width)?;
                    let id = self.payload.y.len() as u32;
                    self.payload.y.push(Style::Stroke {
                        c,
                        o,
                        w,
                        lc: linecap_num(*linecap),
                        lj: linejoin_num(*linejoin),
                        ml: *miter_limit,
                    });
                    own_styles.insert(0, id);
                }
                ir::ShapeNode::GradientStroke {
                    gradient,
                    width,
                    opacity,
                    start,
                    end,
                    kind,
                    linecap,
                    linejoin,
                    miter_limit,
                    ..
                } => {
                    let w = self.inline_scalar(width)?;
                    let o = self.inline_scalar(opacity)?;
                    let s = match start {
                        Some(p) => Some(self.inline_vec2(p)?),
                        None => None,
                    };
                    let e = match end {
                        Some(p) => Some(self.inline_vec2(p)?),
                        None => None,
                    };
                    let id = self.payload.y.len() as u32;
                    self.payload.y.push(Style::GradientStroke {
                        g: gradient.raw.clone().unwrap_or(serde_json::Value::Null),
                        w,
                        o,
                        s,
                        e,
                        gk: match kind {
                            ir::GradientKind::Linear => 1,
                            ir::GradientKind::Radial => 2,
                        },
                        lc: linecap_num(*linecap),
                        lj: linejoin_num(*linejoin),
                        ml: *miter_limit,
                    });
                    own_styles.insert(0, id);
                }
                ir::ShapeNode::GradientFill {
                    gradient,
                    opacity,
                    start,
                    end,
                    kind,
                    rule,
                    ..
                } => {
                    let o = self.inline_scalar(opacity)?;
                    let s = match start {
                        Some(p) => Some(self.inline_vec2(p)?),
                        None => None,
                    };
                    let e = match end {
                        Some(p) => Some(self.inline_vec2(p)?),
                        None => None,
                    };
                    let id = self.payload.y.len() as u32;
                    self.payload.y.push(Style::GradientFill {
                        g: gradient.raw.clone().unwrap_or(serde_json::Value::Null),
                        o,
                        s,
                        e,
                        gk: match kind {
                            ir::GradientKind::Linear => 1,
                            ir::GradientKind::Radial => 2,
                        },
                        fr: match rule {
                            ir::FillRule::NonZero => 1,
                            ir::FillRule::EvenOdd => 2,
                        },
                    });
                    own_styles.insert(0, id);
                }
                ir::ShapeNode::TrimPath {
                    start,
                    end,
                    offset,
                    multiple_shapes,
                    ..
                } => {
                    let s = self.inline_scalar(start)?;
                    let e = self.inline_scalar(end)?;
                    let o = self.inline_scalar(offset)?;
                    let id = self.payload.y.len() as u32;
                    self.payload.y.push(Style::TrimPath {
                        s,
                        e,
                        o,
                        m: match multiple_shapes {
                            ir::TrimMultipleShapes::Simultaneously => 1,
                            ir::TrimMultipleShapes::Individually => 2,
                        },
                    });
                    current_trim = Some(id);
                }
                ir::ShapeNode::Group { items, .. } => {
                    let styles = styles_at(&own_styles, inherited_styles);
                    self.encode_shape_tree_with(items, target, &styles, current_trim)?;
                }
                ir::ShapeNode::Rectangle {
                    size,
                    position,
                    radius,
                    ..
                } => {
                    let sz = self.inline_vec2(size)?;
                    let ps = self.inline_vec2(position)?;
                    let rd = self.inline_scalar(radius)?;
                    let sid = self.payload.s.len() as u32;
                    self.payload.s.push(Shape::Rect {
                        sz,
                        ps,
                        rd,
                        nm: None,
                    });
                    target.push(ShapeRef::Prim(PrimRef {
                        s: sid,
                        y: styles_at(&own_styles, inherited_styles),
                        tm: current_trim,
                    }));
                }
                ir::ShapeNode::Ellipse { size, position, .. } => {
                    let sz = self.inline_vec2(size)?;
                    let ps = self.inline_vec2(position)?;
                    let sid = self.payload.s.len() as u32;
                    self.payload.s.push(Shape::Ellipse { sz, ps, nm: None });
                    target.push(ShapeRef::Prim(PrimRef {
                        s: sid,
                        y: styles_at(&own_styles, inherited_styles),
                        tm: current_trim,
                    }));
                }
                ir::ShapeNode::Path { ks, .. } => {
                    let pt = self.inline_path(ks)?;
                    let sid = self.payload.s.len() as u32;
                    self.payload.s.push(Shape::Path { pt, nm: None });
                    target.push(ShapeRef::Prim(PrimRef {
                        s: sid,
                        y: styles_at(&own_styles, inherited_styles),
                        tm: current_trim,
                    }));
                }
                ir::ShapeNode::PolyStar {
                    kind,
                    points,
                    position,
                    rotation,
                    outer_radius,
                    inner_radius,
                    outer_roundness,
                    inner_roundness,
                    ..
                } => {
                    let pt = self.inline_scalar(points)?;
                    let ps = self.inline_vec2(position)?;
                    let rt = self.inline_scalar(rotation)?;
                    let or = self.inline_scalar(outer_radius)?;
                    let ir = match inner_radius {
                        Some(p) => self.inline_scalar(p)?,
                        None => self.inline_scalar(&ir::Property::Static(0.0))?,
                    };
                    let os = match outer_roundness {
                        Some(p) => Some(self.inline_scalar(p)?),
                        None => None,
                    };
                    let is = match inner_roundness {
                        Some(p) => Some(self.inline_scalar(p)?),
                        None => None,
                    };
                    let sid = self.payload.s.len() as u32;
                    self.payload.s.push(Shape::PolyStar {
                        sy: match kind {
                            ir::PolyStarKind::Star => 1,
                            ir::PolyStarKind::Polygon => 2,
                        },
                        pt,
                        ps,
                        or,
                        ir,
                        rt,
                        os,
                        is,
                        nm: None,
                    });
                    target.push(ShapeRef::Prim(PrimRef {
                        s: sid,
                        y: styles_at(&own_styles, inherited_styles),
                        tm: current_trim,
                    }));
                }
                _ => {}
            }
        }

        if emit_into_group {
            let tr = group_transform.unwrap();
            let group = GroupRef {
                c: emitted,
                p: Some(self.inline_vec3(&tr.position)?),
                a: Some(self.inline_vec3(&tr.anchor)?),
                sc: Some(self.inline_vec3(&tr.scale)?),
                r: Some(self.inline_scalar(&tr.rotation)?),
                o: Some(self.inline_scalar(&tr.opacity)?),
            };
            out.push(ShapeRef::Group(group));
        }
        Ok(())
    }

    // -- Inline property converters -----------------------------------------

    fn inline_scalar(&self, p: &ir::Property<f64>) -> Result<InlineProp> {
        Ok(match p {
            ir::Property::Static(v) => InlineProp::Static(Value::Scalar(*v)),
            ir::Property::Animated(kf) => InlineProp::Animated(encode_keyframes_scalar(kf)),
            ir::Property::Expression { fallback, expr } => {
                let (fb, kf) = match fallback {
                    ir::ValueSource::Static(v) => (Some(Value::Scalar(*v)), None),
                    ir::ValueSource::Animated(kf) => (None, Some(encode_keyframes_scalar(kf))),
                };
                InlineProp::Expression(ExprProp { e: expr.0, fb, kf })
            }
        })
    }

    fn inline_vec2(&self, p: &ir::Property<ir::Vec2>) -> Result<InlineProp> {
        Ok(match p {
            ir::Property::Static(v) => InlineProp::Static(Value::Vector(v.to_vec())),
            ir::Property::Animated(kf) => InlineProp::Animated(encode_keyframes_vec(kf, 2)),
            ir::Property::Expression { fallback, expr } => {
                let (fb, kf) = match fallback {
                    ir::ValueSource::Static(v) => (Some(Value::Vector(v.to_vec())), None),
                    ir::ValueSource::Animated(kf) => (None, Some(encode_keyframes_vec(kf, 2))),
                };
                InlineProp::Expression(ExprProp { e: expr.0, fb, kf })
            }
        })
    }

    fn inline_vec3(&self, p: &ir::Property<ir::Vec3>) -> Result<InlineProp> {
        Ok(match p {
            ir::Property::Static(v) => InlineProp::Static(Value::Vector(v.to_vec())),
            ir::Property::Animated(kf) => InlineProp::Animated(encode_keyframes_vec(kf, 3)),
            ir::Property::Expression { fallback, expr } => {
                let (fb, kf) = match fallback {
                    ir::ValueSource::Static(v) => (Some(Value::Vector(v.to_vec())), None),
                    ir::ValueSource::Animated(kf) => (None, Some(encode_keyframes_vec(kf, 3))),
                };
                InlineProp::Expression(ExprProp { e: expr.0, fb, kf })
            }
        })
    }

    fn inline_path(&self, p: &ir::Property<ir::PathData>) -> Result<InlineProp> {
        Ok(match p {
            ir::Property::Static(pd) => InlineProp::Static(Value::Path(path_to_wire(pd))),
            ir::Property::Animated(kf) => InlineProp::Animated(encode_keyframes_path(kf)),
            ir::Property::Expression { fallback, expr } => {
                let fb = match fallback {
                    ir::ValueSource::Static(pd) => Some(Value::Path(path_to_wire(pd))),
                    ir::ValueSource::Animated(_) => None,
                };
                InlineProp::Expression(ExprProp {
                    e: expr.0,
                    fb,
                    kf: None,
                })
            }
        })
    }

    fn inline_color(&self, p: &ir::Property<ir::Color>) -> Result<InlineProp> {
        Ok(match p {
            ir::Property::Static(v) => InlineProp::Static(Value::Vector(v.to_vec())),
            ir::Property::Animated(kf) => InlineProp::Animated(encode_keyframes_vec(kf, 4)),
            ir::Property::Expression { fallback, expr } => {
                let (fb, kf) = match fallback {
                    ir::ValueSource::Static(v) => (Some(Value::Vector(v.to_vec())), None),
                    ir::ValueSource::Animated(kf) => (None, Some(encode_keyframes_vec(kf, 4))),
                };
                InlineProp::Expression(ExprProp { e: expr.0, fb, kf })
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Keyframe encoding
// ---------------------------------------------------------------------------

fn encode_keyframes_scalar(kf: &ir::Keyframes<f64>) -> Keyframes {
    let mut times = Vec::with_capacity(kf.frames.len());
    let mut values = Vec::with_capacity(kf.frames.len());
    let mut ends: Vec<Option<Value>> = Vec::with_capacity(kf.frames.len());
    let mut oi_list: Vec<EasingPair> = Vec::with_capacity(kf.frames.len());
    let mut any_end = false;
    let mut any_easing = false;
    let mut holds: Vec<bool> = Vec::with_capacity(kf.frames.len());
    let mut any_hold = false;

    for frame in &kf.frames {
        times.push(frame.time);
        holds.push(frame.hold);
        if frame.hold {
            any_hold = true;
        }
        // An absent value is written as the empty marker, not as zero. Lottie's
        // older keyframe form puts a segment's destination in `e` on the
        // *first* keyframe and leaves the last one a bare terminator with no
        // `s`; both readers resolve that by looking back at `e[i-1]`, but only
        // if they can tell "absent" from "zero". `unwrap_or(0.0)` could not, so
        // a two-keyframe legacy property looked constant and collapsed to a
        // static value — which is how `starfish`'s eye lost its blink: its time
        // remap ramps 0 → 2.333s entirely through `e`, and folded to a
        // motionless 0. The vector encoder below has always done this.
        values.push(match frame.value {
            Some(v) => Value::Scalar(v),
            None => Value::Vector(Vec::new()),
        });
        ends.push(frame.end_value.map(Value::Scalar));
        if frame.end_value.is_some() {
            any_end = true;
        }
        if let (Some(o), Some(i)) = (&frame.easing_out, &frame.easing_in) {
            oi_list.push(EasingPair {
                o: convert_easing(o),
                i: convert_easing(i),
            });
            any_easing = true;
        } else {
            oi_list.push(default_linear_easing());
        }
    }

    Keyframes {
        t: times,
        v: values,
        e: if any_end { Some(ends) } else { None },
        oi: if any_easing { Some(oi_list) } else { None },
        to: None,
        ti: None,
        h: if any_hold { Some(holds) } else { None },
    }
}

fn encode_keyframes_vec<const N: usize>(kf: &ir::Keyframes<[f64; N]>, _dim: usize) -> Keyframes {
    let mut times = Vec::with_capacity(kf.frames.len());
    let mut values = Vec::with_capacity(kf.frames.len());
    let mut ends: Vec<Option<Value>> = Vec::with_capacity(kf.frames.len());
    let mut oi_list: Vec<EasingPair> = Vec::with_capacity(kf.frames.len());
    let mut to_list: Vec<Vec<f64>> = Vec::with_capacity(kf.frames.len());
    let mut ti_list: Vec<Vec<f64>> = Vec::with_capacity(kf.frames.len());
    let mut any_end = false;
    let mut any_easing = false;
    let mut any_spatial = false;
    let mut holds: Vec<bool> = Vec::with_capacity(kf.frames.len());
    let mut any_hold = false;

    for frame in &kf.frames {
        times.push(frame.time);
        holds.push(frame.hold);
        if frame.hold {
            any_hold = true;
        }
        values.push(Value::Vector(
            frame.value.map(|v| v.to_vec()).unwrap_or_default(),
        ));
        ends.push(frame.end_value.map(|v| Value::Vector(v.to_vec())));
        if frame.end_value.is_some() {
            any_end = true;
        }
        if let (Some(o), Some(i)) = (&frame.easing_out, &frame.easing_in) {
            oi_list.push(EasingPair {
                o: convert_easing(o),
                i: convert_easing(i),
            });
            any_easing = true;
        } else {
            oi_list.push(default_linear_easing());
        }
        match (frame.spatial_out, frame.spatial_in) {
            (Some(o), Some(i)) => {
                to_list.push(o.to_vec());
                ti_list.push(i.to_vec());
                if o.iter().any(|&x| x != 0.0) || i.iter().any(|&x| x != 0.0) {
                    any_spatial = true;
                }
            }
            _ => {
                to_list.push(vec![0.0; N]);
                ti_list.push(vec![0.0; N]);
            }
        }
    }

    Keyframes {
        t: times,
        v: values,
        e: if any_end { Some(ends) } else { None },
        oi: if any_easing { Some(oi_list) } else { None },
        to: if any_spatial { Some(to_list) } else { None },
        ti: if any_spatial { Some(ti_list) } else { None },
        h: if any_hold { Some(holds) } else { None },
    }
}

fn encode_keyframes_path(kf: &ir::Keyframes<ir::PathData>) -> Keyframes {
    let mut times = Vec::with_capacity(kf.frames.len());
    let mut values = Vec::with_capacity(kf.frames.len());
    let mut ends: Vec<Option<Value>> = Vec::with_capacity(kf.frames.len());
    let mut oi_list: Vec<EasingPair> = Vec::with_capacity(kf.frames.len());
    let mut any_end = false;
    let mut any_easing = false;
    let mut holds: Vec<bool> = Vec::with_capacity(kf.frames.len());
    let mut any_hold = false;

    for frame in &kf.frames {
        times.push(frame.time);
        holds.push(frame.hold);
        if frame.hold {
            any_hold = true;
        }
        match &frame.value {
            Some(pd) => values.push(Value::Path(path_to_wire(pd))),
            None => values.push(Value::Vector(Vec::new())),
        }
        ends.push(
            frame
                .end_value
                .as_ref()
                .map(|pd| Value::Path(path_to_wire(pd))),
        );
        if frame.end_value.is_some() {
            any_end = true;
        }
        if let (Some(o), Some(i)) = (&frame.easing_out, &frame.easing_in) {
            oi_list.push(EasingPair {
                o: convert_easing(o),
                i: convert_easing(i),
            });
            any_easing = true;
        } else {
            oi_list.push(default_linear_easing());
        }
    }

    Keyframes {
        t: times,
        v: values,
        e: if any_end { Some(ends) } else { None },
        oi: if any_easing { Some(oi_list) } else { None },
        to: None,
        ti: None,
        h: if any_hold { Some(holds) } else { None },
    }
}

fn convert_easing(h: &ir::EasingHandle) -> EasingHandle {
    EasingHandle {
        x: convert_easing_component(&h.x),
        y: convert_easing_component(&h.y),
    }
}

fn convert_easing_component(c: &ir::EasingValue) -> EasingComponent {
    match c {
        ir::EasingValue::Scalar(s) => EasingComponent::Scalar(*s),
        ir::EasingValue::PerComponent(v) => EasingComponent::PerComponent(v.clone()),
    }
}

fn default_linear_easing() -> EasingPair {
    EasingPair {
        o: EasingHandle {
            x: EasingComponent::Scalar(0.0),
            y: EasingComponent::Scalar(0.0),
        },
        i: EasingHandle {
            x: EasingComponent::Scalar(1.0),
            y: EasingComponent::Scalar(1.0),
        },
    }
}

fn layer_ty_num(kind: &ir::LayerKind) -> u32 {
    match kind {
        ir::LayerKind::Precomp { .. } => 0,
        ir::LayerKind::Solid { .. } => 1,
        ir::LayerKind::Image { .. } => 2,
        ir::LayerKind::Null => 3,
        ir::LayerKind::Shape { .. } => 4,
        ir::LayerKind::Other { ty } => *ty,
    }
}

fn linecap_num(c: ir::LineCap) -> u8 {
    match c {
        ir::LineCap::Butt => 1,
        ir::LineCap::Round => 2,
        ir::LineCap::Square => 3,
    }
}

fn linejoin_num(j: ir::LineJoin) -> u8 {
    match j {
        ir::LineJoin::Miter => 1,
        ir::LineJoin::Round => 2,
        ir::LineJoin::Bevel => 3,
    }
}

fn path_to_wire(pd: &ir::PathData) -> PathValue {
    PathValue {
        v: pd.vertices.iter().map(|p| [p[0], p[1]]).collect(),
        i: pd.in_tangents.iter().map(|p| [p[0], p[1]]).collect(),
        o: pd.out_tangents.iter().map(|p| [p[0], p[1]]).collect(),
        c: pd.closed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(time: f64, value: Option<f64>, end: Option<f64>) -> ir::Keyframe<f64> {
        ir::Keyframe {
            time,
            value,
            end_value: end,
            easing_in: None,
            easing_out: None,
            spatial_in: None,
            spatial_out: None,
            hold: false,
        }
    }

    /// Lottie's older keyframe form puts a segment's destination in `e` on the
    /// *first* keyframe and leaves the last one a bare terminator with no `s`.
    /// Both readers resolve that by looking back at `e[i-1]` — but only if the
    /// encoder distinguishes "absent" from "zero".
    ///
    /// Writing the terminator as a literal `0` made such a property look
    /// constant, so `classify_keyframes` collapsed it to a static value. That is
    /// how `starfish`'s eye lost its blink: its time remap ramps 0 → 2.333s
    /// entirely through `e`, folded to a motionless 0, and the eyelid mask sat
    /// on its first keyframe forever.
    #[test]
    fn a_keyframe_with_no_start_value_encodes_as_absent_not_zero() {
        let kf = ir::Keyframes {
            frames: vec![frame(0.0, Some(0.0), Some(2.333)), frame(140.0, None, None)],
        };
        let out = encode_keyframes_scalar(&kf);
        assert_eq!(out.v[0], Value::Scalar(0.0), "a real value stays a scalar");
        assert!(
            matches!(&out.v[1], Value::Vector(x) if x.is_empty()),
            "the terminator must carry the empty marker, not a zero: {:?}",
            out.v[1]
        );
        // …and the destination it stands for is still on the wire.
        assert_eq!(
            out.e.as_ref().and_then(|e| e[0].clone()),
            Some(Value::Scalar(2.333))
        );
    }

    // -- the shape walk ------------------------------------------------------

    fn path_named(nm: &str) -> ir::ShapeNode {
        ir::ShapeNode::Path {
            name: Some(nm.into()),
            ks: ir::Property::Static(ir::PathData::default()),
            direction: ir::ShapeDirection::Normal,
            hidden: false,
        }
    }

    fn fill(r: f64) -> ir::ShapeNode {
        ir::ShapeNode::Fill {
            name: None,
            match_name: None,
            color: ir::Property::Static([r, 0.0, 0.0, 1.0]),
            opacity: ir::Property::Static(100.0),
            rule: ir::FillRule::NonZero,
            hidden: false,
        }
    }

    fn group(items: Vec<ir::ShapeNode>) -> ir::ShapeNode {
        ir::ShapeNode::Group {
            name: None,
            match_name: None,
            items,
            hidden: false,
        }
    }

    /// The red channel of each emitted primitive's first style, in the order
    /// they were emitted. Enough to tell both the paint order and which style
    /// reached which shape.
    fn walk(shapes: &[ir::ShapeNode]) -> Vec<f64> {
        let mut enc = Encoder::new();
        let mut out = Vec::new();
        enc.encode_shape_tree(shapes, &mut out).unwrap();
        out.iter()
            .map(|r| match r {
                ShapeRef::Prim(p) => match p.y.first().and_then(|id| enc.payload.y.get(*id as usize))
                {
                    Some(Style::Fill { c: InlineProp::Static(Value::Vector(v)), .. }) => v[0],
                    _ => -1.0,
                },
                ShapeRef::Group(_) => -2.0,
            })
            .collect()
    }

    /// Lottie authors `it` top-first and lottie-web appends it backwards, so
    /// `it[0]` paints last. Reading the list forwards drew every overlapping
    /// group in the wrong order — `lf20_hewaysm0` is two groups, and the white
    /// heart came out underneath the red disc that should sit behind it.
    #[test]
    fn group_items_paint_in_reverse_it_order() {
        let shapes = vec![group(vec![
            group(vec![path_named("first"), fill(0.1)]),
            group(vec![path_named("second"), fill(0.2)]),
        ])];
        assert_eq!(walk(&shapes), vec![0.2, 0.1]);
    }

    /// A fill applies to the paths *above* it and to nothing below, because
    /// that is where the backwards walk has already been. Collecting every
    /// style in the group first and handing all of them to every shape painted
    /// the lower shapes with a fill After Effects never applied to them.
    #[test]
    fn a_fill_reaches_only_the_items_above_it() {
        let shapes = vec![group(vec![
            path_named("top"),
            fill(0.1),
            path_named("bottom"),
            fill(0.2),
        ])];
        // Emitted bottom-first: `bottom` sees only the fill below it, `top`
        // sees both and takes the nearer one.
        assert_eq!(walk(&shapes), vec![0.2, 0.1]);
    }

    /// An inner group's own fill outranks one inherited from further out: its
    /// element is appended after the outer style's, so it paints on top.
    #[test]
    fn an_inner_fill_outranks_an_inherited_one() {
        let shapes = vec![group(vec![group(vec![path_named("p"), fill(0.1)]), fill(0.2)])];
        assert_eq!(walk(&shapes), vec![0.1]);
    }

    /// Two contours over the same paint are one path, so the fill rule can
    /// cancel the inner one. Emitted as two elements they are two solid shapes
    /// and the counter fills in — which is what `krrt-7ccaf912`'s wordmark did
    /// to every Korean letter in it.
    ///
    /// The merge is `Planner::merge_paths`; this drives the whole compiler so
    /// what is asserted is the emitted document, not an intermediate.
    #[test]
    fn contours_sharing_a_paint_become_one_path() {
        let doc = crate::compile_document(&ring("sh")).unwrap();
        assert_eq!(
            doc.matches("<path").count(),
            1,
            "the two contours share a fill and must share an element:\n{doc}"
        );
        // Both are still in it — merging is not dropping.
        let d = doc.split("d=\"").nth(1).and_then(|s| s.split('"').next());
        assert_eq!(
            d.map(|d| d.matches('M').count()),
            Some(2),
            "…and both contours are in it:\n{doc}"
        );
    }

    /// Where the merge stops, written down rather than discovered.
    ///
    /// A static `rc` or `el` is emitted as a native `<rect>`/`<ellipse>`, which
    /// is smaller and rounder than the path it would otherwise be — and cannot
    /// join a `d`. lottie-web turns every primitive into path data, so a hole
    /// authored as two rectangles *would* cancel there and does not here. No
    /// file in three corpora draws one that way (a counter is a `sh`), so the
    /// native elements are kept; merging them means giving up the element and
    /// getting the Lottie winding right on a generated outline.
    #[test]
    fn native_rect_and_ellipse_do_not_merge() {
        let doc = crate::compile_document(&ring("rc")).unwrap();
        assert_eq!(
            doc.matches("<rect").count(),
            2,
            "known limit — if this ever merges, the comment above is stale:\n{doc}"
        );
    }

    /// A 100×100 square with a 40×40 square inside it and one fill between
    /// them: a frame, not two blocks. `kind` is `sh` or `rc`.
    fn ring(kind: &str) -> String {
        let shape = |w: f64| match kind {
            "rc" => format!(
                r#"{{"ty":"rc","s":{{"a":0,"k":[{w},{w}]}},"p":{{"a":0,"k":[100,100]}},"r":{{"a":0,"k":0}}}}"#
            ),
            _ => {
                let (lo, hi) = (100.0 - w / 2.0, 100.0 + w / 2.0);
                format!(
                    r#"{{"ty":"sh","ks":{{"a":0,"k":{{"c":true,
                       "v":[[{lo},{lo}],[{hi},{lo}],[{hi},{hi}],[{lo},{hi}]],
                       "i":[[0,0],[0,0],[0,0],[0,0]],"o":[[0,0],[0,0],[0,0],[0,0]]}}}}}}"#
                )
            }
        };
        format!(
            r#"{{"v":"5.7.0","fr":30,"ip":0,"op":30,"w":200,"h":200,"assets":[],"layers":[
              {{"ind":1,"ty":4,"nm":"ring","sr":1,
               "ks":{{"o":{{"a":0,"k":100}},"r":{{"a":0,"k":0}},"p":{{"a":0,"k":[0,0,0]}},
                     "a":{{"a":0,"k":[0,0,0]}},"s":{{"a":0,"k":[100,100,100]}}}},
               "ao":0,"ip":0,"op":30,"st":0,"bm":0,
               "shapes":[{{"ty":"gr","it":[
                 {},
                 {},
                 {{"ty":"fl","c":{{"a":0,"k":[1,0,0,1]}},"o":{{"a":0,"k":100}}}},
                 {{"ty":"tr","p":{{"a":0,"k":[0,0]}},"a":{{"a":0,"k":[0,0]}},
                   "s":{{"a":0,"k":[100,100]}},"r":{{"a":0,"k":0}},"o":{{"a":0,"k":100}}}}
               ]}}]}}
            ]}}"#,
            shape(100.0),
            shape(40.0)
        )
    }

    /// A group transform is only identity when every component is *statically*
    /// identity. Reading an animated one as its default deleted the group and
    /// the animation with it — `lf30_t0igqye8` rotates a banknote about its own
    /// anchor, so position, anchor, scale and opacity all looked like nothing
    /// and the rotation was the only thing that moved.
    #[test]
    fn an_animated_transform_is_never_identity() {
        let animated = ir::Transform {
            anchor: ir::Property::Static([0.0, 0.0, 0.0]),
            position: ir::Property::Static([0.0, 0.0, 0.0]),
            scale: ir::Property::Static([100.0, 100.0, 100.0]),
            rotation: ir::Property::Animated(ir::Keyframes {
                frames: vec![frame(0.0, Some(-17.0), None), frame(62.0, Some(0.0), None)],
            }),
            opacity: ir::Property::Static(100.0),
            skew: None,
            skew_axis: None,
        };
        assert!(!transform_is_identity(&animated));

        let mut still = animated.clone();
        still.rotation = ir::Property::Static(0.0);
        assert!(transform_is_identity(&still));
    }
}
