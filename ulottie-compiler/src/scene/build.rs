//! Walking the payload: layers → element tree + bindings.
//!
//! Every decision here is "can this be answered at compile time?". When the
//! answer is yes the value goes into the element's attribute list and no
//! binding is created at all.

use anyhow::Result;

use crate::data::{self, InlineProp, Shape, ShapeRef, Style, Value};
use crate::eval::geometry;

use super::prop::{Anim, AnimKind, Prop};
use super::svg::{self, FlatPath};
use std::collections::HashMap;

use super::{Arg, Binding, Caps, Effect, EffectParam, LayerRecord, Planner, op};

/// After Effects' `ADBE Fill`. The numbering is Lottie's, and lottie-web keys
/// its own filter table on it — see `registerEffect(21, SVGFillFilter)`.
const EFFECT_FILL: u32 = 21;
/// An effect parameter holding a colour.
const EFFECT_PARAM_COLOR: u32 = 2;

/// Clock a subtree runs on.
#[derive(Clone, Copy)]
pub enum TimeCtx {
    /// The composition clock, slot 0.
    Root,
    /// Inside a precomp: the parent's clock, shifted by the precomp layer's
    /// own start time.
    ///
    /// A layer's `st` shifts *nothing else*. The reference renderer subtracts
    /// it from the frame and from every keyframe time it is compared against,
    /// so the two cancel — which is why `car-5` keyframes a layer at t=55..70
    /// and gives it `st: 55` and still means composition time. The one place
    /// it survives is here, where a precomp hands a clock to its children.
    Inner { parent: u32, offset: f64 },
}

struct LayerNode {
    outer: usize,
    /// What the parent (or the root list) actually attaches. Normally `outer`;
    /// a matted layer gets an untransformed wrapper above it, because a mask is
    /// resolved in the user space its own element establishes — hanging it on
    /// `outer` would put the matte through `outer`'s transform a second time.
    mounted: usize,
    dead: bool,
    /// Index into the expression layer table, when one was built.
    record: Option<u32>,
    /// Timeline slot the layer's own bindings run on. Kept so a *different*
    /// layer can re-emit this one's transform — see `wrap_ancestors`.
    slot: u32,
}

