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
  getRequestBinding,
  createRequestBinding,
  updateRequestBinding,
  clearRequestBinding,
  applyTimelineStreamPatch,
  getTimelineMessageById,
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
} from './message-utils';

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
const processedEventKeys = new Set<string>();
const processedEventKeyQueue: string[] = [];
const MAX_TRACKED_EVENT_KEYS = 4000;

function resetEventSeqTracking(seed: number = 0): void {
  lastAppliedEventSeq = seed > 0 ? Math.floor(seed) : 0;
  processedEventKeys.clear();
  processedEventKeyQueue.length = 0;
}

function syncEventSeqTrackingFromProjection(message: ClientBridgeMessage): void {
  const projection = message.timelineProjection as SessionTimelineProjection | undefined;
  const seed = typeof projection?.lastAppliedEventSeq === 'number' && Number.isFinite(projection.lastAppliedEventSeq)
    ? Math.max(0, Math.floor(projection.lastAppliedEventSeq))
    : 0;
  // 只能向前推进：如果 projection 里的 seed 落后于当前 live 追踪，
  // 不回退 lastAppliedEventSeq，避免重复 bootstrap 把基线拉回到过去。
  if (seed > lastAppliedEventSeq) {
    lastAppliedEventSeq = seed;
  }
  // 不清除 processedEventKeys：同 session 的重复 bootstrap 不应丢弃去重记忆
}

