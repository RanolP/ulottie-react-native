import React from 'react';
import { AppRegistry } from 'react-native';
import LottieView from 'lottie-react-native';
const anim = require('../assets/boucing_ball.json');
AppRegistry.registerComponent('main', () => () =>
  React.createElement(LottieView, { source: anim, autoPlay: true, loop: true }),
);
