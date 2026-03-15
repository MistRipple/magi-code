/**
 * 消息处理器 - 处理来自 VS Code 扩展的消息
 */

import { vscode, type WebviewMessage } from '../lib/vscode-bridge';
import {
  getState,
  addThreadMessage,
  updateThreadMessage,
  addAgentMessage,
  updateAgentMessage,
  replaceThreadMessage,
  removeThreadMessage,
  setIsProcessing,
  setCurrentSessionId,
  updateSessions,
  setQueuedMessages,
  setAppState,
  setMissionPlan,
  updateWorkerWaitResults,
  setInteractionMode,
  getRequestedInteractionMode,
  clearRequestedInteractionMode,
  clearPendingInteractions,
  clearAllMessages,
  setThreadMessages,
  setAgentOutputs,
  addToast,
  addWorkerSession,
  updateWorkerSession,
  markMessageActive,
  markMessageComplete,
  addPendingRequest,
  clearPendingRequest,
  setProcessingActor,
  getBackendProcessing,
  getActiveInteractionType,
  getRequestBinding,
  createRequestBinding,
  updateRequestBinding,
  clearRequestBinding,
  clearAllRequestBindings,
  clearProcessingState,
  settleProcessingForManualInteraction,
  setRetryRuntime,
  clearRetryRuntime,
  sealAllStreamingMessages,
  setOrchestratorRuntimeDiagnostics,
} from '../stores/messages.svelte';
import type { Message, AppState, Session, ContentBlock, ToolCall, ThinkingBlock, MissionPlan, AssignmentPlan, AssignmentTodo, WorkerSessionState, Task, SubTaskItem, Edit, ModelStatusMap, ActivePlanState, PlanLedgerRecord, PlanLedgerAttempt, QueuedMessage, RetryRuntimeState, AgentType, WaitForWorkersResult, WaitForWorkersResultItem, OrchestratorRuntimeDiagnostics } from '../types/message';
import type { StandardMessage, StreamUpdate, ContentBlock as StandardContentBlock } from '../../../../protocol/message-protocol';
import { MessageType, MessageCategory } from '../../../../protocol/message-protocol';
import { routeStandardMessage, getMessageTarget, clearMessageTargets, clearMessageTarget, setMessageTarget } from './message-router';
import { normalizeWorkerSlot } from './message-classifier';
import { buildAssignmentTaskCardKey, buildWaitResultFromTaskCardMessage, resolveTaskCardKeyFromMetadata, resolveTaskCardScopeId } from './task-card-runtime';
import { ensureArray } from './utils';
import { i18n } from '../stores/i18n.svelte';
import { terminalSessions, type TerminalStreamEventPayload } from '../stores/terminal-sessions.svelte';

function normalizeRestoredMessages(messages: Message[]): Message[] {
  const seen = new Set<string>();
  const normalized: Message[] = [];
  for (const msg of ensureArray<Message>(messages)) {
    if (!msg || typeof msg !== 'object') {
      throw new Error('[MessageHandler] 恢复消息包含无效对象');
    }
    const rawId = typeof msg.id === 'string' ? msg.id.trim() : '';
    if (!rawId) {
      throw new Error('[MessageHandler] 恢复消息缺少 id');
    }
    if (seen.has(rawId)) {
      throw new Error(`[MessageHandler] 恢复消息 id 重复: ${rawId}`);
    }
    seen.add(rawId);
    normalized.push({ ...msg, id: rawId });
  }
  return normalized;
}

function assertStandardMessageId(standard: StandardMessage): StandardMessage {
  if (standard.id && standard.id.trim()) {
    return standard;
  }
  throw new Error('[MessageHandler] 标准消息缺少 id');
}

function extractTextFromStandardBlocks(blocks?: StandardContentBlock[]): string {
  if (!Array.isArray(blocks) || blocks.length === 0) return '';
  return blocks
    .filter((block) => block.type === 'text' || block.type === 'thinking')
    .map((block) => (block as any).content || '')
    .filter(Boolean)
    .join('\n');
}

const WAIT_RESULT_STATUS_SET = new Set<WaitForWorkersResultItem['status']>([
  'completed',
  'failed',
  'skipped',
  'cancelled',
]);
const WAIT_RESULT_WAIT_STATUS_SET = new Set<WaitForWorkersResult['wait_status']>([
  'completed',
  'timeout',
]);

function normalizeWaitResultItem(raw: Record<string, unknown>): WaitForWorkersResultItem | null {
  const taskId = typeof raw.task_id === 'string' ? raw.task_id.trim() : '';
  const worker = typeof raw.worker === 'string' ? raw.worker.trim() : '';
  const statusRaw = typeof raw.status === 'string' ? raw.status.trim() : '';
  if (!taskId || !worker || !WAIT_RESULT_STATUS_SET.has(statusRaw as WaitForWorkersResultItem['status'])) {
    return null;
  }
  const summary = typeof raw.summary === 'string' ? raw.summary : '';
  const modifiedFiles = Array.isArray(raw.modified_files)
    ? raw.modified_files.filter((file): file is string => typeof file === 'string' && file.trim().length > 0)
    : [];
  const errors = Array.isArray(raw.errors)
    ? raw.errors.filter((err): err is string => typeof err === 'string' && err.trim().length > 0)
    : undefined;
  return {
    task_id: taskId,
    worker,
    status: statusRaw as WaitForWorkersResultItem['status'],
    summary,
    modified_files: modifiedFiles,
    ...(errors && errors.length > 0 ? { errors } : {}),
  };
}

function parseWaitForWorkersPayload(raw: unknown, timestamp: number): WaitForWorkersResult | null {
  if (!raw) return null;
  const payload = typeof raw === 'string' ? safeParseJson(raw) : (raw as Record<string, unknown>);
  if (!payload || typeof payload !== 'object') return null;
  const waitStatusRaw = typeof payload.wait_status === 'string' ? payload.wait_status.trim() : '';
  if (!WAIT_RESULT_WAIT_STATUS_SET.has(waitStatusRaw as WaitForWorkersResult['wait_status'])) {
    return null;
  }
  const results = ensureArray(payload.results)
    .filter((item): item is Record<string, unknown> => !!item && typeof item === 'object')
    .map(normalizeWaitResultItem)
    .filter((item): item is WaitForWorkersResultItem => !!item);
  const pendingTaskIds = ensureArray(payload.pending_task_ids)
    .filter((item): item is string => typeof item === 'string' && item.trim().length > 0);
  const waitedMs = typeof payload.waited_ms === 'number' && Number.isFinite(payload.waited_ms)
    ? payload.waited_ms
    : 0;
  return {
    results,
    wait_status: waitStatusRaw as WaitForWorkersResult['wait_status'],
    timed_out: Boolean(payload.timed_out),
    pending_task_ids: pendingTaskIds,
    waited_ms: waitedMs,
    audit: payload.audit,
    updatedAt: timestamp,
  };
}

function extractWaitForWorkersPayloadFromMessage(message: Message): WaitForWorkersResult | null {
  const blocks = ensureArray(message.blocks);
  if (blocks.length === 0) return null;
  for (const block of blocks as any[]) {
    if (block.type !== 'tool_call' || !block.toolCall) continue;
    if (block.toolCall.name !== 'worker_wait') continue;
    const rawPayload = block.toolCall.result ?? block.toolCall.standardized?.data;
    const parsed = parseWaitForWorkersPayload(rawPayload, message.timestamp || Date.now());
    if (parsed) {
      return parsed;
    }
  }
  return null;
}

function syncWorkerWaitResultsFromMessage(message: Message): void {
  const payload = extractWaitForWorkersPayloadFromMessage(message);
  if (payload && payload.results.length > 0) {
    const updates: Record<string, WaitForWorkersResult> = {};
    const updatedAt = typeof payload.updatedAt === 'number' ? payload.updatedAt : (message.timestamp || Date.now());
    const scopeId = resolveTaskCardScopeId(message.metadata as Record<string, unknown> | undefined);
    const grouped = new Map<string, WaitForWorkersResultItem[]>();
    for (const result of payload.results) {
      const taskId = typeof result.task_id === 'string' ? result.task_id.trim() : '';
      if (!taskId) continue;
      const cardKey = buildAssignmentTaskCardKey(taskId, scopeId);
      const list = grouped.get(cardKey) || [];
      list.push(result);
      grouped.set(cardKey, list);
    }
    for (const [cardKey, results] of grouped.entries()) {
      updates[cardKey] = {
        ...payload,
        results,
        updatedAt,
      };
    }
    if (Object.keys(updates).length > 0) {
      updateWorkerWaitResults(updates);
    }
  }

  const taskCardResult = buildWaitResultFromTaskCardMessage(message);
  if (taskCardResult) {
    updateWorkerWaitResults({ [taskCardResult.cardKey]: taskCardResult.result });
  }
}

function handleRetryRuntimePayload(payload: Record<string, unknown>): void {
  const messageId = typeof payload.messageId === 'string' ? payload.messageId.trim() : '';
  if (!messageId) {
    return;
  }

  const phase = payload.phase;
  if (phase === 'settled') {
    clearRetryRuntime(messageId);
    return;
  }

  const attempt = typeof payload.attempt === 'number' && Number.isFinite(payload.attempt)
    ? payload.attempt
    : 0;
  const maxAttempts = typeof payload.maxAttempts === 'number' && Number.isFinite(payload.maxAttempts)
    ? payload.maxAttempts
    : 0;
  if (attempt <= 0 || maxAttempts <= 0) {
    return;
  }

  if (phase === 'attempt_started') {
    const runtime: RetryRuntimeState = {
      phase,
      attempt,
      maxAttempts,
    };
    setRetryRuntime(messageId, runtime);
    return;
  }

  if (phase !== 'scheduled') {
    return;
  }

  const delayMs = typeof payload.delayMs === 'number' && Number.isFinite(payload.delayMs)
    ? Math.max(0, payload.delayMs)
    : 0;
  const nextRetryAt = typeof payload.nextRetryAt === 'number' && Number.isFinite(payload.nextRetryAt)
    ? payload.nextRetryAt
    : Date.now() + delayMs;

  setRetryRuntime(messageId, {
    phase,
    attempt,
    maxAttempts,
    delayMs,
    nextRetryAt,
  });
}

/**
 * 初始化消息处理器
 */
export function initMessageHandler() {
  vscode.onMessage(handleMessage);
  console.log('[MessageHandler] 消息处理器已初始化');
}

let lastAppliedEventSeq = 0;
const processedEventKeys = new Set<string>();
const processedEventKeyQueue: string[] = [];
const MAX_TRACKED_EVENT_KEYS = 4000;

function resetEventSeqTracking(): void {
  lastAppliedEventSeq = 0;
  processedEventKeys.clear();
  processedEventKeyQueue.length = 0;
}

function rememberProcessedEventKey(eventKey: string): void {
  if (processedEventKeys.has(eventKey)) {
    return;
  }
  processedEventKeys.add(eventKey);
  processedEventKeyQueue.push(eventKey);
  if (processedEventKeyQueue.length > MAX_TRACKED_EVENT_KEYS) {
    const oldest = processedEventKeyQueue.shift();
    if (oldest) {
      processedEventKeys.delete(oldest);
    }
  }
}

