#import "UlottieRtView.h"

#import "ulottie_rt.h"

#import <React/RCTComponentViewFactory.h>
#import <react/renderer/components/UlottieRtSpec/ComponentDescriptors.h>
#import <react/renderer/components/UlottieRtSpec/Props.h>

using namespace facebook::react;

/** The tiny-skia symbol set; everything else lives in UlottieRtBaseView. */
static const UlottieRtBackendFns kBackendFns = {
    ulottie_rt_instance_create,     ulottie_rt_instance_destroy,
    ulottie_rt_instance_load,       ulottie_rt_instance_set_buffer,
    ulottie_rt_render_frame,
};

@implementation UlottieRtView

+ (const UlottieRtBackendFns *)backendFns {
  return &kBackendFns;
}

+ (ComponentDescriptorProvider)componentDescriptorProvider {
  return concreteComponentDescriptorProvider<UlottieRtViewComponentDescriptor>();
}

- (instancetype)initWithFrame:(CGRect)frame {
  if (self = [super initWithFrame:frame]) {
    static const auto defaultProps = std::make_shared<const UlottieRtViewProps>();
    _props = defaultProps;
  }
  return self;
}

- (void)updateProps:(const Props::Shared &)props
           oldProps:(const Props::Shared &)oldProps {
  const auto &newViewProps =
      *std::static_pointer_cast<const UlottieRtViewProps>(props);
  [self setNativeIdProp:newViewProps.surfaceId];
  [super updateProps:props oldProps:oldProps];
}

@end

Class<RCTComponentViewProtocol> UlottieRtViewCls(void) {
  return UlottieRtView.class;
}
