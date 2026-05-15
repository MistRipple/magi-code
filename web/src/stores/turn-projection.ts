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

function normalizeCanonicalWorkerId(item: CanonicalTurnItem): AgentId | undefined {
  const workerId = typeof item.worker?.workerId === 'string' ? item.worker.workerId.trim() : '';
  return (workerId || undefined) as AgentId | undefined;
}

function normalizeCanonicalRoleId(item: CanonicalTurnItem): AgentId | undefined {
  const roleId = typeof item.worker?.roleId === 'string' ? item.worker.roleId.trim() : '';
  return (roleId || undefined) as AgentId | undefined;
}

function resolveWorkerTabAggregateId(item: CanonicalTurnItem): AgentId | undefined {
  // Tab 聚合键只接受 roleId。worker 实例 id 通过 metadata.workerId 单独暴露，
  // 不再参与 tab 维度，避免 drawer 切换和 tab 聚合混用两根身份。
  return normalizeCanonicalRoleId(item);
}

function isWorkerSidechainItem(item: CanonicalTurnItem): boolean {
  // P7.E：item 是否归属 worker drawer 由 worker.roleId 单一信号决定。
  // 归属 orchestrator thread 的 item 没有 roleId 或 roleId === 'orchestrator'。
  const roleId = typeof item.worker?.roleId === 'string' ? item.worker.roleId.trim() : '';
  return roleId.length > 0 && roleId !== 'orchestrator';
}

function resolveVisibleWorkerTabId(item: CanonicalTurnItem): AgentId | undefined {
  return isWorkerSidechainItem(item) ? resolveWorkerTabAggregateId(item) : undefined;
}

