// Layer in/out point. Only emitted for layers whose span is narrower than the
// composition's.

export function bDisplay(el, S, a) {
  const ip = S[a] / 1000, op = S[a + 1] / 1000;
  let on = null;
  return (f) => {
    const v = f >= ip && f < op;
    if (v !== on) { on = v; el.style.display = v ? '' : 'none'; }
  };
}
