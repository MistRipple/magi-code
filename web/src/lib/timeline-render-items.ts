import type {
  AgentId,
  Message,
  SessionTimelineProjection,
  TimelineProjectionArtifact,
  TimelineRenderItem,
  WorkerLaneStatus,
} from '../types/message';
import { cloneMessagePayload } from './message-payload';

export interface TimelinePanelView {
  items: TimelineRenderItem[];
  messages: Message[];
}

export type TimelineDisplayContext = 'thread' | 'worker';

export interface WorkerStageGroupLabels {
  stageFallback: string;
  directTitle: string;
  ungroupedTitle: string;
}

export interface WorkerStageRenderGroup {
  key: string;
  title: string;
  status: WorkerLaneStatus;
  displayIndex: number;
  laneSeq?: number;
  isDirect: boolean;
  items: TimelineRenderItem[];
  toolUseCount: number;
  replyCount: number;
}

interface MutableWorkerStageRenderGroup extends WorkerStageRenderGroup {
  firstItemSeq: number;
  dispatchStatus?: WorkerLaneStatus;
  hasRunningItem: boolean;
  hasPendingItem: boolean;
}

const DEFAULT_WORKER_STAGE_LABELS: WorkerStageGroupLabels = {
  stageFallback: '执行阶段',
  directTitle: '任务总控',
  ungroupedTitle: '执行补充',
};

function prepareRenderMessage(message: Message, displayContext: TimelineDisplayContext): Message {
  void displayContext;
  return cloneMessagePayload(message);
}

function normalizeText(value: unknown): string {
  return typeof value === 'string' ? value.trim() : '';
}

function normalizeNumber(value: unknown): number | undefined {
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    return undefined;
  }
  return value;
}

function messageMetadata(message: Message): Record<string, unknown> {
  return message.metadata && typeof message.metadata === 'object'
    ? message.metadata as Record<string, unknown>
    : {};
}

function messageItemSeq(item: TimelineRenderItem): number {
  return normalizeNumber(messageMetadata(item.message).itemSeq) ?? Number.MAX_SAFE_INTEGER;
}

function messageTurnItemKind(item: TimelineRenderItem): string {
  return normalizeText(messageMetadata(item.message).turnItemKind);
}

function messageTurnItemStatus(item: TimelineRenderItem): WorkerLaneStatus {
  const status = normalizeText(messageMetadata(item.message).turnItemStatus);
  switch (status) {
    case 'blocked':
      return 'blocked';
    case 'failed':
      return 'failed';
    case 'cancelled':
      return 'cancelled';
    case 'completed':
      return 'completed';
    case 'running':
      return 'running';
    case 'pending':
    default:
      return 'pending';
  }
}

function resolveWorkerStageDescriptor(
  item: TimelineRenderItem,
  labels: WorkerStageGroupLabels,
): { key: string; title: string; laneSeq?: number; isDirect: boolean } {
  const metadata = messageMetadata(item.message);
  const laneId = normalizeText(metadata.laneId);
  const laneSeq = normalizeNumber(metadata.laneSeq);
  if (laneId) {
    const title = normalizeText(metadata.laneTitle) || `${labels.stageFallback} ${laneSeq ?? ''}`.trim();
    return {
      key: `lane:${laneId}`,
      title,
      ...(laneSeq !== undefined ? { laneSeq } : {}),
      isDirect: false,
    };
  }

  const taskId = normalizeText(metadata.taskId);
  if (taskId) {
    return {
      key: `task:${taskId}`,
      title: labels.directTitle,
      isDirect: true,
    };
  }

  return {
    key: 'worker:ungrouped',
    title: labels.ungroupedTitle,
    isDirect: true,
  };
}

function compareWorkerStageGroups(
  left: MutableWorkerStageRenderGroup,
  right: MutableWorkerStageRenderGroup,
): number {
  const leftSeq = left.laneSeq ?? Number.MAX_SAFE_INTEGER;
  const rightSeq = right.laneSeq ?? Number.MAX_SAFE_INTEGER;
  return leftSeq - rightSeq || left.firstItemSeq - right.firstItemSeq || left.key.localeCompare(right.key);
}

function resolveWorkerStageStatus(group: MutableWorkerStageRenderGroup): WorkerLaneStatus {
  if (group.dispatchStatus) {
    return group.dispatchStatus;
  }
  if (group.hasRunningItem) {
    return 'running';
  }
  if (group.hasPendingItem) {
    return 'pending';
  }
  return 'completed';
}

