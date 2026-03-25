import type { ContentBlock } from '../protocol/message-protocol';
import type { AgentType } from '../types/agent-types';
import { isTimelineWorkerLifecycleMessageType } from './timeline-presentation';

export interface TimelineFragmentMessage {
  id: string;
  role: 'user' | 'assistant' | 'system';
  content: string;
  agent?: AgentType;
  source?: string;
  timestamp: number;
  updatedAt?: number;
  attachments?: { name: string; path: string; mimeType?: string }[];
  images?: Array<{ dataUrl: string }>;
  blocks?: ContentBlock[];
  type?: string;
  category?: string;
  visibility?: string;
  noticeType?: string;
  isStreaming?: boolean;
  isComplete?: boolean;
  interaction?: unknown;
  metadata?: Record<string, unknown>;
}

function cloneSerializable<T>(value: T): T {
  return structuredClone(value);
}

function cloneMetadata(metadata: Record<string, unknown> | undefined): Record<string, unknown> | undefined {
  return metadata ? cloneSerializable(metadata) : undefined;
}

function resolveMetadata(message: Pick<TimelineFragmentMessage, 'metadata'>): Record<string, unknown> | undefined {
  return message.metadata && typeof message.metadata === 'object' && !Array.isArray(message.metadata)
    ? message.metadata
    : undefined;
}

function resolvePositiveTimestamp(value: unknown): number {
  if (typeof value !== 'number' || !Number.isFinite(value) || value <= 0) {
    return 0;
  }
  return Math.floor(value);
}

function resolveCardId(message: TimelineFragmentMessage): string {
  const metadata = resolveMetadata(message);
  const rawCardId = typeof metadata?.cardId === 'string' ? metadata.cardId.trim() : '';
  return rawCardId || message.id;
}

function sanitizeFragmentToken(rawValue: string | undefined, fallback: string): string {
  const normalized = (rawValue || '')
    .trim()
    .replace(/[^a-zA-Z0-9_-]+/g, '_')
    .replace(/^_+|_+$/g, '');
  return normalized || fallback;
}

function buildFragmentId(baseMessageId: string, kind: string, token: string): string {
  return `${baseMessageId}::${kind}:${token}`;
}

function resolveThinkingBlockId(block: ContentBlock | undefined): string | undefined {
  const candidate = block as unknown as {
    id?: unknown;
    blockId?: unknown;
    thinking?: { blockId?: unknown };
  } | undefined;
  if (!candidate) {
    return undefined;
  }
  if (typeof candidate.blockId === 'string' && candidate.blockId.trim().length > 0) {
    return candidate.blockId;
  }
  if (typeof candidate.id === 'string' && candidate.id.trim().length > 0) {
    return candidate.id;
  }
  if (typeof candidate.thinking?.blockId === 'string' && candidate.thinking.blockId.trim().length > 0) {
    return candidate.thinking.blockId;
  }
  return undefined;
}

function resolveToolBlockName(block: ContentBlock | undefined): string {
  const candidate = block as unknown as {
    toolName?: unknown;
    toolCall?: { name?: unknown };
  } | undefined;
  if (!candidate) {
    return '';
  }
  if (typeof candidate.toolName === 'string' && candidate.toolName.trim().length > 0) {
    return candidate.toolName.trim();
  }
  if (typeof candidate.toolCall?.name === 'string' && candidate.toolCall.name.trim().length > 0) {
    return candidate.toolCall.name.trim();
  }
  return '';
}

function resolveToolBlockId(block: ContentBlock | undefined): string | undefined {
  const candidate = block as unknown as {
    toolId?: unknown;
    toolCall?: { id?: unknown; name?: unknown };
  } | undefined;
  if (!candidate) {
    return undefined;
  }
  if (typeof candidate.toolId === 'string' && candidate.toolId.trim().length > 0) {
    return candidate.toolId;
  }
  if (typeof candidate.toolCall?.id === 'string' && candidate.toolCall.id.trim().length > 0) {
    return candidate.toolCall.id;
  }
  if (typeof candidate.toolCall?.name === 'string' && candidate.toolCall.name.trim().length > 0) {
    return candidate.toolCall.name;
  }
  return undefined;
}

