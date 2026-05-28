// μlottie data-driven runtime.
//
// `run(data, container, exprs?)` decodes a Payload (see src/data/mod.rs for
// the wire format) and renders the animation as an <svg>. One driver, many
// animations.
//
// Phase D1 scope: shape layers (ty=4), rect / ellipse / path / group, static
// and keyframe-animated transforms + style properties, fill / stroke / trim.
// No expressions, no precomps yet — both arrive in later phases.

const NS = 'http://www.w3.org/2000/svg';

// Compile-time feature flags. The dev runtime keeps all features on so
// driver.js runs unchanged when imported as a shared module (extern mode).
// In embedded mode the compiler replaces these declarations with the actual
// flags before running the result through a minifier — `if (HAS_FOO)` gates
// fold away, the now-unreferenced functions get DCE'd, and the embedded
// output ships only the runtime the animation actually uses.
const HAS_EXPRESSIONS = true;
const HAS_TRIM_PATH = true;
const HAS_GRADIENT = true;

export function run(data, container, exprs = []) {
  let currentFrame = 0;
  // The root composition's layer scope. Each precomp instance later adds its
  // own scope on top so `thisComp.layer('name')` from inside a precomp finds
  // siblings within the same precomp, not whichever layer happened to be
  // built last across all instances.
  const rootScope = newScope();
  // `ctx` carries the per-animation state that runtime helpers + compiled
  // expression functions need. Expression bodies receive `ctx` and dispatch
  // through it (e.g. `ctx.thisComp.layer('foo')`).
  const ctx = {
    data,
    exprs,
    currentFrame: 0,
    frameRate: data.c.fr,
    // Root-level scope, also tracked separately as `_currentScope` for
    // expression dispatch. Inner precomp expressions see their own scope.
    layersByName: rootScope.byName,
    layersByIndex: rootScope.byIndex,
    rootScope,
    _currentScope: rootScope,
    thisComp: null,
    // Math + path helpers attached after creation, so expression code that
    // closes over `ctx` sees them.
  };
  if (HAS_EXPRESSIONS) attachExpressionRuntime(ctx);

  // SVG root + a single shared <defs> for gradients.
  const svg = document.createElementNS(NS, 'svg');
  svg.setAttribute('viewBox', `0 0 ${data.c.w} ${data.c.h}`);
  // Match lottie-web: render at the container's size, not the comp's native
  // pixel dimensions. This way a 120×120 voice-listening fixture fills a
  // 300×300 panel via SVG viewBox scaling instead of rendering tiny in the
  // corner. Width/height attributes still set to the comp values so the
  // SVG has an intrinsic aspect ratio when sized "auto".
  svg.setAttribute('width', '100%');
  svg.setAttribute('height', '100%');
  svg.setAttribute('preserveAspectRatio', 'xMidYMid meet');
  svg.style.overflow = 'hidden';
  const defs = document.createElementNS(NS, 'defs');
  svg.appendChild(defs);
  ctx.defs = defs;
  ctx.gradientCount = 0;
  ctx.maskCount = 0;
  // Cache: style id → gradient element id. Lets us share one <linearGradient>
  // across all shapes that reference the same style.
  ctx.gradientCache = new Map();

  // Build layer DOM + wire parent → child relationships for expression
  // `thisLayer.parent` and (critically) DOM nesting: children must live
  // *inside* their parent's <g> so SVG's transform inheritance carries the
  // parent's transform onto them. Without nesting, the eyes/mouth of a
  // starfish would render at the origin instead of on the parent's face.
  const layerInfos = data.l.map(layer => buildLayer(layer, ctx, rootScope));
  for (let i = 0; i < data.l.length; i++) {
    const layer = data.l[i];
    if (layer.pr !== undefined) {
      const parent = layerInfos[layer.pr];
      if (parent) layerInfos[i].proxy.parentLayer = parent.proxy;
    }
  }
  // DOM nesting + Z-order. We walk top-down (source order = top-first),
  // appending each layer under its parent's outerG (or the root SVG if no
  // parent). To match Lottie's z-order (first-listed = top), we reverse the
  // iteration so the first-listed layer is appended LAST among its siblings
  // (which puts it on top in SVG paint order).
  for (let i = data.l.length - 1; i >= 0; i--) {
    const layer = data.l[i];
    const info = layerInfos[i];
    if (layer.pr !== undefined && layerInfos[layer.pr]) {
      // Children mount inside the parent's *inner* group so the parent's
      // opacity scope doesn't multiply-stack onto them; the parent's outer
      // transform still applies via SVG inheritance.
      layerInfos[layer.pr].outerG.appendChild(info.outerG);
    } else {
      svg.appendChild(info.outerG);
    }
  }

  function updateFrame(frame) {
    currentFrame = frame;
    ctx.currentFrame = frame;
    for (const info of layerInfos) updateLayer(info, frame, ctx);
  }

  // Initial render so even paused players have a valid frame 0.
  updateFrame(0);

  // rAF loop.
  let startTime = null;
  let rafId = null;
  function animate(ts) {
    if (startTime === null) startTime = ts;
    const elapsed = (ts - startTime) / 1000;
    const span = data.c.op - data.c.ip;
    const frame = data.c.ip + ((elapsed * data.c.fr) % span);
    updateFrame(frame);
    rafId = requestAnimationFrame(animate);
  }
  rafId = requestAnimationFrame(animate);

  container.appendChild(svg);

  return {
    svg,
    totalFrames: data.c.op - data.c.ip,
    frameRate: data.c.fr,
    destroy() {
      if (rafId !== null) cancelAnimationFrame(rafId);
      rafId = null;
    },
    goToFrame(f) {
      if (rafId !== null) cancelAnimationFrame(rafId);
      rafId = null;
      startTime = null;
      updateFrame(f);
    },
  };
}

// ---------------------------------------------------------------------------
// Property evaluation
// ---------------------------------------------------------------------------

function evalProp(pid, frame, ctx, thisLayer, thisProperty) {
  const p = ctx.data.p[pid];
  if (p.k !== undefined) return p.k;
  // Expression takes precedence over kf. The kf entry on an ExprProp is the
  // *underlying* value source the expression reads through `thisProperty`
  // (`valueAtTime`, `loopOut`, etc.) — not a fallback for the property's
  // current value. Reading kf directly would skip the expression entirely.
  if (HAS_EXPRESSIONS && p.e !== undefined) {
    const fn = ctx.exprs[p.e];
    if (!fn) return p.fb ?? 0;
    // Build a `thisProperty` appropriate for the underlying value type.
    // Paths get the path API (.points/.inTangents/...); keyframe-driven
    // scalar/vec properties get the AE keyframe API (.numKeys, .key(),
    // .nearestKey(), .valueAtTime(), .velocityAtTime()). Static fallbacks
    // present a minimal stub.
    let tp = thisProperty;
    if (!tp) {
      tp = makeThisProperty(p, ctx);
    }
    // Switch into the executing layer's scope so `thisComp.layer('name')` in
    // the expression resolves against the right composition (each precomp
    // instance has its own scope). Restore the previous scope after eval so
    // nested expression calls work correctly.
    const prevScope = ctx._currentScope;
    if (thisLayer && thisLayer._scope) ctx._currentScope = thisLayer._scope;
    // `value` inside the expression is the property's underlying value at
    // the current time — keyframe-interpolated when animated, the static
    // fallback otherwise. Bouncy-overshoot expressions add their oscillation
    // on top of this, so feeding them `undefined` collapses the base
    // position to NaN.
    const baseValue = p.kf
      ? interpolateKf(p.kf, frame)
      : (p.fb !== undefined ? p.fb : 0);
    try {
      return fn(baseValue, thisLayer || null, tp || null, frame, ctx);
    } catch (err) {
      if (!ctx._loggedExprErrors) ctx._loggedExprErrors = new Set();
      if (!ctx._loggedExprErrors.has(p.e)) {
        ctx._loggedExprErrors.add(p.e);
        console.warn(`ulottie: expression E[${p.e}] threw:`, err.message);
      }
      // Prefer keyframes when the expression can't run: that's the property's
      // underlying value source. A property that's *only* expression-driven
      // (no kf, no fb) returns 0 as a last resort.
      if (p.kf) return interpolateKf(p.kf, frame);
      return p.fb ?? 0;
    } finally {
      ctx._currentScope = prevScope;
    }
  }
  // Plain keyframe-driven property (no expression). Interpolate the keyframes
  // at the current frame.
  if (p.kf) return interpolateKf(p.kf, frame);
  if (p.d !== undefined) return evalPattern(p, frame, ctx);
  return 0;
}

