import type {
  StandardMessage,
  ContentBlock,
  InteractionRequest,
  MessageCategory,
  MessageSource,
  MessageVisibility,
} from '../protocol/message-protocol';
import type { AgentType } from '../types/agent-types';

export interface PersistedStandardMessagePayload {
  role: 'user' | 'assistant' | 'system';
  content: string;
  type: string;
  category: MessageCategory;
  visibility?: MessageVisibility;
  timestamp: number;
  updatedAt: number;
  source: MessageSource;
  agent?: AgentType;
  interaction?: InteractionRequest;
  metadata?: Record<string, unknown>;
  blocks?: ContentBlock[];
}

interface PersistedMessageMetadataCarrier {
  role?: 'user' | 'assistant' | 'system';
  type?: string;
  content?: string;
  metadata?: Record<string, unknown>;
  blocks?: ContentBlock[];
}

const ALWAYS_TRANSIENT_METADATA_KEYS = new Set([
  'justCompleted',
  'sendingAnimation',
  'wasPlaceholder',
]);

const PLACEHOLDER_METADATA_KEYS = new Set([
  'isPlaceholder',
  'placeholderState',
]);

function resolvePersistedRole(message: StandardMessage): 'user' | 'assistant' | 'system' {
  if (message.type === 'user_input') {
    return 'user';
  }
  if (message.type === 'system-notice') {
    return 'system';
  }
  return 'assistant';
}

function hasRenderablePersistedBlocks(blocks: ContentBlock[] | undefined): boolean {
  if (!Array.isArray(blocks) || blocks.length === 0) {
    return false;
  }
  return blocks.some((block) => {
    if (!block) {
      return false;
    }
    switch (block.type) {
      case 'text':
      case 'code':
        return Boolean(block.content && block.content.trim());
      case 'thinking':
        return Boolean(block.content && block.content.trim());
      case 'tool_call':
      case 'file_change':
      case 'plan':
        return true;
      default:
        return false;
    }
  });
}

function shouldRetainPlaceholderMetadata(message: PersistedMessageMetadataCarrier): boolean {
  const metadata = message.metadata && typeof message.metadata === 'object' && !Array.isArray(message.metadata)
    ? message.metadata
    : undefined;
  if (metadata?.isPlaceholder !== true) {
    return false;
  }
  const hasRenderableContent = Boolean(message.content && message.content.trim())
    || hasRenderablePersistedBlocks(message.blocks);
  return !hasRenderableContent;
}

export function sanitizePersistedMessageMetadata(
  message: PersistedMessageMetadataCarrier,
): Record<string, unknown> | undefined {
  const metadata = message.metadata && typeof message.metadata === 'object' && !Array.isArray(message.metadata)
    ? { ...message.metadata }
    : undefined;
  if (!metadata) {
    return undefined;
  }

  for (const key of ALWAYS_TRANSIENT_METADATA_KEYS) {
    delete metadata[key];
  }

  if (!shouldRetainPlaceholderMetadata(message)) {
    for (const key of PLACEHOLDER_METADATA_KEYS) {
      delete metadata[key];
    }
  }

  return Object.keys(metadata).length > 0 ? metadata : undefined;
}

/**
 * 将标准消息归一为 session 可持久化的结构，确保恢复时能还原结构化卡片。
 */
export function buildPersistedStandardMessagePayload(message: StandardMessage): PersistedStandardMessagePayload {
  const content = Array.isArray(message.blocks)
    ? message.blocks
        .filter((block) => block.type === 'text')
        .map((block) => block.content || '')
        .join('\n')
    : '';
  const metadata = sanitizePersistedMessageMetadata({
    role: resolvePersistedRole(message),
    type: message.type,
    content,
    blocks: Array.isArray(message.blocks) ? message.blocks : undefined,
    metadata: message.metadata && typeof message.metadata === 'object'
      ? { ...message.metadata } as Record<string, unknown>
      : undefined,
  });

  return {
    role: resolvePersistedRole(message),
    content,
    type: typeof message.type === 'string' ? message.type : 'task_card',
    category: message.category,
    visibility: message.visibility,
    timestamp: typeof message.timestamp === 'number' && Number.isFinite(message.timestamp)
      ? message.timestamp
      : Date.now(),
    updatedAt: typeof message.updatedAt === 'number' && Number.isFinite(message.updatedAt)
      ? message.updatedAt
      : (typeof message.timestamp === 'number' && Number.isFinite(message.timestamp) ? message.timestamp : Date.now()),
    source: typeof message.source === 'string' ? message.source : 'orchestrator',
    agent: message.agent,
    interaction: message.interaction,
    metadata,
    blocks: Array.isArray(message.blocks) && message.blocks.length > 0
      ? message.blocks
      : undefined,
  };
}
