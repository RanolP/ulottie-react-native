// The Rust compile server: build it, start it, wait for it, stop it.
//
// It owns `POST /compile` and `/.output/**` — compiling on demand and caching
// under `.output/` — while Vite owns the page and proxies to it. Three callers
// share this one implementation:
//
//   • `vite.config.ts`, which runs one alongside `vite dev`
//   • vitest, via the `globalSetup` default export
//   • `node global-setup.ts`, to run one on its own
//
// An instance already listening is adopted rather than replaced, so a server
// you started yourself keeps being used.

import { execFileSync, spawn } from 'node:child_process';
import { cp, rm, stat } from 'node:fs/promises';
import * as path from 'node:path';

const workspace = path.dirname(import.meta.dirname);

/** Paired with `vite dev`. */
export const DEV_PORT = 4599;
/** The test harness gets its own, so a dev session and a test run cannot
 *  fight over one port. */
export const TEST_PORT = 4567;

/** Where the wasm build of the compiler lives for the demo (and the wasm-path
 *  test) to import — in the module graph, not `public/`, because Vite will not
 *  transform a module served out of `public/`. */
const wasmDst = path.join(workspace, 'ulottie-dev-server', 'demo', 'src', 'generated', 'wasm');

const exists = (p: string) => stat(p).then(() => true, () => false);

const run = (cmd: string, args: string[], cwd: string) =>
  new Promise<void>((resolve, reject) => {
    const child = spawn(cmd, args, { stdio: 'inherit', cwd });
    child.on('exit', (code) =>
      code === 0 ? resolve() : reject(new Error(`${cmd} ${args.join(' ')} exited ${code}`)),
    );
    child.on('error', reject);
  });

/**
 * Build the wasm compiler into `demo/src/generated/wasm`.
 *
 * One implementation shared by `vite build`/`vite dev` (see `demoAssets` in
 * vite.config.ts) and the vitest global setup — the test suite exercises the
 * wasm path, so its artifact must be built by the same rule that builds the
 * shipping one, or the browser build can silently drift from the crate.
 * `ULOTTIE_SKIP_WASM` stays as the manual escape hatch for a quick iteration
 * loop; nothing sets it in CI.
 */
export async function buildWasm(): Promise<void> {
  if (process.env.ULOTTIE_SKIP_WASM) return;
  const compilerDir = path.join(workspace, 'ulottie-compiler');
  // wasm-pack 0.15's `--out-dir` maps onto cargo's unstable
  // `--artifact-dir`, which the stable toolchain rejects — so build into the
  // crate's default pkg/ and move it ourselves.
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
}

const ping = (port: number) =>
  fetch(`http://127.0.0.1:${port}/healthz`).then((r) => r.ok, () => false);

/** Start the server on `port`; returns a function that stops it. */
export async function startCompileServer(port: number): Promise<() => void> {
  if (await ping(port)) return () => {};

  // Release binaries up front, so the server starts instantly and a compile
  // error surfaces here rather than as a confused browser. The compiler binary
  // comes too: the snapshot suite shells out to it directly.
  execFileSync(
    'cargo',
    ['build', '--release', '-p', 'ulottie-dev-server', '-p', 'ulottie-compiler', '-q'],
    { cwd: workspace, stdio: 'inherit' },
  );

  const bin = path.join(workspace, 'target', 'release', 'ulottie-dev-server');
  const child = spawn(bin, ['serve', '--port', String(port)], {
    cwd: workspace,
    stdio: ['ignore', 'inherit', 'inherit'],
  });
  child.on('error', (err) => console.error('ulottie-dev-server spawn error:', err));

  const deadline = Date.now() + 10_000;
  while (Date.now() < deadline) {
    if (await ping(port)) {
      return () => {
        if (!child.killed) child.kill('SIGTERM');
      };
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  child.kill('SIGTERM');
  throw new Error(`ulottie-dev-server did not become ready on ${port} within 10s`);
}

/** vitest `globalSetup`: the returned function is the teardown. */
export default async function setup() {
  // Before the server: the wasm-path browser test imports the glue, and the
  // artifact must reflect the crate as it is now, not as of the last
  // `vite dev` — under test the demoAssets plugin does not run.
  await buildWasm();
  return startCompileServer(TEST_PORT);
}

// `node global-setup.ts` runs one standalone. The spawned child keeps the
// event loop alive, so this stays up until interrupted.
if (import.meta.main) {
  await startCompileServer(DEV_PORT);
}