// Construct the `thisProperty` object expression bodies see. AE expressions
// query thisProperty for the underlying value source: `numKeys`, `key(n)`,
// `nearestKey(time)`, `valueAtTime(t)`, `velocityAtTime(t)`. Without these,
// bouncy-overshoot patterns (which are how the starfish face and lights
// nulls animate) all evaluate to zero.
function makeThisProperty(p, ctx) {
  // Path fallback wears its v/i/o/c attributes (lights wire path expr).
  if (p.fb && typeof p.fb === 'object' && p.fb.v) {
    const path = p.fb;
    return {
      v: path.v,
      i: path.i,
      o: path.o,
      c: path.c,
      points: () => path.v.map(pt => pt.slice()),
      inTangents: () => path.i.map(pt => pt.slice()),
      outTangents: () => path.o.map(pt => pt.slice()),
      isClosed: () => !!path.c,
      propertyGroup: () => (() => true),
      numKeys: 0,
    };
  }
  // Keyframed fallback: expose the keyframe API. Frame rate stays in `ctx`.
  if (p.kf && p.kf.t && p.kf.t.length > 0) {
    const kf = p.kf;
    const fr = ctx.frameRate;
    const numKeys = kf.t.length;
    return {
      numKeys,
      key(n) {
        const idx = Math.max(0, Math.min(numKeys - 1, n - 1));
        return { index: n, time: kf.t[idx] / fr, value: kf.v[idx] };
      },
      nearestKey(time) {
        const tf = time * fr;
        let nearest = 0, best = Infinity;
        for (let i = 0; i < numKeys; i++) {
          const d = Math.abs(kf.t[i] - tf);
          if (d < best) { best = d; nearest = i; }
        }
        return { index: nearest + 1, time: kf.t[nearest] / fr };
      },
      valueAtTime(time) {
        return interpolateKf(kf, time * fr);
      },
      velocityAtTime(time) {
        // Symmetric finite-difference matching lottie-web's `dt = 0.001s`.
        const dt = 0.001;
        const a = interpolateKf(kf, (time - dt / 2) * fr);
        const b = interpolateKf(kf, (time + dt / 2) * fr);
        const inv = 1 / dt;
        if (Array.isArray(a)) return a.map((x, i) => (b[i] - x) * inv);
        return (b - a) * inv;
      },
      // Wrap the time outside [t[0], t[-1]] back into range. Modes:
      //   "cycle"     — repeat (default)
      //   "pingpong"  — alternate forward/backward
      //   "offset"    — repeat + accumulate the value delta each cycle
      //   "continue"  — extrapolate from the last segment's velocity
      // The ripple fixture's traceNull Progress relies on "cycle".
      loopOut(mode = 'cycle', _numKeyframes) {
        const t0 = kf.t[0];
        const tN = kf.t[numKeys - 1];
        const span = tN - t0;
        if (span <= 0) return kf.v[0];
        const tf = ctx.currentFrame;
        if (tf <= tN) return interpolateKf(kf, tf);
        const past = tf - tN;
        const m = mode === 'pingpong' || mode === 'pingPong' ? 'pingpong' : 'cycle';
        if (m === 'pingpong') {
          const cycles = Math.floor(past / span);
          const r = past - cycles * span;
          const t = cycles % 2 === 0 ? tN - r : t0 + r;
          return interpolateKf(kf, t);
        }
        // cycle
        const r = past - Math.floor(past / span) * span;
        return interpolateKf(kf, t0 + r);
      },
      propertyGroup: () => (() => true),
    };
  }
  // Static fallback or empty: minimal stub.
  return {
    numKeys: 0,
    nearestKey: () => ({ index: 1, time: 0 }),
    key: (n) => ({ time: 0, value: p.fb ?? 0, index: n }),
    valueAtTime: () => p.fb ?? 0,
    velocityAtTime: () => 0,
    propertyGroup: () => (() => true),
  };
}

// Placeholder; Phase D4 fills this in.
function evalPattern(_p, _frame, _ctx) {
  return 0;
}

function interpolateKf(kf, frame) {
  const n = kf.t.length;
  if (frame <= kf.t[0]) return kf.v[0];
  if (frame >= kf.t[n - 1]) {
    // The Lottie convention is that the final keyframe carries only `t` (no
    // `s`/value) because there's no segment starting there. The "value at the
    // end" lives on the previous segment's `e[n-2]`. Fall back through the
    // options so we never hand back the empty marker `[]`.
    const lastE = kf.e?.[n - 1];
    if (lastE != null && !(Array.isArray(lastE) && lastE.length === 0)) return lastE;
    const lastV = kf.v[n - 1];
    if (lastV != null && !(Array.isArray(lastV) && lastV.length === 0)) return lastV;
    const prevE = kf.e?.[n - 2];
    if (prevE != null) return prevE;
    return kf.v[n - 2];
  }
  for (let i = 0; i < n - 1; i++) {
    const at = kf.t[i];
    const bt = kf.t[i + 1];
    if (frame >= at && frame <= bt) {
      const dt = bt - at;
      if (dt === 0) return kf.v[i + 1];
      const progress = (frame - at) / dt;

      // Easing
      let t = progress;
      const oi = kf.oi?.[i];
      if (oi) {
        const ox = pick(oi.o.x);
        const oy = pick(oi.o.y);
        const ix = pick(oi.i.x);
        const iy = pick(oi.i.y);
        t = cubicBezier(ox, oy, ix, iy, progress);
      }

      const startV = kf.v[i];
      const endV = kf.e?.[i] ?? kf.v[i + 1];
      const spatialOut = kf.to?.[i];
      const spatialIn = kf.ti?.[i];
      return lerpValue(startV, endV, t, spatialOut, spatialIn);
    }
  }
  return kf.v[n - 1];
}

function pick(v) {
  return Array.isArray(v) ? v[0] : v;
}

function lerpValue(a, b, t, spatialOut, spatialIn) {
  if (typeof a === 'number') return a + (b - a) * t;
  // Path-valued keyframes (animated bezier shapes — used by layer masks like
  // the starfish wink). Interpolate v/i/o componentwise; `c` snaps to b at
  // t > 0.5. Endpoint paths must agree on vertex count.
  if (a && typeof a === 'object' && Array.isArray(a.v)) {
    if (!b || !Array.isArray(b.v) || b.v.length !== a.v.length) {
      return a;
    }
    const out = { v: [], i: [], o: [], c: t < 0.5 ? a.c : b.c };
    for (let k = 0; k < a.v.length; k++) {
      out.v.push([
        a.v[k][0] + (b.v[k][0] - a.v[k][0]) * t,
        a.v[k][1] + (b.v[k][1] - a.v[k][1]) * t,
      ]);
      out.i.push([
        a.i[k][0] + (b.i[k][0] - a.i[k][0]) * t,
        a.i[k][1] + (b.i[k][1] - a.i[k][1]) * t,
      ]);
      out.o.push([
        a.o[k][0] + (b.o[k][0] - a.o[k][0]) * t,
        a.o[k][1] + (b.o[k][1] - a.o[k][1]) * t,
      ]);
    }
    return out;
  }
  const dim = a.length;
  const out = new Array(dim);
  const hasSpatial =
    (spatialOut && (spatialOut[0] || spatialOut[1] || spatialOut[2])) ||
    (spatialIn && (spatialIn[0] || spatialIn[1] || spatialIn[2]));
  if (hasSpatial) {
    // lottie-web parameterizes the spatial cubic bezier by ARC LENGTH, not by
    // bezier parameter. The bouncing ball relies on this — without it the ball
    // appears halfway through its trajectory at the temporal midpoint, instead
    // of near the top (its tangents push the curve there). See
    // PropertyFactory.js's `distanceInLine = bezierData.segmentLength * perc`.
    return sampleSpatialBezierByArcLength(a, b, spatialOut, spatialIn, t);
  }
  for (let d = 0; d < dim; d++) {
    out[d] = a[d] + (b[d] - a[d]) * t;
  }
  return out;
}

// Sample a cubic bezier curve at the fraction `t` of its arc length. The
// control points are p0 = `a`, p1 = a + spatialOut, p2 = b + spatialIn,
// p3 = `b`. We pre-sample the curve at fixed parameter steps, accumulate
// segment lengths to get total arc length, then walk through the samples to
// find the point at distance `t * totalArcLength`.
const _SPATIAL_SEGMENTS = 200;
function sampleSpatialBezierByArcLength(a, b, spatialOut, spatialIn, t) {
  const dim = a.length;
  const so = spatialOut || [0, 0, 0];
  const si = spatialIn || [0, 0, 0];
  // Pre-sample points along the curve in parameter space, accumulating
  // straight-line distances between consecutive samples.
  const points = new Array(_SPATIAL_SEGMENTS);
  const partials = new Array(_SPATIAL_SEGMENTS);
  let total = 0;
  let prev = null;
  for (let k = 0; k < _SPATIAL_SEGMENTS; k++) {
    const u = k / (_SPATIAL_SEGMENTS - 1);
    const omu = 1 - u;
    const c0 = omu * omu * omu;
    const c1 = 3 * omu * omu * u;
    const c2 = 3 * omu * u * u;
    const c3 = u * u * u;
    const pt = new Array(dim);
    for (let d = 0; d < dim; d++) {
      const p0 = a[d];
      const p3 = b[d];
      const p1 = p0 + (so[d] ?? 0);
      const p2 = p3 + (si[d] ?? 0);
      pt[d] = c0 * p0 + c1 * p1 + c2 * p2 + c3 * p3;
    }
    let dist = 0;
    if (prev) {
      let s = 0;
      for (let d = 0; d < dim; d++) {
        const dd = pt[d] - prev[d];
        s += dd * dd;
      }
      dist = Math.sqrt(s);
    }
    partials[k] = dist;
    total += dist;
    points[k] = pt;
    prev = pt;
  }
  if (total === 0) return points[0];
  // Walk partials until we've covered `t * total`.
  const target = t * total;
  let acc = 0;
  for (let k = 0; k < _SPATIAL_SEGMENTS - 1; k++) {
    const next = acc + partials[k + 1];
    if (next >= target) {
      const segFrac = partials[k + 1] === 0 ? 0 : (target - acc) / partials[k + 1];
      const out = new Array(dim);
      for (let d = 0; d < dim; d++) {
        out[d] = points[k][d] + (points[k + 1][d] - points[k][d]) * segFrac;
      }
      return out;
    }
    acc = next;
  }
  return points[_SPATIAL_SEGMENTS - 1];
}

function cubicBezier(x1, y1, x2, y2, t) {
  let u = t;
  for (let i = 0; i < 8; i++) {
    const omu = 1 - u;
    const x = 3 * omu * omu * u * x1 + 3 * omu * u * u * x2 + u * u * u - t;
    const dx = 3 * omu * omu * x1 + 6 * omu * u * (x2 - x1) + 3 * u * u * (1 - x2);
    if (Math.abs(dx) < 1e-6) break;
    u -= x / dx;
    u = Math.max(0, Math.min(1, u));
  }
  const omu = 1 - u;
  return 3 * omu * omu * u * y1 + 3 * omu * u * u * y2 + u * u * u;
}

