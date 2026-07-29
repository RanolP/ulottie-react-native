//! The IR pass: fold every expression-driven property, then drop the bodies
//! nothing references any more.
//!
//! Deleting at the IR level is what makes the saving compound. Downstream,
//! nothing knows this pass ran: the planner sees a plain keyframed property, so
//! it never interns the layer name, never builds the record, never sets
//! `Caps::EXPRESSIONS`. An animation whose last expression folds away stops
//! shipping the engine entirely — not because anything removes it, but because
//! nothing asks for it.
//!
//! The sweep at the end is not an afterthought. Bodies are deduplicated, so one
//! serves many properties and only becomes dead when the *last* of them folds.
//! Folding without sweeping moves nothing measurable: the property gets cheaper
//! and the module still carries the body, the names it looks up, and the engine
//! that would have run it.

use std::collections::{BTreeSet, HashMap};

use super::{Facts, Outcome, fold, resolve};
use crate::ir;

/// Fold what can be folded, sweep what that orphans, and report how many
/// properties changed.
pub fn fold_module(module: &mut ir::Module) -> usize {
    if module.expressions.is_empty() {
        return 0;
    }

    // The bodies are read while the properties holding their ids are rewritten,
    // so they sit outside the module for the duration of the walk.
    let bodies = std::mem::take(&mut module.expressions);
    let mut cx = Cx {
        bodies: &bodies,
        folded: 0,
        seen: BTreeSet::new(),
        live: BTreeSet::new(),
        remap: None,
        collecting: false,
        found: Vec::new(),
        per_layer: Vec::new(),
    };
    walk_all(module, &mut cx);
    let (folded, seen, live) = (cx.folded, cx.seen, cx.live);
    module.expressions = bodies;

    // Sweep only what the walk actually visited and then folded away. An id the
    // walk never reached is kept, so a property site this pass does not know
    // about costs bytes rather than losing the body it still points at — which
    // is how `ripple` lost every expression it had: its animation lives inside
    // a precomp asset, the walk only covered the root layers, and an empty
    // `live` set read as "nothing references anything".
    let keep: BTreeSet<u32> = (0..module.expressions.len() as u32)
        .filter(|id| live.contains(id) || !seen.contains(id))
        .collect();
    if keep.len() < module.expressions.len() {
        let map = module.expressions.retain(&keep);
        let empty = ir::ExprTable::new();
        let mut cx = Cx {
            bodies: &empty,
            folded: 0,
            seen: BTreeSet::new(),
            live: BTreeSet::new(),
            remap: Some(&map),
            collecting: false,
            found: Vec::new(),
            per_layer: Vec::new(),
        };
        walk_all(module, &mut cx);
    }

    // Now that the survivors are known, resolve the names inside them, then
    // decide the guards those names were mostly there to serve.
    resolve_refs(module);
    fold_branches(module);
    folded
}

/// Rewrite `effect('name')('param')` to `effect(i)(j)` in every surviving body.
///
/// Resolved once per *using* layer and applied only when they all agree: the
/// bodies are shared, so a rewrite that is right for one layer and wrong for
/// another would silently read the wrong parameter.
fn resolve_refs(module: &mut ir::Module) {
    let mut refs: HashMap<u32, Vec<resolve::Ref>> = HashMap::new();
    for e in module.expressions.iter() {
        let found = resolve::refs(&e.body);
        if !found.is_empty() {
            refs.insert(e.id.0, found);
        }
    }
    if refs.is_empty() {
        return;
    }

    // `None` marks a body whose uses disagree, or one whose layer does not
    // answer to a name it asks for.
    let mut agreed: HashMap<u32, Option<Vec<Vec<u32>>>> = HashMap::new();
    let mut uses = Uses {
        refs: &refs,
        agreed: &mut agreed,
    };
    collect_uses(module, &mut uses);
    drop(uses);

    let mut rewritten = Vec::new();
    for (id, indices) in agreed {
        let Some(indices) = indices else { continue };
        let list = &refs[&id];
        let pairs: Vec<(&resolve::Ref, Vec<u32>)> = list.iter().zip(indices).collect();
        rewritten.push((
            id,
            resolve::rewrite(&module.expressions.get(ir::ExprId(id)).body, &pairs),
        ));
    }
    for (id, body) in rewritten {
        module.expressions.get_mut(ir::ExprId(id)).body = body;
    }
}

