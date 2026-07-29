// Compiler output snapshots.
//
// The shipped module is one minified line, which makes a compiler change
// invisible in review. These snapshots hold the same module unminified and
// line-oriented, checked in next to the fixtures, so a diff shows exactly which
// attribute, keyframe or binding moved — and whether a change was meant to move
// them at all.
//
// Update with `yarn test -u` (or `vitest run --project snapshot -u`) and read
// the diff before accepting it.

import { execFileSync } from 'node:child_process';
import { mkdtempSync, readdirSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, test } from 'vitest';

const here = dirname(fileURLToPath(import.meta.url));
const project = resolve(here, '../..');
const fixtureDir = join(project, '_fixtures/animations');
const snapshotDir = join(project, '_fixtures/__snapshots__');
const compiler = join(project, 'target/release/ulottie-compiler');

/**
 * Unsupported features each fixture is allowed to use. Compilation fails on
 * anything else, so this file is the visible record of every known rendering
 * degradation in the corpus.
 */
const ALLOWANCES: Record<string, string[]> = JSON.parse(
  readFileSync(join(project, '_fixtures/allowances.json'), 'utf8'),
);
const allowFor = (name: string) =>
  Array.isArray(ALLOWANCES[name]) ? ['--allow', ALLOWANCES[name].join(',')] : [];

const FIXTURES = readdirSync(fixtureDir)
  .filter((f) => f.endsWith('.json'))
  .map((f) => f.replace(/\.json$/, ''))
  .sort();

const scratch = mkdtempSync(join(tmpdir(), 'ulottie-snapshot-'));

/**
 * Compile one fixture's document template — standalone SVG, no script.
 *
 * Always fully expanded, whatever the module chose to inline or instance, so
 * these snapshots stay stable while the emission strategy is tuned.
 */
function document_(name: string, extra: string[] = []): string {
  const out = join(scratch, `doc-${name}.svg`);
  execFileSync(
    compiler,
    [
      join(fixtureDir, `${name}.json`),
      '--document',
      '--pretty',
      '-o',
      out,
      ...allowFor(name),
      ...extra,
    ],
    { stdio: ['ignore', 'ignore', 'pipe'] },
  );
  return readFileSync(out, 'utf8');
}

/**
 * Compile one fixture unminified, at the compiler's smallest setting.
 *
 * `--instance-precomps` is on here even though it is off by default: these
 * snapshots exist to show what the compiler can emit, and a precomp instanced
 * forty-six times is exactly where the interesting decisions are. Sizes in
 * `sizes` reflect the shipping default instead, so the two will not match.
 */
function compile(name: string, mode: 'extern' | 'embedded'): string {
  const out = join(scratch, `${mode}-${name}.js`);
  execFileSync(
    compiler,
    [
      join(fixtureDir, `${name}.json`),
      '--pretty',
      '-o',
      out,
      ...allowFor(name),
      '--instance-precomps',
      ...(mode === 'embedded' ? ['--embedded'] : []),
    ],
    { stdio: ['ignore', 'ignore', 'pipe'] },
  );
  return readFileSync(out, 'utf8');
}

/**
 * Compile one fixture with its markup extracted, returning both artifacts.
 *
 * The sprite is written per-fixture here; a real build points several
 * animations at one path and the compiler accumulates them.
 */
function extracted(name: string): { js: string; sprite: string } {
  const js = join(scratch, `ex-${name}.js`);
  const sprite = join(scratch, `ex-${name}.svg`);
  execFileSync(
    compiler,
    [
      join(fixtureDir, `${name}.json`),
      '--pretty',
      '-o',
      js,
      '--extract',
      sprite,
      '--symbol-id',
      name,
      ...allowFor(name),
    ],
    { stdio: ['ignore', 'ignore', 'pipe'] },
  );
  return { js: readFileSync(js, 'utf8'), sprite: readFileSync(sprite, 'utf8') };
}

/**
 * The highest element index the module's bindings address.
 *
 * Comparing the sprite against the *document template* is not enough: both come
 * from the same planner call, so they agree with each other even when neither
 * agrees with the module. An instanced module binds against a tree the runtime
 * expands at mount, which has more elements than the document — that mismatch
 * shipped, and only a browser test caught it.
 */
function highestElementIndex(prettyJs: string): number {
  const i = prettyJs.indexOf('const D = ');
  if (i < 0) return -1;
  const rest = prettyJs.slice(i + 10);
  const end = rest.search(/\n(export |const [A-Z]+ =)/);
  const D = JSON.parse(rest.slice(0, end < 0 ? rest.length : end).replace(/;\s*$/, ''));
  let max = -1;
  // The element column is delta-encoded, so it has to be summed to compare.
  let acc = 0;
  for (const b of D.b ?? []) max = Math.max(max, (acc += b[1]));
  for (const u of D.n ?? []) {
    let local = 0;
    for (const b of D.q[u[0]].b ?? []) max = Math.max(max, u[1] + (local += b[1]));
  }
  return max;
}

/** Opening tags, i.e. elements. */
const elementCount = (markup: string) => (markup.match(/<[a-zA-Z]/g) ?? []).length;

