package dev.ulottie.rttinyskia;

import android.content.Context;

import dev.ulottie.rtshared.UlottieRtBaseView;

/**
 * The tiny-skia rasterizer surface: UlottieRtBaseView (which owns the
 * bitmap/draw/registry mechanics) routed to this package's UlottieRtNative —
 * whose loadLibrary pulls in the tiny-skia backend .so.
 */
public class UlottieRtView extends UlottieRtBaseView {
  public UlottieRtView(Context context) {
    super(context);
  }

  @Override
  protected long createInstance() {
    return UlottieRtNative.nativeCreateInstance();
  }

  @Override
  protected void destroyInstance(long rustId) {
    UlottieRtNative.nativeDestroyInstance(rustId);
  }

  @Override
  protected void registerView(int nativeId, long rustId) {
    UlottieRtNative.nativeRegister(nativeId, this, rustId);
  }

  @Override
  protected void unregisterView(int nativeId) {
    UlottieRtNative.nativeUnregister(nativeId);
  }
}
