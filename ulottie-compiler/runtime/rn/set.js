// The react-native-svg write point — the RN counterpart of set.js's `put`.
//
// An element handle is not a DOM node here. `mountRn` hands the ops an array
// of plain records `{ i, p, d, q }`: the element's slot index in the emitted
// `tree`, its current dynamic props, a dirty flag, and the shared dirty
// queue. `put` keeps set.js's contract — write only on change — and adds the
// RN half: the SVG attribute name maps to the camelCased react-native-svg
// prop, a `transform` string becomes the ColumnMajorTransformMatrix array
// (a transform *string* throws on Fabric iOS: "JSON value of type NSString
// cannot be converted to a CATransform3D"), and a changed element lands in
// the dirty queue once per flush so the consumer only touches what moved.

export function put(el, name, v, w, i) {
  if (v !== w[i]) {
    w[i] = v;
    el.p[rnProp(name)] = name === 'transform' ? rnMatrix(v) : v;
    if (!el.d) { el.d = 1; el.q.push(el); }
  }
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