/// Replace an `if` whose test the compiler can decide with the arm it takes.
///
/// Bodymovin puts a guard in front of anything a checkbox can turn off, built
/// out of things the compiler resolved long ago. Deciding it here removes the
/// test, the literals in it, and — through the lexical rule in
/// `prune_effect_names` — the payload names those literals kept alive.
///
/// Decided per *property* and applied only when they agree. Per property, not
/// per layer: `thisProperty.numKeys` is half of the guard `ripple` carries, and
/// one body serves properties that do not agree about it. Evaluating with a
/// stand-in count is what broke `ripple` the first time this was attempted, and
/// the across-the-animation geometry gate caught it at t=0.25.
fn fold_branches(module: &mut ir::Module) {
    let mut found: HashMap<u32, Vec<resolve::Branch>> = HashMap::new();
    for e in module.expressions.iter() {
        let b = resolve::branches(&e.body);
        if !b.is_empty() {
            found.insert(e.id.0, b);
        }
    }
    if found.is_empty() {
        return;
    }

    let empty = ir::ExprTable::new();
    let mut cx = Cx {
        bodies: &empty,
        folded: 0,
        seen: BTreeSet::new(),
        live: BTreeSet::new(),
        remap: None,
        collecting: true,
        found: Vec::new(),
        per_layer: Vec::new(),
    };
    walk_all(module, &mut cx);

    let mut agreed: HashMap<u32, Option<Vec<Option<bool>>>> = HashMap::new();
    for (uses, effects) in cx.per_layer {
        for (id, num_keys) in uses {
            let Some(list) = found.get(&id) else { continue };
            // Each test on its own, against this property. One the evaluator
            // cannot decide is `None` — kept, never guessed at.
            let facts = Facts {
                effects: &effects,
                num_keys,
                value_range: None,
            };
            let decided: Vec<Option<bool>> = list
                .iter()
                .map(
                    |b| match super::fold(&format!("$bm_rt = ({});", b.test), &facts) {
                        Outcome::Constant(n) => Some(n != 0.0),
                        _ => None,
                    },
                )
                .collect();
            match agreed.entry(id) {
                std::collections::hash_map::Entry::Vacant(v) => {
                    v.insert(Some(decided));
                }
                std::collections::hash_map::Entry::Occupied(mut o) => {
                    if o.get().as_ref() != Some(&decided) {
                        o.insert(None);
                    }
                }
            }
        }
    }

    let mut rewritten = Vec::new();
    for (id, verdicts) in agreed {
        let Some(verdicts) = verdicts else { continue };
        let body = &module.expressions.get(ir::ExprId(id)).body;
        let cuts: Vec<_> = found[&id]
            .iter()
            .zip(verdicts)
            .filter_map(|(b, v)| b.arm(body, v?))
            .collect();
        if !cuts.is_empty() {
            rewritten.push((id, resolve::take_branches(body, &cuts)));
        }
    }
    for (id, body) in rewritten {
        module.expressions.get_mut(ir::ExprId(id)).body = body;
    }
}

struct Uses<'a> {
    refs: &'a HashMap<u32, Vec<resolve::Ref>>,
    agreed: &'a mut HashMap<u32, Option<Vec<Vec<u32>>>>,
}

impl Uses<'_> {
    /// One property still carrying expression `id`, on a layer with `effects`.
    fn record(&mut self, id: u32, effects: &[ir::Effect]) {
        let Some(list) = self.refs.get(&id) else {
            return;
        };
        let resolved: Option<Vec<Vec<u32>>> = list.iter().map(|r| r.resolve(effects)).collect();
        match self.agreed.entry(id) {
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(resolved);
            }
            std::collections::hash_map::Entry::Occupied(mut o) => {
                if *o.get() != resolved {
                    o.insert(None);
                }
            }
        }
    }
}

/// Every property still holding an expression, grouped by the layer it is on.
fn collect_uses(module: &mut ir::Module, uses: &mut Uses) {
    let empty = ir::ExprTable::new();
    let mut cx = Cx {
        bodies: &empty,
        folded: 0,
        seen: BTreeSet::new(),
        live: BTreeSet::new(),
        remap: None,
        collecting: true,
        found: Vec::new(),
        per_layer: Vec::new(),
    };
    walk_all(module, &mut cx);
    for (ids, effects) in cx.per_layer {
        for (id, _) in ids {
            uses.record(id, &effects);
        }
    }
}

/// Every layer in the module: the root composition and each precomp body.
fn walk_all(module: &mut ir::Module, cx: &mut Cx) {
    walk(&mut module.layers, cx);
    for asset in &mut module.assets {
        if let ir::AssetKind::Precomp { layers } = &mut asset.kind {
            walk(layers, cx);
        }
    }
}

