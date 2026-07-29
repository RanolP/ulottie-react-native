//! Compact value formatting for baked markup and payload numbers.
//!
//! Everything the compiler can decide ahead of time ends up as literal text via
//! this module, so the runtime never formats a static value. Formatting is
//! tuned for bytes: numbers are quantized and stripped of redundant zeroes,
//! colours use hex, and attributes equal to the SVG default are dropped by the
//! caller rather than written.

/// Placeholder the runtime substitutes with a per-mount suffix, so two mounts
/// of the same module don't collide on `id`s.
///
/// Plain ASCII rather than a control character: the same markup is written to
/// standalone `.svg` sprites, and XML 1.0 forbids C0 controls — a marker that
/// only worked inside a JS string would make those files invalid.
pub const ID_MARK: &str = "--u";

/// Marker for an id defined *inside* a precomp body. Those bodies are cloned
/// per use, so every clone needs its own id — this gets a per-clone suffix
/// where [`ID_MARK`] gets a per-mount one.
pub const CLONE_MARK: &str = "--c";

/// Quantize to 3 decimals. SVG user units on a ≤2048px viewBox resolve far
/// above this, so the error is invisible while the strings get materially
/// shorter.
pub fn q(x: f64) -> f64 {
    let v = (x * 1000.0).round() / 1000.0;
    // Normalize -0.0 to 0.0 so it prints as "0" rather than "-0".
    if v == 0.0 { 0.0 } else { v }
}

/// Quantized number that serializes as an integer whenever it is one.
///
/// `serde_json` always writes an f64 with a fractional part (`0` → `0.0`), and
/// most coordinates in a Lottie file are whole numbers — so this is worth two
/// bytes apiece across the entire payload, and it keeps the output tight even
/// when the minifier is disabled.
#[derive(Debug, Clone, Copy)]
pub struct Num(pub f64);

impl serde::Serialize for Num {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let v = q(self.0);
        if v.fract() == 0.0 && v.abs() < 9.007199254740992e15 {
            s.serialize_i64(v as i64)
        } else {
            s.serialize_f64(v)
        }
    }
}

/// Format a number as compactly as SVG/JS will accept: quantized, no trailing
/// zeroes, leading zero of a fraction dropped (`0.5` → `.5`).
pub fn n(x: f64) -> String {
    nd(x, 1000.0)
}

/// Same, at an explicit scale (`10^decimals`).
pub fn nd(x: f64, scale: f64) -> String {
    let v = (x * scale).round() / scale;
    let v = if v == 0.0 { 0.0 } else { v };
    if !v.is_finite() {
        return "0".into();
    }
    let mut s = format!("{v}");
    if let Some(rest) = s.strip_prefix("0.") {
        s = format!(".{rest}");
    } else if let Some(rest) = s.strip_prefix("-0.") {
        s = format!("-.{rest}");
    }
    s
}

/// A transform's linear part scales every coordinate under it, so its error
/// budget is `quantum × extent` — at a 1000px extent, 3 decimals is already
/// visible. The translation part contributes absolute error only, so it can be
/// far coarser. Splitting the two is both smaller and ~5x more accurate than
/// quantizing all six uniformly.
/// The shortest spelling of a transform.
///
/// `matrix(1,0,0,1,x,y)` is `translate(x,y)` — same transform, five bytes less,
/// and the one an author would have written. A pure translation is the most
/// common transform in a Lottie file by a wide margin, so this is not a corner
/// case being tidied: it is the default case no longer paying for generality it
/// does not use.
///
/// Only for values the compiler bakes and the runtime never rewrites. An
/// attribute a binding also writes has to keep the binder's spelling, or the
/// two disagree about a value they agree on — see `scene::bake`.
pub fn transform_str(m: &[f64; 6]) -> String {
    if m[0] == 1.0 && m[1] == 0.0 && m[2] == 0.0 && m[3] == 1.0 {
        return format!("translate({},{})", nd(m[4], 100.0), nd(m[5], 100.0));
    }
    matrix_str(m)
}

pub fn matrix_str(m: &[f64; 6]) -> String {
    format!(
        "matrix({},{},{},{},{},{})",
        nd(m[0], 1e5),
        nd(m[1], 1e5),
        nd(m[2], 1e5),
        nd(m[3], 1e5),
        nd(m[4], 100.0),
        nd(m[5], 100.0)
    )
}