function resolveEventSeqAndKey(message: WebviewMessage): { eventSeq?: number; eventKey?: string } {
  if (message.type === 'unifiedUpdate') {
    const update = message.update as StreamUpdate | undefined;
    const seq = update?.eventSeq;
    if (typeof seq !== 'number' || !Number.isFinite(seq)) {
      return {};
    }
    const eventId = typeof update?.eventId === 'string' && update.eventId.trim()
      ? update.eventId.trim()
      : `upd:${update?.messageId || 'unknown'}:${update?.updateType || 'unknown'}:${update?.cardStreamSeq || 0}:${seq}`;
    return { eventSeq: seq, eventKey: `unifiedUpdate:${eventId}:${seq}` };
  }
  if (message.type === 'unifiedMessage' || message.type === 'unifiedComplete') {
    const standard = message.message as StandardMessage | undefined;
    const seq = standard?.eventSeq;
    if (typeof seq !== 'number' || !Number.isFinite(seq)) {
      return {};
    }
    const eventId = typeof standard?.eventId === 'string' && standard.eventId.trim()
      ? standard.eventId.trim()
      : `msg:${standard?.id || 'unknown'}:${standard?.lifecycle || 'unknown'}:${standard?.category || 'unknown'}:${seq}`;
    return { eventSeq: seq, eventKey: `${message.type}:${eventId}:${seq}` };
  }
  return {};
}

function shouldProcessByEventSeq(message: WebviewMessage): boolean {
  const { eventSeq, eventKey } = resolveEventSeqAndKey(message);
  if (eventSeq === undefined) {
    return true;
  }
  if (eventKey && processedEventKeys.has(eventKey)) {
    console.warn('[MessageHandler] 忽略重复事件', {
      eventSeq,
      eventKey,
      type: message.type,
    });
    return false;
  }
  // 多通道并发时事件到达顺序与 eventSeq 可能存在短暂交错，
  // 不丢弃逆序事件。真正去重由 eventKey 与 cardStreamSeq 负责。
  if (eventSeq < lastAppliedEventSeq) {
    console.warn('[MessageHandler] 检测到事件逆序到达，继续处理', {
      eventSeq,
      lastAppliedEventSeq,
      type: message.type,
    });
  }
  if (eventSeq > lastAppliedEventSeq + 1 && lastAppliedEventSeq > 0) {
    console.warn('[MessageHandler] 检测到事件序号跳跃', {
      eventSeq,
      lastAppliedEventSeq,
      gap: eventSeq - lastAppliedEventSeq,
      type: message.type,
    });
  }
  lastAppliedEventSeq = Math.max(lastAppliedEventSeq, eventSeq);
  if (eventKey) {
    rememberProcessedEventKey(eventKey);
  }
  return true;
}

const SESSION_LIFECYCLE_DATA_TYPES = new Set<string>([
  'sessionCreated',
  'sessionLoaded',
  'sessionSwitched',
  'sessionMessagesLoaded',
  'planLedgerLoaded',
  'sessionsUpdated',
]);

function shouldBypassCrossSessionFilter(message: WebviewMessage): boolean {
  if (message.type !== 'unifiedMessage' && message.type !== 'unifiedComplete') {
    return false;
  }
  const standard = message.message as StandardMessage | undefined;
  if (standard?.category !== MessageCategory.DATA) {
    return false;
  }

  const dataType = standard.data?.dataType;
  if (typeof dataType === 'string' && SESSION_LIFECYCLE_DATA_TYPES.has(dataType)) {
    return true;
  }

  return false;
}

function shouldIgnoreCrossSessionUnifiedMessage(message: WebviewMessage): boolean {
  if (
    message.type !== 'unifiedMessage'
    && message.type !== 'unifiedUpdate'
    && message.type !== 'unifiedComplete'
  ) {
    return false;
  }

  if (shouldBypassCrossSessionFilter(message)) {
    return false;
  }

  const incomingSessionId = typeof message.sessionId === 'string' ? message.sessionId.trim() : '';
  if (!incomingSessionId) {
    return false;
  }

  const currentSessionId = getState().currentSessionId;
  if (!currentSessionId || incomingSessionId === currentSessionId) {
    return false;
  }

  console.warn('[MessageHandler] 忽略跨会话消息', {
    type: message.type,
    incomingSessionId,
    currentSessionId,
  });
  return true;
}

/**
 * 处理来自扩展的消息
 */
function handleMessage(message: WebviewMessage) {
  const { type } = message;

  if (shouldIgnoreCrossSessionUnifiedMessage(message)) {
    return;
  }

  if (!shouldProcessByEventSeq(message)) {
    return;
  }

  try {
    switch (type) {
      case 'unifiedMessage':
        handleUnifiedMessage(message);
        break;

      case 'unifiedUpdate':
        handleStandardUpdate(message);
        break;

      case 'unifiedComplete':
        handleStandardComplete(message);
        break;

      default:
        console.warn('[MessageHandler] 未知消息类型:', type, message);
        break;
    }
  } catch (error) {
    console.error('[MessageHandler] 处理消息时发生未捕获异常:', error, message);
  }
}

// ============ 消息处理函数 ============

function resolveInteractionMode(raw: unknown): 'ask' | 'auto' | null {
  if (raw === 'ask' || raw === 'auto') {
    return raw;
  }
  return null;
}

function isEffectiveAutoMode(): boolean {
  const store = getState();
  const requestedMode = resolveInteractionMode(getRequestedInteractionMode());
  if (requestedMode === 'auto') {
    return true;
  }
  const backendMode = resolveInteractionMode(store.appState?.interactionMode);
  if (backendMode === 'auto') {
    return true;
  }
  const localMode = resolveInteractionMode(store.interactionMode);
  return backendMode === null && localMode === 'auto';
}

function resolvePendingInteractionsOnAutoMode(options?: { showToast?: boolean }): void {
  const store = getState();
  const showToast = options?.showToast === true;
  const hasAutoResolvablePending = Boolean(
    store.pendingToolAuthorization
    || store.pendingConfirmation
    || store.pendingRecovery
    || store.pendingDeliveryRepair
    || store.pendingClarification
    || store.pendingWorkerQuestion
  );
  if (!hasAutoResolvablePending) {
    return;
  }

  const pendingToolAuth = store.pendingToolAuthorization;
  if (pendingToolAuth?.requestId) {
    vscode.postMessage({ type: 'toolAuthorizationResponse', requestId: pendingToolAuth.requestId, allowed: true });
  }

  if (store.pendingConfirmation) {
    vscode.postMessage({ type: 'confirmPlan', confirmed: true });
  }

  if (store.pendingRecovery) {
    const decision: 'retry' | 'continue' = store.pendingRecovery.canRetry
      ? 'retry'
      : 'continue';
    vscode.postMessage({ type: 'confirmRecovery', decision });
  }

  if (store.pendingDeliveryRepair) {
    vscode.postMessage({ type: 'confirmDeliveryRepair', decision: 'repair' });
  }

  if (store.pendingClarification) {
    vscode.postMessage({
      type: 'answerClarification',
      answers: null,
      additionalInfo: null,
      autoSkipped: true,
    });
  }

  if (store.pendingWorkerQuestion) {
    vscode.postMessage({ type: 'answerWorkerQuestion', answer: null });
  }

  // auto 模式下所有交互请求都可自动闭环。
  store.pendingToolAuthorization = null;
  store.pendingConfirmation = null;
  store.pendingRecovery = null;
  store.pendingDeliveryRepair = null;
  store.pendingClarification = null;
  store.pendingWorkerQuestion = null;
  clearRequestedInteractionMode();
  setIsProcessing(true);
  if (showToast) {
    addToast('info', i18n.t('messageHandler.autoModePendingResolved'), undefined, {
      category: 'audit',
      source: 'interaction-mode',
      countUnread: false,
    });
  }
}

function applyInteractionModeFromPayload(
  rawMode: unknown,
  source: string,
  rawUpdatedAt?: unknown,
  previousModeRaw?: unknown,
): void {
  const resolved = resolveInteractionMode(rawMode);
  if (!resolved) {
    console.error(`[MessageHandler] ${source} 收到非法 interactionMode:`, rawMode);
    addToast('error', i18n.t('messageHandler.invalidInteractionMode'), undefined, {
      category: 'incident',
      source: 'interaction-mode',
      actionRequired: true,
    });
    return;
  }
  const previousMode = resolveInteractionMode(previousModeRaw)
    || resolveInteractionMode(getState().appState?.interactionMode)
    || 'auto';
  const requestedModeBeforeApply = getRequestedInteractionMode();
  const updatedAt = typeof rawUpdatedAt === 'number' ? rawUpdatedAt : undefined;
  setInteractionMode(resolved, updatedAt);
  const switchedToAuto = resolved === 'auto' && previousMode !== 'auto';
  const userRequestedAuto = requestedModeBeforeApply === 'auto';
  if (resolved === 'auto' && (switchedToAuto || userRequestedAuto)) {
    resolvePendingInteractionsOnAutoMode({
      showToast: switchedToAuto,
    });
  }
}

function isStaleInteractionModeUpdate(payload: Record<string, unknown>, source: string): boolean {
  const incomingUpdatedAt = typeof payload.updatedAt === 'number' ? payload.updatedAt : undefined;
  if (incomingUpdatedAt === undefined) return false;

  const currentUpdatedAt = typeof getState().appState?.interactionModeUpdatedAt === 'number'
    ? (getState().appState?.interactionModeUpdatedAt as number)
    : undefined;

  if (currentUpdatedAt === undefined) return false;
  const stale = incomingUpdatedAt < currentUpdatedAt;
  if (stale) {
    console.warn(`[MessageHandler] 忽略过期 interactionMode 更新(${source})`, {
      incomingUpdatedAt,
      currentUpdatedAt,
      mode: payload.mode,
    });
  }
  return stale;
}

function handleStateUpdate(message: WebviewMessage) {
  const state = message.state as AppState;
  if (!state) return;
  const previousModeRaw = getState().appState?.interactionMode;
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

  const nextUpdatedAt = typeof state.interactionModeUpdatedAt === 'number' ? state.interactionModeUpdatedAt : undefined;
  const currentUpdatedAt = typeof getState().appState?.interactionModeUpdatedAt === 'number'
    ? (getState().appState?.interactionModeUpdatedAt as number)
    : undefined;

  if (nextUpdatedAt !== undefined && currentUpdatedAt !== undefined && nextUpdatedAt < currentUpdatedAt) {
    console.warn('[MessageHandler] 忽略过期 stateUpdate.interactionMode', {
      incomingUpdatedAt: nextUpdatedAt,
      currentUpdatedAt,
      mode: state.interactionMode,
    });
    return;
  }

  setAppState(state);
  if (state.locale === 'zh-CN' || state.locale === 'en-US') {
    i18n.setLocale(state.locale);
  }

  if (state.sessions) {
    updateSessions(ensureArray(state.sessions) as Session[]);
  }

  if ((state as any).currentSessionId) {
    setCurrentSessionId((state as any).currentSessionId as string);
  }

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
        continuationPolicy: typeof raw.continuationPolicy === 'string' ? raw.continuationPolicy as Task['continuationPolicy'] : undefined,
        continuationReason: typeof raw.continuationReason === 'string' ? raw.continuationReason : undefined,
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
      // 避免覆盖 workerStatusUpdate 通过真实连接测试得出的结果
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

  // 🔧 已移除 isRunning/isProcessing 对处理状态的设置
  // 根因：sendStateUpdate() 是异步 fire-and-forget，stateUpdate 可能在 task_completed 之后
  // 延迟到达前端，携带过期的 isRunning=true，导致 clearProcessingState() 被覆盖。
  // 处理状态只由控制消息（task_started/task_completed）和消息生命周期驱动，不再接受 stateUpdate 的冗余信号。

  if (state.recovered === true) {
    sealAllStreamingMessages();
  }

  if (typeof state.interactionMode === 'string') {
    applyInteractionModeFromPayload(
      state.interactionMode,
      'stateUpdate',
      state.interactionModeUpdatedAt,
      previousModeRaw,
    );
  }
}


