import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import WebSocket from 'ws';

const schema = JSON.parse(await readFile(new URL('../../contracts/app-server/app-server.schema.json', import.meta.url), 'utf8'));

// 验收专用的单次 RPC 传输；断连/超时失败，不重发变更、不持有业务状态。
export async function appServerRequest(endpoint, method, params) {
  assert.equal(schema['x-method-signatures'][method]?.kind, 'request', `未知请求 ${method}`);
  const socket = new WebSocket(endpoint);
  const pending = new Map();
  let nextId = 0;
  function fail(error) {
    for (const request of pending.values()) request.reject(error);
    pending.clear();
  }
  socket.on('error', fail);
  socket.on('close', () => fail(new Error('App Server 连接关闭，未重发请求')));
  socket.on('message', raw => {
    try {
      const message = JSON.parse(raw.toString());
      assert.equal(message.jsonrpc, '2.0');
      if (message.method) {
        if (message.id !== undefined) fail(new Error('未声明审批能力的验收客户端收到服务端请求'));
        return;
      }
      const request = pending.get(message.id);
      assert.ok(request, '响应必须匹配在途请求');
      pending.delete(message.id);
      if (message.error) request.reject(new Error(`App Server ${message.error.code}: ${message.error.message}`));
      else request.resolve(message.result);
    } catch (error) { fail(error); }
  });
  const timer = setTimeout(() => {
    fail(new Error(`App Server 超时 ${method}，未重发请求`));
    socket.terminate();
  }, 120_000);
  const request = (name, value) => new Promise((resolve, reject) => {
    const id = `test-${++nextId}`;
    pending.set(id, { resolve, reject });
    socket.send(JSON.stringify({ jsonrpc: '2.0', id, method: name, params: value }));
  });
  try {
    await new Promise((resolve, reject) => {
      socket.once('open', resolve);
      socket.once('error', reject);
      socket.once('close', () => reject(new Error('App Server 握手连接关闭')));
    });
    const { major, minor } = schema['x-protocol'];
    const initialized = await request('initialize', {
      clientInfo: { name: 'magi-controls-test' }, protocol: { major, minor }, capabilities: {},
    });
    assert.equal(initialized.protocol.major, major);
    assert.ok(initialized.runtimeEpoch);
    socket.send(JSON.stringify({ jsonrpc: '2.0', method: 'initialized', params: {} }));
    return await request(method, params);
  } finally {
    clearTimeout(timer);
    socket.close();
  }
}