/// Lottie stores colour channels as 0..=1 floats. SVG wants 0..=255 ints.
pub fn channel(c: f64) -> u8 {
    (c.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// `[r, g, b]` in 0..=1 → `#rrggbb`, collapsed to the 3-digit form when every
/// channel is a repeated nibble.
pub fn hex_color(c: &[f64]) -> String {
    let r = channel(c.first().copied().unwrap_or(0.0));
    let g = channel(c.get(1).copied().unwrap_or(0.0));
    let b = channel(c.get(2).copied().unwrap_or(0.0));
    if r >> 4 == r & 15 && g >> 4 == g & 15 && b >> 4 == b & 15 {
        format!("#{:x}{:x}{:x}", r >> 4, g >> 4, b >> 4)
    } else {
        format!("#{r:02x}{g:02x}{b:02x}")
    }
}

/// Combined alpha for a paint: the colour's own alpha times the style opacity
/// (0..=100). Returns `None` when fully opaque, so the caller can drop the
/// `*-opacity` attribute entirely.
pub fn paint_alpha(color: &[f64], opacity: f64) -> Option<f64> {
    let a = color.get(3).copied().unwrap_or(1.0) * (opacity / 100.0);
    if a >= 1.0 { None } else { Some(a.max(0.0)) }
}

// ---------------------------------------------------------------------------
// Path serialization
// ---------------------------------------------------------------------------

/// A bezier path in the flat wire layout: `v`/`i`/`o` are `[x0,y0,x1,y1,…]`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FlatPath {
    pub v: Vec<f64>,
    pub i: Vec<f64>,
    pub o: Vec<f64>,
    pub c: bool,
}

impl FlatPath {
    pub fn from_parts(v: &[[f64; 2]], i: &[[f64; 2]], o: &[[f64; 2]], c: bool) -> Self {
        Self {
            v: v.iter().flat_map(|p| [p[0], p[1]]).collect(),
            i: i.iter().flat_map(|p| [p[0], p[1]]).collect(),
            o: o.iter().flat_map(|p| [p[0], p[1]]).collect(),
            c,
        }
    }

    pub fn len(&self) -> usize {
        self.v.len() / 2
    }

    pub fn is_empty(&self) -> bool {
        self.v.is_empty()
    }

    /// Serialize to an SVG `d` attribute. Straight segments (both adjacent
    /// tangents at the origin) collapse to `L`, which is both shorter and
    /// cheaper for the rasterizer than a degenerate cubic.
    pub fn to_d(&self) -> String {
        let n_pts = self.len();
        if n_pts == 0 {
            return String::new();
        }
        let mut d = String::with_capacity(n_pts * 16);
        d.push('M');
        push_pair(&mut d, self.v[0], self.v[1]);
        let segs = if self.c { n_pts } else { n_pts - 1 };
        for s in 0..segs {
            let a = s * 2;
            let b = ((s + 1) % n_pts) * 2;
            let (ox, oy) = (self.o[a], self.o[a + 1]);
            let (ix, iy) = (self.i[b], self.i[b + 1]);
            if ox.abs() < 1e-6 && oy.abs() < 1e-6 && ix.abs() < 1e-6 && iy.abs() < 1e-6 {
                d.push('L');
                push_pair(&mut d, self.v[b], self.v[b + 1]);
            } else {
                d.push('C');
                push_pair(&mut d, self.v[a] + ox, self.v[a + 1] + oy);
                push_sep(&mut d, self.v[b] + ix);
                push_pair(&mut d, self.v[b] + ix, self.v[b + 1] + iy);
                push_sep(&mut d, self.v[b]);
                push_pair(&mut d, self.v[b], self.v[b + 1]);
            }
        }
        if self.c {
            d.push('Z');
        }
        d
    }
}

fn push_sep(d: &mut String, next: f64) {
    // A leading '-' is self-delimiting in the SVG path grammar, so the comma
    // is only needed when the next number starts with a digit or '.'.
    if !n(next).starts_with('-') {
        d.push(',');
    }
}

fn push_pair(d: &mut String, x: f64, y: f64) {
    d.push_str(&n(x));
    push_sep(d, y);
    d.push_str(&n(y));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_compact() {
        assert_eq!(n(256.0), "256");
        assert_eq!(n(0.5), ".5");
        assert_eq!(n(-0.25), "-.25");
        assert_eq!(n(256.00000000000003), "256");
        assert_eq!(n(-0.0), "0");
        assert_eq!(n(1.23456), "1.235");
    }

    #[test]
    fn colors_use_hex_and_collapse() {
        assert_eq!(hex_color(&[1.0, 1.0, 1.0, 1.0]), "#fff");
        assert_eq!(hex_color(&[1.0, 0.9804, 0.2824, 1.0]), "#fffa48");
        assert_eq!(hex_color(&[0.0, 0.0, 0.0]), "#000");
    }

    #[test]
    fn opaque_paints_drop_their_opacity_attribute() {
        assert_eq!(paint_alpha(&[0.0, 0.0, 0.0, 1.0], 100.0), None);
        assert_eq!(paint_alpha(&[0.0, 0.0, 0.0, 1.0], 50.0), Some(0.5));
        assert_eq!(paint_alpha(&[0.0, 0.0, 0.0, 0.5], 100.0), Some(0.5));
    }

    #[test]
    fn straight_segments_become_line_commands() {
        let p = FlatPath {
            v: vec![0.0, 0.0, 10.0, 0.0, 10.0, 10.0],
            i: vec![0.0; 6],
            o: vec![0.0; 6],
            c: true,
        };
        assert_eq!(p.to_d(), "M0,0L10,0L10,10L0,0Z");
    }

    #[test]
    fn negative_coordinates_drop_the_separator() {
        let p = FlatPath {
            v: vec![0.0, 0.0, -10.0, -5.0],
            i: vec![0.0; 4],
            o: vec![0.0; 4],
            c: false,
        };
        assert_eq!(p.to_d(), "M0,0L-10-5");
    }
}
