//! Scene planner — the AOT stage.
//!
//! Takes the fully-resolved [`data::Payload`] (which stays the analysis IR and
//! the frame evaluator's input) and splits the animation in two:
//!
//! * everything that cannot change over time is **evaluated here** and written
//!   into a single SVG markup string — geometry as `d`, transforms as a folded
//!   `matrix()`, paints as literal attributes;
//! * everything that can change becomes a compact entry in a flat binding
//!   table, addressed by the element's document-order index.
//!
//! The runtime then parses the markup once and turns each binding into one
//! closure. A frame is a straight loop over those closures — no tree walk, no
//! variant dispatch, and for a fully static animation, no loop at all.

mod build;
mod instance;
pub mod prop;
mod template;
pub mod svg;

use std::collections::HashMap;

use anyhow::Result;
use bitflags::bitflags;
use serde::ser::SerializeSeq;
use serde::{Serialize, Serializer};

use crate::data::Payload;

use instance::AssetPlan;
use prop::{Easing, Prop, LINEAR};
use svg::ID_MARK;

// ---------------------------------------------------------------------------
// Binder op codes — kept in sync with `runtime/ops/*.js`.
// ---------------------------------------------------------------------------

pub mod op {
    pub const TRANSFORM: u8 = 0;
    /// Specialization of `TRANSFORM` for the overwhelmingly common case where
    /// only the position animates: the rotation/scale/anchor part of the
    /// matrix is a compile-time constant string prefix.
    pub const TRANSLATE: u8 = 1;
    pub const OPACITY: u8 = 2;
    pub const DISPLAY: u8 = 3;
    pub const SHAPE: u8 = 4;
    pub const RECT: u8 = 5;
    pub const ELLIPSE: u8 = 6;
    pub const FILL: u8 = 7;
    pub const STROKE: u8 = 8;
    pub const GRADIENT: u8 = 9;
    /// Layer transform/opacity read from the expression layer table, so the
    /// keyframes are stored once instead of once per consumer.
    pub const LAYER_TX: u8 = 10;
    pub const LAYER_OP: u8 = 11;
}

/// Geometry descriptor tags used inside a `SHAPE` binding.
pub mod geo {
    pub const PATH: u8 = 0;
    pub const RECT: u8 = 1;
    pub const ELLIPSE: u8 = 2;
    pub const POLYSTAR: u8 = 3;
}

bitflags! {
    /// What the runtime must be able to do for this animation. Drives which
    /// modules the embedded bundle imports, so unused capability code is never
    /// emitted rather than emitted-and-hopefully-tree-shaken.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Caps: u32 {
        const TRANSFORM   = 1 << 0;
        const TRANSLATE   = 1 << 1;
        const OPACITY     = 1 << 2;
        const DISPLAY     = 1 << 3;
        const SHAPE       = 1 << 4;
        const RECT        = 1 << 5;
        const ELLIPSE     = 1 << 6;
        const FILL        = 1 << 7;
        const STROKE      = 1 << 8;
        const GRADIENT    = 1 << 9;
        /// Any keyframed property at all (pulls in the interpolator).
        const KEYFRAMES   = 1 << 10;
        /// Non-linear easing (pulls in the cubic-bezier solver).
        const EASING      = 1 << 11;
        /// Spatial motion paths (pulls in the arc-length sampler).
        const SPATIAL     = 1 << 12;
        /// Keyframed bezier paths (pulls in path interpolation).
        const PATH_KF     = 1 << 13;
        /// Held ("step") keyframes.
        const HOLD        = 1 << 14;
        const GEOM_RECT   = 1 << 15;
        const GEOM_ELLIPSE= 1 << 16;
        const GEOM_STAR   = 1 << 17;
        /// Path serialization at runtime (any dynamic geometry).
        const PATH_D      = 1 << 18;
        const TRIM        = 1 << 19;
        const TIMELINE    = 1 << 20;
        const EXPRESSIONS = 1 << 21;
        const LAYER_TX    = 1 << 22;
        const LAYER_OP    = 1 << 23;
        /// Repeated subtrees were factored out and need expanding at mount.
        const TEMPLATES   = 1 << 24;
        /// Precomps are planned once and replayed per use.
        const INSTANCES   = 1 << 25;
        /// The initial markup lives in an external sprite, not in the module.
        const EXTRACTED   = 1 << 26;
        /// A precomp's clock is driven by a time-remap property.
        const TIME_REMAP  = 1 << 27;
    }
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

pub struct Scene {
    /// The document template: complete `<svg>…</svg>` markup with every static
    /// value baked in. Always fully expanded, whatever the module inlines.
    pub markup: String,
    /// What the module should inline. Equal to `markup` under the inline
    /// budget; above it, repeated subtrees are factored into `data.tpl` and
    /// replaced by placeholders the runtime expands before indexing.
    pub inline: String,
    pub data: SceneData,
    pub caps: Caps,
}

impl Scene {
    /// Drop interned layer names outside `keep`, and renumber what is left.
    ///
    /// The name table exists only so `thisComp.layer('…')` can resolve, and the
    /// planner has no view of the expressions, so it interns every layer's
    /// name. Once the bodies are known most of them turn out to be unreachable.
    pub fn prune_names(&mut self, keep: &std::collections::BTreeSet<String>) {
        let mut remap = vec![None; self.data.names.len()];
        let mut kept = Vec::new();
        for (i, name) in self.data.names.iter().enumerate() {
            if keep.contains(name) {
                remap[i] = Some(kept.len() as u32);
                kept.push(name.clone());
            }
        }
        if kept.len() == self.data.names.len() {
            return;
        }
        self.data.names = kept;
        let renumber = |r: &mut LayerRecord| {
            r.n = r.n.and_then(|i| remap.get(i as usize).copied().flatten());
        };
        self.data.layers.iter_mut().for_each(renumber);
        for a in &mut self.data.assets {
            a.records.iter_mut().for_each(renumber);
        }
    }

