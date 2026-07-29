// Spatial bezier motion paths.
//
// A Lottie position keyframe can carry in/out tangents that bend the path
// between two values, and the result is parameterized by arc length so the
// motion is evenly paced. Both endpoints and both tangents are constants for a
// given segment, so the sample table is built once and reused.
//
// The split is deliberate: `spBuild` takes plain numbers and runs once per
// segment, `spSample` runs per frame and touches nothing but the table it is
// given. That keeps it usable from both callers — the interpreter, which has
// its values in an `Int32Array` and materializes four small arrays on a
// segment's first visit, and generated code, which knows the endpoints at
// compile time and can build the table when the module loads.

const SP_SEG = 200;

/**
 * Arc-length sample table for one segment.
 *
 * @param {ArrayLike<number>} a start value, `d` components
 * @param {ArrayLike<number>} b end value
 * @param {ArrayLike<number>} to out-tangent, relative to `a`
 * @param {ArrayLike<number>} ti in-tangent, relative to `b`
 */
export function spBuild(a, b, to, ti, d) {
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
      const p0 = a[j];
      const p3 = b[j];
      const p1 = p0 + to[j];
      const p2 = p3 + ti[j];
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
  return { pts, cum, total, d };
}

/** Sample a built table at `u`, by arc length, into `out`. */
export function spSample(tab, u, out) {
  const { pts, cum, total, d } = tab;
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

/**
 * Build a segment's table straight from the payload stream.
 *
 * Lives here rather than in the interpolator so it is gated with the rest of
 * the motion-path code: inline in `keyframed`, these four little arrays cost
 * every keyframed animation ~135 bytes for a branch it never takes.
 */
export function spSeg(S, ai, bi, so, si, d, iv) {
  const a = [], b = [], to = [], ti = [];
  for (let k = 0; k < d; k++) {
    a.push(S[ai + k] * iv);
    b.push(S[bi + k] * iv);
    to.push(S[so + k] * iv);
    ti.push(S[si + k] * iv);
  }
  return spBuild(a, b, to, ti, d);
}
