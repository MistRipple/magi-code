import type { ContentBlock } from '../protocol/message-protocol';
import {
  compareTimelineSemanticOrder,
  resolveTimelineBlockSeqFromMetadata,
} from '../shared/timeline-ordering';
import { resolveTimelinePrimaryToolCallName } from '../shared/timeline-presentation';
import { sanitizePersistedMessageMetadata } from './standard-message-session-persistence';
import type { TimelineRecord } from './timeline-record';
import type { SessionTimelineProjectionMessage } from './session-timeline-projection';
import type { SessionMessage } from './unified-session-manager';

function cloneSerializable<T>(value: T): T {
  return structuredClone(value);
}

function cloneBlocks(blocks: ContentBlock[] | undefined): ContentBlock[] | undefined {
  return Array.isArray(blocks) && blocks.length > 0
    ? cloneSerializable(blocks)
    : undefined;
}

function cloneNonEmptyArray<T>(value: T[] | undefined): T[] | undefined {
  return Array.isArray(value) && value.length > 0
    ? cloneSerializable(value)
    : undefined;
}

function resolveRecordMetadata(record: Pick<TimelineRecord, 'metadata'>): Record<string, unknown> | undefined {
  return record.metadata && typeof record.metadata === 'object' && !Array.isArray(record.metadata)
    ? record.metadata
    : undefined;
}

function resolveSourceMessageId(record: TimelineRecord): string {
  const metadata = resolveRecordMetadata(record);
  const originMessageId = typeof metadata?.originMessageId === 'string' ? metadata.originMessageId.trim() : '';
  return originMessageId || record.messageId;
}

function resolveSourceMessageType(records: TimelineRecord[]): string | undefined {
  for (const record of records) {
    const metadata = resolveRecordMetadata(record);
    const originMessageType = typeof metadata?.originMessageType === 'string' ? metadata.originMessageType.trim() : '';
    if (originMessageType) {
      return originMessageType;
    }
  }
  return records.find((record) => typeof record.messageType === 'string' && record.messageType.trim())?.messageType;
}

function stripFragmentMetadata(metadata: Record<string, unknown> | undefined): Record<string, unknown> | undefined {
  if (!metadata) {
    return undefined;
  }
  const next = { ...metadata };
  delete next.originMessageId;
  delete next.originMessageType;
  delete next.blockSeq;
  delete next.timelineFragmentType;
  return Object.keys(next).length > 0 ? next : undefined;
}

function resolvePositiveMetadataNumber(
  metadata: Record<string, unknown> | undefined,
  key: string,
): number {
  const rawValue = metadata?.[key];
  if (typeof rawValue !== 'number' || !Number.isFinite(rawValue) || rawValue <= 0) {
    return 0;
  }
  return Math.floor(rawValue);
}

function mergeSourceMessageMetadata(
  groupedRecords: TimelineRecord[],
  metadata: Record<string, unknown> | undefined,
): Record<string, unknown> | undefined {
  const next = metadata ? cloneSerializable(metadata) : {};

  const earliestAnchorTimestamp = groupedRecords.reduce((minValue, record) => {
    const candidate = resolvePositiveMetadataNumber(resolveRecordMetadata(record), 'timelineAnchorTimestamp');
    if (candidate <= 0) {
      return minValue;
    }
    return minValue <= 0 ? candidate : Math.min(minValue, candidate);
  }, 0);
  if (earliestAnchorTimestamp > 0) {
    next.timelineAnchorTimestamp = earliestAnchorTimestamp;
  }

  const maxCardStreamSeq = groupedRecords.reduce((maxValue, record) => (
    Math.max(maxValue, resolvePositiveMetadataNumber(resolveRecordMetadata(record), 'cardStreamSeq'))
  ), 0);
  if (maxCardStreamSeq > 0) {
    next.cardStreamSeq = maxCardStreamSeq;
  }

  const maxFinalStreamSeq = groupedRecords.reduce((maxValue, record) => (
    Math.max(maxValue, resolvePositiveMetadataNumber(resolveRecordMetadata(record), 'finalStreamSeq'))
  ), 0);
  if (maxFinalStreamSeq > 0) {
    next.finalStreamSeq = maxFinalStreamSeq;
  }

  return Object.keys(next).length > 0 ? next : undefined;
}

function buildContentFromBlocks(blocks: ContentBlock[] | undefined): string {
  if (!Array.isArray(blocks) || blocks.length === 0) {
    return '';
  }
  return blocks
    .filter((block) => block.type === 'text')
    .map((block) => block.content || '')
    .join('\n');
}

function resolveSourceMessageContent(
  groupedRecords: TimelineRecord[],
  mergedBlocks: ContentBlock[] | undefined,
  firstRecord: TimelineRecord,
  lastRecord: TimelineRecord,
): string {
  const blockContent = buildContentFromBlocks(mergedBlocks);
  if (groupedRecords.length > 1) {
    return blockContent || lastRecord.content || firstRecord.content;
  }
  return lastRecord.content || blockContent || firstRecord.content;
}

