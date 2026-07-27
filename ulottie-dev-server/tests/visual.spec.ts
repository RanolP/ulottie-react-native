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

/** `page.screenshot` is typed as returning a path, but hands back a
 *  `{ path }` descriptor in some builds. Accept either. */
const shotPath = (shot: unknown): string =>
  typeof shot === 'string' ? shot : (shot as { path: string }).path;
import { commands, page } from 'vitest/browser';
import { lottie } from '../demo/src/lottie.js';

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
  variant: 'extern' | 'embedded' | 'extracted' | 'instanced' = 'extern',
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
  // Extracted mode carries no markup: the elements live in a sprite the page
  // has to have in the document before `init()` runs. Injecting it here is the
  // test's stand-in for a server inlining it into the HTML.
  if (variant === 'extracted') {
    const svg = await fetch(`/.output/${name}.sprite.svg`).then(r => r.text());
    const holder = document.createElement('div');
    holder.innerHTML = svg;
    document.body.appendChild(holder);
  }

  const suffix =
    variant === 'embedded' ? '.embedded.js'
    : variant === 'extracted' ? '.extracted.js'
    : variant === 'instanced' ? '.instanced.js'
    : '.js';
  const ulottieMod = await import(/* @vite-ignore */ `/.output/${name}${suffix}?t=${Date.now()}`);
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

// The extern and embedded builds are different assemblies of the same scene:
// extern imports the runtime as ES modules, embedded inlines a tree-shaken copy.
// Only testing one of them leaves the other free to break — which it did.
describe('embedded build renders', () => {
  beforeAll(async () => {
    await page.viewport(VIEWPORT_W * 2 + 40, VIEWPORT_H + 40);
  });

  for (const fx of FIXTURES) {
    test(`${fx.name}`, { timeout: 30_000 }, async () => {
      const { ref, ulottie } = mountContainers();
      const anim = await loadFixture(fx.name, ref, ulottie, 'embedded');
      try {
        anim.goToFrame(Math.floor(anim.totalFrames * 0.5));
        await new Promise(r => requestAnimationFrame(() => r(undefined)));
        await new Promise(r => requestAnimationFrame(() => r(undefined)));

        const refShot = await page.screenshot({ element: ref, save: true });
        const ulottieShot = await page.screenshot({ element: ulottie, save: true });
        const refPath = shotPath(refShot);
        const ulottiePath = shotPath(ulottieShot);

        const diff = await commands.odiffCompare(refPath, ulottiePath, { antialiasing: true });
        if (!diff.match) {
          const ratio = diff.diffPercentage / 100;
          const tolerance = fx.tolerance ?? DEFAULT_TOLERANCE;
          if (ratio > tolerance) {
            throw new Error(
              `Embedded build of ${fx.name} diverges: ` +
                `${diff.diffPercentage.toFixed(3)}% > ${(tolerance * 100).toFixed(3)}%. ` +
                `diff: ${diff.diffPath}`,
            );
          }
        }
      } finally {
        anim.destroy();
      }
    });
  }
});

// Extracted markup takes a different path to the same DOM: the module clones a
// `<symbol>`'s children into its shell instead of parsing a string. The element
// sequence `querySelectorAll('*')` yields has to come out identical, or every
// binding addresses the wrong node — which a pixel diff catches immediately.
describe('extracted markup renders', () => {
  beforeAll(async () => {
    await page.viewport(VIEWPORT_W * 2 + 40, VIEWPORT_H + 40);
  });

  for (const fx of FIXTURES) {
    test(`${fx.name}`, { timeout: 30_000 }, async () => {
      const { ref, ulottie } = mountContainers();
      const anim = await loadFixture(fx.name, ref, ulottie, 'extracted');
      try {
        anim.goToFrame(Math.floor(anim.totalFrames * 0.5));
        await new Promise(r => requestAnimationFrame(() => r(undefined)));
        await new Promise(r => requestAnimationFrame(() => r(undefined)));

        const refShot = await page.screenshot({ element: ref, save: true });
        const ulottieShot = await page.screenshot({ element: ulottie, save: true });
        const refPath = shotPath(refShot);
        const ulottiePath = shotPath(ulottieShot);

        const diff = await commands.odiffCompare(refPath, ulottiePath, { antialiasing: true });
        if (!diff.match) {
          const ratio = diff.diffPercentage / 100;
          const tolerance = fx.tolerance ?? DEFAULT_TOLERANCE;
          if (ratio > tolerance) {
            throw new Error(
              `Extracted build of ${fx.name} diverges: ` +
                `${diff.diffPercentage.toFixed(3)}% > ${(tolerance * 100).toFixed(3)}%. ` +
                `diff: ${diff.diffPath}`,
            );
          }
        }
      } finally {
        anim.destroy();
      }
    });
  }
});

