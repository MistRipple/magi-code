import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
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
  let session = 'first', busy = false, counter = 0, calls = 0, admitted = true, current = true, textGranted = true;
  let completeText, beforeText;
  const textCalls = [];
  const images = new Map();
  const release = ports.registerPluginComposer({
    sessionId: () => session,
    writable: () => !busy && images.size < 5,
    textWritable: () => !busy && images.size === 0,
    beginRead: () => busy ? null : () => {},
    add(items) { if (busy || items.length + images.size > 5) return null; const ids = items.map(() => `id-${++counter}`); ids.forEach((id, i) => images.set(id, items[i])); return ids; },
    has: (id) => images.has(id), remove: (id) => images.delete(id), focus() {}, notify() {},
    async submitText(input, admit) {
      await beforeText?.();
      await admit();
      textCalls.push(input);
      return new Promise(resolve => { completeText = status => resolve({ submissionId: input.submissionId, status }); });
    },
  });
  const bridge = createPluginComposerBridge(async target => { calls++; if (!admitted || target === 'text' && !textGranted) throw new Error('denied'); }, () => current, 'test.first');
  const input = { dataUrl: 'data:image/png;base64,eA==', name: 'image.png', size: 1 };
  try {
    assert.throws(() => ports.registerPluginComposer({}), /已有/);
    assert.deepEqual(await bridge.call('composer.snapshot', {}), { sessionId: 'first', attachmentIds: [], writable: true });
    const accepted = await bridge.call('composer.addImages', { sessionId: 'first', images: [input, input] });
    assert.equal(accepted.accepted, true); assert.equal(accepted.ids.length, 2); assert.equal(images.size, 2);
    const other = createPluginComposerBridge(async () => {}, () => true, 'test.other');
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
    const text = { sessionId: 'first', submissionId: 'text-1', text: 'Follow up 🙂' };
    textGranted = false;
    await assert.rejects(bridge.call('composer.submitText', text), /denied/, '图片授权不能继承为文本提交');
    assert.equal(textCalls.length, 0);
    textGranted = true;
    const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(JSON.stringify(['test.first',text.sessionId,text.submissionId])));
    const nativeId = 'plugin-composer-' + [...new Uint8Array(digest)].map(byte=>byte.toString(16).padStart(2,'0')).join('');
    const nativeText = { ...text, submissionId: nativeId };
    const first = ports.submitPluginComposerText(nativeText, async()=>{});
    const duplicate = ports.submitPluginComposerText({ ...nativeText }, async()=>{});
    await new Promise(setImmediate);
    assert.equal(textCalls.length, 1, '相同在途操作只提交一次');
    await assert.rejects(bridge.call('composer.submitText', { ...text, text: 'changed' }), /正在提交|in progress/);
    completeText('unknown');
    assert.deepEqual(await first, { submissionId: nativeId, status: 'unknown' });
    assert.deepEqual(await duplicate, await first);
    const retry = bridge.call('composer.submitText', text);
    while(textCalls.length<2) await new Promise(setImmediate);
    assert.deepEqual(textCalls[1], textCalls[0], '显式重试保留原输入与提交身份，由 daemon 判重');
    assert.match(textCalls[0].submissionId, /^plugin-composer-[0-9a-f]{64}$/);
    assert.notEqual(textCalls[0].submissionId, text.submissionId, '插件操作身份不能占用用户 requestId');
    completeText('accepted');
    assert.equal((await retry).status, 'accepted');
    for (const mutate of [() => { session = 'second'; }, () => { current = false; }, () => { textGranted = false; }]) {
      const count = textCalls.length;
      beforeText = mutate;
      await assert.rejects(bridge.call('composer.submitText', { ...text, submissionId: 'cancelled' }));
      assert.equal(textCalls.length, count, '派发前会话、页面或授权失效不能提交');
      session = 'first'; current = true; textGranted = true;
    }
    beforeText = undefined;
    await assert.rejects(bridge.call('composer.submitText', { ...text, text: 'x'.repeat(10001) }));
    await assert.rejects(bridge.call('composer.submitText', { ...text, submissionId: ' invalid ' }));
    globalThis.window = new EventTarget();
    try {
      const observed = ports.observeComposerSubmission('observed');
      let settled = false; observed.result.then(() => { settled = true; });
      window.dispatchEvent(new CustomEvent('magi:sessionTurnSubmissionSettled', { detail: { requestId: 'other', status: 'accepted' } }));
      await Promise.resolve(); assert.equal(settled, false);
      window.dispatchEvent(new CustomEvent('magi:sessionTurnSubmissionSettled', { detail: { requestId: 'observed', status: 'accepted' } }));
      assert.deepEqual(await observed.result, { submissionId: 'observed', status: 'accepted' });
      const failed = ports.observeComposerSubmission('failed');
      window.dispatchEvent(new CustomEvent('magi:sessionTurnSubmissionSettled', { detail: { requestId: 'failed', status: 'failed' } }));
      assert.equal((await failed.result).status, 'unknown', '失败等待不等于未接纳');
      const closed = ports.observeComposerSubmission('closed'); closed.dispose();
      assert.equal((await closed.result).status, 'unknown');
    } finally { delete globalThis.window; }
  } finally { release(); }
  assert.equal(ports.pluginComposerSession(), null);
});
// 切换会话时内嵌视图（输入区左侧、消息附加视图）必须保持同一个 iframe：会话只通过 conversation 消息下发。
// 把会话放进文档身份会让每次切换都销毁并重建文档，输入区左下角随之闪烁。
const pluginFrame = await readFile(new URL('../src/components/tabs/PluginFrame.svelte', import.meta.url), 'utf8');
const identity = pluginFrame.match(/const documentIdentity = \$derived\(JSON\.stringify\(\[([^\]]*)\]\)\);/);
assert.ok(identity, '必须能找到 PluginFrame 的文档身份定义');
assert.doesNotMatch(identity[1], /sessionId|conversation/, '会话不能参与内嵌视图的文档身份');
assert.match(pluginFrame, /void conversation;\s*sendHostContext\(\);/, '会话变化必须经 conversation 消息下发给已存在的文档');
console.log('plugin composer golden checks passed');
