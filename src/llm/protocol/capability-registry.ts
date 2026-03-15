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

export function resolveProtocolId(
  provider: LLMProvider,
  openaiProtocol?: 'responses' | 'chat',
): ProtocolId {
  if (provider === 'openai') {
    return openaiProtocol === 'chat'
      ? 'openai.chat-completions'
      : 'openai.responses';
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
