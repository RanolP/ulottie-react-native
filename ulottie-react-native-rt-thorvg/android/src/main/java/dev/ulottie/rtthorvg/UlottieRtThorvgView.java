package dev.ulottie.rtthorvg;

import android.content.Context;

import dev.ulottie.rtshared.UlottieRtBaseView;

/**
 * The ThorVG rasterizer surface: UlottieRtBaseView (which owns the
 * bitmap/draw/registry mechanics) routed to this package's UlottieRtNative —
 * whose loadLibrary pulls in the ThorVG backend .so.
 */
public class UlottieRtThorvgView extends UlottieRtBaseView {
  public UlottieRtThorvgView(Context context) {
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
