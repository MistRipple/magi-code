import { createServer } from 'node:http';
import { Readable } from 'node:stream';

// 仅控制本次验收 daemon 的安装解析。原包、integrity、业务路由和
// registry 其他请求仍转发官方字节，不修改插件或用户正在运行的实例。
export async function createPluginRegistryGate() {
  let paused;
  const held = new Set(), errors = [];
  const server = createServer(async (request, response) => {
    if (paused && request.headers['pacote-pkg-id'] === `registry:${paused}`) {
      held.add(response);
      response.on('close', () => held.delete(response));
      return;
    }
    try {
      const upstream = await fetch(`https://registry.npmjs.org${request.url}`, {
        headers: { accept: request.headers.accept ?? 'application/json' }, signal: AbortSignal.timeout(120_000),
      });
      if (!upstream.ok) errors.push({ path: request.url, status: upstream.status });
      response.writeHead(upstream.status, { 'content-type': upstream.headers.get('content-type') ?? 'application/json' });
      Readable.fromWeb(upstream.body).on('error', () => response.destroy()).pipe(response);
    } catch (error) {
      errors.push({ path: request.url, error: String(error), cause: String(error.cause ?? '') });
      response.writeHead(502); response.end(String(error));
    }
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  return {
    registry: `http://127.0.0.1:${server.address().port}`,
    held,
    errors,
    pause(name) { paused = name; },
    release() { paused = undefined; for (const response of held) response.destroy(); },
    async close() {
      paused = undefined;
      server.closeAllConnections();
      await new Promise(resolve => server.close(resolve));
    },
  };
}
