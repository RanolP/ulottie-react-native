#import <UlottieRtShared/UlottieRtBaseView.h>

/**
 * The tiny-skia rasterizer surface: UlottieRtBaseView (which owns all the
 * buffer/blit/registry mechanics) bound to the `ulottie_rt_*` C ABI symbol
 * set, plus this package's codegen Fabric descriptor.
 */
@interface UlottieRtView : UlottieRtBaseView
@end
