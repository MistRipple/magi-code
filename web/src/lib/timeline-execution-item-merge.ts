import {
  compareTimelineSemanticOrder,
  resolveTimelineBlockSeqFromMetadata,
  resolveTimelineSemanticMessageType,
} from '../shared/timeline-ordering';
import { resolveTimelinePrimaryToolCallName } from '../shared/timeline-presentation';
import type { Message, TimelineExecutionItem } from '../types/message';
import { mergeCompleteBlocksForFinalization } from './streaming-complete-merge';

function resolveMessageBlockSeq(message: Pick<Message, 'metadata'> | undefined): number {
  const metadata = message?.metadata && typeof message.metadata === 'object'
    ? message.metadata as Record<string, unknown>
    : undefined;
  return resolveTimelineBlockSeqFromMetadata(metadata);
}

function compareExecutionItems(left: TimelineExecutionItem, right: TimelineExecutionItem): number {
  return compareTimelineSemanticOrder(
    {
      timestamp: left.timestamp,
      stableId: left.itemId,
      itemOrder: left.itemOrder,
      messageType: resolveTimelineSemanticMessageType(left.message.type, left.message.metadata as Record<string, unknown> | undefined),
      primaryToolCallName: resolveTimelinePrimaryToolCallName(left.message.blocks),
      anchorEventSeq: left.anchorEventSeq,
      blockSeq: resolveMessageBlockSeq(left.message),
    },
    {
      timestamp: right.timestamp,
      stableId: right.itemId,
      itemOrder: right.itemOrder,
      messageType: resolveTimelineSemanticMessageType(right.message.type, right.message.metadata as Record<string, unknown> | undefined),
      primaryToolCallName: resolveTimelinePrimaryToolCallName(right.message.blocks),
      anchorEventSeq: right.anchorEventSeq,
      blockSeq: resolveMessageBlockSeq(right.message),
    },
  );
}

export function mergeFragmentExecutionItems(params: {
  existingItems: TimelineExecutionItem[] | undefined;
  nextItems: TimelineExecutionItem[];
}): TimelineExecutionItem[] {
  const existingItems = Array.isArray(params.existingItems) ? params.existingItems : [];
  const nextItems = Array.isArray(params.nextItems) ? params.nextItems : [];
  if (existingItems.length === 0) {
    return nextItems.map((item, index) => ({ ...item, itemOrder: index + 1 }));
  }
  if (nextItems.length === 0) {
    return existingItems.map((item, index) => ({ ...item, itemOrder: index + 1 }));
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

    const mergedBlocks = mergeCompleteBlocksForFinalization(
      existingItem.message.blocks,
      nextItem.message.blocks,
      nextItem.message.blocks,
    );
    const mergedMessage: Message = {
      ...existingItem.message,
      ...nextItem.message,
      ...(mergedBlocks ? { blocks: mergedBlocks } : {}),
      content: nextItem.message.content || existingItem.message.content,
    };

    merged.push({
      ...existingItem,
      ...nextItem,
      anchorEventSeq: existingItem.anchorEventSeq,
      latestEventSeq: Math.max(existingItem.latestEventSeq, nextItem.latestEventSeq),
      cardStreamSeq: Math.max(existingItem.cardStreamSeq, nextItem.cardStreamSeq),
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

  return merged
    .sort(compareExecutionItems)
    .map((item, index) => ({ ...item, itemOrder: index + 1 }));
}
