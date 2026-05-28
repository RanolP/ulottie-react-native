// Browser pixel-diff tests. For each fixture, render lottie-web (reference)
// and ulottie (compiled) at sample frames, screenshot both, and compare with
// odiff via the host-side `odiffCompare` command.
//
// This is the visual ground-truth regression gate. The Rust-side
// `tests/frame_snapshot.rs` is the fast unit-level gate; this catches
// genuine rendering divergence that text snapshots miss.
//
// Prereq: harness/<fixture>.json and harness/<fixture>.js must exist. Run
// `node build.mjs` (or rely on the snapshot.mjs pipeline) first.

import { afterEach, beforeAll, describe, expect, test } from 'vitest';
import { commands, page } from 'vitest/browser';
import lottie from 'lottie-web';

// Fixture × sample-frame grid. Sample at canonical anchors so a regression
// in mid-animation is caught even if the endpoints happen to coincide.
const FIXTURES: ReadonlyArray<{ name: string; tolerance?: number }> = [
  { name: 'rectangle' },
  { name: 'ellipse' },
  { name: 'fill' },
  { name: 'trim_path' },
  { name: 'bouncy_ball' },
  { name: 'boucing-ball' },
  { name: 'lottie-logo' },
  { name: 'starfish' },
  { name: 'ripple' },
  { name: 'precomp_star_circle' },
  { name: 'lights' },
];

const SAMPLES = [0, 0.25, 0.5, 0.75, 0.99] as const;

// Acceptable pixel-diff ratio. Differences below this pass; above this fail.
// Default starts wide; per-fixture overrides tune as we go.
const DEFAULT_TOLERANCE = 0.005; // 0.5%

const VIEWPORT_W = 320;
const VIEWPORT_H = 320;

function mountContainers(): { ref: HTMLDivElement; ulottie: HTMLDivElement } {
  document.body.innerHTML = '';
  // Side-by-side panels — both square, identical CSS, so renderers can't
  // accidentally be different sizes.
  const wrap = document.createElement('div');
  wrap.style.cssText = `display:flex;gap:0;align-items:flex-start;background:white;padding:0;margin:0;`;
  const ref = document.createElement('div');
  ref.id = 'ref-panel';
  const ulottie = document.createElement('div');
  ulottie.id = 'ulottie-panel';
  for (const el of [ref, ulottie]) {
    el.style.cssText = `width:${VIEWPORT_W}px;height:${VIEWPORT_H}px;overflow:hidden;background:white;`;
  }
  wrap.appendChild(ref);
  wrap.appendChild(ulottie);
  document.body.appendChild(wrap);
  return { ref, ulottie };
}

async function loadFixture(
  name: string,
  refEl: HTMLElement,
  ulottieEl: HTMLElement,
): Promise<{
  totalFrames: number;
  goToFrame: (f: number) => void;
  destroy: () => void;
}> {
  // Reference panel: lottie-web fetches the source via `path`. The dev
  // server serves registered fixtures at `/.output/<name>.json` (with a
  // fallback to `_fixtures/animations/`).
  const refAnim = lottie.loadAnimation({
    container: refEl,
    renderer: 'svg',
    loop: false,
    autoplay: false,
    path: `/.output/${name}.json`,
    rendererSettings: {
      preserveAspectRatio: 'xMidYMid meet',
    },
  });
  await new Promise<void>((resolve, reject) => {
    refAnim.addEventListener('DOMLoaded', () => resolve());
    refAnim.addEventListener('data_failed', () =>
      reject(new Error('lottie-web failed to load ' + name)),
    );
  });

  // ulottie panel: dynamic import the compiled module. The dev server
  // lazy-compiles `_fixtures/animations/<name>.json` into `.output/<name>.js`
  // before ServeDir hands it over.
  const ulottieMod = await import(/* @vite-ignore */ `/.output/${name}.js?t=${Date.now()}`);
  const ulottieResult = ulottieMod.init(ulottieEl);
  const totalFrames = Math.round(refAnim.totalFrames || ulottieResult.totalFrames || 0);
  // Both rendered immediately at frame 0; pause both.
  refAnim.goToAndStop(0, true);
  if (ulottieResult.goToFrame) ulottieResult.goToFrame(0);
  return {
    totalFrames,
    goToFrame: (f: number) => {
      refAnim.goToAndStop(f, true);
      if (ulottieResult.goToFrame) ulottieResult.goToFrame(f);
    },
    destroy: () => {
      refAnim.destroy();
      if (ulottieResult.destroy) ulottieResult.destroy();
    },
  };
}

describe('visual parity vs lottie-web', () => {
  beforeAll(async () => {
    await page.viewport(VIEWPORT_W * 2 + 40, VIEWPORT_H + 40);
  });

  for (const fx of FIXTURES) {
    describe(fx.name, () => {
      let anim: Awaited<ReturnType<typeof loadFixture>> | undefined;

      afterEach(() => {
        if (anim) {
          anim.destroy();
          anim = undefined;
        }
      });

      for (const t of SAMPLES) {
        test(`frame at t=${t}`, { timeout: 30_000 }, async () => {
          const { ref, ulottie } = mountContainers();
          anim = await loadFixture(fx.name, ref, ulottie);
          const frame = Math.floor(anim.totalFrames * t);
          anim.goToFrame(frame);

          // Let the browser render the new frame before screenshotting.
          await new Promise(r => requestAnimationFrame(() => r(undefined)));
          await new Promise(r => requestAnimationFrame(() => r(undefined)));

          const refShot = await page.screenshot({ element: ref, save: true });
          const ulottieShot = await page.screenshot({ element: ulottie, save: true });
          const refPath = typeof refShot === 'string' ? refShot : refShot.path;
          const ulottiePath = typeof ulottieShot === 'string' ? ulottieShot : ulottieShot.path;

          const diff = await commands.odiffCompare(refPath, ulottiePath, {
            antialiasing: true,
          });
          if (!diff.match) {
            const ratio = diff.diffPercentage / 100;
            const tolerance = fx.tolerance ?? DEFAULT_TOLERANCE;
            expect.soft(ratio).toBeLessThanOrEqual(tolerance);
            // If the soft check passed (within tolerance) but odiff still
            // reported "no match", that means absolute mismatch was within
            // tolerance. Anything past tolerance is a hard fail.
            if (ratio > tolerance) {
              throw new Error(
                `Visual mismatch on ${fx.name} frame=${frame} (t=${t}): ` +
                  `${diff.diffPercentage.toFixed(3)}% > ${(tolerance * 100).toFixed(3)}%. ` +
                  `diff: ${diff.diffPath}`,
              );
            }
          }
        });
      }
    });
  }
});
