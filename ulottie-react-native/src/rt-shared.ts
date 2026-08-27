import React, { forwardRef, useEffect, useImperativeHandle, useMemo } from 'react';
import type { ComponentType } from 'react';
import type { StyleProp, ViewStyle } from 'react-native';
import { scheduleOnUI } from 'react-native-worklets';
// Type-only, so this entry point never loads react-native-svg at runtime.
import type { UlottieMeta, UlottieProps, UlottieRef } from './index';

// --- the compiler's module contract (rt target) ---

export interface UlottieRtModule {
  /** The base64 RTDL blob; handed to the native rasterizer once, at mount. */
  rtdl: string;
  meta: UlottieMeta;
  init: () => UlottieMeta;
}

/**
 * What a platform package (e.g. ulottie-react-native-rt-tiny-skia) plugs in:
 * its Fabric surface view and the JSI installer.
 */
export interface UlottieRtEnv {
  View: ComponentType<{ surfaceId: number; style?: StyleProp<ViewStyle> }>;
  install: () => boolean;
}

/**
 * JS allocates view identities, exactly like rn-skia's SkiaViewNativeId. One
 * counter here serves every backend package — all of them feed the one shared
 * native registry, so a single sequence is what keeps ids disjoint.
 */
let nextNativeId = 1000;
export function allocateNativeId(): number {
  return nextNativeId++;
}

type UlottieRtApi = {
  renderFrame(nativeId: number, frame: number): boolean;
  loadAnimation(nativeId: number, rtdlBase64: string): boolean;
};

/** Per-player UI-runtime state, keyed by nativeId on the worklets runtime. */
type PlayerState = {
  alive: boolean;
  loaded: boolean;
  playing: boolean;
  clock: number;
  /** Absolute frame while the `progress` prop pins playback, else null. */
  pinned: number | null;
  loop: boolean;
  speed: number;
  /** Failed loadAnimation attempts so far; bounded — see the tick. */
  loadTries: number;
};

type Players = { [nativeId: number]: PlayerState };

function playersOn(g: unknown): Players {
  'worklet';
  const holder = g as { __ulottieRtPlayers?: Players };
  return (holder.__ulottieRtPlayers = holder.__ulottieRtPlayers ?? {});
}

export type { UlottieMeta, UlottieProps, UlottieRef };

/**
 * The rt player: the same props/ref contract as the svg and skia players, but
 * the whole clock lives in one `requestAnimationFrame` worklet on the
 * react-native-worklets UI runtime (no reanimated). Per frame the only JS→
 * native traffic is `UlottieRtApi.renderFrame(nativeId, frame)`; the RTDL
 * blob crosses once, retried until the Fabric commit lands the view (the loop
 * starts from a React effect, which wins no ordering guarantee against the
 * mount).
 */
