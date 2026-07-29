import { resolve } from '../kf.js';
import { r } from '../num.js';
import { attr } from '../set.js';

export function bOpacity(el, S, a, ctx, at) {
  const o = resolve(S[a], ctx, at);
  const set = attr(el, 'opacity');
  return (f) => set(r(o(f) / 100));
}
