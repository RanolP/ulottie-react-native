#import <Foundation/Foundation.h>

/**
 * TurboModule whose only job is existing: creating it installs the
 * `global.UlottieRtApi` JSI binding (RCTTurboModuleWithJSIBindings), and the
 * spec's `install()` just confirms that from JS.
 */
@interface UlottieRtModule : NSObject
@end
