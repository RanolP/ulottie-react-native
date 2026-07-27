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
import * as path from 'node:path';

const workspace = path.dirname(import.meta.dirname);

/** Paired with `vite dev`. */
export const DEV_PORT = 4599;
/** The test harness gets its own, so a dev session and a test run cannot
 *  fight over one port. */
export const TEST_PORT = 4567;

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
export default function setup() {
  return startCompileServer(TEST_PORT);
}

// `node global-setup.ts` runs one standalone. The spawned child keeps the
// event loop alive, so this stays up until interrupted.
if (import.meta.main) {
  await startCompileServer(DEV_PORT);
}
