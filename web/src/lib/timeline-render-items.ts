import type {
  AgentId,
  Message,
  SessionTimelineProjection,
  TimelineProjectionArtifact,
  TimelineProjectionRenderEntry,
  TimelineNode,
  TimelineRenderItem,
} from '../types/message';
import { cloneMessagePayload } from './message-payload';
import {
  compareTimelineSemanticOrder,
  resolveTimelineBlockSeqFromMetadata,
  resolveTimelineSemanticMessageType,
} from '../shared/timeline-ordering';
import { resolveTimelinePrimaryToolCallName } from '../shared/timeline-presentation';

export interface TimelinePanelView {
  items: TimelineRenderItem[];
  messages: Message[];
}

export type TimelineDisplayContext = 'thread' | 'worker';
export type TimelineNodeLookup = Map<string, TimelineNode>;

function isSessionTimelineProjection(value: unknown): value is SessionTimelineProjection {
  return Boolean(
    value
    && typeof value === 'object'
    && !Array.isArray(value)
    && (value as { schemaVersion?: string }).schemaVersion === 'session-timeline-projection.v2'
    && Array.isArray((value as { artifacts?: unknown[] }).artifacts)
    && Array.isArray((value as { threadRenderEntries?: unknown[] }).threadRenderEntries),
  );
}

function prepareRenderMessage(message: Message, displayContext: TimelineDisplayContext): Message {
  void displayContext;
  return cloneMessagePayload(message);
}

function shouldRenderNodeHostMessage(
  node: Pick<TimelineNode, 'kind' | 'message' | 'executionItems'>,
  displayContext: TimelineDisplayContext
): boolean {
  void displayContext;
  if (Array.isArray(node.executionItems) && node.executionItems.length > 0) {
    return false;
  }
  return true;
}

interface LiveFlatRenderEntry {
  entryId: string;
  artifactId: string;
  executionItemId?: string;
  groupId: string;
  message: Message;
  timestamp: number;
  displayOrder?: number;
  itemOrder?: number;
  anchorEventSeq: number;
  blockSeq?: number;
  frozenSemanticStage?: number;
}

function resolveMessageBlockSeq(message: Pick<Message, 'metadata'> | undefined): number {
  return resolveTimelineBlockSeqFromMetadata(
    message?.metadata && typeof message.metadata === 'object'
      ? message.metadata as Record<string, unknown>
      : undefined,
  );
}

function compareLiveFlatRenderEntry(
  left: LiveFlatRenderEntry,
  right: LiveFlatRenderEntry,
): number {
  const sameGroup = left.groupId === right.groupId;
  return compareTimelineSemanticOrder(
    {
      timestamp: left.timestamp,
      stableId: left.entryId,
      displayOrder: sameGroup ? left.displayOrder : undefined,
      itemOrder: sameGroup ? left.itemOrder : undefined,
      messageType: resolveTimelineSemanticMessageType(
        left.message.type,
        left.message.metadata && typeof left.message.metadata === 'object'
          ? left.message.metadata as Record<string, unknown>
          : undefined,
      ),
      primaryToolCallName: resolveTimelinePrimaryToolCallName(left.message.blocks),
      anchorEventSeq: left.anchorEventSeq,
      blockSeq: sameGroup ? left.blockSeq : undefined,
      frozenSemanticStage: left.frozenSemanticStage,
    },
    {
      timestamp: right.timestamp,
      stableId: right.entryId,
      displayOrder: sameGroup ? right.displayOrder : undefined,
      itemOrder: sameGroup ? right.itemOrder : undefined,
      messageType: resolveTimelineSemanticMessageType(
        right.message.type,
        right.message.metadata && typeof right.message.metadata === 'object'
          ? right.message.metadata as Record<string, unknown>
          : undefined,
      ),
      primaryToolCallName: resolveTimelinePrimaryToolCallName(right.message.blocks),
      anchorEventSeq: right.anchorEventSeq,
      blockSeq: sameGroup ? right.blockSeq : undefined,
      frozenSemanticStage: right.frozenSemanticStage,
    },
  );
}

