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
  /aria-expanded=\{hasDetail \? recordExpanded : undefined\}[\s\S]*?\{#if recordExpanded\}[\s\S]*?runtime-diagnostics__record-detail/,
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

await withGoldenViteServer(async (server) => {
  const panel = await server.ssrLoadModule('/src/lib/runtime-state-panel.ts');
  const conversationRecords = await server.ssrLoadModule('/src/lib/conversation-runtime-records.ts');
  const runtimeTimeline = await server.ssrLoadModule('/src/lib/runtime-timeline.ts');
  const rustContract = await server.ssrLoadModule('/src/shared/bridges/rust-daemon-contract.ts');

  assert.equal(
    panel.shouldShowRuntimePanel({ status: 'idle', isProcessing: false, activeAssignmentCount: 0 }),
    false,
    'idle sessions without active assignments must not reserve homepage space',
  );
  assert.equal(
    panel.shouldShowRuntimePanel({ status: 'running', isProcessing: false, activeAssignmentCount: 0 }),
    true,
    'active runtime state must remain visible',
  );
  assert.equal(
    panel.shouldShowRuntimePanel({ status: 'idle', isProcessing: false, activeAssignmentCount: 2 }),
    true,
    'active assignments keep the runtime panel visible even if the aggregate status is stale',
  );
  assert.equal(
    panel.shouldShowRuntimePanel({ status: 'completed', isProcessing: false, activeAssignmentCount: 0 }),
    false,
    'completed sessions must not keep reserving homepage space',
  );
  assert.equal(
    panel.shouldShowRuntimePanel({ status: 'completed', isProcessing: false, activeAssignmentCount: 2 }),
    false,
    'historical assignments must not reopen a completed runtime panel',
  );
  assert.equal(
    panel.shouldShowRuntimePanel({ status: 'cancelled', isProcessing: false, activeAssignmentCount: 0 }),
    false,
    'cancelled sessions must not keep reserving homepage space',
  );
  assert.equal(
    panel.shouldShowRuntimePanel({ status: 'failed', isProcessing: false, activeAssignmentCount: 0 }),
    true,
    'failed sessions must keep their runtime diagnostics available',
  );

  assert.equal(panel.runtimeAssignmentIsActive('running'), true);
  assert.equal(panel.runtimeAssignmentIsActive('failed'), false);
  assert.equal(panel.runtimeAssignmentIsActive('completed'), false);
  assert.equal(panel.runtimeAssignmentIsActive('cancelled'), false);

  assert.equal(
    panel.resolveRuntimePanelStatus({ status: 'failed', isProcessing: true }),
    'running',
    '运行仍在继续时，过程中的失败事件不得把整体状态悬挂为失败',
  );
  assert.equal(
    panel.resolveRuntimePanelStatus({ status: 'completed', isProcessing: false }),
    'completed',
    '运行结束后必须保留权威完成终态',
  );
  assert.equal(
    panel.resolveRuntimePanelStatus({ status: 'failed', isProcessing: false }),
    'failed',
    '只有权威终态失败时才显示整体失败',
  );

  assert.equal(panel.shouldShowRuntimePhase('running', 'running'), false);
  assert.equal(panel.shouldShowRuntimePhase('idle', 'idle'), false);
  assert.equal(panel.shouldShowRuntimePhase('running', 'verify'), true);

  assert.equal(panel.shouldShowRuntimeBudget('normal'), false);
  assert.equal(panel.shouldShowRuntimeBudget(undefined), false);
  assert.equal(panel.shouldShowRuntimeBudget('notice'), true);
  assert.equal(panel.shouldShowRuntimeBudget('warning'), true);
  assert.equal(panel.shouldShowRuntimeBudget('danger'), true);

  assert.equal(panel.shouldShowRuntimeCache('healthy'), false);
  assert.equal(panel.shouldShowRuntimeCache('cold'), false);
  assert.equal(panel.shouldShowRuntimeCache('degraded'), true);

  assert.deepEqual(
    panel.resolveRuntimeTaskProgress({
      requiredTotal: 5,
      failedRequired: 1,
      runningOrPendingRequired: 2,
    }),
    { completed: 2, failed: 1, running: 2, total: 5, percent: 40 },
  );
  assert.equal(panel.resolveRuntimeTaskProgress(undefined), null);

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
      isProcessing: true,
      processingStartedAt: 500,
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
  assert.equal(
    conversationRecords.resolveCurrentConversationTurnStartedAt([{
      key: 'turn-start',
      message: {
        id: 'turn-start-message',
        role: 'user',
        source: 'user',
        content: '开始',
        timestamp: 700,
        isStreaming: false,
        isComplete: true,
        metadata: { turnSeq: 5 },
      },
    }, {
      key: 'turn-progress',
      message: {
        id: 'turn-progress-message',
        role: 'assistant',
        source: 'orchestrator',
        content: '',
        timestamp: 710,
        isStreaming: true,
        isComplete: false,
        metadata: { turnSeq: 5 },
      },
    }]),
    700,
    '当前轮次起点必须来自该轮最早的规范化消息时间',
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
      isProcessing: false,
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
  assert.deepEqual(normalized.orchestratorRuntimeState?.errors, [failureDetail, `cargo_check_failed: ${toolErrorDetail}`]);
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
  assert.equal(normalized.orchestratorRuntimeState?.opsView?.knowledgeAudit, undefined);

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
  assert.equal(
    resumed.orchestratorRuntimeState?.status,
    'completed',
    '续跑完成后必须以最新轮次为当前状态，历史失败只能保留在运行记录中',
  );
  assert.equal(
    panel.shouldShowRuntimePanel({
      status: resumed.orchestratorRuntimeState?.status,
      isProcessing: false,
      activeAssignmentCount: 0,
    }),
    false,
    '续跑成功后不得继续展示历史失败状态面板',
  );

  console.log('runtime state panel golden passed');
});
