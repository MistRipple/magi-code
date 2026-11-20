import assert from 'node:assert/strict';
import { randomBytes } from 'node:crypto';
import { request } from 'node:http';
import test from 'node:test';
import { PluginLifecycle } from '../lifecycle.mjs';
import { createSessionReader } from '../session-reader.mjs';
import { createWebRouteCarrier } from '../web-routes.mjs';

async function fixture(t, sessionReader) {
  const lifecycle = new PluginLifecycle({ hostGeneration: 1, onStatus() {}, onDiagnostic() {} });
  const proof = randomBytes(32).toString('hex');
  const carrier = await createWebRouteCarrier(lifecycle, proof, sessionReader);
  t.after(async () => { try { await lifecycle.close(); } finally { await carrier.close(); } });
  const identity = { hostGeneration: 1, pluginId: 'fixture', instanceId: 'include:fixture', version: '1.0.0' };
  const epochs = new Map();
  const activate = async (plugin, activationGeneration = 1) => {
    const state = await lifecycle.activate({ ...identity, activationGeneration, plugin, config: {} });
    epochs.set(activationGeneration, state.callEpoch);
  };
  const deactivate = (activationGeneration = 1) => lifecycle.deactivate({ ...identity, activationGeneration });
  const headers = (generation = 1, instance = identity.instanceId) => ({ 'x-magi-route-proof': proof,
    'x-magi-route-instance': Buffer.from(instance).toString('base64url'), 'x-magi-route-generation': String(generation),
    'x-magi-route-epoch': String(epochs.get(generation) ?? 1) });
  const send = (path, options) => fetch(new URL(path, carrier.origin), options);
  return { carrier, activate, deactivate, headers, send, lifecycle };
}

test('a session route reads a fixed committed revision and revokes its handle after the call', async t => {
  const requests = [];
  const sessionReader = createSessionReader(async (method, params) => {
    assert.equal(method, 'host.session.history');
    requests.push(params);
    const session = { id: 'native-session', createdAt: 10, cwd: '/workspace' };
    const observation = { kind: 'textInput', messageId: 'stored-message', text: 'committed input' };
    const events = params.after === 0 ? [{ seq: 0, sourceSequence: 2, occurredAt: 11, observation }] : [];
    return { session, through: 3, nextAfter: null, events };
  });
  const f = await fixture(t, sessionReader);
  await f.lifecycle.context.plugin(sessionReader.Sessions).await();
  await f.lifecycle.context.plugin(sessionReader.WorkingDirectory).await();
  const identity = { id: 'native-session', createdAt: 10, initialDirectory: '/workspace', executionDirectory: '/worktree' };
  const headers = { ...f.headers(), 'x-magi-route-session': Buffer.from(JSON.stringify(identity)).toString('base64url') };
  let escaped;
  await f.activate({
    inject: ['sessions', 'workingDirectory'],
    apply(context) {
      return f.carrier.register(context, { kind: 'exact', path: '/inspect', handler(request, response) {
        assert.equal(request.headers['x-magi-route-session'], undefined);
        const session = context.sessions.get(identity.id);
        assert.throws(() => context.sessions.get('another-session'), /outside the admitted call/);
        assert.equal(context.workingDirectory.get(session), '/worktree');
        assert.equal(session.header.cwd, '/workspace');
        assert.equal(session.revision, 3);
        assert.equal(session.seq, 1);
        assert.equal(session.eventAt(0), session.snapshotEvents()[0]);
        escaped = session;
        response.end(session.eventAt(0).data.content[0].text);
      } });
    },
  });
  assert.equal((await f.send('/inspect', { headers: { ...headers, ...f.headers(2) } })).status, 409);
  assert.equal(requests.length, 0);
  for (let i = 0; i < 2; i++) {
    assert.equal(await (await f.send('/inspect', { headers })).text(), 'committed input');
    assert.throws(() => escaped.snapshotEvents(), /call has ended|route has closed/);
  }
  assert.deepEqual(requests.map(({ sessionId, after }) => [sessionId, after]), [
    [identity.id, 0], [identity.id, 3],
  ]);
});

