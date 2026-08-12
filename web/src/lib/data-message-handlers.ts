/**
 * DATA / CONTROL / NOTIFY message handlers
 * Extracted mechanically from message-handler.ts.
 */

import type { ClientBridgeMessage } from '../shared/bridges/client-bridge';
import {
  getState,
  setIsProcessing,
  setCurrentSessionId,
  adoptCurrentSessionIdForLiveTurn,
  advanceWorkspaceSessionProjectionCursor,
  replaceWorkspaceSessionProjection,
  clearWorkspaceSessionProjection,
  setQueuedMessages,
  setAppState,
  clearPendingInteractions,
  clearAllMessages,
  setCanonicalTimelineProjection,
  clearPendingRequest,
  setProcessingActor,
  getRequestBinding,
  clearRequestBinding,
  listRequestBindings,
  clearAllRequestBindings,
  clearProcessingState,
  setOrchestratorRuntimeState,
  replaceOrchestratorRuntimeState,
  applyAuthoritativeProcessingState,
  markMessageComplete,
  updateRequestBinding,
  getTimelineProjectionMessageById,
  settleProcessingAfterResponseCompletion,
  settleAuthoritativeIdleState,
  applyNotificationsSnapshot,
  applyNotificationsStatus,
  batchWebviewStatePersistence,
  setEnabledAgents,
  setSessionHistoryState,
  messagesState,
  hasActiveLocalTimelineTurn,
  setChangeMutationStatus,
} from '../stores/messages.svelte';
import { reportIncident, showFeedback } from './notifications';
import type { IncidentScope } from './notification-policy';
import type {
  AppState, Message, Session,
  Edit,
  ModelStatus, ModelStatusMap, ModelStatusType, OrchestratorRuntimeState,
} from '../types/message';
import type { StandardMessage, ContentBlock as StandardContentBlock } from '../shared/protocol/message-protocol';
import type { SessionBootstrapSnapshot } from '../shared/session-bootstrap';
import type { SettingsBootstrapSnapshot } from '../shared/settings-bootstrap';
import { resolveNotificationPresentation } from '../shared/notification-presentation';
import { ensureArray } from './utils';
import { i18n } from '../stores/i18n.svelte';
import {
  handleRetryRuntimePayload,
} from './message-utils';
import { buildEmptyWorkspaceAppState } from '../shared/bridges/empty-workspace-state';
import { failSessionNavigation, settleSessionNavigation } from '../shared/session-navigation.svelte';
import { selectComposerDraftWorkspace } from '../stores/composer-workspace.svelte';
import { settingsBootstrapMatchesCurrentWorkspace } from '../web/agent-api';
import {
  isCanonicalTerminalStatus,
  type CanonicalTurn,
  type CanonicalTurnEvent,
} from '../shared/protocol/canonical-turn';
import { deriveProcessingStateFromCanonicalTurns } from '../shared/protocol/canonical-processing';
import {
  applyCanonicalTurnEvent,
  clearCanonicalSessionTurns,
  replaceCanonicalSessionTurns,
  turnStoreState,
} from '../stores/turn-store.svelte';
import { postBridgeMessage } from '../shared/bridges/bridge-runtime';

let canonicalRecoveryRequestedAt = 0;

function requestCanonicalTimelineRecovery(): void {
  const now = Date.now();
  if (now - canonicalRecoveryRequestedAt < 1_000) {
    return;
  }
  canonicalRecoveryRequestedAt = now;
  postBridgeMessage({ type: 'requestState' });
}

function normalizeStateSliceVersion(value: unknown): number {
  return typeof value === 'number' && Number.isFinite(value) ? Math.floor(value) : 0;
}

function normalizeOptionalEditString(...values: unknown[]): string | undefined {
  for (const value of values) {
    if (typeof value === 'string' && value.trim()) {
      return value.trim();
    }
  }
  return undefined;
}

function currentWorkspaceIdValue(): string {
  return typeof messagesState.currentWorkspaceId === 'string'
    ? messagesState.currentWorkspaceId.trim()
    : '';
}

function clearCurrentSessionBeforeWorkspaceChange(nextWorkspaceId: string): void {
  const currentWorkspaceId = currentWorkspaceIdValue();
  const normalizedNextWorkspaceId = typeof nextWorkspaceId === 'string' ? nextWorkspaceId.trim() : '';
  if (
    normalizedNextWorkspaceId
    && currentWorkspaceId
    && normalizedNextWorkspaceId !== currentWorkspaceId
    && messagesState.currentSessionId
  ) {
    setCurrentSessionId(null);
  }
}

const MODEL_STATUS_TYPES = new Set<ModelStatusType>([
  'available',
  'connected',
  'configured',
  'disabled',
  'not_configured',
  'checking',
  'error',
  'unavailable',
  'invalid_model',
  'auth_failed',
  'network_error',
  'timeout',
  'orchestrator',
]);

function normalizeModelStatusType(status: unknown): ModelStatusType {
  return typeof status === 'string' && MODEL_STATUS_TYPES.has(status as ModelStatusType)
    ? status as ModelStatusType
    : 'error';
}

function safeModelStatusError(status: ModelStatusType): string | undefined {
  switch (status) {
    case 'error':
      return i18n.t('settings.status.error');
    case 'unavailable':
      return i18n.t('settings.status.unavailable');
    case 'invalid_model':
      return i18n.t('settings.status.invalidModel');
    case 'auth_failed':
      return i18n.t('settings.status.authFailed');
    case 'network_error':
      return i18n.t('settings.status.networkError');
    case 'timeout':
      return i18n.t('settings.status.timeout');
    default:
      return undefined;
  }
}

function sanitizeModelStatusValue(value: unknown, fallbackModel?: string): ModelStatus | null {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    return null;
  }
  const raw = value as Record<string, unknown>;
  const status = normalizeModelStatusType(raw.status);
  const model = typeof raw.model === 'string' && raw.model.trim()
    ? raw.model.trim()
    : fallbackModel;
  const next: ModelStatus = {
    status,
    ...(model ? { model } : {}),
  };
  if (typeof raw.version === 'string' && raw.version.trim()) {
    next.version = raw.version.trim();
  }
  if (typeof raw.tokens === 'number' && Number.isFinite(raw.tokens)) {
    next.tokens = raw.tokens;
  }
  const safeError = safeModelStatusError(status);
  if (safeError) {
    next.error = safeError;
  }
  return next;
}

function sanitizeModelStatusMap(
  statuses: unknown,
  existing: ModelStatusMap,
): ModelStatusMap {
  if (!statuses || typeof statuses !== 'object' || Array.isArray(statuses)) {
    return {};
  }
  const next: ModelStatusMap = {};
  for (const [key, value] of Object.entries(statuses as Record<string, unknown>)) {
    const normalizedKey = key.trim();
    if (!normalizedKey) {
      continue;
    }
    const sanitized = sanitizeModelStatusValue(value, existing[normalizedKey]?.model);
    if (sanitized) {
      next[normalizedKey] = sanitized;
    }
  }
  return next;
}

function shouldApplyStateSlice(params: {
  incomingVersion: number;
  currentVersion: number;
  incomingLength: number;
  currentLength: number;
}): boolean {
  const { incomingVersion, currentVersion, incomingLength, currentLength } = params;
  if (incomingVersion > currentVersion) {
    return true;
  }
  if (incomingVersion < currentVersion) {
    return false;
  }
  if (incomingLength === 0 && currentLength > 0) {
    return false;
  }
  return true;
}

