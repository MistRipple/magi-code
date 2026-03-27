/**
 * 消息处理器 - 处理来自宿主桥接层的消息
 *
 * 仅负责 CONTENT 消息的接收、路由、流式更新和完成处理。
 * DATA / CONTROL / NOTIFY 消息处理已拆分到 data-message-handlers.ts。
 * 共享工具函数位于 message-utils.ts。
 */

import type { ClientBridge, ClientBridgeMessage } from '../../../shared/bridges/client-bridge';
import { getClientBridge } from '../../../shared/bridges/bridge-runtime';
import {
  getState,
  markMessageActive,
  markMessageComplete,
  addPendingRequest,
  clearPendingRequest,
  addToast,
  getRequestBinding,
  createRequestBinding,
  updateRequestBinding,
  clearRequestBinding,
  settleProcessingForManualInteraction,
  sealAllStreamingMessages,
  applyTimelineStreamPatch,
  getTimelineMessageById,
  getTimelineMessageByCardId,
  upsertTimelineNode,
} from '../stores/messages.svelte';
import type { Message, ContentBlock, SessionTimelineProjection } from '../types/message';
import type { StandardMessage, StreamUpdate } from '../../../../protocol/message-protocol';
import { MessageType, MessageCategory } from '../../../../protocol/message-protocol';
import { normalizeWorkerSlot } from './message-classifier';
import { resolveTaskCardKeyFromMetadata } from './task-card-runtime';
import { ensureArray } from './utils';
import { i18n } from '../stores/i18n.svelte';
import { canBindRequestPlaceholder } from '../../../../shared/request-placeholder-binding';
import {
  messageHasRenderableTimelineContent,
  resolveTimelineWorkerVisibility as resolveSharedTimelineWorkerVisibility,
} from '../../../../shared/timeline-presentation';
import {
  handleUnifiedControlMessage,
  handleUnifiedNotify,
  handleUnifiedData,
} from './data-message-handlers';
import {
  type WorkerSlot,
  mapStandardBlocks,
  formatPlanBlock,
  syncWorkerWaitResultsFromMessage,
} from './message-utils';
import { mergeCompleteBlocksForFinalization } from './streaming-complete-merge';

// Re-export for external consumers
export {
  normalizeRestoredMessages,
  handleRetryRuntimePayload,
  rebuildWorkerWaitResultsFromMessages,
  mapStandardBlocks,
} from './message-utils';


