import React from 'react';
import { AppRegistry } from 'react-native';
import { createUlottieRt } from 'ulottie-react-native-rt-thorvg';
import * as mod from '../assets/boucing_ball.rt.lottie.json';
const Player = createUlottieRt(mod);
AppRegistry.registerComponent('main', () => () => React.createElement(Player));
