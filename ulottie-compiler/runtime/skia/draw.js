// The Skia draw walker — mount-time prepare plus the per-frame recursive
// record pass.
//
// `skPrepare` turns the compile-time display-list descriptor `dl` into live
// node records: SkPath for baked `d` strings and clip paths, SkPaint pairs
// for fills/strokes, mask and color-filter layer paints, gradient shaders.
// Records are built fresh per instance — worklet closures capture by copy,
// so caching them on the (captured) descriptor would never survive a second
// `init()` on the UI runtime. Slotted nodes link back onto the element
// handles (`el.n`) so `runtime/skia/set.js` can write converted values in
// place; the walk then reads only node fields and mutates nothing — zero
// per-frame allocation, matching the HANDOFF perf discipline.

import { skGeom, skPaints, skFxPaint } from './set.js';

/**
 * Build one node record from its descriptor. `d.k`: 0 group, 1 path,
 * 2 rect, 3 ellipse, 4 embedded image. See `backend/skia.rs` for the
 * descriptor grammar.
 */
export function skPrepare(d, els, Sk) {
  // Hidden is `hd` in the descriptor (a rect's height already owns `h`
  // there); the live record uses `h` because that is what skSet 'display'
  // writes.
  const n = { k: d.k, h: d.hd ? 1 : 0, m: d.m || null, o: d.o == null ? 1 : d.o, OP: null };
  if (d.s != null && els[d.s]) { els[d.s].n = n; els[d.s].K = Sk; }
  if (d.k === 0) {
    if (d.clip) {
      if (d.clip.r) {
        const r = d.clip.r;
        n.CR = { x: r[0], y: r[1], width: r[2], height: r[3] };
      } else {
        // A path clip is a slim node record of its own: a lottie layer mask
        // animates its bezier, so the clip's slot links here and the write
        // point rebuilds `P` exactly as for a drawable path.
        const cl = { k: 1, eo: d.clip.eo ? 1 : 0, P: null };
        if (d.clip.d) {
          cl.P = Sk.Path.MakeFromSVGString(d.clip.d);
          if (cl.P && cl.eo) cl.P.setFillType(1);
        }
        if (d.clip.s != null && els[d.clip.s]) { els[d.clip.s].n = cl; els[d.clip.s].K = Sk; }
        n.CL = cl;
      }
    }
    if (d.bm) {
      // mix-blend-mode: the group's layer composites against the backdrop
      // with the given Skia BlendMode on restore.
      const p = Sk.Paint();
      p.setBlendMode(d.bm);
      n.BM = p;
    }
    if (d.fx) {
      // Layer effects: stages of passes, innermost stage first. A pass is
      // null (source drawn unchanged) or a record with a layer paint `P`;
      // shadow/blur pass records also carry the parameter fields the FX ops
      // write, slot-linked like any other element.
      const st = [];
      for (let i = 0; i < d.fx.length; i++) {
        const row = [];
        for (let j = 0; j < d.fx[i].length; j++) {
          const p = d.fx[i][j];
          if (!p) {
            row.push(null);
          } else if (p.cf) {
            const q = Sk.Paint();
            q.setColorFilter(Sk.ColorFilter.MakeMatrix(p.cf));
            row.push({ P: q });
          } else if (p.cf2) {
            const q = Sk.Paint();
            q.setColorFilter(Sk.ColorFilter.MakeCompose(
              Sk.ColorFilter.MakeMatrix(p.cf2[0]),
              Sk.ColorFilter.MakeMatrix(p.cf2[1]),
            ));
            row.push({ P: q });
          } else if (p.sh) {
            const r = {
              k: 6, t: 2, P: Sk.Paint(),
              dx: p.dx || 0, dy: p.dy || 0, sd: p.sd || 0,
              c: p.c, fo: p.fo == null ? 1 : p.fo,
            };
            skFxPaint(r, Sk);
            if (p.sb != null && els[p.sb]) { els[p.sb].n = r; els[p.sb].K = Sk; }
            if (p.so != null && els[p.so]) { els[p.so].n = r; els[p.so].K = Sk; }
            if (p.sf != null && els[p.sf]) { els[p.sf].n = r; els[p.sf].K = Sk; }
            row.push(r);
          } else {
            const r = {
              k: 6, t: 1, P: Sk.Paint(),
              sx: p.sx || 0, sy: p.sy || 0, tm: p.tm || 0,
            };
            skFxPaint(r, Sk);
            if (p.s != null && els[p.s]) { els[p.s].n = r; els[p.s].K = Sk; }
            row.push(r);
          }
        }
        st.push(row);
      }
      n.FX = st;
    }
    if (d.cf) {
      const p = Sk.Paint();
      p.setColorFilter(Sk.ColorFilter.MakeMatrix(d.cf.m));
      const r = d.cf.r;
      n.CF = { P: p, R: { x: r[0], y: r[1], width: r[2], height: r[3] } };
    }
    if (d.mask) {
      // Composite the mask over the content layer with DstIn: content keeps
      // the mask's coverage. A luminance mask first maps luma into alpha.
      const p = Sk.Paint();
      p.setBlendMode(6);
      if (d.mask.luma) {
        p.setColorFilter(Sk.ColorFilter.MakeMatrix([
          0, 0, 0, 0, 0,
          0, 0, 0, 0, 0,
          0, 0, 0, 0, 0,
          0.2125, 0.7154, 0.0721, 0, 0,
        ]));
      }
      const mc = [];
      for (let i = 0; i < d.mask.c.length; i++) mc.push(skPrepare(d.mask.c[i], els, Sk));
      n.MK = { P: p, c: mc };
    }
    n.c = [];
    for (let i = 0; i < d.c.length; i++) n.c.push(skPrepare(d.c[i], els, Sk));
    return n;
  }
  if (d.k === 4) {
    // Embedded image: decode once at mount. The compiler bakes the box at
    // the asset's natural size, and lottie-web's `xMidYMid slice` fit is a
    // center-crop of the source to the box's aspect — exact here as a src
    // rect, even when the decoded bitmap disagrees with the declared size.
    const img = Sk.Image.MakeImageFromEncoded(Sk.Data.fromBase64(d.u));
    if (!img) throw new Error('ulottie: embedded image failed to decode');
    const sc = Math.max(d.w / img.width(), d.h / img.height());
    const sw = d.w / sc, sh = d.h / sc;
    n.IM = img;
    n.SRC = { x: (img.width() - sw) / 2, y: (img.height() - sh) / 2, width: sw, height: sh };
    n.DST = { x: 0, y: 0, width: d.w, height: d.h };
    n.P = Sk.Paint();
    return n;
  }
  // Shape: paint fields live flat on the record so the write point can
  // rebuild paints incrementally.
  n.eo = d.eo ? 1 : 0;
  const p = d.paint;
  n.f = p.f == null ? null : skGradRec(p.f, els, Sk, n);
  n.fo = p.fo == null ? null : p.fo;
  n.sc = p.sc == null ? null : skGradRec(p.sc, els, Sk, n);
  n.so = p.so == null ? null : p.so;
  n.sw = p.sw == null ? null : p.sw;
  n.cap = p.cap || 0;
  n.join = p.join || 0;
  n.ml = p.ml == null ? null : p.ml;
  n.da = p.da || null;
  n.doff = p.doff == null ? null : p.doff;
  n.po = p.po ? 1 : 0;
  n.FP = null;
  n.SP = null;
  n.R = { x: 0, y: 0, width: 0, height: 0 };
  if (n.k === 1) {
    n.P = d.d ? Sk.Path.MakeFromSVGString(d.d) : null;
    if (n.P && n.eo) n.P.setFillType(1);
  } else if (n.k === 2) {
    n.gx = d.x; n.gy = d.y; n.gw = d.w; n.gh = d.h;
    n.rx = d.rx || 0; n.ry = d.ry || 0;
    n.RR = { rect: n.R, rx: n.rx, ry: n.ry };
    skGeom(n);
  } else {
    n.cx = d.cx; n.cy = d.cy; n.rx = d.rx; n.ry = d.ry;
    skGeom(n);
  }
  skPaints(n, Sk);
  return n;
}

