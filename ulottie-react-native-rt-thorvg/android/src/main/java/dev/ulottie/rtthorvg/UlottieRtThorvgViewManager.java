package dev.ulottie.rtthorvg;

import com.facebook.react.uimanager.SimpleViewManager;
import com.facebook.react.uimanager.ThemedReactContext;
import com.facebook.react.uimanager.ViewManagerDelegate;
import com.facebook.react.viewmanagers.UlottieRtThorvgViewManagerDelegate;
import com.facebook.react.viewmanagers.UlottieRtThorvgViewManagerInterface;

public class UlottieRtThorvgViewManager extends SimpleViewManager<UlottieRtThorvgView>
    implements UlottieRtThorvgViewManagerInterface<UlottieRtThorvgView> {
  public static final String REACT_CLASS = "UlottieRtThorvgView";

  private final ViewManagerDelegate<UlottieRtThorvgView> mDelegate =
      new UlottieRtThorvgViewManagerDelegate<>(this);

  @Override
  public String getName() {
    return REACT_CLASS;
  }

  @Override
  protected ViewManagerDelegate<UlottieRtThorvgView> getDelegate() {
    return mDelegate;
  }

  @Override
  protected UlottieRtThorvgView createViewInstance(ThemedReactContext context) {
    return new UlottieRtThorvgView(context);
  }

  @Override
  public void setSurfaceId(UlottieRtThorvgView view, int value) {
    view.setNativeIdProp(value);
  }

  @Override
  public void onDropViewInstance(UlottieRtThorvgView view) {
    super.onDropViewInstance(view);
    view.tearDown();
  }
}
