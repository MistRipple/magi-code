import { createServer } from 'vite';
import { fileURLToPath } from 'node:url';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const webRoot = fileURLToPath(new URL('..', import.meta.url));

// Svelte rune stores are also imported by SSR golden tests.  The tests exercise
// pure helpers rather than Svelte's runtime, so provide deterministic no-op
// rune implementations for modules such as the shared i18n store.
if (typeof globalThis.$state !== 'function') {
  globalThis.$state = (value) => value;
}
if (typeof globalThis.$derived !== 'function') {
  globalThis.$derived = (value) => typeof value === 'function' ? value() : value;
}

export async function withGoldenViteServer(callback, options = {}) {
  // Golden SSR checks must not replace the running daemon's client dependency cache.
  const cacheDir = await mkdtemp(join(tmpdir(), 'magi-golden-vite-'));
  const server = await createServer({
    cacheDir,
    root: options.root ?? webRoot,
    configFile: options.configFile ?? false,
    ...(options.plugins ? { plugins: options.plugins } : {}),
    ...(options.ssr ? { ssr: options.ssr } : {}),
    logLevel: 'silent',
    server: { middlewareMode: true },
  });

  try {
    return await callback(server);
  } finally {
    await options.cleanup?.();
    await server.close();
    await rm(cacheDir, { recursive: true, force: true });
  }
}