export function createUlottieRtPlayer(env: UlottieRtEnv, mod: UlottieRtModule) {
  const { rtdl, meta } = mod;

  return forwardRef<UlottieRef, UlottieProps>(function UlottieRt(props, ref) {
    const { progress, autoplay = true, loop = true, speed = 1, style } = props;
    const nativeId = useMemo(allocateNativeId, []);

    // Mount: install the JSI api and start this player's frame loop. The
    // cleanup only flips `alive`; the loop notices on its next tick and
    // unregisters itself — renderFrame on a torn-down view is a native no-op,
    // so the race with unmount is safe by construction.
    useEffect(() => {
      env.install();
      // Capture the host object as a local before the worklet is created:
      // worklets serializes host objects by reference, and a bare `global.X`
      // inside the worklet fails under the babel plugin's strictGlobal mode.
      const api = (globalThis as { UlottieRtApi?: UlottieRtApi }).UlottieRtApi;
      if (!api) {
        throw new Error('ulottie-rt: install() did not expose global.UlottieRtApi');
      }
      const initialPlaying = autoplay;
      scheduleOnUI(() => {
        'worklet';
        const players = playersOn(globalThis);
        const st: PlayerState = (players[nativeId] = {
          alive: true,
          loaded: false,
          playing: initialPlaying,
          clock: meta.ip,
          pinned: null,
          loop: true,
          speed: 1,
          loadTries: 0,
        });
        let prevTs = -1;
        const tick = (ts: number) => {
          if (!st.alive) {
            delete players[nativeId];
            return;
          }
          if (!st.loaded) {
            // Retries exist only for the mount race (the view's Fabric commit
            // may land after this loop starts). A blob the native side keeps
            // refusing — bad base64, RTDL version skew — will never succeed,
            // so give up after ~2s of frames instead of retrying forever.
            st.loaded = api.loadAnimation(nativeId, rtdl);
            if (!st.loaded && ++st.loadTries >= 120) {
              console.warn(
                `ulottie-rt: loadAnimation still failing after ${st.loadTries} attempts ` +
                  `for view ${nativeId}; stopping this player (undecodable RTDL blob, ` +
                  'or a compiler/runtime version mismatch)',
              );
              delete players[nativeId];
              return;
            }
          }
          const dt = prevTs < 0 ? 0 : (ts - prevTs) / 1000;
          prevTs = ts;
          let frame: number;
          if (st.pinned !== null) {
            frame = st.pinned;
          } else {
            if (st.playing) {
              let f = st.clock + dt * meta.fr * st.speed;
              if (f >= meta.op) {
                if (st.loop) {
                  f = meta.ip + ((f - meta.ip) % (meta.op - meta.ip));
                } else {
                  f = meta.op;
                  st.playing = false;
                }
              }
              st.clock = f;
            }
            frame = st.clock;
          }
          // Called every tick: the native view tracks its own last-rendered
          // frame and surface size, so an unchanged frame on an unchanged
          // surface is a cheap native early-out — while a pre-layout no-op or
          // a resize re-renders on the next tick with no JS-side staleness.
          if (st.loaded) {
            api.renderFrame(nativeId, frame);
          }
          requestAnimationFrame(tick);
        };
        requestAnimationFrame(tick);
      });
      return () => {
        scheduleOnUI(() => {
          'worklet';
          const st = playersOn(globalThis)[nativeId];
          if (st) {
            st.alive = false;
          }
        });
      };
    }, []);

    // Prop changes ride to the UI runtime as plain state writes. This effect
    // is declared after the mount effect, so on first render the state object
    // already exists when it runs.
    useEffect(() => {
      const pinned = progress !== undefined ? meta.ip + progress * (meta.op - meta.ip) : null;
      scheduleOnUI(() => {
        'worklet';
        const st = playersOn(globalThis)[nativeId];
        if (st) {
          st.pinned = pinned;
          st.loop = loop;
          st.speed = speed;
        }
      });
    }, [progress, loop, speed]);

    useImperativeHandle(
      ref,
      () => ({
        play() {
          scheduleOnUI(() => {
            'worklet';
            const st = playersOn(globalThis)[nativeId];
            if (st) {
              if (st.clock >= meta.op) {
                st.clock = meta.ip;
              }
              st.playing = true;
            }
          });
        },
        pause() {
          scheduleOnUI(() => {
            'worklet';
            const st = playersOn(globalThis)[nativeId];
            if (st) {
              st.playing = false;
            }
          });
        },
        seek(p: number) {
          const frame = meta.ip + p * (meta.op - meta.ip);
          scheduleOnUI(() => {
            'worklet';
            const st = playersOn(globalThis)[nativeId];
            if (st) {
              st.clock = frame;
            }
          });
        },
        goToFrame(frame: number) {
          scheduleOnUI(() => {
            'worklet';
            const st = playersOn(globalThis)[nativeId];
            if (st) {
              st.clock = frame;
            }
          });
        },
      }),
      [],
    );

    // The native view letterboxes for itself (`xMidYMid meet` in ffi.rs), so
    // the surface simply fills the styled box.
    return React.createElement(env.View, { surfaceId: nativeId, style });
  });
}
