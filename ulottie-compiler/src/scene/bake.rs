//! Initial-frame bake — the picture a document has to show before any script
//! runs.
//!
//! The planner splits an animation in two: markup for everything that cannot
//! change, and a binding table for everything that can. That split is right for
//! a module, which writes the moving half on mount — and wrong for a document
//! served on its own. A layer whose transform is animated has no `transform` at
//! all, so it draws at the origin; a shape whose `d` is animated has no `d`, so
//! it draws nothing. `bouncy_ball` rendered off-centre and `lights` rendered
//! blank for exactly those two reasons.
//!
//! So the standalone forms — [`crate::compile_document`] and the sprite behind
//! [`crate::compile_symbol`] — are baked at the composition's first frame:
//! every binding is evaluated once, here, and written as an ordinary attribute.
//! The result renders with no JavaScript, which is what SSR, `<noscript>` and
//! `<img src="…svg">` all need, and the module can still hydrate it, because
//! baking only ever *adds attributes* — it never changes the element tree the
//! bindings are indexed by.
//!
//! Two rules keep hydration sound, and together they are why this mirrors
//! `runtime/ops/*.js` op by op instead of reusing the planner's own
//! static-value branches:
//!
//! * **Write exactly what the op writes.** The planner bakes a static fill as
//!   `fill="#f00" fill-opacity=".5"`, but `bFill` writes one combined
//!   `fill="rgba(…)"` and never touches `fill-opacity`. Baking the planner's
//!   form would leave an opacity attribute behind that the runtime never
//!   overwrites, and the alpha would apply twice from the first frame on.
//! * **Write nothing the op would not have written.** A binding the runtime
//!   skips — one gated off at this frame, inside a layer that is not yet on —
//!   is skipped here too, so nothing is left for the runtime to fail to clear.
//!
//! Expressions are the one place this cannot be exact: evaluating them needs
//! the expression engine, which is JavaScript. An expression-driven property
//! bakes to its fallback — the keyframes it reads as `value` — which is what
//! the runtime itself falls back to when no engine is present.

use std::collections::HashMap;

use super::prop::{Anim, AnimKind, Easing};
use super::svg::FlatPath;
use super::{Arg, Binding, Planner, Prop, op, svg};

/// Extra attributes per element, keyed by arena id. Applied while serializing
/// the document, so the module's own copy of the markup stays lean.
pub(crate) type Overlay = HashMap<usize, Vec<(String, String)>>;

/// Arc-length samples per spatial segment. Matches `SP_SEG` in spatial.js.
const SP_SEG: usize = 200;

