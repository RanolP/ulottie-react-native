# ulottie-dev-server

Three things that share one config: a compile server, the test suites, and the
comparison demo.

- **`src/main.rs`** — a small Axum server that compiles on demand and serves the
  results. It serves data, not a page.
- **`tests/`** — output snapshots (node), plus pixel-diff, geometry-parity and
  performance suites that drive a real browser.
- **`demo/`** — the comparison page, a Vite app.

`vite.config.ts` is the only build file: `vite` serves the demo, `vite build`
produces the static deploy, `vitest` runs the suites.

## Common commands

```sh
# The demo page. One command — Vite builds and starts the Rust compile server
# alongside itself, and stops it on exit.
yarn workspace ulottie-dev-server dev
# then visit the URL Vite prints (http://localhost:5173 by default)

# Every suite. Spawns its own compile server on 4567.
yarn workspace ulottie-dev-server test
yarn workspace ulottie-dev-server test:watch

# Types. Vite transpiles without checking, so this is the gate — it covers
# the demo, the suites and the config itself.
yarn workspace ulottie-dev-server typecheck

# The static build — no backend at all. Runs wasm-pack, copies the fixtures
# and bundles, all from vite.config.ts.
yarn workspace ulottie-dev-server build:demo   # = vite build
yarn workspace ulottie-dev-server preview

# A compile server on its own, if you want one. Vite and vitest adopt an
# instance that is already listening rather than starting a second.
node ulottie-dev-server/global-setup.ts
```

The page compiles through whichever backend is there: the Rust server when Vite
is proxying one, the in-browser wasm build otherwise. `?compiler=wasm` forces
the wasm path in dev, `?compiler=api` the other way.

## Suites

| Suite | Catches | Notes |
|---|---|---|
| `tests/output.spec.ts` | Any change to emitted bytes | Unminified snapshots next to the fixtures, so a codegen diff is readable |
| `tests/visual.spec.ts` → *visual parity* | Rendering divergence from lottie-web | odiff pixel diff, 0.5% default tolerance |
| `tests/visual.spec.ts` → *geometry parity* | What the pixel diff cannot see | odiff runs with `antialiasing: true` and discounts hairlines; this compares the bounding box of all drawn geometry instead. It caught `ripple` rendering 67% too wide while the pixel diff passed |
| `tests/visual.spec.ts` → other blocks | Embedded, extracted and instanced builds; two-mount id collisions; sprite XML validity | Each is a different assembly of the same scene, and each has shipped a bug the others missed |
| `tests/perf.spec.ts` | Frame time and DOM writes vs lottie-web | Prints the comparison table. Served cross-origin isolated, so it samples the same 5 µs clock the demo does |

Plus `cargo nextest run --features eval` in the workspace root — 87 tests,
including frame snapshots, size budgets and output hygiene.

## How it works

### URL layout

Vite owns the page and proxies these.

```
/healthz            GET  readiness probe (the harness waits on it)
/compile           POST  body = raw Lottie JSON; returns sizes + plan + URLs
/.output/<id>.js    GET  compiled JS (fixture stem or upload hash)
/.output/<id>.slice.js GET  just the runtime modules that build imports
/.output/<id>.pretty.* GET  any artifact unminified, from the compiler
/.output/<id>.json  GET  fixture source or upload source
/.output/driver.js  GET  the whole runtime, minified (the size ceiling)
/.output/runtime/** GET  the runtime as an ES module tree, so extern-mode
                          output resolves its imports
/_fixtures/<n>.json GET  registered fixture source
```

Compilation is **on-demand and disk-cached**: a request for `/.output/<name>.js`
recompiles only if `_fixtures/animations/<name>.json` — or the compiler binary —
is newer than the cache entry. `<name>.embedded.js`, `.extracted.js`,
`.instanced.js` and `.sprite.svg` build the other delivery modes from the same
source.

The server compiles with **every unsupported feature allowed**. It is a viewer:
refusing an upload would show nothing where it could show the degraded render
next to a warning, and the response reports every finding so nothing is silent.
The strict gate lives in the CLI and in `_fixtures/allowances.json`.

### File layout

```
ulottie-dev-server/
├── Cargo.toml                 Rust crate manifest
├── src/main.rs                Axum compile server
├── package.json               Node deps (vite, vitest, lottie-web, tinybench, …)
├── tsconfig.json              Typecheck config (`tsc --noEmit`; Vite never emits)
├── vite.config.ts             Everything: demo app, static build, test suites
├── global-setup.ts            Builds/starts/stops the compile server. Used by
│                                vite dev, by vitest, and runnable directly.
├── tests/
│   ├── output.spec.ts         Compiler output snapshots
│   ├── visual.spec.ts         Pixel-diff + geometry parity + mode coverage
│   └── perf.spec.ts           Frame-time and DOM-write report
├── demo/                      The page (Vite root)
│   ├── index.html
│   ├── src/                   app.ts, compiler{,-api,-wasm}.ts, worker,
│   │                            types.ts (the compile contract, both backends)
│   └── public/                Served verbatim: wasm/, _fixtures/ (generated)
├── dist/                      Static build output (gitignored)
└── .output/                   Disk cache (gitignored)
```

`_fixtures/animations/` (workspace root) is the pre-registered fixture source —
referred to as `__fixtures__` in the server's internals.

### Pixel-diff flow

1. `global-setup.ts` builds the release binaries and starts the compile server
   on 4567, unless one is already listening.
2. Vitest's browser-mode server proxies `/.output/*` and `/compile` to it.
3. `visual.spec.ts` mounts two square panels and, per fixture × `{0, 25, 50,
   75, 99}%` of total frames, renders lottie-web from `/.output/<name>.json`,
   dynamic-imports the compiled module from `/.output/<name>.js`, seeks both,
   and screenshots each panel.
4. The host-side `odiffCompare` command (in `vite.config.ts`) pixel-diffs the
   pair and the test asserts the ratio is under the per-fixture tolerance.

Step 4 is not sufficient on its own — see the geometry-parity row above.
