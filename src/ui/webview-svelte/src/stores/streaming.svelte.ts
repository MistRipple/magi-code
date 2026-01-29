/**
 * 流式状态管理 - Svelte 5 Runes
 * 专门处理流式输出的细粒度响应式状态
 */

import type { Message } from '../types/message';

// ============ 流式消息状态 ============

// 当前正在流式输出的消息 ID
let streamingMessageId = $state<string | null>(null);

// 流式内容缓冲区（用于增量更新）
let streamingBuffer = $state<string>('');

// 上次更新时间戳（用于节流控制）
let lastUpdateTime = $state<number>(0);

// 流式状态标志
let isStreamingActive = $state<boolean>(false);

// 更新间隔（16ms ≈ 60fps）
const MIN_UPDATE_INTERVAL = 16;

// ============ 导出 Getter ============

export function getStreamingState() {
  return {
    get messageId() { return streamingMessageId; },
    get buffer() { return streamingBuffer; },
    get isActive() { return isStreamingActive; },
    get lastUpdateTime() { return lastUpdateTime; },
  };
}

// ============ 流式操作 ============

/**
 * 开始新的流式输出
 */
export function startStreaming(messageId: string) {
  streamingMessageId = messageId;
  streamingBuffer = '';
  isStreamingActive = true;
  lastUpdateTime = Date.now();
}

/**
 * 追加流式内容（增量更新）
 */
export function appendStreamingContent(delta: string): boolean {
  if (!isStreamingActive || !streamingMessageId) {
    return false;
  }
  
  streamingBuffer += delta;
  
  // 检查是否需要节流
  const now = Date.now();
  if (now - lastUpdateTime < MIN_UPDATE_INTERVAL) {
    return false; // 返回 false 表示不需要立即渲染
  }
  
  lastUpdateTime = now;
  return true; // 返回 true 表示可以渲染
}

/**
 * 获取并清空缓冲区
 */
export function flushBuffer(): string {
  const content = streamingBuffer;
  // 注意：不清空缓冲区，因为需要累积内容
  return content;
}

/**
 * 结束流式输出
 */
export function endStreaming(): string {
  const finalContent = streamingBuffer;
  streamingMessageId = null;
  streamingBuffer = '';
  isStreamingActive = false;
  return finalContent;
}

/**
 * 强制刷新（忽略节流）
 */
export function forceFlush() {
  lastUpdateTime = 0;
}

/**
 * 检查指定消息是否正在流式输出
 */
export function isMessageStreaming(messageId: string): boolean {
  return isStreamingActive && streamingMessageId === messageId;
}

// ============ 流式消息创建辅助 ============

/**
 * 创建一个新的流式消息
 */
export function createStreamingMessage(
  id: string,
  source: 'orchestrator' | 'claude' | 'codex' | 'gemini' = 'orchestrator'
): Message {
  return {
    id,
    role: 'assistant',
    source,
    content: '',
    timestamp: Date.now(),
    isStreaming: true,
    isComplete: false,
  };
}

/**
 * 标记消息完成
 */
export function completeMessage(message: Message, finalContent: string): Message {
  return {
    ...message,
    content: finalContent,
    isStreaming: false,
    isComplete: true,
  };
}

