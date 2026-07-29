import { resolve } from '../kf.js';
import { css } from '../css.js';
import { r } from '../num.js';
import { attr } from '../set.js';

export function bStroke(el, S, a, ctx, at) {
  const o = resolve(S[a + 1], ctx, at);
  const w = resolve(S[a + 2], ctx, at);
  const setW = attr(el, 'stroke-width');
  if (!S[a]) {
    const setO = attr(el, 'stroke-opacity');
    return (f) => {
      setO(r(o(f) / 100));
      setW(r(w(f)));
    };
  }
  const c = resolve(S[a], ctx, at);
  const setC = attr(el, 'stroke');
  return (f) => {
    setC(css(c(f), o(f)));
    setW(r(w(f)));
  };
}
