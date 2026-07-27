// Runtime shape geometry.
//
// Only reached for shapes whose parameters are animated — every static
// rectangle, ellipse, star and path was turned into markup by the compiler.
//
// Each generator writes into a caller-owned path object so a steady-state
// frame allocates nothing. Setting `.length` on the arrays reuses their
// backing storage when the vertex count changes.

const KAPPA = 0.5522847498307933;

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

export function rectPath(out, cx, cy, w, h, rad) {
  const hw = w / 2, hh = h / 2;
  const l = cx - hw, t = cy - hh, ri = cx + hw, b = cy + hh;
  if (!(rad >= 1e-3)) {
    geoSize(out, 4, false);
    const v = out.v;
    v[0] = ri; v[1] = t; v[2] = ri; v[3] = b;
    v[4] = l; v[5] = b; v[6] = l; v[7] = t;
    return out;
  }
  const rr = Math.min(rad, hw, hh), k = rr * KAPPA;
  geoSize(out, 8, true);
  const v = out.v, i = out.i, o = out.o;
  const V = [ri, t + rr, ri, b - rr, ri - rr, b, l + rr, b, l, b - rr, l, t + rr, l + rr, t, ri - rr, t];
  const I = [0, 0, 0, 0, k, 0, 0, 0, 0, 0, 0, 0, -k, 0, 0, 0];
  const O = [0, 0, 0, k, 0, 0, 0, 0, 0, -k, 0, 0, 0, 0, k, 0];
  for (let j = 0; j < 16; j++) { v[j] = V[j]; i[j] = I[j]; o[j] = O[j]; }
  return out;
}

export function ellipsePath(out, cx, cy, rx, ry) {
  const kx = rx * KAPPA, ky = ry * KAPPA;
  geoSize(out, 4, true);
  const v = out.v, i = out.i, o = out.o;
  v[0] = cx; v[1] = cy - ry; v[2] = cx + rx; v[3] = cy;
  v[4] = cx; v[5] = cy + ry; v[6] = cx - rx; v[7] = cy;
  i[0] = -kx; i[1] = 0; i[2] = 0; i[3] = -ky;
  i[4] = kx; i[5] = 0; i[6] = 0; i[7] = ky;
  o[0] = kx; o[1] = 0; o[2] = 0; o[3] = ky;
  o[4] = -kx; o[5] = 0; o[6] = 0; o[7] = -ky;
  return out;
}

/** `sy === 1` alternates outer/inner radii (star); anything else is a polygon. */
export function starPath(out, sy, pts, cx, cy, or, ir, rot) {
  const p = Math.round(pts);
  if (p < 3) return geoSize(out, 0, false);
  const star = sy === 1;
  const n = star ? p * 2 : p;
  geoSize(out, n, false);
  const v = out.v;
  const step = (Math.PI * 2) / n;
  const a0 = (rot * Math.PI) / 180 - Math.PI / 2;
  for (let k = 0; k < n; k++) {
    const a = a0 + k * step;
    const rr = star && k & 1 ? ir : or;
    v[k * 2] = cx + rr * Math.cos(a);
    v[k * 2 + 1] = cy + rr * Math.sin(a);
  }
  return out;
}
