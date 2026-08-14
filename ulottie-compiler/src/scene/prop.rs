//! Wire encoding for the properties that survive AOT analysis.
//!
//! Anything the planner could evaluate at compile time never reaches this
//! module — it was baked into the markup. What's left is genuinely
//! time-varying, so the encoding optimizes for two things: bytes, and how
//! cheaply the runtime can turn it into a specialized closure.
//!
//! Discriminating a `Prop` in JS costs one check, once, at mount:
//!
//! ```text
//! number        → static scalar
//! Array         → static vector
//! obj.t         → keyframed
//! obj.x         → expression
//! otherwise     → static path {v,i,o,c}
//! ```

use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Serialize, Serializer};

use super::svg::{FlatPath, Num};

/// Easing handle pair `[outX, outY, inX, inY]`, interned across the module.
pub type Easing = [f64; 4];

/// The linear handle. Interned at index 0 so the runtime can skip the bezier
/// solve with a single `=== 0` check.
pub const LINEAR: Easing = [0.0, 0.0, 1.0, 1.0];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimKind {
    Scalar = 0,
    Vector = 1,
    Path = 2,
}

#[derive(Debug, Clone)]
pub enum Prop {
    Scalar(f64),
    Vector(Vec<f64>),
    Path(FlatPath),
    Anim(Box<Anim>),
    Expr {
        id: u32,
        /// Value source the expression reads through `value` / `thisProperty`.
        fallback: Option<Box<Prop>>,
        /// Layer this property belongs to, as an index into the scene's layer
        /// table — this is what `thisLayer` resolves to inside the expression.
        layer: Option<u32>,
    },
}

impl Prop {
    /// Whether this is a compile-time constant exactly equal to `want`.
    ///
    /// Used to drop wire entries the runtime would default to the same value.
    /// Only a literal scalar or vector can match: anything keyframed or
    /// expression-driven varies, whatever it happens to equal at frame 0.
    /// Comparison is against the unrounded value, so a property that merely
    /// *serializes* to the default is kept — conservative on purpose.
    pub fn is_exactly(&self, want: &[f64]) -> bool {
        match self {
            Prop::Scalar(v) => want.len() == 1 && want[0] == *v,
            Prop::Vector(v) => v.as_slice() == want,
            _ => false,
        }
    }

    pub fn is_static(&self) -> bool {
        matches!(self, Prop::Scalar(_) | Prop::Vector(_) | Prop::Path(_))
    }

    pub fn as_scalar(&self) -> Option<f64> {
        match self {
            Prop::Scalar(v) => Some(*v),
            Prop::Vector(v) => v.first().copied(),
            _ => None,
        }
    }

    pub fn as_vec(&self) -> Option<&[f64]> {
        match self {
            Prop::Vector(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_path(&self) -> Option<&FlatPath> {
        match self {
            Prop::Path(p) => Some(p),
            _ => None,
        }
    }
}

/// A keyframed property in columnar form. `v` is flat with stride `dim` so the
/// runtime can hold it in a `Float64Array` and interpolate without chasing
/// nested array pointers.
#[derive(Debug, Clone)]
pub struct Anim {
    pub kind: AnimKind,
    pub dim: usize,
    /// Keyframe times, ascending.
    pub t: Vec<f64>,
    /// Values at each time — flat for scalar/vector kinds.
    pub v: Vec<f64>,
    /// Path values, used only when `kind == Path`.
    pub paths: Vec<FlatPath>,
    /// Per-segment easing index into the module easing table. `None` when
    /// every segment is linear.
    pub ez: Option<Vec<u32>>,
    /// Per-segment hold flags, packed as 0/1. `None` when nothing is held.
    pub hold: Option<Vec<u8>>,
    /// Spatial bezier tangents, flat with stride `dim`. `None` when all zero.
    pub to: Option<Vec<f64>>,
    pub ti: Option<Vec<f64>>,
}

impl Anim {
    pub fn segments(&self) -> usize {
        self.t.len().saturating_sub(1)
    }
}

// ---------------------------------------------------------------------------
// Serialization
// ---------------------------------------------------------------------------

fn qs<S: Serializer>(vals: &[f64], s: S) -> Result<S::Ok, S::Error> {
    let mut seq = s.serialize_seq(Some(vals.len()))?;
    for v in vals {
        seq.serialize_element(&Num(*v))?;
    }
    seq.end()
}

struct Quantized<'a>(&'a [f64]);

impl Serialize for Quantized<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        qs(self.0, s)
    }
}