    /// True when the animation has nothing to update — the module can skip the
    /// entire player.
    pub fn is_static(&self) -> bool {
        self.data.b.is_empty()
            && self.data.assets.iter().all(|a| a.bindings.is_empty())
    }
}

#[derive(Default)]
pub struct SceneData {
    pub fr: f64,
    pub ip: f64,
    pub op: f64,
    /// Markup contains `id`s that need per-mount uniquing.
    pub uses_ids: bool,
    /// Precomp bodies define `id`s that need per-clone uniquing.
    pub uses_clone_ids: bool,
    pub easings: Vec<Easing>,
    /// `[parentSlot, offset, loopIp, loopOp]`; slot 0 is the root clock.
    pub timelines: Vec<[f64; 4]>,
    /// Timeline slot per binding; all-zero arrays are dropped on the wire.
    pub slots: Vec<u32>,
    /// `[ip, op)` visibility windows. A binding inside a layer that is off at
    /// the current frame is skipped entirely, the way lottie-web skips hidden
    /// layers — without this, a scene of staggered layers pays for all of them
    /// on every frame.
    pub gates: Vec<[f64; 2]>,
    /// 1-based gate index per binding; 0 means always evaluated.
    pub bind_gate: Vec<u32>,
    /// Markup for repeated subtrees and precomp bodies, referenced from
    /// placeholders in the inlined markup.
    pub tpl: Vec<String>,
    /// Precomps, planned once each.
    pub assets: Vec<AssetPlan>,
    /// Every instantiation, with absolute positions.
    pub uses: Vec<instance::Use>,
    /// Layers as expressions can observe them. Only populated when the module
    /// has expressions — `thisLayer`, `thisComp.layer()` and `effect()` read
    /// nothing else.
    pub layers: Vec<LayerRecord>,
    /// Time-remap property per clock slot, parallel to `timelines`. A slot
    /// with one takes its time from the property instead of from
    /// `parent - offset`, and neither the offset nor the loop applies.
    pub remaps: Vec<Option<Prop>>,
    /// Composition scope per layer record, parallel to `layers`.
    ///
    /// `thisComp.layer()` — by name *or* by index — resolves within one
    /// composition, so a document that inlines two precomps holding a
    /// `Shape Layer 1`, or simply two layers both at index 1, needs to tell
    /// them apart. After Effects auto-names layers per comp, so this is the
    /// common case rather than an exotic one.
    ///
    /// It lives here rather than on the record because it is the one field
    /// that differs between otherwise-identical inlined copies of a precomp:
    /// in the record it makes all 46 of `ripple`'s copies unique and costs
    /// ~650 gzipped bytes, and as a separate delta-encoded column it costs
    /// almost nothing while carrying the same information.
    pub scopes: Vec<u32>,
    /// Names referenced by `thisComp.layer('…')`.
    pub names: Vec<String>,
    pub b: Vec<Binding>,
}

/// One layer, as the expression runtime sees it. Field names match what the
/// proxy reads so there is no translation layer between wire and runtime.
#[derive(Default)]
pub struct LayerRecord {
    /// Composition index (Lottie `ind`).
    pub i: u32,
    /// Index into `SceneData::names`.
    pub n: Option<u32>,
    /// Parent layer, as an index into the layer table.
    pub pr: Option<u32>,
    pub p: Option<Prop>,
    pub a: Option<Prop>,
    pub sc: Option<Prop>,
    pub r: Option<Prop>,
    pub o: Option<Prop>,
    pub ef: Option<serde_json::Value>,
    /// First path shape on the layer, for `pointOnPath` / `points()`.
    pub h: Option<Prop>,
}

impl Serialize for LayerRecord {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = s.serialize_map(None)?;
        m.serialize_entry("i", &self.i)?;
        if let Some(n) = self.n {
            m.serialize_entry("n", &n)?;
        }
        if let Some(pr) = self.pr {
            m.serialize_entry("pr", &pr)?;
        }
        // A field the runtime would default to the same value anyway is not
        // worth a wire entry. The defaults below are the ones every read site
        // supplies — ops/layer.js:14-17,33, expr.js:251-255 and
        // `getLocalTransform` at expr.js:260-266 — and they must stay in step:
        // `o` defaults to **100**, not 0, so eliding an explicit `o: 0` would
        // turn a hidden layer fully opaque.
        for (k, v, default_to) in [
            ("p", &self.p, Some(&[0.0, 0.0, 0.0][..])),
            ("a", &self.a, Some(&[0.0, 0.0, 0.0][..])),
            ("sc", &self.sc, Some(&[100.0, 100.0, 100.0][..])),
            ("r", &self.r, Some(&[0.0][..])),
            ("o", &self.o, Some(&[100.0][..])),
            ("h", &self.h, None),
        ] {
            if let Some(v) = v {
                if default_to.is_some_and(|d| v.is_exactly(d)) {
                    continue;
                }
                m.serialize_entry(k, v)?;
            }
        }
        if let Some(ef) = &self.ef {
            m.serialize_entry("ef", ef)?;
        }
        m.end()
    }
}

