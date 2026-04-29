import type { AgentId, Message, OrchestratorRuntimeState } from '../types/message';
import {
  deriveDisplayTaskStatusFromSemanticStatuses,
  isTaskSemanticPausedStatus,
  isTaskSemanticRunningStatus,
  resolveTaskSemanticStatus,
  type TaskSemanticStatus,
} from '../shared/task-status-semantics';

export type WorkerRuntimeStatus = 'idle' | 'pending' | 'awaiting_approval' | 'review_required' | 'running' | 'blocked' | 'failed' | 'completed' | 'cancelled';
export type WorkerRuntimeSource = 'runtime' | 'none';
export type WorkerRuntimeMap = Record<AgentId, WorkerRuntimeState>;

export interface WorkerRuntimeState {
  worker?: AgentId;
  status: WorkerRuntimeStatus;
  source: WorkerRuntimeSource;
  hasPendingRequest: boolean;
  hasStreaming: boolean;
  hasBottomStreamingMessage: boolean;
  timerStartAt: number | null;
  latestRoundAnchorMessageId: string | null;
  activeLifecycleMessageId: string | null;
  bottomStreamingMessageId: string | null;
  /** 最近活动描述（对标 Claude 的 lastActivity.activityDescription） */
  lastActivityDescription?: string;
  /** 正在执行的工具名称 */
  activeToolName?: string;
  /** 工具调用累计次数 */
  toolUseCount?: number;
}

interface DeriveWorkerPanelStateParams {
  messages: Message[];
  workerName?: AgentId;
  pendingRequestIds: Iterable<string>;
  runtimeState?: OrchestratorRuntimeState | null;
}

function normalizeWorkerName(workerName: unknown): string | null {
  if (typeof workerName !== 'string') return null;
  const normalized = workerName.trim();
  if (!normalized) return null;
  return normalized;
}

export function isWorkerExecutingStatus(status: WorkerRuntimeStatus | null | undefined): boolean {
  return status === 'running' || status === 'pending';
}

export function selectWorkerRuntime(
  workerRuntimeMap: Partial<WorkerRuntimeMap> | null | undefined,
  workerName?: AgentId | null,
): WorkerRuntimeState | null {
  if (!workerName) return null;
  return workerRuntimeMap?.[workerName] || null;
}


function isTerminalWorkerRuntimeStatus(status: WorkerRuntimeStatus | null | undefined): boolean {
  return status === 'completed' || status === 'failed' || status === 'cancelled';
}

function resolveTaskLikeStatus(task: Record<string, unknown> | null | undefined): TaskSemanticStatus | null {
  if (!task || typeof task !== 'object') {
    return null;
  }
  return resolveTaskSemanticStatus(task);
}

function getLaneTasks(message: Message): Array<Record<string, unknown>> {
  const laneTasks = message.metadata?.laneTasks;
  return Array.isArray(laneTasks)
    ? laneTasks.filter((task): task is Record<string, unknown> => Boolean(task && typeof task === 'object'))
    : [];
}

function resolveLaneTasksRuntimeStatus(laneTasks: Array<Record<string, unknown>>): WorkerRuntimeStatus | null {
  if (laneTasks.length === 0) return null;
  return deriveDisplayTaskStatusFromSemanticStatuses(
    laneTasks.map((task) => resolveTaskLikeStatus(task)),
  );
}

function resolveTaskCardRuntimeStatus(message: Message): WorkerRuntimeStatus | null {
  if (message.type !== 'task_card') return null;
  const subTaskCard = message.metadata?.subTaskCard as {
    status?: unknown;
    wait_status?: unknown;
    approvalStatus?: unknown;
    reviewStatus?: unknown;
  } | undefined;
  const semanticStatus = resolveTaskSemanticStatus({
    status: typeof subTaskCard?.wait_status === 'string' ? subTaskCard.wait_status : subTaskCard?.status,
    approvalStatus: subTaskCard?.approvalStatus,
    reviewStatus: subTaskCard?.reviewStatus,
  });
  return deriveDisplayTaskStatusFromSemanticStatuses([semanticStatus]);
}

