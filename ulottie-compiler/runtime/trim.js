// Trim-path modifier.
//
// Splitting a path at an arc-length fraction needs a per-segment length table.
// That table depends only on the geometry, so when the source path is static
// (the common case — a traced outline being drawn on) it is built once at
// mount and reused for the whole animation.

const SAMPLES = 30;

/** Build the per-segment arc-length table for a flat path. */
export function trimTable(p) {
  const v = p.v, ti = p.i, to = p.o;
  const n = v.length >> 1;
  const segCount = p.c ? n : n - 1;
  if (n < 2) return null;
  const segs = new Array(segCount);
  let total = 0;
  for (let s = 0; s < segCount; s++) {
    const a = s * 2, b = ((s + 1) % n) * 2;
    const p0x = v[a], p0y = v[a + 1], p3x = v[b], p3y = v[b + 1];
    const p1x = p0x + (to ? to[a] : 0), p1y = p0y + (to ? to[a + 1] : 0);
    const p2x = p3x + (ti ? ti[b] : 0), p2y = p3y + (ti ? ti[b + 1] : 0);
    const dist = new Float64Array(SAMPLES + 1);
    let cum = 0, px = p0x, py = p0y;
    for (let k = 1; k <= SAMPLES; k++) {
      const t = k / SAMPLES, u = 1 - t;
      const u3 = u * u * u, u2t = 3 * u * u * t, ut2 = 3 * u * t * t, t3 = t * t * t;
      const x = u3 * p0x + u2t * p1x + ut2 * p2x + t3 * p3x;
      const y = u3 * p0y + u2t * p1y + ut2 * p2y + t3 * p3y;
      cum += Math.hypot(x - px, y - py);
      dist[k] = cum;
      px = x; py = y;
    }
    segs[s] = { len: cum, dist, p: [p0x, p0y, p1x, p1y, p2x, p2y, p3x, p3y] };
    total += cum;
  }
  return { segs, total, closed: !!p.c };
}

/**
 * Trim `tab` to the arc-length range `[lo, hi]` (fractions of total length),
 * rotated by `offset` turns. Returns a flat open path.
 */
export function trimApply(tab, lo, hi, offset) {
  if (!tab || tab.total === 0) return { v: [], i: null, o: null, c: 0 };
  if (hi - lo >= 1) return null;   // caller keeps the untrimmed path
  let a = lo + offset, b = hi + offset;
  if (tab.closed) {
    const floor = Math.floor(a);
    a -= floor; b -= floor;
    if (b > 1) return tmConcat(tmCut(tab, a, 1), tmCut(tab, 0, b - 1));
    return tmCut(tab, a, b);
  }
  a = a < 0 ? 0 : a > 1 ? 1 : a;
  b = b < 0 ? 0 : b > 1 ? 1 : b;
  if (b <= a) return { v: [], i: null, o: null, c: 0 };
  return tmCut(tab, a, b);
}

function tmConcat(x, y) {
  if (!x.v.length) return y;
  if (!y.v.length) return x;
  return { v: x.v.concat(y.v), i: x.i.concat(y.i), o: x.o.concat(y.o), c: 0 };
}

function tmCut(tab, af, bf) {
  const aLoc = tmLocate(tab, af * tab.total);
  const bLoc = tmLocate(tab, bf * tab.total);
  const v = [], i = [], o = [];

  if (aLoc.s === bLoc.s) {
    const p = tmBetween(tab.segs[aLoc.s].p, aLoc.t, bLoc.t);
    v.push(p[0], p[1], p[6], p[7]);
    i.push(0, 0, p[4] - p[6], p[5] - p[7]);
    o.push(p[2] - p[0], p[3] - p[1], 0, 0);
    return { v, i, o, c: 0 };
  }

  const head = tmBetween(tab.segs[aLoc.s].p, aLoc.t, 1);
  v.push(head[0], head[1]);
  i.push(0, 0);
  o.push(head[2] - head[0], head[3] - head[1]);
  let px = head[4], py = head[5], ex = head[6], ey = head[7];

  for (let s = aLoc.s + 1; s < bLoc.s; s++) {
    const p = tab.segs[s].p;
    v.push(p[0], p[1]);
    i.push(px - ex, py - ey);
    o.push(p[2] - p[0], p[3] - p[1]);
    px = p[4]; py = p[5]; ex = p[6]; ey = p[7];
  }

  const tail = tmBetween(tab.segs[bLoc.s].p, 0, bLoc.t);
  v.push(tail[0], tail[1]);
  i.push(px - ex, py - ey);
  o.push(tail[2] - tail[0], tail[3] - tail[1]);
  v.push(tail[6], tail[7]);
  i.push(tail[4] - tail[6], tail[5] - tail[7]);
  o.push(0, 0);

  return { v, i, o, c: 0 };
}

function tmLocate(tab, dist) {
  let acc = 0;
  const segs = tab.segs;
  for (let s = 0; s < segs.length; s++) {
    const seg = segs[s];
    if (dist <= acc + seg.len || s === segs.length - 1) {
      const local = Math.max(0, dist - acc);
      const d = seg.dist;
      let lo = 0, hi = SAMPLES;
      while (lo < hi) {
        const m = (lo + hi) >> 1;
        if (d[m] < local) lo = m + 1; else hi = m;
      }
      const up = lo, low = up > 0 ? up - 1 : 0;
      const dl = d[low], dh = d[up];
      const f = dh === dl ? 0 : (local - dl) / (dh - dl);
      const t = (low + f) / SAMPLES;
      return { s, t: t < 0 ? 0 : t > 1 ? 1 : t };
    }
    acc += seg.len;
  }
  return { s: segs.length - 1, t: 1 };
}

/** Sub-curve of a cubic between parameters `a` and `b`. */
function tmBetween(p, a, b) {
  const left = tmSplit(p, b).left;
  return tmSplit(left, b === 0 ? 0 : a / b).right;
}

function tmSplit(p, t) {
  const u = 1 - t;
  const a01x = u * p[0] + t * p[2], a01y = u * p[1] + t * p[3];
  const a12x = u * p[2] + t * p[4], a12y = u * p[3] + t * p[5];
  const a23x = u * p[4] + t * p[6], a23y = u * p[5] + t * p[7];
  const b01x = u * a01x + t * a12x, b01y = u * a01y + t * a12y;
  const b12x = u * a12x + t * a23x, b12y = u * a12y + t * a23y;
  const cx = u * b01x + t * b12x, cy = u * b01y + t * b12y;
  return {
    left: [p[0], p[1], a01x, a01y, b01x, b01y, cx, cy],
    right: [cx, cy, b12x, b12y, a23x, a23y, p[6], p[7]],
  };
}
