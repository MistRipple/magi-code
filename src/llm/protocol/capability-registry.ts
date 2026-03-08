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

export function resolveProtocolId(provider: LLMProvider): ProtocolId {
  if (provider === 'openai') {
    return 'openai.responses';
  }
  return 'anthropic.messages';
}

export function resolveProviderProtocolProfile(provider: LLMProvider): ProviderProtocolProfile {
  const protocol = resolveProtocolId(provider);
  const capabilities = CAPABILITIES_BY_PROTOCOL[protocol];
  return {
    provider,
    protocol,
    capabilities: { ...capabilities },
  };
}
