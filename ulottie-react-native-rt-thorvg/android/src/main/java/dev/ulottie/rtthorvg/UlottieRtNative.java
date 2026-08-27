package dev.ulottie.rtthorvg;

import dev.ulottie.rtshared.UlottieRtBaseView;

/**
 * JNI surface; methods are bound via RegisterNatives in the shared adapter
 * (registerRtAndroidAdapter, called from this package's JNI_OnLoad).
 */
final class UlottieRtNative {
  static {
    System.loadLibrary("ulottiertthorvg");
  }

  private UlottieRtNative() {}

  /** Forces the static initializer (library load) to run. */
  static void ensureLoaded() {}

  static native long nativeCreateInstance();

  static native void nativeDestroyInstance(long rustId);

  static native void nativeRegister(int nativeId, UlottieRtBaseView view, long rustId);

  static native void nativeUnregister(int nativeId);

  /** Installs global.UlottieRtApi into the JSI runtime at {@code runtimePtr}. */
  static native void nativeInstall(long runtimePtr);
}
