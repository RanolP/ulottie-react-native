#include "UlottieRtRegistry.h"

namespace ulottie {

RtRegistry &RtRegistry::instance() {
  static RtRegistry registry;
  return registry;
}

void RtRegistry::add(int32_t nativeId, std::shared_ptr<RtViewHandle> view) {
  std::unique_lock lock(mutex_);
  views_[nativeId] = std::move(view);
}

void RtRegistry::remove(int32_t nativeId) {
  std::unique_lock lock(mutex_);
  views_.erase(nativeId);
}

std::shared_ptr<RtViewHandle> RtRegistry::get(int32_t nativeId) {
  std::shared_lock lock(mutex_);
  auto it = views_.find(nativeId);
  return it == views_.end() ? nullptr : it->second;
}

} // namespace ulottie
