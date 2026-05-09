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
  clearPendingRequest,
  settleProcessingAfterResponseCompletion,
  addToast,
  getRequestBinding,
  createRequestBinding,
  updateRequestBinding,
  clearRequestBinding,
  addPendingRequest,
  settleProcessingForManualInteraction,
  sealAllStreamingMessages,
  messagesState,
} from '../stores/messages.svelte';
import type { StandardMessage, StreamUpdate } from '../shared/protocol/message-protocol';
import { MessageType, MessageCategory, MessageLifecycle } from '../shared/protocol/message-protocol';
import {
  handleUnifiedControlMessage,
  handleUnifiedNotify,
  handleUnifiedData,
} from './data-message-handlers';
import { resolveStandardMessageSessionBinding } from '../shared/standard-message-session-binding';
import {
  resolveTimelineCanonicalTurnSeqFromMetadata,
  resolveTimelineTurnOrderSeqFromMetadata,
} from '../shared/timeline-ordering';

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
    if (!binding.sessionId) {
      return true;
    }
    const metadata = standard.metadata && typeof standard.metadata === 'object'
      ? standard.metadata as Record<string, unknown>
      : undefined;
    const requestId = typeof metadata?.requestId === 'string' ? metadata.requestId.trim() : '';
    return Boolean(requestId && getRequestBinding(requestId));
  }
  if (!binding.sessionId) {
    return true;
  }
  return binding.sessionId === currentSessionId;
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

