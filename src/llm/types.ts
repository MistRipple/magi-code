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
}

/**
 * 工具调用结果
 */
export interface ToolResult {
  toolCallId: string;
  content: string;
  isError?: boolean;
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
