package dev.ulottie.rtshared;

import android.content.Context;
import android.graphics.Bitmap;
import android.graphics.Canvas;
import android.view.View;

/**
 * The backend-agnostic rasterizer surface: owns one Rust instance and the
 * Bitmap it renders into. ensureBitmap/publishFrame are called from JNI
 * (UlottieRtAndroidAdapter.cpp in libulottiertshared.so) on the UI thread —
 * the worklets rAF loop rides ReactChoreographer's UI-thread callback.
 *
 * A backend package subclasses this and routes the four abstract methods to
 * its own UlottieRtNative (whose static initializer loads that backend's
 * .so); everything else — bitmap ownership, draw, registry lifecycle — is
 * shared here.
 */
public abstract class UlottieRtBaseView extends View {
  private final long mRustId;
  private int mNativeId = 0;
  private Bitmap mBitmap;

  protected UlottieRtBaseView(Context context) {
    super(context);
    mRustId = createInstance();
  }

  /** {@code UlottieRtNative.nativeCreateInstance()} of this backend. */
  protected abstract long createInstance();

  /** {@code UlottieRtNative.nativeDestroyInstance(rustId)}. */
  protected abstract void destroyInstance(long rustId);

  /** {@code UlottieRtNative.nativeRegister(nativeId, this, rustId)}. */
  protected abstract void registerView(int nativeId, long rustId);

  /** {@code UlottieRtNative.nativeUnregister(nativeId)}. */
  protected abstract void unregisterView(int nativeId);

  public void setNativeIdProp(int nativeId) {
    if (nativeId == mNativeId) {
      return;
    }
    if (mNativeId != 0) {
      unregisterView(mNativeId);
    }
    mNativeId = nativeId;
    if (nativeId != 0) {
      registerView(nativeId, mRustId);
    }
  }

  /** JNI: returns the frame buffer, sized to the current layout; null before layout. */
  Bitmap ensureBitmap() {
    int w = getWidth();
    int h = getHeight();
    if (w <= 0 || h <= 0) {
      return null;
    }
    if (mBitmap == null || mBitmap.getWidth() != w || mBitmap.getHeight() != h) {
      mBitmap = Bitmap.createBitmap(w, h, Bitmap.Config.ARGB_8888);
    }
    return mBitmap;
  }

  /** JNI: the Rust side finished writing a frame into the bitmap. */
  void publishFrame() {
    invalidate();
  }

  public void tearDown() {
    if (mNativeId != 0) {
      unregisterView(mNativeId);
      mNativeId = 0;
    }
    destroyInstance(mRustId);
    mBitmap = null;
  }

  @Override
  protected void onDraw(Canvas canvas) {
    super.onDraw(canvas);
    if (mBitmap != null) {
      canvas.drawBitmap(mBitmap, 0f, 0f, null);
    }
  }
}
