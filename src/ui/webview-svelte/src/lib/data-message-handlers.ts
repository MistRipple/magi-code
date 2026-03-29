/**
 * DATA / CONTROL / NOTIFY message handlers
 * Extracted mechanically from message-handler.ts.
 */

import type { ClientBridgeMessage } from '../../../shared/bridges/client-bridge';
import { postBridgeMessage } from '../../../shared/bridges/bridge-runtime';
import {
  getState,
  patchThreadPlaceholderMessage,
  setIsProcessing,
  setCurrentSessionId,
  updateSessions,
  setQueuedMessages,
  setAppState,
  setMissionPlan,
  clearPendingInteractions,
  clearAllMessages,
  setTimelineProjection,
  restoreTimelineProjectionIfNewer,
  addToast,
  clearPendingRequest,
  setProcessingActor,
  getBackendProcessing,
  getRequestBinding,
  clearRequestBinding,
  clearAllRequestBindings,
  clearProcessingState,
  settleProcessingForManualInteraction,
  sealAllStreamingMessages,
  setOrchestratorRuntimeState,
  applyAuthoritativeProcessingState,
  markMessageComplete,
  getActiveInteractionType,
  applySessionNotifications,
  batchWebviewStatePersistence,
  setInterruptedChain,
} from '../stores/messages.svelte';
import type {
  AppState, Session, MissionPlan, AssignmentPlan,
  AssignmentTodo, Task, SubTaskItem, Edit,
  ModelStatusMap, ActivePlanState, PlanLedgerRecord, PlanLedgerAttempt,
  QueuedMessage, OrchestratorRuntimeState, SessionTimelineProjection,
} from '../types/message';
import type { StandardMessage, ContentBlock as StandardContentBlock } from '../../../../protocol/message-protocol';
import type { SessionBootstrapSnapshot } from '../../../../shared/session-bootstrap';
import { resolveNotificationPresentation } from '../../../../shared/notification-presentation';
import { ensureArray } from './utils';
import { i18n } from '../stores/i18n.svelte';
import {
  rebuildWorkerWaitResultsFromMessages,
  handleRetryRuntimePayload,
} from './message-utils';