impl Planner<'_> {
    // -----------------------------------------------------------------------
    // Layers
    // -----------------------------------------------------------------------

    pub(super) fn build_layer_forest(
        &mut self,
        layers: &[data::Layer],
        ctx: TimeCtx,
    ) -> Result<Vec<usize>> {
        // Each composition gets its own scope so `thisComp.layer('x')` inside a
        // precomp resolves against that precomp's layers.
        let scope = match ctx {
            TimeCtx::Root => 0,
            TimeCtx::Inner { .. } => self.next_scope(),
        };
        // Parenting contributes a transform and nothing else — in After Effects
        // a layer is painted at its own place in the list whether or not it has
        // a parent. Nesting the child inside the parent's group is the cheap way
        // to inherit that transform, and it is sound only while it leaves the
        // layers in the order they have to be painted in. When it does not, each
        // child stays where it belongs and carries its ancestors' transforms on
        // wrappers of its own.
        //
        // The decision comes first because it changes what `build_layer` emits:
        // whether a layer can be seen at all is a question about its span and
        // the composition's, which needs nothing built.
        // A layer is hidden when its span is narrower than the composition it
        // is in. The document's is on the payload; a precomp asset does not
        // carry one, so it is taken to be the union of its layers' — which is
        // what makes "covers the whole composition" mean "always on" in both.
        let span = match ctx {
            TimeCtx::Root => (self.payload.c.ip, self.payload.c.op),
            TimeCtx::Inner { .. } => layers.iter().fold((f64::MAX, f64::MIN), |(lo, hi), l| {
                (lo.min(l.ip), hi.max(l.op))
            }),
        };
        let dead: Vec<bool> = match ctx {
            TimeCtx::Root => layers
                .iter()
                .map(|l| l.op <= span.0 || l.ip >= span.1)
                .collect(),
            // A precomp's clock is the instance's, so no layer of it can be
            // ruled out at compile time the way a root layer can.
            TimeCtx::Inner { .. } => vec![false; layers.len()],
        };
        let nested = nesting_preserves_order(layers, &dead);

        // A null draws nothing: it is in the document only so its children can
        // inherit its transform. Flat mode gives them wrappers of their own, so
        // the null's own group would be an empty node writing a matrix nobody
        // reads — starfish parents thirteen layers to one. Suppressing the
        // transform leaves the group empty and unpinned, which pruning removes.
        let inert = |i: usize| !nested && layers[i].ty == 3;

        let outer_scope = self.scope;
        self.scope = scope;
        let mut nodes = Vec::with_capacity(layers.len());
        for (i, l) in layers.iter().enumerate() {
            nodes.push(self.build_layer(l, ctx, inert(i), span)?);
        }
        self.scope = outer_scope;

        // Expression layer parenting is by record index, resolved once the
        // whole composition has been walked.
        if self.has_exprs {
            for (i, l) in layers.iter().enumerate() {
                if let (Some(pr), Some(rec)) = (l.pr, nodes[i].record) {
                    if let Some(prec) = nodes.get(pr as usize).and_then(|n| n.record) {
                        self.layers[rec as usize].pr = Some(prec);
                    }
                }
            }
        }
        // Track mattes. A layer carrying `td` is a matte: it is never drawn on
        // its own, it masks another layer, which carries `tt` with the mode.
        // Turning the pair into a `<mask>` has to happen before roots are
        // collected, so the matte source goes into `<defs>` rather than into
        // the picture.
        //
        // Which layer it mattes is `tp`, naming it by `ind` — and only when
        // that is absent is it the layer immediately above. `car-13` relies on
        // both halves of that: one of its mattes is four layers away from what
        // it masks, and another masks two layers at once.
        let mut matte_source = vec![false; layers.len()];
        // Layers whose ancestor transforms are already on wrappers of their
        // own, and so must not be put under a shared one as well.
        let mut chained = vec![false; layers.len()];
        // Masks built so far, by (source layer, mode). A source can serve
        // several layers, and a mask is referenced by id, so the second one to
        // ask for it reuses the first's rather than needing its own copy of a
        // subtree that can only be in one place.
        let mut masks: HashMap<(usize, u8), String> = HashMap::new();
        for i in 0..layers.len() {
            let Some(tt) = layers[i].tt else { continue };
            let Some(j) = layers[i]
                .tp
                .map(|p| p as usize)
                .or_else(|| i.checked_sub(1))
                .filter(|&j| j < layers.len())
            else {
                continue;
            };
            if nodes[i].dead || nodes[j].dead {
                continue;
            }
            if !nested {
                // A mask is resolved in composition space, and both halves of
                // the pair have to be *in* it before they meet: the matte is a
                // sibling, authored where it is drawn. So each gets its chain
                // privately, under the mask rather than over it — which is
                // also why neither can join a shared wrapper afterwards.
                nodes[i].outer = self.wrap_ancestors(layers, &nodes, i, nodes[i].outer);
                chained[i] = true;
            }
            let id = match masks.get(&(j, tt)) {
                Some(id) => id.clone(),
                None => {
                    let content = self.matte_content(layers, &mut nodes, j, nested);
                    let id = self.matte_mask(content, tt);
                    masks.insert((j, tt), id.clone());
                    id
                }
            };
            nodes[i].mounted = self.masked(nodes[i].outer, &id);
        }
        // A matte is out of the picture whether or not anything ended up
        // masked by it — the reference renderer moves every `td` layer into
        // `<defs>` on sight. `car-13` has one nothing points at, and drawing
        // it put a white card over the animation.
        for (i, l) in layers.iter().enumerate() {
            if l.td.is_some() {
                matte_source[i] = true;
            }
        }

        // The reference renderer appends layers back-to-front, so the first
        // layer in the list ends up on top.
        let mut roots = Vec::new();
        // Ancestor wrappers currently open, outermost first, as
        // `(ancestor layer, element)`. Consecutive layers that need the same
        // ancestor share the wrapper: the walk closes only what the next layer
        // does not need, so thirteen layers under one null cost one wrapper
        // rather than thirteen — and a composition whose layers all share the
        // whole chain gets exactly the tree nesting would have built.
        let mut open: Vec<(usize, usize)> = Vec::new();
        for i in (0..layers.len()).rev() {
            // An inert layer is not in the document at all. Walking it would
            // open a wrapper for its ancestors that nothing then fills —
            // starfish's ten nulls sit between the two layers that do need one,
            // and closing and reopening around them left an empty group behind
            // that still wrote a matrix every frame.
            if nodes[i].dead || matte_source[i] || inert(i) {
                continue;
            }
            if nested {
                match layers[i].pr {
                    Some(pr) if (pr as usize) < layers.len() && !nodes[pr as usize].dead => {
                        // Into the parent's own group, so the child inherits its
                        // transform — and, for a matted parent, its mask. AE mattes
                        // only the layer itself; no fixture parents to a matted
                        // layer, so that difference is untested and unhandled.
                        let parent = nodes[pr as usize].outer;
                        let child = nodes[i].mounted;
                        self.els[parent].children.push(child);
                    }
                    _ => roots.push(nodes[i].mounted),
                }
                continue;
            }

            let chain = if chained[i] {
                Vec::new()
            } else {
                ancestors(layers, &dead, i)
            };
            let mut k = 0;
            while k < open.len() && k < chain.len() && open[k].0 == chain[k] {
                k += 1;
            }
            open.truncate(k);
            for &a in &chain[k..] {
                let w = self.el("g");
                self.emit_ancestor_transform(w, layers, &nodes, a);
                match open.last() {
                    Some(&(_, parent)) => self.els[parent].children.push(w),
                    None => roots.push(w),
                }
                open.push((a, w));
            }
            let node = nodes[i].mounted;
            match open.last() {
                Some(&(_, parent)) => self.els[parent].children.push(node),
                None => roots.push(node),
            }
        }
        Ok(roots)
    }

    /// A layer transform whose properties live in the expression layer table:
    /// baked into the markup when nothing about it can move, a reference to
    /// the table's record when something can.
    fn emit_record_transform(
        &mut self,
        el: usize,
        p: Option<Prop>,
        a: Option<Prop>,
        s: Option<Prop>,
        r: Option<Prop>,
        rec: u32,
        slot: u32,
    ) {
        let dp = p.unwrap_or(Prop::Vector(vec![0.0, 0.0, 0.0]));
        let da = a.unwrap_or(Prop::Vector(vec![0.0, 0.0, 0.0]));
        let ds = s.unwrap_or(Prop::Vector(vec![100.0, 100.0, 100.0]));
        let dr = r.unwrap_or(Prop::Scalar(0.0));
        if dp.is_static() && da.is_static() && ds.is_static() && dr.is_static() {
            let m = matrix(&dp, &da, &ds, &dr);
            if !is_identity(&m) {
                self.set(el, "transform", svg::transform_str(&m));
            }
        } else {
            self.bind(op::LAYER_TX, el, vec![Arg::Num(rec as f64)], slot);
        }
    }

    /// One ancestor's transform, on an element that is not the ancestor.
    ///
    /// It is re-emitted rather than shared because an element can only be in
    /// one place in a document and the ancestor is already in its own. The
    /// matrix is therefore computed once per wrapper per frame; sharing a
    /// wrapper between consecutive layers is what keeps that from multiplying,
    /// and `prune_list` drops the wrapper outright when the transform turns
    /// out to be a static identity.
    fn emit_ancestor_transform(
        &mut self,
        el: usize,
        layers: &[data::Layer],
        nodes: &[LayerNode],
        a: usize,
    ) {
        match nodes[a].record {
            // With expressions the ancestor's properties are already in the
            // layer table — read them back rather than classifying the same
            // wire entries a second time.
            Some(rec) => {
                let e = &self.layers[rec as usize];
                let (p, an, s, r) = (e.p.clone(), e.a.clone(), e.sc.clone(), e.r.clone());
                self.emit_record_transform(el, p, an, s, r, rec, nodes[a].slot);
            }
            None => self.emit_transform(
                el,
                layers[a].p.as_ref(),
                layers[a].a.as_ref(),
                layers[a].sc.as_ref(),
                layers[a].r.as_ref(),
                nodes[a].slot,
            ),
        }
    }

    /// The subtree a `<mask>` is built from, for the layer that mattes.
    ///
    /// A layer carrying `td` is out of the picture entirely, so the mask can
    /// simply take it. A layer merely *named* by another's `tp` is **also
    /// drawn** — lottie-web only moves a `td` layer into `<defs>`, and reaches
    /// anything else from the mask with `<use>`, which is the only way one
    /// subtree can be in two places in a document.
    ///
    /// Hiding every matte source cost a whole picture: the krrt map animation
    /// has a layer that is both, masked by the `td` layer above it and the
    /// matte for two waves below, and taking it out of the picture took the map
    /// with it.
    ///
    /// The `<use>` carries the ancestors' transforms on wrappers of its own so
    /// the mask lands in the same space the matte was authored in; the layer's
    /// own group keeps its own transform and stays where the tree puts it.
    fn matte_content(
        &mut self,
        layers: &[data::Layer],
        nodes: &mut [LayerNode],
        j: usize,
        nested: bool,
    ) -> usize {
        if layers[j].td.is_some() {
            if !nested {
                nodes[j].outer = self.wrap_ancestors(layers, nodes, j, nodes[j].outer);
            }
            return nodes[j].outer;
        }
        let id = self.next_id("u");
        self.set(nodes[j].outer, "id", id.clone());
        let u = self.el("use");
        self.set(u, "href", format!("#{id}"));
        if nested {
            u
        } else {
            self.wrap_ancestors(layers, nodes, j, u)
        }
    }

    /// Wrap `node` in one `<g>` per ancestor of layer `i`, each carrying that
    /// ancestor's transform, outermost last. Returns the outermost wrapper.
    ///
    /// Used where a layer cannot join a shared wrapper — see the matte pass.
    fn wrap_ancestors(
        &mut self,
        layers: &[data::Layer],
        nodes: &[LayerNode],
        i: usize,
        node: usize,
    ) -> usize {
        let dead: Vec<bool> = nodes.iter().map(|n| n.dead).collect();
        let mut node = node;
        for &a in ancestors(layers, &dead, i).iter().rev() {
            let w = self.el("g");
            self.emit_ancestor_transform(w, layers, nodes, a);
            self.els[w].children.push(node);
            node = w;
        }
        node
    }

    /// `inert` marks a layer that will draw nothing and be inherited from by
    /// nobody — its group is emitted empty so pruning can take it. `span` is
    /// the composition's own `[ip, op)`, against which a narrower layer needs
    /// hiding — the document's for a root layer, the asset's inside a precomp.
    fn build_layer(
        &mut self,
        layer: &data::Layer,
        ctx: TimeCtx,
        inert: bool,
        span: (f64, f64),
    ) -> Result<LayerNode> {
        let (c_ip, c_op) = span;
        let (slot, range_hidden) = match ctx {
            TimeCtx::Root => {
                // A layer whose span never overlaps the composition can never
                // be seen — drop the whole subtree.
                if layer.op <= c_ip || layer.ip >= c_op {
                    let outer = self.el("g");
                    return Ok(LayerNode {
                        outer,
                        mounted: outer,
                        dead: true,
                        record: None,
                        slot: 0,
                    });
                }
                let hides = layer.ip > c_ip || layer.op < c_op;
                (0u32, hides)
            }
            TimeCtx::Inner { parent, offset } => {
                self.caps |= Caps::TIMELINE;
                self.timelines
                    .push([parent as f64, offset, layer.ip, layer.op]);
                // A precomp's layers come and go on the precomp's own clock,
                // exactly as the document's do on its. Treating them as always
                // present drew every one of `car-4`'s four staggered states at
                // once, each frozen at the first frame it was authored for.
                let hides = layer.ip > c_ip || layer.op < c_op;
                (self.timelines.len() as u32, hides)
            }
        };

        // Reserve the record first: classifying this layer's properties stamps
        // the record index into every expression they carry.
        let record = if self.has_exprs {
            let idx = self.layers.len() as u32;
            let name = layer
                .n
                .and_then(|i| self.payload.st.get(i as usize).cloned());
            let n = name.map(|s| self.intern_name(&s));
            self.layers.push(LayerRecord {
                i: layer.i,
                n,
                ..Default::default()
            });
            // Parallel column, not a record field — see `SceneData::scopes`.
            self.scopes.push(self.scope);
            Some(idx)
        } else {
            None
        };
        let outer_rec = self.layer_rec;
        self.layer_rec = record;

        let outer = self.el("g");
        let has_content = layer.ty != 3;
        // A precomp layer is clipped to the composition it references.
        // lottie-web gives every `ty: 0` layer a `clipPath` of `w × h`, so
        // anything authored outside that frame does not draw; nothing here did,
        // and `lf20_GsFUFN` stages a character just off the left edge of its
        // 1200×800 precomp — we drew the part After Effects cropped.
        //
        // A nested `<svg>` is that rect with no id to mint: `overflow: hidden`
        // is the UA default for `svg:not(:root)`, and a viewport at 0,0 shifts
        // no coordinates. A `<clipPath>` would need a document-unique id, which
        // is what drags `runtime/ids.js` into every animation with a precomp —
        // measured at +425 B on `precomp_star_circle`, for the same rectangle.
        //
        // It goes on the content group, *inside* the layer transform, because
        // the frame is in the precomp's own coordinates. Same rule that sends a
        // track matte to an untransformed wrapper, read the other way round.
        let clip = match (layer.ty, layer.sw, layer.sh) {
            (0, Some(w), Some(h)) if w > 0 && h > 0 => Some((w, h)),
            _ => None,
        };
        let inner = if has_content {
            let n = self.el(if clip.is_some() { "svg" } else { "g" });
            if let Some((w, h)) = clip {
                self.set(n, "width", svg::n(w as f64));
                self.set(n, "height", svg::n(h as f64));
            }
            self.els[outer].children.push(n);
            n
        } else {
            outer
        };

        // A layer that is off for part of the composition gets a gate, and
        // everything inside it is skipped on frames where it is invisible.
        // The DISPLAY binding itself stays ungated — it is what turns the
        // group back on.
        let outer_gate = self.gate;
        if range_hidden && !inert {
            self.bind(
                op::DISPLAY,
                outer,
                vec![Arg::Num(layer.ip), Arg::Num(layer.op)],
                slot,
            );
            // The gate table is evaluated against the composition clock, so it
            // can only speak for layers running on it. A precomp's layers have
            // a slot of their own; `oDisplay` reads that slot and hides them
            // correctly, they just do not get the skip.
            if matches!(ctx, TimeCtx::Root) {
                self.gates.push([layer.ip, layer.op]);
                self.gate = self.gates.len() as u32;
            }
        }

        if let Some(rec) = record {
            // The layer table already holds these properties for the expression
            // runtime, so the bindings reference it instead of carrying a second
            // copy. Fully static transforms still bake into the markup.
            let dim = 3;
            let pp = layer.p.as_ref().map(|x| self.classify(x, dim));
            let ap = layer.a.as_ref().map(|x| self.classify(x, dim));
            let sp = layer.sc.as_ref().map(|x| self.classify(x, dim));
            let rp = layer.r.as_ref().map(|x| self.classify(x, 1));
            let op_p = layer.o.as_ref().map(|x| self.classify(x, 1));

            // The record itself is still built — `thisComp.layer('main')` reads
            // the table, not the document — but an inert layer's own group
            // gets nothing written to it.
            if !inert {
                self.emit_record_transform(
                    outer,
                    pp.clone(),
                    ap.clone(),
                    sp.clone(),
                    rp.clone(),
                    rec,
                    slot,
                );
            }

            if has_content {
                let o = op_p.clone().unwrap_or(Prop::Scalar(100.0));
                match o.as_scalar() {
                    Some(v) if o.is_static() => {
                        if (v - 100.0).abs() > 1e-6 {
                            self.set(inner, "opacity", svg::n(v / 100.0));
                        }
                    }
                    _ => self.bind(op::LAYER_OP, inner, vec![Arg::Num(rec as f64)], slot),
                }
            }

            let e = &mut self.layers[rec as usize];
            e.p = pp;
            e.a = ap;
            e.sc = sp;
            e.r = rp;
            e.o = op_p;
        } else {
            if !inert {
                self.emit_transform(
                    outer,
                    layer.p.as_ref(),
                    layer.a.as_ref(),
                    layer.sc.as_ref(),
                    layer.r.as_ref(),
                    slot,
                );
            }
            if has_content {
                self.emit_opacity(inner, layer.o.as_ref(), slot);
            }
        }

        // A mask on a layer with nothing in it clips nothing.
        if let Some(masks) = &layer.mk
            && !masks.is_empty()
            && !inert
        {
            self.emit_masks(inner, masks, slot)?;
        }

        if has_content && !inert {
            self.emit_effects(inner, layer);
        }

        match layer.ty {
            4 => {
                if let Some(shapes) = &layer.shapes {
                    for sr in shapes {
                        self.build_shape_ref(inner, sr, slot)?;
                    }
                }
            }
            1 => {
                let rect = self.el("rect");
                self.set(rect, "width", svg::n(layer.sw.unwrap_or(0) as f64));
                self.set(rect, "height", svg::n(layer.sh.unwrap_or(0) as f64));
                self.set(
                    rect,
                    "fill",
                    layer.cl.clone().unwrap_or_else(|| "#000".into()),
                );
                self.els[inner].children.push(rect);
            }
            2 => {
                if let Some(id) = layer.rf.clone() {
                    self.build_image(inner, &id);
                }
            }
            0 => {
                if let Some(id) = layer.rf.clone() {
                    let offset = layer.st.unwrap_or(0.0);
                    // Time remap replaces the precomp's clock outright: its
                    // inner time is a function of the outer one rather than a
                    // shift of it. Give it a slot of its own that the children
                    // then hang off, so the usual offset path is untouched.
                    let (slot, offset) = match self.remap_slot(layer, slot) {
                        Some(remapped) => (remapped, 0.0),
                        None => (slot, offset),
                    };
                    match self.instantiate(&id, slot, offset)? {
                        Some(node) => self.els[inner].children.push(node),
                        // Not instanceable — walk it inline, as before.
                        None => {
                            let inner_layers: Option<Vec<data::Layer>> = self
                                .payload
                                .a
                                .as_ref()
                                .and_then(|a| a.get(&id))
                                .and_then(|a| match a {
                                    data::Asset::Precomp { l } => Some(l.clone()),
                                    _ => None,
                                });
                            if let Some(l) = inner_layers {
                                let kids = self.build_layer_forest(
                                    &l,
                                    TimeCtx::Inner {
                                        parent: slot,
                                        offset,
                                    },
                                )?;
                                self.els[inner].children.extend(kids);
                            }
                        }
                    }
                }
            }
            _ => {}
        }

        if let Some(rec) = record {
            let ef = self.encode_effects(layer);
            let h = self.first_path_prop(layer);
            let e = &mut self.layers[rec as usize];
            e.ef = ef;
            e.h = h;
        }

        self.gate = outer_gate;
        self.layer_rec = outer_rec;
        Ok(LayerNode {
            outer,
            mounted: outer,
            dead: false,
            record,
            slot,
        })
    }

    /// An image layer: one `<image>` at the asset's natural size.
    ///
    /// Nothing about it can vary — the source, the size and the fit are all
    /// fixed at compile time — so it is pure markup with no binding at all.
    /// The layer's transform is what moves it, exactly as for a shape.
    ///
    /// `preserveAspectRatio` is `xMidYMid slice` because that is lottie-web's
    /// default for images, not SVG's (`xMidYMid meet`). The two differ the
    /// moment the layer's scale is not uniform, and the wrong one letterboxes
    /// where After Effects crops.
    fn build_image(&mut self, parent: usize, id: &str) {
        let Some(asset) = self.payload.a.as_ref().and_then(|a| a.get(id)) else {
            return;
        };
        let data::Asset::Image { u, p, w, h, e } = asset else {
            return;
        };
        // `e` marks `p` as a data URI, already complete. Otherwise the two
        // halves are a directory and a filename, and joining them is all the
        // resolution Lottie defines — anything further is the page's base URL.
        let href = match (e, p) {
            (_, None) => return,
            (1, Some(p)) => p.clone(),
            (_, Some(p)) => format!("{}{}", u.clone().unwrap_or_default(), p),
        };
        let (w, h) = (*w as f64, *h as f64);

        let img = self.el("image");
        self.set(img, "width", svg::n(w));
        self.set(img, "height", svg::n(h));
        self.set(img, "preserveAspectRatio", "xMidYMid slice");
        self.set(img, "href", href);
        self.els[parent].children.push(img);
    }

    /// Effects, in the shape `thisLayer.effect('name')('param')` reads.
    fn encode_effects(&mut self, layer: &data::Layer) -> Vec<Effect> {
        let Some(effects) = layer.ef.as_ref() else {
            return Vec::new();
        };
        effects
            .iter()
            .map(|e| Effect {
                nm: e.nm.clone(),
                mn: e.mn.clone(),
                ef: e
                    .ef
                    .iter()
                    .map(|p| EffectParam {
                        nm: p.nm.clone(),
                        mn: p.mn.clone(),
                        ty: p.ty,
                        v: p.v.map(svg::q),
                        p: p.p.as_ref().map(|prop| self.classify(prop, 1)),
                        p_off: 0,
                    })
                    .collect(),
            })
            .collect()
    }

    /// The layer's first path shape — what a bare `pointOnPath` reads.
    fn first_path_prop(&mut self, layer: &data::Layer) -> Option<Prop> {
        fn walk(payload: &crate::data::Payload, refs: &[ShapeRef]) -> Option<InlineProp> {
            for r in refs {
                match r {
                    ShapeRef::Group(g) => {
                        if let Some(p) = walk(payload, &g.c) {
                            return Some(p);
                        }
                    }
                    ShapeRef::Prim(p) => {
                        if let Some(Shape::Path { pt, .. }) = payload.s.get(p.s as usize) {
                            return Some(pt.clone());
                        }
                    }
                }
            }
            None
        }
        let shapes = layer.shapes.as_ref()?;
        let found = walk(self.payload, shapes)?;
        Some(self.classify(&found, 2))
    }

    /// Place one use of a precomp. The asset is planned the first time it is
    /// seen; a use is just a position, resolved once planning finishes.
    fn instantiate(&mut self, id: &str, parent_slot: u32, offset: f64) -> Result<Option<usize>> {
        let Some(asset) = self.plan_asset(id)? else {
            return Ok(None);
        };
        let node = self.el("g");
        self.els[node].instance = Some(asset);
        self.pending.push(super::instance::Nested {
            asset,
            node,
            el_base: 0,
            parent_slot,
            offset,
        });
        self.caps |= Caps::TIMELINE | Caps::INSTANCES;
        Ok(Some(node))
    }

    /// Allocate a clock slot driven by a layer's time remap, if it has one.
    ///
    /// Lottie stores the remap in **seconds**; the timeline works in frames,
    /// so the runtime scales by the composition frame rate. `[parent, 0, ip,
    /// op]` keeps the shape of a timeline entry, but the runtime takes the
    /// remap branch and never applies the offset or the loop.
    fn remap_slot(&mut self, layer: &data::Layer, parent: u32) -> Option<u32> {
        let tr = layer.tr.as_ref()?;
        let prop = self.classify(tr, 1);
        self.caps |= Caps::TIMELINE | Caps::TIME_REMAP;
        self.timelines
            .push([parent as f64, 0.0, layer.ip, layer.op]);
        let slot = self.timelines.len() as u32;
        self.remaps.resize(self.timelines.len(), None);
        self.remaps[slot as usize - 1] = Some(prop);
        Some(slot)
    }

    /// Plan a precomp once, with element, binding, record and clock indices
    /// local to it.
    fn plan_asset(&mut self, id: &str) -> Result<Option<u32>> {
        if let Some(&i) = self.asset_index.get(id) {
            return Ok(Some(i));
        }
        if !self.instance_precomps || self.uninstanceable.contains(id) {
            return Ok(None);
        }
        let layers: Vec<data::Layer> = match self.payload.a.as_ref().and_then(|a| a.get(id)) {
            Some(data::Asset::Precomp { l }) => l.clone(),
            _ => return Ok(None),
        };

        // Capture everything the walk produces, so it can be replayed per use
        // instead of re-walked.
        let bind_start = self.bindings.len();
        let rec_start = self.layers.len();
        let tl_start = self.timelines.len();
        let pending_start = self.pending.len();
        let outer_defs = self.defs_local.take();
        self.defs_local = Some(Vec::new());
        let outer_rec = self.layer_rec;

        let root = self.el("g");
        // The asset is one template with one root; pruning must not splice it
        // away the way it would an ordinary transparent group.
        self.els[root].pinned = true;
        let kids = self.build_layer_forest(
            &layers,
            TimeCtx::Inner {
                parent: 0,
                offset: 0.0,
            },
        )?;
        self.els[root].children.extend(kids);
        self.layer_rec = outer_rec;

        // Masks and gradients defined by this precomp live inside its body, so
        // each clone gets its own copy — and its own ids.
        let local_defs = self.defs_local.take().unwrap_or_default();
        self.defs_local = outer_defs;
        if !local_defs.is_empty() {
            let d = self.el("defs");
            self.els[d].children.extend(local_defs);
            self.els[root].children.push(d);
        }

        let mut children = vec![root];
        self.prune_all(&mut children);
        let root = match children.first() {
            Some(r) => *r,
            // Pruned away entirely — an empty precomp.
            None => {
                self.bindings.truncate(bind_start);
                self.slots.truncate(bind_start);
                self.bind_gate.truncate(bind_start);
                self.layers.truncate(rec_start);
                // `scopes` is parallel to `layers`; truncating one without the
                // other left every record planned afterwards reading its
                // neighbour's scope.
                self.scopes.truncate(rec_start);
                self.timelines.truncate(tl_start);
                self.pending.truncate(pending_start);
                return Ok(None);
            }
        };

        // Local element order, and the markup for one expansion. The
        // placeholder form is what the template table holds; nested precomps
        // stay placeholders there too, so a body is stored exactly once.
        let mut local = HashMap::new();
        let mut counter = 0u32;
        let mut markup = String::new();
        // No initial-frame bake for an asset body: it is stored once and
        // replayed per instance, and two instances of the same precomp sit at
        // different points on their own clocks, so there is no one frame to
        // bake. Nothing is lost — instancing and the standalone forms never
        // co-occur (see `backend::report`, and `compile_document`, which plans
        // fully expanded).
        self.emit_el(
            root,
            &mut markup,
            &mut counter,
            &mut local,
            &Default::default(),
        );
        let mut inline_counter = 0u32;
        let mut inline_markup = String::new();
        self.emit_inline_el(root, &mut inline_markup, &mut inline_counter);
        debug_assert_eq!(
            counter, inline_counter,
            "element count must not depend on form"
        );

        let mut bindings: Vec<Binding> = self.bindings.drain(bind_start..).collect();
        for b in &mut bindings {
            b.el_index = *local
                .get(&b.el)
                .expect("asset binding target survived pruning");
        }
        let _ = &bindings;
        let slots: Vec<u32> = self
            .slots
            .drain(bind_start..)
            .map(|s| s.saturating_sub(tl_start as u32))
            .collect();
        self.bind_gate.truncate(bind_start);

        // Everything that names a layer record has to be rebased: the record
        // indices are what make an asset relocatable, and they appear in three
        // places — parent links, the layer-transform bindings, and the layer a
        // `Prop::Expr` belongs to.
        let delta = rec_start as u32;
        let mut records: Vec<LayerRecord> = self.layers.drain(rec_start..).collect();
        // Drained with the records they belong to. Leaving them behind made
        // `scopes` longer than `layers`, so every document record planned after
        // the first instanced precomp read the wrong scope.
        let scopes: Vec<u32> = self.scopes.drain(rec_start..).collect();
        for r in &mut records {
            if let Some(pr) = r.pr {
                r.pr = Some(pr - delta);
            }
            for p in [&mut r.p, &mut r.a, &mut r.sc, &mut r.r, &mut r.o, &mut r.h] {
                if let Some(p) = p {
                    rebase_prop(p, delta);
                }
            }
            for e in &mut r.ef {
                for p in &mut e.ef {
                    if let Some(p) = &mut p.p {
                        rebase_prop(p, delta);
                    }
                }
            }
        }
        for b in &mut bindings {
            if b.op == op::LAYER_TX || b.op == op::LAYER_OP {
                if let Some(Arg::Num(n)) = b.args.first_mut() {
                    *n -= delta as f64;
                }
            }
            for a in &mut b.args {
                rebase_arg(a, delta);
            }
        }
        let timelines: Vec<[f64; 4]> = self
            .timelines
            .drain(tl_start..)
            .map(|t| {
                let parent = if t[0] == 0.0 {
                    0.0
                } else {
                    t[0] - tl_start as f64
                };
                [parent, t[1], t[2], t[3]]
            })
            .collect();

        // Nested uses, repositioned relative to this asset.
        let mut nested: Vec<super::instance::Nested> =
            self.pending.drain(pending_start..).collect();
        for nst in &mut nested {
            nst.el_base = local[&nst.node];
            // A parent slot of 0 stays 0: it means "the enclosing instance's
            // own clock", which is only known when the use is expanded.
            if nst.parent_slot != 0 {
                nst.parent_slot -= tl_start as u32;
            }
        }

        let template = self.templates.len() as u32;
        self.templates.push(inline_markup);

        let index = self.assets.len() as u32;
        self.assets.push(super::instance::AssetPlan {
            root,
            el_count: counter,
            markup,
            template,
            bindings,
            slots,
            records,
            scopes,
            timelines,
            nested,
        });
        self.asset_index.insert(id.to_string(), index);
        Ok(Some(index))
    }

    /// Mask `target` with `source`, per the layer's track-matte mode.
    ///
    /// 1 = alpha, 2 = alpha inverted, 3 = luma, 4 = luma inverted. Alpha modes
    /// use `mask-type="alpha"` so the matte's own coverage is what shows
    /// through — a stroked, trimmed matte like `lottie-logo`'s has no
    /// luminance to speak of, only alpha.
    ///
    /// Inversion goes through a filter rather than the usual white-rect trick:
    /// subtracting alpha is not something a mask can express, and forcing the
    /// matte's paint would mean overriding fills the compiler already baked
    /// into it. The filter needs an explicit `userSpaceOnUse` region — the
    /// default is the source's bounding box plus 10%, and outside it the
    /// inverted alpha would read as 0 and hide everything.
    fn matte_mask(&mut self, source: usize, tt: u8) -> String {
        let (cw, ch) = (self.payload.c.w as f64, self.payload.c.h as f64);
        let inverted = tt == 2 || tt == 4;
        let alpha = tt == 1 || tt == 2;

        let content = if inverted {
            let filter_id = self.invert_filter(alpha, cw, ch);
            let g = self.el("g");
            self.set(g, "filter", format!("url(#{filter_id})"));
            self.els[g].children.push(source);
            g
        } else {
            source
        };

        let id = self.next_id("t");
        let mask = self.el("mask");
        self.set(mask, "id", id.clone());
        self.set(mask, "mask-type", if alpha { "alpha" } else { "luminance" });
        self.set(mask, "maskUnits", "userSpaceOnUse");
        self.set(mask, "x", "0");
        self.set(mask, "y", "0");
        self.set(mask, "width", svg::n(cw));
        self.set(mask, "height", svg::n(ch));
        self.els[mask].children.push(content);
        self.add_def(mask);
        id
    }

    /// Layer effects that draw, as an SVG filter on the layer's content.
    ///
    /// Only `ADBE Fill` (`ty: 21`) so far, which is the whole of `lf20_W14Z1y`:
    /// fourteen layers of coloured squares that After Effects paints a single
    /// flat grey. Without it the squares kept their authored colours — a
    /// visibly wrong picture that the pixel gate scored at 0.45%, comfortably
    /// inside tolerance, because six small squares are not many pixels.
    ///
    /// It is one `feColorMatrix` that discards RGB and floods the constant
    /// instead, scaling alpha by the effect's own opacity — the same matrix
    /// `SVGFillFilter` writes, so the two agree to the digit. `support::scan`
    /// reports every other renderable effect type rather than dropping it.
    fn emit_effects(&mut self, target: usize, layer: &data::Layer) {
        let Some(effects) = &layer.ef else { return };
        for e in effects {
            if e.ty != EFFECT_FILL {
                continue;
            }
            // Parameters are addressed by position, the way `SVGFillFilter`
            // reads `effectElements[2]` and `[6]`; a match name is friendlier
            // and survives a reordering neither renderer would tolerate anyway.
            let color = e
                .ef
                .iter()
                .find(|p| p.ty == EFFECT_PARAM_COLOR)
                .and_then(|p| p.c);
            let Some(c) = color else { continue };
            let opacity = e
                .ef
                .iter()
                .find(|p| p.nm.as_deref() == Some("Opacity"))
                .and_then(|p| p.v)
                .unwrap_or(1.0);

            let id = self.next_id("f");
            let f = self.el("filter");
            self.set(f, "id", id.clone());
            let m = self.el("feColorMatrix");
            self.set(m, "type", "matrix");
            self.set(m, "color-interpolation-filters", "sRGB");
            self.set(
                m,
                "values",
                format!(
                    "0 0 0 0 {} 0 0 0 0 {} 0 0 0 0 {} 0 0 0 {} 0",
                    svg::n(c[0]),
                    svg::n(c[1]),
                    svg::n(c[2]),
                    svg::n(opacity)
                ),
            );
            self.els[f].children.push(m);
            self.add_def(f);
            self.set(target, "filter", format!("url(#{id})"));
        }
    }

    /// Put `target` under a mask.
    ///
    /// The mask goes on a wrapper, never on `target` itself. `target` carries
    /// the layer's own transform, and a mask is resolved in the user space that
    /// transform establishes — so putting it there would rotate and shift the
    /// matte along with the layer, clipping the wrong region entirely. The
    /// matte was authored as a sibling, in the composition's space, and an
    /// untransformed wrapper is what preserves that.
    fn masked(&mut self, target: usize, id: &str) -> usize {
        let wrapper = self.el("g");
        self.set(wrapper, "mask", format!("url(#{id})"));
        self.els[wrapper].children.push(target);
        wrapper
    }

    /// A filter that inverts alpha (for alpha mattes) or luminance (for luma
    /// mattes), over the whole composition rect.
    fn invert_filter(&mut self, alpha: bool, cw: f64, ch: f64) -> String {
        let id = self.next_id("i");
        let f = self.el("filter");
        self.set(f, "id", id.clone());
        self.set(f, "filterUnits", "userSpaceOnUse");
        self.set(f, "x", "0");
        self.set(f, "y", "0");
        self.set(f, "width", svg::n(cw));
        self.set(f, "height", svg::n(ch));
        let t = self.el("feComponentTransfer");
        for ch_name in if alpha {
            &["feFuncA"][..]
        } else {
            &["feFuncR", "feFuncG", "feFuncB"][..]
        } {
            let fun = self.el(ch_name);
            self.set(fun, "type", "table");
            self.set(fun, "tableValues", "1 0");
            self.els[t].children.push(fun);
        }
        self.els[f].children.push(t);
        self.add_def(f);
        id
    }

    /// Layer masks, as a `<clipPath>` where one will do and a `<mask>` where it
    /// will not.
    ///
    /// lottie-web picks the same way, in `MaskElement`: `clipPath` until some
    /// mask is subtractive, inverted or not fully opaque, and only then a
    /// luminance `mask`. The two are not interchangeable, and using a mask
    /// throughout cost real coverage — a `<mask>` with no `maskUnits` takes the
    /// default `objectBoundingBox` region, which is the *masked element's*
    /// bounding box plus 10%, so a mask reaching past its content is quietly
    /// cropped to it. The avatars in `krrt-9272cb41` came out with a visible
    /// ring of background where the clip should have run to the edge.
    ///
    /// A clip has no region and no soft edge, which is exactly the semantics of
    /// an additive opaque mask. Several contours in one `<clipPath>` **union**;
    /// they are separate `<path>` children for that reason and must stay
    /// separate — merging them into one `d` would let opposite windings cancel
    /// under `clip-rule="nonzero"`, which is why `merge_paths` does not walk in
    /// here.
    fn emit_masks(&mut self, target: usize, masks: &[data::LayerMask], slot: u32) -> Result<()> {
        let (cw, ch) = (self.payload.c.w as f64, self.payload.c.h as f64);
        // Bodymovin writes `"o": {"a":0,"k":100}` on a mask that is simply
        // opaque, so the test is the *value* and not the field's presence —
        // lottie-web spells it `properties[i].o.k !== 100`. Anything less than
        // fully opaque, or animated, needs real alpha and so needs a mask.
        let opaque = |o: &Option<InlineProp>| match o {
            None => true,
            Some(InlineProp::Static(Value::Scalar(v))) => (*v - 100.0).abs() < 1e-6,
            _ => false,
        };
        let hard = masks
            .iter()
            .all(|m| (m.m == "a" || m.m == "n") && !m.inv && opaque(&m.o));

        let id = self.next_id(if hard { "k" } else { "m" });
        let holder = self.el(if hard { "clipPath" } else { "mask" });
        self.set(holder, "id", id.clone());
        if !hard {
            self.set(holder, "mask-type", "luminance");
        }

        let has_subtract = masks.iter().any(|m| m.m == "s" || m.inv);
        if has_subtract {
            let bg = self.el("rect");
            self.set(bg, "width", svg::n(cw));
            self.set(bg, "height", svg::n(ch));
            self.set(bg, "fill", "#fff");
            self.els[holder].children.push(bg);
        }

        for m in masks {
            let subtract = m.m == "s" || m.inv;
            let p = self.el("path");
            if hard {
                self.set(p, "clip-rule", "nonzero");
            } else {
                self.set(p, "fill", if subtract { "#000" } else { "#fff" });
                self.set(p, "fill-rule", "evenodd");
            }
            let shape = self.classify(&m.pt, 2);
            match &shape {
                Prop::Path(path) => {
                    self.set(p, "d", path.to_d());
                }
                _ => {
                    self.caps |= Caps::PATH_D;
                    self.bind(op::SHAPE, p, vec![Arg::Prop(shape), Arg::Null], slot);
                }
            }
            self.els[holder].children.push(p);
        }

        self.add_def(holder);
        self.set(
            target,
            if hard { "clip-path" } else { "mask" },
            format!("url(#{id})"),
        );
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Transform / opacity
    // -----------------------------------------------------------------------

    fn emit_transform(
        &mut self,
        el: usize,
        p: Option<&InlineProp>,
        a: Option<&InlineProp>,
        s: Option<&InlineProp>,
        r: Option<&InlineProp>,
        slot: u32,
    ) {
        let dim = if self.keep_z { 3 } else { 2 };
        let pp = p
            .map(|x| self.classify(x, dim))
            .unwrap_or(Prop::Vector(vec![0.0, 0.0]));
        let ap = a
            .map(|x| self.classify(x, dim))
            .unwrap_or(Prop::Vector(vec![0.0, 0.0]));
        let sp = s
            .map(|x| self.classify(x, dim))
            .unwrap_or(Prop::Vector(vec![100.0, 100.0]));
        let rp = r.map(|x| self.classify(x, 1)).unwrap_or(Prop::Scalar(0.0));

        let rest_static = ap.is_static() && sp.is_static() && rp.is_static();

        if pp.is_static() && rest_static {
            let m = matrix(&pp, &ap, &sp, &rp);
            if !is_identity(&m) {
                self.set(el, "transform", svg::transform_str(&m));
            }
            return;
        }

        if rest_static {
            // Only the position moves: the linear part of the matrix and the
            // anchor contribution are constants, so the runtime only has to
            // concatenate two numbers onto a baked prefix.
            let zero = Prop::Vector(vec![0.0, 0.0]);
            let m = matrix(&zero, &ap, &sp, &rp);
            // An identity linear part needs no prefix at all: the binder
            // spells that case `translate(x,y)` on its own. That is the
            // majority of translate-only layers, and it was the last thing
            // keeping a string pool alive in an otherwise pool-free module.
            let prefix = if is_identity_linear(&m) {
                Arg::Null
            } else {
                Arg::Str(format!(
                    "matrix({},{},{},{},",
                    svg::nd(m[0], 1e5),
                    svg::nd(m[1], 1e5),
                    svg::nd(m[2], 1e5),
                    svg::nd(m[3], 1e5)
                ))
            };
            self.bind(
                op::TRANSLATE,
                el,
                vec![prefix, Arg::Num(m[4]), Arg::Num(m[5]), Arg::Prop(pp)],
                slot,
            );
            return;
        }

        self.bind(
            op::TRANSFORM,
            el,
            vec![Arg::Prop(pp), Arg::Prop(ap), Arg::Prop(sp), Arg::Prop(rp)],
            slot,
        );
    }

    fn emit_opacity(&mut self, el: usize, o: Option<&InlineProp>, slot: u32) {
        let op_prop = o
            .map(|x| self.classify(x, 1))
            .unwrap_or(Prop::Scalar(100.0));
        match op_prop.as_scalar() {
            Some(v) if op_prop.is_static() => {
                if (v - 100.0).abs() > 1e-6 {
                    self.set(el, "opacity", svg::n(v / 100.0));
                }
            }
            _ => self.bind(op::OPACITY, el, vec![Arg::Prop(op_prop)], slot),
        }
    }

    // -----------------------------------------------------------------------
    // Shapes
    // -----------------------------------------------------------------------

    fn build_shape_ref(&mut self, parent: usize, sr: &ShapeRef, slot: u32) -> Result<()> {
        match sr {
            ShapeRef::Group(g) => {
                let node = self.el("g");
                self.els[parent].children.push(node);
                self.emit_transform(
                    node,
                    g.p.as_ref(),
                    g.a.as_ref(),
                    g.sc.as_ref(),
                    g.r.as_ref(),
                    slot,
                );
                self.emit_opacity(node, g.o.as_ref(), slot);
                for child in &g.c {
                    self.build_shape_ref(node, child, slot)?;
                }
                Ok(())
            }
            ShapeRef::Prim(prim) => {
                let Some(shape) = self.payload.s.get(prim.s as usize).cloned() else {
                    return Ok(());
                };
                // A shape no fill and no stroke reaches is not drawn at all.
                // lottie-web writes a shape's `d` into its *styles'* elements
                // and creates none of its own, so a shape with no style is
                // simply never painted — where SVG would default `fill` to
                // black. After Effects exports one of these per composition as
                // an unpainted `Frame` rectangle, and `lf30_jrfgebuy` drew five
                // solid black boxes over its own artwork.
                if !self.paints(&prim.y) {
                    return Ok(());
                }
                let trim = prim
                    .tm
                    .and_then(|id| self.payload.y.get(id as usize).cloned());
                let node = self.build_primitive(&shape, trim.as_ref(), slot);
                let Some(node) = node else { return Ok(()) };
                self.els[parent].children.push(node);
                self.emit_styles(node, &prim.y, slot);
                Ok(())
            }
        }
    }

    /// Emit the element for one shape primitive, baking its geometry when
    /// every input is static.
    fn build_primitive(&mut self, shape: &Shape, trim: Option<&Style>, slot: u32) -> Option<usize> {
        let trimmed = trim.is_some();

        // A trimmed shape always renders through <path>, since trimming turns
        // any primitive into an arbitrary open curve.
        match shape {
            Shape::Rect { sz, ps, rd, .. } if !trimmed => {
                let szp = self.classify(sz, 2);
                let psp = self.classify(ps, 2);
                let rdp = self.classify(rd, 1);
                let el = self.el("rect");
                let baked = match (szp.as_vec(), psp.as_vec(), rdp.as_scalar()) {
                    (Some(sz), Some(ps), Some(r)) if rdp.is_static() => {
                        Some(([sz[0], sz[1]], [ps[0], ps[1]], r))
                    }
                    _ => None,
                };
                if let Some((sz, ps, r)) = baked {
                    self.set(el, "x", svg::n(ps[0] - sz[0] / 2.0));
                    self.set(el, "y", svg::n(ps[1] - sz[1] / 2.0));
                    self.set(el, "width", svg::n(sz[0]));
                    self.set(el, "height", svg::n(sz[1]));
                    if r > 0.0 {
                        let cr = r.min(sz[0] / 2.0).min(sz[1] / 2.0);
                        self.set(el, "rx", svg::n(cr));
                        self.set(el, "ry", svg::n(cr));
                    }
                } else {
                    self.bind(
                        op::RECT,
                        el,
                        vec![Arg::Prop(szp), Arg::Prop(psp), Arg::Prop(rdp)],
                        slot,
                    );
                }
                Some(el)
            }
            Shape::Ellipse { sz, ps, .. } if !trimmed => {
                let szp = self.classify(sz, 2);
                let psp = self.classify(ps, 2);
                let el = self.el("ellipse");
                let baked = match (szp.as_vec(), psp.as_vec()) {
                    (Some(sz), Some(ps)) => Some(([sz[0], sz[1]], [ps[0], ps[1]])),
                    _ => None,
                };
                if let Some((sz, ps)) = baked {
                    self.set(el, "cx", svg::n(ps[0]));
                    self.set(el, "cy", svg::n(ps[1]));
                    self.set(el, "rx", svg::n(sz[0] / 2.0));
                    self.set(el, "ry", svg::n(sz[1] / 2.0));
                } else {
                    self.bind(op::ELLIPSE, el, vec![Arg::Prop(szp), Arg::Prop(psp)], slot);
                }
                Some(el)
            }
            _ => {
                let el = self.el("path");
                let (geo_op, g) = self.geo_descriptor(shape);
                let baked = self.bake_geometry(shape, &g);
                match (baked, trim) {
                    // Fully static and untrimmed: the `d` is a literal.
                    (Some(path), None) => {
                        self.set(el, "d", path.to_d());
                    }
                    // Static source under a trim. If the trim range is static
                    // too, the trimmed outline itself is frame-invariant — so
                    // evaluate it here and the animation needs no trim code, no
                    // path serializer and no binding at all.
                    (Some(path), Some(t)) => {
                        if let Some(fixed) = static_trim_range(t) {
                            let flat = crate::eval::trim::Flat {
                                v: path.v.clone(),
                                i: path.i.clone(),
                                o: path.o.clone(),
                                c: path.c,
                            };
                            match crate::eval::trim::trim(&flat, fixed.0, fixed.1, fixed.2) {
                                crate::eval::trim::Trimmed::Whole => {
                                    self.set(el, "d", path.to_d());
                                }
                                crate::eval::trim::Trimmed::Empty => return None,
                                crate::eval::trim::Trimmed::Path(out) => {
                                    let baked = FlatPath {
                                        v: out.v,
                                        i: out.i,
                                        o: out.o,
                                        c: out.c,
                                    };
                                    self.set(el, "d", baked.to_d());
                                }
                            }
                            return Some(el);
                        }
                        // Range varies: ship the resolved source path so the
                        // runtime builds its arc-length table exactly once.
                        self.caps |= Caps::TRIM | Caps::PATH_D;
                        let trim_arg = self.trim_descriptor(t, slot);
                        self.bind(
                            op::SHAPE,
                            el,
                            vec![Arg::Prop(Prop::Path(path)), trim_arg],
                            slot,
                        );
                    }
                    (None, t) => {
                        // Only here does the runtime have to *build* geometry;
                        // a baked shape needs no generator.
                        self.caps |= Caps::PATH_D | runtime_geometry(shape);
                        let trim_arg = match t {
                            Some(t) => {
                                self.caps |= Caps::TRIM;
                                self.trim_descriptor(t, slot)
                            }
                            None => Arg::Null,
                        };
                        let mut args = g;
                        args.push(trim_arg);
                        self.bind(geo_op, el, args, slot);
                    }
                }
                Some(el)
            }
        }
    }

    /// A shape's geometry op and its arguments, each input classified.
    ///
    /// The op *is* the kind — there is no tag in the argument list, and none on
    /// the wire. That is also what lets [`Self::bake_geometry`] and
    /// `bake::bake_shape` index the same list at the same offsets; they read one
    /// shape's inputs at two different indices for as long as a tag led it, and
    /// that off-by-one family is what produced two of this pass's three bugs.
    fn geo_descriptor(&mut self, shape: &Shape) -> (u8, Vec<Arg>) {
        match shape {
            Shape::Path { pt, .. } => {
                let p = self.classify(pt, 2);
                if !p.is_static() {
                    self.caps |= Caps::PATH_KF;
                }
                (op::SHAPE, vec![Arg::Prop(p)])
            }
            Shape::Rect { sz, ps, rd, .. } => {
                let a = self.classify(sz, 2);
                let b = self.classify(ps, 2);
                let c = self.classify(rd, 1);
                (
                    op::SHAPE_RECT,
                    vec![Arg::Prop(a), Arg::Prop(b), Arg::Prop(c)],
                )
            }
            Shape::Ellipse { sz, ps, .. } => {
                let a = self.classify(sz, 2);
                let b = self.classify(ps, 2);
                (op::SHAPE_ELLIPSE, vec![Arg::Prop(a), Arg::Prop(b)])
            }
            Shape::PolyStar {
                sy,
                pt,
                ps,
                or,
                ir,
                rt,
                ..
            } => {
                let pt = self.classify(pt, 1);
                let ps = self.classify(ps, 2);
                let or = self.classify(or, 1);
                let ir = self.classify(ir, 1);
                let rt = self.classify(rt, 1);
                (
                    op::SHAPE_STAR,
                    vec![
                        // `Tag`, not `Num`: the star type is an enumeration, and
                        // a `Num` is a measurement the encoder scales by a
                        // thousand.
                        Arg::Tag(*sy as u32),
                        Arg::Prop(pt),
                        Arg::Prop(ps),
                        Arg::Prop(or),
                        Arg::Prop(ir),
                        Arg::Prop(rt),
                    ],
                )
            }
        }
    }

    /// Evaluate a geometry descriptor at compile time when every input turned
    /// out to be static.
    fn bake_geometry(&self, shape: &Shape, g: &[Arg]) -> Option<FlatPath> {
        let all_static = g.iter().all(|a| match a {
            Arg::Prop(p) => p.is_static(),
            _ => true,
        });
        if !all_static {
            return None;
        }
        let num = |i: usize| -> Option<f64> {
            match g.get(i)? {
                Arg::Prop(p) => p.as_scalar(),
                Arg::Tag(t) => Some(*t as f64),
                _ => None,
            }
        };
        let vec2 = |i: usize| -> Option<[f64; 2]> {
            match g.get(i)? {
                Arg::Prop(p) => {
                    let v = p.as_vec()?;
                    Some([*v.first()?, *v.get(1)?])
                }
                _ => None,
            }
        };
        let path = match shape {
            Shape::Path { .. } => match g.first()? {
                Arg::Prop(Prop::Path(p)) => return Some(p.clone()),
                _ => return None,
            },
            Shape::Rect { .. } => geometry::rect_to_path(vec2(1)?, vec2(0)?, num(2)?),
            Shape::Ellipse { .. } => geometry::ellipse_to_path(vec2(1)?, vec2(0)?),
            Shape::PolyStar { .. } => geometry::polystar_to_path(
                num(0)? as u8,
                vec2(2)?,
                num(1)?,
                num(3)?,
                num(4)?,
                num(5)?,
            ),
        };
        Some(FlatPath::from_parts(
            &path.vertices,
            &path.in_tangents,
            &path.out_tangents,
            path.closed,
        ))
    }

    fn trim_descriptor(&mut self, style: &Style, _slot: u32) -> Arg {
        match style {
            Style::TrimPath { s, e, o, m } => {
                let s = self.classify(s, 1);
                let e = self.classify(e, 1);
                let o = self.classify(o, 1);
                Arg::List(vec![
                    Arg::Prop(s),
                    Arg::Prop(e),
                    Arg::Prop(o),
                    Arg::Num(*m as f64),
                ])
            }
            _ => Arg::Null,
        }
    }

    // -----------------------------------------------------------------------
    // Styles
    // -----------------------------------------------------------------------

    /// Whether any of these styles puts paint on the canvas. A trim is a
    /// modifier, not a paint, so a shape carrying only one draws nothing.
    fn paints(&self, ids: &[u32]) -> bool {
        ids.iter().any(|id| {
            matches!(
                self.payload.y.get(*id as usize),
                Some(
                    Style::Fill { .. }
                        | Style::GradientFill { .. }
                        | Style::Stroke { .. }
                        | Style::GradientStroke { .. }
                )
            )
        })
    }

    fn emit_styles(&mut self, el: usize, ids: &[u32], slot: u32) {
        // The reference renderer applies styles back-to-front, so for any one
        // attribute the *first* matching style wins. Picking the first fill-ish
        // and first stroke-ish style reproduces that without the redundant
        // writes.
        let mut fill: Option<Style> = None;
        let mut stroke: Option<Style> = None;
        for id in ids {
            let Some(st) = self.payload.y.get(*id as usize) else {
                continue;
            };
            match st {
                Style::Fill { .. } | Style::GradientFill { .. } if fill.is_none() => {
                    fill = Some(st.clone());
                }
                Style::Stroke { .. } | Style::GradientStroke { .. } if stroke.is_none() => {
                    stroke = Some(st.clone());
                }
                _ => {}
            }
        }
        if fill.is_none() && stroke.is_some() {
            self.set(el, "fill", "none");
        }
        if let Some(f) = fill {
            self.emit_fill(el, &f, slot);
        }
        if let Some(s) = stroke {
            self.emit_stroke(el, &s, slot);
        }
    }

    fn emit_fill(&mut self, el: usize, style: &Style, slot: u32) {
        match style {
            Style::Fill { c, o } => {
                let cp = self.classify(c, 4);
                let op_p = self.classify(o, 1);
                if cp.is_static() && op_p.is_static() {
                    let color = cp.as_vec().unwrap_or(&[0.0, 0.0, 0.0, 1.0]);
                    self.set(el, "fill", svg::hex_color(color));
                    if let Some(a) = svg::paint_alpha(color, op_p.as_scalar().unwrap_or(100.0)) {
                        self.set(el, "fill-opacity", svg::n(a));
                    }
                } else {
                    self.bind(op::FILL, el, vec![Arg::Prop(cp), Arg::Prop(op_p)], slot);
                }
            }
            Style::GradientFill { g, o, s, e, gk, fr } => {
                let id = self.emit_gradient(g, *gk, s.as_ref(), e.as_ref(), slot);
                self.set(el, "fill", format!("url(#{id})"));
                if *fr == 2 {
                    self.set(el, "fill-rule", "evenodd");
                }
                let op_p = self.classify(o, 1);
                if op_p.is_static() {
                    let v = op_p.as_scalar().unwrap_or(100.0);
                    if v < 100.0 {
                        self.set(el, "fill-opacity", svg::n(v / 100.0));
                    }
                } else {
                    self.bind(op::FILL, el, vec![Arg::Null, Arg::Prop(op_p)], slot);
                }
            }
            _ => {}
        }
    }

    fn emit_stroke(&mut self, el: usize, style: &Style, slot: u32) {
        let (paint, opacity, width, lc, lj, ml) = match style {
            Style::Stroke {
                c,
                o,
                w,
                lc,
                lj,
                ml,
            } => {
                let cp = self.classify(c, 4);
                (
                    Some(cp),
                    self.classify(o, 1),
                    self.classify(w, 1),
                    *lc,
                    *lj,
                    *ml,
                )
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
            } => {
                let id = self.emit_gradient(g, *gk, s.as_ref(), e.as_ref(), slot);
                self.set(el, "stroke", format!("url(#{id})"));
                (
                    None,
                    self.classify(o, 1),
                    self.classify(w, 1),
                    *lc,
                    *lj,
                    *ml,
                )
            }
            _ => return,
        };

        // `butt` and `miter` are the SVG defaults — emitting them is pure waste.
        if lc == 2 {
            self.set(el, "stroke-linecap", "round");
        } else if lc == 3 {
            self.set(el, "stroke-linecap", "square");
        }
        if lj == 2 {
            self.set(el, "stroke-linejoin", "round");
        } else if lj == 3 {
            self.set(el, "stroke-linejoin", "bevel");
        }
        // 4 is the SVG default miter limit.
        if let Some(m) = ml
            && m > 0.0
            && (m - 4.0).abs() > 1e-6
        {
            self.set(el, "stroke-miterlimit", svg::n(m));
        }

        let paint_static = paint.as_ref().map(|p| p.is_static()).unwrap_or(true);
        if paint_static && opacity.is_static() && width.is_static() {
            if let Some(p) = &paint {
                let color = p.as_vec().unwrap_or(&[0.0, 0.0, 0.0, 1.0]);
                self.set(el, "stroke", svg::hex_color(color));
                if let Some(a) = svg::paint_alpha(color, opacity.as_scalar().unwrap_or(100.0)) {
                    self.set(el, "stroke-opacity", svg::n(a));
                }
            } else {
                let v = opacity.as_scalar().unwrap_or(100.0);
                if v < 100.0 {
                    self.set(el, "stroke-opacity", svg::n(v / 100.0));
                }
            }
            self.set(el, "stroke-width", svg::n(width.as_scalar().unwrap_or(0.0)));
        } else {
            let paint_arg = paint.map(Arg::Prop).unwrap_or(Arg::Null);
            self.bind(
                op::STROKE,
                el,
                vec![paint_arg, Arg::Prop(opacity), Arg::Prop(width)],
                slot,
            );
        }
    }

    fn emit_gradient(
        &mut self,
        g: &serde_json::Value,
        gk: u8,
        s: Option<&InlineProp>,
        e: Option<&InlineProp>,
        slot: u32,
    ) -> String {
        self.caps |= Caps::GRADIENT;
        let id = self.next_id("g");
        let radial = gk == 2;
        let node = self.el(if radial {
            "radialGradient"
        } else {
            "linearGradient"
        });
        self.set(node, "id", id.clone());
        self.set(node, "gradientUnits", "userSpaceOnUse");

        let sp = s
            .map(|x| self.classify(x, 2))
            .unwrap_or(Prop::Vector(vec![0.0, 0.0]));
        let ep = e
            .map(|x| self.classify(x, 2))
            .unwrap_or(Prop::Vector(vec![0.0, 0.0]));
        if sp.is_static() && ep.is_static() {
            let a = sp.as_vec().unwrap_or(&[0.0, 0.0]);
            let b = ep.as_vec().unwrap_or(&[0.0, 0.0]);
            self.set_gradient_geometry(node, radial, a[0], a[1], b[0], b[1]);
        } else {
            self.bind(
                op::GRADIENT,
                node,
                vec![Arg::Tag(gk as u32), Arg::Prop(sp), Arg::Prop(ep)],
                slot,
            );
        }

        // A keyframed ramp is one binding per stop: each stop's position and
        // colour is a four-component property, and the element count is fixed
        // because Lottie's stop count is.
        if let Some(ramp) = crate::eval::gradient::animated_ramp(g) {
            for values in &ramp.stops {
                let stop = self.el("stop");
                let prop = ramp_prop(&ramp, values);
                let p = self.classify(&prop, 4);
                self.bind(op::RAMP, stop, vec![Arg::Prop(p)], slot);
                self.els[node].children.push(stop);
            }
            self.add_def(node);
            return id;
        }

        // Stops: Lottie interleaves colour and alpha ramps at independent
        // positions, so they're resampled here onto the union of positions.
        if let Ok(stops) = crate::eval::gradient::resolve_stops(g) {
            for st in stops {
                let stop = self.el("stop");
                self.set(stop, "offset", svg::n(st.offset));
                self.set(
                    stop,
                    "stop-color",
                    svg::hex_color(&[st.color.r, st.color.g, st.color.b]),
                );
                if st.color.a < 1.0 {
                    self.set(stop, "stop-opacity", svg::n(st.color.a));
                }
                self.els[node].children.push(stop);
            }
        }

        self.add_def(node);
        id
    }

    fn set_gradient_geometry(
        &mut self,
        node: usize,
        radial: bool,
        sx: f64,
        sy: f64,
        ex: f64,
        ey: f64,
    ) {
        if radial {
            self.set(node, "cx", svg::n(sx));
            self.set(node, "cy", svg::n(sy));
            self.set(
                node,
                "r",
                svg::n(((ex - sx).powi(2) + (ey - sy).powi(2)).sqrt()),
            );
        } else {
            self.set(node, "x1", svg::n(sx));
            self.set(node, "y1", svg::n(sy));
            self.set(node, "x2", svg::n(ex));
            self.set(node, "y2", svg::n(ey));
        }
    }

    // -----------------------------------------------------------------------
    // Property classification
    // -----------------------------------------------------------------------

    /// Turn a payload property into its wire form, folding anything that turns
    /// out to be constant into a static value.
    pub(super) fn classify(&mut self, p: &InlineProp, dim: usize) -> Prop {
        match p {
            InlineProp::Static(v) => static_prop(v, dim),
            InlineProp::Animated(kf) => self.classify_keyframes(kf, dim),
            InlineProp::Expression(e) => {
                self.caps |= Caps::EXPRESSIONS | Caps::KEYFRAMES;
                let fallback = if let Some(kf) = &e.kf {
                    Some(Box::new(self.classify_keyframes(kf, dim)))
                } else {
                    e.fb.as_ref().map(|v| Box::new(static_prop(v, dim)))
                };
                Prop::Expr {
                    id: e.e,
                    fallback,
                    layer: self.layer_rec,
                }
            }
        }
    }

    fn classify_keyframes(&mut self, kf: &data::Keyframes, dim: usize) -> Prop {
        let n = kf.t.len();
        if n == 0 {
            return Prop::Scalar(0.0);
        }

        // Resolve Lottie's hold-with-empty-value keyframes now, so the runtime
        // interpolator never has to.
        let values: Vec<Value> = (0..n).map(|i| resolve_at(kf, i)).collect();
        if n == 1 || values.iter().all(|v| value_eq(v, &values[0])) {
            return static_prop(&values[0], dim);
        }

        let kind = match &values[0] {
            Value::Path(_) => AnimKind::Path,
            Value::Vector(_) if dim > 1 => AnimKind::Vector,
            _ => AnimKind::Scalar,
        };
        let dim = if kind == AnimKind::Scalar { 1 } else { dim };

        self.caps |= Caps::KEYFRAMES;
        if kind == AnimKind::Path {
            self.caps |= Caps::PATH_KF;
        }

        let segments = n - 1;
        let mut ez = Vec::with_capacity(segments);
        let mut any_easing = false;
        if let Some(oi) = &kf.oi {
            for pair in oi.iter().take(segments) {
                let idx = self.intern_easing(pair);
                if idx != 0 {
                    any_easing = true;
                }
                ez.push(idx);
            }
            while ez.len() < segments {
                ez.push(0);
            }
        }
        if any_easing {
            self.caps |= Caps::EASING;
        }

        // Held segments keep their start value until the next keyframe. Only
        // the first `segments` flags matter — the last keyframe has no segment.
        let holds: Option<Vec<u8>> = kf.h.as_ref().and_then(|h| {
            let flags: Vec<u8> = (0..segments)
                .map(|i| h.get(i).copied().unwrap_or(false) as u8)
                .collect();
            flags.iter().any(|f| *f == 1).then_some(flags)
        });
        if holds.is_some() {
            self.caps |= Caps::HOLD;
        }

        let (v, paths) = match kind {
            AnimKind::Path => (Vec::new(), values.iter().map(path_of).collect::<Vec<_>>()),
            _ => (
                values
                    .iter()
                    .flat_map(|v| flatten(v, dim))
                    .collect::<Vec<_>>(),
                Vec::new(),
            ),
        };

        // Legacy `e` end-values only need shipping when they disagree with the
        // following keyframe's start value.
        let mut end: Option<Vec<f64>> = None;
        let mut end_paths: Option<Vec<FlatPath>> = None;
        if let Some(e) = &kf.e {
            let differs = (0..segments).any(|i| match e.get(i).and_then(|x| x.as_ref()) {
                Some(ev) => !value_eq(ev, &values[i + 1]),
                None => false,
            });
            if differs {
                match kind {
                    AnimKind::Path => {
                        end_paths = Some(
                            (0..segments)
                                .map(|i| match e.get(i).and_then(|x| x.as_ref()) {
                                    Some(ev) => path_of(ev),
                                    None => path_of(&values[i + 1]),
                                })
                                .collect(),
                        );
                    }
                    _ => {
                        end = Some(
                            (0..segments)
                                .flat_map(|i| match e.get(i).and_then(|x| x.as_ref()) {
                                    Some(ev) => flatten(ev, dim),
                                    None => flatten(&values[i + 1], dim),
                                })
                                .collect(),
                        );
                    }
                }
            }
        }

        let mut to = None;
        let mut ti = None;
        if let (Some(a), Some(b)) = (&kf.to, &kf.ti) {
            let nonzero = a
                .iter()
                .chain(b.iter())
                .any(|v| v.iter().any(|x| *x != 0.0));
            if nonzero && kind == AnimKind::Vector {
                self.caps |= Caps::SPATIAL;
                to = Some(flatten_rows(a, segments, dim));
                ti = Some(flatten_rows(b, segments, dim));
            }
        }

        Prop::Anim(Box::new(Anim {
            kind,
            dim,
            t: kf.t.clone(),
            v,
            paths,
            end,
            end_paths,
            ez: if any_easing { Some(ez) } else { None },
            hold: holds,
            to,
            ti,
        }))
    }

    fn intern_easing(&mut self, pair: &data::EasingPair) -> u32 {
        let ox = first_component(&pair.o.x);
        let oy = first_component(&pair.o.y);
        let ix = first_component(&pair.i.x);
        let iy = first_component(&pair.i.y);
        // When y mirrors x on both handles the curve maps t to itself, so the
        // segment is linear no matter what the handles look like.
        if (ox - oy).abs() < 1e-9 && (ix - iy).abs() < 1e-9 {
            return 0;
        }
        let e = [ox, oy, ix, iy];
        let key = [ox.to_bits(), oy.to_bits(), ix.to_bits(), iy.to_bits()];
        if let Some(&i) = self.easing_index.get(&key) {
            return i;
        }
        let idx = self.easings.len() as u32;
        self.easings.push(e);
        self.easing_index.insert(key, idx);
        idx
    }
}

