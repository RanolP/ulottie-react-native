//! Resolving the layers an expression body reaches, at compile time.
//!
//! An After Effects body addresses other layers by name (`thisComp.layer('wire')`)
//! or through a layer-control effect, and reads them through a method surface —
//! `.position`, `.toComp(p)`, `.effect(n)(p)`. The runtime used to answer all of
//! that with a per-layer proxy carrying fifteen accessors, reached through two
//! scope-keyed maps, walked once per property per frame.
//!
//! None of it has to happen at runtime. The planner already knows every layer,
//! its composition scope, its parent and its effects; what a body names is a
//! constant. So this pass rewrites each body so that every layer reference is a
//! slot in the owning record's own table and every access is a free call:
//!
//! ```text
//! thisComp.layer('wire').toComp(p)  →  toComp(lyAt(thisLayer, 8), p, frame)
//! ```
//!
//! Two spellings, because neither alone covers the corpus. Bodies are
//! deduplicated across every property they were applied to, so a literal has to
//! be right for all of them:
//!
//! * `lyAt(thisLayer, T)` when every using property agrees on the absolute slot.
//!   `lights` names `wire` from five layers and it is record 8 for all five.
//! * `lyRel(thisLayer, D)` when the absolute slot differs but the offset from
//!   the owner does not. `ripple`'s precomp is inlined twenty-three times, so
//!   its `thisComp.layer('bar')` has twenty-three different answers and one
//!   delta.
//!
//! Anything not resolvable *exactly* keeps working through [`Plan::legacy`]: the
//! body is emitted with a `thisComp` and a proxy view in front of it, exactly as
//! before. That costs bytes and never correctness, which is the house rule. A
//! mechanical scan of the finished text ([`verify`]) has the last word, so an
//! optimistic walk cannot ship a body it only thought it had rewritten.

use std::collections::{BTreeMap, BTreeSet};

use crate::ir;
use crate::scene::prop::Prop;
use crate::scene::{Arg, SceneData};

// ---------------------------------------------------------------------------
// Where a body's properties live
// ---------------------------------------------------------------------------

/// Which record table a property is addressed against.
///
/// A record index only means anything relative to one of these: the document's
/// own table, or one precomp's, whose indices are local to it so the asset can
/// be replayed per instantiation.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Table {
    Doc,
    Asset(u32),
}

/// What `thisProperty` is, for a property carrying a body.
///
/// The runtime picks one of three shapes from the expression's value source
/// (`expr::thisPropertyFor`), and they do not offer the same accessors: a path
/// property has the geometry ones and *not* `key`/`nearestKey`/`valueAtTime`/
/// `velocityAtTime`/`loopOut`. Which one it is has never been a run-time
/// question — it follows from the fallback the planner already resolved.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Surface {
    /// A keyframed value source: the key / velocity / loop API, and a real
    /// `numKeys`.
    Keyed,
    /// A path value source: the geometry accessors, and none of the key ones.
    Path,
    /// Anything else — a static source, a keyframed *path*, or none at all.
    /// The key API is stubbed, so it answers rather than being absent.
    Stub,
}

impl Surface {
    /// Which shape the runtime will build for this value source.
    ///
    /// Mirrors `resolve` in `runtime/kf.js`: only a `T_PATH` property is handed
    /// a `pathv`, and only a non-path `Anim` is handed a `kf`, so a keyframed
    /// path lands on the stub.
    fn of(fallback: Option<&Prop>) -> Self {
        match fallback {
            Some(Prop::Path(_)) => Surface::Path,
            Some(Prop::Anim(a)) if a.kind != crate::scene::prop::AnimKind::Path => Surface::Keyed,
            _ => Surface::Stub,
        }
    }

    /// Whether `key`, `nearestKey`, `valueAtTime`, `velocityAtTime` and
    /// `loopOut` exist on it.
    pub fn has_keys(self) -> bool {
        !matches!(self, Surface::Path)
    }
}

/// One property carrying an expression: which table it is addressed against,
/// which record owns it, which composition that record sits in, and what
/// `thisProperty` will be there.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Site {
    pub table: Table,
    pub owner: u32,
    pub scope: u32,
    pub surface: Surface,
}

/// Every expression site in the scene, grouped by expression id.
///
/// A property with no owning layer contributes a site with no owner, which is
/// recorded as an outright refusal: a body that reaches a layer has nothing to
/// resolve *from* when `thisLayer` is null.
///
/// **Missing a site is a wrong fold, not a missed one.** [`agree`] folds over
/// the uses this finds; a use it never saw cannot disagree, so an incomplete
/// walk emits a literal that is right for the sites it visited and wrong for
/// the one it did not — silently. That is the one failure mode in this file
/// that costs correctness rather than bytes, which is why the two structs are
/// destructured exhaustively below: a `Prop`-carrying field added to either
/// stops compiling here until it is either walked or explicitly waved past.
pub fn sites(data: &SceneData) -> BTreeMap<u32, Vec<Option<Site>>> {
    // Bound rather than reached through `data.`, so the compiler checks the
    // list. The four names below are the whole of it; everything bound to `_`
    // holds numbers, ids or markup and cannot carry a property.
    let SceneData {
        b: bindings,
        layers,
        remaps,
        assets,
        scopes,
        fr: _,
        ip: _,
        op: _,
        uses_ids: _,
        uses_clone_ids: _,
        easings: _,
        timelines: _,
        slots: _,
        gates: _,
        bind_gate: _,
        tpl: _,
        uses: _,
        names: _,
        stream: _,
        strings: _,
    } = data;

    let mut out: BTreeMap<u32, Vec<Option<Site>>> = BTreeMap::new();

    let mut note = |p: &Prop, table: Table, scopes: &[u32]| {
        walk_prop(p, &mut |id, layer, surface| {
            let site = layer.map(|owner| Site {
                table,
                owner,
                scope: scopes.get(owner as usize).copied().unwrap_or(0),
                surface,
            });
            out.entry(id).or_default().push(site);
        });
    };

    let doc = |p: &Prop, note: &mut dyn FnMut(&Prop, Table, &[u32])| {
        note(p, Table::Doc, scopes);
    };

    for b in bindings {
        for a in &b.args {
            walk_arg(a, &mut |p| doc(p, &mut note));
        }
    }
    for r in layers {
        for p in record_props(r) {
            doc(p, &mut note);
        }
    }
    for r in remaps.iter().flatten() {
        doc(r, &mut note);
    }

    for (k, asset) in assets.iter().enumerate() {
        // Same discipline as `SceneData` above. An asset has no remaps of its
        // own: its clock rows are `timelines`, which carry numbers.
        let crate::scene::AssetPlan {
            bindings,
            records,
            scopes: _,
            root: _,
            el_count: _,
            markup: _,
            template: _,
            slots: _,
            timelines: _,
            nested: _,
        } = asset;
        let table = Table::Asset(k as u32);
        for b in bindings {
            for a in &b.args {
                walk_arg(a, &mut |p| note(p, table, &asset.scopes));
            }
        }
        for r in records {
            for p in record_props(r) {
                note(p, table, &asset.scopes);
            }
        }
    }
    out
}

/// Every property hanging off a layer record, transforms and effect parameters
/// alike. Shared with the site walk so the two cannot drift.
fn record_props(r: &crate::scene::LayerRecord) -> impl Iterator<Item = &Prop> {
    let crate::scene::LayerRecord {
        p,
        a,
        sc,
        r: rot,
        o,
        h,
        ef,
        i: _,
        n: _,
        pr: _,
        offs: _,
    } = r;
    [p, a, sc, rot, o, h].into_iter().flatten().chain(
        ef.iter()
            .flat_map(|e| e.ef.iter().filter_map(|p| p.p.as_ref())),
    )
}

fn walk_arg(a: &Arg, f: &mut impl FnMut(&Prop)) {
    match a {
        Arg::Prop(p) => f(p),
        Arg::List(items) => {
            for i in items {
                walk_arg(i, f);
            }
        }
        _ => {}
    }
}

/// Every `Prop::Expr` reachable from `p`, fallbacks included — a fallback is a
/// property in its own right and can carry an expression of its own.
fn walk_prop(p: &Prop, f: &mut impl FnMut(u32, Option<u32>, Surface)) {
    if let Prop::Expr {
        id,
        fallback,
        layer,
    } = p
    {
        f(*id, *layer, Surface::of(fallback.as_deref()));
        if let Some(fb) = fallback {
            walk_prop(fb, f);
        }
    }
}

// ---------------------------------------------------------------------------
// The layer index
// ---------------------------------------------------------------------------

/// `(scope, name)` and `(scope, comp index)` to a record, per table.
///
/// A transcription of what the runtime used to build at mount, and no
/// smarter: a resolver that answered a lookup the runtime would have missed
/// would be resolving to a layer the animation never saw.
pub struct Index {
    by_name: BTreeMap<(Table, u32, String), Option<u32>>,
    by_ind: BTreeMap<(Table, u32, u32), Option<u32>>,
}

impl Index {
    pub fn build(data: &SceneData) -> Index {
        let mut ix = Index {
            by_name: BTreeMap::new(),
            by_ind: BTreeMap::new(),
        };
        ix.add(Table::Doc, &data.layers, &data.scopes, &data.names);
        for (k, a) in data.assets.iter().enumerate() {
            ix.add(Table::Asset(k as u32), &a.records, &a.scopes, &data.names);
        }
        ix
    }

    fn add(
        &mut self,
        table: Table,
        recs: &[crate::scene::LayerRecord],
        scopes: &[u32],
        names: &[String],
    ) {
        for (i, r) in recs.iter().enumerate() {
            let scope = scopes.get(i).copied().unwrap_or(0);
            // A second record answering to the same key poisons the entry
            // rather than overwriting it: the runtime's map keeps the last
            // writer, and picking one here would be a coin toss that renders.
            bump(&mut self.by_ind, (table, scope, r.i), i as u32);
            if let Some(n) = r.n
                && let Some(name) = names.get(n as usize)
            {
                bump(&mut self.by_name, (table, scope, name.clone()), i as u32);
            }
        }
    }

