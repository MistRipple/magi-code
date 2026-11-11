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
    plugins: [
      ...(options.plugins ?? []),
      {
        name: 'magi-golden-ssr-only',
        enforce: 'post',
        configEnvironment(name, environment) {
          if (name !== 'client') return;
          // Golden 只调用 ssrLoadModule；客户端预构建既不参与断言，也不能在关闭后继续写缓存。
          environment.optimizeDeps = {
            ...environment.optimizeDeps,
            noDiscovery: true,
            include: [],
          };
        },
      },
    ],
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