// Precomp instancing plans an asset once and replays its bindings per use, so
// it takes a completely different path through mount() — one that was never
// visually tested, and had shipped a bug: a fully-instanced animation has no
// document-level bindings at all, and playback was gated on that count, so it
// mounted at frame 0 and froze.
describe('instanced precomps render and animate', () => {
  beforeAll(async () => {
    await page.viewport(VIEWPORT_W * 2 + 40, VIEWPORT_H + 40);
  });

  for (const fx of FIXTURES) {
    test(`${fx.name}`, { timeout: 30_000 }, async () => {
      const { ref, ulottie } = mountContainers();
      const anim = await loadFixture(fx.name, ref, ulottie, 'instanced');
      try {
        anim.goToFrame(Math.floor(anim.totalFrames * 0.5));
        await new Promise(r => requestAnimationFrame(() => r(undefined)));
        await new Promise(r => requestAnimationFrame(() => r(undefined)));

        const refShot = await page.screenshot({ element: ref, save: true });
        const ulottieShot = await page.screenshot({ element: ulottie, save: true });
        const refPath = shotPath(refShot);
        const ulottiePath = shotPath(ulottieShot);

        const diff = await commands.odiffCompare(refPath, ulottiePath, { antialiasing: true });
        if (!diff.match) {
          const ratio = diff.diffPercentage / 100;
          const tolerance = fx.tolerance ?? DEFAULT_TOLERANCE;
          if (ratio > tolerance) {
            throw new Error(
              `Instanced build of ${fx.name} diverges: ` +
                `${diff.diffPercentage.toFixed(3)}% > ${(tolerance * 100).toFixed(3)}%. ` +
                `diff: ${diff.diffPath}`,
            );
          }
        }
      } finally {
        anim.destroy();
      }
    });
  }

  // The frozen-at-frame-0 bug rendered correctly at frame 0 and only showed up
  // in motion, which a single-frame pixel diff cannot see. Assert playback
  // actually starts.
  test('an all-instanced animation autoplays', { timeout: 30_000 }, async () => {
    document.body.innerHTML = '';
    const host = document.createElement('div');
    host.style.cssText = 'width:200px;height:200px';
    document.body.appendChild(host);
    // ripple compiles to zero document-level bindings — all 230 belong to
    // instanced assets — which is exactly the case that used to freeze.
    const mod = await import(/* @vite-ignore */ `/.output/ripple.instanced.js?t=${Date.now()}`);
    const player = mod.init(host, { autoplay: true });
    try {
      expect(player.isPlaying, 'a fully-instanced animation must autoplay').toBe(true);
      const start = player.currentFrame;
      await new Promise(r => setTimeout(r, 250));
      expect(player.currentFrame, 'the frame counter must advance').not.toBe(start);
    } finally {
      player.destroy();
    }
  });
});

