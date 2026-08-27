#import "UlottieRtThorvgView.h"

#import "ulottie_rt_tvg.h"

#import <React/RCTComponentViewFactory.h>
#import <react/renderer/components/UlottieRtThorvgSpec/ComponentDescriptors.h>
#import <react/renderer/components/UlottieRtThorvgSpec/Props.h>

using namespace facebook::react;

/** The ThorVG symbol set; everything else lives in UlottieRtBaseView. */
static const UlottieRtBackendFns kBackendFns = {
    ulottie_rt_tvg_instance_create,     ulottie_rt_tvg_instance_destroy,
    ulottie_rt_tvg_instance_load,       ulottie_rt_tvg_instance_set_buffer,
    ulottie_rt_tvg_render_frame,
};

@implementation UlottieRtThorvgView

+ (const UlottieRtBackendFns *)backendFns {
  return &kBackendFns;
}

+ (ComponentDescriptorProvider)componentDescriptorProvider {
  return concreteComponentDescriptorProvider<
      UlottieRtThorvgViewComponentDescriptor>();
}

- (instancetype)initWithFrame:(CGRect)frame {
  if (self = [super initWithFrame:frame]) {
    static const auto defaultProps =
        std::make_shared<const UlottieRtThorvgViewProps>();
    _props = defaultProps;
  }
  return self;
}

- (void)updateProps:(const Props::Shared &)props
           oldProps:(const Props::Shared &)oldProps {
  const auto &newViewProps =
      *std::static_pointer_cast<const UlottieRtThorvgViewProps>(props);
  [self setNativeIdProp:newViewProps.surfaceId];
  [super updateProps:props oldProps:oldProps];
}

@end

Class<RCTComponentViewProtocol> UlottieRtThorvgViewCls(void) {
  return UlottieRtThorvgView.class;
}