function buildLiveRenderEntries(
  nodes: Iterable<TimelineNode>,
  displayContext: TimelineDisplayContext,
  workerName?: AgentId,
): TimelineProjectionRenderEntry[] {
  const flatEntries: LiveFlatRenderEntry[] = [];

  for (const node of nodes) {
    const nodeVisible = displayContext === 'thread'
      ? node.visibleInThread
      : Boolean(workerName && node.workerTabs.includes(workerName));

    if (nodeVisible && node.message && shouldRenderNodeHostMessage(node, displayContext)) {
      flatEntries.push({
        entryId: `artifact:${node.nodeId}`,
        artifactId: node.nodeId,
        groupId: node.nodeId,
        message: node.message,
        timestamp: node.timestamp,
        displayOrder: node.displayOrder,
        anchorEventSeq: node.anchorEventSeq,
        blockSeq: resolveMessageBlockSeq(node.message),
        frozenSemanticStage: node.frozenSemanticStage,
      });
    }

    for (const item of node.executionItems || []) {
      const itemVisible = displayContext === 'thread'
        ? item.threadVisible
        : Boolean(workerName && item.workerTabs.includes(workerName));
      if (!itemVisible) {
        continue;
      }
      flatEntries.push({
        entryId: `item:${node.nodeId}:${item.itemId}`,
        artifactId: node.nodeId,
        executionItemId: item.itemId,
        groupId: node.nodeId,
        message: item.message,
        timestamp: item.timestamp,
        displayOrder: node.displayOrder,
        itemOrder: item.itemOrder,
        anchorEventSeq: item.anchorEventSeq,
        blockSeq: resolveMessageBlockSeq(item.message),
        frozenSemanticStage: node.frozenSemanticStage,
      });
    }
  }

  return flatEntries
    .sort(compareLiveFlatRenderEntry)
    .map((entry) => ({
      entryId: entry.entryId,
      artifactId: entry.artifactId,
      executionItemId: entry.executionItemId,
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
    if (!artifact) {
      continue;
    }
    if (entry.executionItemId) {
      const executionItem = (artifact.executionItems || []).find((item) => item?.itemId === entry.executionItemId);
      if (!executionItem?.message) {
        continue;
      }
      const message = prepareRenderMessage(executionItem.message, displayContext);
      items.push({
        key: entry.entryId,
        message,
      });
      messages.push(message);
      continue;
    }
    if (!artifact.message || !shouldRenderNodeHostMessage({
      kind: artifact.kind,
      message: artifact.message,
      executionItems: artifact.executionItems,
    }, displayContext)) {
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

export function buildTimelineNodeLookup(nodes: TimelineNode[]): TimelineNodeLookup {
  const nodeById: TimelineNodeLookup = new Map<string, TimelineNode>();
  for (const node of nodes || []) {
    if (node?.nodeId && node.message) {
      nodeById.set(node.nodeId, node);
    }
  }
  return nodeById;
}

export function buildTimelinePanelMessages(
  nodeById: TimelineNodeLookup | TimelineNode[] | SessionTimelineProjection,
  displayContext: TimelineDisplayContext,
  workerName?: AgentId,
): Message[] {
  if (isSessionTimelineProjection(nodeById)) {
    return buildProjectionPanelView(nodeById, displayContext, workerName).messages;
  }
  const normalizedNodeLookup = nodeById instanceof Map ? nodeById : buildTimelineNodeLookup(nodeById);
  const entries = buildLiveRenderEntries(normalizedNodeLookup.values(), displayContext, workerName);
  const messages: Message[] = [];
  for (const entry of entries) {
    const node = normalizedNodeLookup.get(entry.artifactId);
    if (!node) {
      continue;
    }
    if (entry.executionItemId) {
      const item = (node.executionItems || []).find((candidate) => candidate?.itemId === entry.executionItemId);
      if (item?.message && shouldRenderNodeHostMessage({ kind: 'message', message: item.message, executionItems: [] }, displayContext)) {
        messages.push(item.message);
      }
      continue;
    }
    if (node.message && shouldRenderNodeHostMessage(node, displayContext)) {
      messages.push(node.message);
    }
  }
  return messages;
}

export function buildTimelinePanelView(
  nodes: TimelineNode[] | TimelineNodeLookup | SessionTimelineProjection,
  displayContext: TimelineDisplayContext,
  workerName?: AgentId,
): TimelinePanelView {
  if (isSessionTimelineProjection(nodes)) {
    return buildProjectionPanelView(nodes, displayContext, workerName);
  }
  const nodeById = nodes instanceof Map ? nodes : buildTimelineNodeLookup(nodes);
  const items: TimelineRenderItem[] = [];
  const messages: Message[] = [];
  const entries = buildLiveRenderEntries(nodeById.values(), displayContext, workerName);
  for (const entry of entries) {
    const node = nodeById.get(entry.artifactId);
    if (!node) {
      continue;
    }
    if (entry.executionItemId) {
      const item = (node.executionItems || []).find((candidate) => candidate?.itemId === entry.executionItemId);
      if (!item?.message) {
        continue;
      }
      const message = prepareRenderMessage(item.message, displayContext);
      items.push({
        key: entry.entryId,
        message,
      });
      messages.push(message);
      continue;
    }
    if (!node.message || !shouldRenderNodeHostMessage(node, displayContext)) {
      continue;
    }
    const message = prepareRenderMessage(node.message, displayContext);
    items.push({
      key: entry.entryId,
      message,
    });
    messages.push(message);
  }

  return {
    items,
    messages,
  };
}

export function buildTimelineRenderItems(
  nodes: TimelineNode[] | TimelineNodeLookup | SessionTimelineProjection,
  displayContext: TimelineDisplayContext,
  workerName?: AgentId,
): TimelineRenderItem[] {
  return buildTimelinePanelView(nodes, displayContext, workerName).items;
}
