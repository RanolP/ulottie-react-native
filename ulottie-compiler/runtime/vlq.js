// The payload decoder: one base36 VLQ string → one `Int32Array`.
//
// Runs once per mount. Everything downstream reads properties straight out of
// the array by offset, so this is the only place the wire format is touched.

/**
 * Decode the stream.
 *
 * Each character carries four data bits plus a continuation bit, written with
 * base36 digits — `0`–`9` are 0–15 and part of `a`–`v` the rest, which is what
 * `parseInt(c, 36)` would give. Doing the digit arithmetic on the char code
 * directly avoids a `parseInt` call per character, and there are a lot of
 * characters.
 *
 * Values are zigzagged, so a small negative costs one character just like a
 * small positive.
 */
export function dec(s) {
  const n = s.length;
  // Every integer takes at least one character, so the string length is a
  // safe upper bound and the array is allocated exactly once.
  const out = new Int32Array(n);
  let k = 0, acc = 0, sh = 0;
  for (let i = 0; i < n; i++) {
    const c = s.charCodeAt(i);
    // '0'..'9' are 48..57, 'a'..'v' are 97..118.
    const d = c < 58 ? c - 48 : c - 87;
    acc |= (d & 15) << sh;
    if (d & 16) {
      sh += 4;
    } else {
      // Un-zigzag. `acc` is a full 32-bit pattern by now, so the shift has to
      // be unsigned or the sign bit would smear.
      out[k++] = (acc >>> 1) ^ -(acc & 1);
      acc = 0;
      sh = 0;
    }
  }
  return out.subarray(0, k);
}
