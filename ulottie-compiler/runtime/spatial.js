// Spatial bezier motion paths.
//
// A Lottie position keyframe can carry in/out tangents that bend the path
// between two values, and the result is parameterized by arc length so the
// motion is evenly paced. Both endpoints and both tangents are constants for
// a given segment, so the sample table is built once and reused — the previous
// implementation rebuilt 200 samples on every frame.

const SP_SEG = 200;

function spBuild(v, ai, bv, bi, to, ti, so, d) {
  const pts = new Float64Array((SP_SEG + 1) * d);
  const cum = new Float64Array(SP_SEG + 1);
  let total = 0;
  for (let k = 0; k <= SP_SEG; k++) {
    const u = k / SP_SEG;
    const m = 1 - u;
    const c0 = m * m * m, c1 = 3 * m * m * u, c2 = 3 * m * u * u, c3 = u * u * u;
    const base = k * d;
    let dist = 0;
    for (let j = 0; j < d; j++) {
      const p0 = v[ai + j];
      const p3 = bv[bi + j];
      const p1 = p0 + to[so + j];
      const p2 = p3 + ti[so + j];
      const x = c0 * p0 + c1 * p1 + c2 * p2 + c3 * p3;
      pts[base + j] = x;
      if (k) {
        const dd = x - pts[base - d + j];
        dist += dd * dd;
      }
    }
    if (k) total += Math.sqrt(dist);
    cum[k] = total;
  }
  return { pts, cum, total };
}

export function spatial(v, ai, bv, bi, to, ti, so, d, u, out, cache, seg) {
  let tab = cache[seg];
  if (!tab) tab = cache[seg] = spBuild(v, ai, bv, bi, to, ti, so, d);
  const { pts, cum, total } = tab;
  if (total === 0) {
    for (let j = 0; j < d; j++) out[j] = pts[j];
    return out;
  }
  const target = u * total;
  let lo = 0, hi = SP_SEG;
  while (hi - lo > 1) {
    const m = (lo + hi) >> 1;
    if (cum[m] <= target) lo = m; else hi = m;
  }
  const span = cum[hi] - cum[lo];
  const f = span > 0 ? (target - cum[lo]) / span : 0;
  const a = lo * d, b = hi * d;
  for (let j = 0; j < d; j++) {
    const x = pts[a + j];
    out[j] = x + (pts[b + j] - x) * f;
  }
  return out;
}
