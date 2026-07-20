import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { withGoldenViteServer } from './golden-vite.mjs';

const railSource = await readFile(
  new URL('../src/components/TurnNavigationRail.svelte', import.meta.url),
  'utf8',
);
const messageListSource = await readFile(
  new URL('../src/components/MessageList.svelte', import.meta.url),
  'utf8',
);
const globalStyleSource = await readFile(
  new URL('../src/styles/global.css', import.meta.url),
  'utf8',
);

function extractRuleBody(source, selector) {
  const escapedSelector = selector.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  return source.match(new RegExp(`${escapedSelector}\\s*\\{([\\s\\S]*?)\\}`))?.[1] || '';
}

assert.match(
  railSource,
  /calculateTurnNavigationMagnet/,
  '生产轨道必须复用经过测试的磁吸强度算法',
);
assert.match(
  railSource,
  /data-testid="turn-navigation-rail"/,
  '宽屏轨道必须提供稳定的浏览器验收入口',
);
assert.match(
  railSource,
  /class="turn-navigation-marker-list"[^>]*bind:this=\{markerListRef\}/,
  '轮次刻度必须位于可独立滚动的固定间距列表中',
);
assert.match(
  railSource,
  /overflow-y:\s*auto/,
  '轨道轮次过多时必须允许内部滚动',
);
assert.match(
  railSource,
  /top:\s*50%[\s\S]*?transform:\s*translateY\(-50%\)/,
  '宽屏轨道必须在对话区域内垂直居中',
);
assert.match(
  railSource,
  /\.turn-navigation-preview\s*\{[\s\S]*?background:\s*var\(--dropdown-bg, var\(--surface-2\)\)/,
  '宽屏悬浮摘要卡片必须保持清晰的高不透明度背景',
);
assert.match(
  railSource,
  /style:--turn-wave-strength=/,
  '鼠标移动时必须把磁吸强度传入每个节点以形成波澜凸起',
);
assert.match(
  railSource,
  /calculateTurnNavigationMagnet\([\s\S]*?event\.clientY\s*-\s*railRect\.top,\s*80,\s*\)/,
  '悬停波澜必须限制在临近节点范围，避免整条轨道同时凸起',
);
assert.match(
  railSource,
  /class:selected-neighbor=/,
  '点击选中后必须单独标记选中节点的上下相邻节点',
);
assert.match(
  railSource,
  /selectedTurnId\s*=\s*item\.turnId/,
  '点击轮次后必须保留独立选中状态，不能被滚动定位覆盖',
);
assert.match(
  railSource,
  /function focusTurn\([\s\S]*?container\.scrollTop\s*=\s*Math\.max\(0,\s*targetTop\);[\s\S]*?activeTurnId\s*=\s*item\.turnId/,
  '点击轮次必须同步定位到目标消息，不能播放消息区滚动过程',
);
assert.doesNotMatch(
  railSource,
  /function focusTurn\([\s\S]*?behavior:\s*['"]smooth['"][\s\S]*?activeTurnId\s*=\s*item\.turnId/,
  '消息区轮次跳转不得保留平滑滚动兼容路径',
);
assert.match(
  railSource,
  /\.turn-navigation-marker\s*\{[\s\S]*?width:\s*6px;/,
  '静默态轨道节点长度必须缩短一半',
);
assert.match(
  railSource,
  /\.turn-navigation-marker-list\s*\{[\s\S]*?gap:\s*11px;/,
  '轨道节点间隔必须缩短约三分之一',
);
assert.match(
  railSource,
  /\.turn-navigation-marker\s*\{[\s\S]*?flex:\s*0 0 2px;[\s\S]*?height:\s*2px;/,
  '轨道节点必须统一使用 2px 细线',
);
for (const selector of [
  '.turn-navigation-marker.active',
  '.turn-navigation-rail.magnetic .turn-navigation-marker.magnetic-focus',
  '.turn-navigation-marker.selected',
]) {
  assert.doesNotMatch(
    extractRuleBody(railSource, selector),
    /(?:height|box-shadow)\s*:/,
    `${selector} 不得通过高度或外描边加粗节点线条`,
  );
}
assert.doesNotMatch(
  railSource,
  /\.turn-navigation-marker\.active\s*\{[\s\S]*?width:\s*27px;/,
  '当前轮次不能因为 active 状态永久拉长',
);
assert.match(
  railSource,
  /\.turn-navigation-rail\.magnetic\s+\.turn-navigation-marker\s*\{[\s\S]*?width:\s*calc\(6px\s*\+\s*var\(--turn-wave-strength,\s*0\)\s*\*\s*14px\)/,
  '悬停波澜必须从 6px 静默态渐进伸展，峰值限制为 20px',
);
assert.match(
  railSource,
  /\.turn-navigation-marker\.selected\s*\{[\s\S]*?width:\s*20px;/,
  '点击选中节点必须在鼠标移出后保留 20px 向右凸起',
);
assert.doesNotMatch(
  railSource,
  /turn-navigation-rail::before/,
  '轨道不得绘制贯穿式竖线',
);
assert.match(
  railSource,
  /@container message-list \(min-width:\s*640px\)[\s\S]*?\.turn-navigation-capsule\s*\{[\s\S]*?display:\s*none;/,
  '普通桌面内容区即使只有 640px 也必须展示轨道，只有手机宽度才收敛为胶囊',
);
assert.match(
  messageListSource,
  /@container message-list \(min-width:\s*640px\)[\s\S]*?padding-left:\s*calc\(var\(--space-4\)\s*\+\s*8px\)/,
  '宽屏轨道右侧安全区必须再收窄 10px，让消息内容更靠左',
);
assert.match(
  messageListSource,
  /<TurnNavigationRail[\s\S]*?items=\{turnNavigationItems\}[\s\S]*?container=\{containerRef\}/,
  '轮次导航必须接入主线消息列表的 canonical turn 数据与真实滚动容器',
);
assert.match(
  messageListSource,
  /showPreTurnProcessingIndicator[\s\S]*?messagesState\.isProcessing[\s\S]*?currentRuntimeRenderItem === null/,
  '消息已提交但 canonical 占位项尚未投影时，主线必须立即显示独立等待动画',
);
assert.match(
  messageListSource,
  /activeThreadRequestId[\s\S]*?messagesState\.pendingRequests[\s\S]*?findRenderItemByRequestId\(activeThreadRequestId\)/,
  '连续轮次必须以当前 pending requestId 定位运行卡片，不能复用上一轮残留流式状态',
);
assert.match(
  messageListSource,
  /runtimeTurnIdentity[\s\S]*?turnIdentity !== stableRuntimeTurnIdentity[\s\S]*?stableStreamingStartAt = 0/,
  '计时器必须在 requestId 或 turnId 变化时立即重置，不能沿用上一轮计时状态',
);
assert.match(
  messageListSource,
  /displayContext === 'thread' && activeThreadRequestId[\s\S]*?stableStreamingStartAt = nextStartAt/,
  '当前本地 requestId 的计时起点必须跟随当前轮次状态，不能被历史事件回放锁定到旧时间',
);
assert.match(
  messageListSource,
  /runtimeLayoutSignature[\s\S]*?const _runtimeSig = runtimeLayoutSignature[\s\S]*?scrollPanelToBottom\(\)/,
  '独立等待动画插入时必须触发自动滚动，避免动画已经渲染但停留在可视区域下方',
);
assert.match(
  messageListSource,
  /return hasTaskRuntime \? \(activeRenderItems\[activeRenderItems\.length - 1\] \|\| null\) : null/,
  '主线提交等待动画不得错误挂到上一轮已完成消息上',
);
assert.match(
  railSource,
  /\.turn-navigation-capsule\s*\{[\s\S]*?right:\s*20px;[\s\S]*?bottom:\s*64px;/,
  '手机端轮次入口必须改为右下角悬浮控件，并避开回到底部按钮',
);
assert.match(
  railSource,
  /class="turn-navigation-capsule-button floating-overlay-control"/,
  '手机端轮次入口必须复用全局悬浮控件视觉基类',
);
assert.match(
  messageListSource,
  /class="scroll-to-bottom floating-overlay-control"/,
  '回到底部按钮必须复用全局悬浮控件视觉基类',
);
assert.match(
  globalStyleSource,
  /\.floating-overlay-control\s*\{[\s\S]*?opacity:\s*0\.46;[\s\S]*?background:\s*color-mix\([^;]+transparent\);[\s\S]*?border:\s*1px solid var\(--border\);[\s\S]*?backdrop-filter:\s*blur\(12px\);/,
  '悬浮控件基类必须统一维护透明度、背景、边框与模糊效果',
);
assert.match(
  globalStyleSource,
  /\.floating-overlay-control:hover,[\s\S]*?\.floating-overlay-control\[aria-expanded=['"]true['"]\][\s\S]*?opacity:\s*1;/,
  '悬浮控件基类必须统一维护悬停、聚焦与展开状态',
);
assert.ok(
  globalStyleSource.indexOf('.floating-overlay-control {') > globalStyleSource.indexOf('button {'),
  '悬浮控件基类必须位于全局按钮重置之后，避免背景和边框被覆盖',
);

for (const [componentName, source, selector] of [
  ['轮次胶囊', railSource, '.turn-navigation-capsule-button'],
  ['回到底部按钮', messageListSource, '.scroll-to-bottom'],
]) {
  const ruleBody = extractRuleBody(source, selector);
  assert.doesNotMatch(
    ruleBody,
    /(?:opacity|background|border(?:-color)?|box-shadow|backdrop-filter)\s*:/,
    `${componentName}不得重复维护共享视觉属性`,
  );
}

assert.doesNotMatch(
  railSource,
  /\.turn-navigation-capsule-button:hover,[\s\S]*?\.turn-navigation-capsule\.open \.turn-navigation-capsule-button/,
  '轮次胶囊不得单独维护共享交互状态',
);

await withGoldenViteServer(async (server) => {
  const navigation = await server.ssrLoadModule('/src/lib/turn-navigation.ts');

  const items = navigation.buildTurnNavigationItems([
    { id: 'assistant-1', turnId: 'turn-1', turnSeq: 1, turnStatus: 'running', type: 'text', content: '正在分析', timestamp: 1_000 },
    { id: 'user-1', turnId: 'turn-1', turnSeq: 1, turnStatus: 'running', type: 'user_input', content: '先分析 Windows 路径问题。', timestamp: 1_100 },
    { id: 'user-2', turnId: 'turn-2', turnSeq: 2, turnStatus: 'completed', type: 'user_input', content: '修复完成了吗？', timestamp: 2_000 },
    { id: 'assistant-2', turnId: 'turn-2', turnSeq: 2, turnStatus: 'completed', type: 'text', content: '已完成修复。', timestamp: 2_100 },
  ]);

  assert.deepEqual(items, [
    {
      turnId: 'turn-1',
      turnSeq: 1,
      index: 1,
      status: 'running',
      messageIds: ['assistant-1', 'user-1'],
      anchorMessageId: 'assistant-1',
      summary: '先分析 Windows 路径问题。',
      sentAt: 1_100,
    },
    {
      turnId: 'turn-2',
      turnSeq: 2,
      index: 2,
      status: 'completed',
      messageIds: ['user-2', 'assistant-2'],
      anchorMessageId: 'user-2',
      summary: '修复完成了吗？',
      sentAt: 2_000,
    },
  ]);

  const magnet = navigation.calculateTurnNavigationMagnet([100, 180, 260, 340], 260);
  assert.equal(magnet.focusIndex, 2, '最近轮次必须成为磁吸焦点');
  assert.equal(magnet.strengths[2], 1, '焦点轮次必须达到最大强度');
  assert.ok(magnet.strengths[1] > magnet.strengths[0], '相邻刻度应按距离渐进增强');
  assert.equal(magnet.strengths[3], magnet.strengths[1], '等距刻度应保持对称');

  const crowdedMagnet = navigation.calculateTurnNavigationMagnet([100, 110, 120], 110);
  assert.equal(crowdedMagnet.strengths[1], 1, '拥挤刻度中的焦点仍应达到最大强度');
  assert.ok(
    Math.max(crowdedMagnet.strengths[0], crowdedMagnet.strengths[2]) <= 0.68,
    '非焦点刻度必须限制最大拉伸，避免与焦点同宽',
  );

  const railLayout = navigation.calculateTurnNavigationRailLayout(10, 120);
  assert.equal(railLayout.itemSpacing, 13, '轨道刻度必须使用缩短后的固定间距');
  assert.equal(railLayout.markerHeight, 2, '轨道布局计算必须与 2px 细线一致');
  assert.equal(railLayout.contentHeight, 143, '轨道内容高度必须按紧凑间距计算');
  assert.equal(railLayout.scrollable, true, '轮次过多时轨道必须具备独立滚动所需的内容高度');

  const markerOffsets = navigation.calculateTurnNavigationMarkerOffsets(3);
  assert.deepEqual(markerOffsets, [13, 26, 39], '刻度中心必须按紧凑固定间距排列');

  assert.equal(
    navigation.calculateTurnNavigationScrollTarget(150, 2, 120, 0, 168),
    48,
    '当前轮次应滚动到轨道可视区域中央',
  );
  assert.equal(
    navigation.calculateTurnNavigationScrollTarget(13, 2, 120, 80, 188),
    0,
    '轨道滚动目标不得小于零',
  );
  assert.equal(navigation.isTurnNavigationNeighbor(2, 1), true, '当前节点的相邻节点必须被识别');
  assert.equal(navigation.isTurnNavigationNeighbor(0, 1), true, '当前节点的上方相邻节点必须被识别');
  assert.equal(navigation.isTurnNavigationNeighbor(3, 1), false, '非相邻节点不能被识别为邻居');
  assert.equal(navigation.isTurnNavigationNeighbor(0, -1), false, '失去焦点时不能保留邻居高亮');

  console.log('turn navigation golden tests passed');
}, { configFile: 'vite.web.config.ts' });
