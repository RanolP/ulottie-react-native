package dev.ulottie.rttinyskia;

import com.facebook.react.TurboReactPackage;
import com.facebook.react.bridge.NativeModule;
import com.facebook.react.bridge.ReactApplicationContext;
import com.facebook.react.module.model.ReactModuleInfo;
import com.facebook.react.module.model.ReactModuleInfoProvider;
import com.facebook.react.uimanager.ViewManager;

import java.util.Collections;
import java.util.HashMap;
import java.util.List;
import java.util.Map;

public class UlottieRtTinySkiaPackage extends TurboReactPackage {
  @Override
  public NativeModule getModule(String name, ReactApplicationContext context) {
    if (UlottieRtModule.NAME.equals(name)) {
      return new UlottieRtModule(context);
    }
    return null;
  }

  @Override
  public ReactModuleInfoProvider getReactModuleInfoProvider() {
    return () -> {
      Map<String, ReactModuleInfo> map = new HashMap<>();
      map.put(
          UlottieRtModule.NAME,
          new ReactModuleInfo(
              UlottieRtModule.NAME,
              UlottieRtModule.class.getName(),
              false, // canOverrideExistingModule
              false, // needsEagerInit
              false, // isCxxModule
              true // isTurboModule
              ));
      return map;
    };
  }

  @Override
  public List<ViewManager> createViewManagers(ReactApplicationContext context) {
    return Collections.singletonList(new UlottieRtViewManager());
  }
}
