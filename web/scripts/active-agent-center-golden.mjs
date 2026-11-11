import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { compileModule } from 'svelte/compiler';
import { transformWithOxc } from 'vite';
import { withGoldenViteServer } from './golden-vite.mjs';

function agent(overrides = {}) {
  return {
    agentRunId: 'agent-run-1',
    parentTaskId: 'task-root',
    rootTaskId: 'task-root',
    displayName: '架构代理',
    goal: '检查架构边界',
    role: 'architect',
    modelSource: 'role_engine',
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
  const { resolveAgentDisplayName } = await server.ssrLoadModule('/src/lib/agent-role-utils.ts');
  const roleTemplates = [
    { templateId: 'reviewer', displayName: '复核代理' },
    { templateId: 'executor', displayName: '执行代理' },
  ];
  for (const reviewerEngine of ['executor', 'shared-model']) {
    const bindings = [
      { templateId: 'reviewer', displayName: '复核代理', engineId: reviewerEngine, order: 0 },
      { templateId: 'executor', displayName: '执行代理', engineId: 'shared-model', order: 1 },
    ];
    const snapshot = { roleTemplates, registryAgents: bindings };
    for (const enabledAgents of [bindings, []]) {
      assert.equal(
        resolveAgentDisplayName('executor', enabledAgents, snapshot),
        '执行代理',
        '角色标签只按角色 ID 解析，不能被另一个代理绑定的同名引擎截获',
      );
      assert.equal(
        resolveAgentDisplayName('reviewer', enabledAgents, snapshot),
        '复核代理',
        '多个代理共用一个模型引擎时仍保持各自身份',
      );
      assert.equal(
        resolveAgentDisplayName('shared-model', enabledAgents, snapshot),
        'Shared-model',
        '仅有模型引擎 ID 时不能猜测为第一个绑定该引擎的代理',
      );
    }
  }

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
    ['running'],
    '运行中代理必须进入正在运行分组',
  );
  assert.deepEqual(
    grouped.queued.map((item) => item.agentRunId),
    ['pending'],
    '排队代理必须单独进入排队分组',
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

  // 异常停止后的“继续推进”是会话级动作，由 daemon 判定；运行视图不再有自己的继续入口。
  assert.equal(center.composerRunAction, undefined, '运行视图不得再提供继续入口');

  // 状态条与列表标题共用的一句话概括：按异常、排队、运行的优先级给出。
  const t = (key, vars = {}) => `${key}:${JSON.stringify(vars)}`;
  const summaryOf = (agents, outcome = null) => center.summarizeActiveAgentRun(
    center.buildActiveAgentSummary(center.groupActiveAgents(agents)), outcome, t,
  );
  assert.match(summaryOf([agent()]), /summaryRunning/, '只有运行中的代理');
  assert.match(
    summaryOf([agent(), agent({ agentRunId: 'q', status: 'pending', lifecycle: 'queued' })]),
    /summaryQueuedRunning/,
  );
  assert.match(summaryOf([agent({ status: 'failed', lifecycle: 'failed' })]), /summaryAttention/);
  assert.match(summaryOf([agent({ status: 'completed', lifecycle: 'completed' })]), /summaryCompleted/);
  assert.match(summaryOf([], 'failed'), /summaryFailed/, '整轮失败时以权威结论为先，即使没有子代理');
  assert.match(summaryOf([agent()], 'recoverable'), /summaryRecoverable/);

  assert.deepEqual(
    center.buildActiveAgentSummary(grouped),
    { queuedCount: 1, activeCount: 1, attentionCount: 2, completedCount: 2, triggerCount: 6 },
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
  assert.equal(center.formatAgentDuration(68), '1m 8s');
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

  const projection = { root_task: { task_id: 'root-new' }, agents: [agent()], outcome: 'active' };
  assert.equal(center.selectActiveAgentRunProjection(projection), projection,
    '运行中心直接读取当前 daemon 投影，保留全部代理');
  assert.equal(center.selectActiveAgentRunProjection(null), null,
    '归档、重连清空或切换 root 后不能继续展示旧运行');
  assert.equal(center.selectActiveAgentRunProjection({ ...projection, agents: [] }), null,
    '没有子代理的新主线不展示上一轮代理');
  const failed = { ...projection, agents: [], outcome: 'failed' };
  assert.equal(center.selectActiveAgentRunProjection(failed), failed,
    '失败 root 即使没有子代理也必须保留诊断与服务端动作');
  assert.equal(center.selectActiveAgentRunProjection({ ...projection, root_task: { task_id: '' } }), null,
    '缺少权威 root 身份不能展示运行');
});

// 用客户端 rune 编译执行真实 store，验证失效与导航会同步更新所有展示 getter。
await withGoldenViteServer(async (server) => {
  const { activeAgentRun } = await server.ssrLoadModule('/src/stores/active-agent-run.svelte.ts');
  const store = await server.ssrLoadModule('/src/stores/agent-run-store.svelte.ts');
  const { messagesState } = await server.ssrLoadModule('/src/stores/messages.svelte.ts');
  const { setClientBridge } = await server.ssrLoadModule('/src/shared/bridges/bridge-runtime.ts');
  const bridgeMessages = [];
  setClientBridge({ postMessage: message => bridgeMessages.push(message), onMessage: () => () => {} });
  const previousFetch = globalThis.fetch;
  let actionResponse;
  globalThis.fetch = async (url) => {
    const path = new URL(String(url)).pathname;
    if (path.endsWith('/agent-runs/action')) return actionResponse.promise;
    const rootTaskId = path.split('/').at(-1);
    return Response.json({
      root_task: { task_id: rootTaskId, status: 'failed' },
      agents: [agent({ rootTaskId, status: 'failed', lifecycle: 'failed' })],
      tasks: [{ task_id: rootTaskId, status: 'failed' }],
      pending_tasks: [], running_tasks: [], outcome: 'failed',
      availableActions: ['archive'], failureSummary: 'model failed',
    });
  };
  try {
    messagesState.currentSessionId = 'session-a';
    messagesState.currentWorkspaceId = 'workspace-a';
    messagesState.currentWorkspacePath = '/workspace-a';
    assert.equal(activeAgentRun.hasRun, false);
    await store.fetchAgentRunProjection('session-a', 'root-a', 'workspace-a');
    assert.equal(activeAgentRun.hasRun, true);
    assert.equal(activeAgentRun.summary.attentionCount, 1);
    assert.deepEqual([...activeAgentRun.availableActions], ['archive']);

    store.clearAgentRunProjection('session-a', undefined, 'workspace-a');
    assert.equal(activeAgentRun.hasRun, false, '底层清空必须立即移除旧视图，无需 effect 再同步');
    assert.equal(activeAgentRun.summary.triggerCount, 0);
    assert.deepEqual([...activeAgentRun.availableActions], []);
    assert.deepEqual([...activeAgentRun.tasks], []);

    await store.fetchAgentRunProjection('session-a', 'root-a', 'workspace-a');
    messagesState.currentWorkspaceId = 'workspace-b';
    assert.equal(activeAgentRun.hasRun, false, '同名会话在其他项目不能继承当前投影');
    messagesState.currentWorkspaceId = 'workspace-a';
    assert.equal(activeAgentRun.hasRun, true);

    actionResponse = Promise.withResolvers();
    const archiving = activeAgentRun.runAction('archive');
    assert.equal(activeAgentRun.hasRun, true, '等待服务端回执时不得提前隐藏运行');
    messagesState.currentSessionId = 'session-b';
    assert.equal(activeAgentRun.hasRun, false, '切换会话不能显示旧会话的列表与动作');
    await store.fetchAgentRunProjection('session-b', 'root-b', 'workspace-a');
    actionResponse.reject(new Error('archive rejected'));
    await archiving;
    assert.equal(activeAgentRun.hasRun, true);
    assert.equal(activeAgentRun.tasks[0].task_id, 'root-b');
    assert.equal(activeAgentRun.actionError, null);
    const incident = bridgeMessages.find(message => message.type === 'reportIncident').incident;
    assert.equal(incident.sessionId, 'session-a', '迟到的归档失败必须记录在发起会话');
    assert.equal(incident.workspaceId, 'workspace-a');

    actionResponse = Promise.withResolvers();
    const archived = activeAgentRun.runAction('archive');
    actionResponse.resolve(Response.json({ action: 'archive', archived: true, sessionId: 'session-b', rootTaskId: 'root-b' }));
    await archived;
    assert.equal(activeAgentRun.hasRun, false, '归档成功后直接由权威缓存清空驱动视图');
    assert.deepEqual([...activeAgentRun.availableActions], []);
    messagesState.currentSessionId = 'session-a';
    assert.equal(activeAgentRun.hasRun, true, '失败的归档保留原会话运行供重试');
    assert.equal(activeAgentRun.actionError, 'archive rejected');
  } finally {
    store.clearAgentRunProjection();
    globalThis.fetch = previousFetch;
  }
}, {
  plugins: [{
    name: 'active-agent-client-runes',
    enforce: 'pre',
    async transform(source, id) {
      if (!id.endsWith('.svelte.ts')) return;
      const { code } = await transformWithOxc(source, id, { lang: 'ts' });
      return compileModule(code, { filename: id, generate: 'client', dev: true }).js;
    },
  }],
});

const read = (path) => readFile(new URL(path, import.meta.url), 'utf8');
const threadPanelSource = await read('../src/components/ThreadPanel.svelte');
const headerSource = await read('../src/components/Header.svelte');
const chipSource = await read('../src/components/AgentStatusChip.svelte');
const bannerSource = await read('../src/components/AgentRunBanner.svelte');
const rowSource = await read('../src/components/AgentRow.svelte');
const listSource = await read('../src/components/tabs/AgentListTabContent.svelte');
const runStoreSource = await read('../src/stores/active-agent-run.svelte.ts');
const goalRunDrawersSource = await read('../src/components/GoalRunDrawers.svelte');
const daemonClientSource = await read('../src/shared/rust-daemon-client.ts');
const agentTabContentSource = await read('../src/components/tabs/AgentTabContent.svelte');
const messageListSource = await read('../src/components/MessageList.svelte');

// 入口形态：状态条常驻顶栏、恢复横幅在消息下方，对话流式布局里不再有会让内容跳动的浮层入口。
assert.doesNotMatch(threadPanelSource, /ActiveAgentCenter/, '旧的悬浮代理中心已移除');
assert.match(
  threadPanelSource,
  /<ConversationApprovalTray[\s\S]*?\/>\s*<AgentRunBanner\s*\/>/,
  '恢复横幅与工具审批同位：消息列表下方、输入区上方，不遮挡消息',
);
assert.match(headerSource, /<div class="header-actions">\s*<AgentStatusChip\s*\/>/, '代理状态条常驻在顶栏右侧');
assert.match(chipSource, /activeAgentRun\.openList\(\)/, '点击状态条打开右栏的代理列表，而不是遮住消息的浮层');
assert.doesNotMatch(chipSource, /position:\s*(absolute|fixed)[\s\S]*agent-center-panel/, '状态条不再带弹层');
assert.match(chipSource, /aria-pressed=\{open\}/, '状态条要体现代理列表是否已打开');
assert.match(listSource, /activeAgentRun\.groups/, '列表只读共用的运行投影');
assert.match(listSource, /retainUiClock\(\)/, '进行中的耗时必须注册统一 UI 时钟');
assert.match(runStoreSource, /openAgentTab\(/, '点击代理必须复用右侧面板的增量代理 Tab');
assert.match(runStoreSource, /selectAgentRun\(/, '打开代理前先选中这次运行，与右栏保持一致');

assert.match(
  agentTabContentSource,
  /runtimeCompletedAt=\{agentRuntimeCompletedAt\}[\s\S]*runtimeDurationMs=\{agentRuntimeDurationMs\}/,
  '代理详情必须把完成时刻与总耗时传给统一消息时间线',
);
assert.match(agentTabContentSource, /uiClockState\.now/, '代理详情中的运行耗时必须读取统一 UI 时钟');
assert.match(agentTabContentSource, /retainUiClock\(\)/, '代理详情中的运行耗时必须注册统一 UI 时钟消费者');
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
assert.doesNotMatch(
  runStoreSource,
  /localStorage|pinnedProjection|dismissedRootTaskId|\$state|\$effect/,
  '运行视图不能保存第二份投影或归档事实；清除和切换直接随底层投影生效',
);
assert.match(
  runStoreSource,
  /performAgentRunAction\(/,
  '失败态操作必须统一提交任务动作',
);
assert.match(
  bannerSource,
  /availableActions\.includes\('archive'\)[\s\S]*runAction\('archive'\)/,
  '横幅的操作由后端 availableActions 驱动，只保留归档',
);
assert.doesNotMatch(
  `${bannerSource}${runStoreSource}${rowSource}`,
  /restartConfirm|requestAction|confirmRestart|action\.restart|'restart'/,
  '不再提供重新执行：需要时直接发新消息',
);
const agentRunStoreSource = await read('../src/stores/agent-run-store.svelte.ts');
assert.doesNotMatch(
  `${runStoreSource}${agentRunStoreSource}`,
  /new_attempt|startedNewAttempt|newRootTaskId|composerAction|composerRunAction/,
  '运行视图只剩归档：继续与新的执行尝试都由 daemon 在会话级统一决定',
);
assert.match(
  runStoreSource,
  /reportIncident\(message,[\s\S]*failureStage: `agent_run_\$\{action\}`/,
  '失败操作必须把真实后端错误写入统一通知中心，不能只显示泛化提示',
);
assert.match(
  rowSource,
  /formatAgentDuration\(agentDurationSeconds\(/,
  '代理行必须展示运行中或已固定的实际耗时',
);
assert.match(
  rowSource,
  /class="agent-goal"[\s\S]*agent-diagnostic/,
  '代理行是两行主信息：名称与状态一行，目标一行；诊断只在真的发生时多一行',
);
const inputAreaSource = await read('../src/components/InputArea.svelte');
assert.match(
  inputAreaSource,
  /const canContinue = \$derived\(activeAgentRun\.continuation !== null\)/,
  '输入框的继续按钮只由 daemon 下发的 continuation 决定，Web 不自行推断',
);
assert.match(
  inputAreaSource,
  /type: 'continueTask'/,
  '所有停止的继续都走同一个会话级动作',
);
assert.doesNotMatch(
  inputAreaSource,
  /composerCanContinue|runContinueAction|activeAgentRun\.runAction\('continue'\)|canResume/,
  '输入框不得保留第二套继续判定或第二条继续路径',
);
assert.doesNotMatch(inputAreaSource, /continueUsesRestart/, '输入框的继续按钮不再有重新执行形态');
assert.match(
  bannerSource,
  /const canContinue = \$derived\(activeAgentRun\.continuation !== null\)/,
  '横幅知道继续由输入框承担，只给出提示，不重复按钮',
);
assert.doesNotMatch(rowSource, /agent-model-line|<span class="agent-goal" title=\{model\}/, '模型不再平铺成独立一行');
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
assert.match(
  daemonClientSource,
  /public async performAgentRunAction\(/,
  '任务恢复动作必须收敛为单一客户端入口',
);
assert.match(
  goalRunDrawersSource,
  /allowedGoalActions\?\.canResume === true/,
  '目标恢复权限必须直接使用后端权威 allowedActions',
);
assert.doesNotMatch(
  goalRunDrawersSource,
  /goalCanResume[\s\S]{0,300}currentPlanPaused|goalCanResume[\s\S]{0,300}currentPlanBlocked/,
  '前端不能再根据计划展示状态推导目标恢复权限',
);

await withGoldenViteServer(async (server) => {
  const center = await server.ssrLoadModule('/src/lib/active-agent-center.ts');
  const picked = center.selectLauncherAgents([
    agent({ agentRunId: 'done', status: 'completed', lifecycle: 'completed', startedAt: 500 }),
    agent({ agentRunId: 'queued-new', status: 'pending', lifecycle: 'queued', startedAt: 400 }),
    agent({ agentRunId: 'running-old', startedAt: 100 }),
    agent({ agentRunId: 'failed', status: 'failed', lifecycle: 'failed', startedAt: 600 }),
    agent({ agentRunId: 'running-new', startedAt: 300 }),
    agent({ agentRunId: 'queued-old', status: 'pending', lifecycle: 'queued', startedAt: 50 }),
  ]).map((item) => item.agentRunId);
  assert.deepEqual(
    picked,
    ['running-new', 'running-old', 'queued-new', 'queued-old'],
    '新标签页只列执行中和排队中的代理，执行中在前，同组较新的在前；已完成和失败的不重复列出',
  );
});

console.log('active agent center golden tests passed');
