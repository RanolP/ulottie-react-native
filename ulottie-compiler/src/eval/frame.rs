//! Frame types + a deterministic text `Display` for snapshot testing.
//!
//! Display format goals: stable across runs, diff-friendly, line-per-element,
//! every float rounded to six fractional digits so macOS↔Linux libm noise
//! doesn't churn baselines.

use std::fmt;

/// One snapshot of the composition at a given frame.
#[derive(Debug, Clone)]
pub struct Frame {
    pub composition: FrameComposition,
    pub layers: Vec<RenderedLayer>,
}

#[derive(Debug, Clone, Copy)]
pub struct FrameComposition {
    pub width: u32,
    pub height: u32,
    pub frame_rate: f64,
    pub frame: f64,
}

#[derive(Debug, Clone)]
pub struct RenderedLayer {
    pub index: u32,
    pub name: Option<String>,
    pub layer_type: u32,
    pub transform: Transform2D,
    pub opacity: f64,
    pub visible: bool,
    pub kind: LayerKind,
    pub masks: Vec<RenderedMask>,
}

#[derive(Debug, Clone)]
pub enum LayerKind {
    /// Shape layer's draw tree.
    Shape(Vec<ShapeTree>),
    /// Precomp instance: nested layers.
    Precomp(Vec<RenderedLayer>),
    /// Null layer (transform-only container).
    Null,
    /// Layer types not yet handled by the evaluator (solid, image, text).
    Unsupported(u32),
}

#[derive(Debug, Clone)]
pub struct RenderedMask {
    pub mode: char, // 'a' or 's'
    pub inverted: bool,
    pub path: BezierPath,
    pub opacity: f64,
}

/// One node inside a shape layer's draw tree. Either a leaf primitive (with
/// its resolved geometry + styles) or a group with a local transform.
#[derive(Debug, Clone)]
pub enum ShapeTree {
    Primitive(RenderedPrimitive),
    Group {
        transform: Transform2D,
        opacity: f64,
        children: Vec<ShapeTree>,
    },
}

#[derive(Debug, Clone)]
pub struct RenderedPrimitive {
    pub geometry: Geometry,
    pub styles: Vec<RenderedStyle>,
}

#[derive(Debug, Clone)]
pub enum Geometry {
    Path(BezierPath),
}

#[derive(Debug, Clone)]
pub struct BezierPath {
    /// Vertex coordinates (`[x, y]`).
    pub vertices: Vec<[f64; 2]>,
    /// In-tangents, paired with `vertices`. Stored as offsets from the vertex.
    pub in_tangents: Vec<[f64; 2]>,
    /// Out-tangents, paired with `vertices`. Stored as offsets from the vertex.
    pub out_tangents: Vec<[f64; 2]>,
    pub closed: bool,
}

