/**
 * 消息处理器 - 处理来自宿主桥接层的消息
 *
 * 仅负责 CONTENT 消息的接收、路由、流式更新和完成处理。
 * DATA / CONTROL / NOTIFY 消息处理已拆分到 data-message-handlers.ts。
 * 共享工具函数位于 message-utils.ts。
 */

import type { ClientBridge, ClientBridgeMessage } from '../shared/bridges/client-bridge';
import { getClientBridge } from '../shared/bridges/bridge-runtime';
import {
  getState,
  markMessageActive,
  markMessageComplete,
  addPendingRequest,
  clearPendingRequest,
  settleProcessingAfterResponseCompletion,
  addToast,
  getRequestBinding,
  createRequestBinding,
  updateRequestBinding,
  clearRequestBinding,
  settleProcessingForManualInteraction,
  sealAllStreamingMessages,
  applyTimelineStreamPatch,
  getTimelineMessageById,
  upsertTimelineNode,
} from '../stores/messages.svelte';
import type { Message, ContentBlock, SessionTimelineProjection } from '../types/message';
import type { StandardMessage, StreamUpdate } from '../shared/protocol/message-protocol';
import { MessageType, MessageCategory, MessageLifecycle } from '../shared/protocol/message-protocol';
import { resolveTaskCardWorkerSlot } from './worker-role-utils';
import { ensureArray } from './utils';
import { i18n } from '../stores/i18n.svelte';
import { upsertDispatchGroupLane } from '../shared/dispatch-group-lane-upsert';
import {
  messageHasRenderableTimelineContent,
  resolveTimelineWorkerVisibility as resolveSharedTimelineWorkerVisibility,
} from '../shared/timeline-presentation';
import {
  handleUnifiedControlMessage,
  handleUnifiedNotify,
  handleUnifiedData,
} from './data-message-handlers';
import {
  mapStandardBlocks,
  formatPlanBlock,
} from './message-utils';
import { mergeCompleteBlocksForFinalization } from './streaming-complete-merge';
import { resolveTimelineWorkerId } from '../shared/timeline-worker-lifecycle';
import {
  canStandardMessageTakeOverRequestPlaceholder,
  shouldTakeOverRequestPlaceholder,
} from './request-placeholder-policy';

import { resolveStandardMessageSessionBinding } from '../shared/standard-message-session-binding';

// Re-export for external consumers
export {
  normalizeRestoredMessages,
  handleRetryRuntimePayload,
  mapStandardBlocks,
} from './message-utils';


function assertStandardMessageId(standard: StandardMessage): StandardMessage {
  if (standard.id && standard.id.trim()) {
    return standard;
  }
  throw new Error('[MessageHandler] 标准消息缺少 id');
}

function shouldAcceptStandardMessageForCurrentSession(standard: StandardMessage): boolean {
  if (standard.category === MessageCategory.DATA) {
    return true;
  }
  const currentSessionId = getState().currentSessionId?.trim() || '';
  const binding = resolveStandardMessageSessionBinding(standard);
  if (!currentSessionId) {
    // 多会话并行：即使 currentSessionId 未设置，也只接受没有 sessionId 标记的消息
    // 防止 session 切换的空窗期泄漏其他 session 的消息
    return !binding.sessionId;
  }
  if (!binding.sessionId) {
    return true;
  }
  return binding.sessionId === currentSessionId;
}

function shouldAcceptUpdateForCurrentSession(
  bridgeMessage: ClientBridgeMessage,
  update: StreamUpdate,
): boolean {
  const currentSessionId = getState().currentSessionId?.trim() || '';
  const bridgeSessionId = typeof bridgeMessage.sessionId === 'string' ? bridgeMessage.sessionId.trim() : '';
  const updateSessionId = typeof (update as StreamUpdate & { sessionId?: unknown }).sessionId === 'string'
    ? ((update as StreamUpdate & { sessionId?: string }).sessionId || '').trim()
    : '';
  const effectiveSessionId = bridgeSessionId || updateSessionId;
  if (!currentSessionId) {
    // 多会话并行：即使 currentSessionId 未设置，也只接受没有 sessionId 标记的更新
    return !effectiveSessionId;
  }
  if (!effectiveSessionId) {
    return false;
  }
  return effectiveSessionId === currentSessionId;
}


function isDebugMode(): boolean {
  if (typeof window === 'undefined') {
    return false;
  }
  if ((window as unknown as Record<string, unknown>).__DEBUG_MODE__ === true) {
    return true;
  }
  try {
    return localStorage.getItem('magi:debugMode') === 'true';
  } catch {
    return false;
  }
}

function shouldHideFromUser(standard: StandardMessage): boolean {
  const visibility = typeof standard.visibility === 'string'
    ? standard.visibility.trim().toLowerCase()
    : '';
  if (visibility === 'system') {
    return true;
  }
  return visibility === 'debug' && !isDebugMode();
}

