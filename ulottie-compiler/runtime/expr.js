// Expression runtime.
//
// A Lottie expression is a Bodymovin-transpiled JS body that runs against an
// After Effects vocabulary: `value`, `thisLayer`, `thisProperty`, vector
// helpers, and a path API. The compiler emits those bodies as functions in
// `E[]` and a layer table in `D.y` shaped exactly the way the readers below
// take it, so nothing translates between wire and runtime.
//
// What a body says about another layer is decided at build time, not here.
// `thisComp.layer('wire')` is a name the planner already resolved, so it
// arrives as `lyAt(thisLayer, 8)` — one indexation into the table the record
// carries — and the method surface arrives as free calls over that record.
// There is no proxy: a reference the compiler cannot resolve fails the build
// rather than falling back to one, so nothing here looks a layer up by name.
//
// Bundled only when the animation has expressions.

import { records, record, lyLink } from './rec.js';
import { H_ASSETS, H_USES, A_STRIDE, A_RECORDS, U_STRIDE } from './wire.js';
import { column } from './col.js';

/**
 * Install the expression engine, and build its records from the payload.
 *
 * The two halves are separate because a generated module has records of its
 * own: it calls `initExpr` and fills `ctx.recs` with handles it emitted itself.
 * Only the middle step reads the stream, and a generated module has none.
 */
export function makeExpr(E, ctx) {
  initExpr(E, ctx);
  const S = ctx.S;
  // The document's own records, then one *fresh* set per instantiation: a
  // precomp's properties are stored once and shared, but each instance runs on
  // its own clock, and one shared evaluator would drag its keyframe cursor
  // between clocks on every frame.
  ctx.recs = records(ctx, ctx.y);
  const uses = S[H_USES], assets = S[H_ASSETS];
  for (let u = 0, n = uses ? S[uses] : 0; u < n; u++) {
    const row = uses + 1 + u * U_STRIDE;
    // Built *before* its records, so the handles they resolve to close over
    // the instantiation they belong to. Safe only because `ctx.expr(h, at)`
    // merely captures `at` and reads `at.recs` at frame time — make `resolve`
    // eager about `at.recs` and every instanced expression breaks at mount.
    // One field, read by `record()`. The asset, record and scope bases the
    // wire row also carries were only ever read by the name lookup, which no
    // emitted body performs any more.
    const a = {};
    a.recs = records(ctx, column(S, S[assets + 1 + S[row] * A_STRIDE + A_RECORDS]), a);
    ctx.byUse[u] = a;
  }
  // Only a module that still has a body looking a layer up by name passes
  // one; everything else resolved its references at build time, and the two
  // maps, the scope column and the proxy view all leave with it.
  return ctx.expr;
}

export function initExpr(E, ctx) {
  ctx.E = E;
  ctx.recs = [];
  ctx.byUse = [];                 // instantiation → { recs, scope }
  // Installed before anything is resolved, not after. Materializing records
  // resolves every layer property, and `resolve` asks `ctx.expr` whether there
  // is an engine to hand an expression to — with the assignment deferred to the
  // return, every expression-driven property silently became its own constant
  // fallback, with nothing in the console to say so.
  ctx.expr = (p, at) => (f) => evalExpr(p, f, ctx, at);
  // Created up front rather than on first use: the ops read `x.S` and `x.z` off
  // this object on every property, and adding a field mid-animation would
  // change its shape underneath them.
  ctx.memo = new Map();
  ctx.tp = new Map();
  ctx.logged = new Set();
  attachHelpers(ctx);
  return ctx.expr;
}

/**
 * Evaluate a property handle, copying vector results so callers cannot alias
 * the evaluator's scratch buffer.
 *
 * A handle is already resolved — there is nothing left to cache, which is what
 * the per-consumer evaluator cache used to be for. Records are materialized per
 * instantiation instead, so the keyframe cursors stay separate by construction.
 */
function readProp(h, frame, fallback) {
  if (!h) return fallback;
  const v = h(frame);
  return Array.isArray(v) ? v.slice() : v;
}

