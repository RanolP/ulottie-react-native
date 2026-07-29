import { resolve } from '../kf.js';
import { css } from '../css.js';
import { r } from '../num.js';
import { attr } from '../set.js';

export function bFill(el, S, a, ctx, at) {
  const o = resolve(S[a + 1], ctx, at);
  // Offset 0 means the paint is a gradient reference already baked into the
  // markup — only its opacity varies.
  if (!S[a]) {
    const setO = attr(el, 'fill-opacity');
    return (f) => setO(r(o(f) / 100));
  }
  const c = resolve(S[a], ctx, at);
  const set = attr(el, 'fill');
  return (f) => set(css(c(f), o(f)));
}