// ---------------------------------------------------------------------------
// Value helpers
// ---------------------------------------------------------------------------

/// Shift every layer reference in a property by `delta`.
/// One gradient stop's `[offset, r, g, b]` over time, as a wire property.
///
/// The times and easing are the ramp's, shared by every stop; only the values
/// differ. Hash-consing collapses the repeated columns downstream.
fn ramp_prop(ramp: &crate::eval::gradient::AnimatedRamp, values: &[[f64; 4]]) -> InlineProp {
    use crate::data::{EasingComponent, EasingHandle, EasingPair, Keyframes};
    let linear = ramp
        .easing
        .iter()
        .all(|e| e[0] == 0.0 && e[1] == 0.0 && e[2] == 1.0 && e[3] == 1.0);
    InlineProp::Animated(Keyframes {
        t: ramp.times.clone(),
        v: values.iter().map(|v| Value::Vector(v.to_vec())).collect(),
        e: None,
        oi: (!linear).then(|| {
            ramp.easing
                .iter()
                .map(|e| EasingPair {
                    o: EasingHandle {
                        x: EasingComponent::Scalar(e[0]),
                        y: EasingComponent::Scalar(e[1]),
                    },
                    i: EasingHandle {
                        x: EasingComponent::Scalar(e[2]),
                        y: EasingComponent::Scalar(e[3]),
                    },
                })
                .collect()
        }),
        to: None,
        ti: None,
        h: ramp.holds.iter().any(|&h| h).then(|| ramp.holds.clone()),
    })
}

