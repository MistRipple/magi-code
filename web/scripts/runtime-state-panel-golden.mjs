import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { withGoldenViteServer } from './golden-vite.mjs';

const runtimePanelSource = await readFile(
  new URL('../src/components/RuntimeStatePanel.svelte', import.meta.url),
  'utf8',
);

assert.doesNotMatch(
  runtimePanelSource,
  /runtimeDiagnostics\.technicalDetails|runtimeDiagnostics\.timelineTitle|technicalDetailsExpanded/,
  '运行态主面板不得继续嵌套技术详情和时间线折叠层',
);
assert.match(
  runtimePanelSource,
  /let expandedRecordIds = \$state<Set<string>>\(new Set\(\)\)/,
  '运行记录必须拥有独立、默认折叠的用户控制状态',
);
assert.match(
  runtimePanelSource,
  /aria-expanded=\{hasDetail \? recordExpanded : undefined\}[\s\S]*?<Disclosure expanded=\{recordExpanded\}>[\s\S]*?runtime-diagnostics__record-detail/,
  '运行记录详情只能在用户逐条展开后显示',
);
assert.doesNotMatch(
  runtimePanelSource,
  /\{#if item\.detail\}[\s\S]*?<pre class="runtime-diagnostics__record-detail">/,
  '运行态面板不得默认摊开全部工具详情',
);
assert.doesNotMatch(
  runtimePanelSource,
  /canonicalFailureActive|conversationRecords\.some\(\(item\) => item\.kind === 'error'\)/,
  '单条工具错误不得覆盖权威运行终态',
);
assert.doesNotMatch(
  runtimePanelSource,
  /runtimeDiagnostics\.failureTitle|failureDetails|runtime-diagnostics__block--failure|runtime-diagnostics__failure-entry/,
  '失败原因不得在关键运行记录之外重复展示，也不得为错误正文增加内嵌强调层',
);
assert.match(
  runtimePanelSource,
  /<div class="runtime-diagnostics-slot" class:runtime-diagnostics-slot--visible=\{panelVisible\}>\s*\{#if panelVisible\}[\s\S]*?<\/div>/,
  '运行态面板必须在稳定容器内按可见状态渲染',
);
assert.match(
  runtimePanelSource,
  /\.runtime-diagnostics-slot\s*\{[\s\S]*?position:\s*absolute;[\s\S]*?height:\s*0;[\s\S]*?pointer-events:\s*none;/,
  '运行态面板隐藏时不得占用主内容布局空间',
);
assert.match(
  runtimePanelSource,
  /\.runtime-diagnostics-slot--visible\s*\{[\s\S]*?position:\s*relative;[\s\S]*?flex:\s*0 0 42px;[\s\S]*?height:\s*42px;/,
  '运行态面板可见时必须在主内容顶部预留卡片高度，避免覆盖首条消息',
);

await withGoldenViteServer(async (server) => {
  const panel = await server.ssrLoadModule('/src/lib/runtime-state-panel.ts');
  const conversationRecords = await server.ssrLoadModule('/src/lib/conversation-runtime-records.ts');
  const runtimeTimeline = await server.ssrLoadModule('/src/lib/runtime-timeline.ts');
  const rustContract = await server.ssrLoadModule('/src/shared/bridges/rust-daemon-contract.ts');

  for (const [status, visible] of [[undefined, false], ['idle', false], ['running', true],
    ['blocked', true], ['failed', true], ['completed', false], ['cancelled', false]]) {
    assert.equal(panel.shouldShowRuntimePanel(status), visible);
  }

  const { render } = await server.ssrLoadModule('svelte/server');
  const { default: RuntimePanel } = await server.ssrLoadModule('/src/components/RuntimeStatePanel.svelte');
  for (const status of ['idle', 'running', 'blocked', 'failed', 'completed', 'cancelled']) {
    const runtimeState = { status, phase: status, statusChangedAt: 100, lastEventAt: 100,
      assignments: [{ status: 'running', workerId: 'independent-agent' }] };
    const html = render(RuntimePanel, { props: {
      runtimeState,
      // 旧调用参数和仍在执行的代理都不能改写主线的权威终态。
      isProcessing: true, processingStartedAt: 200, turnStage: 'streaming',
    } }).body;
    if (['running', 'blocked', 'failed'].includes(status)) {
      assert.ok(html.includes(`summary__badge--${status}`), '状态徽章必须保留主线事实');
    } else {
      assert.ok(!html.includes('runtime-diagnostics__summary-button'), '非活跃主线不能被本地忙碌或子代理重新打开');
    }
  }

  assert.equal(panel.shouldShowRuntimePhase('running', 'running'), false);
  assert.equal(panel.shouldShowRuntimePhase('idle', 'idle'), false);
  assert.equal(panel.shouldShowRuntimePhase('running', 'verify'), true);

  const runningRecords = conversationRecords.buildConversationRuntimeRecords([{
    key: 'latest-tool',
    message: {
      id: 'message-latest-tool',
      role: 'assistant',
      source: 'orchestrator',
      content: 'shell_exec',
      timestamp: 1_700_000_000_200,
      updatedAt: 1_700_000_000_300,
      isStreaming: false,
      isComplete: false,
      type: 'tool_call',
      blocks: [{
        id: 'tool-latest',
        type: 'tool_call',
        content: '',
        toolCall: {
          id: 'call-latest',
          name: 'shell_exec',
          arguments: { command: 'sleep 10; printf done' },
          status: 'running',
        },
      }],
      metadata: { turnSeq: 2, canonicalItemSeq: 3 },
    },
  }, {
    key: 'historical-error',
    message: {
      id: 'message-historical-error',
      role: 'assistant',
      source: 'orchestrator',
      content: 'old error',
      timestamp: 1_700_000_000_100,
      isStreaming: false,
      isComplete: true,
      type: 'error',
      noticeType: 'error',
      metadata: { turnSeq: 1, canonicalItemSeq: 2 },
    },
  }], { isProcessing: true, processingStartedAt: 1_700_000_000_150 });
  assert.deepEqual(
    runningRecords.map((entry) => ({ type: entry.type, detail: entry.detail })),
    [{ type: 'session.tool.running', detail: 'sleep 10; printf done' }],
    '运行态只投影当前轮次，并直接展示正在执行的真实命令',
  );

  const failedRecords = conversationRecords.buildConversationRuntimeRecords([{
    key: 'failed-tool',
    message: {
      id: 'message-failed-tool',
      role: 'assistant',
      source: 'orchestrator',
      content: 'shell_exec',
      timestamp: 1_700_000_000_400,
      isStreaming: false,
      isComplete: true,
      type: 'tool_call',
      blocks: [{
        id: 'tool-failed',
        type: 'tool_call',
        content: '',
        toolCall: {
          id: 'call-failed',
          name: 'shell_exec',
          arguments: { command: 'missing-command' },
          status: 'error',
          error: JSON.stringify({
            error_code: 'shell_exec_command_not_found',
            error: '当前执行环境找不到命令：missing-command',
            summary: '命令依赖不可用',
            status: 'failed',
          }),
        },
      }],
      metadata: { turnSeq: 3, canonicalItemSeq: 4 },
    },
  }], { isProcessing: false });
  assert.equal(
    failedRecords[0]?.detail,
    'shell_exec_command_not_found: 当前执行环境找不到命令：missing-command',
    '工具失败记录必须从结构化载荷提取错误代码与真实核心原因',
  );
  assert.equal(
    failedRecords[0]?.summary,
    '当前执行环境找不到命令：missing-command',
    '折叠标题必须保留真实核心错误，完整诊断留给用户按需展开',
  );

  const succeededRecords = conversationRecords.buildConversationRuntimeRecords([{
    key: 'succeeded-tool',
    message: {
      id: 'message-succeeded-tool',
      role: 'assistant',
      source: 'orchestrator',
      content: 'file_read',
      timestamp: 1_700_000_000_450,
      isStreaming: false,
      isComplete: true,
      type: 'tool_call',
      blocks: [{
        id: 'tool-succeeded',
        type: 'tool_call',
        content: '',
        toolCall: {
          id: 'call-succeeded',
          name: 'file_read',
          arguments: { path: '/workspace/src' },
          status: 'success',
          result: JSON.stringify({
            status: 'succeeded',
            summary: '目录 /workspace/src 包含 4 项',
            entries: ['app.ts', 'lib.ts', 'utils.ts', 'views'],
          }),
        },
      }],
      metadata: { turnSeq: 4, canonicalItemSeq: 5 },
    },
  }], { isProcessing: false });
  assert.equal(
    succeededRecords[0]?.summary,
    '目录 /workspace/src 包含 4 项',
    '成功工具必须提取产品摘要，不能把原始 JSON 当作默认展示内容',
  );

  const processingRecords = conversationRecords.buildConversationRuntimeRecords([], {
    isProcessing: true,
    processingStartedAt: 1_700_000_000_500,
  });
  assert.equal(processingRecords[0]?.type, 'session.turn.processing');
  assert.equal(processingRecords[0]?.timestamp, 1_700_000_000_500);

  assert.deepEqual(
    runtimeTimeline.mergeRuntimeTimelineEntries([{
      eventId: 'older-error',
      seq: 1,
      timestamp: 100,
      type: 'task.failed',
      summary: '失败',
      kind: 'error',
      detail: 'runtime_timeout: provider timeout',
      diffCount: 0,
    }], [{
      eventId: 'newer-duplicate',
      seq: 2,
      timestamp: 200,
      type: 'session.model.failed',
      summary: '模型失败',
      kind: 'error',
      detail: 'provider timeout',
      diffCount: 0,
    }, {
      eventId: 'newest-progress',
      seq: 3,
      timestamp: 300,
      type: 'session.turn.processing',
      summary: '',
      kind: 'progress',
      diffCount: 0,
    }]).map((entry) => entry.eventId),
    ['newest-progress', 'newer-duplicate'],
    '运行记录必须最新优先，并按真实错误正文跨来源去重',
  );
  assert.deepEqual(
    runtimeTimeline.mergeCurrentRuntimeTimelineEntries({
      runtimeEntries: [{
        eventId: 'previous-turn-error',
        seq: 4,
        timestamp: 400,
        type: 'task.failed',
        summary: '上一轮失败',
        kind: 'error',
        detail: 'previous turn failed',
        diffCount: 0,
      }, {
        eventId: 'current-turn-dispatched',
        seq: 5,
        timestamp: 510,
        type: 'task.dispatched',
        summary: '本轮已派发',
        kind: 'progress',
        diffCount: 0,
      }],
      conversationEntries: [{
        ...processingRecords[0],
        timestamp: 505,
      }],
      currentTurnStartedAt: 500,
    }).map((entry) => entry.eventId),
    ['current-turn-processing'],
    '主会话已有当前轮次记录时，不得混入历史事件或内部任务推进噪音',
  );

  const cancelledRecords = conversationRecords.buildConversationRuntimeRecords([{
    key: 'cancelled-tool',
    message: {
      id: 'message-cancelled-tool',
      role: 'assistant',
      source: 'orchestrator',
      content: 'shell_exec',
      timestamp: 600,
      isStreaming: false,
      isComplete: true,
      type: 'tool_call',
      blocks: [{
        id: 'tool-cancelled',
        type: 'tool_call',
        content: '',
        toolCall: {
          id: 'call-cancelled',
          name: 'shell_exec',
          arguments: { command: 'sleep 30' },
          status: 'error',
        },
      }],
      metadata: {
        turnSeq: 4,
        turnItemKind: 'tool_call',
        turnItemStatus: 'cancelled',
        toolName: 'shell_exec',
        toolCallId: 'call-cancelled',
      },
    },
  }], { isProcessing: false });
  assert.deepEqual(
    cancelledRecords,
    [],
    '用户中断造成且没有诊断正文的工具取消不得伪装为工具失败',
  );
  const interruptedRecords = conversationRecords.buildConversationRuntimeRecords([{
    key: 'interrupted-user-message',
    message: {
      id: 'interrupted-user-message',
      role: 'user',
      source: 'user',
      content: '执行长任务',
      timestamp: 800,
      updatedAt: 850,
      isStreaming: false,
      isComplete: true,
      type: 'user_input',
      metadata: {
        turnSeq: 6,
        turnStatus: 'cancelled',
        interruptionSource: 'user',
      },
    },
  }], { isProcessing: false });
  assert.deepEqual(
    interruptedRecords.map((entry) => ({ type: entry.type, kind: entry.kind, detail: entry.detail })),
    [{
      type: 'session.turn.interrupted',
      kind: 'warning',
      detail: 'interruptionSource: user',
    }],
    '用户主动停止应保留为可继续的中断记录，不得标记为工具失败',
  );
  assert.deepEqual(
    runtimeTimeline.mergeCurrentRuntimeTimelineEntries({
      runtimeEntries: [{
        eventId: 'runtime-interrupted',
        seq: 9,
        timestamp: 850,
        type: 'session.turn.interrupted',
        summary: '本轮执行异常中断',
        kind: 'error',
        diffCount: 0,
      }],
      conversationEntries: interruptedRecords,
      currentTurnStartedAt: 800,
    }).map((entry) => entry.eventId),
    ['interrupted-user-message:interrupted'],
    '同一轮会话中断只能保留当前会话事实，不能再叠加后端事件副本',
  );

  const failureDetail = 'provider timeout: request req-20260727-001 returned HTTP 503';
  const toolErrorDetail = 'cargo check failed: unresolved import crate::runtime';
  const normalized = rustContract.normalizeRustBootstrapPayload({
    generatedAt: 1_700_000_000_000,
    currentSession: {
      sessionId: 'session-runtime-failed',
      workspaceId: 'workspace-runtime',
      title: '失败场景',
      createdAt: 1_700_000_000_000,
      updatedAt: 1_700_000_000_100,
    },
    sessions: [{
      sessionId: 'session-runtime-failed',
      workspaceId: 'workspace-runtime',
      title: '失败场景',
      createdAt: 1_700_000_000_000,
      updatedAt: 1_700_000_000_100,
    }],
    workspaces: [{
      workspaceId: 'workspace-runtime',
      rootPath: '/workspace/runtime',
    }],
    runtimeReadModel: {
      details: {
        sessions: [{
          session_id: 'session-runtime-failed',
          current_status: 'failed',
          root_task_id: 'task-runtime-failed',
          root_task_status: 'failed',
          mission_id: 'mission-runtime-failed',
          last_update: 1_700_000_000_100,
        }],
        tasks: [{
          task_id: 'task-runtime-failed',
          mission_id: 'mission-runtime-failed',
          current_status: 'failed',
          failure_detail: failureDetail,
        }],
      },
    },
    canonicalTurns: [{
      sessionId: 'session-runtime-failed', turnId: 'turn-runtime-failed', turnSeq: 1,
      acceptedAt: 1_700_000_000_000, completedAt: 1_700_000_000_100, status: 'failed', items: [],
    }],
    recentEvents: [{
      event_id: 'event-runtime-failed',
      event_type: 'task.status.changed',
      occurred_at: 1_700_000_000_100,
      sequence: 1,
      session_id: 'session-runtime-failed',
      task_id: 'task-runtime-failed',
      payload: {
        new_status: 'Failed',
        failure_detail: failureDetail,
      },
    }, {
      event_id: 'event-tool-runtime-failed',
      event_type: 'tool.call.finished',
      occurred_at: 1_700_000_000_105,
      sequence: 2,
      session_id: 'session-runtime-failed',
      task_id: 'task-runtime-failed',
      payload: {
        tool_name: 'cargo check',
        lifecycle: {
          status: 'failed',
          result_preview: JSON.stringify({
            error_code: 'cargo_check_failed',
            error: toolErrorDetail,
          }),
        },
      },
    }, {
      event_id: 'event-knowledge-not-needed',
      event_type: 'knowledge.context.selected',
      occurred_at: 1_700_000_000_110,
      sequence: 3,
      session_id: 'session-runtime-failed',
      payload: {
        consumer: 'mainline',
        decision: 'not_needed',
      },
    }, {
      event_id: 'event-internal-turn-item',
      event_type: 'session.turn.item',
      occurred_at: 1_700_000_000_120,
      sequence: 4,
      session_id: 'session-runtime-failed',
      payload: {},
    }],
  }, {
    workspaceId: 'workspace-runtime',
    workspacePath: '/workspace/runtime',
    sessionId: 'session-runtime-failed',
  });
  assert.equal(normalized.orchestratorRuntimeState?.status, 'failed');
  assert.equal(normalized.orchestratorRuntimeState?.phase, 'failed');
  assert.deepEqual(
    normalized.orchestratorRuntimeState?.opsView?.recentTimeline.map((entry) => entry.type),
    ['tool.call.finished', 'task.status.changed'],
    '关键运行记录必须过滤内部噪音并按最新事件优先排列',
  );
  assert.equal(normalized.orchestratorRuntimeState?.opsView?.recentTimeline[0]?.kind, 'error');
  assert.equal(normalized.orchestratorRuntimeState?.opsView?.recentTimeline[0]?.source, 'cargo check');
  assert.equal(
    normalized.orchestratorRuntimeState?.opsView?.recentTimeline[0]?.detail,
    `cargo_check_failed: ${toolErrorDetail}`,
  );
  assert.equal(
    normalized.orchestratorRuntimeState?.opsView?.recentTimeline
      .filter((entry) => entry.detail === failureDetail).length,
    1,
    '任务快照与运行事件中的同一错误只能展示一次',
  );
  assert.equal(normalized.orchestratorRuntimeState?.opsView?.failureRootCause, undefined);

  const interrupted = rustContract.normalizeRustBootstrapPayload({
    generatedAt: 1_700_000_001_000,
    currentSession: {
      sessionId: 'session-runtime-interrupted',
      workspaceId: 'workspace-runtime',
      title: '中断场景',
      createdAt: 1_700_000_000_000,
      updatedAt: 1_700_000_001_000,
    },
    sessions: [{
      sessionId: 'session-runtime-interrupted',
      workspaceId: 'workspace-runtime',
      title: '中断场景',
      createdAt: 1_700_000_000_000,
      updatedAt: 1_700_000_001_000,
    }],
    workspaces: [{
      workspaceId: 'workspace-runtime',
      rootPath: '/workspace/runtime',
    }],
    runtimeReadModel: {
      details: {
        sessions: [{
          session_id: 'session-runtime-interrupted',
          current_status: 'bound',
          root_task_id: 'task-runtime-interrupted',
          root_task_status: 'failed',
          mission_id: 'mission-runtime-interrupted',
          last_update: 1_700_000_001_000,
        }],
        tasks: [{
          task_id: 'task-runtime-interrupted',
          mission_id: 'mission-runtime-interrupted',
          current_status: 'failed',
        }],
      },
    },
    canonicalTurns: [{
      sessionId: 'session-runtime-interrupted',
      turnId: 'turn-runtime-interrupted',
      turnSeq: 1,
      acceptedAt: 1_700_000_000_000,
      completedAt: 1_700_000_001_000,
      status: 'interrupted',
      items: [{
        sessionId: 'session-runtime-interrupted',
        turnId: 'turn-runtime-interrupted',
        turnSeq: 1,
        itemId: 'item-runtime-interrupted',
        itemSeq: 1,
        kind: 'system_notice',
        createdAt: 1_700_000_000_000,
        updatedAt: 1_700_000_001_000,
        status: 'completed',
        content: '当前对话发生异常中断，是否继续？',
        sourceThreadId: 'thread-runtime-interrupted',
        visibility: { renderable: true },
        metadata: {
          noticeKind: 'session_interrupted',
          interruptionSource: 'daemon_restart',
        },
      }],
    }],
    recentEvents: [],
  }, {
    workspaceId: 'workspace-runtime',
    workspacePath: '/workspace/runtime',
    sessionId: 'session-runtime-interrupted',
  });
  assert.equal(interrupted.orchestratorRuntimeState?.status, 'failed');
  assert.deepEqual(
    interrupted.orchestratorRuntimeState?.opsView?.recentTimeline.map((entry) => ({
      type: entry.type,
      detail: entry.detail,
    })),
    [{
      type: 'session.turn.interrupted',
      detail: 'interruptionSource: daemon_restart',
    }],
    'daemon 重启后必须从持久化会话项恢复中断原因，不能留下可点击但内容为空的面板',
  );

  const resumed = rustContract.normalizeRustBootstrapPayload({
    generatedAt: 1_700_000_003_000,
    currentSession: {
      sessionId: 'session-runtime-resumed',
      workspaceId: 'workspace-runtime',
      title: '续跑成功场景',
      createdAt: 1_700_000_000_000,
      updatedAt: 1_700_000_003_000,
    },
    sessions: [{
      sessionId: 'session-runtime-resumed',
      workspaceId: 'workspace-runtime',
      title: '续跑成功场景',
      createdAt: 1_700_000_000_000,
      updatedAt: 1_700_000_003_000,
    }],
    workspaces: [{
      workspaceId: 'workspace-runtime',
      rootPath: '/workspace/runtime',
    }],
    runtimeReadModel: {
      details: {
        sessions: [{
          session_id: 'session-runtime-resumed',
          current_status: 'detached',
          root_task_id: null,
          root_task_status: null,
          mission_id: null,
          last_update: 1_700_000_003_000,
        }],
        tasks: [{
          task_id: 'task-runtime-original-failure',
          mission_id: 'mission-runtime-original-failure',
          current_status: 'failed',
          failure_detail: '原轮次中断',
        }],
      },
    },
    canonicalTurns: [{
      sessionId: 'session-runtime-resumed',
      turnId: 'turn-runtime-original-failure',
      turnSeq: 1,
      acceptedAt: 1_700_000_000_000,
      completedAt: 1_700_000_001_000,
      status: 'interrupted',
      items: [],
    }, {
      sessionId: 'session-runtime-resumed',
      turnId: 'turn-runtime-resumed',
      turnSeq: 2,
      acceptedAt: 1_700_000_002_000,
      completedAt: 1_700_000_003_000,
      status: 'completed',
      items: [],
    }],
    recentEvents: [],
  }, {
    workspaceId: 'workspace-runtime',
    workspacePath: '/workspace/runtime',
    sessionId: 'session-runtime-resumed',
  });
  // 停止后能否继续只来自 daemon 读模型的 continuation，Web 不从轮次或任务状态推断。
  const continuationPayload = (continuation) => ({
    generatedAt: 1_700_000_003_000,
    currentSession: { sessionId: 'session-runtime-continue', workspaceId: 'workspace-runtime', title: '继续', createdAt: 1, updatedAt: 2 },
    sessions: [{ sessionId: 'session-runtime-continue', workspaceId: 'workspace-runtime', title: '继续', createdAt: 1, updatedAt: 2 }],
    workspaces: [{ workspaceId: 'workspace-runtime', rootPath: '/workspace/runtime' }],
    runtimeReadModel: {
      details: {
        sessions: [{
          session_id: 'session-runtime-continue',
          current_status: 'detached',
          last_update: 2,
          ...(continuation === undefined ? {} : { continuation }),
        }],
        tasks: [],
      },
    },
    canonicalTurns: [{
      sessionId: 'session-runtime-continue', turnId: 'turn-continue', turnSeq: 1,
      acceptedAt: 1, completedAt: 2, status: 'failed',
      items: [],
    }],
    recentEvents: [],
  });
  const continuationOf = (continuation) => rustContract.normalizeRustBootstrapPayload(
    continuationPayload(continuation),
    { workspaceId: 'workspace-runtime', workspacePath: '/workspace/runtime', sessionId: 'session-runtime-continue' },
  ).orchestratorRuntimeState?.continuation;
  assert.equal(continuationOf('resume_turn'), 'resume_turn');
  assert.equal(continuationOf('resume_chain'), 'resume_chain');
  assert.equal(continuationOf(undefined), undefined, '失败的轮次本身不足以让 Web 判定可继续');
  assert.equal(continuationOf('restart'), undefined, '未知取值一律视为不可继续');
  assert.equal(
    resumed.orchestratorRuntimeState?.status,
    'completed',
    '续跑完成后必须以最新轮次为当前状态，历史失败只能保留在运行记录中',
  );
  assert.equal(
    panel.shouldShowRuntimePanel(resumed.orchestratorRuntimeState?.status),
    false,
    '续跑成功后不得继续展示历史失败状态面板',
  );

  const statusPayload = {
    generatedAt: 100,
    currentSession: { sessionId: 'status-scope', title: '状态归属', createdAt: 1, updatedAt: 100 },
    sessions: [{ sessionId: 'status-scope', title: '状态归属', createdAt: 1, updatedAt: 100 }],
    runtimeReadModel: { details: {
      sessions: [{ session_id: 'status-scope', current_status: 'resumed', root_task_id: 'main-root',
        root_task_status: 'running', mission_id: 'shared-mission', active_task_ids: ['main-root'],
        active_execution_group_ids: ['shared-mission'], execution_chain_ref: 'main-chain',
        has_recoverable_chain: true, recoverable_branch_count: 1,
        active_branches: [
          { task_id: 'main-root', is_primary: true, status: 'running' },
          { task_id: 'known-child', is_primary: false, status: 'running' },
          { task_id: 'missing-child', is_primary: false, status: 'running' },
        ] }],
      tasks: [{ task_id: 'main-root', current_status: 'running', mission_id: 'shared-mission' },
        { task_id: 'known-child', current_status: 'completed', title: '已完成代理', mission_id: 'shared-mission' },
        { task_id: 'transferred-child', current_status: 'running', mission_id: 'shared-mission' }],
      assignments: [{ assignment_id: 'old-assignment', task_ids: ['transferred-child'],
        current_status: 'running', mission_id: 'shared-mission' }],
    } },
    recentEvents: [],
  };
  const projectStatus = (turns) => rustContract.normalizeRustBootstrapPayload({ ...statusPayload, canonicalTurns: turns },
    { sessionId: 'status-scope' });
  const missingMainline = projectStatus([]);
  assert.equal(missingMainline.orchestratorRuntimeState.status, 'idle', 'Task/sidecar 状态不能替代缺失的主线 Turn');
  assert.equal(missingMainline.state.processingState, null);
  assert.equal(missingMainline.orchestratorRuntimeState.continuation, undefined, '缺少主线身份时不能展示继续入口');
  assert.deepEqual(missingMainline.orchestratorRuntimeState.assignments.map(item => [item.assignmentId, item.status]),
    [['known-child', 'completed']], '代理统计只用明确绑定且存在 Task 的分支，忽略旧 assignment 和同 mission 的转交任务');
  for (const continuation of [undefined, 'resume_chain']) {
    const recoveryPayload = structuredClone(statusPayload);
    recoveryPayload.runtimeReadModel.details.sessions[0].continuation = continuation;
    recoveryPayload.runtimeReadModel.recovery = {
      active_recovery_ids: ['own-recovery', 'foreign-recovery'],
      summaries: [
        { recovery_id: 'own-recovery', session_id: 'status-scope', current_status: 'ready', latest_occurred_at: 10 },
        { recovery_id: 'foreign-recovery', session_id: 'foreign-session', mission_id: 'shared-mission',
          execution_chain_ref: 'main-chain', current_status: 'ready', latest_occurred_at: 20 },
      ],
    };
    const recovery = rustContract.normalizeRustBootstrapPayload(recoveryPayload, { sessionId: 'status-scope' })
      .orchestratorRuntimeState.opsView.recovery;
    assert.equal(recovery.pendingTaskCount, 1, '同 mission/chain 的外部恢复记录不能混入当前会话');
    assert.equal(recovery.continuationPolicy, continuation ? 'resumable' : 'none', '恢复记录不能自行授予继续资格');
  }
  for (const [status, expected] of [['completed', 'completed'], ['cancelled', 'cancelled'],
    ['interrupted', 'failed'], ['failed', 'failed'], ['running', 'running']]) {
    const projected = projectStatus([
      { sessionId: 'status-scope', turnId: 'main-turn', turnSeq: 1, acceptedAt: 10,
        ...(status === 'running' ? {} : { completedAt: 20 }), status, items: [] },
      { sessionId: 'status-scope', turnId: 'independent-agent-turn', turnSeq: 2,
        acceptedAt: 30, status: 'running', items: [], metadata: { agentExecution: { taskId: 'transferred-child' } } },
    ]);
    assert.equal(projected.orchestratorRuntimeState.status, expected, '独立代理不能覆盖主线状态');
    assert.equal(Boolean(projected.state.processingState?.isProcessing), status === 'running',
      '独立代理不能让已终止主线恢复输入忙碌状态');
  }

  console.log('runtime state panel golden passed');
}, { configFile: 'vite.web.config.ts' });
