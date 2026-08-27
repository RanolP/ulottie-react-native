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

/// After Effects' effect type numbers — lottie-web keys its filter table on
/// them (`registerEffect(21, SVGFillFilter)` and friends).
const EFFECT_TINT: u32 = 20;
const EFFECT_FILL: u32 = 21;
const EFFECT_SHADOW: u32 = 25;
const EFFECT_BLUR: u32 = 29;
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
    /// it survives is here, where a precomp hands a clock to its children —
    /// as does `sr`, which lottie-web applies only at this same boundary
    /// (`renderedFrame = num / sr` in `CompElement`): a stretched ordinary
    /// layer's keyframes were already exported at composition time.
    Inner { parent: u32, offset: f64, rate: f64 },
}

/// The classified transform properties of one record-backed layer.
struct TxParts {
    p: Option<Prop>,
    a: Option<Prop>,
    s: Option<Prop>,
    r: Option<Prop>,
    sk: Option<Prop>,
    sa: Option<Prop>,
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
        // A matte source keeps its subtree however transparent it is — the
        // mask needs it. Everything else that is *statically* transparent
        // draws nothing, and `inert` below is the transform-only treatment.
        let matte_source_only = |i: usize| {
            layers[i].td.is_some_and(|t| t != 0)
                || layers.iter().any(|m| m.tp == Some(i as u32))
        };
        let transparent = |l: &data::Layer| {
            matches!(
                l.o.as_ref(),
                Some(data::InlineProp::Static(data::Value::Scalar(0.0)))
            )
        };
        let nested = nesting_preserves_order(layers, &dead);