function handleStateUpdate(message: ClientBridgeMessage) {
  const state = message.state as AppState;
  if (!state) return;
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

  setAppState(state);
  applyAuthoritativeProcessingState(state.processingState ?? null);
  if (state.locale === 'zh-CN' || state.locale === 'en-US') {
    i18n.setLocale(state.locale);
  }

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

  const store = getState();
  const taskSeen = new Set<string>();
  store.tasks = ensureArray(state.tasks)
    .filter((task): task is Record<string, unknown> => !!task && typeof task === 'object' && typeof (task as Record<string, unknown>).status === 'string')
    .map((task) => {
      const raw = task as Record<string, unknown>;
      const id = typeof raw.id === 'string' && (raw.id as string).trim() ? (raw.id as string).trim() : '';
      if (!id) {
        throw new Error('[MessageHandler] Task 缺少 id');
      }
      if (taskSeen.has(id)) {
        throw new Error(`[MessageHandler] Task id 重复: ${id}`);
      }
      taskSeen.add(id);
      const subTasks: SubTaskItem[] = ensureArray(raw.subTasks)
        .filter((st): st is Record<string, unknown> => !!st && typeof st === 'object')
        .map((st) => ({
          id: String(st.id || ''),
          description: String(st.description || ''),
          title: typeof st.title === 'string' ? st.title : undefined,
          assignedWorker: String(st.assignedWorker || ''),
          assignmentId: String(st.assignmentId || ''),
          source: typeof st.source === 'string' ? st.source : undefined,
          status: String(st.status || 'pending') as SubTaskItem['status'],
          progress: typeof st.progress === 'number' ? st.progress : 0,
          priority: typeof st.priority === 'number' ? st.priority : 3,
          targetFiles: Array.isArray(st.targetFiles) ? st.targetFiles as string[] : [],
          modifiedFiles: Array.isArray(st.modifiedFiles) ? st.modifiedFiles as string[] : undefined,
          error: typeof st.error === 'string' ? st.error : undefined,
          startedAt: typeof st.startedAt === 'number' ? st.startedAt : undefined,
          completedAt: typeof st.completedAt === 'number' ? st.completedAt : undefined,
        }));
      return {
        id,
        name: String(raw.name || raw.prompt || ''),
        description: typeof raw.description === 'string' ? raw.description : undefined,
        status: String(raw.status) as Task['status'],
        deliveryStatus: typeof raw.deliveryStatus === 'string' ? raw.deliveryStatus as Task['deliveryStatus'] : undefined,
        deliverySummary: typeof raw.deliverySummary === 'string' ? raw.deliverySummary : undefined,
        deliveryDetails: typeof raw.deliveryDetails === 'string' ? raw.deliveryDetails : undefined,
        deliveryWarnings: Array.isArray(raw.deliveryWarnings) ? raw.deliveryWarnings as string[] : undefined,
        subTasks,
        progress: typeof raw.progress === 'number' ? raw.progress : 0,
        missionId: typeof raw.missionId === 'string' ? raw.missionId : id,
        failureReason: typeof raw.failureReason === 'string' ? raw.failureReason : undefined,
      } satisfies Task;
    });
  store.edits = ensureArray(state.pendingChanges)
    .filter((change): change is Edit => !!change && typeof change === 'object' && typeof (change as Edit).filePath === 'string' && !!(change as Edit).filePath)
    .map((change) => {
      // 推断变更类型：后端 PendingChange 不含 type，根据增删行数推断
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
        type: inferredType,
        additions: change.additions,
        deletions: change.deletions,
        contributors: change.contributors,
        workerId: change.workerId,
        missionId: change.missionId,
      };
    });
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
      // 阶段变化：仅同步后端运行态
      // 重要：禁止在这里清空 activeMessageIds/pendingRequests，
      // 避免 Worker 仍在流式输出时 Stop 按钮提前恢复。
      {
        const isRunning = payload?.isRunning as boolean | undefined;
        if (isRunning === true) {
          setIsProcessing(true);
        }
      }
      break;

    case 'task_accepted': {
      // 任务被接受：清除中断链状态（resume 或新任务开始）
      setInterruptedChain(null);
      // 防御性检查：backendProcessing 仍为 false 时先设置处理状态
      const requestId = payload?.requestId as string | undefined;
      if (requestId) {
        if (!getBackendProcessing()) {
          // 异常时序：先确保处理状态为 true，避免 isProcessing 出现空窗期
          setIsProcessing(true);
        }
        clearPendingRequest(requestId);

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
      // 任务开始执行
      setIsProcessing(true);
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
      // true 仍可作为兜底提升信号；
      // false 只有在 provider 明确给出 forced idle 时才允许清空，
      // 避免把“当前无活跃消息卡片”误判成“整个系统已经空闲”。
      if (isProcessing === true) {
        setIsProcessing(true);
      } else if (isProcessing === false && transitionKind === 'forced') {
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

    case 'sessionBootstrapLoaded':
      handleSessionBootstrapLoaded(asMessage({
        sessionId: payload.sessionId,
        sessions: payload.sessions,
        state: payload.state,
        timelineProjection: payload.timelineProjection,
        notifications: payload.notifications,
        queuedMessages: payload.queuedMessages,
        orchestratorRuntimeState: payload.orchestratorRuntimeState,
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

    case 'recoveryRequest':
      handleRecoveryRequest(asMessage(payload));
      break;

    case 'orchestratorRuntimeState':
      handleOrchestratorRuntimeState(asMessage(payload));
      break;

    case 'executionChainInterrupted': {
      const chainPayload = payload as Record<string, unknown>;
      const chainId = typeof chainPayload.chainId === 'string' ? chainPayload.chainId : '';
      const recoverable = chainPayload.recoverable === true;
      if (chainId) {
        setInterruptedChain({ chainId, recoverable });
      }
      break;
    }

    case 'clarificationRequest':
      handleClarificationRequest(asMessage(payload));
      break;

    case 'workerQuestionRequest':
      handleWorkerQuestionRequest(asMessage(payload));
      break;

    case 'missionPlanned':
      handleMissionPlanned(asMessage(payload));
      break;

    case 'assignmentPlanned':
      handleAssignmentPlanned(asMessage(payload));
      break;

    case 'assignmentStarted':
      handleAssignmentStarted(asMessage(payload));
      break;

    case 'assignmentCompleted':
      handleAssignmentCompleted(asMessage(payload));
      break;

    case 'todoStarted':
      handleTodoStarted(asMessage(payload));
      break;

    case 'todoCompleted':
      handleTodoCompleted(asMessage(payload));
      break;

    case 'todoFailed':
      handleTodoFailed(asMessage(payload));
      break;

    case 'dynamicTodoAdded':
      handleDynamicTodoAdded(asMessage(payload));
      break;

    case 'todoApprovalRequested':
      handleTodoApprovalRequested(asMessage(payload));
      break;

    case 'workerSessionCreated':
      handleWorkerSessionCreated(asMessage(payload));
      break;

    case 'workerSessionResumed':
      handleWorkerSessionResumed(asMessage(payload));
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

    case 'missionExecutionFailed':
    case 'missionFailed': {
      // Mission 级失败：只同步 backendProcessing=false。
      // activeMessageIds/pendingRequests 应由消息完成链路和请求绑定分别清理。
      setIsProcessing(false);
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
  const candidate = plan as PlanLedgerRecord;
  if (typeof candidate.planId !== 'string' || typeof candidate.sessionId !== 'string') {
    return null;
  }
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
    ...candidate,
    attempts: normalizedAttempts,
  };
}

function applyTimelineProjectionSnapshot(
  sessionId: string,
  timelineProjection: SessionTimelineProjection,
): void {
  const currentSessionId = getState().currentSessionId || '';
  if (currentSessionId && sessionId && currentSessionId !== sessionId) {
    return;
  }
  setTimelineProjection(timelineProjection);
  rebuildWorkerWaitResultsFromMessages(getState().threadMessages, getState().agentOutputs);
}

function handleTimelineProjectionUpdated(message: ClientBridgeMessage) {
  const sessionId = typeof message.sessionId === 'string' ? message.sessionId.trim() : '';
  const timelineProjection = message.timelineProjection as SessionTimelineProjection | undefined;
  if (!sessionId || !timelineProjection) {
    return;
  }
  if (restoreTimelineProjectionIfNewer(timelineProjection)) {
    rebuildWorkerWaitResultsFromMessages(getState().threadMessages, getState().agentOutputs);
  }
}

function handleSessionBootstrapLoaded(message: ClientBridgeMessage) {
  const sessionId = typeof message.sessionId === 'string' ? message.sessionId.trim() : '';
  const timelineProjection = message.timelineProjection as SessionTimelineProjection | undefined;
  const state = message.state as AppState | undefined;

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
      // 运行态同步：tasks 等非时间轴数据
      handleStateUpdate({
        ...message,
        state: {
          ...state,
          currentSessionId: sessionId,
          sessions: sessions.length > 0 ? sessions : state.sessions,
        },
      });
      setOrchestratorRuntimeState(
        (snapshot.orchestratorRuntimeState as OrchestratorRuntimeState | null | undefined) ?? null,
      );
      if (snapshot.notifications) {
        applySessionNotifications(sessionId, snapshot.notifications.notifications);
      }
      setQueuedMessages(ensureArray<QueuedMessage>(snapshot.queuedMessages));
      restoreTimelineProjectionIfNewer(timelineProjection);
      rebuildWorkerWaitResultsFromMessages(getState().threadMessages, getState().agentOutputs);
    });
    return;
  }

  // 跨 session 切换：完整重建
  batchWebviewStatePersistence(() => {
    clearAllMessages({
      persist: false,
      resetTimelineView: false,
      resetPanelState: false,
    });
    clearAllRequestBindings();
    clearPendingInteractions();
    clearProcessingState();

    const snapshot = message as ClientBridgeMessage & SessionBootstrapSnapshot;
    const sessions = ensureArray(snapshot.sessions) as Session[];
    if (sessions.length > 0) {
      updateSessions(sessions);
    }

    setCurrentSessionId(sessionId);
    applyTimelineProjectionSnapshot(sessionId, timelineProjection);
    handleStateUpdate({
      ...message,
      state: {
        ...state,
        currentSessionId: sessionId,
        sessions: sessions.length > 0 ? sessions : state.sessions,
      },
    });
    setOrchestratorRuntimeState(
      (snapshot.orchestratorRuntimeState as OrchestratorRuntimeState | null | undefined) ?? null,
    );
    if (snapshot.notifications) {
      applySessionNotifications(sessionId, snapshot.notifications.notifications);
    }
    setQueuedMessages(ensureArray<QueuedMessage>(snapshot.queuedMessages));
  });
}

function handleSessionNotificationsLoaded(message: ClientBridgeMessage) {
  const sessionId = typeof message.sessionId === 'string' ? message.sessionId : '';
  if (!sessionId) {
    return;
  }
  applySessionNotifications(sessionId, message.notifications);
}


function handleRecoveryRequest(message: ClientBridgeMessage) {
  const canRetry = Boolean(message.canRetry);
  const decision: 'retry' | 'continue' = canRetry
    ? 'retry'
    : 'continue';
  addToast('info', i18n.t('messageHandler.autoRecovery', { decision: i18n.t(decision === 'retry' ? 'messageHandler.autoRecoveryRetry' : 'messageHandler.autoRecoveryContinue') }), undefined, {
    category: 'audit',
    source: 'recovery',
    countUnread: false,
  });
  postBridgeMessage({ type: 'confirmRecovery', decision });
  setIsProcessing(true);
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

function handleClarificationRequest(message: ClientBridgeMessage) {
  addToast('info', i18n.t('messageHandler.autoSkipClarification'));
  postBridgeMessage({
    type: 'answerClarification',
    answers: null,
    additionalInfo: null,
    autoSkipped: true,
  });
  setIsProcessing(true);
}

function handleWorkerQuestionRequest(message: ClientBridgeMessage) {
  addToast('info', i18n.t('messageHandler.autoAnswerWorkerQuestion'));
  postBridgeMessage({ type: 'answerWorkerQuestion', answer: null });
  setIsProcessing(true);
}



function handleMissionPlanned(message: ClientBridgeMessage) {
  const missionId = typeof message.missionId === 'string' && message.missionId.trim() ? message.missionId.trim() : '';
  if (!missionId) {
    throw new Error('[MessageHandler] MissionPlanned 缺少 missionId');
  }
  const assignments = ensureArray(message.assignments) as any[];
  const assignmentSeen = new Set<string>();
  const mappedAssignments: AssignmentPlan[] = assignments
    .filter((assignment) => assignment && typeof assignment === 'object')
    .map((assignment) => {
      const assignmentId = typeof assignment.id === 'string' && assignment.id.trim() ? assignment.id.trim() : '';
      if (!assignmentId) {
        throw new Error('[MessageHandler] MissionPlanned assignment 缺少 id');
      }
      if (assignmentSeen.has(assignmentId)) {
        throw new Error(`[MessageHandler] MissionPlanned assignment id 重复: ${assignmentId}`);
      }
      assignmentSeen.add(assignmentId);
      const todoSeen = new Set<string>();
      const todos = ensureArray(assignment.todos)
        .filter((todo: any) => !!todo && typeof todo === 'object')
        .map((todo: any) => {
          const todoId = typeof todo.id === 'string' && todo.id.trim() ? todo.id.trim() : '';
          if (!todoId) {
            throw new Error('[MessageHandler] MissionPlanned todo 缺少 id');
          }
          if (todoSeen.has(todoId)) {
            throw new Error(`[MessageHandler] MissionPlanned todo id 重复: ${todoId}`);
          }
          todoSeen.add(todoId);
          return {
            id: todoId,
            assignmentId,
            parentId: todo.parentId,
            source: todo.source,
            content: todo.content || '',
            reasoning: todo.reasoning,
            expectedOutput: todo.expectedOutput,
            type: todo.type || 'implementation',
            priority: typeof todo.priority === 'number' ? todo.priority : 3,
            status: todo.status || 'pending',
            outOfScope: Boolean(todo.outOfScope),
            approvalStatus: todo.approvalStatus,
            approvalNote: todo.approvalNote,
          } as AssignmentTodo;
        });
      return {
        id: assignmentId,
        workerId: assignment.workerId,
        responsibility: assignment.responsibility,
        status: assignment.status,
        progress: assignment.progress,
        todos,
      };
    });
  const plan: MissionPlan = { missionId, assignments: mappedAssignments };
  setMissionPlan(plan);
}

function handleAssignmentPlanned(message: ClientBridgeMessage) {
  const assignmentId = typeof message.assignmentId === 'string' && message.assignmentId.trim()
    ? message.assignmentId.trim()
    : '';
  if (!assignmentId) {
    throw new Error('[MessageHandler] AssignmentPlanned 缺少 assignmentId');
  }
  const todoSeen = new Set<string>();
  const todos = ensureArray(message.todos)
    .filter((todo: any) => !!todo && typeof todo === 'object')
    .map((todo: any) => {
      const todoId = typeof todo.id === 'string' && todo.id.trim() ? todo.id.trim() : '';
      if (!todoId) {
        throw new Error('[MessageHandler] AssignmentPlanned todo 缺少 id');
      }
      if (todoSeen.has(todoId)) {
        throw new Error(`[MessageHandler] AssignmentPlanned todo id 重复: ${todoId}`);
      }
      todoSeen.add(todoId);
      return {
        id: todoId,
        assignmentId,
        parentId: todo.parentId,
        source: todo.source,
        content: todo.content || '',
        reasoning: todo.reasoning,
        expectedOutput: todo.expectedOutput,
        type: todo.type || 'implementation',
        priority: typeof todo.priority === 'number' ? todo.priority : 3,
        status: todo.status || 'pending',
        outOfScope: Boolean(todo.outOfScope),
        approvalStatus: todo.approvalStatus,
        approvalNote: todo.approvalNote,
      };
    });

  updateAssignmentPlan(assignmentId, (assignment) => ({
    ...assignment,
    todos,
  }));
}

function handleAssignmentStarted(message: ClientBridgeMessage) {
  const assignmentId = message.assignmentId as string;
  if (!assignmentId || !assignmentId.trim()) {
    console.warn('[MessageHandler] AssignmentStarted 缺少 assignmentId，已忽略', message);
    return;
  }
  updateAssignmentPlan(assignmentId, (assignment) => ({
    ...assignment,
    status: 'running',
  }));
}

function handleAssignmentCompleted(message: ClientBridgeMessage) {
  const assignmentId = message.assignmentId as string;
  if (!assignmentId || !assignmentId.trim()) {
    console.warn('[MessageHandler] AssignmentCompleted 缺少 assignmentId，已忽略', message);
    return;
  }
  const success = Boolean(message.success);
  updateAssignmentPlan(assignmentId, (assignment) => ({
    ...assignment,
    status: success ? 'completed' : 'failed',
    progress: success ? 100 : assignment.progress,
  }));
}

function handleTodoStarted(message: ClientBridgeMessage) {
  const assignmentId = message.assignmentId as string;
  const todoId = message.todoId as string;
  if (!assignmentId || !assignmentId.trim()) {
    throw new Error('[MessageHandler] TodoStarted 缺少 assignmentId');
  }
  if (!todoId || !todoId.trim()) {
    throw new Error('[MessageHandler] TodoStarted 缺少 todoId');
  }
  updateTodo(assignmentId, todoId, (todo) => ({
    ...todo,
    status: 'in_progress',
  }));
}

function handleTodoCompleted(message: ClientBridgeMessage) {
  const assignmentId = message.assignmentId as string;
  const todoId = message.todoId as string;
  if (!assignmentId || !assignmentId.trim()) {
    throw new Error('[MessageHandler] TodoCompleted 缺少 assignmentId');
  }
  if (!todoId || !todoId.trim()) {
    throw new Error('[MessageHandler] TodoCompleted 缺少 todoId');
  }
  updateTodo(assignmentId, todoId, (todo) => ({
    ...todo,
    status: 'completed',
  }));
}

function handleTodoFailed(message: ClientBridgeMessage) {
  const assignmentId = message.assignmentId as string;
  const todoId = message.todoId as string;
  if (!assignmentId || !assignmentId.trim()) {
    throw new Error('[MessageHandler] TodoFailed 缺少 assignmentId');
  }
  if (!todoId || !todoId.trim()) {
    throw new Error('[MessageHandler] TodoFailed 缺少 todoId');
  }
  updateTodo(assignmentId, todoId, (todo) => ({
    ...todo,
    status: 'failed',
  }));
}

function handleDynamicTodoAdded(message: ClientBridgeMessage) {
  const assignmentId = message.assignmentId as string;
  if (!assignmentId || !assignmentId.trim()) {
    throw new Error('[MessageHandler] DynamicTodoAdded 缺少 assignmentId');
  }
  const todo = message.todo as any;
  if (!todo || typeof todo !== 'object') {
    throw new Error('[MessageHandler] DynamicTodoAdded 缺少 todo');
  }
  const todoId = typeof todo.id === 'string' && todo.id.trim() ? todo.id.trim() : '';
  if (!todoId) {
    throw new Error('[MessageHandler] DynamicTodoAdded todo 缺少 id');
  }
  const newTodo: AssignmentTodo = {
    id: todoId,
    assignmentId,
    parentId: todo?.parentId,
    source: todo?.source,
    content: todo?.content || '',
    reasoning: todo?.reasoning,
    expectedOutput: todo?.expectedOutput,
    type: todo?.type || 'implementation',
    priority: typeof todo?.priority === 'number' ? todo.priority : 3,
    status: todo?.status || 'pending',
    outOfScope: Boolean(todo?.outOfScope),
    approvalStatus: todo?.approvalStatus,
    approvalNote: todo?.approvalNote,
  };
  updateAssignmentPlan(assignmentId, (assignment) => ({
    ...assignment,
    todos: [...assignment.todos, newTodo],
  }));
}

function handleTodoApprovalRequested(message: ClientBridgeMessage) {
  const assignmentId = message.assignmentId as string;
  const todoId = message.todoId as string;
  const reason = message.reason as string;
  if (!assignmentId || !assignmentId.trim()) {
    throw new Error('[MessageHandler] TodoApprovalRequested 缺少 assignmentId');
  }
  if (!todoId || !todoId.trim()) {
    throw new Error('[MessageHandler] TodoApprovalRequested 缺少 todoId');
  }
  if (!reason || !reason.trim()) {
    throw new Error('[MessageHandler] TodoApprovalRequested 缺少 reason');
  }
  const store = getState();
  updateTodo(assignmentId, todoId, (todo) => ({
    ...todo,
    approvalStatus: 'approved',
    approvalNote: reason,
  }));
  postBridgeMessage({
    type: 'interactionResponse',
    requestId: `approval-${todoId}`,
    response: 'approved',
  });
}


function updateAssignmentPlan(assignmentId: string, updater: (assignment: AssignmentPlan) => AssignmentPlan) {
  const store = getState();
  const planMap = store.missionPlan;
  for (const [, plan] of planMap) {
    const index = plan.assignments.findIndex((a) => a.id === assignmentId);
    if (index !== -1) {
      const nextAssignments = plan.assignments.map((assignment, i) =>
        i === index ? updater(assignment) : assignment
      );
      setMissionPlan({ ...plan, assignments: nextAssignments });
      return;
    }
  }
}

function updateTodo(
  assignmentId: string,
  todoId: string,
  updater: (todo: AssignmentTodo) => AssignmentTodo
) {
  updateAssignmentPlan(assignmentId, (assignment) => {
    const idx = assignment.todos.findIndex((todo) => todo.id === todoId);
    if (idx === -1) {
      const placeholder: AssignmentTodo = {
        id: todoId,
        assignmentId,
        content: '',
        type: 'implementation',
        priority: 3,
        status: 'pending',
      };
      return { ...assignment, todos: [...assignment.todos, updater(placeholder)] };
    }
    const nextTodos = assignment.todos.map((todo, i) => (i === idx ? updater(todo) : todo));
    return { ...assignment, todos: nextTodos };
  });
}

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

// ============ Worker Session 事件处理（提案 4.1） ============

function handleWorkerSessionCreated(message: ClientBridgeMessage) {
  const sessionId = (message.sessionId as string) || '';
  const assignmentId = (message.assignmentId as string) || '';
  const workerId = (message.workerId as string) || '';

  if (!sessionId) {
    throw new Error('[MessageHandler] WorkerSessionCreated 缺少 sessionId');
  }
  if (!assignmentId) {
    throw new Error('[MessageHandler] WorkerSessionCreated 缺少 assignmentId');
  }
  if (!workerId) {
    throw new Error('[MessageHandler] WorkerSessionCreated 缺少 workerId');
  }
}

function handleWorkerSessionResumed(message: ClientBridgeMessage) {
  const sessionId = (message.sessionId as string) || '';
  const assignmentId = (message.assignmentId as string) || '';
  const workerId = (message.workerId as string) || '';

  if (!sessionId) {
    throw new Error('[MessageHandler] WorkerSessionResumed 缺少 sessionId');
  }
  if (!assignmentId) {
    throw new Error('[MessageHandler] WorkerSessionResumed 缺少 assignmentId');
  }
  if (!workerId) {
    throw new Error('[MessageHandler] WorkerSessionResumed 缺少 workerId');
  }

  // 执行态统一从 workerRuntime 派生，前端不再维护本地 worker session 副本。
  // 系统通知由 MessageHub 下发，前端不再本地创建。
}


// Named exports
export { handleStateUpdate, handleSessionsUpdated, handleSessionBootstrapLoaded, handleRecoveryRequest, handleOrchestratorRuntimeState, handleClarificationRequest, handleWorkerQuestionRequest, handleMissionPlanned, handleAssignmentPlanned, handleAssignmentStarted, handleAssignmentCompleted, handleTodoStarted, handleTodoCompleted, handleTodoFailed, handleDynamicTodoAdded, handleTodoApprovalRequested, handleWorkerStatusUpdate, handleConnectionTestResult, handleWorkerSessionCreated, handleWorkerSessionResumed, updateAssignmentPlan, updateTodo };
