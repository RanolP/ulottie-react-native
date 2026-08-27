// Bundle-size baseline for THIS app (RN 0.74.1): a bare View. Not comparable
// byte-for-byte with examples/compare's e0 (different RN version).
import React from 'react';
import { AppRegistry, View } from 'react-native';
AppRegistry.registerComponent('CompareLegacy', () => () => React.createElement(View));