impl Planner<'_> {
    /// Every binding's value at the composition's first frame, as attributes.
    pub(crate) fn initial_frame(&self) -> Overlay {
        let f = self.payload.c.ip;
        let times = self.slot_times(f);
        let mut out: Overlay = HashMap::new();
        for (i, b) in self.bindings.iter().enumerate() {
            // Gates are evaluated on the composition clock, not the binding's
            // own — see the `gateOn` loop in core.js.
            let gate = self.bind_gate.get(i).copied().unwrap_or(0);
            if gate > 0 {
                let [lo, hi] = self.gates[gate as usize - 1];
                if f < lo || f >= hi {
                    continue;
                }
            }
            let slot = self.slots.get(i).copied().unwrap_or(0) as usize;
            let at = times.get(slot).copied().unwrap_or(f);
            let mut attrs = Vec::new();
            self.bake(b, at, &mut attrs);
            if !attrs.is_empty() {
                out.entry(b.el).or_default().extend(attrs);
            }
        }
        out
    }

    /// The frame each clock slot reads at composition frame `f`. Mirrors the
    /// `T` loop in core.js: slot 0 is the composition itself, and every other
    /// slot is its parent less that layer's own start time — unless a time
    /// remap replaces the clock outright.
    fn slot_times(&self, f: f64) -> Vec<f64> {
        let mut t = Vec::with_capacity(self.timelines.len() + 1);
        t.push(f);
        for (i, row) in self.timelines.iter().enumerate() {
            let parent = t.get(row[0] as usize).copied().unwrap_or(f);
            if let Some(Some(remap)) = self.remaps.get(i) {
                // Lottie stores the remap in seconds; clocks are in frames.
                let secs = self.scalar(Some(remap), parent, 0.0);
                t.push(secs * self.payload.c.fr);
                continue;
            }
            t.push((parent - row[1]) / row[2]);
        }
        t
    }

    /// One binding's attributes at frame `f`. Each arm is the compile-time
    /// twin of the same-named binder in `runtime/ops/`.
    fn bake(&self, b: &Binding, f: f64, out: &mut Vec<(String, String)>) {
        let prop = |i: usize| match b.args.get(i) {
            Some(Arg::Prop(p)) => Some(p),
            _ => None,
        };
        let num = |i: usize| match b.args.get(i) {
            // Plain numbers travel the wire quantized to three decimals, so
            // the runtime reads them back rounded; match that here.
            Some(Arg::Num(n)) => svg::q(*n),
            Some(Arg::Tag(t)) => *t as f64,
            _ => 0.0,
        };

        match b.op {
            op::TRANSFORM => out.push(matrix_attr(
                self.vec2(prop(0), f, [0.0, 0.0]),
                self.vec2(prop(1), f, [0.0, 0.0]),
                self.vec2(prop(2), f, [100.0, 100.0]),
                self.scalar(prop(3), f, 0.0),
                0.0,
                0.0,
            )),

            // Mirrors `oTransformSkew`.
            op::TRANSFORM_SKEW => out.push(matrix_attr(
                self.vec2(prop(0), f, [0.0, 0.0]),
                self.vec2(prop(1), f, [0.0, 0.0]),
                self.vec2(prop(2), f, [100.0, 100.0]),
                self.scalar(prop(3), f, 0.0),
                self.scalar(prop(4), f, 0.0),
                self.scalar(prop(5), f, 0.0),
            )),

            // The linear part is a constant string the compiler already built;
            // only the two translation components are written per frame.
            op::TRANSLATE => {
                // No prefix is the identity linear part, which the binder
                // spells `translate(`. Matching it matters: this attribute is
                // one the runtime overwrites, and the two must agree.
                let prefix = match b.args.first() {
                    Some(Arg::Str(s)) => s.as_str(),
                    _ => "translate(",
                };
                let p = self.vec2(prop(3), f, [0.0, 0.0]);
                out.push((
                    "transform".into(),
                    format!(
                        "{prefix}{},{})",
                        svg::nd(p[0] + num(1), 100.0),
                        svg::nd(p[1] + num(2), 100.0)
                    ),
                ));
            }

            op::OPACITY => out.push((
                "opacity".into(),
                svg::n(self.scalar(prop(0), f, 100.0) / 100.0),
            )),

            // The runtime clears the inline display when the layer is on, so
            // an attribute is only written when it is off.
            op::DISPLAY => {
                if f < num(0) || f >= num(1) {
                    out.push(hidden());
                }
            }

            op::SHAPE | op::SHAPE_RECT | op::SHAPE_ELLIPSE | op::SHAPE_STAR => {
                self.bake_shape(b, f, out)
            }

            // Animated effect parameters, mirroring `ops/fx.js` write for
            // write.
            op::FX_BLUR => {
                let s = self.scalar(prop(0), f, 0.0) * 0.3;
                let d = num(1) as u32;
                let sx = if d == 3 { 0.0 } else { s };
                let sy = if d == 2 { 0.0 } else { s };
                out.push(("stdDeviation".into(), format!("{sx} {sy}")));
            }
            op::FX_STD => {
                out.push(("stdDeviation".into(), format!("{}", self.scalar(prop(0), f, 0.0) / 4.0)));
            }
            op::FX_FLOOD_O => {
                out.push((
                    "flood-opacity".into(),
                    format!("{}", self.scalar(prop(0), f, 0.0) / 255.0),
                ));
            }
            op::FX_OFFSET => {
                let rad = (self.scalar(prop(0), f, 0.0) - 90.0).to_radians();
                let d = self.scalar(prop(1), f, 0.0);
                out.push(("dx".into(), format!("{}", d * rad.cos())));
                out.push(("dy".into(), format!("{}", d * rad.sin())));
            }

            // Mirrors `oDash`: `[count, length…, offset]`, the same raw
            // space-joined numbers.
            op::DASH => {
                if let Some(Arg::List(items)) = b.args.first() {
                    let count = match items.first() {
                        Some(Arg::Num(n)) => *n as usize,
                        _ => 0,
                    };
                    let at = |i: usize| match items.get(i) {
                        Some(Arg::Prop(p)) => self.scalar(Some(p), f, 0.0),
                        _ => 0.0,
                    };
                    let arr = (0..count)
                        .map(|j| svg::n(at(1 + j)))
                        .collect::<Vec<_>>()
                        .join(" ");
                    out.push(("stroke-dasharray".into(), arr));
                    out.push(("stroke-dashoffset".into(), svg::n(at(1 + count))));
                }
            }

            // Several path properties into one element's `d`; no trim (a
            // trimmed shape never lands in a bucket). Mirrors `oShapeMulti`.
            op::SHAPE_MULTI => {
                let Some(Arg::List(items)) = b.args.first() else {
                    return;
                };
                let mut d = String::new();
                for it in items {
                    if let Arg::Prop(p) = it
                        && let Prop::Path(fp) = self.value_at(p, f)
                    {
                        d.push_str(&fp.to_d());
                    }
                }
                if !d.is_empty() {
                    out.push(("d".into(), d));
                }
            }

            op::RECT => {
                let s = self.vec2(prop(0), f, [0.0, 0.0]);
                let p = self.vec2(prop(1), f, [0.0, 0.0]);
                let r = self.scalar(prop(2), f, 0.0);
                out.push(("x".into(), svg::n(p[0] - s[0] / 2.0)));
                out.push(("y".into(), svg::n(p[1] - s[1] / 2.0)));
                out.push(("width".into(), svg::n(s[0])));
                out.push(("height".into(), svg::n(s[1])));
                if r > 0.0 {
                    let c = svg::n(r.min(s[0] / 2.0).min(s[1] / 2.0));
                    out.push(("rx".into(), c.clone()));
                    out.push(("ry".into(), c));
                }
            }

            op::ELLIPSE => {
                let s = self.vec2(prop(0), f, [0.0, 0.0]);
                let p = self.vec2(prop(1), f, [0.0, 0.0]);
                out.push(("cx".into(), svg::n(p[0])));
                out.push(("cy".into(), svg::n(p[1])));
                out.push(("rx".into(), svg::n(s[0] / 2.0)));
                out.push(("ry".into(), svg::n(s[1] / 2.0)));
            }

            // A null paint means the fill is a gradient reference already in
            // the markup, and only its opacity varies.
            op::FILL => {
                let o = self.scalar(prop(1), f, 100.0);
                match prop(0) {
                    None => out.push(("fill-opacity".into(), svg::n(o / 100.0))),
                    Some(c) => {
                        let c = self.vector(Some(c), f, &[0.0, 0.0, 0.0, 1.0]);
                        out.push(("fill".into(), css(&c, o)));
                    }
                }
            }

            op::STROKE => {
                let o = self.scalar(prop(1), f, 100.0);
                out.push(("stroke-width".into(), svg::n(self.scalar(prop(2), f, 0.0))));
                match prop(0) {
                    None => out.push(("stroke-opacity".into(), svg::n(o / 100.0))),
                    Some(c) => {
                        let c = self.vector(Some(c), f, &[0.0, 0.0, 0.0, 1.0]);
                        out.push(("stroke".into(), css(&c, o)));
                    }
                }
            }

            op::GRADIENT => {
                let a = self.vec2(prop(1), f, [0.0, 0.0]);
                let b2 = self.vec2(prop(2), f, [0.0, 0.0]);
                if num(0) == 2.0 {
                    out.push(("cx".into(), svg::n(a[0])));
                    out.push(("cy".into(), svg::n(a[1])));
                    out.push(("r".into(), svg::n((b2[0] - a[0]).hypot(b2[1] - a[1]))));
                } else {
                    out.push(("x1".into(), svg::n(a[0])));
                    out.push(("y1".into(), svg::n(a[1])));
                    out.push(("x2".into(), svg::n(b2[0])));
                    out.push(("y2".into(), svg::n(b2[1])));
                }
            }

            // One stop of a keyframed ramp. Mirrors `oRamp`, which writes no
            // `stop-opacity` — see there for why a ramp with alpha stops does
            // not take this path at all.
            op::RAMP => {
                let v = self.vector(prop(0), f, &[0.0, 0.0, 0.0, 0.0]);
                out.push(("offset".into(), svg::n(v[0])));
                out.push((
                    "stop-color".into(),
                    format!(
                        "rgb({},{},{})",
                        (v[1] * 255.0 + 0.5) as i64,
                        (v[2] * 255.0 + 0.5) as i64,
                        (v[3] * 255.0 + 0.5) as i64
                    ),
                ));
            }

            // These read the layer table rather than carrying a second copy of
            // the same keyframes. A missing field was elided as equal to its
            // default — the defaults here match `flat::RECORD_DEFAULTS`.
            op::LAYER_TX => {
                let Some(rec) = self.layers.get(num(0) as usize) else {
                    return;
                };
                out.push(matrix_attr(
                    self.vec2(rec.p.as_ref(), f, [0.0, 0.0]),
                    self.vec2(rec.a.as_ref(), f, [0.0, 0.0]),
                    self.vec2(rec.sc.as_ref(), f, [100.0, 100.0]),
                    self.scalar(rec.r.as_ref(), f, 0.0),
                    0.0,
                    0.0,
                ));
            }

            op::LAYER_OP => {
                let Some(rec) = self.layers.get(num(0) as usize) else {
                    return;
                };
                out.push((
                    "opacity".into(),
                    svg::n(self.scalar(rec.o.as_ref(), f, 100.0) / 100.0),
                ));
            }

            _ => {}
        }
    }

    /// Geometry, optionally trimmed, as a `d`. Mirrors `bShape`.
    fn bake_shape(&self, b: &Binding, f: f64, out: &mut Vec<(String, String)>) {
        /// A `Tag` argument's value — an enumeration, absent reads as zero.
        fn tag(g: &[Arg], i: usize) -> u32 {
            match g.get(i) {
                Some(Arg::Tag(t)) => *t,
                _ => 0,
            }
        }
        // The generator is the op; the arguments are the descriptor, minus the
        // tag that used to lead it.
        let g = &b.args;
        let at = |i: usize| match g.get(i) {
            Some(Arg::Prop(p)) => Some(self.value_at(p, f)),
            _ => None,
        };
        let v2 = |i: usize| match at(i) {
            Some(p) => {
                let v = p.as_vec().map(|v| v.to_vec()).unwrap_or_default();
                [
                    v.first().copied().unwrap_or(0.0),
                    v.get(1).copied().unwrap_or(0.0),
                ]
            }
            None => [0.0, 0.0],
        };
        let n1 = |i: usize| at(i).and_then(|p| p.as_scalar()).unwrap_or(0.0);

        let path = match b.op {
            op::SHAPE => match at(0) {
                Some(Prop::Path(p)) => p,
                _ => return,
            },
            op::SHAPE_RECT => flat(crate::eval::geometry::rect_to_path(
                v2(1),
                v2(0),
                n1(2),
                tag(g, 3) != 0,
            )),
            op::SHAPE_ELLIPSE => flat(crate::eval::geometry::ellipse_to_path(
                v2(1),
                v2(0),
                tag(g, 2) != 0,
            )),
            op::SHAPE_STAR => flat(crate::eval::geometry::polystar_to_path(
                tag(g, 0) as u8,
                v2(2),
                n1(1),
                n1(3),
                n1(4),
                n1(5),
                n1(6),
                n1(7),
                tag(g, 8) != 0,
            )),
            _ => return,
        };

        // The trim chain is always the last argument, and `Null` when absent:
        // `[count, (s, e, o, mode) × count]`, steps in application order.
        let Some(Arg::List(t)) = b.args.last() else {
            out.push(("d".into(), path.to_d()));
            return;
        };
        let count = match t.first() {
            Some(Arg::Num(n)) => *n as usize,
            _ => 0,
        };
        let tp = |i: usize| match t.get(i) {
            Some(Arg::Prop(p)) => self.value_at(p, f).as_scalar().unwrap_or(0.0),
            _ => 0.0,
        };
        let steps: Vec<(f64, f64, f64)> = (0..count)
            .map(|j| (tp(1 + j * 4), tp(2 + j * 4), tp(3 + j * 4)))
            .collect();
        let src = crate::eval::trim::Flat {
            v: path.v.clone(),
            i: path.i.clone(),
            o: path.o.clone(),
            c: path.c,
        };
        match crate::eval::trim::trim_chain(&src, &steps) {
            crate::eval::trim::Trimmed::Whole => out.push(("d".into(), path.to_d())),
            crate::eval::trim::Trimmed::Empty => out.push(hidden()),
            crate::eval::trim::Trimmed::Path(p) => out.push((
                "d".into(),
                FlatPath {
                    v: p.v,
                    i: p.i,
                    o: p.o,
                    c: p.c,
                }
                .to_d(),
            )),
        }
    }

    // -----------------------------------------------------------------------
    // Property evaluation
    // -----------------------------------------------------------------------

    fn scalar(&self, p: Option<&Prop>, f: f64, default: f64) -> f64 {
        match p {
            Some(p) => self.value_at(p, f).as_scalar().unwrap_or(default),
            None => default,
        }
    }

    fn vector(&self, p: Option<&Prop>, f: f64, default: &[f64]) -> Vec<f64> {
        match p {
            Some(p) => match self.value_at(p, f) {
                Prop::Vector(v) => v,
                Prop::Scalar(n) => vec![n],
                _ => default.to_vec(),
            },
            None => default.to_vec(),
        }
    }

    fn vec2(&self, p: Option<&Prop>, f: f64, default: [f64; 2]) -> [f64; 2] {
        let v = self.vector(p, f, &default);
        [
            v.first().copied().unwrap_or(default[0]),
            v.get(1).copied().unwrap_or(default[1]),
        ]
    }

    /// A property's value at `f`, as a static `Prop`. Mirrors `resolve` in
    /// kf.js — including its behaviour with no expression engine, where an
    /// expression-driven property falls back to the source it reads.
    fn value_at(&self, p: &Prop, f: f64) -> Prop {
        match p {
            Prop::Anim(a) => self.anim_at(a, f),
            Prop::Expr { fallback, .. } => match fallback {
                Some(fb) => self.value_at(fb, f),
                None => Prop::Scalar(0.0),
            },
            _ => p.clone(),
        }
    }

    /// Keyframe interpolation, mirroring `keyframed` in kf.js.
    fn anim_at(&self, a: &Anim, f: f64) -> Prop {
        let n = a.t.len();
        if n == 0 {
            return Prop::Scalar(0.0);
        }
        let at = |i: usize| key_value(a, &a.v, &a.paths, i);
        if f <= a.t[0] {
            return at(0);
        }
        if f >= a.t[n - 1] {
            return at(n - 1);
        }

        // Largest `i` with `t[i] <= f`, bounded to a real segment.
        let mut i = 0;
        while i + 2 < n && a.t[i + 1] <= f {
            i += 1;
        }

        let span = a.t[i + 1] - a.t[i];
        if span == 0.0 {
            return at(i + 1);
        }
        if a.hold
            .as_ref()
            .is_some_and(|h| h.get(i).copied().unwrap_or(0) != 0)
        {
            return at(i);
        }

        let mut u = (f - a.t[i]) / span;
        if let Some(ez) = &a.ez {
            let k = ez.get(i).copied().unwrap_or(0) as usize;
            if k != 0
                && let Some(e) = self.easings.get(k) {
                    u = ease(e, u);
                }
        }

        let start = at(i);
        let end = at(i + 1);

        // Spatial tangents bend the segment and re-pace it by arc length. The
        // runtime tests only the first two components before taking the slow
        // path, so this does too.
        if a.kind == AnimKind::Vector
            && let (Some(to), Some(ti)) = (&a.to, &a.ti) {
                let base = i * a.dim;
                let live = |c: &[f64], k: usize| c.get(base + k).copied().unwrap_or(0.0) != 0.0;
                if (live(to, 0) || live(to, 1) || live(ti, 0) || live(ti, 1))
                    && let (Prop::Vector(p0), Prop::Vector(p1)) = (&start, &end) {
                        let d = a.dim.min(p0.len()).min(p1.len());
                        return Prop::Vector(spatial(
                            &p0[..d],
                            &p1[..d],
                            &to[base..base + d],
                            &ti[base..base + d],
                            u,
                        ));
                    }
            }

        match (start, end) {
            (Prop::Scalar(x), Prop::Scalar(y)) => Prop::Scalar(x + (y - x) * u),
            (Prop::Vector(x), Prop::Vector(y)) => Prop::Vector(
                (0..x.len().min(y.len()))
                    .map(|k| x[k] + (y[k] - x[k]) * u)
                    .collect(),
            ),
            (Prop::Path(x), Prop::Path(y)) => Prop::Path(lerp_path(&x, &y, u)),
            (x, _) => x,
        }
    }
}