impl Serialize for SceneData {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let any_slot = self.slots.iter().any(|s| *s != 0);
        let any_gate = self.bind_gate.iter().any(|g| *g != 0);
        let mut n = 2; // fr, op
        for present in [
            self.ip != 0.0,
            self.uses_ids,
            self.uses_clone_ids,
            !self.easings.is_empty(),
            !self.timelines.is_empty(),
            any_slot,
            any_gate,
            any_gate,
            !self.tpl.is_empty(),
            !self.assets.is_empty(),
            !self.assets.is_empty(),
            !self.layers.is_empty(),
            !self.names.is_empty(),
            !self.b.is_empty(),
        ] {
            if present {
                n += 1;
            }
        }
        let mut m = s.serialize_map(Some(n))?;
        m.serialize_entry("f", &svg::Num(self.fr))?;
        if self.ip != 0.0 {
            m.serialize_entry("i", &svg::Num(self.ip))?;
        }
        m.serialize_entry("o", &svg::Num(self.op))?;
        if self.uses_ids {
            m.serialize_entry("u", &1u8)?;
        }
        if self.uses_clone_ids {
            m.serialize_entry("c", &1u8)?;
        }
        if !self.easings.is_empty() {
            m.serialize_entry("z", &Quads(&self.easings))?;
        }
        if !self.timelines.is_empty() {
            m.serialize_entry("t", &Quads(&self.timelines))?;
        }
        if self.remaps.iter().any(|r| r.is_some()) {
            m.serialize_entry("rm", &Sparse(&self.remaps))?;
        }
        if any_slot {
            m.serialize_entry("l", &Prefix(&self.slots))?;
        }
        if any_gate {
            m.serialize_entry("k", &Pairs(&self.gates))?;
            m.serialize_entry("g", &self.bind_gate)?;
        }
        if !self.tpl.is_empty() {
            m.serialize_entry("m", &self.tpl)?;
        }
        if !self.assets.is_empty() {
            m.serialize_entry("q", &self.assets)?;
            m.serialize_entry("n", &self.uses)?;
        }
        if !self.layers.is_empty() {
            m.serialize_entry("y", &self.layers)?;
        }
        if self.scopes.iter().any(|g| *g != 0) {
            m.serialize_entry("gy", &Prefix(&self.scopes))?;
        }
        if !self.names.is_empty() {
            m.serialize_entry("s", &self.names)?;
        }
        if !self.b.is_empty() {
            m.serialize_entry("b", &Deltas(&self.b))?;
        }
        m.end()
    }
}

/// `[[a,b], …]` with integral entries written as integers.
struct Pairs<'a>(&'a [[f64; 2]]);

impl Serialize for Pairs<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut outer = s.serialize_seq(Some(self.0.len()))?;
        for row in self.0 {
            outer.serialize_element(&[svg::Num(row[0]), svg::Num(row[1])])?;
        }
        outer.end()
    }
}

/// `[[a,b,c,d], …]` with integral entries written as integers.
pub(crate) struct Quads<'a>(&'a [[f64; 4]]);

impl Serialize for Quads<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut outer = s.serialize_seq(Some(self.0.len()))?;
        for row in self.0 {
            outer.serialize_element(&[
                svg::Num(row[0]),
                svg::Num(row[1]),
                svg::Num(row[2]),
                svg::Num(row[3]),
            ])?;
        }
        outer.end()
    }
}

/// One dynamic binding, serialized as `[op, elementIndex, …args]`.
pub struct Binding {
    pub op: u8,
    /// Arena id while planning; rewritten to a document-order index on emit.
    pub el: usize,
    pub el_index: u32,
    pub args: Vec<Arg>,
}

pub enum Arg {
    Prop(Prop),
    Num(f64),
    Str(String),
    List(Vec<Arg>),
    Null,
}

impl Serialize for Arg {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Arg::Prop(p) => p.serialize(s),
            Arg::Num(n) => svg::Num(*n).serialize(s),
            Arg::Str(v) => s.serialize_str(v),
            Arg::List(items) => {
                let mut seq = s.serialize_seq(Some(items.len()))?;
                for i in items {
                    seq.serialize_element(i)?;
                }
                seq.end()
            }
            Arg::Null => s.serialize_unit(),
        }
    }
}

impl Serialize for Binding {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        Row { b: self, el: self.el_index as i64, rec: None }.serialize(s)
    }
}

/// One binding row with its index columns already reduced to differences.
///
/// The element index and the layer-record index are both absolute positions
/// into ascending-ish sequences, so consecutive rows differ by a small number
/// where the absolute values are large and all distinct. Storing the difference
/// is what lets gzip collapse them — on `ripple`, whose 46 precomp copies bind
/// the same five patterns at ever-increasing indices, it is the difference
/// between 46 unique rows and 46 identical ones.
struct Row<'a> {
    b: &'a Binding,
    el: i64,
    /// Present only for the ops whose first argument is a record index.
    rec: Option<i64>,
}

impl Serialize for Row<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(2 + self.b.args.len()))?;
        seq.serialize_element(&self.b.op)?;
        seq.serialize_element(&self.el)?;
        for (i, a) in self.b.args.iter().enumerate() {
            match (i, self.rec) {
                (0, Some(d)) => seq.serialize_element(&d)?,
                _ => seq.serialize_element(a)?,
            }
        }
        seq.end()
    }
}

/// Ops whose first argument is an index into the layer-record table, and so
/// wants the same delta treatment as the element index. Every other binder
/// reads its arguments as values.
///
/// `mount` spells this `b[0] > 9` to save bytes, which is only correct while
/// these stay the highest op codes — [`record_ops_are_the_highest`] pins it.
fn arg0_is_record(op: u8) -> bool {
    op == op::LAYER_TX || op == op::LAYER_OP
}