function resolveFileChangePayload(block: ContentBlock | undefined): { filePath?: string; diff?: string } {
  const candidate = block as unknown as {
    filePath?: unknown;
    diff?: unknown;
    fileChange?: { filePath?: unknown; diff?: unknown };
  } | undefined;
  if (!candidate) {
    return {};
  }
  return {
    ...(typeof candidate.filePath === 'string' && candidate.filePath.trim().length > 0
      ? { filePath: candidate.filePath }
      : {}),
    ...(typeof candidate.diff === 'string' && candidate.diff.trim().length > 0
      ? { diff: candidate.diff }
      : {}),
    ...((typeof candidate.fileChange?.filePath === 'string' && candidate.fileChange.filePath.trim().length > 0)
      ? { filePath: candidate.fileChange.filePath }
      : {}),
    ...((typeof candidate.fileChange?.diff === 'string' && candidate.fileChange.diff.trim().length > 0)
      ? { diff: candidate.fileChange.diff }
      : {}),
  };
}

function resolvePlanPayload(block: ContentBlock | undefined): { goal?: string; analysis?: string } {
  const candidate = block as unknown as {
    goal?: unknown;
    analysis?: unknown;
    plan?: { goal?: unknown; analysis?: unknown };
  } | undefined;
  if (!candidate) {
    return {};
  }
  return {
    ...(typeof candidate.goal === 'string' && candidate.goal.trim().length > 0
      ? { goal: candidate.goal }
      : {}),
    ...(typeof candidate.analysis === 'string' && candidate.analysis.trim().length > 0
      ? { analysis: candidate.analysis }
      : {}),
    ...((typeof candidate.plan?.goal === 'string' && candidate.plan.goal.trim().length > 0)
      ? { goal: candidate.plan.goal }
      : {}),
    ...((typeof candidate.plan?.analysis === 'string' && candidate.plan.analysis.trim().length > 0)
      ? { analysis: candidate.plan.analysis }
      : {}),
  };
}

function blockHasRenderablePayload(block: ContentBlock | undefined): boolean {
  if (!block) {
    return false;
  }
  switch (block.type) {
    case 'thinking':
    case 'tool_call':
      return true;
    case 'text':
    case 'code':
      return typeof block.content === 'string' && block.content.trim().length > 0;
    case 'file_change': {
      const payload = resolveFileChangePayload(block);
      return Boolean(payload.filePath?.trim() || payload.diff?.trim());
    }
    case 'plan': {
      const payload = resolvePlanPayload(block);
      return Boolean(payload.goal?.trim() || payload.analysis?.trim());
    }
    default:
      return false;
  }
}

function shouldFragmentTimelineMessage(message: TimelineFragmentMessage): boolean {
  if (message.role === 'user' || message.role === 'system') {
    return false;
  }
  if (
    message.type === 'user_input'
    || message.type === 'system-notice'
    || message.type === 'interaction'
    || message.type === 'progress'
    || message.type === 'result'
    || isTimelineWorkerLifecycleMessageType(message.type)
  ) {
    return false;
  }
  const blocks = Array.isArray(message.blocks) ? message.blocks : [];
  if (blocks.length === 0) {
    return false;
  }
  const renderableBlocks = blocks.filter(blockHasRenderablePayload);
  return renderableBlocks.length > 1;
}

function buildFragmentMessage(
  message: TimelineFragmentMessage,
  fragmentId: string,
  fragmentType: string,
  block: ContentBlock,
  blockSeq: number,
  content: string,
  timelineAnchorTimestampOverride?: number,
): TimelineFragmentMessage {
  const baseMetadata = cloneMetadata(resolveMetadata(message)) || {};
  const cardId = resolveCardId(message);
  const nextTimelineAnchorTimestamp = resolvePositiveTimestamp(timelineAnchorTimestampOverride);
  return {
    ...message,
    id: fragmentId,
    content,
    type: fragmentType,
    blocks: [cloneSerializable(block)],
    metadata: {
      ...baseMetadata,
      cardId,
      originMessageId: message.id,
      originMessageType: message.type,
      blockSeq,
      timelineFragmentType: fragmentType,
      ...(nextTimelineAnchorTimestamp > 0 ? { timelineAnchorTimestamp: nextTimelineAnchorTimestamp } : {}),
    },
  };
}

