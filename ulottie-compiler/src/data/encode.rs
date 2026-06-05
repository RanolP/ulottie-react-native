//! Lower an `ir::Module` to a wire-format `Payload`.
//!
//! Walks the IR, allocates property/shape/style ids, flattens Lottie shape
//! groups into `(shape, style)` pairs that the driver renders directly.
//! `can_encode()` returns `false` for unsupported IR shapes; the caller
//! surfaces that as a compile error (no fallback backend exists).

use anyhow::Result;

use crate::data::*;
use crate::ir;

/// True when an `ir::Module` only uses features the data backend currently
/// understands. As features land (D2.3 trim path, D4.2 full expressions, …)
/// this gate loosens.
pub fn can_encode(m: &ir::Module) -> bool {
    // Each precomp asset's inner layers must themselves be representable.
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
        | ir::LayerKind::Precomp { .. } => true,
        ir::LayerKind::Image { .. } | ir::LayerKind::Other { .. } => false,
    };
    if !supported && std::env::var("ULOTTIE_DEBUG_BACKEND").is_ok() {
        eprintln!(
            "data-backend bail: layer ind={} ty={:?}",
            layer.index,
            std::mem::discriminant(&layer.kind),
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
            // All three property variants are now supported: static is the
            // common case, animated is encoded as path keyframes (driver
            // tweens via `interpolateKf` + path-aware `lerpValue`), and
            // expression-driven paths flow through `evalProp`.
            ir::ShapeNode::Path { .. } => {}
            // Gradient strokes: emitted as Style::GradientStroke with the
            // gradient definition stored verbatim. Driver builds <defs> +
            // <linearGradient> on first render.
            ir::ShapeNode::GradientStroke { .. } => {}
            // Gradient fills: emitted as Style::GradientFill with the
            // gradient definition stored verbatim. Driver builds <defs> +
            // <linearGradient>/<radialGradient> on first render and applies
            // `fill="url(#…)"`.
            ir::ShapeNode::GradientFill { .. } => {}
            // Both identity and animated group transforms are now supported:
            // identity transforms are skipped during encoding, animated ones
            // become a GroupRef wrapping the children in a `<g>` per the wire
            // format. No can_encode check needed.
            ir::ShapeNode::Transform { .. } => {}
            ir::ShapeNode::Rectangle { .. }
            | ir::ShapeNode::Ellipse { .. }
            | ir::ShapeNode::PolyStar { .. }
            | ir::ShapeNode::Fill { .. }
            | ir::ShapeNode::Stroke { .. }
            | ir::ShapeNode::TrimPath { .. } => {}
        }
    }
    true
}

