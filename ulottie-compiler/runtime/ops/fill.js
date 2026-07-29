import { xv, xvv, vscratch } from '../pv.js';
import { xcol } from '../kf.js';
import { css } from '../css.js';
import { r } from '../num.js';
import { put } from '../set.js';
import { open } from '../batch.js';

export function bFill(x, base, eb, sb, ps, at) {
  const b = open(x, base, eb, sb, ps, 2);
  const n = b.n;
  return {
    n, E: b.E, G: b.G, L: b.L, K: b.A[0], O: b.A[1],
    XK: x.expr ? xcol(x, b.A[0], n, at) : null,
    XO: x.expr ? xcol(x, b.A[1], n, at) : null,
    CK: new Int32Array(n), CO: new Int32Array(n),
    // Sized per binding rather than per column, because `css` asks the buffer
    // itself whether the colour carries an alpha channel.
    V: vscratch(x, b.A[0], n),
    W: new Array(n),
  };
}

export function oFill(x, s) {
  const n = s.n, E = s.E, G = s.G, L = s.L, K = s.K, W = s.W, T = x.T, ON = x.ON;
  const O = s.O, XK = s.XK, XO = s.XO, CK = s.CK, CO = s.CO, V = s.V;
  for (let i = 0; i < n; i++) {
    if (!ON[G[i]]) continue;
    const t = T[L[i]];
    const o = xv(x, XO, O, i, t, CO);
    // A colour offset of zero means the paint is a gradient reference already
    // baked into the markup, and only its opacity varies.
    put(E[i], K[i] ? 'fill' : 'fill-opacity',
      K[i] ? css(xvv(x, XK, K, i, t, CK, V[i]), o) : r(o / 100), W, i);
  }
}
