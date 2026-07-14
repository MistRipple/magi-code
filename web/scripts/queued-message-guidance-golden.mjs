import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { withGoldenViteServer } from './golden-vite.mjs';

await withGoldenViteServer(async (server) => {
  const guidance = await server.ssrLoadModule('/src/lib/queued-message-guidance.ts');
  const plainTextMessage = {
    id: 'queued-plain',
    content: '优先处理错误路径',
    text: '优先处理错误路径',
    createdAt: Date.now(),
    sessionId: 'session-1',
  };

  assert.equal(guidance.canGuideQueuedMessage(plainTextMessage), true);
  assert.equal(guidance.canGuideQueuedMessage({ ...plainTextMessage, skillName: 'review' }), false);
  assert.equal(guidance.canGuideQueuedMessage({ ...plainTextMessage, goalMode: true }), false);
  assert.equal(guidance.canGuideQueuedMessage({
    ...plainTextMessage,
    images: [{ name: 'image.png', dataUrl: 'data:image/png;base64,AA==' }],
  }), false);
  assert.equal(guidance.canGuideQueuedMessage({
    ...plainTextMessage,
    contextReferences: [{ kind: 'file', path: '/tmp/a', name: 'a' }],
  }), false);
  assert.equal(guidance.canGuideQueuedMessage({ ...plainTextMessage, text: '   ', content: '   ' }), false);
}, { configFile: 'vite.web.config.ts' });

const inputAreaSource = await readFile(
  new URL('../src/components/InputArea.svelte', import.meta.url),
  'utf8',
);
assert.match(
  inputAreaSource,
  /class="ia-queue-action ia-queue-guide"[\s\S]*?input\.queue\.guide/,
  '排队消息悬浮操作区必须提供引导按钮',
);
assert.match(
  inputAreaSource,
  /type: 'guideQueuedMessage'[\s\S]*?queuedMessageId/,
  '引导按钮必须提交排队消息标识，由桥接层原子转换为当前轮引导',
);
assert.match(
  inputAreaSource,
  /\.ia-queue-actions\s*\{[\s\S]*?opacity:\s*0;[\s\S]*?pointer-events:\s*none;[\s\S]*?\.ia-queue-item:hover \.ia-queue-actions,[\s\S]*?opacity:\s*1;[\s\S]*?pointer-events:\s*auto;/,
  '桌面端排队操作必须只在悬浮或键盘聚焦后显示并接收点击',
);

console.log('queued message guidance golden passed');
