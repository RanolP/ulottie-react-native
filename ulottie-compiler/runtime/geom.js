// Runtime shape geometry.
//
// Only reached for shapes whose parameters are animated — every static
// rectangle, ellipse, star and path was turned into markup by the compiler.
// Exact transcriptions of lottie-web's shape properties, down to its
// `roundCorner = 0.5519` (not the true circle kappa): the goal is to match
// the reference renderer digit for digit.
//
// Each generator writes into a caller-owned path object so a steady-state
// frame allocates nothing. Setting `.length` on the arrays reuses their
// backing storage when the vertex count changes.

const ROUND = 0.5519;

function geoSize(p, n, curved) {
  p.v.length = n * 2;
  if (curved) {
    if (!p.i) { p.i = []; p.o = []; }
    p.i.length = n * 2;
    p.o.length = n * 2;
  } else {
    p.i = p.o = null;
  }
  p.c = 1;
  return p;
}

/** Reverse pairs 1..n-1 of a flat [x,y] array, keeping pair 0 in place. */
function geoRev2(a, n) {
  for (let x = 1, y = n - 1; x < y; x++, y--) {
    let t = a[x * 2]; a[x * 2] = a[y * 2]; a[y * 2] = t;
    t = a[x * 2 + 1]; a[x * 2 + 1] = a[y * 2 + 1]; a[y * 2 + 1] = t;
  }
}

/**
 * The same contour traversed the other way: vertex 0 stays first, the rest
 * reverse, and in/out tangents swap roles. Exactly lottie-web's hand-written
 * reversed constructions, verified corner by corner.
 */
function geoRev(p) {
  const n = p.v.length >> 1;
  geoRev2(p.v, n);
  if (p.i) {
    geoRev2(p.i, n);
    geoRev2(p.o, n);
    const t = p.i; p.i = p.o; p.o = t;
  }
  return p;
}

export function rectPath(out, cx, cy, w, h, rad, dir) {
  const hw = w / 2, hh = h / 2;
  const l = cx - hw, t = cy - hh, ri = cx + hw, b = cy + hh;
  if (!(rad >= 1e-3)) {
    geoSize(out, 4, false);
    const v = out.v;
    v[0] = ri; v[1] = t; v[2] = ri; v[3] = b;
    v[4] = l; v[5] = b; v[6] = l; v[7] = t;
    return dir ? geoRev(out) : out;
  }
  const rr = Math.min(rad, hw, hh), k = rr * ROUND;
  geoSize(out, 8, true);
  const v = out.v, i = out.i, o = out.o;
  const V = [ri, t + rr, ri, b - rr, ri - rr, b, l + rr, b, l, b - rr, l, t + rr, l + rr, t, ri - rr, t];
  const I = [0, -k, 0, 0, k, 0, 0, 0, 0, k, 0, 0, -k, 0, 0, 0];
  const O = [0, 0, 0, k, 0, 0, -k, 0, 0, 0, 0, -k, 0, 0, k, 0];
  for (let j = 0; j < 16; j++) { v[j] = V[j]; i[j] = I[j]; o[j] = O[j]; }
  return dir ? geoRev(out) : out;
}

export function ellipsePath(out, cx, cy, rx, ry, dir) {
  const kx = rx * ROUND, ky = ry * ROUND;
  geoSize(out, 4, true);
  const v = out.v, i = out.i, o = out.o;
  v[0] = cx; v[1] = cy - ry; v[2] = cx + rx; v[3] = cy;
  v[4] = cx; v[5] = cy + ry; v[6] = cx - rx; v[7] = cy;
  i[0] = -kx; i[1] = 0; i[2] = 0; i[3] = -ky;
  i[4] = kx; i[5] = 0; i[6] = 0; i[7] = ky;
  o[0] = kx; o[1] = 0; o[2] = 0; o[3] = ky;
  o[4] = -kx; o[5] = 0; o[6] = 0; o[7] = -ky;
  return dir ? geoRev(out) : out;
}

/** `sy === 1` alternates outer/inner radii (star); anything else is a polygon. */
export function starPath(out, sy, pts, cx, cy, or, ir, rot, os, is, dir) {
  const p = Math.floor(pts);
  if (p < 3) return geoSize(out, 0, false);
  const star = sy === 1;
  const n = star ? p * 2 : p;
  geoSize(out, n, true);
  const v = out.v, ti = out.i, to = out.o;
  const d = dir ? -1 : 1;
  const step = ((Math.PI * 2) / n) * d;
  // Perimeter share per segment: the polygon quarters where the star halves.
  const longSeg = (Math.PI * 2 * or) / (n * (star ? 2 : 4));
  const shortSeg = star ? (Math.PI * 2 * ir) / (n * 2) : longSeg;
  let a = (rot * Math.PI) / 180 - Math.PI / 2;
  let long = true;
  for (let k = 0; k < n; k++) {
    const rr = star && !long ? ir : or;
    const rnd = (star && !long ? is : os) / 100;
    const seg = star && !long ? shortSeg : longSeg;
    const x = rr * Math.cos(a);
    const y = rr * Math.sin(a);
    const len = Math.hypot(x, y);
    const ox = len === 0 ? 0 : y / len;
    const oy = len === 0 ? 0 : -x / len;
    const s = seg * rnd * d;
    v[k * 2] = cx + x;
    v[k * 2 + 1] = cy + y;
    to[k * 2] = -ox * s;
    to[k * 2 + 1] = -oy * s;
    ti[k * 2] = ox * s;
    ti[k * 2 + 1] = oy * s;
    long = !long;
    a += step;
  }
  return out;
}
