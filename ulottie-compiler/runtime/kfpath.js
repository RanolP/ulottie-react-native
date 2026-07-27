// Interpolation between two keyframed bezier paths.
//
// Only reachable when a shape's path itself is animated, which the compiler
// reports as the PATH_KF capability — shipped only for those animations.

// Allocates, because callers hand the result straight to the serializer and
// paths are rarely keyframed.
export function lerpPath(a, b, u) {
  const av = a.v;
  if (!b || !b.v || b.v.length !== av.length) return a;
  const n = av.length;
  const v = new Array(n), i = new Array(n), o = new Array(n);
  const ai = a.i, ao = a.o, bi = b.i, bo = b.o;
  for (let k = 0; k < n; k++) {
    v[k] = av[k] + (b.v[k] - av[k]) * u;
    const a1 = ai ? ai[k] : 0, b1 = bi ? bi[k] : 0;
    const a2 = ao ? ao[k] : 0, b2 = bo ? bo[k] : 0;
    i[k] = a1 + (b1 - a1) * u;
    o[k] = a2 + (b2 - a2) * u;
  }
  return { v, i, o, c: u < 0.5 ? a.c : b.c };
}