#[cfg(test)]
#[test]
fn record_ops_are_the_highest() {
    // core.js decodes the record-index column for `b[0] > 9`. If a new op took
    // a code above these, the runtime would start accumulating a value that is
    // not an index and every layer binding after it would read the wrong
    // record — silently, and only in animations that use expressions.
    let all = [
        op::TRANSFORM, op::TRANSLATE, op::OPACITY, op::DISPLAY,
        op::SHAPE, op::RECT, op::ELLIPSE, op::FILL, op::STROKE,
        op::GRADIENT, op::LAYER_TX, op::LAYER_OP,
    ];
    for o in all {
        assert_eq!(
            arg0_is_record(o),
            o > 9,
            "op {o} disagrees with the `b[0] > 9` test in runtime/core.js"
        );
    }
}

/// A binding list with its two index columns delta-encoded.
///
/// Accumulators restart per list, so an asset's bindings decode against their
/// own base rather than the document's — which is what keeps one planned asset
/// replayable at any offset.
pub(crate) struct Deltas<'a>(pub &'a [Binding]);

impl Serialize for Deltas<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut outer = s.serialize_seq(Some(self.0.len()))?;
        let (mut el, mut rec) = (0i64, 0i64);
        for b in self.0 {
            let e = b.el_index as i64;
            let d = if arg0_is_record(b.op) {
                match b.args.first() {
                    Some(Arg::Num(n)) => {
                        let v = *n as i64;
                        let d = v - rec;
                        rec = v;
                        Some(d)
                    }
                    _ => None,
                }
            } else {
                None
            };
            outer.serialize_element(&Row { b, el: e - el, rec: d })?;
            el = e;
        }
        outer.end()
    }
}

/// A mostly-empty column: absent entries ride as `0`, which is shorter than
/// `null` and is what the runtime tests for.
pub(crate) struct Sparse<'a>(pub &'a [Option<Prop>]);

impl Serialize for Sparse<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut q = s.serialize_seq(Some(self.0.len()))?;
        for v in self.0 {
            match v {
                Some(p) => q.serialize_element(p)?,
                None => q.serialize_element(&0u8)?,
            }
        }
        q.end()
    }
}

/// An ascending integer column stored as first differences.
pub(crate) struct Prefix<'a>(pub &'a [u32]);

impl Serialize for Prefix<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut outer = s.serialize_seq(Some(self.0.len()))?;
        let mut prev = 0i64;
        for v in self.0 {
            outer.serialize_element(&(*v as i64 - prev))?;
            prev = *v as i64;
        }
        outer.end()
    }
}

/// Emit the document with precomp instances left as placeholders the runtime
/// expands. Used for the inlined markup; the standalone document is always
/// fully expanded.
pub(crate) fn placeholder(template: u32) -> String {
    format!("<g data-t=\"{template}\"/>")
}

/// Wrap a document template as a sprite `<symbol>`, so several animations can
/// share one file that a page inlines or preloads.
///
/// The symbol keeps the document's `viewBox`; the presentation attributes
/// (`width`, `height`, `preserveAspectRatio`, `overflow`) belong to the
/// `<svg>` the runtime builds around a clone, not to the stored geometry.
pub fn symbol(markup: &str, id: &str) -> String {
    let view_box = markup
        .split_once("viewBox=\"")
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(v, _)| v)
        .unwrap_or("");
    let body = markup
        .find('>')
        .map(|i| &markup[i + 1..])
        .unwrap_or(markup)
        .strip_suffix("</svg>")
        .unwrap_or("");
    format!("<symbol id=\"{id}\" viewBox=\"{view_box}\">{body}</symbol>")
}

/// The document's outer `<svg>` with no children — what an extracted-mode
/// module carries in place of the markup.
///
/// The presentation attributes stay with the module because they describe how
/// *this* mount is laid out, and they are ~100 bytes against a document that
/// is usually kilobytes. The runtime fills the shell from the sprite.
pub fn shell(markup: &str) -> String {
    let open = markup.find('>').map(|i| &markup[..=i]).unwrap_or(markup);
    format!("{open}</svg>")
}

/// A standalone sprite file holding one or more symbols.
pub fn sprite(symbols: &[String]) -> String {
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" style=\"display:none\">{}</svg>",
        symbols.concat()
    )
}

/// Add a symbol to an existing sprite, replacing any symbol already using that
/// id.
///
/// This is what lets several animations share one file without the compiler
/// needing to know about all of them at once: compiling each in turn into the
/// same sprite accumulates them, and recompiling one replaces its symbol
/// instead of duplicating it.
pub fn merge_sprite(existing: &str, symbol: &str, id: &str) -> String {
    // Trim first: a pretty-printed sprite ends with a newline, and failing to
    // find the closing tag would silently drop every symbol already in the
    // file rather than erroring.
    let existing = existing.trim();
    let body = existing
        .find('>')
        .map(|i| &existing[i + 1..])
        .unwrap_or("")
        .strip_suffix("</svg>")
        .unwrap_or("");
    let mut kept: Vec<String> = Vec::new();
    for part in body.split_inclusive("</symbol>") {
        if part.trim().is_empty() {
            continue;
        }
        // Tolerate leading whitespace: a `--pretty` sprite is indented, and it
        // still has to merge with the next animation compiled into it.
        if !part.trim_start().starts_with(&format!("<symbol id=\"{id}\"")) {
            kept.push(part.to_string());
        }
    }
    kept.push(symbol.to_string());
    sprite(&kept)
}