// `thisComp.layer()` — by name or by index — resolves within one composition.
// A document that inlines two precomps holds two sets of layers whose names and
// indices both restart, so the runtime keys its lookup tables by composition
// scope. That scope ships as `D.gy`; before it was wired up every inlined
// record was placed at scope 0 and the last one silently won.
//
// After Effects auto-names layers `Shape Layer 1` / `Null 1` per comp and
// numbers them from 1, so this collision is the common case, not an exotic one.
describe('layer lookup is scoped to its composition', () => {
  test('same name and index in two comps stay distinct', async () => {
    const { makeExpr } = await import(
      /* @vite-ignore */ `/.output/runtime/expr.js?t=${Date.now()}`
    );
    // Two comps, each with one layer called 'Ball' at index 1 — the shape a
    // document gets when it inlines two precomps.
    const D = {
      f: 60, i: 0, o: 60,
      s: ['Ball'],
      y: [{ i: 1, n: 0 }, { i: 1, n: 0 }],
      gy: [1, 1], // delta-encoded: scopes 1 and 2
    };
    // The expression engine decorates `ctx` with the lookup tables under test.
    const ctx = { D, svg: null, z: [], frame: 0 } as unknown as {
      proxies: { _g: number }[];
      byName: Map<string, unknown>;
      byIndex: Map<string, unknown>;
    };
    makeExpr([], ctx);

    expect(ctx.proxies.length).toBe(2);
    expect(ctx.proxies[0]).not.toBe(ctx.proxies[1]);
    // Both maps are scope-keyed, so neither record may have evicted the other.
    expect(ctx.byName.size, 'one entry per (scope, name)').toBe(2);
    expect(ctx.byIndex.size, 'one entry per (scope, index)').toBe(2);
    expect(ctx.proxies[0]._g).not.toBe(ctx.proxies[1]._g);
  });
});

// Geometry parity, as a backstop for the pixel diff.
//
// odiff runs with `antialiasing: true`, which discounts pixels it judges to be
// antialiasing artifacts. For an animation drawn entirely in hairlines that is
// almost every pixel it looks at, so `ripple` passed the pixel gate while
// rendering a visibly different picture. Comparing the bounding box of all
// drawn geometry, normalised to the SVG viewport, is immune to that: it does
// not care about colour, thickness or edge softness, only about where the
// drawing actually is.
const GEOMETRY_TOLERANCE = 0.02; // 2% of the viewport

// Fixtures whose geometry does NOT yet match lottie-web. Same contract as
// `_fixtures/allowances.json`: an entry is a known bug kept visible, and the
// list should only ever shrink. Empty is the goal, and currently the truth —
// it earned its keep by catching `ripple` rendering 67% too wide under precomp
// instancing, which the pixel diff could not see.
const GEOMETRY_DIVERGENCE: Record<string, string> = {};

describe('geometry parity vs lottie-web', () => {
  beforeAll(async () => {
    await page.viewport(VIEWPORT_W * 2 + 40, VIEWPORT_H + 40);
  });

  /** Bounding box of every drawn shape, as a fraction of the SVG viewport. */
  function drawnBox(host: HTMLElement) {
    const svg = host.querySelector('svg');
    if (!svg) return null;
    const s = svg.getBoundingClientRect();
    let x0 = Infinity, y0 = Infinity, x1 = -Infinity, y1 = -Infinity, n = 0;
    for (const el of svg.querySelectorAll('path,rect,ellipse,circle,line,polygon')) {
      const b = el.getBoundingClientRect();
      if (!b.width && !b.height) continue;
      n++;
      x0 = Math.min(x0, b.left); y0 = Math.min(y0, b.top);
      x1 = Math.max(x1, b.right); y1 = Math.max(y1, b.bottom);
    }
    if (!n || !s.width || !s.height) return null;
    return {
      x: (x0 - s.left) / s.width,
      y: (y0 - s.top) / s.height,
      w: (x1 - x0) / s.width,
      h: (y1 - y0) / s.height,
    };
  }

  for (const fx of FIXTURES) {
    test(`${fx.name}`, { timeout: 30_000 }, async () => {
      const { ref, ulottie } = mountContainers();
      const anim = await loadFixture(fx.name, ref, ulottie);
      try {
        anim.goToFrame(Math.floor(anim.totalFrames * 0.5));
        await new Promise(r => requestAnimationFrame(() => r(undefined)));
        await new Promise(r => requestAnimationFrame(() => r(undefined)));

        const a = drawnBox(ref);
        const b = drawnBox(ulottie);
        // A fixture that draws nothing at the midpoint has nothing to compare.
        if (!a || !b) return;

        const off = (Object.keys(a) as (keyof typeof a)[])
          .filter(k => Math.abs(a[k] - b[k]) > GEOMETRY_TOLERANCE)
          .map(k => `${k}: ${a[k].toFixed(3)} vs ${b[k].toFixed(3)}`);

        const known = GEOMETRY_DIVERGENCE[fx.name];
        if (known) {
          // Pin it: if this starts matching, delete the entry.
          expect(off.length, `${fx.name} now matches — remove it from GEOMETRY_DIVERGENCE`).toBeGreaterThan(0);
          return;
        }
        expect(off, `${fx.name} geometry diverges — ${off.join(', ')}`).toEqual([]);
      } finally {
        anim.destroy();
      }
    });
  }
});

