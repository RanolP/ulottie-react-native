#pragma once

#include <cstdint>
#include <memory>
#include <shared_mutex>
#include <unordered_map>

namespace ulottie {

/**
 * A render target `global.UlottieRtApi.renderFrame(nativeId, frame)` can
 * drive. Implemented by the platform view. Calls arrive on the platform main
 * thread (the worklets UI runtime rides the display link there).
 */
class RtViewHandle {
public:
  virtual ~RtViewHandle() = default;
  /** True only when the requested frame is now what the surface shows —
   * either freshly rasterized and published, or an exact early-out because
   * the same frame is already on an unchanged surface. False when nothing
   * could be drawn (pre-layout, no scene loaded, render failure): the caller
   * keeps asking every tick, so the first tick after layout paints. */
  virtual bool renderFrame(double frame) = 0;
  /** Hands the view's rasterizer its RTDL blob (once, at mount). The bytes
   * are only borrowed for the call. False while the view is torn down or the
   * blob does not decode — the caller may retry (mount ordering). */
  virtual bool loadAnimation(const uint8_t *bytes, size_t len) = 0;
};

/**
 * Process-global map nativeId -> view handle.
 *
 * The JS frame loop lives on the worklets UI runtime while unmount comes from
 * React's commit, so a renderFrame for a view that was just torn down is a
 * guaranteed race (rn-skia's registry documents the same one). remove() is
 * the tombstone: after it, get() returns null and the caller no-ops instead
 * of crashing.
 */
class RtRegistry {
public:
  static RtRegistry &instance();

  void add(int32_t nativeId, std::shared_ptr<RtViewHandle> view);
  void remove(int32_t nativeId);
  std::shared_ptr<RtViewHandle> get(int32_t nativeId);

private:
  std::shared_mutex mutex_;
  std::unordered_map<int32_t, std::shared_ptr<RtViewHandle>> views_;
};

} // namespace ulottie
