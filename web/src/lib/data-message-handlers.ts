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
  updateSessions,
  setQueuedMessages,
  setAppState,
  clearPendingInteractions,
  clearAllMessages,
  setCanonicalTimelineProjection,
  addToast,
  clearPendingRequest,
  setProcessingActor,
  getRequestBinding,
  clearRequestBinding,
  listRequestBindings,
  clearAllRequestBindings,
  clearProcessingState,
  sealAllStreamingMessages,
  setOrchestratorRuntimeState,
  replaceOrchestratorRuntimeState,
  applyAuthoritativeProcessingState,
  markMessageComplete,
  updateRequestBinding,
  getTimelineProjectionMessageById,
  settleProcessingAfterResponseCompletion,
  settleAuthoritativeIdleState,
  applySessionNotifications,
  applySessionNotificationsStatus,
  batchWebviewStatePersistence,
  setEnabledAgents,
  setSessionHistoryState,
  messagesState,
  hasActiveLocalTimelineTurn,
} from '../stores/messages.svelte';
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
import { settingsBootstrapMatchesCurrentWorkspace } from '../web/agent-api';
import type { CanonicalTurn, CanonicalTurnEvent } from '../shared/protocol/canonical-turn';
import { isCanonicalTerminalStatus } from '../shared/protocol/canonical-turn';
import { deriveProcessingStateFromCanonicalTurns } from '../shared/protocol/canonical-processing';
import {
  applyCanonicalTurnEvent,
  clearCanonicalSessionTurns,
  replaceCanonicalSessionTurns,
} from '../stores/turn-store.svelte';