// ---------------------------------------------------------------------------
// Layer build / update
// ---------------------------------------------------------------------------

// One layer scope (a composition's namespace for `thisComp.layer('name')`
// lookups and 1-based index lookups). Each precomp instance gets a fresh
// scope so its inner layers don't collide across instances.
function newScope() {
  return { byName: Object.create(null), byIndex: Object.create(null) };
}

function buildLayer(layer, ctx, scope) {
  const outerG = document.createElementNS(NS, 'g');
  // Null layers (ty=3) carry transform but no content of their own; in Lottie
  // their `opacity` field is often set to 0 (as scaffolding scaffolding). We
  // mustn't put a real `opacity=0` on a <g> that nested children rely on for
  // their transform, so for nulls we skip the inner-g wrapper entirely and
  // hang children directly off the outer-g.
  const hasOwnContent = layer.ty !== 3;
  const innerG = hasOwnContent
    ? document.createElementNS(NS, 'g')
    : outerG;
  if (hasOwnContent) outerG.appendChild(innerG);

  // Build SVG mask for the layer if `layer.mk` is present. We create a
  // `<mask>` in <defs> with one `<path>` per mask entry. The path's `d` is
  // updated each frame in updateLayer. Mode `a` (add) means everything
  // inside the path is visible; `s` (subtract) inverts that.
  let masks = null;
  if (layer.mk && layer.mk.length) {
    const maskId = `lmask-${ctx.maskCount++}`;
    const maskEl = document.createElementNS(NS, 'mask');
    maskEl.setAttribute('id', maskId);
    maskEl.setAttribute('mask-type', 'luminance');
    // Background fill: for subtract masks, start with white (everything
    // visible) and the path covers it with black. For add masks, the path
    // itself is white.
    const maskPaths = [];
    for (const m of layer.mk) {
      const path = document.createElementNS(NS, 'path');
      const isSubtract = m.m === 's' || m.inv;
      path.setAttribute('fill', isSubtract ? 'black' : 'white');
      path.setAttribute('fill-rule', 'evenodd');
      maskEl.appendChild(path);
      maskPaths.push({ el: path, mask: m, isSubtract });
    }
    // If any mask is subtract-style, lay down a white background first so
    // the subtract paths cut into something.
    const hasSubtract = maskPaths.some(p => p.isSubtract);
    if (hasSubtract) {
      const bg = document.createElementNS(NS, 'rect');
      bg.setAttribute('x', '0');
      bg.setAttribute('y', '0');
      bg.setAttribute('width', ctx.data.c.w);
      bg.setAttribute('height', ctx.data.c.h);
      bg.setAttribute('fill', 'white');
      maskEl.insertBefore(bg, maskEl.firstChild);
    }
    ctx.defs.appendChild(maskEl);
    // Apply mask to the innerG so the mask path's coordinates (in layer-local
    // space) align with the layer's content. SVG mask coordinates resolve in
    // the user-space where the masked element is referenced; innerG sits
    // *inside* outerG, so its user space already accounts for the layer's
    // transform.
    (hasOwnContent ? innerG : outerG).setAttribute('mask', `url(#${maskId})`);
    masks = maskPaths;
  }

  const shapes = [];
  // Inner-layer infos for precomp instances. Each entry is the same
  // `{ outerG, innerG, layer, shapes, proxy }` shape returned by buildLayer,
  // built for the precomp asset's nested layers and rendered under this
  // outer <g> with the precomp's time-offset applied.
  let precompInner = null;
  if (layer.ty === 4 && layer.shapes) {
    for (const ref of layer.shapes) buildShapeRef(innerG, ref, shapes, ctx);
  } else if (layer.ty === 1) {
    const rect = document.createElementNS(NS, 'rect');
    rect.setAttribute('width', layer.sw ?? 0);
    rect.setAttribute('height', layer.sh ?? 0);
    rect.setAttribute('fill', layer.cl ?? '#000');
    innerG.appendChild(rect);
  } else if (layer.ty === 0 && layer.rf && ctx.data.a && ctx.data.a[layer.rf]) {
    // Precomp instance. Build the asset's inner layers as our children, in a
    // fresh layer-name scope so `thisComp.layer('foo')` from inside any
    // expression resolves to the sibling within *this* instance — not to a
    // homonym in another instance. Each grow-bar in ripple has its own
    // `traceNull` and they must not collide.
    const innerScope = newScope();
    const asset = ctx.data.a[layer.rf];
    const innerLayers = asset.l || [];
    precompInner = innerLayers.map(inner => buildLayer(inner, ctx, innerScope));
    // Wire up parent-layer proxy links (for expressions that walk `.parent`)
    // and figure out which inner layers root the DOM tree (no `pr`).
    for (let j = 0; j < innerLayers.length; j++) {
      const inner = innerLayers[j];
      if (inner.pr !== undefined && precompInner[inner.pr]) {
        precompInner[j].proxy.parentLayer = precompInner[inner.pr].proxy;
      }
    }
    // DOM nesting: same as the top-level layer pass. Children mount inside
    // their parent's outerG so SVG's transform inheritance carries the
    // parent's animated rotation/scale/position to descendants. Without
    // this, a star parented to a rotating "circle" null layer would render
    // at its raw local position instead of orbiting with the parent. The
    // image_llm_loading fixture's star + magnifying glass live inside a
    // "circle" null whose rotation is the entire animation.
    for (let j = innerLayers.length - 1; j >= 0; j--) {
      const inner = innerLayers[j];
      if (inner.pr !== undefined && precompInner[inner.pr]) {
        precompInner[inner.pr].outerG.appendChild(precompInner[j].outerG);
      } else {
        innerG.appendChild(precompInner[j].outerG);
      }
    }
  }

  // Layer proxy — what `thisLayer` resolves to inside an expression.
  // Made callable so chains like
  //   `layer('ADBE Root Vectors Group')(1)('ADBE Vectors Group')(1)('ADBE Vector Shape')`
  // can flow through to the layer's first Path shape, where the
  // `pointOnPath` / `tangentOnPath` methods live. Any non-recognized argument
  // returns the proxy itself, so chained navigation always lands somewhere.
  const name = layer.n !== undefined ? ctx.data.st[layer.n] : null;
  const proxy = function(query) {
    // Property access for chained navigation is handled below by attaching
    // shape methods directly to `proxy` — so we just return ourselves.
    if (query === undefined || typeof query === 'string' || typeof query === 'number') return proxy;
    return proxy;
  };
  // `function.name` is read-only by default; override it so expressions that
  // read `thisLayer.name` work.
  Object.defineProperty(proxy, 'name', { value: name, writable: true, configurable: true });
  proxy.index = layer.i;
  proxy.parentLayer = null; // wired up by the caller
  proxy.transform = null;

  // Hoist the first Path shape's API onto the proxy so expression chains
  // resolve to it. Most AE expressions follow the standard navigation
  // through ADBE Root Vectors Group, and they all end up on the layer's
  // primary shape. Without this, every expression that calls
  // `layer(...)(...).pointOnPath()` would explode.
  if (HAS_EXPRESSIONS) {
    const firstPath = findFirstPathShape(layer, ctx);
    if (firstPath) {
      proxy.pointOnPath = (t) => {
        const path = evalProp(firstPath, ctx.currentFrame, ctx, proxy);
        return path ? pointOnPath(_freezePath(path), t) : [0, 0];
      };
      proxy.tangentOnPath = (t) => {
        const path = evalProp(firstPath, ctx.currentFrame, ctx, proxy);
        return path ? tangentOnPath(_freezePath(path), t) : [1, 0];
      };
      proxy.points = () => {
        const path = evalProp(firstPath, ctx.currentFrame, ctx, proxy);
        return path ? path.v.map(p => p.slice()) : [];
      };
      proxy.inTangents = () => {
        const path = evalProp(firstPath, ctx.currentFrame, ctx, proxy);
        return path ? path.i.map(p => p.slice()) : [];
      };
      proxy.outTangents = () => {
        const path = evalProp(firstPath, ctx.currentFrame, ctx, proxy);
        return path ? path.o.map(p => p.slice()) : [];
      };
      proxy.isClosed = () => {
        const path = evalProp(firstPath, ctx.currentFrame, ctx, proxy);
        return path ? !!path.c : false;
      };
    }
    proxy.toComp = (point) => toComp(proxy, point, ctx);
    proxy.fromCompToSurface = (point) => fromCompToSurface(point, proxy, ctx);
  }
  // Layer's local transform evaluator (used by toComp).
  proxy.getLocalTransform = (frame) => ({
    p: layer.p !== undefined ? evalProp(layer.p, frame, ctx, proxy) : [0, 0, 0],
    a: layer.a !== undefined ? evalProp(layer.a, frame, ctx, proxy) : [0, 0, 0],
    s: layer.sc !== undefined ? evalProp(layer.sc, frame, ctx, proxy) : [100, 100, 100],
    r: layer.r !== undefined ? evalProp(layer.r, frame, ctx, proxy) : 0,
    o: layer.o !== undefined ? evalProp(layer.o, frame, ctx, proxy) : 100,
  });
  // AE/Lottie expression accessors for the layer's own transform values.
  // `position` and `anchorPoint` are the most commonly referenced; we keep
  // them as live getters so they reflect the current frame.
  Object.defineProperty(proxy, 'position', {
    get() {
      return layer.p !== undefined ? evalProp(layer.p, ctx.currentFrame, ctx, proxy) : [0, 0, 0];
    },
  });
  Object.defineProperty(proxy, 'anchorPoint', {
    get() {
      return layer.a !== undefined ? evalProp(layer.a, ctx.currentFrame, ctx, proxy) : [0, 0, 0];
    },
  });
  Object.defineProperty(proxy, 'scale', {
    get() {
      return layer.sc !== undefined ? evalProp(layer.sc, ctx.currentFrame, ctx, proxy) : [100, 100, 100];
    },
  });
  Object.defineProperty(proxy, 'rotation', {
    get() {
      return layer.r !== undefined ? evalProp(layer.r, ctx.currentFrame, ctx, proxy) : 0;
    },
  });
  Object.defineProperty(proxy, 'opacity', {
    get() {
      return layer.o !== undefined ? evalProp(layer.o, ctx.currentFrame, ctx, proxy) : 100;
    },
  });
  // AE-style `thisLayer.transform.position` / `.anchorPoint` / `.scale` /
  // `.rotation` / `.opacity` chains. Each returns a getter-backed object so
  // values stay fresh per frame.
  Object.defineProperty(proxy, 'transform', {
    get() { return proxy; },
  });
  // `content(name)` returns a shape-group accessor. AE expressions chain
  // through this to reach individual paths: `layer.content('Path 1').path`.
  // Our shape tree is flat — the first path's API is hoisted onto the proxy
  // itself, so returning the proxy is a no-op that lets `.path.points()`
  // resolve to the right place. (Multi-shape layers aren't reached by any
  // current fixture.)
  proxy.content = (_name) => proxy;
  // `.path` on a content node points back to the first path's API as well.
  // Defined as a getter so chained `.points()` / `.inTangents()` evaluate
  // against the current frame.
  Object.defineProperty(proxy, 'path', {
    get() { return proxy; },
  });
  // Effects — encoded as a flat array on the layer (`layer.ef`). Each entry
  // is `{ nm, mn, ef: [params] }` mirroring the Lottie source so expressions
  // calling `effect('name')('param')` find what they need. Layer Control
  // params (ty=10) resolve their value through `layersByIndex` so chains like
  // `effect('Foo')('ADBE Layer Control-0001').toComp(...)` see a layer proxy.
  proxy.effect = (n) => {
    if (!layer.ef) return () => 0;
    for (const e of layer.ef) {
      if (e.nm === n || e.mn === n) {
        return (param) => {
          for (const p of e.ef || []) {
            if (p.nm === param || p.mn === param) {
              const raw = p.v !== undefined
                ? p.v
                : (p.p !== undefined ? evalProp(p.p, ctx.currentFrame, ctx, proxy) : 0);
              if (p.ty === 10) {
                const idx = typeof raw === 'number' ? raw : (raw?.[0] ?? 0);
                return ctx.layersByIndex[idx] || null;
              }
              return raw;
            }
          }
          return 0;
        };
      }
    }
    return () => 0;
  };

  if (name) scope.byName[name] = proxy;
  scope.byIndex[layer.i] = proxy;
  // Each proxy carries its own scope so any expression evaluated with this
  // proxy as `thisLayer` resolves `thisComp.layer(...)` against the right
  // composition.
  proxy._scope = scope;

  return { outerG, innerG, layer, shapes, proxy, precompInner, masks };
}

