import type { AgentType, Message, OrchestratorRuntimeState, Task } from '../types/message';

export type WorkerRuntimeStatus = 'idle' | 'pending' | 'running' | 'blocked' | 'failed' | 'completed' | 'cancelled';
export type WorkerRuntimeSource = 'tasks' | 'none';
export type WorkerRuntimeMap = Record<AgentType, WorkerRuntimeState>;

export interface WorkerRuntimeState {
  worker?: AgentType;
  status: WorkerRuntimeStatus;
  source: WorkerRuntimeSource;
  hasPendingRequest: boolean;
  hasStreaming: boolean;
  hasBottomStreamingMessage: boolean;
  timerStartAt: number | null;
  latestRoundAnchorMessageId: string | null;
  activeLifecycleMessageId: string | null;
  bottomStreamingMessageId: string | null;
}

interface DeriveWorkerPanelStateParams {
  messages: Message[];
  workerName?: AgentType;
  pendingRequestIds: Iterable<string>;
  tasks?: Task[];
  runtimeState?: OrchestratorRuntimeState | null;
}

function normalizeWorkerName(workerName: unknown): AgentType | null {
  if (typeof workerName !== 'string') return null;
  const normalized = workerName.trim().toLowerCase();
  if (normalized === 'claude' || normalized === 'codex' || normalized === 'gemini') {
    return normalized;
  }
  return null;
}

export function isWorkerExecutingStatus(status: WorkerRuntimeStatus | null | undefined): boolean {
  return status === 'running';
}

export function selectWorkerRuntime(
  workerRuntimeMap: Partial<WorkerRuntimeMap> | null | undefined,
  workerName?: AgentType | null,
): WorkerRuntimeState | null {
  if (!workerName) return null;
  return workerRuntimeMap?.[workerName] || null;
}

export function selectWorkerRuntimeStatus(
  workerRuntimeMap: Partial<WorkerRuntimeMap> | null | undefined,
  workerName?: AgentType | null,
): WorkerRuntimeStatus {
  return selectWorkerRuntime(workerRuntimeMap, workerName)?.status || 'idle';
}

function isActiveTaskStatus(status: unknown): boolean {
  return status === 'running' || status === 'in_progress';
}

function isBlockedTaskStatus(status: unknown): boolean {
  return status === 'blocked';
}

function isFailedTaskStatus(status: unknown): boolean {
  return status === 'failed';
}

function isPendingTaskStatus(status: unknown): boolean {
  return status === 'pending' || status === 'paused' || status === 'waiting_deps';
}

function isCancelledTaskStatus(status: unknown): boolean {
  return status === 'cancelled';
}

function isCompletedTaskStatus(status: unknown): boolean {
  return status === 'completed' || status === 'skipped';
}

function getLaneTasks(message: Message): Array<Record<string, unknown>> {
  const laneTasks = message.metadata?.laneTasks;
  return Array.isArray(laneTasks)
    ? laneTasks.filter((task): task is Record<string, unknown> => Boolean(task && typeof task === 'object'))
    : [];
}

function resolveLaneTasksRuntimeStatus(laneTasks: Array<Record<string, unknown>>): WorkerRuntimeStatus | null {
  if (laneTasks.length === 0) return null;
  if (laneTasks.some((task) => task.status === 'running')) return 'running';
  if (laneTasks.some((task) => task.status === 'blocked')) return 'blocked';
  if (laneTasks.some((task) => task.status === 'failed')) return 'failed';
  if (laneTasks.some((task) => task.status === 'pending' || task.status === 'paused' || task.status === 'waiting_deps')) {
    return 'pending';
  }
  if (laneTasks.some((task) => task.status === 'cancelled')) {
    return 'cancelled';
  }
  if (laneTasks.some((task) => task.status === 'completed' || task.status === 'skipped')) {
    return 'completed';
  }
  return null;
}

