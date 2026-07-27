// Path serialization.
//
// Only bundled when some geometry is actually animated — every static path in
// the animation was serialized by the compiler and lives in the markup.
//
// Paths use the flat layout `{v:[x,y,…], i:[…], o:[…], c:0|1}`. `i`/`o` are
// null for polygonal paths.

import { r } from './num.js';

/** `x,y` — the comma is dropped when `y` self-delimits via its sign. */
function pdPair(x, y) {
  const b = r(y);
  return r(x) + (b.charCodeAt(0) === 45 ? '' : ',') + b;
}

/** Separator ahead of an already-formatted number. */
function pdSep(s) {
  return s.charCodeAt(0) === 45 ? '' : ',';
}

export function pathD(p) {
  const v = p.v;
  const n = v.length >> 1;
  if (!n) return '';
  const ti = p.i, to = p.o;
  const segs = p.c ? n : n - 1;
  let d = 'M' + pdPair(v[0], v[1]);
  for (let s = 0; s < segs; s++) {
    const a = s * 2;
    const b = ((s + 1) % n) * 2;
    const ox = to ? to[a] : 0, oy = to ? to[a + 1] : 0;
    const ix = ti ? ti[b] : 0, iy = ti ? ti[b + 1] : 0;
    if (!ox && !oy && !ix && !iy) {
      d += 'L' + pdPair(v[b], v[b + 1]);
    } else {
      const c2 = pdPair(v[b] + ix, v[b + 1] + iy);
      const c3 = pdPair(v[b], v[b + 1]);
      d += 'C' + pdPair(v[a] + ox, v[a + 1] + oy) + pdSep(c2) + c2 + pdSep(c3) + c3;
    }
  }
  return p.c ? d + 'Z' : d;
}
