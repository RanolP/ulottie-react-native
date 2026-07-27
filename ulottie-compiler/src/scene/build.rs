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

use super::{geo, op, Arg, Binding, Caps, LayerRecord, Planner};

/// Clock a subtree runs on.
#[derive(Clone, Copy)]
pub enum TimeCtx {
    /// The composition clock; layers hide themselves outside `[ip, op)`.
    Root,
    /// Inside a precomp: shifted by the instance's start time, wrapped to each
    /// layer's own span, and never range-hidden (matching the reference
    /// renderer's precomp behaviour).
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
        let outer_scope = self.scope;
        self.scope = scope;
        let mut nodes = Vec::with_capacity(layers.len());
        for l in layers {
            nodes.push(self.build_layer(l, ctx)?);
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
        // Track mattes. A layer carrying `td` is never drawn on its own: it is
        // the matte for the layer immediately after it in the list, which
        // carries `tt` with the mode. Turning the pair into a `<mask>` has to
        // happen before roots are collected, so the matte source is diverted
        // into `<defs>` rather than into the picture.
        let mut matte_source = vec![false; layers.len()];
        for i in 0..layers.len() {
            let Some(tt) = layers[i].tt else { continue };
            let Some(j) = i.checked_sub(1) else { continue };
            if layers[j].td.is_none() || nodes[i].dead || nodes[j].dead {
                continue;
            }
            nodes[i].mounted = self.apply_matte(nodes[i].outer, nodes[j].outer, tt);
            matte_source[j] = true;
        }

        // The reference renderer appends layers back-to-front, so the first
        // layer in the list ends up on top. Child layers are appended into
        // their parent's outer group after the parent's own content.
        let mut roots = Vec::new();
        for i in (0..layers.len()).rev() {
            if nodes[i].dead || matte_source[i] {
                continue;
            }
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
        }
        Ok(roots)
    }

