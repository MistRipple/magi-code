import test from 'node:test';
import assert from 'node:assert/strict';
import { setTimeout as delay } from 'node:timers/promises';
import { createRouteStreams } from './runtime/route-streams.mjs';
import { createEventSource } from './runtime/client/event-source.mjs';

const context = () => ({ signal: new AbortController().signal });
const open = (streams, path = '/events') => streams.call({ action: 'open', request: { method: 'GET', path } }, context());
const read = (streams, id, after = 0, ctx = context()) => streams.call({ action: 'read', id, after }, ctx);

test('SSE Node 请求保持打开，UTF-8 跨块完整，未确认批次可重读，关闭回收原请求和响应', async (t) => {
  let res, requestClosed = 0, responseClosed = 0;
  const streams = createRouteStreams(() => ({ handler(req, response) {
    res = response;
    req.on('close', () => requestClosed++);
    response.on('close', () => responseClosed++);
    response.writeHead(200, { 'content-type': 'text/event-stream; charset=utf-8' });
    response.write(': connected\n\n');
  } }));
  t.after(() => streams.dispose());
  const { id } = await open(streams);
  await delay(0);
  assert.equal(requestClosed, 0);
  const bytes = Buffer.from('event: change\ndata: 图片\n\n');
  res.write(bytes.subarray(0, bytes.length - 4));
  res.write(bytes.subarray(bytes.length - 4));
  const batch = await read(streams, id);
  assert.match(batch.chunks.map((c) => c.text).join(''), /图片/);
  assert.deepEqual(await read(streams, id), batch);
  await streams.call({ action: 'close', id }, context());
  await delay(0);
  assert.equal(requestClosed, 1); assert.equal(responseClosed, 1);
  await assert.rejects(read(streams, id), /不存在|过期/);
});

test('事件连接拒绝非 SSE 和未登记路由，限制数量/队列，取消读取、闲置过期和停用都可回收', async (t) => {
  let res, closed = 0;
  const streams = createRouteStreams((path) => path === '/missing' ? undefined : ({ handler(_req, response) {
    res = response;
    response.on('close', () => closed++);
    response.writeHead(200, { 'content-type': path === '/json' ? 'application/json' : 'text/event-stream' });
  } }), { maxStreams: 1, maxBytes: 32, pollMs: 100, idleMs: 40 });
  t.after(() => streams.dispose());
  await assert.rejects(open(streams, '/missing'), /未注册/);
  await assert.rejects(open(streams, '/json'), /text\/event-stream/);
  const { id } = await open(streams);
  await assert.rejects(open(streams), /数量/);
  const controller = new AbortController();
  const waiting = read(streams, id, 0, { signal: controller.signal });
  controller.abort(new Error('cancel read'));
  await assert.rejects(waiting, /cancel read/);
  res.write('x'.repeat(33));
  assert.match((await read(streams, id)).error, /上限/);
  await delay(60);
  await assert.rejects(read(streams, id), /过期/);
  const reopened = await open(streams);
  streams.dispose();
  await assert.rejects(read(streams, reopened.id), /不存在|过期/);
  await assert.rejects(open(streams), /停止/);
  assert.equal(closed, 3);
});

test('Fetch SSE 使用原始响应流，断开取消 reader，不因普通 unary 请求结束而销毁连接', async () => {
  let writer, cancelled = 0;
  const streams = createRouteStreams(() => ({ resource(request) {
    assert.equal(request.method, 'GET');
    return new Response(new ReadableStream({ start(c) { writer = c; }, cancel() { cancelled++; } }), { headers: { 'content-type': 'text/event-stream' } });
  } }));
  const { id } = await open(streams);
  writer.enqueue(Buffer.from('data: live\n\n'));
  assert.equal((await read(streams, id)).chunks[0].text, 'data: live\n\n');
  streams.dispose();
  await delay(0);
  assert.equal(cancelled, 1);
});

test('浏览器 EventSource 外观解释跨块 BOM/CRLF、多行 data、命名事件、ID，并只访问本插件', async (t) => {
  let res;
  const streams = createRouteStreams(() => ({ handler(_req, response) {
    res = response;
    response.writeHead(200, { 'content-type': 'text/event-stream' });
  } }));
  const client = createEventSource(async (input, signal) => ({ status: 200, body: await streams.call(input.body, { signal: signal ?? context().signal }) }), 'http://magi.invalid/ui/settings.html');
  t.after(() => { client.dispose(); streams.dispose(); });
  assert.throws(() => new client.EventSource('https://other.example/events'), /当前插件/);
  const es = new client.EventSource('/events');
  await new Promise((resolve) => es.addEventListener('open', resolve, { once: true }));
  const event = new Promise((resolve) => es.addEventListener('change', resolve, { once: true }));
  for (const part of ['\uFEFF: comment\r', '\nid: retained\r\nevent: change\r', '\ndata: 第一行\r\ndata: 第二行\r', '\n\r', '\n']) res.write(part);
  const received = await event;
  assert.equal(received.data, '第一行\n第二行'); assert.equal(received.lastEventId, 'retained');
  es.close();
  assert.equal(es.readyState, es.CLOSED);
});

test('事件确认只释放已收到的数据，大批次有界；旧确认和并发读取不会跳过后续事件', async () => {
  let res;
  const streams = createRouteStreams(() => ({ handler(_req, response) { res = response; response.writeHead(200, { 'content-type': 'text/event-stream' }); } }));
  const { id } = await open(streams);
  const payload = '🙂'.repeat(70_000);
  res.write(payload); res.end();
  let after = 0, value = '', batches = 0;
  while (true) {
    const batch = await read(streams, id, after);
    assert.ok(JSON.stringify(batch).length < 128 * 1024);
    value += batch.chunks.map((item) => item.text).join('');
    after = batch.chunks.at(-1)?.seq ?? after;
    batches++;
    if (batch.ended) break;
  }
  assert.equal(value, payload); assert.ok(batches > 1);
  await read(streams, id, after);
  await assert.rejects(read(streams, id, 0), /序号/);
  streams.dispose();
});

test('连接头超时/打开期间取消均回收上游，EventSource 看到 204 后终止重连', async () => {
  let closed = 0;
  const streams = createRouteStreams((path) => ({ handler(_req, res) {
    res.on('close', () => closed++);
    if (path === '/stop') res.writeHead(204);
  } }), { headerMs: 20 });
  await assert.rejects(open(streams), /超时/);
  const controller = new AbortController();
  const opening = streams.call({ action: 'open', request: { method: 'GET', path: '/events' } }, { signal: controller.signal });
  await delay(0); controller.abort(new Error('open cancelled'));
  await assert.rejects(opening, /open cancelled/);
  let opens = 0;
  const client = createEventSource(async (input, signal) => { opens++; return { status: 200, body: await streams.call(input.body, { signal: signal ?? context().signal }) }; }, 'http://magi.invalid/');
  const es = new client.EventSource('/stop');
  await delay(40);
  assert.equal(es.readyState, es.CLOSED); assert.equal(opens, 1); assert.equal(closed, 3);
  client.dispose(); streams.dispose();
});
