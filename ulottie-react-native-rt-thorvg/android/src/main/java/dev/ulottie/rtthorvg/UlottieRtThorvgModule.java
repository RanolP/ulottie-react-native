package dev.ulottie.rtthorvg;

import com.facebook.react.bridge.ReactApplicationContext;
import com.facebook.react.module.annotations.ReactModule;

@ReactModule(name = UlottieRtThorvgModule.NAME)
public class UlottieRtThorvgModule extends NativeUlottieRtThorvgModuleSpec {
  public UlottieRtThorvgModule(ReactApplicationContext context) {
    super(context);
  }

  /**
   * Synchronous — runs on the JS thread, so writing into the runtime is safe
   * (same pattern as @shopify/react-native-skia's RNSkiaModule.install).
   */
  @Override
  public boolean install() {
    try {
      UlottieRtNative.ensureLoaded();
      long runtimePtr =
          getReactApplicationContext().getJavaScriptContextHolder().get();
      if (runtimePtr == 0) {
        return false;
      }
      UlottieRtNative.nativeInstall(runtimePtr);
      return true;
    } catch (Throwable t) {
      android.util.Log.e("UlottieRtThorvg", "install() failed", t);
      return false;
    }
  }
}