function normalizeIncomingEdits(state: AppState): Edit[] {
  return ensureArray(state.pendingChanges)
    .filter((change): change is Edit => !!change && typeof change === 'object' && typeof (change as Edit).filePath === 'string' && !!(change as Edit).filePath)
    .map((change) => {
      let inferredType = change.type;
      if (!inferredType) {
        const adds = change.additions ?? 0;
        const dels = change.deletions ?? 0;
        if (adds > 0 && dels === 0) inferredType = 'add';
        else if (adds === 0 && dels > 0) inferredType = 'delete';
        else inferredType = 'modify';
      }
      return {
        sessionId: normalizeOptionalEditString(change.sessionId),
        workspaceId: normalizeOptionalEditString(change.workspaceId),
        workspacePath: normalizeOptionalEditString(change.workspacePath),
        filePath: change.filePath,
        oldPath: change.oldPath,
        snapshotId: change.snapshotId,
        updatedAt: typeof change.updatedAt === 'number' ? change.updatedAt : undefined,
        type: inferredType,
        additions: change.additions,
        deletions: change.deletions,
        diff: change.diff,
        originalContent: change.originalContent ?? null,
        previewContent: change.previewContent ?? null,
        previewAbsolutePath: change.previewAbsolutePath,
        previewCanOpenWorkspaceFile: change.previewCanOpenWorkspaceFile,
        contentKind: change.contentKind,
        size: typeof change.size === 'number' ? change.size : undefined,
        mime: change.mime,
        sourceKind: change.sourceKind,
        hasError: change.hasError === true,
        revertible: change.revertible === true,
        symlinkTarget: change.symlinkTarget,
        headSummary: change.headSummary,
        tailSummary: change.tailSummary,
        toolCallId: change.toolCallId,
        workerId: typeof (change as { workerId?: unknown }).workerId === 'string'
          ? (change as { workerId?: string }).workerId
          : undefined,
        contributors: change.contributors,
        executionGroupId: (typeof (change as { executionGroupId?: unknown }).executionGroupId === 'string'
          ? (change as { executionGroupId?: string }).executionGroupId
          : undefined)
          || (typeof (change as { missionId?: unknown }).missionId === 'string'
            ? (change as { missionId?: string }).missionId
            : undefined),
      };
    });
}


function handleStateUpdate(
  message: ClientBridgeMessage,
  options: { preserveLocalProcessing?: boolean } = {},
) {
  const state = message.state as AppState;
  if (!state) return;
  const incomingSessionId = typeof state.currentSessionId === 'string' ? state.currentSessionId.trim() : '';
  const currentSessionId = getState().currentSessionId?.trim() || '';
  const incomingWorkspaceId = typeof state.currentWorkspaceId === 'string' ? state.currentWorkspaceId.trim() : '';
  const currentWorkspaceId = getState().currentWorkspaceId?.trim() || '';
  if (incomingWorkspaceId && currentWorkspaceId && incomingWorkspaceId !== currentWorkspaceId) {
    console.warn('[MessageHandler] 忽略非当前工作区的 stateUpdate', {
      incomingWorkspaceId,
      currentWorkspaceId,
    });
    return;
  }
  if (incomingSessionId && currentSessionId && incomingSessionId !== currentSessionId) {
    console.warn('[MessageHandler] 忽略非当前会话的 stateUpdate', {
      incomingSessionId,
      currentSessionId,
    });
    return;
  }
  const incomingStateUpdatedAt = typeof state.stateUpdatedAt === 'number' ? state.stateUpdatedAt : undefined;
  const currentStateUpdatedAt = typeof getState().appState?.stateUpdatedAt === 'number'
    ? (getState().appState?.stateUpdatedAt as number)
    : undefined;

  if (incomingStateUpdatedAt !== undefined && currentStateUpdatedAt !== undefined && incomingStateUpdatedAt < currentStateUpdatedAt) {
    console.warn('[MessageHandler] 忽略过期 stateUpdate', {
      incomingUpdatedAt: incomingStateUpdatedAt,
      currentUpdatedAt: currentStateUpdatedAt,
    });
    return;
  }

  const store = getState();
  const currentPendingChangesVersion = normalizeStateSliceVersion(store.appState?.pendingChangesStateVersion);
  const incomingPendingChangesVersion = normalizeStateSliceVersion(state.pendingChangesStateVersion);
  const normalizedIncomingEdits = normalizeIncomingEdits(state);
  const currentEdits = ensureArray(store.edits) as Edit[];
  const applyEditsSlice = shouldApplyStateSlice({
    incomingVersion: incomingPendingChangesVersion,
    currentVersion: currentPendingChangesVersion,
    incomingLength: normalizedIncomingEdits.length,
    currentLength: currentEdits.length,
  });
  const mergedEdits = applyEditsSlice ? normalizedIncomingEdits : currentEdits;
  const mergedState: AppState = {
    ...state,
    pendingChanges: mergedEdits,
    pendingChangesStateVersion: applyEditsSlice ? incomingPendingChangesVersion : currentPendingChangesVersion,
  };

  setAppState(mergedState);
  if (!options.preserveLocalProcessing) {
    applyAuthoritativeProcessingState(state.processingState ?? null);
  }
  // currentSessionId 属于显式 bootstrap / switch 的会话锚定语义，
  // 不能由常规 stateUpdate 反向覆盖当前浏览器查看的会话。
  // 否则会出现侧边栏 active、URL、主内容三者分裂，破坏 live/restore 单一真相源。

  // canonical timeline view / runtime state 不再通过 stateUpdate 覆盖。
  // 当前统一约束：
  // 1. sessionBootstrapLoaded 负责 restore / switch 的原子恢复；
  // 2. 活跃会话的实时内容只由 canonical turn event 驱动；
  // 3. stateUpdate 只同步非时间轴、非 runtime state 的运行态。

  store.edits = mergedEdits;
  if (Array.isArray((state as any).workerStatuses)) {
    const statusMap: ModelStatusMap = {};
    for (const status of (state as any).workerStatuses) {
      if (!status?.worker) continue;
      const worker = status.worker;
      const currentStatus = store.modelStatus[worker]?.status;
      // 只有初始状态 'checking' 时才使用 workerStatuses 更新，
      // 避免覆盖 settingsBootstrapLoaded / 连接测试已经同步过的结果
      if (currentStatus === 'checking') {
        statusMap[worker] = {
          status: status.available ? 'available' : 'unavailable',
        };
      }
    }
    if (Object.keys(statusMap).length > 0) {
      store.modelStatus = { ...store.modelStatus, ...statusMap };
    }
  }

  // 处理状态现在只接受后端 processingState 快照或显式 control 终态。
  // 不再使用裸 isRunning/isProcessing 布尔值猜测运行态，避免异步 stateUpdate 把旧状态抬回前端。

}

