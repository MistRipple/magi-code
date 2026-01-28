/**
 * 统一事件名常量
 *
 * 消息流架构（4层）：
 * Layer 1: Normalizer.emit(MESSAGE_EVENTS.MESSAGE/UPDATE/COMPLETE)
 * Layer 2: Adapter → messageBus.sendMessage() [直接调用]
 * Layer 3: MessageBus → emit(MESSAGE_EVENTS.MESSAGE/COMPLETE)
 * Layer 4: WebviewProvider → postMessage(WEBVIEW_MESSAGE_TYPES.*)
 *
 * 错误事件流：
 * Normalizer.emit(ERROR) → Adapter.emit(NORMALIZER_ERROR) → AdapterFactory.emit(ERROR)
 */

/**
 * 消息事件名（用于 Normalizer、MessageBus）
 */
export const MESSAGE_EVENTS = {
  /** 消息开始（STARTED 状态） */
  MESSAGE: 'message',

  /** 消息流式更新 */
  UPDATE: 'update',

  /** 消息完成（COMPLETED/FAILED/CANCELLED 状态） */
  COMPLETE: 'complete',

  /** 错误事件 */
  ERROR: 'error',
} as const;

/**
 * 处理状态事件名
 */
export const PROCESSING_EVENTS = {
  /** 处理状态变化 */
  STATE_CHANGED: 'processingStateChanged',
} as const;

/**
 * Adapter 层事件名
 *
 * 注意：消息事件不再通过 EventEmitter 传递，Adapter 直接调用 MessageBus
 * 这里只保留错误事件和状态事件的常量
 */
export const ADAPTER_EVENTS = {
  /** Normalizer 错误 */
  NORMALIZER_ERROR: 'normalizerError',

  /** 适配器错误 */
  ERROR: 'error',

  /** 工具结果 */
  TOOL_RESULT: 'toolResult',

  /** 状态变化 */
  STATE_CHANGE: 'stateChange',
} as const;

/**
 * Webview 消息类型（用于 postMessage）
 */
export const WEBVIEW_MESSAGE_TYPES = {
  /** 标准消息 */
  STANDARD_MESSAGE: 'standardMessage',

  /** 标准更新 */
  STANDARD_UPDATE: 'standardUpdate',

  /** 标准完成 */
  STANDARD_COMPLETE: 'standardComplete',

  /** 处理状态变化 */
  PROCESSING_STATE_CHANGED: 'processingStateChanged',
} as const;

// 类型导出
export type MessageEventName = typeof MESSAGE_EVENTS[keyof typeof MESSAGE_EVENTS];
export type ProcessingEventName = typeof PROCESSING_EVENTS[keyof typeof PROCESSING_EVENTS];
export type AdapterEventName = typeof ADAPTER_EVENTS[keyof typeof ADAPTER_EVENTS];
export type WebviewMessageType = typeof WEBVIEW_MESSAGE_TYPES[keyof typeof WEBVIEW_MESSAGE_TYPES];

