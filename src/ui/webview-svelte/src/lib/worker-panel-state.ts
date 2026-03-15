import type { AgentType, Message, Task } from '../types/message';

export interface WorkerPanelState {
  latestRoundAnchorMessage: Message | null;
  latestInstructionMessage: Message | null;
  latestRunningInstructionMessage: Message | null;
  latestRoundRequestId: string | null;
  panelHasPendingRequest: boolean;
  hasBottomStreamingMessage: boolean;
  workerHasCurrentRequestActivity: boolean;
}

export type WorkerRuntimeStatus = 'idle' | 'pending' | 'running' | 'blocked' | 'failed' | 'completed';
export type WorkerRuntimeSource = 'tasks' | 'none';

export interface WorkerRuntimeState {
  worker?: AgentType;
  status: WorkerRuntimeStatus;
  source: WorkerRuntimeSource;
  isExecuting: boolean;
  hasPendingRequest: boolean;
  hasStreaming: boolean;
  lastOutputAt: number | null;
  lastInstructionAt: number | null;
  timerStartAt: number | null;
}

interface DeriveWorkerPanelStateParams {
  messages: Message[];
  workerName?: AgentType;
  pendingRequestIds: Iterable<string>;
  tasks?: Task[];
}

function normalizeWorkerName(workerName: unknown): AgentType | null {
  if (typeof workerName !== 'string') return null;
  const normalized = workerName.trim().toLowerCase();
  if (normalized === 'claude' || normalized === 'codex' || normalized === 'gemini') {
    return normalized;
  }
  return null;
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
  return status === 'pending' || status === 'paused';
}

function getLaneTasks(message: Message): Array<Record<string, unknown>> {
  const laneTasks = message.metadata?.laneTasks;
  return Array.isArray(laneTasks)
    ? laneTasks.filter((task): task is Record<string, unknown> => Boolean(task && typeof task === 'object'))
    : [];
}

