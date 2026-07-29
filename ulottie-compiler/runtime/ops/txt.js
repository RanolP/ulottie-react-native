// Translate-only transform: the compiler proved anchor, scale and rotation are
// constant, so the matrix's linear part is a baked string prefix and each frame
// only has to append two numbers.

import { resolve } from '../kf.js';
import { r2 } from '../num.js';
import { attr } from '../set.js';

export function bTranslate(el, S, a, ctx, at) {
  // [prefixString, extraX, extraY, position]
  // No prefix means the linear part was the identity, which `translate()`
  // spells in five fewer bytes — and it is the common case, so the compiler
  // sends nothing rather than a string saying nothing.
  const pre = S[a] ? ctx.str[S[a] - 1] : 'translate(';
  const ex = S[a + 1] / 1000, ey = S[a + 2] / 1000;
  const p = resolve(S[a + 3], ctx, at);
  const set = attr(el, 'transform');
  return (f) => {
    const v = p(f);
    set(pre + r2(v[0] + ex) + ',' + r2(v[1] + ey) + ')');
  };
}