/// One keyframe's value out of a flat column, in the animation's own kind.
fn key_value(a: &Anim, v: &[f64], paths: &[FlatPath], i: usize) -> Prop {
    match a.kind {
        AnimKind::Path => Prop::Path(paths.get(i).cloned().unwrap_or_default()),
        AnimKind::Scalar => Prop::Scalar(v.get(i).copied().unwrap_or(0.0)),
        AnimKind::Vector => {
            let base = i * a.dim;
            Prop::Vector(
                (0..a.dim)
                    .map(|k| v.get(base + k).copied().unwrap_or(0.0))
                    .collect(),
            )
        }
    }
}

/// `translate(p) rotate(r) scale(s) translate(-a)`, folded to one `matrix()`.
/// The same composition — and the same per-role precision — as `bTransform`.
fn matrix_attr(
    p: [f64; 2],
    a: [f64; 2],
    s: [f64; 2],
    r: f64,
    sk: f64,
    sa: f64,
) -> (String, String) {
    let (sn, cs) = r.to_radians().sin_cos();
    let (sx, sy) = (s[0] / 100.0, s[1] / 100.0);
    let (mut m0, mut m1, mut m2, mut m3) = (cs * sx, sn * sx, -sn * sy, cs * sy);
    if sk != 0.0 {
        // The same factor `TransformSpec::to_matrix` folds in.
        let t = (-sk.to_radians()).tan();
        let (s2, c2) = sa.to_radians().sin_cos();
        let (f0, f1, f2, f3) = (
            1.0 + t * s2 * c2,
            t * c2 * c2,
            -t * s2 * s2,
            1.0 - t * s2 * c2,
        );
        let (g0, g1) = (cs * f0 - sn * f2, sn * f0 + cs * f2);
        let (g2, g3) = (cs * f1 - sn * f3, sn * f1 + cs * f3);
        m0 = g0 * sx;
        m1 = g1 * sx;
        m2 = g2 * sy;
        m3 = g3 * sy;
    }
    (
        "transform".into(),
        svg::matrix_str(&[
            m0,
            m1,
            m2,
            m3,
            p[0] - (m0 * a[0] + m2 * a[1]),
            p[1] - (m1 * a[0] + m3 * a[1]),
        ]),
    )
}