function findLatestRunningInstructionMessage(messages: Message[], workerName?: AgentType): Message | null {
  for (let idx = messages.length - 1; idx >= 0; idx -= 1) {
    const message = messages[idx];
    if (message.type !== 'instruction') continue;
    const messageWorker = normalizeWorkerName(message.metadata?.worker);
    if (workerName && messageWorker && messageWorker !== workerName) continue;
    if (getLaneTasks(message).some((task) => task.status === 'running')) {
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
  latestStartedAt: number | null;
}

interface InstructionTaskSnapshot {
  hasAssignments: boolean;
  hasRunning: boolean;
  hasFailed: boolean;
  hasPending: boolean;
}

function snapshotWorkerTasks(tasks: Task[], workerName?: AgentType): WorkerTaskSnapshot {
  const snapshot: WorkerTaskSnapshot = {
    hasAssignments: false,
    hasRunning: false,
    hasBlocked: false,
    hasFailed: false,
    hasPending: false,
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
    hasFailed: false,
    hasPending: false,
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
      case 'failed':
        snapshot.hasFailed = true;
        break;
      case 'pending':
      case 'waiting_deps':
        snapshot.hasPending = true;
        break;
      default:
        break;
    }
  }

  return snapshot;
}

export interface WorkerMessageContext {
  latestRoundAnchorMessage: Message | null;
  latestInstructionMessage: Message | null;
  latestRunningInstructionMessage: Message | null;
  latestWorkerOutputMessage: Message | null;
  latestRoundRequestId: string | null;
  panelHasPendingRequest: boolean;
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
    if (latestInstructionMessage && latestRoundAnchorMessage && latestWorkerOutputMessage) {
      break;
    }
  }

  const latestRunningInstructionMessage = findLatestRunningInstructionMessage(safeMessages, workerName);
  const latestRoundRequestId = getMessageRequestId(latestRoundAnchorMessage);
  const pendingRequestIdSet = pendingRequestIds instanceof Set ? pendingRequestIds : new Set(pendingRequestIds);
  const panelHasPendingRequest = latestRoundRequestId ? pendingRequestIdSet.has(latestRoundRequestId) : false;
  const lastMessage = safeMessages.length > 0 ? safeMessages[safeMessages.length - 1] : null;
  const hasBottomStreamingMessage = Boolean(lastMessage?.isStreaming);

  return {
    latestRoundAnchorMessage,
    latestInstructionMessage,
    latestRunningInstructionMessage,
    latestWorkerOutputMessage,
    latestRoundRequestId,
    panelHasPendingRequest,
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
  const hasAssignments = tasksSnapshot.hasAssignments || instructionSnapshot.hasAssignments;
  const hasRunningTask = tasksSnapshot.hasRunning || instructionSnapshot.hasRunning || Boolean(context.latestRunningInstructionMessage);
  const hasBlocked = tasksSnapshot.hasBlocked;
  const hasFailed = tasksSnapshot.hasFailed || instructionSnapshot.hasFailed;
  const hasPending = tasksSnapshot.hasPending || instructionSnapshot.hasPending;
  const hasStreaming = context.hasBottomStreamingMessage;
  const hasPendingRequest = context.panelHasPendingRequest;

  let status: WorkerRuntimeStatus = 'idle';
  let source: WorkerRuntimeSource = 'none';
  if (hasStreaming || hasRunningTask) {
    status = 'running';
    source = hasAssignments ? 'tasks' : 'none';
  } else if (hasBlocked) {
    status = 'blocked';
    source = hasAssignments ? 'tasks' : 'none';
  } else if (hasFailed) {
    status = 'failed';
    source = hasAssignments ? 'tasks' : 'none';
  } else if (hasPending) {
    status = 'pending';
    source = hasAssignments ? 'tasks' : 'none';
  } else if (hasAssignments) {
    status = 'completed';
    source = 'tasks';
  }

  const lastOutputAt = context.latestWorkerOutputMessage?.timestamp ?? null;
  const lastInstructionAt = context.latestInstructionMessage?.timestamp ?? null;
  const fallbackStartAt = tasksSnapshot.latestStartedAt ?? null;
  const timerStartAt = lastOutputAt ?? lastInstructionAt ?? fallbackStartAt ?? null;

  return {
    worker: worker || undefined,
    status,
    source,
    isExecuting: status === 'running',
    hasPendingRequest,
    hasStreaming,
    lastOutputAt,
    lastInstructionAt,
    timerStartAt,
  };
}

export function getMessageRequestId(message: Message | null | undefined): string | null {
  const requestId = message?.metadata?.requestId;
  if (typeof requestId !== 'string') return null;
  const normalized = requestId.trim();
  return normalized.length > 0 ? normalized : null;
}

export function deriveWorkerPanelState({
  messages,
  workerName,
  pendingRequestIds,
  tasks = [],
}: DeriveWorkerPanelStateParams): WorkerPanelState {
  const context = deriveWorkerMessageContext({ messages, workerName, pendingRequestIds });
  const runtime = deriveWorkerRuntimeState({ messages, workerName, pendingRequestIds, tasks }, context);
  const workerHasCurrentRequestActivity = runtime.isExecuting;

  return {
    latestRoundAnchorMessage: context.latestRoundAnchorMessage,
    latestInstructionMessage: context.latestInstructionMessage,
    latestRunningInstructionMessage: context.latestRunningInstructionMessage,
    latestRoundRequestId: context.latestRoundRequestId,
    panelHasPendingRequest: context.panelHasPendingRequest,
    hasBottomStreamingMessage: context.hasBottomStreamingMessage,
    workerHasCurrentRequestActivity,
  };
}

export function deriveWorkerRuntimeMap(params: {
  pendingRequestIds: Iterable<string>;
  tasks?: Task[];
  messagesByWorker: Record<AgentType, Message[]>;
}): Record<AgentType, WorkerRuntimeState> {
  const { pendingRequestIds, tasks = [], messagesByWorker } = params;
  return {
    claude: deriveWorkerRuntimeState(
      {
        messages: messagesByWorker.claude,
        workerName: 'claude',
        pendingRequestIds,
        tasks,
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
      },
      deriveWorkerMessageContext({
        messages: messagesByWorker.gemini,
        workerName: 'gemini',
        pendingRequestIds,
      })
    ),
  };
}
