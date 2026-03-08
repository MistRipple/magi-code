/**
 * LLM 协议标准化类型
 *
 * 说明：
 * - 编排层只依赖 Normalized* 类型，不依赖具体 Provider 协议格式。
 * - 各 Provider 通过适配器实现协议映射。
 */

import { LLMProvider } from '../../types/agent-types';
import { LLMMessageParams, LLMResponse, LLMStreamChunk } from '../types';

export type ProtocolId = 'openai.responses' | 'anthropic.messages';

export type CapabilitySupport = 'supported' | 'unsupported' | 'unknown';

/**
 * Provider 协议能力声明
 */
export interface ProviderCapabilities {
  supportsTextIO: boolean;
  supportsImageInput: boolean;
  supportsFunctionTools: boolean;
  supportsToolChoice: boolean;
  supportsParallelToolCalls: CapabilitySupport;
  supportsThinkingStream: CapabilitySupport;
  /**
   * 是否支持会话态 continuation（例如 previous_response_id）。
   * 注意：该能力是否可用需以网关实测为准，默认可标记 unknown。
   */
  supportsStatefulConversation: CapabilitySupport;
}

export interface ProviderProtocolProfile {
  provider: LLMProvider;
  protocol: ProtocolId;
  capabilities: ProviderCapabilities;
}

/**
 * 统一请求/响应/事件模型
 */
export type NormalizedModelRequest = LLMMessageParams;
export type NormalizedModelResponse = LLMResponse;
export type NormalizedStreamEvent = LLMStreamChunk;
