// Layer in/out point. Only emitted for layers whose span is narrower than the
// composition's.

export function bDisplay(el, b, ctx, at) {
  const ip = b[2], op = b[3];
  let on = null;
  return (f) => {
    const v = f >= ip && f < op;
    if (v !== on) { on = v; el.style.display = v ? '' : 'none'; }
  };
}
