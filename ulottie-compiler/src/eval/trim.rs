//! Trim-path evaluation.
//!
//! A direct counterpart of `runtime/trim.js` — same sampling density, same
//! bracketing, same sub-curve split — so a trim the planner resolves at compile
//! time is bit-comparable with one the runtime resolves at playback.
//!
//! When a shape's geometry *and* its trim range are both static, the planner
//! evaluates this once and writes the result straight into the markup: the
//! animation then needs no trim code, no path serializer and no binding at all.

/// Flat bezier path: `v`/`i`/`o` are `[x0, y0, x1, y1, …]`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Flat {
    pub v: Vec<f64>,
    pub i: Vec<f64>,
    pub o: Vec<f64>,
    pub c: bool,
}

/// Arc-length samples per cubic segment. Matches `SAMPLES` in trim.js.
const SAMPLES: usize = 30;

struct Seg {
    len: f64,
    dist: [f64; SAMPLES + 1],
    p: [f64; 8],
}

struct Table {
    segs: Vec<Seg>,
    total: f64,
    closed: bool,
}

fn table(path: &Flat) -> Option<Table> {
    let n = path.v.len() / 2;
    if n < 2 {
        return None;
    }
    let seg_count = if path.c { n } else { n - 1 };
    let tan = |src: &Vec<f64>, idx: usize| -> f64 { src.get(idx).copied().unwrap_or(0.0) };
    let mut segs = Vec::with_capacity(seg_count);
    let mut total = 0.0;
    for s in 0..seg_count {
        let a = s * 2;
        let b = ((s + 1) % n) * 2;
        let p0 = (path.v[a], path.v[a + 1]);
        let p3 = (path.v[b], path.v[b + 1]);
        let p1 = (p0.0 + tan(&path.o, a), p0.1 + tan(&path.o, a + 1));
        let p2 = (p3.0 + tan(&path.i, b), p3.1 + tan(&path.i, b + 1));
        let mut dist = [0.0f64; SAMPLES + 1];
        let (mut cum, mut px, mut py) = (0.0, p0.0, p0.1);
        // `k` is the curve parameter as well as the index, so iterating the
        // slice instead would not remove the arithmetic.
        #[allow(clippy::needless_range_loop)]
        for k in 1..=SAMPLES {
            let t = k as f64 / SAMPLES as f64;
            let u = 1.0 - t;
            let (u3, u2t, ut2, t3) =
                (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
            let x = u3 * p0.0 + u2t * p1.0 + ut2 * p2.0 + t3 * p3.0;
            let y = u3 * p0.1 + u2t * p1.1 + ut2 * p2.1 + t3 * p3.1;
            cum += ((x - px).powi(2) + (y - py).powi(2)).sqrt();
            dist[k] = cum;
            px = x;
            py = y;
        }
        total += cum;
        segs.push(Seg { len: cum, dist, p: [p0.0, p0.1, p1.0, p1.1, p2.0, p2.1, p3.0, p3.1] });
    }
    Some(Table { segs, total, closed: path.c })
}

/// Outcome of trimming a path with a constant range.
pub enum Trimmed {
    /// The range covers the whole path — render it unchanged.
    Whole,
    /// The range is empty — the shape draws nothing.
    Empty,
    Path(Flat),
}

/// Trim `path` to `[start, end]` (percent) rotated by `offset` (degrees),
/// using the same normalization as the runtime's shape binder.
pub fn trim(path: &Flat, start: f64, end: f64, offset: f64) -> Trimmed {
    let (s, e) = (start / 100.0, end / 100.0);
    let (lo, hi) = if s < e { (s, e) } else { (e, s) };
    let vis = hi - lo;
    if vis <= 0.0 {
        return Trimmed::Empty;
    }
    if vis >= 1.0 {
        return Trimmed::Whole;
    }
    let Some(tab) = table(path) else { return Trimmed::Whole };
    if tab.total == 0.0 {
        return Trimmed::Empty;
    }
    let off = offset / 360.0;
    let (mut a, mut b) = (lo + off, hi + off);
    let out = if tab.closed {
        let floor = a.floor();
        a -= floor;
        b -= floor;
        if b > 1.0 {
            concat(cut(&tab, a, 1.0), cut(&tab, 0.0, b - 1.0))
        } else {
            cut(&tab, a, b)
        }
    } else {
        let a = a.clamp(0.0, 1.0);
        let b = b.clamp(0.0, 1.0);
        if b <= a {
            return Trimmed::Empty;
        }
        cut(&tab, a, b)
    };
    if out.v.is_empty() {
        Trimmed::Empty
    } else {
        Trimmed::Path(out)
    }
}

fn concat(x: Flat, y: Flat) -> Flat {
    if x.v.is_empty() {
        return y;
    }
    if y.v.is_empty() {
        return x;
    }
    Flat {
        v: [x.v, y.v].concat(),
        i: [x.i, y.i].concat(),
        o: [x.o, y.o].concat(),
        c: false,
    }
}

fn cut(tab: &Table, af: f64, bf: f64) -> Flat {
    let a_loc = locate(tab, af * tab.total);
    let b_loc = locate(tab, bf * tab.total);
    let mut out = Flat { v: vec![], i: vec![], o: vec![], c: false };

    if a_loc.0 == b_loc.0 {
        let p = between(&tab.segs[a_loc.0].p, a_loc.1, b_loc.1);
        out.v.extend([p[0], p[1], p[6], p[7]]);
        out.i.extend([0.0, 0.0, p[4] - p[6], p[5] - p[7]]);
        out.o.extend([p[2] - p[0], p[3] - p[1], 0.0, 0.0]);
        return out;
    }

    let head = between(&tab.segs[a_loc.0].p, a_loc.1, 1.0);
    out.v.extend([head[0], head[1]]);
    out.i.extend([0.0, 0.0]);
    out.o.extend([head[2] - head[0], head[3] - head[1]]);
    let (mut px, mut py, mut ex, mut ey) = (head[4], head[5], head[6], head[7]);

    for s in (a_loc.0 + 1)..b_loc.0 {
        let p = &tab.segs[s].p;
        out.v.extend([p[0], p[1]]);
        out.i.extend([px - ex, py - ey]);
        out.o.extend([p[2] - p[0], p[3] - p[1]]);
        px = p[4];
        py = p[5];
        ex = p[6];
        ey = p[7];
    }

    let tail = between(&tab.segs[b_loc.0].p, 0.0, b_loc.1);
    out.v.extend([tail[0], tail[1]]);
    out.i.extend([px - ex, py - ey]);
    out.o.extend([tail[2] - tail[0], tail[3] - tail[1]]);
    out.v.extend([tail[6], tail[7]]);
    out.i.extend([tail[4] - tail[6], tail[5] - tail[7]]);
    out.o.extend([0.0, 0.0]);

    out
}

fn locate(tab: &Table, dist: f64) -> (usize, f64) {
    let mut acc = 0.0;
    let last = tab.segs.len() - 1;
    for (s, seg) in tab.segs.iter().enumerate() {
        if dist <= acc + seg.len || s == last {
            let local = (dist - acc).max(0.0);
            let (mut lo, mut hi) = (0usize, SAMPLES);
            while lo < hi {
                let m = (lo + hi) / 2;
                if seg.dist[m] < local {
                    lo = m + 1;
                } else {
                    hi = m;
                }
            }
            let up = lo;
            let low = up.saturating_sub(1);
            let (dl, dh) = (seg.dist[low], seg.dist[up]);
            let f = if dh == dl { 0.0 } else { (local - dl) / (dh - dl) };
            return (s, ((low as f64 + f) / SAMPLES as f64).clamp(0.0, 1.0));
        }
        acc += seg.len;
    }
    (last, 1.0)
}

/// Sub-curve of a cubic between parameters `a` and `b`.
fn between(p: &[f64; 8], a: f64, b: f64) -> [f64; 8] {
    let left = split(p, b).0;
    split(&left, if b == 0.0 { 0.0 } else { a / b }).1
}

fn split(p: &[f64; 8], t: f64) -> ([f64; 8], [f64; 8]) {
    let u = 1.0 - t;
    let a01 = (u * p[0] + t * p[2], u * p[1] + t * p[3]);
    let a12 = (u * p[2] + t * p[4], u * p[3] + t * p[5]);
    let a23 = (u * p[4] + t * p[6], u * p[5] + t * p[7]);
    let b01 = (u * a01.0 + t * a12.0, u * a01.1 + t * a12.1);
    let b12 = (u * a12.0 + t * a23.0, u * a12.1 + t * a23.1);
    let c = (u * b01.0 + t * b12.0, u * b01.1 + t * b12.1);
    (
        [p[0], p[1], a01.0, a01.1, b01.0, b01.1, c.0, c.1],
        [c.0, c.1, b12.0, b12.1, a23.0, a23.1, p[6], p[7]],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square() -> Flat {
        Flat {
            v: vec![0.0, 0.0, 100.0, 0.0, 100.0, 100.0, 0.0, 100.0],
            i: vec![0.0; 8],
            o: vec![0.0; 8],
            c: true,
        }
    }

    #[test]
    fn a_full_range_is_left_untouched() {
        assert!(matches!(trim(&square(), 0.0, 100.0, 0.0), Trimmed::Whole));
    }

    #[test]
    fn an_empty_range_draws_nothing() {
        assert!(matches!(trim(&square(), 40.0, 40.0, 0.0), Trimmed::Empty));
    }

    #[test]
    fn half_a_square_walks_two_of_its_four_sides() {
        let Trimmed::Path(p) = trim(&square(), 0.0, 50.0, 0.0) else {
            panic!("expected a trimmed path");
        };
        // Starts at the origin and ends halfway round the perimeter.
        assert!((p.v[0] - 0.0).abs() < 1e-6 && (p.v[1] - 0.0).abs() < 1e-6);
        let n = p.v.len();
        assert!((p.v[n - 2] - 100.0).abs() < 0.5, "end x = {}", p.v[n - 2]);
        assert!((p.v[n - 1] - 100.0).abs() < 0.5, "end y = {}", p.v[n - 1]);
        assert!(!p.c, "a trimmed path is always open");
    }

    #[test]
    fn the_range_is_orientation_independent() {
        let a = trim(&square(), 10.0, 60.0, 0.0);
        let b = trim(&square(), 60.0, 10.0, 0.0);
        match (a, b) {
            (Trimmed::Path(x), Trimmed::Path(y)) => assert_eq!(x, y),
            _ => panic!("expected two trimmed paths"),
        }
    }
}
