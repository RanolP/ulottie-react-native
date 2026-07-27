// Layer records, addressed through the instantiation that owns them.
//
// A precomp's records live on its asset and are stored once, so an index inside
// a precomp is local to it. `at` says which asset this instantiation replays
// and where its records start; at the document level there is no offset.

export function record(ctx, at, i) {
  return at ? ctx.D.q[at.asset].y[i] : ctx.D.y[i];
}

/** Global index of a record, for keying proxies. */
export function recordId(at, i) {
  return at ? at.recBase + i : i;
}