/// True when a group-local Transform doesn't change rendering: anchor == position
/// (so the translate-from-anchor cancels the position translate), scale == 100,
/// rotation == 0, opacity == 100, no skew. This is the common AE-emitted form.
fn transform_is_identity(t: &ir::Transform) -> bool {
    let position = static_vec3(&t.position).unwrap_or([0.0, 0.0, 0.0]);
    let anchor = static_vec3(&t.anchor).unwrap_or([0.0, 0.0, 0.0]);
    if (position[0] - anchor[0]).abs() > 1e-3
        || (position[1] - anchor[1]).abs() > 1e-3
        || (position[2] - anchor[2]).abs() > 1e-3
    {
        return false;
    }
    let scale = static_vec3(&t.scale).unwrap_or([100.0, 100.0, 100.0]);
    // The z-component of scale is meaningless for 2D rendering, and 2D
    // sources (most fixtures) commonly emit `[100, 100]` which lowers to
    // `[100, 100, 0]`. Only the x/y components need to be at 100% for the
    // group transform to be a render no-op.
    if (scale[0] - 100.0).abs() > 1e-3 || (scale[1] - 100.0).abs() > 1e-3 {
        return false;
    }
    let rotation = static_scalar(&t.rotation).unwrap_or(0.0);
    if rotation.abs() > 1e-3 {
        return false;
    }
    let opacity = static_scalar(&t.opacity).unwrap_or(100.0);
    if (opacity - 100.0).abs() > 1e-3 {
        return false;
    }
    // Skews are absent or static-zero.
    let skew_ok = t.skew.as_ref().is_none_or(|p| static_scalar(p) == Some(0.0));
    let skew_axis_ok = t.skew_axis.as_ref().is_none_or(|p| static_scalar(p) == Some(0.0));
    skew_ok && skew_axis_ok
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
    // Encode precomp / image assets. Precomp instances reference these by id
    // via `Layer::rf`. Each precomp asset's inner layers are full Layer
    // records and share the module's property / shape / style tables — no
    // duplication.
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
                ir::AssetKind::Image { path, filename, width, height, embedded } => {
                    assets.insert(asset.id.clone(), Asset::Image {
                        u: path.clone(),
                        p: filename.clone(),
                        w: *width,
                        h: *height,
                        e: if *embedded { 1 } else { 0 },
                    });
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
    /// Intern: serialized-property JSON → id. Lets us collapse repeated
    /// static defaults (e.g. dozens of `[100,100,100]` scales) into one entry.
    prop_cache: std::collections::HashMap<String, u32>,
    /// Intern: layer/effect name → string-table id. Lets us share repeated
    /// names without paying for them every layer.
    str_cache: std::collections::HashMap<String, u32>,
}

impl Encoder {
    fn new() -> Self {
        Self {
            payload: Payload::default(),
            prop_cache: std::collections::HashMap::new(),
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

    // -- layer ------------------------------------------------------------

    fn encode_layer(&mut self, layer: &ir::Layer) -> Result<Layer> {
        let p = self.encode_prop_vec3(&layer.transform.position)?;
        let a = self.encode_prop_vec3(&layer.transform.anchor)?;
        let sc = self.encode_prop_vec3(&layer.transform.scale)?;
        let r = self.encode_prop_scalar(&layer.transform.rotation)?;
        let o = self.encode_prop_scalar(&layer.transform.opacity)?;

        let mut out = Layer {
            i: layer.index,
            ty: layer_ty_num(&layer.kind),
            ip: layer.in_point,
            op: layer.out_point,
            sr: layer.stretch,
            // Precomp instances carry a `start_time` offset that shifts when
            // their inner clock plays back (the ripple bars stagger via this).
            st: if layer.start_time == 0.0 { None } else { Some(layer.start_time) },
            p: Some(p),
            a: Some(a),
            sc: Some(sc),
            r: Some(r),
            o: Some(o),
            ..Default::default()
        };

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
                let pt = self.encode_prop_path(&m.shape)?;
                let o = match &m.opacity {
                    Some(prop) => Some(self.encode_prop_scalar(prop)?),
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
                    let (v, pid) = match &p.value {
                        // Scalar with no animation: emit a constant `v`. Saves
                        // a property-table entry for the common case (ADBE
                        // Slider Control / Trace Path Progress, or a Layer
                        // Control's static layer-index reference).
                        ir::EffectValue::Scalar(ir::Property::Static(s)) => (Some(*s), None),
                        ir::EffectValue::Scalar(prop) => {
                            let id = self.encode_prop_scalar(prop)?;
                            (None, Some(id))
                        }
                        // Non-scalar effect params (color pickers, paths, etc.)
                        // aren't read by any of our fixture expressions yet.
                        // Skip so the encoder stays compact.
                        ir::EffectValue::Other(_) => continue,
                    };
                    params.push(EffectParam {
                        nm: p.name.clone(),
                        mn: p.match_name.clone(),
                        ty: p.ty,
                        v,
                        p: pid,
                    });
                }
                effects.push(Effect {
                    nm: e.name.clone(),
                    mn: e.match_name.clone(),
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
            ir::LayerKind::Solid { color, width, height } => {
                out.cl = Some(color.clone());
                out.sw = Some(*width);
                out.sh = Some(*height);
            }
            ir::LayerKind::Precomp { asset, width, height } => {
                out.rf = Some(asset.clone());
                // Lottie ignores precomp width/height visually unless the
                // composition is clipped — emit them so the driver can clip
                // later if we choose to.
                if *width != 0 {
                    out.sw = Some(*width);
                }
                if *height != 0 {
                    out.sh = Some(*height);
                }
            }
            _ => {}
        }

        Ok(out)
    }

    // -- shapes -----------------------------------------------------------
    //
    // Flatten a Lottie shape tree into (primitive, style) pairs. Strokes /
    // fills / trim paths within a group apply to all primitive siblings; we
    // resolve that scoping here. Lottie's lottie-logo fixture puts a TrimPath
    // at the LAYER level alongside a Group that holds the actual Path — so
    // any trim/style we collect at this scope must also be inherited by
    // recursive descents into nested groups.

    fn encode_shape_tree(
        &mut self,
        shapes: &[ir::ShapeNode],
        out: &mut Vec<ShapeRef>,
    ) -> Result<()> {
        self.encode_shape_tree_with(shapes, out, &[], None)
    }

    fn encode_shape_tree_with(
        &mut self,
        shapes: &[ir::ShapeNode],
        out: &mut Vec<ShapeRef>,
        inherited_styles: &[u32],
        inherited_trim: Option<u32>,
    ) -> Result<()> {
        // Pass 1: collect Fill/Stroke/TrimPath siblings into the current scope's
        // "ambient style" stack, and locate any group-local Transform (Lottie
        // groups emit one as a sibling). Multiple fills/strokes at the same
        // level all apply, painted in source order — emulates Lottie's
        // group-shape style stacking. Inherited values from the enclosing
        // scope come first so locals can override / stack on top.
        let mut current_styles: Vec<u32> = inherited_styles.to_vec();
        let mut current_trim: Option<u32> = inherited_trim;
        let mut group_transform: Option<&ir::Transform> = None;
        for s in shapes {
            match s {
                ir::ShapeNode::Fill { color, opacity, .. } => {
                    let c = self.encode_prop_color(color)?;
                    let o = self.encode_prop_scalar(opacity)?;
                    let id = self.payload.y.len() as u32;
                    self.payload.y.push(Style::Fill { c, o });
                    current_styles.push(id);
                }
                ir::ShapeNode::Stroke {
                    color, opacity, width, linecap, linejoin, miter_limit, ..
                } => {
                    let c = self.encode_prop_color(color)?;
                    let o = self.encode_prop_scalar(opacity)?;
                    let w = self.encode_prop_scalar(width)?;
                    let id = self.payload.y.len() as u32;
                    self.payload.y.push(Style::Stroke {
                        c, o, w,
                        lc: linecap_num(*linecap),
                        lj: linejoin_num(*linejoin),
                        ml: *miter_limit,
                    });
                    current_styles.push(id);
                }
                ir::ShapeNode::GradientStroke {
                    gradient, width, opacity, start, end, kind, linecap, linejoin, miter_limit, ..
                } => {
                    let w = self.encode_prop_scalar(width)?;
                    let o = self.encode_prop_scalar(opacity)?;
                    let s = match start {
                        Some(p) => Some(self.encode_prop_vec2(p)?),
                        None => None,
                    };
                    let e = match end {
                        Some(p) => Some(self.encode_prop_vec2(p)?),
                        None => None,
                    };
                    let id = self.payload.y.len() as u32;
                    self.payload.y.push(Style::GradientStroke {
                        g: gradient.raw.clone().unwrap_or(serde_json::Value::Null),
                        w, o, s, e,
                        gk: match kind {
                            ir::GradientKind::Linear => 1,
                            ir::GradientKind::Radial => 2,
                        },
                        lc: linecap_num(*linecap),
                        lj: linejoin_num(*linejoin),
                        ml: *miter_limit,
                    });
                    current_styles.push(id);
                }
                ir::ShapeNode::GradientFill {
                    gradient, opacity, start, end, kind, rule, ..
                } => {
                    let o = self.encode_prop_scalar(opacity)?;
                    let s = match start {
                        Some(p) => Some(self.encode_prop_vec2(p)?),
                        None => None,
                    };
                    let e = match end {
                        Some(p) => Some(self.encode_prop_vec2(p)?),
                        None => None,
                    };
                    let id = self.payload.y.len() as u32;
                    self.payload.y.push(Style::GradientFill {
                        g: gradient.raw.clone().unwrap_or(serde_json::Value::Null),
                        o, s, e,
                        gk: match kind {
                            ir::GradientKind::Linear => 1,
                            ir::GradientKind::Radial => 2,
                        },
                        fr: match rule {
                            ir::FillRule::NonZero => 1,
                            ir::FillRule::EvenOdd => 2,
                        },
                    });
                    current_styles.push(id);
                }
                ir::ShapeNode::TrimPath { start, end, offset, multiple_shapes, .. } => {
                    let s = self.encode_prop_scalar(start)?;
                    let e = self.encode_prop_scalar(end)?;
                    let o = self.encode_prop_scalar(offset)?;
                    let id = self.payload.y.len() as u32;
                    self.payload.y.push(Style::TrimPath {
                        s, e, o,
                        m: match multiple_shapes {
                            ir::TrimMultipleShapes::Simultaneously => 1,
                            ir::TrimMultipleShapes::Individually => 2,
                        },
                    });
                    current_trim = Some(id);
                }
                ir::ShapeNode::Transform { transform, .. } => {
                    group_transform = Some(transform);
                }
                _ => {} // primitives handled in pass 2
            }
        }

        // Pass 2: emit primitives + sub-groups. If the scope has a group-local
        // Transform that isn't a no-op, collect emissions in a temporary buffer
        // and wrap them in a GroupRef at the end.
        let emit_into_group = group_transform.is_some_and(|t| !transform_is_identity(t));
        let mut emitted: Vec<ShapeRef> = Vec::new();
        let target = if emit_into_group { &mut emitted } else { &mut *out };

        for s in shapes {
            match s {
                ir::ShapeNode::Group { items, .. } => {
                    self.encode_shape_tree_with(
                        items, target, &current_styles, current_trim,
                    )?;
                }
                ir::ShapeNode::Rectangle { size, position, radius, .. } => {
                    let sz = self.encode_prop_vec2(size)?;
                    let ps = self.encode_prop_vec2(position)?;
                    let rd = self.encode_prop_scalar(radius)?;
                    let sid = self.payload.s.len() as u32;
                    self.payload.s.push(Shape::Rect { sz, ps, rd, nm: None });
                    target.push(ShapeRef::Prim(PrimRef { s: sid, y: current_styles.clone(), tm: current_trim }));
                }
                ir::ShapeNode::Ellipse { size, position, .. } => {
                    let sz = self.encode_prop_vec2(size)?;
                    let ps = self.encode_prop_vec2(position)?;
                    let sid = self.payload.s.len() as u32;
                    self.payload.s.push(Shape::Ellipse { sz, ps, nm: None });
                    target.push(ShapeRef::Prim(PrimRef { s: sid, y: current_styles.clone(), tm: current_trim }));
                }
                ir::ShapeNode::Path { ks, .. } => {
                    let pid = self.encode_prop_path(ks)?;
                    let sid = self.payload.s.len() as u32;
                    self.payload.s.push(Shape::Path { pt: pid, nm: None });
                    target.push(ShapeRef::Prim(PrimRef { s: sid, y: current_styles.clone(), tm: current_trim }));
                }
                ir::ShapeNode::PolyStar {
                    kind, points, position, rotation, outer_radius,
                    inner_radius, outer_roundness, inner_roundness, ..
                } => {
                    let pt = self.encode_prop_scalar(points)?;
                    let ps = self.encode_prop_vec2(position)?;
                    let rt = self.encode_prop_scalar(rotation)?;
                    let or = self.encode_prop_scalar(outer_radius)?;
                    let ir = match inner_radius {
                        Some(p) => self.encode_prop_scalar(p)?,
                        None => self.encode_prop_scalar(&ir::Property::Static(0.0))?,
                    };
                    let os = match outer_roundness {
                        Some(p) => Some(self.encode_prop_scalar(p)?),
                        None => None,
                    };
                    let is = match inner_roundness {
                        Some(p) => Some(self.encode_prop_scalar(p)?),
                        None => None,
                    };
                    let sid = self.payload.s.len() as u32;
                    self.payload.s.push(Shape::PolyStar {
                        sy: match kind {
                            ir::PolyStarKind::Star => 1,
                            ir::PolyStarKind::Polygon => 2,
                        },
                        pt, ps, or, ir, rt, os, is, nm: None,
                    });
                    target.push(ShapeRef::Prim(PrimRef { s: sid, y: current_styles.clone(), tm: current_trim }));
                }
                // Already collected in pass 1.
                ir::ShapeNode::Fill { .. }
                | ir::ShapeNode::Stroke { .. }
                | ir::ShapeNode::GradientStroke { .. }
                | ir::ShapeNode::GradientFill { .. }
                | ir::ShapeNode::TrimPath { .. }
                | ir::ShapeNode::Transform { .. } => {}
            }
        }

        if emit_into_group {
            // Wrap the emitted children in a GroupRef with the group's transform.
            // SAFETY of unwrap: `emit_into_group` is only true when `group_transform`
            // is `Some`, so this is unconditional.
            let tr = group_transform.unwrap();
            let group = GroupRef {
                c: emitted,
                p: Some(self.encode_prop_vec3(&tr.position)?),
                a: Some(self.encode_prop_vec3(&tr.anchor)?),
                sc: Some(self.encode_prop_vec3(&tr.scale)?),
                r: Some(self.encode_prop_scalar(&tr.rotation)?),
                o: Some(self.encode_prop_scalar(&tr.opacity)?),
            };
            out.push(ShapeRef::Group(group));
        }
        Ok(())
    }

    // -- properties --------------------------------------------------------

    fn encode_prop_scalar(&mut self, p: &ir::Property<f64>) -> Result<u32> {
        let prop = match p {
            ir::Property::Static(v) => Property::Static(StaticProp { k: Value::Scalar(*v) }),
            ir::Property::Animated(kf) => Property::Animated(AnimatedProp {
                kf: encode_keyframes_scalar(kf),
            }),
            ir::Property::Expression { fallback, expr } => {
                let (fb, kf) = match fallback {
                    ir::ValueSource::Static(v) => (Some(Value::Scalar(*v)), None),
                    ir::ValueSource::Animated(kf) => (None, Some(encode_keyframes_scalar(kf))),
                };
                Property::Expression(ExprProp { e: expr.0, fb, kf })
            }
        };
        Ok(self.intern_prop(prop))
    }

    fn encode_prop_vec2(&mut self, p: &ir::Property<ir::Vec2>) -> Result<u32> {
        let prop = match p {
            ir::Property::Static(v) => Property::Static(StaticProp { k: Value::Vector(v.to_vec()) }),
            ir::Property::Animated(kf) => Property::Animated(AnimatedProp {
                kf: encode_keyframes_vec(kf, 2),
            }),
            ir::Property::Expression { fallback, expr } => {
                let (fb, kf) = match fallback {
                    ir::ValueSource::Static(v) => (Some(Value::Vector(v.to_vec())), None),
                    ir::ValueSource::Animated(kf) => (None, Some(encode_keyframes_vec(kf, 2))),
                };
                Property::Expression(ExprProp { e: expr.0, fb, kf })
            }
        };
        Ok(self.intern_prop(prop))
    }

    fn encode_prop_vec3(&mut self, p: &ir::Property<ir::Vec3>) -> Result<u32> {
        let prop = match p {
            ir::Property::Static(v) => Property::Static(StaticProp { k: Value::Vector(v.to_vec()) }),
            ir::Property::Animated(kf) => Property::Animated(AnimatedProp {
                kf: encode_keyframes_vec(kf, 3),
            }),
            ir::Property::Expression { fallback, expr } => {
                let (fb, kf) = match fallback {
                    ir::ValueSource::Static(v) => (Some(Value::Vector(v.to_vec())), None),
                    ir::ValueSource::Animated(kf) => (None, Some(encode_keyframes_vec(kf, 3))),
                };
                Property::Expression(ExprProp { e: expr.0, fb, kf })
            }
        };
        Ok(self.intern_prop(prop))
    }

    fn encode_prop_path(&mut self, p: &ir::Property<ir::PathData>) -> Result<u32> {
        let prop = match p {
            ir::Property::Static(pd) => Property::Static(StaticProp {
                k: Value::Path(path_to_wire(pd)),
            }),
            ir::Property::Animated(kf) => Property::Animated(AnimatedProp {
                kf: encode_keyframes_path(kf),
            }),
            ir::Property::Expression { fallback, expr } => {
                // Preserve the fallback path so the driver can pass it as
                // `thisProperty` to the expression (lights wire's `origPoints
                // = thisProperty.points()` needs this).
                let fb = match fallback {
                    ir::ValueSource::Static(pd) => Some(Value::Path(path_to_wire(pd))),
                    ir::ValueSource::Animated(_) => None,
                };
                Property::Expression(ExprProp { e: expr.0, fb, kf: None })
            }
        };
        Ok(self.intern_prop(prop))
    }

    fn encode_prop_color(&mut self, p: &ir::Property<ir::Color>) -> Result<u32> {
        let prop = match p {
            ir::Property::Static(v) => Property::Static(StaticProp { k: Value::Vector(v.to_vec()) }),
            ir::Property::Animated(kf) => Property::Animated(AnimatedProp {
                kf: encode_keyframes_vec(kf, 4),
            }),
            ir::Property::Expression { fallback, expr } => {
                let (fb, kf) = match fallback {
                    ir::ValueSource::Static(v) => (Some(Value::Vector(v.to_vec())), None),
                    ir::ValueSource::Animated(kf) => (None, Some(encode_keyframes_vec(kf, 4))),
                };
                Property::Expression(ExprProp { e: expr.0, fb, kf })
            }
        };
        Ok(self.intern_prop(prop))
    }

    fn intern_prop(&mut self, p: Property) -> u32 {
        let key = serde_json::to_string(&p).expect("serialize prop for intern key");
        if let Some(&id) = self.prop_cache.get(&key) {
            return id;
        }
        let id = self.payload.p.len() as u32;
        self.payload.p.push(p);
        self.prop_cache.insert(key, id);
        id
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

    // In Lottie JSON, both `o` and `i` on keyframe N describe the segment
    // *starting* at N — `o` is the out handle leaving N, `i` is the in
    // handle of the bezier control point heading toward N+1. lottie-web
    // pairs them directly from the same keyData (see PropertyFactory.js's
    // `BezierFactory.getBezierEasing(keyData.o.x, keyData.o.y, keyData.i.x,
    // keyData.i.y)`). Mirror that here so eased progress matches lottie-web.
    for frame in &kf.frames {
        times.push(frame.time);
        values.push(Value::Scalar(frame.value.unwrap_or(0.0)));
        ends.push(frame.end_value.map(Value::Scalar));
        if frame.end_value.is_some() { any_end = true; }
        if let (Some(o), Some(i)) = (&frame.easing_out, &frame.easing_in) {
            oi_list.push(EasingPair { o: convert_easing(o), i: convert_easing(i) });
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

    // `to[i]` (out tangent) and `ti[i]` (in tangent) both belong to the
    // segment STARTING at frame i, and both live on `keyData` in lottie-web
    // (see PropertyFactory.js: `bez.buildBezierData(keyData.s, nextKeyData.s,
    // keyData.to, keyData.ti)` — `ti` is read from the same keyframe as `to`,
    // not from the next one). Same pairing applies to `o`/`i` for temporal
    // easing.
    for frame in &kf.frames {
        times.push(frame.time);
        values.push(Value::Vector(frame.value.map(|v| v.to_vec()).unwrap_or_default()));
        ends.push(frame.end_value.map(|v| Value::Vector(v.to_vec())));
        if frame.end_value.is_some() { any_end = true; }
        if let (Some(o), Some(i)) = (&frame.easing_out, &frame.easing_in) {
            oi_list.push(EasingPair { o: convert_easing(o), i: convert_easing(i) });
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
    }
}

/// Encode an animated path keyframe list. Each keyframe holds a full
/// `{v, i, o, c}` path; we serialize the values as `Value::Path` entries so
/// the driver's `interpolateKf` (combined with its path-aware lerp) can
/// blend between adjacent paths over time.
fn encode_keyframes_path(kf: &ir::Keyframes<ir::PathData>) -> Keyframes {
    let mut times = Vec::with_capacity(kf.frames.len());
    let mut values = Vec::with_capacity(kf.frames.len());
    let mut ends: Vec<Option<Value>> = Vec::with_capacity(kf.frames.len());
    let mut oi_list: Vec<EasingPair> = Vec::with_capacity(kf.frames.len());
    let mut any_end = false;
    let mut any_easing = false;

    for frame in &kf.frames {
        times.push(frame.time);
        match &frame.value {
            Some(pd) => values.push(Value::Path(path_to_wire(pd))),
            None => values.push(Value::Vector(Vec::new())),
        }
        ends.push(frame.end_value.as_ref().map(|pd| Value::Path(path_to_wire(pd))));
        if frame.end_value.is_some() { any_end = true; }
        if let (Some(o), Some(i)) = (&frame.easing_out, &frame.easing_in) {
            oi_list.push(EasingPair { o: convert_easing(o), i: convert_easing(i) });
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
        o: EasingHandle { x: EasingComponent::Scalar(0.0), y: EasingComponent::Scalar(0.0) },
        i: EasingHandle { x: EasingComponent::Scalar(1.0), y: EasingComponent::Scalar(1.0) },
    }
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

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