function evalExpr(p, frame, ctx, at) {
  // `p` is a handle: { x: expression id, src: value source, l: layer index }.
  const id = p.x;
  const fn = ctx.E[id];
  // A property inside a precomp names its layer relative to that precomp, so
  // the same property resolves to a different record per instantiation.
  const rec = p.l !== undefined ? record(ctx, at, p.l) : null;
  if (!fn) return baseValue(ctx, p, frame);

  // Within a frame an expression is a pure function of (property, layer), so
  // repeated reads collapse to one evaluation. Keyed per property object —
  // one expression id serves every property it was applied to.
  // Keyed per record: instances share the wire property but not the record.
  const memo = rec ? (rec._m || (rec._m = new Map())) : ctx.memo;
  const hit = memo.get(p);
  if (hit && hit.f === frame) return hit.v;

  const prevFrame = ctx.frame;
  // `loopOut` and the fallback view's getters read `ctx.frame`; inside a
  // precomp the binding runs on a shifted clock, so they have to see that one.
  // Reading another layer re-enters here, which is why it is saved.
  ctx.frame = frame;
  try {
    const v = fn(baseValue(ctx, p, frame), rec, thisPropertyFor(ctx, p), frame, ctx);
    memo.set(p, { f: frame, v });
    return v;
  } catch (err) {
    if (!ctx.logged.has(id)) {
      ctx.logged.add(id);
      console.warn(`ulottie: expression E[${id}] threw:`, err.message);
    }
    // Memoize the fallback too. A throwing expression is still a pure function
    // of (property, layer, frame), and without this it re-runs — inside a
    // try/catch — on every read rather than once per frame. That is not a
    // rounding error: it was 84% of instanced `ripple`'s frame time.
    const v = baseValue(ctx, p, frame);
    memo.set(p, { f: frame, v });
    return v;
  } finally {
    ctx.frame = prevFrame;
  }
}

/** What the expression sees as `value`: the property's own keyframes or static. */
function baseValue(ctx, p, frame) {
  return readProp(p.src, frame, 0);
}

// ---------------------------------------------------------------------------
// thisProperty
// ---------------------------------------------------------------------------

function thisPropertyFor(ctx, p) {
  // Keyed by offset rather than hung off the property: the property is an
  // integer now, and there is nothing to hang anything on.
  const cache = ctx.tp;
  const hit = cache.get(p);
  if (hit) return hit;
  const src = p.src;
  let tp;
  if (src && src.kf) {
    // Keyframed: exposes the key / velocity / loop API.
    tp = keyedProperty(ctx, src);
  } else if (src && src.pathv) {
    // A path property. Expressions that rewrite a shape read its geometry
    // straight off `thisProperty`.
    tp = pathProperty(src.pathv);
  } else {
    tp = stubProperty(ctx, p);
  }
  cache.set(p, tp);
  return tp;
}

function pathProperty(path) {
  return {
    numKeys: 0,
    v: path.v,
    i: path.i,
    o: path.o,
    c: path.c,
    points: () => pairs(path),
    inTangents: () => pairs(path, 'i'),
    outTangents: () => pairs(path, 'o'),
    isClosed: () => !!path.c,
    propertyGroup: () => (() => true),
  };
}

/** Slice one keyframe's value out of the columnar layout. */
function keyValue(a, i) {
  if (a.kind === 2 || a.d === 1) return a.v[i];
  return a.v.slice(i * a.d, i * a.d + a.d);
}

function keyedProperty(ctx, src) {
  const fr = ctx.fr;
  const a = src.kf;
  const n = a.t.length;
  /** Keyframe time `i`, in frames. */
  const kt = (i) => a.t[i];
  // The handle is the evaluator; there is nothing to resolve.
  const ev = src;
  const at = (frame) => {
    const v = ev(frame);
    return Array.isArray(v) ? v.slice() : v;
  };
  return {
    numKeys: n,
    key(i) {
      const k = Math.max(0, Math.min(n - 1, i - 1));
      return { index: i, time: kt(k) / fr, value: keyValue(a, k) };
    },
    nearestKey(time) {
      const tf = time * fr;
      let best = 0, dist = Infinity;
      for (let i = 0; i < n; i++) {
        const d = Math.abs(kt(i) - tf);
        if (d < dist) { dist = d; best = i; }
      }
      return { index: best + 1, time: kt(best) / fr };
    },
    valueAtTime(time) { return at(time * fr); },
    velocityAtTime(time) {
      // Centred difference. A backward difference is closer to what
      // lottie-web's expressionHelpers documents, but measured against the
      // reference renderer this matches better — notably at t=0, where a
      // backward sample falls before the first keyframe and clamps.
      const dt = 0.001;
      const a = at((time - dt / 2) * fr);
      const b = at((time + dt / 2) * fr);
      const inv = 1 / dt;
      return Array.isArray(a) ? a.map((x, i) => (b[i] - x) * inv) : (b - a) * inv;
    },
    loopOut(mode) {
      const t0 = kt(0), tN = kt(n - 1);
      const span = tN - t0;
      if (span <= 0) return at(t0);
      const f = ctx.frame;
      if (f <= tN) return at(f);
      const past = f - tN;
      if (mode === 'pingpong' || mode === 'pingPong') {
        const cycles = Math.floor(past / span);
        const r = past - cycles * span;
        return at(cycles % 2 === 0 ? tN - r : t0 + r);
      }
      return at(t0 + (past - Math.floor(past / span) * span));
    },
    propertyGroup: () => (() => true),
  };
}

