// Shared number → string formatting for attribute values.
//
// Precision is chosen per role, because the error budgets differ by orders of
// magnitude: a matrix's linear part multiplies every coordinate beneath it
// (error ~ quantum * extent), while a translation or a path coordinate
// contributes absolute error only.

function fmt(x, scale) {
  const s = '' + (Math.round(x * scale) / scale);
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
