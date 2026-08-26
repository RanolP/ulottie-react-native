const { getDefaultConfig } = require('expo/metro-config');
const withUlottie = require('ulottie-react-native/metro/withUlottie');

const config = getDefaultConfig(__dirname);

// The ulottie-react-native workspace carries its own nested node_modules
// (react-native 0.83 + reanimated/svg copies, devDependencies for its
// typecheck). Metro's hierarchical lookup from ulottie-react-native/src would
// bundle those duplicates next to the app's react-native 0.86. Expo's default
// nodeModulesPaths already lists [app, repo root], so turning hierarchical
// lookup off routes every import through the correct copies.
config.resolver.disableHierarchicalLookup = true;

// lottie_logo_1 carries an inverted alpha track matte (tt: 2); rn-svg has no
// working filter primitives, so its `.lottie.json` compile only passes as an
// explicit degradation — it renders without the inversion. The `.skia.…` twin
// needs no allowance: the skia-aot target lowers the inversion exactly.
module.exports = withUlottie(config, { allow: ['track-matte-inverted'] });