export function buildWorkerStageRenderGroups(
  items: TimelineRenderItem[],
  labels: Partial<WorkerStageGroupLabels> = {},
): WorkerStageRenderGroup[] {
  const resolvedLabels = { ...DEFAULT_WORKER_STAGE_LABELS, ...labels };
  const groupsByKey = new Map<string, MutableWorkerStageRenderGroup>();

  for (const item of items || []) {
    if (!item?.message?.id) {
      continue;
    }
    const descriptor = resolveWorkerStageDescriptor(item, resolvedLabels);
    const itemSeq = messageItemSeq(item);
    let group = groupsByKey.get(descriptor.key);
    if (!group) {
      group = {
        key: descriptor.key,
        title: descriptor.title,
        status: 'pending',
        displayIndex: 0,
        ...(descriptor.laneSeq !== undefined ? { laneSeq: descriptor.laneSeq } : {}),
        isDirect: descriptor.isDirect,
        items: [],
        toolUseCount: 0,
        replyCount: 0,
        firstItemSeq: itemSeq,
        hasRunningItem: false,
        hasPendingItem: false,
      };
      groupsByKey.set(descriptor.key, group);
    } else {
      group.firstItemSeq = Math.min(group.firstItemSeq, itemSeq);
      if (!group.laneSeq && descriptor.laneSeq !== undefined) {
        group.laneSeq = descriptor.laneSeq;
      }
      if (group.title === resolvedLabels.stageFallback || group.title === resolvedLabels.ungroupedTitle) {
        group.title = descriptor.title;
      }
    }

    const itemKind = messageTurnItemKind(item);
    const itemStatus = messageTurnItemStatus(item);
    if (itemStatus === 'running') {
      group.hasRunningItem = true;
    } else if (itemStatus === 'pending') {
      group.hasPendingItem = true;
    }

    if (itemKind === 'worker_dispatch') {
      group.dispatchStatus = itemStatus;
      group.title = descriptor.title;
      continue;
    }

    if (itemKind === 'tool_call') {
      group.toolUseCount += 1;
    } else if (itemKind === 'assistant_text') {
      group.replyCount += 1;
    }
    group.items.push(item);
  }

  return Array.from(groupsByKey.values())
    .sort(compareWorkerStageGroups)
    .map((group, index) => ({
      key: group.key,
      title: group.title,
      status: resolveWorkerStageStatus(group),
      displayIndex: index + 1,
      ...(group.laneSeq !== undefined ? { laneSeq: group.laneSeq } : {}),
      isDirect: group.isDirect,
      items: group.items,
      toolUseCount: group.toolUseCount,
      replyCount: group.replyCount,
    }));
}

function buildProjectionArtifactLookup(
  projection: SessionTimelineProjection,
): Map<string, TimelineProjectionArtifact> {
  const artifactById = new Map<string, TimelineProjectionArtifact>();
  for (const artifact of projection.artifacts || []) {
    if (artifact?.artifactId) {
      artifactById.set(artifact.artifactId, artifact);
    }
  }
  return artifactById;
}

function buildProjectionPanelView(
  projection: SessionTimelineProjection,
  displayContext: TimelineDisplayContext,
  workerName?: AgentId,
): TimelinePanelView {
  const artifactById = buildProjectionArtifactLookup(projection);
  const renderEntries = displayContext === 'thread'
    ? projection.threadRenderEntries
    : (workerName ? projection.workerRenderEntries[workerName] || [] : []);
  const items: TimelineRenderItem[] = [];
  const messages: Message[] = [];

  for (const entry of renderEntries) {
    const artifact = artifactById.get(entry.artifactId);
    if (!artifact?.message) {
      continue;
    }
    const message = prepareRenderMessage(artifact.message, displayContext);
    items.push({
      key: entry.entryId,
      message,
    });
    messages.push(message);
  }

  return { items, messages };
}

export function buildTimelinePanelMessages(
  projection: SessionTimelineProjection,
  displayContext: TimelineDisplayContext,
  workerName?: AgentId,
): Message[] {
  return buildProjectionPanelView(projection, displayContext, workerName).messages;
}

export function buildTimelinePanelView(
  projection: SessionTimelineProjection,
  displayContext: TimelineDisplayContext,
  workerName?: AgentId,
): TimelinePanelView {
  return buildProjectionPanelView(projection, displayContext, workerName);
}

export function buildTimelineRenderItems(
  projection: SessionTimelineProjection,
  displayContext: TimelineDisplayContext,
  workerName?: AgentId,
): TimelineRenderItem[] {
  return buildProjectionPanelView(projection, displayContext, workerName).items;
}
