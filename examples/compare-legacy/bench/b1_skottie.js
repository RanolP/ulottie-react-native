// margelo react-native-skottie + one raw Lottie JSON, on the b0 baseline.
import React from 'react';
import { AppRegistry } from 'react-native';
import { Skottie } from 'react-native-skottie';
const anim = require('../assets/boucing_ball.json');
AppRegistry.registerComponent('CompareLegacy', () => () =>
  React.createElement(Skottie, {
    source: anim,
    autoPlay: true,
    loop: true,
    style: { width: 160, height: 160 },
  }),
);
