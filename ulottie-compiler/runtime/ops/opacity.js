import { resolve } from '../kf.js';
import { r } from '../num.js';
import { attr } from '../set.js';

export function bOpacity(el, b, ctx, at) {
  const o = resolve(b[2], ctx, at);
  const set = attr(el, 'opacity');
  return (f) => set(r(o(f) / 100));
}
