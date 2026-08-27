// The tiny-skia backend's whole JNI surface: hand the `ulottie_rt_*` C ABI
// table to the shared adapter (libulottiertshared.so, prefab), which owns the
// view/bitmap/blit logic and binds this package's UlottieRtNative methods.

#include <jni.h>

#include <UlottieRtShared/UlottieRtAndroidAdapter.h>

#include "ulottie_rt.h"

extern "C" JNIEXPORT jint JNI_OnLoad(JavaVM *vm, void *) {
  const UlottieRtBackendFns fns = {
      ulottie_rt_instance_create,     ulottie_rt_instance_destroy,
      ulottie_rt_instance_load,       ulottie_rt_instance_set_buffer,
      ulottie_rt_render_frame,
  };
  return ulottie::registerRtAndroidAdapter(
      vm, "dev/ulottie/rttinyskia/UlottieRtNative", fns);
}
