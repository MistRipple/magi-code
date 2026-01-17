/**
 * Normalizer 模块导出
 *
 * 提供各 CLI 的消息标准化器
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

import type { CLIType } from '../cli/types';
import { BaseNormalizer } from './base-normalizer';
import { ClaudeNormalizer } from './claude-normalizer';
import { CodexNormalizer } from './codex-normalizer';
import { GeminiNormalizer } from './gemini-normalizer';
import type { MessageSource } from '../protocol';

/**
 * 创建 CLI 对应的 Normalizer
 */
export function createNormalizer(
  cli: CLIType,
  source: MessageSource = 'worker',
  debug = false
): BaseNormalizer {
  switch (cli) {
    case 'claude':
      return new ClaudeNormalizer({ cli, defaultSource: source, debug });
    case 'codex':
      return new CodexNormalizer({ cli, defaultSource: source, debug });
    case 'gemini':
      return new GeminiNormalizer({ cli, defaultSource: source, debug });
    default:
      throw new Error(`Unknown CLI type: ${cli}`);
  }
}