/// What the walk is doing this time round.
struct Cx<'a> {
    bodies: &'a ir::ExprTable,
    folded: usize,
    /// Every expression id the walk reached. What is *not* here was never
    /// examined, and is kept for that reason.
    seen: BTreeSet<u32>,
    /// Ids still referenced once folding is done.
    live: BTreeSet<u32>,
    /// Set on the second walk: old → new ids after the sweep.
    remap: Option<&'a HashMap<u32, u32>>,
    /// Third walk: gather which expressions each layer uses, so a shared body
    /// can be resolved against every layer it is applied to. Reuses this walk
    /// rather than repeating the property list, which would be one more place
    /// to forget a site.
    collecting: bool,
    found: Vec<(u32, usize)>,
    per_layer: Vec<(Vec<(u32, usize)>, Vec<ir::Effect>)>,
}

fn walk(layers: &mut [ir::Layer], cx: &mut Cx) {
    for layer in layers {
        // Effects are read while properties are rewritten, so they are taken
        // aside too. An effect parameter's own expression folds against the
        // layer's *other* effects, which is what AE's `propertyGroup` reaches —
        // and it could not observe its own unfolded self in any case.
        let mut effects = std::mem::take(&mut layer.effects);

        let t = &mut layer.transform;
        for p in [&mut t.anchor, &mut t.position, &mut t.scale] {
            p.visit(cx, &effects);
        }
        t.rotation.visit(cx, &effects);
        t.opacity.visit(cx, &effects);
        for p in [&mut t.skew, &mut t.skew_axis].into_iter().flatten() {
            p.visit(cx, &effects);
        }
        if let Some(p) = &mut layer.time_remap {
            p.visit(cx, &effects);
        }
        for m in &mut layer.masks {
            m.shape.visit(cx, &effects);
        }
        if let ir::LayerKind::Shape { shapes } = &mut layer.kind {
            for s in shapes.iter_mut() {
                shape(s, cx, &effects);
            }
        }

        // Effect parameters against a snapshot, so a parameter that folds
        // cannot change what its siblings fold against mid-walk.
        let snapshot = effects.clone();
        for e in &mut effects {
            for p in &mut e.parameters {
                if let ir::EffectValue::Scalar(prop) = &mut p.value {
                    prop.visit(cx, &snapshot);
                }
            }
        }
        if cx.collecting && !cx.found.is_empty() {
            let ids = std::mem::take(&mut cx.found);
            cx.per_layer.push((ids, effects.clone()));
        }
        layer.effects = effects;
    }
}

fn shape(node: &mut ir::ShapeNode, cx: &mut Cx, fx: &[ir::Effect]) {
    match node {
        ir::ShapeNode::Group { items, .. } => {
            for c in items.iter_mut() {
                shape(c, cx, fx);
            }
        }
        ir::ShapeNode::Transform { transform: t, .. } => {
            for p in [&mut t.anchor, &mut t.position, &mut t.scale] {
                p.visit(cx, fx);
            }
            t.rotation.visit(cx, fx);
            t.opacity.visit(cx, fx);
            for p in [&mut t.skew, &mut t.skew_axis].into_iter().flatten() {
                p.visit(cx, fx);
            }
        }
        ir::ShapeNode::Path { ks, .. } => ks.visit(cx, fx),
        ir::ShapeNode::Rectangle {
            size,
            position,
            radius,
            ..
        } => {
            size.visit(cx, fx);
            position.visit(cx, fx);
            radius.visit(cx, fx);
        }
        ir::ShapeNode::Ellipse { size, position, .. } => {
            size.visit(cx, fx);
            position.visit(cx, fx);
        }
        ir::ShapeNode::PolyStar {
            points,
            position,
            rotation,
            outer_radius,
            inner_radius,
            outer_roundness,
            inner_roundness,
            ..
        } => {
            points.visit(cx, fx);
            position.visit(cx, fx);
            rotation.visit(cx, fx);
            outer_radius.visit(cx, fx);
            for p in [inner_radius, outer_roundness, inner_roundness]
                .into_iter()
                .flatten()
            {
                p.visit(cx, fx);
            }
        }
        ir::ShapeNode::Fill { color, opacity, .. } => {
            color.visit(cx, fx);
            opacity.visit(cx, fx);
        }
        ir::ShapeNode::Stroke {
            color,
            opacity,
            width,
            ..
        } => {
            color.visit(cx, fx);
            opacity.visit(cx, fx);
            width.visit(cx, fx);
        }
        ir::ShapeNode::GradientFill {
            opacity,
            start,
            end,
            ..
        } => {
            opacity.visit(cx, fx);
            for p in [start, end].into_iter().flatten() {
                p.visit(cx, fx);
            }
        }
        ir::ShapeNode::GradientStroke {
            width,
            opacity,
            start,
            end,
            ..
        } => {
            width.visit(cx, fx);
            opacity.visit(cx, fx);
            for p in [start, end].into_iter().flatten() {
                p.visit(cx, fx);
            }
        }
        ir::ShapeNode::TrimPath {
            start, end, offset, ..
        } => {
            start.visit(cx, fx);
            end.visit(cx, fx);
            offset.visit(cx, fx);
        }
    }
}

