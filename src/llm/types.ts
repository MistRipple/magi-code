/**
 * LLM 客户端相关类型定义
 */

import { AgentType, LLMConfig } from '../types/agent-types';

// ============================================================================
// 工具相关类型
// ============================================================================

/**
 * 工具定义（Claude API 格式）
 */
export interface ToolDefinition {
  name: string;
  description: string;
  input_schema: {
    type: 'object';
    properties: Record<string, any>;
    required?: string[];
  };
}

/**
 * 工具调用
 */
export interface ToolCall {
  id: string;
  name: string;
  arguments: Record<string, any>;
  /** 参数解析失败时写入错误信息，供上层直接返回 tool_result 错误，避免误执行工具 */
  argumentParseError?: string;
  /** 参数解析失败时保留原始文本（用于日志与错误回传） */
  rawArguments?: string;
}

/**
 * 文件变更元数据（用于前端 FileChangeCard 差异化面板展示）
 */
export interface FileChangeMetadata {
  filePath: string;
  changeType: 'create' | 'modify' | 'delete';
  additions: number;
  deletions: number;
  /** unified diff 格式文本 */
  diff: string;
}

/**
 * 工具调用结果
 */
export interface ToolResult {
  toolCallId: string;
  content: string;
  isError?: boolean;
  /** 文件变更工具专用：携带 diff 数据供前端差异化面板展示 */
  fileChange?: FileChangeMetadata;
}

// ============================================================================
// LLM 消息相关类型
// ============================================================================

/**
 * 内容块（支持文本、图片和工具调用）
 */
export type ContentBlock =
  | { type: 'text'; text: string }
  | { type: 'image'; source: { type: 'base64'; media_type: string; data: string } }
  | { type: 'tool_use'; id: string; name: string; input: Record<string, any> }
  | { type: 'tool_result'; tool_use_id: string; content: string; is_error?: boolean };

/**
 * LLM 消息
 */
export interface LLMMessage {
  role: 'user' | 'assistant' | 'system';
  content: string | ContentBlock[];
}

/**
 * LLM 请求参数
 */
export interface LLMMessageParams {
  messages: LLMMessage[];
  maxTokens?: number;
  temperature?: number;
  tools?: ToolDefinition[];
  stream?: boolean;
  systemPrompt?: string;
  toolChoice?: ToolChoice;
  /** 取消信号，用于中断正在进行的 LLM 请求 */
  signal?: AbortSignal;
}

export type ToolChoice =
  | 'auto'
  | 'none'
  | 'required'
  | { type: 'any' }
  | { type: 'tool'; name: string };

/**
 * LLM 响应
 */
export interface LLMResponse {
  content: string;
  toolCalls?: ToolCall[];
  usage: {
    inputTokens: number;
    outputTokens: number;
    cacheReadTokens?: number;
    cacheWriteTokens?: number;
    estimated?: boolean;
  };
  stopReason: 'end_turn' | 'max_tokens' | 'tool_use' | 'stop_sequence';
}

/**
 * LLM 流式块
 */
export interface LLMStreamChunk {
  type: 'content_start' | 'content_delta' | 'content_end' | 'tool_call_start' | 'tool_call_delta' | 'tool_call_end' | 'thinking' | 'usage';
  content?: string;
  toolCall?: Partial<ToolCall>;
  thinking?: string;
  usage?: {
    inputTokens?: number;
    outputTokens?: number;
    cacheReadTokens?: number;
    cacheWriteTokens?: number;
    estimated?: boolean;
  };
}

// ============================================================================
// LLM 客户端接口
// ============================================================================

/**
 * LLM 客户端接口
 */
export interface LLMClient {
  config: LLMConfig;

  /**
   * 发送消息（非流式）
   */
  sendMessage(params: LLMMessageParams): Promise<LLMResponse>;

  /**
   * 发送消息（流式）
   */
  streamMessage(
    params: LLMMessageParams,
    onChunk: (chunk: LLMStreamChunk) => void
  ): Promise<LLMResponse>;

  /**
   * 测试连接
   */
  testConnection(): Promise<boolean>;

  /**
   * 快速测试连接（使用 Models API，不消耗 tokens）
   */
  testConnectionFast(): Promise<{
    success: boolean;
    modelExists?: boolean;
    error?: string;
  }>;
}

// ============================================================================
// 配置相关类型
// ============================================================================

/**
 * Worker 配置
 */
export interface WorkerLLMConfig {
  claude: LLMConfig;
  codex: LLMConfig;
  gemini: LLMConfig;
}

/**
 * 完整的 LLM 配置
 */
export interface FullLLMConfig {
  orchestrator: LLMConfig;
  workers: WorkerLLMConfig;
  compressor: LLMConfig;
  userRules?: {
    enabled: boolean;
    content: string;
  };
}

// ============================================================================
// 消息清洗工具
// ============================================================================

/**
 * 清理消息历史中悬空的 tool_use/tool_result 对。
 * 确保每个 assistant(tool_use) 紧跟对应的 user(tool_result)，
 * 丢弃因中断、截断等原因产生的不完整工具调用链路。
 *
 * 所有需要清洗工具调用顺序的地方统一调用此函数（禁止多重实现）。
 */