function buildShapeRef(parent, shapeRef, accum, ctx) {
  // GroupRef variant — has a `c` (children) array. The data backend uses this
  // to wrap children in a nested <g> with its own animated transform.
  if (shapeRef.c) {
    const g = document.createElementNS(NS, 'g');
    parent.appendChild(g);
    const groupChildren = [];
    for (const child of shapeRef.c) {
      buildShapeRef(g, child, groupChildren, ctx);
    }
    accum.push({ kind: 'group', el: g, ref: shapeRef, children: groupChildren });
    return;
  }
  // PrimRef variant. When a trim style applies, force the element to be a
  // `<path>` so stroke-dasharray works regardless of the source primitive.
  const shape = ctx.data.s[shapeRef.s];
  if (!shape) return;
  // When a TrimPath applies, force a `<path>` so we can swap in geometrically
  // trimmed sub-path data each frame. Without trim, primitives stay as their
  // natural elements (`<rect>`, `<ellipse>`, etc).
  const asPath = shapeRef.tm !== undefined && shapeRef.tm !== null;
  const el = primitiveElement(shape, asPath);
  if (!el) return;
  parent.appendChild(el);
  accum.push({ kind: 'primitive', el, shape, ref: shapeRef, asPath });
}

function primitiveElement(shape, asPath) {
  if (asPath) return document.createElementNS(NS, 'path');
  switch (shape.t) {
    case 'r': return document.createElementNS(NS, 'rect');
    case 'e': return document.createElementNS(NS, 'ellipse');
    case 'p': return document.createElementNS(NS, 'path');
    case 's': return document.createElementNS(NS, 'path');
    default: return null;
  }
}

function updateLayer(info, frame, ctx, isInner = false) {
  const { outerG, innerG, layer, shapes, proxy, precompInner, masks } = info;

  // Honor the layer's [ip, op) lifetime. Top-level layers respect both
  // bounds strictly. Inner precomp layers WRAP at their `op` instead of
  // disappearing — lottie-web replays the asset's inner timeline so the
  // starfish wink animation (mask keyframed at frames 26..44 inside an
  // op=140 layer) repeats every 140 frames at the outer clock.
  if (!isInner) {
    const inRange = frame >= layer.ip && frame < layer.op;
    outerG.style.display = inRange ? '' : 'none';
    if (!inRange) return;
  } else {
    outerG.style.display = '';
    const period = layer.op - layer.ip;
    if (period > 0 && frame >= layer.op) {
      frame = layer.ip + ((frame - layer.ip) % period);
      ctx.currentFrame = frame;
    }
  }

  // Transform. Expression-driven properties receive `proxy` as `thisLayer`.
  const p = layer.p !== undefined ? evalProp(layer.p, frame, ctx, proxy) : [0, 0, 0];
  const a = layer.a !== undefined ? evalProp(layer.a, frame, ctx, proxy) : [0, 0, 0];
  const s = layer.sc !== undefined ? evalProp(layer.sc, frame, ctx, proxy) : [100, 100, 100];
  const r = layer.r !== undefined ? evalProp(layer.r, frame, ctx, proxy) : 0;
  const o = layer.o !== undefined ? evalProp(layer.o, frame, ctx, proxy) : 100;
  outerG.setAttribute('transform', composeTransform(p, a, s, r));
  // Only set opacity on the inner content group. For null layers (ty=3), we
  // reuse outerG and skip the opacity attribute so nested children — whose
  // transforms inherit from outerG — aren't affected by Lottie's "null
  // layers have opacity 0" convention.
  if (layer.ty !== 3) {
    const opacityScalar = typeof o === 'number' ? o : (o?.[0] ?? 100);
    innerG.setAttribute('opacity', opacityScalar / 100);
  }

  // Re-evaluate mask paths at the current frame and write to the SVG.
  if (masks) {
    for (const m of masks) {
      const pathData = evalProp(m.mask.pt, frame, ctx, proxy);
      if (pathData && pathData.v) {
        m.el.setAttribute('d', pathToSvgD(pathData));
      }
    }
  }

  for (const item of shapes) updateShape(item, frame, ctx, proxy);

  // Precomp instance: drive its inner layers with the precomp's start-time
  // offset applied. AE/Lottie semantics: a precomp at frame F shows its
  // inner content as if the inner clock were at (F - st), so a precomp's
  // `st` parameter shifts when its content plays back.
  if (precompInner) {
    const st = layer.st || 0;
    const innerFrame = frame - st;
    // Save & restore the comp-wide currentFrame so expressions inside the
    // precomp see the shifted clock too.
    const savedFrame = ctx.currentFrame;
    ctx.currentFrame = innerFrame;
    for (const inner of precompInner) updateLayer(inner, innerFrame, ctx, true);
    ctx.currentFrame = savedFrame;
  }
}

function composeTransform(p, a, s, r) {
  const px = nth(p, 0), py = nth(p, 1);
  const ax = nth(a, 0), ay = nth(a, 1);
  const sx = nth(s, 0, 100) / 100;
  const sy = nth(s, 1, 100) / 100;
  const rot = typeof r === 'number' ? r : nth(r, 0);
  return `translate(${px},${py}) rotate(${rot}) scale(${sx},${sy}) translate(${-ax},${-ay})`;
}

function nth(v, idx, fallback = 0) {
  if (v == null) return fallback;
  if (typeof v === 'number') return idx === 0 ? v : fallback;
  return v[idx] ?? fallback;
}