function stubProperty(ctx, p) {
  const v = () => baseValue(ctx, p, ctx.frame, null);
  return {
    numKeys: 0,
    key: (i) => ({ time: 0, value: v(), index: i }),
    nearestKey: () => ({ index: 1, time: 0 }),
    valueAtTime: v,
    velocityAtTime: () => 0,
    loopOut: v,
    propertyGroup: () => (() => true),
  };
}

// ---------------------------------------------------------------------------
// Layer references
//
// A record is the handle. Resolving a reference is picking a slot out of the
// table the record already carries, which is why all three of these are one
// indexation with no map and no global index space behind them.
//
// Each answers `undefined` on a miss. That is deliberate: the proxy these
// replaced answered `null`, and the guard After Effects generates is
// `x != null`, which the two satisfy identically.
// ---------------------------------------------------------------------------

/** Record `i` of `rec`'s own table — an absolute slot every use agreed on. */
export const lyAt = (rec, i) => rec && rec._t[i];

/**
 * The record `d` slots along from `rec`.
 *
 * One body serves every property it was applied to, and inside an inlined
 * precomp those sit at twenty-three different places in the table. The
 * absolute index differs per use; the offset from the owner does not.
 */
export const lyRel = (rec, d) => rec && rec._t[rec._i + d];

/** `X.parentLayer`. Undefined when there is no parent, which ends a walk. */
export const lyParent = (rec) => rec && rec._t[rec.pr];

// ---------------------------------------------------------------------------
// Layer access
//
// Defaults are written per call rather than hoisted: `readProp` copies arrays
// because the keyframe evaluator hands back one shared scratch buffer, and a
// shared default literal would reintroduce exactly the aliasing that copy
// exists to prevent. They stay in step with `flat::RECORD_DEFAULTS` and
// ops/layer.js — opacity defaults to 100, not 0.
// ---------------------------------------------------------------------------

export const lyPos = (rec, f) => readProp(rec && rec.p, f, [0, 0, 0]);
export const lyAnchor = (rec, f) => readProp(rec && rec.a, f, [0, 0, 0]);
export const lyScale = (rec, f) => readProp(rec && rec.sc, f, [100, 100, 100]);
export const lyRot = (rec, f) => readProp(rec && rec.r, f, 0);
export const lyOpacity = (rec, f) => readProp(rec && rec.o, f, 100);

/**
 * The layer's first path shape, or null.
 *
 * Handed back by identity — `readProp` only copies arrays — so `arcTable`'s
 * `path._arc` cache still finds its table on the second read within a frame.
 */
export const lyPath = (rec, f) => readProp(rec && rec.h, f, null);

/** The four fields the space walks compose. `o` takes no part in a transform. */
function localTransform(rec, f) {
  return { p: lyPos(rec, f), a: lyAnchor(rec, f), s: lyScale(rec, f), r: lyRot(rec, f) };
}

export const lyPoints = (rec, f, k) => pairs(lyPath(rec, f), k);
export const lyClosed = (rec, f) => { const p = lyPath(rec, f); return !!(p && p.c); };

/**
 * The parameter object `X.effect(name)(param)` selects, or null.
 *
 * `name` and `param` may each be a number or a string. The dual form stays
 * because a name only becomes a slot when every use of a shared body agrees on
 * which slot it meant — `expr::resolve` for the owning layer's own effects,
 * `backend::layers::render_effect` for anyone else's — and both spellings then
 * ship in one module.
 */
function effectParam(rec, name, param) {
  const list = (rec && rec.ef) || [];
  let e = null;
  if (typeof name === 'number') e = list[name];
  else for (const x of list) if (x.nm === name || x.mn === name) { e = x; break; }
  if (!e) return null;
  if (typeof param === 'number') return e.ef[param] || null;
  for (const p of e.ef) if (p.nm === param || p.mn === param) return p;
  return null;
}

/** A parameter's value: a constant on the wire, or a property to evaluate. */
const effectValue = (ep, f) => (ep.v !== undefined ? ep.v : readProp(ep.p, f, 0));