/// The layer's parent, if it has one that is actually in the document.
fn parent_of(layers: &[data::Layer], dead: &[bool], i: usize) -> Option<usize> {
    match layers[i].pr {
        Some(pr) if (pr as usize) < layers.len() && !dead[pr as usize] => Some(pr as usize),
        _ => None,
    }
}

/// The layer's ancestors, **outermost first** — the order wrappers nest in.
fn ancestors(layers: &[data::Layer], dead: &[bool], i: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let mut next = parent_of(layers, dead, i);
    // A cyclic `pr` chain is not something the format allows, but it would
    // hang rather than fail if one arrived, so bound the walk.
    while let Some(a) = next {
        if out.len() >= layers.len() {
            break;
        }
        out.push(a);
        next = parent_of(layers, dead, a);
    }
    out.reverse();
    out
}

/// Would attaching every child into its parent's group leave the layers in the
/// order Lottie says they must be painted in?
///
/// Paint order is the layer list, reversed, and nothing else — parenting
/// contributes a transform. Nesting is the cheaper way to inherit that
/// transform, but it also moves the child to sit directly after its parent,
/// which is only harmless when that is where it already belonged. In
/// `car-5.json` eight facial features are parented to a null that is *last* in
/// the list, so nesting sank all eight to the bottom of the composition and
/// the face was drawn over its own eyes.
///
/// The check is a simulation rather than a rule about indices: it walks the
/// tree nesting would build and compares the sequence with the required one.
fn nesting_preserves_order(layers: &[data::Layer], dead: &[bool]) -> bool {
    let n = layers.len();

    let mut kids: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut order = Vec::with_capacity(n);
    let mut required = Vec::with_capacity(n);
    for i in (0..n).rev() {
        if dead[i] {
            continue;
        }
        required.push(i);
        match parent_of(layers, dead, i) {
            Some(pr) => kids[pr].push(i),
            None => order.push(i),
        }
    }

    // Pre-order over the forest nesting would produce, iteratively: a `pr`
    // chain deep enough to overflow a stack is a malformed file, not an error.
    let mut produced = Vec::with_capacity(n);
    let mut stack: Vec<usize> = order.into_iter().rev().collect();
    let mut guard = 0;
    while let Some(i) = stack.pop() {
        guard += 1;
        if guard > n {
            return false; // a cycle in `pr`; take the flat path, which cannot loop
        }
        produced.push(i);
        stack.extend(kids[i].iter().rev().copied());
    }

    produced == required
}