/// Substitute the per-instance id marker. The runtime does this with a unique
/// suffix per mount; a standalone document uses a fixed one.
pub fn resolve_ids(markup: &str, instance: u32) -> String {
    markup.replace(svg::ID_MARK, &format!("-{instance}"))
}

// ---------------------------------------------------------------------------
// Element arena
// ---------------------------------------------------------------------------

pub(crate) struct El {
    tag: &'static str,
    attrs: Vec<(String, String)>,
    children: Vec<usize>,
    /// Element is a binding target and must survive pruning.
    pinned: bool,
    /// This node stands in for a precomp instance; the document expands to the
    /// asset's subtree here.
    pub(crate) instance: Option<u32>,
}

// ---------------------------------------------------------------------------
// Planner
// ---------------------------------------------------------------------------

pub fn plan(payload: &Payload, keep_z: bool) -> Result<Scene> {
    plan_with(payload, keep_z, DEFAULT_INLINE_LIMIT, false)
}

/// Above this many bytes of markup, repeated subtrees are factored out rather
/// than inlined. Chosen so ordinary animations keep a literal, directly
/// SSR-able document and only heavily-instanced ones pay for expansion code.
pub const DEFAULT_INLINE_LIMIT: usize = 24 * 1024;

/// Whether this animation has any precomp asset at all.
///
/// Only these can differ between the inlined and instanced builds, so
/// `Instancing::Auto` skips its second compile for everything else — which is
/// most of the corpus.
pub fn has_reusable_precomps(payload: &Payload) -> bool {
    payload
        .a
        .as_ref()
        .is_some_and(|a| a.values().any(|x| matches!(x, crate::data::Asset::Precomp { .. })))
}

pub fn plan_with(
    payload: &Payload,
    keep_z: bool,
    inline_limit: usize,
    instance_precomps: bool,
) -> Result<Scene> {
    let mut p = Planner {
        payload,
        keep_z,
        els: Vec::new(),
        bindings: Vec::new(),
        slots: Vec::new(),
        gates: Vec::new(),
        bind_gate: Vec::new(),
        gate: 0,
        inline_limit,
        instance_precomps,
        layers: Vec::new(),
        remaps: Vec::new(),
        scopes: Vec::new(),
        names: Vec::new(),
        name_index: HashMap::new(),
        layer_rec: None,
        scope: 0,
        scope_seq: 0,
        has_exprs: keep_z,
        easings: vec![LINEAR],
        easing_index: HashMap::new(),
        timelines: Vec::new(),
        assets: Vec::new(),
        asset_index: HashMap::new(),
        uninstanceable: Default::default(),
        pending: Vec::new(),
        uses: Vec::new(),
        rec_high: 0,
        el_index: HashMap::new(),
        templates: Vec::new(),
        defs: Vec::new(),
        caps: Caps::empty(),
        uses_ids: false,
        uses_clone_ids: false,
        defs_local: None,
        id_seq: 0,
    };

    let roots = p.build_layer_forest(&payload.l, build::TimeCtx::Root)?;

    // Definitions (masks, gradients) live at the end of the root: SVG resolves
    // `url(#…)` references regardless of document order, and putting them last
    // keeps the visible tree's element indices stable and small.
    let mut root_children = roots;
    if !p.defs.is_empty() {
        let defs = std::mem::take(&mut p.defs);
        let node = p.el("defs");
        p.els[node].children = defs;
        root_children.push(node);
    }

    p.prune_all(&mut root_children);

    // The fully-expanded document, for `Scene::markup`.
    let markup = p.emit(&root_children, payload);
    // What the module inlines: precomp bodies are always placeholders, and the
    // repeated-subtree pass may factor out more on top of that.
    let with_instances = p.emit_inline(&root_children, payload);
    let (inline, mut templates) = p.templated(&with_instances, &root_children, payload);
    let mut tpl = std::mem::take(&mut p.templates);
    tpl.append(&mut templates);
    if !tpl.is_empty() {
        p.caps |= Caps::TEMPLATES;
    }
    let templates = tpl;
    // Records for the document itself come first; instantiations follow.
    p.rec_high = p.layers.len() as u32;
    p.expand_uses();
    let assets = std::mem::take(&mut p.assets);
    let uses = std::mem::take(&mut p.uses);

    let caps = p.caps;
    let uses_ids = p.uses_ids;
    let uses_clone_ids = p.uses_clone_ids;
    let easings = if p.easings.len() > 1 { p.easings.clone() } else { Vec::new() };
    let timelines = p.timelines.clone();
    let slots = p.slots.clone();
    let gates = p.gates.clone();
    let bind_gate = p.bind_gate.clone();
    let layers = std::mem::take(&mut p.layers);
    let mut remaps = std::mem::take(&mut p.remaps);
    remaps.resize(timelines.len(), None);
    let scopes = std::mem::take(&mut p.scopes);
    let names = std::mem::take(&mut p.names);
    let bindings = p.bindings;

    Ok(Scene {
        markup,
        inline,
        data: SceneData {
            tpl: templates,
            assets,
            uses,
            fr: payload.c.fr,
            ip: payload.c.ip,
            op: payload.c.op,
            uses_ids,
            uses_clone_ids,
            easings,
            timelines,
            slots,
            gates,
            bind_gate,
            layers,
            remaps,
            scopes,
            names,
            b: bindings,
        },
        caps,
    })
}

