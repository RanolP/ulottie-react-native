#include "UlottieRtApi.h"
#include "UlottieRtRegistry.h"

#include <array>
#include <cstdint>
#include <string>
#include <vector>

#ifdef __ANDROID__
#include <android/log.h>
#else
#include <cstdio>
#endif

namespace ulottie {

using namespace facebook;

namespace {

void logError(const char *message) {
#ifdef __ANDROID__
  __android_log_print(ANDROID_LOG_ERROR, "UlottieRt", "%s", message);
#else
  fprintf(stderr, "UlottieRt: %s\n", message);
#endif
}

/**
 * Standard-alphabet base64 → bytes; whitespace skipped, `=` padding accepted.
 * Returns an empty vector on any other character — the compiler's `rtdl`
 * export is machine-written, so a bad decode means a wrong argument, not a
 * formatting variant worth tolerating.
 */
std::vector<uint8_t> decodeBase64(const std::string &in) {
  static const auto table = [] {
    std::array<int8_t, 256> t;
    t.fill(-1);
    const char *alphabet =
        "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    for (int i = 0; i < 64; i++) {
      t[static_cast<uint8_t>(alphabet[i])] = static_cast<int8_t>(i);
    }
    return t;
  }();
  std::vector<uint8_t> out;
  out.reserve(in.size() / 4 * 3);
  uint32_t acc = 0;
  int bits = 0;
  for (char c : in) {
    if (c == '=' || c == '\n' || c == '\r' || c == ' ') {
      continue;
    }
    int8_t v = table[static_cast<uint8_t>(c)];
    if (v < 0) {
      return {};
    }
    acc = (acc << 6) | static_cast<uint32_t>(v);
    bits += 6;
    if (bits >= 8) {
      bits -= 8;
      out.push_back(static_cast<uint8_t>((acc >> bits) & 0xff));
    }
  }
  return out;
}

class UlottieRtApiHostObject : public jsi::HostObject {
public:
  jsi::Value get(jsi::Runtime &rt, const jsi::PropNameID &name) override {
    auto prop = name.utf8(rt);
    if (prop == "renderFrame") {
      return jsi::Function::createFromHostFunction(
          rt, name, 2,
          [](jsi::Runtime &rt, const jsi::Value &, const jsi::Value *args,
             size_t count) -> jsi::Value {
            if (count < 2 || !args[0].isNumber() || !args[1].isNumber()) {
              throw jsi::JSError(
                  rt, "UlottieRtApi.renderFrame expects (nativeId, frame)");
            }
            auto view = RtRegistry::instance().get(
                static_cast<int32_t>(args[0].asNumber()));
            if (!view) {
              // Torn down (or not yet mounted): the frame loop outlives the
              // view by design, so this is a silent no-op, never a crash.
              return jsi::Value(false);
            }
            // True only when the frame is actually on the surface — a
            // pre-layout view returns false and the loop retries next tick.
            return jsi::Value(view->renderFrame(args[1].asNumber()));
          });
    }
    if (prop == "loadAnimation") {
      return jsi::Function::createFromHostFunction(
          rt, name, 2,
          [](jsi::Runtime &rt, const jsi::Value &, const jsi::Value *args,
             size_t count) -> jsi::Value {
            if (count < 2 || !args[0].isNumber() || !args[1].isString()) {
              throw jsi::JSError(
                  rt,
                  "UlottieRtApi.loadAnimation expects (nativeId, rtdlBase64)");
            }
            auto bytes = decodeBase64(args[1].asString(rt).utf8(rt));
            if (bytes.empty()) {
              // Never throw here: this call runs inside the worklets rAF
              // tick, where an escaping JSError would kill the frame loop
              // silently. Log once, return false — the JS side bounds its
              // retries and warns with the nativeId.
              static bool warnedBadBase64 = false;
              if (!warnedBadBase64) {
                warnedBadBase64 = true;
                logError(
                    "UlottieRtApi.loadAnimation: rtdl is not valid base64");
              }
              return jsi::Value(false);
            }
            auto view = RtRegistry::instance().get(
                static_cast<int32_t>(args[0].asNumber()));
            // False while the view has not mounted yet — the player loop
            // retries until the Fabric commit lands the view.
            return jsi::Value(view != nullptr &&
                              view->loadAnimation(bytes.data(), bytes.size()));
          });
    }
    return jsi::Value::undefined();
  }

  std::vector<jsi::PropNameID> getPropertyNames(jsi::Runtime &rt) override {
    return jsi::PropNameID::names(rt, "renderFrame", "loadAnimation");
  }
};

} // namespace

void installUlottieRtApi(jsi::Runtime &runtime) {
  runtime.global().setProperty(
      runtime, "UlottieRtApi",
      jsi::Object::createFromHostObject(
          runtime, std::make_shared<UlottieRtApiHostObject>()));
}

} // namespace ulottie
