import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { withGoldenViteServer } from './golden-vite.mjs';

function agent(overrides = {}) {
  return {
    agentRunId: 'agent-run-1',
    parentTaskId: 'task-root',
    rootTaskId: 'task-root',
    displayName: '架构代理',
    goal: '检查架构边界',
    role: 'architect',
    modelSource: 'engine',
    status: 'running',
    statusLabel: '执行中',
    lifecycle: 'running',
    accessMode: 'full_access',
    startedAt: 100,
    updatedAt: 200,
    ...overrides,
  };
}

await withGoldenViteServer(async (server) => {
  const center = await server.ssrLoadModule('/src/lib/active-agent-center.ts');

  const grouped = center.groupActiveAgents([
    agent({ agentRunId: 'running', status: 'running', lifecycle: 'running' }),
    agent({ agentRunId: 'pending', status: 'pending', lifecycle: 'queued' }),
    agent({ agentRunId: 'failed', status: 'failed', lifecycle: 'failed' }),
    agent({ agentRunId: 'degraded', status: 'completed', lifecycle: 'degraded' }),
    agent({ agentRunId: 'completed', status: 'completed', lifecycle: 'completed' }),
    agent({ agentRunId: 'killed', status: 'killed', lifecycle: 'killed' }),
  ]);

  assert.deepEqual(
    grouped.running.map((item) => item.agentRunId),
    ['running', 'pending'],
    '运行中与排队代理必须进入正在运行分组',
  );
  assert.deepEqual(
    grouped.attention.map((item) => item.agentRunId),
    ['failed', 'degraded'],
    '失败与降级代理必须进入需要关注分组',
  );
  assert.deepEqual(
    grouped.completed.map((item) => item.agentRunId),
    ['completed', 'killed'],
    '正常完成与主动终止代理必须进入本轮已完成分组',
  );

  assert.equal(
    center.shouldShowActiveAgentCenter(grouped),
    true,
    '存在运行中或异常代理时必须显示活跃代理中心',
  );

  const settled = center.groupActiveAgents([
    agent({ agentRunId: 'completed', status: 'completed', lifecycle: 'completed' }),
  ]);
  assert.equal(
    center.shouldShowActiveAgentCenter(settled),
    true,
    '代理全部完成但主线仍在汇总时必须继续显示',
  );
  assert.equal(
    center.shouldShowActiveAgentCenter(settled),
    true,
    '代理完成后入口必须持续显示，直到用户清空或新一轮代理替换',
  );

  const failed = center.groupActiveAgents([
    agent({ agentRunId: 'failed', status: 'failed', lifecycle: 'failed' }),
  ]);
  assert.equal(
    center.shouldShowActiveAgentCenter(failed),
    true,
    '主线结束后异常代理仍必须持续显示',
  );

  assert.deepEqual(
    center.buildActiveAgentSummary(grouped),
    { activeCount: 2, attentionCount: 2, completedCount: 2, triggerCount: 6 },
    '入口数量必须统计当前固定列表中的全部代理',
  );

  assert.equal(
    center.agentDurationSeconds(agent({ startedAt: 1_000, updatedAt: 9_900 }), 20_000),
    19,
    '运行中代理必须使用当前时间持续累计耗时',
  );
  assert.equal(
    center.agentDurationSeconds(agent({
      status: 'completed',
      lifecycle: 'completed',
      startedAt: 1_000,
      updatedAt: 9_900,
      completedAt: 9_900,
      responseDurationMs: 8_900,
    }), 20_000),
    8,
    '终态代理必须固定使用后端投影的完成时间与总耗时',
  );
  assert.equal(center.formatAgentDuration(8), '8s');
  assert.equal(center.formatAgentDuration(68), '1m 08s');
  assert.equal(center.formatAgentDuration(3_728), '1h 02m');

  assert.deepEqual(
    center.agentRuntimeTiming(agent({
      status: 'completed',
      lifecycle: 'completed',
      startedAt: 1_000,
      updatedAt: 9_900,
      completedAt: 9_900,
      responseDurationMs: 8_900,
    }), 20_000),
    { active: false, startedAt: 1_000, completedAt: 9_900, durationMs: 8_900 },
    '终态代理必须提供完成时刻与毫秒级总耗时，供右侧详情与主线复用同一展示组件',
  );

  assert.equal(
    center.shouldPinAgentProjection('root-new', 2, 'root-old'),
    true,
    '不同 rootTaskId 的新代理组必须替换上一轮固定列表',
  );
  assert.equal(
    center.shouldPinAgentProjection('root-cleared', 2, 'root-cleared'),
    false,
    '用户已清空的同一轮代理不能被轮询重新打开',
  );
  assert.equal(
    center.shouldPinAgentProjection('root-empty', 0, ''),
    false,
    '没有实际子代理的普通任务不能刷新固定列表',
  );
});

