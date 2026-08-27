// Minimal perf harness for margelo's react-native-skottie on its happy-path
// stack (RN 0.74.1 / rn-skia 1.2.3 / reanimated 3.8.1, old architecture).
// The probe is a direct port of examples/compare/src/PerfScreen.js so the
// metrics (firstFrame, steady mean/p50/p95/p99/max, dropped/10 s) are
// byte-for-byte comparable in shape; only two players exist here: margelo
// skottie and none (probe baseline).
import React, { useCallback, useEffect, useRef, useState } from 'react';
import { Pressable, SafeAreaView, StyleSheet, Text, View } from 'react-native';
import { runOnJS, runOnUI, useFrameCallback, useSharedValue } from 'react-native-reanimated';
import { Skottie } from 'react-native-skottie';

const FIXTURE = 'boucing_ball';
const PLAYERS = ['skottie', 'none'];
const COUNTS = [1, 4, 9, 16, 'mixed'];
// Same 16 heaviest distinct fixtures as examples/compare's mixed16 cell.
const MIXED16 = [
  'bodymoovin',
  'lottie_logo_3',
  'fireworks',
  'lottie_logo_2',
  'matte_luma_inv',
  'android_wave',
  'lottie_logo_1',
  'fx_effects',
  'matte_alpha',
  'matte_luma',
  'precomp_star_circle',
  'gradient_radial',
  'stroke_under_fill',
  'gradient_animated',
  'blend_multiply',
  'mask_subtract',
];
const SOURCE_BY_NAME = {
  bodymoovin: require('./assets/bodymoovin.json'),
  lottie_logo_3: require('./assets/lottie_logo_3.json'),
  fireworks: require('./assets/fireworks.json'),
  lottie_logo_2: require('./assets/lottie_logo_2.json'),
  matte_luma_inv: require('./assets/matte_luma_inv.json'),
  android_wave: require('./assets/android_wave.json'),
  lottie_logo_1: require('./assets/lottie_logo_1.json'),
  fx_effects: require('./assets/fx_effects.json'),
  matte_alpha: require('./assets/matte_alpha.json'),
  matte_luma: require('./assets/matte_luma.json'),
  precomp_star_circle: require('./assets/precomp_star_circle.json'),
  gradient_radial: require('./assets/gradient_radial.json'),
  stroke_under_fill: require('./assets/stroke_under_fill.json'),
  gradient_animated: require('./assets/gradient_animated.json'),
  blend_multiply: require('./assets/blend_multiply.json'),
  mask_subtract: require('./assets/mask_subtract.json'),
  boucing_ball: require('./assets/boucing_ball.json'),
};
const WINDOW_MS = 10000;
const GRID = 320;
const DROP_MS = 21;

function FrameProbe({ onFirstFrame, onDone }) {
  const samples = useSharedValue([]);
  const seenFirst = useSharedValue(false);
  useFrameCallback((info) => {
    'worklet';
    if (!seenFirst.value) {
      seenFirst.value = true;
      runOnJS(onFirstFrame)();
    }
    const dt = info.timeSincePreviousFrame;
    if (dt !== null && dt !== undefined) {
      samples.modify((arr) => {
        'worklet';
        arr.push(dt);
        return arr;
      });
    }
  });
  useEffect(() => {
    const t = setTimeout(() => {
      runOnUI(() => {
        'worklet';
        runOnJS(onDone)(samples.value.slice());
      })();
    }, WINDOW_MS);
    return () => clearTimeout(t);
  }, []);
  return null;
}

function percentile(sorted, p) {
  if (sorted.length === 0) return 0;
  const idx = Math.min(sorted.length - 1, Math.ceil((p / 100) * sorted.length) - 1);
  return sorted[Math.max(0, idx)];
}

function Chip({ label, testID, selected, onPress }) {
  return (
    <Pressable
      testID={testID}
      onPress={onPress}
      style={[styles.chip, selected && styles.chipSelected]}
    >
      <Text style={[styles.chipText, selected && styles.chipTextSelected]}>{label}</Text>
    </Pressable>
  );
}

// Auto sweep on launch: 3x skottie/mixed16 then 3x none/mixed16, results as
// PERF_RESULTS lines in the Metro log. Empty queue = manual chips only.
const AUTO_QUEUE = [
  ['skottie', 'mixed'],
  ['skottie', 'mixed'],
  ['skottie', 'mixed'],
  ['none', 'mixed'],
  ['none', 'mixed'],
  ['none', 'mixed'],
];

