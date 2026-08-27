package dev.ulottie.rtthorvg;

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

public class UlottieRtThorvgPackage extends TurboReactPackage {
  @Override
  public NativeModule getModule(String name, ReactApplicationContext context) {
    if (UlottieRtThorvgModule.NAME.equals(name)) {
      return new UlottieRtThorvgModule(context);
    }
    return null;
  }

  @Override
  public ReactModuleInfoProvider getReactModuleInfoProvider() {
    return () -> {
      Map<String, ReactModuleInfo> map = new HashMap<>();
      map.put(
          UlottieRtThorvgModule.NAME,
          new ReactModuleInfo(
              UlottieRtThorvgModule.NAME,
              UlottieRtThorvgModule.class.getName(),
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
    return Collections.singletonList(new UlottieRtThorvgViewManager());
  }
}
