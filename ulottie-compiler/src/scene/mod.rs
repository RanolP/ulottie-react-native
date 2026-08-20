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

mod bake;
mod build;
pub mod assets;
pub mod flat;
mod instance;
pub mod prop;
pub mod svg;
mod template;

use std::collections::HashMap;

use anyhow::Result;
use bitflags::bitflags;
use serde::ser::SerializeSeq;
use serde::{Serialize, Serializer};

use crate::data::Payload;
use crate::ir;

pub use instance::AssetPlan;
use prop::{Easing, LINEAR, Prop};
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
    /// Write `d` from a bezier path property. The three generated forms below
    /// are separate ops rather than a tag inside this one: which generator a
    /// shape needs is a compile-time fact, so an animation that draws only
    /// paths should neither ship the polystar generator nor branch past it two
    /// hundred times a frame.
    pub const SHAPE: u8 = 4;
    /// Native `<rect>`/`<ellipse>` attributes — not a generated outline.
    pub const RECT: u8 = 5;
    pub const ELLIPSE: u8 = 6;
    pub const FILL: u8 = 7;
    pub const STROKE: u8 = 8;
    pub const GRADIENT: u8 = 9;
    /// Layer transform/opacity read from the expression layer table, so the
    /// keyframes are stored once instead of once per consumer.
    pub const LAYER_TX: u8 = 10;
    pub const LAYER_OP: u8 = 11;
    /// Write `d` from a generated outline.
    pub const SHAPE_RECT: u8 = 12;
    pub const SHAPE_ELLIPSE: u8 = 13;
    pub const SHAPE_STAR: u8 = 14;
    /// One `<stop>` of a keyframed gradient ramp: its offset and its colour.
    pub const RAMP: u8 = 15;
    /// Write one element's `d` from several path properties concatenated —
    /// the shapes one style paints all land in that style's single element,
    /// so same-style contours share a fill rule and interact. The one
    /// argument is a list section of property offsets.
    pub const SHAPE_MULTI: u8 = 16;
    /// Write `stroke-dasharray`/`stroke-dashoffset` from an animated dash
    /// pattern. The one argument is `[count, length…, offset]`.
    pub const DASH: u8 = 17;
    /// `TRANSFORM` with a live skew — its own op so the skew factor costs
    /// nothing on the overwhelmingly skewless majority.
    pub const TRANSFORM_SKEW: u8 = 18;
    /// Animated effect parameters, each one small attribute write:
    /// a gaussian blur's `stdDeviation` (sigma × 0.3, one axis zeroed by the
    /// dimensions tag), a scaled scalar (`stdDeviation`, `flood-opacity`),
    /// and a drop shadow's polar `dx`/`dy`.
    pub const FX_BLUR: u8 = 19;
    pub const FX_STD: u8 = 20;
    pub const FX_FLOOD_O: u8 = 21;
    pub const FX_OFFSET: u8 = 22;
}

