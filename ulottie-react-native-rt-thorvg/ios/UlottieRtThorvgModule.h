#import <Foundation/Foundation.h>

/**
 * TurboModule whose only job is existing: creating it installs the
 * `global.UlottieRtApi` JSI binding (RCTTurboModuleWithJSIBindings), and the
 * spec's `install()` just confirms that from JS. The binding is the shared
 * one from UlottieRtShared — installing it from more than one backend pod is
 * fine, it dispatches by nativeId through the one shared registry.
 */
@interface UlottieRtThorvgModule : NSObject
@end
