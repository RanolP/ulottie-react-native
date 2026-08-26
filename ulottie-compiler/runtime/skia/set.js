// The Skia write point — the skia-aot counterpart of `runtime/rn/set.js`.
//
// An element handle is the same `{ i, p, d, q }` record `mountRn` builds,
// plus two fields `skPrepare` links at mount: `el.n`, the display-list node
// the slot lowered to, and `el.K`, the Skia factory. The ops keep producing
// the web runtime's strings — identity is what the change detection
// compares, so every conversion below runs on actual changes only — and the
// converted value lands on the node record the draw walker reads: matrices
// as 9-number row-major arrays, `d` strings as cached SkPath objects,
// paint attributes as rebuilt SkPaint state.

export function put(el, name, v, w, i) {
  if (v !== w[i]) {
    w[i] = v;
    skSet(el, name, v);
    if (!el.d) { el.d = 1; el.q.push(el); }
  }
}

/**
 * Direct write for the loops that write outside `put`'s one-attribute guard
 * (the display gates, the rect radius pair). The caller has already
 * change-detected; this only converts and marks the element dirty.
 */
export function rput(el, prop, v) {
  skSet(el, prop, v);
  if (!el.d) { el.d = 1; el.q.push(el); }
}

/**
 * One attribute write onto a display-list node. The name set is closed: it
 * is exactly what the ops inside the skia-aot capability whitelist emit, and
 * an unknown name throws instead of dropping the write — the runtime twin of
 * the compiler's named-refusal rule.
 */
function skSet(el, name, v) {
  const n = el.n, Sk = el.K;
  switch (name) {
    case 'transform': n.m = skMatrix(v); break;
    case 'opacity': n.o = +v; break;
    case 'display': n.h = v === 'none' ? 1 : 0; break;
    case 'd': {
      n.P = Sk.Path.MakeFromSVGString(v);
      if (n.P && n.eo) n.P.setFillType(1);
      break;
    }
    case 'fill': n.f = v; skPaints(n, Sk); break;
    case 'fill-opacity': n.fo = +v; skPaints(n, Sk); break;
    case 'stroke': n.sc = v; skPaints(n, Sk); break;
    case 'stroke-opacity': n.so = +v; skPaints(n, Sk); break;
    case 'stroke-width': n.sw = +v; skPaints(n, Sk); break;
    case 'stroke-dasharray': n.da = skNums(v); skPaints(n, Sk); break;
    case 'stroke-dashoffset': n.doff = +v; skPaints(n, Sk); break;
    case 'x': n.gx = +v; skGeom(n); break;
    case 'y': n.gy = +v; skGeom(n); break;
    case 'width': n.gw = +v; skGeom(n); break;
    case 'height': n.gh = +v; skGeom(n); break;
    // `cx`/`cy` are written by both the ellipse geometry ops and the radial
    // GRADIENT op; the record kind (4 = gradient) dispatches.
    case 'cx': if (n.k === 4) { n.g.cx = +v; skOwn(n, Sk); } else { n.cx = +v; skGeom(n); } break;
    case 'cy': if (n.k === 4) { n.g.cy = +v; skOwn(n, Sk); } else { n.cy = +v; skGeom(n); } break;
    case 'rx': n.rx = +v; skGeom(n); break;
    case 'ry': n.ry = +v; skGeom(n); break;
    // Animated gradient geometry (ops/grad.js) on a `k: 4` record.
    case 'r': n.g.r = +v; skOwn(n, Sk); break;
    case 'x1': n.g.x1 = +v; skOwn(n, Sk); break;
    case 'y1': n.g.y1 = +v; skOwn(n, Sk); break;
    case 'x2': n.g.x2 = +v; skOwn(n, Sk); break;
    case 'y2': n.g.y2 = +v; skOwn(n, Sk); break;
    // A keyframed ramp stop (ops/ramp.js) on a `k: 5` record.
    case 'offset': n.g.st[n.i][0] = +v; skOwn(n, Sk); break;
    case 'stop-color': n.g.st[n.i][1] = v; skOwn(n, Sk); break;
    // Animated layer-effect parameters (ops/fx.js) on a `k: 6` record.
    // `stdDeviation` arrives as an `"sx sy"` pair from the blur op and as a
    // lone scalar from the shadow-softness op; the record type (2 = shadow)
    // dispatches.
    case 'stdDeviation': {
      if (n.t === 2) {
        n.sd = +v;
      } else {
        const a = skNums(v);
        n.sx = a[0]; n.sy = a[1];
      }
      skFxPaint(n, Sk);
      break;
    }
    case 'dx': n.dx = +v; skFxPaint(n, Sk); break;
    case 'dy': n.dy = +v; skFxPaint(n, Sk); break;
    case 'flood-opacity': n.fo = +v; skFxPaint(n, Sk); break;
    default: throw new Error('skia-aot runtime has no write for `' + name + '`');
  }
}

/**
 * `matrix(a,b,c,d,e,f)` / `translate(x,y)` → row-major 3x3 for
 * `canvas.concat`. Those two spellings are the only ones the compiler ever
 * writes (see `scene::svg::matrix`); the parse mirrors `rnMatrix`.
 */
