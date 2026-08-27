import React from 'react';
import { AppRegistry } from 'react-native';
import { createSkiaSkottie } from '../src/baselines';
const anim = require('../assets/boucing_ball.json');
const Player = createSkiaSkottie(anim);
AppRegistry.registerComponent('main', () => () =>
  React.createElement(Player, { style: { width: 160, height: 160 } }),
);
