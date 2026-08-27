import React, { useState } from 'react';
import { Pressable, StyleSheet, Text, View } from 'react-native';
import { createUlottieRt } from 'ulottie-react-native-rt-tiny-skia';
import { createUlottieRt as createUlottieRtThorvg } from 'ulottie-react-native-rt-thorvg';

// The `.rt.lottie.json` name selects the rt target: the module ships an RTDL
// blob (rtdl/meta/init) that the native rasterizers play — the same module
// feeds both the tiny-skia and the ThorVG player below.
import * as boucing_ball_rt from '../assets/boucing_ball.rt.lottie.json';
import * as lottie_logo_1_rt from '../assets/lottie_logo_1.rt.lottie.json';

const BoucingBall = createUlottieRt(boucing_ball_rt);
const LottieLogo = createUlottieRt(lottie_logo_1_rt);
const BoucingBallTvg = createUlottieRtThorvg(boucing_ball_rt);
const LottieLogoTvg = createUlottieRtThorvg(lottie_logo_1_rt);

/**
 * The native-rasterizer target, end to end: RTDL crosses once at mount, then
 * every frame is one `renderFrame(nativeId, frame)` from the worklets UI
 * runtime. Left column tiny-skia, right column ThorVG — both backends linked
 * into this one binary, dispatched through the one shared registry. "Unmount"
 * while the loop keeps running is the teardown-race check — the loop must go
 * no-op, not crash.
 */
export default function RtScreen() {
  const [mounted, setMounted] = useState(true);
  return (
    <View style={styles.root}>
      <Pressable
        testID="rt-toggle-view"
        onPress={() => setMounted((m) => !m)}
        style={styles.button}
      >
        <Text style={styles.buttonText}>{mounted ? 'Unmount' : 'Mount'}</Text>
      </Pressable>
      <View style={styles.row}>
        <View style={styles.cell}>
          <Text style={styles.label}>tiny-skia</Text>
          {mounted ? (
            <BoucingBall style={styles.surface} />
          ) : (
            <View style={[styles.surface, styles.placeholder]} />
          )}
          <LottieLogo style={styles.surface} />
          {/* Pinned + paused: must still render its one frame after mount
              (the pre-layout renderFrame returns false until the surface
              exists, so the JS loop keeps calling until a frame lands). */}
          <BoucingBall style={styles.surface} progress={0.5} autoplay={false} />
        </View>
        <View style={styles.cell}>
          <Text style={styles.label}>thorvg</Text>
          {mounted ? (
            <BoucingBallTvg style={styles.surface} />
          ) : (
            <View style={[styles.surface, styles.placeholder]} />
          )}
          <LottieLogoTvg style={styles.surface} />
          <BoucingBallTvg
            style={styles.surface}
            progress={0.5}
            autoplay={false}
          />
        </View>
      </View>
    </View>
  );
}

const styles = StyleSheet.create({
  root: { flex: 1, alignItems: 'center', paddingTop: 16, gap: 12 },
  button: {
    paddingHorizontal: 14,
    paddingVertical: 8,
    borderRadius: 6,
    backgroundColor: '#ccddff',
  },
  buttonText: { fontSize: 14, color: '#222222' },
  row: { flexDirection: 'row', gap: 12 },
  cell: { alignItems: 'center', gap: 12 },
  label: { fontSize: 12, color: '#666666' },
  surface: { width: 160, height: 160 },
  placeholder: { backgroundColor: '#eeeeee' },
});