// ---------------------------------------------------------------------------
// Property sites
// ---------------------------------------------------------------------------

/// One property the walk can reach. Implemented per value type because a
/// constant verdict has to be written back as that type — a number is a value a
/// `Scalar` can take and a `PathData` cannot.
trait Site {
    fn visit(&mut self, cx: &mut Cx, effects: &[ir::Effect]);
}

/// The verdict for one property, and what to put in its place.
struct Decision<T: Clone> {
    id: u32,
    outcome: Outcome,
    fallback: ir::ValueSource<T>,
}

/// Renumber on the second walk; otherwise fold and hand the verdict back.
///
/// Deliberately does *not* record liveness itself: whether an expression
/// survives depends on what the caller does with the verdict, and a type that
/// declines a constant has to keep the expression it declined. Recording here
/// would sweep a body a property still points at.
fn decide<T: Clone>(
    prop: &mut ir::Property<T>,
    cx: &mut Cx,
    effects: &[ir::Effect],
    range: impl Fn(&ir::ValueSource<T>) -> Option<(f64, f64)>,
) -> Option<Decision<T>> {
    let ir::Property::Expression { fallback, expr } = prop else {
        return None;
    };
    if let Some(map) = cx.remap {
        *expr = ir::ExprId(map[&expr.0]);
        return None;
    }
    if cx.collecting {
        cx.found.push((expr.0, num_keys(fallback)));
        return None;
    }
    let facts = Facts {
        effects,
        num_keys: num_keys(fallback),
        value_range: range(fallback),
    };
    cx.seen.insert(expr.0);
    Some(Decision {
        id: expr.0,
        outcome: fold(&cx.bodies.get(*expr).body, &facts),
        fallback: fallback.clone(),
    })
}

/// What `thisProperty.numKeys` reports for this property.
fn num_keys<T: Clone>(fallback: &ir::ValueSource<T>) -> usize {
    match fallback {
        ir::ValueSource::Animated(kf) => kf.frames.len(),
        ir::ValueSource::Static(_) => 0,
    }
}

/// A property becomes its own fallback: exactly what the expression returned.
fn demote<T: Clone>(prop: &mut ir::Property<T>, fallback: ir::ValueSource<T>) {
    *prop = match fallback {
        ir::ValueSource::Static(v) => ir::Property::Static(v),
        ir::ValueSource::Animated(kf) => ir::Property::Animated(kf),
    };
}

impl Site for ir::Property<ir::Scalar> {
    fn visit(&mut self, cx: &mut Cx, effects: &[ir::Effect]) {
        let Some(d) = decide(self, cx, effects, scalar_range) else {
            return;
        };
        match d.outcome {
            Outcome::Identity => demote(self, d.fallback),
            Outcome::Constant(n) => *self = ir::Property::Static(n),
            Outcome::Open => {
                cx.live.insert(d.id);
                return;
            }
        }
        cx.folded += 1;
    }
}

fn scalar_range(v: &ir::ValueSource<ir::Scalar>) -> Option<(f64, f64)> {
    match v {
        ir::ValueSource::Static(n) => Some((*n, *n)),
        ir::ValueSource::Animated(kf) => span(
            kf.frames
                .iter()
                .flat_map(|f| f.value.iter().copied().chain(f.end_value.iter().copied())),
        ),
    }
}

fn span(values: impl Iterator<Item = f64>) -> Option<(f64, f64)> {
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    let mut any = false;
    for v in values {
        any = true;
        lo = lo.min(v);
        hi = hi.max(v);
    }
    any.then_some((lo, hi))
}

/// The vector, colour and path types take an identity verdict and decline a
/// constant one — a number is not a value they can hold, and inventing a
/// conversion for a case no body in the corpus produces would be guessing.
macro_rules! identity_only {
    ($t:ty) => {
        impl Site for ir::Property<$t> {
            fn visit(&mut self, cx: &mut Cx, effects: &[ir::Effect]) {
                let Some(d) = decide(self, cx, effects, |_| None) else {
                    return;
                };
                if d.outcome == Outcome::Identity {
                    demote(self, d.fallback);
                    cx.folded += 1;
                } else {
                    cx.live.insert(d.id);
                }
            }
        }
    };
}

identity_only!(ir::Vec2);
identity_only!(ir::Vec3);
identity_only!(ir::Color);
identity_only!(ir::PathData);
