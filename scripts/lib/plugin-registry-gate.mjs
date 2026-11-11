import { createServer } from 'node:http';
import { Readable } from 'node:stream';

// 仅控制本次验收 daemon 的安装解析。原包、integrity、业务路由和
// registry 其他请求仍转发官方字节，不修改插件或用户正在运行的实例。
export async function createPluginRegistryGate() {
  let control;
  const held = new Set(), errors = [];
  const requests = new Map(), rejections = new Map();
  const server = createServer(async (request, response) => {
    const identity = request.headers['pacote-pkg-id'];
    const name = typeof identity === 'string' && identity.startsWith('registry:') ? identity.slice('registry:'.length) : null;
    if (name) requests.set(name, (requests.get(name) ?? 0) + 1);
    if (control?.name === name && control.mode === 'fail') {
      rejections.set(name, (rejections.get(name) ?? 0) + 1);
      response.writeHead(404, { 'content-type': 'application/json' });
      response.end('{"error":"acceptance-resolution-failure"}');
      return;
    }
    if (control?.name === name && control.mode === 'pause') {
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
    requests(name) { return requests.get(name) ?? 0; },
    rejected(name) { return rejections.get(name) ?? 0; },
    fail(name) { control = { name, mode: 'fail' }; },
    pause(name) { control = { name, mode: 'pause' }; },
    release() { control = undefined; for (const response of held) response.destroy(); },
    async close() {
      control = undefined;
      server.closeAllConnections();
      await new Promise(resolve => server.close(resolve));
    },
  };
}