export default function App() {
  const [player, setPlayer] = useState('skottie');
  const [count, setCount] = useState('mixed');
  const [running, setRunning] = useState(false);
  const [results, setResults] = useState(null);
  const startRef = useRef(0);
  const firstFrameRef = useRef(null);
  const queueRef = useRef([...AUTO_QUEUE]);

  const start = useCallback(() => {
    setRunning(false);
    setResults(null);
    firstFrameRef.current = null;
    setTimeout(() => {
      startRef.current = performance.now();
      setRunning(true);
    }, 50);
  }, []);

  const onFirstFrame = useCallback(() => {
    if (firstFrameRef.current === null) {
      firstFrameRef.current = performance.now() - startRef.current;
    }
  }, []);

  const onDone = useCallback(
    (samples) => {
      setRunning(false);
      const sorted = [...samples].sort((a, b) => a - b);
      const mean = sorted.length ? sorted.reduce((a, b) => a + b, 0) / sorted.length : 0;
      const round = (v) => Math.round(v * 100) / 100;
      const result = {
        player,
        count: count === 'mixed' ? MIXED16.length : count,
        fixture: count === 'mixed' ? 'mixed16' : FIXTURE,
        windowMs: WINDOW_MS,
        firstFrameMs: round(firstFrameRef.current ?? -1),
        frames: sorted.length,
        meanMs: round(mean),
        p50Ms: round(percentile(sorted, 50)),
        p95Ms: round(percentile(sorted, 95)),
        p99Ms: round(percentile(sorted, 99)),
        maxMs: round(sorted.length ? sorted[sorted.length - 1] : 0),
        dropped: sorted.filter((d) => d > DROP_MS).length,
      };
      console.log('PERF_RESULTS', JSON.stringify(result));
      setResults(result);
      setTimeout(stepQueue, 2000);
    },
    [player, count],
  );

  const stepQueue = useCallback(() => {
    const next = queueRef.current.shift();
    if (!next) {
      console.log('PERF_SWEEP_DONE');
      return;
    }
    setPlayer(next[0]);
    setCount(next[1]);
    setTimeout(start, 500);
  }, [start]);

  useEffect(() => {
    const t = setTimeout(stepQueue, 3000);
    return () => clearTimeout(t);
  }, []);

  const mixed = count === 'mixed';
  const gridFixtures = mixed
    ? MIXED16
    : Array.from({ length: count }, () => FIXTURE);
  const cols = Math.round(Math.sqrt(gridFixtures.length));
  const cell = GRID / cols;
  const cellStyle = { width: cell, height: cell };

  return (
    <SafeAreaView style={styles.root}>
      <View style={styles.row}>
        {PLAYERS.map((p) => (
          <Chip
            key={p}
            label={p}
            testID={`perf-player-${p}`}
            selected={p === player}
            onPress={() => setPlayer(p)}
          />
        ))}
      </View>
      <View style={styles.row}>
        {COUNTS.map((n) => (
          <Chip
            key={n}
            label={`${n}`}
            testID={`perf-count-${n}`}
            selected={n === count}
            onPress={() => setCount(n)}
          />
        ))}
      </View>
      <View style={styles.row}>
        <Chip label={running ? 'running…' : 'Start'} testID="perf-start" onPress={start} />
      </View>
      {results && (
        <Text testID="perf-results" style={styles.results}>
          {JSON.stringify(results, null, 2)}
        </Text>
      )}
      {running && (
        <>
          <FrameProbe onFirstFrame={onFirstFrame} onDone={onDone} />
          <View style={styles.grid}>
            {gridFixtures.map((name, i) =>
              player === 'skottie' ? (
                <Skottie
                  key={i}
                  source={SOURCE_BY_NAME[name]}
                  autoPlay
                  loop
                  resizeMode="contain"
                  style={cellStyle}
                />
              ) : (
                <View key={i} style={cellStyle} />
              ),
            )}
          </View>
        </>
      )}
    </SafeAreaView>
  );
}

const styles = StyleSheet.create({
  root: { flex: 1, backgroundColor: '#ffffff' },
  row: { flexDirection: 'row', gap: 6, padding: 8, flexWrap: 'wrap' },
  chip: {
    paddingHorizontal: 12,
    paddingVertical: 8,
    borderRadius: 6,
    backgroundColor: '#eeeeee',
  },
  chipSelected: { backgroundColor: '#2255cc' },
  chipText: { fontSize: 13, color: '#222222' },
  chipTextSelected: { color: '#ffffff' },
  results: { padding: 8, fontSize: 10, color: '#222222', fontVariant: ['tabular-nums'] },
  grid: {
    width: GRID,
    flexDirection: 'row',
    flexWrap: 'wrap',
    alignSelf: 'center',
  },
});