/**
 * A gradient paint descriptor → a live gradient record (`k: 4`), the value a
 * shape node's `f`/`sc` holds in place of a colour string. The gradient's
 * mutable fields live on `.g` (what `skGrad` builds the shader from) and
 * `.O` lists the owner shape nodes whose paints rebuild when a
 * GRADIENT/RAMP op writes. A gradient shared by two shapes resolves inline
 * twice with the same slots, so the second sight of a slotted descriptor
 * reuses the first's record instead of re-linking the slot away from it.
 * Bound stops link slim `k: 5` records that share `.g` and `.O`.
 */
export function skGradRec(g, els, Sk, owner) {
  if (typeof g === 'string') return g;
  let probe = g.s == null ? null : g.s;
  if (probe == null) {
    for (let i = 0; i < g.st.length; i++) {
      if (g.st[i].length > 2) { probe = g.st[i][2]; break; }
    }
  }
  if (probe != null && els[probe] && els[probe].n) {
    els[probe].n.O.push(owner);
    return els[probe].n;
  }
  const st = [];
  const rec = {
    k: 4,
    g: {
      rad: g.rad ? 1 : 0,
      cx: g.cx || 0, cy: g.cy || 0, r: g.r || 0,
      x1: g.x1 || 0, y1: g.y1 || 0, x2: g.x2 || 0, y2: g.y2 || 0,
      gt: g.gt || null,
      st,
    },
    O: [owner],
  };
  for (let i = 0; i < g.st.length; i++) {
    st.push([g.st[i][0], g.st[i][1]]);
    if (g.st[i].length > 2 && els[g.st[i][2]]) {
      els[g.st[i][2]].n = { k: 5, g: rec.g, i, O: rec.O };
      els[g.st[i][2]].K = Sk;
    }
  }
  if (g.s != null && els[g.s]) { els[g.s].n = rec; els[g.s].K = Sk; }
  return rec;
}