export function handleUnifiedControlMessage(standard: StandardMessage) {
  if (!standard.control) {
    throw new Error('[MessageHandler] 控制消息缺少 control 字段');
  }

  const { controlType, payload } = standard.control as {
    controlType: string;
    payload: Record<string, unknown>;
  };

  switch (controlType) {
    case 'phase_changed':
      // 阶段变化只作为事件提示存在，实际处理态统一由 pending request、
      // 活跃流式消息和 authoritative snapshot 驱动。
      break;

    case 'task_accepted': {
      // 任务已被接受，当前轮保持 pending request，
      // 等权威 bootstrap / 流式消息接管后再自然清空
      break;
    }

    case 'task_rejected': {
      const requestId = payload?.requestId as string | undefined;
      const reasonRaw = payload?.reason;
      const reason = typeof reasonRaw === 'string' ? reasonRaw.trim() : '';
      const modelOriginIssue = payload?.modelOriginIssue === true;
      const toastLevel = modelOriginIssue ? 'warning' : 'error';
      const fallbackReason = i18n.t('messageHandler.requestRejected');
      const finalReason = reason || fallbackReason;
      if (reason) {
        console.warn('[MessageHandler] 任务请求被拒绝:', reason);
      }

      if (requestId) {
        clearPendingRequest(requestId);
        const binding = getRequestBinding(requestId);
        if (binding?.timeoutId) {
          clearTimeout(binding.timeoutId);
        }
        if (binding) {
          clearRequestBinding(requestId);
        }
      }

      reportIncident(finalReason, {
        scope: 'session',
        level: toastLevel,
        title: fallbackReason,
        failureStage: 'task_admission',
        requestId,
        source: modelOriginIssue ? 'model-runtime' : 'task-runtime',
      });
      break;
    }

    case 'task_started':
      // 任务开始执行后由权威快照和实时流接管
      break;

    case 'task_completed':
    case 'task_failed':
      // 请求级终态由 unifiedComplete 和 processingStateChanged 处理
      break;

    case 'worker_status': {
      // 代理状态更新：从控制消息同步状态到 UI
      const store = getState();
      const worker = payload?.worker as string | undefined;
      const available = payload?.available as boolean | undefined;
      if (worker && typeof available === 'boolean') {
        store.modelStatus = {
          ...store.modelStatus,
          [worker]: { status: available ? 'available' : 'unavailable' },
        };
      }
      break;
    }

    default:
      console.warn(`[MessageHandler] 未知控制消息类型: ${controlType}`, standard);
  }
}


/** 从标准消息块中提取文本内容 */
function extractTextFromStandardBlocks(blocks?: StandardContentBlock[]): string {
  if (!Array.isArray(blocks) || blocks.length === 0) return '';
  return blocks
    .filter((block) => block.type === 'text' || block.type === 'thinking')
    .map((block) => (block as any).content || '')
    .filter(Boolean)
    .join('\n');
}

export function handleUnifiedNotify(standard: StandardMessage) {
  const notify = standard.notify;
  const content = extractTextFromStandardBlocks(standard.blocks);
  if (!content) {
    console.warn('[MessageHandler] 通知消息缺少内容，跳过:', standard);
    return;
  }
  const presentation = resolveNotificationPresentation(notify, 'model-runtime');
  if (presentation.displayMode === 'silent') {
    return;
  }
  if (presentation.category === 'incident') {
    const scope: IncidentScope = messagesState.currentSessionId
      ? 'session'
      : messagesState.currentWorkspaceId
        ? 'workspace'
        : 'app';
    reportIncident(content, {
      scope,
      level: presentation.level === 'warning' ? 'warning' : 'error',
      title: presentation.title,
      source: presentation.source,
      actionRequired: presentation.actionRequired,
      duration: presentation.duration,
    });
    return;
  }
  showFeedback(presentation.level, content, {
    title: presentation.title,
    source: presentation.source,
    duration: presentation.duration,
  });
}