/**
 * `X.effect(name)(param)`, uncurried — the *value* of the parameter.
 *
 * Answers 0 for a missing layer, effect or parameter, which is what the
 * emitted `() => 0` shim produced and what the bodies guarding on it expect.
 *
 * Value-only, deliberately: a parameter of type 10 is a layer control naming
 * another layer, and turning it into one needs the composition-scope index that
 * only the fallback builds. `render_effect` refuses a body that reads one, so
 * nothing reaches this with a layer control in hand.
 */
export function lyEffect(rec, name, param, f) {
  const ep = effectParam(rec, name, param);
  return ep ? effectValue(ep, f) : 0;
}

/** Flat `[x,y,…]` → `[[x,y],…]`, the shape AE's path accessors return. */
function pairs(path, key) {
  if (!path) return [];
  const src = key ? path[key] : path.v;
  if (!src) return path.v ? new Array(path.v.length >> 1).fill(0).map(() => [0, 0]) : [];
  const out = [];
  for (let i = 0; i < src.length; i += 2) out.push([src[i], src[i + 1]]);
  return out;
}

/**
 * A point in layer space, expressed in the composition's.
 *
 * The parent walk is `rec._t[rec.pr]` — the record's own table, indexed by the
 * parent link it already carries — so a level costs one indexation. Opacity is
 * read at no level: it takes no part in the transform.
 */
export function toComp(rec, point, f) {
  let p = [point[0], point[1]];
  let l = rec;
  while (l) {
    const t = localTransform(l, f);
    let x = p[0] - (t.a[0] || 0);
    let y = p[1] - (t.a[1] || 0);
    x *= (t.s[0] || 100) / 100;
    y *= (t.s[1] || 100) / 100;
    if (t.r) {
      const rad = (typeof t.r === 'number' ? t.r : t.r[0]) * Math.PI / 180;
      const cs = Math.cos(rad), sn = Math.sin(rad);
      [x, y] = [x * cs - y * sn, x * sn + y * cs];
    }
    p = [x + (t.p[0] || 0), y + (t.p[1] || 0)];
    l = lyParent(l);
  }
  return p;
}

/** The inverse, applied outermost first — hence the stack. */
export function fromCompToSurface(point, rec, f) {
  const stack = [];
  for (let l = rec; l; l = lyParent(l)) stack.unshift(l);
  let p = [point[0], point[1]];
  for (const lyr of stack) {
    const t = localTransform(lyr, f);
    let x = p[0] - (t.p[0] || 0);
    let y = p[1] - (t.p[1] || 0);
    if (t.r) {
      const rad = -(typeof t.r === 'number' ? t.r : t.r[0]) * Math.PI / 180;
      const cs = Math.cos(rad), sn = Math.sin(rad);
      [x, y] = [x * cs - y * sn, x * sn + y * cs];
    }
    x = x * 100 / (t.s[0] || 100);
    y = y * 100 / (t.s[1] || 100);
    p = [x + (t.a[0] || 0), y + (t.a[1] || 0)];
  }
  return p;
}

// ---------------------------------------------------------------------------
// ctx helpers the expression bodies destructure
// ---------------------------------------------------------------------------

function attachHelpers(ctx) {
  ctx.frameRate = ctx.fr;
  const zip = (op, unit) => (a, b) => {
    if (Array.isArray(a) && Array.isArray(b)) return a.map((v, i) => op(v, b[i] ?? unit));
    if (Array.isArray(a)) return a.map((v) => op(v, b));
    if (Array.isArray(b)) return b.map((v) => op(a, v));
    return op(a, b);
  };
  ctx.sum = zip((a, b) => a + b, 0);
  ctx.sub = zip((a, b) => a - b, 0);
  ctx.mul = zip((a, b) => a * b, 1);
  ctx.div = zip((a, b) => a / b, 1);
  ctx.clamp = (v, lo, hi) =>
    Array.isArray(v) ? v.map((x) => Math.max(lo, Math.min(hi, x))) : Math.max(lo, Math.min(hi, v));
  ctx.radiansToDegrees = (r) => r * 180 / Math.PI;
  ctx.degreesToRadians = (d) => d * Math.PI / 180;
  ctx.createPath = createPath;
  ctx.pointOnPath = pointOnPath;
  ctx.tangentOnPath = tangentOnPath;
}

