package dev.ulottie.rtshared;

import com.facebook.react.ReactPackage;
import com.facebook.react.bridge.NativeModule;
import com.facebook.react.bridge.ReactApplicationContext;
import com.facebook.react.uimanager.ViewManager;

import java.util.Collections;
import java.util.List;

/**
 * Intentionally empty: this package exists so React Native autolinking
 * accepts the library. The real content is native — libulottiertshared.so
 * (JSI api + view registry), which the rasterizer packages link via prefab.
 */
public class UlottieRtSharedPackage implements ReactPackage {
  @Override
  public List<NativeModule> createNativeModules(ReactApplicationContext reactContext) {
    return Collections.emptyList();
  }

  @Override
  public List<ViewManager> createViewManagers(ReactApplicationContext reactContext) {
    return Collections.emptyList();
  }
}