pub(crate) struct Planner<'a> {
    payload: &'a Payload,
    /// Preserve the z component of 3-vectors (needed only when expressions can
    /// observe a raw property value).
    keep_z: bool,
    pub(crate) els: Vec<El>,
    bindings: Vec<Binding>,
    slots: Vec<u32>,
    gates: Vec<[f64; 2]>,
    bind_gate: Vec<u32>,
    /// Gate covering the subtree currently being walked (1-based, 0 = none).
    gate: u32,
    /// Byte budget for inlining the document template. Above it the markup is
    /// templated instead — see `Scene::inline`.
    inline_limit: usize,
    /// Plan precomps once and replay them, rather than walking each use.
    pub(crate) instance_precomps: bool,
    /// Layer table, built only when the module has expressions.
    pub(crate) layers: Vec<LayerRecord>,
    /// Time-remap per clock slot; see `SceneData::remaps`.
    pub(crate) remaps: Vec<Option<Prop>>,
    /// Composition scope per record; see `SceneData::scopes`.
    pub(crate) scopes: Vec<u32>,
    pub(crate) names: Vec<String>,
    name_index: HashMap<String, u32>,
    /// Layer record the property currently being classified belongs to.
    pub(crate) layer_rec: Option<u32>,
    /// Composition scope currently being walked.
    pub(crate) scope: u32,
    scope_seq: u32,
    /// Whether this module has any expressions at all.
    pub(crate) has_exprs: bool,
    easings: Vec<Easing>,
    easing_index: HashMap<[u64; 4], u32>,
    timelines: Vec<[f64; 4]>,
    /// Precomps, planned once each.
    pub(crate) assets: Vec<AssetPlan>,
    pub(crate) asset_index: HashMap<String, u32>,
    /// Precomps that define their own masks or gradients, so cloning them would
    /// duplicate a document-scoped id. Walked inline instead.
    pub(crate) uninstanceable: std::collections::HashSet<String>,
    /// Uses recorded while walking, positioned once planning finishes.
    pub(crate) pending: Vec<instance::Nested>,
    /// The finished, fully-expanded list of instantiations.
    pub(crate) uses: Vec<instance::Use>,
    /// Next free layer-record slot, once the document's own records are placed.
    pub(crate) rec_high: u32,
    /// Element index of every node in the inlined document.
    pub(crate) el_index: HashMap<usize, u32>,
    /// Template markup, shared by precomp instances and by the repeated-subtree
    /// pass.
    pub(crate) templates: Vec<String>,
    defs: Vec<usize>,
    caps: Caps,
    uses_ids: bool,
    uses_clone_ids: bool,
    /// While planning a precomp, its definitions collect here so they can live
    /// inside the cloned body instead of the document's shared `<defs>`.
    defs_local: Option<Vec<usize>>,
    id_seq: usize,
}

impl<'a> Planner<'a> {
    // -- arena --------------------------------------------------------------

    /// Record a `<defs>` entry, in the precomp body when planning one.
    pub(crate) fn add_def(&mut self, node: usize) {
        match &mut self.defs_local {
            Some(v) => v.push(node),
            None => self.defs.push(node),
        }
    }

    fn el(&mut self, tag: &'static str) -> usize {
        self.els.push(El {
            tag,
            attrs: Vec::new(),
            children: Vec::new(),
            pinned: false,
            instance: None,
        });
        self.els.len() - 1
    }

    fn set(&mut self, id: usize, name: &str, value: impl Into<String>) {
        self.els[id].attrs.push((name.to_string(), value.into()));
    }

    fn bind(&mut self, op: u8, el: usize, args: Vec<Arg>, slot: u32) {
        self.els[el].pinned = true;
        self.bindings.push(Binding { op, el, el_index: 0, args });
        self.slots.push(slot);
        self.bind_gate.push(self.gate);
        self.caps |= match op {
            op::TRANSFORM => Caps::TRANSFORM,
            op::TRANSLATE => Caps::TRANSLATE,
            op::OPACITY => Caps::OPACITY,
            op::DISPLAY => Caps::DISPLAY,
            op::SHAPE => Caps::SHAPE,
            op::RECT => Caps::RECT,
            op::ELLIPSE => Caps::ELLIPSE,
            op::FILL => Caps::FILL,
            op::STROKE => Caps::STROKE,
            op::GRADIENT => Caps::GRADIENT,
            op::LAYER_TX => Caps::LAYER_TX,
            op::LAYER_OP => Caps::LAYER_OP,
            _ => Caps::empty(),
        };
    }

    // -- pruning ------------------------------------------------------------

    /// Drop groups that carry nothing: no attributes, no bindings, or no
    /// children at all. A `<g>` that only ever existed to hold an identity
    /// transform costs a DOM node and a layout box for nothing.
    fn prune_all(&mut self, roots: &mut Vec<usize>) {
        let spliced = self.prune_list(std::mem::take(roots));
        *roots = spliced;
    }

    fn prune_list(&mut self, list: Vec<usize>) -> Vec<usize> {
        let mut out = Vec::with_capacity(list.len());
        for id in list {
            let children = std::mem::take(&mut self.els[id].children);
            let children = self.prune_list(children);
            self.els[id].children = children;
            let e = &self.els[id];
            // A precomp instance carries no arena children — its content is the
            // asset's subtree — so it must not be mistaken for an empty group.
            let placeholder = e.instance.is_some();
            let transparent = e.tag == "g" && e.attrs.is_empty() && !e.pinned && !placeholder;
            let hollow = e.tag == "g" && e.children.is_empty() && !e.pinned && !placeholder;
            if hollow {
                continue;
            }
            if transparent {
                let kids = std::mem::take(&mut self.els[id].children);
                out.extend(kids);
            } else {
                out.push(id);
            }
        }
        out
    }

