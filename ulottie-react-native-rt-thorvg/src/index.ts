import {
  createUlottieRtPlayer,
  type UlottieProps,
  type UlottieRef,
  type UlottieRtModule,
} from 'ulottie-react-native/rt-shared';
import NativeUlottieRtThorvgModule from './specs/NativeUlottieRtThorvgModule';
import UlottieRtThorvgViewNativeComponent from './specs/UlottieRtThorvgViewNativeComponent';

export type { UlottieProps, UlottieRef, UlottieRtModule };

/** The Fabric view whose surface `renderFrame(nativeId, frame)` draws into. */
export const UlottieRtThorvgView = UlottieRtThorvgViewNativeComponent;

/**
 * Installs `global.UlottieRtApi` on the RN runtime (idempotent — the binding
 * is shared with the other rasterizer pods and dispatches by nativeId).
 * Importing the spec already created the TurboModule and with it the binding;
 * this call is the explicit, checkable form.
 */
export function install(): boolean {
  return NativeUlottieRtThorvgModule.install();
}

/**
 * A player component for one compiled `--target rt` module (the
 * `*.rt.lottie.json` Metro convention): ThorVG's SW engine rasterizes
 * natively, the shared worklets clock drives it — see
 * ulottie-react-native/src/rt-shared.ts for the loop and the props/ref
 * contract it shares with the other players. (View identities come from
 * rt-shared's single counter — one sequence for every backend feeding the
 * one shared native registry.)
 */
export function createUlottieRt(mod: UlottieRtModule) {
  return createUlottieRtPlayer({ View: UlottieRtThorvgView, install }, mod);
}