impl Serialize for FlatPath {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        // `i`/`o` are omitted when every tangent is zero — polygonal paths are
        // common (rectangles, polystars, traced outlines) and this halves them.
        let poly = self.i.iter().all(|x| *x == 0.0) && self.o.iter().all(|x| *x == 0.0);
        let mut n = 1;
        if !poly {
            n += 2;
        }
        if self.c {
            n += 1;
        }
        let mut m = s.serialize_map(Some(n))?;
        m.serialize_entry("v", &Quantized(&self.v))?;
        if !poly {
            m.serialize_entry("i", &Quantized(&self.i))?;
            m.serialize_entry("o", &Quantized(&self.o))?;
        }
        if self.c {
            m.serialize_entry("c", &1u8)?;
        }
        m.end()
    }
}

impl Serialize for Anim {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut n = 2;
        if self.kind != AnimKind::Scalar {
            n += 1;
        }
        if self.dim > 1 {
            n += 1;
        }
        for present in [
            self.ez.is_some(),
            self.hold.is_some(),
            self.to.is_some(),
        ] {
            if present {
                n += 1;
            }
        }
        if self.ti.is_some() {
            n += 1;
        }

        let mut m = s.serialize_map(Some(n))?;
        m.serialize_entry("t", &Quantized(&self.t))?;
        match self.kind {
            AnimKind::Path => m.serialize_entry("v", &self.paths)?,
            _ => m.serialize_entry("v", &Quantized(&self.v))?,
        }
        if self.kind != AnimKind::Scalar {
            m.serialize_entry("k", &(self.kind as u8))?;
        }
        if self.dim > 1 {
            m.serialize_entry("d", &self.dim)?;
        }
        if let Some(z) = &self.ez {
            m.serialize_entry("z", z)?;
        }
        if let Some(h) = &self.hold {
            m.serialize_entry("h", h)?;
        }
        if let Some(to) = &self.to {
            m.serialize_entry("to", &Quantized(to))?;
        }
        if let Some(ti) = &self.ti {
            m.serialize_entry("ti", &Quantized(ti))?;
        }
        m.end()
    }
}

impl Serialize for Prop {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Prop::Scalar(v) => Num(*v).serialize(s),
            Prop::Vector(v) => Quantized(v).serialize(s),
            Prop::Path(p) => p.serialize(s),
            Prop::Anim(a) => a.serialize(s),
            Prop::Expr {
                id,
                fallback,
                layer,
            } => {
                let n = 1 + fallback.is_some() as usize + layer.is_some() as usize;
                let mut m = s.serialize_map(Some(n))?;
                m.serialize_entry("x", id)?;
                if let Some(fb) = fallback {
                    m.serialize_entry("f", fb)?;
                }
                if let Some(l) = layer {
                    m.serialize_entry("l", l)?;
                }
                m.end()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(p: &Prop) -> String {
        serde_json::to_string(p).unwrap()
    }

    #[test]
    fn static_values_serialize_bare() {
        assert_eq!(json(&Prop::Scalar(100.0)), "100");
        assert_eq!(json(&Prop::Vector(vec![256.0, 256.0])), "[256,256]");
    }

    #[test]
    fn polygonal_paths_drop_their_zero_tangents() {
        let p = Prop::Path(FlatPath {
            v: vec![0.0, 0.0, 1.0, 1.0],
            i: vec![0.0; 4],
            o: vec![0.0; 4],
            c: true,
        });
        assert_eq!(json(&p), r#"{"v":[0,0,1,1],"c":1}"#);
    }

    #[test]
    fn curved_paths_keep_their_tangents() {
        let p = Prop::Path(FlatPath {
            v: vec![0.0, 0.0, 1.0, 1.0],
            i: vec![0.0, 0.0, -0.5, 0.0],
            o: vec![0.5, 0.0, 0.0, 0.0],
            c: false,
        });
        assert_eq!(
            json(&p),
            r#"{"v":[0,0,1,1],"i":[0,0,-0.5,0],"o":[0.5,0,0,0]}"#
        );
    }

    #[test]
    fn scalar_animation_omits_kind_and_dim() {
        let a = Prop::Anim(Box::new(Anim {
            kind: AnimKind::Scalar,
            dim: 1,
            t: vec![0.0, 10.0],
            v: vec![0.0, 100.0],
            paths: vec![],
            ez: None,
            hold: None,
            to: None,
            ti: None,
        }));
        assert_eq!(json(&a), r#"{"t":[0,10],"v":[0,100]}"#);
    }
}