function skMatrix(v) {
  const a = v.slice(v.indexOf('(') + 1, v.length - 1).split(',');
  return v.charCodeAt(0) === 116
    ? [1, 0, +a[0], 0, 1, +a[1], 0, 0, 1]
    : [+a[0], +a[2], +a[4], +a[1], +a[3], +a[5], 0, 0, 1];
}

/** Space-separated number list (the dash op's `"4 2"` spelling). */
function skNums(v) {
  const parts = v.split(' ');
  const out = [];
  for (let i = 0; i < parts.length; i++) out.push(+parts[i]);
  return out;
}

/**
 * Refresh the scratch draw rect from the geometry fields. Rect nodes carry
 * `gx/gy/gw/gh` (+ corner `rx/ry`); ellipse nodes carry center + radii. One
 * mutable rect per node — the draw walk allocates nothing per frame.
 */
export function skGeom(n) {
  const R = n.R;
  if (n.k === 2) {
    R.x = n.gx; R.y = n.gy; R.width = n.gw; R.height = n.gh;
  } else {
    R.x = n.cx - n.rx; R.y = n.cy - n.ry; R.width = 2 * n.rx; R.height = 2 * n.ry;
  }
}

/**
 * Rebuild a shape node's fill/stroke SkPaint pair from its paint fields.
 * Coarse on purpose: any paint-field change rebuilds both paints, which
 * keeps the write point tiny and still runs only on change (the string
 * guards upstream).
 */
export function skPaints(n, Sk) {
  const f = n.f;
  if (f == null || f === 'none') {
    n.FP = null;
  } else {
    const p = n.FP || (n.FP = Sk.Paint());
    p.setAntiAlias(true);
    if (typeof f === 'string') {
      p.setShader(null);
      p.setColor(Sk.Color(f));
    } else {
      p.setColor(Sk.Color('#000'));
      p.setShader(skGrad(f.g, Sk));
    }
    if (n.fo != null) p.setAlphaf(p.getAlphaf() * n.fo);
  }
  const s = n.sc;
  if (s == null || s === 'none') {
    n.SP = null;
  } else {
    const p = n.SP || (n.SP = Sk.Paint());
    p.setAntiAlias(true);
    p.setStyle(1);
    if (typeof s === 'string') {
      p.setShader(null);
      p.setColor(Sk.Color(s));
    } else {
      p.setColor(Sk.Color('#000'));
      p.setShader(skGrad(s.g, Sk));
    }
    if (n.so != null) p.setAlphaf(p.getAlphaf() * n.so);
    p.setStrokeWidth(n.sw == null ? 1 : n.sw);
    p.setStrokeCap(n.cap || 0);
    p.setStrokeJoin(n.join || 0);
    if (n.ml != null) p.setStrokeMiter(n.ml);
    if (n.da && n.da.length) {
      // Skia requires an even interval count; SVG doubles an odd list.
      let iv = n.da;
      if (iv.length & 1) iv = iv.concat(iv);
      p.setPathEffect(Sk.PathEffect.MakeDash(iv, n.doff || 0));
    } else {
      p.setPathEffect(null);
    }
  }
}

/**
 * A live gradient object (`skGradRec`'s `.g`) → SkShader. Runs at mount and
 * again whenever a GRADIENT/RAMP op writes a field, via `skOwn`.
 */
function skGrad(g, Sk) {
  const colors = [];
  const pos = [];
  for (let i = 0; i < g.st.length; i++) {
    pos.push(g.st[i][0]);
    colors.push(Sk.Color(g.st[i][1]));
  }
  const lm = g.gt ? Sk.Matrix(g.gt) : undefined;
  return g.rad
    ? Sk.Shader.MakeRadialGradient(Sk.Point(g.cx, g.cy), g.r, colors, pos, 0, lm)
    : Sk.Shader.MakeLinearGradient(Sk.Point(g.x1, g.y1), Sk.Point(g.x2, g.y2), colors, pos, 0, lm);
}

/**
 * A gradient field changed: rebuild the shader-carrying paints of every
 * shape that draws with this gradient. `n` is a `k: 4` gradient record or a
 * `k: 5` stop record; both share the owner list.
 */
function skOwn(n, Sk) {
  const O = n.O;
  for (let i = 0; i < O.length; i++) skPaints(O[i], Sk);
}

/**
 * Rebuild an effect pass's layer image filter from its record fields.
 * `t: 1` is a gaussian blur (`MakeBlur`, tile mode from the effect's edge
 * behaviour); `t: 2` is a drop shadow (`MakeDropShadow` draws shadow AND
 * content — exactly the `feMerge(shadow, source)` the SVG chain ends with),
 * with the flood opacity folded into the shadow colour's alpha
 * (`Skia.Color` returns a mutable RGBA Float32Array).
 */
export function skFxPaint(n, Sk) {
  if (n.t === 1) {
    n.P.setImageFilter(Sk.ImageFilter.MakeBlur(n.sx, n.sy, n.tm, null));
  } else {
    const col = Sk.Color(n.c);
    col[3] = n.fo;
    n.P.setImageFilter(Sk.ImageFilter.MakeDropShadow(n.dx, n.dy, n.sd, n.sd, col, null));
  }
}
