import type { ContentBlock, ToolCallStatus } from '../types/message';
import { ensureArray } from './utils';

export function mergeCompleteBlocksForFinalization(
  existingBlocks: ContentBlock[] | undefined,
  completeBlocks: ContentBlock[] | undefined,
  baseBlocks: ContentBlock[] | undefined,
): ContentBlock[] | undefined {
  const safeExisting = ensureArray(existingBlocks).filter((block): block is ContentBlock => !!block && typeof block === 'object' && 'type' in block);
  const safeComplete = ensureArray(completeBlocks).filter((block): block is ContentBlock => !!block && typeof block === 'object' && 'type' in block);

  if (safeExisting.length > 0 && safeComplete.length > 0) {
    const completeTextCodeBlocks = safeComplete.filter(isRenderableTextOrCodeBlock);
    const completeHasTextCode = completeTextCodeBlocks.length > 0;
    let insertedCompleteTextCode = false;
    const textMergedBlocks = completeHasTextCode
      ? safeExisting.flatMap((block) => {
          if (block.type !== 'text' && block.type !== 'code') {
            return [block];
          }
          if (insertedCompleteTextCode) {
            return [];
          }
          insertedCompleteTextCode = true;
          return completeTextCodeBlocks;
      })
      : safeExisting;
    if (completeHasTextCode && !insertedCompleteTextCode) {
      textMergedBlocks.push(...completeTextCodeBlocks);
    }

    const baseBlocks = mergeDuplicateToolBlocks(textMergedBlocks);
    const toolIndexById = indexToolBlocks(baseBlocks);
    const existingThinkingIds = new Set(
      baseBlocks
        .filter(block => block.type === 'thinking' && (block.id || block.thinking?.blockId))
        .map(block => block.id || block.thinking!.blockId),
    );

    const supplements: ContentBlock[] = [];
    const existingStructuredFingerprints = new Set(
      baseBlocks
        .map((block) => buildStructuredBlockFingerprint(block))
        .filter((fingerprint): fingerprint is string => Boolean(fingerprint)),
    );
    for (const block of safeComplete) {
      const toolBlockId = resolveToolBlockId(block);
      if (toolBlockId) {
        const existingToolIndex = toolIndexById.get(toolBlockId);
        if (existingToolIndex !== undefined) {
          baseBlocks[existingToolIndex] = mergeToolBlocks(baseBlocks[existingToolIndex], block);
        } else {
          toolIndexById.set(toolBlockId, baseBlocks.length);
          baseBlocks.push(normalizeToolBlock(block));
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

    return supplements.length > 0 ? [...baseBlocks, ...supplements] : baseBlocks;
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

function isRenderableTextOrCodeBlock(block: ContentBlock): boolean {
  return (block.type === 'text' || block.type === 'code')
    && typeof block.content === 'string'
    && block.content.length > 0;
}

function resolveToolBlockId(block: ContentBlock): string {
  if (block.type !== 'tool_call' && block.type !== 'tool_result') {
    return '';
  }
  return typeof block.toolCall?.id === 'string' ? block.toolCall.id.trim() : '';
}

function indexToolBlocks(blocks: ContentBlock[]): Map<string, number> {
  const indexById = new Map<string, number>();
  blocks.forEach((block, index) => {
    const toolBlockId = resolveToolBlockId(block);
    if (toolBlockId) {
      indexById.set(toolBlockId, index);
    }
  });
  return indexById;
}

function mergeDuplicateToolBlocks(blocks: ContentBlock[]): ContentBlock[] {
  const merged: ContentBlock[] = [];
  const toolIndexById = new Map<string, number>();
  for (const block of blocks) {
    const toolBlockId = resolveToolBlockId(block);
    if (!toolBlockId) {
      merged.push(block);
      continue;
    }
    const existingIndex = toolIndexById.get(toolBlockId);
    if (existingIndex === undefined) {
      toolIndexById.set(toolBlockId, merged.length);
      merged.push(normalizeToolBlock(block));
      continue;
    }
    merged[existingIndex] = mergeToolBlocks(merged[existingIndex], block);
  }
  return merged;
}

function normalizeToolBlock(block: ContentBlock): ContentBlock {
  if (block.type !== 'tool_result') {
    return block;
  }
  return {
    ...block,
    type: 'tool_call',
  };
}

function toolStatusRank(status: ToolCallStatus | undefined): number {
  switch (status) {
    case 'success':
    case 'error':
      return 30;
    case 'running':
      return 20;
    case 'pending':
      return 10;
    default:
      return 0;
  }
}

function mergeToolStatus(
  existingStatus: ToolCallStatus | undefined,
  incomingStatus: ToolCallStatus | undefined,
): ToolCallStatus {
  const existingRank = toolStatusRank(existingStatus);
  const incomingRank = toolStatusRank(incomingStatus);
  if (incomingRank >= existingRank && incomingStatus) {
    return incomingStatus;
  }
  return existingStatus || incomingStatus || 'running';
}

function hasToolArguments(argumentsValue: Record<string, unknown> | undefined): boolean {
  return Boolean(argumentsValue && Object.keys(argumentsValue).length > 0);
}

function mergeToolName(existingName: string | undefined, incomingName: string | undefined): string {
  const normalizedIncoming = typeof incomingName === 'string' ? incomingName.trim() : '';
  if (normalizedIncoming && normalizedIncoming !== 'tool_result') {
    return normalizedIncoming;
  }
  const normalizedExisting = typeof existingName === 'string' ? existingName.trim() : '';
  return normalizedExisting || normalizedIncoming || 'Tool';
}

function mergeToolBlocks(existingBlock: ContentBlock, incomingBlock: ContentBlock): ContentBlock {
  const existingCall = existingBlock.toolCall;
  const incomingCall = incomingBlock.toolCall;
  const mergedStatus = mergeToolStatus(existingCall?.status, incomingCall?.status);
  return {
    ...normalizeToolBlock(existingBlock),
    type: 'tool_call',
    content: incomingBlock.content || existingBlock.content || '',
    toolCall: {
      ...existingCall,
      ...incomingCall,
      id: incomingCall?.id || existingCall?.id || '',
      name: mergeToolName(existingCall?.name, incomingCall?.name),
      status: mergedStatus,
      arguments: hasToolArguments(incomingCall?.arguments)
        ? incomingCall!.arguments
        : (existingCall?.arguments || incomingCall?.arguments || {}),
      result: incomingCall?.result ?? existingCall?.result ?? (incomingBlock.content || undefined),
      error: incomingCall?.error ?? existingCall?.error,
      standardized: incomingCall?.standardized ?? existingCall?.standardized,
      durationMs: incomingCall?.durationMs ?? existingCall?.durationMs,
      startTime: existingCall?.startTime ?? incomingCall?.startTime,
      endTime: incomingCall?.endTime ?? existingCall?.endTime,
    },
  };
}

function buildStructuredBlockFingerprint(block: ContentBlock): string | null {
  if (!block || typeof block !== 'object') {
    return null;
  }
  switch (block.type) {
    case 'tool_call':
      return block.toolCall?.id ? `tool_call:${block.toolCall.id}` : null;
    case 'tool_result':
      return block.toolCall?.id ? `tool_result:${block.toolCall.id}:${block.toolCall.error || block.toolCall.result || block.content || ''}` : null;
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
    case 'dispatch_group': {
      const blockId = typeof block.blockId === 'string' ? block.blockId : '';
      const waveId = typeof block.dispatchWaveId === 'string' ? block.dispatchWaveId : '';
      return `dispatch_group:${blockId}:${waveId}`;
    }
    default:
      return null;
  }
}
