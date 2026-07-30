/// <reference types="vitest" />
import { spawn } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { cp, mkdir, readdir, rm, stat, utimes } from 'node:fs/promises';
import { mkdtemp } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { gzipSync } from 'node:zlib';
import * as path from 'node:path';

import { playwright } from '@vitest/browser-playwright';
import { compare as odiffCompare } from 'odiff-bin';
import { defineConfig, type Plugin } from 'vitest/config';

import { DEV_PORT, TEST_PORT, startCompileServer } from './global-setup.ts';

// One config for three jobs: `vite` serves the demo, `vite build` produces the
// static deploy, `vitest` runs the suites. They share almost nothing except the
// dev server they talk to, which is why they can live together.

const here = import.meta.dirname;
const workspace = path.dirname(here);
const compilerDir = path.join(workspace, 'ulottie-compiler');
const fixturesDir = path.join(workspace, '_fixtures', 'animations');
const publicDir = path.join(here, 'demo', 'public');
// The wasm glue is an ES module the worker imports, so it has to be in the
// module graph — Vite refuses to transform JS served out of `public/`.
const generatedDir = path.join(here, 'demo', 'src', 'generated');

/**
 * Size of the installed `lottie-web`, for the demo's baseline row.
 *
 * Read here rather than in the page: fetching the file's URL at runtime goes
 * back through Vite's transform and returns the instrumented module, which
 * measured 1.56 MB for a 298 KB file. The Rust backend reads the same path.
 */
function lottieWebSize() {
  const file = path.join(workspace, 'node_modules/lottie-web/build/player/lottie.min.js');
  const bytes = readFileSync(file);
  return { raw: bytes.length, gzipped: gzipSync(bytes, { level: 6 }).length };
}

/// Vite owns the page and proxies compilation to the Rust server — which it
/// also starts, see `./global-setup.ts`. A static build has neither and falls
/// back to the in-browser wasm compiler; see `demo/src/compiler.js`.
const at = (port: number) => `http://127.0.0.1:${port}`;

const ISOLATION = {
  'Cross-Origin-Opener-Policy': 'same-origin',
  'Cross-Origin-Embedder-Policy': 'require-corp',
};

// Vitest evaluates this same file, and `root: 'demo'` would send it looking for
// tests in the demo app. It sets VITEST in the environment before loading the
// config, which is the only signal available this early.
const underTest = !!process.env.VITEST;

const run = (cmd: string, args: string[], cwd: string) =>
  new Promise<void>((resolve, reject) => {
    const child = spawn(cmd, args, { stdio: 'inherit', cwd });
    child.on('exit', (code) =>
      code === 0 ? resolve() : reject(new Error(`${cmd} ${args.join(' ')} exited ${code}`)),
    );
    child.on('error', reject);
  });

const exists = (p: string) => stat(p).then(() => true, () => false);

/** Run the compile server for as long as `vite dev` is up. */
function compileServer(port: number): Plugin {
  let stop: (() => void) | undefined;
  const cleanup = () => {
    stop?.();
    stop = undefined;
  };
  return {
    name: 'ulottie-compile-server',
    apply: 'serve',
    async configureServer(server) {
      stop = await startCompileServer(port);
      // Vite's own close hook does not fire on Ctrl-C, so cover both. `exit`
      // must be synchronous, which `kill` is.
      server.httpServer?.once('close', cleanup);
      process.once('exit', cleanup);
      for (const sig of ['SIGINT', 'SIGTERM'] as const) {
        process.once(sig, () => {
          cleanup();
          process.exit(0);
        });
      }
    },
  };
}

/**
 * Generate the two things the demo needs but Vite cannot make: the wasm build
 * of the compiler, and a copy of the fixtures.
 *
 * Three of them, in fact:
 *
 *   • `demo/src/generated/bindings.ts` — the `/compile` contract, emitted by
 *     `build.rs` from `src/contract.rs`. Running cargo is what produces it.
 *   • `demo/src/generated/wasm/` — the wasm build of the compiler. It sits in
 *     the module graph rather than `public/` because the worker imports it,
 *     and Vite will not transform a module served out of `public/`.
 *   • `demo/public/_fixtures/` — served verbatim and fetched at runtime.
 *
 * wasm-pack takes ~10 s, so `vite dev` only runs it when the output is missing.
 * A build always runs it, because that artifact is what ships.
 */
function demoAssets(command: 'serve' | 'build'): Plugin {
  return {
    name: 'ulottie-demo-assets',
    async buildStart() {
      // `build.rs` writes the contract bindings. Cargo tracks inputs, not
      // outputs, so deleting the generated file does not make it rerun —
      // touching the input does. Cheap when cargo is otherwise warm, and
      // without it a fresh checkout cannot resolve `./generated/bindings.ts`.
      if (!(await exists(path.join(generatedDir, 'bindings.ts')))) {
        const now = new Date();
        await utimes(path.join(here, 'src', 'contract.rs'), now, now);
        await run('cargo', ['build', '--release', '-p', 'ulottie-dev-server', '-q'], workspace);
      }

      const fixturesDst = path.join(publicDir, '_fixtures');
      await mkdir(fixturesDst, { recursive: true });
      for (const entry of await readdir(fixturesDir)) {
        if (entry.endsWith('.json')) {
          await cp(path.join(fixturesDir, entry), path.join(fixturesDst, entry));
        }
      }

      const wasmDst = path.join(generatedDir, 'wasm');
      if (command === 'serve' && (await exists(path.join(wasmDst, 'ulottie_compiler_bg.wasm')))) {
        return;
      }
      if (process.env.ULOTTIE_SKIP_WASM) return;

      // wasm-pack 0.15's `--out-dir` maps onto cargo's unstable
      // `--artifact-dir`, which the stable toolchain rejects — so build into
      // the crate's default pkg/ and move it ourselves.
      const pkgDir = path.join(compilerDir, 'pkg');
      await rm(pkgDir, { recursive: true, force: true });
      await run(
        'wasm-pack',
        ['build', '--release', '--target', 'web', '--no-default-features', '--features', 'wasm,eval'],
        compilerDir,
      );
      if (!(await exists(path.join(pkgDir, 'ulottie_compiler_bg.wasm')))) {
        throw new Error('wasm-pack produced no wasm — check the build log above');
      }
      await rm(wasmDst, { recursive: true, force: true });
      await cp(pkgDir, wasmDst, { recursive: true });
    },
  };
}

