package dev.ulottie.rttinyskia;

import com.facebook.react.uimanager.SimpleViewManager;
import com.facebook.react.uimanager.ThemedReactContext;
import com.facebook.react.uimanager.ViewManagerDelegate;
import com.facebook.react.viewmanagers.UlottieRtViewManagerDelegate;
import com.facebook.react.viewmanagers.UlottieRtViewManagerInterface;

public class UlottieRtViewManager extends SimpleViewManager<UlottieRtView>
    implements UlottieRtViewManagerInterface<UlottieRtView> {
  public static final String REACT_CLASS = "UlottieRtView";

  private final ViewManagerDelegate<UlottieRtView> mDelegate =
      new UlottieRtViewManagerDelegate<>(this);

  @Override
  public String getName() {
    return REACT_CLASS;
  }

  @Override
  protected ViewManagerDelegate<UlottieRtView> getDelegate() {
    return mDelegate;
  }

  @Override
  protected UlottieRtView createViewInstance(ThemedReactContext context) {
    return new UlottieRtView(context);
  }

  @Override
  public void setSurfaceId(UlottieRtView view, int value) {
    view.setNativeIdProp(value);
  }

  @Override
  public void onDropViewInstance(UlottieRtView view) {
    super.onDropViewInstance(view);
    view.tearDown();
  }
}