function resolveLaneWorkerSlot(standard: StandardMessage): string | null {
  const metadata = standard.metadata as Record<string, unknown> | undefined;
  if (standard.type === MessageType.USER_INPUT) {
    return resolveTimelineWorkerId(metadata) || null;
  }
  if (standard.type === MessageType.TASK_CARD) {
    return resolveTaskCardWorkerSlot(metadata);
  }
  if (standard.type === MessageType.INSTRUCTION) {
    return resolveTimelineWorkerId(metadata, { fallbacks: [standard.agent] }) || null;
  }
  if (standard.source === 'worker') {
    return resolveTimelineWorkerId(metadata, { fallbacks: [standard.agent, standard.source] }) || null;
  }
  return null;
}

function requiresWorkerLane(standard: StandardMessage): boolean {
  if (standard.type === MessageType.TASK_CARD && standard.source === 'orchestrator') {
    return false;
  }
  return standard.source === 'worker'
    || standard.type === MessageType.INSTRUCTION
    || standard.type === MessageType.TASK_CARD;
}

function resolveTimelineVisibilityForStandard(standard: StandardMessage): {
  thread: boolean;
  workerTabs?: string[];
} {
  const workerSlot = resolveLaneWorkerSlot(standard);
  const visibility = resolveSharedTimelineWorkerVisibility({
    hasWorker: Boolean(workerSlot),
    type: standard.type,
    source: standard.source,
    blocks: standard.blocks,
    metadata: standard.metadata as Record<string, unknown> | undefined,
  });
  return {
    thread: visibility.threadVisible,
    workerTabs: visibility.includeWorkerTab && workerSlot ? [workerSlot] : undefined,
  };
}





/**
 * 初始化消息处理器。
 * Web 端 bridge 是全局单例，必须保证同一时刻只有一个 message listener，
 * 否则会导致同一条桥消息被重复消费，破坏 eventSeq 单调性。
 */
let activeBridgeListenerCleanup: (() => void) | null = null;
let activeBridgeInstance: ClientBridge | null = null;

export function initMessageHandler(bridge: ClientBridge = getClientBridge()) {
  if (activeBridgeInstance === bridge && activeBridgeListenerCleanup) {
    console.log('[MessageHandler] 消息处理器已初始化，跳过重复绑定');
    return;
  }
  if (activeBridgeListenerCleanup) {
    activeBridgeListenerCleanup();
    activeBridgeListenerCleanup = null;
  }
  activeBridgeInstance = bridge;
  activeBridgeListenerCleanup = bridge.onMessage(handleMessage);
  console.log('[MessageHandler] 消息处理器已初始化');
}

let lastAppliedEventSeq = 0;
let eventSeqSessionId = '';
const processedEventKeys = new Set<string>();
const processedEventKeyQueue: string[] = [];
const MAX_TRACKED_EVENT_KEYS = 4000;
const UNHANDLED_MESSAGE_TOAST_WINDOW_MS = 3000;
let lastUnhandledMessageErrorSignature = '';
let lastUnhandledMessageErrorAt = 0;
const pendingTimelineUpdatesByMessageId = new Map<string, StreamUpdate[]>();
const MAX_PENDING_TIMELINE_UPDATES_PER_KEY = 200;

function resetEventSeqTracking(seed: number = 0, sessionId?: string): void {
  lastAppliedEventSeq = seed > 0 ? Math.floor(seed) : 0;
  eventSeqSessionId = typeof sessionId === 'string' ? sessionId.trim() : '';
  processedEventKeys.clear();
  processedEventKeyQueue.length = 0;
  pendingTimelineUpdatesByMessageId.clear();
}

export function primeEventSeqTracking(
  sessionId: string | null | undefined,
  seed: number = 0,
): void {
  const normalizedSessionId = typeof sessionId === 'string' ? sessionId.trim() : '';
  resetEventSeqTracking(seed, normalizedSessionId);
}

function syncEventSeqTrackingFromProjection(message: ClientBridgeMessage): void {
  const incomingSessionId = resolveEventTrackingSessionId(message);
  if (incomingSessionId && incomingSessionId !== eventSeqSessionId) {
    resetEventSeqTracking(0, incomingSessionId);
  }
  const projection = message.timelineProjection as SessionTimelineProjection | undefined;
  const seed = typeof projection?.lastAppliedEventSeq === 'number' && Number.isFinite(projection.lastAppliedEventSeq)
    ? Math.max(0, Math.floor(projection.lastAppliedEventSeq))
    : 0;
  // 只能向前推进：如果 projection 里的 seed 落后于当前 live 追踪，
  // 不回退 lastAppliedEventSeq，避免重复 bootstrap 把基线拉回到过去。
  if (seed > lastAppliedEventSeq) {
    lastAppliedEventSeq = seed;
  }
  if (incomingSessionId) {
    eventSeqSessionId = incomingSessionId;
  }
  // 不清除 processedEventKeys：同 session 的重复 bootstrap 不应丢弃去重记忆
}

