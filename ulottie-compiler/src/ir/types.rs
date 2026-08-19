//! μlottie intermediate representation.
//!
//! Higher level than JS, lower level than the Lottie AST. Parent/child layer
//! links are resolved into `LayerId`s; properties are typed (`Vec2`/`Vec3`/
//! `Color`/`PathData`/`f64`); expressions are interned into one `ExprTable`.
//!
//! Each optimization pass mutates a `Module` in place. The backend in turn
//! lowers a `Module` into a JS AST.

use bitflags::bitflags;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// IDs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LayerId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ExprId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ShapeId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AssetId(pub u32);

// ---------------------------------------------------------------------------
// Concrete value types carried by `Property<T>`
// ---------------------------------------------------------------------------

pub type Scalar = f64;
pub type Vec2 = [f64; 2];
pub type Vec3 = [f64; 3];
/// Linear RGBA in 0..=1.
pub type Color = [f64; 4];

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PathData {
    pub vertices: Vec<Vec2>,
    pub in_tangents: Vec<Vec2>,
    pub out_tangents: Vec<Vec2>,
    pub closed: bool,
}

// ---------------------------------------------------------------------------
// Property — static / animated / expression-driven
// ---------------------------------------------------------------------------

/// A Lottie property. The variants follow the AE semantic where, when an
/// expression is present, the keyframes (or static value) act as the *value
/// source* that the expression can read via `value` or `thisProperty`.
#[derive(Debug, Clone, PartialEq)]
pub enum Property<T: Clone> {
    Static(T),
    Animated(Keyframes<T>),
    Expression {
        fallback: ValueSource<T>,
        expr: ExprId,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ValueSource<T: Clone> {
    Static(T),
    Animated(Keyframes<T>),
}

impl<T: Clone> Property<T> {
    pub fn is_static(&self) -> bool {
        matches!(self, Property::Static(_))
    }
    pub fn is_animated(&self) -> bool {
        matches!(self, Property::Animated(_))
    }
    pub fn has_expression(&self) -> bool {
        matches!(self, Property::Expression { .. })
    }
    pub fn static_value(&self) -> Option<&T> {
        match self {
            Property::Static(v) => Some(v),
            _ => None,
        }
    }
    /// First keyframe's start value, if it has one. Useful for the "initial"
    /// render before any animation has run.
    pub fn initial_value(&self) -> Option<&T> {
        match self {
            Property::Static(v) => Some(v),
            Property::Animated(kf) => kf.frames.first().and_then(|f| f.value.as_ref()),
            Property::Expression { fallback, .. } => match fallback {
                ValueSource::Static(v) => Some(v),
                ValueSource::Animated(kf) => kf.frames.first().and_then(|f| f.value.as_ref()),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Keyframes<T: Clone> {
    pub frames: Vec<Keyframe<T>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Keyframe<T: Clone> {
    pub time: f64,
    pub value: Option<T>,
    pub easing_in: Option<EasingHandle>,
    pub easing_out: Option<EasingHandle>,
    pub spatial_in: Option<Vec3>,
    pub spatial_out: Option<Vec3>,
    pub hold: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EasingHandle {
    pub x: EasingValue,
    pub y: EasingValue,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EasingValue {
    Scalar(f64),
    PerComponent(Vec<f64>),
}

// ---------------------------------------------------------------------------
// Transform
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct Transform {
    pub anchor: Property<Vec3>,
    pub position: Property<Vec3>,
    pub scale: Property<Vec3>,
    pub rotation: Property<Scalar>,
    pub opacity: Property<Scalar>,
    pub skew: Option<Property<Scalar>>,
    pub skew_axis: Option<Property<Scalar>>,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            anchor: Property::Static([0.0, 0.0, 0.0]),
            position: Property::Static([0.0, 0.0, 0.0]),
            scale: Property::Static([100.0, 100.0, 100.0]),
            rotation: Property::Static(0.0),
            opacity: Property::Static(100.0),
            skew: None,
            skew_axis: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Shape tree
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum ShapeNode {
    Group {
        name: Option<String>,
        match_name: Option<String>,
        items: Vec<ShapeNode>,
        hidden: bool,
    },
    Path {
        name: Option<String>,
        ks: Property<PathData>,
        direction: ShapeDirection,
        hidden: bool,
    },
    Ellipse {
        name: Option<String>,
        size: Property<Vec2>,
        position: Property<Vec2>,
        direction: ShapeDirection,
        hidden: bool,
    },
    Rectangle {
        name: Option<String>,
        size: Property<Vec2>,
        position: Property<Vec2>,
        radius: Property<Scalar>,
        direction: ShapeDirection,
        hidden: bool,
    },
    PolyStar {
        name: Option<String>,
        kind: PolyStarKind,
        points: Property<Scalar>,
        position: Property<Vec2>,
        rotation: Property<Scalar>,
        outer_radius: Property<Scalar>,
        inner_radius: Option<Property<Scalar>>,
        outer_roundness: Option<Property<Scalar>>,
        inner_roundness: Option<Property<Scalar>>,
        direction: ShapeDirection,
        hidden: bool,
    },
    /// Group-local transform applied to siblings within a `Group`.
    Transform {
        name: Option<String>,
        transform: Transform,
        hidden: bool,
    },
    Fill {
        name: Option<String>,
        match_name: Option<String>,
        color: Property<Color>,
        opacity: Property<Scalar>,
        rule: FillRule,
        hidden: bool,
    },
    Stroke {
        name: Option<String>,
        match_name: Option<String>,
        color: Property<Color>,
        opacity: Property<Scalar>,
        width: Property<Scalar>,
        linecap: LineCap,
        linejoin: LineJoin,
        miter_limit: Option<f64>,
        /// Dash pattern in draw order; empty means solid.
        dash: Vec<DashStop>,
        hidden: bool,
    },
    GradientStroke {
        name: Option<String>,
        gradient: GradientDef,
        width: Property<Scalar>,
        opacity: Property<Scalar>,
        start: Option<Property<Vec2>>,
        end: Option<Property<Vec2>>,
        kind: GradientKind,
        linecap: LineCap,
        linejoin: LineJoin,
        miter_limit: Option<f64>,
        /// Dash pattern in draw order; empty means solid.
        dash: Vec<DashStop>,
        hidden: bool,
    },
    GradientFill {
        name: Option<String>,
        gradient: GradientDef,
        opacity: Property<Scalar>,
        start: Option<Property<Vec2>>,
        end: Option<Property<Vec2>>,
        kind: GradientKind,
        rule: FillRule,
        hidden: bool,
    },
    TrimPath {
        name: Option<String>,
        start: Property<Scalar>,
        end: Property<Scalar>,
        offset: Property<Scalar>,
        multiple_shapes: TrimMultipleShapes,
        hidden: bool,
    },
}

/// One entry of a stroke's dash pattern. Lengths keep their authored order —
/// lottie-web joins them into `stroke-dasharray` as they come — and the
/// offset feeds `stroke-dashoffset`.
#[derive(Debug, Clone, PartialEq)]
pub struct DashStop {
    pub offset: bool,
    pub value: Property<Scalar>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeDirection {
    Normal,   // 1
    Reversed, // 3
}

impl ShapeDirection {
    pub fn from_lottie(d: Option<u8>) -> Self {
        match d {
            Some(3) => ShapeDirection::Reversed,
            _ => ShapeDirection::Normal,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolyStarKind {
    Star,    // sy=1
    Polygon, // sy=2
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillRule {
    NonZero,
    EvenOdd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineCap {
    Butt,   // 1
    Round,  // 2
    Square, // 3
}

impl LineCap {
    pub fn from_lottie(v: Option<u8>) -> Self {
        match v {
            Some(2) => LineCap::Round,
            Some(3) => LineCap::Square,
            _ => LineCap::Butt,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineJoin {
    Miter, // 1
    Round, // 2
    Bevel, // 3
}

impl LineJoin {
    pub fn from_lottie(v: Option<u8>) -> Self {
        match v {
            Some(2) => LineJoin::Round,
            Some(3) => LineJoin::Bevel,
            _ => LineJoin::Miter,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GradientKind {
    Linear, // 1
    Radial, // 2
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrimMultipleShapes {
    /// All shapes share one trim.
    Simultaneously, // 1
    /// Each shape is trimmed independently.
    Individually, // 2
}

/// Stored verbatim for now; gradient parsing happens at the backend until
/// optimization passes need to peek inside.
#[derive(Debug, Clone, PartialEq)]
pub struct GradientDef {
    pub raw: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Layer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Layer {
    pub id: LayerId,
    pub name: Option<String>,
    /// 1-based composition index (matches Lottie's `ind`). Preserved so that
    /// expression layer-lookups by index still work after lowering.
    pub index: u32,
    pub parent: Option<LayerId>,
    pub kind: LayerKind,
    pub transform: Transform,
    pub effects: Vec<Effect>,
    pub in_point: f64,
    pub out_point: f64,
    /// Time-stretch (Lottie `sr`); 1.0 means no stretch.
    pub stretch: f64,
    /// Inner start time used by precomp instances to offset their child clock.
    pub start_time: f64,
    /// Time remap: the precomp's inner time in seconds as a function of the
    /// outer time. Replaces the usual `outer - start_time` clock entirely.
    pub time_remap: Option<Property<Scalar>>,
    pub is_3d: bool,
    pub auto_orient: bool,
    pub hidden: bool,
    pub blend_mode: u8,
    pub track_matte: Option<u8>,
    /// The layer that mattes this one. `None` means the one before it, which
    /// is what Lottie assumed before `tp` existed.
    pub matte_parent: Option<LayerId>,
    pub matte_layer_for_above: bool,
    pub has_mask: bool,
    /// Per-layer SVG masks. When non-empty the layer's outerG gets a
    /// `mask="url(#...)"` attribute; the driver builds a `<mask>` element
    /// out of each mask's animated path.
    pub masks: Vec<LayerMask>,
}

#[derive(Debug, Clone)]
pub struct LayerMask {
    pub mode: MaskMode,
    pub inverted: bool,
    /// Animated or static bezier path describing the mask shape.
    pub shape: Property<PathData>,
    /// Mask opacity (0..100). When static and 100, omitted on the wire.
    pub opacity: Option<Property<Scalar>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaskMode {
    /// "a" — only area inside the mask is visible.
    Add,
    /// "s" — area inside the mask is hidden.
    Subtract,
    /// "i" — everything so far, seen through this path.
    Intersect,
    /// "n" — the path is carried but draws nothing (lottie-web parks it in
    /// defs).
    None,
    /// "d", "f", "l" — modes lottie-web's untested branch paints white, so
    /// they render as Add there and here.
    Other,
}

#[derive(Debug, Clone)]
pub enum LayerKind {
    /// Type 0: instance of a precomposition asset.
    Precomp {
        asset: String,
        width: f64,
        height: f64,
    },
    /// Type 1: solid color.
    Solid {
        color: String,
        width: f64,
        height: f64,
    },
    /// Type 2: image asset.
    Image { asset: String },
    /// Type 3: null (no visible content; serves as parent or scaffolding).
    Null,
    /// Type 4: shape-bearing layer.
    Shape { shapes: Vec<ShapeNode> },
    /// Anything else we don't yet model (text, audio, camera, …).
    Other { ty: u32 },
}

// ---------------------------------------------------------------------------
// Effects (kept loose for now; specific effect types interpreted at codegen)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Effect {
    pub name: Option<String>,
    pub match_name: Option<String>,
    pub ty: u32,
    pub index: Option<u32>,
    pub enabled: bool,
    pub parameters: Vec<EffectParam>,
}

#[derive(Debug, Clone)]
pub struct EffectParam {
    pub name: Option<String>,
    pub match_name: Option<String>,
    pub ty: u32,
    pub index: Option<u32>,
    /// Most effect parameters are scalar properties driven by an expression
    /// that returns a single number. We keep the raw value here and let the
    /// backend extract whatever it needs.
    pub value: EffectValue,
}

#[derive(Debug, Clone)]
pub enum EffectValue {
    Scalar(Property<Scalar>),
    Other(serde_json::Value),
}

// ---------------------------------------------------------------------------
// Expressions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Expression {
    pub id: ExprId,
    /// Raw Bodymovin-transpiled JS body (the value of the `x` field).
    pub body: String,
    /// Hash of the canonical (whitespace-normalized) body. Used by the dedup
    /// pass to recognize textually-identical expressions across properties.
    pub canonical_hash: u64,
    /// Runtime APIs this expression touches, populated by the AnalyzeRuntime
    /// pass. Used by the inline-mode backend to tree-shake unused helpers.
    pub used_apis: ApiSet,
    pub uses_value: bool,
    pub uses_this_property: bool,
    pub uses_loop_out: bool,
    /// Layer names referenced via `thisComp.layer('name')`.
    pub references_layers: Vec<String>,
    /// Effect names referenced via `effect('name')` or `effect('match-name')`.
    pub references_effects: Vec<String>,
}

#[derive(Debug, Default, Clone)]
pub struct ExprTable {
    expressions: Vec<Expression>,
    /// Inverse map: canonical_hash → ExprId. Populated lazily; passes that
    /// dedupe expressions use this to find existing entries.
    by_hash: HashMap<u64, ExprId>,
}

impl ExprTable {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn len(&self) -> usize {
        self.expressions.len()
    }
    pub fn is_empty(&self) -> bool {
        self.expressions.is_empty()
    }
    pub fn iter(&self) -> impl Iterator<Item = &Expression> {
        self.expressions.iter()
    }
    pub fn get(&self, id: ExprId) -> &Expression {
        &self.expressions[id.0 as usize]
    }
    pub fn get_mut(&mut self, id: ExprId) -> &mut Expression {
        &mut self.expressions[id.0 as usize]
    }
    pub fn lookup_by_hash(&self, hash: u64) -> Option<ExprId> {
        self.by_hash.get(&hash).copied()
    }
    /// Drop every expression outside `keep`, and renumber what is left.
    ///
    /// Returns the old → new id map. Every property still holding an id has to
    /// be rewritten through it, which is why this is not public API for
    /// anything but [`crate::expr`] — the ids are stored across the whole
    /// module and a partial remap would silently point a property at the wrong
    /// body.
    pub(crate) fn retain(&mut self, keep: &std::collections::BTreeSet<u32>) -> HashMap<u32, u32> {
        let mut map = HashMap::new();
        let mut kept = Vec::with_capacity(keep.len());
        for (i, e) in std::mem::take(&mut self.expressions)
            .into_iter()
            .enumerate()
        {
            if keep.contains(&(i as u32)) {
                map.insert(i as u32, kept.len() as u32);
                kept.push(e);
            }
        }
        self.by_hash = kept
            .iter()
            .enumerate()
            .map(|(i, e)| (e.canonical_hash, ExprId(i as u32)))
            .collect();
        for (i, e) in kept.iter_mut().enumerate() {
            e.id = ExprId(i as u32);
        }
        self.expressions = kept;
        map
    }
    /// Insert an expression, reusing an identical one if it is already here.
    ///
    /// After Effects duplicates the same expression onto every layer it is
    /// applied to, so a file routinely carries twenty copies of six distinct
    /// bodies. Properties store the returned id, so handing back the existing
    /// one deduplicates the emitted `E[]` with no remapping pass.
    ///
    /// The hash indexes the lookup and body equality confirms it, so a hash
    /// collision costs a missed dedup rather than merging two expressions that
    /// only happened to collide.
    pub fn insert(&mut self, mut e: Expression) -> ExprId {
        if let Some(&existing) = self.by_hash.get(&e.canonical_hash)
            && self.expressions[existing.0 as usize].body == e.body {
                return existing;
            }
        let id = ExprId(self.expressions.len() as u32);
        e.id = id;
        self.by_hash.insert(e.canonical_hash, id);
        self.expressions.push(e);
        id
    }
}

// ---------------------------------------------------------------------------
// Shape interning (used by the InternShapes pass; lowering leaves this empty)
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
pub struct ShapeTable {
    pub entries: Vec<ShapeNode>,
}

// ---------------------------------------------------------------------------
// Asset
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Asset {
    pub id: String,
    pub name: Option<String>,
    pub kind: AssetKind,
}

#[derive(Debug, Clone)]
pub enum AssetKind {
    Precomp {
        layers: Vec<Layer>,
    },
    Image {
        path: Option<String>,
        filename: Option<String>,
        width: f64,
        height: f64,
        embedded: bool,
    },
}

// ---------------------------------------------------------------------------
// Composition + Module
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Composition {
    pub name: Option<String>,
    pub width: f64,
    pub height: f64,
    pub frame_rate: f64,
    pub in_point: f64,
    pub out_point: f64,
    pub is_3d: bool,
}

#[derive(Debug, Clone)]
pub struct Module {
    pub composition: Composition,
    pub layers: Vec<Layer>,
    pub assets: Vec<Asset>,
    pub expressions: ExprTable,
    pub shapes_table: ShapeTable,
    pub runtime_required: ApiSet,
}

impl Module {
    pub fn new(composition: Composition) -> Self {
        Self {
            composition,
            layers: Vec::new(),
            assets: Vec::new(),
            expressions: ExprTable::new(),
            shapes_table: ShapeTable::default(),
            // Until AnalyzeRuntime narrows it, assume everything is needed.
            runtime_required: ApiSet::all(),
        }
    }

    pub fn layer(&self, id: LayerId) -> &Layer {
        &self.layers[id.0 as usize]
    }
    pub fn layer_mut(&mut self, id: LayerId) -> &mut Layer {
        &mut self.layers[id.0 as usize]
    }
}

// ---------------------------------------------------------------------------
// Runtime API bitset
// ---------------------------------------------------------------------------

bitflags! {
    /// Set of runtime helpers used by a module. The AnalyzeRuntime pass walks
    /// all expressions and unions in the flags they touch; the inline backend
    /// then emits only those helpers.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct ApiSet: u32 {
        // Top-level (always emitted by codegen today; will be tree-shaken later)
        const CUBIC_BEZIER          = 1 << 0;
        const LERP                  = 1 << 1;
        const LERP_ARRAY            = 1 << 2;
        const INTERPOLATE_KF        = 1 << 3;
        const POLYSTAR_PATH         = 1 << 4;
        // Vector arithmetic
        const SUM                   = 1 << 5;
        const SUB                   = 1 << 6;
        const MUL                   = 1 << 7;
        const DIV                   = 1 << 8;
        const CLAMP                 = 1 << 9;
        // Geometry / path
        const CREATE_PATH           = 1 << 10;
        const POINT_ON_PATH         = 1 << 11;
        const TANGENT_ON_PATH       = 1 << 12;
        // Layer / comp transforms
        const TO_COMP               = 1 << 13;
        const FROM_COMP_TO_SURFACE  = 1 << 14;
        // Time / keyframe
        const MAKE_THIS_PROPERTY    = 1 << 15;
        const VELOCITY_AT_TIME      = 1 << 16;
        const NEAREST_KEY           = 1 << 17;
        const LOOP_OUT              = 1 << 18;
        // Misc
        const RADIANS_DEGREES       = 1 << 19;
        const LAYER_REGISTRY        = 1 << 20;
        const COMP_SCOPE            = 1 << 21;
    }
}
