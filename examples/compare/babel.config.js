module.exports = function (api) {
  api.cache(true);
  return {
    // babel-preset-expo (SDK 57) auto-includes react-native-worklets/plugin
    // when the package is installed — listing it here again would run the
    // worklet transform twice.
    presets: ['babel-preset-expo'],
  };
};