function normalizeStateSliceVersion(value: unknown): number {
  return typeof value === 'number' && Number.isFinite(value) ? Math.floor(value) : 0;
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
  if (state.sessions) {
    updateSessions(ensureArray(state.sessions) as Session[]);
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

  if (state.recovered === true) {
    sealAllStreamingMessages();
  }
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
      const finalReason = i18n.t('messageHandler.requestRejected');
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

      addToast(toastLevel, finalReason, undefined, {
        category: 'incident',
        source: modelOriginIssue ? 'model-runtime' : 'task-runtime',
        actionRequired: true,
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
  addToast(presentation.level, content, presentation.title, {
    category: presentation.category,
    source: presentation.source,
    actionRequired: presentation.actionRequired,
    persistToCenter: presentation.persistToCenter,
    countUnread: presentation.countUnread,
    displayMode: presentation.displayMode,
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
      // 当前只接受强制 idle 终态信号。
      // processing=true 统一由本地 pending request 或后端 authoritative snapshot 驱动，
      // 这里不再保留兜底抬升路径，避免处理态出现双真相源。
      if (isProcessing === false && transitionKind === 'forced') {
        clearProcessingState();
        // forced idle 代表“全局终态已确认”，需要同步封口残留流式内容，避免 UI 仍显示执行中动画。
        sealAllStreamingMessages();
      }
      const source = payload.source as string | undefined;
      const agent = payload.agent as string | undefined;
      if (source) {
        setProcessingActor(source, agent);
      }
      break;
    }

    case 'sessionsUpdated':
      handleSessionsUpdated(asMessage({ sessions: payload.sessions }));
      break;

    case 'emptyWorkspaceStateLoaded':
      handleEmptyWorkspaceStateLoaded(asMessage({
        state: payload.state,
        workspaceId: payload.workspaceId,
        workspacePath: payload.workspacePath,
      }));
      break;

    case 'workspaceSessionCleared':
      batchWebviewStatePersistence(() => {
        messagesState.sessionHydrating = false;
        const hasPendingLocalTurn = messagesState.pendingRequests.size > 0;
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
        }
        setCurrentSessionId(null);
        messagesState.currentWorkspaceId = typeof payload.workspaceId === 'string' && payload.workspaceId.trim()
          ? payload.workspaceId.trim()
          : messagesState.currentWorkspaceId;
        messagesState.currentWorkspacePath = typeof payload.workspacePath === 'string' ? payload.workspacePath.trim() : '';
        if (!hasPendingLocalTurn) {
          setQueuedMessages([]);
          clearCanonicalSessionTurns();
        }
        setOrchestratorRuntimeState(null);
        setAppState({
          ...buildEmptyWorkspaceAppState(Date.now()),
          currentSessionId: '',
        });
      });
      break;

    case 'sessionBootstrapLoaded':
      handleSessionBootstrapLoaded(asMessage({
        sessionId: payload.sessionId,
        workspace: payload.workspace,
        sessions: payload.sessions,
        state: payload.state,
        canonicalTurns: payload.canonicalTurns,
        notifications: payload.notifications,
        orchestratorRuntimeState: payload.orchestratorRuntimeState,
        hasMoreBefore: payload.hasMoreBefore,
        beforeCursor: payload.beforeCursor,
      }));
      break;

    case 'sessionTurnAccepted': {
      const sessionId = typeof payload.sessionId === 'string' ? payload.sessionId.trim() : '';
      if (sessionId) {
        adoptCurrentSessionIdForLiveTurn(sessionId);
      }
      break;
    }

    case 'sessionTurnCanonicalEventUpdated':
      handleSessionTurnCanonicalEventUpdated(asMessage({
        sessionId: payload.sessionId,
        canonicalEvent: payload.canonicalEvent,
      }));
      break;

    case 'sessionNotificationsLoaded':
      handleSessionNotificationsLoaded(asMessage({
        sessionId: payload.sessionId,
        workspaceId: payload.workspaceId,
        notifications: payload.notifications,
      }));
      break;

    case 'sessionNotificationsStatus':
      applySessionNotificationsStatus(payload);
      break;

    case 'orchestratorRuntimeState':
      handleOrchestratorRuntimeState(asMessage(payload));
      break;

    case 'clarificationRequest':
      handleClarificationRequest(asMessage(payload));
      break;

    // missionPlanned / assignmentPlanned / assignmentStarted / assignmentCompleted
    // handlers removed — old Mission/Assignment model superseded by Task Projection.

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

      // 当前只弹出失败态，完成态由任务面板和时间线自然呈现，避免重复通知。
      if (newStatus === 'Failed') {
        addToast('error', i18n.t('messageHandler.taskFailedNotification', { label }), undefined, {
          category: 'incident',
          source: 'task-runtime',
          actionRequired: true,
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
  if (sessions) {
    updateSessions(ensureArray(sessions));
  }
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
    }
    updateSessions([]);
    setCurrentSessionId(null);
    messagesState.currentWorkspaceId = workspaceId || messagesState.currentWorkspaceId;
    messagesState.currentWorkspacePath = workspacePath;
    setAppState({
      ...state,
      sessions: [],
      currentSessionId: '',
      pendingChanges: [],
      tasks: [],
      edits: [],
      isProcessing: false,
      processingState: null,
    });
    setOrchestratorRuntimeState(null);
    if (!hasPendingLocalTurn) {
      setQueuedMessages([]);
    }
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

function reconcileRequestBindingsFromAuthoritativeThread(sessionId: string): void {
  const currentSessionId = getState().currentSessionId || '';
  if (!sessionId || !currentSessionId || currentSessionId !== sessionId) {
    return;
  }

  for (const binding of listRequestBindings()) {
    const matchedAssistant = findTerminalAssistantByRequestIdentity(binding);

    if (!matchedAssistant) {
      continue;
    }

    markMessageComplete(matchedAssistant.id);
    clearPendingRequest(binding.requestId);
    updateRequestBinding(binding.requestId, {
      realMessageId: matchedAssistant.id,
      timeoutId: undefined,
    });
    if (binding.timeoutId) {
      clearTimeout(binding.timeoutId);
    }
    clearRequestBinding(binding.requestId);
  }

  settleProcessingAfterResponseCompletion();
}

function handleSessionTurnCanonicalEventUpdated(message: ClientBridgeMessage) {
  const sessionId = typeof message.sessionId === 'string' ? message.sessionId.trim() : '';
  const canonicalEvent = message.canonicalEvent as CanonicalTurnEvent | undefined;
  if (!sessionId || !canonicalEvent || canonicalEvent.sessionId !== sessionId) {
    return;
  }
  adoptCurrentSessionIdForLiveTurn(sessionId);
  const projection = applyCanonicalTurnEvent(canonicalEvent);
  if (projection) {
    setCanonicalTimelineProjection(projection);
    if (canonicalEvent.turn) {
      const processingState = deriveProcessingStateFromCanonicalTurns([canonicalEvent.turn], sessionId);
      applyAuthoritativeProcessingState(processingState);
      if (!processingState && isCanonicalTerminalStatus(canonicalEvent.turn.status)) {
        clearProcessingState();
      }
    }
    reconcileRequestBindingsFromAuthoritativeThread(sessionId);
  }
}

function applyCanonicalTurnsSnapshot(sessionId: string, turns: unknown): boolean {
  if (!Array.isArray(turns)) {
    return false;
  }
  const canonicalTurns = (turns as CanonicalTurn[])
    .filter((turn) => turn?.sessionId === sessionId)
    .sort((left, right) => left.turnSeq - right.turnSeq || left.turnId.localeCompare(right.turnId));
  const projection = replaceCanonicalSessionTurns(sessionId, canonicalTurns);
  if (!projection) {
    return false;
  }
  setCanonicalTimelineProjection(projection);
  return true;
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

  if (!state) {
    return;
  }
  if (!sessionId) {
    const snapshot = message as ClientBridgeMessage & SessionBootstrapSnapshot;
    const sessions = ensureArray(snapshot.sessions) as Session[];
    const hasPendingLocalTurn = messagesState.pendingRequests.size > 0;
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
      updateSessions(sessions);
      setCurrentSessionId(null);
      messagesState.currentWorkspaceId = workspaceId || messagesState.currentWorkspaceId;
      messagesState.currentWorkspacePath = workspacePath;
      setSessionHistoryState(null, { workspaceId });
      setAppState({
        ...state,
        sessions,
        currentSession: undefined,
        currentSessionId: '',
        isProcessing: false,
        processingState: null,
      });
      setOrchestratorRuntimeState(null);
    });
    return;
  }

  const currentSessionId = getState().currentSessionId || '';
  const isSameSession = currentSessionId === sessionId;

  // 同 session 恢复（SSE 重连 / 后端重启）：活跃轮次期间只同步非时间线状态，
  // 避免 bootstrap 快照整包替换 live 过程态；空闲时再接管权威历史投影。
  if (isSameSession) {
    batchWebviewStatePersistence(() => {
      messagesState.sessionHydrating = false;
      const snapshot = message as ClientBridgeMessage & SessionBootstrapSnapshot;
      const sessions = ensureArray(snapshot.sessions) as Session[];
      if (sessions.length > 0) {
        updateSessions(sessions);
      }
      messagesState.currentWorkspaceId = workspaceId || messagesState.currentWorkspaceId;
      messagesState.currentWorkspacePath = workspacePath || messagesState.currentWorkspacePath;
      const hadLiveTurnBeforeSnapshot = hasActiveLocalTimelineTurn();
      const hadPendingLocalRequestBeforeSnapshot = hasPendingLocalRequest();
      const authoritativeSnapshotIsIdle = state.isProcessing !== true
        && state.processingState?.isProcessing !== true;
      const preserveLocalTurnDuringStaleIdle = hadLiveTurnBeforeSnapshot
        && hadPendingLocalRequestBeforeSnapshot
        && authoritativeSnapshotIsIdle;

      handleStateUpdate({
        ...message,
        state: {
          ...state,
          currentSessionId: sessionId,
          sessions: sessions.length > 0 ? sessions : state.sessions,
        },
      }, { preserveLocalProcessing: preserveLocalTurnDuringStaleIdle });

      if (!preserveLocalTurnDuringStaleIdle) {
        replaceOrchestratorRuntimeState(
          (snapshot.orchestratorRuntimeState as OrchestratorRuntimeState | null | undefined) ?? null,
        );
      }

      if (snapshot.notifications) {
        applySessionNotifications(sessionId, snapshot.notifications.notifications, workspaceId);
      }
      setSessionHistoryState(sessionId, {
        workspaceId,
        hasMoreBefore,
        beforeCursor,
        isLoadingBefore: false,
        preserveLoadedWindow: true,
      });

      if (!hadLiveTurnBeforeSnapshot) {
        applyCanonicalTurnsSnapshot(sessionId, canonicalTurns);
        reconcileRequestBindingsFromAuthoritativeThread(sessionId);
      }
      if (authoritativeSnapshotIsIdle && !hadLiveTurnBeforeSnapshot) {
        settleAuthoritativeIdleState();
      }
    });
    return;
  }

  // 跨 session 切换：完整重建
  batchWebviewStatePersistence(() => {
    messagesState.sessionHydrating = false;
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
    if (sessions.length > 0) {
      updateSessions(sessions);
    }
    messagesState.currentWorkspaceId = workspaceId || messagesState.currentWorkspaceId;
    messagesState.currentWorkspacePath = workspacePath || messagesState.currentWorkspacePath;

    setCurrentSessionId(sessionId);
    applyCanonicalTurnsSnapshot(sessionId, canonicalTurns);
    handleStateUpdate({
      ...message,
      state: {
        ...state,
        currentSessionId: sessionId,
        sessions: sessions.length > 0 ? sessions : state.sessions,
      },
    });
    replaceOrchestratorRuntimeState(
      (snapshot.orchestratorRuntimeState as OrchestratorRuntimeState | null | undefined) ?? null,
    );
    if (snapshot.notifications) {
      applySessionNotifications(sessionId, snapshot.notifications.notifications, workspaceId);
    }
    setSessionHistoryState(sessionId, {
      workspaceId,
      hasMoreBefore,
      beforeCursor,
      isLoadingBefore: false,
    });
    reconcileRequestBindingsFromAuthoritativeThread(sessionId);
  });
}

function handleSessionNotificationsLoaded(message: ClientBridgeMessage) {
  const sessionId = typeof message.sessionId === 'string' ? message.sessionId : '';
  const workspaceId = typeof message.workspaceId === 'string' ? message.workspaceId : '';
  if (!sessionId) {
    return;
  }
  applySessionNotifications(sessionId, message.notifications, workspaceId);
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
  addToast('info', i18n.t('messageHandler.autoSkipClarification'));
}




// handleMissionPlanned, handleAssignmentPlanned, handleAssignmentStarted,
// handleAssignmentCompleted, updateAssignmentPlan — removed.
// Old Mission/Assignment incremental handlers superseded by Task Projection model.

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