function isLocalUnifiedTimelineContent(standard: StandardMessage): boolean {
  const metadata = standard.metadata && typeof standard.metadata === 'object'
    ? standard.metadata as Record<string, unknown>
    : {};
  if (standard.type === MessageType.USER_INPUT) {
    return true;
  }
  return metadata.isPlaceholder === true || metadata.wasPlaceholder === true;
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
): void {
  const normalizedSessionId = typeof sessionId === 'string' ? sessionId.trim() : '';
  resetEventSeqTracking(0, normalizedSessionId);
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
  if (eventSeq !== undefined && eventSeq < lastAppliedEventSeq) {
    console.warn('[MessageHandler] 丢弃逆序 eventSeq', {
      eventSeq,
      lastAppliedEventSeq,
      type: message.type,
    });
    return false;
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
  if (shouldHideFromUser(standard) || !isLocalUnifiedTimelineContent(standard)) {
    return;
  }
  const meta = standard.metadata as Record<string, unknown> | undefined;
  const requestId = meta?.requestId as string | undefined;
  const isUserMessage = standard.type === MessageType.USER_INPUT;
  const incomingTurnOrderSeq = resolveTimelineTurnOrderSeqFromMetadata(meta);
  const incomingCanonicalTurnSeq = resolveTimelineCanonicalTurnSeqFromMetadata(meta);

  // 本地 CONTENT 只维护 request binding。用户可见内容统一由 canonical turn/projection 渲染，
  // 这里不再写任何本地可见时间线，避免 local unified 与 canonical 主线形成双事实源。
  if (isUserMessage) {
    if (requestId) {
      const binding = getRequestBinding(requestId);
      if (!binding) {
        createRequestBinding({
          requestId,
          userMessageId: standard.id,
          turnOrderSeq: incomingTurnOrderSeq || undefined,
          turnSeq: incomingCanonicalTurnSeq || undefined,
          createdAt: standard.timestamp || Date.now(),
        });
      } else if (!binding.userMessageId) {
        updateRequestBinding(requestId, { userMessageId: standard.id });
      }
      updateRequestBindingTurnFacts(requestId, incomingTurnOrderSeq, incomingCanonicalTurnSeq);
      addPendingRequest(requestId);
    }
    return;
  }

  const requestBinding = requestId ? getRequestBinding(requestId) : undefined;
  if (requestId && requestBinding) {
    updateRequestBindingTurnFacts(requestId, incomingTurnOrderSeq, incomingCanonicalTurnSeq);
  }
  const isPlaceholderMessage = meta?.isPlaceholder === true;
  const placeholderMessageId = typeof meta?.placeholderMessageId === 'string'
    ? meta.placeholderMessageId.trim()
    : '';
  const existingRealMessageId = typeof requestBinding?.realMessageId === 'string'
    ? requestBinding.realMessageId.trim()
    : '';
  if (isPlaceholderMessage && existingRealMessageId && existingRealMessageId !== standard.id) {
    return;
  }

  if (requestId && requestBinding) {
    if (requestBinding.timeoutId) {
      clearTimeout(requestBinding.timeoutId);
    }
    if (isPlaceholderMessage) {
      updateRequestBinding(requestId, {
        placeholderMessageId: placeholderMessageId || standard.id,
        timeoutId: undefined,
      });
    } else {
      updateRequestBinding(requestId, {
        realMessageId: standard.id,
        timeoutId: undefined,
      });
    }
  }
}


function handleStandardUpdate(message: ClientBridgeMessage) {
  void message;
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

  if (
    standard.category !== MessageCategory.CONTENT
    || shouldHideFromUser(standard)
    || !isLocalUnifiedTimelineContent(standard)
  ) {
    return;
  }

  const meta = standard.metadata as Record<string, unknown> | undefined;
  const requestId = meta?.requestId as string | undefined;
  const incomingTurnOrderSeq = resolveTimelineTurnOrderSeqFromMetadata(meta);
  const incomingCanonicalTurnSeq = resolveTimelineCanonicalTurnSeqFromMetadata(meta);
  const actualMessageId = standard.id;
  const requestBinding = requestId ? getRequestBinding(requestId) : undefined;
  if (requestId && requestBinding) {
    updateRequestBindingTurnFacts(requestId, incomingTurnOrderSeq, incomingCanonicalTurnSeq);
  }

  // 只有最终 assistant 回复或 assistant 错误才能结束本轮请求。
  // 工具卡、思考块和中途 assistant_stream 完成不能提前清理 pending 状态。
  if (requestId && isTerminalRequestResponseStandard(standard)) {
    finalizeTerminalRequestResponse(requestId, actualMessageId);
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

function updateRequestBindingTurnFacts(
  requestId: string,
  incomingTurnOrderSeq: number,
  incomingCanonicalTurnSeq: number,
): void {
  const binding = getRequestBinding(requestId);
  if (!binding) {
    return;
  }
  const updates: Parameters<typeof updateRequestBinding>[1] = {};
  const existingExplicitTurnOrderSeq = typeof binding.turnOrderSeq === 'number' && Number.isFinite(binding.turnOrderSeq)
    ? Math.floor(binding.turnOrderSeq)
    : 0;
  if (existingExplicitTurnOrderSeq <= 0 && incomingTurnOrderSeq > 0) {
    updates.turnOrderSeq = incomingTurnOrderSeq;
  }
  if (incomingCanonicalTurnSeq > 0) {
    updates.turnSeq = incomingCanonicalTurnSeq;
  }
  if (Object.keys(updates).length > 0) {
    updateRequestBinding(requestId, updates);
  }
}

function resolveStandardTurnItemKind(standard: StandardMessage): string {
  const metadata = standard.metadata as Record<string, unknown> | undefined;
  return typeof metadata?.turnItemKind === 'string' ? metadata.turnItemKind.trim() : '';
}

function isTerminalRequestResponseStandard(standard: StandardMessage): boolean {
  if (!isTerminalLifecycle(standard.lifecycle)) {
    return false;
  }
  const turnItemKind = resolveStandardTurnItemKind(standard);
  if (turnItemKind) {
    return turnItemKind === 'assistant_text'
      || turnItemKind === 'assistant_final'
      || turnItemKind === 'assistant_error';
  }
  return standard.type !== MessageType.USER_INPUT
    && standard.type !== MessageType.TOOL_CALL
    && standard.type !== MessageType.THINKING
    && standard.type !== MessageType.TASK_CARD
    && standard.type !== MessageType.SYSTEM;
}

function finalizeTerminalRequestResponse(requestId: string, actualMessageId: string): void {
  if (messagesState.backendProcessing) {
    return;
  }
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