function createPath(verts, inTan, outTan, closed) {
  const flat = (src) => {
    const out = [];
    for (const p of src || []) out.push(p[0], p[1]);
    return out;
  };
  const p = { v: flat(verts), i: flat(inTan), o: flat(outTan), c: closed ? 1 : 0 };
  p.points = () => pairs(p);
  p.inTangents = () => pairs(p, 'i');
  p.outTangents = () => pairs(p, 'o');
  p.isClosed = () => !!p.c;
  return p;
}

// ---------------------------------------------------------------------------
// Arc-length path sampling
// ---------------------------------------------------------------------------

const ARC = 300;

/**
 * Cumulative arc lengths for a path, cached on the path object. Expression
 * bodies re-read the same path every frame, and rebuilding this was the single
 * largest per-frame cost in the corpus.
 */
function arcTable(path) {
  if (path._arc) return path._arc;
  const v = path.v, ti = path.i, to = path.o;
  const n = v.length >> 1;
  const segs = path.c ? n : n - 1;
  const cumul = [];
  let total = 0;
  for (let s = 0; s < segs; s++) {
    const a = s * 2, b = ((s + 1) % n) * 2;
    const p0x = v[a], p0y = v[a + 1], p3x = v[b], p3y = v[b + 1];
    const p1x = p0x + (to ? to[a] : 0), p1y = p0y + (to ? to[a + 1] : 0);
    const p2x = p3x + (ti ? ti[b] : 0), p2y = p3y + (ti ? ti[b + 1] : 0);
    const samples = new Float64Array(ARC + 1);
    let acc = 0, px = p0x, py = p0y;
    for (let k = 1; k <= ARC; k++) {
      const t = k / ARC, u = 1 - t;
      const u3 = u * u * u, u2t = 3 * u * u * t, ut2 = 3 * u * t * t, t3 = t * t * t;
      const x = u3 * p0x + u2t * p1x + ut2 * p2x + t3 * p3x;
      const y = u3 * p0y + u2t * p1y + ut2 * p2y + t3 * p3y;
      acc += Math.hypot(x - px, y - py);
      samples[k] = acc;
      px = x; py = y;
    }
    total += acc;
    cumul.push({ samples, len: acc, p: [p0x, p0y, p1x, p1y, p2x, p2y, p3x, p3y] });
  }
  path._arc = { cumul, total };
  return path._arc;
}

/** Locate `(segment, t)` at arc-length fraction `u`. */
function locate(path, u) {
  const tab = arcTable(path);
  if (!tab.cumul.length || tab.total === 0) return null;
  const target = Math.max(0, Math.min(1, u)) * tab.total;
  let acc = 0;
  for (let s = 0; s < tab.cumul.length; s++) {
    const seg = tab.cumul[s];
    if (target <= acc + seg.len || s === tab.cumul.length - 1) {
      const local = target - acc;
      let lo = 0, hi = ARC;
      while (lo < hi) {
        const m = (lo + hi) >> 1;
        if (seg.samples[m] < local) lo = m + 1; else hi = m;
      }
      const up = lo, low = up > 0 ? up - 1 : 0;
      const dl = seg.samples[low], dh = seg.samples[up];
      const f = dh === dl ? 0 : (local - dl) / (dh - dl);
      return { p: seg.p, t: Math.min(1, Math.max(0, (low + f) / ARC)) };
    }
    acc += seg.len;
  }
  return null;
}

function pointOnPath(path, u) {
  // A layer with no path shape reads as null. The proxy used to guard this at
  // the call site; now the sampler owns it, so both paths answer the same.
  if (!path) return [0, 0];
  const at = locate(path, u);
  if (!at) return [path.v[0] || 0, path.v[1] || 0];
  const p = at.p, t = at.t, m = 1 - t;
  const u3 = m * m * m, u2t = 3 * m * m * t, ut2 = 3 * m * t * t, t3 = t * t * t;
  return [
    u3 * p[0] + u2t * p[2] + ut2 * p[4] + t3 * p[6],
    u3 * p[1] + u2t * p[3] + ut2 * p[5] + t3 * p[7],
  ];
}

function tangentOnPath(path, u) {
  if (!path) return [1, 0];
  const at = locate(path, u);
  if (!at) return [1, 0];
  const p = at.p, t = at.t, m = 1 - t;
  return [
    3 * m * m * (p[2] - p[0]) + 6 * m * t * (p[4] - p[2]) + 3 * t * t * (p[6] - p[4]),
    3 * m * m * (p[3] - p[1]) + 6 * m * t * (p[5] - p[3]) + 3 * t * t * (p[7] - p[5]),
  ];
}
