import { createServer } from 'vite';
import { fileURLToPath } from 'node:url';

const webRoot = fileURLToPath(new URL('..', import.meta.url));

export async function withGoldenViteServer(callback, options = {}) {
  const server = await createServer({
    root: options.root ?? webRoot,
    configFile: options.configFile ?? false,
    logLevel: 'silent',
    server: { middlewareMode: true },
  });

  try {
    return await callback(server);
  } finally {
    await options.cleanup?.();
    await server.close();
  }
}