        // A null draws nothing: it is in the document only so its children can
        // inherit its transform. Flat mode gives them wrappers of their own, so
        // the null's own group would be an empty node writing a matrix nobody
        // reads — starfish parents thirteen layers to one. Suppressing the
        // transform leaves the group empty and unpinned, which pruning removes.
        // `inert` marks a layer that will draw nothing and be inherited from
        // by children only: a null, and a layer whose opacity is statically
        // zero (`lottie_logo_2` parks nine white solids at `o: 0` purely as
        // transform anchors — rendering them as `opacity="0"` rects matched
        // pixels and nothing else, and carried geometry no gate could see).
        // A matte source is content even when transparent, so it stays.
        let inert = |i: usize| {
            !nested
                && (layers[i].ty == 3
                    || (transparent(&layers[i]) && !matte_source_only(i)))
        };

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
                if let (Some(pr), Some(rec)) = (l.pr, nodes[i].record)
                    && let Some(prec) = nodes.get(pr as usize).and_then(|n| n.record) {
                        self.layers[rec as usize].pr = Some(prec);
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
            let Some(tt) = layers[i].tt.filter(|&t| t != 0) else {
                continue;
            };
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
    fn emit_record_transform(&mut self, el: usize, parts: TxParts, rec: u32, slot: u32) {
        let TxParts { p, a, s, r, sk, sa } = parts;
        let dp = p.unwrap_or(Prop::Vector(vec![0.0, 0.0, 0.0]));
        let da = a.unwrap_or(Prop::Vector(vec![0.0, 0.0, 0.0]));
        let ds = s.unwrap_or(Prop::Vector(vec![100.0, 100.0, 100.0]));
        let dr = r.unwrap_or(Prop::Scalar(0.0));
        let dsk = sk.unwrap_or(Prop::Scalar(0.0));
        let dsa = sa.unwrap_or(Prop::Scalar(0.0));
        let skew_static = dsk.is_static() && dsa.is_static();
        if dp.is_static() && da.is_static() && ds.is_static() && dr.is_static() && skew_static {
            let m = matrix_skewed(
                &dp,
                &da,
                &ds,
                &dr,
                dsk.as_scalar().unwrap_or(0.0),
                dsa.as_scalar().unwrap_or(0.0),
            );
            if !is_identity(&m) {
                self.set(el, "transform", svg::transform_str(&m));
            }
        } else if !(skew_static && dsk.as_scalar().unwrap_or(0.0) == 0.0) {
            // A live skew takes the direct-prop op — the record table does
            // not carry skew, and nothing reads it through an expression.
            self.bind(
                op::TRANSFORM_SKEW,
                el,
                vec![
                    Arg::Prop(dp),
                    Arg::Prop(da),
                    Arg::Prop(ds),
                    Arg::Prop(dr),
                    Arg::Prop(dsk),
                    Arg::Prop(dsa),
                ],
                slot,
            );
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
                let sk = layers[a].sk.as_ref().map(|x| self.classify(x, 1));
                let sa = layers[a].sa.as_ref().map(|x| self.classify(x, 1));
                self.emit_record_transform(
                    el,
                    TxParts { p, a: an, s, r, sk, sa },
                    rec,
                    nodes[a].slot,
                );
            }
            None => self.emit_transform_skewed(
                el,
                layers[a].p.as_ref(),
                layers[a].a.as_ref(),
                layers[a].sc.as_ref(),
                layers[a].r.as_ref(),
                layers[a].sk.as_ref(),
                layers[a].sa.as_ref(),
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
            TimeCtx::Inner { parent, offset, rate } => {
                self.caps |= Caps::TIMELINE;
                // Same decision-boundary treatment as the display gate below.
                let gate_time = |x: f64| (x * 1000.0).ceil() / 1000.0;
                self.timelines.push([
                    parent as f64,
                    offset,
                    rate,
                    gate_time(layer.ip),
                    gate_time(layer.op),
                ]);
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
        // Blend mode, the same spelling lottie-web writes
        // (`setBlendMode`): CSS `mix-blend-mode` on the layer's own group —
        // inside any matte wrapper, so the mode applies to the layer against
        // what is underneath it, exactly as in AE. Only 1–15 exist; 0 is
        // normal and never written.
        if let Some(bm) = layer.bm {
            const MODES: [&str; 16] = [
                "", "multiply", "screen", "overlay", "darken", "lighten", "color-dodge",
                "color-burn", "hard-light", "soft-light", "difference", "exclusion",
                "hue", "saturation", "color", "luminosity",
            ];
            if let Some(mode) = MODES.get(bm as usize).filter(|m| !m.is_empty()) {
                self.set(outer, "style", format!("mix-blend-mode:{mode}"));
            }
        }
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
            (0, Some(w), Some(h)) if w > 0.0 && h > 0.0 => Some((w, h)),
            _ => None,
        };
        let inner = if has_content {
            let n = self.el(if clip.is_some() { "svg" } else { "g" });
            if let Some((w, h)) = clip {
                self.set(n, "width", svg::n(w));
                self.set(n, "height", svg::n(h));
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
        //
        // In/out points ship as their *decision boundary* at wire precision:
        // the ×1000 quantization would round `ip: 35.0000014` to 35 and turn
        // "starts just after frame 35" into "on at 35" — while lottie-web,
        // whose first frame is `Math.round(ip)`, opens that composition on a
        // blank frame (`loading_indicator`). Ceiling preserves the strict
        // comparison for fractional bounds and is exact for whole ones.
        let gate_time = |x: f64| (x * 1000.0).ceil() / 1000.0;
        let outer_gate = self.gate;
        if range_hidden && !inert {
            self.bind(
                op::DISPLAY,
                outer,
                vec![Arg::Num(gate_time(layer.ip)), Arg::Num(gate_time(layer.op))],
                slot,
            );
            // The gate table is evaluated against the composition clock, so it
            // can only speak for layers running on it. A precomp's layers have
            // a slot of their own; `oDisplay` reads that slot and hides them
            // correctly, they just do not get the skip.
            if matches!(ctx, TimeCtx::Root) {
                self.gates.push([gate_time(layer.ip), gate_time(layer.op)]);
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
                let sk = layer.sk.as_ref().map(|x| self.classify(x, 1));
                let sa = layer.sa.as_ref().map(|x| self.classify(x, 1));
                self.emit_record_transform(
                    outer,
                    TxParts {
                        p: pp.clone(),
                        a: ap.clone(),
                        s: sp.clone(),
                        r: rp.clone(),
                        sk,
                        sa,
                    },
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
                self.emit_transform_skewed(
                    outer,
                    layer.p.as_ref(),
                    layer.a.as_ref(),
                    layer.sc.as_ref(),
                    layer.r.as_ref(),
                    layer.sk.as_ref(),
                    layer.sa.as_ref(),
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
            self.emit_effects(inner, layer, slot);
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
                self.set(rect, "width", svg::n(layer.sw.unwrap_or(0.0)));
                self.set(rect, "height", svg::n(layer.sh.unwrap_or(0.0)));
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
                    let rate = if layer.sr == 0.0 { 1.0 } else { layer.sr };
                    // Time remap replaces the precomp's clock outright: its
                    // inner time is a function of the outer one rather than a
                    // shift of it. Give it a slot of its own that the children
                    // then hang off, so the usual offset path is untouched.
                    let (slot, offset, rate) = match self.remap_slot(layer, slot) {
                        Some(remapped) => (remapped, 0.0, 1.0),
                        None => (slot, offset, rate),
                    };
                    match self.instantiate(&id, slot, offset, rate)? {
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
                                        rate,
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
        let (w, h) = (*w, *h);

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
    fn instantiate(
        &mut self,
        id: &str,
        parent_slot: u32,
        offset: f64,
        rate: f64,
    ) -> Result<Option<usize>> {
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
            rate,
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
            .push([parent as f64, 0.0, 1.0, layer.ip, layer.op]);
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
                rate: 1.0,
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
            for p in [&mut r.p, &mut r.a, &mut r.sc, &mut r.r, &mut r.o, &mut r.h].into_iter().flatten() {
                rebase_prop(p, delta);
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
            if (b.op == op::LAYER_TX || b.op == op::LAYER_OP)
                && let Some(Arg::Num(n)) = b.args.first_mut() {
                    *n -= delta as f64;
                }
            for a in &mut b.args {
                rebase_arg(a, delta);
            }
        }
        let timelines: Vec<[f64; 5]> = self
            .timelines
            .drain(tl_start..)
            .map(|t| {
                let parent = if t[0] == 0.0 {
                    0.0
                } else {
                    t[0] - tl_start as f64
                };
                [parent, t[1], t[2], t[3], t[4]]
            })
            .collect();

        // Nested uses, repositioned relative to this asset. One whose
        // placeholder did not survive pruning — the whole subtree around it
        // was invisible — renders nothing, and keeping it would index an
        // element the template no longer has.
        let mut nested: Vec<super::instance::Nested> =
            self.pending.drain(pending_start..).collect();
        nested.retain(|nst| local.contains_key(&nst.node));
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
    /// through — a stroked, trimmed matte like `lottie_logo_1`'s has no
    /// luminance to speak of, only alpha.
    ///
    /// Inversion goes through a filter rather than the usual white-rect trick:
    /// subtracting alpha is not something a mask can express, and forcing the
    /// matte's paint would mean overriding fills the compiler already baked
    /// into it. The filter needs an explicit `userSpaceOnUse` region — the
    /// default is the source's bounding box plus 10%, and outside it the
    /// inverted alpha would read as 0 and hide everything.
    fn matte_mask(&mut self, source: usize, tt: u8) -> String {
        let (cw, ch) = (self.payload.c.w, self.payload.c.h);
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
    /// Layer effects that draw, as one SVG filter chaining every supported
    /// effect's primitives — exactly the shape lottie-web's `SVGEffects`
    /// builds. Fill, tint, drop shadow and gaussian blur are transcriptions
    /// of `SVGFillFilter`, `SVGTintFilter`, `SVGDropShadowEffect` and
    /// `SVGGaussianBlurEffect`, quirks intact (drop shadow keeps the default
    /// `0%/100%` region that clips it to the element's own box; the blur is
    /// what widens the region, because the last constructor to touch the
    /// shared filter wins there too). Effect types lottie-web never
    /// registered — Bulge, Warp, expression sliders — are skipped without a
    /// finding, because the reference skips them the same way.
    fn emit_effects(&mut self, target: usize, layer: &data::Layer, slot: u32) {
        let Some(effects) = &layer.ef else { return };
        let supported: Vec<&data::Effect> = effects
            .iter()
            .filter(|e| matches!(e.ty, EFFECT_TINT | EFFECT_FILL | EFFECT_SHADOW | EFFECT_BLUR))
            .collect();
        if supported.is_empty() {
            return;
        }

        let id = self.next_id("f");
        let f = self.el("filter");
        self.set(f, "id", id.clone());
        if supported.iter().any(|e| e.ty == EFFECT_SHADOW) {
            // lottie-web's default `filterSize` — the shadow clips to it.
            self.set(f, "x", "0%");
            self.set(f, "y", "0%");
            self.set(f, "width", "100%");
            self.set(f, "height", "100%");
        }
        if supported.iter().any(|e| e.ty == EFFECT_BLUR) {
            self.set(f, "x", "-100%");
            self.set(f, "y", "-100%");
            self.set(f, "width", "300%");
            self.set(f, "height", "300%");
        }

        // The running input: `SourceGraphic` until an effect merges, then that
        // effect's named result. Only shadow and tint reference it by name.
        let mut source = String::from("SourceGraphic");
        // The previous effect's last primitive, so a by-name reference can
        // give it the `result` it needs.
        let mut last_prim: Option<usize> = None;
        for (k, e) in supported.iter().enumerate() {
            let name = |suffix: &str| format!("{id}_{k}{suffix}");
            // A by-name consumer needs the running source to *have* a name.
            let mut named_source = source.clone();
            if matches!(e.ty, EFFECT_TINT | EFFECT_SHADOW)
                && source != "SourceGraphic"
                && let Some(p) = last_prim
            {
                named_source = format!("{id}_{}s", k);
                self.set(p, "result", named_source.clone());
            }
            match e.ty {
                EFFECT_FILL => {
                    // `SVGFillFilter`: one matrix that floods the colour and
                    // scales alpha by the effect's own opacity.
                    let Some(c) = e.ef.iter().find(|p| p.ty == EFFECT_PARAM_COLOR).and_then(|p| p.c)
                    else {
                        continue;
                    };
                    let opacity = e
                        .ef
                        .iter()
                        .find(|p| p.nm.as_deref() == Some("Opacity"))
                        .and_then(|p| p.v)
                        .unwrap_or(1.0);
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
                    last_prim = Some(m);
                    source = String::new(); // implicit chain from here on
                }
                EFFECT_TINT => {
                    // `SVGTintFilter`: luminance scaled by intensity, mapped
                    // onto the black→white ramp, merged over the source.
                    let black = e.ef.first().and_then(|p| p.c).unwrap_or([0.0; 4]);
                    let white = e.ef.get(1).and_then(|p| p.c).unwrap_or([1.0, 1.0, 1.0, 1.0]);
                    let opacity = e.ef.get(2).and_then(|p| p.v).unwrap_or(100.0) / 100.0;
                    let lin = self.el("feColorMatrix");
                    self.set(lin, "type", "matrix");
                    self.set(lin, "color-interpolation-filters", "linearRGB");
                    self.set(
                        lin,
                        "values",
                        format!(
                            // lottie-web's `linearFilterValue`, verbatim: an
                            // equal-weight average, not a real luma vector.
                            "0.3333 0.3333 0.3333 0 0 0.3333 0.3333 0.3333 0 0 0.3333 0.3333 0.3333 0 0 0 0 0 {} 0",
                            svg::n(opacity)
                        ),
                    );
                    self.set(lin, "result", name("t1"));
                    self.els[f].children.push(lin);
                    let m = self.el("feColorMatrix");
                    self.set(m, "type", "matrix");
                    self.set(m, "color-interpolation-filters", "sRGB");
                    self.set(
                        m,
                        "values",
                        format!(
                            "{} 0 0 0 {} {} 0 0 0 {} {} 0 0 0 {} 0 0 0 1 0",
                            svg::n(white[0] - black[0]),
                            svg::n(black[0]),
                            svg::n(white[1] - black[1]),
                            svg::n(black[1]),
                            svg::n(white[2] - black[2]),
                            svg::n(black[2])
                        ),
                    );
                    self.set(m, "result", name("t2"));
                    self.els[f].children.push(m);
                    let merged = name("");
                    for input in [named_source.as_str(), &name("t1"), &name("t2")] {
                        let n = self.el("feMergeNode");
                        self.set(n, "in", input);
                        let merge = match self.els[f].children.last() {
                            Some(&m2) if self.els[m2].tag == "feMerge" => m2,
                            _ => {
                                let m2 = self.el("feMerge");
                                self.set(m2, "result", merged.clone());
                                self.els[f].children.push(m2);
                                m2
                            }
                        };
                        self.els[merge].children.push(n);
                    }
                    last_prim = self.els[f].children.last().copied();
                    source = merged;
                }
                EFFECT_SHADOW => {
                    // `SVGDropShadowEffect`, primitive for primitive. Params
                    // by position: colour, opacity (0–255), direction,
                    // distance, softness.
                    let color = e.ef.first().and_then(|p| p.c).unwrap_or([0.0; 4]);
                    let blur = self.el("feGaussianBlur");
                    self.set(blur, "in", "SourceAlpha");
                    self.set(blur, "result", name("d1"));
                    let softness = self.fx_scalar(e, 4);
                    match softness.as_scalar() {
                        Some(v) if softness.is_static() => {
                            self.set(blur, "stdDeviation", svg::n(v / 4.0));
                        }
                        _ => {
                            self.caps |= Caps::FX;
                            self.bind(op::FX_STD, blur, vec![Arg::Prop(softness)], slot);
                        }
                    }
                    self.els[f].children.push(blur);
                    let off = self.el("feOffset");
                    self.set(off, "in", name("d1"));
                    self.set(off, "result", name("d2"));
                    let dir = self.fx_scalar(e, 2);
                    let dist = self.fx_scalar(e, 3);
                    match (dir.as_scalar(), dist.as_scalar()) {
                        (Some(a), Some(d)) if dir.is_static() && dist.is_static() => {
                            let rad = (a - 90.0).to_radians();
                            self.set(off, "dx", svg::n(d * rad.cos()));
                            self.set(off, "dy", svg::n(d * rad.sin()));
                        }
                        _ => {
                            self.caps |= Caps::FX;
                            self.bind(
                                op::FX_OFFSET,
                                off,
                                vec![Arg::Prop(dir), Arg::Prop(dist)],
                                slot,
                            );
                        }
                    }
                    self.els[f].children.push(off);
                    let flood = self.el("feFlood");
                    self.set(
                        flood,
                        "flood-color",
                        svg::hex_color(&[color[0], color[1], color[2], 1.0]),
                    );
                    let opacity = self.fx_scalar(e, 1);
                    match opacity.as_scalar() {
                        Some(v) if opacity.is_static() => {
                            self.set(flood, "flood-opacity", svg::n(v / 255.0));
                        }
                        _ => {
                            self.caps |= Caps::FX;
                            self.bind(op::FX_FLOOD_O, flood, vec![Arg::Prop(opacity)], slot);
                        }
                    }
                    self.set(flood, "result", name("d3"));
                    self.els[f].children.push(flood);
                    let comp = self.el("feComposite");
                    self.set(comp, "in", name("d3"));
                    self.set(comp, "in2", name("d2"));
                    self.set(comp, "operator", "in");
                    self.set(comp, "result", name("d4"));
                    self.els[f].children.push(comp);
                    let merged = name("");
                    let merge = self.el("feMerge");
                    self.set(merge, "result", merged.clone());
                    for input in [&name("d4"), named_source.as_str()] {
                        let n = self.el("feMergeNode");
                        self.set(n, "in", input);
                        self.els[merge].children.push(n);
                    }
                    self.els[f].children.push(merge);
                    last_prim = Some(merge);
                    source = merged;
                }
                EFFECT_BLUR => {
                    // `SVGGaussianBlurEffect`: sigma is blurriness × 0.3, the
                    // dimensions switch zeroes one axis, and edge mode 1 wraps.
                    let blur = self.el("feGaussianBlur");
                    let sigma = self.fx_scalar(e, 0);
                    let dims = e.ef.get(1).and_then(|p| p.v).unwrap_or(1.0) as u32;
                    match sigma.as_scalar() {
                        Some(v) if sigma.is_static() => {
                            let s = v * 0.3;
                            let sx = if dims == 3 { 0.0 } else { s };
                            let sy = if dims == 2 { 0.0 } else { s };
                            self.set(
                                blur,
                                "stdDeviation",
                                format!("{} {}", svg::n(sx), svg::n(sy)),
                            );
                        }
                        _ => {
                            self.caps |= Caps::FX;
                            self.bind(
                                op::FX_BLUR,
                                blur,
                                vec![Arg::Prop(sigma), Arg::Tag(dims)],
                                slot,
                            );
                        }
                    }
                    let edge = e.ef.get(2).and_then(|p| p.v).unwrap_or(0.0);
                    self.set(blur, "edgeMode", if edge == 1.0 { "wrap" } else { "duplicate" });
                    self.els[f].children.push(blur);
                    last_prim = Some(blur);
                    source = String::new();
                }
                _ => {}
            }
        }

        self.add_def(f);
        self.set(target, "filter", format!("url(#{id})"));
    }

    /// One effect parameter as a classified scalar property — the animated
    /// form when the export carries keyframes, the static value otherwise.
    fn fx_scalar(&mut self, e: &data::Effect, i: usize) -> Prop {
        match e.ef.get(i) {
            Some(p) => match &p.p {
                Some(inline) => self.classify(inline, 1),
                None => Prop::Scalar(p.v.unwrap_or(0.0)),
            },
            None => Prop::Scalar(0.0),
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
    /// Layer masks, mirroring lottie-web's `MaskElement` decision for
    /// decision:
    ///
    /// * `clipPath` until some mask is non-Add, inverted, or not fully opaque
    ///   — a `<mask>`'s default region quietly crops, so the hard form is
    ///   never given up casually.
    /// * The full-frame white rect exists **only when the first counted mask
    ///   is Subtract or Intersect** (`count === 0` in their constructor).
    ///   Adding it under a leading Add turned "A minus S" into "everything
    ///   minus S" — `Tests_MaskInv`, 71.6% wrong, was exactly that.
    /// * A Subtract paints black; *every* other counted mode paints white —
    ///   including `f`, which lottie-web never special-cases.
    /// * `n` masks draw nothing at all (lottie-web parks the path in defs).
    /// * Intersect wraps everything accumulated so far in a `<g>` alpha-masked
    ///   by its own path.
    /// * An inverted mask is the composition-sized rect plus the path in one
    ///   `d` — winding inversion, their `createLayerSolidPath`. Static paths
    ///   only; an inverted animated path is the one remaining refusal.
    fn emit_masks(&mut self, target: usize, masks: &[data::LayerMask], slot: u32) -> Result<()> {
        let (cw, ch) = (self.payload.c.w, self.payload.c.h);
        // Bodymovin writes `"o": {"a":0,"k":100}` on a mask that is simply
        // opaque, so the test is the *value* and not the field's presence —
        // lottie-web spells it `properties[i].o.k !== 100`.
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
        let outer = holder;
        self.set(holder, "id", id.clone());
        if !hard {
            self.set(holder, "mask-type", "luminance");
        }

        let mut count = 0usize;
        for m in masks {
            if m.m == "n" {
                continue;
            }
            if (m.m == "s" || m.m == "i") && count == 0 {
                let bg = self.el("rect");
                self.set(bg, "width", svg::n(cw));
                self.set(bg, "height", svg::n(ch));
                self.set(bg, "fill", "#ffffff");
                self.els[holder].children.push(bg);
            }
            count += 1;
            let p = self.el("path");
            if hard {
                self.set(p, "clip-rule", "nonzero");
            } else {
                self.set(p, "fill", if m.m == "s" { "#000" } else { "#fff" });
            }
            match &m.o {
                Some(o) if !opaque(&m.o) => {
                    let op_p = self.classify(o, 1);
                    match op_p.as_scalar() {
                        Some(v) if op_p.is_static() => {
                            self.set(p, "fill-opacity", svg::n(v / 100.0));
                        }
                        // The colourless fill op writes exactly
                        // `fill-opacity` — the same binding a gradient's
                        // animated opacity uses.
                        _ => self.bind(op::FILL, p, vec![Arg::Null, Arg::Prop(op_p)], slot),
                    }
                }
                _ => {}
            }
            let shape = self.classify(&m.pt, 2);
            match &shape {
                Prop::Path(path) if m.inv => {
                    // `createLayerSolidPath` + the path, one `d`: the rect
                    // winds once around everything, the contour cuts its hole.
                    self.set(
                        p,
                        "d",
                        format!(
                            "M0,0h{}v{}h-{}v-{}z{}",
                            svg::n(cw),
                            svg::n(ch),
                            svg::n(cw),
                            svg::n(ch),
                            path.to_d()
                        ),
                    );
                }
                Prop::Path(path) => {
                    self.set(p, "d", path.to_d());
                }
                _ => {
                    self.caps |= Caps::PATH_D;
                    self.bind(op::SHAPE, p, vec![Arg::Prop(shape), Arg::Null], slot);
                }
            }
            if m.m == "i" {
                // Everything so far, seen through this path.
                let mid = format!("{id}_{count}");
                let am = self.el("mask");
                self.set(am, "id", mid.clone());
                self.set(am, "mask-type", "alpha");
                self.els[am].children.push(p);
                self.add_def(am);
                let g = self.el("g");
                self.set(g, "mask", format!("url(#{mid})"));
                let kids = std::mem::take(&mut self.els[holder].children);
                self.els[g].children.extend(kids);
                self.els[holder].children.push(g);
            } else {
                self.els[holder].children.push(p);
            }
        }

        // No counted mask, no attribute: an *empty* `<clipPath>` clips
        // everything away, where lottie-web's `count > 0` guard leaves the
        // layer unmasked (`Tests_MaskNone` is all `n` masks).
        if count == 0 {
            return Ok(());
        }
        self.add_def(outer);
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

    #[allow(clippy::too_many_arguments)]
    fn emit_transform_skewed(
        &mut self,
        el: usize,
        p: Option<&InlineProp>,
        a: Option<&InlineProp>,
        s: Option<&InlineProp>,
        r: Option<&InlineProp>,
        sk: Option<&InlineProp>,
        sa: Option<&InlineProp>,
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
        let skp = sk.map(|x| self.classify(x, 1)).unwrap_or(Prop::Scalar(0.0));
        let sap = sa.map(|x| self.classify(x, 1)).unwrap_or(Prop::Scalar(0.0));

        // A skew that is statically zero costs nothing anywhere; a live one
        // takes the skewed op, whose matrix carries the extra factor.
        let skew_zero = skp.is_static()
            && sap.is_static()
            && skp.as_scalar().unwrap_or(0.0) == 0.0;

        let rest_static = ap.is_static() && sp.is_static() && rp.is_static();
        let skew_static = skp.is_static() && sap.is_static();

        if pp.is_static() && rest_static && skew_static {
            let m = matrix_skewed(
                &pp,
                &ap,
                &sp,
                &rp,
                skp.as_scalar().unwrap_or(0.0),
                sap.as_scalar().unwrap_or(0.0),
            );
            if !is_identity(&m) {
                self.set(el, "transform", svg::transform_str(&m));
            }
            return;
        }

        if !skew_zero {
            self.bind(
                op::TRANSFORM_SKEW,
                el,
                vec![
                    Arg::Prop(pp),
                    Arg::Prop(ap),
                    Arg::Prop(sp),
                    Arg::Prop(rp),
                    Arg::Prop(skp),
                    Arg::Prop(sap),
                ],
                slot,
            );
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

    /// One style element per paint style for a group whose children are all
    /// untrimmed bezier paths — see `build_shape_ref`. Returns false (having
    /// emitted nothing) when the group does not qualify, so the caller falls
    /// back to per-primitive emission.
    fn try_style_buckets(&mut self, parent: usize, children: &[ShapeRef], slot: u32) -> Result<bool> {
        // Every child must be an untrimmed path primitive with paint; a
        // subgroup or a trimmed/generated shape keeps the per-primitive walk.
        let mut prims = Vec::new();
        for c in children {
            let ShapeRef::Prim(prim) = c else { return Ok(false) };
            let Some(shape) = self.payload.s.get(prim.s as usize) else {
                return Ok(false);
            };
            if !matches!(shape, Shape::Path { .. }) || !prim.tm.is_empty() || prim.y.is_empty() {
                return Ok(false);
            }
            prims.push(prim);
        }
        // Classify geometry up front — a bailing classification must happen
        // before the first element is emitted. An expression-driven path is
        // not bucketable: the multi op has no expression column.
        let mut props = Vec::with_capacity(prims.len());
        for prim in &prims {
            let Shape::Path { pt, .. } = self.payload.s.get(prim.s as usize).unwrap() else {
                unreachable!();
            };
            let p = self.classify(pt, 2);
            if matches!(&p, Prop::Expr { .. }) {
                return Ok(false);
            }
            if !p.is_static() {
                self.caps |= Caps::PATH_KF;
            }
            props.push(p);
        }
        // Distinct paint styles, in walk-encounter order.
        let mut fill_styles: Vec<u32> = Vec::new();
        let mut stroke_styles: Vec<u32> = Vec::new();
        for prim in &prims {
            for id in prim.y.iter().rev() {
                match self.payload.y.get(*id as usize) {
                    Some(Style::Fill { .. } | Style::GradientFill { .. }) => {
                        if !fill_styles.contains(id) {
                            fill_styles.push(*id);
                        }
                    }
                    Some(Style::Stroke { .. } | Style::GradientStroke { .. }) => {
                        if !stroke_styles.contains(id) {
                            stroke_styles.push(*id);
                        }
                    }
                    _ => {}
                }
            }
        }
        // One fill and one stroke, each painting at most one shape, is the
        // combined-element case the per-primitive walk already handles (with
        // `paint-order`). Anything more — a second style, or a style that
        // paints several shapes — buckets: lottie-web concatenates every
        // shape a style paints into that style's one element.
        let mut multi = fill_styles.len() > 1 || stroke_styles.len() > 1;
        if !multi {
            'outer: for id in fill_styles.iter().chain(&stroke_styles) {
                let mut n = 0;
                for prim in &prims {
                    if prim.y.contains(id) {
                        n += 1;
                        if n > 1 {
                            multi = true;
                            break 'outer;
                        }
                    }
                }
            }
        }
        if !multi {
            return Ok(false);
        }
        for (is_fill, ids) in [(true, &fill_styles), (false, &stroke_styles)] {
            for id in ids {
                let Some(st) = self.payload.y.get(*id as usize).cloned() else {
                    continue;
                };
                let el = self.el("path");
                self.els[parent].children.push(el);
                // Every shape this style paints, in walk order; for one
                // element the subpath order is irrelevant to the fill.
                let mut bucket = Vec::new();
                for (prim, p) in prims.iter().zip(&props) {
                    if prim.y.contains(id) {
                        bucket.push(p.clone());
                    }
                }
                let all_static = bucket.iter().all(|p| p.is_static());
                if all_static {
                    let mut d = String::new();
                    for p in &bucket {
                        if let Prop::Path(fp) = p {
                            d.push_str(&fp.to_d());
                        }
                    }
                    if !d.is_empty() {
                        self.set(el, "d", d);
                    }
                } else {
                    let mut list = Vec::with_capacity(bucket.len() + 1);
                    // A list section carries no count of its own (the trim
                    // triple is fixed-length), so the count rides along as
                    // the first raw element.
                    list.push(Arg::Num(bucket.len() as f64));
                    list.extend(bucket.into_iter().map(Arg::Prop));
                    self.bind(op::SHAPE_MULTI, el, vec![Arg::List(list)], slot);
                }
                if is_fill {
                    self.emit_fill(el, &st, slot);
                } else {
                    self.set(el, "fill", "none");
                    self.emit_stroke(el, &st, slot);
                }
            }
        }
        Ok(true)
    }

    fn build_shape_ref(&mut self, parent: usize, sr: &ShapeRef, slot: u32) -> Result<()> {
        match sr {
            ShapeRef::Group(g) => {
                let node = self.el("g");
                self.els[parent].children.push(node);
                self.emit_transform_skewed(
                    node,
                    g.p.as_ref(),
                    g.a.as_ref(),
                    g.sc.as_ref(),
                    g.r.as_ref(),
                    g.sk.as_ref(),
                    g.sa.as_ref(),
                    slot,
                );
                self.emit_opacity(node, g.o.as_ref(), slot);
                // lottie-web gives every style ONE element and writes each
                // shape it paints into that element — so contours that share
                // a style share a fill rule, and their windings interact.
                // That is how a group with two fills over the same shapes
                // composes (`[sh, sh, mm, fl, sh, fl, tr]` — bootymovin's
                // heart), and it is also what makes a dropped `mm` modifier
                // invisible on static shapes. One fill and one stroke stay
                // the combined single element with `paint-order`, which is
                // the measured equivalent; more than that, or more than one
                // shape per style, buckets per style.
                if self.try_style_buckets(node, &g.c, slot)? {
                    return Ok(());
                }
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
                // The trim chain, in application order, with the steps that
                // statically cover the whole path dropped — a full window
                // rotated by any offset is still the full window, and this
                // fixture pattern is common: bodymovin leaves a `(0, 100)`
                // trim inside the group while the live one sits at the layer
                // level (`lottie_logo_3`'s lettermark).
                let trim: Vec<Style> = prim
                    .tm
                    .iter()
                    .filter_map(|id| self.payload.y.get(*id as usize).cloned())
                    .filter(|st| {
                        !static_trim_range(st).is_some_and(|(s, e, _)| (s - e).abs() >= 100.0)
                    })
                    .collect();
                // lottie-web's `setElementStyles` draws a shape into *every*
                // style element open below it in the walk — a shape between
                // two fills lands in both, duplicated, the lower style's
                // element first. A group like `[sh, sh, mm, fl, sh, fl, tr]`
                // relies on it: its two upper contours appear in
                // both the dark and the pink element, and their winding
                // interacts *within* each. Collapsing to the first fill per
                // kind — an equivalence that holds only while the top paint is
                // opaque and unholed — is what one-element emission did.
                //
                // More than one fill-ish or more than one stroke-ish style:
                // one element per paint, in the walk's encounter order
                // (deepest first), geometry duplicated. The single-fill +
                // single-stroke shape keeps its one element and its
                // `paint-order`, which is the measured equivalent of
                // lottie-web's two.
                let fill_count = prim
                    .y
                    .iter()
                    .filter(|id| {
                        matches!(
                            self.payload.y.get(**id as usize),
                            Some(Style::Fill { .. } | Style::GradientFill { .. })
                        )
                    })
                    .count();
                let stroke_count = prim
                    .y
                    .iter()
                    .filter(|id| {
                        matches!(
                            self.payload.y.get(**id as usize),
                            Some(Style::Stroke { .. } | Style::GradientStroke { .. })
                        )
                    })
                    .count();
                if fill_count > 1 || stroke_count > 1 {
                    for id in prim.y.iter().rev() {
                        let Some(st) = self.payload.y.get(*id as usize).cloned() else {
                            continue;
                        };
                        let is_fill =
                            matches!(st, Style::Fill { .. } | Style::GradientFill { .. });
                        let is_stroke = !is_fill
                            && matches!(
                                st,
                                Style::Stroke { .. } | Style::GradientStroke { .. }
                            );
                        if !is_fill && !is_stroke {
                            continue;
                        }
                        let dashed = style_dashes(&st);
                        let Some(node) = self.build_primitive(&shape, &trim, dashed, slot)
                        else {
                            continue;
                        };
                        self.els[parent].children.push(node);
                        if is_fill {
                            self.emit_fill(node, &st, slot);
                        } else {
                            self.set(node, "fill", "none");
                            self.emit_stroke(node, &st, slot);
                        }
                    }
                    return Ok(());
                }
                let dashed = prim.y.iter().any(|id| {
                    self.payload
                        .y
                        .get(*id as usize)
                        .is_some_and(style_dashes)
                });
                let node = self.build_primitive(&shape, &trim, dashed, slot);
                let Some(node) = node else { return Ok(()) };
                self.els[parent].children.push(node);
                self.emit_styles(node, &prim.y, slot);
                Ok(())
            }
        }
    }

    /// Emit the element for one shape primitive, baking its geometry when
    /// every input is static.
    ///
    /// `trim` is the modifier chain in application order — usually empty or
    /// one step; a shape under a group trim *and* a layer trim carries both.
    fn build_primitive(
        &mut self,
        shape: &Shape,
        trim: &[Style],
        dashed: bool,
        slot: u32,
    ) -> Option<usize> {
        // A dashed shape needs the `<path>` spelling too: the dash pattern
        // walks the contour from its start point, and a native `<ellipse>`
        // begins at 3 o'clock where lottie-web's outline begins at 12.
        let trimmed = !trim.is_empty() || dashed;

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
                match baked {
                    // Fully static and untrimmed: the `d` is a literal.
                    Some(path) if !trimmed => {
                        self.set(el, "d", path.to_d());
                    }
                    // Static source under trims. If every step's range is
                    // static too, the trimmed outline itself is
                    // frame-invariant — so evaluate the chain here and the
                    // animation needs no trim code, no path serializer and no
                    // binding at all.
                    Some(path) => {
                        let fixed: Option<Vec<(f64, f64, f64)>> =
                            trim.iter().map(static_trim_range).collect();
                        if let Some(steps) = fixed {
                            let flat = crate::eval::trim::Flat {
                                v: path.v.clone(),
                                i: path.i.clone(),
                                o: path.o.clone(),
                                c: path.c,
                            };
                            match crate::eval::trim::trim_chain(&flat, &steps) {
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
                        let trim_arg = self.trim_descriptor(trim);
                        self.bind(
                            op::SHAPE,
                            el,
                            vec![Arg::Prop(Prop::Path(path)), trim_arg],
                            slot,
                        );
                    }
                    None => {
                        // Only here does the runtime have to *build* geometry;
                        // a baked shape needs no generator.
                        self.caps |= Caps::PATH_D | runtime_geometry(shape);
                        let trim_arg = if trimmed {
                            self.caps |= Caps::TRIM;
                            self.trim_descriptor(trim)
                        } else {
                            Arg::Null
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
            Shape::Rect { sz, ps, rd, rv, .. } => {
                let a = self.classify(sz, 2);
                let b = self.classify(ps, 2);
                let c = self.classify(rd, 1);
                (
                    op::SHAPE_RECT,
                    vec![
                        Arg::Prop(a),
                        Arg::Prop(b),
                        Arg::Prop(c),
                        Arg::Tag(*rv as u32),
                    ],
                )
            }
            Shape::Ellipse { sz, ps, rv, .. } => {
                let a = self.classify(sz, 2);
                let b = self.classify(ps, 2);
                (
                    op::SHAPE_ELLIPSE,
                    vec![Arg::Prop(a), Arg::Prop(b), Arg::Tag(*rv as u32)],
                )
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
                let pt = self.classify(pt, 1);
                let ps = self.classify(ps, 2);
                let or = self.classify(or, 1);
                let ir = self.classify(ir, 1);
                let rt = self.classify(rt, 1);
                let zero = InlineProp::Static(Value::Scalar(0.0));
                let os = self.classify(os.as_ref().unwrap_or(&zero), 1);
                let is = self.classify(is.as_ref().unwrap_or(&zero), 1);
                (
                    op::SHAPE_STAR,
                    vec![
                        // `Tag`, not `Num`: the star type is an enumeration, and
                        // a `Num` is a measurement the encoder scales by a
                        // thousand. Same for the direction flag.
                        Arg::Tag(*sy as u32),
                        Arg::Prop(pt),
                        Arg::Prop(ps),
                        Arg::Prop(or),
                        Arg::Prop(ir),
                        Arg::Prop(rt),
                        Arg::Prop(os),
                        Arg::Prop(is),
                        Arg::Tag(*rv as u32),
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
            Shape::Rect { .. } => {
                geometry::rect_to_path(vec2(1)?, vec2(0)?, num(2)?, num(3)? != 0.0)
            }
            Shape::Ellipse { .. } => {
                geometry::ellipse_to_path(vec2(1)?, vec2(0)?, num(2)? != 0.0)
            }
            Shape::PolyStar { .. } => geometry::polystar_to_path(
                num(0)? as u8,
                vec2(2)?,
                num(1)?,
                num(3)?,
                num(4)?,
                num(5)?,
                num(6)?,
                num(7)?,
                num(8)? != 0.0,
            ),
        };
        Some(FlatPath::from_parts(
            &path.vertices,
            &path.in_tangents,
            &path.out_tangents,
            path.closed,
        ))
    }

    /// The trim chain as a list section: `[count, (s, e, o, mode) × count]`,
    /// steps in application order. The count rides in the section because a
    /// list carries no length of its own — same convention as `SHAPE_MULTI`.
    fn trim_descriptor(&mut self, chain: &[Style]) -> Arg {
        let mut items = Vec::with_capacity(1 + chain.len() * 4);
        items.push(Arg::Num(0.0));
        let mut count = 0u32;
        for style in chain {
            let Style::TrimPath { s, e, o, m } = style else {
                continue;
            };
            items.push(Arg::Prop(self.classify(s, 1)));
            items.push(Arg::Prop(self.classify(e, 1)));
            items.push(Arg::Prop(self.classify(o, 1)));
            items.push(Arg::Num(*m as f64));
            count += 1;
        }
        if count == 0 {
            return Arg::Null;
        }
        if count > 1 {
            self.caps |= Caps::TRIM_CHAIN;
        }
        items[0] = Arg::Num(count as f64);
        Arg::List(items)
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
        let mut fill: Option<(usize, Style)> = None;
        let mut stroke: Option<(usize, Style)> = None;
        for (i, id) in ids.iter().enumerate() {
            let Some(st) = self.payload.y.get(*id as usize) else {
                continue;
            };
            match st {
                Style::Fill { .. } | Style::GradientFill { .. } if fill.is_none() => {
                    fill = Some((i, st.clone()));
                }
                Style::Stroke { .. } | Style::GradientStroke { .. } if stroke.is_none() => {
                    stroke = Some((i, st.clone()));
                }
                _ => {}
            }
        }
        // **Which of the two is on top is Lottie's decision, not SVG's.** A
        // style's element is appended where the backwards walk meets it, so a
        // style earlier in `it` — earlier in this list — paints later, on top.
        // SVG has one order and it is fill-then-stroke, so a stroke Lottie put
        // *under* its fill would be drawn over it, eating half its own width
        // out of the fill: three avatars in `krrt-7ab2a6d4` are a green disc
        // ringed in white, `it` order `el fl st tr`, and the ring ate 15 units
        // into the disc.
        //
        // `paint-order="stroke"` is that ordering in one attribute. Measured
        // against lottie-web's two elements in Chrome: 120 px of fill across
        // the middle either way, against 90 px for the SVG default.
        if let (Some((f, _)), Some((s, _))) = (&fill, &stroke)
            && f < s
        {
            self.set(el, "paint-order", "stroke");
        }
        if fill.is_none() && stroke.is_some() {
            self.set(el, "fill", "none");
        }
        if let Some((_, f)) = fill {
            self.emit_fill(el, &f, slot);
        }
        if let Some((_, s)) = stroke {
            self.emit_stroke(el, &s, slot);
        }
    }

    fn emit_fill(&mut self, el: usize, style: &Style, slot: u32) {
        match style {
            Style::Fill { c, o, fr } => {
                // The rule is a static fact of the style — Lottie has no
                // animated fill rule — so it is always markup, never a binding.
                if *fr == 2 {
                    self.set(el, "fill-rule", "evenodd");
                }
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

    /// A stroke's dash pattern. Static values bake to `stroke-dasharray` /
    /// `stroke-dashoffset` — the same raw, space-joined numbers lottie-web's
    /// `DashProperty` writes. Anything animated binds `op::DASH`, whose one
    /// list argument is `[count, length…, offset]` (the offset rides last so
    /// the count stays the length count).
    fn emit_dash(
        &mut self,
        el: usize,
        dl: &[InlineProp],
        dof: Option<&InlineProp>,
        slot: u32,
    ) {
        if dl.is_empty() {
            return;
        }
        let lengths: Vec<Prop> = dl.iter().map(|p| self.classify(p, 1)).collect();
        let offset = dof.map(|p| self.classify(p, 1));
        let offset_static = offset.as_ref().map(|p| p.is_static()).unwrap_or(true);
        if lengths.iter().all(|p| p.is_static()) && offset_static {
            let arr = lengths
                .iter()
                .map(|p| svg::n(p.as_scalar().unwrap_or(0.0)))
                .collect::<Vec<_>>()
                .join(" ");
            self.set(el, "stroke-dasharray", arr);
            let o = offset
                .as_ref()
                .and_then(|p| p.as_scalar())
                .unwrap_or(0.0);
            if o != 0.0 {
                self.set(el, "stroke-dashoffset", svg::n(o));
            }
            return;
        }
        self.caps |= Caps::DASH;
        let mut items = Vec::with_capacity(lengths.len() + 2);
        items.push(Arg::Num(lengths.len() as f64));
        items.extend(lengths.into_iter().map(Arg::Prop));
        items.push(match offset {
            Some(p) => Arg::Prop(p),
            None => Arg::Null,
        });
        self.bind(op::DASH, el, vec![Arg::List(items)], slot);
    }

    fn emit_stroke(&mut self, el: usize, style: &Style, slot: u32) {
        let (paint, opacity, width, lc, lj, ml, dl, dof) = match style {
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
                let cp = self.classify(c, 4);
                (
                    Some(cp),
                    self.classify(o, 1),
                    self.classify(w, 1),
                    *lc,
                    *lj,
                    *ml,
                    dl.clone(),
                    dof.clone(),
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
                dl,
                dof,
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
                    dl.clone(),
                    dof.clone(),
                )
            }
            _ => return,
        };
        self.emit_dash(el, &dl, dof.as_ref(), slot);

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
        // The capability is set by the *binding*, not by the gradient. A
        // gradient whose handles never move bakes into the markup completely —
        // `<linearGradient>` with its coordinates written out — and then
        // `bGradient`/`oGradient` are code the module cannot reach.
        // `gradient_radial` is exactly that shape, and said so the first time
        // it ran: `output_hygiene` named both symbols.
        if sp.is_static() && ep.is_static() {
            let a = sp.as_vec().unwrap_or(&[0.0, 0.0]);
            let b = ep.as_vec().unwrap_or(&[0.0, 0.0]);
            self.set_gradient_geometry(node, radial, a[0], a[1], b[0], b[1]);
        } else {
            self.caps |= Caps::GRADIENT;
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
            flags.contains(&1).then_some(flags)
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

        // (Legacy `e` end-values were normalized into the following
        // keyframe's start value at the parse boundary; a segment's
        // destination is always the next keyframe's start.)

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

/// Whether a style draws a dashed stroke — which forces the `<path>`
/// spelling, since the dash pattern depends on where the contour starts.
fn style_dashes(style: &Style) -> bool {
    matches!(
        style,
        Style::Stroke { dl, .. } | Style::GradientStroke { dl, .. } if !dl.is_empty()
    )
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
    if i > 0 && !is_value_empty(&kf.v[i - 1]) {
        return kf.v[i - 1].clone();
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
    matrix_skewed(p, a, s, r, 0.0, 0.0)
}

pub(super) fn matrix_skewed(
    p: &Prop,
    a: &Prop,
    s: &Prop,
    r: &Prop,
    sk: f64,
    sa: f64,
) -> [f64; 6] {
    let pv = p.as_vec().map(|v| [v[0], v[1]]).unwrap_or([0.0, 0.0]);
    let av = a.as_vec().map(|v| [v[0], v[1]]).unwrap_or([0.0, 0.0]);
    let sv = s.as_vec().map(|v| [v[0], v[1]]).unwrap_or([100.0, 100.0]);
    let rv = r.as_scalar().unwrap_or(0.0);
    let spec = crate::eval::transform::TransformSpec {
        position: pv,
        anchor: av,
        scale: sv,
        rotation: rv,
        skew: sk,
        skew_axis: sa,
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
