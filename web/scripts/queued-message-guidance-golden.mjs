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
  /class="ia-queue-action ia-queue-guide"[\s\S]*?<Icon name="corner-down-right"[\s\S]*?input\.queue\.guide/,
  '引导操作必须使用折返箭头图标并保留文字标签',
);
assert.match(
  inputAreaSource,
  /type: 'guideQueuedMessage'[\s\S]*?queuedMessageId/,
  '引导按钮必须提交排队消息标识，由桥接层原子转换为当前轮引导',
);
assert.match(
  inputAreaSource,
  /followUpMode:\s*!isDraftSession\s*&&\s*isSending\s*\?\s*'queue'\s*:\s*undefined/,
  '执行中从输入框发送必须固定进入队列',
);
assert.doesNotMatch(
  inputAreaSource,
  /FollowUpMode|followUpMode\s*=|input-followup-mode-button|ia-followup-mode/,
  '输入框不得保留排队与引导切换状态或入口',
);
assert.match(
  inputAreaSource,
  /\.ia-queue-panel\s*\{[\s\S]*?overflow:\s*hidden;/,
  '方案 B 必须使用单一静默托盘容器承载队列',
);
assert.match(
  inputAreaSource,
  /\.ia-queue-index\s*\{[\s\S]*?width:\s*5px;[\s\S]*?height:\s*5px;[\s\S]*?border-radius:\s*50%;/,
  '方案 B 的排队序号必须收敛为低对比度圆点',
);
assert.match(
  inputAreaSource,
  /\.ia-queue-item\s*\+[\s\S]*?\.ia-queue-item\s*\{[\s\S]*?border-top:/,
  '方案 B 的消息条目必须由细分隔线组织，不能拆成独立卡片',
);
assert.match(
  inputAreaSource,
  /\.ia-queue-action\s*\{[\s\S]*?border:\s*0;[\s\S]*?background:\s*transparent;/,
  '排队操作必须使用无边框、无胶囊背景的轻量按钮',
);
assert.match(
  inputAreaSource,
  /\.ia-queue-actions\s*\{[\s\S]*?opacity:\s*0;[\s\S]*?pointer-events:\s*none;[\s\S]*?\.ia-queue-item:hover \.ia-queue-actions,[\s\S]*?opacity:\s*1;[\s\S]*?pointer-events:\s*auto;/,
  '桌面端排队操作必须只在悬浮或键盘聚焦后显示并接收点击',
);

console.log('queued message guidance golden passed');
