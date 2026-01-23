/**
 * Normalizer 模块导出
 *
 * 提供各模型的消息标准化器
 */

export {
  BaseNormalizer,
  type NormalizerConfig,
  type NormalizerEvents,
  type ParseContext,
} from './base-normalizer';

export { ClaudeNormalizer } from './claude-normalizer';
export { CodexNormalizer } from './codex-normalizer';
export { GeminiNormalizer } from './gemini-normalizer';
export { MessageBridge, type MessageBridgeConfig, type MessageBridgeEvents } from './message-bridge';
export {
  normalizeOrchestratorMessage,
  createOrchestratorUpdate,
  isInternalStateMessage,
  getMessagePriority,
} from './orchestrator-normalizer';
export { MessageBatcher, type BatcherConfig, type BatchCallback } from './message-batcher';
export { MessageDeduplicator, type DeduplicationConfig } from './message-deduplicator';
export {
  UnifiedMessageBus,
  type MessageBusConfig,
  type MessageBusEvents,
  type ProcessingState,
} from './unified-message-bus';

import type { AgentType, WorkerSlot } from '../types/agent-types';  // ✅ 使用 AgentType
import { BaseNormalizer } from './base-normalizer';
import { ClaudeNormalizer } from './claude-normalizer';
import { CodexNormalizer } from './codex-normalizer';
import { GeminiNormalizer } from './gemini-normalizer';
import type { MessageSource } from '../protocol';

/**
 * 创建模型对应的 Normalizer
 */
export function createNormalizer(
  agent: WorkerSlot,  // ✅ 使用 WorkerSlot (不包含 orchestrator)
  source: MessageSource = 'worker',
  debug = false
): BaseNormalizer {
  switch (agent) {
    case 'claude':
      return new ClaudeNormalizer({ agent, defaultSource: source, debug });
    case 'codex':
      return new CodexNormalizer({ agent, defaultSource: source, debug });
    case 'gemini':
      return new GeminiNormalizer({ agent, defaultSource: source, debug });
    default:
      throw new Error(`Unknown agent type: ${agent}`);
  }
}