export function handleUnifiedData(standard: StandardMessage) {
  const data = standard.data;
  if (!data) {
    console.warn('[MessageHandler] 数据消息缺少 data 字段，跳过:', standard);
    return;
  }
  const { dataType, payload } = data;
  const asMessage = (extra: Record<string, unknown>) => ({ ...extra } as ClientBridgeMessage);

  switch (dataType) {
    case 'llmRetryRuntime':
      if (payload && typeof payload === 'object') {
        handleRetryRuntimePayload(payload as Record<string, unknown>);
      }
      break;

    case 'stateUpdate':
      handleStateUpdate(asMessage({ state: payload.state }));
      break;

    case 'processingStateChanged': {
      const isProcessing = payload.isProcessing as boolean | undefined;
      const transitionKind = payload.transitionKind as 'derived' | 'forced' | undefined;
      const reason = typeof payload.reason === 'string' ? payload.reason.trim() : '';
      // 当前只接受强制 idle 终态信号。
      // processing=true 统一由本地 pending request 或后端 authoritative snapshot 驱动，
      // 这里不再保留兜底抬升路径，避免处理态出现双真相源。
      if (isProcessing === false && transitionKind === 'forced') {
        // 同一 workspace 的 SSE 重放会包含当前 session 的历史终态。新请求已经建立
        // 本地绑定、但 canonical started 尚未投影时，历史 forced idle 不得终止新轮次。
        // 用户主动中断是唯一允许越过本地绑定直接收敛的显式操作。
        const isExplicitUserInterrupt = reason.startsWith('user_') && reason.includes('interrupt');
        const shouldPreserveBoundSubmission = hasBoundLocalPendingRequest()
          && !isExplicitUserInterrupt;
        if (!shouldPreserveBoundSubmission) {
          clearProcessingState();
        }
      }
      const source = payload.source as string | undefined;
      const agent = payload.agent as string | undefined;
      if (source) {
        setProcessingActor(source, agent);
      }
      break;
    }

    case 'sessionsUpdated':
      handleSessionsUpdated(asMessage({
        workspaceId: payload.workspaceId,
        sessions: payload.sessions,
        runtimeEpoch: payload.runtimeEpoch,
        eventStreamNextSequence: payload.eventStreamNextSequence,
      }));
      break;

    case 'emptyWorkspaceStateLoaded':
      handleEmptyWorkspaceStateLoaded(asMessage({
        state: payload.state,
        workspaceId: payload.workspaceId,
        workspacePath: payload.workspacePath,
      }));
      break;

    case 'sessionNavigationFailed':
      if (failSessionNavigation(payload.requestId)) {
        showFeedback('error', typeof payload.message === 'string' ? payload.message : i18n.t('web.workbenchActionFailed', {
          action: i18n.t('bridge.action.switchSession'),
        }), {
          source: 'session-management',
          presentation: 'toast',
        });
      }
      break;

    case 'sessionBootstrapLoaded':
      handleSessionBootstrapLoaded(asMessage({
        agent: payload.agent,
        sessionId: payload.sessionId,
        workspace: payload.workspace,
        sessions: payload.sessions,
        state: payload.state,
        canonicalTurns: payload.canonicalTurns,
        eventStreamNextSequence: payload.eventStreamNextSequence,
        notifications: payload.notifications,
        orchestratorRuntimeState: payload.orchestratorRuntimeState,
        hasMoreBefore: payload.hasMoreBefore,
        beforeCursor: payload.beforeCursor,
        canonicalHasMoreBefore: payload.canonicalHasMoreBefore,
        canonicalBeforeCursor: payload.canonicalBeforeCursor,
        navigationRequestId: payload.navigationRequestId,
        navigationTarget: payload.navigationTarget,
        navigationOrchestratorSessionConfig: payload.navigationOrchestratorSessionConfig,
      }));
      break;

    case 'sessionTurnAccepted': {
      const sessionId = typeof payload.sessionId === 'string' ? payload.sessionId.trim() : '';
      const workspaceId = typeof payload.workspaceId === 'string' ? payload.workspaceId.trim() : '';
      if (sessionId) {
        adoptCurrentSessionIdForLiveTurn(sessionId);
      }
      const runtimeEpoch = typeof payload.runtimeEpoch === 'string' ? payload.runtimeEpoch.trim() : '';
      const eventStreamNextSequence = Number(payload.eventStreamNextSequence);
      if (workspaceId && runtimeEpoch && Number.isFinite(eventStreamNextSequence) && eventStreamNextSequence >= 1) {
        advanceWorkspaceSessionProjectionCursor(workspaceId, {
          runtimeEpoch,
          eventStreamNextSequence,
        });
      }
      break;
    }

    case 'sessionTurnCanonicalEventUpdated':
      handleSessionTurnCanonicalEventUpdated(asMessage({
        sessionId: payload.sessionId,
        canonicalEvent: payload.canonicalEvent,
      }));
      break;

    case 'notificationsLoaded':
      handleNotificationsLoaded(asMessage({
        sessionId: payload.sessionId,
        workspaceId: payload.workspaceId,
        notifications: payload.notifications,
      }));
      break;

    case 'notificationsStatus':
      applyNotificationsStatus(payload);
      break;

    case 'changeMutationStatus':
      setChangeMutationStatus({
        isMutating: payload.isMutating === true,
        sessionId: typeof payload.sessionId === 'string' ? payload.sessionId : null,
        workspaceId: typeof payload.workspaceId === 'string' ? payload.workspaceId : null,
        workspacePath: typeof payload.workspacePath === 'string' ? payload.workspacePath : null,
        updatedAt: typeof payload.updatedAt === 'number' ? payload.updatedAt : undefined,
      });
      break;

    case 'orchestratorRuntimeState':
      handleOrchestratorRuntimeState(asMessage(payload));
      break;

    case 'clarificationRequest':
      handleClarificationRequest(asMessage(payload));
      break;

    case 'settingsBootstrapLoaded':
      handleSettingsBootstrapLoaded(asMessage(payload));
      break;

    case 'workerConnectionTestResult':
      handleConnectionTestResult(asMessage(payload));
      break;

    case 'orchestratorConnectionTestResult':
      handleConnectionTestResult({ ...asMessage(payload), _target: 'orchestrator' });
      break;

    case 'auxiliaryConnectionTestResult':
      handleConnectionTestResult({ ...asMessage(payload), _target: 'auxiliary' });
      break;

    case 'missionExecutionFailed': {
      // Mission 级失败：只同步 backendProcessing=false。
      // activeMessageIds/pendingRequests 应由消息完成链路和请求绑定分别清理。
      setIsProcessing(false);
      break;
    }

    case 'registryAgentsLoaded': {
      // Registry agents 加载完成：写入全局 enabledAgents 状态
      const agents = payload.enabledAgents;
      if (Array.isArray(agents)) {
        setEnabledAgents(agents);
      }
      const store = getState();
      if (
        Array.isArray(payload.roleTemplates)
        && Array.isArray(payload.registryEngines)
        && Array.isArray(payload.registryAgents)
      ) {
        store.settingsRegistrySnapshot = {
          roleTemplates: payload.roleTemplates,
          registryEngines: payload.registryEngines,
          registryAgents: payload.registryAgents,
        };
      }
      break;
    }

    case 'taskStatusChanged': {
      const newStatus = typeof payload.newStatus === 'string' ? payload.newStatus : '';
      const title = typeof payload.title === 'string' && payload.title.trim() ? payload.title.trim() : '';
      const label = title || i18n.t('messageHandler.taskDefaultLabel');
      const taskId = typeof payload.taskId === 'string' ? payload.taskId.trim() : '';
      const failureDetail = typeof payload.failureDetail === 'string' ? payload.failureDetail.trim() : '';
      const failureStage = typeof payload.failureStage === 'string' ? payload.failureStage.trim() : '';

      // 失败通知承担错误日志职责，必须记录运行时给出的直接错误；没有错误详情时
      // 使用明确的诊断缺失信息，不能再生成“请查看其他面板”的空泛模板。
      if (newStatus === 'Failed') {
        const directError = failureDetail || i18n.t('notification.missingRuntimeDetail');
        reportIncident(directError, {
          scope: 'session',
          source: 'task-runtime',
          title: label,
          failureStage: failureStage || 'task_execution',
          taskId: taskId || undefined,
        });
      }
      break;
    }

    case 'messageCreated': {
      // 消息内容由时间线承载，不再重复写入通知中心。
      break;
    }

    default:
      break;
  }
}


function handleSessionsUpdated(message: ClientBridgeMessage) {
  const sessions = message.sessions as Session[];
  const workspaceId = typeof message.workspaceId === 'string' ? message.workspaceId.trim() : '';
  if (!workspaceId) {
    console.warn('[MessageHandler] 忽略缺少 workspaceId 的会话目录更新');
    return;
  }
  const currentWorkspaceId = currentWorkspaceIdValue();
  if (currentWorkspaceId && workspaceId !== currentWorkspaceId) {
    return;
  }
  if (sessions) {
    replaceWorkspaceSessionProjection(workspaceId, ensureArray(sessions), {
      runtimeEpoch: typeof message.runtimeEpoch === 'string' ? message.runtimeEpoch : '',
      eventStreamNextSequence: Number(message.eventStreamNextSequence),
    }, {
      allowRuntimeEpochChange: message.allowRuntimeEpochChange === true,
    });
  }
}

function workspaceSessionCursorFromBootstrap(message: ClientBridgeMessage) {
  const agentRecord = (message as Record<string, unknown>).agent;
  const agent = agentRecord && typeof agentRecord === 'object' && !Array.isArray(agentRecord)
    ? agentRecord as Record<string, unknown>
    : null;
  return {
    runtimeEpoch: typeof agent?.runtimeEpoch === 'string' ? agent.runtimeEpoch : '',
    eventStreamNextSequence: Number((message as Record<string, unknown>).eventStreamNextSequence),
  };
}

function handleEmptyWorkspaceStateLoaded(message: ClientBridgeMessage) {
  const state = (message.state as AppState | undefined) ?? buildEmptyWorkspaceAppState(Date.now());
  const hasPendingLocalTurn = messagesState.pendingRequests.size > 0;
  const workspaceId = typeof (message as Record<string, unknown>).workspaceId === 'string'
    ? ((message as Record<string, unknown>).workspaceId as string).trim()
    : '';
  const workspacePath = typeof (message as Record<string, unknown>).workspacePath === 'string'
    ? ((message as Record<string, unknown>).workspacePath as string).trim()
    : '';

  batchWebviewStatePersistence(() => {
    messagesState.sessionHydrating = false;
    if (!hasPendingLocalTurn) {
      clearAllMessages({
        persist: false,
        resetTimelineView: true,
        resetPanelState: true,
        skipAntiLiftBack: true,
      });
      clearAllRequestBindings();
      clearPendingInteractions();
      clearProcessingState({ skipAntiLiftBack: true });
      clearCanonicalSessionTurns();
      messagesState.canonicalTimelineProjection = null;
      setQueuedMessages([]);
    }
    clearCurrentSessionBeforeWorkspaceChange(workspaceId);
    setCurrentSessionId(null);
    messagesState.currentWorkspaceId = workspaceId;
    messagesState.currentWorkspacePath = workspacePath;
    clearWorkspaceSessionProjection();
    setSessionHistoryState(null, { workspaceId });
    setAppState({
      ...state,
      sessions: [],
      currentSessionId: '',
      currentWorkspaceId: workspaceId,
      currentWorkspacePath: workspacePath,
      pendingChanges: [],
      tasks: [],
      isProcessing: false,
      processingState: null,
    });
    setOrchestratorRuntimeState(null);
  });
}