function resolveEventTrackingSessionId(
  message: ClientBridgeMessage,
  options: { allowCurrentSessionFallback?: boolean } = {},
): string {
  if (typeof message.sessionId === 'string' && message.sessionId.trim()) {
    return message.sessionId.trim();
  }
  if (message.type === 'unifiedMessage' || message.type === 'unifiedComplete') {
    const standard = message.message as StandardMessage | undefined;
    const metadata = standard?.metadata && typeof standard.metadata === 'object'
      ? standard.metadata as Record<string, unknown>
      : undefined;
    if (typeof metadata?.sessionId === 'string' && metadata.sessionId.trim()) {
      return metadata.sessionId.trim();
    }
    const payload = standard?.data?.payload;
    if (payload && typeof payload === 'object' && !Array.isArray(payload)) {
      const payloadSessionId = (payload as Record<string, unknown>).sessionId;
      if (typeof payloadSessionId === 'string' && payloadSessionId.trim()) {
        return payloadSessionId.trim();
      }
    }
  }
  return options.allowCurrentSessionFallback === true
    ? (getState().currentSessionId?.trim() || '')
    : '';
}

function syncEventSeqTrackingFromBootstrap(standard: StandardMessage): void {
  const payload = standard.data?.payload as Record<string, unknown> | undefined;
  if (!payload) {
    return;
  }
  const incomingSessionId = typeof payload.sessionId === 'string' ? payload.sessionId.trim() : '';
  if (incomingSessionId && incomingSessionId !== eventSeqSessionId) {
    resetEventSeqTracking(0, incomingSessionId);
  } else if (incomingSessionId) {
    eventSeqSessionId = incomingSessionId;
  }
  syncEventSeqTrackingFromProjection({
    type: 'sessionBootstrapLoaded',
    sessionId: incomingSessionId,
    timelineProjection: payload.timelineProjection,
  });
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

function enqueuePendingTimelineUpdate(update: StreamUpdate): void {
  const messageId = typeof update.messageId === 'string' ? update.messageId.trim() : '';
  if (!messageId) {
    return;
  }
  const pushUpdate = (store: Map<string, StreamUpdate[]>, key: string) => {
    if (!key) {
      return;
    }
    const queue = store.get(key) || [];
    queue.push(update);
    if (queue.length > MAX_PENDING_TIMELINE_UPDATES_PER_KEY) {
      queue.splice(0, queue.length - MAX_PENDING_TIMELINE_UPDATES_PER_KEY);
    }
    store.set(key, queue);
  };
  pushUpdate(pendingTimelineUpdatesByMessageId, messageId);
}

function drainPendingTimelineUpdates(messageIds: string | string[]): StreamUpdate[] {
  const normalizedMessageIds = (Array.isArray(messageIds) ? messageIds : [messageIds])
    .map((messageId) => (typeof messageId === 'string' ? messageId.trim() : ''))
    .filter((messageId, index, array) => messageId.length > 0 && array.indexOf(messageId) === index);
  if (normalizedMessageIds.length === 0) {
    return [];
  }
  const drained: StreamUpdate[] = [];
  const seen = new Set<string>();
  const take = (store: Map<string, StreamUpdate[]>, key: string) => {
    if (!key) {
      return;
    }
    const queued = store.get(key);
    if (!queued || queued.length === 0) {
      store.delete(key);
      return;
    }
    store.delete(key);
    for (const item of queued) {
      const signature = `${item.messageId}:${item.eventSeq || 0}:${item.updateType || ''}:${item.timestamp || 0}`;
      if (seen.has(signature)) {
        continue;
      }
      seen.add(signature);
      drained.push(item);
    }
  };
  for (const normalizedMessageId of normalizedMessageIds) {
    take(pendingTimelineUpdatesByMessageId, normalizedMessageId);
  }
  drained.sort((a, b) => (a.eventSeq || 0) - (b.eventSeq || 0));
  return drained;
}

function applyPendingTimelineUpdatesForAnchor(message: Message, aliasIds: string[] = []): void {
  const pendingUpdates = drainPendingTimelineUpdates([message.id, ...aliasIds]);
  if (pendingUpdates.length === 0) {
    return;
  }
  for (const update of pendingUpdates) {
    const existingMessage = getTimelineMessageById(update.messageId);
    const updateAnchorId = update.messageId;
    if (!existingMessage) {
      enqueuePendingTimelineUpdate(update);
      continue;
    }
    if (update.updateType === 'lifecycle' && update.lifecycle === 'completed') {
      markMessageComplete(updateAnchorId);
      if (updateAnchorId !== update.messageId) {
        markMessageComplete(update.messageId);
      }
    }
    if (!update.updateType) {
      continue;
    }
    const patch = applyStreamUpdate(existingMessage, update);
    if (Object.keys(patch).length > 0) {
      applyTimelineStreamPatch(updateAnchorId, patch);
    }
  }
}

function resolveEventSeqAndKey(message: ClientBridgeMessage): { eventSeq?: number; eventKey?: string } {
  if (message.type === 'unifiedUpdate') {
    const update = message.update as StreamUpdate | undefined;
    const seq = update?.eventSeq;
    if (typeof seq !== 'number' || !Number.isFinite(seq)) {
      return {};
    }
    const eventId = typeof update?.eventId === 'string' && update.eventId.trim()
      ? update.eventId.trim()
      : `upd:${update?.messageId || 'unknown'}:${update?.updateType || 'unknown'}:${seq}`;
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

function shouldProcessByEventSeq(message: ClientBridgeMessage): boolean {
  const incomingSessionId = resolveEventTrackingSessionId(message);
  // 会话切换时：无条件重置 eventSeq 追踪状态
  // 这确保后端重启或新会话时，eventSeq 从 0 开始不会被误判为逆序
  if (incomingSessionId && incomingSessionId !== eventSeqSessionId) {
    resetEventSeqTracking(0, incomingSessionId);
  } else if (!eventSeqSessionId && incomingSessionId) {
    eventSeqSessionId = incomingSessionId;
  }
  const { eventSeq, eventKey } = resolveEventSeqAndKey(message);
  // SSE 单连接语义下，不应出现严格递减序号。
  // 一旦出现，通常意味着后端 runtime 重启后序号重新起算，必须重置基线。
  if (eventSeq !== undefined && eventSeq < lastAppliedEventSeq) {
    console.warn('[MessageHandler] 检测到 eventSeq 回退，重置追踪基线', {
      eventSeq,
      lastAppliedEventSeq,
      type: message.type,
    });
    resetEventSeqTracking(eventSeq - 1, incomingSessionId || eventSeqSessionId);
  }
  if (eventSeq === undefined) {
    return true;
  }
  if (eventKey && processedEventKeys.has(eventKey)) {
    return false;
  }
  // SSE 是单连接串行传输，正常情况下不会出现逆序。
  // 如果出现，说明发送侧存在重复注入（如重复 bootstrap），直接丢弃。
  if (eventSeq <= lastAppliedEventSeq) {
    return false;
  }
  lastAppliedEventSeq = Math.max(lastAppliedEventSeq, eventSeq);
  if (eventKey) {
    rememberProcessedEventKey(eventKey);
  }
  return true;
}

function buildRequestPlaceholderMetadata(
  existingMetadata: Record<string, unknown> | undefined,
  incomingMetadata: Record<string, unknown> | undefined,
  requestId: string,
  keepPlaceholderVisible: boolean,
): Record<string, unknown> {
  const preservedPlaceholderState = (
    typeof existingMetadata?.placeholderState === 'string' && existingMetadata.placeholderState.trim()
      ? existingMetadata.placeholderState.trim()
      : (typeof incomingMetadata?.placeholderState === 'string' && incomingMetadata.placeholderState.trim()
          ? incomingMetadata.placeholderState.trim()
          : 'pending')
  );

  return {
    ...(existingMetadata || {}),
    ...(incomingMetadata || {}),
    requestId,
    isPlaceholder: keepPlaceholderVisible,
    wasPlaceholder: keepPlaceholderVisible
      ? existingMetadata?.wasPlaceholder === true
      : true,
    placeholderState: keepPlaceholderVisible ? preservedPlaceholderState : undefined,
  };
}

function hasVisibleUserContent(message: Message): boolean {
  if (message.content && message.content.trim()) {
    return true;
  }
  if (!Array.isArray(message.blocks) || message.blocks.length === 0) {
    return false;
  }
  return message.blocks.some((block) => {
    if (!block) {
      return false;
    }
    if ((block.type === 'text' || block.type === 'code') && block.content && block.content.trim()) {
      return true;
    }
    if (block.type === 'thinking') {
      return Boolean(block.thinking?.content && block.thinking.content.trim());
    }
    return block.type === 'tool_call'
      || block.type === 'tool_result'
      || block.type === 'file_change'
      || block.type === 'plan'
      || block.type === 'dispatch_group';
  });
}

function buildPlaceholderTakeoverMessage(
  baseMessage: Message,
  placeholderMessage: Message | undefined,
  requestId: string,
): Message {
  const placeholderMetadata = placeholderMessage?.metadata && typeof placeholderMessage.metadata === 'object'
    ? placeholderMessage.metadata as Record<string, unknown>
    : undefined;
  const baseMetadata = baseMessage.metadata && typeof baseMessage.metadata === 'object'
    ? baseMessage.metadata as Record<string, unknown>
    : undefined;
  const keepPlaceholderVisible = !hasVisibleUserContent(baseMessage);
  return {
    ...baseMessage,
    metadata: buildRequestPlaceholderMetadata(
      placeholderMetadata,
      baseMetadata,
      requestId,
      keepPlaceholderVisible,
    ),
  };
}


function handleUnhandledMessageError(error: unknown, message?: ClientBridgeMessage): void {
  console.error('[MessageHandler] 处理消息时发生未捕获异常:', error, message);
  // 统一降级策略：消息处理崩溃后立即收敛前端运行态，避免用户看到“持续执行但无输出”假象。
  sealAllStreamingMessages();
  settleProcessingForManualInteraction();

  const detail = error instanceof Error ? error.message.trim() : String(error || '').trim();
  const messageType = typeof message?.type === 'string' && message.type.trim() ? message.type : 'unknown';
  const signature = `${messageType}:${detail || 'unknown'}`;
  const now = Date.now();
  const withinDedupWindow = (
    signature === lastUnhandledMessageErrorSignature
    && now - lastUnhandledMessageErrorAt < UNHANDLED_MESSAGE_TOAST_WINDOW_MS
  );
  if (withinDedupWindow) {
    return;
  }
  lastUnhandledMessageErrorSignature = signature;
  lastUnhandledMessageErrorAt = now;
  addToast(
    'error',
    detail
      ? `消息同步异常，已终止当前执行态：${detail}`
      : '消息同步异常，已终止当前执行态，请重试。',
    undefined,
    {
      category: 'incident',
      source: 'message-handler',
      actionRequired: true,
      persistToCenter: true,
    },
  );
}


/**
 * 处理来自扩展的消息
 */
function handleMessage(message: ClientBridgeMessage) {
  if (!message || typeof message !== 'object') {
    return;
  }
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

      case 'rustTaskEvent':
        break;

      default:
        console.warn('[MessageHandler] 未知消息类型:', type, message);
        break;

    }
  } catch (error) {
    handleUnhandledMessageError(error, message);
  }
}



function handleUnifiedMessage(message: ClientBridgeMessage) {
  const rawStandard = message.message as StandardMessage;
  if (!rawStandard) {
    console.warn('[MessageHandler] unifiedMessage 缺少 message 字段:', message);
    return;
  }
  const standard = assertStandardMessageId(rawStandard);

  if (!shouldAcceptStandardMessageForCurrentSession(standard)) {
    return;
  }

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
      if (standard.data?.dataType === 'sessionBootstrapLoaded') {
        syncEventSeqTrackingFromBootstrap(standard);
      }
      handleUnifiedData(standard);
      break;
    default:
      console.warn('[MessageHandler] 未知消息类别:', standard.category, standard);
      break;
  }
}