function findLatestLifecycleStatus(messages: Message[], workerName?: AgentId): WorkerRuntimeStatus | null {
  for (let idx = messages.length - 1; idx >= 0; idx -= 1) {
    const message = messages[idx];
    if (message.type === 'instruction') {
      const messageWorker = normalizeWorkerName(message.metadata?.worker);
      if (workerName && messageWorker && messageWorker !== workerName) continue;
      const status = resolveLaneTasksRuntimeStatus(getLaneTasks(message));
      if (status) return status;
      continue;
    }
    if (message.type !== 'task_card') {
      continue;
    }
    const subTaskCard = message.metadata?.subTaskCard as { worker?: unknown } | undefined;
    const messageWorker = normalizeWorkerName(subTaskCard?.worker || message.metadata?.assignedWorker || message.metadata?.worker);
    if (workerName && messageWorker && messageWorker !== workerName) continue;
    const status = resolveTaskCardRuntimeStatus(message);
    if (status) return status;
  }
  return null;
}

function findLatestRunningLifecycleMessage(messages: Message[], workerName?: AgentId): Message | null {
  for (let idx = messages.length - 1; idx >= 0; idx -= 1) {
    const message = messages[idx];
    if (message.type === 'instruction') {
      const messageWorker = normalizeWorkerName(message.metadata?.worker);
      if (workerName && messageWorker && messageWorker !== workerName) continue;
      if (resolveLaneTasksRuntimeStatus(getLaneTasks(message)) === 'running') {
        return message;
      }
      continue;
    }
    if (message.type !== 'task_card') {
      continue;
    }
    const subTaskCard = message.metadata?.subTaskCard as { worker?: unknown; status?: unknown; wait_status?: unknown } | undefined;
    const messageWorker = normalizeWorkerName(subTaskCard?.worker || message.metadata?.assignedWorker || message.metadata?.worker);
    if (workerName && messageWorker && messageWorker !== workerName) continue;
    if (resolveTaskCardRuntimeStatus(message) === 'running') {
      return message;
    }
  }
  return null;
}

interface InstructionTaskSnapshot {
  hasAssignments: boolean;
  statuses: TaskSemanticStatus[];
}

interface WorkerAssignmentSnapshot {
  hasAssignments: boolean;
  statuses: TaskSemanticStatus[];
}

function resolveAssignmentSemanticStatus(status: unknown): TaskSemanticStatus | null {
  if (typeof status !== 'string') {
    return null;
  }
  const normalized = status.trim().toLowerCase();
  if (!normalized) {
    return null;
  }
  if (
    normalized === 'running'
    || normalized === 'executing'
    || normalized === 'in_progress'
    || normalized === 'verifying'
    || normalized === 'repairing'
  ) {
    return 'running';
  }
  if (normalized === 'awaiting_approval') {
    return 'awaiting_approval';
  }
  if (normalized === 'review_required') {
    return 'review_required';
  }
  if (normalized === 'blocked') {
    return 'blocked';
  }
  if (normalized === 'failed') {
    return 'failed';
  }
  if (
    normalized === 'pending'
    || normalized === 'paused'
    || normalized === 'waiting_deps'
    || normalized === 'queued'
    || normalized === 'created'
    || normalized === 'planning'
    || normalized === 'ready'
  ) {
    return 'pending';
  }
  if (normalized === 'cancelled') {
    return 'cancelled';
  }
  if (normalized === 'completed' || normalized === 'skipped') {
    return normalized === 'skipped' ? 'skipped' : 'completed';
  }
  return null;
}

