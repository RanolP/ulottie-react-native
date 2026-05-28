// Pre-flight: spawn `ulottie-dev-server` so the visual-diff suite can fetch
// fixtures and compiled modules over HTTP. The server compiles on demand and
// caches under `<crate>/.output/`; nothing is written into `public/`.
//
// vitest's Vite config proxies `/.output/*` and `/compile` to this server
// (see vitest.config.ts).

import { spawn, execFileSync, type ChildProcess } from 'node:child_process';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const crate = resolve(here, '..');
const project = resolve(crate, '..');

const DEV_SERVER_PORT = 4567;
const DEV_SERVER_URL = `http://127.0.0.1:${DEV_SERVER_PORT}`;

export default async function setup() {
  // Build release binaries up front so the spawned server starts instantly
  // (and so any compile-time errors surface before the browser comes up).
  execFileSync('cargo', ['build', '--release', '-p', 'ulottie-dev-server', '-q'], {
    cwd: project,
    stdio: 'inherit',
  });

  // If a dev server is already up on the port, reuse it.
  if (await pingReady(DEV_SERVER_URL)) {
    return;
  }

  const bin = resolve(project, 'target', 'release', 'ulottie-dev-server');
  const child: ChildProcess = spawn(bin, ['--port', String(DEV_SERVER_PORT)], {
    cwd: project,
    stdio: ['ignore', 'inherit', 'inherit'],
    detached: false,
  });
  child.on('error', err => {
    console.error('ulottie-dev-server spawn error:', err);
  });

  // Wait for the server to start accepting connections.
  const deadline = Date.now() + 10_000;
  while (Date.now() < deadline) {
    if (await pingReady(DEV_SERVER_URL)) {
      // Returned teardown stops the child on test-suite end.
      return () => {
        if (!child.killed) child.kill('SIGTERM');
      };
    }
    await new Promise(r => setTimeout(r, 100));
  }
  throw new Error(`ulottie-dev-server did not become ready within 10s`);
}

async function pingReady(url: string): Promise<boolean> {
  try {
    const res = await fetch(`${url}/compare-all.html`, { method: 'HEAD' });
    return res.ok;
  } catch {
    return false;
  }
}