function updateShape(item, frame, ctx, layerProxy) {
  if (item.kind === 'group') {
    // Apply the group-local transform. Any of p/a/sc/r/o may be absent; the
    // driver substitutes identity defaults.
    const ref = item.ref;
    const p = ref.p !== undefined ? evalProp(ref.p, frame, ctx, layerProxy) : [0, 0, 0];
    const a = ref.a !== undefined ? evalProp(ref.a, frame, ctx, layerProxy) : [0, 0, 0];
    const s = ref.sc !== undefined ? evalProp(ref.sc, frame, ctx, layerProxy) : [100, 100, 100];
    const r = ref.r !== undefined ? evalProp(ref.r, frame, ctx, layerProxy) : 0;
    const o = ref.o !== undefined ? evalProp(ref.o, frame, ctx, layerProxy) : 100;
    item.el.setAttribute('transform', composeTransform(p, a, s, r));
    if (o !== 100) item.el.setAttribute('opacity', (typeof o === 'number' ? o : o[0]) / 100);
    for (const child of item.children) updateShape(child, frame, ctx, layerProxy);
    return;
  }
  const { el, shape, ref, asPath } = item;
  // Geometry. When `asPath` is set (because a trim style applies), we always
  // produce a structured path first so the trim can subdivide segments
  // geometrically — stroke-dasharray with round caps creates rendering
  // artifacts (stray dots from overlapping caps) when the visible dash is
  // shorter than the stroke width.
  let structuredPath = null;
  if (shape.t === 'r') {
    const sz = evalProp(shape.sz, frame, ctx, layerProxy);
    const ps = evalProp(shape.ps, frame, ctx, layerProxy);
    const rd = evalProp(shape.rd, frame, ctx, layerProxy);
    const w = nth(sz, 0);
    const h = nth(sz, 1);
    const cx = nth(ps, 0);
    const cy = nth(ps, 1);
    const r = typeof rd === 'number' ? rd : nth(rd, 0);
    if (asPath) {
      structuredPath = rectToPath(cx, cy, w, h, r);
    } else {
      el.setAttribute('x', cx - w / 2);
      el.setAttribute('y', cy - h / 2);
      el.setAttribute('width', w);
      el.setAttribute('height', h);
      if (r > 0) {
        const cr = Math.min(r, w / 2, h / 2);
        el.setAttribute('rx', cr);
        el.setAttribute('ry', cr);
      }
    }
  } else if (shape.t === 'e') {
    const sz = evalProp(shape.sz, frame, ctx, layerProxy);
    const ps = evalProp(shape.ps, frame, ctx, layerProxy);
    if (asPath) {
      structuredPath = ellipseToPath(nth(ps, 0), nth(ps, 1), nth(sz, 0) / 2, nth(sz, 1) / 2);
    } else {
      el.setAttribute('cx', nth(ps, 0));
      el.setAttribute('cy', nth(ps, 1));
      el.setAttribute('rx', nth(sz, 0) / 2);
      el.setAttribute('ry', nth(sz, 1) / 2);
    }
  } else if (shape.t === 'p') {
    structuredPath = evalProp(shape.pt, frame, ctx, layerProxy);
  } else if (shape.t === 's') {
    structuredPath = polystarToPath(shape, frame, ctx, layerProxy);
  }
  // Trim is applied geometrically: subdivide the structured path at the
  // start/end fractions, then convert the trimmed result to an SVG `d`.
  let hidden = false;
  if (HAS_TRIM_PATH && structuredPath && ref.tm !== undefined && ref.tm !== null) {
    const trim = computeTrimRange(ctx.data.y[ref.tm], frame, ctx, layerProxy);
    if (trim.visible <= 0) {
      hidden = true;
    } else if (trim.visible < 1) {
      structuredPath = trimPath(structuredPath, trim.lo, trim.hi, trim.offset);
      if (!structuredPath || !structuredPath.v || structuredPath.v.length === 0) {
        hidden = true;
      }
    }
  }
  if (hidden) {
    el.style.display = 'none';
  } else {
    el.style.display = '';
    if (structuredPath && structuredPath.v) {
      el.setAttribute('d', pathToSvgD(structuredPath));
    }
  }
  // Apply styles. Multiple styles (e.g. both stroke + fill) stack on the
  // same SVG element — fill attrs and stroke attrs don't conflict.
  //
  // Iterate in REVERSE: when a group lists multiple fills (or multiple
  // strokes), Lottie's render order draws the first-listed style on top.
  // Our single-element approach can't draw two overlapping fills the way
  // lottie-web emits two `<path>` elements, but for the common case where
  // the top fill is opaque (flame's gradient over a gray base) we can match
  // visually by letting the first-in-JSON style override later ones. Stroke
  // and fill don't conflict — they target different SVG attributes — so
  // reversing doesn't affect that combination.
  if (ref.y && ref.y.length > 0) {
    let strokeApplied = false;
    let fillApplied = false;
    for (let i = ref.y.length - 1; i >= 0; i--) {
      const yId = ref.y[i];
      const style = ctx.data.y[yId];
      if (!style) continue;
      applyStyle(el, style, frame, ctx, yId, layerProxy);
      if (style.t === 'st' || style.t === 'gs') strokeApplied = true;
      if (style.t === 'fl' || style.t === 'gf') fillApplied = true;
    }
    // If no fill was specified but a stroke was, ensure fill="none" so the
    // shape doesn't get a default black fill underneath the stroke.
    if (strokeApplied && !fillApplied) el.setAttribute('fill', 'none');
  }
}

// rect → structured path `{v, i, o, c}`. Top-right corner first, clockwise.
// Matches lottie-web's traversal order so trim animations sweep the perimeter
// in the same direction. With non-zero corner radius, each corner becomes a
// bezier-arc segment between two adjacent vertices.
function rectToPath(cx, cy, w, h, r) {
  const hw = w / 2, hh = h / 2;
  const l = cx - hw;
  const t = cy - hh;
  const ri = cx + hw;
  const b = cy + hh;
  if (!r || r < 1e-3) {
    return {
      v: [[ri, t], [ri, b], [l, b], [l, t]],
      i: [[0, 0], [0, 0], [0, 0], [0, 0]],
      o: [[0, 0], [0, 0], [0, 0], [0, 0]],
      c: true,
    };
  }
  const rr = Math.min(r, hw, hh);
  const k = rr * 0.5522847498;
  // 8 vertices, alternating "start of corner curve" and "end of corner curve".
  // The straight edges between them have zero tangents (linear).
  return {
    v: [
      [ri, t + rr],         // 0: top-right corner start
      [ri, b - rr],         // 1: bottom-right corner start
      [ri - rr, b],         // 2: bottom-right corner end
      [l + rr, b],          // 3: bottom-left corner start
      [l, b - rr],          // 4: bottom-left corner end
      [l, t + rr],          // 5: top-left corner start
      [l + rr, t],          // 6: top-left corner end
      [ri - rr, t],         // 7: top-right corner start
    ],
    // Bezier handles are placed so each "corner" segment is one cubic arc.
    o: [
      [0, 0],               // 0→1: straight edge (right side)
      [0, k],               // 1→2: bottom-right curve out
      [0, 0],               // 2→3: straight edge (bottom)
      [-k, 0],              // 3→4: bottom-left curve out
      [0, 0],               // 4→5: straight edge (left)
      [0, -k],              // 5→6: top-left curve out
      [0, 0],               // 6→7: straight edge (top)
      [k, 0],               // 7→0: top-right curve out
    ],
    i: [
      [0, k],               // 0: top-right curve in
      [0, 0],               // 1: linear from previous
      [k, 0],               // 2: bottom-right curve in
      [0, 0],               // 3: linear from previous
      [0, -k],              // 4: bottom-left curve in
      [0, 0],               // 5: linear from previous
      [-k, 0],              // 6: top-left curve in
      [0, 0],               // 7: linear from previous
    ],
    c: true,
  };
}

// ellipse → structured path. 4 vertices (top/right/bottom/left) with bezier
// handles tuned so each segment approximates a quarter circle.
function ellipseToPath(cx, cy, rx, ry) {
  const k = 0.5522847498;
  const kx = rx * k;
  const ky = ry * k;
  return {
    v: [[cx, cy - ry], [cx + rx, cy], [cx, cy + ry], [cx - rx, cy]],
    o: [[kx, 0], [0, ky], [-kx, 0], [0, -ky]],
    i: [[-kx, 0], [0, -ky], [kx, 0], [0, ky]],
    c: true,
  };
}

// polystar → structured path. Star (sy=1) alternates outer/inner vertices;
// polygon (sy=2) walks the outer vertices only. Roundness is approximated
// linearly — lottie-web sets bezier handles based on `os` / `is` but our
// fixtures don't exercise roundness so we keep the straight-line form.
function polystarToPath(shape, frame, ctx, layerProxy) {
  const pt = Math.round(evalProp(shape.pt, frame, ctx, layerProxy) || 5);
  const ps = evalProp(shape.ps, frame, ctx, layerProxy) || [0, 0];
  const cx = nth(ps, 0), cy = nth(ps, 1);
  const or = evalProp(shape.or, frame, ctx, layerProxy);
  const ir = evalProp(shape.ir, frame, ctx, layerProxy);
  const rot = evalProp(shape.rt, frame, ctx, layerProxy) || 0;
  const sy = shape.sy;
  const step = Math.PI * 2 / pt;
  const r0 = rot * Math.PI / 180 - Math.PI / 2;
  const v = [];
  for (let i = 0; i < pt; i++) {
    const a = r0 + i * step;
    v.push([cx + or * Math.cos(a), cy + or * Math.sin(a)]);
    if (sy === 1) {
      const ia = a + step / 2;
      v.push([cx + ir * Math.cos(ia), cy + ir * Math.sin(ia)]);
    }
  }
  const zero = v.map(() => [0, 0]);
  return { v, i: zero, o: zero.map(p => p.slice()), c: true };
}

// Resolve a TrimPath style at the current frame into normalized fractions:
//   `lo` and `hi` are the lower / upper endpoints of the visible range, each
//   on [0, 1]. `offset` is the rotational offset (also 0..1). `visible` is
//   `hi - lo` clamped to [0, 1] for cheap on/off checks.
function computeTrimRange(style, frame, ctx, layerProxy) {
  const start = evalProp(style.s, frame, ctx, layerProxy);
  const end = evalProp(style.e, frame, ctx, layerProxy);
  const offset = evalProp(style.o, frame, ctx, layerProxy);
  const s = (typeof start === 'number' ? start : nth(start, 0)) / 100;
  const e2 = (typeof end === 'number' ? end : nth(end, 0)) / 100;
  const o = (typeof offset === 'number' ? offset : nth(offset, 0)) / 360;
  const lo = Math.min(s, e2);
  const hi = Math.max(s, e2);
  return { lo, hi, offset: o, visible: Math.max(0, Math.min(1, hi - lo)) };
}

