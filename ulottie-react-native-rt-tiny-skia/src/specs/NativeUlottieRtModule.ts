import type { TurboModule } from 'react-native';
import { TurboModuleRegistry } from 'react-native';

export interface Spec extends TurboModule {
  /**
   * Forces the TurboModule into existence. The `global.UlottieRtApi` JSI
   * binding installs as a side effect of module creation
   * (`installJSIBindingsWithRuntime:` on iOS); this method only confirms it.
   */
  install(): boolean;
}

export default TurboModuleRegistry.getEnforcing<Spec>('UlottieRtModule');
