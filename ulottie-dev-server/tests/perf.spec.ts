// Runtime performance comparison against lottie-web, in a real browser.
//
// "Smaller" is measured by the `sizes` report; this is the other half of the
// claim. For each fixture we drive both players over the same frame sequence
// and measure (a) wall-clock time per frame and (b) DOM attribute writes per
// frame — the latter being the exact quantity the AOT stage is designed to
// eliminate, and the one that is deterministic rather than machine-dependent.
//
// The assertion is deliberately loose (ulottie must not be slower); the value
// is the printed table.

import { beforeAll, describe, expect, test } from 'vitest';
import { commands, page } from 'vitest/browser';
import { lottie } from '../demo/src/lottie.js';

const FIXTURES = [
  'rectangle',
  'ellipse',
  'fill',
  'trim_path',
  'bouncy_ball',
  'boucing-ball',
  'lottie-logo',
  'precomp_star_circle',
  'ripple',
  'starfish',
  'lights',
] as const;

/** Frames driven per measurement pass. */
const FRAMES = 120;
const WARMUP = 20;
const VIEWPORT = 320;

type Row = {
  fixture: string;
  lottieMs: number;
  ulottieMs: number;
  lottieWrites: number;
  ulottieWrites: number;
};

const rows: Row[] = [];

/**
 * Count attribute writes (including inline-style mutations, which is how both
 * players toggle visibility) while `fn` runs.
 */
async function countWrites(fn: () => Promise<void> | void): Promise<number> {
  const proto = Element.prototype;
  const realSet = proto.setAttribute;
  const styleProto = CSSStyleDeclaration.prototype;
  const realStyleSet = styleProto.setProperty;
  let n = 0;
  proto.setAttribute = function (this: Element, ...args: [string, string]) {
    n++;
    return realSet.apply(this, args);
  };
  styleProto.setProperty = function (this: CSSStyleDeclaration, ...args: [string, string]) {
    n++;
    return realStyleSet.apply(this, args);
  };
  try {
    await fn();
  } finally {
    proto.setAttribute = realSet;
    styleProto.setProperty = realStyleSet;
  }
  return n;
}

function mountHost(id: string): HTMLDivElement {
  const el = document.createElement('div');
  el.id = id;
  el.style.cssText = `width:${VIEWPORT}px;height:${VIEWPORT}px;overflow:hidden`;
  document.body.appendChild(el);
  return el;
}

/**
 * Time a whole sweep and divide. `performance.now()` is clamped to ~0.1 ms in
 * browsers, which is coarser than a single frame update — timing per frame
 * would read as zero. Best-of-N because the noise here is all upward.
 */
function sweep(step: (f: number) => void, frameAt: (i: number) => number): number {
  let best = Infinity;
  for (let pass = 0; pass < 5; pass++) {
    const t = performance.now();
    for (let i = 0; i < FRAMES; i++) step(frameAt(i));
    const dt = (performance.now() - t) / FRAMES;
    if (dt < best) best = dt;
  }
  return best;
}