function syncEventSeqTrackingFromBootstrap(standard: StandardMessage): void {
  const payload = standard.data?.payload as Record<string, unknown> | undefined;
  if (!payload) {
    return;
  }
  const incomingSessionId = typeof payload.sessionId === 'string' ? payload.sessionId.trim() : '';
  const currentSessionId = getState().currentSessionId?.trim() || '';
  if (incomingSessionId && currentSessionId && incomingSessionId !== currentSessionId) {
    resetEventSeqTracking();
  }
  syncEventSeqTrackingFromProjection({
    type: 'sessionBootstrapLoaded',
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
function handleMessage(message: ClientBridgeMessage) {
  const { type } = message;

  if (type === 'unifiedUpdate') {
    console.log('[STREAM_DEBUG] handleMessage 收到 unifiedUpdate');
  }

  if (shouldIgnoreCrossSessionUnifiedMessage(message)) {
    if (type === 'unifiedUpdate') {
      console.warn('[STREAM_DEBUG] unifiedUpdate 被跨会话过滤器丢弃');
    }
    return;
  }

  if (!shouldProcessByEventSeq(message)) {
    if (type === 'unifiedUpdate') {
      console.warn('[STREAM_DEBUG] unifiedUpdate 被 eventSeq 过滤器丢弃');
    }
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
  const meta = standard.metadata as Record<string, unknown> | undefined;
  const requestId = meta?.requestId as string | undefined;
  const isPlaceholder = Boolean(meta?.isPlaceholder);
  const isUserMessage = standard.type === MessageType.USER_INPUT;

  // === 占位消息：先展示 thinking 卡片，等后续真实消息替换 ===
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
    if (uiMessage.isStreaming) {
      markMessageActive(uiMessage.id);
    }
    return;
  }

  // === 用户消息 ===
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
    return;
  }

  // === 检查是否有对应的占位消息需要替换 ===
  if (requestId && canStandardMessageTakeOverRequestPlaceholder(standard)) {
    const binding = getRequestBinding(requestId);
    if (binding && !binding.realMessageId) {
      if (binding.timeoutId) {
        clearTimeout(binding.timeoutId);
      }
      updateRequestBinding(requestId, {
        realMessageId: standard.id,
        placeholderMessageId: standard.id,
        timeoutId: undefined,
      });
    }
  }

  if (uiMessage.isStreaming) {
    markMessageActive(uiMessage.id);
  }
}


// 缓冲队列：当 unifiedUpdate 到达时如果 timeline 中尚无对应消息节点
// （例如 projection broadcast 还在 200ms debounce 中），先缓存 update，
// 等 projection 创建节点后自动回放。
const pendingStreamUpdates = new Map<string, StreamUpdate[]>();
const PENDING_UPDATE_MAX_AGE_MS = 10_000;
const PENDING_UPDATE_MAX_PER_MESSAGE = 500;

function bufferPendingStreamUpdate(messageId: string, update: StreamUpdate): void {
  let queue = pendingStreamUpdates.get(messageId);
  if (!queue) {
    queue = [];
    pendingStreamUpdates.set(messageId, queue);
  }
  if (queue.length < PENDING_UPDATE_MAX_PER_MESSAGE) {
    queue.push(update);
  }
}

/**
 * 回放缓冲的 stream updates。
 * 在 timelineProjectionUpdated / sessionBootstrapLoaded 完成后调用，
 * 此时 timeline 中已有对应的消息节点，可以安全地应用增量补丁。
 */
export function flushPendingStreamUpdates(): void {
  if (pendingStreamUpdates.size === 0) return;
  console.log('[STREAM_DEBUG] flushPendingStreamUpdates', {
    pendingCount: pendingStreamUpdates.size,
    messageIds: Array.from(pendingStreamUpdates.keys()),
  });
  const now = Date.now();
  const keysToDelete: string[] = [];
  for (const [messageId, queue] of pendingStreamUpdates) {
    // 过期清理
    if (queue.length > 0 && queue[0].timestamp && now - queue[0].timestamp > PENDING_UPDATE_MAX_AGE_MS) {
      console.log('[STREAM_DEBUG] 过期清理', { messageId, age: now - queue[0].timestamp! });
      keysToDelete.push(messageId);
      continue;
    }
    const existingMessage = getTimelineMessageById(messageId);
    if (!existingMessage) {
      console.log('[STREAM_DEBUG] flush 时消息仍未找到', { messageId });
      continue;
    }
    console.log('[STREAM_DEBUG] flush 回放', { messageId, queueLength: queue.length });
    // 逐条回放
    let currentMessage = existingMessage;
    for (const update of queue) {
      const patch = applyStreamUpdate(currentMessage, update);
      if (Object.keys(patch).length > 0) {
        applyTimelineStreamPatch(messageId, patch);
        currentMessage = { ...currentMessage, ...patch };
      }
    }
    keysToDelete.push(messageId);
  }
  for (const key of keysToDelete) {
    pendingStreamUpdates.delete(key);
  }
}

function handleStandardUpdate(message: ClientBridgeMessage) {
  const rawUpdate = message.update as StreamUpdate;
  if (!rawUpdate?.messageId || !rawUpdate.messageId.trim()) {
    throw new Error('[MessageHandler] 流式更新缺少 messageId');
  }
  const update = rawUpdate;

  console.log('[STREAM_DEBUG] handleStandardUpdate', {
    messageId: update.messageId,
    updateType: update.updateType,
    appendText: update.appendText?.substring(0, 40),
    lifecycle: update.lifecycle,
  });

  // 处理 lifecycle 变更
  if (typeof update.lifecycle === 'string' && update.lifecycle === 'completed') {
    markMessageComplete(update.messageId);
  }

  // 应用增量内容更新（append/replace/block_update/lifecycle_change）
  if (update.updateType) {
    const existingMessage = getTimelineMessageById(update.messageId);
    if (existingMessage) {
      console.log('[STREAM_DEBUG] 找到消息，应用 patch', {
        messageId: update.messageId,
        existingContent: existingMessage.content?.substring(0, 40),
        isStreaming: existingMessage.isStreaming,
      });
      const patch = applyStreamUpdate(existingMessage, update);
      if (Object.keys(patch).length > 0) {
        applyTimelineStreamPatch(update.messageId, patch);
        console.log('[STREAM_DEBUG] patch 已应用', {
          patchKeys: Object.keys(patch),
          newContent: (patch.content as string)?.substring(0, 60),
        });
      } else {
        console.warn('[STREAM_DEBUG] patch 为空！');
      }
    } else {
      // 时间线中尚无此消息（projection 还未到达），缓存等待回放
      console.log('[STREAM_DEBUG] 消息未找到，缓存 update', { messageId: update.messageId });
      bufferPendingStreamUpdate(update.messageId, update);
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

  markMessageComplete(actualMessageId);
  // 消息已完成，清理可能残留的缓冲增量更新
  pendingStreamUpdates.delete(actualMessageId);

  // 清理请求绑定
  if (requestId) {
    clearPendingRequest(requestId);
    const binding = getRequestBinding(requestId);
    if (binding?.timeoutId) {
      clearTimeout(binding.timeoutId);
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



function mergeCompleteBlocks(
  existingBlocks: ContentBlock[] | undefined,
  completeBlocks: ContentBlock[] | undefined,
  baseBlocks: ContentBlock[] | undefined,
): ContentBlock[] | undefined {
  const safeExisting = ensureArray(existingBlocks).filter((block): block is ContentBlock => !!block && typeof block === 'object' && 'type' in block);
  const safeComplete = ensureArray(completeBlocks).filter((block): block is ContentBlock => !!block && typeof block === 'object' && 'type' in block);

  if (safeExisting.length > 0 && safeComplete.length > 0) {
    // 流式已有内容时，complete 仅补充 existing 中不存在的结构化块（tool_call/thinking）。
    // 不调用 mergeBlocks 避免 text 追加导致内容翻倍。
    const existingToolIds = new Set(
      safeExisting
        .filter(b => b.type === 'tool_call' && b.toolCall?.id)
        .map(b => b.toolCall!.id),
    );
    const existingThinkingIds = new Set(
      safeExisting
        .filter(b => b.type === 'thinking' && (b.id || b.thinking?.blockId))
        .map(b => b.id || b.thinking!.blockId),
    );

    const supplements: ContentBlock[] = [];
    for (const block of safeComplete) {
      if (block.type === 'tool_call' && block.toolCall?.id) {
        if (!existingToolIds.has(block.toolCall.id)) {
          supplements.push(block);
        }
      } else if (block.type === 'thinking') {
        const blockId = block.id || block.thinking?.blockId;
        if (blockId && !existingThinkingIds.has(blockId)) {
          supplements.push(block);
        }
      }
      // text/code 等块不补充，流式已累积完整
    }

    return supplements.length > 0 ? [...safeExisting, ...supplements] : safeExisting;
  }
  if (safeExisting.length > 0) {
    return safeExisting;
  }
  if (safeComplete.length > 0) {
    return safeComplete;
  }

  const safeBase = ensureArray(baseBlocks).filter((block): block is ContentBlock => !!block && typeof block === 'object' && 'type' in block);
  return safeBase.length > 0 ? safeBase : undefined;
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
