import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

await withGoldenViteServer(async (server) => {
  const navigation = await server.ssrLoadModule('/src/lib/browser-navigation.ts');

  delete globalThis.window;
  assert.equal(navigation.canOpenHtmlFileInMagiBrowser(), false);
  assert.equal(navigation.requestOpenHtmlFileInBrowser('/tmp/index.html'), false);

  const events = [];
  globalThis.window = {
    location: { search: '' },
    dispatchEvent(event) {
      events.push(event);
      return true;
    },
  };
  assert.equal(navigation.requestOpenHtmlFileInBrowser('/tmp/index.html'), false);
  assert.equal(events.length, 0);

  window.location.search = '?desktopSurface=app';
  assert.equal(navigation.requestOpenHtmlFileInBrowser('/tmp/index.html'), false);
  assert.equal(events.length, 0);

  window.magiDesktop = { runtime: 'electron', surface: 'app' };
  assert.equal(navigation.requestOpenHtmlFileInBrowser('/tmp/index.html'), true);
  assert.equal(events.length, 1);
  assert.equal(events[0].type, navigation.OPEN_HTML_FILE_IN_BROWSER_EVENT);
  assert.deepEqual(events[0].detail, { filepath: '/tmp/index.html' });

  assert.equal(navigation.requestOpenHtmlFileInBrowser('/tmp/readme.md'), false);
  assert.equal(events.length, 1);

  // 链接点击：只有 http(s) 才派发「在内置浏览器打开」，其余协议（mailto、
  // javascript 等）不在这里处理，由调用方交给外部处理器。
  const before = events.length;
  assert.equal(navigation.requestOpenUrlInBrowser('https://example.com/a?b=1'), true);
  assert.equal(events.length, before + 1);
  assert.equal(events[events.length - 1].type, navigation.OPEN_URL_IN_BROWSER_EVENT);
  assert.deepEqual(events[events.length - 1].detail, { url: 'https://example.com/a?b=1' });
  assert.equal(navigation.requestOpenUrlInBrowser('mailto:dev@example.com'), false);
  assert.equal(navigation.requestOpenUrlInBrowser('javascript:alert(1)'), false);
  assert.equal(navigation.requestOpenUrlInBrowser('   '), false);
  assert.equal(events.length, before + 1);

  delete globalThis.window;
  console.log('browser navigation golden replay passed');
});