function handleUnifiedMessage(message: WebviewMessage) {
  const rawStandard = message.message as StandardMessage;
  if (!rawStandard) {
    console.warn('[MessageHandler] unifiedMessage 缺少 message 字段:', message);
    return;
  }
  const standard = assertStandardMessageId(rawStandard);

  switch (standard.category) {
    case MessageCategory.CONTENT:
      handleContentMessage(standard);
      break;
    case MessageCategory.CONTROL:
      handleUnifiedControlMessage(standard);
      break;
    case MessageCategory.NOTIFY:
      handleUnifiedNotify(standard);
      break;
    case MessageCategory.DATA:
      handleUnifiedData(standard);
      break;
    default:
      console.warn('[MessageHandler] 未知消息类别:', standard.category, standard);
      break;
  }
}

// ===== 流式更新缓冲：防止 update 先于 message 到达导致更新丢失 =====
const pendingStreamUpdates = new Map<string, StreamUpdate[]>();
const pendingStreamUpdateWarnings = new Set<string>();
const pendingStreamUpdateTimers = new Map<string, ReturnType<typeof setTimeout>>();
const STREAM_UPDATE_BUFFER_TIMEOUT = 30000; // 30 秒超时自动清理
// ===== complete 缓冲：防止 complete 先于 message 到达导致卡片不收口 =====
const pendingCompletes = new Map<string, { message: WebviewMessage; retryCount: number; timerId: ReturnType<typeof setTimeout> }>();
const MAX_COMPLETE_RETRIES = 3;
const COMPLETE_RETRY_INTERVAL = 1000;
const WORKER_SLOTS = ['claude', 'codex', 'gemini'] as const;
type WorkerSlot = typeof WORKER_SLOTS[number];
type ResolvedTarget = NonNullable<ReturnType<typeof getMessageTarget>>;

function clearPendingStreamUpdateBuffer(): void {
  pendingStreamUpdates.clear();
  pendingStreamUpdateWarnings.clear();
  for (const timerId of pendingStreamUpdateTimers.values()) {
    clearTimeout(timerId);
  }
  pendingStreamUpdateTimers.clear();
  for (const pending of pendingCompletes.values()) {
    clearTimeout(pending.timerId);
  }
  pendingCompletes.clear();
}

function rebuildSessionMessageTargets(
  threadMessages: Message[],
  workerMessages: { claude: Message[]; codex: Message[]; gemini: Message[] }
): void {
  const workerByMessageId = new Map<string, WorkerSlot>();
  for (const worker of WORKER_SLOTS) {
    for (const message of workerMessages[worker]) {
      const existingWorker = workerByMessageId.get(message.id);
      if (existingWorker && existingWorker !== worker) {
        throw new Error(`[MessageHandler] SessionMessagesLoaded worker 消息 id 冲突: ${message.id}`);
      }
      workerByMessageId.set(message.id, worker);
    }
  }

  const threadMessageIds = new Set<string>();
  for (const message of threadMessages) {
    threadMessageIds.add(message.id);
    const worker = workerByMessageId.get(message.id);
    if (worker) {
      setMessageTarget(message.id, { location: 'both', worker, reason: 'session-restored' });
      continue;
    }
    setMessageTarget(message.id, { location: 'thread', reason: 'session-restored' });
  }

  for (const worker of WORKER_SLOTS) {
    for (const message of workerMessages[worker]) {
      if (threadMessageIds.has(message.id)) {
        continue;
      }
      setMessageTarget(message.id, { location: 'worker', worker, reason: 'session-restored' });
    }
  }
}

/**
 * 会话恢复时从已有消息重建 workerWaitResults。
 * workerWaitResults 是运行时状态（不持久化），恢复后需要从 task_card 消息和
 * 包含 worker_wait 结果的消息中重建，否则卡片完成态丢失。
 */
function rebuildWorkerWaitResultsFromMessages(
  threadMessages: Message[],
  workerMessages: { claude: Message[]; codex: Message[]; gemini: Message[] }
): void {
  const allMessages = [
    ...threadMessages,
    ...workerMessages.claude,
    ...workerMessages.codex,
    ...workerMessages.gemini,
  ];
  for (const message of allMessages) {
    if (message.type === 'task_card' || ensureArray(message.blocks).some((b: any) => b?.toolCall?.name === 'worker_wait')) {
      syncWorkerWaitResultsFromMessage(message);
    }
  }
}

function queueStreamUpdate(update: StreamUpdate): void {
  const list = pendingStreamUpdates.get(update.messageId) || [];
  list.push(update);
  pendingStreamUpdates.set(update.messageId, list);

  // 超时自动清理：避免缓冲永久驻留
  if (!pendingStreamUpdateTimers.has(update.messageId)) {
    const timerId = setTimeout(() => {
      const stale = pendingStreamUpdates.get(update.messageId);
      if (stale) {
        console.warn(`[MessageHandler] 流式缓冲超时清理: ${update.messageId} (${stale.length} 条更新)`);
        pendingStreamUpdates.delete(update.messageId);
        pendingStreamUpdateWarnings.delete(update.messageId);
      }
      pendingStreamUpdateTimers.delete(update.messageId);
    }, STREAM_UPDATE_BUFFER_TIMEOUT);
    pendingStreamUpdateTimers.set(update.messageId, timerId);
  }
}

function hasRenderableUpdatePayload(update: StreamUpdate): boolean {
  if (update.updateType === 'append') {
    return Boolean(update.appendText && update.appendText.length > 0);
  }
  if (update.updateType === 'replace' || update.updateType === 'block_update') {
    return Array.isArray(update.blocks) && update.blocks.length > 0;
  }
  return false;
}

function recoverTargetByMessageId(messageId: string): ResolvedTarget | null {
  const state = getState();
  const inThread = state.threadMessages.some((msg) => msg.id === messageId);
  let workerHit: WorkerSlot | null = null;
  for (const worker of WORKER_SLOTS) {
    if (state.agentOutputs[worker].some((msg) => msg.id === messageId)) {
      workerHit = worker;
      break;
    }
  }
  if (inThread && workerHit) {
    const target = { location: 'both' as const, worker: workerHit, reason: 'stream-recover-by-id' };
    setMessageTarget(messageId, target);
    return target;
  }
  if (inThread) {
    const target = { location: 'thread' as const, reason: 'stream-recover-by-id' };
    setMessageTarget(messageId, target);
    return target;
  }
  if (workerHit) {
    const target = { location: 'worker' as const, worker: workerHit, reason: 'stream-recover-by-id' };
    setMessageTarget(messageId, target);
    return target;
  }
  return null;
}

function findLastMessageByCardId(messages: Message[], cardId: string): Message | undefined {
  for (let idx = messages.length - 1; idx >= 0; idx -= 1) {
    if (messages[idx]?.metadata?.cardId === cardId) {
      return messages[idx];
    }
  }
  return undefined;
}

function isStableCardMessage(message: Message | undefined): boolean {
  if (!message) {
    return false;
  }
  return message.type === 'task_card' || message.type === 'instruction';
}

function recoverTargetByCardId(cardId: string): { target: ResolvedTarget; messageId: string } | null {
  if (!cardId.trim()) {
    return null;
  }
  const state = getState();
  const threadMessageCandidate = findLastMessageByCardId(state.threadMessages, cardId);
  const threadMessage = isStableCardMessage(threadMessageCandidate) ? threadMessageCandidate : undefined;
  const workerMatches: Array<{ worker: WorkerSlot; message: Message }> = [];
  for (const worker of WORKER_SLOTS) {
    const messageCandidate = findLastMessageByCardId(state.agentOutputs[worker], cardId);
    const message = isStableCardMessage(messageCandidate) ? messageCandidate : undefined;
    if (message) {
      workerMatches.push({ worker, message });
    }
  }

  if (threadMessage && workerMatches.length > 0) {
    const workerMatch = workerMatches[0];
    const target = { location: 'both' as const, worker: workerMatch.worker, reason: 'stream-recover-by-cardId' };
    return { target, messageId: threadMessage.id };
  }
  if (threadMessage) {
    return {
      target: { location: 'thread' as const, reason: 'stream-recover-by-cardId' },
      messageId: threadMessage.id,
    };
  }
  if (workerMatches.length > 0) {
    return {
      target: { location: 'worker' as const, worker: workerMatches[0].worker, reason: 'stream-recover-by-cardId' },
      messageId: workerMatches[0].message.id,
    };
  }
  return null;
}

function createRecoveryStreamingCard(update: StreamUpdate): ResolvedTarget {
  const actor = getState().processingActor;
  const actorSource = String(actor.source || '');
  const worker = normalizeWorkerSlot(actorSource) || (actorSource === 'worker' ? normalizeWorkerSlot(actor.agent) : null);
  const timestamp = typeof update.timestamp === 'number' ? update.timestamp : Date.now();
  const fallbackTarget = worker
    ? { location: 'worker' as const, worker, reason: 'stream-recover-create-card' }
    : { location: 'thread' as const, reason: 'stream-recover-create-card' };
  const syntheticMessage: Message = {
    id: update.messageId,
    role: 'assistant',
    source: worker ?? 'orchestrator',
    content: '',
    blocks: [],
    timestamp,
    isStreaming: true,
    isComplete: false,
    type: 'text',
    metadata: {
      cardId: update.cardId || update.messageId,
      cardStreamSeq: update.cardStreamSeq,
      eventId: update.eventId,
      eventSeq: update.eventSeq,
      streamRecoveryCreated: true,
    },
  };

  if (fallbackTarget.location === 'worker') {
    addAgentMessage(fallbackTarget.worker, syntheticMessage);
  } else {
    addThreadMessage(syntheticMessage);
  }
  setMessageTarget(update.messageId, fallbackTarget);
  return fallbackTarget;
}

function tryUpsertInstructionByCardId(location: ResolvedTarget, uiMessage: Message): boolean {
  if (uiMessage.type !== 'instruction') {
    return false;
  }
  const cardId = typeof uiMessage.metadata?.cardId === 'string'
    ? uiMessage.metadata.cardId.trim()
    : '';
  if (!cardId) {
    return false;
  }

  if (location.location === 'worker') {
    const messages = getState().agentOutputs[location.worker];
    const existing = findLastMessageByCardId(messages, cardId);
    if (!existing) {
      return false;
    }
    updateAgentMessage(location.worker, existing.id, { ...uiMessage, id: existing.id });
    if (uiMessage.id !== existing.id) {
      setMessageTarget(uiMessage.id, location);
    }
    return true;
  }

  if (location.location === 'thread') {
    const existing = findLastMessageByCardId(getState().threadMessages, cardId);
    if (!existing) {
      return false;
    }
    updateThreadMessage(existing.id, { ...uiMessage, id: existing.id });
    if (uiMessage.id !== existing.id) {
      setMessageTarget(uiMessage.id, location);
    }
    return true;
  }

  if (location.location === 'both') {
    const existingThread = findLastMessageByCardId(getState().threadMessages, cardId);
    if (existingThread) {
      updateThreadMessage(existingThread.id, { ...uiMessage, id: existingThread.id });
    }
    const existingWorker = findLastMessageByCardId(getState().agentOutputs[location.worker], cardId);
    if (existingWorker) {
      updateAgentMessage(location.worker, existingWorker.id, { ...uiMessage, id: existingWorker.id });
    }
    if (existingThread || existingWorker) {
      if (uiMessage.id !== (existingThread?.id || existingWorker?.id)) {
        setMessageTarget(uiMessage.id, location);
      }
      return true;
    }
  }

  return false;
}