function pickFirstNonEmptyArray<T>(
  records: TimelineRecord[],
  selector: (record: TimelineRecord) => T[] | undefined,
): T[] | undefined {
  for (const record of records) {
    const value = selector(record);
    if (Array.isArray(value) && value.length > 0) {
      return cloneSerializable(value);
    }
  }
  return undefined;
}

function compareTimelineRecordOrder(left: TimelineRecord, right: TimelineRecord): number {
  return compareTimelineSemanticOrder(
    {
      timestamp: left.anchorTimestamp || left.createdAt || left.messageTimestamp,
      stableId: left.stableKey,
      messageType: left.messageType,
      primaryToolCallName: resolveTimelinePrimaryToolCallName(left.blocks),
      anchorEventSeq: left.anchorEventSeq,
      blockSeq: resolveTimelineBlockSeqFromMetadata(resolveRecordMetadata(left)),
      cardStreamSeq: left.cardStreamSeq,
    },
    {
      timestamp: right.anchorTimestamp || right.createdAt || right.messageTimestamp,
      stableId: right.stableKey,
      messageType: right.messageType,
      primaryToolCallName: resolveTimelinePrimaryToolCallName(right.blocks),
      anchorEventSeq: right.anchorEventSeq,
      blockSeq: resolveTimelineBlockSeqFromMetadata(resolveRecordMetadata(right)),
      cardStreamSeq: right.cardStreamSeq,
    },
  );
}

export function sortTimelineRecordsBySemanticOrder(records: TimelineRecord[]): TimelineRecord[] {
  return [...records].sort(compareTimelineRecordOrder);
}

export function materializeProjectionMessageFromTimelineRecord(
  record: TimelineRecord,
): SessionTimelineProjectionMessage {
  const blocks = cloneBlocks(record.blocks);
  return {
    id: record.messageId,
    role: record.role,
    content: record.content,
    agent: record.agent,
    source: record.source as SessionTimelineProjectionMessage['source'],
    timestamp: record.messageTimestamp || record.createdAt,
    updatedAt: record.updatedAt,
    attachments: cloneNonEmptyArray(record.attachments),
    images: cloneNonEmptyArray(record.images),
    blocks,
    type: record.messageType,
    noticeType: record.noticeType,
    isStreaming: record.isStreaming,
    isComplete: record.isComplete,
    metadata: sanitizePersistedMessageMetadata({
      role: record.role,
      type: record.messageType,
      content: record.content,
      blocks,
      metadata: record.metadata ? cloneSerializable(record.metadata) : undefined,
    }),
  };
}

export function materializeSessionMessageFromTimelineRecord(record: TimelineRecord): SessionMessage {
  const blocks = cloneBlocks(record.blocks);
  return {
    id: record.messageId,
    role: record.role,
    content: record.content,
    agent: record.agent,
    source: record.source as SessionMessage['source'],
    timestamp: record.messageTimestamp || record.createdAt,
    updatedAt: record.updatedAt,
    attachments: cloneNonEmptyArray(record.attachments),
    images: cloneNonEmptyArray(record.images),
    blocks,
    type: record.messageType,
    category: typeof record.category === 'string' ? record.category as SessionMessage['category'] : undefined,
    visibility: record.visibility,
    noticeType: record.noticeType,
    isStreaming: record.isStreaming,
    isComplete: record.isComplete,
    interaction: record.interaction ? cloneSerializable(record.interaction) : undefined,
    metadata: sanitizePersistedMessageMetadata({
      role: record.role,
      type: record.messageType,
      content: record.content,
      blocks,
      metadata: record.metadata ? cloneSerializable(record.metadata) : undefined,
    }),
  };
}

