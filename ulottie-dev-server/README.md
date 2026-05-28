# ulottie-dev-server

The visual comparison harness — a small Axum dev server plus a Vitest
browser-mode pixel-diff suite.

## Tiers

| Tier | Location | What it catches | How fast |
|---|---|---|---|
| Rust frame snapshots | `ulottie-compiler/tests/frame_snapshot.rs` | Property eval, transforms, keyframes, gradients, masks, precomps | <1 s (`cargo nx`) |
| Browser pixel-diff | `tests/visual.spec.ts` | Genuine rendering divergence between ulottie and lottie-web | ~6 s (`yarn workspace ulottie-dev-server test`) |

Both must stay green to ship a codegen change.

## Common commands

```sh
# From this directory:

# Browser pixel-diff suite (live ulottie vs lottie-web, odiff comparator):
yarn workspace ulottie-dev-server test

# Watch mode:
yarn workspace ulottie-dev-server test:watch

# Static server only — open compare-all.html in your browser to scrub
# frames side-by-side with lottie-web. Run from repo root or here:
cargo run -p ulottie-dev-server
# then visit http://127.0.0.1:4567/compare-all.html
```

## How it works

### URL layout

```
/                       redirect → /compare-all.html
/compile          POST  body = raw Lottie JSON; returns flat URLs
/.output/<id>.js   GET  compiled JS (fixture stem or upload hash)
/.output/<id>.json GET  fixture source or upload source
/.output/driver.js GET  mirrored shared runtime
/<anything-else>   GET  static UI under public/
```

Compilation is **on-demand and disk-cached**. When a request for
`/.output/<name>.js` arrives, a middleware checks whether
`_fixtures/animations/<name>.json` is newer than `.output/<name>.js` and
recompiles if so. Misses just write the cache file and let the static
service serve it.

### File layout

```
ulottie-dev-server/
├── Cargo.toml                 Rust crate manifest
├── src/main.rs                Axum dev server
├── package.json               Node deps (vitest, lottie-web, odiff-bin, …)
├── vitest.config.ts           Browser-mode config + odiff command + proxy
├── tests/
│   ├── global-setup.ts        Spawns the dev server before tests
│   └── visual.spec.ts         Pixel-diff suite
├── public/                    Static UI (compare-all.html)
└── .output/                   Disk cache (gitignored): compiled JS,
                                upload sources, mirrored runtime
```

`_fixtures/animations/` (workspace root) is the pre-registered fixture
source location — referred to as `__fixtures__` in the server's internals.

### Vitest pixel-diff flow

1. `tests/global-setup.ts` spawns `ulottie-dev-server` if it isn't already
   running on port 4567.
2. The Vite test server proxies `/.output/*` and `/compile` to the spawned
   server (`vitest.config.ts` → `server.proxy`).
3. `tests/visual.spec.ts` mounts two square panels and, for each fixture ×
   `{0%, 25%, 50%, 75%, 99%}` of total frames:
   - Renders the fixture via lottie-web in the reference panel
     (`path: '/.output/<name>.json'`).
   - Dynamic-imports the compiled ulottie module
     (`import '/.output/<name>.js'`).
   - `goToFrame` on both, then `page.screenshot({ element })` on each.
4. The host-side `odiffCompare` command (in `vitest.config.ts`) runs
   `odiff-bin` to pixel-diff the PNG pair. The test asserts the diff ratio
   is below the per-fixture tolerance (default 0.5%).
