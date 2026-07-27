import { resolve } from '../kf.js';
import { css } from '../css.js';
import { r } from '../num.js';
import { attr } from '../set.js';

export function bStroke(el, b, ctx, at) {
  const o = resolve(b[3], ctx, at);
  const w = resolve(b[4], ctx, at);
  const setW = attr(el, 'stroke-width');
  if (b[2] === null) {
    const setO = attr(el, 'stroke-opacity');
    return (f) => {
      setO(r(o(f) / 100));
      setW(r(w(f)));
    };
  }
  const c = resolve(b[2], ctx, at);
  const setC = attr(el, 'stroke');
  return (f) => {
    setC(css(c(f), o(f)));
    setW(r(w(f)));
  };
}
