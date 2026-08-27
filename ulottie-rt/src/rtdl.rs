//! RTDL — the rt target's wire format.
//!
//! One blob carries a whole animation: a static display list (the scene at
//! frame `ip`, mirroring the skia-aot descriptor node for node) plus numeric
//! keyframe tracks (the compiler's frame-bake sampled at every integer frame
//! and compressed to change points). Everything is numbers — paths are verb +
//! point arrays, colors are float RGBA, transforms are six floats — so the
//! format is renderer-agnostic: nothing in it names tiny-skia, SVG, or any
//! string grammar.
//!
//! **The wire encoding is postcard over these structs.** The compiler links
//! this very module (`ulottie-rt` with `default-features = false`) to encode,
//! and the device runtime decodes with it, so the struct definitions below
//! *are* the format specification; there is no second document to drift from
//! it. Layout in brief:
//!
//! * [`Animation`] — design size, clock (`fr`/`ip`/`op`), a node arena
//!   (`nodes[0]` is the root group), a gradient pool, raw-RGBA images, and
//!   the tracks.
//! * [`Node`] — group / shape / image, exactly the skia-aot display-list
//!   grammar: groups carry transform, opacity, blend, clip, mask (luma or
//!   alpha, children in the arena), the matte-inversion color filter, and
//!   layer-effect stages; shapes carry geometry plus a full paint.
//! * [`Group::bbox`] — mandatory on every group that forces an offscreen
//!   layer (opacity < 1 or animated, blend, mask, matte filter, effects):
//!   the union over all sampled frames of the subtree's geometry in the
//!   group's inner space, padded for strokes and effect reach. The
//!   rasterizer sizes its scratch layer to this box; an unbounded layer was
//!   measured 13× slower. A matte-inversion group uses the filter region
//!   instead (the inversion paints the whole region, not just geometry).
//! * [`Track`] — per bound element (`slot` = the compiler's document-order
//!   element index), a set of [`Channel`]s. Each channel is a [`Keys`] list:
//!   ascending frames plus one value per frame, compressed to change points
//!   with an anchor entry before each change so piecewise-linear
//!   interpolation between entries reproduces the dense integer-frame bake
//!   exactly. Sampling before the first key writes nothing (the static node
//!   value stands, mirroring a runtime that has not applied yet); after the
//!   last key the value holds. Between integer frames the runtime
//!   interpolates linearly — eases are already folded into the per-frame
//!   samples, so fractional frames are a linear blend of two exact frames
//!   (a documented, sub-frame-only divergence from the web runtime).
//!
//! Slots appear on any entity a track can address: nodes, gradients, stops,
//! an animated clip path, and effect passes. Only bound entities carry one.

use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

extern crate alloc;