function hasRenderableAssistantContent(message: Message): boolean {
  if (typeof message?.content === 'string' && message.content.trim().length > 0) {
    return true;
  }
  return Array.isArray(message?.blocks) && message.blocks.length > 0;
}

function resolveMessageMetadataString(message: Message, key: string): string {
  const metadata = message.metadata && typeof message.metadata === 'object'
    ? message.metadata as Record<string, unknown>
    : {};
  const raw = metadata[key];
  return typeof raw === 'string' ? raw.trim() : '';
}

function isTerminalAssistantResponse(message: Message): boolean {
  if (
    message.role !== 'assistant'
    || message.source === 'system'
    || message.isStreaming === true
    || !hasRenderableAssistantContent(message)
  ) {
    return false;
  }
  const turnItemKind = typeof message.metadata?.turnItemKind === 'string'
    ? message.metadata.turnItemKind.trim()
    : '';
  if (!turnItemKind) {
    return message.type !== 'tool_call' && message.type !== 'thinking';
  }
  return turnItemKind === 'assistant_text'
    || turnItemKind === 'assistant_final'
    || turnItemKind === 'assistant_error';
}

function messageMatchesRequestBinding(
  message: Message,
  binding: ReturnType<typeof listRequestBindings>[number],
): boolean {
  const messageRequestId = resolveMessageMetadataString(message, 'requestId');
  if (messageRequestId && messageRequestId === binding.requestId) {
    return true;
  }
  const exactIds = new Set(
    [binding.realMessageId, binding.placeholderMessageId]
      .map((id) => typeof id === 'string' ? id.trim() : '')
      .filter((id) => id.length > 0),
  );
  return exactIds.has(message.id);
}

function findTerminalAssistantByRequestIdentity(
  binding: ReturnType<typeof listRequestBindings>[number],
): Message | undefined {
  const directIds = [binding.realMessageId, binding.placeholderMessageId]
    .map((id) => typeof id === 'string' ? id.trim() : '')
    .filter((id) => id.length > 0);
  for (const id of directIds) {
    const directMessage = getTimelineProjectionMessageById(id);
    if (
      directMessage
      && isTerminalAssistantResponse(directMessage)
      && messageMatchesRequestBinding(directMessage, binding)
    ) {
      return directMessage;
    }
  }

  const threadMessages = getState().threadMessages;
  if (!Array.isArray(threadMessages) || threadMessages.length === 0) {
    return undefined;
  }
  for (let index = threadMessages.length - 1; index >= 0; index -= 1) {
    const message = threadMessages[index];
    if (
      isTerminalAssistantResponse(message)
      && messageMatchesRequestBinding(message, binding)
    ) {
      return message;
    }
  }
  return undefined;
}


function hasPendingLocalRequest(): boolean {
  return messagesState.pendingRequests.size > 0;
}

function hasBoundLocalPendingRequest(): boolean {
  return listRequestBindings().some((binding) => (
    messagesState.pendingRequests.has(binding.requestId)
  ));
}

function canonicalTurnRequestId(turn: CanonicalTurn): string {
  const turnRequestId = typeof turn.metadata?.requestId === 'string'
    ? turn.metadata.requestId.trim()
    : '';
  if (turnRequestId) {
    return turnRequestId;
  }
  for (const item of turn.items) {
    const itemRequestId = typeof item.metadata?.requestId === 'string'
      ? item.metadata.requestId.trim()
      : '';
    if (itemRequestId) {
      return itemRequestId;
    }
  }
  return '';
}

function canonicalTurnMatchesRequestBinding(
  turn: CanonicalTurn,
  binding: ReturnType<typeof listRequestBindings>[number],
): boolean {
  const requestId = canonicalTurnRequestId(turn);
  if (requestId) {
    return requestId === binding.requestId;
  }
  const itemIds = new Set(
    [binding.userMessageId, binding.placeholderMessageId, binding.realMessageId]
      .map((itemId) => typeof itemId === 'string' ? itemId.trim() : '')
      .filter(Boolean),
  );
  return itemIds.size > 0 && turn.items.some((item) => itemIds.has(item.itemId));
}

function findCanonicalTurnByRequestBinding(
  sessionId: string,
  binding: ReturnType<typeof listRequestBindings>[number],
): CanonicalTurn | undefined {
  return canonicalTurnsForSession(sessionId, turnStoreState.reducer.turns)
    .findLast((turn) => canonicalTurnMatchesRequestBinding(turn, binding));
}

function canonicalSnapshotContainsAllBoundPendingRequests(turns: CanonicalTurn[]): boolean {
  const pendingRequestIds = new Set(
    listRequestBindings()
      .map((binding) => binding.requestId)
      .filter((requestId) => messagesState.pendingRequests.has(requestId)),
  );
  if (pendingRequestIds.size === 0) {
    return false;
  }
  const snapshotRequestIds = new Set(turns.map(canonicalTurnRequestId).filter(Boolean));
  return [...pendingRequestIds].every((requestId) => snapshotRequestIds.has(requestId));
}

function canonicalTurnsForSession(sessionId: string, turns: unknown): CanonicalTurn[] {
  if (!Array.isArray(turns)) {
    return [];
  }
  return (turns as CanonicalTurn[])
    .filter((turn) => turn?.sessionId === sessionId)
    .sort((left, right) => left.turnSeq - right.turnSeq || left.turnId.localeCompare(right.turnId));
}

function reconcileRuntimeStateFromCanonicalTurns(sessionId: string): void {
  const latestTurn = canonicalTurnsForSession(sessionId, turnStoreState.reducer.turns).at(-1);
  if (!latestTurn) {
    return;
  }
  const status: OrchestratorRuntimeState['status'] = latestTurn.status === 'pending'
    || latestTurn.status === 'running'
    ? 'running'
    : latestTurn.status === 'completed'
      ? 'completed'
      : latestTurn.status === 'blocked'
        ? 'blocked'
        : latestTurn.status === 'failed' || latestTurn.status === 'interrupted'
          ? 'failed'
          : latestTurn.status === 'cancelled' || latestTurn.status === 'superseded'
            ? 'cancelled'
            : 'idle';
  const statusChangedAt = Math.max(
    latestTurn.acceptedAt,
    latestTurn.completedAt ?? 0,
    ...latestTurn.items.map((item) => item.updatedAt),
  );
  const current = messagesState.orchestratorRuntimeState;
  replaceOrchestratorRuntimeState({
    ...(current ?? {
      errors: [],
      assignments: [],
      lastEventAt: statusChangedAt,
    }),
    sessionId,
    status,
    phase: status,
    statusChangedAt,
    lastEventAt: Math.max(current?.lastEventAt ?? 0, statusChangedAt),
    ...(status === 'running'
      ? { endedAt: undefined }
      : { endedAt: latestTurn.completedAt ?? statusChangedAt }),
  });
}