// Geometric trim. Returns a new structured path containing only the portion
// of `path` between arc-length fractions `lo` and `hi`, with `offset`
// rotating the window around the perimeter for closed paths.
//
// The algorithm:
//   1. Pre-sample each bezier segment to compute arc length and per-sample
//      cumulative distance. This is the same arc-length technique
//      `sampleSpatialBezierByArcLength` uses, but per-segment.
//   2. Convert `lo`/`hi` into a (segment_index, t_within_segment) pair via
//      binary search on the cumulative table.
//   3. For boundary segments, subdivide the cubic with de Casteljau at the
//      relevant `t`. For interior segments, keep them whole.
//   4. Stitch the resulting vertices and tangents into a new structured path.
function trimPath(path, lo, hi, offset = 0) {
  const v = path.v;
  const inTan = path.i;
  const outTan = path.o;
  const closed = path.c;
  const n = v.length;
  if (n < 2) return path;
  const segCount = closed ? n : n - 1;
  // Per-segment arc tables. Each entry is { length, samples } where samples
  // is an array of {t, dist} cumulative entries used to invert distance →
  // parameter.
  const segTables = new Array(segCount);
  let totalLen = 0;
  const SAMPLES = 30;
  for (let i = 0; i < segCount; i++) {
    const next = (i + 1) % n;
    const p0 = v[i];
    const p3 = v[next];
    const p1 = [p0[0] + outTan[i][0], p0[1] + outTan[i][1]];
    const p2 = [p3[0] + inTan[next][0], p3[1] + inTan[next][1]];
    const samples = new Array(SAMPLES + 1);
    let cum = 0;
    let prev = p0;
    samples[0] = { t: 0, dist: 0 };
    for (let k = 1; k <= SAMPLES; k++) {
      const t = k / SAMPLES;
      const pt = cubicEval(p0, p1, p2, p3, t);
      cum += Math.hypot(pt[0] - prev[0], pt[1] - prev[1]);
      samples[k] = { t, dist: cum };
      prev = pt;
    }
    segTables[i] = { length: cum, samples, p0, p1, p2, p3 };
    totalLen += cum;
  }
  if (totalLen === 0) return { v: [], i: [], o: [], c: closed };

  const visible = hi - lo;
  if (visible >= 1) return path; // full visibility — return original

  // Apply offset by shifting the window. For closed paths the window can
  // wrap past the seam; for open paths we simply translate within [0, 1].
  // CAUTION: `hi + offset` may equal 1.0 — taking `% 1` would collapse it to
  // 0 and silently flip the window's direction. Only apply the modulo when
  // we actually exceed the [0, 1] band.
  let aRaw = lo + offset;
  let bRaw = hi + offset;
  if (closed) {
    // Normalize both endpoints into [0, 1) using floor, preserving relative
    // distance so wrap detection still works.
    const aFloor = Math.floor(aRaw);
    aRaw -= aFloor;
    bRaw -= aFloor;
    if (bRaw > 1) {
      // Window wraps the seam: split into [aRaw, 1] and [0, bRaw - 1].
      const part1 = trimByFraction(segTables, totalLen, aRaw, 1, closed);
      const part2 = trimByFraction(segTables, totalLen, 0, bRaw - 1, closed);
      return concatPaths(part1, part2);
    }
    return trimByFraction(segTables, totalLen, aRaw, bRaw, closed);
  }
  // Open path: clamp the window into [0, 1] — anything outside has no path
  // to clip against.
  const a = Math.max(0, Math.min(1, aRaw));
  const b = Math.max(0, Math.min(1, bRaw));
  if (b <= a) return { v: [], i: [], o: [], c: false };
  return trimByFraction(segTables, totalLen, a, b, closed);
}

// Walk the precomputed segment tables to build a structured path covering
// the arc-length range [aFrac, bFrac] of the input. Result is open (`c=false`)
// because trim always produces an open sub-path.
function trimByFraction(segTables, totalLen, aFrac, bFrac, _closed) {
  const aDist = aFrac * totalLen;
  const bDist = bFrac * totalLen;
  const aLoc = locateDist(segTables, aDist);
  const bLoc = locateDist(segTables, bDist);
  const outV = [];
  const outI = [];
  const outO = [];

  // If start and end land in the same segment, take that single piece.
  if (aLoc.seg === bLoc.seg) {
    const piece = subdivideBezierBetween(segTables[aLoc.seg], aLoc.t, bLoc.t);
    outV.push(piece.p0, piece.p3);
    outI.push([0, 0], [piece.p2[0] - piece.p3[0], piece.p2[1] - piece.p3[1]]);
    outO.push([piece.p1[0] - piece.p0[0], piece.p1[1] - piece.p0[1]], [0, 0]);
    return { v: outV, i: outI, o: outO, c: false };
  }

  // Boundary segment at the start: take its tail from `aLoc.t` to 1.
  const head = subdivideBezierBetween(segTables[aLoc.seg], aLoc.t, 1);
  outV.push(head.p0);
  outI.push([0, 0]);
  outO.push([head.p1[0] - head.p0[0], head.p1[1] - head.p0[1]]);
  let prevP2 = head.p2;
  let prevEndpoint = head.p3;

  // Interior segments are taken whole.
  for (let i = aLoc.seg + 1; i < bLoc.seg; i++) {
    const seg = segTables[i];
    outV.push(seg.p0);
    outI.push([prevP2[0] - prevEndpoint[0], prevP2[1] - prevEndpoint[1]]);
    outO.push([seg.p1[0] - seg.p0[0], seg.p1[1] - seg.p0[1]]);
    prevP2 = seg.p2;
    prevEndpoint = seg.p3;
  }

  // Boundary segment at the end: take its head from 0 to `bLoc.t`.
  const tail = subdivideBezierBetween(segTables[bLoc.seg], 0, bLoc.t);
  outV.push(tail.p0);
  outI.push([prevP2[0] - prevEndpoint[0], prevP2[1] - prevEndpoint[1]]);
  outO.push([tail.p1[0] - tail.p0[0], tail.p1[1] - tail.p0[1]]);
  outV.push(tail.p3);
  outI.push([tail.p2[0] - tail.p3[0], tail.p2[1] - tail.p3[1]]);
  outO.push([0, 0]);

  return { v: outV, i: outI, o: outO, c: false };
}

// Concatenate two open structured paths. Used for trim windows that wrap a
// closed path's seam.
function concatPaths(a, b) {
  if (!a || !a.v.length) return b;
  if (!b || !b.v.length) return a;
  return {
    v: a.v.concat(b.v),
    i: a.i.concat(b.i),
    o: a.o.concat(b.o),
    c: false,
  };
}

// Given a target arc-length distance `dist`, find the (segment_index, t)
// where t is the bezier parameter on that segment. Linear-interpolates within
// the precomputed sample table.
function locateDist(segTables, dist) {
  let acc = 0;
  for (let i = 0; i < segTables.length; i++) {
    const seg = segTables[i];
    if (dist <= acc + seg.length || i === segTables.length - 1) {
      const local = Math.max(0, dist - acc);
      const samples = seg.samples;
      let lo = 0, hi = samples.length - 1;
      while (lo < hi) {
        const mid = (lo + hi) >> 1;
        if (samples[mid].dist < local) lo = mid + 1; else hi = mid;
      }
      const upper = lo;
      const lower = Math.max(0, upper - 1);
      const dlo = samples[lower].dist;
      const dhi = samples[upper].dist;
      const frac = dhi === dlo ? 0 : (local - dlo) / (dhi - dlo);
      const t = samples[lower].t + (samples[upper].t - samples[lower].t) * frac;
      return { seg: i, t: Math.max(0, Math.min(1, t)) };
    }
    acc += seg.length;
  }
  return { seg: segTables.length - 1, t: 1 };
}

// Evaluate a cubic bezier at parameter `t`. p0..p3 are 2D points.
function cubicEval(p0, p1, p2, p3, t) {
  const u = 1 - t;
  const u3 = u * u * u;
  const u2t = 3 * u * u * t;
  const ut2 = 3 * u * t * t;
  const t3 = t * t * t;
  return [
    u3 * p0[0] + u2t * p1[0] + ut2 * p2[0] + t3 * p3[0],
    u3 * p0[1] + u2t * p1[1] + ut2 * p2[1] + t3 * p3[1],
  ];
}

// De Casteljau split of a cubic bezier between parameters `a` and `b`.
// Returns the new control points (p0..p3) for the sub-curve. When a/b are 0
// or 1 the relevant endpoint comes back exact, avoiding rounding drift.
function subdivideBezierBetween(seg, a, b) {
  // Step 1: split at `b`, keep the left half. That gives a cubic from 0 to b
  // on the original parameterization. Step 2: split that at `a/b`, keep the
  // right half.
  const left = splitCubicAt(seg.p0, seg.p1, seg.p2, seg.p3, b).left;
  // After splitting at b, the left sub-curve is parameterized [0, 1] = [0, b]
  // of the original. To take its [a, b] portion, we split at a/b.
  const t2 = b === 0 ? 0 : a / b;
  return splitCubicAt(left.p0, left.p1, left.p2, left.p3, t2).right;
}

// Standard de Casteljau split. Returns { left: {p0..p3}, right: {p0..p3} }.
function splitCubicAt(p0, p1, p2, p3, t) {
  const u = 1 - t;
  const a01 = [u * p0[0] + t * p1[0], u * p0[1] + t * p1[1]];
  const a12 = [u * p1[0] + t * p2[0], u * p1[1] + t * p2[1]];
  const a23 = [u * p2[0] + t * p3[0], u * p2[1] + t * p3[1]];
  const b01 = [u * a01[0] + t * a12[0], u * a01[1] + t * a12[1]];
  const b12 = [u * a12[0] + t * a23[0], u * a12[1] + t * a23[1]];
  const c = [u * b01[0] + t * b12[0], u * b01[1] + t * b12[1]];
  return {
    left: { p0, p1: a01, p2: b01, p3: c },
    right: { p0: c, p1: b12, p2: a23, p3 },
  };
}