const threadPanelSource = await readFile(
  new URL('../src/components/ThreadPanel.svelte', import.meta.url),
  'utf8',
);
const activeAgentCenterSource = await readFile(
  new URL('../src/components/ActiveAgentCenter.svelte', import.meta.url),
  'utf8',
);
const goalRunDrawersSource = await readFile(
  new URL('../src/components/GoalRunDrawers.svelte', import.meta.url),
  'utf8',
);
const daemonClientSource = await readFile(
  new URL('../src/shared/rust-daemon-client.ts', import.meta.url),
  'utf8',
);

assert.match(
  threadPanelSource,
  /<ActiveAgentCenter\s*\/>/,
  '活跃代理中心必须挂载在主对话区域，而不是输入框上方抽屉',
);
assert.match(
  activeAgentCenterSource,
  /openAgentTab\(/,
  '点击代理必须复用右侧面板的增量代理 Tab',
);

const agentTabContentSource = await readFile(
  new URL('../src/components/tabs/AgentTabContent.svelte', import.meta.url),
  'utf8',
);
const messageListSource = await readFile(
  new URL('../src/components/MessageList.svelte', import.meta.url),
  'utf8',
);
assert.match(
  agentTabContentSource,
  /runtimeCompletedAt=\{agentRuntimeCompletedAt\}[\s\S]*runtimeDurationMs=\{agentRuntimeDurationMs\}/,
  '代理详情必须把完成时刻与总耗时传给统一消息时间线',
);
assert.match(
  messageListSource,
  /displayContext === 'task'[\s\S]*normalizedRuntimeStartedAt[\s\S]*hasRuntimeToTrack[\s\S]*runtimeActive/,
  '任务详情即使尚未产出消息，也必须从任务开始时刻持续显示计时器',
);
assert.match(
  messageListSource,
  /shouldShowRuntimeSummary[\s\S]*normalizedRuntimeCompletedAt[\s\S]*normalizedRuntimeDurationMs[\s\S]*TurnRuntimeSummary/,
  '任务详情必须在终态渲染与主线一致的总耗时和结束时间',
);
assert.match(
  activeAgentCenterSource,
  /localStorage/,
  '代理中心必须按会话持久化固定 rootTaskId 与清空标记',
);
assert.doesNotMatch(
  activeAgentCenterSource,
  /expanded\s*=\s*!mobile/,
  '恢复已有代理任务时，悬浮面板必须保持收起，只保留入口按钮供用户主动展开',
);
assert.match(
  activeAgentCenterSource,
  /activeAgentCenter\.clearAndClose/,
  '展开面板必须提供清空并关闭操作',
);
assert.match(
  activeAgentCenterSource,
  /formatAgentDuration\(agentDurationSeconds\(/,
  '代理行必须展示运行中或已固定的实际耗时',
);
assert.match(
  activeAgentCenterSource,
  /@media \(max-width:\s*768px\)[\s\S]*?\.agent-center-panel\s*\{[\s\S]*?position:\s*fixed;[\s\S]*?bottom:\s*8px;/,
  '手机端必须使用底部抽屉而不是右上角悬浮层',
);
assert.doesNotMatch(
  goalRunDrawersSource,
  /agent-run-panel|agentRunDrawerExpanded|stopCurrentAgentRun|restartCurrentAgentRun/,
  '目标任务抽屉必须删除重复的代理运行面板与废弃控制逻辑',
);
assert.doesNotMatch(
  daemonClientSource,
  /public async (?:continueSession|restartAgentRun|archiveAgentRun)\(/,
  '删除旧代理抽屉后必须同步清理只为该抽屉服务的客户端方法',
);

console.log('active agent center golden tests passed');