    /// The record a `thisComp.layer('name')` in `scope` resolves to.
    pub fn by_name(&self, table: Table, scope: u32, name: &str) -> Option<u32> {
        *self.by_name.get(&(table, scope, name.to_string()))?
    }

    /// The record a composition index resolves to — what a layer-control
    /// effect parameter holds, and what `thisComp.layer(3)` would mean.
    pub fn by_index(&self, table: Table, scope: u32, ind: u32) -> Option<u32> {
        *self.by_ind.get(&(table, scope, ind))?
    }
}

/// What every body of a fixture rewrites to, against the scene that ships.
///
/// These run the whole pass — site collection, the index, the typed walk and
/// the splice — over the real corpus rather than over synthetic input, because
/// every interesting case here is one After Effects actually emitted and none
/// of them would survive being paraphrased.
#[cfg(test)]
mod rewrite_tests {
    use super::*;

    fn plans(name: &str, instance: bool) -> Result<Vec<Plan>> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../_fixtures/animations")
            .join(format!("{name}.json"));
        let json = std::fs::read_to_string(path).unwrap();
        let animation: crate::lottie::Animation = serde_json::from_str(&json).unwrap();
        let module = crate::ir::lower(&animation).unwrap();
        let payload = crate::data::encode(&module).unwrap();
        let exprs: Vec<_> = module.expressions.iter().cloned().collect();
        let scene = crate::scene::plan_with(&payload, true, 24576, instance, &exprs).unwrap();
        plan_bodies(&scene.data, &exprs)
    }

    /// A body with every run of whitespace collapsed, so an assertion can be
    /// written on one line without pinning the formatter.
    fn flat(p: &Plan) -> String {
        p.body.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn every_corpus_body_resolves() {
        // There is no fallback: a reference the pass cannot resolve fails the
        // compile. So this is the gate that says the pass still covers the
        // corpus — run with ULOTTIE_WHY=1 to see which construct defeated it.
        for (name, inst) in [
            ("lights", false),
            ("starfish", false),
            ("ripple", false),
            ("ripple", true),
        ] {
            plans(name, inst).unwrap_or_else(|e| panic!("{name} instanced={inst}: {e}"));
        }
    }

    #[test]
    fn a_name_lookup_and_a_drill_chain_collapse_to_one_slot() {
        // lights E[0], verbatim, is the whole inventory in five lines: a name
        // lookup, an already-resolved effect read, the argument-ignoring
        // content drill, and the two space/path calls. `wire` is record 8 for
        // all five layers that name it, so the absolute spelling is valid.
        let p = &plans("lights", false).unwrap()[0];
        assert_eq!(
            flat(p),
            "var $bm_rt; \
             var pathLayer = lyAt(thisLayer, 8); \
             var progress = div(lyEffect(thisLayer, 0, 0, frame), 100); \
             var pathToTrace = pathLayer; \
             $bm_rt = toComp(pathLayer, pointOnPath(lyPath(pathToTrace, frame), progress), frame);"
        );
        // The drill chain landed on the layer's first path, not on some deeper
        // object: reproducing After Effects' real drill-down would move pixels.
        assert!(p.helpers.contains("lyPath"));
    }

    #[test]
    fn an_inlined_precomp_resolves_by_offset_and_an_instanced_one_absolutely() {
        // The same four ripple bodies, both ways round. Inlined, `comp_79` is
        // flattened twenty-three times and one body serves all of them, so no
        // absolute literal is valid and only the owner-relative offset is.
        let inlined = plans("ripple", false).unwrap();
        assert!(
            flat(&inlined[0]).contains("lyRel(thisLayer, 3)"),
            "{}",
            inlined[0].body
        );
        assert!(
            flat(&inlined[2]).contains("lyRel(thisLayer, 2)"),
            "{}",
            inlined[2].body
        );
        assert_eq!(
            flat(&inlined[3]),
            "var $bm_rt; $bm_rt = lyPos(lyRel(thisLayer, -2), frame);"
        );
        assert!(
            flat(&inlined[4]).contains("lyEffect(lyRel(thisLayer, -3),"),
            "{}",
            inlined[4].body
        );

        // Instanced, the asset is planned once with indices local to it, so the
        // absolute slot is the same for all forty-eight instantiations.
        let instanced = plans("ripple", true).unwrap();
        assert!(
            flat(&instanced[0]).contains("lyAt(thisLayer, 3)"),
            "{}",
            instanced[0].body
        );
        assert_eq!(
            flat(&instanced[3]),
            "var $bm_rt; $bm_rt = lyPos(lyAt(thisLayer, 0), frame);"
        );
    }

    #[test]
    fn a_layer_control_table_becomes_a_literal_list_of_slots() {
        // lights E[3] and starfish E[2]: `effect(names[i])(0)` where the
        // parameter is a ty-10 layer control holding a static comp index. The
        // loop still runs, but every element is already the layer.
        let lights = &plans("lights", false).unwrap()[3];
        assert!(
            flat(lights).contains(
                "var nullLayerNames = [lyAt(thisLayer, 3), lyAt(thisLayer, 2), lyAt(thisLayer, 1)];"
            ),
            "{}",
            lights.body
        );
        assert!(
            flat(lights).contains("getNullLayers.push(nullLayerNames[i]);"),
            "{}",
            lights.body
        );

        let starfish = &plans("starfish", false).unwrap()[2];
        let want: Vec<String> = (5..=14)
            .rev()
            .map(|i| format!("lyAt(thisLayer, {i})"))
            .collect();
        assert!(
            flat(starfish).contains(&format!("var nullLayerNames = [{}];", want.join(", "))),
            "{}",
            starfish.body
        );
    }

    #[test]
    fn the_property_surface_is_not_mistaken_for_a_layer() {
        // The easiest thing in this pass to get wrong. lights E[3] reads
        // `origPath.points()` off `thisProperty` and, four lines later,
        // `getNullLayers[i]` off a layer. Same spelling, different objects.
        let p = &plans("lights", false).unwrap()[3];
        let body = flat(p);
        assert!(
            body.contains("var origPoints = origPath.points();"),
            "{body}"
        );
        assert!(body.contains("origPath.inTangents()"), "{body}");
        assert!(body.contains("origPath.isClosed()"), "{body}");
        // …while the layer half did move.
        assert!(body.contains("lyAnchor(getNullLayers[i], frame)"), "{body}");
        // `.index` on a layer is `rec.i`; `nearestKey(time).index` in E[2] is a
        // keyframe's and must not be touched.
        assert!(body.contains("getNullLayers[i].i != thisLayer.i"), "{body}");
        assert!(flat(&plans("lights", false).unwrap()[2]).contains("nearestKey(time).index"));
    }

    #[test]
    fn a_bare_free_call_is_spelled_out_rather_than_shimmed() {
        // `fromCompToSurface(pt)` means the owning layer's inverse transform.
        // Binding a shim for it would shadow the runtime function the rewrite
        // now calls, and recurse.
        let p = &plans("starfish", false).unwrap()[2];
        assert!(
            flat(p).contains("fromCompToSurface(toComp(getNullLayers[i], lyAnchor(getNullLayers[i], frame), frame), thisLayer, frame)"),
            "{}",
            p.body
        );
    }

    #[test]
    fn thiscomp_frame_duration_becomes_a_preamble_binding() {
        // The only other thing bodies asked `thisComp` for. Once it and the
        // layer lookup are gone, `thisComp` itself can leave the runtime.
        let p = &plans("starfish", false).unwrap()[1];
        assert!(p.frame_duration, "{}", p.body);
        assert!(!p.body.contains("thisComp"), "{}", p.body);
        assert!(flat(p).contains("div(frameDuration, 10)"), "{}", p.body);
    }

    #[test]
    fn an_effect_read_on_another_layer_resolves_to_slots() {
        // ripple E[4] is `thisComp.layer('traceNull').effect('Trace Path')
        // ('Progress')`. `expr::resolve` leaves it alone — it only resolves the
        // owning layer's own effects — so this is the last name lookup in the
        // corpus, and without it the shipped module would still scan an effect
        // list by string once per frame, per binding.
        let inlined = &plans("ripple", false).unwrap()[4];
        assert_eq!(
            flat(inlined),
            "var $bm_rt; $bm_rt = lyEffect(lyRel(thisLayer, -3), 0, 0, frame);"
        );
        let instanced = &plans("ripple", true).unwrap()[4];
        assert_eq!(
            flat(instanced),
            "var $bm_rt; $bm_rt = lyEffect(lyAt(thisLayer, 0), 0, 0, frame);"
        );
    }

    #[test]
    fn a_body_that_reaches_no_layer_is_left_exactly_as_it_was() {
        // starfish E[0] and ripple E[5] are `loopOut('cycle')` — no layer, no
        // helpers, nothing to rewrite. A pass that touched these would be
        // churning bytes for nothing.
        let p = &plans("starfish", false).unwrap()[0];
        assert_eq!(flat(p), "var $bm_rt; $bm_rt = loopOut('cycle');");
        assert!(p.helpers.is_empty());
    }
}

#[cfg(test)]
mod index_tests {
    use super::*;
    use crate::scene::Scene;

    /// Plan a fixture the way `backend::report` does, so the record table under
    /// test is the one that ships.
    fn plan(name: &str, instance: bool) -> Scene {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../_fixtures/animations")
            .join(format!("{name}.json"));
        let json = std::fs::read_to_string(path).unwrap();
        let animation: crate::lottie::Animation = serde_json::from_str(&json).unwrap();
        let module = crate::ir::lower(&animation).unwrap();
        let payload = crate::data::encode(&module).unwrap();
        let bodies: Vec<_> = module.expressions.iter().cloned().collect();
        crate::scene::plan_with(&payload, true, 24576, instance, &bodies).unwrap()
    }