#[derive(Debug, Clone)]
pub enum RenderedStyle {
    Paint(Paint),
    Stroke {
        color: Color,
        opacity: f64,
        width: f64,
        linecap: u8,
        linejoin: u8,
        miter_limit: Option<f64>,
    },
    GradientStroke {
        kind: GradientKind,
        stops: Vec<GradientStop>,
        start: [f64; 2],
        end: [f64; 2],
        opacity: f64,
        width: f64,
        linecap: u8,
        linejoin: u8,
        miter_limit: Option<f64>,
    },
    TrimPath {
        start: f64,
        end: f64,
        offset: f64,
        mode: u8,
    },
    Unsupported(&'static str),
}

#[derive(Debug, Clone)]
pub enum Paint {
    Solid {
        color: Color,
        opacity: f64,
    },
    Gradient {
        kind: GradientKind,
        rule: FillRule,
        stops: Vec<GradientStop>,
        start: [f64; 2],
        end: [f64; 2],
        opacity: f64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GradientKind {
    Linear,
    Radial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillRule {
    NonZero,
    EvenOdd,
}

#[derive(Debug, Clone, Copy)]
pub struct GradientStop {
    pub offset: f64,
    pub color: Color,
}

#[derive(Debug, Clone, Copy)]
pub struct Color {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

/// 2D affine transform represented as `[m00 m01 m10 m11 dx dy]`. Column-major
/// is conventional for SVG `matrix(a,b,c,d,e,f)`; we follow the same order
/// (m00=a, m10=b, m01=c, m11=d, dx=e, dy=f).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform2D {
    pub m: [f64; 6],
}

impl Transform2D {
    pub fn identity() -> Self {
        Self { m: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0] }
    }
}

// ---------------------------------------------------------------------------
// Display
// ---------------------------------------------------------------------------

fn f(x: f64) -> String {
    // Six-digit rounding stabilises macOS↔Linux libm differences in trig and
    // sqrt without sacrificing useful diff resolution. `-0.000000` collapses
    // to `0.000000`.
    let r = (x * 1_000_000.0).round() / 1_000_000.0;
    let r = if r == 0.0 { 0.0 } else { r };
    format!("{r:.6}")
}

impl fmt::Display for Frame {
    fn fmt(&self, w: &mut fmt::Formatter<'_>) -> fmt::Result {
        let c = self.composition;
        writeln!(
            w,
            "Frame frame={} composition={}x{} fr={}",
            f(c.frame),
            c.width,
            c.height,
            f(c.frame_rate)
        )?;
        for layer in &self.layers {
            write_layer(w, layer, 1)?;
        }
        Ok(())
    }
}

fn indent(w: &mut fmt::Formatter<'_>, level: usize) -> fmt::Result {
    for _ in 0..level {
        w.write_str("  ")?;
    }
    Ok(())
}

fn write_layer(
    w: &mut fmt::Formatter<'_>,
    layer: &RenderedLayer,
    depth: usize,
) -> fmt::Result {
    indent(w, depth)?;
    write!(w, "Layer #{} ", layer.index)?;
    if let Some(name) = &layer.name {
        write!(w, "{:?} ", name)?;
    }
    write!(
        w,
        "ty={} opacity={} visible={}",
        layer.layer_type,
        f(layer.opacity),
        layer.visible
    )?;
    writeln!(w)?;
    indent(w, depth + 1)?;
    writeln!(w, "transform={}", fmt_xform(layer.transform))?;
    for m in &layer.masks {
        indent(w, depth + 1)?;
        writeln!(
            w,
            "Mask mode={} inv={} opacity={} d={:?}",
            m.mode,
            m.inverted,
            f(m.opacity),
            path_to_d(&m.path)
        )?;
    }
    match &layer.kind {
        LayerKind::Shape(trees) => {
            for t in trees {
                write_shape(w, t, depth + 1)?;
            }
        }
        LayerKind::Precomp(layers) => {
            indent(w, depth + 1)?;
            writeln!(w, "Precomp")?;
            for sub in layers {
                write_layer(w, sub, depth + 2)?;
            }
        }
        LayerKind::Null => {
            indent(w, depth + 1)?;
            writeln!(w, "Null")?;
        }
        LayerKind::Unsupported(ty) => {
            indent(w, depth + 1)?;
            writeln!(w, "Unsupported layer ty={ty}")?;
        }
    }
    Ok(())
}

fn write_shape(w: &mut fmt::Formatter<'_>, tree: &ShapeTree, depth: usize) -> fmt::Result {
    match tree {
        ShapeTree::Primitive(p) => {
            indent(w, depth)?;
            let d = match &p.geometry {
                Geometry::Path(path) => path_to_d(path),
            };
            writeln!(w, "Path d={:?}", d)?;
            for style in &p.styles {
                indent(w, depth + 1)?;
                write_style(w, style)?;
                writeln!(w)?;
            }
        }
        ShapeTree::Group { transform, opacity, children } => {
            indent(w, depth)?;
            writeln!(
                w,
                "Group transform={} opacity={}",
                fmt_xform(*transform),
                f(*opacity)
            )?;
            for c in children {
                write_shape(w, c, depth + 1)?;
            }
        }
    }
    Ok(())
}

fn write_style(w: &mut fmt::Formatter<'_>, style: &RenderedStyle) -> fmt::Result {
    match style {
        RenderedStyle::Paint(Paint::Solid { color, opacity }) => {
            write!(w, "Fill color={} opacity={}", fmt_color(*color), f(*opacity))
        }
        RenderedStyle::Paint(Paint::Gradient {
            kind, rule, stops, start, end, opacity,
        }) => {
            write!(
                w,
                "GradientFill kind={} rule={} opacity={} start=({},{}) end=({},{}) stops=[{}]",
                fmt_kind(*kind),
                fmt_rule(*rule),
                f(*opacity),
                f(start[0]),
                f(start[1]),
                f(end[0]),
                f(end[1]),
                fmt_stops(stops),
            )
        }
        RenderedStyle::Stroke {
            color, opacity, width, linecap, linejoin, miter_limit,
        } => {
            write!(
                w,
                "Stroke color={} opacity={} width={} lc={} lj={}",
                fmt_color(*color),
                f(*opacity),
                f(*width),
                linecap,
                linejoin,
            )?;
            if let Some(ml) = miter_limit {
                write!(w, " ml={}", f(*ml))?;
            }
            Ok(())
        }
        RenderedStyle::GradientStroke {
            kind, stops, start, end, opacity, width, linecap, linejoin, miter_limit,
        } => {
            write!(
                w,
                "GradientStroke kind={} opacity={} width={} lc={} lj={} start=({},{}) end=({},{}) stops=[{}]",
                fmt_kind(*kind),
                f(*opacity),
                f(*width),
                linecap,
                linejoin,
                f(start[0]),
                f(start[1]),
                f(end[0]),
                f(end[1]),
                fmt_stops(stops),
            )?;
            if let Some(ml) = miter_limit {
                write!(w, " ml={}", f(*ml))?;
            }
            Ok(())
        }
        RenderedStyle::TrimPath { start, end, offset, mode } => {
            write!(
                w,
                "TrimPath s={} e={} o={} m={}",
                f(*start),
                f(*end),
                f(*offset),
                mode
            )
        }
        RenderedStyle::Unsupported(label) => write!(w, "Unsupported style {label}"),
    }
}

fn fmt_kind(k: GradientKind) -> &'static str {
    match k {
        GradientKind::Linear => "linear",
        GradientKind::Radial => "radial",
    }
}

fn fmt_rule(r: FillRule) -> &'static str {
    match r {
        FillRule::NonZero => "nonzero",
        FillRule::EvenOdd => "evenodd",
    }
}

fn fmt_stops(stops: &[GradientStop]) -> String {
    let mut s = String::new();
    for (i, st) in stops.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(&format!("({},{})", f(st.offset), fmt_color(st.color)));
    }
    s
}

