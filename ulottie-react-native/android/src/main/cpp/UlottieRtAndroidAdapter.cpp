// The Android half of the shared rt view layer: bridges the shared C++
// registry to the Java UlottieRtBaseView, and a backend's C ABI table
// (UlottieRtBackendFns) to Android Bitmap pixels. Compiled once into
// libulottiertshared.so; each backend package's JNI_OnLoad hands in its
// function table via registerRtAndroidAdapter.
//
// Threading: renderFrame arrives on the Android UI thread (the worklets rAF
// loop runs on ReactChoreographer's UI-thread callback), so invalidate() via
// publishFrame() is legal directly. env() still attaches defensively.
//
// Pixel contract: Bitmap.Config.ARGB_8888 stores premultiplied RGBA bytes in
// memory — exactly what the Rust side writes. The Rust buffer pointer is only
// valid between lockPixels/unlockPixels, so set_buffer + render_frame happen
// inside that window every frame; the pointer is re-set each frame and never
// dereferenced outside it.

#include "UlottieRtAndroidAdapter.h"

#include <android/bitmap.h>
#include <android/log.h>

#include "UlottieRtApi.h"
#include "UlottieRtRegistry.h"

#include <cmath>
#include <memory>
#include <string>

#define LOG_TAG "UlottieRtShared"
#define LOGE(...) __android_log_print(ANDROID_LOG_ERROR, LOG_TAG, __VA_ARGS__)

namespace {

JavaVM *gVm = nullptr;
jmethodID gEnsureBitmap = nullptr; // UlottieRtBaseView.ensureBitmap()Landroid/graphics/Bitmap;
jmethodID gPublishFrame = nullptr; // UlottieRtBaseView.publishFrame()V

JNIEnv *env() {
  JNIEnv *e = nullptr;
  if (gVm->GetEnv(reinterpret_cast<void **>(&e), JNI_VERSION_1_6) == JNI_OK) {
    return e;
  }
  if (gVm->AttachCurrentThread(&e, nullptr) == JNI_OK) {
    return e;
  }
  return nullptr;
}

class AndroidViewHandle : public ulottie::RtViewHandle {
public:
  AndroidViewHandle(JNIEnv *e, jobject view, uint64_t rustId,
                    const UlottieRtBackendFns *fns)
      : weakView_(e->NewWeakGlobalRef(view)), rustId_(rustId), fns_(fns) {}

  ~AndroidViewHandle() override {
    if (JNIEnv *e = env()) {
      e->DeleteWeakGlobalRef(weakView_);
    }
  }

  bool renderFrame(double frame) override {
    JNIEnv *e = env();
    if (e == nullptr) {
      return false;
    }
    jobject view = e->NewLocalRef(weakView_);
    if (view == nullptr) {
      return false; // View collected — tombstone, same as iOS __weak nil.
    }
    jobject bitmap = e->CallObjectMethod(view, gEnsureBitmap);
    if (e->ExceptionCheck()) {
      e->ExceptionDescribe();
      e->ExceptionClear();
      bitmap = nullptr;
    }
    if (bitmap == nullptr) {
      e->DeleteLocalRef(view);
      return false; // Not laid out yet — the rAF loop retries next tick.
    }

    AndroidBitmapInfo info;
    void *pixels = nullptr;
    bool ok = false;
    if (AndroidBitmap_getInfo(e, bitmap, &info) != ANDROID_BITMAP_RESULT_SUCCESS) {
      info = {};
    }
    if (frame == lastFrame_ && info.width == lastWidth_ &&
        info.height == lastHeight_ && info.width != 0) {
      // The bitmap already shows exactly this frame at this size; skip the
      // rasterize. A resize swaps the bitmap (ensureBitmap) and lands in the
      // render path below, so nothing ever stays stale.
      ok = true;
    } else if (info.format == ANDROID_BITMAP_FORMAT_RGBA_8888 &&
               info.stride == info.width * 4 &&
               AndroidBitmap_lockPixels(e, bitmap, &pixels) ==
                   ANDROID_BITMAP_RESULT_SUCCESS) {
      ok = fns_->instance_set_buffer(rustId_, static_cast<uint8_t *>(pixels),
                                     info.width, info.height, info.stride) &&
           fns_->render_frame(rustId_, static_cast<float>(frame));
      AndroidBitmap_unlockPixels(e, bitmap); // Bumps generation: hardware canvas re-uploads.
      if (ok) {
        lastFrame_ = frame;
        lastWidth_ = info.width;
        lastHeight_ = info.height;
        e->CallVoidMethod(view, gPublishFrame);
        if (e->ExceptionCheck()) {
          e->ExceptionDescribe();
          e->ExceptionClear();
        }
      }
    } else if (!warned_) {
      warned_ = true;
      LOGE("renderFrame: bitmap not tight RGBA_8888 (format=%d stride=%u width=%u)",
           info.format, info.stride, info.width);
    }

    e->DeleteLocalRef(bitmap);
    e->DeleteLocalRef(view);
    return ok;
  }

