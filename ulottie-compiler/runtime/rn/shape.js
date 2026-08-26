// The web `trim` settles a fully-closed trim range by writing
// `el.style.display` — the second of the three direct DOM writes in the
// runtime. Same body, with the hide landing in the RN prop store instead.
// Every other declaration in ops/shape.js is DOM-free and stays shared;
// this one keeps its name so the `TRIM` capability gate still cuts it.

import { xv } from '../pv.js';
import { trimTable, trimApply } from '../trim.js';
import { rput } from './set.js';

function trim(x, m, i, t, src, el) {
  const a = xv(x, m.X, m.M, i, t, m.C) / 100;
  const z = xv(x, m.X2, m.M2, i, t, m.C2) / 100;
  let lo = a < z ? a : z, hi = a < z ? z : a;
  let off = xv(x, m.X3, m.M3, i, t, m.C3) / 360;
  if (m.R && m.R.r[i]) {
    const w = trimChainWin(x, m.R, i, t, lo + off, hi - lo);
    lo = w[0]; hi = w[1]; off = 0;
  }
  const vis = hi - lo;
  let out = null, hide = false;
  if (vis <= 0) {
    hide = true;
  } else if (vis < 1) {
    out = trimApply(m.B[i] || trimTable(src), lo, hi, off);
    if (out && !out.v.length) hide = true;
  }
  if (hide !== m.W[i]) { m.W[i] = hide; rput(el, 'display', hide ? 'none' : ''); }
  return hide ? null : out || src;
}
