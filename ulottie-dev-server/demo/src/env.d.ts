/// <reference types="vite/client" />

/**
 * Size of the installed `lottie-web` bundle, injected by `vite.config.ts`.
 * A build-time constant so the demo reports the real dependency rather than a
 * number that goes stale.
 */
declare const __LOTTIE_WEB_SIZE__: { raw: number; gzipped: number };
