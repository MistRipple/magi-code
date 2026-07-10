import type {
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

export interface TimelineRenderScope {
  workspaceId?: string | null;
  workspacePath?: string | null;
  sessionId?: string | null;
}

/**
 * 时间线显示上下文：
 * - `thread`：主对话区，渲染主线代理的 artifacts
 * - `task`：右侧 RightPane 代理运行 tab，按 `metadata.taskId` 过滤该代理自身的 artifacts
 */
export type TimelineDisplayContext = 'thread' | 'task';

function normalizeScopeValue(value: string | null | undefined): string | undefined {
  return typeof value === 'string' && value.trim() ? value.trim() : undefined;
}

function renderItemScope(
  projection: SessionTimelineProjection,
  scope: TimelineRenderScope = {},
): Pick<TimelineRenderItem, 'sessionId' | 'workspaceId' | 'workspacePath'> {
  return {
    sessionId: normalizeScopeValue(scope.sessionId) ?? normalizeScopeValue(projection.sessionId),
    workspaceId: normalizeScopeValue(scope.workspaceId),
    workspacePath: normalizeScopeValue(scope.workspacePath),
  };
}

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

function artifactTaskId(artifact: TimelineProjectionArtifact): string {
  const metadata = artifact.message?.metadata;
  if (!metadata || typeof metadata !== 'object') return '';
  const raw = (metadata as Record<string, unknown>).taskId;
  return typeof raw === 'string' ? raw.trim() : '';
}

function buildProjectionPanelView(
  projection: SessionTimelineProjection,
  displayContext: TimelineDisplayContext,
  taskId?: string,
  scope: TimelineRenderScope = {},
): TimelinePanelView {
  const artifactById = buildProjectionArtifactLookup(projection);
  const itemScope = renderItemScope(projection, scope);
  const items: TimelineRenderItem[] = [];
  const messages: Message[] = [];

  if (displayContext === 'thread') {
    for (const entry of projection.threadRenderEntries) {
      const artifact = artifactById.get(entry.artifactId);
      if (!artifact?.message) continue;
      const message = prepareRenderMessage(artifact.message, displayContext);
      items.push({ key: entry.entryId, message, ...itemScope });
      messages.push(message);
    }
    return { items, messages };
  }

  // `task` 上下文（代理运行）：按 metadata.taskId 过滤所有 artifacts（保留 projection.artifacts 原序）。
  const targetTaskId = typeof taskId === 'string' ? taskId.trim() : '';
  if (!targetTaskId) {
    return { items, messages };
  }
  for (const artifact of projection.artifacts || []) {
    if (artifactTaskId(artifact) !== targetTaskId) continue;
    if (!artifact.message) continue;
    const message = prepareRenderMessage(artifact.message, displayContext);
    items.push({ key: `artifact:${artifact.artifactId}`, message, ...itemScope });
    messages.push(message);
  }
  return { items, messages };
}

export function buildTimelinePanelMessages(
  projection: SessionTimelineProjection,
  displayContext: TimelineDisplayContext,
  taskId?: string,
  scope: TimelineRenderScope = {},
): Message[] {
  return buildProjectionPanelView(projection, displayContext, taskId, scope).messages;
}

export function buildTimelinePanelView(
  projection: SessionTimelineProjection,
  displayContext: TimelineDisplayContext,
  taskId?: string,
  scope: TimelineRenderScope = {},
): TimelinePanelView {
  return buildProjectionPanelView(projection, displayContext, taskId, scope);
}

export function buildTimelineRenderItems(
  projection: SessionTimelineProjection,
  displayContext: TimelineDisplayContext,
  taskId?: string,
  scope: TimelineRenderScope = {},
): TimelineRenderItem[] {
  return buildProjectionPanelView(projection, displayContext, taskId, scope).items;
}