/// One animation: the static scene plus its tracks.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Animation {
    /// Design size — the space the root group draws in.
    pub width: f32,
    pub height: f32,
    /// Frame rate, first frame, one-past-last frame (Lottie semantics).
    pub fr: f32,
    pub ip: f32,
    pub op: f32,
    /// Node arena; `nodes[0]` is the root group. Group and mask children are
    /// arena indices, so the runtime can address any node by index.
    pub nodes: Vec<Node>,
    /// Gradient pool, referenced by [`PaintSource::Gradient`].
    pub gradients: Vec<Gradient>,
    /// Image pool, referenced by [`ImageRef::image`].
    pub images: Vec<Image>,
    pub tracks: Vec<Track>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Node {
    Group(Group),
    Shape(Shape),
    Image(ImageRef),
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Group {
    pub slot: Option<u32>,
    /// SVG-order `[a, b, c, d, e, f]`: maps (x, y) to
    /// (a·x + c·y + e, b·x + d·y + f). `None` is the identity.
    pub matrix: Option<[f32; 6]>,
    /// 0..=1; 1 when the markup carried none.
    pub opacity: f32,
    pub hidden: bool,
    pub blend: Option<Blend>,
    pub clip: Option<Clip>,
    pub mask: Option<Mask>,
    /// Matte inversion (the one `userSpaceOnUse` filter the compiler emits).
    pub cf: Option<MatteInvert>,
    /// Layer-effect stages, applied in order; each stage consumes the
    /// running content and replaces it.
    pub fx: Vec<FxStage>,
    /// `[x0, y0, x1, y1]` in this group's inner space (the space its
    /// children draw in). See the module doc; `None` on a layer-forcing
    /// group means the subtree never draws anything.
    pub bbox: Option<[f32; 4]>,
    pub children: Vec<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Shape {
    pub slot: Option<u32>,
    pub matrix: Option<[f32; 6]>,
    pub opacity: f32,
    pub hidden: bool,
    pub geom: Geom,
    /// Even-odd fill rule (`fill-rule="evenodd"`); nonzero winding otherwise.
    pub even_odd: bool,
    pub paint: Paint,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Geom {
    Path(PathData),
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        rx: f32,
        ry: f32,
    },
    Ellipse {
        cx: f32,
        cy: f32,
        rx: f32,
        ry: f32,
    },
}

/// A path as parallel verb and point arrays. Verbs: 0 = move-to (2 floats),
/// 1 = line-to (2), 2 = cubic-to (6: c1x c1y c2x c2y x y), 3 = close (0).
/// Empty verbs draw nothing (a shape whose geometry is animated from frame
/// `ip`, or trimmed to nothing).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PathData {
    pub verbs: Vec<u8>,
    pub points: Vec<f32>,
}

pub const VERB_MOVE: u8 = 0;
pub const VERB_LINE: u8 = 1;
pub const VERB_CUBIC: u8 = 2;
pub const VERB_CLOSE: u8 = 3;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Paint {
    /// `None` = no fill (`fill="none"`); the compiler writes the SVG default
    /// black explicitly, so absence really means none.
    pub fill: Option<PaintSource>,
    pub fill_opacity: f32,
    pub stroke: Option<PaintSource>,
    pub stroke_opacity: f32,
    pub stroke_width: f32,
    /// 0 = butt, 1 = round, 2 = square.
    pub cap: u8,
    /// 0 = miter, 1 = round, 2 = bevel.
    pub join: u8,
    pub miter_limit: f32,
    pub dash: Option<Dash>,
    /// `paint-order="stroke"`: the stroke draws under the fill.
    pub stroke_first: bool,
}

impl Default for Paint {
    fn default() -> Self {
        Paint {
            fill: None,
            fill_opacity: 1.0,
            stroke: None,
            stroke_opacity: 1.0,
            stroke_width: 1.0,
            cap: 0,
            join: 0,
            miter_limit: 4.0,
            dash: None,
            stroke_first: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Dash {
    pub array: Vec<f32>,
    pub offset: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum PaintSource {
    /// Straight (unpremultiplied) RGBA, 0..=1.
    Color([f32; 4]),
    /// Index into [`Animation::gradients`].
    Gradient(u32),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Gradient {
    pub slot: Option<u32>,
    pub radial: bool,
    /// Linear: `[x1, y1, x2, y2]`. Radial: `[cx, cy, r, 0]`.
    pub coords: [f32; 4],
    pub stops: Vec<Stop>,
    /// `gradientTransform`, same layout as [`Group::matrix`].
    pub transform: Option<[f32; 6]>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Stop {
    pub slot: Option<u32>,
    pub offset: f32,
    pub color: [f32; 4],
}

/// CSS `mix-blend-mode` values the compiler emits (normal never appears —
/// it is the absent case).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Blend {
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
    Hue,
    Saturation,
    Color,
    Luminosity,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Clip {
    /// `[x, y, w, h]`.
    Rect([f32; 4]),
    /// A single-shape `<clipPath>`; the path may be animated (its slot).
    Path {
        slot: Option<u32>,
        path: PathData,
        even_odd: bool,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Mask {
    /// Luminance mask (SVG default); false = alpha mask.
    pub luma: bool,
    pub children: Vec<u32>,
}

/// The matte-inversion color filter: content is clipped to `rect`, run
/// through the 4×5 `matrix` (rows R G B A, columns r g b a offset; all
/// channels and offsets 0..=1, applied to straight alpha).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MatteInvert {
    pub matrix: [f32; 20],
    pub rect: [f32; 4],
}

/// One effect stage: its passes each read the stage's *input* content and
/// stack over each other in order (`feMerge` semantics).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FxStage {
    pub passes: Vec<FxPass>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum FxPass {
    /// The input content, unchanged (the tint chain's base).
    Source,
    /// One 4×5 color matrix (same layout as [`MatteInvert::matrix`]).
    ColorMatrix([f32; 20]),
    /// Two matrices composed: `outer ∘ inner` (the tint ramp over luma).
    ColorMatrix2 { outer: [f32; 20], inner: [f32; 20] },
    /// Drop shadow: blur the content's alpha by `std_dev`, offset by
    /// (`dx`, `dy`), fill with `color × flood_opacity`, then the content
    /// draws over its shadow.
    Shadow {
        blur_slot: Option<u32>,
        offset_slot: Option<u32>,
        flood_slot: Option<u32>,
        std_dev: f32,
        dx: f32,
        dy: f32,
        color: [f32; 4],
        flood_opacity: f32,
    },
    /// Gaussian blur (approximated by three box passes), per-axis sigma.
    /// `wrap` mirrors SVG `edgeMode="wrap"`; false clamps edges.
    Blur {
        slot: Option<u32>,
        sx: f32,
        sy: f32,
        wrap: bool,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImageRef {
    /// Index into [`Animation::images`].
    pub image: u32,
    /// The layer box; the image center-crop-covers it (lottie-web's fit).
    pub w: f32,
    pub h: f32,
}

/// Decoded pixels: straight (unpremultiplied) RGBA8, row-major, tightly
/// packed. Decoded from the embedded data URI at compile time.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// All the channels bound to one element (`slot` = document-order index).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Track {
    pub slot: u32,
    pub channels: Vec<Channel>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Channel {
    /// Node transform (layer or group/shape transform ops).
    Matrix(Keys<[f32; 6]>),
    /// Node opacity, 0..=1.
    Opacity(Keys<f32>),
    /// Node hidden flag (the DISPLAY op); step-interpolated, 1 = hidden.
    Hidden(Keys<f32>),
    /// Path geometry (shape ops, trims folded in; also an animated clip
    /// path). Interpolated pointwise when two keys share a verb layout,
    /// stepped otherwise. Empty verbs = draws nothing this frame.
    Path(Keys<PathData>),
    /// Rect geometry `[x, y, w, h, rx, ry]`.
    Rect(Keys<[f32; 6]>),
    /// Ellipse geometry `[cx, cy, rx, ry]`.
    Ellipse(Keys<[f32; 4]>),
    /// Fill color (straight RGBA, style opacity folded into alpha).
    Fill(Keys<[f32; 4]>),
    /// Fill opacity alone (the paint is a gradient reference).
    FillOpacity(Keys<f32>),
    Stroke(Keys<[f32; 4]>),
    StrokeOpacity(Keys<f32>),
    StrokeWidth(Keys<f32>),
    /// Dash pattern + offset; stepped when the pattern length changes.
    Dash(Keys<Dash>),
    /// Gradient geometry, same layout as [`Gradient::coords`].
    Gradient(Keys<[f32; 4]>),
    /// One gradient stop: `[offset, r, g, b]` (the RAMP op writes no alpha).
    Stop(Keys<[f32; 4]>),
    /// Effect blur sigma `[sx, sy]` (targets an fx Blur pass).
    BlurStd(Keys<[f32; 2]>),
    /// Drop-shadow blur sigma (targets an fx Shadow pass).
    ShadowStd(Keys<f32>),
    /// Drop-shadow flood opacity (targets an fx Shadow pass).
    FloodOpacity(Keys<f32>),
    /// Drop-shadow offset `[dx, dy]` (targets an fx Shadow pass).
    ShadowOffset(Keys<[f32; 2]>),
}

/// Change-point keyframes: `frames` ascend, `values` is parallel.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Keys<T> {
    pub frames: Vec<f32>,
    pub values: Vec<T>,
}

impl<T> Default for Keys<T> {
    fn default() -> Self {
        Keys {
            frames: Vec::new(),
            values: Vec::new(),
        }
    }
}

/// Piecewise-linear interpolation between keyed values.
pub trait Lerp: Clone {
    fn lerp(a: &Self, b: &Self, t: f32) -> Self;
}

impl Lerp for f32 {
    fn lerp(a: &Self, b: &Self, t: f32) -> Self {
        a + (b - a) * t
    }
}

impl<const N: usize> Lerp for [f32; N] {
    fn lerp(a: &Self, b: &Self, t: f32) -> Self {
        core::array::from_fn(|i| a[i] + (b[i] - a[i]) * t)
    }
}

impl Lerp for PathData {
    /// Pointwise when the verb layouts match; hold `a` otherwise (the bake
    /// emits a step there, so this only decides sub-frame behavior).
    fn lerp(a: &Self, b: &Self, t: f32) -> Self {
        if a.verbs != b.verbs || a.points.len() != b.points.len() {
            return a.clone();
        }
        PathData {
            verbs: a.verbs.clone(),
            points: a
                .points
                .iter()
                .zip(&b.points)
                .map(|(x, y)| x + (y - x) * t)
                .collect(),
        }
    }
}

impl Lerp for Dash {
    fn lerp(a: &Self, b: &Self, t: f32) -> Self {
        if a.array.len() != b.array.len() {
            return a.clone();
        }
        Dash {
            array: a
                .array
                .iter()
                .zip(&b.array)
                .map(|(x, y)| x + (y - x) * t)
                .collect(),
            offset: a.offset + (b.offset - a.offset) * t,
        }
    }
}

impl<T: Lerp> Keys<T> {
    /// The value at frame `f`: `None` before the first key (nothing has been
    /// applied yet — the static node value stands), held after the last,
    /// linear in between. `step` truncates instead of blending.
    pub fn at(&self, f: f32, step: bool) -> Option<T> {
        let n = self.frames.len();
        if n == 0 || f < self.frames[0] {
            return None;
        }
        // Index of the last frame <= f.
        let i = match self
            .frames
            .binary_search_by(|k| k.partial_cmp(&f).unwrap_or(core::cmp::Ordering::Less))
        {
            Ok(i) => i,
            Err(0) => return None,
            Err(i) => i - 1,
        };
        if step || i + 1 >= n {
            return Some(self.values[i].clone());
        }
        let (f0, f1) = (self.frames[i], self.frames[i + 1]);
        let t = if f1 > f0 { (f - f0) / (f1 - f0) } else { 0.0 };
        Some(T::lerp(&self.values[i], &self.values[i + 1], t.clamp(0.0, 1.0)))
    }
}

/// Leads every RTDL blob, ahead of the postcard payload.
pub const MAGIC: [u8; 4] = *b"RTDL";
/// Bumped on any change to the postcard-visible shape of the structs above.
/// Postcard carries no schema, so this version is the only thing standing
/// between an OTA-updated JS bundle and an older binary: a mismatch must
/// become a refused load (`decode` errors → `loadAnimation` returns false),
/// never a garbage decode in a panic=abort profile.
pub const VERSION: u16 = 1;

/// Why a blob was refused. `Display` keeps the compiler-side test messages
/// readable; the device runtime only turns any variant into `false`.
#[derive(Debug)]
pub enum DecodeError {
    /// Too short, or the leading bytes are not `RTDL`.
    Header,
    /// The blob's format version (the compiler that wrote it is newer or
    /// older than this runtime).
    Version(u16),
    Wire(postcard::Error),
    /// Decoded, but an internal reference or count is inconsistent — the
    /// named invariant from [`Animation::validate`].
    Invalid(&'static str),
}

impl core::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            DecodeError::Header => write!(f, "not an RTDL blob (bad magic)"),
            DecodeError::Version(v) => {
                write!(f, "RTDL version {v} (this runtime speaks {VERSION})")
            }
            DecodeError::Wire(e) => write!(f, "RTDL wire decode: {e}"),
            DecodeError::Invalid(what) => write!(f, "RTDL invalid: {what}"),
        }
    }
}

/// Encode an animation to the wire bytes (used by the compiler backend):
/// magic + version, then the postcard payload.
pub fn encode(anim: &Animation) -> Result<Vec<u8>, postcard::Error> {
    let mut out = Vec::with_capacity(64);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    postcard::to_extend(anim, out)
}

/// Decode wire bytes back into an animation: header check, postcard decode,
/// then [`Animation::validate`] — so every index a renderer will chase
/// unchecked is known to be in range before the blob is accepted.
pub fn decode(bytes: &[u8]) -> Result<Animation, DecodeError> {
    if bytes.len() < 6 || bytes[..4] != MAGIC {
        return Err(DecodeError::Header);
    }
    let version = u16::from_le_bytes([bytes[4], bytes[5]]);
    if version != VERSION {
        return Err(DecodeError::Version(version));
    }
    let anim: Animation = postcard::from_bytes(&bytes[6..]).map_err(DecodeError::Wire)?;
    anim.validate().map_err(DecodeError::Invalid)?;
    Ok(anim)
}

/// `points` holds exactly what `verbs` will consume, and every verb is known.
fn path_consistent(p: &PathData) -> bool {
    let mut needed = 0usize;
    for &v in &p.verbs {
        needed += match v {
            VERB_MOVE | VERB_LINE => 2,
            VERB_CUBIC => 6,
            VERB_CLOSE => 0,
            _ => return false,
        };
    }
    needed == p.points.len()
}

fn keys_parallel<T>(k: &Keys<T>) -> bool {
    k.frames.len() == k.values.len()
}

impl Animation {
    /// The invariants the renderers rely on without checking: arena indices,
    /// gradient/image references, verb/point counts, and the frames/values
    /// parallelism of every key list. Anything a hostile or skewed blob could
    /// use to index out of range fails here, as an error the platform layer
    /// surfaces as a refused load — never a panic (the device profile is
    /// panic=abort).
    pub fn validate(&self) -> Result<(), &'static str> {
        let nodes = self.nodes.len() as u64;
        let gradients = self.gradients.len() as u32;
        let images = self.images.len() as u32;
        let paint_ok = |p: &Paint| {
            [&p.fill, &p.stroke].into_iter().all(|src| match src {
                Some(PaintSource::Gradient(g)) => *g < gradients,
                _ => true,
            })
        };
        for node in &self.nodes {
            match node {
                Node::Group(g) => {
                    if g.children.iter().any(|&c| u64::from(c) >= nodes) {
                        return Err("group child index out of range");
                    }
                    if let Some(m) = &g.mask
                        && m.children.iter().any(|&c| u64::from(c) >= nodes)
                    {
                        return Err("mask child index out of range");
                    }
                    if let Some(Clip::Path { path, .. }) = &g.clip
                        && !path_consistent(path)
                    {
                        return Err("clip path verbs/points inconsistent");
                    }
                }
                Node::Shape(s) => {
                    if let Geom::Path(p) = &s.geom
                        && !path_consistent(p)
                    {
                        return Err("shape path verbs/points inconsistent");
                    }
                    if !paint_ok(&s.paint) {
                        return Err("gradient reference out of range");
                    }
                }
                Node::Image(r) => {
                    if r.image >= images {
                        return Err("image reference out of range");
                    }
                }
            }
        }
        // The renderers and the bounds pass recurse the graph from node 0;
        // a cycle — or any node reachable along two paths — would overflow
        // the native stack (SIGSEGV under panic=abort) or blow up the walk.
        // An explicit-stack walk that refuses re-entry also bounds depth.
        if !self.nodes.is_empty() {
            let mut visited = alloc::vec![false; self.nodes.len()];
            let mut stack: Vec<u32> = alloc::vec![0];
            while let Some(i) = stack.pop() {
                let seen = &mut visited[i as usize];
                if *seen {
                    return Err("node graph is cyclic");
                }
                *seen = true;
                if let Node::Group(g) = &self.nodes[i as usize] {
                    stack.extend_from_slice(&g.children);
                    if let Some(m) = &g.mask {
                        stack.extend_from_slice(&m.children);
                    }
                }
            }
        }
        for img in &self.images {
            let expected = (img.width as usize)
                .checked_mul(img.height as usize)
                .and_then(|n| n.checked_mul(4));
            if expected != Some(img.rgba.len()) {
                return Err("image pixel count does not match its dimensions");
            }
        }
        for track in &self.tracks {
            for ch in &track.channels {
                let ok = match ch {
                    Channel::Matrix(k) => keys_parallel(k),
                    Channel::Opacity(k)
                    | Channel::Hidden(k)
                    | Channel::FillOpacity(k)
                    | Channel::StrokeOpacity(k)
                    | Channel::StrokeWidth(k)
                    | Channel::ShadowStd(k)
                    | Channel::FloodOpacity(k) => keys_parallel(k),
                    Channel::Path(k) => {
                        keys_parallel(k) && k.values.iter().all(path_consistent)
                    }
                    Channel::Rect(k) => keys_parallel(k),
                    Channel::Ellipse(k)
                    | Channel::Fill(k)
                    | Channel::Stroke(k)
                    | Channel::Gradient(k)
                    | Channel::Stop(k) => keys_parallel(k),
                    Channel::Dash(k) => keys_parallel(k),
                    Channel::BlurStd(k) | Channel::ShadowOffset(k) => keys_parallel(k),
                };
                if !ok {
                    return Err("track keys frames/values inconsistent");
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one runnable check: a populated animation round-trips through the
    /// wire encoding unchanged, and key sampling honors the
    /// none-before-first / hold-after-last / lerp-between contract.
    #[test]
    fn roundtrip_and_sampling() {
        let keys = Keys {
            frames: alloc::vec![10.0, 20.0],
            values: alloc::vec![0.0f32, 1.0],
        };
        assert_eq!(keys.at(9.0, false), None);
        assert_eq!(keys.at(10.0, false), Some(0.0));
        assert_eq!(keys.at(15.0, false), Some(0.5));
        assert_eq!(keys.at(15.0, true), Some(0.0));
        assert_eq!(keys.at(99.0, false), Some(1.0));

        let anim = Animation {
            width: 100.0,
            height: 50.0,
            fr: 30.0,
            ip: 0.0,
            op: 60.0,
            nodes: alloc::vec![
                Node::Group(Group {
                    children: alloc::vec![1],
                    opacity: 1.0,
                    ..Group::default()
                }),
                Node::Shape(Shape {
                    slot: Some(1),
                    matrix: Some([1.0, 0.0, 0.0, 1.0, 5.0, 5.0]),
                    opacity: 1.0,
                    hidden: false,
                    geom: Geom::Ellipse {
                        cx: 0.0,
                        cy: 0.0,
                        rx: 10.0,
                        ry: 10.0,
                    },
                    even_odd: false,
                    paint: Paint {
                        fill: Some(PaintSource::Color([1.0, 0.0, 0.0, 1.0])),
                        ..Paint::default()
                    },
                }),
            ],
            gradients: Vec::new(),
            images: Vec::new(),
            tracks: alloc::vec![Track {
                slot: 1,
                channels: alloc::vec![Channel::Ellipse(Keys {
                    frames: alloc::vec![0.0, 30.0],
                    values: alloc::vec![[0.0, 0.0, 10.0, 10.0], [0.0, 0.0, 20.0, 20.0]],
                })],
            }],
        };
        let bytes = encode(&anim).unwrap();
        assert_eq!(&bytes[..4], &MAGIC);
        assert!(matches!(decode(&bytes[6..]), Err(DecodeError::Header)));
        let mut skewed = bytes.clone();
        skewed[4] = 0xff;
        assert!(matches!(
            decode(&skewed),
            Err(DecodeError::Version(0x00ff))
        ));
        let mut bad = anim.clone();
        if let Node::Group(g) = &mut bad.nodes[0] {
            g.children = alloc::vec![99];
        }
        assert!(matches!(
            decode(&encode(&bad).unwrap()),
            Err(DecodeError::Invalid(_))
        ));
        let mut cyclic = anim.clone();
        if let Node::Group(g) = &mut cyclic.nodes[0] {
            g.children = alloc::vec![0];
        }
        assert!(matches!(
            decode(&encode(&cyclic).unwrap()),
            Err(DecodeError::Invalid("node graph is cyclic"))
        ));
        let back = decode(&bytes).unwrap();
        assert_eq!(back.nodes.len(), 2);
        assert_eq!(back.tracks.len(), 1);
        let Node::Shape(s) = &back.nodes[1] else {
            panic!("shape survived");
        };
        assert_eq!(s.matrix, Some([1.0, 0.0, 0.0, 1.0, 5.0, 5.0]));
    }
}
