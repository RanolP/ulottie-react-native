// Geometry binding: build a path for the frame, optionally trim it, write `d`.
//
// Reached only when something about the shape actually moves. A static shape
// under an animated trim still comes through here, but with its source path
// already resolved by the compiler — so its arc-length table is built once.

import { resolve } from '../kf.js';
import { pathD } from '../path.js';
import { rectPath, ellipsePath, starPath } from '../geom.js';
import { trimTable, trimApply } from '../trim.js';
import { attr } from '../set.js';

export function bShape(el, b, ctx, at) {
  const g = b[2];
  const tm = b[3];
  const kind = g[0];
  const scratch = { v: [], i: null, o: null, c: 1 };

  let geo, fixed = null;
  if (kind === 0) {
    const p = g[1];
    geo = resolve(p, ctx, at);
    if (p && typeof p === 'object' && p.v && p.t === undefined) fixed = p;
  } else if (kind === 1) {
    const sz = resolve(g[1], ctx, at), ps = resolve(g[2], ctx, at), rd = resolve(g[3], ctx, at);
    geo = (f) => {
      const s = sz(f), p = ps(f);
      return rectPath(scratch, p[0], p[1], s[0], s[1], rd(f));
    };
  } else if (kind === 2) {
    const sz = resolve(g[1], ctx, at), ps = resolve(g[2], ctx, at);
    geo = (f) => {
      const s = sz(f), p = ps(f);
      return ellipsePath(scratch, p[0], p[1], s[0] / 2, s[1] / 2);
    };
  } else {
    const sy = g[1];
    const pt = resolve(g[2], ctx, at), ps = resolve(g[3], ctx, at);
    const or = resolve(g[4], ctx, at), ir = resolve(g[5], ctx, at), rt = resolve(g[6], ctx, at);
    geo = (f) => {
      const p = ps(f);
      return starPath(scratch, sy, pt(f), p[0], p[1], or(f), ir(f), rt(f));
    };
  }

  const setD = attr(el, 'd');
  if (!tm) return (f) => setD(pathD(geo(f)));

  const ts = resolve(tm[0], ctx, at), te = resolve(tm[1], ctx, at), to = resolve(tm[2], ctx, at);
  const table = fixed ? trimTable(fixed) : null;
  let hidden = null;
  return (f) => {
    const src = geo(f);
    const s = ts(f) / 100, e = te(f) / 100;
    const lo = s < e ? s : e, hi = s < e ? e : s;
    const vis = hi - lo;
    let out = null, hide = false;
    if (vis <= 0) {
      hide = true;
    } else if (vis < 1) {
      out = trimApply(table || trimTable(src), lo, hi, to(f) / 360);
      if (out && !out.v.length) hide = true;
    }
    if (hide !== hidden) {
      hidden = hide;
      el.style.display = hide ? 'none' : '';
    }
    if (!hide) setD(pathD(out || src));
  };
}
