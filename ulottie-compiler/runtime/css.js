// Colour serialization. Lottie stores channels as 0..1 floats plus a separate
// 0..100 style opacity; SVG wants one paint string.

export function css(c, o) {
  const r = (c[0] * 255 + 0.5) | 0;
  const g = (c[1] * 255 + 0.5) | 0;
  const b = (c[2] * 255 + 0.5) | 0;
  const a = (c.length > 3 ? c[3] : 1) * o / 100;
  return a >= 1 ? 'rgb(' + r + ',' + g + ',' + b + ')'
                : 'rgba(' + r + ',' + g + ',' + b + ',' + a + ')';
}
