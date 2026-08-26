// Layer in/out on a react-native-svg handle — replaces the web `oDisplay`
// at declaration granularity (`bDisplay` is DOM-free and stays shared).
//
// There is no style object on an RN handle, so the gate lands in the props
// record as `display: '' | 'none'`, and the consumer's element wrapper
// renders nothing while it is 'none'. Not `opacity`: a layer can carry an
// animated opacity *and* an in/out span, and folding the gate into opacity
// would have the two fight over one prop slot.

import { rput } from './set.js';

export function oDisplay(x, s) {
  const n = s.n, E = s.E, G = s.G, L = s.L, O = s.O, Q = s.Q, W = s.W;
  const T = x.T, ON = x.ON;
  for (let i = 0; i < n; i++) {
    if (!ON[G[i]]) continue;
    const t = T[L[i]];
    const v = t >= O[i] && t < Q[i];
    if (v !== W[i]) { W[i] = v; rput(E[i], 'display', v ? '' : 'none'); }
  }
}