bitflags! {
    /// What the runtime must be able to do for this animation. Drives which
    /// modules the embedded bundle imports, so unused capability code is never
    /// emitted rather than emitted-and-hopefully-tree-shaken.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Caps: u64 {
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
        // What the expression *bodies* reach for. The preamble in front of each
        // body is already emitted from what that body references; these carry
        // the same analysis to the runtime, so an animation whose expressions
        // only call `loopOut` stops shipping the comp-space transforms and the
        // path sampler. Set by the backend after planning, from
        // `emit_expressions::vocabulary`.
        /// `numKeys`, `key`, `valueAtTime`, `loopOut` — the `thisProperty` surface.
        const EXPR_PROPERTY = 1 << 28;
        /// `thisComp`, `toComp`, `fromCompToSurface`.
        const EXPR_COMP     = 1 << 29;
        /// `createPath`, `pointOnPath`, `points` — the path API.
        const EXPR_PATH     = 1 << 30;

        // Back to the planner's own capabilities. The three above were given
        // the top of a `u32` when that was the whole set, so anything added
        // since goes after them rather than into the gap — where it would
        // silently alias `EXPR_PROPERTY` and turn its runtime off.
        /// A gradient whose colour ramp is keyframed, not just its handles.
        const RAMP          = 1 << 31;
        /// One element fed by several path properties (`SHAPE_MULTI`). Its
        /// own bit, not `SHAPE`'s: capability bits root the op's declarations,
        /// and sharing one would ship the multi loop with every ordinary
        /// shape animation.
        const SHAPE_MULTI  = 1 << 32;
        /// A shape under more than one live trim (a group trim nested inside
        /// a layer trim). Its own bit, not `TRIM`'s: the composing helpers
        /// would otherwise ship with every ordinarily-trimmed animation.
        const TRIM_CHAIN   = 1 << 33;
        /// An animated stroke dash pattern.
        const DASH         = 1 << 34;
        /// An animated transform with a live skew.
        const TRANSFORM_SKEW = 1 << 35;
        /// Animated layer-effect parameters (blur sigma, shadow offset…).
        const FX           = 1 << 36;
        /// The markup is served by the page and the module adopts it
        /// (`MarkupMode::None`) — and it carries generated ids, so the
        /// adopted tree's id marker has to be rewritten per mount. Like
        /// `EXTRACTED`, a delivery property the planner never sets.
        const SERVED       = 1 << 37;
    }
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

pub struct Scene {
    /// The standalone document: complete `<svg>…</svg>` markup showing the
    /// composition's first frame, so it renders with no script at all. Always
    /// fully expanded, whatever the module inlines. See [`bake`].
    pub markup: String,
    /// What the module should inline. The same tree, but *without* the
    /// first-frame bake — the runtime writes those attributes on mount, so
    /// carrying them would be dead bytes. Above the inline budget, repeated
    /// subtrees are factored into `data.tpl` and replaced by placeholders the
    /// runtime expands before indexing.
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
    /// Encode the scene into its wire form. Every mutation of the planned
    /// scene has to be followed by this, because the stream is a snapshot.
    pub fn seal(&mut self) -> Result<()> {
        let flat = flat::flatten(&self.data)?;
        self.data.stream = flat.encode();
        self.data.strings = flat.strings().to_vec();
        Ok(())
    }

    /// Drop effect and parameter names no surviving expression mentions.
    ///
    /// They are in the payload for one reason — `proxy.effect('name')` matching
    /// them at runtime — so once [`crate::expr::resolve`] has rewritten a
    /// lookup to an index, the name is dead weight. The effects themselves stay
    /// exactly where they are: the indices address them positionally.
    ///
    /// `mentioned` is asked about the finished body text rather than its
    /// syntax, so a lookup this compiler did not recognise keeps its name.
    pub fn prune_effect_names(&mut self, mentioned: &dyn Fn(&str) -> bool) -> Result<()> {
        let mut dropped = false;
        let mut cull = |slot: &mut Option<String>| {
            if let Some(n) = slot
                && !mentioned(n)
            {
                *slot = None;
                dropped = true;
            }
        };
        // Both tables, the way `prune_names` does it: an instanced precomp
        // keeps its records on the asset, and those carry the effects every
        // instantiation replays. Walking only the document's left the
        // instanced candidate shipping every name — two dead `Pseudo/ADBE …`
        // strings on `ripple`, which is the candidate that wins.
        let assets = self
            .data
            .assets
            .iter_mut()
            .flat_map(|a| a.records.iter_mut());
        for rec in self.data.layers.iter_mut().chain(assets) {
            for e in &mut rec.ef {
                cull(&mut e.nm);
                cull(&mut e.mn);
                for p in &mut e.ef {
                    cull(&mut p.nm);
                    cull(&mut p.mn);
                }
            }
        }
        if dropped {
            self.seal()?;
        }
        Ok(())
    }

