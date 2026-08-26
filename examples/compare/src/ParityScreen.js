import React, { useCallback, useRef, useState } from 'react';
import { Pressable, ScrollView, StyleSheet, Text, View } from 'react-native';
import LottieView from 'lottie-react-native';
import { FIXTURES, FIXTURE_BY_NAME } from './registry';

const PCTS = [0, 25, 50, 75, 100];
const SIZE = 300;

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

/** Wrapper the automation locates and crops; reports absolute window coords. */
function PlayerBox({ player, children }) {
  const ref = useRef(null);
  const onLayout = useCallback(() => {
    ref.current?.measureInWindow((x, y, width, height) => {
      console.log('PARITY_LAYOUT', JSON.stringify({ player, x, y, width, height }));
    });
  }, [player]);
  return (
    <View ref={ref} testID={`player-${player}`} onLayout={onLayout} style={styles.playerBox}>
      {children}
    </View>
  );
}

export default function ParityScreen() {
  const [fixtureName, setFixtureName] = useState('boucing_ball');
  const [pct, setPct] = useState(0);
  const fixture = FIXTURE_BY_NAME[fixtureName];
  const Ulottie = fixture.Ulottie;
  const UlottieSkia = fixture.UlottieSkia;

  return (
    <View style={styles.root}>
      <ScrollView horizontal style={styles.picker} contentContainerStyle={styles.pickerContent}>
        {FIXTURES.map((f) => (
          <Chip
            key={f.name}
            label={f.name}
            testID={`fixture-${f.name}`}
            selected={f.name === fixtureName}
            onPress={() => setFixtureName(f.name)}
          />
        ))}
      </ScrollView>
      <View style={styles.frameRow}>
        {PCTS.map((p) => (
          <Chip
            key={p}
            label={`${p}`}
            testID={`frame-${p}`}
            selected={p === pct}
            onPress={() => setPct(p)}
          />
        ))}
      </View>
      <Text testID="parity-state" style={styles.state}>
        {fixtureName} @ {pct}%
      </Text>
      <ScrollView>
        <PlayerBox player="ulottie">
          {Ulottie ? (
            <Ulottie key={fixtureName} progress={pct / 100} style={styles.player} />
          ) : (
            <Text style={styles.state}>skia-only fixture — no svg player</Text>
          )}
        </PlayerBox>
        <PlayerBox player="ulottie-skia">
          <UlottieSkia key={fixtureName} progress={pct / 100} style={styles.player} />
        </PlayerBox>
        <PlayerBox player="lottie">
          <LottieView
            key={fixtureName}
            source={fixture.lottieSource}
            progress={pct / 100}
            autoPlay={false}
            loop={false}
            style={styles.player}
          />
        </PlayerBox>
      </ScrollView>
    </View>
  );
}

const styles = StyleSheet.create({
  root: { flex: 1, backgroundColor: '#ffffff' },
  picker: { flexGrow: 0 },
  pickerContent: { padding: 8, gap: 6 },
  frameRow: { flexDirection: 'row', gap: 6, paddingHorizontal: 8 },
  chip: {
    paddingHorizontal: 10,
    paddingVertical: 6,
    borderRadius: 6,
    backgroundColor: '#eeeeee',
  },
  chipSelected: { backgroundColor: '#2255cc' },
  chipText: { fontSize: 12, color: '#222222' },
  chipTextSelected: { color: '#ffffff' },
  state: { paddingHorizontal: 8, paddingVertical: 4, fontSize: 12, color: '#222222' },
  playerBox: {
    width: SIZE,
    height: SIZE,
    backgroundColor: '#ffffff',
    alignSelf: 'center',
  },
  player: { width: SIZE, height: SIZE },
});
