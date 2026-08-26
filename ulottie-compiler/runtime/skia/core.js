// μLottie Skia mount — `mountRn`'s data half plus the prepared draw tree.
//
// The clock/gate/decode machinery is target-agnostic and stays `mountRn`'s
// byte for byte; this wrapper only prepares the display list against the
// caller's Skia factory and hands back the same instance record with one
// extra member: `draw(canvas)`, which records the current node state. The
// `draw` closure is created here at mount time on whichever runtime called
// `init`, so it needs no own 'worklet' directive — `skDraw` carries one.

import { mountRn } from '../rn/core.js';
import { skPrepare, skDraw } from './draw.js';

export function mountSkia(D, P, A, N, ext, Sk, dl) {
  const h = mountRn(D, P, A, N, ext);
  const x = skPrepare(dl, h.els, Sk);
  h.draw = function (c) { skDraw(c, x, Sk); };
  return h;
}
