import type {
  AgentId,
  ContentBlock,
  DispatchGroupLane,
  Message,
  SessionTimelineProjection,
  TimelineProjectionArtifact,
  TimelineProjectionRenderEntry,
  WorkerLaneStatus,
} from '../types/message';
import type {
  CanonicalToolCall,
  CanonicalTurn,
  CanonicalTurnItem,
  CanonicalTurnItemStatus,
} from '../shared/protocol/canonical-turn';
import { isCanonicalTerminalStatus } from '../shared/protocol/canonical-turn';
import type { CanonicalTurnReducerState } from './turn-reducer';

interface LaneProjectionMeta {
  laneId: string;
  laneSeq?: number;
  title: string;
  status: WorkerLaneStatus;
}

interface LaneActivitySnapshot {
  text: string;
  itemSeq: number;
}

function normalizeSessionId(value: string | null | undefined): string {
  return typeof value === 'string' ? value.trim() : '';
}

function normalizeWorkerId(item: CanonicalTurnItem): AgentId | undefined {
  const workerRole = typeof item.worker?.roleId === 'string' ? item.worker.roleId.trim() : '';
  const workerTab = item.visibility.workerTabIds?.find((value) => typeof value === 'string' && value.trim());
  return (workerRole || workerTab || item.worker?.workerId || undefined) as AgentId | undefined;
}

function resolveVisibleWorkerId(item: CanonicalTurnItem): AgentId | undefined {
  return item.visibility.workerVisible ? normalizeWorkerId(item) : undefined;
}

