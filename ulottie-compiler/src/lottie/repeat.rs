//! The repeater modifier (`rp`): n copies of the shapes above it, each a
//! transform application further along.
//!
//! lottie-web's `RepeaterModifier` is a run-time clone factory: it splices
//! `ceil(c)` groups into the item list and, per frame, writes each group a
//! cumulative matrix — rotation and scale compose about the transform's
//! anchor, translation accumulates `k · p`, and opacity ramps `so → eo`
//! across the copies. Which copy sits where in paint order depends on `m`
//! (sequential puts copy 0 on top).
//!
//! A **static** repeater — every parameter constant, integral copy count,
//! zero offset — is frame-invariant, so it expands here at the parse
//! boundary: the covered items are replaced by `n` groups, copy `k`
//! carrying the composed transform as an ordinary group `tr`. Nothing
//! downstream learns a repeater existed. Anything else (animated copies,
//! fractional offset) stays a refusal; `support::scan` reaches the same
//! verdict through the same [`expand`].

use super::graphic::{GraphicElement, RepeatTransform};
use super::property::Property;

/// An affine 2D transform as an SVG `matrix(a,b,c,d,e,f)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Affine(pub [f64; 6]);

impl Affine {
    fn translate(tx: f64, ty: f64) -> Self {
        Affine([1.0, 0.0, 0.0, 1.0, tx, ty])
    }

    /// `this = this · other` — apply `other` first, then `this`.
    fn then(&self, other: Affine) -> Affine {
        let a = self.0;
        let b = other.0;
        Affine([
            a[0] * b[0] + a[2] * b[1],
            a[1] * b[0] + a[3] * b[1],
            a[0] * b[2] + a[2] * b[3],
            a[1] * b[2] + a[3] * b[3],
            a[0] * b[4] + a[2] * b[5] + a[4],
            a[1] * b[4] + a[3] * b[5] + a[5],
        ])
    }
}

/// A static number, or none.
fn stat(p: &Property) -> Option<f64> {
    match p {
        Property::Static(s) => s.value.as_f64(),
        _ => None,
    }
}

/// A static 2-vector (which may be a 2- or 3-component array), or none.
fn stat2(p: &Option<Property>, default: [f64; 2]) -> Option<[f64; 2]> {
    match p {
        Some(Property::Static(s)) => {
            let a = s.value.as_array()?;
            Some([
                a.first().and_then(|v| v.as_f64()).unwrap_or(default[0]),
                a.get(1).and_then(|v| v.as_f64()).unwrap_or(default[1]),
            ])
        }
        _ => None,
    }
}

/// A static number with a default, for optional properties.
fn statd(p: &Option<Property>, default: f64) -> Option<f64> {
    match p {
        Some(Property::Static(s)) => Some(s.value.as_f64().unwrap_or(default)),
        None => Some(default),
        _ => None,
    }
}

/// The transform application lottie-web's `applyTransforms` makes `k` times:
/// rotation about the anchor, scale about the anchor (compounding to
/// `s^k`), translation by `k · p`. Rotation follows the SVG sign convention
/// (positive turns clockwise on screen); lottie-web applies `-r`, mirrored
/// back here by composing with the angle it writes.
fn applications(tr: &RepeatTransform, k: f64) -> Option<Affine> {
    let a = stat2(&tr.a, [0.0, 0.0])?;
    let p = stat2(&tr.p, [0.0, 0.0])?;
    let s = stat2(&tr.s, [100.0, 100.0])?;
    let r = statd(&tr.r, 0.0)?;

    let theta = r.to_radians() * k;
    let (sin, cos) = theta.sin_cos();
    let rot = Affine([cos, sin, -sin, cos, 0.0, 0.0]);
    let sx = (s[0] / 100.0).powf(k);
    let sy = (s[1] / 100.0).powf(k);
    let scale = Affine([sx, 0.0, 0.0, sy, 0.0, 0.0]);
    let pre = Affine::translate(-a[0], -a[1]);
    let post = Affine::translate(a[0], a[1]);
    let move_p = Affine::translate(p[0] * k, p[1] * k);

    // rMatrix · sMatrix · pMatrix, each about the anchor.
    Some(post.then(rot).then(pre).then(post).then(scale).then(pre).then(move_p))
}