  bool loadAnimation(const uint8_t *data, size_t size) override {
    if (!fns_->instance_load(rustId_, data, size)) {
      return false;
    }
    lastFrame_ = NAN; // whatever the bitmap shows is not this scene
    return true;
  }

private:
  jweak weakView_;
  uint64_t rustId_;
  const UlottieRtBackendFns *fns_;
  // What the bitmap currently shows; NAN/0 until the first render succeeds.
  double lastFrame_ = NAN;
  uint32_t lastWidth_ = 0;
  uint32_t lastHeight_ = 0;
  bool warned_ = false;
};

// RegisterNatives binds bare function pointers, so per-backend state rides in
// indexed slots and one trampoline instantiation per slot. Two backends exist
// today (tiny-skia, ThorVG); four slots is headroom, not a design point.
constexpr int kMaxBackends = 4;
UlottieRtBackendFns gFns[kMaxBackends];
int gFnsCount = 0;

template <int I> jlong nativeCreateInstance(JNIEnv *, jclass) {
  return static_cast<jlong>(gFns[I].instance_create());
}

template <int I> void nativeDestroyInstance(JNIEnv *, jclass, jlong rustId) {
  gFns[I].instance_destroy(static_cast<uint64_t>(rustId));
}

template <int I>
void nativeRegister(JNIEnv *e, jclass, jint nativeId, jobject view, jlong rustId) {
  ulottie::RtRegistry::instance().add(
      static_cast<int32_t>(nativeId),
      std::make_shared<AndroidViewHandle>(
          e, view, static_cast<uint64_t>(rustId), &gFns[I]));
}

void nativeUnregister(JNIEnv *, jclass, jint nativeId) {
  ulottie::RtRegistry::instance().remove(static_cast<int32_t>(nativeId));
}

void nativeInstall(JNIEnv *, jclass, jlong runtimePtr) {
  auto *runtime = reinterpret_cast<facebook::jsi::Runtime *>(runtimePtr);
  if (runtime != nullptr) {
    ulottie::installUlottieRtApi(*runtime);
  }
}

struct SlotFns {
  void *create;
  void *destroy;
  void *reg;
};

// reinterpret_cast keeps this out of constexpr; load-time init is fine.
template <int I> SlotFns slotFns() {
  return {reinterpret_cast<void *>(&nativeCreateInstance<I>),
          reinterpret_cast<void *>(&nativeDestroyInstance<I>),
          reinterpret_cast<void *>(&nativeRegister<I>)};
}

const SlotFns kSlots[kMaxBackends] = {slotFns<0>(), slotFns<1>(), slotFns<2>(),
                                      slotFns<3>()};

} // namespace

namespace ulottie {

jint registerRtAndroidAdapter(JavaVM *vm, const char *nativeClassName,
                              const UlottieRtBackendFns &fns) {
  gVm = vm;
  JNIEnv *e = nullptr;
  if (vm->GetEnv(reinterpret_cast<void **>(&e), JNI_VERSION_1_6) != JNI_OK) {
    return JNI_ERR;
  }
  if (gEnsureBitmap == nullptr || gPublishFrame == nullptr) {
    jclass viewClass = e->FindClass("dev/ulottie/rtshared/UlottieRtBaseView");
    if (viewClass == nullptr) {
      LOGE("UlottieRtBaseView not found — is ulottie-react-native's android "
           "library on the classpath?");
      return JNI_ERR;
    }
    gEnsureBitmap =
        e->GetMethodID(viewClass, "ensureBitmap", "()Landroid/graphics/Bitmap;");
    gPublishFrame = e->GetMethodID(viewClass, "publishFrame", "()V");
    if (gEnsureBitmap == nullptr || gPublishFrame == nullptr) {
      return JNI_ERR;
    }
  }
  if (gFnsCount >= kMaxBackends) {
    LOGE("too many rt backends registered (max %d)", kMaxBackends);
    return JNI_ERR;
  }
  const int slot = gFnsCount++;
  gFns[slot] = fns;
  jclass nativeClass = e->FindClass(nativeClassName);
  if (nativeClass == nullptr) {
    LOGE("%s not found", nativeClassName);
    return JNI_ERR;
  }
  const JNINativeMethod methods[] = {
      {"nativeCreateInstance", "()J", kSlots[slot].create},
      {"nativeDestroyInstance", "(J)V", kSlots[slot].destroy},
      {"nativeRegister", "(ILdev/ulottie/rtshared/UlottieRtBaseView;J)V",
       kSlots[slot].reg},
      {"nativeUnregister", "(I)V", reinterpret_cast<void *>(&nativeUnregister)},
      {"nativeInstall", "(J)V", reinterpret_cast<void *>(&nativeInstall)},
  };
  if (e->RegisterNatives(nativeClass, methods,
                         sizeof(methods) / sizeof(methods[0])) != JNI_OK) {
    LOGE("RegisterNatives failed for %s", nativeClassName);
    return JNI_ERR;
  }
  return JNI_VERSION_1_6;
}

} // namespace ulottie