test('official named route matching runs only under the owning instance and cleans up replacement routes', async t => {
  const f = await fixture(t);
  let calls = 0, removeExact;
  function plugin(context) {
    f.carrier.register(context, { kind: 'prefix', path: '/api', handler(_request, response) { response.end('prefix'); } });
    removeExact = f.carrier.register(context, { kind: 'exact', path: '/api/entry', async handler(request, response) {
      assert.equal(request.headers['x-magi-route-proof'], undefined);
      assert.equal(request.headersDistinct['x-magi-route-proof'], undefined);
      assert.equal(request.rawHeaders.some(header => header.toLowerCase() === 'x-magi-route-proof'), false);
      const chunks = [];
      for await (const chunk of request) chunks.push(chunk);
      response.end(`${request.url}:${Buffer.concat(chunks)}`);
      calls++;
    } });
  }
  await f.activate(plugin);
  assert.equal((await f.send('/api/entry')).status, 403);
  assert.equal((await f.send('/api/entry', { headers: f.headers(1, 'another-instance') })).status, 404);
  assert.equal((await f.send('/api/entry', { headers: f.headers(2) })).status, 409);
  assert.equal(calls, 0);
  const response = await f.send('/api/entry?view=settings', { headers: f.headers(), method: 'POST', body: 'source' });
  assert.equal(await response.text(), '/api/entry?view=settings:source');
  assert.equal(calls, 1);
  await removeExact();
  assert.equal(await (await f.send('/api/entry', { headers: f.headers() })).text(), 'prefix');
  await f.deactivate();
  assert.equal((await f.send('/api/entry', { headers: f.headers() })).status, 404);
  await f.activate(plugin, 2);
  assert.equal((await f.send('/api/entry', { headers: f.headers() })).status, 409);
  assert.equal(await (await f.send('/api/entry', { headers: f.headers(2) })).text(), '/api/entry:');
  assert.equal(calls, 2);
});

test('disposing the activation aborts a live resource body and an upgraded socket', async t => {
  const f = await fixture(t);
  const cancelled = Promise.withResolvers();
  const closed = Promise.withResolvers();
  const lateStarted = Promise.withResolvers();
  const lateResponse = Promise.withResolvers();
  const lateCancelled = Promise.withResolvers();
  await f.activate(context => {
    f.carrier.register(context, { kind: 'exact', path: '/events', async resource(request) {
      assert.equal(request.headers.has('x-magi-route-proof'), false);
      return new Response(new ReadableStream({
        start(controller) { controller.enqueue(new TextEncoder().encode('first')); },
        cancel() { cancelled.resolve(); },
      }), { headers: { 'content-type': 'text/event-stream' } });
    } });
    f.carrier.registerUpgrade(context, { path: '/upgrade', handler(request, socket) {
      assert.equal(request.headers['x-magi-route-proof'], undefined);
      socket.once('close', closed.resolve);
      socket.write('HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: plugin-test\r\n\r\n');
    } });
    f.carrier.register(context, { kind: 'exact', path: '/late', resource() {
      lateStarted.resolve();
      return lateResponse.promise;
    } });
  });
  const connection = await new Promise((resolve, reject) => {
    const pending = request(new URL('/upgrade', f.carrier.origin), { headers: { ...f.headers(), Connection: 'Upgrade', Upgrade: 'plugin-test' } });
    pending.once('upgrade', (_response, socket) => resolve(socket));
    pending.once('error', reject);
    pending.end();
  });
  t.after(() => connection.destroy());
  const stream = await f.send('/events', { headers: f.headers() });
  const reader = stream.body.getReader();
  assert.equal(new TextDecoder().decode((await reader.read()).value), 'first');
  const next = assert.rejects(reader.read());
  const late = f.send('/late', { headers: f.headers() });
  await lateStarted.promise;
  await f.deactivate();
  await Promise.all([cancelled.promise, closed.promise, next]);
  assert.equal((await late).status, 409);
  lateResponse.resolve(new Response(new ReadableStream({ cancel() { lateCancelled.resolve(); } })));
  await lateCancelled.promise;
  assert.equal((await f.send('/events', { headers: f.headers() })).status, 404);
});
