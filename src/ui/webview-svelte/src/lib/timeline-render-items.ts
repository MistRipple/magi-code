import type {
  AgentType,
  Message,
  SessionTimelineProjection,
  TimelineExecutionItem,
  TimelineNode,
  TimelineProjectionRenderEntry,
  TimelineRenderItem,
} from '../types/message';
import { cloneMessagePayload } from './message-payload';

export interface TimelinePanelView {
  items: TimelineRenderItem[];
  messages: Message[];
}

export type TimelineDisplayContext = 'thread' | 'worker';
export type TimelineNodeLookup = Map<string, TimelineNode>;

function isContainerOnlyMessage(message: Pick<Message, 'metadata'> | undefined): boolean {
  return message?.metadata && typeof message.metadata === 'object'
    ? message.metadata.timelineContainerOnly === true
    : false;
}

function shouldRenderNodeHostMessage(node: Pick<TimelineNode, 'kind' | 'message' | 'executionItems'>): boolean {
  if (node.kind !== 'worker_lifecycle' && Array.isArray(node.executionItems) && node.executionItems.length > 0) {
    return false;
  }
  return !isContainerOnlyMessage(node.message);
}


function resolveProjectionRenderEntries(
  projection: SessionTimelineProjection | null | undefined,
  displayContext: TimelineDisplayContext,
  workerName?: AgentType,
): TimelineProjectionRenderEntry[] {
  if (!projection) {
    return [];
  }
  if (displayContext === 'thread') {
    return Array.isArray(projection.threadRenderEntries)
      ? projection.threadRenderEntries
      : [];
  }
  if (!workerName) {
    return [];
  }
  const workerEntries = projection.workerRenderEntries?.[workerName];
  return Array.isArray(workerEntries) ? workerEntries : [];
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
  nodeById: TimelineNodeLookup,
  projection: SessionTimelineProjection | null | undefined,
  displayContext: TimelineDisplayContext,
  workerName?: AgentType,
): Message[] {
  const entries = resolveProjectionRenderEntries(projection, displayContext, workerName);
  const messages: Message[] = [];
  for (const entry of entries) {
    const node = nodeById.get(entry.artifactId);
    if (!node) {
      continue;
    }
    if (entry.executionItemId) {
      const item = (node.executionItems || []).find((candidate) => candidate?.itemId === entry.executionItemId);
      if (item?.message) {
        messages.push(item.message);
      }
      continue;
    }
    if (node.message && shouldRenderNodeHostMessage(node)) {
      messages.push(node.message);
    }
  }
  return messages;
}

export function buildTimelinePanelView(
  nodes: TimelineNode[] | TimelineNodeLookup,
  projection: SessionTimelineProjection | null | undefined,
  displayContext: TimelineDisplayContext,
  workerName?: AgentType,
): TimelinePanelView {
  const nodeById = nodes instanceof Map ? nodes : buildTimelineNodeLookup(nodes);
  const items: TimelineRenderItem[] = [];
  const messages: Message[] = [];
  const entries = resolveProjectionRenderEntries(projection, displayContext, workerName);
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
      const message = cloneMessagePayload(item.message);
      items.push({
        key: entry.entryId,
        message,
      });
      messages.push(message);
      continue;
    }
    if (!node.message || !shouldRenderNodeHostMessage(node)) {
      continue;
    }
    const message = cloneMessagePayload(node.message);
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
  nodes: TimelineNode[] | TimelineNodeLookup,
  projection: SessionTimelineProjection | null | undefined,
  displayContext: TimelineDisplayContext,
  workerName?: AgentType,
): TimelineRenderItem[] {
  return buildTimelinePanelView(nodes, projection, displayContext, workerName).items;
}