function snapshotCanonicalWorkerLanes(
  messages: Message[],
  workerName?: AgentId,
): WorkerAssignmentSnapshot {
  const snapshot = createEmptyWorkerAssignmentSnapshot();
  const worker = normalizeWorkerName(workerName);
  if (!worker) return snapshot;

  for (let idx = messages.length - 1; idx >= 0; idx -= 1) {
    const message = messages[idx];
    const lanes = message.metadata?.currentTurnWorkerLanes;
    if (!Array.isArray(lanes) || lanes.length === 0) {
      continue;
    }

    const laneStatuses: TaskSemanticStatus[] = [];
    for (const lane of lanes) {
      if (!lane || typeof lane !== 'object') {
        continue;
      }
      const record = lane as Record<string, unknown>;
      if (record.isPrimary === true) {
        continue;
      }
      const roleId = normalizeWorkerName(record.roleId);
      const laneWorker = normalizeWorkerName(record.worker);
      if (roleId !== worker && laneWorker !== worker) {
        continue;
      }
      const semanticStatus = resolveAssignmentSemanticStatus(record.status);
      if (semanticStatus) {
        laneStatuses.push(semanticStatus);
      }
    }

    if (laneStatuses.length > 0) {
      snapshot.hasAssignments = true;
      snapshot.statuses.push(...laneStatuses);
      return snapshot;
    }
  }

  return snapshot;
}

function resolveLiveTaskSnapshotStatus(
  snapshot: InstructionTaskSnapshot,
): WorkerRuntimeStatus | null {
  return deriveDisplayTaskStatusFromSemanticStatuses(snapshot.statuses);
}

function resolveLifecycleTimerStartAt(message: Message | null): number | null {
  if (!message) return null;
  const metadata = message.metadata && typeof message.metadata === 'object'
    ? message.metadata as Record<string, unknown>
    : null;
  const rawAnchorTimestamp = metadata?.timelineAnchorTimestamp;
  if (typeof rawAnchorTimestamp === 'number' && Number.isFinite(rawAnchorTimestamp) && rawAnchorTimestamp > 0) {
    return Math.floor(rawAnchorTimestamp);
  }
  return typeof message.timestamp === 'number' && Number.isFinite(message.timestamp) && message.timestamp > 0
    ? Math.floor(message.timestamp)
    : null;
}

function snapshotInstructionLaneTasks(message: Message | null): InstructionTaskSnapshot {
  const snapshot: InstructionTaskSnapshot = {
    hasAssignments: false,
    statuses: [],
  };

  if (!message || message.type !== 'instruction') return snapshot;
  const laneTasks = getLaneTasks(message);
  if (laneTasks.length === 0) return snapshot;

  snapshot.hasAssignments = true;
  for (const task of laneTasks) {
    const semanticStatus = resolveTaskLikeStatus(task);
    if (semanticStatus) {
      snapshot.statuses.push(semanticStatus);
    }
  }

  return snapshot;
}

function createEmptyWorkerAssignmentSnapshot(): WorkerAssignmentSnapshot {
  return {
    hasAssignments: false,
    statuses: [],
  };
}

function snapshotRuntimeAssignments(
  runtimeState: OrchestratorRuntimeState | null | undefined,
  workerName?: AgentId,
): WorkerAssignmentSnapshot {
  const snapshot = createEmptyWorkerAssignmentSnapshot();
  if (!workerName) return snapshot;
  const assignments = Array.isArray(runtimeState?.assignments)
    ? runtimeState.assignments
    : [];
  for (const assignment of assignments) {
    if (!assignment || typeof assignment !== 'object') {
      continue;
    }
    const assignmentWorker = normalizeWorkerName(assignment.workerId);
    if (assignmentWorker !== workerName) {
      continue;
    }
    snapshot.hasAssignments = true;

    const semanticStatus = resolveAssignmentSemanticStatus(assignment.status);
    if (semanticStatus) {
      snapshot.statuses.push(semanticStatus);
    }
  }
  return snapshot;
}

