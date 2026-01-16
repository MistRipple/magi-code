/**
 * StandardMessageAdapter - Webview 端标准消息适配器
 *
 * 将标准消息协议转换为 Webview 可渲染的格式
 * 支持渐进式迁移，同时兼容旧格式和新格式
 */

import {
  MessageLifecycle,
  type StandardMessage,
  type StreamUpdate,
  type ContentBlock,
  type TextBlock,
  type CodeBlock,
  type ThinkingBlock,
  type ToolCallBlock,
  type MessageType,
} from '../../protocol';

/**
 * Webview 消息格式（现有格式）
 */
export interface WebviewMessage {
  role: 'user' | 'assistant' | 'system' | 'cli_question';
  content: string;
  time: string;
  timestamp: number;
  streaming?: boolean;
  startedAt?: number;
  source?: 'worker' | 'orchestrator' | 'system';
  cli?: string;
  streamKey?: string;
  thinking?: Array<{ content: string; summary?: string }>;
  toolCalls?: Array<{
    name: string;
    id: string;
    status: string;
    input?: unknown;
    output?: string;
    error?: string;
  }>;
  interrupted?: boolean;
  timeout?: boolean;
  error?: string;
  // 标准消息扩展字段
  standardMessageId?: string;
  traceId?: string;
  lifecycle?: string;
  messageType?: string;
}

/**
 * 将标准消息转换为 Webview 消息格式
 */
export function standardToWebview(message: StandardMessage): WebviewMessage {
  const webviewMsg: WebviewMessage = {
    role: mapSourceToRole(message.source),
    content: extractTextContent(message.blocks),
    time: new Date(message.timestamp).toLocaleTimeString().slice(0, 5),
    timestamp: message.timestamp,
    streaming: message.lifecycle === 'streaming',
    source: message.source,
    cli: message.cli,
    thinking: extractThinking(message.blocks),
    toolCalls: extractToolCalls(message.blocks),
    // 标准消息扩展字段
    standardMessageId: message.id,
    traceId: message.traceId,
    lifecycle: message.lifecycle,
    messageType: message.type,
  };

  // 处理特殊状态
  if (message.lifecycle === 'interrupted') {
    webviewMsg.interrupted = true;
    webviewMsg.streaming = false;
  }

  if (message.lifecycle === 'failed') {
    webviewMsg.error = message.metadata?.error as string;
    webviewMsg.streaming = false;
  }

  return webviewMsg;
}

/**
 * 应用流式更新到 Webview 消息
 */
export function applyStreamUpdate(
  message: WebviewMessage,
  update: StreamUpdate
): WebviewMessage {
  const updated = { ...message };

  switch (update.updateType) {
    case 'append':
      if (update.appendText) {
        updated.content = (updated.content || '') + update.appendText;
      }
      break;

    case 'block_update':
      if (update.blocks) {
        // 更新 thinking 和 toolCalls
        const newThinking = extractThinking(update.blocks);
        const newToolCalls = extractToolCalls(update.blocks);

        if (newThinking.length > 0) {
          updated.thinking = [...(updated.thinking || []), ...newThinking];
        }
        if (newToolCalls.length > 0) {
          // 合并工具调用（按 ID 去重更新）
          const toolMap = new Map(
            (updated.toolCalls || []).map(t => [t.id, t])
          );
          for (const tool of newToolCalls) {
            toolMap.set(tool.id, tool);
          }
          updated.toolCalls = Array.from(toolMap.values());
        }
      }
      break;

    case 'lifecycle_change':
      if (update.lifecycle === MessageLifecycle.COMPLETED ||
          update.lifecycle === MessageLifecycle.FAILED ||
          update.lifecycle === MessageLifecycle.INTERRUPTED) {
        updated.streaming = false;
      }
      break;
  }

  return updated;
}

/**
 * 检测消息是否为标准格式
 */
export function isStandardMessage(msg: unknown): msg is StandardMessage {
  if (!msg || typeof msg !== 'object') return false;
  const m = msg as Record<string, unknown>;
  return (
    typeof m.id === 'string' &&
    typeof m.type === 'string' &&
    typeof m.source === 'string' &&
    typeof m.lifecycle === 'string' &&
    Array.isArray(m.blocks)
  );
}

/**
 * 检测消息是否为流式更新
 */
export function isStreamUpdate(msg: unknown): msg is StreamUpdate {
  if (!msg || typeof msg !== 'object') return false;
  const m = msg as Record<string, unknown>;
  return (
    typeof m.messageId === 'string' &&
    typeof m.updateType === 'string'
  );
}

// ============ 辅助函数 ============

function mapSourceToRole(source: string): 'user' | 'assistant' | 'system' {
  switch (source) {
    case 'user':
      return 'user';
    case 'system':
      return 'system';
    default:
      return 'assistant';
  }
}

function extractTextContent(blocks: ContentBlock[]): string {
  return blocks
    .filter((b): b is TextBlock => b.type === 'text')
    .map(b => b.content)
    .join('\n');
}

function extractThinking(blocks: ContentBlock[]): Array<{ content: string; summary?: string }> {
  return blocks
    .filter((b): b is ThinkingBlock => b.type === 'thinking')
    .map(b => ({
      content: b.content,
      summary: b.summary,
    }));
}

function extractToolCalls(blocks: ContentBlock[]): Array<{
  name: string;
  id: string;
  status: string;
  input?: unknown;
  output?: string;
  error?: string;
}> {
  return blocks
    .filter((b): b is ToolCallBlock => b.type === 'tool_call')
    .map(b => ({
      name: b.toolName,
      id: b.toolId,
      status: b.status,
      input: b.input,
      output: b.output,
      error: b.error,
    }));
}

/**
 * 根据消息类型获取渲染提示
 */
export function getMessageRenderHint(message: StandardMessage): {
  showThinking: boolean;
  showToolCalls: boolean;
  showInteraction: boolean;
  isError: boolean;
} {
  return {
    showThinking: message.blocks.some(b => b.type === 'thinking'),
    showToolCalls: message.blocks.some(b => b.type === 'tool_call'),
    showInteraction: message.type === 'interaction' && !!message.interaction,
    isError: message.type === 'error' || message.lifecycle === 'failed',
  };
}

/**
 * 格式化消息用于调试
 */
export function formatMessageForDebug(message: StandardMessage): string {
  return JSON.stringify({
    id: message.id,
    type: message.type,
    source: message.source,
    cli: message.cli,
    lifecycle: message.lifecycle,
    blocksCount: message.blocks.length,
    blockTypes: message.blocks.map(b => b.type),
  }, null, 2);
}