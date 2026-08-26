import React, { forwardRef, useEffect, useImperativeHandle, useMemo } from 'react';
import { View, type StyleProp, type ViewStyle } from 'react-native';
import {
  Canvas,
  Group,
  Picture,
  Skia,
  type SkCanvas,
  type SkPicture,
} from '@shopify/react-native-skia';
import {
  runOnUI,
  useDerivedValue,
  useFrameCallback,
  useSharedValue,
} from 'react-native-reanimated';
// Type-only, so the Skia entry point never loads react-native-svg at runtime.
import type { UlottieHandles, UlottieMeta, UlottieProps, UlottieRef } from './index';

// --- the compiler's module contract (skia-aot target) ---

export interface UlottieSkiaHandles extends UlottieHandles {
  /** Record the current node state onto a canvas. */
  draw: (canvas: SkCanvas) => void;
}

export interface UlottieSkiaModule {
  /** The display-list descriptor; consumed by `init`, opaque to the player. */
  dl: unknown;
  meta: UlottieMeta;
  init: (Sk: typeof Skia) => UlottieSkiaHandles;
}

export type { UlottieMeta, UlottieProps, UlottieRef };

function emptyPicture(): SkPicture {
  const rec = Skia.PictureRecorder();
  rec.beginRecording(Skia.XYWHRect(0, 0, 1, 1));
  return rec.finishRecordingAsPicture();
}

export function createUlottieSkia(mod: UlottieSkiaModule) {
  const { meta, init } = mod;

  return forwardRef<UlottieRef, UlottieProps>(function UlottieSkia(props, ref) {
    const { progress, autoplay = true, loop = true, speed = 1, style } = props;
    const pinned = progress !== undefined;

    // UI-thread state; same clock as src/index.ts, but instead of shared-value
    // prop writes the dirty drain triggers a picture re-record.
    const handles = useSharedValue<UlottieSkiaHandles | null>(null);
    const clock = useSharedValue(meta.ip);
    const playing = useSharedValue(autoplay);
    const lastApplied = useSharedValue(NaN); // NaN ≠ anything → first frame always applies
    const recorded = useSharedValue(false);
    // useSharedValue reads its initial-value argument on every render, so the
    // placeholder is memoized — one native recorder+picture per mount, not
    // one per re-render.
    const blank = useMemo(emptyPicture, []);
    const picture = useSharedValue<SkPicture>(blank);

    // Mount-time warm-up, mirroring src/index.ts: overlap init() with the
    // commit; the frame callback keeps its lazy fallback because runOnUI
    // ordering against the first frame is not guaranteed.
    useEffect(() => {
      runOnUI(() => {
        'worklet';
        if (handles.value === null) handles.value = init(Skia);
      })();
    }, []);

    useFrameCallback((info) => {
      'worklet';
      let h = handles.value;
      if (h === null) {
        h = init(Skia);
        handles.value = h;
      }
      const ip = h.ip;
      const op = h.op;
      let frame: number;
      if (pinned) {
        frame = ip + (progress as number) * (op - ip);
      } else {
        if (playing.value) {
          const dt = (info.timeSincePreviousFrame ?? 0) / 1000;
          let f = clock.value + dt * h.fr * speed;
          if (f >= op) {
            if (loop) {
              f = ip + ((f - ip) % (op - ip));
            } else {
              f = op;
              playing.value = false;
            }
          }
          clock.value = f;
        }
        frame = clock.value;
      }
      if (frame !== lastApplied.value) {
        lastApplied.value = frame;
        h.apply(frame);
      }
      const dirty = h.dirty;
      // The converted writes already landed on the node records (skSet); the
      // drain only clears the queue — a non-empty queue means re-record.
      if (dirty.length === 0 && recorded.value) return;
      for (let i = 0; i < dirty.length; i++) dirty[i].d = 0;
      dirty.length = 0;
      recorded.value = true;
      const rec = Skia.PictureRecorder();
      const canvas = rec.beginRecording(Skia.XYWHRect(0, 0, meta.width, meta.height));
      h.draw(canvas);
      picture.value = rec.finishRecordingAsPicture();
    });

    useImperativeHandle(
      ref,
      () => ({
        play() {
          runOnUI(() => {
            'worklet';
            const op = handles.value?.op ?? meta.op;
            if (clock.value >= op) clock.value = handles.value?.ip ?? meta.ip;
            playing.value = true;
          })();
        },
        pause() {
          playing.value = false;
        },
        seek(p: number) {
          clock.value = meta.ip + p * (meta.op - meta.ip);
        },
        goToFrame(frame: number) {
          clock.value = frame;
        },
      }),
      [],
    );

    // `preserveAspectRatio="xMidYMid meet"` (the only value the compiler
    // emits): uniform scale, centered.
    const size = useSharedValue({ width: 0, height: 0 });
    const fit = useDerivedValue(() => {
      const cw = size.value.width;
      const ch = size.value.height;
      if (!cw || !ch) return Skia.Matrix();
      const s = Math.min(cw / meta.width, ch / meta.height);
      return Skia.Matrix([
        s, 0, (cw - meta.width * s) / 2,
        0, s, (ch - meta.height * s) / 2,
        0, 0, 1,
      ]);
    });

    // The user's style goes on a wrapper View so percentage-free Canvas
    // sizing ('flex: 1') resolves against it — same shape as the svg player.
    return React.createElement(
      View,
      { style },
      React.createElement(
        Canvas,
        { style: { flex: 1 }, onSize: size },
        React.createElement(Group, { matrix: fit }, React.createElement(Picture, { picture })),
      ),
    );
  });
}
