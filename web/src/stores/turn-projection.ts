import type {
  AgentId,
  ContentBlock,
  Message,
  SessionTimelineProjection,
  TimelineProjectionArtifact,
  TimelineProjectionRenderEntry,
} from '../types/message';
import type {
  CanonicalToolCall,
  CanonicalTurn,
  CanonicalTurnItem,
  CanonicalTurnItemStatus,
} from '../shared/protocol/canonical-turn';
import { isCanonicalTerminalStatus } from '../shared/protocol/canonical-turn';
import type { CanonicalTurnReducerState } from './turn-reducer';

function normalizeSessionId(value: string | null | undefined): string {
  return typeof value === 'string' ? value.trim() : '';
}

function normalizeWorkerId(item: CanonicalTurnItem): AgentId | undefined {
  const workerRole = typeof item.worker?.roleId === 'string' ? item.worker.roleId.trim() : '';
  const workerTab = item.visibility.workerTabIds?.find((value) => typeof value === 'string' && value.trim());
  return (workerRole || workerTab || item.worker?.workerId || undefined) as AgentId | undefined;
}

function statusToToolStatus(status: CanonicalTurnItemStatus): 'pending' | 'running' | 'success' | 'error' {
  if (status === 'completed') {
    return 'success';
  }
  if (status === 'blocked' || status === 'failed' || status === 'cancelled') {
    return 'error';
  }
  if (status === 'running') {
    return 'running';
  }
  return 'pending';
}

function toolArgumentsToRecord(value: unknown): Record<string, unknown> {
  return value && typeof value === 'object' && !Array.isArray(value)
    ? value as Record<string, unknown>
    : {};
}