fn rebase_prop(p: &mut Prop, delta: u32) {
    if let Prop::Expr {
        layer, fallback, ..
    } = p
    {
        if let Some(l) = layer {
            *l -= delta;
        }
        if let Some(fb) = fallback {
            rebase_prop(fb, delta);
        }
    }
}

fn rebase_arg(a: &mut Arg, delta: u32) {
    match a {
        Arg::Prop(p) => rebase_prop(p, delta),
        Arg::List(items) => {
            for i in items {
                rebase_arg(i, delta);
            }
        }
        _ => {}
    }
}

/// The geometry generator a shape needs when it cannot be baked.
fn runtime_geometry(shape: &Shape) -> Caps {
    match shape {
        Shape::Rect { .. } => Caps::GEOM_RECT,
        Shape::Ellipse { .. } => Caps::GEOM_ELLIPSE,
        Shape::PolyStar { .. } => Caps::GEOM_STAR,
        Shape::Path { .. } => Caps::empty(),
    }
}

/// `(start, end, offset)` when a trim style never moves.
fn static_trim_range(style: &Style) -> Option<(f64, f64, f64)> {
    let Style::TrimPath { s, e, o, .. } = style else {
        return None;
    };
    let one = |p: &InlineProp| match p {
        InlineProp::Static(Value::Scalar(n)) => Some(*n),
        InlineProp::Static(Value::Vector(v)) => v.first().copied(),
        _ => None,
    };
    Some((one(s)?, one(e)?, one(o)?))
}