    // -- emission -----------------------------------------------------------

    fn emit(&mut self, roots: &[usize], payload: &Payload) -> String {
        let mut index = HashMap::new();
        let mut counter = 0u32;
        let mut buf = String::with_capacity(4096);
        buf.push_str(&format!(
            "<svg viewBox=\"0 0 {} {}\" width=\"100%\" height=\"100%\" \
             preserveAspectRatio=\"xMidYMid meet\" style=\"overflow:hidden\">",
            payload.c.w, payload.c.h
        ));
        for r in roots {
            self.emit_el(*r, &mut buf, &mut counter, &mut index);
        }
        buf.push_str("</svg>");
        for b in &mut self.bindings {
            b.el_index = *index.get(&b.el).expect("binding target survived pruning");
        }
        buf
    }

    /// Same walk, but a precomp instance is left as a placeholder for the
    /// runtime to expand. Records each instance's element base, which is what
    /// its asset's local binding indices are relative to.
    fn emit_inline(&mut self, roots: &[usize], payload: &Payload) -> String {
        let mut counter = 0u32;
        let mut buf = String::with_capacity(4096);
        buf.push_str(&format!(
            "<svg viewBox=\"0 0 {} {}\" width=\"100%\" height=\"100%\" \
             preserveAspectRatio=\"xMidYMid meet\" style=\"overflow:hidden\">",
            payload.c.w, payload.c.h
        ));
        for r in roots {
            self.emit_inline_el(*r, &mut buf, &mut counter);
        }
        buf.push_str("</svg>");
        buf
    }

    fn emit_inline_el(&mut self, id: usize, buf: &mut String, counter: &mut u32) {
        self.el_index.insert(id, *counter);
        if let Some(asset) = self.els[id].instance {
            *counter += self.assets[asset as usize].el_count;
            buf.push_str(&placeholder(self.assets[asset as usize].template));
            return;
        }
        *counter += 1;
        let (tag, attrs, children) = {
            let e = &self.els[id];
            (e.tag, e.attrs.clone(), e.children.clone())
        };
        buf.push('<');
        buf.push_str(tag);
        for (k, v) in &attrs {
            buf.push(' ');
            buf.push_str(k);
            buf.push_str("=\"");
            buf.push_str(v);
            buf.push('"');
        }
        if children.is_empty() {
            buf.push_str("/>");
            return;
        }
        buf.push('>');
        for c in children {
            self.emit_inline_el(c, buf, counter);
        }
        buf.push_str("</");
        buf.push_str(tag);
        buf.push('>');
    }

    fn emit_el(
        &self,
        id: usize,
        buf: &mut String,
        counter: &mut u32,
        index: &mut HashMap<usize, u32>,
    ) {
        let e = &self.els[id];
        index.insert(id, *counter);
        // A precomp instance expands to its asset's subtree, so it occupies
        // that whole span of the document's element order. The asset's own
        // bindings address elements relative to this base.
        if let Some(asset) = e.instance {
            let a = &self.assets[asset as usize];
            buf.push_str(&a.markup);
            *counter += a.el_count;
            return;
        }
        *counter += 1;
        buf.push('<');
        buf.push_str(e.tag);
        for (k, v) in &e.attrs {
            buf.push(' ');
            buf.push_str(k);
            buf.push_str("=\"");
            buf.push_str(v);
            buf.push('"');
        }
        if e.children.is_empty() {
            buf.push_str("/>");
            return;
        }
        buf.push('>');
        for c in &e.children {
            self.emit_el(*c, buf, counter, index);
        }
        buf.push_str("</");
        buf.push_str(e.tag);
        buf.push('>');
    }

    /// Intern a layer name for `thisComp.layer('…')`.
    pub(crate) fn intern_name(&mut self, name: &str) -> u32 {
        if let Some(&i) = self.name_index.get(name) {
            return i;
        }
        let i = self.names.len() as u32;
        self.names.push(name.to_string());
        self.name_index.insert(name.to_string(), i);
        i
    }

    /// Allocate a composition scope. Each precomp instance gets its own, so
    /// `thisComp.layer()` inside it resolves against its own layers.
    pub(crate) fn next_scope(&mut self) -> u32 {
        self.scope_seq += 1;
        self.scope_seq
    }

    /// Turn the recorded uses into absolute positions.
    ///
    /// Every use contributes its asset's clocks and records to the scene, and
    /// each use nested inside it contributes again — ripple's outer comp is
    /// used twice and holds twenty-three uses of the inner one, so the
    /// instantiation list is the transitive expansion while the *bodies* stay
    /// stored once each.
    fn expand_uses(&mut self) {
        let top: Vec<instance::Nested> = std::mem::take(&mut self.pending);
        // Document-level uses sit after the document's own elements; their
        // element bases were assigned by the inline walk.
        for n in top {
            let el_base = self.el_base_of(n.node);
            self.expand_one(n.asset, el_base, n.parent_slot, n.offset);
        }
    }

