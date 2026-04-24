/**
 * DATA / CONTROL / NOTIFY message handlers
 * Extracted mechanically from message-handler.ts.
 */

import type { ClientBridgeMessage } from '../shared/bridges/client-bridge';
import {
  getState,
  patchThreadPlaceholderMessage,
  setIsProcessing,
  setCurrentSessionId,
  getQueuedMessages,
  updateSessions,
  setQueuedMessages,
  setAppState,
  clearPendingInteractions,
  clearAllMessages,
  setTimelineProjection,
  restoreTimelineProjectionIfNewer,
  prependTimelineProjectionPage,
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
  applyTimelineStreamPatch,
  settleProcessingAfterResponseCompletion,
  settleAuthoritativeIdleState,
  applySessionNotifications,
  batchWebviewStatePersistence,
  setEnabledAgents,
  setSessionHistoryState,
  messagesState,
  timelineProjectionConfirmsLocalAssistantResponse,
} from '../stores/messages.svelte';
import type {
  AppState, Message, Session,
  Edit,
  ModelStatusMap, ActivePlanState, PlanLedgerRecord, PlanLedgerAttempt,
  QueuedMessage, OrchestratorRuntimeState, SessionTimelineProjection,
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

function normalizeStateSliceVersion(value: unknown): number {
  return typeof value === 'number' && Number.isFinite(value) ? Math.floor(value) : 0;
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

function applyQueuedMessagesFromAuthoritativeSnapshot(queuedMessages: QueuedMessage[]) {
  const incoming = ensureArray<QueuedMessage>(queuedMessages);
  if (incoming.length > 0 || getQueuedMessages().length === 0) {
    setQueuedMessages(incoming);
  }
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
        snapshotId: change.snapshotId,
        updatedAt: typeof change.updatedAt === 'number' ? change.updatedAt : undefined,
        type: inferredType,
        additions: change.additions,
        deletions: change.deletions,
        diff: change.diff,
        originalContent: change.originalContent,
        previewContent: change.previewContent,
        previewAbsolutePath: change.previewAbsolutePath,
        previewCanOpenWorkspaceFile: change.previewCanOpenWorkspaceFile,
        contributors: change.contributors,
        workerId: change.workerId,
        executionGroupId: (typeof (change as { executionGroupId?: unknown }).executionGroupId === 'string'
          ? (change as { executionGroupId?: string }).executionGroupId
          : undefined)
          || (typeof (change as { missionId?: unknown }).missionId === 'string'
            ? (change as { missionId?: string }).missionId
            : undefined),
      };
    });
}