function mergeRuntimeSnapshots(
  ...snapshots: Array<InstructionTaskSnapshot | WorkerAssignmentSnapshot>
): InstructionTaskSnapshot {
  return snapshots.reduce<InstructionTaskSnapshot>((merged, snapshot) => ({
    hasAssignments: merged.hasAssignments || snapshot.hasAssignments,
    statuses: merged.statuses.concat(snapshot.statuses),
  }), {
    hasAssignments: false,
    statuses: [],
  });
}

export interface WorkerMessageContext {
  latestRoundAnchorMessage: Message | null;
  latestInstructionMessage: Message | null;
  latestRunningLifecycleMessage: Message | null;
  latestLifecycleStatus: WorkerRuntimeStatus | null;
  latestWorkerOutputMessage: Message | null;
  latestStreamingMessage: Message | null;
  latestRoundRequestId: string | null;
  panelHasPendingRequest: boolean;
  hasAnyStreamingMessage: boolean;
  hasBottomStreamingMessage: boolean;
}

export function deriveWorkerMessageContext({
  messages,
  workerName,
  pendingRequestIds,
}: {
  messages: Message[];
  workerName?: AgentId;
  pendingRequestIds: Iterable<string>;
}): WorkerMessageContext {
  const safeMessages = (messages || []).filter((message): message is Message => Boolean(message?.id));
  let latestRoundAnchorMessage: Message | null = null;
  let latestInstructionMessage: Message | null = null;
  let latestWorkerOutputMessage: Message | null = null;
  let latestStreamingMessage: Message | null = null;

  for (let idx = safeMessages.length - 1; idx >= 0; idx -= 1) {
    const message = safeMessages[idx];
    if (!latestInstructionMessage && message.type === 'instruction') {
      latestInstructionMessage = message;
    }
    if (!latestRoundAnchorMessage && (message.type === 'instruction' || message.type === 'user_input')) {
      latestRoundAnchorMessage = message;
    }
    if (!latestWorkerOutputMessage && workerName && message.source === workerName) {
      latestWorkerOutputMessage = message;
    }
    if (!latestStreamingMessage && message.isStreaming) {
      latestStreamingMessage = message;
    }
    if (latestInstructionMessage && latestRoundAnchorMessage && latestWorkerOutputMessage && latestStreamingMessage) {
      break;
    }
  }

  const latestRunningLifecycleMessage = findLatestRunningLifecycleMessage(safeMessages, workerName);
  const latestLifecycleStatus = findLatestLifecycleStatus(safeMessages, workerName);
  const latestRoundRequestId = getMessageRequestId(latestRoundAnchorMessage);
  const pendingRequestIdSet = pendingRequestIds instanceof Set ? pendingRequestIds : new Set(pendingRequestIds);
  const panelHasPendingRequest = latestRoundRequestId ? pendingRequestIdSet.has(latestRoundRequestId) : false;
  const lastMessage = safeMessages.length > 0 ? safeMessages[safeMessages.length - 1] : null;
  const hasBottomStreamingMessage = Boolean(lastMessage?.isStreaming);
  const hasAnyStreamingMessage = Boolean(latestStreamingMessage);

  return {
    latestRoundAnchorMessage,
    latestInstructionMessage,
    latestRunningLifecycleMessage,
    latestLifecycleStatus,
    latestWorkerOutputMessage,
    latestStreamingMessage,
    latestRoundRequestId,
    panelHasPendingRequest,
    hasAnyStreamingMessage,
    hasBottomStreamingMessage,
  };
}

