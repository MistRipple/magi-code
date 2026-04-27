import type { Message, TimelineExecutionItem } from '../types/message';
import { mergeCompleteBlocksForFinalization } from './streaming-complete-merge';

function compareExecutionItems(left: TimelineExecutionItem, right: TimelineExecutionItem): number {
  if (left.anchorEventSeq !== right.anchorEventSeq) {
    return left.anchorEventSeq - right.anchorEventSeq;
  }
  return left.itemId.localeCompare(right.itemId);
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

  return merged.sort(compareExecutionItems);
}
