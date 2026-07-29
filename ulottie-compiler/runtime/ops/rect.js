import { resolve } from '../kf.js';
import { r } from '../num.js';
import { attr } from '../set.js';

export function bRect(el, S, a, ctx, at) {
  const sz = resolve(S[a], ctx, at);
  const ps = resolve(S[a + 1], ctx, at);
  const rd = resolve(S[a + 2], ctx, at);
  const setX = attr(el, 'x'), setY = attr(el, 'y');
  const setW = attr(el, 'width'), setH = attr(el, 'height');
  const setRx = attr(el, 'rx'), setRy = attr(el, 'ry');
  return (f) => {
    const s = sz(f), p = ps(f), rad = rd(f);
    setX(r(p[0] - s[0] / 2));
    setY(r(p[1] - s[1] / 2));
    setW(r(s[0]));
    setH(r(s[1]));
    if (rad > 0) {
      const c = r(Math.min(rad, s[0] / 2, s[1] / 2));
      setRx(c);
      setRy(c);
    }
  };
}
