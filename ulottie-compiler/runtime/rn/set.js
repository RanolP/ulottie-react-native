// The react-native-svg write point — the RN counterpart of set.js's `put`.
//
// An element handle is not a DOM node here. `mountRn` hands the ops an array
// of plain records `{ i, p, d, q }`: the element's slot index in the emitted
// `tree`, its current dynamic props, a dirty flag, and the shared dirty
// queue. `put` keeps set.js's contract — write only on change — and adds the
// RN half: the SVG attribute name maps to the camelCased react-native-svg
// prop, a `transform` string becomes the ColumnMajorTransformMatrix array
// (a transform *string* throws on Fabric iOS: "JSON value of type NSString
// cannot be converted to a CATransform3D"), a prop the native side reads as a
// `Double` is handed a number (see `rnNumeric`), and a changed element lands
// in the dirty queue once per flush so the consumer only touches what moved.

export function put(el, name, v, w, i) {
  if (v !== w[i]) {
    w[i] = v;
    const p = rnProp(name);
    el.p[p] = name === 'transform' ? rnMatrix(v) : rnNumeric(p) ? +v : v;
    if (!el.d) { el.d = 1; el.q.push(el); }
  }
}

/**
 * Whether react-native-svg's native side reads this prop as a raw `Double`.
 *
 * These writes bypass rn-svg's JS prop extraction — the consumer pushes `el.p`
 * straight into Fabric — so whatever the op produced is what the generated
 * `RNSVG*ManagerDelegate.setProperty` casts. For these four the generated
 * Java is `((Double) value).floatValue()`, and a `String` there throws
 * `ClassCastException: String cannot be cast to Double`, which kills the app
 * (observed on a Pixel 8 with the `mixed16` fixture; iOS tolerates it).
 *
 * Every other prop the ops write is either a `String` on the native side
 * (`d`, `display`) or goes through `DynamicFromObject`, which parses a
 * numeric string via `SVGLength` — `x`/`y`/`width`/`height`/`rx`/`ry`/
 * `cx`/`cy`/`strokeWidth`/`strokeDasharray`/`fill`/`stroke`. Those keep the
 * op's string, so the shared change detection above still compares the exact
 * bytes the web runtime would have written.
 *
 * The number is `+v` on a value the op already rounded (`r`, `r2`), so this
 * only drops the stringification — it does not change the value.
 */
function rnNumeric(p) {
  return p === 'opacity' || p === 'fillOpacity' || p === 'strokeOpacity'
    || p === 'strokeDashoffset';
}

/**
 * Direct prop write for the few loops that write outside `put`'s
 * one-attribute guard (the display gates, the rect radius pair). The caller
 * has already change-detected; this only records the prop and marks the
 * element dirty.
 */
export function rput(el, prop, v) {
  el.p[prop] = v;
  if (!el.d) { el.d = 1; el.q.push(el); }
}

/** `fill-opacity` → `fillOpacity`. A name without a dash passes through. */
function rnProp(name) {
  if (name.indexOf('-') < 0) return name;
  let out = '';
  for (let i = 0; i < name.length; i++) {
    const c = name[i];
    if (c === '-') { i++; out += name[i].toUpperCase(); } else out += c;
  }
  return out;
}

/**
 * `matrix(a,b,c,d,e,f)` / `translate(x,y)` → `[a, b, c, d, e, f]`.
 *
 * The ops keep producing the transform *string* — its identity is what the
 * change detection above compares, so the parse runs only on actual changes,
 * and the string builders (`mtx`, the translate prefix) stay shared with the
 * web runtime byte for byte. Those two spellings are the only ones the
 * compiler ever writes (see `scene::svg::matrix`).
 */
function rnMatrix(v) {
  const a = v.slice(v.indexOf('(') + 1, v.length - 1).split(',');
  return v.charCodeAt(0) === 116
    ? [1, 0, 0, 1, +a[0], +a[1]]
    : [+a[0], +a[1], +a[2], +a[3], +a[4], +a[5]];
}