/// What the runtime's `el.style.display = 'none'` looks like in markup.
fn hidden() -> (String, String) {
    ("style".into(), "display:none".into())
}

fn flat(p: crate::eval::BezierPath) -> FlatPath {
    FlatPath::from_parts(&p.vertices, &p.in_tangents, &p.out_tangents, p.closed)
}

/// Lottie's 0..1 channels plus a 0..100 style opacity, as one SVG paint.
/// Byte-for-byte the same string `css()` builds in css.js.
fn css(c: &[f64], o: f64) -> String {
    let ch = |x: f64| (x * 255.0 + 0.5) as i64;
    let (r, g, b) = (
        ch(c.first().copied().unwrap_or(0.0)),
        ch(c.get(1).copied().unwrap_or(0.0)),
        ch(c.get(2).copied().unwrap_or(0.0)),
    );
    let a = c.get(3).copied().unwrap_or(1.0) * o / 100.0;
    if a >= 1.0 {
        format!("rgb({r},{g},{b})")
    } else {
        format!("rgba({r},{g},{b},{a})")
    }
}

/// Cubic-bezier timing solve. Mirrors `EASE` in ease.js, iteration for
/// iteration — including both early exits, which change the last digit.
fn ease(e: &Easing, u: f64) -> f64 {
    let (x1, y1, x2, y2) = (e[0], e[1], e[2], e[3]);
    let mut s = u;
    for _ in 0..8 {
        let m = 1.0 - s;
        let x = 3.0 * m * m * s * x1 + 3.0 * m * s * s * x2 + s * s * s - u;
        if x > -1e-6 && x < 1e-6 {
            break;
        }
        let dx = 3.0 * m * m * x1 + 6.0 * m * s * (x2 - x1) + 3.0 * s * s * (1.0 - x2);
        if dx > -1e-6 && dx < 1e-6 {
            break;
        }
        s = (s - x / dx).clamp(0.0, 1.0);
    }
    let m = 1.0 - s;
    3.0 * m * m * s * y1 + 3.0 * m * s * s * y2 + s * s * s
}

