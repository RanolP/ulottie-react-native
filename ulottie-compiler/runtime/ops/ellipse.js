import { resolve } from '../kf.js';
import { r } from '../num.js';
import { attr } from '../set.js';

export function bEllipse(el, S, a, ctx, at) {
  const sz = resolve(S[a], ctx, at);
  const ps = resolve(S[a + 1], ctx, at);
  const setCx = attr(el, 'cx'), setCy = attr(el, 'cy');
  const setRx = attr(el, 'rx'), setRy = attr(el, 'ry');
  return (f) => {
    const s = sz(f), p = ps(f);
    setCx(r(p[0]));
    setCy(r(p[1]));
    setRx(r(s[0] / 2));
    setRy(r(s[1] / 2));
  };
}