    fn build_layer(&mut self, layer: &data::Layer, ctx: TimeCtx) -> Result<LayerNode> {
        let (c_ip, c_op) = (self.payload.c.ip, self.payload.c.op);
        let (slot, range_hidden) = match ctx {
            TimeCtx::Root => {
                // A layer whose span never overlaps the composition can never
                // be seen — drop the whole subtree.
                if layer.op <= c_ip || layer.ip >= c_op {
                    let outer = self.el("g");
                    return Ok(LayerNode { outer, mounted: outer, dead: true, record: None });
                }
                let hides = layer.ip > c_ip || layer.op < c_op;
                (0u32, hides)
            }
            TimeCtx::Inner { parent, offset } => {
                self.caps |= Caps::TIMELINE;
                self.timelines.push([parent as f64, offset, layer.ip, layer.op]);
                (self.timelines.len() as u32, false)
            }
        };

        // Reserve the record first: classifying this layer's properties stamps
        // the record index into every expression they carry.
        let record = if self.has_exprs {
            let idx = self.layers.len() as u32;
            let name = layer.n.and_then(|i| self.payload.st.get(i as usize).cloned());
            let n = name.map(|s| self.intern_name(&s));
            self.layers.push(LayerRecord { i: layer.i, n, ..Default::default() });
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
        let inner = if has_content {
            let n = self.el("g");
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
        if range_hidden {
            self.bind(
                op::DISPLAY,
                outer,
                vec![Arg::Num(layer.ip), Arg::Num(layer.op)],
                slot,
            );
            self.gates.push([layer.ip, layer.op]);
            self.gate = self.gates.len() as u32;
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

            let dp = pp.clone().unwrap_or(Prop::Vector(vec![0.0, 0.0, 0.0]));
            let da = ap.clone().unwrap_or(Prop::Vector(vec![0.0, 0.0, 0.0]));
            let ds = sp.clone().unwrap_or(Prop::Vector(vec![100.0, 100.0, 100.0]));
            let dr = rp.clone().unwrap_or(Prop::Scalar(0.0));
            if dp.is_static() && da.is_static() && ds.is_static() && dr.is_static() {
                let m = matrix(&dp, &da, &ds, &dr);
                if !is_identity(&m) {
                    self.set(outer, "transform", svg::matrix_str(&m));
                }
            } else {
                self.bind(op::LAYER_TX, outer, vec![Arg::Num(rec as f64)], slot);
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
            self.emit_transform(
                outer,
                layer.p.as_ref(),
                layer.a.as_ref(),
                layer.sc.as_ref(),
                layer.r.as_ref(),
                slot,
            );
            if has_content {
                self.emit_opacity(inner, layer.o.as_ref(), slot);
            }
        }

        if let Some(masks) = &layer.mk
            && !masks.is_empty()
        {
            self.emit_masks(inner, masks, slot)?;
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
                self.set(rect, "fill", layer.cl.clone().unwrap_or_else(|| "#000".into()));
                self.els[inner].children.push(rect);
            }
            0 => {
                if let Some(id) = layer.rf.clone() {
                    let offset = layer.st.unwrap_or(0.0);
                    // Time remap replaces the precomp's clock outright: its
                    // inner time is a function of the outer one rather than a
                    // shift of it. Give it a slot of its own that the children
                    // then hang off, so the usual offset/loop path is untouched.
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
                                    TimeCtx::Inner { parent: slot, offset },
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
        Ok(LayerNode { outer, mounted: outer, dead: false, record })
    }

    /// Effects, in the shape `thisLayer.effect('name')('param')` reads.
    fn encode_effects(&mut self, layer: &data::Layer) -> Option<serde_json::Value> {
        let effects = layer.ef.as_ref()?;
        let mut out = Vec::new();
        for e in effects {
            let params: Vec<serde_json::Value> = e
                .ef
                .iter()
                .map(|p| {
                    let mut m = serde_json::Map::new();
                    if let Some(n) = &p.nm {
                        m.insert("nm".into(), serde_json::Value::String(n.clone()));
                    }
                    if let Some(n) = &p.mn {
                        m.insert("mn".into(), serde_json::Value::String(n.clone()));
                    }
                    if p.ty != 0 {
                        m.insert("ty".into(), p.ty.into());
                    }
                    if let Some(v) = p.v {
                        m.insert("v".into(), serde_json::json!(svg::q(v)));
                    }
                    if let Some(prop) = &p.p {
                        let classified = self.classify(prop, 1);
                        m.insert(
                            "p".into(),
                            serde_json::to_value(&classified).unwrap_or(serde_json::Value::Null),
                        );
                    }
                    serde_json::Value::Object(m)
                })
                .collect();
            let mut m = serde_json::Map::new();
            if let Some(n) = &e.nm {
                m.insert("nm".into(), serde_json::Value::String(n.clone()));
            }
            if let Some(n) = &e.mn {
                m.insert("mn".into(), serde_json::Value::String(n.clone()));
            }
            m.insert("ef".into(), serde_json::Value::Array(params));
            out.push(serde_json::Value::Object(m));
        }
        (!out.is_empty()).then(|| serde_json::Value::Array(out))
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
        let Some(asset) = self.plan_asset(id)? else { return Ok(None) };
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
        self.timelines.push([parent as f64, 0.0, layer.ip, layer.op]);
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
        let layers: Vec<data::Layer> = match self
            .payload
            .a
            .as_ref()
            .and_then(|a| a.get(id))
        {
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
        let kids = self.build_layer_forest(&layers, TimeCtx::Inner { parent: 0, offset: 0.0 })?;
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
        self.emit_el(root, &mut markup, &mut counter, &mut local);
        let mut inline_counter = 0u32;
        let mut inline_markup = String::new();
        self.emit_inline_el(root, &mut inline_markup, &mut inline_counter);
        debug_assert_eq!(counter, inline_counter, "element count must not depend on form");

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
        for r in &mut records {
            if let Some(pr) = r.pr {
                r.pr = Some(pr - delta);
            }
            for p in [&mut r.p, &mut r.a, &mut r.sc, &mut r.r, &mut r.o, &mut r.h] {
                if let Some(p) = p {
                    rebase_prop(p, delta);
                }
            }
            if let Some(ef) = &mut r.ef {
                rebase_json(ef, delta);
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
                let parent = if t[0] == 0.0 { 0.0 } else { t[0] - tl_start as f64 };
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
    fn apply_matte(&mut self, target: usize, source: usize, tt: u8) -> usize {
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

        // The mask goes on a wrapper, not on `target`. `target` carries the
        // layer's own transform, and a mask is resolved in the user space that
        // transform establishes — so putting it there would rotate and shift
        // the matte along with the layer, clipping the wrong region entirely.
        // The matte was authored as a sibling, in the composition's space, and
        // an untransformed wrapper is what preserves that.
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
        for ch_name in if alpha { &["feFuncA"][..] } else { &["feFuncR", "feFuncG", "feFuncB"][..] } {
            let fun = self.el(ch_name);
            self.set(fun, "type", "table");
            self.set(fun, "tableValues", "1 0");
            self.els[t].children.push(fun);
        }
        self.els[f].children.push(t);
        self.add_def(f);
        id
    }

    fn emit_masks(&mut self, target: usize, masks: &[data::LayerMask], slot: u32) -> Result<()> {
        let (cw, ch) = (self.payload.c.w as f64, self.payload.c.h as f64);
        let id = self.next_id("m");
        let mask = self.el("mask");
        self.set(mask, "id", id.clone());
        self.set(mask, "mask-type", "luminance");

        let has_subtract = masks.iter().any(|m| m.m == "s" || m.inv);
        if has_subtract {
            let bg = self.el("rect");
            self.set(bg, "width", svg::n(cw));
            self.set(bg, "height", svg::n(ch));
            self.set(bg, "fill", "#fff");
            self.els[mask].children.push(bg);
        }

        for m in masks {
            let subtract = m.m == "s" || m.inv;
            let p = self.el("path");
            self.set(p, "fill", if subtract { "#000" } else { "#fff" });
            self.set(p, "fill-rule", "evenodd");
            let shape = self.classify(&m.pt, 2);
            match &shape {
                Prop::Path(path) => {
                    self.set(p, "d", path.to_d());
                }
                _ => {
                    self.caps |= Caps::PATH_D;
                    self.bind(
                        op::SHAPE,
                        p,
                        vec![Arg::List(vec![Arg::Num(geo::PATH as f64), Arg::Prop(shape)]), Arg::Null],
                        slot,
                    );
                }
            }
            self.els[mask].children.push(p);
        }

        self.add_def(mask);
        self.set(target, "mask", format!("url(#{id})"));
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
        let pp = p.map(|x| self.classify(x, dim)).unwrap_or(Prop::Vector(vec![0.0, 0.0]));
        let ap = a.map(|x| self.classify(x, dim)).unwrap_or(Prop::Vector(vec![0.0, 0.0]));
        let sp = s.map(|x| self.classify(x, dim)).unwrap_or(Prop::Vector(vec![100.0, 100.0]));
        let rp = r.map(|x| self.classify(x, 1)).unwrap_or(Prop::Scalar(0.0));

        let rest_static = ap.is_static() && sp.is_static() && rp.is_static();

        if pp.is_static() && rest_static {
            let m = matrix(&pp, &ap, &sp, &rp);
            if !is_identity(&m) {
                self.set(el, "transform", svg::matrix_str(&m));
            }
            return;
        }

        if rest_static {
            // Only the position moves: the linear part of the matrix and the
            // anchor contribution are constants, so the runtime only has to
            // concatenate two numbers onto a baked prefix.
            let zero = Prop::Vector(vec![0.0, 0.0]);
            let m = matrix(&zero, &ap, &sp, &rp);
            let prefix = format!(
                "matrix({},{},{},{},",
                svg::nd(m[0], 1e5),
                svg::nd(m[1], 1e5),
                svg::nd(m[2], 1e5),
                svg::nd(m[3], 1e5)
            );
            self.bind(
                op::TRANSLATE,
                el,
                vec![Arg::Str(prefix), Arg::Num(m[4]), Arg::Num(m[5]), Arg::Prop(pp)],
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
        let op_prop = o.map(|x| self.classify(x, 1)).unwrap_or(Prop::Scalar(100.0));
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
                let trim = prim.tm.and_then(|id| self.payload.y.get(id as usize).cloned());
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
    fn build_primitive(
        &mut self,
        shape: &Shape,
        trim: Option<&Style>,
        slot: u32,
    ) -> Option<usize> {
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
                let g = self.geo_descriptor(shape);
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
                                    let baked =
                                        FlatPath { v: out.v, i: out.i, o: out.o, c: out.c };
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
                            vec![
                                Arg::List(vec![
                                    Arg::Num(geo::PATH as f64),
                                    Arg::Prop(Prop::Path(path)),
                                ]),
                                trim_arg,
                            ],
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
                        self.bind(op::SHAPE, el, vec![Arg::List(g), trim_arg], slot);
                    }
                }
                Some(el)
            }
        }
    }

    /// Build the geometry descriptor for a shape, classifying each input.
    fn geo_descriptor(&mut self, shape: &Shape) -> Vec<Arg> {
        match shape {
            Shape::Path { pt, .. } => {
                let p = self.classify(pt, 2);
                if !p.is_static() {
                    self.caps |= Caps::PATH_KF;
                }
                vec![Arg::Num(geo::PATH as f64), Arg::Prop(p)]
            }
            Shape::Rect { sz, ps, rd, .. } => {
                let a = self.classify(sz, 2);
                let b = self.classify(ps, 2);
                let c = self.classify(rd, 1);
                vec![Arg::Num(geo::RECT as f64), Arg::Prop(a), Arg::Prop(b), Arg::Prop(c)]
            }
            Shape::Ellipse { sz, ps, .. } => {
                let a = self.classify(sz, 2);
                let b = self.classify(ps, 2);
                vec![Arg::Num(geo::ELLIPSE as f64), Arg::Prop(a), Arg::Prop(b)]
            }
            Shape::PolyStar { sy, pt, ps, or, ir, rt, .. } => {
                let pt = self.classify(pt, 1);
                let ps = self.classify(ps, 2);
                let or = self.classify(or, 1);
                let ir = self.classify(ir, 1);
                let rt = self.classify(rt, 1);
                vec![
                    Arg::Num(geo::POLYSTAR as f64),
                    Arg::Num(*sy as f64),
                    Arg::Prop(pt),
                    Arg::Prop(ps),
                    Arg::Prop(or),
                    Arg::Prop(ir),
                    Arg::Prop(rt),
                ]
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
                Arg::Num(n) => Some(*n),
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
            Shape::Path { .. } => match g.get(1)? {
                Arg::Prop(Prop::Path(p)) => return Some(p.clone()),
                _ => return None,
            },
            Shape::Rect { .. } => geometry::rect_to_path(vec2(2)?, vec2(1)?, num(3)?),
            Shape::Ellipse { .. } => geometry::ellipse_to_path(vec2(2)?, vec2(1)?),
            Shape::PolyStar { .. } => geometry::polystar_to_path(
                num(1)? as u8,
                vec2(3)?,
                num(2)?,
                num(4)?,
                num(5)?,
                num(6)?,
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

    fn emit_styles(&mut self, el: usize, ids: &[u32], slot: u32) {
        // The reference renderer applies styles back-to-front, so for any one
        // attribute the *first* matching style wins. Picking the first fill-ish
        // and first stroke-ish style reproduces that without the redundant
        // writes.
        let mut fill: Option<Style> = None;
        let mut stroke: Option<Style> = None;
        for id in ids {
            let Some(st) = self.payload.y.get(*id as usize) else { continue };
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
            Style::Stroke { c, o, w, lc, lj, ml } => {
                let cp = self.classify(c, 4);
                (Some(cp), self.classify(o, 1), self.classify(w, 1), *lc, *lj, *ml)
            }
            Style::GradientStroke { g, w, o, s, e, gk, lc, lj, ml } => {
                let id = self.emit_gradient(g, *gk, s.as_ref(), e.as_ref(), slot);
                self.set(el, "stroke", format!("url(#{id})"));
                (None, self.classify(o, 1), self.classify(w, 1), *lc, *lj, *ml)
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
        let node = self.el(if radial { "radialGradient" } else { "linearGradient" });
        self.set(node, "id", id.clone());
        self.set(node, "gradientUnits", "userSpaceOnUse");

        let sp = s.map(|x| self.classify(x, 2)).unwrap_or(Prop::Vector(vec![0.0, 0.0]));
        let ep = e.map(|x| self.classify(x, 2)).unwrap_or(Prop::Vector(vec![0.0, 0.0]));
        if sp.is_static() && ep.is_static() {
            let a = sp.as_vec().unwrap_or(&[0.0, 0.0]);
            let b = ep.as_vec().unwrap_or(&[0.0, 0.0]);
            self.set_gradient_geometry(node, radial, a[0], a[1], b[0], b[1]);
        } else {
            self.bind(
                op::GRADIENT,
                node,
                vec![Arg::Num(gk as f64), Arg::Prop(sp), Arg::Prop(ep)],
                slot,
            );
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
            self.set(node, "r", svg::n(((ex - sx).powi(2) + (ey - sy).powi(2)).sqrt()));
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
                Prop::Expr { id: e.e, fallback, layer: self.layer_rec }
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
            AnimKind::Path => (
                Vec::new(),
                values.iter().map(path_of).collect::<Vec<_>>(),
            ),
            _ => (
                values.iter().flat_map(|v| flatten(v, dim)).collect::<Vec<_>>(),
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
            let nonzero = a.iter().chain(b.iter()).any(|v| v.iter().any(|x| *x != 0.0));
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
        let key = [
            ox.to_bits(),
            oy.to_bits(),
            ix.to_bits(),
            iy.to_bits(),
        ];
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
fn rebase_prop(p: &mut Prop, delta: u32) {
    if let Prop::Expr { layer, fallback, .. } = p {
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

/// Effect parameters carry serialized properties, so their layer references
/// need the same shift.
fn rebase_json(v: &mut serde_json::Value, delta: u32) {
    match v {
        serde_json::Value::Object(m) => {
            let is_expr = m.contains_key("x") && m.contains_key("l");
            if is_expr {
                if let Some(l) = m.get_mut("l").and_then(|l| l.as_u64()) {
                    m.insert("l".into(), (l - delta as u64).into());
                }
            }
            for (_, val) in m.iter_mut() {
                rebase_json(val, delta);
            }
        }
        serde_json::Value::Array(a) => {
            for x in a {
                rebase_json(x, delta);
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
    let Style::TrimPath { s, e, o, .. } = style else { return None };
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
        if let Some(e) = kf.e.as_ref().and_then(|e| e.get(i - 1).and_then(|x| x.clone()))
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

pub(super) fn is_identity(m: &[f64; 6]) -> bool {
    (m[0] - 1.0).abs() < 1e-6
        && m[1].abs() < 1e-6
        && m[2].abs() < 1e-6
        && (m[3] - 1.0).abs() < 1e-6
        && m[4].abs() < 1e-6
        && m[5].abs() < 1e-6
}