    pub fn prune_names(&mut self, keep: &std::collections::BTreeSet<String>) -> Result<()> {
        let mut remap = vec![None; self.data.names.len()];
        let mut kept = Vec::new();
        for (i, name) in self.data.names.iter().enumerate() {
            if keep.contains(name) {
                remap[i] = Some(kept.len() as u32);
                kept.push(name.clone());
            }
        }
        if kept.len() == self.data.names.len() {
            return Ok(());
        }
        self.data.names = kept;
        let renumber = |r: &mut LayerRecord| {
            r.n = r.n.and_then(|i| remap.get(i as usize).copied().flatten());
        };
        self.data.layers.iter_mut().for_each(renumber);
        for a in &mut self.data.assets {
            a.records.iter_mut().for_each(renumber);
        }
        // The stream already holds the old numbering, so it has to be rebuilt.
        // Without this the pruning silently did nothing and every unreachable
        // layer name still shipped.
        self.seal()
    }

    /// True when the animation has nothing to update — the module can skip the
    /// entire player.
    pub fn is_static(&self) -> bool {
        self.data.b.is_empty() && self.data.assets.iter().all(|a| a.bindings.is_empty())
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
    pub timelines: Vec<[f64; 5]>,
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
    /// The whole scene as one VLQ base36 integer stream, and the only text
    /// that could not become an integer. Filled by [`flat::flatten`]; together
    /// they are the entire payload.
    pub stream: String,
    pub strings: Vec<String>,
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
    pub ef: Vec<Effect>,
    /// First path shape on the layer, for `pointOnPath` / `points()`.
    pub h: Option<Prop>,
    /// Stream offsets for `p, a, sc, r, o, h`, filled by [`flat::flatten`].
    /// A zero means the field was absent or equal to the runtime's default.
    pub offs: [u32; 6],
}

/// One effect, in the shape `thisLayer.effect('name')('param')` reads.
#[derive(Default)]
pub struct Effect {
    pub nm: Option<String>,
    pub mn: Option<String>,
    pub ef: Vec<EffectParam>,
}

#[derive(Default)]
pub struct EffectParam {
    pub nm: Option<String>,
    pub mn: Option<String>,
    pub ty: u32,
    pub v: Option<f64>,
    pub p: Option<Prop>,
    /// Stream offset of `p`, filled by [`flat::flatten`].
    pub p_off: u32,
}

impl Serialize for Effect {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = s.serialize_map(None)?;
        if let Some(n) = &self.nm {
            m.serialize_entry("nm", n)?;
        }
        if let Some(n) = &self.mn {
            m.serialize_entry("mn", n)?;
        }
        m.serialize_entry("ef", &self.ef)?;
        m.end()
    }
}

impl Serialize for EffectParam {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = s.serialize_map(None)?;
        if let Some(n) = &self.nm {
            m.serialize_entry("nm", n)?;
        }
        if let Some(n) = &self.mn {
            m.serialize_entry("mn", n)?;
        }
        if self.ty != 0 {
            m.serialize_entry("ty", &self.ty)?;
        }
        if let Some(v) = self.v {
            m.serialize_entry("v", &svg::Num(v))?;
        }
        if self.p_off != 0 {
            m.serialize_entry("p", &self.p_off)?;
        }
        m.end()
    }
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
        // Each field is a stream offset, and `flatten` has already dropped the
        // ones the runtime would default to the same value anyway — see
        // [`flat::RECORD_DEFAULTS`] for the defaults and why `o` is not 0.
        for (k, off) in ["p", "a", "sc", "r", "o", "h"].iter().zip(self.offs) {
            if off != 0 {
                m.serialize_entry(k, &off)?;
            }
        }
        if !self.ef.is_empty() {
            m.serialize_entry("ef", &self.ef)?;
        }
        m.end()
    }
}

