#pragma once

#include <jsi/jsi.h>

namespace ulottie {

/**
 * Installs the `global.UlottieRtApi` host object into `runtime`:
 *   renderFrame(nativeId: number, frame: number): boolean
 * Safe to call more than once (overwrites with a fresh instance).
 */
void installUlottieRtApi(facebook::jsi::Runtime &runtime);

} // namespace ulottie
