#import <React/RCTViewComponentView.h>
#import <UIKit/UIKit.h>

#import <UlottieRtShared/UlottieRtBackend.h>

/**
 * The backend-agnostic Fabric view owning a rasterizer surface: a CALayer
 * whose contents is a CGImage wrapping (not copying) the pixel buffer Rust
 * just rendered into. Registers itself in the global RtRegistry under its
 * `nativeId` prop; -renderFrame: arrives from `global.UlottieRtApi` on the
 * main thread.
 *
 * A backend package subclasses this with exactly three things: the
 * `+backendFns` table (its C ABI symbol set), its codegen component
 * descriptor, and an `-updateProps:` that forwards `nativeId` to
 * `-setNativeIdProp:` (the props type is per-package codegen output; the
 * mechanism here is shared).
 */
@interface UlottieRtBaseView : RCTViewComponentView

/** The five ffi.rs functions of this view's rasterizer; subclass must
 * override (the base implementation asserts). */
+ (const UlottieRtBackendFns *)backendFns;

/** Re-registers the view in the shared registry under the new id. */
- (void)setNativeIdProp:(int32_t)nativeId;

/** True when `frame` is now what the surface shows (rendered, or an exact
 * early-out: same frame, unchanged surface). False pre-layout / on failure —
 * the JS loop calls every tick, so the first post-layout tick paints. */
- (BOOL)renderFrame:(double)frame;
- (BOOL)loadAnimation:(const uint8_t *)bytes length:(size_t)len;

@end
