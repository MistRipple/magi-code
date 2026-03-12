/**
 * Provider 协议能力注册表
 */

import { LLMProvider } from '../../types/agent-types';
import { ProviderCapabilities, ProviderProtocolProfile, ProtocolId } from './types';

const CAPABILITIES_BY_PROTOCOL: Record<ProtocolId, ProviderCapabilities> = {
  'openai.responses': {
    supportsTextIO: true,
    supportsImageInput: true,
    supportsFunctionTools: true,
    supportsToolChoice: true,
    supportsParallelToolCalls: 'supported',
    supportsThinkingStream: 'supported',
    supportsStatefulConversation: 'unknown',
  },
  'openai.chat-completions': {
    supportsTextIO: true,
    supportsImageInput: true,
    supportsFunctionTools: true,
    supportsToolChoice: true,
    supportsParallelToolCalls: 'supported',
    supportsThinkingStream: 'supported',
    supportsStatefulConversation: 'unsupported',
  },
  'anthropic.messages': {
    supportsTextIO: true,
    supportsImageInput: true,
    supportsFunctionTools: true,
    supportsToolChoice: true,
    supportsParallelToolCalls: 'supported',
    supportsThinkingStream: 'supported',
    supportsStatefulConversation: 'unsupported',
  },
};

/**
 * 判断 baseUrl 是否为 OpenAI 官方端点
 */
function isOfficialOpenAI(baseUrl?: string): boolean {
  if (!baseUrl) return true;
  try {
    const url = new URL(baseUrl);
    return url.hostname === 'api.openai.com';
  } catch {
    return true;
  }
}

export function resolveProtocolId(provider: LLMProvider, baseUrl?: string): ProtocolId {
  if (provider === 'openai') {
    // 仅 OpenAI 官方端点使用 Responses API，第三方兼容 API 使用 Chat Completions
    return isOfficialOpenAI(baseUrl) ? 'openai.responses' : 'openai.chat-completions';
  }
  return 'anthropic.messages';
}

export function resolveProviderProtocolProfile(
  provider: LLMProvider,
  protocolOverride?: ProtocolId,
): ProviderProtocolProfile {
  const protocol = protocolOverride ?? resolveProtocolId(provider);
  const capabilities = CAPABILITIES_BY_PROTOCOL[protocol];
  return {
    provider,
    protocol,
    capabilities: { ...capabilities },
  };
}
