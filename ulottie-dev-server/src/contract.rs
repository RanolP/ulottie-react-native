//! The `/compile` contract.
//!
//! This is the single definition of what the demo receives. `build.rs` runs
//! `rkyv-js-codegen` over this file and emits `demo/src/generated/bindings.ts`,
//! so the TypeScript types and their decoder are derived from these structs
//! rather than written twice and kept in step by hand.
//!
//! Field names are camelCased on the wire by the generator, matching what the
//! page already reads.
//!
//! Keep this module free of types the generator cannot see through — it parses
//! Rust source, so every field type must be a primitive, a container, or
//! another type declared here.

use rkyv::{Archive, Serialize};

/// Raw and compressed bytes of one artifact.
#[derive(Archive, Serialize)]
pub struct SizeEntry {
    pub raw: u32,
    pub gzipped: u32,
}

/// Which optional runtime features the embedded build inlined, and what each
/// costs. Mirrors `ulottie_compiler::EmbeddedFeatures` plus the measured cost,
/// flattened so the generator does not need to reach into another crate.
#[derive(Archive, Serialize)]
pub struct FeatureReport {
    pub expressions: bool,
    pub trim_path: bool,
    pub gradient: bool,
    /// Raw bytes each feature contributes, measured by diffing the embedded
    /// build with and without it. `i32` rather than `i64` so it arrives as a
    /// JS number instead of a bigint — these are kilobytes.
    pub expressions_cost: i32,
    pub trim_path_cost: i32,
    pub gradient_cost: i32,
}

#[derive(Archive, Serialize)]
pub struct Sizes {
    /// Lottie source JSON.
    pub json: SizeEntry,
    /// The compiled module, extern mode.
    pub js: SizeEntry,
    /// The runtime slice this animation imports — what a bundler ships for it.
    pub runtime_slice: SizeEntry,
    /// Whole runtime, every capability on. An upper bound, not a payload.
    pub ulottie_runtime: SizeEntry,
    /// Markup extracted to a sprite: the module, and the sprite it needs.
    pub js_extracted: SizeEntry,
    pub sprite: SizeEntry,
    /// Self-contained: runtime tree-shaken and inlined.
    pub js_embedded: SizeEntry,
    pub features: FeatureReport,
    /// The baseline a regular Lottie pipeline ships for the same fixture.
    pub lottie_runtime: SizeEntry,
}

/// What the AOT stage decided — the part of the report that explains the
/// numbers, as opposed to what the source contained.
#[derive(Archive, Serialize)]
pub struct Plan {
    /// Capability names the scene actually reaches.
    pub caps: Vec<String>,
    /// Runtime modules an extern build imports. Empty when fully static.
    pub modules: Vec<String>,
    /// Nothing varies over time: no runtime, no data table, no frame loop.
    pub is_static: bool,
    /// Precomps planned once and replayed, chosen by measuring both ways.
    pub instanced: bool,
    /// Repeated subtrees factored out, expanded at mount.
    pub templated: bool,
    pub elements: u32,
    pub bindings: u32,
    pub records: u32,
}

/// A feature the backend does not implement, and what it does to the picture.
#[derive(Archive, Serialize)]
pub struct Unsupported {
    pub feature: String,
    pub effect: String,
    /// Whether it was accepted anyway. The viewer accepts everything, so this
    /// is always true there; the CLI is the strict gate.
    pub allowed: bool,
}

#[derive(Archive, Serialize)]
pub struct CompileResponse {
    pub id: String,
    pub json_url: String,
    pub js_url: String,
    /// URL for the embedded (tree-shaken, self-contained) variant.
    pub js_embedded_url: String,
    pub name: Option<String>,
    pub total_frames: f64,
    pub sizes: Sizes,
    pub plan: Plan,
    /// Reported so a degraded render is never silent.
    pub unsupported: Vec<Unsupported>,
}