fn fmt_color(c: Color) -> String {
    format!(
        "rgba({},{},{},{})",
        f(c.r), f(c.g), f(c.b), f(c.a)
    )
}

fn fmt_xform(t: Transform2D) -> String {
    format!(
        "[{} {} {} {} {} {}]",
        f(t.m[0]), f(t.m[1]), f(t.m[2]), f(t.m[3]), f(t.m[4]), f(t.m[5])
    )
}

/// SVG path "d" expression — vertices joined with C (cubic bezier) when either
/// tangent is non-zero, else L. The first move is always M; closed paths get a
/// trailing Z.
fn path_to_d(p: &BezierPath) -> String {
    if p.vertices.is_empty() {
        return String::new();
    }
    let n = p.vertices.len();
    let mut s = String::new();
    let v0 = p.vertices[0];
    s.push_str(&format!("M{},{}", f(v0[0]), f(v0[1])));
    let segs = if p.closed { n } else { n - 1 };
    for i in 0..segs {
        let a = i;
        let b = (i + 1) % n;
        let va = p.vertices[a];
        let vb = p.vertices[b];
        let oa = p.out_tangents[a];
        let ib = p.in_tangents[b];
        if oa == [0.0, 0.0] && ib == [0.0, 0.0] {
            s.push_str(&format!(" L{},{}", f(vb[0]), f(vb[1])));
        } else {
            let c1 = [va[0] + oa[0], va[1] + oa[1]];
            let c2 = [vb[0] + ib[0], vb[1] + ib[1]];
            s.push_str(&format!(
                " C{},{} {},{} {},{}",
                f(c1[0]), f(c1[1]), f(c2[0]), f(c2[1]), f(vb[0]), f(vb[1])
            ));
        }
    }
    if p.closed {
        s.push_str(" Z");
    }
    s
}
