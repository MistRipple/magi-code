import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

class FakeEventSource {
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSED = 2;
  static instances = [];

  constructor(url) {
    this.url = url;
    this.readyState = FakeEventSource.CONNECTING;
    this.closed = false;
    this.onopen = null;
    this.onmessage = null;
    this.onerror = null;
    FakeEventSource.instances.push(this);
  }

  close() {
    this.closed = true;
    this.readyState = FakeEventSource.CLOSED;
  }
}

globalThis.EventSource = FakeEventSource;

await withGoldenViteServer(async (server) => {
  const transportModule = await server.ssrLoadModule('/src/shared/transport.ts');
  const transport = transportModule.getTransport();
  let openCount = 0;
  let errorCount = 0;
  const messages = [];

  const connection = transport.connectEventStream('/events?workspaceId=workspace-transport-golden', {
    onOpen() {
      openCount += 1;
    },
    onMessage(data) {
      messages.push(data);
    },
    onError() {
      errorCount += 1;
    },
  });

  const stream = FakeEventSource.instances[0];
  assert.ok(stream, 'transport should create an EventSource instance');
  assert.equal(stream.url, '/events?workspaceId=workspace-transport-golden');

  stream.onopen();
  assert.equal(openCount, 1, 'EventSource open must propagate to handlers');

  stream.onmessage({ data: '{"event_type":"event.stream.keep_alive"}' });
  assert.deepEqual(messages, ['{"event_type":"event.stream.keep_alive"}']);

  stream.readyState = FakeEventSource.CONNECTING;
  stream.onerror();
  assert.equal(
    errorCount,
    1,
    'CONNECTING errors must be reported so the bridge can enter bootstrap recovery immediately',
  );

  connection.close();
  assert.equal(stream.closed, true, 'connection.close must close the EventSource');

  stream.onerror();
  assert.equal(
    errorCount,
    1,
    'client-initiated close must not be reported as a transport failure',
  );

  const requestedUrls = [];
  globalThis.window = {
    location: {
      href: 'https://example.trycloudflare.com/web.html?tunnel_token=secret-token',
    },
  };
  globalThis.fetch = async (url) => {
    requestedUrls.push(String(url));
    return new Response('{}', { status: 200 });
  };
  await transport.request('https://example.trycloudflare.com/api/goals/current?sessionId=session-1');
  const publicRequestUrl = new URL(requestedUrls[0]);
  assert.equal(publicRequestUrl.searchParams.get('tunnel_token'), 'secret-token');

  transport.connectEventStream('https://example.trycloudflare.com/events?sessionId=session-1', {
    onOpen() {},
    onMessage() {},
    onError() {},
  });
  const publicStreamUrl = new URL(FakeEventSource.instances.at(-1).url);
  assert.equal(publicStreamUrl.searchParams.get('tunnel_token'), 'secret-token');

  console.log('live transport golden replay passed');
});