function reconcileRequestBindingsFromAuthoritativeThread(sessionId: string): void {
  const currentSessionId = getState().currentSessionId || '';
  if (!sessionId || !currentSessionId || currentSessionId !== sessionId) {
    return;
  }

  let settledResponse = false;
  for (const binding of listRequestBindings()) {
    const matchedTurn = findCanonicalTurnByRequestBinding(sessionId, binding);
    if (!matchedTurn || !isCanonicalTerminalStatus(matchedTurn.status)) {
      continue;
    }
    const matchedAssistant = findTerminalAssistantByRequestIdentity(binding);
    if (matchedAssistant) {
      markMessageComplete(matchedAssistant.id);
    }
    clearPendingRequest(binding.requestId);
    updateRequestBinding(binding.requestId, {
      ...(matchedAssistant ? { realMessageId: matchedAssistant.id } : {}),
      timeoutId: undefined,
    });
    if (binding.timeoutId) {
      clearTimeout(binding.timeoutId);
    }
    clearRequestBinding(binding.requestId);
    settledResponse = true;
  }

  if (settledResponse) {
    settleProcessingAfterResponseCompletion();
  }
}

function handleSessionTurnCanonicalEventUpdated(message: ClientBridgeMessage) {
  const sessionId = typeof message.sessionId === 'string' ? message.sessionId.trim() : '';
  const canonicalEvent = message.canonicalEvent as CanonicalTurnEvent | undefined;
  if (!sessionId || !canonicalEvent || canonicalEvent.sessionId !== sessionId) {
    return;
  }
  if (!adoptCurrentSessionIdForLiveTurn(sessionId)) {
    return;
  }
  const projection = applyCanonicalTurnEvent(canonicalEvent);
  if (!projection && turnStoreState.lastError) {
    requestCanonicalTimelineRecovery();
    return;
  }
  if (projection && setCanonicalTimelineProjection(projection)) {
    // processing 是会话级状态，必须从 reducer 中该会话的完整 turn 集合推导。
    // 事件流重连会回放历史 turn；若只看当前单条历史终态事件，会错误清除刚提交的新轮次。
    const processingState = deriveProcessingStateFromCanonicalTurns(
      turnStoreState.reducer.turns,
      sessionId,
    );
    applyAuthoritativeProcessingState(processingState);
    reconcileRuntimeStateFromCanonicalTurns(sessionId);
    reconcileRequestBindingsFromAuthoritativeThread(sessionId);
  }
}

function applyCanonicalTurnsSnapshot(
  sessionId: string,
  turns: unknown,
  lastAppliedEventSeq: number,
): boolean {
  if (!Array.isArray(turns)) {
    return false;
  }
  const canonicalTurns = canonicalTurnsForSession(sessionId, turns);
  const projection = replaceCanonicalSessionTurns(sessionId, canonicalTurns, lastAppliedEventSeq);
  if (!projection) {
    return false;
  }
  return setCanonicalTimelineProjection(projection);
}

function clearStaleSettingsBootstrapSnapshot(): void {
  const store = getState();
  if (store.settingsBootstrapSnapshot && !settingsBootstrapMatchesCurrentWorkspace(store.settingsBootstrapSnapshot)) {
    store.settingsBootstrapSnapshot = null;
  }
}

