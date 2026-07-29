//! Precomp instancing.
//!
//! A precomp used forty-six times used to be walked forty-six times: the same
//! subtree, the same bindings, and — once expressions are involved — the same
//! layer records, written out once per instance. On `ripple` that was 56 KB of
//! layer table and 12 KB of bindings for two distinct layers.
//!
//! So an asset is planned **once**, into element, binding, record and timeline
//! lists whose indices are local to it. An instance is then four numbers: where
//! its elements start in the expanded document, where its records start, where
//! its clocks start, and which composition scope it is. The runtime replays the
//! asset's binding list per instance with those offsets applied.
//!
//! Local indices are what makes this work, and they are why `Prop::Expr` stores
//! a layer index that is only meaningful relative to an instance.

use super::{Binding, LayerRecord};

/// One precomp, planned once.
pub struct AssetPlan {
    /// Root node of the asset's subtree in the element arena.
    pub root: usize,
    /// Elements in the expanded subtree, including the root.
    pub el_count: u32,
    /// Fully-expanded markup for the subtree.
    pub markup: String,
    /// Index into the scene's template table, so an instance can be a
    /// placeholder the runtime clones.
    pub template: u32,
    /// Bindings with element and record indices local to this asset.
    pub bindings: Vec<Binding>,
    /// Timeline slot each binding runs on, local to this asset.
    pub slots: Vec<u32>,
    /// Layer records with parent indices local to this asset.
    pub records: Vec<LayerRecord>,
    /// Composition scope per record, parallel to `records`.
    ///
    /// Compile-time only: an instantiation's records all share the scope the
    /// planner allocated for that *use*, so the runtime never reads this. It
    /// exists so the layer resolver can answer `thisComp.layer('x')` inside an
    /// asset the same way it does in the document.
    pub scopes: Vec<u32>,
    /// `[parentSlot, offset, loopIp, loopOp]`, with `parentSlot` 0 meaning the
    /// instance's own clock and `n` meaning this asset's local slot `n - 1`.
    pub timelines: Vec<[f64; 4]>,
    /// Precomps used *inside* this one, with offsets local to it. Both real
    /// fixtures nest — ripple's outer comp holds 23 uses of the inner one — so
    /// expansion has to recurse.
    pub nested: Vec<Nested>,
}

/// A use of one precomp inside another, positioned relative to the enclosing
/// asset. Absolute positions fall out of composing these down the tree.
pub struct Nested {
    pub asset: u32,
    pub node: usize,
    /// Where the instance's elements start within the enclosing asset.
    pub el_base: u32,
    /// Local slot of the precomp layer this instance hangs off, 0 meaning the
    /// enclosing instance's own clock.
    pub parent_slot: u32,
    /// The precomp layer's start time.
    pub offset: f64,
}

/// One instantiation in the finished scene: an asset plus where its elements,
/// records and clocks live in the expanded document.
pub struct Use {
    pub asset: u32,
    pub el_base: u32,
    pub rec_base: u32,
    pub slot_base: u32,
    pub parent_slot: u32,
    pub scope: u32,
}


