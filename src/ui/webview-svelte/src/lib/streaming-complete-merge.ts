import type { ContentBlock } from '../types/message';
import { ensureArray } from './utils';

export function mergeCompleteBlocksForFinalization(
  existingBlocks: ContentBlock[] | undefined,
  completeBlocks: ContentBlock[] | undefined,
  baseBlocks: ContentBlock[] | undefined,
): ContentBlock[] | undefined {
  const safeExisting = ensureArray(existingBlocks).filter((block): block is ContentBlock => !!block && typeof block === 'object' && 'type' in block);
  const safeComplete = ensureArray(completeBlocks).filter((block): block is ContentBlock => !!block && typeof block === 'object' && 'type' in block);

  if (safeExisting.length > 0 && safeComplete.length > 0) {
    const existingToolIds = new Set(
      safeExisting
        .filter(block => block.type === 'tool_call' && block.toolCall?.id)
        .map(block => block.toolCall!.id),
    );
    const existingThinkingIds = new Set(
      safeExisting
        .filter(block => block.type === 'thinking' && (block.id || block.thinking?.blockId))
        .map(block => block.id || block.thinking!.blockId),
    );

    const supplements: ContentBlock[] = [];
    const existingStructuredFingerprints = new Set(
      safeExisting
        .map((block) => buildStructuredBlockFingerprint(block))
        .filter((fingerprint): fingerprint is string => Boolean(fingerprint)),
    );
    for (const block of safeComplete) {
      if (block.type === 'tool_call' && block.toolCall?.id) {
        if (!existingToolIds.has(block.toolCall.id)) {
          supplements.push(block);
        }
      } else if (block.type === 'thinking') {
        const blockId = block.id || block.thinking?.blockId;
        if (blockId && !existingThinkingIds.has(blockId)) {
          supplements.push(block);
        }
      } else if (block.type !== 'text' && block.type !== 'code') {
        const fingerprint = buildStructuredBlockFingerprint(block);
        if (!fingerprint || existingStructuredFingerprints.has(fingerprint)) {
          continue;
        }
        existingStructuredFingerprints.add(fingerprint);
        supplements.push(block);
      }
    }

    return supplements.length > 0 ? [...safeExisting, ...supplements] : safeExisting;
  }

  if (safeExisting.length > 0) {
    return safeExisting;
  }
  if (safeComplete.length > 0) {
    return safeComplete;
  }

  const safeBase = ensureArray(baseBlocks).filter((block): block is ContentBlock => !!block && typeof block === 'object' && 'type' in block);
  return safeBase.length > 0 ? safeBase : undefined;
}

function buildStructuredBlockFingerprint(block: ContentBlock): string | null {
  if (!block || typeof block !== 'object') {
    return null;
  }
  switch (block.type) {
    case 'tool_call':
      return block.toolCall?.id ? `tool_call:${block.toolCall.id}` : null;
    case 'thinking': {
      const blockId = block.id || block.thinking?.blockId;
      return blockId ? `thinking:${blockId}` : null;
    }
    case 'file_change': {
      const payload = block.fileChange;
      if (!payload) return null;
      return `file_change:${payload.filePath}:${payload.changeType}:${payload.diff || ''}`;
    }
    case 'plan': {
      const payload = block.plan;
      if (!payload) return null;
      return `plan:${payload.goal || ''}:${payload.analysis || ''}:${(payload.acceptanceCriteria || []).join('|')}:${(payload.constraints || []).join('|')}`;
    }
    default:
      return null;
  }
}