export function materializeProjectionSourceMessagesFromTimelineRecords(
  records: TimelineRecord[],
): SessionTimelineProjectionMessage[] {
  const orderedRecords = sortTimelineRecordsBySemanticOrder(records);
  const orderedMessageIds: string[] = [];
  const recordsByMessageId = new Map<string, TimelineRecord[]>();

  for (const record of orderedRecords) {
    const messageId = resolveSourceMessageId(record);
    const existing = recordsByMessageId.get(messageId);
    if (existing) {
      existing.push(record);
      continue;
    }
    orderedMessageIds.push(messageId);
    recordsByMessageId.set(messageId, [record]);
  }

  return orderedMessageIds.map((messageId) => {
    const groupedRecords = sortTimelineRecordsBySemanticOrder(recordsByMessageId.get(messageId) || []);
    const [firstRecord] = groupedRecords;
    const lastRecord = groupedRecords[groupedRecords.length - 1] || firstRecord;
    const mergedBlocks = groupedRecords.flatMap((record) => (Array.isArray(record.blocks) && record.blocks.length > 0 ? record.blocks : []));
    const resolvedContent = resolveSourceMessageContent(groupedRecords, mergedBlocks, firstRecord, lastRecord);
    const resolvedType = resolveSourceMessageType(groupedRecords);
    const mergedMetadata = groupedRecords.reduce<Record<string, unknown> | undefined>((acc, record) => {
      const metadata = stripFragmentMetadata(resolveRecordMetadata(record));
      if (!metadata) {
        return acc;
      }
      return {
        ...(acc || {}),
        ...metadata,
      };
    }, undefined);
    const normalizedMetadata = mergeSourceMessageMetadata(groupedRecords, mergedMetadata);

    return {
      id: messageId,
      role: firstRecord.role,
      content: resolvedContent,
      agent: lastRecord.agent || firstRecord.agent,
      source: (lastRecord.source || firstRecord.source) as SessionTimelineProjectionMessage['source'],
      timestamp: groupedRecords.reduce((minTs, record) => Math.min(minTs, record.messageTimestamp || record.createdAt), firstRecord.messageTimestamp || firstRecord.createdAt),
      updatedAt: groupedRecords.reduce((maxTs, record) => Math.max(maxTs, record.updatedAt), firstRecord.updatedAt),
      attachments: pickFirstNonEmptyArray(groupedRecords, (record) => record.attachments),
      images: pickFirstNonEmptyArray(groupedRecords, (record) => record.images),
      blocks: mergedBlocks.length > 0 ? mergedBlocks : undefined,
      type: resolvedType,
      noticeType: lastRecord.noticeType,
      isStreaming: lastRecord.isStreaming,
      isComplete: lastRecord.isComplete,
      metadata: sanitizePersistedMessageMetadata({
        role: firstRecord.role,
        type: resolvedType,
        content: resolvedContent,
        blocks: mergedBlocks.length > 0 ? mergedBlocks : undefined,
        metadata: normalizedMetadata,
      }),
    };
  });
}

export function materializeSessionMessagesFromTimelineRecords(
  records: TimelineRecord[],
): SessionMessage[] {
  const orderedRecords = sortTimelineRecordsBySemanticOrder(records);
  const orderedMessageIds: string[] = [];
  const recordsByMessageId = new Map<string, TimelineRecord[]>();

  for (const record of orderedRecords) {
    const messageId = resolveSourceMessageId(record);
    const existing = recordsByMessageId.get(messageId);
    if (existing) {
      existing.push(record);
      continue;
    }
    orderedMessageIds.push(messageId);
    recordsByMessageId.set(messageId, [record]);
  }

  return orderedMessageIds.map((messageId) => {
    const groupedRecords = sortTimelineRecordsBySemanticOrder(recordsByMessageId.get(messageId) || []);
    const [firstRecord] = groupedRecords;
    const lastRecord = groupedRecords[groupedRecords.length - 1] || firstRecord;
    const mergedBlocks = groupedRecords.flatMap((record) => cloneBlocks(record.blocks) || []);
    const resolvedContent = resolveSourceMessageContent(groupedRecords, mergedBlocks, firstRecord, lastRecord);
    const resolvedType = resolveSourceMessageType(groupedRecords);
    const mergedMetadata = groupedRecords.reduce<Record<string, unknown> | undefined>((acc, record) => {
      const metadata = stripFragmentMetadata(resolveRecordMetadata(record));
      if (!metadata) {
        return acc;
      }
      return {
        ...(acc || {}),
        ...cloneSerializable(metadata),
      };
    }, undefined);
    const normalizedMetadata = mergeSourceMessageMetadata(groupedRecords, mergedMetadata);

    return {
      id: messageId,
      role: firstRecord.role,
      content: resolvedContent,
      agent: lastRecord.agent || firstRecord.agent,
      source: (lastRecord.source || firstRecord.source) as SessionMessage['source'],
      timestamp: groupedRecords.reduce((minTs, record) => Math.min(minTs, record.messageTimestamp || record.createdAt), firstRecord.messageTimestamp || firstRecord.createdAt),
      updatedAt: groupedRecords.reduce((maxTs, record) => Math.max(maxTs, record.updatedAt), firstRecord.updatedAt),
      attachments: pickFirstNonEmptyArray(groupedRecords, (record) => record.attachments),
      images: pickFirstNonEmptyArray(groupedRecords, (record) => record.images),
      blocks: mergedBlocks.length > 0 ? mergedBlocks : undefined,
      type: resolvedType,
      category: typeof lastRecord.category === 'string' ? lastRecord.category as SessionMessage['category'] : undefined,
      visibility: lastRecord.visibility,
      noticeType: lastRecord.noticeType,
      isStreaming: lastRecord.isStreaming,
      isComplete: lastRecord.isComplete,
      interaction: lastRecord.interaction ? cloneSerializable(lastRecord.interaction) : undefined,
      metadata: sanitizePersistedMessageMetadata({
        role: firstRecord.role,
        type: resolvedType,
        content: resolvedContent,
        blocks: mergedBlocks.length > 0 ? mergedBlocks : undefined,
        metadata: normalizedMetadata,
      }),
    };
  });
}
