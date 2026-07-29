// Attribute writes with change detection.
//
// A DOM attribute write invalidates style and layout even when the value is
// identical, so comparing first is strictly cheaper than writing blind. The
// compiler already removed every write that can never change; this catches the
// ones that merely happen not to change on a given frame — keyframe plateaus,
// held segments, and quantized values that round to the same string.

/**
 * Write one attribute of one binding, remembering what was last written.
 *
 * `w` is the batch's column of last values, indexed by binding. This used to be
 * a closure per attribute per binding holding that state in a capture; it is one
 * array per attribute now, and a single call target for every op that writes.
 * Generated code inlines the same three lines, because it knows the slot's name.
 */
export function put(el, name, v, w, i) {
  if (v !== w[i]) {
    w[i] = v;
    el.setAttribute(name, v);
  }
}
