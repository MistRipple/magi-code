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
const messageListSource = await readFile(
  new URL('../src/components/MessageList.svelte', import.meta.url),
  'utf8',
);
const messageItemSource = await readFile(
  new URL('../src/components/MessageItem.svelte', import.meta.url),
  'utf8',
);
const bridgeSource = await readFile(
  new URL('../src/shared/bridges/web-client-bridge.ts', import.meta.url),
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
  /followUpMode:\s*!replaceTurnId\s*&&\s*!isDraftSession\s*&&\s*isSending\s*\?\s*'queue'\s*:\s*undefined/,
  '执行中普通消息必须固定进入队列，编辑重发不能被重新排队',
);
assert.match(
  inputAreaSource,
  /sendPreparing \? 'loader' : 'send'[\s\S]*?sendPreparing \? 'spinning' : ''/,
  '发送预处理阶段必须立即把发送图标切换为加载动画',
);
assert.match(
  inputAreaSource,
  /\.ia-container :global\(\.spinning\) \{ animation: ia-spin 0\.8s linear infinite; \}/,
  '输入区加载图标必须具有真实旋转动画，不能只切换静态 loader 图标',
);
assert.doesNotMatch(
  inputAreaSource,
  /FollowUpMode|followUpMode\s*=|input-followup-mode-button|ia-followup-mode/,
  '输入框不得保留排队与引导切换状态或入口',
);
assert.match(
  messageListSource,
  /metadata\.turnStatus === 'cancelled'[\s\S]*?metadata\.interruptionSource === 'user'/,
  '只有最近一条由用户主动停止的消息可以进入编辑模式',
);
assert.match(
  inputAreaSource,
  /input\.editingPreviousMessage[\s\S]*?input\.cancelEditing/,
  '编辑模式必须复用底部输入框并提供紧凑取消入口',
);
assert.match(
  inputAreaSource,
  /replaceTurnId,[\s\S]*?if \(!replaceTurnId\) \{[\s\S]*?clearComposerState\(\)/,
  '编辑重发必须携带 replaceTurnId，且提交前不能清空输入内容',
);
assert.match(
  messageItemSource,
  /\{:else if isUser\}[\s\S]*?messageItem\.copyTitle/,
  '复制入口必须只存在于用户消息分支',
);
assert.equal(
  (messageItemSource.match(/messageItem\.copyTitle/g) || []).length,
  1,
  '系统通知和助手消息不得显示复制入口',
);
assert.match(
  messageItemSource,
  /\.user-message-actions\s*\{[\s\S]*?display:\s*flex;[\s\S]*?justify-content:\s*flex-end;[\s\S]*?margin-bottom:/,
  '用户消息操作区必须在气泡上方正常占位，并与气泡右边缘对齐',
);
const messageActionsStyle = messageItemSource.match(/\.user-message-actions\s*\{([\s\S]*?)\n  \}/)?.[1] || '';
assert.doesNotMatch(
  messageActionsStyle,
  /position:\s*absolute|top:|right:|border:\s*1px|box-shadow:|transform:|opacity:|pointer-events:/,
  '消息操作区不得绝对定位覆盖气泡，也不得渲染为悬浮窗',
);
assert.doesNotMatch(
  messageItemSource,
  /\.message-item:hover\s*>\s*\.message-actions|\.system-notice:hover\s*>\s*\.message-actions/,
  '消息操作区不得通过消息悬停状态控制显示',
);
assert.match(
  bridgeSource,
  /messagesState\.editingTurn !== null[\s\S]*?completeTurnEditing\(replaceTurnId\)/,
  '编辑期间必须暂停队列，并且只在替换请求成功后结束编辑态',
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