function resolveTaskCardRuntimeStatus(message: Message): WorkerRuntimeStatus | null {
  if (message.type !== 'task_card') return null;
  const subTaskCard = message.metadata?.subTaskCard as { status?: unknown; wait_status?: unknown } | undefined;
  const status = typeof subTaskCard?.wait_status === 'string'
    ? subTaskCard.wait_status.trim()
    : (typeof subTaskCard?.status === 'string' ? subTaskCard.status.trim() : '');
  switch (status) {
    case 'running':
    case 'in_progress':
      return 'running';
    case 'blocked':
      return 'blocked';
    case 'pending':
    case 'paused':
    case 'waiting_deps':
      return 'pending';
    case 'failed':
      return 'failed';
    case 'completed':
    case 'skipped':
      return 'completed';
    case 'cancelled':
      return 'cancelled';
    default:
      return null;
  }
}

function findLatestLifecycleStatus(messages: Message[], workerName?: AgentType): WorkerRuntimeStatus | null {
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

function findLatestRunningLifecycleMessage(messages: Message[], workerName?: AgentType): Message | null {
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

interface WorkerTaskSnapshot {
  hasAssignments: boolean;
  hasRunning: boolean;
  hasBlocked: boolean;
  hasFailed: boolean;
  hasPending: boolean;
  hasCancelled: boolean;
  hasCompleted: boolean;
  latestStartedAt: number | null;
}

interface InstructionTaskSnapshot {
  hasAssignments: boolean;
  hasRunning: boolean;
  hasBlocked: boolean;
  hasFailed: boolean;
  hasPending: boolean;
  hasCancelled: boolean;
  hasCompleted: boolean;
}

interface WorkerAssignmentSnapshot {
  hasAssignments: boolean;
  hasRunning: boolean;
  hasBlocked: boolean;
  hasFailed: boolean;
  hasPending: boolean;
  hasCancelled: boolean;
  hasCompleted: boolean;
}

function resolveLiveTaskSnapshotStatus(
  snapshot: WorkerTaskSnapshot | InstructionTaskSnapshot,
): WorkerRuntimeStatus | null {
  if (snapshot.hasRunning) return 'running';
  if (snapshot.hasBlocked) return 'blocked';
  if (snapshot.hasFailed) return 'failed';
  if (snapshot.hasPending) return 'pending';
  if (snapshot.hasCancelled) return 'cancelled';
  if (snapshot.hasCompleted) return 'completed';
  return null;
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

function snapshotWorkerTasks(tasks: Task[], workerName?: AgentType): WorkerTaskSnapshot {
  const snapshot: WorkerTaskSnapshot = {
    hasAssignments: false,
    hasRunning: false,
    hasBlocked: false,
    hasFailed: false,
    hasPending: false,
    hasCancelled: false,
    hasCompleted: false,
    latestStartedAt: null,
  };
  if (!workerName) return snapshot;
  for (const task of tasks) {
    for (const subTask of task.subTasks || []) {
      if (normalizeWorkerName(subTask.assignedWorker) !== workerName) continue;
      snapshot.hasAssignments = true;
      if (isActiveTaskStatus(subTask.status)) {
        snapshot.hasRunning = true;
      }
      if (isBlockedTaskStatus(subTask.status)) {
        snapshot.hasBlocked = true;
      }
      if (isFailedTaskStatus(subTask.status)) {
        snapshot.hasFailed = true;
      }
      if (isPendingTaskStatus(subTask.status)) {
        snapshot.hasPending = true;
      }
      if (isCancelledTaskStatus(subTask.status)) {
        snapshot.hasCancelled = true;
      }
      if (isCompletedTaskStatus(subTask.status)) {
        snapshot.hasCompleted = true;
      }
      if (typeof subTask.startedAt === 'number') {
        snapshot.latestStartedAt = snapshot.latestStartedAt === null
          ? subTask.startedAt
          : Math.max(snapshot.latestStartedAt, subTask.startedAt);
      }
    }
  }
  return snapshot;
}

function snapshotInstructionLaneTasks(message: Message | null): InstructionTaskSnapshot {
  const snapshot: InstructionTaskSnapshot = {
    hasAssignments: false,
    hasRunning: false,
    hasBlocked: false,
    hasFailed: false,
    hasPending: false,
    hasCancelled: false,
    hasCompleted: false,
  };

  if (!message || message.type !== 'instruction') return snapshot;
  const laneTasks = getLaneTasks(message);
  if (laneTasks.length === 0) return snapshot;

  snapshot.hasAssignments = true;
  for (const task of laneTasks) {
    switch (task.status) {
      case 'running':
        snapshot.hasRunning = true;
        break;
      case 'blocked':
        snapshot.hasBlocked = true;
        break;
      case 'failed':
        snapshot.hasFailed = true;
        break;
      case 'pending':
      case 'paused':
      case 'waiting_deps':
        snapshot.hasPending = true;
        break;
      case 'cancelled':
        snapshot.hasCancelled = true;
        break;
      case 'completed':
      case 'skipped':
        snapshot.hasCompleted = true;
        break;
      default:
        break;
    }
  }

  return snapshot;
}

function createEmptyWorkerAssignmentSnapshot(): WorkerAssignmentSnapshot {
  return {
    hasAssignments: false,
    hasRunning: false,
    hasBlocked: false,
    hasFailed: false,
    hasPending: false,
    hasCancelled: false,
    hasCompleted: false,
  };
}

function snapshotRuntimeAssignments(
  runtimeState: OrchestratorRuntimeState | null | undefined,
  workerName?: AgentType,
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

    const status = typeof assignment.status === 'string'
      ? assignment.status.trim().toLowerCase()
      : '';
    const runningTodos = typeof assignment.runningTodos === 'number' && Number.isFinite(assignment.runningTodos)
      ? assignment.runningTodos
      : 0;
    const failedTodos = typeof assignment.failedTodos === 'number' && Number.isFinite(assignment.failedTodos)
      ? assignment.failedTodos
      : 0;

    if (runningTodos > 0 || status === 'running' || status === 'executing' || status === 'in_progress') {
      snapshot.hasRunning = true;
    }
    if (status === 'blocked') {
      snapshot.hasBlocked = true;
    }
    if (failedTodos > 0 || status === 'failed') {
      snapshot.hasFailed = true;
    }
    if (
      status === 'pending'
      || status === 'paused'
      || status === 'waiting_deps'
      || status === 'ready'
      || status === 'queued'
      || status === 'created'
      || status === 'planning'
    ) {
      snapshot.hasPending = true;
    }
    if (status === 'cancelled') {
      snapshot.hasCancelled = true;
    }
    if (status === 'completed' || status === 'skipped') {
      snapshot.hasCompleted = true;
    }
  }
  return snapshot;
}

function mergeRuntimeSnapshots(
  ...snapshots: Array<WorkerTaskSnapshot | InstructionTaskSnapshot | WorkerAssignmentSnapshot>
): InstructionTaskSnapshot {
  return snapshots.reduce<InstructionTaskSnapshot>((merged, snapshot) => ({
    hasAssignments: merged.hasAssignments || snapshot.hasAssignments,
    hasRunning: merged.hasRunning || snapshot.hasRunning,
    hasBlocked: merged.hasBlocked || snapshot.hasBlocked,
    hasFailed: merged.hasFailed || snapshot.hasFailed,
    hasPending: merged.hasPending || snapshot.hasPending,
    hasCancelled: merged.hasCancelled || snapshot.hasCancelled,
    hasCompleted: merged.hasCompleted || snapshot.hasCompleted,
  }), {
    hasAssignments: false,
    hasRunning: false,
    hasBlocked: false,
    hasFailed: false,
    hasPending: false,
    hasCancelled: false,
    hasCompleted: false,
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
  workerName?: AgentType;
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
  const tasksSnapshot = snapshotWorkerTasks(params.tasks || [], worker || undefined);
  const instructionSnapshot = snapshotInstructionLaneTasks(context.latestInstructionMessage);
  const assignmentSnapshot = snapshotRuntimeAssignments(params.runtimeState, worker || undefined);
  const liveRuntimeSnapshot = mergeRuntimeSnapshots(tasksSnapshot, instructionSnapshot, assignmentSnapshot);
  const liveTaskStatus = resolveLiveTaskSnapshotStatus(liveRuntimeSnapshot);
  const hasStreaming = context.hasAnyStreamingMessage;
  const hasPendingRequest = context.panelHasPendingRequest;
  const hasAssignments = liveRuntimeSnapshot.hasAssignments;
  const staleRunningLifecycle = context.latestLifecycleStatus === 'running'
    && !hasStreaming
    && !hasPendingRequest
    && !liveTaskStatus
    && !liveRuntimeSnapshot.hasRunning
    && !liveRuntimeSnapshot.hasBlocked
    && !liveRuntimeSnapshot.hasPending;
  const latestLifecycleStatus = staleRunningLifecycle ? 'cancelled' : context.latestLifecycleStatus;

  let status: WorkerRuntimeStatus = 'idle';
  let source: WorkerRuntimeSource = 'none';
  if (hasStreaming) {
    status = 'running';
    source = hasAssignments || latestLifecycleStatus ? 'tasks' : 'none';
  } else if (liveTaskStatus) {
    status = liveTaskStatus;
    source = 'tasks';
  } else if (hasPendingRequest) {
    status = 'pending';
    source = 'none';
  } else if (latestLifecycleStatus) {
    status = latestLifecycleStatus;
    source = 'tasks';
  }

  const fallbackStartAt = tasksSnapshot.latestStartedAt ?? null;
  const lifecycleTimerStartAt = resolveLifecycleTimerStartAt(
    context.latestRunningLifecycleMessage
    || context.latestInstructionMessage
    || context.latestRoundAnchorMessage
  );
  const timerStartAt = lifecycleTimerStartAt ?? fallbackStartAt ?? null;
  const bottomStreamingMessageId = context.hasBottomStreamingMessage
    ? (params.messages[params.messages.length - 1]?.id || null)
    : null;
  const activeLifecycleMessageId = (() => {
    if (status === 'running' || status === 'pending' || status === 'blocked') {
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
  tasks?: Task[];
  messagesByWorker: Record<AgentType, Message[]>;
  runtimeState?: OrchestratorRuntimeState | null;
}): WorkerRuntimeMap {
  const { pendingRequestIds, tasks = [], messagesByWorker, runtimeState = null } = params;
  return {
    claude: deriveWorkerRuntimeState(
      {
        messages: messagesByWorker.claude,
        workerName: 'claude',
        pendingRequestIds,
        tasks,
        runtimeState,
      },
      deriveWorkerMessageContext({
        messages: messagesByWorker.claude,
        workerName: 'claude',
        pendingRequestIds,
      })
    ),
    codex: deriveWorkerRuntimeState(
      {
        messages: messagesByWorker.codex,
        workerName: 'codex',
        pendingRequestIds,
        tasks,
        runtimeState,
      },
      deriveWorkerMessageContext({
        messages: messagesByWorker.codex,
        workerName: 'codex',
        pendingRequestIds,
      })
    ),
    gemini: deriveWorkerRuntimeState(
      {
        messages: messagesByWorker.gemini,
        workerName: 'gemini',
        pendingRequestIds,
        tasks,
        runtimeState,
      },
      deriveWorkerMessageContext({
        messages: messagesByWorker.gemini,
        workerName: 'gemini',
        pendingRequestIds,
      })
    ),
  };
}
