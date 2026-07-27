// Attribute writes with change detection.
//
// A DOM attribute write invalidates style and layout even when the value is
// identical, so comparing first is strictly cheaper than writing blind. The
// compiler already removed every write that can never change; this catches the
// ones that merely happen not to change on a given frame — keyframe plateaus,
// held segments, and quantized values that round to the same string.

/** Bind a single attribute on one element, writing only on change. */
export function attr(el, name) {
  let last;
  return (v) => {
    if (v !== last) { last = v; el.setAttribute(name, v); }
  };
}
