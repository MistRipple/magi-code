import type { Message, TimelineExecutionItem } from '../types/message';
import {
  compareTimelineSemanticOrder,
  resolveTimelineBlockSeqFromMetadata,
  resolveTimelineItemSeqFromMetadata,
  resolveTimelineLaneSeqFromMetadata,
  resolveTimelineTurnOrderSeqFromMetadata,
} from '../shared/timeline-ordering';

function compareExecutionItems(left: TimelineExecutionItem, right: TimelineExecutionItem): number {
  const semanticOrder = compareTimelineSemanticOrder(
    {
      turnOrderSeq: resolveTimelineTurnOrderSeqFromMetadata(left.message.metadata),
      itemSeq: resolveTimelineItemSeqFromMetadata(left.message.metadata) || left.itemOrder,
      laneSeq: resolveTimelineLaneSeqFromMetadata(left.message.metadata),
      blockSeq: resolveTimelineBlockSeqFromMetadata(left.message.metadata),
      displayOrder: left.itemOrder || 0,
    },
    {
      turnOrderSeq: resolveTimelineTurnOrderSeqFromMetadata(right.message.metadata),
      itemSeq: resolveTimelineItemSeqFromMetadata(right.message.metadata) || right.itemOrder,
      laneSeq: resolveTimelineLaneSeqFromMetadata(right.message.metadata),
      blockSeq: resolveTimelineBlockSeqFromMetadata(right.message.metadata),
      displayOrder: right.itemOrder || 0,
    },
  );
  if (semanticOrder !== 0) {
    return semanticOrder;
  }
  return left.itemId.localeCompare(right.itemId);
}

function readPositiveMetadataNumber(metadata: Record<string, unknown> | undefined, key: string): number {
  const raw = metadata?.[key];
  if (typeof raw !== 'number' || !Number.isFinite(raw)) {
    return 0;
  }
  const normalized = Math.floor(raw);
  return normalized > 0 ? normalized : 0;
}

function preserveStableTurnOrderFact(existingMessage: Message, nextMessage: Message): Message {
  const existingTurnOrderSeq = readPositiveMetadataNumber(existingMessage.metadata, 'turnOrderSeq');
  const nextTurnOrderSeq = readPositiveMetadataNumber(nextMessage.metadata, 'turnOrderSeq');
  if (existingTurnOrderSeq <= 0 || nextTurnOrderSeq > 0) {
    return nextMessage;
  }
  return {
    ...nextMessage,
    metadata: {
      ...(nextMessage.metadata || {}),
      turnOrderSeq: existingTurnOrderSeq,
    },
  };
}

export function mergeFragmentExecutionItems(params: {
  existingItems: TimelineExecutionItem[] | undefined;
  nextItems: TimelineExecutionItem[];
}): TimelineExecutionItem[] {
  const existingItems = Array.isArray(params.existingItems) ? params.existingItems : [];
  const nextItems = Array.isArray(params.nextItems) ? params.nextItems : [];
  if (existingItems.length === 0) {
    return nextItems.slice().sort(compareExecutionItems);
  }
  if (nextItems.length === 0) {
    return existingItems.slice().sort(compareExecutionItems);
  }

  const existingById = new Map(existingItems.map((item) => [item.itemId, item]));
  const merged: TimelineExecutionItem[] = [];
  const seen = new Set<string>();

  for (const nextItem of nextItems) {
    const existingItem = existingById.get(nextItem.itemId);
    if (!existingItem) {
      merged.push(nextItem);
      seen.add(nextItem.itemId);
      continue;
    }

    const nextBlocks = Array.isArray(nextItem.message.blocks) && nextItem.message.blocks.length > 0
      ? nextItem.message.blocks
      : existingItem.message.blocks;
    const mergedMessage: Message = preserveStableTurnOrderFact(existingItem.message, {
      ...existingItem.message,
      ...nextItem.message,
      blocks: nextBlocks,
      content: nextItem.message.content || existingItem.message.content,
    });

    merged.push({
      ...existingItem,
      ...nextItem,
      anchorEventSeq: existingItem.anchorEventSeq,
      latestEventSeq: Math.max(existingItem.latestEventSeq, nextItem.latestEventSeq),
      cardStreamSeq: existingItem.cardStreamSeq || nextItem.cardStreamSeq,
      timestamp: Math.min(existingItem.timestamp, nextItem.timestamp),
      workerTabs: Array.from(new Set([...(existingItem.workerTabs || []), ...(nextItem.workerTabs || [])])),
      messageIds: Array.from(new Set([...(existingItem.messageIds || []), ...(nextItem.messageIds || [])])),
      message: mergedMessage,
    });
    seen.add(nextItem.itemId);
  }

  for (const existingItem of existingItems) {
    if (!seen.has(existingItem.itemId)) {
      merged.push(existingItem);
    }
  }

  return merged.sort(compareExecutionItems);
}
