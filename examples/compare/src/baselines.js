// Comparison-baseline players that are not μLottie targets.
//
// SkiaSkottie: @shopify/react-native-skia's Skottie module (Skia's C++ Lottie
// player, JSON parsed at runtime). The <Skottie> element renders at the
// animation's own pixel size, so a Group scale maps it into the cell.
// DotLottie: LottieFiles' official player (dotlottie-rs / ThorVG native core).
// Its RN API takes a file source only (require()'d asset or URL), so each
// fixture ships as a `.lottie` zip wrapping the same raw Lottie JSON.
import React, { useMemo } from 'react';
import { Canvas, Group, Skia, Skottie, useClock } from '@shopify/react-native-skia';
import { useDerivedValue } from 'react-native-reanimated';
import { DotLottie } from '@lottiefiles/dotlottie-react-native';

export function createDotLottie(asset) {
  return function DotLottiePlayer({ style }) {
    return <DotLottie source={asset} style={style} autoplay loop />;
  };
}

export function createSkiaSkottie(sourceJson) {
  const json = JSON.stringify(sourceJson);
  return function SkiaSkottiePlayer({ style }) {
    const animation = useMemo(() => Skia.Skottie.Make(json), []);
    const clock = useClock();
    const frame = useDerivedValue(() => {
      const durationMs = animation.duration() * 1000;
      return ((clock.value % durationMs) / 1000) * animation.fps();
    });
    const scale = (style?.width ?? 160) / animation.size().width;
    return (
      <Canvas style={style}>
        <Group transform={[{ scale }]}>
          <Skottie animation={animation} frame={frame} />
        </Group>
      </Canvas>
    );
  };
}