fn static_prop(v: &Value, dim: usize) -> Prop {
    match v {
        Value::Scalar(n) if dim <= 1 => Prop::Scalar(*n),
        Value::Scalar(n) => Prop::Vector(vec![*n; dim]),
        Value::Vector(vs) if dim <= 1 => Prop::Scalar(vs.first().copied().unwrap_or(0.0)),
        Value::Vector(vs) => Prop::Vector(fit(vs, dim)),
        Value::Path(p) => Prop::Path(FlatPath::from_parts(&p.v, &p.i, &p.o, p.c)),
    }
}

fn fit(vs: &[f64], dim: usize) -> Vec<f64> {
    let mut out = Vec::with_capacity(dim);
    for i in 0..dim {
        out.push(vs.get(i).copied().unwrap_or(0.0));
    }
    out
}

fn flatten(v: &Value, dim: usize) -> Vec<f64> {
    match v {
        Value::Scalar(n) if dim <= 1 => vec![*n],
        Value::Scalar(n) => vec![*n; dim],
        Value::Vector(vs) if dim <= 1 => vec![vs.first().copied().unwrap_or(0.0)],
        Value::Vector(vs) => fit(vs, dim),
        Value::Path(_) => vec![0.0; dim.max(1)],
    }
}

fn flatten_rows(rows: &[Vec<f64>], segments: usize, dim: usize) -> Vec<f64> {
    let mut out = Vec::with_capacity(segments * dim);
    for i in 0..segments {
        let row = rows.get(i);
        for d in 0..dim {
            out.push(row.and_then(|r| r.get(d)).copied().unwrap_or(0.0));
        }
    }
    out
}

