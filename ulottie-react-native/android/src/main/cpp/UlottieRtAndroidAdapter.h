#pragma once

#include <jni.h>

#include "UlottieRtBackend.h"

namespace ulottie {

/**
 * Binds one rasterizer backend to Java: registers the five
 * `UlottieRtNative.native*` methods on `nativeClassName` (JNI slashed name,
 * e.g. "dev/ulottie/rttinyskia/UlottieRtNative"), each backed by `fns` and by
 * the shared view/bitmap/blit logic in UlottieRtAndroidAdapter.cpp. The view
 * side is always `dev.ulottie.rtshared.UlottieRtBaseView` — the backend
 * package's view is a trivial subclass.
 *
 * Call from the backend package's `JNI_OnLoad`; returns JNI_VERSION_1_6, or
 * JNI_ERR on failure (already logged via android.util.Log semantics).
 */
jint registerRtAndroidAdapter(JavaVM *vm, const char *nativeClassName,
                              const UlottieRtBackendFns &fns);

} // namespace ulottie
