import React from 'react';
import { AppRegistry } from 'react-native';
import { createUlottie } from 'ulottie-react-native';
import * as mod from '../assets/boucing_ball.lottie.json';
const Player = createUlottie(mod);
AppRegistry.registerComponent('main', () => () => React.createElement(Player));