function handleContentMessage(standard: StandardMessage) {
  if (shouldHideFromUser(standard)) {
    return;
  }
  const uiMessage = mapStandardMessage(standard);
  const visibility = resolveTimelineVisibilityForStandard(standard);
  const meta = standard.metadata as Record<string, unknown> | undefined;
  const requestId = meta?.requestId as string | undefined;
  const isPlaceholder = Boolean(meta?.isPlaceholder);
  const isUserMessage = standard.type === MessageType.USER_INPUT;

  // === 占位消息：只展示 pending/connecting 状态，等待后续真实消息原位接管 ===
  // 占位消息也需要创建 timeline node 锚点，确保后续真实消息能够原位接管。
  if (isPlaceholder) {
    if (!requestId) {
      throw new Error('[MessageHandler] 占位消息缺少 requestId');
    }
    const userMessageId = meta?.userMessageId as string | undefined;
    if (!userMessageId) {
      throw new Error('[MessageHandler] 占位消息缺少 userMessageId');
    }
    const binding = getRequestBinding(requestId);
    if (!binding) {
      createRequestBinding({
        requestId,
        userMessageId,
        placeholderMessageId: standard.id,
        createdAt: standard.timestamp || Date.now(),
      });
    } else {
      updateRequestBinding(requestId, { placeholderMessageId: standard.id, userMessageId });
    }
    addPendingRequest(requestId);
    if (!getTimelineMessageById(uiMessage.id)) {
      const timelineMessage = upsertTimelineNode(uiMessage, visibility);
      applyPendingTimelineUpdatesForAnchor(timelineMessage, [standard.id]);
    }
    if (uiMessage.isStreaming) {
      markMessageActive(uiMessage.id);
    }
    return;
  }

  // === 用户消息 ===
  // 用户消息也应该在 unifiedMessage 首次到达时立即创建 timeline node 锚点，
  // 与 assistant streaming 走同一条建链路径，消除双真相竞争。
  // 注意：user_input 通常不是 streaming 的（isStreaming=false），但仍然需要建节点。
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
    const existingTimelineMessage = getTimelineMessageById(uiMessage.id);
    const timelineMessage = upsertTimelineNode(uiMessage, visibility);
    if (!existingTimelineMessage) {
      applyPendingTimelineUpdatesForAnchor(timelineMessage, [standard.id]);
    }
    return;
  }

  const requestBinding = requestId ? getRequestBinding(requestId) : undefined;
  const replaceMessageId = (
    requestId
    && shouldTakeOverRequestPlaceholder(standard, requestBinding)
    && requestBinding?.placeholderMessageId
  )
    ? requestBinding.placeholderMessageId
    : undefined;
  const placeholderMessage = replaceMessageId
    ? getTimelineMessageById(replaceMessageId)
    : undefined;
  const effectiveMessageBase = (requestId && replaceMessageId)
    ? buildPlaceholderTakeoverMessage(uiMessage, placeholderMessage, requestId)
    : uiMessage;
  const effectiveMessage = effectiveMessageBase;

  if (requestId && canStandardMessageTakeOverRequestPlaceholder(standard) && requestBinding) {
    if (requestBinding.timeoutId) {
      clearTimeout(requestBinding.timeoutId);
    }
    updateRequestBinding(requestId, {
      realMessageId: standard.id,
      timeoutId: undefined,
    });
  }
  if (replaceMessageId) {
    markMessageComplete(replaceMessageId);
  }

  const hasExistingTimelineMessage = Boolean(getTimelineMessageById(effectiveMessage.id));
  if (
    effectiveMessage.isStreaming
    || hasRenderableContent(effectiveMessage)
    || Boolean(replaceMessageId)
    || hasExistingTimelineMessage
  ) {
    const timelineMessage = upsertTimelineNode(
      effectiveMessage,
      visibility,
      replaceMessageId ? { replaceMessageId } : {},
    );
    applyPendingTimelineUpdatesForAnchor(
      timelineMessage,
      [
        standard.id,
        ...(replaceMessageId ? [replaceMessageId] : []),
      ],
    );
  }

  if (effectiveMessage.isStreaming) {
    markMessageActive(effectiveMessage.id);
  }
}