/** Record one node (and its subtree) onto a canvas. */
export function skDraw(c, n, Sk) {
  if (n.h) return;
  c.save();
  if (n.m) c.concat(n.m);
  if (n.k) {
    if (n.o < 1) {
      const p = n.OP || (n.OP = Sk.Paint());
      p.setAlphaf(n.o);
      c.saveLayer(p);
      skShape(c, n);
      c.restore();
    } else {
      skShape(c, n);
    }
    c.restore();
    return;
  }
  if (n.CL) {
    // An animated clip whose first `d` has not landed yet clips everything
    // out — matching an SVG <clipPath> with no shape.
    if (n.CL.P) c.clipPath(n.CL.P, 1, true);
    else c.clipRect({ x: 0, y: 0, width: 0, height: 0 }, 1, true);
  }
  if (n.CR) c.clipRect(n.CR, 1, true);
  let pops = 0;
  if (n.BM) {
    // Blend outermost: everything the group draws — clipped, filtered,
    // masked — composites against the backdrop in one blended restore.
    c.saveLayer(n.BM);
    pops++;
  }
  if (n.CF) {
    // The filter region clips its output (userSpaceOnUse, resolved at
    // compile time); the color matrix rides the layer's restore paint.
    c.clipRect(n.CF.R, 1, true);
    c.saveLayer(n.CF.P);
    pops++;
  }
  if (n.o < 1) {
    const p = n.OP || (n.OP = Sk.Paint());
    p.setAlphaf(n.o);
    c.saveLayer(p);
    pops++;
  }
  if (n.MK) { c.saveLayer(); pops++; }
  // skDraw is passed down as a value rather than called by name from skFx:
  // the worklets babel plugin captures a worklet's free variables when its
  // factory runs at module evaluation, so a skDraw↔skFx name cycle leaves
  // whichever is defined second captured as `undefined` on the UI runtime.
  if (n.FX) skFx(c, n, Sk, n.FX.length - 1, skDraw);
  else for (let i = 0; i < n.c.length; i++) skDraw(c, n.c[i], Sk);
  if (n.MK) {
    c.saveLayer(n.MK.P);
    for (let i = 0; i < n.MK.c.length; i++) skDraw(c, n.MK.c[i], Sk);
    c.restore();
  }
  while (pops--) c.restore();
  c.restore();
}

/**
 * Draw a group's children through its effect stages. Stage `si` re-draws
 * the running content — stage `si - 1`'s output, down to the raw children —
 * once per pass: a null pass draws it plain, a paint pass wraps it in a
 * layer whose restore applies the pass's colour or image filter. SVG's
 * sequential primitive chain, as nested Skia layers.
 */
function skFx(c, n, Sk, si, draw) {
  if (si < 0) {
    for (let i = 0; i < n.c.length; i++) draw(c, n.c[i], Sk);
    return;
  }
  const row = n.FX[si];
  for (let i = 0; i < row.length; i++) {
    const p = row[i];
    if (p) {
      c.saveLayer(p.P);
      skFx(c, n, Sk, si - 1, draw);
      c.restore();
    } else {
      skFx(c, n, Sk, si - 1, draw);
    }
  }
}

/**
 * The two styled draws of one shape. `paint-order="stroke"` — the rn-svg
 * target's one documented degradation — is exact here: the stroke draw
 * simply issues first.
 */
function skShape(c, n) {
  if (n.k === 4) {
    c.drawImageRect(n.IM, n.SRC, n.DST, n.P);
    return;
  }
  const F = n.FP, S = n.SP;
  if (n.po && S) {
    skGeomDraw(c, n, S);
    if (F) skGeomDraw(c, n, F);
    return;
  }
  if (F) skGeomDraw(c, n, F);
  if (S) skGeomDraw(c, n, S);
}

/** One geometry draw with one paint. */
function skGeomDraw(c, n, p) {
  if (n.k === 1) {
    if (n.P) c.drawPath(n.P, p);
  } else if (n.k === 2) {
    if (n.rx || n.ry) {
      const rr = n.RR;
      rr.rx = n.rx; rr.ry = n.ry;
      c.drawRRect(rr, p);
    } else {
      c.drawRect(n.R, p);
    }
  } else {
    c.drawOval(n.R, p);
  }
}