    // Every number below was read out of a planned scene, not out of the Lottie
    // source: `LayerRecord.i` is the After Effects `ind` and the record index is
    // the position in the table, and they only coincide by accident.

    #[test]
    fn lights_finds_the_layer_its_expressions_name() {
        let s = plan("lights", false);
        let ix = Index::build(&s.data);
        // Five layers name `wire`, and it is one record for all five — the case
        // an absolute handle exists for.
        assert_eq!(ix.by_name(Table::Doc, 0, "wire"), Some(8));
        assert_eq!(ix.by_name(Table::Doc, 0, "no such layer"), None);
    }

    #[test]
    fn lights_resolves_its_layer_control_targets() {
        let s = plan("lights", false);
        let ix = Index::build(&s.data);
        // `wire`'s three `ADBE Layer Control-0001` parameters hold comp indices
        // 4, 3 and 2, which are records 3, 2 and 1.
        let got: Vec<_> = [4, 3, 2]
            .iter()
            .map(|i| ix.by_index(Table::Doc, 0, *i))
            .collect();
        assert_eq!(got, [Some(3), Some(2), Some(1)]);
    }

    #[test]
    fn starfish_layer_controls_are_the_records_they_name() {
        let s = plan("starfish", false);
        let ix = Index::build(&s.data);
        for ind in 5u32..=14 {
            assert_eq!(
                ix.by_index(Table::Doc, 0, ind),
                Some(ind),
                "starfish comp index {ind}"
            );
        }
    }

    #[test]
    fn ripple_answers_per_composition_when_its_precomp_is_inlined() {
        // Twenty-three copies of one precomp, each its own scope, each with a
        // `bar`. A resolver that ignored scope would answer with whichever copy
        // happened to be written last.
        let s = plan("ripple", false);
        let ix = Index::build(&s.data);
        assert_eq!(ix.by_name(Table::Doc, 2, "bar"), Some(5));
        assert_eq!(ix.by_name(Table::Doc, 3, "bar"), Some(10));
        assert_eq!(ix.by_name(Table::Doc, 2, "traceNull"), Some(2));
        assert_eq!(ix.by_name(Table::Doc, 3, "traceNull"), Some(7));
    }

    #[test]
    fn ripple_instanced_answers_against_the_assets_own_table() {
        let s = plan("ripple", true);
        let ix = Index::build(&s.data);
        // The asset is planned once, with indices local to it, and every
        // instantiation replays those.
        let a = Table::Asset(0);
        let scope = s.data.assets[0].scopes[0];
        assert_eq!(ix.by_name(a, scope, "traceNull"), Some(0));
        assert_eq!(ix.by_name(a, scope, "bar"), Some(3));
    }

    #[test]
    fn a_name_two_records_share_resolves_to_neither() {
        // The fail-safe the runtime cannot express: its map keeps the last
        // writer, so picking one here would render a coin toss.
        let mut m: BTreeMap<(Table, u32, String), Option<u32>> = BTreeMap::new();
        bump(&mut m, (Table::Doc, 0, "Ball".into()), 1);
        bump(&mut m, (Table::Doc, 0, "Ball".into()), 4);
        assert_eq!(m[&(Table::Doc, 0, "Ball".to_string())], None);
    }

    #[test]
    fn agreement_prefers_an_absolute_slot_and_falls_back_to_a_delta() {
        let site = |owner| Site {
            table: Table::Doc,
            owner,
            scope: 0,
            surface: Surface::Stub,
        };
        let rec = |o: u32| (site(o), Target::Rec(8));
        // lights: five owners, one target.
        let lights: Vec<_> = [0, 4, 5, 6, 7].iter().map(|o| rec(*o)).collect();
        assert_eq!(agree(&lights), Some(Handle::At(8)));
        // ripple inlined: the target moves with the owner, so only the offset
        // is shared. No absolute literal is valid here at all.
        let ripple: Vec<_> = [2u32, 7, 12]
            .iter()
            .map(|o| (site(*o), Target::Rec(o + 3)))
            .collect();
        assert_eq!(agree(&ripple), Some(Handle::Rel(3)));
        // Neither: refuse rather than pick.
        assert_eq!(
            agree(&[(site(0), Target::Rec(8)), (site(1), Target::Rec(20))]),
            None
        );
        // Found here, absent there, is not an agreement either — one use would
        // take the guarded branch and the other would not.
        assert_eq!(
            agree(&[(site(0), Target::Rec(8)), (site(4), Target::NoLayer)]),
            None
        );
        // Nor is a reference every use agrees resolves to *nothing*. There is
        // no spelling for it: the free functions are null-safe, so neither
        // `null` nor `0` would throw the way the proxy did, and the body would
        // compute on a fabricated value instead of falling back. See `Handle`.
        assert_eq!(
            agree(&[(site(0), Target::NoLayer), (site(4), Target::NoLayer)]),
            None
        );
        assert_eq!(
            agree(&[(site(0), Target::NoEffect), (site(4), Target::NoLayer)]),
            None
        );
        assert_eq!(
            agree(&[(site(0), Target::NoEffect), (site(4), Target::NoEffect)]),
            None
        );
    }
}

fn bump<K: Ord>(m: &mut BTreeMap<K, Option<u32>>, k: K, v: u32) {
    m.entry(k)
        .and_modify(|slot| {
            if *slot != Some(v) {
                *slot = None;
            }
        })
        .or_insert(Some(v));
}

// ---------------------------------------------------------------------------
// Agreeing on a spelling
// ---------------------------------------------------------------------------

/// What one using property's copy of a reference resolves to.
///
/// Only [`Target::Rec`] has a spelling among the free functions. The other two
/// are recorded rather than collapsed into a refusal so that a disagreement is
/// still a disagreement — two uses that both found nothing are not the same
/// case as one that found a layer and one that did not — but neither of them
/// can be *written*, and [`agree`] says so.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    /// A record, by index into that use's table.
    Rec(u32),
    /// The lookup succeeded and found nothing. The proxy answered `null`.
    NoLayer,
    /// There was no effect or parameter to ask. `effect(k)` answered `() => 0`,
    /// so `effect(k)(0)` was the *number* 0.
    NoEffect,
}

/// How a resolved reference is written into a body.
///
/// Both spellings produce a record, and every free function over one is
/// null-safe by design — which is exactly why there is no third variant for a
/// reference that resolved to nothing. The proxy answered `null` for a missing
/// layer and the number `0` for a missing effect, and a body that went on to
/// read a member off either *threw*, landing in `evalExpr`'s catch and falling
/// back to the property's own value. Emitting `null` or `0` here would not
/// reproduce that: `lyPos(null, f)` is `[0, 0, 0]` and `lyAnchor(0, f)` is
/// `[0, 0, 0]`, so the body would compute on with a fabricated point instead of
/// aborting. Refusing hands the case to [`Plan::legacy`], which reproduces it
/// exactly — a gap costs bytes, never a rendering change.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Handle {
    /// The same absolute slot for every use.
    At(u32),
    /// The same offset from the owner for every use. Signed because a
    /// reference backwards is as common as one forwards.
    Rel(i64),
}

impl Handle {
    pub fn emit(&self) -> String {
        match self {
            Handle::At(i) => format!("lyAt(thisLayer, {i})"),
            Handle::Rel(d) => format!("lyRel(thisLayer, {d})"),
        }
    }

    pub fn helper(&self) -> &'static str {
        match self {
            Handle::At(_) => "lyAt",
            Handle::Rel(_) => "lyRel",
        }
    }
}

/// Fold one resolution per use into a single spelling, or refuse.
///
/// Mirrors the discipline `expr::resolve::Ref::resolve` already applies to
/// effect indices: resolve once per user, compare, and take nothing on a
/// disagreement. Absolute is tried first because it is shorter and because it
/// survives a reference crossing tables; the delta is what rescues a body
/// shared by the twenty-three inlined copies of one precomp, where no absolute
/// literal is valid at all.
pub fn agree(per_use: &[(Site, Target)]) -> Option<Handle> {
    let first = *per_use.first()?;
    let slot = |t: Target| match t {
        Target::Rec(r) => Some(r),
        // Not an agreement to be had: see [`Handle`]. Nothing the resolved
        // spelling can write behaves the way a reference to nothing behaved.
        _ => None,
    };
    if per_use.iter().any(|(_, t)| slot(*t).is_none()) {
        return None;
    }
    let same_table = per_use.iter().all(|(s, _)| s.table == first.0.table);
    if same_table && per_use.iter().all(|(_, t)| *t == first.1) {
        return Some(Handle::At(slot(first.1)?));
    }
    let delta = |(s, t): &(Site, Target)| Some(slot(*t)? as i64 - s.owner as i64);
    let d = delta(&first)?;
    per_use
        .iter()
        .all(|u| delta(u) == Some(d))
        .then_some(Handle::Rel(d))
}

// ---------------------------------------------------------------------------
// Rewriting a body
// ---------------------------------------------------------------------------

use anyhow::Result;
use oxc_allocator::Allocator;
use oxc_ast::ast;
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType};

/// What the backend needs to emit one body.
pub struct Plan {
    pub body: String,
    /// Runtime symbols the rewritten body calls, as shake roots. Exact by
    /// construction, unlike inferring them from a word scan.
    pub helpers: BTreeSet<&'static str>,
    /// `thisComp.frameDuration` was rewritten to a bare name.
    pub frame_duration: bool,
    /// What `thisProperty` is, when every property using this body agrees.
    ///
    /// `None` when they do not — bodies are deduplicated across every property
    /// they were applied to, so one preamble has to be right for all of them.
    /// The same rule as [`agree`]: fold only what every use site says.
    pub surface: Option<Surface>,
}