// A sprite is a real `.svg` file, so it has to be well-formed XML — unlike
// inlined markup, which the page parses with the lenient HTML parser. This is
// not theoretical: the id placeholder used to be U+0001, which `innerHTML`
// accepts and `DOMParser` rejects outright, so the mode would have shipped
// files no browser could load as an image.
describe('the sprite is a valid standalone SVG', () => {
  for (const fx of FIXTURES) {
    test(`${fx.name}`, { timeout: 30_000 }, async () => {
      const text = await fetch(`/.output/${fx.name}.sprite.svg`).then(r => r.text());
      const doc = new DOMParser().parseFromString(text, 'image/svg+xml');
      const err = doc.querySelector('parsererror');
      expect(err?.textContent ?? '').toBe('');
      expect(doc.documentElement.namespaceURI).toBe('http://www.w3.org/2000/svg');
      expect(doc.querySelector(`symbol[id="${fx.name}"]`)).toBeTruthy();
    });
  }
});

// `url(#…)` resolves document-wide, so two mounts of one module must not share
// a gradient or mask id. Inline mode substitutes the marker in the string;
// extracted mode has no string to substitute and rewrites the built DOM
// instead — a separate code path, and the one that would fail silently by
// painting the second mount with the first one's gradient.
describe('two mounts do not share generated ids', () => {
  // `ripple` defines 92 marked ids, `starfish` 2.
  for (const name of ['ripple', 'starfish']) {
    for (const variant of ['extern', 'extracted'] as const) {
      test(`${name} (${variant})`, { timeout: 30_000 }, async () => {
        document.body.innerHTML = '';
        if (variant === 'extracted') {
          const svg = await fetch(`/.output/${name}.sprite.svg`).then(r => r.text());
          const holder = document.createElement('div');
          holder.innerHTML = svg;
          document.body.appendChild(holder);
        }
        const suffix = variant === 'extracted' ? '.extracted.js' : '.js';
        const mod = await import(
          /* @vite-ignore */ `/.output/${name}${suffix}?t=${Date.now()}`
        );

        const ids = (host: HTMLElement) =>
          [...host.querySelectorAll('[id]')].map(e => e.id);
        const mounts = [0, 1].map(() => {
          const host = document.createElement('div');
          document.body.appendChild(host);
          return { host, player: mod.init(host) };
        });
        try {
          const [a, b] = mounts.map(m => ids(m.host));
          expect(a.length, 'the fixture should define ids').toBeGreaterThan(0);
          expect(b.length).toBe(a.length);
          const shared = a.filter(id => b.includes(id));
          expect(shared, `ids reused across mounts: ${shared.slice(0, 5)}`).toEqual([]);

          // An id is only useful if something points at it: check the
          // references moved with it, which is the failure case that renders
          // wrong instead of throwing.
          for (const m of mounts) {
            const own = new Set(ids(m.host));
            const refs = [...m.host.querySelectorAll('*')]
              .flatMap(e => [...e.attributes])
              .flatMap(at => [...at.value.matchAll(/url\(#([^)]+)\)/g)].map(x => x[1]));
            expect(refs.length).toBeGreaterThan(0);
            const dangling = refs.filter(r => !own.has(r));
            expect(dangling, `references outside this mount: ${dangling.slice(0, 5)}`).toEqual([]);
          }
        } finally {
          for (const m of mounts) m.player.destroy?.();
        }
      });
    }
  }
});

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
          const refPath = shotPath(refShot);
          const ulottiePath = shotPath(ulottieShot);

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
