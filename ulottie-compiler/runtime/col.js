// Reading a `[count, …values]` column out of the stream.

/**
 * Materialize a column, undoing the running sum.
 *
 * Both surviving callers read the record-offset table, which ships as first
 * differences — it is the one column in the format that ascends and never
 * repeats a value. Returns null for an absent section, which every caller
 * treats as all-zero.
 */
export function column(S, off) {
  if (!off) return null;
  const n = S[off];
  const out = new Array(n);
  let run = 0;
  for (let i = 0; i < n; i++) out[i] = (run += S[off + 1 + i]);
  return out;
}
