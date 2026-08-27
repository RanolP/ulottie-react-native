#import "UlottieRtBaseView.h"

#import <UlottieRtShared/UlottieRtRegistry.h>

#include <cmath>

namespace {

/**
 * Registry-facing handle. Holds the view weakly so a registration the JS side
 * failed to remove can never extend the view's life or dangle: after
 * teardown the weak load nulls and renderFrame is a no-op.
 */
class IosViewHandle : public ulottie::RtViewHandle {
public:
  explicit IosViewHandle(UlottieRtBaseView *view) : view_(view) {}
  bool renderFrame(double frame) override {
    UlottieRtBaseView *view = view_;
    return view != nil && [view renderFrame:frame];
  }
  bool loadAnimation(const uint8_t *bytes, size_t len) override {
    UlottieRtBaseView *view = view_;
    return view != nil && [view loadAnimation:bytes length:len];
  }

private:
  __weak UlottieRtBaseView *view_;
};

} // namespace

@implementation UlottieRtBaseView {
  CALayer *_surfaceLayer;
  uint64_t _rustId;
  int32_t _nativeId;
  // Double buffer: Rust renders into the back buffer while the CGImage
  // assigned to the layer still wraps the front one, so the memory Core
  // Animation reads is never the memory being written.
  void *_buffers[2];
  size_t _bufferLen;
  int _backBuffer;
  size_t _bufWidth;
  size_t _bufHeight;
  // The frame the surface currently shows; NAN when nothing does (fresh
  // buffers after init/resize/load). The JS loop calls renderFrame every
  // tick, and this is what turns an unchanged tick into an early-out — and
  // what forces a re-render on the first tick after layout or a resize.
  double _lastRenderedFrame;
}

+ (const UlottieRtBackendFns *)backendFns {
  NSAssert(NO, @"UlottieRtBaseView subclass must override +backendFns");
  return nullptr;
}

- (instancetype)initWithFrame:(CGRect)frame {
  if (self = [super initWithFrame:frame]) {
    _rustId = [[self class] backendFns]->instance_create();
    _nativeId = 0;
    _lastRenderedFrame = NAN;
    _surfaceLayer = [CALayer layer];
    _surfaceLayer.magnificationFilter = kCAFilterLinear;
    [self.layer addSublayer:_surfaceLayer];
  }
  return self;
}

- (void)setNativeIdProp:(int32_t)nativeId {
  if (nativeId == _nativeId) {
    return;
  }
  if (_nativeId != 0) {
    ulottie::RtRegistry::instance().remove(_nativeId);
  }
  _nativeId = nativeId;
  if (_nativeId != 0) {
    ulottie::RtRegistry::instance().add(
        _nativeId, std::make_shared<IosViewHandle>(self));
  }
}

- (void)layoutSubviews {
  [super layoutSubviews];
  _surfaceLayer.frame = self.bounds;
}

- (BOOL)loadAnimation:(const uint8_t *)bytes length:(size_t)len {
  if (_rustId == 0 ||
      ![[self class] backendFns]->instance_load(_rustId, bytes, len)) {
    return NO;
  }
  _lastRenderedFrame = NAN; // whatever is on screen is not this scene
  return YES;
}

- (BOOL)renderFrame:(double)frame {
  const UlottieRtBackendFns *fns = [[self class] backendFns];
  [self ensureBuffers];
  if (_buffers[0] == nullptr) {
    return NO; // not laid out yet — the JS loop retries next tick
  }
  if (frame == _lastRenderedFrame) {
    return YES; // already on the surface, and ensureBuffers saw no resize
  }
  int back = _backBuffer;
  fns->instance_set_buffer(_rustId, static_cast<uint8_t *>(_buffers[back]),
                           (uint32_t)_bufWidth, (uint32_t)_bufHeight,
                           (uint32_t)(_bufWidth * 4));
  if (!fns->render_frame(_rustId, (float)frame)) {
    return NO;
  }
  // Wrap the just-rendered buffer without copying: the provider borrows the
  // pointer (no release callback — the view owns the memory and frees it only
  // after detaching layer contents), CGImageCreate is lazy.
  CGDataProviderRef provider =
      CGDataProviderCreateWithData(nullptr, _buffers[back], _bufferLen, nullptr);
  CGColorSpaceRef colorSpace = CGColorSpaceCreateDeviceRGB();
  CGImageRef image = CGImageCreate(
      _bufWidth, _bufHeight, 8, 32, _bufWidth * 4, colorSpace,
      kCGImageAlphaPremultipliedLast | kCGBitmapByteOrder32Big, provider,
      nullptr, false, kCGRenderingIntentDefault);
  CGColorSpaceRelease(colorSpace);
  CGDataProviderRelease(provider);
  if (image == nullptr) {
    return NO;
  }
  _surfaceLayer.contents = (__bridge id)image;
  CGImageRelease(image);
  _backBuffer = 1 - back;
  _lastRenderedFrame = frame;
  return YES;
}

/** (Re)allocates the two pixel buffers to the view's current pixel size. */
- (void)ensureBuffers {
  CGFloat scale = self.traitCollection.displayScale;
  if (scale <= 0) {
    scale = UIScreen.mainScreen.scale;
  }
  size_t w = (size_t)llround(self.bounds.size.width * scale);
  size_t h = (size_t)llround(self.bounds.size.height * scale);
  if (w == 0 || h == 0) {
    return;
  }
  if (_buffers[0] != nullptr && w == _bufWidth && h == _bufHeight) {
    return;
  }
  [self releaseBuffers];
  _bufWidth = w;
  _bufHeight = h;
  _bufferLen = w * h * 4;
  _buffers[0] = calloc(1, _bufferLen);
  _buffers[1] = calloc(1, _bufferLen);
  _backBuffer = 0;
  _lastRenderedFrame = NAN; // fresh (or resized) buffers show nothing yet
  _surfaceLayer.contentsScale = scale;
}

/** Detaches the layer from the buffers, then frees them. Main thread only:
 * the last committed contents were copied to the render server, so freeing
 * after the detach cannot race a read. */
- (void)releaseBuffers {
  _surfaceLayer.contents = nil;
  free(_buffers[0]);
  free(_buffers[1]);
  _buffers[0] = _buffers[1] = nullptr;
  _bufWidth = _bufHeight = _bufferLen = 0;
  _lastRenderedFrame = NAN;
}

- (void)tearDown {
  if (_nativeId != 0) {
    ulottie::RtRegistry::instance().remove(_nativeId);
    _nativeId = 0;
  }
  if (_rustId != 0) {
    [[self class] backendFns]->instance_destroy(_rustId);
    _rustId = 0;
  }
  [self releaseBuffers];
}

- (void)prepareForRecycle {
  [super prepareForRecycle];
  [self tearDown];
  // The recycled view needs a live rasterizer instance again.
  _rustId = [[self class] backendFns]->instance_create();
}

- (void)dealloc {
  [self tearDown];
}

@end