describe('compiled output', () => {
  test('every fixture is covered', () => {
    expect(FIXTURES.length).toBeGreaterThan(0);
  });

  // An allowance is a known rendering difference. Listing one for a fixture
  // that no longer needs it would quietly re-open the door, so require every
  // entry to still be load-bearing.
  test('no allowance is stale', () => {
    const stale: string[] = [];
    for (const [name, features] of Object.entries(ALLOWANCES)) {
      if (name.startsWith('_') || !Array.isArray(features)) continue;
      try {
        execFileSync(compiler, [join(fixtureDir, `${name}.json`), '-o', join(scratch, 'x.js')], {
          stdio: ['ignore', 'ignore', 'pipe'],
        });
        stale.push(`${name}: compiles without --allow ${features.join(',')}`);
      } catch {
        // Still needed — good.
      }
    }
    expect(stale, 'remove these from _fixtures/allowances.json').toEqual([]);
  });

  for (const name of FIXTURES) {
    // Extern mode only: it is the compiler's own output for this animation.
    // Embedded mode would bury it under the bundled runtime, which changes for
    // reasons that have nothing to do with the animation.
    test(name, { timeout: 30_000 }, async () => {
      await expect(compile(name, 'extern')).toMatchFileSnapshot(
        join(snapshotDir, `${name}.js`),
      );
    });
  }

  // The document template on its own: every value the compiler could resolve
  // ahead of time, as SVG that renders with no script. Snapshotting it apart
  // from the JS module keeps the two reviewable independently — and this one is
  // unaffected by whether the module chose to inline or factor it.
  describe('document template', () => {
    for (const name of FIXTURES) {
      test(name, { timeout: 30_000 }, async () => {
        await expect(document_(name)).toMatchFileSnapshot(
          join(snapshotDir, `${name}.svg`),
        );
      });
    }

    // The document is what the animation *is*; how the module chooses to carry
    // it is a separate decision. Instancing a precomp must not change it.
    test('does not depend on how the module carries it', () => {
      for (const name of FIXTURES) {
        expect(document_(name, ['--instance-precomps']), name).toEqual(document_(name));
      }
    });
  });

  // Markup extracted to an external sprite: the module keeps only the `<svg>`
  // shell and clones the symbol's children into it at mount.
  describe('extracted markup', () => {
    // One worked example, module and sprite side by side, so the shape of the
    // mode is reviewable. The per-fixture contract is asserted below instead of
    // snapshotted — eleven more copies of the same markup would not be read.
    test('lottie-logo', { timeout: 30_000 }, async () => {
      const { js, sprite } = extracted('lottie-logo');
      await expect(
        `${js}\n/* --- sprite: lottie-logo.sprite.svg --- */\n${sprite}\n`,
      ).toMatchFileSnapshot(join(snapshotDir, 'lottie-logo.extracted.js'));
    });

    // Bindings address elements by document-order index, so the sprite has to
    // hold exactly the elements the module would otherwise have inlined —
    // no wrapper retained, nothing dropped. Off by one and every binding
    // silently drives the wrong node.
    test('the sprite holds every element the module binds', () => {
      const mismatched: string[] = [];
      for (const name of FIXTURES) {
        const { js, sprite } = extracted(name);
        // The symbol wrapper replaces the `<svg>` the shell provides, so the
        // totals match without either being counted twice.
        const inSprite = elementCount(sprite) - 1; // less the sprite's own <svg>
        const inDocument = elementCount(document_(name));
        if (inSprite !== inDocument) {
          mismatched.push(`${name}: sprite ${inSprite} vs document ${inDocument}`);
        }
        // The binding contract: every index the module addresses has to exist
        // in the sprite it will be filled from.
        const needed = highestElementIndex(js);
        if (needed >= inSprite) {
          mismatched.push(
            `${name}: bindings address element ${needed} but the sprite has ${inSprite}`,
          );
        }
        if (elementCount(js.slice(js.indexOf('const M ='), js.indexOf('const D ='))) > 1) {
          mismatched.push(`${name}: module still carries markup`);
        }
      }
      expect(mismatched).toEqual([]);
    });
  });

  // A mask is resolved in the user space its own element establishes, so a
  // `mask` and a `transform` on the same element push the matte through that
  // transform a second time. `lottie-logo`'s matte layer sits under a rotation,
  // and putting the two together silently clipped the wrong part of the logo —
  // invisible to both the pixel diff (hairline antialiasing) and the geometry
  // check (the bounding box does not move).
  test('a mask is never applied to a transformed element', () => {
    const offenders: string[] = [];
    for (const name of FIXTURES) {
      for (const line of document_(name).split('\n')) {
        if (line.includes('mask="url(') && /\stransform="/.test(line)) {
          offenders.push(`${name}: ${line.trim().slice(0, 110)}`);
        }
      }
    }
    expect(offenders, 'put the mask on an untransformed wrapper').toEqual([]);
  });

  // What an embedded build actually carries, after symbol-level tree shaking.
  // This is the file to read when asking "did my change make anything start
  // shipping code it does not reach?" — one line per fixture, sorted.
  test('runtime surface', async () => {
    const rows = FIXTURES.map((name) => {
      const src = compile(name, 'embedded');
      const caps = /^\/\/ caps: (.*)$/m.exec(src)?.[1] ?? 'legacy runtime (expressions)';
      const symbols = /^\/\/ runtime symbols: (.*)$/m.exec(src)?.[1];
      const kept = symbols ? symbols.split(', ') : [];
      const surface = kept.length
        ? `${kept.length} symbols: ${[...kept].sort().join(', ')}`
        : 'none (fully static, or legacy runtime)';
      return `${name}\n  caps:    ${caps}\n  runtime: ${surface}`;
    });
    await expect(rows.join('\n\n') + '\n').toMatchFileSnapshot(
      join(snapshotDir, 'runtime-surface.txt'),
    );
  });
});
