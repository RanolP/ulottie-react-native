'use strict';
const path = require('path');

/**
 * Wrap a Metro config (Expo's `getDefaultConfig(__dirname)` or any other) so
 * `import anim from './foo.lottie.json'` compiles through the ulottie AOT
 * compiler at bundle time. The previous `transformerPath` keeps handling
 * everything that is not a `*.lottie.json` file.
 *
 * `.json` is already in Metro's default `resolver.sourceExts`, so no resolver
 * change is needed for the import to resolve.
 *
 * @param {object} config the Metro config to wrap
 * @param {{ allow?: string[], target?: string }} [opts] `allow` accepts the
 *   compiler's named degradations (passed as `--allow <name>` per entry); the
 *   compiler refuses a file needing one it was not given, and Metro surfaces
 *   that refusal. `target` picks the compiler backend for `*.lottie.json`
 *   files ('reanimated-aot', the default, or 'skia-aot'); a
 *   `*.skia.lottie.json` file compiles as 'skia-aot' regardless, so one
 *   bundle can mix targets.
 */
function withUlottie(config, opts) {
  const inner =
    config.transformerPath ||
    require.resolve('metro-transform-worker', {
      paths: [config.projectRoot || process.cwd()],
    });
  return {
    ...config,
    transformerPath: path.join(__dirname, 'transform-worker.js'),
    transformer: {
      ...config.transformer,
      // Metro serializes `transformer` and hands it to the worker processes;
      // that is the only channel through which the worker can learn which
      // transformer it wraps and which degradations are allowed.
      ulottie: {
        transformerPath: inner,
        allow: (opts && opts.allow) || [],
        target: (opts && opts.target) || 'reanimated-aot',
      },
    },
  };
}

module.exports = withUlottie;
module.exports.withUlottie = withUlottie;
