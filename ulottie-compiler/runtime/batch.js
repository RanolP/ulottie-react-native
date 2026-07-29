// Opening one op's batch.
//
// The compiler groups every binding by op and writes each group as a
// struct-of-arrays batch:
//
// ```text
// [count, flags,
//  el…     count, first differences
//  gate…   count, when flags & 1
//  slot…   count, when flags & 2
//  arg0…   count
//  …
//  argK…   count]
// ```
//
// An op comes in two halves, and neither is a callback. `bXxx` binds a batch
// once and hands back a plain state record; `oXxx(x, s)` is the frame, called
// directly. The module names both, because which op a binding is was never
// anything but a compile-time fact — so nothing here closes over anything, and
// the only closure left in an animation is the one `player` is handed.
//
// Gates and clocks are materialized as dense columns even when the wire omits
// them, so the loop reads `ON[G[i]]` and `T[L[i]]` unconditionally — gate 0 is
// pinned on and slot 0 is the composition clock. A present-or-absent branch per
// binding per frame buys nothing against two loads from a small typed array.

/**
 * Resolve a batch's columns: elements, gate, clock, and `k` argument columns.
 *
 * `eb`/`sb`/`ps` position the batch within an instantiation — the element base,
 * the clock base, and the clock the instance itself runs on. All three are zero
 * for the document's own bindings, which is what lets one code path serve both.
 */
export function open(x, base, eb, sb, ps, k) {
  const S = x.S;
  const n = S[base];
  const flags = S[base + 1];
  let c = base + 2;
  // Element indices ship as first differences: consecutive bindings sit close
  // together in document order, and an instanced asset's column then replays at
  // any base.
  const E = new Array(n);
  for (let i = 0, e = 0; i < n; i++) E[i] = x.els[eb + (e += S[c + i])];
  c += n;
  const G = new Int32Array(n);
  if (flags & 1) {
    for (let i = 0; i < n; i++) G[i] = S[c + i];
    c += n;
  }
  const L = new Int32Array(n);
  if (flags & 2) {
    // A local slot of zero is the instance's own clock, not the composition's.
    for (let i = 0; i < n; i++) { const v = S[c + i]; L[i] = v ? sb + v : ps; }
    c += n;
  } else if (ps) {
    L.fill(ps);
  }
  // Arguments are copied out of the stream rather than indexed through it, so
  // the frame loop reads a dense typed array it can hold in a register.
  const A = new Array(k);
  for (let j = 0; j < k; j++, c += n) {
    const col = new Int32Array(n);
    for (let i = 0; i < n; i++) col[i] = S[c + i];
    A[j] = col;
  }
  return { n, E, G, L, A };
}

/** Undo a column's first differences, in place. */
export function runsum(col, n) {
  for (let i = 1; i < n; i++) col[i] += col[i - 1];
  return col;
}