function shouldIgnoreSealedUpdate(existing: Message, update: StreamUpdate): boolean {
  // 1. 如果消息本身已经被标记为完成（例如收到过 task_completed 或 unifiedComplete）
  //    并且不是处于流式转为完成的临时动画期，我们应该拒绝所有会改变流式状态的更新
  //    这能防止 Worker 消息在完成后被迟到的 delta 重新开启 streaming
  if (existing.isComplete && update.updateType === 'lifecycle_change' && update.lifecycle !== 'completed') {
    console.warn('[MessageHandler] 已完成卡片收到非 completed 的 lifecycle_change，忽略', {
      messageId: existing.id,
      lifecycle: update.lifecycle,
    });
    return true;
  }

  // 2. 现有的 finalStreamSeq 检查机制
  const finalStreamSeq = typeof existing.metadata?.finalStreamSeq === 'number'
    ? existing.metadata.finalStreamSeq
    : undefined;
  if (finalStreamSeq === undefined) {
    return false;
  }

  const incomingStreamSeq = typeof update.cardStreamSeq === 'number'
    ? update.cardStreamSeq
    : undefined;
  if (incomingStreamSeq === undefined) {
    console.warn('[MessageHandler] 已封口卡片收到无序号更新，忽略', {
      messageId: existing.id,
      cardId: existing.metadata?.cardId,
    });
    return true;
  }

  if (incomingStreamSeq <= finalStreamSeq) {
    return true;
  }

  console.warn('[MessageHandler] 已封口卡片收到晚到更新，已忽略（应由后端补遗）', {
    messageId: existing.id,
    cardId: existing.metadata?.cardId,
    incomingStreamSeq,
    finalStreamSeq,
  });
  return true;
}

function applyUpdateToLocation(location: ReturnType<typeof getMessageTarget>, update: StreamUpdate): boolean {
  if (!location) return false;
  if (location.location === 'none' || location.location === 'task') {
    return true;
  }

  let applied = false;
  if (location.location === 'thread') {
    const existing = getState().threadMessages.find(m => m.id === update.messageId);
    if (existing) {
      if (shouldIgnoreSealedUpdate(existing, update)) {
        return true;
      }
      const streamUpdates = applyStreamUpdate(existing, update);
      let nextMessage: Message = { ...existing, ...streamUpdates };
      if (existing.metadata?.isPlaceholder && hasRenderableContent(nextMessage)) {
        nextMessage = {
          ...nextMessage,
          metadata: {
            ...(nextMessage.metadata || {}),
            isPlaceholder: false,
            wasPlaceholder: true,
            placeholderState: undefined,
          },
        };
      }
      updateThreadMessage(update.messageId, nextMessage);
      syncWorkerWaitResultsFromMessage(nextMessage);
      applied = true;
    }
  } else if (location.location === 'worker') {
    const existing = getState().agentOutputs[location.worker].find(m => m.id === update.messageId);
    if (existing) {
      if (shouldIgnoreSealedUpdate(existing, update)) {
        return true;
      }
      const streamUpdates = applyStreamUpdate(existing, update);
      const nextMessage = { ...existing, ...streamUpdates };
      updateAgentMessage(location.worker, update.messageId, nextMessage);
      syncWorkerWaitResultsFromMessage(nextMessage);
      applied = true;
    }
  } else if (location.location === 'both') {
    const threadExisting = getState().threadMessages.find(m => m.id === update.messageId);
    if (threadExisting) {
      if (shouldIgnoreSealedUpdate(threadExisting, update)) {
        return true;
      }
      const streamUpdates = applyStreamUpdate(threadExisting, update);
      let nextMessage: Message = { ...threadExisting, ...streamUpdates };
      if (threadExisting.metadata?.isPlaceholder && hasRenderableContent(nextMessage)) {
        nextMessage = {
          ...nextMessage,
          metadata: {
            ...(nextMessage.metadata || {}),
            isPlaceholder: false,
            wasPlaceholder: true,
            placeholderState: undefined,
          },
        };
      }
      updateThreadMessage(update.messageId, nextMessage);
      syncWorkerWaitResultsFromMessage(nextMessage);
      applied = true;
    }
    const agentExisting = getState().agentOutputs[location.worker].find(m => m.id === update.messageId);
    if (agentExisting) {
      if (shouldIgnoreSealedUpdate(agentExisting, update)) {
        return true;
      }
      const streamUpdates = applyStreamUpdate(agentExisting, update);
      const nextMessage = { ...agentExisting, ...streamUpdates };
      updateAgentMessage(location.worker, update.messageId, nextMessage);
      syncWorkerWaitResultsFromMessage(nextMessage);
      applied = true;
    }
  }
  return applied;
}

function flushPendingStreamUpdates(messageId: string): void {
  const updates = pendingStreamUpdates.get(messageId);
  if (!updates || updates.length === 0) {
    return;
  }
  const location = getMessageTarget(messageId);
  if (!location) {
    return;
  }
  const remaining: StreamUpdate[] = [];
  for (const update of updates) {
    const applied = applyUpdateToLocation(location, update);
    if (!applied) {
      remaining.push(update);
    }
  }
  if (remaining.length > 0) {
    pendingStreamUpdates.set(messageId, remaining);
  } else {
    pendingStreamUpdates.delete(messageId);
    pendingStreamUpdateWarnings.delete(messageId);
    // 缓冲已全部消费，清理对应的超时定时器
    const timerId = pendingStreamUpdateTimers.get(messageId);
    if (timerId) {
      clearTimeout(timerId);
      pendingStreamUpdateTimers.delete(messageId);
    }
  }
}

function handleContentMessage(standard: StandardMessage) {
  const uiMessage = mapStandardMessage(standard);
  const meta = standard.metadata as Record<string, unknown> | undefined;
  const requestId = meta?.requestId as string | undefined;
  const isPlaceholder = Boolean(meta?.isPlaceholder);
  const isUserMessage = standard.type === MessageType.USER_INPUT;

  const upsertThreadMessage = (message: Message) => {
    const existing = getState().threadMessages.find(m => m.id === message.id);
    if (existing) {
      updateThreadMessage(message.id, message);
    } else {
      addThreadMessage(message);
    }
  };

  if (isPlaceholder) {
    if (!requestId) {
      throw new Error('[MessageHandler] 占位消息缺少 requestId');
    }
    const userMessageId = meta?.userMessageId as string | undefined;
    if (!userMessageId) {
      throw new Error('[MessageHandler] 占位消息缺少 userMessageId');
    }
    const binding = getRequestBinding(requestId);

    // 创建 60 秒超时定时器（首 token 超时保护）
    const timeoutId = setTimeout(() => {
      const currentBinding = getRequestBinding(requestId);
      // 只有在没有收到真实消息时才触发超时
      if (currentBinding && !currentBinding.realMessageId) {
        console.warn('[MessageHandler] 首 token 超时，移除占位消息:', requestId);
        // 移除占位消息
        removeThreadMessage(currentBinding.placeholderMessageId);
        clearMessageTarget(currentBinding.placeholderMessageId);
        // 清理请求绑定
        clearRequestBinding(requestId);
        clearPendingRequest(requestId);
        markMessageComplete(currentBinding.placeholderMessageId);
        // 显示超时错误提示
        addToast('error', i18n.t('messageHandler.responseTimeout'), undefined, {
          category: 'incident',
          source: 'model-runtime',
          actionRequired: true,
        });
      }
    }, 60000); // 60 秒超时

    if (!binding) {
      createRequestBinding({
        requestId,
        userMessageId,
        placeholderMessageId: standard.id,
        createdAt: standard.timestamp || Date.now(),
        timeoutId,
      });
    } else {
      // 清除旧的超时定时器
      if (binding.timeoutId) {
        clearTimeout(binding.timeoutId);
      }
      updateRequestBinding(requestId, { placeholderMessageId: standard.id, userMessageId, timeoutId });
    }
    addPendingRequest(requestId);
    upsertThreadMessage(uiMessage);
    syncWorkerWaitResultsFromMessage(uiMessage);
    routeStandardMessage(standard);
    if (uiMessage.isStreaming) {
      markMessageActive(uiMessage.id);
    }
    // 回放可能提前到达的流式更新
    flushPendingStreamUpdates(standard.id);
    return;
  }

  if (isUserMessage) {
    if (requestId) {
      const placeholderMessageId = meta?.placeholderMessageId as string | undefined;
      const binding = getRequestBinding(requestId);
      if (!binding && placeholderMessageId) {
        createRequestBinding({
          requestId,
          userMessageId: standard.id,
          placeholderMessageId,
          createdAt: standard.timestamp || Date.now(),
        });
      } else if (binding) {
        updateRequestBinding(requestId, { userMessageId: standard.id });
      }
    }
    upsertThreadMessage(uiMessage);
    // 注册用户消息的路由
    routeStandardMessage(standard);

    // 如果用户指定了 Worker 直接对话，同时把用户消息添加到对应 Worker 面板
    const targetWorker = normalizeWorkerSlot(meta?.targetWorker);
    if (targetWorker) {
      addAgentMessage(targetWorker, uiMessage);
    }
    return;
  }

  // === 检查是否有对应的占位消息需要替换 ===
  if (requestId) {
    const binding = getRequestBinding(requestId);

    if (binding && !binding.realMessageId) {
      // 首次收到真实消息，需要原地替换占位消息
      // 清除超时定时器（已收到真实消息）
      if (binding.timeoutId) {
        clearTimeout(binding.timeoutId);
      }
      const placeholderId = binding.placeholderMessageId;
      const existingPlaceholder = getState().threadMessages.find(m => m.id === placeholderId);
      if (!existingPlaceholder) {
        console.warn('[MessageHandler] 占位消息不存在，改为按真实消息接入主链路', {
          requestId,
          placeholderId,
          incomingMessageId: standard.id,
        });
        if (placeholderId !== standard.id) {
          clearMessageTarget(placeholderId);
        }
        updateRequestBinding(requestId, { realMessageId: standard.id, placeholderMessageId: standard.id, timeoutId: undefined });
      } else {
        if (placeholderId !== standard.id) {
          // ID 不匹配时执行原地替换
          // 后端可能生成了新的 ID 而未复用占位 ID
          console.warn(`[MessageHandler] 响应 ID 变更，执行原地替换: ${placeholderId} -> ${standard.id}`);

          // 1. 更新绑定关系指向新 ID
          updateRequestBinding(requestId, { realMessageId: standard.id, placeholderMessageId: standard.id });

          // 2. 若流更新先于真实 STARTED 抵达，列表里可能已经存在 synthetic/恢复出来的实体。
          // 必须合并该实体与占位消息，保留已累积的流式内容，并在最终列表中只保留一个消息节点。
          const existingRealMessage = getState().threadMessages.find(m => m.id === standard.id);
          const replacementBase = existingRealMessage && hasRenderableContent(existingRealMessage)
            ? existingRealMessage
            : existingPlaceholder;
          const shouldUseIncomingContent = hasRenderableContent(uiMessage);
          const newMessage: import('../types/message').Message = {
            ...replacementBase,
            ...uiMessage,
            id: standard.id,
            content: shouldUseIncomingContent ? uiMessage.content : replacementBase.content,
            blocks: shouldUseIncomingContent ? uiMessage.blocks : replacementBase.blocks,
            metadata: {
              ...(replacementBase.metadata || {}),
              ...(uiMessage.metadata || {}),
              requestId, // 保持请求关联
              isPlaceholder: false,
              wasPlaceholder: true,
              placeholderState: undefined,
            },
          };

          // 3. 在 UI 中原地替换（保持滚动位置和顺序）
          replaceThreadMessage(placeholderId, newMessage);
          syncWorkerWaitResultsFromMessage(newMessage);

          // 4. 真实消息 ID 与占位 ID 不一致时，必须重建路由
          // 否则 unifiedUpdate/unifiedComplete 会因找不到 messageId 对应目标而被暂存/忽略
          clearMessageTarget(placeholderId);
          routeStandardMessage(standard);

          // 5. 标记活跃并回放缓冲
          if (newMessage.isStreaming) {
            markMessageActive(newMessage.id);
          }
          flushPendingStreamUpdates(standard.id);

          return;
        }

        // 占位消息即真实消息，直接在同一条消息上更新
        updateRequestBinding(requestId, { realMessageId: standard.id, placeholderMessageId: standard.id });
        const mergedMessage: import('../types/message').Message = {
          ...existingPlaceholder,
          ...uiMessage,
          metadata: {
            ...(existingPlaceholder.metadata || {}),
            ...(uiMessage.metadata || {}),
            isPlaceholder: false,
            wasPlaceholder: true,
            placeholderState: undefined,
            requestId,
          },
        };
        updateThreadMessage(placeholderId, mergedMessage);
        syncWorkerWaitResultsFromMessage(mergedMessage);

        // 标记为活跃消息
        if (uiMessage.isStreaming) {
          markMessageActive(placeholderId);
        }

        // 可能存在提前到达的流式更新，立即补齐
        flushPendingStreamUpdates(standard.id);

        return;
      }
    }
  }

  // === 后续消息处理（非首次或无占位消息关联） ===
  let target = routeStandardMessage(standard);

  // L5 路由层安全拦截：routing-table 已将 WORKER_* 类别路由到 worker，
  // 此检查作为防御性补充——当分类器未能识别 Worker 消息类别时，
  // 根据 source 字段拦截并重定向，防止 Worker 内容意外写入主对话区。
  // 例外：ERROR 和 INTERACTION 类型允许 Worker 写入主对话区，确保用户可见。
  if (target.location === 'thread' && standard.source === 'worker') {
    const allowedInThread = [MessageType.ERROR, MessageType.INTERACTION].includes(standard.type as MessageType);
    if (!allowedInThread) {
      console.warn('[MessageHandler] 安全拦截: Worker 试图写入主对话区，强制重定向', { id: standard.id });
      const workerSlot = normalizeWorkerSlot(standard.agent) || 'claude';
      target = { location: 'worker', worker: workerSlot };
    }
  }

  if (target.location === 'none' || target.location === 'task') {
    return;
  }

  // 流式消息标记为活跃，驱动 isProcessing 状态
  if (uiMessage.isStreaming) {
    markMessageActive(uiMessage.id);
  }

  if (tryUpsertInstructionByCardId(target, uiMessage)) {
    flushPendingStreamUpdates(standard.id);
    return;
  }

    if (target.location === 'thread') {
      const existing = getState().threadMessages.find(m => m.id === uiMessage.id);
      if (existing) {
        updateThreadMessage(uiMessage.id, uiMessage);
      } else {
        addThreadMessage(uiMessage);
      }
    } else if (target.location === 'worker') {
      console.log('[MessageHandler] 🎯 路由 Worker 消息:', {
        messageId: uiMessage.id,
        worker: target.worker,
        isStreaming: uiMessage.isStreaming,
        blocksCount: uiMessage.blocks?.length ?? 0,
      });
      const existing = getState().agentOutputs[target.worker].find(m => m.id === uiMessage.id);
      if (existing) {
        updateAgentMessage(target.worker, uiMessage.id, uiMessage);
      } else {
        addAgentMessage(target.worker, uiMessage);
        console.log('[MessageHandler] ✅ Worker 消息已添加:', target.worker, uiMessage.id);
      }
    } else if (target.location === 'both') {
      const threadExisting = getState().threadMessages.find(m => m.id === uiMessage.id);
      if (threadExisting) {
        updateThreadMessage(uiMessage.id, uiMessage);
      } else {
        addThreadMessage(uiMessage);
      }
      const agentExisting = getState().agentOutputs[target.worker].find(m => m.id === uiMessage.id);
      if (agentExisting) {
        updateAgentMessage(target.worker, uiMessage.id, uiMessage);
      } else {
        addAgentMessage(target.worker, uiMessage);
      }
    }

    syncWorkerWaitResultsFromMessage(uiMessage);
    // 可能存在提前到达的流式更新，立即补齐
    flushPendingStreamUpdates(standard.id);
}


