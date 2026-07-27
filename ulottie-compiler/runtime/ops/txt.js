// Translate-only transform: the compiler proved anchor, scale and rotation are
// constant, so the matrix's linear part is a baked string prefix and each frame
// only has to append two numbers.

import { resolve } from '../kf.js';
import { r2 } from '../num.js';
import { attr } from '../set.js';

export function bTranslate(el, b, ctx, at) {
  const pre = b[2], ex = b[3], ey = b[4];
  const p = resolve(b[5], ctx, at);
  const set = attr(el, 'transform');
  return (f) => {
    const v = p(f);
    set(pre + r2(v[0] + ex) + ',' + r2(v[1] + ey) + ')');
  };
}