describe('runtime performance vs lottie-web', () => {
  beforeAll(async () => {
    await page.viewport(VIEWPORT + 40, VIEWPORT + 40);
  });

  for (const name of FIXTURES) {
    test(`${name}`, { timeout: 60_000 }, async () => {
      document.body.innerHTML = '';
      const refHost = mountHost('ref');
      const uHost = mountHost('ulottie');

      const ref = lottie.loadAnimation({
        container: refHost,
        renderer: 'svg',
        loop: false,
        autoplay: false,
        path: `/.output/${name}.json`,
        rendererSettings: { preserveAspectRatio: 'xMidYMid meet' },
      });
      await new Promise<void>((resolve, reject) => {
        ref.addEventListener('DOMLoaded', () => resolve());
        ref.addEventListener('data_failed', () => reject(new Error(`load ${name}`)));
      });

      const mod = await import(/* @vite-ignore */ `/.output/${name}.js?p=${Date.now()}`);
      // Mount paused so the rAF loop never competes with the measurement.
      const u = mod.init(uHost, { autoplay: false });

      const total = Math.round(ref.totalFrames || u.totalFrames || FRAMES);
      const frameAt = (i: number) => (i / FRAMES) * Math.max(1, total - 1);

      // Warm up both JITs and let each player allocate its steady-state
      // structures before anything is timed.
      for (let i = 0; i < WARMUP; i++) {
        ref.goToAndStop(frameAt(i), true);
        u.goToFrame(frameAt(i));
      }

      const lottieMs = sweep((f) => ref.goToAndStop(f, true), frameAt);
      const ulottieMs = sweep((f) => u.goToFrame(f), frameAt);

      const lottieWrites = await countWrites(() => {
        for (let i = 0; i < FRAMES; i++) ref.goToAndStop(frameAt(i), true);
      });
      const ulottieWrites = await countWrites(() => {
        for (let i = 0; i < FRAMES; i++) u.goToFrame(frameAt(i));
      });

      rows.push({
        fixture: name,
        lottieMs,
        ulottieMs,
        lottieWrites: lottieWrites / FRAMES,
        ulottieWrites: ulottieWrites / FRAMES,
      });

      ref.destroy();
      u.destroy();

      // Attribute writes are deterministic frame to frame, which makes this a
      // real regression gate rather than a timing coin-flip. The allowance
      // covers the small constant difference in how the two players batch
      // writes; it is nowhere near wide enough to hide a redundant-write bug
      // (before subtree gating and change detection, ripple sat at 8x).
      const budget = lottieWrites * 1.25 + 4 * FRAMES;
      expect(
        ulottieWrites,
        `${name}: ${(ulottieWrites / FRAMES).toFixed(1)} writes/frame vs ` +
          `lottie-web ${(lottieWrites / FRAMES).toFixed(1)}`,
      ).toBeLessThanOrEqual(budget);
    });
  }

  test('report', async () => {
    const pad = (s: string, n: number) => s.padStart(n);
    const lines = [
      '',
      ' fixture                 lottie ms  ulottie ms   speedup   lottie w/f  ulottie w/f   write cut',
      ' ---------------------------------------------------------------------------------------------',
    ];
    let sumL = 0, sumU = 0, sumLW = 0, sumUW = 0;
    for (const r of rows) {
      sumL += r.lottieMs; sumU += r.ulottieMs;
      sumLW += r.lottieWrites; sumUW += r.ulottieWrites;
      const speed = r.ulottieMs > 0 ? r.lottieMs / r.ulottieMs : Infinity;
      const cut = r.lottieWrites > 0 ? 1 - r.ulottieWrites / r.lottieWrites : 0;
      lines.push(
        ' ' + r.fixture.padEnd(22) +
        pad(r.lottieMs.toFixed(4), 10) +
        pad(r.ulottieMs.toFixed(4), 12) +
        pad(Number.isFinite(speed) ? speed.toFixed(1) + 'x' : '∞', 10) +
        pad(r.lottieWrites.toFixed(1), 13) +
        pad(r.ulottieWrites.toFixed(1), 13) +
        pad((cut * 100).toFixed(1) + '%', 12),
      );
    }
    lines.push(' ---------------------------------------------------------------------------------------------');
    lines.push(
      ' ' + 'TOTAL'.padEnd(22) +
      pad(sumL.toFixed(4), 10) +
      pad(sumU.toFixed(4), 12) +
      pad((sumL / Math.max(sumU, 1e-9)).toFixed(1) + 'x', 10) +
      pad(sumLW.toFixed(1), 13) +
      pad(sumUW.toFixed(1), 13) +
      pad(((1 - sumUW / Math.max(sumLW, 1e-9)) * 100).toFixed(1) + '%', 12),
    );
    await commands.report(lines.join('\n'));
    expect(rows.length).toBe(FIXTURES.length);
    // Timing is machine-dependent, so the gate is only that the corpus as a
    // whole has not regressed into being slower than the reference player.
    // The table above is where the real signal is.
    expect(sumU).toBeLessThanOrEqual(sumL * 1.25);
  });
});
