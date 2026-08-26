'use strict';
// Metro transform worker that compiles `*.lottie.json` imports at bundle time.
//
// The interception lives at `transformerPath` (the whole worker), not at
// `babelTransformerPath`: metro-transform-worker classifies files by extension
// before the babel transformer is consulted, and `.json` files short-circuit
// into JSON wrapping without ever reaching babel. Compiling here and renaming
// the file to `.js` routes the generated module through the normal source
// pipeline — babel and the reanimated worklet plugin — of whatever worker the
// config already used (Expo's, by default).

const crypto = require('crypto');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { isLottieFile, compileLottie, cacheKeyFragment } = require('./compile');

// The filename handed to the inner worker must be a REAL file holding exactly
// the generated source: react-native-worklets' babel plugin rebuilds each
// worklet's source map by fs.readFileSync-ing every entry of the input map's
// `sources` (which babel seeds with the filename), and crashes with ENOENT on
// a virtual path. Content-addressed, so concurrent workers write identical
// bytes and cached files are reused across rebuilds.
function materialize(js) {
  const dir = path.join(os.tmpdir(), 'ulottie-virtual');
  fs.mkdirSync(dir, { recursive: true });
  const name = crypto.createHash('sha256').update(js).digest('hex').slice(0, 16);
  const file = path.join(dir, `${name}.lottie.js`);
  if (!fs.existsSync(file)) {
    const tmp = `${file}.${process.pid}.tmp`;
    fs.writeFileSync(tmp, js);
    fs.renameSync(tmp, file);
  }
  return file;
}

function innerWorker(config) {
  const p = config.ulottie && config.ulottie.transformerPath;
  if (!p) {
    throw new Error(
      'ulottie: `transformer.ulottie` missing from the Metro config — ' +
        "wrap it with `withUlottie()` from 'ulottie-react-native/metro/withUlottie'.",
    );
  }
  return require(p);
}

async function transform(config, projectRoot, filename, data, options) {
  const inner = innerWorker(config);
  if (!isLottieFile(filename)) {
    return inner.transform(config, projectRoot, filename, data, options);
  }
  const js = compileLottie(data, filename, config.ulottie);
  return inner.transform(config, projectRoot, materialize(js), Buffer.from(js, 'utf8'), options);
}

function getCacheKey(config) {
  const inner = innerWorker(config);
  const upstream = typeof inner.getCacheKey === 'function' ? inner.getCacheKey(config) : '';
  return upstream + '$' + cacheKeyFragment(config.ulottie);
}

module.exports = { transform, getCacheKey };
