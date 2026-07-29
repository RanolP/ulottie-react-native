// Shared number → string formatting for attribute values.
//
// Precision is chosen per role, because the error budgets differ by orders of
// magnitude: a matrix's linear part multiplies every coordinate beneath it
// (error ~ quantum * extent), while a translation or a path coordinate
// contributes absolute error only.
//
// Do not "optimize" this by assembling the digits from the rounded integer
// instead of handing a float to V8's dtoa. That version was written, tested
// byte-identical over 7434 real interpolated values, and measured 2.36× faster
// in isolation — and it was still reverted:
//
//   • A CDP sampling profile puts `fmt` at 3.0% of frame time on `lottie-logo`
//     (the most format-heavy fixture) and below the top twelve on `ripple`, so
//     2.36× caps out at a 1.7% win.
//   • End-to-end frame time, alternating four runs, showed no effect outside
//     noise.
//   • It cost +118 B gzipped on `lottie-logo`, +2.2% of the whole module.
//
// The profile says the time is in path-string assembly (`pathD` + `pdPair` +
// `pdSep`, 15% together) and in `setAttribute` and `evalExpr` — not here.
function fmt(x, scale) {
  const s = '' + Math.round(x * scale) / scale;
  // A bare leading zero is redundant in SVG: ".5" and "-.5" parse identically
  // and are shorter.
  if (s.charCodeAt(0) === 48 && s.charCodeAt(1) === 46) return s.slice(1);
  if (s.charCodeAt(0) === 45 && s.charCodeAt(1) === 48 && s.charCodeAt(2) === 46) {
    return '-' + s.slice(2);
  }
  return s;
}

/** Coordinates and plain attribute values: 3 decimals. */
export function r(x) {
  return fmt(x, 1000);
}

/** Matrix linear part: 5 decimals. */
export function r5(x) {
  return fmt(x, 1e5);
}

/** Matrix translation: 2 decimals. */
export function r2(x) {
  return fmt(x, 100);
}