function handleStateUpdate(message: ClientBridgeMessage) {
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
  applyAuthoritativeProcessingState(state.processingState ?? null);
  if (state.sessions) {
    updateSessions(ensureArray(state.sessions) as Session[]);
  }

  // currentSessionId 属于显式 bootstrap / switch 的会话锚定语义，
  // 不能由常规 stateUpdate 反向覆盖当前浏览器查看的会话。
  // 否则会出现侧边栏 active、URL、主内容三者分裂，破坏 live/restore 单一真相源。

  // timelineProjection / runtime state 不再通过 stateUpdate 覆盖。
  // 当前统一约束：
  // 1. sessionBootstrapLoaded 负责 restore / switch 的原子恢复；
  // 2. 活跃会话的实时内容只由 unified message/update/complete 驱动；
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
      // 任务已被接受，但当前轮仍保持 pending request，
      // 等权威 bootstrap / 流式消息接管后再自然清空，避免接受瞬间出现 processing 空窗。
      const requestId = payload?.requestId as string | undefined;
      if (requestId) {
        // 更新占位消息状态：pending → received
        const binding = getRequestBinding(requestId);
        if (binding) {
          const placeholder = getState().threadMessages.find(m => m.id === binding.placeholderMessageId);
          const baseMetadata = (placeholder?.metadata && typeof placeholder.metadata === 'object')
            ? placeholder.metadata
            : {};
          patchThreadPlaceholderMessage(binding.placeholderMessageId, {
            metadata: {
              ...baseMetadata,
              isPlaceholder: true,
              placeholderState: 'received',
              requestId,
            },
          });
        }
      }
      break;
    }

    case 'task_rejected': {
      const requestId = payload?.requestId as string | undefined;
      const reasonRaw = payload?.reason;
      const reason = typeof reasonRaw === 'string' ? reasonRaw.trim() : '';
      const modelOriginIssue = payload?.modelOriginIssue === true;
      const toastLevel = modelOriginIssue ? 'warning' : 'error';
      const finalReason = reason || i18n.t('messageHandler.requestRejected');

      if (requestId) {
        clearPendingRequest(requestId);

        const binding = getRequestBinding(requestId);
        if (binding?.timeoutId) {
          clearTimeout(binding.timeoutId);
        }

        if (binding) {
          const placeholderId = binding.placeholderMessageId;
          const placeholder = getState().threadMessages.find((m) => m.id === placeholderId);

          if (placeholder && placeholder.metadata?.isPlaceholder) {
            const baseMetadata = (placeholder.metadata && typeof placeholder.metadata === 'object')
              ? placeholder.metadata
              : {};
            patchThreadPlaceholderMessage(placeholderId, {
              ...placeholder,
              role: 'system',
              source: 'orchestrator',
              content: finalReason,
              blocks: [{ type: 'text', content: finalReason }],
              type: 'error',
              noticeType: toastLevel,
              isStreaming: false,
              isComplete: true,
              metadata: {
                ...baseMetadata,
                isPlaceholder: false,
                wasPlaceholder: true,
                placeholderState: undefined,
                requestId,
                ...(modelOriginIssue ? { modelOriginIssue: true } : {}),
              },
            });
            markMessageComplete(placeholderId);
          }

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
      // 任务开始执行后由权威快照和实时流接管，不在这里额外抬升 processing。
      {
        const requestId = payload?.requestId as string | undefined;
        if (requestId) {
          const binding = getRequestBinding(requestId);
          if (binding) {
            const placeholder = getState().threadMessages.find(m => m.id === binding.placeholderMessageId);
            const baseMetadata = (placeholder?.metadata && typeof placeholder.metadata === 'object')
              ? placeholder.metadata
              : {};
            patchThreadPlaceholderMessage(binding.placeholderMessageId, {
              metadata: {
                ...baseMetadata,
                isPlaceholder: true,
                placeholderState: 'thinking',
                requestId,
              },
            });
          }
        }
      }
      break;

    case 'task_completed':
    case 'task_failed': {
      // 请求级终态不能再触发“全局封口”。
      // 否则主线 placeholder 会在真实首答尚未到达时被前端提前删除，
      // 造成线程里只剩 worker 卡、刷新后又从 restore 投影里重新出现的 live/restore 分叉。
      //
      // 统一约束：
      // 1. 真实消息的结束由 unifiedComplete 负责；
      // 2. 整个界面/会话的强制收口由 processingStateChanged(false, forced)
      //    与恢复/切会话等显式场景负责；
      // 3. task_rejected / 错误正文等用户可见消息可显式接管 placeholder。
      break;
    }

    case 'worker_status': {
      // Worker 状态更新：从控制消息同步状态到 UI
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

    case 'queuedMessagesUpdated': {
      const currentSessionId = getState().currentSessionId || '';
      const incomingSessionId = typeof payload.sessionId === 'string' ? payload.sessionId : '';
      if (incomingSessionId && currentSessionId && incomingSessionId !== currentSessionId) {
        break;
      }
      setQueuedMessages(ensureArray<QueuedMessage>(payload.queuedMessages));
      break;
    }

    case 'sessionsUpdated':
      handleSessionsUpdated(asMessage({ sessions: payload.sessions }));
      break;

    case 'emptyWorkspaceStateLoaded':
      handleEmptyWorkspaceStateLoaded(asMessage({
        state: payload.state,
      }));
      break;

    case 'workspaceSessionCleared':
      batchWebviewStatePersistence(() => {
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
        if (!hasPendingLocalTurn) {
          setQueuedMessages([]);
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
        sessions: payload.sessions,
        state: payload.state,
        timelineProjection: payload.timelineProjection,
        notifications: payload.notifications,
        queuedMessages: payload.queuedMessages,
        orchestratorRuntimeState: payload.orchestratorRuntimeState,
        hasMoreBefore: payload.hasMoreBefore,
        beforeCursor: payload.beforeCursor,
      }));
      break;

    case 'timelineProjectionUpdated':
      handleTimelineProjectionUpdated(asMessage({
        sessionId: payload.sessionId,
        timelineProjection: payload.timelineProjection,
      }));
      break;

    case 'sessionNotificationsLoaded':
      handleSessionNotificationsLoaded(asMessage({
        sessionId: payload.sessionId,
        notifications: payload.notifications,
      }));
      break;

    case 'planLedgerLoaded':
    case 'planLedgerUpdated':
      applyPlanLedgerSnapshot(payload);
      break;

    case 'orchestratorRuntimeState':
      handleOrchestratorRuntimeState(asMessage(payload));
      break;

    case 'clarificationRequest':
      handleClarificationRequest(asMessage(payload));
      break;

    case 'workerQuestionRequest':
      handleWorkerQuestionRequest(asMessage(payload));
      break;

    // missionPlanned / assignmentPlanned / assignmentStarted / assignmentCompleted
    // handlers removed — old Mission/Assignment model superseded by Task Graph.

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
      const kind = typeof payload.kind === 'string' ? payload.kind : '';

      // Only surface Completed and Failed status transitions as system messages.
      if (newStatus === 'Completed') {
        const label = title || (kind ? `${kind} task` : 'Task');
        addToast('success', `${label} completed`, undefined, {
          category: 'audit',
          source: 'task-runtime',
          displayMode: 'notification_center',
        });
      } else if (newStatus === 'Failed') {
        const label = title || (kind ? `${kind} task` : 'Task');
        addToast('error', `${label} failed`, undefined, {
          category: 'incident',
          source: 'task-runtime',
          actionRequired: true,
        });
      }
      break;
    }

    case 'messageCreated': {
      const role = typeof payload.role === 'string' ? payload.role : '';
      const content = typeof payload.content === 'string' ? payload.content : '';
      if (role === 'assistant' && content) {
        addToast('info', content.length > 80 ? content.slice(0, 80) + '…' : content, undefined, {
          category: 'audit',
          source: 'message-runtime',
          displayMode: 'notification_center',
        });
      }
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

function applyPlanLedgerSnapshot(payload: Record<string, unknown>) {
  const store = getState();
  const incomingSessionId = typeof payload.sessionId === 'string' ? payload.sessionId.trim() : '';
  const currentSessionId = store.currentSessionId?.trim() || '';
  if (incomingSessionId && currentSessionId && incomingSessionId !== currentSessionId) {
    console.warn('[MessageHandler] 忽略非当前会话的计划账本快照', {
      incomingSessionId,
      currentSessionId,
    });
    return;
  }

  const rawActivePlan = payload.activePlan;
  const normalizedActivePlan: ActivePlanState | null = (
    rawActivePlan
    && typeof rawActivePlan === 'object'
    && typeof (rawActivePlan as ActivePlanState).planId === 'string'
    && typeof (rawActivePlan as ActivePlanState).formattedPlan === 'string'
    && typeof (rawActivePlan as ActivePlanState).updatedAt === 'number'
  )
    ? (rawActivePlan as ActivePlanState)
    : null;

  const normalizedPlanHistory = ensureArray(payload.plans)
    .map((plan) => normalizePlanLedgerRecord(plan))
    .filter((plan): plan is PlanLedgerRecord => Boolean(plan));

  const currentState = (store.appState || {}) as AppState;
  const nextState: AppState = {
    ...currentState,
    activePlan: normalizedActivePlan,
    planHistory: normalizedPlanHistory,
  };
  setAppState(nextState);
}

function normalizePlanLedgerRecord(plan: unknown): PlanLedgerRecord | null {
  if (!plan || typeof plan !== 'object') {
    return null;
  }
  const candidate = plan as PlanLedgerRecord & { missionId?: string; executionGroupId?: string };
  if (typeof candidate.planId !== 'string' || typeof candidate.sessionId !== 'string') {
    return null;
  }
  const { missionId: _legacyMissionId, ...rest } = candidate;
  const normalizedAttempts = ensureArray((candidate as { attempts?: unknown[] }).attempts)
    .filter((attempt): attempt is PlanLedgerAttempt => {
      if (!attempt || typeof attempt !== 'object') return false;
      const a = attempt as PlanLedgerAttempt;
      return typeof a.attemptId === 'string'
        && typeof a.scope === 'string'
        && typeof a.targetId === 'string'
        && typeof a.sequence === 'number'
        && typeof a.status === 'string';
    });
  return {
    ...rest,
    executionGroupId: (typeof candidate.executionGroupId === 'string'
      ? candidate.executionGroupId
      : undefined)
      || (typeof _legacyMissionId === 'string'
        ? _legacyMissionId
        : undefined),
    attempts: normalizedAttempts,
  };
}

function applyTimelineProjectionSnapshot(
  sessionId: string,
  timelineProjection: SessionTimelineProjection,
  options: { hydrateNodes?: boolean } = {},
): void {
  const currentSessionId = getState().currentSessionId || '';
  if (currentSessionId && sessionId && currentSessionId !== sessionId) {
    return;
  }
  setTimelineProjection(timelineProjection, options);
}

function hasRenderableAssistantContent(message: Message): boolean {
  if (typeof message?.content === 'string' && message.content.trim().length > 0) {
    return true;
  }
  return Array.isArray(message?.blocks) && message.blocks.length > 0;
}

function findAssistantByThreadOrder(
  threadMessages: Message[],
  binding: ReturnType<typeof listRequestBindings>[number],
): Message | undefined {
  const userIndex = threadMessages.findIndex((message) => message.id === binding.userMessageId);
  if (userIndex < 0) {
    return undefined;
  }
  for (let index = threadMessages.length - 1; index > userIndex; index -= 1) {
    const message = threadMessages[index];
    if (
      message.role === 'assistant'
      && message.source === 'orchestrator'
      && message.metadata?.isPlaceholder !== true
      && message.isStreaming !== true
      && hasRenderableAssistantContent(message)
    ) {
      return message;
    }
  }
  return undefined;
}

function reconcileRequestBindingsFromAuthoritativeThread(sessionId: string): void {
  const currentSessionId = getState().currentSessionId || '';
  if (!sessionId || !currentSessionId || currentSessionId !== sessionId) {
    return;
  }

  const threadMessages = getState().threadMessages;
  if (!Array.isArray(threadMessages) || threadMessages.length === 0) {
    return;
  }

  for (const binding of listRequestBindings()) {
    const userMessage = threadMessages.find((message) => message.id === binding.userMessageId);
    const lowerBoundTimestamp = typeof userMessage?.timestamp === 'number'
      ? userMessage.timestamp
      : binding.createdAt;
    const matchedAssistant = (
      (binding.realMessageId
        ? threadMessages.find((message) => message.id === binding.realMessageId)
        : undefined)
      || [...threadMessages].reverse().find((message) => (
        message.role === 'assistant'
        && message.source === 'orchestrator'
        && message.metadata?.isPlaceholder !== true
        && message.isStreaming !== true
        && typeof message.timestamp === 'number'
        && message.timestamp >= lowerBoundTimestamp
        && hasRenderableAssistantContent(message)
      ))
      || findAssistantByThreadOrder(threadMessages, binding)
    );

    if (!matchedAssistant) {
      continue;
    }

    const responseDurationMs = Math.max(0, matchedAssistant.timestamp - binding.createdAt);
    const existingMetadata = matchedAssistant.metadata && typeof matchedAssistant.metadata === 'object'
      ? matchedAssistant.metadata
      : {};
    applyTimelineStreamPatch(matchedAssistant.id, {
      metadata: {
        ...existingMetadata,
        responseDurationMs,
      },
    });
    markMessageComplete(binding.placeholderMessageId);
    clearPendingRequest(binding.requestId);
    updateRequestBinding(binding.requestId, {
      realMessageId: matchedAssistant.id,
      timeoutId: undefined,
    });
    if (binding.timeoutId) {
      clearTimeout(binding.timeoutId);
    }
  }

  settleProcessingAfterResponseCompletion();
}

function handleTimelineProjectionUpdated(message: ClientBridgeMessage) {
  const sessionId = typeof message.sessionId === 'string' ? message.sessionId.trim() : '';
  const timelineProjection = message.timelineProjection as SessionTimelineProjection | undefined;
  if (!sessionId || !timelineProjection) {
    return;
  }
  const currentSessionId = getState().currentSessionId || '';
  if (!currentSessionId || currentSessionId !== sessionId) {
    return;
  }
  restoreTimelineProjectionIfNewer(timelineProjection, {
    source: 'authoritative',
  });
  reconcileRequestBindingsFromAuthoritativeThread(sessionId);
}

function handleSessionBootstrapLoaded(message: ClientBridgeMessage) {
  const sessionId = typeof message.sessionId === 'string' ? message.sessionId.trim() : '';
  const timelineProjection = message.timelineProjection as SessionTimelineProjection | undefined;
  const state = message.state as AppState | undefined;
  const hasMoreBefore = message.hasMoreBefore === true;
  const beforeCursor = typeof message.beforeCursor === 'string' && message.beforeCursor.trim()
    ? message.beforeCursor.trim()
    : null;

  if (!sessionId || !timelineProjection || !state) {
    return;
  }

  const currentSessionId = getState().currentSessionId || '';
  const isSameSession = currentSessionId === sessionId;

  // 同 session 恢复（SSE 重连 / 后端重启）：优先保留当前 live 时间线。
  // 仅当后端 bootstrap projection 明确比本地更新时，才允许用快照接管并修复断连期间丢失的节点。
  if (isSameSession) {
    batchWebviewStatePersistence(() => {
      const snapshot = message as ClientBridgeMessage & SessionBootstrapSnapshot;
      const sessions = ensureArray(snapshot.sessions) as Session[];
      if (sessions.length > 0) {
        updateSessions(sessions);
      }
      // 运行态同步：只更新 authoritative snapshot 与非时间轴补充数据。
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
        applySessionNotifications(sessionId, snapshot.notifications.notifications);
      }
      applyQueuedMessagesFromAuthoritativeSnapshot(ensureArray<QueuedMessage>(snapshot.queuedMessages));
      setSessionHistoryState(sessionId, {
        hasMoreBefore,
        beforeCursor,
        isLoadingBefore: false,
      });
      const hasLiveTurn = messagesState.pendingRequests.size > 0 || messagesState.activeMessageIds.size > 0;
      const authoritativeSnapshotIsIdle = state.isProcessing !== true
        && state.processingState?.isProcessing !== true;
      const canAuthoritativeProjectionSettleLiveTurn = (
        hasLiveTurn
        && (
          authoritativeSnapshotIsIdle
          || timelineProjectionConfirmsLocalAssistantResponse(timelineProjection)
        )
      );
      if (!hasLiveTurn || canAuthoritativeProjectionSettleLiveTurn) {
        prependTimelineProjectionPage(sessionId, timelineProjection);
        reconcileRequestBindingsFromAuthoritativeThread(sessionId);
        if (authoritativeSnapshotIsIdle) {
          settleAuthoritativeIdleState();
        }
      }
    });
    return;
  }

  // 跨 session 切换：完整重建
  batchWebviewStatePersistence(() => {
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

    const snapshot = message as ClientBridgeMessage & SessionBootstrapSnapshot;
    const sessions = ensureArray(snapshot.sessions) as Session[];
    if (sessions.length > 0) {
      updateSessions(sessions);
    }

    setCurrentSessionId(sessionId);
    applyTimelineProjectionSnapshot(sessionId, timelineProjection, { hydrateNodes: true });
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
      applySessionNotifications(sessionId, snapshot.notifications.notifications);
    }
    applyQueuedMessagesFromAuthoritativeSnapshot(ensureArray<QueuedMessage>(snapshot.queuedMessages));
    setSessionHistoryState(sessionId, {
      hasMoreBefore,
      beforeCursor,
      isLoadingBefore: false,
    });
    reconcileRequestBindingsFromAuthoritativeThread(sessionId);
  });
}

function handleSessionNotificationsLoaded(message: ClientBridgeMessage) {
  const sessionId = typeof message.sessionId === 'string' ? message.sessionId : '';
  if (!sessionId) {
    return;
  }
  applySessionNotifications(sessionId, message.notifications);
}


function handleOrchestratorRuntimeState(message: ClientBridgeMessage) {
  const store = getState();
  const status = message.status === 'idle'
    || message.status === 'running'
    || message.status === 'waiting'
    || message.status === 'paused'
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

function handleWorkerQuestionRequest(_message: ClientBridgeMessage) {
  addToast('info', i18n.t('messageHandler.autoAnswerWorkerQuestion'));
}



// handleMissionPlanned, handleAssignmentPlanned, handleAssignmentStarted,
// handleAssignmentCompleted, updateAssignmentPlan — removed.
// Old Mission/Assignment incremental handlers superseded by Task Graph model.

/**
 * 处理 Worker 状态更新消息
 * 将检测到的模型状态同步到全局 store，供 BottomTabs 和 SettingsPanel 共用
 */
function handleWorkerStatusUpdate(message: ClientBridgeMessage) {
  const statuses = message.statuses as ModelStatusMap;
  if (!statuses) return;

  const store = getState();

  // 直接存储完整的状态信息，不再简化
  // 这样 BottomTabs 和 SettingsPanel 可以使用同一个数据源
  store.modelStatus = { ...store.modelStatus, ...statuses };
}

function handleSettingsBootstrapLoaded(message: ClientBridgeMessage) {
  const store = getState();
  store.settingsBootstrapSnapshot = {
    ...message,
  } as unknown as SettingsBootstrapSnapshot;

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
 * BottomTabs 等其他组件也能获取最新状态。
 */
function handleConnectionTestResult(message: ClientBridgeMessage) {
  const store = getState();
  const success = Boolean(message.success);
  const error = message.error as string | undefined;

  // Worker 连接测试
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
          error,
        },
      };
    }
  }
}

// Named exports
export { handleStateUpdate, handleSessionsUpdated, handleEmptyWorkspaceStateLoaded, handleSessionBootstrapLoaded, handleTimelineProjectionUpdated, handleOrchestratorRuntimeState, handleClarificationRequest, handleWorkerQuestionRequest, handleWorkerStatusUpdate, handleConnectionTestResult };
