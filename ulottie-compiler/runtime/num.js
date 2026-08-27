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
//   • A CDP sampling profile puts `fmt` at 3.0% of frame time on `lottie_logo_1`
//     (the most format-heavy fixture) and below the top twelve on `ripple`, so
//     2.36× caps out at a 1.7% win.
//   • End-to-end frame time, alternating four runs, showed no effect outside
//     noise.
//   • It cost +118 B gzipped on `lottie_logo_1`, +2.2% of the whole module.
//
// The profile says the time is in path-string assembly (`pathD` + `pdPair` +
// `pdSep`, 15% together) and in `setAttribute` and `evalExpr` — not here.
//
// A fraction keeps its leading zero even though SVG parses ".5" fine. The
// react-native-svg target shares this helper, and its JS prop parser rejects
// ".47" ("not a valid number or percentage string"), leaves the prop a String,
// and Fabric's generated RNSVGGroupManagerDelegate.setProperty then throws
// ClassCastException: String cannot be cast to Double — an app-killing crash on
// Android, observed on a Pixel 8 with the `mixed16` fixture.
function fmt(x, scale) {
  return '' + Math.round(x * scale) / scale;
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
