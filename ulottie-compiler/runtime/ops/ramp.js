// One `<stop>` of a keyframed gradient ramp.
//
// A ramp that only moves its handles is `ops/grad.js`; its stops were resolved
// into the markup at compile time. This is the other half — the ramp itself is
// keyframed, so each stop's position and colour move.
//
// One binding per stop rather than one per gradient. SVG has no way to address
// a stop except as an element, and the batch machinery already resolves
// elements and clocks per binding; a single binding over a whole ramp would
// have to walk siblings and carry a per-stop cache of its own to compare
// against. The property is hash-consed, so the stops of one ramp share their
// time and easing columns on the wire and pay only for their own values.

import { xvv } from '../pv.js';
import { xcol } from '../kf.js';
import { r } from '../num.js';
import { put } from '../set.js';
import { open } from '../batch.js';

export function bRamp(x, base, eb, sb, ps, at) {
  const b = open(x, base, eb, sb, ps, 1);
  const n = b.n;
  const V = new Array(n);
  // `[offset, r, g, b]` — a fixed four, so the scratch is not sized per
  // binding the way a paint colour's is.
  for (let i = 0; i < n; i++) V[i] = [0, 0, 0, 0];
  return {
    n, E: b.E, G: b.G, L: b.L, K: b.A[0],
    XK: x.expr ? xcol(x, b.A[0], n, at) : null,
    CK: new Int32Array(n), V,
    W: new Array(n), W2: new Array(n),
  };
}

export function oRamp(x, s) {
  const n = s.n, E = s.E, G = s.G, L = s.L, K = s.K, T = x.T, ON = x.ON;
  const XK = s.XK, CK = s.CK, V = s.V, W = s.W, W2 = s.W2;
  for (let i = 0; i < n; i++) {
    if (!ON[G[i]]) continue;
    const v = xvv(x, XK, K, i, T[L[i]], CK, V[i]);
    put(E[i], 'offset', r(v[0]), W, i);
    // Channels are 0..1 floats here, as everywhere in Lottie. `stop-opacity`
    // is not written: a ramp carrying alpha stops is not planned this way,
    // because their positions are independent of the colour stops'.
    put(E[i], 'stop-color',
      'rgb(' + ((v[1] * 255 + 0.5) | 0) + ',' + ((v[2] * 255 + 0.5) | 0) + ','
             + ((v[3] * 255 + 0.5) | 0) + ')', W2, i);
  }
}