/// The expression table for one scene: the source, plus what emitting it costs.
pub struct Exprs {
    /// The whole `const E = [ … ];` declaration.
    pub src: String,
    /// Runtime symbols the bodies call, as shake roots and extern imports.
    pub helpers: BTreeSet<&'static str>,
    /// The finished body texts, for the analyses that decide what the payload
    /// can drop. They run on this rather than on the IR so they see what
    /// actually ships — a name the rewrite removed is a name nothing looks up.
    pub bodies: Vec<String>,
}

/// Rewrite every body in the module against a planned scene.
///
/// Runs per scene rather than once per module, because a record index only
/// means anything relative to the table the planner built — and the inlined and
/// instanced candidates build different ones.
pub fn plan_bodies(data: &SceneData, exprs: &[ir::Expression]) -> Result<Vec<Plan>> {
    let index = Index::build(data);
    let all = sites(data);
    exprs
        .iter()
        .map(|e| {
            let id = e.id.0;
            match all.get(&id) {
                // Every property using this body must have an owning layer:
                // `lyAt`/`lyRel` are rooted at `thisLayer`, and a body with no
                // owner has nothing to root them in. A body no property uses
                // at all is refused for the same reason — there is nothing to
                // agree with.
                Some(uses) if !uses.is_empty() && uses.iter().all(|u| u.is_some()) => {
                    let uses: Vec<Site> = uses.iter().map(|u| u.unwrap()).collect();
                    rewrite(&e.body, &uses, &index, data, id)
                }
                _ => {
                    why(id, "no owning layer to resolve against");
                    None
                }
            }
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "expression E[{id}] reaches a layer in a way the compiler cannot resolve. \
                     Set ULOTTIE_WHY=1 for the reason.\n{}",
                    e.body.trim()
                )
            })
        })
        .collect()
}

///
/// A refused body is refused precisely because the pass could not tell what it
/// reaches, so the fallback surface ships whole rather than trimmed to a word
/// scan of a body nobody understood. That is the house rule: a gap costs bytes.
/// Rewrite every body and emit the `const E = […]` table for one scene.
///
/// The table is built here rather than once per module because a record index
/// only means anything against the table the planner built, and the inlined and
/// instanced candidates build different ones.
pub fn table(data: &SceneData, exprs: &[ir::Expression]) -> Result<Exprs> {
    let plans = plan_bodies(data, exprs)?;

    let mut src = String::from("const E = [\n");
    let mut helpers: BTreeSet<&'static str> = BTreeSet::new();
    let mut bodies = Vec::with_capacity(plans.len());

    for plan in &plans {
        if super::emit_expressions::emit_one(&mut src, &plan.body, plan) {
            helpers.insert("thisPropertyFor");
        }
        helpers.extend(plan.helpers.iter().copied());
        // What ships, not what the IR held: the analyses that decide whether a
        // name can leave the payload run on this, and a lookup the rewrite
        // removed is a lookup nothing performs any more.
        bodies.push(plan.body.clone());
    }
    src.push_str("];\n");

    // Path helpers called by bare name in the shipped bodies. These are exported
    // from `expr.js`, not `ctx` properties, so they have to be roots to be
    // retained (embedded) or imported (extern). The rewrite reports
    // `pointOnPath`/`tangentOnPath` via `need()` when it converts member calls,
    // but `createPath` is a bare call it does not touch — scanning the finished
    // bodies catches all three.
    helpers.extend(super::emit_expressions::bare_helpers(&bodies));

    Ok(Exprs {
        src,
        helpers,
        bodies,
    })
}

fn why(id: u32, reason: &str) {
    if super::why() {
        eprintln!("layers: E[{id}] falls back — {reason}");
    }
}

/// A value's type, as far as the rewrite needs to care.
///
/// The direction matters more than the lattice does: `origPath.points()` in
/// `lights` and `barLayer.content('Path 1').path.points()` in `ripple` are the
/// same three tokens, and only one of them is a layer. Rewriting the wrong one
/// silently reads geometry off a different object.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ty {
    Layer,
    /// An array whose elements are layers.
    LayerArray,
    Other,
}

struct Rw<'a> {
    src: &'a str,
    uses: &'a [Site],
    index: &'a Index,
    data: &'a SceneData,
    id: u32,
    ty: BTreeMap<String, Ty>,
    /// Integer arrays that index into `effect(…)` and so hold layer controls.
    tables: BTreeMap<String, LayerTable>,
    helpers: BTreeSet<&'static str>,
    refused: bool,
    frame_duration: bool,
}

fn rewrite(body: &str, uses: &[Site], index: &Index, data: &SceneData, id: u32) -> Option<Plan> {
    let first = uses.first().map(|u| u.surface);
    let surface = first.filter(|s| uses.iter().all(|u| u.surface == *s));
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, body, SourceType::cjs()).parse();
    if !parsed.errors.is_empty() {
        why(id, "the body does not parse");
        return None;
    }

    let mut rw = Rw {
        src: body,
        uses,
        index,
        data,
        id,
        ty: BTreeMap::new(),
        tables: BTreeMap::new(),
        helpers: BTreeSet::new(),
        refused: false,
        frame_duration: false,
    };

    // Types first, over the whole body. A single ordered pass would do for the
    // straight-line shape After Effects emits, but `getNullLayers` is declared
    // empty and only becomes an array of layers once the loop below it pushes
    // into it — so the walk runs to a fixed point instead of assuming an order.
    rw.tables = layer_control_tables(&parsed.program);
    for _ in 0..2 {
        for stmt in &parsed.program.body {
            rw.infer_stmt(stmt);
        }
    }

    let mut cuts: Vec<(usize, usize, String)> = Vec::new();
    for stmt in &parsed.program.body {
        rw.cut_stmt(stmt, &mut cuts);
    }

    if rw.refused {
        return None;
    }
    let mut out = body.to_string();
    cuts.sort_by_key(|(s, _, _)| std::cmp::Reverse(*s));
    for (s, e, text) in cuts {
        out.replace_range(s..e, &text);
    }

    // The walk's own optimism does not get the last word.
    let legacy = verify(&out);
    if legacy {
        why(id, "a layer member survived the rewrite");
        return None;
    }
    Some(Plan {
        body: out,
        helpers: rw.helpers,
        frame_duration: rw.frame_duration,
        surface,
    })
}

// ---------------------------------------------------------------------------
// Type inference
// ---------------------------------------------------------------------------

/// Integer arrays that only ever index into `effect(…)`, with the parameter
/// slot every element shares.
///
/// `expr::resolve::Ref::Table` has already turned the names After Effects wrote
/// into effect indices, so what is left is `var t = [0, 1, 2]` iterated with
/// `t.length` and read as `effect(t[i])(0)`. Each element then names a layer
/// through a layer-control parameter, which is a compile-time constant.
fn layer_control_tables(program: &ast::Program) -> BTreeMap<String, LayerTable> {
    let mut arrays: BTreeMap<String, (Vec<u32>, (u32, u32))> = BTreeMap::new();
    let mut used: BTreeMap<String, Option<u32>> = BTreeMap::new();
    collect_tables(&program.body, &mut arrays, &mut used);
    used.into_iter()
        .filter_map(|(name, param)| {
            let (elems, span) = arrays.get(&name)?;
            Some((
                name,
                LayerTable {
                    param: param?,
                    elems: elems.clone(),
                    span: *span,
                },
            ))
        })
        .collect()
}

/// One `var t = [0, 1, 2]` read as `effect(t[i])(param)`.
///
/// The span is what the rewrite keys on: keying on the variable name would
/// depend on where the declaration sits, and keying on the literal's contents
/// would confuse two tables that happen to hold the same indices.
struct LayerTable {
    param: u32,
    elems: Vec<u32>,
    span: (u32, u32),
}

fn collect_tables(
    stmts: &oxc_allocator::Vec<ast::Statement>,
    arrays: &mut BTreeMap<String, (Vec<u32>, (u32, u32))>,
    used: &mut BTreeMap<String, Option<u32>>,
) {
    for s in stmts {
        each_stmt_expr(s, &mut |e| find_table_use(e, used));
        each_sub_stmt(s, &mut |inner| collect_tables_one(inner, used));
        if let ast::Statement::VariableDeclaration(d) = s {
            for decl in &d.declarations {
                let (Some(id), Some(ast::Expression::ArrayExpression(a))) =
                    (decl.id.get_binding_identifier(), decl.init.as_ref())
                else {
                    continue;
                };
                let nums: Option<Vec<u32>> = a
                    .elements
                    .iter()
                    .map(|el| match el.as_expression()? {
                        ast::Expression::NumericLiteral(n)
                            if n.value >= 0.0 && n.value.fract() == 0.0 =>
                        {
                            Some(n.value as u32)
                        }
                        _ => None,
                    })
                    .collect();
                if let Some(nums) = nums.filter(|n| !n.is_empty()) {
                    arrays.insert(id.name.to_string(), (nums, (a.span.start, a.span.end)));
                }
            }
        }
    }
}

fn collect_tables_one(s: &ast::Statement, used: &mut BTreeMap<String, Option<u32>>) {
    each_stmt_expr(s, &mut |e| find_table_use(e, used));
    each_sub_stmt(s, &mut |inner| collect_tables_one(inner, used));
}

/// `effect(t[i])(<int>)` anywhere under `e`.
fn find_table_use(e: &ast::Expression, used: &mut BTreeMap<String, Option<u32>>) {
    each_sub_expr(e, &mut |x| find_table_use(x, used));
    let ast::Expression::CallExpression(outer) = e else {
        return;
    };
    let Some(param) = int_arg(&outer.arguments) else {
        return;
    };
    let ast::Expression::CallExpression(inner) = &outer.callee else {
        return;
    };
    if !is_bare_effect(&inner.callee) || inner.arguments.len() != 1 {
        return;
    }
    let Some(ast::Expression::ComputedMemberExpression(m)) =
        inner.arguments.first().and_then(|a| a.as_expression())
    else {
        return;
    };
    let ast::Expression::Identifier(id) = &m.object else {
        return;
    };
    // Two different parameter slots off one table is a shape After Effects
    // does not generate, and one literal cannot serve both.
    used.entry(id.name.to_string())
        .and_modify(|p| {
            if *p != Some(param) {
                *p = None;
            }
        })
        .or_insert(Some(param));
}