function handleStandardUpdate(message: WebviewMessage) {
  const rawUpdate = message.update as StreamUpdate;
  if (!rawUpdate?.messageId || !rawUpdate.messageId.trim()) {
    throw new Error('[MessageHandler] 流式更新缺少 messageId');
  }
  let update = rawUpdate;

  // 查找路由
  let location = getMessageTarget(update.messageId);

  if (!location) {
    location = recoverTargetByMessageId(update.messageId);
  }

  if (!location && update.cardId) {
    const recovered = recoverTargetByCardId(update.cardId);
    if (recovered) {
      location = recovered.target;
      setMessageTarget(update.messageId, recovered.target);
      if (recovered.messageId !== update.messageId) {
        update = { ...update, messageId: recovered.messageId };
      }
    }
  }

  if (!location && hasRenderableUpdatePayload(update)) {
    location = createRecoveryStreamingCard(update);
  }

  if (!location) {
    if (!pendingStreamUpdateWarnings.has(update.messageId)) {
      console.warn(`[MessageHandler] 未找到流式更新路由，已暂存并等待主消息: ${update.messageId}`);
      pendingStreamUpdateWarnings.add(update.messageId);
    }
    queueStreamUpdate(update);
    return;
  }

  pendingStreamUpdateWarnings.delete(update.messageId);
  const applied = applyUpdateToLocation(location, update);
  if (!applied) {
    if (!pendingStreamUpdateWarnings.has(update.messageId)) {
      console.warn(`[MessageHandler] 流式更新路由已命中但目标未就绪，继续暂存: ${update.messageId}`);
      pendingStreamUpdateWarnings.add(update.messageId);
    }
    queueStreamUpdate(update);
    return;
  }
  pendingStreamUpdateWarnings.delete(update.messageId);
}

function handleStandardComplete(message: WebviewMessage) {
  const rawStandard = message.message as StandardMessage;
  if (!rawStandard) {
    throw new Error('[MessageHandler] unifiedComplete 缺少 message');
  }
  const standard = assertStandardMessageId(rawStandard);

  // 只处理 CONTENT 类别的消息，其他类别不参与卡片渲染
  if (standard.category !== MessageCategory.CONTENT) {
    console.debug('[MessageHandler] 跳过非 CONTENT 类别的 complete 消息:', standard.category, standard.id);
    return;
  }

  const requestId = (standard.metadata as Record<string, unknown> | undefined)?.requestId as string | undefined;
  const actualMessageId = standard.id;

  // 清除该消息的暂存重试（如果是重试触发的调用）
  const pendingEntry = pendingCompletes.get(actualMessageId);
  if (pendingEntry) {
    clearTimeout(pendingEntry.timerId);
    pendingCompletes.delete(actualMessageId);
  }

  // 路由查找：先查缓存，缺失时尝试恢复（与 handleStandardUpdate 一致的策略）
  let location = getMessageTarget(actualMessageId);
  if (!location) {
    location = recoverTargetByMessageId(actualMessageId);
  }
  if (!location) {
    // 路由恢复失败，暂存并延迟重试
    const retryCount = pendingEntry ? pendingEntry.retryCount + 1 : 0;
    if (retryCount < MAX_COMPLETE_RETRIES) {
      const timerId = setTimeout(() => {
        handleStandardComplete(message);
      }, COMPLETE_RETRY_INTERVAL);
      pendingCompletes.set(actualMessageId, { message, retryCount, timerId });
      console.warn(`[MessageHandler] 完成消息缺少路由，暂存等待重试 (${retryCount + 1}/${MAX_COMPLETE_RETRIES}): ${standard.id}`);
    } else {
      console.warn(`[MessageHandler] 完成消息路由恢复失败，已达最大重试次数，放弃: ${standard.id}`);
      // 即使路由找不到，也标记完成以清理活跃状态
      markMessageComplete(actualMessageId);
    }
    return;
  }

  if (location.location === 'none' || location.location === 'task') {
    return;
  }

  // 先检查消息是否存在
  // complete 消息是用来"完成"已有消息的
  let messageExists = false;
  if (location.location === 'thread') {
    messageExists = getState().threadMessages.some(m => m.id === actualMessageId);
  } else if (location.location === 'worker') {
    messageExists = getState().agentOutputs[location.worker].some(m => m.id === actualMessageId);
  } else if (location.location === 'both') {
    messageExists = getState().threadMessages.some(m => m.id === actualMessageId) ||
                    getState().agentOutputs[location.worker].some(m => m.id === actualMessageId);
  }

  if (!messageExists) {
    // 消息实体不存在，使用与路由缺失相同的重试策略
    const retryCount = pendingEntry ? pendingEntry.retryCount + 1 : 0;
    if (retryCount < MAX_COMPLETE_RETRIES) {
      const timerId = setTimeout(() => {
        handleStandardComplete(message);
      }, COMPLETE_RETRY_INTERVAL);
      pendingCompletes.set(actualMessageId, { message, retryCount, timerId });
      console.warn(`[MessageHandler] 完成消息未找到对应卡片，暂存等待重试 (${retryCount + 1}/${MAX_COMPLETE_RETRIES}): ${actualMessageId}`);
    } else {
      console.warn(`[MessageHandler] 完成消息对应卡片始终不存在，放弃: ${actualMessageId}`);
      markMessageComplete(actualMessageId);
    }
    return;
  }

  markMessageComplete(actualMessageId);

  const uiMessage = mapStandardMessage(standard);
  const hasContent = hasRenderableContent(uiMessage);

  // 保留已有内容：complete 消息可能没有 blocks/content
  const getExistingMessage = () => {
    if (location.location === 'thread') {
      return getState().threadMessages.find(m => m.id === actualMessageId);
    }
    if (location.location === 'worker') {
      return getState().agentOutputs[location.worker].find(m => m.id === actualMessageId);
    }
    if (location.location === 'both') {
      return getState().threadMessages.find(m => m.id === actualMessageId)
        || getState().agentOutputs[location.worker].find(m => m.id === actualMessageId);
    }
    return undefined;
  };

  const existingMessage = getExistingMessage();
  // 优先保留流式累积的内容，避免 COMPLETE 的结构化 blocks 替换导致 DOM 重构闪烁
  const baseMessage = existingMessage && hasRenderableContent(existingMessage)
    ? existingMessage
    : (hasContent ? uiMessage : (existingMessage || uiMessage));
  if (!baseMessage) {
    clearMessageTarget(actualMessageId);
    return;
  }

  const shouldConvertFromPlaceholder = Boolean(existingMessage?.metadata?.isPlaceholder)
    && hasRenderableContent(baseMessage);

  // 添加完成动画标记，并确保流式结束
  const completedMessage = {
    ...baseMessage,
    id: actualMessageId, // 使用实际的消息 ID
    isStreaming: false,
    isComplete: true,
    metadata: {
      ...(baseMessage.metadata || {}),
      ...(uiMessage.metadata || {}),
      justCompleted: true,
      ...(shouldConvertFromPlaceholder
        ? {
            isPlaceholder: false,
            wasPlaceholder: true,
            placeholderState: undefined,
          }
        : {}),
    },
  };

  // 更新已存在的消息
  if (location.location === 'thread') {
    updateThreadMessage(actualMessageId, completedMessage);
  } else if (location.location === 'worker') {
    updateAgentMessage(location.worker, actualMessageId, completedMessage);
  } else if (location.location === 'both') {
    updateThreadMessage(actualMessageId, completedMessage);
    updateAgentMessage(location.worker, actualMessageId, completedMessage);
  }
  syncWorkerWaitResultsFromMessage(completedMessage);

  // 补齐可能提前到达的流式更新
  flushPendingStreamUpdates(actualMessageId);

  // 强制再次锁定完成状态！
  // 因为 flushPendingStreamUpdates 会重放之前的 stream event，
  // 某些残留的 stream 事件可能会包含 lifecycle: 'streaming'，从而将 isStreaming 重新置为 true。
  // 我们必须在 flush 之后再次确保消息是被封口的。
  const lockCompleteState = (msgId: string) => {
    const override = { isStreaming: false, isComplete: true };
    if (location.location === 'thread' || location.location === 'both') {
      updateThreadMessage(msgId, override);
    }
    if (location.location === 'worker' || location.location === 'both') {
      updateAgentMessage(location.worker, msgId, override);
    }
  };
  lockCompleteState(actualMessageId);

  // 清理请求绑定
  if (requestId) {
    const binding = getRequestBinding(requestId);
    if (binding?.timeoutId) {
      clearTimeout(binding.timeoutId);
    }
    // 延迟清理，确保动画完成
    setTimeout(() => {
      clearRequestBinding(requestId);
    }, 1000);
  }

  // 移除 justCompleted 标记（动画完成后）
  setTimeout(() => {
    const cleanedMessage = {
      ...completedMessage,
      metadata: {
        ...(completedMessage.metadata || {}),
        justCompleted: false,
      },
    };
    if (location.location === 'thread' || location.location === 'both') {
      updateThreadMessage(actualMessageId, cleanedMessage);
    }
    if (location.location === 'worker' || location.location === 'both') {
      updateAgentMessage(location.worker, actualMessageId, cleanedMessage);
    }
  }, 500);

  // 不立即清除 messageTarget：同一 requestId 下可能有后续流式轮次（tool calling round）
  // messageTarget 会在 clearMessageTargets()（新会话/重置时）或 requestBinding 清理时被清除
}