// Host-side scratch dir for odiff diff PNGs. One per test session.
let diffDir: string | undefined;
async function ensureDiffDir(): Promise<string> {
  diffDir ??= await mkdtemp(path.join(tmpdir(), 'ulottie-odiff-'));
  return diffDir;
}

declare module 'vitest/browser' {
  interface BrowserCommands {
    /**
     * Compare two PNGs on disk via odiff. Returns the percentage of pixels
     * that differ. Paths come from `page.screenshot()` (host filesystem).
     */
    odiffCompare: (
      refPath: string,
      candPath: string,
      options?: { antialiasing?: boolean; threshold?: number },
    ) => Promise<{ match: boolean; diffPercentage: number; diffPath?: string; reason?: string }>;
    /** Print a block of text on the host, so browser-side reports are visible. */
    report: (text: string) => Promise<void>;
  }
}

export default defineConfig(({ command }) => ({
  root: underTest ? undefined : 'demo',
  define: { __LOTTIE_WEB_SIZE__: JSON.stringify(lottieWebSize()) },
  plugins: underTest ? [] : [demoAssets(command), compileServer(DEV_PORT)],
  build: {
    outDir: '../dist',
    emptyOutDir: true,
    // The wasm bundle is large and already compressed; warning about it on
    // every build is noise.
    chunkSizeWarningLimit: 3000,
  },
  // `#wasm` rather than a relative path: wasm-pack's glue really is `.js`,
  // and an editor that rewrites relative `.js` specifiers to `.ts` (correct
  // for our own sources) would silently break it. An alias has no extension
  // to rewrite.
  resolve: {
    alias: { '#wasm': path.join(generatedDir, 'wasm', 'ulottie_compiler.js') },
  },
  // Cross-origin isolation, for `performance.now()`. Chrome clamps it to 100 µs
  // by default as a Spectre mitigation and relaxes to 5 µs once a document is
  // isolated — a twentyfold improvement in the resolution the benchmark
  // samples against. Everything the page loads is same-origin, so
  // `require-corp` costs nothing here.
  //
  // `_headers` in `demo/public/` says the same thing to the static host.
  server: {
    headers: ISOLATION,
    proxy: {
      '/.output': { target: at(DEV_PORT), changeOrigin: false },
      '/compile': { target: at(DEV_PORT), changeOrigin: false },
    },
  },
  // `preview` inherits `server.proxy` unless told otherwise, which would send a
  // built demo's requests to a dev server that is the whole point of it not
  // needing.
  preview: { headers: ISOLATION, proxy: {} },
  // The worker imports the wasm glue, which wasm-pack emits as an ES module.
  worker: { format: 'es' },

  test: {
    globalSetup: ['./global-setup.ts'],
    projects: [
      {
        // Compiler output snapshots. Runs in node because it shells out to the
        // compiler binary and writes files next to the fixtures.
        test: {
          name: 'snapshot',
          include: ['tests/output.spec.ts', 'tests/coverage.spec.ts'],
          environment: 'node',
        },
      },
      {
        test: {
          name: 'browser',
          include: ['tests/visual.spec.ts', 'tests/perf.spec.ts'],
          browser: {
            enabled: true,
            provider: playwright(),
            instances: [{ browser: 'chromium' }],
            headless: true,
            commands: {
              async report(_ctx: unknown, text: string) {
                process.stdout.write(text + '\n');
              },
              async odiffCompare(
                _ctx: unknown,
                refPath: string,
                candPath: string,
                options: { antialiasing?: boolean; threshold?: number } = {},
              ) {
                const dir = await ensureDiffDir();
                const diffPath = path.join(
                  dir,
                  `diff-${Date.now()}-${Math.random().toString(36).slice(2, 8)}.png`,
                );
                const res = await odiffCompare(refPath, candPath, diffPath, {
                  antialiasing: options.antialiasing ?? true,
                  threshold: options.threshold ?? 0.1,
                });
                if (res.match) return { match: true, diffPercentage: 0 };
                return {
                  match: false,
                  diffPercentage: (res as any).diffPercentage ?? -1,
                  diffPath,
                  reason: (res as any).reason ?? 'mismatch',
                };
              },
            },
          },
        },
        server: {
          // Same isolation as the demo, so `perf.spec.ts` samples against the
          // same 5 µs clock and its numbers are comparable to the panel's.
          headers: ISOLATION,
          // Forward fixture + compile traffic to the spawned dev server. Vite
          // handles everything else (harness sources, node_modules, vitest
          // internals).
          proxy: {
            '/.output': { target: at(TEST_PORT), changeOrigin: false },
            '/compile': { target: at(TEST_PORT), changeOrigin: false },
          },
        },
      },
    ],
  },
}));