export function deriveWorkerRuntimeState(
  params: DeriveWorkerPanelStateParams,
  context: WorkerMessageContext,
): WorkerRuntimeState {
  const worker = normalizeWorkerName(params.workerName || '');
  const laneSnapshot = snapshotCanonicalWorkerLanes(params.messages, worker || undefined);
  const instructionSnapshot = snapshotInstructionLaneTasks(context.latestInstructionMessage);
  const assignmentSnapshot = snapshotRuntimeAssignments(params.runtimeState, worker || undefined);
  const liveRuntimeSnapshot = mergeRuntimeSnapshots(laneSnapshot, instructionSnapshot, assignmentSnapshot);
  const liveTaskStatus = resolveLiveTaskSnapshotStatus(liveRuntimeSnapshot);
  const hasStreaming = context.hasAnyStreamingMessage;
  const hasPendingRequest = context.panelHasPendingRequest;
  const hasAssignments = liveRuntimeSnapshot.hasAssignments;
  const hasRunningSemanticStatus = liveRuntimeSnapshot.statuses.some((status) => isTaskSemanticRunningStatus(status));
  const hasPausedSemanticStatus = liveRuntimeSnapshot.statuses.some((status) => isTaskSemanticPausedStatus(status));
  const staleRunningLifecycle = context.latestLifecycleStatus === 'running'
    && !hasStreaming
    && !hasPendingRequest
    && !liveTaskStatus
    && !hasRunningSemanticStatus
    && !hasPausedSemanticStatus;
  const latestLifecycleStatus = staleRunningLifecycle ? null : context.latestLifecycleStatus;

  let status: WorkerRuntimeStatus = 'idle';
  let source: WorkerRuntimeSource = 'none';
  if (hasStreaming && !isTerminalWorkerRuntimeStatus(liveTaskStatus)) {
    status = 'running';
    source = hasAssignments || latestLifecycleStatus ? 'runtime' : 'none';
  } else if (liveTaskStatus) {
    status = liveTaskStatus;
    source = 'runtime';
  } else if (hasPendingRequest) {
    status = 'pending';
    source = 'none';
  } else if (latestLifecycleStatus) {
    status = latestLifecycleStatus;
    source = 'runtime';
  }

  const lifecycleTimerStartAt = resolveLifecycleTimerStartAt(
    context.latestRunningLifecycleMessage
    || context.latestInstructionMessage
    || context.latestRoundAnchorMessage
  );
  const timerStartAt = lifecycleTimerStartAt ?? null;
  const bottomStreamingMessageId = context.hasBottomStreamingMessage
    ? (params.messages[params.messages.length - 1]?.id || null)
    : null;
  const activeLifecycleMessageId = (() => {
    if (status === 'running' || status === 'pending' || status === 'awaiting_approval' || status === 'review_required' || status === 'blocked') {
      return context.latestRunningLifecycleMessage?.id
        || context.latestInstructionMessage?.id
        || null;
    }
    return context.latestRunningLifecycleMessage?.id || null;
  })();

  return {
    worker: worker || undefined,
    status,
    source,
    hasPendingRequest,
    hasStreaming,
    hasBottomStreamingMessage: context.hasBottomStreamingMessage,
    timerStartAt,
    latestRoundAnchorMessageId: context.latestRoundAnchorMessage?.id || null,
    activeLifecycleMessageId,
    bottomStreamingMessageId,
  };
}

export function getMessageRequestId(message: Message | null | undefined): string | null {
  const requestId = message?.metadata?.requestId;
  if (typeof requestId !== 'string') return null;
  const normalized = requestId.trim();
  return normalized.length > 0 ? normalized : null;
}

export function deriveWorkerRuntimeMap(params: {
  pendingRequestIds: Iterable<string>;
  messagesByWorker: Record<string, Message[]>;
  runtimeState?: OrchestratorRuntimeState | null;
}): WorkerRuntimeMap {
  const { pendingRequestIds, messagesByWorker, runtimeState = null } = params;
  const result: WorkerRuntimeMap = {};
  for (const workerId of Object.keys(messagesByWorker)) {
    result[workerId] = deriveWorkerRuntimeState(
      {
        messages: messagesByWorker[workerId] ?? [],
        workerName: workerId,
        pendingRequestIds,
        runtimeState,
      },
      deriveWorkerMessageContext({
        messages: messagesByWorker[workerId] ?? [],
        workerName: workerId,
        pendingRequestIds,
      })
    );
  }
  return result;
}
