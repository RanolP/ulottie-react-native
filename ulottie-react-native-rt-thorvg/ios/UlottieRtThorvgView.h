#import <UlottieRtShared/UlottieRtBaseView.h>

/**
 * The ThorVG rasterizer surface: UlottieRtBaseView (which owns all the
 * buffer/blit/registry mechanics) bound to the `ulottie_rt_tvg_*` C ABI
 * symbol set, plus this package's codegen Fabric descriptor.
 */
@interface UlottieRtThorvgView : UlottieRtBaseView
@end
