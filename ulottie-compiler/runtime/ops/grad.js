// Animated gradient geometry. The stops themselves were resolved and written
// into the markup at compile time; only the start/end handles move.

import { resolve } from '../kf.js';
import { r } from '../num.js';
import { attr } from '../set.js';

export function bGradient(el, b, ctx, at) {
  const radial = b[2] === 2;
  const s = resolve(b[3], ctx, at);
  const e = resolve(b[4], ctx, at);
  const k = radial ? ['cx', 'cy', 'r'] : ['x1', 'y1', 'x2', 'y2'];
  const set = k.map((name) => attr(el, name));
  return (f) => {
    const a = s(f), c = e(f);
    set[0](r(a[0]));
    set[1](r(a[1]));
    if (radial) {
      set[2](r(Math.hypot(c[0] - a[0], c[1] - a[1])));
    } else {
      set[2](r(c[0]));
      set[3](r(c[1]));
    }
  };
}
