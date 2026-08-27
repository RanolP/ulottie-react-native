import React, { useCallback, useEffect, useRef, useState } from 'react';
import { Pressable, StyleSheet, Text, View } from 'react-native';
import { runOnJS, runOnUI, useFrameCallback, useSharedValue } from 'react-native-reanimated';
import LottieView from 'lottie-react-native';
import { FIXTURE_BY_NAME } from './registry';

const FIXTURE = 'boucing_ball';
const PLAYERS = [
  'ulottie',
  'ulottie-skia',
  'rt-tinyskia',
  'rt-thorvg',
  'lottie',
  'skottie-skia',
  'dotlottie',
  'none',
];
const COUNTS = [1, 4, 9, 16, 'mixed'];
// Heterogeneous cell: the 16 heaviest distinct fixtures by baked-tree node
// count (heaviest first). Fixtures without an svg module render as empty
// Views for the 'ulottie' player, so that row mounts the 12 with svg modules
// (fx_effects, gradient_animated, blend_multiply, matte_luma_inv are
// skia-only in the registry).
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
const WINDOW_MS = 10000;
const GRID = 320;
// Simulator refresh assumed 60 Hz; a delta past 21 ms means at least one
// missed 16.7 ms slot.
const DROP_MS = 21;

/**
 * One probe measures UI-thread health for BOTH player types: it records the
 * deltas between its own useFrameCallback invocations on the UI thread, so a
 * player that stalls that thread shows up regardless of how it renders.
 */
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

export default function PerfScreen() {
  const [player, setPlayer] = useState('ulottie');
  const [count, setCount] = useState(4);
  const [running, setRunning] = useState(false);
  const [results, setResults] = useState(null);
  const startRef = useRef(0);
  const firstFrameRef = useRef(null);

  const start = useCallback(() => {
    // Unmount whatever grid is up first, then mount fresh on the next tick so
    // the measured window starts at a clean mount.
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
    },
    [player, count],
  );

  const mixed = count === 'mixed';
  const gridFixtures = mixed
    ? MIXED16.map((n) => FIXTURE_BY_NAME[n])
    : Array.from({ length: count }, () => FIXTURE_BY_NAME[FIXTURE]);
  const cols = Math.round(Math.sqrt(gridFixtures.length));
  const cell = GRID / cols;
  const cellStyle = { width: cell, height: cell };

  return (
    <View style={styles.root}>
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
            {gridFixtures.map((f, i) => {
              if (player === 'ulottie' && f.Ulottie) {
                const Ulottie = f.Ulottie;
                return <Ulottie key={i} style={cellStyle} />;
              }
              if (player === 'ulottie-skia') {
                const UlottieSkia = f.UlottieSkia;
                return <UlottieSkia key={i} style={cellStyle} />;
              }
              if (player === 'rt-tinyskia') {
                const RtTinySkia = f.RtTinySkia;
                return <RtTinySkia key={i} style={cellStyle} />;
              }
              if (player === 'rt-thorvg') {
                const RtThorvg = f.RtThorvg;
                return <RtThorvg key={i} style={cellStyle} />;
              }
              if (player === 'skottie-skia') {
                const SkiaSkottie = f.SkiaSkottie;
                return <SkiaSkottie key={i} style={cellStyle} />;
              }
              if (player === 'dotlottie') {
                const DotLottie = f.DotLottie;
                return <DotLottie key={i} style={cellStyle} />;
              }
              if (player === 'lottie')
                return (
                  <LottieView
                    key={i}
                    source={f.lottieSource}
                    autoPlay
                    loop
                    style={cellStyle}
                  />
                );
              return <View key={i} style={cellStyle} />;
            })}
          </View>
        </>
      )}
    </View>
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
