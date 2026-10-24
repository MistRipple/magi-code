import { createServer } from 'vite';
import { fileURLToPath } from 'node:url';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const webRoot = fileURLToPath(new URL('..', import.meta.url));

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