impl Serialize for SceneData {
    /// The integer stream, and nothing else.
    ///
    /// It was an object for as long as there was a second entry to hold — the
    /// strings that could not become integers. Layer names, effect names and
    /// factored-out markup have each stopped being payload since, so the
    /// wrapper was one key describing a table with one row. What strings remain
    /// possible reach the runtime as a named module constant instead, the same
    /// way templates do.
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.stream.serialize(s)
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
    /// A small enumeration rather than a measurement — a gradient kind, a
    /// geometry kind. Stored as-is, where `Num` is scaled by a thousand.
    /// Getting these two confused makes `2` arrive as `2000`.
    Tag(u32),
    Num(f64),
    Str(String),
    List(Vec<Arg>),
    Null,
}

impl Serialize for Arg {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Arg::Prop(p) => p.serialize(s),
            Arg::Tag(t) => t.serialize(s),
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

/// Ops whose first argument is an index into the layer-record table, and so
/// wants the same delta treatment as the element index. Every other op reads
/// its arguments as values.
///
/// Each op knows its own argument shape, so this is asked once per batch at
/// compile time rather than decoded per binding at mount.
fn arg0_is_record(op: u8) -> bool {
    op == op::LAYER_TX || op == op::LAYER_OP
}

/// The runtime capability an op needs.
///
/// One mapping, asked by both sides: the planner sets it when it binds, and the
/// emitter uses it to decide which loops to import and retain. The three
/// generated-outline ops share `Caps::GEOM_*` with the generator each one
/// calls — an op and the outline it builds are never present separately.
pub fn caps_for_op(op: u8) -> Caps {
    match op {
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
        op::SHAPE_RECT => Caps::GEOM_RECT,
        op::SHAPE_ELLIPSE => Caps::GEOM_ELLIPSE,
        op::SHAPE_STAR => Caps::GEOM_STAR,
        op::RAMP => Caps::RAMP,
        op::DASH => Caps::DASH,
        op::TRANSFORM_SKEW => Caps::TRANSFORM_SKEW,
        op::FX_BLUR | op::FX_STD | op::FX_FLOOD_O | op::FX_OFFSET => Caps::FX,
        op::SHAPE_MULTI => Caps::SHAPE_MULTI,
        _ => Caps::empty(),
    }
}

/// The ops a binding list uses, ascending and deduplicated.
///
/// This is the whole contract between the encoder and the emitter. `flat.rs`
/// writes one struct-of-arrays batch per entry, in this order; `emit.rs` writes
/// one call per entry, in this order, naming the op function directly. Neither
/// side needs the op code on the wire, and nothing looks a binder up at mount.
pub fn program_ops(list: &[Binding]) -> Vec<u8> {
    let mut ops: Vec<u8> = list.iter().map(|b| b.op).collect();
    ops.sort_unstable();
    ops.dedup();
    ops
}

/// Two capabilities sharing a bit is invisible: the flag still sets, still
/// reads back true, and still names itself — it just also turns something else
/// on, or off. `RAMP` was first written as `1 << 28`, which is `EXPR_PROPERTY`;
/// the caps line kept reading as valid and `lights` quietly stopped importing
/// the half of the expression runtime that draws it.
#[cfg(test)]
#[test]
fn every_capability_has_a_bit_to_itself() {
    // Over the *declared* flags, not `all().iter()`: that walks the set bits of
    // the union, so two names on one bit yield a single entry and the check
    // passes. This is exactly the bug, so the list has to be the source one.
    use bitflags::Flags;
    let mut seen: Vec<(&str, u64)> = Vec::new();
    for flag in Caps::FLAGS {
        let bit = flag.value().bits();
        assert_eq!(
            bit.count_ones(),
            1,
            "{} is not a single bit — a composite belongs in a const fn, not the flag set",
            flag.name()
        );
        if let Some((other, _)) = seen.iter().find(|(_, b)| b & bit != 0) {
            panic!(
                "{} and {other} are both bit {}",
                flag.name(),
                bit.trailing_zeros()
            );
        }
        seen.push((flag.name(), bit));
    }
}

#[cfg(test)]
#[test]
fn a_program_names_each_op_once_in_a_fixed_order() {
    // The order is the only thing keeping the emitted calls lined up with the
    // encoded batches. Sorting by op code makes it a property of the op set
    // rather than of the order the planner happened to bind things in.
    let b = |op| Binding {
        op,
        el: 0,
        el_index: 0,
        args: Vec::new(),
    };
    let list = [
        b(op::FILL),
        b(op::TRANSFORM),
        b(op::FILL),
        b(op::OPACITY),
        b(op::TRANSFORM),
    ];
    assert_eq!(
        program_ops(&list),
        vec![op::TRANSFORM, op::OPACITY, op::FILL]
    );
    assert!(program_ops(&[]).is_empty());
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
///
/// The root keeps itself out of the page by being positioned out of flow at
/// zero size — **never `display:none`**. A `<symbol>` draws nothing on its
/// own either way, but the paint servers it carries (`ripple`'s gradient
/// strokes) are referenced by id from wherever the symbol is *instanced*,
/// and Chromium does not paint a gradient whose definition sits inside a
/// `display:none` subtree: a page that inlines the sprite and draws
/// `<use href="#id">` — the script-free first frame — then gets every plain
/// fill and none of the gradient strokes. That was `ripple`'s "dots but no
/// wires" (see HANDOFF, *Extracted markup*); masks and clip paths happen to
/// survive it, which is why no other fixture showed anything. Inline style,
/// not `width="0"` attributes, so a page's own `svg { width: 100% }` cannot
/// undo it.
pub fn sprite(symbols: &[String]) -> String {
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" style=\"position:absolute;width:0;height:0\">{}</svg>",
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
        if !part
            .trim_start()
            .starts_with(&format!("<symbol id=\"{id}\""))
        {
            kept.push(part.to_string());
        }
    }
    kept.push(symbol.to_string());
    sprite(&kept)
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

pub fn plan(payload: &Payload, keep_z: bool, bodies: &[ir::Expression]) -> Result<Scene> {
    plan_with(payload, keep_z, DEFAULT_INLINE_LIMIT, false, bodies)
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
    payload.a.as_ref().is_some_and(|a| {
        a.values()
            .any(|x| matches!(x, crate::data::Asset::Precomp { .. }))
    })
}

pub fn plan_with(
    payload: &Payload,
    keep_z: bool,
    inline_limit: usize,
    instance_precomps: bool,
    bodies: &[ir::Expression],
) -> Result<Scene> {
    let mut p = Planner {
        payload,
        keep_z,
        bodies,
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
        el_index: HashMap::new(),
        templates: Vec::new(),
        defs: Vec::new(),
        caps: Caps::empty(),
        uses_ids: false,
        uses_clone_ids: false,
        defs_local: None,
        id_seq: 0,
        expr_cache: std::cell::RefCell::new(HashMap::new()),
        expr_depth: std::cell::Cell::new(0),
    };

    let roots = p.build_layer_forest(&payload.l, build::TimeCtx::Root)?;

    // The composition clips to its own frame, and the root `<svg>` cannot do
    // it. `overflow:hidden` there clips to the *viewport* — the box the module
    // was mounted in — while `viewBox` and `preserveAspectRatio` map the
    // composition into part of that box. Letterbox a 667×508 animation into a
    // square and the bands above and below are inside the viewport and outside
    // the composition, so a layer overhanging the frame draws in them:
    // `lf20_qbvfmu8h` renders 555 composition units tall instead of 508.
    //
    // lottie-web wraps its whole tree in `<g clip-path="…">` over a `w × h`
    // rect. A nested `<svg>` is the same rectangle with no id to mint, and it
    // has to be one wrapper above everything rather than an attribute per
    // layer, because the frame is in composition space — outside every layer's
    // own transform.
    let frame = p.el("svg");
    p.set(frame, "width", svg::n(payload.c.w));
    p.set(frame, "height", svg::n(payload.c.h));
    p.els[frame].children = roots;

    // Definitions (masks, gradients) live at the end of the root: SVG resolves
    // `url(#…)` references regardless of document order, and putting them last
    // keeps the visible tree's element indices stable and small.
    let mut root_children = vec![frame];
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
    p.expand_uses();
    let assets = std::mem::take(&mut p.assets);
    let uses = std::mem::take(&mut p.uses);

    let caps = p.caps;
    let uses_ids = p.uses_ids;
    let uses_clone_ids = p.uses_clone_ids;
    let easings = if p.easings.len() > 1 {
        p.easings.clone()
    } else {
        Vec::new()
    };
    let timelines = p.timelines.clone();
    let slots = p.slots.clone();
    let gates = p.gates.clone();
    let bind_gate = p.bind_gate.clone();
    let layers = std::mem::take(&mut p.layers);
    let mut remaps = std::mem::take(&mut p.remaps);
    remaps.resize(timelines.len(), None);
    let scopes = std::mem::take(&mut p.scopes);
    // The two are one table split in half, and `plan_asset` drains both. A
    // mismatch here means a record is about to answer with a neighbour's scope,
    // which resolves `thisComp.layer()` to a layer in the wrong composition.
    debug_assert_eq!(
        scopes.len(),
        layers.len(),
        "scopes must stay parallel to layers"
    );
    debug_assert!(
        assets.iter().all(|a| a.scopes.len() == a.records.len()),
        "an asset's scopes must stay parallel to its records"
    );
    let names = std::mem::take(&mut p.names);
    let bindings = p.bindings;

    let mut scene = Scene {
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
            stream: String::new(),
            strings: Vec::new(),
            b: bindings,
        },
        caps,
    };

    // Everything time-varying moves into one integer stream, and the structures
    // above keep only offsets into it. This is the last thing planning does, so
    // the planner itself never has to think in offsets.
    scene.seal()?;
    Ok(scene)
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
    timelines: Vec<[f64; 5]>,
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
    /// Expression bodies, indexed by the `Prop::Expr` id — what the bake's
    /// frame-aware interpreter ([`bake`]) runs. Empty when there are none.
    bodies: &'a [ir::Expression],
    /// Bake-time expression evaluation: `(id, owner, frame) → value`, so a
    /// body many bindings share is interpreted once. Also the cycle breaker
    /// under the depth cap.
    expr_cache: std::cell::RefCell<ExprCache>,
    expr_depth: std::cell::Cell<u32>,
}

/// Bake-time expression evaluation: `(id, owner, frame) → value`.
type ExprCache = HashMap<(u32, Option<u32>, u64), Option<prop::Prop>>;

/// Containers whose children describe a region rather than draw one. A `<path>`
/// in here is a contour of a shape, not a mark on the canvas.
const NON_RENDERING: &[&str] = &["defs", "clipPath", "mask", "symbol", "pattern", "marker"];

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
        self.bindings.push(Binding {
            op,
            el,
            el_index: 0,
            args,
        });
        self.slots.push(slot);
        self.bind_gate.push(self.gate);
        self.caps |= caps_for_op(op);
    }

    // -- pruning ------------------------------------------------------------

    /// Drop groups that carry nothing: no attributes, no bindings, or no
    /// children at all. A `<g>` that only ever existed to hold an identity
    /// transform costs a DOM node and a layout box for nothing.
    fn prune_all(&mut self, roots: &mut Vec<usize>) {
        let spliced = self.prune_list(std::mem::take(roots));
        self.merge_paths(&spliced);
        *roots = spliced;
    }

    /// Shapes that share a style are one path, because the fill rule applies
    /// *across* them.
    ///
    /// lottie-web builds a `<path>` per **style** and writes every shape using
    /// that style into the one element; this compiler builds one per shape. For
    /// a hole — two contours wound opposite ways — that is the entire
    /// difference: in one element the non-zero rule cancels the inner contour,
    /// in two the inner one is filled solid. Type is full of them, and
    /// `krrt-7ccaf912`'s wordmark came out as eight solid blobs with every
    /// counter filled in. The pixel gate scored that **0.03%**, because the
    /// letters are small and odiff's antialiasing pass discounts thin marks —
    /// it was found by looking at it.
    ///
    /// The test is deliberately syntactic: adjacent siblings, both plain
    /// `<path>`, every attribute equal but `d`, and neither carrying a binding
    /// (`pinned`). Under exactly those conditions the two are the same paint in
    /// the same space at adjacent z, so concatenating their `d` changes nothing
    /// a renderer can see except the winding — which is the point. It runs
    /// after pruning, so paths that a dropped identity `<g>` was separating
    /// merge too.
    ///
    /// A shape whose geometry or paint is written per frame keeps its own
    /// element. Merging those means one binding owning a *slice* of an
    /// attribute, which the wire has no room for; the case that matters is
    /// static, because a hole is drawn once.
    fn merge_paths(&mut self, roots: &[usize]) {
        for &id in roots {
            // Contours inside a clip or a mask **union**; they are separate
            // children for that reason. Concatenating them would put them under
            // one fill rule and let opposite windings cancel, which is the
            // opposite of what a two-contour mask means.
            if NON_RENDERING.contains(&self.els[id].tag) {
                continue;
            }
            let kids = std::mem::take(&mut self.els[id].children);
            self.merge_paths(&kids);
            let mut out: Vec<usize> = Vec::with_capacity(kids.len());
            for k in kids {
                match out.last().copied() {
                    Some(prev) if self.same_paint(prev, k) => {
                        let more = self.attr(k, "d").unwrap_or_default();
                        if let Some(a) = self.els[prev].attrs.iter_mut().find(|(n, _)| n == "d") {
                            a.1.push_str(&more);
                        }
                    }
                    _ => out.push(k),
                }
            }
            self.els[id].children = out;
        }
    }

    /// Whether `b` can be folded into `a`: the same `<path>` in every respect
    /// but its `d`, and neither a binding target.
    fn same_paint(&self, a: usize, b: usize) -> bool {
        let (x, y) = (&self.els[a], &self.els[b]);
        if x.tag != "path" || y.tag != "path" || x.pinned || y.pinned {
            return false;
        }
        if x.instance.is_some() || y.instance.is_some() || !x.children.is_empty() {
            return false;
        }
        fn keyed(e: &El) -> Vec<(&str, &str)> {
            let mut v: Vec<(&str, &str)> = e
                .attrs
                .iter()
                .filter(|(n, _)| n != "d")
                .map(|(n, s)| (n.as_str(), s.as_str()))
                .collect();
            v.sort_unstable();
            v
        }
        // A path with no `d` draws nothing and has nothing to contribute.
        self.attr(a, "d").is_some() && self.attr(b, "d").is_some() && keyed(x) == keyed(y)
    }

    fn attr(&self, id: usize, name: &str) -> Option<String> {
        self.els[id]
            .attrs
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
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

    /// The fully-expanded document, baked at the composition's first frame.
    ///
    /// This is the standalone form — served on its own it has to render with
    /// no script at all — so every binding is evaluated once and written as an
    /// ordinary attribute. The module's own copy comes from `emit_inline`,
    /// which stays lean because the runtime writes those attributes on mount.
    /// See [`bake`] for why the two forms must not simply share.
    fn emit(&mut self, roots: &[usize], payload: &Payload) -> String {
        let overlay = self.initial_frame();
        let mut index = HashMap::new();
        let mut counter = 0u32;
        let mut buf = String::with_capacity(4096);
        buf.push_str(&format!(
            "<svg viewBox=\"0 0 {} {}\" width=\"100%\" height=\"100%\" \
             preserveAspectRatio=\"xMidYMid meet\" style=\"overflow:hidden\">",
            payload.c.w, payload.c.h
        ));
        for r in roots {
            self.emit_el(*r, &mut buf, &mut counter, &mut index, &overlay);
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
        overlay: &bake::Overlay,
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
        for (k, v) in Self::merged(&e.attrs, overlay.get(&id)) {
            buf.push(' ');
            buf.push_str(&k);
            buf.push_str("=\"");
            buf.push_str(&v);
            buf.push('"');
        }
        if e.children.is_empty() {
            buf.push_str("/>");
            return;
        }
        buf.push('>');
        for c in &e.children {
            self.emit_el(*c, buf, counter, index, overlay);
        }
        buf.push_str("</");
        buf.push_str(e.tag);
        buf.push('>');
    }

    /// Base attributes with the initial-frame overlay applied. An overlay entry
    /// replaces a same-named attribute and is appended otherwise, so an element
    /// ends up carrying exactly one value per name.
    fn merged(
        base: &[(String, String)],
        extra: Option<&Vec<(String, String)>>,
    ) -> Vec<(String, String)> {
        let mut out = base.to_vec();
        let Some(extra) = extra else { return out };
        for (k, v) in extra {
            match out.iter_mut().find(|(n, _)| n == k) {
                // `style` is a list rather than a single value: a baked
                // `display:none` has to join what the element already carries.
                Some(slot) if k == "style" => {
                    if !slot.1.split(';').any(|part| part.trim() == v) {
                        slot.1.push(';');
                        slot.1.push_str(v);
                    }
                }
                Some(slot) => slot.1 = v.clone(),
                None => out.push((k.clone(), v.clone())),
            }
        }
        out
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
            self.expand_one(n.asset, el_base, n.parent_slot, n.offset, n.rate);
        }
    }

    fn expand_one(&mut self, asset: u32, el_base: u32, parent_slot: u32, offset: f64, rate: f64) {
        // An unstretched use folds its start time into each first-level row;
        // a stretched one needs a clock of its own, because
        // `(parent − offset) / rate` does not distribute over the rows'
        // subtractions.
        let instance_slot = if rate != 1.0 {
            self.timelines
                .push([parent_slot as f64, offset, rate, -1e6, 1e6]);
            self.timelines.len() as u32
        } else {
            parent_slot
        };
        let fold = if rate != 1.0 { 0.0 } else { offset };
        let slot_base = self.timelines.len() as u32;

        let specs = self.assets[asset as usize].timelines.clone();
        for spec in &specs {
            // A local parent of 0 is this instance's own clock.
            let parent = if spec[0] == 0.0 {
                instance_slot as f64
            } else {
                slot_base as f64 + spec[0]
            };
            self.timelines
                .push([parent, spec[1] + fold, spec[2], spec[3], spec[4]]);
        }

        self.uses.push(instance::Use {
            asset,
            el_base,
            slot_base,
            parent_slot,
        });

        let nested = std::mem::take(&mut self.assets[asset as usize].nested);
        for n in &nested {
            let inner_parent = if n.parent_slot == 0 {
                parent_slot
            } else {
                slot_base + n.parent_slot
            };
            self.expand_one(n.asset, el_base + n.el_base, inner_parent, n.offset, n.rate);
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
        let body = s
            .split_once('>')
            .unwrap()
            .1
            .strip_suffix("</symbol>")
            .unwrap();
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
            !out.chars()
                .any(|c| c.is_control() && c != '\t' && c != '\n' && c != '\r'),
            "sprite carries a character XML 1.0 forbids"
        );
        for mark in [svg::ID_MARK, svg::CLONE_MARK] {
            assert!(
                mark.chars().all(|c| c.is_ascii_graphic()),
                "{mark:?} is not XML-safe"
            );
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
        assert!(
            two.contains("id=\"a\""),
            "merging into a formatted sprite lost `a`"
        );
        assert_eq!(two.matches("<symbol ").count(), 2);
    }

    #[test]
    fn a_sprite_holds_several_symbols_and_does_not_render() {
        let out = sprite(&[symbol(DOC, "a"), symbol(DOC, "b")]);
        assert!(out.starts_with(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" style=\"position:absolute;width:0;height:0\">"
        ));
        // Out of flow, not `display:none`: the paint servers inside would not
        // be painted through `<use>` otherwise (see `sprite`).
        assert!(!out.contains("display:none"));
        assert!(out.contains("id=\"a\"") && out.contains("id=\"b\""));
        assert!(out.ends_with("</svg>"));
    }
}
