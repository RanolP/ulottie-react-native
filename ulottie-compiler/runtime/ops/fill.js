import { resolve } from '../kf.js';
import { css } from '../css.js';
import { r } from '../num.js';
import { attr } from '../set.js';

export function bFill(el, b, ctx, at) {
  const o = resolve(b[3], ctx, at);
  // A null colour means the paint is a gradient reference already baked into
  // the markup — only its opacity varies.
  if (b[2] === null) {
    const setO = attr(el, 'fill-opacity');
    return (f) => setO(r(o(f) / 100));
  }
  const c = resolve(b[2], ctx, at);
  const set = attr(el, 'fill');
  return (f) => set(css(c(f), o(f)));
}