// Convert a {v, i, o, c} path value to an SVG `d` attribute. Tangents are
// stored relative to their vertex (Lottie convention); we convert to the
// absolute control points SVG `C` expects.
function pathToSvgD(p) {
  const v = p.v, ti = p.i, to = p.o, closed = p.c;
  if (!v.length) return '';
  let d = `M${v[0][0]},${v[0][1]}`;
  const segs = closed ? v.length : v.length - 1;
  for (let i = 0; i < segs; i++) {
    const next = (i + 1) % v.length;
    const cp1x = v[i][0] + to[i][0];
    const cp1y = v[i][1] + to[i][1];
    const cp2x = v[next][0] + ti[next][0];
    const cp2y = v[next][1] + ti[next][1];
    const linear =
      Math.abs(to[i][0]) < 1e-6 && Math.abs(to[i][1]) < 1e-6 &&
      Math.abs(ti[next][0]) < 1e-6 && Math.abs(ti[next][1]) < 1e-6;
    if (linear) {
      d += ` L${v[next][0]},${v[next][1]}`;
    } else {
      d += ` C${cp1x},${cp1y} ${cp2x},${cp2y} ${v[next][0]},${v[next][1]}`;
    }
  }
  if (closed) d += 'Z';
  return d;
}

function rectPath(cx, cy, w, h, r) {
  const halfW = w / 2;
  const halfH = h / 2;
  const rr = Math.max(0, Math.min(r || 0, halfW, halfH));
  const x = cx - halfW;
  const y = cy - halfH;
  if (rr === 0) {
    return `M${x},${y} L${x + w},${y} L${x + w},${y + h} L${x},${y + h} Z`;
  }
  return `M${x + rr},${y} L${x + w - rr},${y} A${rr},${rr} 0 0 1 ${x + w},${y + rr} L${x + w},${y + h - rr} A${rr},${rr} 0 0 1 ${x + w - rr},${y + h} L${x + rr},${y + h} A${rr},${rr} 0 0 1 ${x},${y + h - rr} L${x},${y + rr} A${rr},${rr} 0 0 1 ${x + rr},${y} Z`;
}

function applyStyle(el, style, frame, ctx, styleId, layerProxy) {
  if (!style) return;
  if (style.t === 'fl') {
    const c = evalProp(style.c, frame, ctx, layerProxy);
    const o = evalProp(style.o, frame, ctx, layerProxy);
    el.setAttribute('fill', colorToCss(c, o));
  } else if (style.t === 'st') {
    const c = evalProp(style.c, frame, ctx, layerProxy);
    const o = evalProp(style.o, frame, ctx, layerProxy);
    const w = evalProp(style.w, frame, ctx, layerProxy);
    el.setAttribute('stroke', colorToCss(c, o));
    el.setAttribute('stroke-width', typeof w === 'number' ? w : w[0]);
    el.setAttribute('stroke-linecap', ['butt', 'round', 'square'][(style.lc || 1) - 1]);
    el.setAttribute('stroke-linejoin', ['miter', 'round', 'bevel'][(style.lj || 1) - 1]);
    if (style.ml != null) el.setAttribute('stroke-miterlimit', style.ml);
  } else if (HAS_GRADIENT && style.t === 'gs') {
    // Gradient stroke. Build the <linearGradient>/<radialGradient> once per
    // unique style id and reference it via stroke="url(#grad-N)".
    const gradId = ensureGradient(style, styleId, ctx, layerProxy);
    el.setAttribute('stroke', `url(#${gradId})`);
    const w = evalProp(style.w, frame, ctx, layerProxy);
    el.setAttribute('stroke-width', typeof w === 'number' ? w : w[0]);
    el.setAttribute('stroke-linecap', ['butt', 'round', 'square'][(style.lc || 1) - 1]);
    el.setAttribute('stroke-linejoin', ['miter', 'round', 'bevel'][(style.lj || 1) - 1]);
    if (style.ml != null) el.setAttribute('stroke-miterlimit', style.ml);
  } else if (HAS_GRADIENT && style.t === 'gf') {
    // Gradient fill. Shares the gradient cache with `gs`; only the SVG attr
    // it stamps on the element differs.
    const gradId = ensureGradient(style, styleId, ctx, layerProxy);
    el.setAttribute('fill', `url(#${gradId})`);
    const o = evalProp(style.o, frame, ctx, layerProxy);
    if (typeof o === 'number' && o < 100) el.setAttribute('fill-opacity', o / 100);
    if (style.fr === 2) el.setAttribute('fill-rule', 'evenodd');
  }
}

// Create-and-cache an SVG gradient element from a Lottie gradient definition.
// Lottie packs gradient stops as a flat number array:
//   color stops: [pos, r, g, b, pos, r, g, b, ...]  — `p` entries
//   alpha stops follow: [pos, a, pos, a, ...]
// The `g.p` field gives the count of color stops.
function ensureGradient(style, styleId, ctx, layerProxy) {
  if (ctx.gradientCache.has(styleId)) return ctx.gradientCache.get(styleId);
  const gradId = `grad-${ctx.gradientCount++}`;
  const g = style.g;
  const colorStops = (g && g.p) || 0;
  const stopsArr = (g && g.k && g.k.k) || [];
  // Determine gradient endpoints from style.s / style.e (property ids).
  const startVal = style.s !== undefined && style.s !== null
    ? evalProp(style.s, ctx.currentFrame, ctx, layerProxy) : [0, 0];
  const endVal = style.e !== undefined && style.e !== null
    ? evalProp(style.e, ctx.currentFrame, ctx, layerProxy) : [0, 0];
  const sx = Array.isArray(startVal) ? startVal[0] : 0;
  const sy = Array.isArray(startVal) ? startVal[1] : 0;
  const ex = Array.isArray(endVal) ? endVal[0] : 0;
  const ey = Array.isArray(endVal) ? endVal[1] : 0;
  const grad = document.createElementNS(NS, style.gk === 2 ? 'radialGradient' : 'linearGradient');
  grad.setAttribute('id', gradId);
  grad.setAttribute('gradientUnits', 'userSpaceOnUse');
  if (style.gk === 2) {
    // Radial: cx/cy = start, r = distance to end, fx/fy = start.
    const r = Math.hypot(ex - sx, ey - sy);
    grad.setAttribute('cx', sx);
    grad.setAttribute('cy', sy);
    grad.setAttribute('r', r);
    grad.setAttribute('fx', sx);
    grad.setAttribute('fy', sy);
  } else {
    grad.setAttribute('x1', sx);
    grad.setAttribute('y1', sy);
    grad.setAttribute('x2', ex);
    grad.setAttribute('y2', ey);
  }
  // Color stops.
  const colorStride = 4;
  // Collect color stops and alpha stops, then merge them into a unified
  // stop list. Lottie packs them separately and the positions don't always
  // line up exactly (e.g. ripple has color at 0.254 and alpha at 0.253),
  // so we treat the two lists as independent piecewise-linear functions and
  // sample them at every position that appears in either.
  const colorList = [];
  for (let i = 0; i < colorStops; i++) {
    const base = i * colorStride;
    colorList.push({ pos: stopsArr[base], r: stopsArr[base + 1], g: stopsArr[base + 2], b: stopsArr[base + 3] });
  }
  const alphaList = [];
  for (let i = colorStops * colorStride; i + 1 < stopsArr.length; i += 2) {
    alphaList.push({ pos: stopsArr[i], a: stopsArr[i + 1] });
  }
  // Build the union of offsets from both lists, sorted ascending.
  const positions = new Set();
  for (const c of colorList) positions.add(c.pos);
  for (const a of alphaList) positions.add(a.pos);
  const sortedPositions = [...positions].sort((x, y) => x - y);
  for (const pos of sortedPositions) {
    const c = sampleStopList(colorList, pos, sampleColor);
    const a = alphaList.length > 0 ? sampleStopList(alphaList, pos, sampleAlpha) : 1;
    const stop = document.createElementNS(NS, 'stop');
    stop.setAttribute('offset', pos);
    stop.setAttribute('stop-color', `rgb(${Math.round(c[0] * 255)},${Math.round(c[1] * 255)},${Math.round(c[2] * 255)})`);
    if (a < 1) stop.setAttribute('stop-opacity', a);
    grad.appendChild(stop);
  }
  ctx.defs.appendChild(grad);
  ctx.gradientCache.set(styleId, gradId);
  return gradId;
}

// Piecewise-linear sample of a sorted list of `{pos, ...}` entries at a
// given position. The user supplies a per-pair lerp (`sample(a, b, t)`) so
// the same routine handles RGB and alpha lists.
function sampleStopList(list, pos, sample) {
  if (list.length === 0) return sample(null, null, 0);
  if (pos <= list[0].pos) return sample(list[0], list[0], 0);
  if (pos >= list[list.length - 1].pos) return sample(list[list.length - 1], list[list.length - 1], 0);
  for (let i = 0; i < list.length - 1; i++) {
    const a = list[i], b = list[i + 1];
    if (pos >= a.pos && pos <= b.pos) {
      const t = b.pos === a.pos ? 0 : (pos - a.pos) / (b.pos - a.pos);
      return sample(a, b, t);
    }
  }
  return sample(list[list.length - 1], list[list.length - 1], 0);
}

function sampleColor(a, b, t) {
  if (!a) return [0, 0, 0];
  return [a.r + (b.r - a.r) * t, a.g + (b.g - a.g) * t, a.b + (b.b - a.b) * t];
}

function sampleAlpha(a, b, t) {
  if (!a) return 1;
  return a.a + (b.a - a.a) * t;
}

function colorToCss(c, opacity) {
  // c may be [r,g,b] or [r,g,b,a] in 0..1; opacity scalar 0..100.
  if (!Array.isArray(c)) return 'rgb(0,0,0)';
  const r = Math.round((c[0] ?? 0) * 255);
  const g = Math.round((c[1] ?? 0) * 255);
  const b = Math.round((c[2] ?? 0) * 255);
  const alpha = (c[3] ?? 1) * (opacity ?? 100) / 100;
  if (alpha >= 1) return `rgb(${r},${g},${b})`;
  return `rgba(${r},${g},${b},${alpha})`;
}

// Walk a layer's ShapeRefs and return the property id of the first Path-typed
// primitive — that's what `layer(...)(...).pointOnPath()` chains target.
function findFirstPathShape(layer, ctx) {
  if (!layer.shapes) return null;
  const visit = (refs) => {
    for (const r of refs) {
      if (r.c) {
        const inner = visit(r.c);
        if (inner !== null) return inner;
      } else if (r.s !== undefined) {
        const shape = ctx.data.s[r.s];
        if (shape && shape.t === 'p') return shape.pt;
      }
    }
    return null;
  };
  return visit(layer.shapes);
}