    fn expand_one(&mut self, asset: u32, el_base: u32, parent_slot: u32, offset: f64) {
        let slot_base = self.timelines.len() as u32;
        let rec_base = self.rec_high;
        self.rec_high += self.assets[asset as usize].records.len() as u32;
        let scope = self.next_scope();

        let specs = self.assets[asset as usize].timelines.clone();
        for spec in &specs {
            // A local parent of 0 is this instance's own clock.
            let parent = if spec[0] == 0.0 {
                parent_slot as f64
            } else {
                slot_base as f64 + spec[0]
            };
            self.timelines.push([parent, spec[1] + offset, spec[2], spec[3]]);
        }

        self.uses.push(instance::Use {
            asset,
            el_base,
            rec_base,
            slot_base,
            parent_slot,
            scope,
        });

        let nested = std::mem::take(&mut self.assets[asset as usize].nested);
        for n in &nested {
            let inner_parent = if n.parent_slot == 0 {
                parent_slot
            } else {
                slot_base + n.parent_slot
            };
            self.expand_one(n.asset, el_base + n.el_base, inner_parent, n.offset);
        }
        self.assets[asset as usize].nested = nested;
    }

    fn el_base_of(&self, node: usize) -> u32 {
        *self.el_index.get(&node).unwrap_or(&0)
    }

    fn next_id(&mut self, prefix: &str) -> String {
        let n = self.id_seq;
        self.id_seq += 1;
        // Inside a precomp the definition is cloned with the body, so the id
        // has to differ per clone rather than per mount.
        if self.defs_local.is_some() {
            self.uses_clone_ids = true;
            format!("{prefix}{n}{}", svg::CLONE_MARK)
        } else {
            self.uses_ids = true;
            format!("{prefix}{n}{ID_MARK}")
        }
    }
}

#[cfg(test)]
mod sprite_tests {
    use super::*;

    const DOC: &str = "<svg viewBox=\"0 0 10 20\" width=\"100%\" height=\"100%\" \
                       preserveAspectRatio=\"xMidYMid meet\" style=\"overflow:hidden\">\
                       <rect x=\"1\"/><g><path d=\"M0,0\"/></g></svg>";

    #[test]
    fn a_symbol_keeps_the_geometry_and_drops_the_presentation() {
        let s = symbol(DOC, "anim");
        assert_eq!(
            s,
            "<symbol id=\"anim\" viewBox=\"0 0 10 20\">\
             <rect x=\"1\"/><g><path d=\"M0,0\"/></g></symbol>"
        );
    }

    #[test]
    fn the_symbol_body_is_exactly_the_document_children() {
        // Element order is the contract: bindings address elements by their
        // document-order index, so nothing may be added or dropped here.
        let s = symbol(DOC, "x");
        assert_eq!(s.matches('<').count(), DOC.matches('<').count());
    }

    #[test]
    fn the_shell_is_the_document_with_no_children() {
        // What an extracted module carries. It has to keep every presentation
        // attribute — those describe this mount, not the stored geometry — and
        // none of the elements, which the runtime clones in.
        assert_eq!(
            shell(DOC),
            "<svg viewBox=\"0 0 10 20\" width=\"100%\" height=\"100%\" \
             preserveAspectRatio=\"xMidYMid meet\" style=\"overflow:hidden\"></svg>"
        );
    }

    #[test]
    fn shell_and_symbol_together_reconstruct_the_document() {
        let s = symbol(DOC, "anim");
        let body = s.split_once('>').unwrap().1.strip_suffix("</symbol>").unwrap();
        let rebuilt = shell(DOC).replace("></svg>", &format!(">{body}</svg>"));
        assert_eq!(rebuilt, DOC);
    }

    #[test]
    fn a_sprite_is_valid_xml() {
        // A sprite is a real `.svg`, parsed by a strict XML parser rather than
        // the lenient HTML one that handles inlined markup. The id markers ride
        // along inside it, so they have to be legal XML characters.
        let out = sprite(&[symbol(DOC, "a")]);
        assert!(
            !out.chars().any(|c| c.is_control() && c != '\t' && c != '\n' && c != '\r'),
            "sprite carries a character XML 1.0 forbids"
        );
        for mark in [svg::ID_MARK, svg::CLONE_MARK] {
            assert!(mark.chars().all(|c| c.is_ascii_graphic()), "{mark:?} is not XML-safe");
        }
    }

    #[test]
    fn merging_replaces_a_symbol_rather_than_duplicating_it() {
        // Compiling several animations into one sprite accumulates them, and
        // recompiling one of them has to be idempotent.
        let one = sprite(&[symbol(DOC, "a")]);
        let two = merge_sprite(&one, &symbol(DOC, "b"), "b");
        assert_eq!(two.matches("<symbol ").count(), 2);

        let again = merge_sprite(&two, &symbol(DOC, "a"), "a");
        assert_eq!(again.matches("<symbol ").count(), 2);
        assert_eq!(again.matches("id=\"a\"").count(), 1);
        assert!(again.contains("id=\"b\""));
    }

    #[test]
    fn merging_survives_a_reformatted_sprite() {
        // `--pretty` reformats the file, and the next animation compiled into
        // it still has to merge. Getting this wrong drops the symbols already
        // there instead of failing.
        let one = crate::backend::pretty::markup_plain(&sprite(&[symbol(DOC, "a")]));
        let two = merge_sprite(&one, &symbol(DOC, "b"), "b");
        assert!(two.contains("id=\"a\""), "merging into a formatted sprite lost `a`");
        assert_eq!(two.matches("<symbol ").count(), 2);
    }

    #[test]
    fn a_sprite_holds_several_symbols_and_does_not_render() {
        let out = sprite(&[symbol(DOC, "a"), symbol(DOC, "b")]);
        assert!(out.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\" style=\"display:none\">"));
        assert!(out.contains("id=\"a\"") && out.contains("id=\"b\""));
        assert!(out.ends_with("</svg>"));
    }
}
