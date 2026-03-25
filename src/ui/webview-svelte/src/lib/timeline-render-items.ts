import type {
  AgentType,
  Message,
  SessionTimelineProjection,
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

function resolvePanelRenderEntries(
  projection: SessionTimelineProjection | null | undefined,
  displayContext: TimelineDisplayContext,
  workerName?: AgentType,
): TimelineProjectionRenderEntry[] {
  if (!projection) {
    return [];
  }
  if (displayContext === 'thread') {
    return Array.isArray(projection.threadRenderEntries) ? projection.threadRenderEntries : [];
  }
  if (!workerName) {
    return [];
  }
  const entries = projection.workerRenderEntries?.[workerName];
  return Array.isArray(entries) ? entries : [];
}

function resolveProjectionMessage(
  nodeById: TimelineNodeLookup,
  entry: TimelineProjectionRenderEntry,
  clonePayload: boolean,
): Message | null {
  const node = nodeById.get(entry.artifactId);
  if (!node) {
    return null;
  }
  const sourceMessage = typeof entry.executionItemId === 'string' && entry.executionItemId.trim()
    ? (node.executionItems || []).find((item) => item.itemId === entry.executionItemId)?.message
    : node.message;
  if (!sourceMessage) {
    return null;
  }
  return clonePayload ? cloneMessagePayload(sourceMessage) : sourceMessage;
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
  const messages: Message[] = [];
  for (const entry of resolvePanelRenderEntries(projection, displayContext, workerName)) {
    const message = resolveProjectionMessage(nodeById, entry, false);
    if (message) {
      messages.push(message);
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
  for (const entry of resolvePanelRenderEntries(projection, displayContext, workerName)) {
    const message = resolveProjectionMessage(nodeById, entry, true);
    if (!message) {
      continue;
    }
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