function resolveMessageSource(item: CanonicalTurnItem): Message['source'] {
  if (item.kind === 'user_message') {
    return 'user';
  }
  return resolveVisibleWorkerId(item) || 'orchestrator';
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

function readObject(value: unknown): Record<string, unknown> | undefined {
  return value && typeof value === 'object' && !Array.isArray(value)
    ? value as Record<string, unknown>
    : undefined;
}

function normalizeActivityText(value: unknown): string {
  return typeof value === 'string'
    ? value.replace(/\s+/g, ' ').trim()
    : '';
}

function readStringField(record: Record<string, unknown> | undefined, keys: string[]): string {
  if (!record) {
    return '';
  }
  for (const key of keys) {
    const value = normalizeActivityText(record[key]);
    if (value) {
      return value;
    }
  }
  return '';
}

function summarizeToolResult(value: unknown): string {
  if (typeof value === 'string') {
    const trimmed = value.trim();
    if (!trimmed) {
      return '';
    }
    try {
      const parsed = JSON.parse(trimmed);
      return summarizeToolResult(parsed) || normalizeActivityText(trimmed);
    } catch {
      return normalizeActivityText(trimmed);
    }
  }
  const record = readObject(value);
  return readStringField(record, ['summary', 'message', 'error']);
}

function summarizeLaneItem(item: CanonicalTurnItem): string {
  if (item.kind === 'worker_dispatch' || item.kind === 'user_message' || item.kind === 'system_notice') {
    return '';
  }
  const metadata = readObject(item.metadata);
  const explicitSummary = readStringField(metadata, [
    'stageSummary',
    'stage_summary',
    'summary',
    'message',
  ]);
  if (explicitSummary) {
    return explicitSummary;
  }
  if (item.kind === 'tool_call') {
    return summarizeToolResult(item.tool?.result)
      || normalizeActivityText(item.content)
      || normalizeActivityText(item.title || item.tool?.name);
  }
  return normalizeActivityText(item.content) || normalizeActivityText(item.title);
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
    return item.visibility.threadVisible ? 'text' : 'system-notice';
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
  if (item.visibility.renderable === false) {
    return false;
  }
  if (
    item.kind === 'system_notice'
    && item.visibility.threadVisible === false
    && item.visibility.workerVisible === false
  ) {
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
    && item.kind !== 'system_notice'
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

function resolveDispatchGroupResponseDurationMs(turn: CanonicalTurn): number | undefined {
  if (
    !isCanonicalTerminalStatus(turn.status)
    || typeof turn.responseDurationMs !== 'number'
    || !Number.isFinite(turn.responseDurationMs)
    || turn.responseDurationMs < 0
  ) {
    return undefined;
  }
  return findTurnResponseDurationAnchor(turn) ? undefined : turn.responseDurationMs;
}

function collectLaneProjectionMeta(turn: CanonicalTurn): Map<string, LaneProjectionMeta> {
  const metaByLaneId = new Map<string, LaneProjectionMeta>();
  for (const item of turn.items) {
    if (item.kind !== 'worker_dispatch') {
      continue;
    }
    const laneId = typeof item.laneId === 'string' ? item.laneId.trim() : '';
    if (!laneId) {
      continue;
    }
    const title = (item.title || item.worker?.title || laneId).trim();
    metaByLaneId.set(laneId, {
      laneId,
      ...(typeof item.laneSeq === 'number' && Number.isFinite(item.laneSeq) ? { laneSeq: item.laneSeq } : {}),
      title,
      status: canonicalStatusToWorkerLaneStatus(item.status),
    });
  }
  return metaByLaneId;
}

function buildMessage(
  turn: CanonicalTurn,
  item: CanonicalTurnItem,
  artifactId: string,
  laneMetaById: Map<string, LaneProjectionMeta>,
): Message {
  const content = resolveItemContent(item);
  const worker = resolveVisibleWorkerId(item);
  const blocks = buildMessageBlocks(item, content);
  const isStreaming = item.kind === 'assistant_text' && !isCanonicalTerminalStatus(item.status);
  const responseDurationMs = canShowTurnResponseDuration(turn, item)
    ? turn.responseDurationMs
    : undefined;
  const laneId = typeof item.laneId === 'string' ? item.laneId.trim() : '';
  const laneMeta = laneId ? laneMetaById.get(laneId) : undefined;
  return {
    id: artifactId,
    role: resolveMessageRole(item),
    source: resolveMessageSource(item),
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
      ...(laneMeta?.title ? { laneTitle: laneMeta.title } : {}),
      ...(laneMeta?.status ? { laneStatus: laneMeta.status } : {}),
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

function buildArtifact(
  turn: CanonicalTurn,
  item: CanonicalTurnItem,
  laneMetaById: Map<string, LaneProjectionMeta>,
): TimelineProjectionArtifact | null {
  if (!shouldRenderItem(item)) {
    return null;
  }
  const artifactId = resolveArtifactId(turn, item);
  const worker = resolveVisibleWorkerId(item);
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
    message: buildMessage(turn, item, artifactId, laneMetaById),
  };
}

function canonicalStatusToWorkerLaneStatus(status: CanonicalTurnItemStatus): WorkerLaneStatus {
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

function mergeLaneStatus(current: WorkerLaneStatus, next: WorkerLaneStatus): WorkerLaneStatus {
  if (next === 'failed' || current === 'failed') return 'failed';
  if (next === 'blocked' || current === 'blocked') return 'blocked';
  if (next === 'cancelled' || current === 'cancelled') return 'cancelled';
  if (next === 'running' || current === 'running') return 'running';
  if (next === 'pending' || current === 'pending') return 'pending';
  return 'completed';
}

function buildDispatchGroupArtifact(turn: CanonicalTurn): TimelineProjectionArtifact | null {
  const dispatchItems = turn.items
    .filter((item) => item.kind === 'worker_dispatch' && typeof item.laneId === 'string' && item.laneId.trim())
    .sort((left, right) => left.itemSeq - right.itemSeq || left.itemId.localeCompare(right.itemId));
  if (dispatchItems.length === 0) {
    return null;
  }

  const laneById = new Map<string, DispatchGroupLane>();
  const laneVersionById = new Map<string, number>();
  const laneActivityById = new Map<string, LaneActivitySnapshot>();
  for (const item of dispatchItems) {
    const laneId = item.laneId?.trim();
    if (!laneId) {
      continue;
    }
    const worker = normalizeWorkerId(item) || 'orchestrator';
    const title = (item.title || item.worker?.title || laneId).trim();
    const status = canonicalStatusToWorkerLaneStatus(item.status);
    laneById.set(laneId, {
      laneId,
      laneVersion: item.itemSeq,
      worker,
      title,
      description: item.content || title,
      status,
      startedAt: item.createdAt,
      ...(isCanonicalTerminalStatus(item.status) ? { endedAt: item.updatedAt } : {}),
      jumpTarget: { workerTabId: worker },
    });
    laneVersionById.set(laneId, item.itemSeq);
  }

  for (const item of turn.items) {
    const laneId = item.laneId?.trim();
    if (!laneId || !laneById.has(laneId)) {
      continue;
    }
    const lane = laneById.get(laneId)!;
    lane.laneVersion = Math.max(lane.laneVersion, item.itemSeq);
    laneVersionById.set(laneId, lane.laneVersion);
    if (item.kind === 'tool_call') {
      lane.toolUseCount = (lane.toolUseCount || 0) + 1;
    }
    const activityText = summarizeLaneItem(item);
    if (activityText) {
      const currentActivity = laneActivityById.get(laneId);
      if (!currentActivity || item.itemSeq >= currentActivity.itemSeq) {
        laneActivityById.set(laneId, { text: activityText, itemSeq: item.itemSeq });
      }
    }
    if (isCanonicalTerminalStatus(item.status)) {
      lane.endedAt = item.updatedAt;
    }
  }

  for (const [laneId, activity] of laneActivityById.entries()) {
    const lane = laneById.get(laneId);
    if (!lane) {
      continue;
    }
    if (lane.status === 'running' || lane.status === 'pending') {
      lane.liveActivity = activity.text;
    } else {
      lane.summary = activity.text;
    }
  }

  const lanes = Array.from(laneById.values())
    .sort((left, right) => {
      const leftSeq = dispatchItems.find((item) => item.laneId === left.laneId)?.laneSeq ?? Number.MAX_SAFE_INTEGER;
      const rightSeq = dispatchItems.find((item) => item.laneId === right.laneId)?.laneSeq ?? Number.MAX_SAFE_INTEGER;
      return leftSeq - rightSeq || left.laneId.localeCompare(right.laneId);
    })
    .map((lane) => {
      const totalTaskCount = 1;
      return {
        ...lane,
        progressSummary: {
          totalTaskCount,
          completedTaskCount: lane.status === 'completed' ? 1 : 0,
          blockedTaskCount: lane.status === 'blocked' ? 1 : 0,
          awaitingApprovalTaskCount: lane.status === 'awaiting_approval' ? 1 : 0,
          reviewRequiredTaskCount: lane.status === 'review_required' ? 1 : 0,
        },
        tasks: [{
          taskId: dispatchItems.find((item) => item.laneId === lane.laneId)?.worker?.taskId,
          title: lane.title,
          status: lane.status,
          isCurrent: lane.status === 'running' || lane.status === 'pending',
          seq: dispatchItems.find((item) => item.laneId === lane.laneId)?.laneSeq,
        }],
      };
    });
  if (lanes.length === 0) {
    return null;
  }

  const groupStatus = lanes.reduce<WorkerLaneStatus>(
    (status, lane) => mergeLaneStatus(status, lane.status),
    'completed',
  );
  const firstItem = dispatchItems[0];
  const artifactId = `turn:${turn.turnId}:worker-dispatch-group`;
  const responseDurationMs = resolveDispatchGroupResponseDurationMs(turn);
  const block: ContentBlock = {
    type: 'dispatch_group',
    content: '',
    blockId: `dispatch-group:${turn.turnId}`,
    dispatchWaveId: turn.turnId,
    status: groupStatus,
    lanes,
  };
  const message: Message = {
    id: artifactId,
    role: 'assistant',
    source: 'orchestrator',
    content: '',
    blocks: [block],
    timestamp: firstItem.createdAt,
    updatedAt: Math.max(...dispatchItems.map((item) => item.updatedAt || item.createdAt)),
    isStreaming: groupStatus === 'running' || groupStatus === 'pending',
    isComplete: groupStatus !== 'running' && groupStatus !== 'pending',
    type: 'text',
    metadata: {
      turnId: turn.turnId,
      turnSeq: turn.turnSeq,
      turnStatus: turn.status,
      turnItemId: artifactId,
      turnItemKind: 'worker_dispatch',
      turnItemStatus: groupStatus,
      itemSeq: firstItem.itemSeq,
      blockSeq: firstItem.itemSeq,
      cardStreamSeq: firstItem.itemSeq,
      dispatchWaveId: turn.turnId,
      ...(responseDurationMs !== undefined ? { responseDurationMs } : {}),
      canonical: true,
    },
  };

  return {
    artifactId,
    kind: 'message',
    displayOrder: turn.turnSeq * 1000 + firstItem.itemSeq,
    artifactVersion: Math.max(...Array.from(laneVersionById.values())),
    anchorEventSeq: 0,
    latestEventSeq: 0,
    cardStreamSeq: firstItem.itemSeq,
    timestamp: firstItem.createdAt,
    cardId: artifactId,
    dispatchWaveId: turn.turnId,
    threadVisible: true,
    workerTabs: [],
    messageIds: [artifactId, ...dispatchItems.map((item) => item.itemId)],
    message,
  };
}

function buildTurnProjectionArtifacts(turn: CanonicalTurn): Array<TimelineProjectionArtifact | null> {
  const laneMetaById = collectLaneProjectionMeta(turn);
  return [
    buildDispatchGroupArtifact(turn),
    ...turn.items.map((item) => buildArtifact(turn, item, laneMetaById)),
  ];
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
    .flatMap((turn) => buildTurnProjectionArtifacts(turn))
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