fn path_of(v: &Value) -> FlatPath {
    match v {
        Value::Path(p) => FlatPath::from_parts(&p.v, &p.i, &p.o, p.c),
        _ => FlatPath::default(),
    }
}

fn value_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Scalar(x), Value::Scalar(y)) => (x - y).abs() < 1e-9,
        (Value::Vector(x), Value::Vector(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| (p - q).abs() < 1e-9)
        }
        (Value::Path(x), Value::Path(y)) => x == y,
        _ => false,
    }
}

fn is_value_empty(v: &Value) -> bool {
    matches!(v, Value::Vector(x) if x.is_empty())
}

/// Mirror of the frame evaluator's hold-keyframe resolution: Lottie writes an
/// empty vector at keyframes that only exist to terminate the previous
/// segment.
fn resolve_at(kf: &data::Keyframes, i: usize) -> Value {
    let n = kf.v.len();
    if i >= n {
        return kf.v.last().cloned().unwrap_or(Value::Scalar(0.0));
    }
    if !is_value_empty(&kf.v[i]) {
        return kf.v[i].clone();
    }
    if i > 0 {
        if let Some(e) =
            kf.e.as_ref()
                .and_then(|e| e.get(i - 1).and_then(|x| x.clone()))
            && !is_value_empty(&e)
        {
            return e;
        }
        if !is_value_empty(&kf.v[i - 1]) {
            return kf.v[i - 1].clone();
        }
    }
    for j in i + 1..n {
        if !is_value_empty(&kf.v[j]) {
            return kf.v[j].clone();
        }
    }
    kf.v[i].clone()
}

