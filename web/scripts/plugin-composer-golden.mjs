import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

// 测试只替代浏览器解码；真实 Chromium 解码与输入挂载由 Electron 验收覆盖。
let decoded;
globalThis.Image = class {
  naturalWidth = 2; naturalHeight = 2;
  async decode() { await decoded?.(); }
};
await withGoldenViteServer(async (server) => {
  const ports = await server.ssrLoadModule('/src/lib/plugin-composer.ts');
  const { createPluginComposerBridge } = await server.ssrLoadModule('/src/lib/plugin-composer-bridge.ts');
  let session = 'first', busy = false, counter = 0, calls = 0, admitted = true, current = true;
  const images = new Map();
  const release = ports.registerPluginComposer({
    sessionId: () => session,
    writable: () => !busy && images.size < 5,
    beginRead: () => busy ? null : () => {},
    add(items) { if (busy || items.length + images.size > 5) return null; const ids = items.map(() => `id-${++counter}`); ids.forEach((id, i) => images.set(id, items[i])); return ids; },
    has: (id) => images.has(id), remove: (id) => images.delete(id), focus() {}, notify() {},
  });
  const bridge = createPluginComposerBridge(async () => { calls++; if (!admitted) throw new Error('denied'); }, () => current);
  const input = { dataUrl: 'data:image/png;base64,eA==', name: 'image.png', size: 1 };
  try {
    assert.throws(() => ports.registerPluginComposer({}), /已有/);
    assert.deepEqual(await bridge.call('composer.snapshot', {}), { sessionId: 'first', attachmentIds: [], writable: true });
    const accepted = await bridge.call('composer.addImages', { sessionId: 'first', images: [input, input] });
    assert.equal(accepted.accepted, true); assert.equal(accepted.ids.length, 2); assert.equal(images.size, 2);
    const other = createPluginComposerBridge(async () => {}, () => true);
    await assert.rejects(other.call('composer.removeImage', { sessionId: 'first', id: accepted.ids[0] }), /不属于/);
    assert.deepEqual((await other.call('composer.snapshot', {})).attachmentIds, []);
    busy = true;
    assert.equal((await bridge.call('composer.addImages', { sessionId: 'first', images: [input] })).accepted, false);
    busy = false;
    const previous = images.size;
    await assert.rejects(bridge.call('composer.addImages', { sessionId: 'first', images: [input, { ...input, size: 99 }] }), /大小/);
    assert.equal(images.size, previous, '批次包含无效图片时不部分挂载');
    for (const type of ['image/avif', 'image/bmp', 'image/x-icon', 'image/svg+xml']) {
      const result = await bridge.call('composer.addImages', { sessionId: 'first', images: [{ ...input, dataUrl: `data:${type};base64,eA==` }] });
      assert.equal(result.accepted, true, `${type} 交给浏览器真实解码，不由格式白名单遗漏`);
      await bridge.call('composer.removeImage', { sessionId: 'first', id: result.ids[0] });
    }
    await assert.rejects(bridge.call('composer.addImages', { sessionId: 'first', images: [{ ...input, dataUrl: 'data:text/html;base64,eA==' }] }), /格式/);
    admitted = false;
    await assert.rejects(bridge.call('composer.addImages', { sessionId: 'first', images: [input] }), /denied/);
    admitted = true;
    decoded = () => { session = 'second'; };
    await assert.rejects(bridge.call('composer.addImages', { sessionId: 'first', images: [input] }), /当前活动会话/);
    assert.equal(images.size, previous, '解码期间切换会话不能投递到新会话');
    decoded = undefined; session = 'first'; current = false;
    await assert.rejects(bridge.call('composer.addImages', { sessionId: 'first', images: [input] }), /身份已失效/);
    current = true;
    decoded = () => { admitted = false; };
    await assert.rejects(bridge.call('composer.addImages', { sessionId: 'first', images: [input] }), /denied/);
    assert.equal(images.size, previous, '解码期间撤权必须在提交前重新拒绝');
    admitted = true;
    decoded = () => { current = false; };
    await assert.rejects(bridge.call('composer.addImages', { sessionId: 'first', images: [input] }), /身份已失效/);
    assert.equal(images.size, previous, '解码期间旧文档失效不能提交');
    current = true; decoded = undefined;
    assert.equal((await bridge.call('composer.removeImage', { sessionId: 'first', id: accepted.ids[0] })).removed, true);
    assert.equal(images.size, 1); assert.ok(calls > 0);
  } finally { release(); }
  assert.equal(ports.pluginComposerSession(), null);
});
console.log('plugin composer golden checks passed');