// Path values from the wire arrive as plain objects. The arc-length helpers
// expect a stable object identity (they cache `__arcTable` on it), so the
// FIRST evaluation produces the canonical instance — subsequent identical
// values reuse it. Match lottie-web's behavior: arc table is frozen on
// first use even if `v`/`i`/`o` mutate later.
function _freezePath(path) {
  if (path.__frozen) return path;
  path.__frozen = true;
  return path;
}

// ---------------------------------------------------------------------------
// Expression runtime
// ---------------------------------------------------------------------------
//
// Wires `ctx` with the helpers compiled-expression bodies expect to find:
//   thisComp, sum/sub/mul/div/clamp, radiansToDegrees, degreesToRadians,
//   createPath, pointOnPath, tangentOnPath, makeThisProperty.
// Compiled-expression bodies look like
//
//   const { sum, div, thisComp } = ctx;
//   ...expression body...
//   return $bm_rt;
//
// so adding a helper here makes it available to every expression.

function attachExpressionRuntime(ctx) {
  ctx.thisComp = {
    layer(nameOrIndex) {
      // Prefer the scope of the currently-executing layer so a precomp
      // expression looks up siblings within its own precomp instance, falling
      // back to the root scope for top-level layers / direct calls.
      const scope = ctx._currentScope || ctx.rootScope;
      if (typeof nameOrIndex === 'number') return scope.byIndex[nameOrIndex];
      return scope.byName[nameOrIndex];
    },
    get frameDuration() { return 1 / ctx.frameRate; },
  };

  // Vector-aware arithmetic. Bodymovin replaces +/-/*/÷ with these so they
  // work on both numbers and arrays.
  ctx.sum = (a, b) => {
    if (Array.isArray(a) && Array.isArray(b)) return a.map((v, i) => v + (b[i] ?? 0));
    if (Array.isArray(a)) return a.map(v => v + b);
    if (Array.isArray(b)) return b.map(v => a + v);
    return a + b;
  };
  ctx.sub = (a, b) => {
    if (Array.isArray(a) && Array.isArray(b)) return a.map((v, i) => v - (b[i] ?? 0));
    if (Array.isArray(a)) return a.map(v => v - b);
    if (Array.isArray(b)) return b.map(v => a - v);
    return a - b;
  };
  ctx.mul = (a, b) => {
    if (Array.isArray(a) && Array.isArray(b)) return a.map((v, i) => v * (b[i] ?? 1));
    if (Array.isArray(a)) return a.map(v => v * b);
    if (Array.isArray(b)) return b.map(v => a * v);
    return a * b;
  };
  ctx.div = (a, b) => {
    if (Array.isArray(a) && Array.isArray(b)) return a.map((v, i) => v / (b[i] ?? 1));
    if (Array.isArray(a)) return a.map(v => v / b);
    if (Array.isArray(b)) return b.map(v => a / v);
    return a / b;
  };
  ctx.clamp = (v, mn, mx) => {
    if (Array.isArray(v)) return v.map(x => Math.max(mn, Math.min(mx, x)));
    return Math.max(mn, Math.min(mx, v));
  };
  ctx.radiansToDegrees = r => r * 180 / Math.PI;
  ctx.degreesToRadians = d => d * Math.PI / 180;
  ctx.createPath = createPath;
  ctx.pointOnPath = pointOnPath;
  ctx.tangentOnPath = tangentOnPath;
}

// Layer-space → composition-space conversion (lottie-web's `toComp`). Walks
// up the parent chain applying each layer's transform in turn.
function toComp(layer, point, ctx) {
  let p = [point[0], point[1]];
  let l = layer;
  while (l) {
    const t = l.getLocalTransform(ctx.currentFrame);
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
    l = l.parentLayer;
  }
  return p;
}

// Composition-space → layer-space conversion, the inverse of `toComp`. Walks
// the chain top-down and undoes each ancestor's transform.
function fromCompToSurface(point, layer, ctx) {
  const stack = [];
  let l = layer;
  while (l) { stack.unshift(l); l = l.parentLayer; }
  let p = [point[0], point[1]];
  for (const lyr of stack) {
    const t = lyr.getLocalTransform(ctx.currentFrame);
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
// Path API used by expressions (createPath / pointOnPath / tangentOnPath)
// ---------------------------------------------------------------------------

function createPath(verts, inTan, outTan, closed) {
  // Matches Lottie/lottie-web shape: the path object exposes `.v`, `.i`, `.o`,
  // `.c` directly so expression code that mutates these (e.g. the lights wire)
  // can still drive rendering.
  return {
    v: verts.map(p => p.slice()),
    i: inTan.map(p => p.slice()),
    o: outTan.map(p => p.slice()),
    c: !!closed,
    points() { return this.v.map(p => p.slice()); },
    inTangents() { return this.i.map(p => p.slice()); },
    outTangents() { return this.o.map(p => p.slice()); },
    isClosed() { return this.c; },
  };
}

const _ARC_SAMPLES = 800;

// Arc-length parameterization for cubic-bezier paths. Once computed, the
// table is frozen on the path; subsequent mutations to `path.v` are ignored
// for parameterization purposes — matching lottie-web's behavior, which is
// what fixtures like the lights wire depend on.
function _getArcTable(path) {
  if (path.__arcTable) return path.__arcTable;
  const v = path.v, ti = path.i, to = path.o;
  const n = v.length;
  const segs = path.c ? n : n - 1;
  const segCumul = new Array(segs);
  const segSamples = new Array(segs);
  let total = 0;
  for (let i = 0; i < segs; i++) {
    const next = (i + 1) % n;
    const p0 = v[i], p3 = v[next];
    const p1 = [p0[0] + to[i][0], p0[1] + to[i][1]];
    const p2 = [p3[0] + ti[next][0], p3[1] + ti[next][1]];
    const cumul = new Float64Array(_ARC_SAMPLES + 1);
    let prev = p0;
    let acc = 0;
    for (let k = 1; k <= _ARC_SAMPLES; k++) {
      const lt = k / _ARC_SAMPLES;
      const u = 1 - lt;
      const u3 = u * u * u, u2t = 3 * u * u * lt, ut2 = 3 * u * lt * lt, t3 = lt * lt * lt;
      const pt = [
        u3 * p0[0] + u2t * p1[0] + ut2 * p2[0] + t3 * p3[0],
        u3 * p0[1] + u2t * p1[1] + ut2 * p2[1] + t3 * p3[1],
      ];
      acc += Math.hypot(pt[0] - prev[0], pt[1] - prev[1]);
      cumul[k] = acc;
      prev = pt;
    }
    segSamples[i] = cumul;
    total += acc;
    segCumul[i] = total;
  }
  path.__arcTable = { segCumul, segSamples, total, segs };
  return path.__arcTable;
}

function _resolveArcT(path, t) {
  const tab = _getArcTable(path);
  if (tab.segs === 0 || tab.total === 0) return [0, 0];
  t = Math.max(0, Math.min(1, t));
  const target = t * tab.total;
  let segIdx = 0;
  while (segIdx < tab.segs - 1 && tab.segCumul[segIdx] < target) segIdx++;
  const segStart = segIdx === 0 ? 0 : tab.segCumul[segIdx - 1];
  const local = target - segStart;
  const samples = tab.segSamples[segIdx];
  let lo = 0, hi = _ARC_SAMPLES;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (samples[mid] < local) lo = mid + 1; else hi = mid;
  }
  const upper = lo;
  const lower = Math.max(0, upper - 1);
  const lenL = samples[lower];
  const lenU = samples[upper];
  const frac = lenU === lenL ? 0 : (local - lenL) / (lenU - lenL);
  return [segIdx, Math.min(1, Math.max(0, (lower + frac) / _ARC_SAMPLES))];
}

function pointOnPath(path, t) {
  const v = path.v, ti = path.i, to = path.o;
  const n = v.length;
  const segs = path.c ? n : n - 1;
  if (segs === 0) return v[0] || [0, 0];
  const [segIdx, lt] = _resolveArcT(path, t);
  const next = (segIdx + 1) % n;
  const p0 = v[segIdx], p3 = v[next];
  const p1 = [p0[0] + to[segIdx][0], p0[1] + to[segIdx][1]];
  const p2 = [p3[0] + ti[next][0], p3[1] + ti[next][1]];
  const u = 1 - lt;
  const u3 = u * u * u, u2t = 3 * u * u * lt, ut2 = 3 * u * lt * lt, t3 = lt * lt * lt;
  return [
    u3 * p0[0] + u2t * p1[0] + ut2 * p2[0] + t3 * p3[0],
    u3 * p0[1] + u2t * p1[1] + ut2 * p2[1] + t3 * p3[1],
  ];
}

function tangentOnPath(path, t) {
  const v = path.v, ti = path.i, to = path.o;
  const n = v.length;
  const segs = path.c ? n : n - 1;
  if (segs === 0) return [1, 0];
  const [segIdx, lt] = _resolveArcT(path, t);
  const next = (segIdx + 1) % n;
  const p0 = v[segIdx], p3 = v[next];
  const p1 = [p0[0] + to[segIdx][0], p0[1] + to[segIdx][1]];
  const p2 = [p3[0] + ti[next][0], p3[1] + ti[next][1]];
  const u = 1 - lt;
  const dx = 3 * u * u * (p1[0] - p0[0]) + 6 * u * lt * (p2[0] - p1[0]) + 3 * lt * lt * (p3[0] - p2[0]);
  const dy = 3 * u * u * (p1[1] - p0[1]) + 6 * u * lt * (p2[1] - p1[1]) + 3 * lt * lt * (p3[1] - p2[1]);
  return [dx, dy];
}