fn int_arg(args: &oxc_allocator::Vec<ast::Argument>) -> Option<u32> {
    if args.len() != 1 {
        return None;
    }
    match args.first()?.as_expression()? {
        ast::Expression::NumericLiteral(n) if n.value >= 0.0 && n.value.fract() == 0.0 => {
            Some(n.value as u32)
        }
        _ => None,
    }
}

fn is_bare_effect(callee: &ast::Expression) -> bool {
    matches!(callee, ast::Expression::Identifier(id) if id.name == "effect")
}

impl Rw<'_> {
    fn ty_of_name(&self, name: &str) -> Ty {
        self.ty.get(name).copied().unwrap_or(Ty::Other)
    }

    /// Record a binding's type, poisoning on a second, different assignment.
    fn bind(&mut self, name: &str, ty: Ty) {
        match self.ty.get(name) {
            Some(prev) if *prev != ty => {
                self.ty.insert(name.to_string(), Ty::Other);
            }
            Some(_) => {}
            None => {
                self.ty.insert(name.to_string(), ty);
            }
        }
    }

    fn infer_stmt(&mut self, s: &ast::Statement) {
        if let ast::Statement::VariableDeclaration(d) = s {
            for decl in &d.declarations {
                let Some(id) = decl.id.get_binding_identifier() else {
                    continue;
                };
                let name = id.name.to_string();
                if self.tables.contains_key(&name) {
                    self.bind(&name, Ty::LayerArray);
                    continue;
                }
                match decl.init.as_ref() {
                    // `var out = []` only says what it holds once something is
                    // pushed into it, so it stays unknown until the push is seen.
                    Some(ast::Expression::ArrayExpression(a)) if a.elements.is_empty() => {}
                    Some(e) => {
                        let ty = if self.is_layer(e) {
                            Ty::Layer
                        } else {
                            Ty::Other
                        };
                        self.bind(&name, ty);
                    }
                    None => {}
                }
            }
        }
        each_stmt_expr(s, &mut |e| {
            // Assignment and `arr.push(x)` are the two ways a name acquires a
            // type outside its declaration.
            match e {
                ast::Expression::AssignmentExpression(a) => {
                    if let Some(id) = a.left.get_identifier_name() {
                        let ty = if self.is_layer(&a.right) {
                            Ty::Layer
                        } else {
                            Ty::Other
                        };
                        let name = id.to_string();
                        self.bind(&name, ty);
                    }
                }
                ast::Expression::CallExpression(c) => {
                    if let ast::Expression::StaticMemberExpression(m) = &c.callee
                        && m.property.name == "push"
                        && let ast::Expression::Identifier(id) = &m.object
                    {
                        let holds_layer = c
                            .arguments
                            .iter()
                            .filter_map(|a| a.as_expression())
                            .all(|x| self.is_layer(x) || is_null(x));
                        let name = id.name.to_string();
                        if holds_layer && !c.arguments.is_empty() {
                            self.bind(&name, Ty::LayerArray);
                        } else {
                            self.bind(&name, Ty::Other);
                        }
                    }
                }
                _ => {}
            }
        });
        each_sub_stmt(s, &mut |inner| self.infer_stmt(inner));
    }

    /// Whether `e` produces a layer. Answers without rewriting anything, so it
    /// is safe to call during inference.
    fn is_layer(&self, e: &ast::Expression) -> bool {
        match e {
            ast::Expression::Identifier(id) => {
                id.name == "thisLayer" || self.ty_of_name(&id.name) == Ty::Layer
            }
            ast::Expression::ParenthesizedExpression(p) => self.is_layer(&p.expression),
            ast::Expression::StaticMemberExpression(m) => {
                matches!(
                    m.property.name.as_str(),
                    "transform" | "path" | "parentLayer"
                ) && self.is_layer(&m.object)
            }
            ast::Expression::ComputedMemberExpression(m) => match &m.object {
                ast::Expression::Identifier(id) => self.ty_of_name(&id.name) == Ty::LayerArray,
                _ => false,
            },
            ast::Expression::CallExpression(c) => {
                if comp_layer_name(c).is_some() {
                    return true;
                }
                // `effect(t[i])(p)` over a layer-control table.
                if self.layer_control_call(c).is_some() {
                    return true;
                }
                // The drill-down chain, and `.content(…)`: the proxy is a
                // function returning itself, so both land back on the layer.
                match &c.callee {
                    ast::Expression::StaticMemberExpression(m) if m.property.name == "content" => {
                        self.is_layer(&m.object)
                    }
                    other => self.is_layer(other),
                }
            }
            _ => false,
        }
    }

    /// `effect(t[i])(p)` where `t` is a layer-control table — the array name
    /// and the source text of the index expression.
    fn layer_control_call(&self, c: &ast::CallExpression) -> Option<String> {
        let param = int_arg(&c.arguments)?;
        let ast::Expression::CallExpression(inner) = &c.callee else {
            return None;
        };
        if !is_bare_effect(&inner.callee) || inner.arguments.len() != 1 {
            return None;
        }
        let ast::Expression::ComputedMemberExpression(m) =
            inner.arguments.first()?.as_expression()?
        else {
            return None;
        };
        let ast::Expression::Identifier(id) = &m.object else {
            return None;
        };
        (self.tables.get(id.name.as_str()).map(|t| t.param) == Some(param))
            .then(|| self.text(m.span()))
    }

    fn text(&self, sp: oxc_span::Span) -> String {
        self.src[sp.start as usize..sp.end as usize].to_string()
    }
}

fn is_null(e: &ast::Expression) -> bool {
    matches!(e, ast::Expression::NullLiteral(_))
}

/// One `effect(…)` argument, as far as it can be read at build time.
///
/// Both spellings ship: `expr::resolve` has already turned the owning layer's
/// own lookups into indices, and what is left is the names of effects on some
/// *other* layer, which it deliberately leaves alone.
enum Sel {
    Idx(u32),
    Name(String),
}

impl Sel {
    fn of(e: &ast::Expression) -> Option<Sel> {
        match e {
            ast::Expression::NumericLiteral(n) if n.value >= 0.0 && n.value.fract() == 0.0 => {
                Some(Sel::Idx(n.value as u32))
            }
            ast::Expression::StringLiteral(s) => Some(Sel::Name(s.value.to_string())),
            _ => None,
        }
    }

    /// Which slot of `names` this selects: `Some(None)` for nothing at all,
    /// `None` when two entries answer to it.
    ///
    /// The runtime takes the first match and so would picking one here, but a
    /// duplicate name means the payload is not what this pass thinks it is, and
    /// the fallback is cheaper than being clever. `expr::resolve::effect_index`
    /// refuses on the same ground.
    fn find(&self, names: &[(Option<&str>, Option<&str>)]) -> Option<Option<u32>> {
        match self {
            Sel::Idx(i) => Some(((*i as usize) < names.len()).then_some(*i)),
            Sel::Name(want) => {
                let mut found = None;
                for (k, (nm, mn)) in names.iter().enumerate() {
                    if *nm == Some(want.as_str()) || *mn == Some(want.as_str()) {
                        if found.is_some() {
                            return None;
                        }
                        found = Some(k as u32);
                    }
                }
                Some(found)
            }
        }
    }
}