function assertStandardMessageId(standard: StandardMessage): StandardMessage {
  if (standard.id && standard.id.trim()) {
    return standard;
  }
  throw new Error('[MessageHandler] 标准消息缺少 id');
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

function resolveLaneWorkerSlot(standard: StandardMessage): WorkerSlot | null {
  const metadata = standard.metadata as Record<string, unknown> | undefined;
  if (standard.type === MessageType.USER_INPUT) {
    return normalizeWorkerSlot(metadata?.worker);
  }
  if (standard.type === MessageType.TASK_CARD) {
    return normalizeWorkerSlot(metadata?.assignedWorker);
  }
  if (standard.type === MessageType.INSTRUCTION) {
    return normalizeWorkerSlot(standard.agent);
  }
  if (standard.source === 'worker') {
    return normalizeWorkerSlot(standard.agent);
  }
  return null;
}

function requiresWorkerLane(standard: StandardMessage): boolean {
  return standard.source === 'worker'
    || standard.type === MessageType.INSTRUCTION
    || standard.type === MessageType.TASK_CARD;
}

function resolveTimelineVisibilityForStandard(standard: StandardMessage): {
  thread: boolean;
  workerTabs?: WorkerSlot[];
} {
  const workerSlot = resolveLaneWorkerSlot(standard);
  const visibility = resolveSharedTimelineWorkerVisibility({
    hasWorker: Boolean(workerSlot),
    type: standard.type,
    source: standard.source,
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

function resetEventSeqTracking(seed: number = 0, sessionId?: string): void {
  lastAppliedEventSeq = seed > 0 ? Math.floor(seed) : 0;
  eventSeqSessionId = typeof sessionId === 'string' ? sessionId.trim() : '';
  processedEventKeys.clear();
  processedEventKeyQueue.length = 0;
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

function resolveEventSeqAndKey(message: ClientBridgeMessage): { eventSeq?: number; eventKey?: string } {
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
    console.warn('[MessageHandler] 忽略重复事件', {
      eventSeq,
      eventKey,
      type: message.type,
    });
    return false;
  }
  // SSE 是单连接串行传输，正常情况下不会出现逆序。
  // 如果出现，说明发送侧存在重复注入（如重复 bootstrap），直接丢弃。
  if (eventSeq <= lastAppliedEventSeq) {
    console.warn('[MessageHandler] 丢弃逆序/重复事件', {
      eventSeq,
      lastAppliedEventSeq,
      type: message.type,
    });
    return false;
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

function canStandardMessageTakeOverRequestPlaceholder(standard: StandardMessage): boolean {
  return canBindRequestPlaceholder({
    type: standard.type,
    source: standard.source,
    visibility: standard.visibility,
    metadata: standard.metadata as Record<string, unknown> | undefined,
  });
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
          : 'thinking')
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
    return block.type === 'tool_call' || block.type === 'file_change' || block.type === 'plan';
  });
}

function resolveBoundUserAnchorTimestamp(userMessageId: string | undefined): number | null {
  if (!userMessageId) {
    return null;
  }
  const userMessage = getTimelineMessageById(userMessageId);
  if (!userMessage) {
    return null;
  }
  const metadata = userMessage.metadata && typeof userMessage.metadata === 'object'
    ? userMessage.metadata as Record<string, unknown>
    : undefined;
  const metadataAnchor = typeof metadata?.timelineAnchorTimestamp === 'number'
    && Number.isFinite(metadata.timelineAnchorTimestamp)
    && metadata.timelineAnchorTimestamp > 0
    ? Math.floor(metadata.timelineAnchorTimestamp)
    : 0;
  if (metadataAnchor > 0) {
    return metadataAnchor;
  }
  return typeof userMessage.timestamp === 'number' && Number.isFinite(userMessage.timestamp) && userMessage.timestamp > 0
    ? Math.floor(userMessage.timestamp)
    : null;
}

function bindMessageToUserAnchor(
  message: Message,
  userMessageId: string | undefined,
): Message {
  const anchorTimestamp = resolveBoundUserAnchorTimestamp(userMessageId);
  if (!anchorTimestamp) {
    return message;
  }
  const metadata = message.metadata && typeof message.metadata === 'object'
    ? message.metadata as Record<string, unknown>
    : undefined;
  if (metadata?.timelineAnchorTimestamp === anchorTimestamp) {
    return message;
  }
  return {
    ...message,
    metadata: {
      ...(metadata || {}),
      timelineAnchorTimestamp: anchorTimestamp,
    },
  };
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

const SESSION_LIFECYCLE_DATA_TYPES = new Set<string>([
  'sessionBootstrapLoaded',
]);

function shouldBypassCrossSessionFilter(message: ClientBridgeMessage): boolean {
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

function shouldIgnoreCrossSessionUnifiedMessage(message: ClientBridgeMessage): boolean {
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

  const incomingSessionId = resolveEventTrackingSessionId(message);
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

function handleUnhandledMessageError(error: unknown, message: ClientBridgeMessage): void {
  console.error('[MessageHandler] 处理消息时发生未捕获异常:', error, message);
  // 统一降级策略：消息处理崩溃后立即收敛前端运行态，避免用户看到“持续执行但无输出”假象。
  sealAllStreamingMessages();
  settleProcessingForManualInteraction();

  const detail = error instanceof Error ? error.message.trim() : String(error || '').trim();
  const signature = `${message.type}:${detail || 'unknown'}`;
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

  // === 占位消息：先展示 thinking 卡片，等后续真实消息接管同一时间轴位置 ===
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
    const anchoredPlaceholder = bindMessageToUserAnchor(uiMessage, userMessageId);
    if (!getTimelineMessageById(anchoredPlaceholder.id)) {
      upsertTimelineNode(anchoredPlaceholder, visibility);
    }
    if (anchoredPlaceholder.isStreaming) {
      markMessageActive(anchoredPlaceholder.id);
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
    if (!getTimelineMessageById(uiMessage.id)) {
      upsertTimelineNode(uiMessage, visibility);
    }
    return;
  }

  const requestBinding = requestId ? getRequestBinding(requestId) : undefined;
  const replaceMessageId = (
    requestId
    && canStandardMessageTakeOverRequestPlaceholder(standard)
    && requestBinding?.placeholderMessageId
    && requestBinding.placeholderMessageId !== standard.id
  )
    ? requestBinding.placeholderMessageId
    : undefined;
  const placeholderMessage = replaceMessageId
    ? getTimelineMessageById(replaceMessageId)
    : undefined;
  const effectiveMessageBase = (requestId && replaceMessageId)
    ? buildPlaceholderTakeoverMessage(uiMessage, placeholderMessage, requestId)
    : uiMessage;
  const boundUserMessageId = requestBinding?.userMessageId
    || (typeof meta?.userMessageId === 'string' ? meta.userMessageId : undefined);
  const effectiveMessage = (requestId && visibility.thread)
    ? bindMessageToUserAnchor(effectiveMessageBase, boundUserMessageId)
    : effectiveMessageBase;

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
    syncWorkerWaitResultsFromMessage(timelineMessage);
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
  const normalizedCardId = typeof update.cardId === 'string' ? update.cardId.trim() : '';
  const anchorByMessageId = getTimelineMessageById(update.messageId);
  const anchorByCardId = !anchorByMessageId && normalizedCardId
    ? getTimelineMessageByCardId(normalizedCardId)
    : undefined;
  const existingMessage = anchorByMessageId || anchorByCardId;
  const updateAnchorId = anchorByMessageId
    ? update.messageId
    : (anchorByCardId?.id || update.messageId);

  // 处理 lifecycle 变更
  if (typeof update.lifecycle === 'string' && update.lifecycle === 'completed') {
    markMessageComplete(updateAnchorId);
    if (updateAnchorId !== update.messageId) {
      markMessageComplete(update.messageId);
    }
  }

  // 应用增量内容更新（append/replace/block_update/lifecycle_change）
  if (update.updateType) {
    if (!existingMessage) {
      console.error('[MessageHandler] 流式更新缺少时间轴锚点，拒绝静默丢弃:', {
        messageId: update.messageId,
        cardId: normalizedCardId || undefined,
        updateType: update.updateType,
        eventSeq: update.eventSeq,
      });
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

  if (standard.category !== MessageCategory.CONTENT || shouldHideFromUser(standard)) {
    if (standard.category !== MessageCategory.CONTENT) {
      console.debug('[MessageHandler] 跳过非时间轴消息的 complete 消息:', standard.category, standard.id);
    }
    return;
  }

  const requestId = (standard.metadata as Record<string, unknown> | undefined)?.requestId as string | undefined;
  const actualMessageId = standard.id;
  const uiMessage = mapStandardMessage(standard);
  const visibility = resolveTimelineVisibilityForStandard(standard);
  const requestBinding = requestId ? getRequestBinding(requestId) : undefined;
  const replaceMessageId = (
    requestId
    && canStandardMessageTakeOverRequestPlaceholder(standard)
    && requestBinding?.placeholderMessageId
    && requestBinding.placeholderMessageId !== standard.id
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
    ...(mergedBlocks ? {
      blocks: mergedBlocks,
      content: blocksToContent(mergedBlocks),
    } : {}),
  };
  const completedMessageBaseWithPlaceholder = (requestId && replaceMessageId)
    ? buildPlaceholderTakeoverMessage(completedMessageBase, placeholderMessage, requestId)
    : completedMessageBase;
  const boundUserMessageId = requestBinding?.userMessageId
    || (typeof (standard.metadata as Record<string, unknown> | undefined)?.userMessageId === 'string'
      ? (standard.metadata as Record<string, unknown>).userMessageId as string
      : undefined);
  const completedMessage = (requestId && visibility.thread)
    ? bindMessageToUserAnchor(completedMessageBaseWithPlaceholder, boundUserMessageId)
    : completedMessageBaseWithPlaceholder;

  if (replaceMessageId) {
    markMessageComplete(replaceMessageId);
  }
  if (
    hasRenderableContent(completedMessage)
    || getTimelineMessageById(actualMessageId)
    || placeholderMessage
  ) {
    const timelineMessage = upsertTimelineNode(
      completedMessage,
      visibility,
      replaceMessageId ? { replaceMessageId } : {},
    );
    syncWorkerWaitResultsFromMessage(timelineMessage);
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
    }
    setTimeout(() => {
      clearRequestBinding(requestId);
    }, 1000);
  }
}


/**
 * 处理控制消息
 *
 * 控制消息通过 MessageHub.sendControl() 发送，包含 controlType 和 payload
 */
function mapStandardMessage(standard: StandardMessage): Message {
  const blocks = mapStandardBlocks(standard.blocks || []);
  const content = blocksToContent(blocks);
  const isStreaming = standard.lifecycle === 'streaming' || standard.lifecycle === 'started';
  const isComplete = standard.lifecycle === 'completed';
  const isSystemNotice = standard.type === MessageType.SYSTEM;

  // 区分消息来源与展示来源：
  // 标准消息 source 为 orchestrator/worker，UI 展示具体 Worker 槽位
  const originSource = standard.source;
  const resolvedWorker = resolveLaneWorkerSlot(standard);
  if (requiresWorkerLane(standard) && !resolvedWorker) {
    throw new Error(`[MessageHandler] 消息缺少明确 worker 槽位: ${standard.id}`);
  }
  const displaySource: Message['source'] =
    originSource === 'orchestrator'
      ? 'orchestrator'
      : (resolvedWorker as Message['source']);

  const baseMetadata = { ...(standard.metadata || {}) } as Record<string, unknown>;
  const rawCardId = typeof baseMetadata.cardId === 'string' ? baseMetadata.cardId.trim() : '';
  const rawWorkerCardId = typeof baseMetadata.workerCardId === 'string' ? baseMetadata.workerCardId.trim() : '';
  const resolvedAssignmentCardId = resolveTaskCardKeyFromMetadata(baseMetadata);
  const shouldUseAssignmentCard = standard.type === MessageType.INSTRUCTION || standard.type === MessageType.TASK_CARD;

  // Worker 生命周期卡片必须优先使用 workerCardId 固定实体身份。
  // lifecycleKey 决定“属于哪条 wave/lane 执行链”，workerCardId 决定“是哪一张卡片实体”。
  const isWorkerLifecycleCard = shouldUseAssignmentCard && rawWorkerCardId.length > 0;
  const cardId = isWorkerLifecycleCard
    ? rawWorkerCardId
    : (shouldUseAssignmentCard
        ? (rawCardId || resolvedAssignmentCardId || standard.id)
        : (rawCardId || standard.id));
  const uiMessageId = isWorkerLifecycleCard ? rawWorkerCardId : standard.id;

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
    updatedAt: standard.updatedAt || standard.timestamp || Date.now(),
    isStreaming,
    isComplete,
    type: resolvedType,
    noticeType: isSystemNotice ? 'info' : undefined,
    metadata: {
      ...baseMetadata,
      eventId: standard.eventId,
      eventSeq: standard.eventSeq,
      cardId,
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
  if (update.updateType === 'append' && update.appendText) {
    updates.content = (message.content || '') + update.appendText;
    const nextBlocks = [...(message.blocks || [])];
    const lastIndex = nextBlocks.length - 1;
    const lastBlock = nextBlocks[nextBlocks.length - 1];
    if (lastBlock?.type === 'text') {
      const current = lastBlock;
      nextBlocks[lastIndex] = {
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

  if (typeof update.timestamp === 'number' && Number.isFinite(update.timestamp)) {
    updates.updatedAt = Math.floor(update.timestamp);
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
    if (block.type === 'file_change' && block.fileChange) {
      textParts.push(i18n.t('messageHandler.fileChange', { filePath: block.fileChange.filePath, changeType: block.fileChange.changeType }));
    }
    if (block.type === 'plan' && block.plan) {
      textParts.push(formatPlanBlock(block.plan));
    }
  }
  return textParts.join('\n\n');
}