fn first_component(c: &data::EasingComponent) -> f64 {
    match c {
        data::EasingComponent::Scalar(n) => *n,
        data::EasingComponent::PerComponent(v) => v.first().copied().unwrap_or(0.0),
    }
}

// ---------------------------------------------------------------------------
// Matrix helpers
// ---------------------------------------------------------------------------

pub(super) fn matrix(p: &Prop, a: &Prop, s: &Prop, r: &Prop) -> [f64; 6] {
    let pv = p.as_vec().map(|v| [v[0], v[1]]).unwrap_or([0.0, 0.0]);
    let av = a.as_vec().map(|v| [v[0], v[1]]).unwrap_or([0.0, 0.0]);
    let sv = s.as_vec().map(|v| [v[0], v[1]]).unwrap_or([100.0, 100.0]);
    let rv = r.as_scalar().unwrap_or(0.0);
    let spec = crate::eval::transform::TransformSpec {
        position: pv,
        anchor: av,
        scale: sv,
        rotation: rv,
        opacity: 100.0,
    };
    spec.to_matrix().m
}

/// The 2×2 part is the identity: no rotation, no scale, no skew.
fn is_identity_linear(m: &[f64; 6]) -> bool {
    m[0] == 1.0 && m[1] == 0.0 && m[2] == 0.0 && m[3] == 1.0
}

pub(super) fn is_identity(m: &[f64; 6]) -> bool {
    (m[0] - 1.0).abs() < 1e-6
        && m[1].abs() < 1e-6
        && m[2].abs() < 1e-6
        && (m[3] - 1.0).abs() < 1e-6
        && m[4].abs() < 1e-6
        && m[5].abs() < 1e-6
}