/// Expand a static repeater. `items` is the full item list; `at` is the
/// repeater's index in it. Returns `None` when the repeater is not
/// statically expandable (the caller keeps its refusal), `Some(nodes)` when
/// the covered items have been replaced by the copies.
pub fn expand(items: &[GraphicElement], at: usize) -> Option<Vec<GraphicElement>> {
    let GraphicElement::Repeater {
        hidden, c, o, m, tr, ..
    } = &items[at]
    else {
        return None;
    };
    if *hidden {
        return None;
    }
    // `m=2` (simultaneous) differs from `m=1` only in paint order; accept
    // both and order copies the way each mode lays them out.
    let sequential = m.unwrap_or(1) == 1;

    let copies = match (stat(c), stat(o)) {
        (Some(c), Some(o)) if o == 0.0 && c.fract() == 0.0 && (1.0..=512.0).contains(&c) => {
            c as u32
        }
        _ => return None,
    };

    let head = &items[..at];
    // The covered items must not carry their own transform — the copy
    // transform is appended as a `tr`, and two at one level do not compose
    // the way the repeater means.
    if head.iter().any(|e| {
        matches!(e, GraphicElement::Transform { .. } | GraphicElement::Repeater { .. })
    }) {
        return None;
    }

    let so = statd(&tr.so, 100.0)?;
    let eo = statd(&tr.eo, 100.0)?;

    let mut out = Vec::with_capacity(copies as usize + items.len() - at);
    for k in 0..copies {
        let m = applications(tr, k as f64)?;
        // Decompose the linear part into rotation · scale: the repeater
        // composes only rotations and axis-aligned scales, so it is exactly
        // representable as a Lottie transform with anchor 0.
        let [a0, a1, a2, a3, tx, ty] = m.0;
        let phi = a1.atan2(a0);
        let sx = a0.hypot(a1);
        let sy = a2.hypot(a3);
        let opacity = if copies > 1 {
            so + (eo - so) * (k as f64 / (copies - 1) as f64)
        } else {
            so
        };
        let mut it = head.to_vec();
        it.push(GraphicElement::Transform {
            name: None,
            hidden: false,
            p: Some(static_prop(vec![tx, ty])),
            a: Some(static_prop(vec![0.0, 0.0])),
            s: Some(static_prop(vec![sx * 100.0, sy * 100.0])),
            r: Some(static_prop_num(phi.to_degrees())),
            o: Some(static_prop_num(opacity)),
            sk: None,
            sa: None,
        });
        out.push(GraphicElement::Group {
            name: None,
            hidden: false,
            it,
            np: None,
            cix: None,
            bm: None,
            ix: None,
            match_name: None,
        });
    }
    // `m=1` splices copy 0 at the top of the list; `m=2` fills from the
    // other end. `it` is top-first, so sequential is already in order and
    // simultaneous reverses.
    if !sequential {
        out.reverse();
    }
    out.extend_from_slice(&items[at + 1..]);
    Some(out)
}

fn static_prop(v: Vec<f64>) -> Property {
    Property::Static(StaticProperty {
        animated: None,
        value: serde_json::Value::Array(v.into_iter().map(serde_json::Value::from).collect()),
        ix: None,
        x: None,
    })
}

fn static_prop_num(v: f64) -> Property {
    Property::Static(StaticProperty {
        animated: None,
        value: serde_json::Value::from(v),
        ix: None,
        x: None,
    })
}

use super::property::StaticProperty;

#[cfg(test)]
mod tests {
    use super::*;

    /// A rotation-only repeater: copy `k` turns `k · r` degrees about the
    /// anchor, exactly the cumulative `rMatrix` lottie-web builds.
    #[test]
    fn rotation_repeater_places_copies_on_a_circle() {
        let rp = GraphicElement::Repeater {
            name: None,
            hidden: false,
            c: num(20.0),
            o: num(0.0),
            m: Some(1),
            tr: RepeatTransform {
                ty: None,
                name: None,
                p: Some(vec2([0.0, 0.0])),
                a: Some(vec2([0.0, 0.0])),
                s: Some(vec2([100.0, 100.0])),
                r: Some(num(48.0)),
                so: Some(num(100.0)),
                eo: Some(num(100.0)),
            },
        };
        let marker = GraphicElement::Path {
            name: None,
            hidden: false,
            d: None,
            closed: None,
            ks: Property::Static(StaticProperty {
                animated: None,
                value: serde_json::Value::from(0.0),
                ix: None,
                x: None,
            }),
        };
        let items = vec![marker.clone(), rp];
        let out = expand(&items, 1).expect("static repeater expands");
        assert_eq!(out.len(), 20, "the original is replaced by the copies");
        for (k, copy) in out.iter().enumerate() {
            let GraphicElement::Group { it, .. } = copy else {
                panic!("copy {k} is a group");
            };
            let GraphicElement::Transform { p, r, .. } = it.last().unwrap() else {
                panic!("copy {k} carries its transform");
            };
            let rot = match r {
                Some(Property::Static(s)) => s.value.as_f64().unwrap(),
                _ => panic!("static"),
            };
            // The decomposed angle wraps to (−180°, 180°]; compare mod 360.
            let want = (48.0 * k as f64) % 360.0;
            let got = rot.rem_euclid(360.0);
            assert!(
                (got - want).abs() < 1e-9,
                "copy {k} at {rot}°, want {want}°"
            );
            assert!(matches!(p, Some(Property::Static(_))));
        }
    }

    fn num(v: f64) -> Property {
        Property::Static(StaticProperty {
            animated: None,
            value: serde_json::Value::from(v),
            ix: None,
            x: None,
        })
    }

    fn vec2(v: [f64; 2]) -> Property {
        Property::Static(StaticProperty {
            animated: None,
            value: serde_json::Value::Array(
                v.iter().map(|x| serde_json::Value::from(*x)).collect(),
            ),
            ix: None,
            x: None,
        })
    }
}
