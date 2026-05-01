import type {
  AgentId,
  Message,
  SessionTimelineProjection,
  TimelineProjectionArtifact,
  TimelineRenderItem,
} from '../types/message';
import { cloneMessagePayload } from './message-payload';

export interface TimelinePanelView {
  items: TimelineRenderItem[];
  messages: Message[];
}

export type TimelineDisplayContext = 'thread' | 'worker';

function prepareRenderMessage(message: Message, displayContext: TimelineDisplayContext): Message {
  void displayContext;
  return cloneMessagePayload(message);
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
