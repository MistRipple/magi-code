import type { AgentType, Message, MissionPlan, Task } from '../types/message';

export interface WorkerPanelState {
  latestRoundAnchorMessage: Message | null;
  latestInstructionMessage: Message | null;
  latestRunningInstructionMessage: Message | null;
  latestRoundRequestId: string | null;
  panelHasPendingRequest: boolean;
  hasBottomStreamingMessage: boolean;
  workerHasCurrentRequestActivity: boolean;
}

export interface WorkerActivityState {
  latestInstructionMessage: Message | null;
  latestRunningInstructionMessage: Message | null;
  hasPendingRequest: boolean;
  hasStreaming: boolean;
  isExecuting: boolean;
}

interface DeriveWorkerPanelStateParams {
  messages: Message[];
  workerName?: AgentType;
  pendingRequestIds: Iterable<string>;
  tasks?: Task[];
  missionPlans?: Iterable<MissionPlan>;
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

function hasWorkerPlanAssignments(missionPlans: Iterable<MissionPlan>, workerName?: AgentType): boolean {
  if (!workerName) return false;
  for (const plan of missionPlans) {
    for (const assignment of plan.assignments || []) {
      if (normalizeWorkerName(assignment.workerId) === workerName) {
        return true;
      }
    }
  }
  return false;
}

function hasWorkerTaskAssignments(tasks: Task[], workerName?: AgentType): boolean {
  if (!workerName) return false;
  return tasks.some((task) =>
    (task.subTasks || []).some((subTask) =>
      normalizeWorkerName(subTask.assignedWorker) === workerName
    )
  );
}

function hasWorkerMissionActivity(missionPlans: Iterable<MissionPlan>, workerName?: AgentType): boolean {
  if (!workerName) return false;
  for (const plan of missionPlans) {
    for (const assignment of plan.assignments || []) {
      if (normalizeWorkerName(assignment.workerId) !== workerName) continue;
      if (assignment.status === 'running') {
        return true;
      }
      if ((assignment.todos || []).some((todo) => isActiveTaskStatus(todo.status))) {
        return true;
      }
    }
  }
  return false;
}

function hasWorkerTaskActivity(tasks: Task[], workerName?: AgentType): boolean {
  if (!workerName) return false;
  return tasks.some((task) =>
    (task.subTasks || []).some((subTask) =>
      normalizeWorkerName(subTask.assignedWorker) === workerName && isActiveTaskStatus(subTask.status)
    )
  );
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
  missionPlans = [],
}: DeriveWorkerPanelStateParams): WorkerPanelState {
  const safeMessages = (messages || []).filter((message): message is Message => Boolean(message?.id));
  let latestRoundAnchorMessage: Message | null = null;
  let latestInstructionMessage: Message | null = null;

  for (let idx = safeMessages.length - 1; idx >= 0; idx -= 1) {
    const message = safeMessages[idx];
    if (!latestInstructionMessage && message.type === 'instruction') {
      latestInstructionMessage = message;
    }
    if (!latestRoundAnchorMessage && (message.type === 'instruction' || message.type === 'user_input')) {
      latestRoundAnchorMessage = message;
    }
    if (latestInstructionMessage && latestRoundAnchorMessage) {
      break;
    }
  }

  const latestRunningInstructionMessage = findLatestRunningInstructionMessage(safeMessages, workerName);
  const latestRoundRequestId = getMessageRequestId(latestRoundAnchorMessage);
  const pendingRequestIdSet = pendingRequestIds instanceof Set ? pendingRequestIds : new Set(pendingRequestIds);
  const panelHasPendingRequest = latestRoundRequestId ? pendingRequestIdSet.has(latestRoundRequestId) : false;
  const lastMessage = safeMessages.length > 0 ? safeMessages[safeMessages.length - 1] : null;
  const hasBottomStreamingMessage = Boolean(lastMessage?.isStreaming);
  const missionPlanList = Array.from(missionPlans || []);
  // 以 TaskView 为单一真实来源，missionPlan 仅作为任务尚未落地时的兜底
  const workerHasTaskAssignments = hasWorkerTaskAssignments(tasks, workerName);
  const workerHasPlanAssignments = hasWorkerPlanAssignments(missionPlanList, workerName);
  const workerHasTaskExecution = workerHasTaskAssignments
    ? hasWorkerTaskActivity(tasks, workerName)
    : (workerHasPlanAssignments ? hasWorkerMissionActivity(missionPlanList, workerName) : false);
  const workerHasCurrentRequestActivity = hasBottomStreamingMessage
    || Boolean(latestRunningInstructionMessage)
    || workerHasTaskExecution;

  return {
    latestRoundAnchorMessage,
    latestInstructionMessage,
    latestRunningInstructionMessage,
    latestRoundRequestId,
    panelHasPendingRequest,
    hasBottomStreamingMessage,
    workerHasCurrentRequestActivity,
  };
}

export function deriveWorkerActivityState(params: DeriveWorkerPanelStateParams): WorkerActivityState {
  const panelState = deriveWorkerPanelState(params);
  return {
    latestInstructionMessage: panelState.latestInstructionMessage,
    latestRunningInstructionMessage: panelState.latestRunningInstructionMessage,
    hasPendingRequest: panelState.panelHasPendingRequest,
    hasStreaming: panelState.hasBottomStreamingMessage,
    isExecuting: panelState.workerHasCurrentRequestActivity,
  };
}