function handleStandardUpdate(message: ClientBridgeMessage) {
  const rawUpdate = message.update as StreamUpdate;
  if (!rawUpdate?.messageId || !rawUpdate.messageId.trim()) {
    throw new Error('[MessageHandler] 流式更新缺少 messageId');
  }
  const update = rawUpdate;
  if (!shouldAcceptUpdateForCurrentSession(message, update)) {
    return;
  }
  const existingMessage = getTimelineMessageById(update.messageId);
  const updateAnchorId = update.messageId;

  // 处理 lifecycle 变更
  if (update.updateType === 'lifecycle' && isTerminalLifecycle(update.lifecycle)) {
    markMessageComplete(updateAnchorId);
    if (updateAnchorId !== update.messageId) {
      markMessageComplete(update.messageId);
    }
  }

  // 应用增量内容更新（append/replace/block_update/lifecycle_change）
  if (update.updateType) {
    if (!existingMessage) {
      enqueuePendingTimelineUpdate(update);
      return;
    }
    const patch = applyStreamUpdate(existingMessage, update);
    if (existingMessage.metadata?.isPlaceholder === true) {
      const requestIdFromMessage = typeof existingMessage.metadata.requestId === 'string'
        ? existingMessage.metadata.requestId
        : '';
      if (requestIdFromMessage) {
        patch.metadata = buildRequestPlaceholderMetadata(
          existingMessage.metadata as Record<string, unknown>,
          patch.metadata && typeof patch.metadata === 'object'
            ? patch.metadata as Record<string, unknown>
            : undefined,
          requestIdFromMessage,
          !hasVisibleUserContent({
            ...existingMessage,
            ...patch,
            content: patch.content ?? existingMessage.content,
            blocks: patch.blocks ?? existingMessage.blocks,
          }),
        );
      }
    }
    if (Object.keys(patch).length > 0) {
      applyTimelineStreamPatch(updateAnchorId, patch);
    }
  }
}

