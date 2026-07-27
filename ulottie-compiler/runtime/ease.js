// Cubic-bezier timing solve.
//
// Only bundled when the animation actually has a non-linear easing handle —
// the compiler folds every linear (and linear-equivalent) segment to index 0
// and the interpolator skips this entirely for those.

/**
 * Solve `x(s) = u` for the bezier through (0,0), (x1,y1), (x2,y2), (1,1),
 * then return `y(s)`. Newton–Raphson seeded at `u`; eight iterations is well
 * past convergence for the handle ranges Lottie produces.
 *
 * @param {ArrayLike<number>} e `[x1, y1, x2, y2]`
 */
export function EASE(e, u) {
  const x1 = e[0], y1 = e[1], x2 = e[2], y2 = e[3];
  let s = u;
  for (let i = 0; i < 8; i++) {
    const m = 1 - s;
    const x = 3 * m * m * s * x1 + 3 * m * s * s * x2 + s * s * s - u;
    if (x > -1e-6 && x < 1e-6) break;
    const dx = 3 * m * m * x1 + 6 * m * s * (x2 - x1) + 3 * s * s * (1 - x2);
    if (dx > -1e-6 && dx < 1e-6) break;
    s -= x / dx;
    s = s < 0 ? 0 : s > 1 ? 1 : s;
  }
  const m = 1 - s;
  return 3 * m * m * s * y1 + 3 * m * s * s * y2 + s * s * s;
}
