import React from 'react';
import { AppRegistry } from 'react-native';
import { createUlottieSkia } from 'ulottie-react-native/skia';
import * as mod from '../assets/boucing_ball.skia.lottie.json';
const Player = createUlottieSkia(mod);
AppRegistry.registerComponent('main', () => () => React.createElement(Player));
