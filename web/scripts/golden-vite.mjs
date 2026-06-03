import { createServer } from 'vite';

export async function withGoldenViteServer(callback, options = {}) {
  const server = await createServer({
    root: process.cwd(),
    configFile: options.configFile ?? false,
    logLevel: 'silent',
    server: { middlewareMode: true },
  });

  try {
    return await callback(server);
  } finally {
    await server.close();
  }
}
