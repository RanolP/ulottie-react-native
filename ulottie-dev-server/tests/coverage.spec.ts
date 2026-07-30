//! Which Lottie constructs the fixture set exercises — as a number, not a
//! judgement call.
//!
//! Eleven fixtures were the whole gate for a long time, and an external corpus
//! of forty-two animations then found nine compiler bugs they could not. Two of
//! those were in code the fixtures ran on every commit; the rest were in code
//! nothing ran at all. This is the second kind, made visible.
//!
//! `census.mjs` already answers "what does this file use", and is the only
//! implementation of that question — the gate imports it rather than growing a
//! second scanner that could drift. `FEATURES` is its closed vocabulary and
//! `_fixtures/coverage.json` is the gap, one line and one reason each.
//!
//! The assertion is equality, in both directions:
//!
//! * a construct with no fixture and no entry **fails** — so adding a check to
//!   the census, or a feature to the compiler, cannot quietly go untested;
//! * an entry that is now covered **fails** — so the list only ever shrinks,
//!   and writing the fixture is what deletes the line.

import { readFile, readdir } from 'node:fs/promises';
import { join } from 'node:path';

import { describe, expect, test } from 'vitest';

import { census, FEATURES, OPEN_ENDED } from '../tools/census.mjs';

const fixtureDir = new URL('../../_fixtures/animations', import.meta.url).pathname;
const coverageFile = new URL('../../_fixtures/coverage.json', import.meta.url).pathname;

/** Every construct the committed fixtures actually contain. */
async function exercised(): Promise<Set<string>> {
  const seen = new Set<string>();
  const files = (await readdir(fixtureDir)).filter((f) => f.endsWith('.json'));
  for (const f of files) {
    for (const key of Object.keys(await census(join(fixtureDir, f)))) seen.add(key);
  }
  return seen;
}

async function declared(): Promise<Record<string, string>> {
  return JSON.parse(await readFile(coverageFile, 'utf8')).uncovered;
}

describe('feature coverage', () => {
  test('the gap is exactly what `_fixtures/coverage.json` says it is', async () => {
    const seen = await exercised();
    const gap = Object.keys(FEATURES).filter((f) => !seen.has(f)).sort();
    const known = Object.keys(await declared()).sort();

    const undocumented = gap.filter((f) => !known.includes(f));
    const stale = known.filter((f) => !gap.includes(f));

    expect(
      undocumented,
      'no fixture covers these, and nothing says why — add one, or add a line to _fixtures/coverage.json',
    ).toEqual([]);
    expect(
      stale,
      'a fixture now covers these — delete their lines from _fixtures/coverage.json',
    ).toEqual([]);
  });

  test('every documented gap says something', async () => {
    const thin = Object.entries(await declared())
      .filter(([, why]) => why.trim().length < 20)
      .map(([f]) => f);
    expect(thin, 'a gap with no reason is a to-do nobody can act on').toEqual([]);
  });

  /**
   * The census emits some keys that name a *value* — a schema version, a frame
   * rate, an After Effects effect id. Those are worth counting and not worth a
   * fixture each. Anything else it emits and `FEATURES` does not name is a
   * construct nobody has decided about, which is the case this whole tool
   * exists to surface.
   */
  test('the fixtures use nothing the vocabulary has not named', async () => {
    const seen = await exercised();
    const unnamed = [...seen]
      .filter((k) => !(k in FEATURES) && !OPEN_ENDED.some((p) => k.startsWith(p)))
      .sort();
    expect(unnamed, 'name it in census.mjs FEATURES, or widen OPEN_ENDED').toEqual([]);
  });

});

// The standing itself — "22 of 59" — is not asserted here. A number that only
// ever moves in one direction is not a gate, and a snapshot of it would say the
// same thing as the deleted line in `coverage.json` that produced it. It is a
// question for a person, so it lives where a person can ask it:
//
//   node ulottie-dev-server/tools/census.mjs --coverage