function resolveMessageSource(item: CanonicalTurnItem): Message['source'] {
  if (item.kind === 'user_message') {
    return 'user';
  }
  return resolveVisibleWorkerTabId(item) || 'orchestrator';
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
    id: `tool_call:${tool.callId}`,
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
    const blockId = `thinking:${item.itemId}`;
    return [{
      id: blockId,
      type: 'thinking',
      content,
      thinking: {
        content,
        isComplete: isCanonicalTerminalStatus(item.status),
        blockId,
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
    return isWorkerSidechainItem(item) ? 'system-notice' : 'text';
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
  if (item.kind === 'worker_status') {
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
    && !isWorkerSidechainItem(item)
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
  const workerTabId = resolveVisibleWorkerTabId(item);
  const isWorkerSidechain = isWorkerSidechainItem(item);
  const workerId = isWorkerSidechain ? normalizeCanonicalWorkerId(item) : undefined;
  const roleId = isWorkerSidechain ? normalizeCanonicalRoleId(item) : undefined;
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
      ...(workerId ? { workerId } : {}),
      ...(roleId ? { roleId } : {}),
      ...(workerTabId ? { workerTabId } : {}),
      // P7.E：UI 路由单一信号 —— worker.roleId 区分主线与 worker drawer，
      // sourceThreadId 透传给后续多 thread 视图使用（resume、thread tree、跨 worker 追踪）。
      ...(item.sourceThreadId ? { sourceThreadId: item.sourceThreadId } : {}),
      taskId: item.worker?.taskId,
      toolCallId: item.tool?.callId,
      toolName: item.tool?.name,
      ...(responseDurationMs !== undefined ? { responseDurationMs } : {}),
      canonical: true,
    },
  };
}

function resolveArtifactId(turn: CanonicalTurn, item: CanonicalTurnItem): string {
  // P7：artifact 身份统一用 itemId，所有 item kind（含 tool_call）走同一路径。
  // 历史上 tool_call 曾走 `callId || itemId` 双轨：first emit 时 item.tool 尚未填充会落 itemId，
  // 后续 callId 出现又切到 callId，导致同一工具卡在流式过程中 artifactId 漂移、被 Svelte 当作
  // 两个 entry 销毁重建，引起视觉位置错位。callId 仍保留在 block id 与 metadata.toolCallId
  // 用于工具协议层匹配 tool_result，不再参与 artifact 身份。
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
  const workerTabId = resolveVisibleWorkerTabId(item);
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
    worker: workerTabId,
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
    const worker = resolveWorkerTabAggregateId(item) || 'orchestrator';
    const title = (item.title || item.worker?.title || laneId).trim();
    const status = canonicalStatusToWorkerLaneStatus(item.status);
    laneById.set(laneId, {
      laneId,
      laneVersion: item.itemSeq,
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
    // P2：lane liveActivity / summary 只接受后端显式发出的 `worker_status` 摘要 item，
    // 其它 worker 侧事件（thinking/tool_call/stream）不再参与主线摘要聚合——主线
    // 信息密度由后端控制，drawer 仍保留完整事件流。
    if (item.kind === 'worker_status') {
      const activityText = summarizeLaneItem(item);
      if (activityText) {
        const currentActivity = laneActivityById.get(laneId);
        if (!currentActivity || item.itemSeq >= currentActivity.itemSeq) {
          laneActivityById.set(laneId, { text: activityText, itemSeq: item.itemSeq });
        }
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
  const dispatchBlockId = `dispatch-group:${turn.turnId}`;
  const block: ContentBlock = {
    id: dispatchBlockId,
    type: 'dispatch_group',
    content: '',
    blockId: dispatchBlockId,
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
    artifactId: first.artifactId,
    displayOrder: Math.min(first.displayOrder, latest.displayOrder),
    cardStreamSeq: Math.min(first.cardStreamSeq, latest.cardStreamSeq),
    timestamp: Math.min(first.timestamp, latest.timestamp),
    cardId: first.cardId,
    laneId: first.laneId || latest.laneId,
    worker: first.worker || latest.worker,
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

function resolveArtifactCollapseKey(artifact: TimelineProjectionArtifact): string {
  const metadata = artifact.message.metadata || {};
  const toolCallId = typeof metadata.toolCallId === 'string' ? metadata.toolCallId.trim() : '';
  const turnId = typeof metadata.turnId === 'string' ? metadata.turnId.trim() : '';
  if (artifact.kind === 'tool' && toolCallId && turnId) {
    // artifactId 仍以 itemId 为稳定 UI 身份，避免流式过程中 first emit 无 tool 时发生身份漂移；
    // collapse key 只用于同一 turn 内 tool started/result 分裂 item 的投影归并。
    return `turn:${turnId}:tool-call:${toolCallId}`;
  }
  return artifact.artifactId;
}

function collapseArtifactsByStableCard(
  artifacts: TimelineProjectionArtifact[],
): TimelineProjectionArtifact[] {
  const artifactByCollapseKey = new Map<string, TimelineProjectionArtifact>();
  for (const artifact of artifacts) {
    const collapseKey = resolveArtifactCollapseKey(artifact);
    const existing = artifactByCollapseKey.get(collapseKey);
    artifactByCollapseKey.set(
      collapseKey,
      existing ? mergeDuplicateArtifact(existing, artifact) : artifact,
    );
  }
  return Array.from(artifactByCollapseKey.values()).sort(compareArtifacts);
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
  // P7.E：路由由 artifact.worker 单一信号决定。
  // - artifact.worker 为空 → orchestrator 主线 thread；
  // - artifact.worker 为非空字符串 → 对应 roleId 的 worker drawer。
  const threadRenderEntries = artifacts
    .filter((artifact) => !artifact.worker)
    .map(renderEntry);
  const workerRenderEntries: Record<string, TimelineProjectionRenderEntry[]> = {};
  for (const artifact of artifacts) {
    const workerId = artifact.worker;
    if (!workerId) {
      continue;
    }
    if (!workerRenderEntries[workerId]) {
      workerRenderEntries[workerId] = [];
    }
    workerRenderEntries[workerId].push(renderEntry(artifact));
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
