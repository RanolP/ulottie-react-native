// Reading a `[count, …values]` column out of the stream.

/**
 * Materialize a column, undoing the running sum for the ones that ship as
 * first differences. Returns null for an absent section, which every caller
 * treats as all-zero.
 */
export function column(S, off, delta) {
  if (!off) return null;
  const n = S[off];
  const out = new Array(n);
  let run = 0;
  for (let i = 0; i < n; i++) out[i] = delta ? (run += S[off + 1 + i]) : S[off + 1 + i];
  return out;
}