function valueToDisplayText(value: unknown): string | undefined {
  if (value === undefined || value === null) {
    return undefined;
  }
  if (typeof value === 'string') {
    return value;
  }
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

function buildToolBlock(tool: CanonicalToolCall, status: CanonicalTurnItemStatus): ContentBlock {
  return {
    type: 'tool_call',
    content: '',
    toolCall: {
      id: tool.callId,
      name: tool.name,
      arguments: toolArgumentsToRecord(tool.arguments),
      status: statusToToolStatus(status),
      result: valueToDisplayText(tool.result),
      error: tool.error,
    },
  };
}

function buildMessageBlocks(item: CanonicalTurnItem, content: string): ContentBlock[] | undefined {
  if (item.kind === 'tool_call' && item.tool) {
    return [buildToolBlock(item.tool, item.status)];
  }
  if (item.kind === 'assistant_thinking') {
    return [{
      type: 'thinking',
      content,
      thinking: {
        content,
        isComplete: isCanonicalTerminalStatus(item.status),
        blockId: `thinking:${item.itemId}`,
      },
    }];
  }
  return undefined;
}

function resolveMessageRole(item: CanonicalTurnItem): Message['role'] {
  if (item.kind === 'user_message') {
    return 'user';
  }
  if (item.kind === 'system_notice' || item.kind === 'task_status') {
    return 'system';
  }
  return 'assistant';
}

function resolveMessageType(item: CanonicalTurnItem): Message['type'] {
  if (item.kind === 'user_message') {
    return 'user_input';
  }
  if (item.kind === 'assistant_thinking') {
    return 'thinking';
  }
  if (item.kind === 'tool_call') {
    return 'tool_call';
  }
  if (item.kind === 'system_notice') {
    return 'system-notice';
  }
  if (item.kind === 'assistant_text') {
    return 'text';
  }
  if (item.status === 'failed') {
    return 'error';
  }
  return 'text';
}

function resolveItemContent(item: CanonicalTurnItem): string {
  if (typeof item.content === 'string') {
    return item.content;
  }
  if (item.kind === 'assistant_text') {
    return '';
  }
  if (item.kind === 'tool_call') {
    return item.title || item.tool?.name || '';
  }
  return item.title || '';
}

function shouldRenderItem(item: CanonicalTurnItem): boolean {
  if (item.visibility.renderable === false || item.kind === 'system_notice') {
    return false;
  }
  if (
    item.kind === 'assistant_text'
    && isCanonicalTerminalStatus(item.status)
    && resolveItemContent(item).trim().length === 0
  ) {
    return false;
  }
  return true;
}

function isTurnResponseDurationAnchorCandidate(item: CanonicalTurnItem): boolean {
  return item.kind !== 'user_message'
    && item.visibility.threadVisible !== false
    && shouldRenderItem(item);
}

function findTurnResponseDurationAnchor(turn: CanonicalTurn): CanonicalTurnItem | undefined {
  if (
    !isCanonicalTerminalStatus(turn.status)
    || typeof turn.responseDurationMs !== 'number'
    || !Number.isFinite(turn.responseDurationMs)
    || turn.responseDurationMs < 0
  ) {
    return undefined;
  }
  return turn.items
    .filter(isTurnResponseDurationAnchorCandidate)
    .sort((left, right) => left.itemSeq - right.itemSeq || left.itemId.localeCompare(right.itemId))
    .at(-1);
}

function canShowTurnResponseDuration(turn: CanonicalTurn, item: CanonicalTurnItem): boolean {
  const anchor = findTurnResponseDurationAnchor(turn);
  return anchor?.itemId === item.itemId;
}

function buildMessage(turn: CanonicalTurn, item: CanonicalTurnItem, artifactId: string): Message {
  const content = resolveItemContent(item);
  const worker = normalizeWorkerId(item);
  const blocks = buildMessageBlocks(item, content);
  const isStreaming = item.kind === 'assistant_text' && !isCanonicalTerminalStatus(item.status);
  const responseDurationMs = canShowTurnResponseDuration(turn, item)
    ? turn.responseDurationMs
    : undefined;
  return {
    id: artifactId,
    role: resolveMessageRole(item),
    source: item.kind === 'user_message' ? 'user' : (worker || 'orchestrator'),
    content,
    ...(blocks ? { blocks } : {}),
    timestamp: item.createdAt,
    updatedAt: item.updatedAt,
    isStreaming,
    isComplete: !isStreaming,
    type: resolveMessageType(item),
    metadata: {
      turnId: item.turnId,
      turnSeq: item.turnSeq,
      turnStatus: turn.status,
      turnItemId: item.itemId,
      turnItemKind: item.kind,
      turnItemStatus: item.status,
      itemSeq: item.itemSeq,
      blockSeq: item.itemSeq,
      laneId: item.laneId,
      laneSeq: item.laneSeq,
      cardStreamSeq: item.itemSeq,
      workerId: item.worker?.workerId,
      roleId: worker,
      taskId: item.worker?.taskId,
      toolCallId: item.tool?.callId,
      toolName: item.tool?.name,
      ...(responseDurationMs !== undefined ? { responseDurationMs } : {}),
      canonical: true,
    },
  };
}

function resolveArtifactId(turn: CanonicalTurn, item: CanonicalTurnItem): string {
  if (item.kind === 'tool_call') {
    return `turn:${turn.turnId}:${item.tool?.callId || item.itemId}`;
  }
  return `turn:${turn.turnId}:${item.itemId}`;
}

function buildArtifact(turn: CanonicalTurn, item: CanonicalTurnItem): TimelineProjectionArtifact | null {
  if (!shouldRenderItem(item)) {
    return null;
  }
  const artifactId = resolveArtifactId(turn, item);
  const worker = normalizeWorkerId(item);
  const workerTabs = item.visibility.workerVisible && worker ? [worker] : [];
  return {
    artifactId,
    kind: item.kind === 'tool_call' ? 'tool' : 'message',
    displayOrder: turn.turnSeq * 1000 + item.itemSeq,
    artifactVersion: item.itemVersion,
    anchorEventSeq: 0,
    latestEventSeq: 0,
    cardStreamSeq: item.itemSeq,
    timestamp: item.createdAt,
    cardId: artifactId,
    laneId: item.laneId,
    worker,
    threadVisible: item.visibility.threadVisible !== false,
    workerTabs,
    messageIds: [artifactId, item.itemId],
    message: buildMessage(turn, item, artifactId),
  };
}

function compareArtifacts(left: TimelineProjectionArtifact, right: TimelineProjectionArtifact): number {
  return left.displayOrder - right.displayOrder || left.artifactId.localeCompare(right.artifactId);
}

function mergeWorkerTabs(
  left: AgentId[] | undefined,
  right: AgentId[] | undefined,
): AgentId[] {
  const merged: AgentId[] = [];
  const seen = new Set<string>();
  for (const workerId of [...(left || []), ...(right || [])]) {
    if (!workerId || seen.has(workerId)) {
      continue;
    }
    seen.add(workerId);
    merged.push(workerId);
  }
  return merged;
}

function mergeMessageIds(left: string[] | undefined, right: string[] | undefined): string[] {
  const merged: string[] = [];
  const seen = new Set<string>();
  for (const messageId of [...(left || []), ...(right || [])]) {
    if (!messageId || seen.has(messageId)) {
      continue;
    }
    seen.add(messageId);
    merged.push(messageId);
  }
  return merged;
}

function mergeDuplicateArtifact(
  first: TimelineProjectionArtifact,
  latest: TimelineProjectionArtifact,
): TimelineProjectionArtifact {
  const firstMetadata = first.message.metadata || {};
  const latestMetadata = latest.message.metadata || {};
  const stableItemSeq = typeof firstMetadata.itemSeq === 'number'
    ? firstMetadata.itemSeq
    : latestMetadata.itemSeq;
  const stableBlockSeq = typeof firstMetadata.blockSeq === 'number'
    ? firstMetadata.blockSeq
    : latestMetadata.blockSeq;
  const stableCardStreamSeq = typeof firstMetadata.cardStreamSeq === 'number'
    ? firstMetadata.cardStreamSeq
    : latestMetadata.cardStreamSeq;

  return {
    ...latest,
    displayOrder: Math.min(first.displayOrder, latest.displayOrder),
    cardStreamSeq: Math.min(first.cardStreamSeq, latest.cardStreamSeq),
    timestamp: Math.min(first.timestamp, latest.timestamp),
    cardId: first.cardId,
    laneId: first.laneId || latest.laneId,
    worker: first.worker || latest.worker,
    threadVisible: first.threadVisible !== false || latest.threadVisible !== false,
    workerTabs: mergeWorkerTabs(first.workerTabs, latest.workerTabs),
    messageIds: mergeMessageIds(first.messageIds, latest.messageIds),
    message: {
      ...latest.message,
      id: first.message.id,
      timestamp: first.message.timestamp,
      metadata: {
        ...latestMetadata,
        itemSeq: stableItemSeq,
        blockSeq: stableBlockSeq,
        cardStreamSeq: stableCardStreamSeq,
      },
    },
  };
}

function collapseArtifactsByStableCard(
  artifacts: TimelineProjectionArtifact[],
): TimelineProjectionArtifact[] {
  const artifactById = new Map<string, TimelineProjectionArtifact>();
  for (const artifact of artifacts) {
    const existing = artifactById.get(artifact.artifactId);
    artifactById.set(
      artifact.artifactId,
      existing ? mergeDuplicateArtifact(existing, artifact) : artifact,
    );
  }
  return Array.from(artifactById.values()).sort(compareArtifacts);
}

function renderEntry(artifact: TimelineProjectionArtifact): TimelineProjectionRenderEntry {
  return {
    entryId: artifact.artifactId,
    artifactId: artifact.artifactId,
  };
}

export function buildCanonicalTimelineProjection(state: CanonicalTurnReducerState): SessionTimelineProjection | null {
  const sessionId = normalizeSessionId(state.sessionId);
  if (!sessionId) {
    return null;
  }
  const artifacts = collapseArtifactsByStableCard(state.turns
    .flatMap((turn) => turn.items.map((item) => buildArtifact(turn, item)))
    .filter((artifact): artifact is TimelineProjectionArtifact => Boolean(artifact))
    .sort(compareArtifacts));
  const threadRenderEntries = artifacts
    .filter((artifact) => artifact.threadVisible !== false)
    .map(renderEntry);
  const workerRenderEntries: Record<string, TimelineProjectionRenderEntry[]> = {};
  for (const artifact of artifacts) {
    for (const workerId of artifact.workerTabs) {
      if (!workerRenderEntries[workerId]) {
        workerRenderEntries[workerId] = [];
      }
      workerRenderEntries[workerId].push(renderEntry(artifact));
    }
  }
  return {
    schemaVersion: 'session-timeline-projection.v2',
    sessionId,
    updatedAt: Date.now(),
    lastAppliedEventSeq: state.lastAppliedEventSeq,
    artifacts,
    threadRenderEntries,
    workerRenderEntries,
  };
}
