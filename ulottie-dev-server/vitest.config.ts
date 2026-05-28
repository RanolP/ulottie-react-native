/// <reference types="vitest" />
import { defineConfig } from 'vitest/config';
import { playwright } from '@vitest/browser-playwright';
import { compare as odiffCompare } from 'odiff-bin';
import { mkdtemp } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

// Host-side scratch dir for odiff diff PNGs. One per test session.
let diffDir: string | undefined;
async function ensureDiffDir(): Promise<string> {
  if (diffDir) return diffDir;
  diffDir = await mkdtemp(join(tmpdir(), 'ulottie-odiff-'));
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
  }
}

const DEV_SERVER = 'http://127.0.0.1:4567';

export default defineConfig({
  test: {
    globalSetup: ['./tests/global-setup.ts'],
    browser: {
      enabled: true,
      provider: playwright(),
      instances: [{ browser: 'chromium' }],
      headless: true,
      commands: {
        async odiffCompare(_ctx, refPath: string, candPath: string, options = {}) {
          const dir = await ensureDiffDir();
          const diffPath = join(dir, `diff-${Date.now()}-${Math.random().toString(36).slice(2, 8)}.png`);
          const res = await odiffCompare(refPath, candPath, diffPath, {
            antialiasing: options.antialiasing ?? true,
            threshold: options.threshold ?? 0.1,
          });
          if (res.match) {
            return { match: true, diffPercentage: 0 };
          }
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
    // Forward fixture + compile traffic to the spawned dev server. Vite
    // handles everything else (harness sources, node_modules, vitest
    // internals).
    proxy: {
      '/.output': { target: DEV_SERVER, changeOrigin: false },
      '/compile': { target: DEV_SERVER, changeOrigin: false },
    },
  },
});