export function sanitizeToolOrder(inputMessages: LLMMessage[]): LLMMessage[] {
  const hasToolUse = (msg: LLMMessage): boolean =>
    Array.isArray(msg.content) && msg.content.some((b: any) => b?.type === 'tool_use');

  const isToolResultUser = (msg?: LLMMessage): boolean =>
    !!msg && msg.role === 'user' && Array.isArray(msg.content)
    && msg.content.some((b: any) => b?.type === 'tool_result');

  const isUserOrToolResult = (msg?: LLMMessage): boolean =>
    !!msg && msg.role === 'user';

  const usedToolIds = new Set<string>();
  let syntheticIdSeq = 0;

  const allocateToolId = (preferred?: string): string => {
    const normalizedPreferred = typeof preferred === 'string' ? preferred.trim() : '';
    if (normalizedPreferred && !usedToolIds.has(normalizedPreferred)) {
      usedToolIds.add(normalizedPreferred);
      return normalizedPreferred;
    }

    let candidate = '';
    do {
      candidate = `magi_tool_${syntheticIdSeq++}`;
    } while (usedToolIds.has(candidate));
    usedToolIds.add(candidate);
    return candidate;
  };

  const normalizeAssistantToolUse = (
    message: LLMMessage
  ): { message: LLMMessage; orderedToolIds: string[] } => {
    if (!Array.isArray(message.content)) {
      return { message, orderedToolIds: [] };
    }

    const orderedToolIds: string[] = [];
    const normalizedBlocks = (message.content as any[]).map((block) => {
      if (block?.type !== 'tool_use') {
        return block;
      }
      const id = allocateToolId(block.id);
      orderedToolIds.push(id);
      return { ...block, id };
    });

    return {
      message: { ...message, content: normalizedBlocks as ContentBlock[] },
      orderedToolIds,
    };
  };

  const normalizeUserToolResult = (
    message: LLMMessage,
    orderedToolIds: string[]
  ): LLMMessage | null => {
    if (!Array.isArray(message.content)) {
      return null;
    }

    const availableIds = orderedToolIds.slice();
    const availableIdSet = new Set(availableIds);
    const consumed = new Set<string>();
    let nextAvailableIndex = 0;
    const normalizedBlocks: any[] = [];
    let validResultCount = 0;

    const takeNextAvailableId = (): string => {
      while (nextAvailableIndex < availableIds.length) {
        const candidate = availableIds[nextAvailableIndex++];
        if (!consumed.has(candidate)) {
          return candidate;
        }
      }
      return '';
    };

    for (const block of message.content as any[]) {
      if (block?.type !== 'tool_result') {
        normalizedBlocks.push(block);
        continue;
      }

      const incomingId = typeof block.tool_use_id === 'string' ? block.tool_use_id.trim() : '';
      let resolvedId = '';

      if (incomingId && availableIdSet.has(incomingId) && !consumed.has(incomingId)) {
        resolvedId = incomingId;
      } else {
        resolvedId = takeNextAvailableId();
      }

      if (!resolvedId) {
        continue; // 丢弃无法配对的悬空 tool_result
      }

      consumed.add(resolvedId);
      validResultCount++;
      const normalizedContent = typeof block.content === 'string'
        ? block.content
        : (block.content == null ? '' : JSON.stringify(block.content));

      normalizedBlocks.push({
        ...block,
        tool_use_id: resolvedId,
        content: normalizedContent,
      });
    }

    if (validResultCount === 0) {
      return null;
    }

    return {
      ...message,
      content: normalizedBlocks as ContentBlock[],
    };
  };

  const cleaned: LLMMessage[] = [];
  for (let i = 0; i < inputMessages.length; i++) {
    const msg = inputMessages[i];

    if (msg.role === 'assistant' && hasToolUse(msg)) {
      const next = inputMessages[i + 1];
      const prev = cleaned[cleaned.length - 1];
      if (!next || !isToolResultUser(next) || !isUserOrToolResult(prev)) {
        continue;
      }

      const normalizedAssistant = normalizeAssistantToolUse(msg);
      if (normalizedAssistant.orderedToolIds.length === 0) {
        continue;
      }

      const normalizedResult = normalizeUserToolResult(next, normalizedAssistant.orderedToolIds);
      if (!normalizedResult) {
        i += 1;
        continue;
      }

      cleaned.push(normalizedAssistant.message);
      cleaned.push(normalizedResult);
      i += 1;
      continue;
    }

    if (isToolResultUser(msg)) {
      const prev = cleaned[cleaned.length - 1];
      if (!prev || !hasToolUse(prev)) {
        // 保留同条消息中的普通文本/图片，丢弃悬空 tool_result
        const retained = Array.isArray(msg.content)
          ? (msg.content as any[]).filter((b: any) => b?.type !== 'tool_result')
          : [];
        if (retained.length > 0) {
          cleaned.push({ ...msg, content: retained as ContentBlock[] });
        }
        continue;
      }
    }

    cleaned.push(msg);
  }
  return cleaned;
}