function handleSessionBootstrapLoaded(message: ClientBridgeMessage) {
  const sessionId = typeof message.sessionId === 'string' ? message.sessionId.trim() : '';
  const state = message.state as AppState | undefined;
  const workspaceRecord = (message as Record<string, unknown>).workspace;
  const workspace = workspaceRecord && typeof workspaceRecord === 'object'
    ? workspaceRecord as Record<string, unknown>
    : null;
  const workspaceId = typeof workspace?.workspaceId === 'string' ? workspace.workspaceId.trim() : '';
  const workspacePath = typeof workspace?.rootPath === 'string' ? workspace.rootPath.trim() : '';
  const hasMoreBefore = message.hasMoreBefore === true;
  const beforeCursor = typeof message.beforeCursor === 'string' && message.beforeCursor.trim()
    ? message.beforeCursor.trim()
    : null;
  const canonicalTurns = (message as Record<string, unknown>).canonicalTurns;
  const eventStreamNextSequence = Number((message as Record<string, unknown>).eventStreamNextSequence);
  const canonicalEventWatermark = Number.isFinite(eventStreamNextSequence)
    ? Math.max(0, Math.floor(eventStreamNextSequence) - 1)
    : 0;
  const navigationRequestId = typeof message.navigationRequestId === 'string'
    ? message.navigationRequestId.trim()
    : '';
  const navigationTarget = message.navigationTarget === 'draft'
    ? 'draft'
    : message.navigationTarget === 'session' ? 'session' : null;
  const navigationDraftConfig = message.navigationOrchestratorSessionConfig
    && typeof message.navigationOrchestratorSessionConfig === 'object'
    && !Array.isArray(message.navigationOrchestratorSessionConfig)
    ? message.navigationOrchestratorSessionConfig as Record<string, unknown>
    : {};

  const settleCommittedNavigation = (): boolean => {
    if (!navigationRequestId || !navigationTarget) return false;
    return settleSessionNavigation(navigationRequestId, {
      kind: navigationTarget,
      workspaceId,
      sessionId,
    });
  };

  if (!state) {
    return;
  }
  if (!sessionId) {
    const snapshot = message as ClientBridgeMessage & SessionBootstrapSnapshot;
    const sessions = ensureArray(snapshot.sessions) as Session[];
    const hasPendingLocalTurn = messagesState.pendingRequests.size > 0;
    const preserveExistingDraftConfig = navigationTarget === null
      && !getState().currentSessionId
      && getState().currentWorkspaceId?.trim() === workspaceId;
    batchWebviewStatePersistence(() => {
      messagesState.sessionHydrating = false;
      if (!hasPendingLocalTurn) {
        clearAllMessages({
          persist: false,
          resetTimelineView: true,
          resetPanelState: true,
          skipAntiLiftBack: true,
        });
        clearAllRequestBindings();
        clearPendingInteractions();
        clearProcessingState({ skipAntiLiftBack: true });
        clearCanonicalSessionTurns();
        messagesState.canonicalTimelineProjection = null;
        setQueuedMessages([]);
      }
      clearCurrentSessionBeforeWorkspaceChange(workspaceId);
      messagesState.currentWorkspaceId = workspaceId || messagesState.currentWorkspaceId;
      messagesState.currentWorkspacePath = workspacePath;
      replaceWorkspaceSessionProjection(
        workspaceId,
        sessions,
        workspaceSessionCursorFromBootstrap(message),
        { allowRuntimeEpochChange: true },
      );
      setCurrentSessionId(null);
      messagesState.draftOrchestratorSessionConfig = navigationTarget === 'draft'
        ? { ...navigationDraftConfig }
        : preserveExistingDraftConfig ? { ...messagesState.draftOrchestratorSessionConfig } : {};
      selectComposerDraftWorkspace(workspaceId);
      clearStaleSettingsBootstrapSnapshot();
      setSessionHistoryState(null, { workspaceId });
      setAppState({
        ...state,
        sessions,
        currentSession: undefined,
        currentSessionId: '',
        currentWorkspaceId: workspaceId,
        isProcessing: false,
        processingState: null,
      });
      setOrchestratorRuntimeState(null);
      if (snapshot.notifications) {
        applyNotificationsSnapshot(null, snapshot.notifications.notifications, workspaceId);
      }
    });
    settleCommittedNavigation();
    return;
  }

  const currentSessionId = getState().currentSessionId || '';
  const currentWorkspaceId = getState().currentWorkspaceId?.trim() || '';
  const isSameSession = currentSessionId === sessionId
    && (!workspaceId || !currentWorkspaceId || workspaceId === currentWorkspaceId);

  // 同 session 恢复按事件水位决定是否接管快照；水位不落后的快照必须进入 reducer，
  // 否则 bridge 已推进 SSE cursor 而 canonical 状态未同步，会永久跳过恢复区间。
  if (isSameSession) {
    batchWebviewStatePersistence(() => {
      messagesState.sessionHydrating = false;
      messagesState.draftOrchestratorSessionConfig = {};
      const snapshot = message as ClientBridgeMessage & SessionBootstrapSnapshot;
      const sessions = ensureArray(snapshot.sessions) as Session[];
      messagesState.currentWorkspaceId = workspaceId || messagesState.currentWorkspaceId;
      messagesState.currentWorkspacePath = workspacePath || messagesState.currentWorkspacePath;
      replaceWorkspaceSessionProjection(
        workspaceId,
        sessions,
        workspaceSessionCursorFromBootstrap(message),
        { allowRuntimeEpochChange: true },
      );
      clearStaleSettingsBootstrapSnapshot();
      const hadLiveTurnBeforeSnapshot = hasActiveLocalTimelineTurn();
      const hadPendingLocalRequestBeforeSnapshot = hasPendingLocalRequest();
      const authoritativeSnapshotIsIdle = state.isProcessing !== true
        && state.processingState?.isProcessing !== true;
      const canonicalSessionTurns = canonicalTurnsForSession(sessionId, canonicalTurns);
      const shouldApplyCanonicalSnapshot = canonicalEventWatermark
        >= turnStoreState.reducer.lastAppliedEventSeq;
      // bootstrap 的处理态和 canonical turn 可能并非同一时刻落盘。只要快照还没有
      // 当前绑定的 requestId，就不能用它替换本地轮次，否则无论快照显示 idle 还是
      // processing，都会把刚发送的用户消息从对话区擦掉。
      const preserveLocalTurnDuringStaleSnapshot = hadLiveTurnBeforeSnapshot
        && hadPendingLocalRequestBeforeSnapshot
        && hasBoundLocalPendingRequest()
        && !canonicalSnapshotContainsAllBoundPendingRequests(canonicalSessionTurns);

      handleStateUpdate({
        ...message,
        state: {
          ...state,
          currentSessionId: sessionId,
          currentWorkspaceId: workspaceId,
          sessions,
        },
      }, { preserveLocalProcessing: preserveLocalTurnDuringStaleSnapshot });

      if (!preserveLocalTurnDuringStaleSnapshot && shouldApplyCanonicalSnapshot) {
        replaceOrchestratorRuntimeState(
          (snapshot.orchestratorRuntimeState as OrchestratorRuntimeState | null | undefined) ?? null,
        );
      }

      if (snapshot.notifications) {
        applyNotificationsSnapshot(sessionId, snapshot.notifications.notifications, workspaceId);
      }
      setSessionHistoryState(sessionId, {
        workspaceId,
        hasMoreBefore,
        beforeCursor,
        canonicalHasMoreBefore: message.canonicalHasMoreBefore === true,
        canonicalBeforeCursor: typeof message.canonicalBeforeCursor === 'string'
          && message.canonicalBeforeCursor.trim()
          ? message.canonicalBeforeCursor.trim()
          : null,
        isLoadingBefore: false,
        preserveLoadedWindow: true,
      });

      if (shouldApplyCanonicalSnapshot && !preserveLocalTurnDuringStaleSnapshot) {
        applyCanonicalTurnsSnapshot(sessionId, canonicalSessionTurns, canonicalEventWatermark);
        reconcileRuntimeStateFromCanonicalTurns(sessionId);
        reconcileRequestBindingsFromAuthoritativeThread(sessionId);
      }
      if (
        authoritativeSnapshotIsIdle
        && shouldApplyCanonicalSnapshot
        && !hasPendingLocalRequest()
      ) {
        settleAuthoritativeIdleState();
      }
    });
    settleCommittedNavigation();
    return;
  }

  // 跨 session 切换：完整重建
  batchWebviewStatePersistence(() => {
    messagesState.sessionHydrating = false;
    messagesState.draftOrchestratorSessionConfig = {};
    // skipAntiLiftBack: 跨 session 切换后紧接着 applyAuthoritativeProcessingState
    // 恢复新会话的权威状态，不能让防回抬保护阻断新会话的 processing 写入
    clearAllMessages({
      persist: false,
      resetTimelineView: false,
      resetPanelState: false,
      skipAntiLiftBack: true,
    });
    clearAllRequestBindings();
    clearPendingInteractions();
    clearProcessingState({ skipAntiLiftBack: true });
    clearCanonicalSessionTurns(sessionId);

    const snapshot = message as ClientBridgeMessage & SessionBootstrapSnapshot;
    const sessions = ensureArray(snapshot.sessions) as Session[];
    clearCurrentSessionBeforeWorkspaceChange(workspaceId);
    messagesState.currentWorkspaceId = workspaceId || messagesState.currentWorkspaceId;
    messagesState.currentWorkspacePath = workspacePath || messagesState.currentWorkspacePath;
    replaceWorkspaceSessionProjection(
      workspaceId,
      sessions,
      workspaceSessionCursorFromBootstrap(message),
      { allowRuntimeEpochChange: true },
    );

    setCurrentSessionId(sessionId);
    clearStaleSettingsBootstrapSnapshot();
    applyCanonicalTurnsSnapshot(sessionId, canonicalTurns, canonicalEventWatermark);
    handleStateUpdate({
      ...message,
      state: {
        ...state,
        currentSessionId: sessionId,
        currentWorkspaceId: workspaceId,
        sessions,
      },
    });
    replaceOrchestratorRuntimeState(
      (snapshot.orchestratorRuntimeState as OrchestratorRuntimeState | null | undefined) ?? null,
    );
    if (snapshot.notifications) {
      applyNotificationsSnapshot(sessionId, snapshot.notifications.notifications, workspaceId);
    }
    setSessionHistoryState(sessionId, {
      workspaceId,
      hasMoreBefore,
      beforeCursor,
      canonicalHasMoreBefore: message.canonicalHasMoreBefore === true,
      canonicalBeforeCursor: typeof message.canonicalBeforeCursor === 'string'
        && message.canonicalBeforeCursor.trim()
        ? message.canonicalBeforeCursor.trim()
        : null,
      isLoadingBefore: false,
    });
    reconcileRequestBindingsFromAuthoritativeThread(sessionId);
  });
  settleCommittedNavigation();
}

