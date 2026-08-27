#import "UlottieRtModule.h"

#import <UlottieRtShared/UlottieRtApi.h>

#import <ReactCommon/RCTTurboModuleWithJSIBindings.h>
#import <UlottieRtSpec/UlottieRtSpec.h>

@interface UlottieRtModule () <NativeUlottieRtModuleSpec,
                               RCTTurboModuleWithJSIBindings>
@end

@implementation UlottieRtModule

RCT_EXPORT_MODULE()

- (NSNumber *)install {
  return @YES;
}

- (void)installJSIBindingsWithRuntime:(facebook::jsi::Runtime &)runtime
                          callInvoker:
                              (const std::shared_ptr<facebook::react::CallInvoker>
                                   &)callInvoker {
  ulottie::installUlottieRtApi(runtime);
}

- (std::shared_ptr<facebook::react::TurboModule>)getTurboModule:
    (const facebook::react::ObjCTurboModule::InitParams &)params {
  return std::make_shared<facebook::react::NativeUlottieRtModuleSpecJSI>(params);
}

@end