/// Per-vertex path lerp. Mirrors `lerpPath` in kfpath.js, closure flip and all.
fn lerp_path(a: &FlatPath, b: &FlatPath, u: f64) -> FlatPath {
    if a.v.len() != b.v.len() {
        return a.clone();
    }
    let mix = |x: &[f64], y: &[f64]| -> Vec<f64> {
        (0..a.v.len())
            .map(|k| {
                let (p, q) = (
                    x.get(k).copied().unwrap_or(0.0),
                    y.get(k).copied().unwrap_or(0.0),
                );
                p + (q - p) * u
            })
            .collect()
    };
    FlatPath {
        v: mix(&a.v, &b.v),
        i: mix(&a.i, &b.i),
        o: mix(&a.o, &b.o),
        c: if u < 0.5 { a.c } else { b.c },
    }
}

/// Arc-length sample of one spatial segment. `spBuild` and `spSample` from
/// spatial.js, fused — nothing here is reused across frames, since there is
/// only ever one frame.
fn spatial(a: &[f64], b: &[f64], to: &[f64], ti: &[f64], u: f64) -> Vec<f64> {
    let d = a.len();
    let mut pts = vec![0.0; (SP_SEG + 1) * d];
    let mut cum = vec![0.0; SP_SEG + 1];
    let mut total = 0.0;
    // `k` addresses three buffers at once (`cum[k]`, this chunk, the chunk
    // before it), so the index is the loop.
    #[allow(clippy::needless_range_loop)]
    for k in 0..=SP_SEG {
        let t = k as f64 / SP_SEG as f64;
        let m = 1.0 - t;
        let (c0, c1, c2, c3) = (m * m * m, 3.0 * m * m * t, 3.0 * m * t * t, t * t * t);
        let base = k * d;
        let mut dist = 0.0;
        for j in 0..d {
            let (p0, p3) = (a[j], b[j]);
            let x = c0 * p0 + c1 * (p0 + to[j]) + c2 * (p3 + ti[j]) + c3 * p3;
            pts[base + j] = x;
            if k > 0 {
                let dd = x - pts[base - d + j];
                dist += dd * dd;
            }
        }
        if k > 0 {
            total += dist.sqrt();
        }
        cum[k] = total;
    }
    if total == 0.0 {
        return pts[..d].to_vec();
    }
    let target = u * total;
    let (mut lo, mut hi) = (0usize, SP_SEG);
    while hi - lo > 1 {
        let m = (lo + hi) / 2;
        if cum[m] <= target {
            lo = m;
        } else {
            hi = m;
        }
    }
    let span = cum[hi] - cum[lo];
    let frac = if span > 0.0 {
        (target - cum[lo]) / span
    } else {
        0.0
    };
    (0..d)
        .map(|j| {
            let x = pts[lo * d + j];
            x + (pts[hi * d + j] - x) * frac
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_paint_is_one_attribute_not_two() {
        // The planner would write `fill="#f00" fill-opacity=".5"` here. The
        // runtime writes one combined paint and never clears a separate
        // opacity, so baking the planner's form would double the alpha.
        assert_eq!(css(&[1.0, 0.0, 0.0, 1.0], 50.0), "rgba(255,0,0,0.5)");
        assert_eq!(css(&[1.0, 0.0, 0.0, 1.0], 100.0), "rgb(255,0,0)");
    }

    #[test]
    fn easing_matches_the_runtime_solver() {
        // Symmetric handles keep the midpoint at the midpoint.
        assert!((ease(&[0.4, 0.0, 0.6, 1.0], 0.5) - 0.5).abs() < 1e-9);
        assert_eq!(ease(&[0.4, 0.0, 0.6, 1.0], 0.0), 0.0);
        assert!((ease(&[0.4, 0.0, 0.6, 1.0], 1.0) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_straight_spatial_segment_is_paced_evenly() {
        // No tangents: arc length is proportional to `u`, so the sample lands
        // exactly where a plain lerp would.
        let p = spatial(&[0.0, 0.0], &[10.0, 0.0], &[0.0, 0.0], &[0.0, 0.0], 0.25);
        assert!((p[0] - 2.5).abs() < 1e-6, "got {p:?}");
    }
}