/**
 * 处理控制消息
 *
 * 控制消息通过 MessageHub.sendControl() 发送，包含 controlType 和 payload
 */
function handleUnifiedControlMessage(standard: StandardMessage) {
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
          updateThreadMessage(binding.placeholderMessageId, {
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

          if (placeholder) {
            const baseMetadata = (placeholder.metadata && typeof placeholder.metadata === 'object')
              ? placeholder.metadata
              : {};
            updateThreadMessage(placeholderId, {
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
            updateThreadMessage(binding.placeholderMessageId, {
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
      // 请求级完成不等于系统级空闲。
      // deep/恢复/队列续跑场景下，provider 仍可能马上进入下一轮；
      // 真正的 idle 只能由 processingStateChanged(false, forced) 裁决。
      // 终结所有未完成的流式消息：保留已输出内容，移除空占位消息，停止 streaming 动画
      sealAllStreamingMessages();
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

function handleUnifiedNotify(standard: StandardMessage) {
  const level = standard.notify?.level || 'info';
  const content = extractTextFromStandardBlocks(standard.blocks);
  if (!content) {
    console.warn('[MessageHandler] 通知消息缺少内容，跳过:', standard);
    return;
  }
  if (level === 'error') {
    addToast(level, content, undefined, {
      category: 'incident',
      source: 'model-runtime',
      actionRequired: true,
    });
    return;
  }
  if (level === 'warning') {
    addToast(level, content, undefined, {
      category: 'audit',
      source: 'model-runtime',
      countUnread: false,
    });
    return;
  }
  addToast(level, content);
}

function handleUnifiedData(standard: StandardMessage) {
  const data = standard.data;
  if (!data) {
    console.warn('[MessageHandler] 数据消息缺少 data 字段，跳过:', standard);
    return;
  }
  const { dataType, payload } = data;
  const asMessage = (extra: Record<string, unknown>) => ({ ...extra } as WebviewMessage);

  switch (dataType) {
    case 'llmRetryRuntime':
      if (payload && typeof payload === 'object') {
        handleRetryRuntimePayload(payload as Record<string, unknown>);
      }
      break;

    case 'terminalStreamStarted':
    case 'terminalStreamFrame':
    case 'terminalStreamCompleted':
      terminalSessions.ingestStreamEvent({
        ...(payload as TerminalStreamEventPayload),
        eventType: dataType,
      });
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

    case 'sessionCreated':
    case 'sessionLoaded':
    case 'sessionSwitched':
      handleSessionChanged(asMessage({
        sessionId: payload.sessionId,
        session: payload.session
      }));
      break;

    case 'sessionMessagesLoaded':
      handleSessionMessagesLoaded(asMessage({
        sessionId: payload.sessionId,
        messages: payload.messages,
        workerMessages: payload.workerMessages
      }));
      break;

    case 'planLedgerLoaded':
    case 'planLedgerUpdated':
      applyPlanLedgerSnapshot(payload);
      break;

    case 'confirmationRequest':
      handleConfirmationRequest(asMessage(payload));
      break;

    case 'recoveryRequest':
      handleRecoveryRequest(asMessage(payload));
      break;

    case 'deliveryRepairRequest':
      handleDeliveryRepairRequest(asMessage(payload));
      break;
    case 'orchestratorRuntimeDiagnostics':
      handleOrchestratorRuntimeDiagnostics(asMessage(payload));
      break;

    case 'clarificationRequest':
      handleClarificationRequest(asMessage(payload));
      break;

    case 'workerQuestionRequest':
      handleWorkerQuestionRequest(asMessage(payload));
      break;

    case 'toolAuthorizationRequest':
      handleToolAuthorizationRequest(asMessage(payload));
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

    case 'workerStatusUpdate':
      handleWorkerStatusUpdate(asMessage(payload));
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

    case 'interactionModeChanged':
      if (isStaleInteractionModeUpdate(payload, 'interactionModeChanged')) {
        break;
      }
      applyInteractionModeFromPayload(
        payload.mode,
        'interactionModeChanged',
        payload.updatedAt,
        getState().appState?.interactionMode,
      );
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

function handleSessionsUpdated(message: WebviewMessage) {
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

function handleSessionChanged(message: WebviewMessage) {
  // 获取新的 sessionId
  const newSessionId = message.sessionId as string || (message.session as Session)?.id;

  if (newSessionId) {
    const store = getState();
    const currentId = store.currentSessionId;

    // 如果是不同的会话，清空当前消息和请求绑定
    if (currentId !== newSessionId) {
      clearAllMessages();
      clearMessageTargets();
      clearAllRequestBindings();
      clearPendingStreamUpdateBuffer();
      resetEventSeqTracking();

      const currentState = (store.appState || {}) as AppState;
      setAppState({
        ...currentState,
        activePlan: null,
        planHistory: [],
      });
    }

    setCurrentSessionId(newSessionId);
  }
}

function handleSessionMessagesLoaded(message: WebviewMessage) {
  // 切换会话时，后端发送完整的消息历史（包括主对话和 worker 消息）
  const sessionId = message.sessionId as string;
  const messages = message.messages as any[];
  const workerMessages = message.workerMessages as { claude?: any[]; codex?: any[]; gemini?: any[] } | undefined;

  if (sessionId) {
    // 先清空当前消息
    clearAllMessages();
    clearMessageTargets();
    clearAllRequestBindings();
    clearPendingStreamUpdateBuffer();
    resetEventSeqTracking();
    setCurrentSessionId(sessionId);

    // 格式化消息的辅助函数
    const formatMessage = (m: any): Message => {
      const id = typeof m?.id === 'string' && m.id.trim() ? m.id.trim() : '';
      if (!id) {
        throw new Error('[MessageHandler] SessionMessagesLoaded 消息缺少 id');
      }
      const role = m?.role;
      if (role !== 'user' && role !== 'assistant' && role !== 'system') {
        throw new Error('[MessageHandler] SessionMessagesLoaded 消息 role 无效');
      }
      if (typeof m?.content !== 'string') {
        throw new Error('[MessageHandler] SessionMessagesLoaded 消息 content 非字符串');
      }
      if (typeof m?.timestamp !== 'number') {
        throw new Error('[MessageHandler] SessionMessagesLoaded 消息 timestamp 无效');
      }

      // 优先使用已有 type 字段，否则根据 role 映射
      let resolvedType: import('../types/message').MessageType;
      if (m.type && typeof m.type === 'string') {
        resolvedType = m.type as import('../types/message').MessageType;
      } else {
        // role 回退映射
        switch (role) {
          case 'user':
            resolvedType = 'user_input';
            break;
          case 'system':
            resolvedType = 'system-notice';
            break;
          default:
            resolvedType = 'text';
        }
      }

      return {
        id,
        role,
        content: m.content,
        source: m.source || 'orchestrator',
        timestamp: m.timestamp,
        isStreaming: Boolean(m?.isStreaming),
        isComplete: typeof m?.isComplete === 'boolean' ? Boolean(m.isComplete) : !Boolean(m?.isStreaming),
        type: resolvedType,
        noticeType: typeof m?.noticeType === 'string' ? m.noticeType : undefined,
        blocks: mapStandardBlocks(
          (Array.isArray(m.blocks) && m.blocks.length > 0)
            ? m.blocks
            : [{
                type: 'text' as const,
                content: m.content || '',
              }]
        ),
        metadata: m?.metadata && typeof m.metadata === 'object'
          ? { ...(m.metadata as Record<string, unknown>) }
          : undefined,
      };
    };

    const formattedThreadMessages: Message[] = normalizeRestoredMessages(
      ensureArray(messages).map(formatMessage)
    );
    const formattedWorkerMessages = {
      claude: normalizeRestoredMessages(ensureArray(workerMessages?.claude).map(formatMessage)),
      codex: normalizeRestoredMessages(ensureArray(workerMessages?.codex).map(formatMessage)),
      gemini: normalizeRestoredMessages(ensureArray(workerMessages?.gemini).map(formatMessage)),
    };

    setThreadMessages(formattedThreadMessages);
    setAgentOutputs(formattedWorkerMessages);
    rebuildSessionMessageTargets(formattedThreadMessages, formattedWorkerMessages);
    // 会话恢复时从已有 task_card 消息重建 workerWaitResults，
    // 修复"恢复后卡片完成态丢失"问题（workerWaitResults 不持久化，需从消息重建）
    rebuildWorkerWaitResultsFromMessages(formattedThreadMessages, formattedWorkerMessages);
  }
}

function handleConfirmationRequest(message: WebviewMessage) {
  const store = getState();
  const forceManual = message.forceManual === true;
  if (isEffectiveAutoMode()) {
    addToast('info', i18n.t('messageHandler.autoConfirmPlan'), undefined, {
      category: 'audit',
      source: 'interaction-mode',
      countUnread: false,
    });
    vscode.postMessage({ type: 'confirmPlan', confirmed: true });
    clearRequestedInteractionMode();
    setIsProcessing(true);
    return;
  }
  store.pendingConfirmation = {
    plan: message.plan,
    formattedPlan: message.formattedPlan as string | undefined,
    forceManual,
  };
  settleProcessingForManualInteraction();
  sealAllStreamingMessages();
}

function handleRecoveryRequest(message: WebviewMessage) {
  const store = getState();
  if (store.appState?.interactionMode === 'auto') {
    const canRetry = Boolean(message.canRetry);
    const decision: 'retry' | 'continue' = canRetry
      ? 'retry'
      : 'continue';
    addToast('info', i18n.t('messageHandler.autoRecovery', { decision: i18n.t(decision === 'retry' ? 'messageHandler.autoRecoveryRetry' : 'messageHandler.autoRecoveryContinue') }), undefined, {
      category: 'audit',
      source: 'recovery',
      countUnread: false,
    });
    vscode.postMessage({ type: 'confirmRecovery', decision });
    setIsProcessing(true);
    return;
  }
  store.pendingRecovery = {
    taskId: (message.taskId as string) || '',
    error: message.error,
    canRetry: Boolean(message.canRetry),
    canRollback: Boolean(message.canRollback),
  };
  settleProcessingForManualInteraction();
  sealAllStreamingMessages();
}

function handleDeliveryRepairRequest(message: WebviewMessage) {
  const store = getState();
  const requestType = message.requestType === 'replan_followup'
    ? 'replan_followup'
    : 'delivery_repair';
  if (store.appState?.interactionMode === 'auto') {
    const toastKey = requestType === 'replan_followup'
      ? 'messageHandler.autoReplanFollowUp'
      : 'messageHandler.autoDeliveryRepair';
    addToast('info', i18n.t(toastKey), undefined, {
      category: 'audit',
      source: 'delivery-repair',
      countUnread: false,
    });
    vscode.postMessage({ type: 'confirmDeliveryRepair', decision: 'repair' });
    setIsProcessing(true);
    return;
  }
  store.pendingDeliveryRepair = {
    missionId: (message.missionId as string) || '',
    summary: String(message.summary || ''),
    details: typeof message.details === 'string' ? message.details : undefined,
    round: typeof message.round === 'number' ? message.round : 1,
    maxRounds: typeof message.maxRounds === 'number' ? message.maxRounds : 1,
    requestType,
  };
  settleProcessingForManualInteraction();
  sealAllStreamingMessages();
}

function handleOrchestratorRuntimeDiagnostics(message: WebviewMessage) {
  const store = getState();
  const runtimeReason = typeof message.runtimeReason === 'string' ? message.runtimeReason.trim() : '';
  const finalStatus = message.finalStatus === 'completed'
    || message.finalStatus === 'failed'
    || message.finalStatus === 'cancelled'
    || message.finalStatus === 'paused'
    ? message.finalStatus
    : null;
  if (!runtimeReason || !finalStatus) {
    return;
  }
  const sessionId = typeof message.sessionId === 'string' && message.sessionId.trim().length > 0
    ? message.sessionId.trim()
    : undefined;
  const currentSessionId = store.currentSessionId?.trim() || '';
  if (sessionId && currentSessionId && sessionId !== currentSessionId) {
    return;
  }
  const diagnostics: OrchestratorRuntimeDiagnostics = {
    runtimeReason,
    finalStatus,
    ...(sessionId ? { sessionId } : {}),
    ...(typeof message.requestId === 'string' && message.requestId.trim().length > 0
      ? { requestId: message.requestId.trim() }
      : {}),
    runtimeSnapshot: message.runtimeSnapshot && typeof message.runtimeSnapshot === 'object'
      ? (message.runtimeSnapshot as OrchestratorRuntimeDiagnostics['runtimeSnapshot'])
      : null,
    runtimeDecisionTrace: Array.isArray(message.runtimeDecisionTrace)
      ? (message.runtimeDecisionTrace as OrchestratorRuntimeDiagnostics['runtimeDecisionTrace'])
      : [],
    updatedAt: typeof message.updatedAt === 'number' && Number.isFinite(message.updatedAt)
      ? message.updatedAt
      : Date.now(),
  };
  setOrchestratorRuntimeDiagnostics(diagnostics);
}

function handleClarificationRequest(message: WebviewMessage) {
  const store = getState();
  if (store.appState?.interactionMode === 'auto') {
    addToast('info', i18n.t('messageHandler.autoSkipClarification'));
    vscode.postMessage({
      type: 'answerClarification',
      answers: null,
      additionalInfo: null,
      autoSkipped: true,  // 标记为自动跳过
    });
    setIsProcessing(true);
    return;
  }
  store.pendingClarification = {
    questions: ensureArray<string>(message.questions),
    context: message.context as string | undefined,
    ambiguityScore: message.ambiguityScore as number | undefined,
    originalPrompt: message.originalPrompt as string | undefined,
  };
  settleProcessingForManualInteraction();
  sealAllStreamingMessages();
}

function handleWorkerQuestionRequest(message: WebviewMessage) {
  const store = getState();
  if (store.appState?.interactionMode === 'auto') {
    addToast('info', i18n.t('messageHandler.autoAnswerWorkerQuestion'));
    vscode.postMessage({ type: 'answerWorkerQuestion', answer: null });
    setIsProcessing(true);
    return;
  }
  store.pendingWorkerQuestion = {
    workerId: (message.workerId as string) || '',
    question: (message.question as string) || '',
    context: message.context as string | undefined,
    options: message.options,
  };
  settleProcessingForManualInteraction();
  sealAllStreamingMessages();
}

function handleToolAuthorizationRequest(message: WebviewMessage) {
  const store = getState();
  const requestId = typeof message.requestId === 'string' && message.requestId.trim().length > 0
    ? message.requestId.trim()
    : '';
  if (!requestId) {
    console.error('[MessageHandler] toolAuthorizationRequest 缺少 requestId:', message);
    addToast('error', i18n.t('messageHandler.toolAuthMissingRequestId'), undefined, {
      category: 'incident',
      source: 'tool-auth',
      actionRequired: true,
    });
    return;
  }

  if (store.appState?.interactionMode === 'auto') {
    addToast('info', i18n.t('messageHandler.autoToolAuthorization'));
    vscode.postMessage({ type: 'toolAuthorizationResponse', requestId, allowed: true });
    clearRequestedInteractionMode();
    setIsProcessing(true);
    return;
  }

  // ask 模式下若已有交互弹窗，按规范拒绝并提示，避免覆盖当前待处理请求
  if (getActiveInteractionType()) {
    console.warn('[MessageHandler] toolAuthorizationRequest 与现有交互冲突，自动拒绝:', {
      requestId,
      activeInteraction: getActiveInteractionType(),
    });
    vscode.postMessage({ type: 'toolAuthorizationResponse', requestId, allowed: false });
    addToast('warning', i18n.t('messageHandler.toolAuthConflict'), undefined, {
      category: 'incident',
      source: 'tool-auth',
      actionRequired: true,
    });
    return;
  }

  clearPendingInteractions();
  store.pendingToolAuthorization = {
    requestId,
    toolName: (message.toolName as string) || '',
    toolArgs: message.toolArgs,
  };
  settleProcessingForManualInteraction();
  sealAllStreamingMessages();
}

function handleMissionPlanned(message: WebviewMessage) {
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

function handleAssignmentPlanned(message: WebviewMessage) {
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

function handleAssignmentStarted(message: WebviewMessage) {
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

function handleAssignmentCompleted(message: WebviewMessage) {
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

function handleTodoStarted(message: WebviewMessage) {
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

function handleTodoCompleted(message: WebviewMessage) {
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

function handleTodoFailed(message: WebviewMessage) {
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

function handleDynamicTodoAdded(message: WebviewMessage) {
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

function handleTodoApprovalRequested(message: WebviewMessage) {
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
  if (store.appState?.interactionMode === 'auto') {
    updateTodo(assignmentId, todoId, (todo) => ({
      ...todo,
      approvalStatus: 'approved',
      approvalNote: reason,
    }));
    vscode.postMessage({
      type: 'interactionResponse',
      requestId: `approval-${todoId}`,
      response: 'approved',
    });
    return;
  }

  updateTodo(assignmentId, todoId, (todo) => ({
    ...todo,
    approvalStatus: 'pending',
    approvalNote: reason,
  }));
}

function mapStandardMessage(standard: StandardMessage): Message {
  const blocks = mapStandardBlocks(standard.blocks || []);
  const content = blocksToContent(blocks);
  const isStreaming = standard.lifecycle === 'streaming' || standard.lifecycle === 'started';
  const isComplete = standard.lifecycle === 'completed';
  const isSystemNotice = standard.type === MessageType.SYSTEM || standard.type === MessageType.ERROR;
  const isErrorNotice = standard.type === MessageType.ERROR;

  // 区分消息来源与展示来源：
  // 标准消息 source 为 orchestrator/worker，UI 展示具体 Worker 槽位
  const originSource = standard.source;
  const agentSlot = normalizeWorkerSlot(standard.agent);
  const metaSlot = normalizeWorkerSlot((standard.metadata as { worker?: unknown } | undefined)?.worker);
  const metaAssigned = normalizeWorkerSlot((standard.metadata as { assignedWorker?: unknown } | undefined)?.assignedWorker);
  const resolvedWorker = agentSlot ?? metaSlot ?? metaAssigned ?? null;
  const displaySource: Message['source'] =
    originSource === 'orchestrator'
      ? 'orchestrator'
      : (resolvedWorker ?? 'claude');

  const baseMetadata = { ...(standard.metadata || {}) } as Record<string, unknown>;
  const rawCardId = typeof baseMetadata.cardId === 'string' ? baseMetadata.cardId.trim() : '';
  const resolvedAssignmentCardId = resolveTaskCardKeyFromMetadata(baseMetadata);
  const shouldUseAssignmentCard = standard.type === MessageType.INSTRUCTION || standard.type === MessageType.TASK_CARD;
  const cardId = shouldUseAssignmentCard
    ? (resolvedAssignmentCardId || rawCardId || standard.id)
    : (rawCardId || standard.id);
  const uiMessageId = (standard.type === MessageType.INSTRUCTION && resolvedAssignmentCardId)
    ? cardId
    : standard.id;

  const dispatchToWorker = Boolean(baseMetadata.dispatchToWorker);

  // 根据消息类型正确映射 role：用户输入消息 → 'user'，系统通知 → 'system'，其余 → 'assistant'
  const resolvedRole: 'user' | 'assistant' | 'system' =
    isSystemNotice ? 'system'
    : (standard.type === MessageType.USER_INPUT ? 'user' : 'assistant');

  // 直接传递 MessageType，UI 层使用 type === 'user_input' 判断用户消息
  const resolvedType = standard.type as import('../types/message').MessageType;

  return {
    id: uiMessageId,
    role: resolvedRole,
    source: displaySource,
    content,
    blocks,
    timestamp: standard.timestamp || Date.now(),
    isStreaming,
    isComplete,
    type: resolvedType,
    noticeType: isSystemNotice ? (isErrorNotice ? 'error' : 'info') : undefined,
    metadata: {
      ...baseMetadata,
      eventId: standard.eventId,
      eventSeq: standard.eventSeq,
      cardId,
      interaction: standard.interaction,
      worker: originSource === 'worker'
        ? (resolvedWorker ?? undefined)
        : (dispatchToWorker ? (resolvedWorker ?? undefined) : undefined),
    },
  };
}

function hasRenderableContent(message: Message): boolean {
  if (message.type === 'system-notice') return true;
  if (message.type === 'task_card') return true;
  if (message.type === 'instruction') return true;
  if (message.type === 'thinking') return true;
  if (message.content && message.content.trim()) return true;
  if (message.blocks && message.blocks.length > 0) {
    return message.blocks.some((block) => {
      if (block.type === 'text' || block.type === 'code' || block.type === 'thinking') {
        return Boolean(block.content && block.content.trim());
      }
      if (block.type === 'tool_call') {
        return true;
      }
      if (block.type === 'file_change' || block.type === 'plan') {
        return true;
      }
      return false;
    });
  }
  return false;
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

function mapStandardBlocks(blocks: StandardContentBlock[]): ContentBlock[] {
  const list = ensureArray<StandardContentBlock>(blocks);
  const invalid = list.filter((block) => !block || typeof block !== 'object' || !('type' in block));
  if (invalid.length > 0) {
    throw new Error('[MessageHandler] 标准消息块无效');
  }
  return list.map((block) => {
    switch (block.type) {
      case 'text':
        return {
          type: 'text',
          content: typeof block.content === 'string' ? block.content : '',
        };
      case 'code':
        return {
          type: 'code',
          content: typeof block.content === 'string' ? block.content : '',
          language: typeof block.language === 'string' ? block.language : undefined,
        };
      case 'thinking': {
        const thinking: ThinkingBlock = {
          content: typeof block.content === 'string' ? block.content : '',
          isComplete: true,
          summary: typeof block.summary === 'string' ? block.summary : undefined,
        };
        return {
          type: 'thinking',
          content: typeof block.content === 'string' ? block.content : '',
          thinking,
        };
      }
      case 'tool_call': {
        const toolStatus = mapToolStatus(
          block.status,
          block.standardized?.status,
          block.output,
          block.error
        );
        const standardizedStatus = (block.standardized?.status || '').toLowerCase();
        const standardizedHardError = standardizedStatus === 'error'
          || standardizedStatus === 'timeout'
          || standardizedStatus === 'killed';
        const standardizedError = block.standardized
          && standardizedHardError
          ? (block.standardized.message || undefined)
          : undefined;
        const toolCall: ToolCall = {
          id: block.toolId,
          name: block.toolName,
          arguments: safeParseJson(block.input) || {},
          status: toolStatus,
          result: block.output,
          error: block.error || standardizedError,
          standardized: block.standardized,
        };
        terminalSessions.ingestToolCall(toolCall);
        return {
          type: 'tool_call',
          content: '',
          toolCall,
        };
      }
      case 'file_change':
        return {
          type: 'file_change',
          content: '',
          fileChange: {
            filePath: block.filePath,
            changeType: block.changeType,
            additions: block.additions,
            deletions: block.deletions,
            diff: block.diff,
          },
        };
      case 'plan':
        return {
          type: 'plan',
          content: '',
          plan: {
            goal: block.goal,
            analysis: block.analysis,
            constraints: block.constraints,
            acceptanceCriteria: block.acceptanceCriteria,
            riskLevel: block.riskLevel,
            riskFactors: block.riskFactors,
            rawJson: block.rawJson,
          },
        };
      default:
        throw new Error(`[MessageHandler] 未支持的标准消息块类型: ${(block as { type: string }).type}`);
    }
  });
}

function applyStreamUpdate(message: Message, update: StreamUpdate): Partial<Message> {
  const updates: Partial<Message> = {};
  if (update.updateType === 'append' && update.appendText) {
    updates.content = (message.content || '') + update.appendText;
    const nextBlocks = [...(message.blocks || [])];
    let lastTextIndex = -1;
    for (let i = nextBlocks.length - 1; i >= 0; i--) {
      if (nextBlocks[i].type === 'text') {
        lastTextIndex = i;
        break;
      }
    }
    if (lastTextIndex >= 0) {
      const current = nextBlocks[lastTextIndex];
      nextBlocks[lastTextIndex] = {
        ...current,
        content: (current.content || '') + update.appendText,
      };
    } else {
      nextBlocks.push({ type: 'text', content: update.appendText });
    }
    updates.blocks = nextBlocks;
  } else if (update.updateType === 'replace') {
    if (update.blocks) {
      const blocks = mapStandardBlocks(update.blocks);
      updates.blocks = blocks;
      updates.content = blocksToContent(blocks);
    }
  } else if (update.updateType === 'block_update') {
    if (update.blocks) {
      const incoming = mapStandardBlocks(update.blocks);
      const merged = mergeBlocks(message.blocks || [], incoming);
      updates.blocks = merged;
      updates.content = blocksToContent(merged);
    }
  } else if (update.updateType === 'lifecycle_change' && update.lifecycle) {
    updates.isStreaming = update.lifecycle === 'streaming' || update.lifecycle === 'started';
    updates.isComplete = update.lifecycle === 'completed';
  }

  if (update.cardId || typeof update.cardStreamSeq === 'number' || update.eventId || typeof update.eventSeq === 'number') {
    updates.metadata = {
      ...(message.metadata || {}),
      ...(update.cardId ? { cardId: update.cardId } : {}),
      ...(typeof update.cardStreamSeq === 'number' ? { cardStreamSeq: update.cardStreamSeq } : {}),
      ...(update.eventId ? { eventId: update.eventId } : {}),
      ...(typeof update.eventSeq === 'number' ? { eventSeq: update.eventSeq } : {}),
    };
  }
  return updates;
}

function mergeBlocks(existing: ContentBlock[], incoming: ContentBlock[]): ContentBlock[] {
  const safeExisting = ensureArray(existing).filter((block): block is ContentBlock => !!block && typeof block === 'object' && 'type' in block);
  const safeIncoming = ensureArray(incoming).filter((block): block is ContentBlock => !!block && typeof block === 'object' && 'type' in block);
  const next = [...safeExisting];
  for (const block of safeIncoming) {
    if (block.type === 'tool_call' && block.toolCall?.id) {
      const idx = next.findIndex((b) => b.type === 'tool_call' && b.toolCall?.id === block.toolCall?.id);
      if (idx >= 0) {
        const prev = next[idx];
        const prevToolCall = prev.toolCall;
        const incomingToolCall = block.toolCall;
        next[idx] = {
          ...prev,
          ...block,
          toolCall: {
            ...prevToolCall,
            ...incomingToolCall,
            // block_update 允许只传增量字段，缺失字段必须保留已有值，
            // 否则会把 result/arguments 覆盖成 undefined，导致 terminalId 丢失（UI 显示 #-）
            arguments: incomingToolCall?.arguments ?? prevToolCall?.arguments,
            result: incomingToolCall?.result ?? prevToolCall?.result,
            error: incomingToolCall?.error ?? prevToolCall?.error,
            standardized: incomingToolCall?.standardized ?? prevToolCall?.standardized,
          },
        };
      } else {
        next.push(block);
      }
      continue;
    }
    if (block.type === 'thinking') {
      const idx = next.findIndex((b) => b.type === 'thinking');
      if (idx >= 0) {
        const prev = next[idx];
        const prevThinking = prev.thinking || { content: '', isComplete: false };
        const blockThinking = block.thinking || { content: '', isComplete: false };
        const mergedThinking = {
          content: blockThinking.content || prevThinking.content || block.content || prev.content || '',
          isComplete: blockThinking.isComplete ?? prevThinking.isComplete ?? true,
        };
        next[idx] = {
          ...prev,
          ...block,
          thinking: mergedThinking,
        };
      } else {
        next.push(block);
      }
      continue;
    }
    if (block.type === 'text') {
      const idx = [...next].map((b) => b.type).lastIndexOf('text');
      if (idx >= 0) {
        const prev = next[idx];
        next[idx] = { ...prev, content: (prev.content || '') + (block.content || '') };
      } else {
        next.push(block);
      }
      continue;
    }
    next.push(block);
  }
  return next;
}

function blocksToContent(blocks: ContentBlock[]): string {
  const textParts: string[] = [];
  for (const block of blocks) {
    if (!block) continue;
    if (block.type === 'text' || block.type === 'code' || block.type === 'thinking') {
      if (block.content) textParts.push(block.content);
    }
    if (block.type === 'file_change' && block.fileChange) {
      textParts.push(i18n.t('messageHandler.fileChange', { filePath: block.fileChange.filePath, changeType: block.fileChange.changeType }));
    }
    if (block.type === 'plan' && block.plan) {
      textParts.push(formatPlanBlock(block.plan));
    }
  }
  return textParts.join('\n\n');
}

function mapToolStatus(
  status: string | undefined,
  standardizedStatus?: string,
  output?: string,
  error?: string,
): ToolCall['status'] {
  switch (status) {
    case 'pending':
      return 'pending';
    case 'running':
      return 'running';
    case 'success':
      return 'success';
    case 'error':
      return 'error';
    case 'completed':
      return 'success';
    case 'failed':
      return 'error';
  }

  if (standardizedStatus) {
    switch (standardizedStatus.toLowerCase()) {
      case 'success':
        return 'success';
      case 'error':
      case 'timeout':
      case 'killed':
        return 'error';
      case 'blocked':
      case 'rejected':
      case 'aborted':
        return 'success';
      default:
        break;
    }
  }

  if (error) return 'error';
  if (output) return 'success';
  return 'running';
}

function safeParseJson(value?: string): Record<string, unknown> | null {
  if (!value || typeof value !== 'string') return null;
  try {
    return JSON.parse(value) as Record<string, unknown>;
  } catch {
    return null;
  }
}

function formatPlanBlock(block: any): string {
  const parts: string[] = [];
  if (block.goal) parts.push(i18n.t('messageHandler.planGoal', { goal: block.goal }));
  if (block.analysis) parts.push(i18n.t('messageHandler.planAnalysis', { analysis: block.analysis }));
  if (Array.isArray(block.constraints) && block.constraints.length > 0) {
    parts.push(`${i18n.t('messageHandler.planConstraints')}\n- ${block.constraints.join('\n- ')}`);
  }
  if (Array.isArray(block.acceptanceCriteria) && block.acceptanceCriteria.length > 0) {
    parts.push(`${i18n.t('messageHandler.planAcceptanceCriteria')}\n- ${block.acceptanceCriteria.join('\n- ')}`);
  }
  if (block.riskLevel) parts.push(i18n.t('messageHandler.planRiskLevel', { riskLevel: block.riskLevel }));
  if (Array.isArray(block.riskFactors) && block.riskFactors.length > 0) {
    parts.push(`${i18n.t('messageHandler.planRiskFactors')}\n- ${block.riskFactors.join('\n- ')}`);
  }
  return parts.join('\n\n');
}

/**
 * 处理 Worker 状态更新消息
 * 将检测到的模型状态同步到全局 store，供 BottomTabs 和 SettingsPanel 共用
 */
function handleWorkerStatusUpdate(message: WebviewMessage) {
  const statuses = message.statuses as ModelStatusMap;
  if (!statuses) return;

  const store = getState();

  // 直接存储完整的状态信息，不再简化
  // 这样 BottomTabs 和 SettingsPanel 可以使用同一个数据源
  store.modelStatus = { ...store.modelStatus, ...statuses };
}

/**
 * 处理连接测试结果消息（全局）
 * 将连接测试的状态同步到全局 store，确保即使 SettingsPanel 已卸载，
 * BottomTabs 等其他组件也能获取最新状态。
 */
function handleConnectionTestResult(message: WebviewMessage) {
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

function handleWorkerSessionCreated(message: WebviewMessage) {
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

  const session: WorkerSessionState = {
    sessionId,
    assignmentId,
    workerId,
    isResumed: false,
    completedTodos: 0,
  };

  addWorkerSession(session);
}

function handleWorkerSessionResumed(message: WebviewMessage) {
  const sessionId = (message.sessionId as string) || '';
  const assignmentId = (message.assignmentId as string) || '';
  const completedTodos = (message.completedTodos as number) || 0;
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

  // 更新现有 session 或创建新的
  const store = getState();
  const existing = store.workerSessions.get(sessionId);

  if (existing) {
    updateWorkerSession(sessionId, {
      isResumed: true,
      completedTodos,
    });
  } else {
    const session: WorkerSessionState = {
      sessionId,
      assignmentId,
      workerId,
      isResumed: true,
      completedTodos,
    };
    addWorkerSession(session);
  }

  // 系统通知由 MessageHub 下发，前端不再本地创建
}
