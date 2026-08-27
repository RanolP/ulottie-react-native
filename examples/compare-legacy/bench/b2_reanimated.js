// Reanimated-only baseline: attributes how much of b1's delta is the
// reanimated 3 JS layer that react-native-skottie pulls in via its wrapper.
import React from 'react';
import { AppRegistry, View } from 'react-native';
import { useSharedValue } from 'react-native-reanimated';
function Probe() {
  useSharedValue(0);
  return React.createElement(View);
}
AppRegistry.registerComponent('CompareLegacy', () => Probe);