function handleStandardComplete(message: ClientBridgeMessage) {
  const rawStandard = message.message as StandardMessage;
  if (!rawStandard) {
    throw new Error('[MessageHandler] unifiedComplete 缺少 message');
  }
  const standard = assertStandardMessageId(rawStandard);
  if (!shouldAcceptStandardMessageForCurrentSession(standard)) {
    return;
  }

  if (standard.category !== MessageCategory.CONTENT || shouldHideFromUser(standard)) {
    return;
  }

  const requestId = (standard.metadata as Record<string, unknown> | undefined)?.requestId as string | undefined;
  const actualMessageId = standard.id;
  const uiMessage = mapStandardMessage(standard);
  const visibility = resolveTimelineVisibilityForStandard(standard);
  const requestBinding = requestId ? getRequestBinding(requestId) : undefined;
  const computedResponseDurationMs = requestBinding?.createdAt
    ? Math.max(0, (standard.timestamp || Date.now()) - requestBinding.createdAt)
    : undefined;
  const uiMetadata = uiMessage.metadata && typeof uiMessage.metadata === 'object'
    ? uiMessage.metadata
    : {};
  const authoritativeResponseDurationMs = typeof uiMetadata.responseDurationMs === 'number'
    ? uiMetadata.responseDurationMs
    : undefined;
  const responseDurationMs = typeof authoritativeResponseDurationMs === 'number'
    ? authoritativeResponseDurationMs
    : computedResponseDurationMs;
  const replaceMessageId = (
    requestId
    && shouldTakeOverRequestPlaceholder(standard, requestBinding)
    && requestBinding?.placeholderMessageId
  )
    ? requestBinding.placeholderMessageId
    : undefined;
  const placeholderMessage = replaceMessageId
    ? getTimelineMessageById(replaceMessageId)
    : undefined;
  const existingMessage = getTimelineMessageById(actualMessageId) || placeholderMessage;
  const mergedBlocks = mergeCompleteBlocksForFinalization(
    existingMessage?.blocks,
    uiMessage.blocks,
    uiMessage.blocks,
  );
  const completedMessageBase: Message = {
    ...uiMessage,
    isStreaming: false,
    isComplete: true,
    metadata: {
      ...uiMetadata,
      ...(typeof responseDurationMs === 'number' ? { responseDurationMs } : {}),
    },
    ...(mergedBlocks ? {
      blocks: mergedBlocks,
      content: blocksToContent(mergedBlocks),
    } : {}),
  };
  const completedMessageBaseWithPlaceholder = (requestId && replaceMessageId)
    ? buildPlaceholderTakeoverMessage(completedMessageBase, placeholderMessage, requestId)
    : completedMessageBase;
  const completedMessage = completedMessageBaseWithPlaceholder;

  if (replaceMessageId) {
    markMessageComplete(replaceMessageId);
  }
  if (
    hasRenderableContent(completedMessage)
    || getTimelineMessageById(actualMessageId)
    || placeholderMessage
  ) {
    upsertTimelineNode(
      completedMessage,
      visibility,
      replaceMessageId ? { replaceMessageId } : {},
    );
  }

  markMessageComplete(actualMessageId);

  // 清理请求绑定
  if (requestId) {
    clearPendingRequest(requestId);
    const binding = getRequestBinding(requestId);
    if (binding?.timeoutId) {
      clearTimeout(binding.timeoutId);
    }
    if (binding) {
      updateRequestBinding(requestId, {
        realMessageId: actualMessageId,
        timeoutId: undefined,
      });
      clearRequestBinding(requestId);
    }
  }
  settleProcessingAfterResponseCompletion();
}