/// `thisComp.layer('name')` — the literal name.
fn comp_layer_name(c: &ast::CallExpression) -> Option<String> {
    let ast::Expression::StaticMemberExpression(m) = &c.callee else {
        return None;
    };
    if m.property.name != "layer" {
        return None;
    }
    if !matches!(&m.object, ast::Expression::Identifier(o) if o.name == "thisComp") {
        return None;
    }
    match c.arguments.first()?.as_expression()? {
        ast::Expression::StringLiteral(s) if c.arguments.len() == 1 => Some(s.value.to_string()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

impl Rw<'_> {
    fn need(&mut self, sym: &'static str) {
        self.helpers.insert(sym);
    }

    fn refuse(&mut self, reason: &str) {
        why(self.id, reason);
        self.refused = true;
    }

    fn cut_stmt(&mut self, s: &ast::Statement, cuts: &mut Vec<(usize, usize, String)>) {
        each_stmt_expr(s, &mut |e| {
            if let Some(text) = self.render(e) {
                let sp = e.span();
                cuts.push((sp.start as usize, sp.end as usize, text));
            }
        });
        each_sub_stmt(s, &mut |inner| self.cut_stmt(inner, cuts));
    }

    /// The rewritten text for `e`, or `None` when nothing under it changed.
    ///
    /// Rendering recursively rather than collecting independent spans is what
    /// makes nesting safe: `pathLayer.toComp(pathToTrace.pointOnPath(u))` has an
    /// outer rewrite whose span *contains* an inner one, and a back-to-front
    /// splice of the two would apply the outer last and lose the inner.
    fn render(&mut self, e: &ast::Expression) -> Option<String> {
        // A layer-control table: its elements stop being effect indices and
        // become the layers those effects name.
        if let ast::Expression::ArrayExpression(a) = e
            && let Some(text) = self.render_control_table((a.span.start, a.span.end))
        {
            return Some(text);
        }
        // A layer-typed expression is rewritten to whatever produces its record.
        if self.is_layer(e) {
            // …except a chain that *ends* at `.path`, which is not a layer but
            // that layer's geometry. [`Self::is_layer`] calls it one on purpose:
            // that is what lets `X.content('a').content('b').path.points()`
            // resolve the record through the chain. In value position the
            // record is not the value, and emitting it produced a body that
            // returned a *layer object* — which `pathD` then read a `.v.length`
            // off and threw, taking the whole animation down at mount.
            if let ast::Expression::StaticMemberExpression(m) = e
                && m.property.name == "path"
            {
                let obj = self.layer_text(e)?;
                self.need("lyPath");
                return Some(format!("lyPath({obj}, frame)"));
            }
            let t = self.layer_text(e)?;
            return (t != self.text(e.span())).then_some(t);
        }
        match e {
            ast::Expression::StaticMemberExpression(m) => self.render_member(m),
            ast::Expression::CallExpression(c) => self.render_call(c),
            _ => self.render_children(e),
        }
    }

    /// `X.<accessor>` where `X` is a layer, plus `thisComp.frameDuration`.
    fn render_member(&mut self, m: &ast::StaticMemberExpression) -> Option<String> {
        if matches!(&m.object, ast::Expression::Identifier(o) if o.name == "thisComp")
            && m.property.name == "frameDuration"
        {
            self.frame_duration = true;
            return Some("frameDuration".to_string());
        }
        let Some(obj) = self.layer_text_if_layer(&m.object) else {
            return self.render_children_of_member(m);
        };
        let scalar = |f: &mut Self, sym: &'static str| {
            f.need(sym);
            Some(format!("{sym}({obj}, frame)"))
        };
        match m.property.name.as_str() {
            "position" => scalar(self, "lyPos"),
            "anchorPoint" => scalar(self, "lyAnchor"),
            "scale" => scalar(self, "lyScale"),
            "rotation" => scalar(self, "lyRot"),
            "opacity" => scalar(self, "lyOpacity"),
            // The Lottie `ind`, which is what the proxy's `index` was.
            "index" => Some(format!("{obj}.i")),
            // Reading a name means the name table has to survive, whatever the
            // rest of this analysis concluded.
            "name" => Some(format!("({obj}.n ?? null)")),
            _ => {
                self.refuse("an unrecognised layer member");
                None
            }
        }
    }

    fn render_call(&mut self, c: &ast::CallExpression) -> Option<String> {
        // Bare `fromCompToSurface(pt)` is AE for the owning layer's inverse.
        if let ast::Expression::Identifier(id) = &c.callee
            && id.name == "fromCompToSurface"
            && c.arguments.len() == 1
        {
            let pt = self.arg_text(c.arguments.first()?)?;
            self.need("fromCompToSurface");
            return Some(format!("fromCompToSurface({pt}, thisLayer, frame)"));
        }
        // `X.effect(a)(b)` and bare `effect(a)(b)`, uncurried.
        if let Some(text) = self.render_effect(c) {
            return Some(text);
        }
        let ast::Expression::StaticMemberExpression(m) = &c.callee else {
            return self.render_children_of_call(c);
        };
        let Some(obj) = self.layer_text_if_layer(&m.object) else {
            return self.render_children_of_call(c);
        };
        let arg = |f: &mut Self| -> Option<String> {
            match c.arguments.first() {
                Some(a) => f.arg_text(a),
                None => Some(String::new()),
            }
        };
        match m.property.name.as_str() {
            "toComp" => {
                let pt = arg(self)?;
                self.need("toComp");
                Some(format!("toComp({obj}, {pt}, frame)"))
            }
            "fromCompToSurface" => {
                let pt = arg(self)?;
                self.need("fromCompToSurface");
                Some(format!("fromCompToSurface({pt}, {obj}, frame)"))
            }
            "pointOnPath" | "tangentOnPath" => {
                let u = arg(self)?;
                let sym = if m.property.name == "pointOnPath" {
                    "pointOnPath"
                } else {
                    "tangentOnPath"
                };
                self.need(sym);
                self.need("lyPath");
                Some(format!("{sym}(lyPath({obj}, frame), {u})"))
            }
            "points" => {
                self.need("lyPoints");
                Some(format!("lyPoints({obj}, frame)"))
            }
            "inTangents" => {
                self.need("lyPoints");
                Some(format!("lyPoints({obj}, frame, 'i')"))
            }
            "outTangents" => {
                self.need("lyPoints");
                Some(format!("lyPoints({obj}, frame, 'o')"))
            }
            "isClosed" => {
                self.need("lyClosed");
                Some(format!("lyClosed({obj}, frame)"))
            }
            _ => {
                self.refuse("an unrecognised layer method");
                None
            }
        }
    }

    /// `X.effect(a)(b)` / `effect(a)(b)`. The layer-control form is a layer and
    /// was handled before this; what is left produces a value.
    ///
    /// The selectors are resolved to slots here, once per using property, for
    /// two reasons. The first is correctness: a parameter of type 10 is a layer
    /// control, which the proxy turned into a *layer* and `lyEffect` hands back
    /// as a raw number, so a body reading one has to take the fallback. The
    /// second is the point of the whole pass — `expr::resolve` already does
    /// this for the owning layer's own effects, and doing it here as well
    /// leaves nothing in the shipped module that searches an effect list by
    /// string once per frame.
    fn render_effect(&mut self, c: &ast::CallExpression) -> Option<String> {
        if c.arguments.len() != 1 {
            return None;
        }
        let ast::Expression::CallExpression(inner) = &c.callee else {
            return None;
        };
        if inner.arguments.len() != 1 {
            return None;
        }
        // `None` is the bare call, which After Effects defines as the owning
        // layer's own effects — no expression to resolve, the site says which.
        let obj = match &inner.callee {
            ast::Expression::Identifier(id) if id.name == "effect" => None,
            ast::Expression::StaticMemberExpression(m) if m.property.name == "effect" => {
                if !self.is_layer(&m.object) {
                    return None;
                }
                Some(&m.object)
            }
            _ => return None,
        };
        let name_e = inner.arguments.first()?.as_expression()?;
        let param_e = c.arguments.first()?.as_expression()?;
        let (Some(nsel), Some(psel)) = (Sel::of(name_e), Sel::of(param_e)) else {
            self.refuse("an effect selector that is not a literal");
            return None;
        };

        let slots = self.effect_slots(obj, &nsel, &psel)?;
        // Fold to positions only when every use put them in the same place. A
        // use that found nothing keeps the original spelling: `lyEffect`
        // answers 0 for a name it cannot find, which is what the proxy did too,
        // so the lookup is still correct — just not free.
        let agreed = slots
            .first()
            .copied()
            .flatten()
            .filter(|first| slots.iter().all(|s| *s == Some(*first)));
        let (name, param) = match agreed {
            Some((e, p)) => (e.to_string(), p.to_string()),
            None => (self.text(name_e.span()), self.text(param_e.span())),
        };

        let target = match obj {
            None => "thisLayer".to_string(),
            Some(o) => self.layer_text(o)?,
        };
        self.need("lyEffect");
        Some(format!("lyEffect({target}, {name}, {param}, frame)"))
    }

    /// Where `(nsel, psel)` lands on the layer `obj` denotes, once per use.
    ///
    /// `None` for a use whose effect or parameter is simply not there — a
    /// resolved answer, and the same 0 on both paths. `None` for the whole list
    /// is a refusal already recorded.
    fn effect_slots(
        &mut self,
        obj: Option<&ast::Expression>,
        nsel: &Sel,
        psel: &Sel,
    ) -> Option<Vec<Option<(u32, u32)>>> {
        // Copied out because resolving one use borrows `self` mutably to
        // refuse; `Site` is `Copy` and there are never many.
        let uses = self.uses.to_vec();
        let mut out = Vec::with_capacity(uses.len());
        for site in &uses {
            let rec = match obj {
                None => Some(site.owner),
                Some(o) => self.record_at(*site, o),
            }
            .and_then(|r| self.recs_of(site.table).get(r as usize));
            let Some(rec) = rec else {
                self.refuse("an effect read on a layer that would not resolve");
                return None;
            };
            let names: Vec<_> = rec
                .ef
                .iter()
                .map(|e| (e.nm.as_deref(), e.mn.as_deref()))
                .collect();
            let Some(es) = nsel.find(&names) else {
                self.refuse("two effects answer to one name");
                return None;
            };
            let Some(ei) = es else {
                out.push(None);
                continue;
            };
            let effect = &rec.ef[ei as usize];
            let params: Vec<_> = effect
                .ef
                .iter()
                .map(|p| (p.nm.as_deref(), p.mn.as_deref()))
                .collect();
            let Some(ps) = psel.find(&params) else {
                self.refuse("two parameters answer to one name");
                return None;
            };
            let Some(pi) = ps else {
                out.push(None);
                continue;
            };
            // A layer control names another layer. The proxy read it as one and
            // handed back a view; `lyEffect` is value-only and hands back the
            // raw index, so a body that walks into the result would read a
            // number where it expects a layer. The table form of this is folded
            // by `render_control_table` before ever reaching here; anything
            // else is a shape this pass does not know, so it goes to the
            // fallback rather than shipping a wrong answer.
            if effect.ef[pi as usize].ty == 10 {
                self.refuse("an effect read that lands on a layer-control parameter");
                return None;
            }
            out.push(Some((ei, pi)));
        }
        Some(out)
    }

    /// The records one table holds.
    fn recs_of(&self, table: Table) -> &[crate::scene::LayerRecord] {
        match table {
            Table::Doc => &self.data.layers,
            Table::Asset(i) => &self.data.assets[i as usize].records,
        }
    }

    /// The record index the layer expression `e` denotes at one use site.
    ///
    /// The build-time shadow of [`Self::layer_text`], which produces the text
    /// that will find the record at runtime. This answers the one question that
    /// text cannot be asked: what is actually *on* that layer. It is
    /// deliberately narrower than `layer_text` — a variable holding a layer has
    /// a spelling but no build-time answer — and every form it does not know is
    /// `None`, which its callers turn into a refusal rather than a guess.
    fn record_at(&self, site: Site, e: &ast::Expression) -> Option<u32> {
        match e {
            ast::Expression::Identifier(id) if id.name == "thisLayer" => Some(site.owner),
            ast::Expression::ParenthesizedExpression(p) => self.record_at(site, &p.expression),
            ast::Expression::StaticMemberExpression(m) => match m.property.name.as_str() {
                "transform" | "path" => self.record_at(site, &m.object),
                "parentLayer" => {
                    let r = self.record_at(site, &m.object)?;
                    self.recs_of(site.table).get(r as usize)?.pr
                }
                _ => None,
            },
            ast::Expression::CallExpression(c) => {
                if let Some(name) = comp_layer_name(c) {
                    return self.index.by_name(site.table, site.scope, &name);
                }
                match &c.callee {
                    ast::Expression::StaticMemberExpression(m) if m.property.name == "content" => {
                        self.record_at(site, &m.object)
                    }
                    other => self.record_at(site, other),
                }
            }
            _ => None,
        }
    }

    /// The record-producing text for `e`, or `None` when `e` is not a layer.
    fn layer_text_if_layer(&mut self, e: &ast::Expression) -> Option<String> {
        if !self.is_layer(e) {
            return None;
        }
        self.layer_text(e)
    }

    /// JS producing the record `e` denotes.
    fn layer_text(&mut self, e: &ast::Expression) -> Option<String> {
        match e {
            ast::Expression::Identifier(id) => Some(id.name.to_string()),
            ast::Expression::ParenthesizedExpression(p) => self.layer_text(&p.expression),
            ast::Expression::ComputedMemberExpression(m) => {
                // `arr[i]` over an array of records: already a record.
                let idx = self.render(&m.expression);
                let obj = self.text(m.object.span());
                Some(match idx {
                    Some(t) => format!("{obj}[{t}]"),
                    None => self.text(m.span()),
                })
            }
            ast::Expression::StaticMemberExpression(m) => match m.property.name.as_str() {
                // Identity on the proxy, so they erase.
                "transform" | "path" => self.layer_text(&m.object),
                "parentLayer" => {
                    let o = self.layer_text(&m.object)?;
                    self.need("lyParent");
                    Some(format!("lyParent({o})"))
                }
                _ => None,
            },
            ast::Expression::CallExpression(c) => {
                if let Some(name) = comp_layer_name(c) {
                    return self.resolve_name(&name);
                }
                if let Some(idx) = self.layer_control_call(c) {
                    return Some(idx);
                }
                match &c.callee {
                    // `X.content('Path 1')` hands back the layer.
                    ast::Expression::StaticMemberExpression(m) if m.property.name == "content" => {
                        self.layer_text(&m.object)
                    }
                    // The drill-down chain. The proxy is a function returning
                    // itself, so every call in it ignores its argument and the
                    // whole chain collapses to its root. That is the runtime's
                    // approximation of AE's content drilling, not AE's own
                    // semantics — reproducing the real thing would move pixels.
                    other => self.layer_text(other),
                }
            }
            _ => None,
        }
    }

    /// Fold `var t = [0, 1, 2]` into the layers those layer-control effects
    /// name, so `effect(t[i])(p)` can collapse to `t[i]`.
    ///
    /// Each element is resolved against the *owning* layer's effect table, so
    /// like everything else here it is resolved once per using property and
    /// only written if they agree. An element that resolves to no layer at all
    /// takes the whole body to the fallback rather than being written as `null`
    /// or `0` — see [`Handle`] for why neither reproduces what the proxy did.
    fn render_control_table(&mut self, span: (u32, u32)) -> Option<String> {
        let (name, table) = self.tables.iter().find(|(_, t)| t.span == span)?;
        let (name, param, elems) = (name.clone(), table.param, table.elems.clone());
        let mut out = Vec::with_capacity(elems.len());
        for k in elems {
            let per: Option<Vec<(Site, Target)>> = self
                .uses
                .iter()
                .map(|s| Some((*s, self.control_target(*s, k, param)?)))
                .collect();
            match per.as_deref().and_then(agree) {
                Some(h) => {
                    self.need(h.helper());
                    out.push(h.emit());
                }
                None => {
                    self.refuse(&format!("layer control {name}[{k}] will not fold"));
                    return None;
                }
            }
        }
        Some(format!("[{}]", out.join(", ")))
    }

    /// The layer one `effect(k)(param)` on `site`'s owner names, or `None` when
    /// the whole body has to fall back.
    ///
    /// The distinction that matters: an *absent* effect is a resolved answer,
    /// even though nothing can be written for it — the proxy handed back
    /// `() => 0` and the body computed with the number 0, and knowing that is
    /// what lets [`agree`] tell "all uses found nothing" from "the uses
    /// disagree". A parameter that exists but is not a layer control, or one
    /// that is animated, is not an answer at all.
    fn control_target(&self, site: Site, k: u32, param: u32) -> Option<Target> {
        let recs = match site.table {
            Table::Doc => &self.data.layers,
            Table::Asset(i) => &self.data.assets[i as usize].records,
        };
        let rec = recs.get(site.owner as usize)?;
        let Some(ep) = rec
            .ef
            .get(k as usize)
            .and_then(|e| e.ef.get(param as usize))
        else {
            return Some(Target::NoEffect);
        };
        if ep.ty != 10 {
            return None;
        }
        let v = ep.v?;
        Some(
            match self.index.by_index(site.table, site.scope, v as u32) {
                Some(r) => Target::Rec(r),
                None => Target::NoLayer,
            },
        )
    }

    /// `thisComp.layer('name')`, resolved once per using property.
    fn resolve_name(&mut self, name: &str) -> Option<String> {
        let per: Vec<(Site, Target)> = self
            .uses
            .iter()
            .map(|s| {
                let t = match self.index.by_name(s.table, s.scope, name) {
                    Some(r) => Target::Rec(r),
                    None => Target::NoLayer,
                };
                (*s, t)
            })
            .collect();
        match agree(&per) {
            Some(h) => {
                self.need(h.helper());
                Some(h.emit())
            }
            None => {
                self.refuse(&format!("uses disagree about which layer {name:?} is"));
                None
            }
        }
    }

    /// One argument, rendered.
    fn arg_text(&mut self, a: &ast::Argument) -> Option<String> {
        let e = a.as_expression()?;
        Some(self.render(e).unwrap_or_else(|| self.text(e.span())))
    }

    fn render_children(&mut self, e: &ast::Expression) -> Option<String> {
        let mut cuts = Vec::new();
        each_sub_expr(e, &mut |x| {
            if let Some(t) = self.render(x) {
                cuts.push((x.span(), t));
            }
        });
        self.splice(e.span(), cuts)
    }

    fn render_children_of_member(&mut self, m: &ast::StaticMemberExpression) -> Option<String> {
        let inner = self.render(&m.object)?;
        self.splice(m.span(), vec![(m.object.span(), inner)])
    }

    fn render_children_of_call(&mut self, c: &ast::CallExpression) -> Option<String> {
        let mut cuts = Vec::new();
        if let Some(t) = self.render(&c.callee) {
            cuts.push((c.callee.span(), t));
        }
        for a in &c.arguments {
            if let Some(x) = a.as_expression()
                && let Some(t) = self.render(x)
            {
                cuts.push((x.span(), t));
            }
        }
        self.splice(c.span(), cuts)
    }

    /// Rebuild `span`'s source with its immediate children replaced. Those are
    /// pairwise disjoint, so back-to-front is enough here.
    fn splice(
        &self,
        span: oxc_span::Span,
        mut cuts: Vec<(oxc_span::Span, String)>,
    ) -> Option<String> {
        if cuts.is_empty() {
            return None;
        }
        cuts.sort_by_key(|(s, _)| std::cmp::Reverse(s.start));
        let base = span.start as usize;
        let mut out = self.text(span);
        for (s, t) in cuts {
            out.replace_range(s.start as usize - base..s.end as usize - base, &t);
        }
        Some(out)
    }
}

// ---------------------------------------------------------------------------
// AST traversal
//
// Statement and expression walks shared by the inference and rendering passes,
// so the two cannot see different parts of a body.
// ---------------------------------------------------------------------------

/// Every expression in an expression position directly under `s`.
fn each_stmt_expr(s: &ast::Statement, f: &mut impl FnMut(&ast::Expression)) {
    match s {
        ast::Statement::ExpressionStatement(x) => f(&x.expression),
        ast::Statement::VariableDeclaration(d) => {
            for decl in &d.declarations {
                if let Some(e) = &decl.init {
                    f(e);
                }
            }
        }
        ast::Statement::IfStatement(x) => f(&x.test),
        ast::Statement::WhileStatement(x) => f(&x.test),
        ast::Statement::ReturnStatement(x) => {
            if let Some(e) = &x.argument {
                f(e);
            }
        }
        ast::Statement::ForStatement(x) => {
            if let Some(ast::ForStatementInit::VariableDeclaration(d)) = &x.init {
                for decl in &d.declarations {
                    if let Some(e) = &decl.init {
                        f(e);
                    }
                }
            }
            for e in [&x.test, &x.update].into_iter().flatten() {
                f(e);
            }
        }
        _ => {}
    }
}

/// Every statement directly under `s`.
fn each_sub_stmt(s: &ast::Statement, f: &mut impl FnMut(&ast::Statement)) {
    match s {
        ast::Statement::BlockStatement(b) => {
            for x in &b.body {
                f(x);
            }
        }
        ast::Statement::IfStatement(x) => {
            f(&x.consequent);
            if let Some(a) = &x.alternate {
                f(a);
            }
        }
        ast::Statement::ForStatement(x) => f(&x.body),
        ast::Statement::WhileStatement(x) => f(&x.body),
        ast::Statement::TryStatement(x) => {
            for st in &x.block.body {
                f(st);
            }
            if let Some(h) = &x.handler {
                for st in &h.body.body {
                    f(st);
                }
            }
            if let Some(fin) = &x.finalizer {
                for st in &fin.body {
                    f(st);
                }
            }
        }
        _ => {}
    }
}

/// Every expression directly under `e`.
fn each_sub_expr(e: &ast::Expression, f: &mut impl FnMut(&ast::Expression)) {
    match e {
        ast::Expression::CallExpression(c) => {
            f(&c.callee);
            for a in &c.arguments {
                if let Some(x) = a.as_expression() {
                    f(x);
                }
            }
        }
        ast::Expression::StaticMemberExpression(m) => f(&m.object),
        ast::Expression::ComputedMemberExpression(m) => {
            f(&m.object);
            f(&m.expression);
        }
        ast::Expression::ParenthesizedExpression(p) => f(&p.expression),
        ast::Expression::SequenceExpression(s) => {
            for x in &s.expressions {
                f(x);
            }
        }
        ast::Expression::AssignmentExpression(a) => f(&a.right),
        ast::Expression::BinaryExpression(b) => {
            f(&b.left);
            f(&b.right);
        }
        ast::Expression::LogicalExpression(l) => {
            f(&l.left);
            f(&l.right);
        }
        ast::Expression::ConditionalExpression(c) => {
            f(&c.test);
            f(&c.consequent);
            f(&c.alternate);
        }
        ast::Expression::UnaryExpression(u) => f(&u.argument),
        ast::Expression::ArrayExpression(a) => {
            for el in &a.elements {
                if let Some(x) = el.as_expression() {
                    f(x);
                }
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// The verifier
// ---------------------------------------------------------------------------

/// Layer members that must not survive a rewrite.
///
/// Deliberately excludes `points`, `inTangents`, `outTangents`, `isClosed` and
/// `index`: `thisProperty`, a `createPath` result and a keyframe object all
/// legitimately carry those, and `nearestKey(time).index` is a keyframe's.
const LAYER_MEMBERS: &[&str] = &[
    "position",
    "anchorPoint",
    "scale",
    "rotation",
    "opacity",
    "transform",
    "parentLayer",
    "getLocalTransform",
    "toComp",
    "fromCompToSurface",
    "pointOnPath",
    "tangentOnPath",
    "content",
    "path",
    "effect",
    "name",
];

/// Free names that only the fallback preamble binds.
///
/// `effect` and `thisComp` used to be bound in front of every body; they are
/// emitted only under [`Plan::legacy`] now, because a resolved body has both
/// spelled out at the call site. So a mention of either that the walk did not
/// rewrite is a `ReferenceError` on every frame, which `evalExpr`'s catch turns
/// into a silent fall back to the base value — fail-open, which is the one
/// thing this pass is not allowed to be. Catching them here sends the body to
/// the fallback, where both are bound, and costs bytes instead.
///
/// `.effect` is in [`LAYER_MEMBERS`] as well: that catches `X.effect(…)` on a
/// layer the walk left alone, this catches the bare call.
const FALLBACK_ONLY: &[&str] = &["effect", "thisComp"];

/// Whether `body[i..]` starts a whole identifier rather than sitting inside a
/// longer word or after a `.`.
///
/// Bytes rather than chars: `match_indices` hands back byte offsets, and the
/// two only agree on ASCII. A byte in the middle of a multi-byte character is
/// none of the classes below, so the worst a non-ASCII neighbour can do is make
/// this answer `true` — one more body on the fallback, never one fewer.
fn whole_word(body: &str, i: usize, len: usize) -> bool {
    let src = body.as_bytes();
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$';
    if i > 0 && (ident(src[i - 1]) || src[i - 1] == b'.') {
        return false;
    }
    !src.get(i + len).copied().is_some_and(ident)
}

/// Whether a finished body still reaches a layer, and so has to keep the
/// fallback surface.
///
/// This has the last word over the walk above, and it is a text scan rather
/// than a second analysis on purpose: the failure it guards against is the walk
/// believing it rewrote something it did not, and a scan that shares no code
/// with the walk cannot share its blind spot.
pub fn verify(body: &str) -> bool {
    for w in FALLBACK_ONLY {
        if body
            .match_indices(w)
            .any(|(i, _)| whole_word(body, i, w.len()))
        {
            return true;
        }
    }
    let src = body.as_bytes();
    for m in LAYER_MEMBERS {
        for (i, _) in body.match_indices(&format!(".{m}")) {
            // Only a whole member name, so `.pathData` is not `.path`.
            let after = body[i + 1 + m.len()..].chars().next();
            if after.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '$') {
                continue;
            }
            // `..` cannot happen and `?.` reads the same; what this skips is a
            // decimal point, which no member access follows.
            if i > 0 && src[i - 1].is_ascii_digit() {
                continue;
            }
            return true;
        }
    }
    false
}

#[cfg(test)]
mod verify_tests {
    use super::*;

    /// Run the pass over a hand-written body, against a real planned scene so
    /// the index and the sites are the ones that ship.
    fn rewrite_against(fixture: &str, body: &str) -> Option<Plan> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../_fixtures/animations")
            .join(format!("{fixture}.json"));
        let json = std::fs::read_to_string(path).unwrap();
        let animation: crate::lottie::Animation = serde_json::from_str(&json).unwrap();
        let module = crate::ir::lower(&animation).unwrap();
        let payload = crate::data::encode(&module).unwrap();
        let bodies: Vec<_> = module.expressions.iter().cloned().collect();
        let scene = crate::scene::plan_with(&payload, true, 24576, false, &bodies).unwrap();
        let index = Index::build(&scene.data);
        // Whatever the first body's properties are; all that matters here is
        // that the sites are real and agree.
        let uses: Vec<Site> = sites(&scene.data)[&0].iter().map(|u| u.unwrap()).collect();
        rewrite(body, &uses, &index, &scene.data, 0)
    }

    #[test]
    fn a_layer_the_walk_cannot_type_is_refused() {
        // Two lookups behind a condition: the value is a layer, but which one
        // is not decidable, so no literal is. The pass must refuse rather than
        // pick — and the refusal has to survive being noticed late.
        let p = rewrite_against(
            "lights",
            "var l = c ? thisComp.layer('wire') : thisComp.layer('orange');\n$bm_rt = l.position;",
        );
        assert!(p.is_none());
    }

    #[test]
    fn the_property_surface_does_not_trip_the_verifier() {
        // `points`/`isClosed` on `thisProperty` are not layer members, and a
        // verifier that treated them as such would push every path-rewriting
        // body onto a refusal for nothing.
        assert!(!verify(
            "$bm_rt = createPath(thisProperty.points(), 0, 0, thisProperty.isClosed());"
        ));
        assert!(!verify("$bm_rt = nearestKey(time).index;"));
        // A decimal point is not a member access.
        assert!(!verify("$bm_rt = 1.5;"));
    }

    #[test]
    fn a_surviving_layer_member_is_caught_whatever_the_walk_believed() {
        // The verifier's whole job: it shares no code with the walk, so it
        // cannot share the walk's blind spot.
        assert!(verify("$bm_rt = x.position;"));
        assert!(verify("$bm_rt = x.toComp(p);"));
        assert!(verify("$bm_rt = thisComp.layer('wire');"));
        // Whole member names only, so a longer name that starts the same is not
        // a hit.
        assert!(!verify("$bm_rt = x.pathData;"));
        assert!(!verify("$bm_rt = x.contentType;"));
    }

    #[test]
    fn a_binding_only_the_fallback_supplies_is_caught() {
        // Neither name is bound in front of a resolved body any more, so a
        // mention the walk did not rewrite would throw once per frame and be
        // swallowed into a silent fallback to the base value. These are the
        // shapes `render_effect` declines — two arguments to the outer call,
        // and a curried reference held in a variable.
        assert!(verify("$bm_rt = effect('x')(0, 1);"));
        assert!(verify("var e = effect('x'); $bm_rt = e(0);"));
        assert!(verify("$bm_rt = thisComp.width;"));
        assert!(verify("$bm_rt = thisComp.layer('wire');"));
        // What the rewrite emits in their place is not a hit.
        assert!(!verify("$bm_rt = lyEffect(thisLayer, 0, 0, frame);"));
        assert!(!verify("$bm_rt = div(frameDuration, 10);"));
        // Nor is a longer word that merely contains one.
        assert!(!verify("$bm_rt = effects_total;"));
        assert!(!verify("$bm_rt = myEffect;"));
    }

    #[test]
    fn a_layer_control_read_outside_the_table_form_is_refused() {
        // `wire`'s effects are `ADBE Layer Control`s: parameter 0 of each is
        // type 10 and names another layer. `render_control_table` folds the
        // shape After Effects actually generates (`effect(t[i])(0)` over a
        // literal array); this is the same read written any other way, and the
        // free `lyEffect` is value-only — it would hand back the raw comp index
        // where the proxy handed back a layer, so `.index` on the result would
        // read `undefined` instead of a number. Nothing catches that
        // downstream: `index` is not a layer member the verifier knows.
        let p = rewrite_against(
            "lights",
            "$bm_rt = thisComp.layer('wire').effect(0)(0).index;",
        );
        assert!(p.is_none());
        // The parameter next to it is an ordinary value and still folds.
        let ok = rewrite_against("lights", "$bm_rt = effect(0)(0);");
        assert!(ok.is_some());
    }

    #[test]
    fn an_effect_selector_that_is_not_a_literal_is_refused() {
        // Without a literal there is no way to know whether the parameter is a
        // layer control, so there is no way to know the free function computes
        // what the proxy did.
        let p = rewrite_against("lights", "var k = 0;\n$bm_rt = effect(k)(0);");
        assert!(p.is_none());
    }

    #[test]
    fn a_body_the_walk_leaves_a_bare_effect_in_is_refused() {
        // End to end, not just the scan: `render_effect` matches only the
        // 1-arg/1-arg shape, so this one is untouched by the walk and the
        // verifier is the only thing standing between it and a body that
        // throws every frame. Refusing fails the compile, which is loud.
        let p = rewrite_against("lights", "$bm_rt = effect('x')(0, 1);");
        assert!(p.is_none());
    }
}
