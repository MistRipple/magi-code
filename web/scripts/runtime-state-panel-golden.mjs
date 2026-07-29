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
  /runtimeDiagnostics\.keyRecords[\s\S]*?runtime-diagnostics__record-kind[\s\S]*?item\.detail[\s\S]*?runtime-diagnostics__record-detail/,
  '关键运行记录必须在同一层直接展示工具或步骤的真实错误正文',
);
assert.doesNotMatch(
  runtimePanelSource,
  /runtimeDiagnostics\.failureTitle|failureDetails|runtime-diagnostics__block--failure|runtime-diagnostics__failure-entry/,
  '失败原因不得在关键运行记录之外重复展示，也不得为错误正文增加内嵌强调层',
);

await withGoldenViteServer(async (server) => {
  const panel = await server.ssrLoadModule('/src/lib/runtime-state-panel.ts');
  const rustContract = await server.ssrLoadModule('/src/shared/bridges/rust-daemon-contract.ts');

  assert.equal(
    panel.shouldShowRuntimePanel({ status: 'idle', isProcessing: false, attentionAssignmentCount: 0 }),
    false,
    'idle sessions without active assignments must not reserve homepage space',
  );
  assert.equal(
    panel.shouldShowRuntimePanel({ status: 'running', isProcessing: false, attentionAssignmentCount: 0 }),
    true,
    'active runtime state must remain visible',
  );
  assert.equal(
    panel.shouldShowRuntimePanel({ status: 'idle', isProcessing: false, attentionAssignmentCount: 2 }),
    true,
    'active assignments keep the runtime panel visible even if the aggregate status is stale',
  );
  assert.equal(
    panel.shouldShowRuntimePanel({ status: 'completed', isProcessing: false, attentionAssignmentCount: 0 }),
    false,
    'completed sessions must not keep reserving homepage space',
  );
  assert.equal(
    panel.shouldShowRuntimePanel({ status: 'completed', isProcessing: false, attentionAssignmentCount: 2 }),
    false,
    'historical assignments must not reopen a completed runtime panel',
  );
  assert.equal(
    panel.shouldShowRuntimePanel({ status: 'cancelled', isProcessing: false, attentionAssignmentCount: 0 }),
    false,
    'cancelled sessions must not keep reserving homepage space',
  );
  assert.equal(
    panel.shouldShowRuntimePanel({ status: 'failed', isProcessing: false, attentionAssignmentCount: 0 }),
    true,
    'failed sessions must keep their runtime diagnostics available',
  );

  assert.equal(panel.runtimeAssignmentNeedsAttention('running'), true);
  assert.equal(panel.runtimeAssignmentNeedsAttention('failed'), true);
  assert.equal(panel.runtimeAssignmentNeedsAttention('completed'), false);
  assert.equal(panel.runtimeAssignmentNeedsAttention('cancelled'), false);

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
      attentionAssignmentCount: 0,
    }),
    false,
    '续跑成功后不得继续展示历史失败状态面板',
  );

  console.log('runtime state panel golden passed');
});