/**
 * 处理控制消息
 *
 * 控制消息通过 MessageHub.sendControl() 发送，包含 controlType 和 payload
 */
function isTerminalLifecycle(lifecycle: StandardMessage['lifecycle'] | undefined): boolean {
  return lifecycle === MessageLifecycle.COMPLETED
    || lifecycle === MessageLifecycle.FAILED
    || lifecycle === MessageLifecycle.CANCELLED;
}

function isStreamingLifecycle(lifecycle: StandardMessage['lifecycle'] | undefined): boolean {
  return lifecycle === MessageLifecycle.STREAMING || lifecycle === MessageLifecycle.STARTED;
}

function mapStandardMessage(standard: StandardMessage): Message {
  const blocks = mapStandardBlocks(standard.blocks || []);
  const content = blocksToContent(blocks);
  const isStreaming = isStreamingLifecycle(standard.lifecycle);
  const isComplete = isTerminalLifecycle(standard.lifecycle);
  const isSystemNotice = standard.type === MessageType.SYSTEM;

  // 区分消息来源与展示来源：
  // 标准消息 source 为 orchestrator/worker，UI 展示具体 Worker 槽位
  const originSource = standard.source;
  const resolvedWorker = resolveLaneWorkerSlot(standard);
  if (requiresWorkerLane(standard) && !resolvedWorker) {
    console.warn(`[MessageHandler] TASK_CARD 消息无法解析 worker 槽位，回退至 source: ${standard.id}`);
  }
  const displaySource: Message['source'] =
    originSource === 'orchestrator'
      ? 'orchestrator'
      : (resolvedWorker as Message['source']);

  const baseMetadata = { ...(standard.metadata || {}) } as Record<string, unknown>;

  // 根据消息类型正确映射 role：用户输入消息 → 'user'，系统通知 → 'system'，其余 → 'assistant'
  const resolvedRole: 'user' | 'assistant' | 'system' =
    isSystemNotice ? 'system'
    : (standard.type === MessageType.USER_INPUT ? 'user' : 'assistant');

  // 直接传递 MessageType，UI 层使用 type === 'user_input' 判断用户消息
  const resolvedType = standard.type as import('../types/message').MessageType;

  return {
    id: standard.id,
    role: resolvedRole,
    source: displaySource,
    content,
    blocks,
    timestamp: standard.timestamp || Date.now(),
    updatedAt: standard.updatedAt || standard.timestamp || Date.now(),
    isStreaming,
    isComplete,
    type: resolvedType,
    noticeType: standard.type === MessageType.ERROR ? 'error' : (isSystemNotice ? 'info' : undefined),
    metadata: {
      ...baseMetadata,
      eventId: standard.eventId,
      eventSeq: standard.eventSeq,
      interaction: standard.interaction,
      worker: resolvedWorker ?? undefined,
    },
  };


}

