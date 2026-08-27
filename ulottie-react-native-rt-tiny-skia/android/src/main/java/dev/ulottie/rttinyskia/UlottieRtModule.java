package dev.ulottie.rttinyskia;

import com.facebook.react.bridge.ReactApplicationContext;
import com.facebook.react.module.annotations.ReactModule;

@ReactModule(name = UlottieRtModule.NAME)
public class UlottieRtModule extends NativeUlottieRtModuleSpec {
  public UlottieRtModule(ReactApplicationContext context) {
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
      android.util.Log.e("UlottieRt", "install() failed", t);
      return false;
    }
  }
}
