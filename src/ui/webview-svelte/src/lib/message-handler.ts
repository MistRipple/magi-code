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
  setAppState,
  setMissionPlan,
  setInteractionMode,
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
} from '../stores/messages.svelte';
import type { Message, AppState, Session, ContentBlock, ToolCall, ThinkingBlock, MissionPlan, AssignmentPlan, AssignmentTodo, WorkerSessionState, Task, Edit, ModelStatusMap } from '../types/message';
import type { StandardMessage, StreamUpdate, ContentBlock as StandardContentBlock } from '../../../../protocol/message-protocol';
import { MessageType, MessageCategory } from '../../../../protocol/message-protocol';
import { routeStandardMessage, getMessageTarget, clearMessageTargets, clearMessageTarget, setMessageTarget } from './message-router';
import { normalizeWorkerSlot } from './message-classifier';
import { ensureArray } from './utils';

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

/**
 * 处理来自扩展的消息
 */
function handleMessage(message: WebviewMessage) {
  const { type } = message;

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

function resolvePendingInteractionsOnAutoMode(): void {
  const store = getState();
  const hasPending = Boolean(
    store.pendingToolAuthorization
    || store.pendingConfirmation
    || store.pendingRecovery
    || store.pendingQuestion
    || store.pendingClarification
    || store.pendingWorkerQuestion
  );
  if (!hasPending) {
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
    const decision: 'retry' | 'rollback' | 'continue' = store.pendingRecovery.canRetry
      ? 'retry'
      : (store.pendingRecovery.canRollback ? 'rollback' : 'continue');
    vscode.postMessage({ type: 'confirmRecovery', decision });
  }

  if (store.pendingQuestion) {
    vscode.postMessage({ type: 'answerQuestions', answer: null });
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

  clearPendingInteractions();
  clearRequestedInteractionMode();
  setIsProcessing(true);
  addToast('info', '已切换到自动模式，待处理交互已按自动策略继续');
}

function applyInteractionModeFromPayload(rawMode: unknown, source: string, rawUpdatedAt?: unknown): void {
  const resolved = resolveInteractionMode(rawMode);
  if (!resolved) {
    console.error(`[MessageHandler] ${source} 收到非法 interactionMode:`, rawMode);
    addToast('error', '收到非法交互模式，已忽略该更新');
    return;
  }
  const updatedAt = typeof rawUpdatedAt === 'number' ? rawUpdatedAt : undefined;
  setInteractionMode(resolved, updatedAt);
  if (resolved === 'auto') {
    resolvePendingInteractionsOnAutoMode();
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

  if (state.sessions) {
    updateSessions(ensureArray(state.sessions) as Session[]);
  }

  if ((state as any).currentSessionId) {
    setCurrentSessionId((state as any).currentSessionId as string);
  }

  const store = getState();
  const taskSeen = new Set<string>();
  store.tasks = ensureArray(state.tasks)
    .filter((task): task is Task => !!task && typeof task === 'object' && typeof (task as Task).status === 'string')
    .map((task) => {
      const id = typeof task.id === 'string' && task.id.trim() ? task.id.trim() : '';
      if (!id) {
        throw new Error('[MessageHandler] Task 缺少 id');
      }
      if (taskSeen.has(id)) {
        throw new Error(`[MessageHandler] Task id 重复: ${id}`);
      }
      taskSeen.add(id);
      return {
        id,
      name: task.name || task.prompt || '',
      description: task.description,
      status: task.status,
      };
    });
  const editSeen = new Set<string>();
  store.edits = ensureArray(state.pendingChanges)
    .filter((change): change is Edit => !!change && typeof change === 'object' && typeof (change as Edit).filePath === 'string' && !!(change as Edit).filePath)
    .filter((change) => {
      if (editSeen.has(change.filePath)) {
        return false;
      }
      editSeen.add(change.filePath);
      return true;
    })
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
        type: inferredType,
        additions: change.additions,
        deletions: change.deletions,
        contributors: change.contributors,
        workerId: change.workerId,
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

  if (typeof state.interactionMode === 'string') {
    applyInteractionModeFromPayload(state.interactionMode, 'stateUpdate', state.interactionModeUpdatedAt);
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

function recoverTargetByCardId(cardId: string): { target: ResolvedTarget; messageId: string } | null {
  if (!cardId.trim()) {
    return null;
  }
  const state = getState();
  const threadMessage = findLastMessageByCardId(state.threadMessages, cardId);
  const workerMatches: Array<{ worker: WorkerSlot; message: Message }> = [];
  for (const worker of WORKER_SLOTS) {
    const message = findLastMessageByCardId(state.agentOutputs[worker], cardId);
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

function shouldIgnoreSealedUpdate(existing: Message, update: StreamUpdate): boolean {
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
      applied = true;
    }
  } else if (location.location === 'worker') {
    const existing = getState().agentOutputs[location.worker].find(m => m.id === update.messageId);
    if (existing) {
      if (shouldIgnoreSealedUpdate(existing, update)) {
        return true;
      }
      const streamUpdates = applyStreamUpdate(existing, update);
      updateAgentMessage(location.worker, update.messageId, { ...existing, ...streamUpdates });
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
      applied = true;
    }
    const agentExisting = getState().agentOutputs[location.worker].find(m => m.id === update.messageId);
    if (agentExisting) {
      if (shouldIgnoreSealedUpdate(agentExisting, update)) {
        return true;
      }
      const streamUpdates = applyStreamUpdate(agentExisting, update);
      updateAgentMessage(location.worker, update.messageId, { ...agentExisting, ...streamUpdates });
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
        addToast('error', '等待响应超时，请重试');
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

          // 2. 构造新消息对象
          const newMessage: import('../types/message').Message = {
            ...uiMessage,
            metadata: {
              ...(uiMessage.metadata || {}),
              requestId, // 保持请求关联
              isPlaceholder: false,
              wasPlaceholder: true,
            },
          };

          // 3. 在 UI 中原地替换（保持滚动位置和顺序）
          replaceThreadMessage(placeholderId, newMessage);

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

  // 补齐可能提前到达的流式更新
  flushPendingStreamUpdates(actualMessageId);

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
      if (requestId) {
        clearPendingRequest(requestId);
      }
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
      // 任务生命周期终极信号：彻底清除所有处理态
      // 清除 backendProcessing + activeMessageIds + pendingRequests 三重条件，
      // 防止 Worker 多轮工具调用中间消息的 COMPLETE 事件延迟或丢失导致按钮状态残留
      clearProcessingState();
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
    case 'stateUpdate':
      handleStateUpdate(asMessage({ state: payload.state }));
      break;

    case 'processingStateChanged': {
      const isProcessing = payload.isProcessing as boolean | undefined;
      // 仅将 true 作为兜底提升信号，禁止用该通道提前清空运行态
      // 运行态结束必须由 task_completed/task_failed 决定
      if (isProcessing === true) {
        setIsProcessing(true);
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

    case 'confirmationRequest':
      handleConfirmationRequest(asMessage(payload));
      break;

    case 'recoveryRequest':
      handleRecoveryRequest(asMessage(payload));
      break;

    case 'questionRequest':
      handleQuestionRequest(asMessage(payload));
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

    case 'interactionModeChanged':
      if (isStaleInteractionModeUpdate(payload, 'interactionModeChanged')) {
        break;
      }
      applyInteractionModeFromPayload(payload.mode, 'interactionModeChanged', payload.updatedAt);
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
  }
}

function handleConfirmationRequest(message: WebviewMessage) {
  const store = getState();
  if (store.appState?.interactionMode === 'auto') {
    addToast('info', '自动模式已确认执行计划并继续');
    vscode.postMessage({ type: 'confirmPlan', confirmed: true });
    clearRequestedInteractionMode();
    setIsProcessing(true);
    return;
  }
  store.pendingConfirmation = {
    plan: message.plan,
    formattedPlan: message.formattedPlan as string | undefined,
  };
  setIsProcessing(false);
}

function handleRecoveryRequest(message: WebviewMessage) {
  const store = getState();
  if (store.appState?.interactionMode === 'auto') {
    const canRetry = Boolean(message.canRetry);
    const canRollback = Boolean(message.canRollback);
    const decision: 'retry' | 'rollback' | 'continue' = canRetry
      ? 'retry'
      : (canRollback ? 'rollback' : 'continue');
    addToast('info', `自动处理恢复请求：已选择${decision === 'retry' ? '重试' : decision === 'rollback' ? '回滚' : '继续'}`);
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
  setIsProcessing(false);
}

function handleQuestionRequest(message: WebviewMessage) {
  const store = getState();
  if (store.appState?.interactionMode === 'auto') {
    addToast('info', '自动处理提问：未提供答案，按默认策略继续');
    vscode.postMessage({ type: 'answerQuestions', answer: null });
    setIsProcessing(true);
    return;
  }
  store.pendingQuestion = {
    questions: ensureArray<string>(message.questions),
    plan: message.plan,
  };
  setIsProcessing(false);
}

function handleClarificationRequest(message: WebviewMessage) {
  const store = getState();
  if (store.appState?.interactionMode === 'auto') {
    addToast('info', '自动模式已跳过澄清并继续执行');
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
  setIsProcessing(false);
}

function handleWorkerQuestionRequest(message: WebviewMessage) {
  const store = getState();
  if (store.appState?.interactionMode === 'auto') {
    addToast('info', '自动处理 Worker 提问：未提供答案，按默认策略继续');
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
  setIsProcessing(false);
}

function handleToolAuthorizationRequest(message: WebviewMessage) {
  const store = getState();
  const requestId = typeof message.requestId === 'string' && message.requestId.trim().length > 0
    ? message.requestId.trim()
    : '';
  if (!requestId) {
    console.error('[MessageHandler] toolAuthorizationRequest 缺少 requestId:', message);
    addToast('error', '工具授权请求缺少标识，已忽略该请求');
    return;
  }

  if (store.appState?.interactionMode === 'auto') {
    addToast('info', '自动模式已自动授权工具调用并继续');
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
    addToast('warning', '当前有待处理交互，已自动拒绝本次工具授权请求');
    return;
  }

  clearPendingInteractions();
  store.pendingToolAuthorization = {
    requestId,
    toolName: (message.toolName as string) || '',
    toolArgs: message.toolArgs,
  };
  setIsProcessing(false);
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
    throw new Error('[MessageHandler] AssignmentStarted 缺少 assignmentId');
  }
  updateAssignmentPlan(assignmentId, (assignment) => ({
    ...assignment,
    status: 'running',
  }));
}

function handleAssignmentCompleted(message: WebviewMessage) {
  const assignmentId = message.assignmentId as string;
  if (!assignmentId || !assignmentId.trim()) {
    throw new Error('[MessageHandler] AssignmentCompleted 缺少 assignmentId');
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
  const resolvedWorker = agentSlot ?? metaSlot ?? null;
  const displaySource: Message['source'] =
    originSource === 'orchestrator'
      ? 'orchestrator'
      : (resolvedWorker ?? 'claude');

  const baseMetadata = { ...(standard.metadata || {}) } as Record<string, unknown>;
  const cardId = typeof baseMetadata.cardId === 'string' && baseMetadata.cardId.trim()
    ? baseMetadata.cardId.trim()
    : standard.id;

  const dispatchToWorker = Boolean(baseMetadata.dispatchToWorker);

  // role 字段仅用于系统消息，用户消息通过 type 判断
  const resolvedRole: 'user' | 'assistant' | 'system' =
    isSystemNotice ? 'system' : 'assistant';

  // 直接传递 MessageType，UI 层使用 type === 'user_input' 判断用户消息
  const resolvedType = standard.type as import('../types/message').MessageType;

  return {
    id: standard.id,
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
  const plan = store.missionPlan;
  if (!plan) return;
  const index = plan.assignments.findIndex((a) => a.id === assignmentId);
  if (index === -1) return;
  const nextAssignments = plan.assignments.map((assignment, i) =>
    i === index ? updater(assignment) : assignment
  );
  setMissionPlan({ ...plan, assignments: nextAssignments });
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
      case 'code':
        return {
          type: 'code',
          content: block.content,
          language: block.language,
        };
      case 'thinking': {
        const thinking: ThinkingBlock = {
          content: block.content || '',
          isComplete: true,
          summary: (block as any).summary,
        };
        return {
          type: 'thinking',
          content: block.content || '',
          thinking,
        };
      }
      case 'tool_call': {
        const toolCall: ToolCall = {
          id: block.toolId,
          name: block.toolName,
          arguments: safeParseJson(block.input) || {},
          status: mapToolStatus(block.status),
          result: block.output,
          error: block.error,
        };
        return {
          type: 'tool_call',
          content: '',
          toolCall,
        };
      }
      case 'file_change': {
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
      }
      case 'plan': {
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
      }
      default:
        return { type: 'text', content: block.content || '' };
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
        next[idx] = {
          ...prev,
          ...block,
          toolCall: { ...prev.toolCall, ...block.toolCall },
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
      textParts.push(`文件变更: ${block.fileChange.filePath} (${block.fileChange.changeType})`);
    }
    if (block.type === 'plan' && block.plan) {
      textParts.push(formatPlanBlock(block.plan));
    }
  }
  return textParts.join('\n\n');
}

function mapToolStatus(status: string | undefined): ToolCall['status'] {
  switch (status) {
    case 'pending':
      return 'pending';
    case 'running':
      return 'running';
    case 'completed':
      return 'success';
    case 'failed':
      return 'error';
    default:
      return 'success';
  }
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
  if (block.goal) parts.push(`目标: ${block.goal}`);
  if (block.analysis) parts.push(`分析: ${block.analysis}`);
  if (Array.isArray(block.constraints) && block.constraints.length > 0) {
    parts.push(`约束:\n- ${block.constraints.join('\n- ')}`);
  }
  if (Array.isArray(block.acceptanceCriteria) && block.acceptanceCriteria.length > 0) {
    parts.push(`验收标准:\n- ${block.acceptanceCriteria.join('\n- ')}`);
  }
  if (block.riskLevel) parts.push(`风险等级: ${block.riskLevel}`);
  if (Array.isArray(block.riskFactors) && block.riskFactors.length > 0) {
    parts.push(`风险因素:\n- ${block.riskFactors.join('\n- ')}`);
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