function hasRenderableContent(message: Message): boolean {
  return messageHasRenderableTimelineContent(message);
}



function applyStreamUpdate(message: Message, update: StreamUpdate): Partial<Message> {
  const updates: Partial<Message> = {};
  switch (update.updateType) {
    case 'append_text': {
      updates.content = (message.content || '') + update.text;
      const nextBlocks = [...(message.blocks || [])];
      const lastIndex = nextBlocks.length - 1;
      const lastBlock = nextBlocks[nextBlocks.length - 1];
      if (lastBlock?.type === 'text') {
        const current = lastBlock;
        nextBlocks[lastIndex] = {
          ...current,
          content: (current.content || '') + update.text,
        };
      } else {
        nextBlocks.push({ type: 'text', content: update.text });
      }
      updates.blocks = nextBlocks;
      break;
    }
    case 'replace_blocks':
      if (update.blocks) {
        const blocks = mapStandardBlocks(update.blocks);
        updates.blocks = blocks;
        updates.content = blocksToContent(blocks);
      }
      break;
    case 'merge_block':
      if (update.blocks) {
        const incoming = mapStandardBlocks(update.blocks);
        const merged = mergeBlocks(message.blocks || [], incoming);
        updates.blocks = merged;
        updates.content = blocksToContent(merged);
      }
      break;
    case 'lifecycle':
      updates.isStreaming = isStreamingLifecycle(update.lifecycle);
      updates.isComplete = isTerminalLifecycle(update.lifecycle);
      break;
    case 'block_insert':
      updates.blocks = [...(message.blocks || []), update.block as unknown as ContentBlock];
      break;
    case 'block_patch':
      updates.blocks = (message.blocks || []).map(b =>
        ('blockId' in b && b.blockId === update.blockId) ? { ...b, ...update.patch } as ContentBlock : b
      );
      break;
    case 'dispatch_lane_patch':
      updates.blocks = upsertDispatchGroupLane(message.blocks, update) as ContentBlock[];
      break;
    default: {
      const _exhaustive: never = update;
      void _exhaustive;
    }
  }

  if (typeof update.timestamp === 'number' && Number.isFinite(update.timestamp)) {
    updates.updatedAt = Math.floor(update.timestamp);
  }

  if (update.eventId || typeof update.eventSeq === 'number') {
    updates.metadata = {
      ...(message.metadata || {}),
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
    if ((block.type === 'tool_call' || block.type === 'tool_result') && block.toolCall?.id) {
      const idx = next.findIndex((b) => b.type === block.type && b.toolCall?.id === block.toolCall?.id);
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
            durationMs: incomingToolCall?.durationMs ?? prevToolCall?.durationMs,
            startTime: incomingToolCall?.startTime ?? prevToolCall?.startTime,
            endTime: incomingToolCall?.endTime ?? prevToolCall?.endTime,
          },
        };
      } else {
        next.push(block);
      }
      continue;
    }
    if (block.type === 'thinking') {
      const incomingBlockId = block.id || block.thinking?.blockId;
      // 仅当双方都有 blockId 且匹配时才合并（更新同一个 thinking 块）
      // 无 blockId 一律视为新块，避免不同 thinking 被错误合并
      const idx = incomingBlockId
        ? next.findIndex((b) => {
            if (b.type !== 'thinking') return false;
            const existingBlockId = b.id || b.thinking?.blockId;
            return existingBlockId === incomingBlockId;
          })
        : -1;
      if (idx >= 0) {
        const prev = next[idx];
        const prevThinking = prev.thinking || { content: '', isComplete: false };
        const blockThinking = block.thinking || { content: '', isComplete: false };
        const mergedThinking = {
          content: blockThinking.content || prevThinking.content || block.content || prev.content || '',
          isComplete: blockThinking.isComplete ?? prevThinking.isComplete ?? true,
          summary: blockThinking.summary ?? prevThinking.summary,
          blockId: incomingBlockId || prevThinking.blockId,
        };
        next[idx] = {
          ...prev,
          ...block,
          ...(incomingBlockId ? { id: incomingBlockId } : {}),
          thinking: mergedThinking,
        };
      } else {
        next.push(block);
      }
      continue;
    }
    if (block.type === 'text') {
      const lastIndex = next.length - 1;
      const prev = next[next.length - 1];
      if (prev?.type === 'text') {
        next[lastIndex] = { ...prev, content: (prev.content || '') + (block.content || '') };
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
    if (block.type === 'tool_result') {
      const toolContent = block.toolCall?.error || block.toolCall?.result || block.content;
      if (toolContent) textParts.push(toolContent);
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