function handleNotificationsLoaded(message: ClientBridgeMessage) {
  const sessionId = typeof message.sessionId === 'string' ? message.sessionId : '';
  const workspaceId = typeof message.workspaceId === 'string' ? message.workspaceId : '';
  if (!workspaceId) {
    return;
  }
  applyNotificationsSnapshot(sessionId, message.notifications, workspaceId);
}


function handleOrchestratorRuntimeState(message: ClientBridgeMessage) {
  const store = getState();
  const status = message.status === 'idle'
    || message.status === 'running'
    || message.status === 'waiting'
    || message.status === 'paused'
    || message.status === 'blocked'
    || message.status === 'completed'
    || message.status === 'failed'
    || message.status === 'cancelled'
    ? message.status
    : null;
  const phase = typeof message.phase === 'string' ? message.phase.trim() : '';
  const statusChangedAt = typeof message.statusChangedAt === 'number' && Number.isFinite(message.statusChangedAt)
    ? Math.floor(message.statusChangedAt)
    : null;
  const lastEventAt = typeof message.lastEventAt === 'number' && Number.isFinite(message.lastEventAt)
    ? Math.floor(message.lastEventAt)
    : null;
  if (!status || !phase || statusChangedAt === null || lastEventAt === null) {
    return;
  }
  const sessionId = typeof message.sessionId === 'string' && message.sessionId.trim().length > 0
    ? message.sessionId.trim()
    : undefined;
  const currentSessionId = store.currentSessionId?.trim() || '';
  if (sessionId && currentSessionId && sessionId !== currentSessionId) {
    return;
  }
  const runtimeState: OrchestratorRuntimeState = {
    status,
    phase,
    errors: Array.isArray(message.errors)
      ? message.errors
        .filter((item: unknown): item is string => typeof item === 'string' && item.trim().length > 0)
        .map((item: string) => item.trim())
      : [],
    statusChangedAt,
    lastEventAt,
    assignments: Array.isArray(message.assignments)
      ? (message.assignments as OrchestratorRuntimeState['assignments'])
      : [],
    ...(sessionId ? { sessionId } : {}),
    ...(typeof message.requestId === 'string' && message.requestId.trim().length > 0
      ? { requestId: message.requestId.trim() }
      : {}),
    ...(message.chain && typeof message.chain === 'object'
      ? { chain: message.chain as OrchestratorRuntimeState['chain'] }
      : {}),
    ...(typeof message.statusReason === 'string' && message.statusReason.trim().length > 0
      ? { statusReason: message.statusReason.trim() }
      : {}),
    ...(message.canResume === true ? { canResume: true } : {}),
    ...(typeof message.runtimeReason === 'string' && message.runtimeReason.trim().length > 0
      ? { runtimeReason: message.runtimeReason.trim() }
      : {}),
    ...(typeof message.failureReason === 'string' && message.failureReason.trim().length > 0
      ? { failureReason: message.failureReason.trim() }
      : {}),
    ...(typeof message.startedAt === 'number' && Number.isFinite(message.startedAt) && message.startedAt > 0
      ? { startedAt: Math.floor(message.startedAt) }
      : {}),
    ...(typeof message.endedAt === 'number' && Number.isFinite(message.endedAt) && message.endedAt > 0
      ? { endedAt: Math.floor(message.endedAt) }
      : {}),
    runtimeSnapshot: message.runtimeSnapshot && typeof message.runtimeSnapshot === 'object'
      ? (message.runtimeSnapshot as OrchestratorRuntimeState['runtimeSnapshot'])
      : null,
    runtimeDecisionTrace: Array.isArray(message.runtimeDecisionTrace)
      ? (message.runtimeDecisionTrace as OrchestratorRuntimeState['runtimeDecisionTrace'])
      : [],
    opsView: message.opsView && typeof message.opsView === 'object'
      ? (message.opsView as OrchestratorRuntimeState['opsView'])
      : null,
  };
  setOrchestratorRuntimeState(runtimeState);
}

function handleClarificationRequest(_message: ClientBridgeMessage) {
  showFeedback('info', i18n.t('messageHandler.autoSkipClarification'));
}
/**
 * 处理代理状态更新消息
 * 将检测到的模型状态同步到全局 store，供设置和执行状态共用
 */
function handleWorkerStatusUpdate(message: ClientBridgeMessage) {
  const store = getState();
  const statuses = sanitizeModelStatusMap(message.statuses, store.modelStatus);
  if (Object.keys(statuses).length === 0) return;

  store.modelStatus = { ...store.modelStatus, ...statuses };
}

function handleSettingsBootstrapLoaded(message: ClientBridgeMessage) {
  const store = getState();
  const snapshot = {
    ...message,
  } as unknown as SettingsBootstrapSnapshot;
  if (!settingsBootstrapMatchesCurrentWorkspace(snapshot)) {
    return;
  }
  store.settingsBootstrapSnapshot = snapshot;

  handleWorkerStatusUpdate({
    statuses: message.workerStatuses,
  } as unknown as ClientBridgeMessage);

  const runtimeSettings = (
    message.runtimeSettings
    && typeof message.runtimeSettings === 'object'
    && !Array.isArray(message.runtimeSettings)
  )
    ? message.runtimeSettings as { locale?: unknown }
    : null;
  if (runtimeSettings?.locale === 'zh-CN' || runtimeSettings?.locale === 'en-US') {
    i18n.setLocale(runtimeSettings.locale);
  }
}

/**
 * 处理连接测试结果消息（全局）
 * 将连接测试的状态同步到全局 store，确保即使 SettingsPanel 已卸载，
 * 任务执行状态等其他组件也能获取最新状态。
 */
function handleConnectionTestResult(message: ClientBridgeMessage) {
  const store = getState();
  const success = Boolean(message.success);
  const error = safeModelStatusError('error');

  // 代理连接测试
  const worker = message.worker as string | undefined;
  if (worker) {
    store.modelStatus = {
      ...store.modelStatus,
      [worker]: {
        status: success ? 'available' : 'error',
        model: store.modelStatus[worker]?.model,
        error: success ? undefined : error,
      },
    };
    return;
  }

  // orchestratorConnectionTestResult / auxiliaryConnectionTestResult
  // 通过 dataType 区分，由调用方传入 target
  const target = message._target as 'orchestrator' | 'auxiliary' | undefined;
  if (!target) return;

  if (target === 'orchestrator') {
    store.modelStatus = {
      ...store.modelStatus,
      orchestrator: {
        status: success ? 'available' : 'error',
        model: store.modelStatus.orchestrator?.model,
        error: success ? undefined : error,
      },
    };
  } else if (target === 'auxiliary') {
    if (success) {
      store.modelStatus = {
        ...store.modelStatus,
        auxiliary: {
          status: 'available',
          model: store.modelStatus.auxiliary?.model,
        },
      };
    } else {
      const orchestratorModel = (message.orchestratorModel as string) || store.modelStatus.orchestrator?.model;
      store.modelStatus = {
        ...store.modelStatus,
        auxiliary: {
          status: 'orchestrator',
          model: orchestratorModel || store.modelStatus.auxiliary?.model,
        },
      };
    }
  }
}

// Named exports
export { handleStateUpdate, handleSessionsUpdated, handleEmptyWorkspaceStateLoaded, handleSessionBootstrapLoaded, handleOrchestratorRuntimeState, handleClarificationRequest, handleWorkerStatusUpdate, handleConnectionTestResult };
