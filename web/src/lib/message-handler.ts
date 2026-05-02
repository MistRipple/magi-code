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
  getTimelineMessageById,
  upsertTimelineNode,
  messagesState,
} from '../stores/messages.svelte';
import type { Message, ContentBlock } from '../types/message';
import type { StandardMessage, StreamUpdate } from '../shared/protocol/message-protocol';
import { MessageType, MessageCategory, MessageLifecycle } from '../shared/protocol/message-protocol';
import { resolveTaskCardWorkerSlot } from './worker-role-utils';
import { i18n } from '../stores/i18n.svelte';
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
import { resolveTimelineWorkerId } from '../shared/timeline-worker-lifecycle';
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

function resolveTimelineVisibilityForStandard(standard: StandardMessage): {
  thread: boolean;
  workerTabs?: string[];
} {
  const workerSlot = resolveLaneWorkerSlot(standard);
  const metadata = standard.metadata as Record<string, unknown> | undefined;
  const visibility = resolveSharedTimelineWorkerVisibility({
    hasWorker: Boolean(workerSlot),
    type: standard.type,
    source: standard.source,
    blocks: standard.blocks,
    metadata,
  });
  const explicitThreadVisible = typeof metadata?.threadVisible === 'boolean'
    ? metadata.threadVisible
    : undefined;
  const explicitWorkerVisible = typeof metadata?.workerVisible === 'boolean'
    ? metadata.workerVisible
    : undefined;
  const includeWorkerTab = explicitWorkerVisible ?? visibility.includeWorkerTab;
  return {
    thread: explicitThreadVisible ?? visibility.threadVisible,
    workerTabs: includeWorkerTab && workerSlot ? [workerSlot] : undefined,
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
  let uiMessage = mapStandardMessage(standard);
  const visibility = resolveTimelineVisibilityForStandard(standard);
  const meta = standard.metadata as Record<string, unknown> | undefined;
  const requestId = meta?.requestId as string | undefined;
  const isUserMessage = standard.type === MessageType.USER_INPUT;
  const incomingTurnOrderSeq = resolveTimelineTurnOrderSeqFromMetadata(meta);
  const incomingCanonicalTurnSeq = resolveTimelineCanonicalTurnSeqFromMetadata(meta);

  // === 用户消息 ===
  // 用户消息在 unifiedMessage 首次到达时立即创建 timeline node 锚点
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
      uiMessage = applyRequestTurnFacts(uiMessage, getRequestBinding(requestId) || binding);
      addPendingRequest(requestId);

      // 若后端 canonical 消息 id 与前端临时消息 id 不同，替换现有节点而非创建新节点
      if (binding?.userMessageId && binding.userMessageId !== uiMessage.id) {
        upsertTimelineNode(uiMessage, visibility, { replaceMessageId: binding.userMessageId });
        return;
      }
    }
    upsertTimelineNode(uiMessage, visibility);
    return;
  }

  // === Assistant 消息 ===
  const requestBinding = requestId ? getRequestBinding(requestId) : undefined;
  if (requestId && requestBinding) {
    updateRequestBindingTurnFacts(requestId, incomingTurnOrderSeq, incomingCanonicalTurnSeq);
  }
  uiMessage = applyRequestTurnFacts(uiMessage, requestId ? getRequestBinding(requestId) : requestBinding);
  const isPlaceholderMessage = uiMessage.metadata?.isPlaceholder === true;
  const placeholderMessageId = typeof uiMessage.metadata?.placeholderMessageId === 'string'
    ? uiMessage.metadata.placeholderMessageId.trim()
    : '';
  const canBindAsAssistantResponse = isAssistantResponsePlaceholderTarget(uiMessage);
  const existingRealMessageId = typeof requestBinding?.realMessageId === 'string'
    ? requestBinding.realMessageId.trim()
    : '';
  if (isPlaceholderMessage && existingRealMessageId && existingRealMessageId !== uiMessage.id) {
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
    } else if (canBindAsAssistantResponse) {
      updateRequestBinding(requestId, {
        realMessageId: standard.id,
        timeoutId: undefined,
      });
    }
  }

  const hasExistingTimelineMessage = Boolean(getTimelineMessageById(uiMessage.id));
  const placeholderReplacementId = !isPlaceholderMessage && canBindAsAssistantResponse
    ? resolvePlaceholderReplacementId(requestBinding, uiMessage.id)
    : undefined;
  if (
    uiMessage.isStreaming
    || hasRenderableContent(uiMessage)
    || hasExistingTimelineMessage
    || Boolean(placeholderReplacementId)
  ) {
    upsertTimelineNode(
      uiMessage,
      visibility,
      placeholderReplacementId ? { replaceMessageId: placeholderReplacementId } : {},
    );
    if (placeholderReplacementId) {
      markMessageComplete(placeholderReplacementId);
    }
  }

  if (uiMessage.isStreaming) {
    markMessageActive(uiMessage.id);
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
  const visibility = resolveTimelineVisibilityForStandard(standard);
  const requestBinding = requestId ? getRequestBinding(requestId) : undefined;
  if (requestId && requestBinding) {
    updateRequestBindingTurnFacts(requestId, incomingTurnOrderSeq, incomingCanonicalTurnSeq);
  }
  const uiMessage = applyRequestTurnFacts(
    mapStandardMessage(standard),
    requestId ? getRequestBinding(requestId) : requestBinding,
  );
  const placeholderReplacementId = isAssistantResponsePlaceholderTarget(uiMessage)
    ? resolvePlaceholderReplacementId(requestBinding, actualMessageId)
    : undefined;
  const uiMetadata = uiMessage.metadata && typeof uiMessage.metadata === 'object'
    ? uiMessage.metadata
    : {};
  const authoritativeResponseDurationMs = typeof uiMetadata.responseDurationMs === 'number'
    ? uiMetadata.responseDurationMs
    : undefined;
  const { responseDurationMs: _nonTerminalResponseDurationMs, ...uiMetadataWithoutResponseDuration } = uiMetadata;

  const existingMessage = getTimelineMessageById(actualMessageId)
    || (placeholderReplacementId ? getTimelineMessageById(placeholderReplacementId) : undefined);
  if (!existingMessage) {
    const isTerminalRequestResponse = isRequestTerminalAssistantResponse(uiMessage);
    markMessageComplete(actualMessageId);
    if (placeholderReplacementId) {
      markMessageComplete(placeholderReplacementId);
    }
    if (requestId && isTerminalRequestResponse) {
      finalizeTerminalRequestResponse(requestId, actualMessageId);
    }
    settleProcessingAfterResponseCompletion();
    return;
  }
  const completedMessageBase: Message = {
    ...existingMessage,
    isStreaming: false,
    isComplete: true,
    metadata: {
      ...(existingMessage.metadata || {}),
      ...uiMetadata,
    },
  };
  const isTerminalRequestResponse = isRequestTerminalAssistantResponse(completedMessageBase);
  const completedMetadata = completedMessageBase.metadata || {};
  const completedMessage = {
    ...completedMessageBase,
    metadata: isTerminalRequestResponse
      ? {
          ...completedMetadata,
          ...(typeof authoritativeResponseDurationMs === 'number' ? { responseDurationMs: authoritativeResponseDurationMs } : {}),
        }
      : {
          ...completedMetadata,
          ...uiMetadataWithoutResponseDuration,
        },
  };

  if (
    hasRenderableContent(completedMessage)
    || getTimelineMessageById(actualMessageId)
    || Boolean(placeholderReplacementId)
  ) {
    upsertTimelineNode(
      completedMessage,
      visibility,
      placeholderReplacementId ? { replaceMessageId: placeholderReplacementId } : {},
    );
    if (placeholderReplacementId) {
      markMessageComplete(placeholderReplacementId);
    }
  }

  markMessageComplete(actualMessageId);

  // 只有最终 assistant 回复或 assistant 错误才能结束本轮请求。
  // 工具卡、思考块和中途 assistant_stream 完成只更新原位节点，不能提前清理 pending 状态。
  if (requestId && isTerminalRequestResponse) {
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

function isStreamingLifecycle(lifecycle: StandardMessage['lifecycle'] | undefined): boolean {
  return lifecycle === MessageLifecycle.STREAMING || lifecycle === MessageLifecycle.STARTED;
}

function resolveBindingTurnOrderSeq(
  binding: ReturnType<typeof getRequestBinding> | undefined,
): number {
  const turnOrderSeq = typeof binding?.turnOrderSeq === 'number' && Number.isFinite(binding.turnOrderSeq)
    ? Math.floor(binding.turnOrderSeq)
    : 0;
  if (turnOrderSeq > 0) {
    return turnOrderSeq;
  }
  const canonicalTurnSeq = typeof binding?.turnSeq === 'number' && Number.isFinite(binding.turnSeq)
    ? Math.floor(binding.turnSeq)
    : 0;
  return canonicalTurnSeq > 0 ? canonicalTurnSeq : 0;
}

function resolveBindingCanonicalTurnSeq(
  binding: ReturnType<typeof getRequestBinding> | undefined,
): number {
  const canonicalTurnSeq = typeof binding?.turnSeq === 'number' && Number.isFinite(binding.turnSeq)
    ? Math.floor(binding.turnSeq)
    : 0;
  return canonicalTurnSeq > 0 ? canonicalTurnSeq : 0;
}

function readPositiveMetadataNumber(metadata: Record<string, unknown>, key: string): number {
  const raw = metadata[key];
  if (typeof raw !== 'number' || !Number.isFinite(raw)) {
    return 0;
  }
  const normalized = Math.floor(raw);
  return normalized > 0 ? normalized : 0;
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

function applyRequestTurnFacts(
  message: Message,
  binding: ReturnType<typeof getRequestBinding> | undefined,
): Message {
  const metadata = message.metadata && typeof message.metadata === 'object'
    ? message.metadata as Record<string, unknown>
    : {};
  const existingTurnOrderSeq = readPositiveMetadataNumber(metadata, 'turnOrderSeq');
  const existingCanonicalTurnSeq = resolveTimelineCanonicalTurnSeqFromMetadata(metadata);
  const boundTurnOrderSeq = resolveBindingTurnOrderSeq(binding);
  const boundCanonicalTurnSeq = resolveBindingCanonicalTurnSeq(binding);
  if (
    (existingTurnOrderSeq > 0 || boundTurnOrderSeq <= 0)
    && (existingCanonicalTurnSeq > 0 || boundCanonicalTurnSeq <= 0)
  ) {
    return message;
  }
  return {
    ...message,
    metadata: {
      ...metadata,
      ...(existingTurnOrderSeq <= 0 && boundTurnOrderSeq > 0 ? { turnOrderSeq: boundTurnOrderSeq } : {}),
      ...(existingCanonicalTurnSeq <= 0 && boundCanonicalTurnSeq > 0 ? { turnSeq: boundCanonicalTurnSeq } : {}),
    },
  };
}

function resolveTurnItemKind(message: Message): string {
  return typeof message.metadata?.turnItemKind === 'string'
    ? message.metadata.turnItemKind.trim()
    : '';
}

function isRequestTerminalAssistantResponse(message: Message): boolean {
  if (
    message.role !== 'assistant'
    || message.isStreaming === true
    || !hasRenderableContent(message)
  ) {
    return false;
  }
  const turnItemKind = resolveTurnItemKind(message);
  if (turnItemKind) {
    return turnItemKind === 'assistant_text'
      || turnItemKind === 'assistant_final'
      || turnItemKind === 'assistant_error';
  }
  return message.type !== MessageType.TOOL_CALL && message.type !== MessageType.THINKING;
}

function isAssistantResponsePlaceholderTarget(message: Message): boolean {
  if (message.metadata?.isPlaceholder === true || message.role !== 'assistant') {
    return false;
  }
  const turnItemKind = resolveTurnItemKind(message);
  if (turnItemKind) {
    return turnItemKind === 'assistant_stream'
      || turnItemKind === 'assistant_text'
      || turnItemKind === 'assistant_final'
      || turnItemKind === 'assistant_error';
  }
  return message.type !== MessageType.TOOL_CALL && message.type !== MessageType.THINKING;
}

function resolvePlaceholderReplacementId(
  requestBinding: ReturnType<typeof getRequestBinding> | undefined,
  incomingMessageId: string,
): string | undefined {
  const placeholderMessageId = typeof requestBinding?.placeholderMessageId === 'string'
    ? requestBinding.placeholderMessageId.trim()
    : '';
  if (!placeholderMessageId || placeholderMessageId === incomingMessageId) {
    return undefined;
  }
  const realMessageId = typeof requestBinding?.realMessageId === 'string'
    ? requestBinding.realMessageId.trim()
    : '';
  if (realMessageId && realMessageId !== placeholderMessageId && realMessageId !== incomingMessageId) {
    return undefined;
  }
  return placeholderMessageId;
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
