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

  delete globalThis.window;
  console.log('browser navigation golden replay passed');
});