function resolvePostWorkerWaitAnchorTimestamp(
  message: TimelineFragmentMessage,
): {
  waitBlockIndex: number;
  anchorTimestamp?: number;
} {
  const blocks = Array.isArray(message.blocks) ? message.blocks : [];
  let waitBlockIndex = -1;
  for (let index = blocks.length - 1; index >= 0; index -= 1) {
    const block = blocks[index];
    if (block?.type !== 'tool_call') {
      continue;
    }
    const toolName = resolveToolBlockName(block);
    if (toolName === 'worker_wait') {
      waitBlockIndex = index;
      break;
    }
  }

  if (waitBlockIndex < 0) {
    return { waitBlockIndex: -1 };
  }

  const metadata = resolveMetadata(message);
  const baseAnchorTimestamp = resolvePositiveTimestamp(metadata?.timelineAnchorTimestamp);
  const resolvedAnchorTimestamp = resolvePositiveTimestamp(message.updatedAt) || resolvePositiveTimestamp(message.timestamp);
  if (resolvedAnchorTimestamp <= 0 || (baseAnchorTimestamp > 0 && resolvedAnchorTimestamp <= baseAnchorTimestamp)) {
    return { waitBlockIndex };
  }

  return {
    waitBlockIndex,
    anchorTimestamp: resolvedAnchorTimestamp,
  };
}

export function expandRenderableTimelineMessages<T extends TimelineFragmentMessage>(message: T): T[] {
  if (!message?.id || typeof message.id !== 'string') {
    return [];
  }

  if (!shouldFragmentTimelineMessage(message)) {
    return [{ ...message, metadata: cloneMetadata(resolveMetadata(message)) } as T];
  }

  const blocks = Array.isArray(message.blocks) ? message.blocks : [];
  const postWorkerWaitAnchor = resolvePostWorkerWaitAnchorTimestamp(message);
  const fragments: T[] = [];
  let textIndex = 0;
  let thinkingIndex = 0;
  let toolIndex = 0;
  let contentIndex = 0;

  for (let index = 0; index < blocks.length; index += 1) {
    const block = blocks[index];
    if (!blockHasRenderablePayload(block)) {
      continue;
    }

    if (block.type === 'thinking') {
      thinkingIndex += 1;
      const blockId = resolveThinkingBlockId(block);
      const token = sanitizeFragmentToken(blockId, `thinking_${thinkingIndex}`);
      const timelineAnchorTimestampOverride = (
        postWorkerWaitAnchor.anchorTimestamp
        && index > postWorkerWaitAnchor.waitBlockIndex
      )
        ? postWorkerWaitAnchor.anchorTimestamp
        : undefined;
      fragments.push(buildFragmentMessage(
        message,
        buildFragmentId(message.id, 'thinking', token),
        'thinking',
        block,
        index,
        typeof block.content === 'string' ? block.content : '',
        timelineAnchorTimestampOverride,
      ) as T);
      continue;
    }

    if (block.type === 'tool_call') {
      toolIndex += 1;
      const toolToken = sanitizeFragmentToken(
        resolveToolBlockId(block) || resolveToolBlockName(block) || undefined,
        `tool_${toolIndex}`,
      );
      const timelineAnchorTimestampOverride = (
        postWorkerWaitAnchor.anchorTimestamp
        && index > postWorkerWaitAnchor.waitBlockIndex
      )
        ? postWorkerWaitAnchor.anchorTimestamp
        : undefined;
      fragments.push(buildFragmentMessage(
        message,
        buildFragmentId(message.id, 'tool', toolToken),
        'tool_call',
        block,
        index,
        '',
        timelineAnchorTimestampOverride,
      ) as T);
      continue;
    }

    textIndex += 1;
    contentIndex += 1;
    const fragmentType = block.type === 'plan' ? 'plan' : 'text';
    const token = sanitizeFragmentToken(
      undefined,
      `${block.type}_${contentIndex}`,
    );
    const content = (() => {
      switch (block.type) {
        case 'text':
        case 'code':
          return typeof block.content === 'string' ? block.content : (message.content || '');
        case 'file_change': {
          const payload = resolveFileChangePayload(block);
          return payload.filePath || payload.diff || message.content || '';
        }
        case 'plan': {
          const payload = resolvePlanPayload(block);
          return payload.goal || payload.analysis || message.content || '';
        }
        default:
          return message.content || '';
      }
    })();
    fragments.push(buildFragmentMessage(
      message,
      buildFragmentId(message.id, 'content', token),
      fragmentType,
      block,
      index,
      content,
      postWorkerWaitAnchor.anchorTimestamp && index > postWorkerWaitAnchor.waitBlockIndex
        ? postWorkerWaitAnchor.anchorTimestamp
        : undefined,
    ) as T);
  }

  if (fragments.length === 0) {
    return [{ ...message, metadata: cloneMetadata(resolveMetadata(message)) } as T];
  }

  return fragments;
}
