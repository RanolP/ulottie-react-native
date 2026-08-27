import React from 'react';
import { AppRegistry } from 'react-native';
import { DotLottie } from '@lottiefiles/dotlottie-react-native';
const anim = require('../assets/boucing_ball.lottie');
AppRegistry.registerComponent('main', () => () =>
  React.createElement(DotLottie, {
    source: anim,
    autoplay: true,
    loop: true,
    style: { width: 160, height: 160 },
  }),
);
