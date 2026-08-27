#import "UlottieRtThorvgModule.h"

#import <UlottieRtShared/UlottieRtApi.h>

#import <ReactCommon/RCTTurboModuleWithJSIBindings.h>
#import <UlottieRtThorvgSpec/UlottieRtThorvgSpec.h>

@interface UlottieRtThorvgModule () <NativeUlottieRtThorvgModuleSpec,
                                     RCTTurboModuleWithJSIBindings>
@end

@implementation UlottieRtThorvgModule

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
  return std::make_shared<facebook::react::NativeUlottieRtThorvgModuleSpecJSI>(
      params);
}

@end
