import type { DispatchGroupLane, WorkerLaneStatus } from '../types/message';
import type { TaskDto, TaskProjectionDto, TaskStatus } from '../shared/rust-backend-types';

export interface TaskOrchestrationPhase {
  key: string;
  taskId: string;
  title: string;
  status: WorkerLaneStatus;
  goal: string;
  actionTitles: string[];
  validationStatus: WorkerLaneStatus | null;
  validationTitle: string;
  executorRoleIds: string[];
  workerTabId: string;
  toolUseCount: number;
  fileChangeCount: number;
  summary: string;
}

export interface TaskOrchestrationViewModel {
  title: string;
  status: WorkerLaneStatus;
  phases: TaskOrchestrationPhase[];
  totalPhaseCount: number;
  completedPhaseCount: number;
  totalActionCount: number;
  workerRoleIds: string[];
}

function normalizeText(value: unknown): string {
  return typeof value === 'string' ? value.trim() : '';
}

function compactText(value: string): string {
  const firstLine = value
    .split(/\n\s*\n/)
    .map((part) => part.trim())
    .find(Boolean) || '';
  return firstLine.length > 140 ? `${firstLine.slice(0, 137)}...` : firstLine;
}

function taskStatusToLaneStatus(status: TaskStatus | undefined): WorkerLaneStatus {
  switch (status) {
    case 'Completed':
    case 'Skipped':
      return 'completed';
    case 'Failed':
      return 'failed';
    case 'Cancelled':
      return 'cancelled';
    case 'Blocked':
      return 'blocked';
    case 'AwaitingApproval':
      return 'awaiting_approval';
    case 'Running':
    case 'Verifying':
    case 'Repairing':
      return 'running';
    case 'Draft':
    case 'Ready':
    default:
      return 'pending';
  }
}

function mergeStatus(current: WorkerLaneStatus, next: WorkerLaneStatus): WorkerLaneStatus {
  if (next === 'failed' || current === 'failed') return 'failed';
  if (next === 'blocked' || current === 'blocked') return 'blocked';
  if (next === 'cancelled' || current === 'cancelled') return 'cancelled';
  if (next === 'awaiting_approval' || current === 'awaiting_approval') return 'awaiting_approval';
  if (next === 'review_required' || current === 'review_required') return 'review_required';
  if (next === 'running' || current === 'running') return 'running';
  if (next === 'pending' || current === 'pending') return 'pending';
  return 'completed';
}

function sortTasksByCreation(left: TaskDto, right: TaskDto): number {
  return left.created_at - right.created_at || left.task_id.localeCompare(right.task_id);
}

function buildChildrenIndex(tasks: TaskDto[]): Map<string, TaskDto[]> {
  const childrenByParent = new Map<string, TaskDto[]>();
  for (const task of tasks) {
    const parentId = normalizeText(task.parent_task_id);
    if (!parentId) continue;
    const siblings = childrenByParent.get(parentId) || [];
    siblings.push(task);
    childrenByParent.set(parentId, siblings);
  }
  for (const siblings of childrenByParent.values()) {
    siblings.sort(sortTasksByCreation);
  }
  return childrenByParent;
}

function collectDescendants(task: TaskDto, childrenByParent: Map<string, TaskDto[]>): TaskDto[] {
  const result: TaskDto[] = [];
  const pending = [...(childrenByParent.get(task.task_id) || [])];
  for (let index = 0; index < pending.length; index += 1) {
    const child = pending[index];
    result.push(child);
    pending.push(...(childrenByParent.get(child.task_id) || []));
  }
  return result;
}

function laneTaskIds(lane: DispatchGroupLane): string[] {
  return Array.isArray(lane.tasks)
    ? lane.tasks.map((task) => normalizeText(task.taskId)).filter(Boolean)
    : [];
}

function buildLaneByTaskId(lanes: DispatchGroupLane[]): Map<string, DispatchGroupLane> {
  const laneByTaskId = new Map<string, DispatchGroupLane>();
  for (const lane of lanes) {
    for (const taskId of laneTaskIds(lane)) {
      laneByTaskId.set(taskId, lane);
    }
  }
  return laneByTaskId;
}

function taskExecutorRoleId(task: TaskDto): string {
  return normalizeText(task.executor_binding?.target_role);
}

function uniqueNonEmpty(values: string[]): string[] {
  return [...new Set(values.map(normalizeText).filter(Boolean))];
}

function summarizePhase(
  phase: TaskDto,
  actions: TaskDto[],
  validations: TaskDto[],
  laneByTaskId: Map<string, DispatchGroupLane>,
): string {
  const actionSummary = actions
    .map((task) => {
      const lane = laneByTaskId.get(task.task_id);
      return compactText(normalizeText(lane?.summary) || normalizeText(lane?.liveActivity));
    })
    .find(Boolean);
  if (actionSummary) return actionSummary;

  const validation = validations.find((task) => task.status === 'Completed') || validations[0];
  if (validation) {
    const validationStatus = taskStatusToLaneStatus(validation.status);
    if (validationStatus === 'completed') return '验证通过';
    if (validationStatus === 'running') return '验证中';
    if (validationStatus === 'failed') return '验证未通过';
  }
  return compactText(phase.goal);
}

function phaseStatus(phase: TaskDto, descendants: TaskDto[]): WorkerLaneStatus {
  const statuses = [phase, ...descendants].map((task) => taskStatusToLaneStatus(task.status));
  return statuses.reduce<WorkerLaneStatus>(mergeStatus, 'completed');
}

export function buildTaskOrchestrationView(
  projection: TaskProjectionDto | null | undefined,
  lanes: DispatchGroupLane[],
): TaskOrchestrationViewModel | null {
  if (!projection || projection.execution_mode !== 'deep') {
    return null;
  }

  const tasks = Array.isArray(projection.tasks) ? projection.tasks : [];
  const childrenByParent = buildChildrenIndex(tasks);
  const rootId = projection.root_task.task_id;
  const phases = (childrenByParent.get(rootId) || [])
    .filter((task) => task.kind === 'Phase')
    .sort(sortTasksByCreation);
  if (phases.length === 0) {
    return null;
  }

  const laneByTaskId = buildLaneByTaskId(lanes);
  const viewPhases = phases.map((phase) => {
    const descendants = collectDescendants(phase, childrenByParent);
    const actions = descendants.filter((task) => task.kind === 'Action').sort(sortTasksByCreation);
    const validations = descendants.filter((task) => task.kind === 'Validation').sort(sortTasksByCreation);
    const validation = validations[validations.length - 1] || null;
    const executorRoleIds = uniqueNonEmpty([
      ...actions.map(taskExecutorRoleId),
      ...validations.map(taskExecutorRoleId),
    ]);
    const relatedTaskIds = [phase.task_id, ...descendants.map((task) => task.task_id)];
    const relatedLanes = uniqueNonEmpty(relatedTaskIds)
      .map((taskId) => laneByTaskId.get(taskId))
      .filter((lane): lane is DispatchGroupLane => Boolean(lane));
    const toolUseCount = relatedLanes.reduce((total, lane) => total + (lane.toolUseCount || 0), 0);
    const fileChangeCount = relatedLanes.reduce((total, lane) => total + (lane.fileChangeCount || 0), 0);
    const jumpLane = relatedLanes.find((lane) => normalizeText(lane.jumpTarget?.workerTabId))
      || relatedLanes[0];

    return {
      key: phase.task_id,
      taskId: phase.task_id,
      title: normalizeText(phase.title) || '任务阶段',
      status: phaseStatus(phase, descendants),
      goal: phase.goal,
      actionTitles: actions.map((task) => normalizeText(task.title)).filter(Boolean),
      validationStatus: validation ? taskStatusToLaneStatus(validation.status) : null,
      validationTitle: validation ? normalizeText(validation.title) : '',
      executorRoleIds,
      workerTabId: executorRoleIds[0]
        || normalizeText(jumpLane?.jumpTarget?.workerTabId)
        || normalizeText(jumpLane?.worker)
        || '',
      toolUseCount,
      fileChangeCount,
      summary: summarizePhase(phase, actions, validations, laneByTaskId),
    };
  });

  const status = taskStatusToLaneStatus(projection.aggregate_status);
  return {
    title: normalizeText(projection.root_task.title) || normalizeText(projection.root_task.goal) || '任务编排',
    status,
    phases: viewPhases,
    totalPhaseCount: viewPhases.length,
    completedPhaseCount: viewPhases.filter((phase) => phase.status === 'completed').length,
    totalActionCount: viewPhases.reduce((total, phase) => total + phase.actionTitles.length, 0),
    workerRoleIds: uniqueNonEmpty(viewPhases.flatMap((phase) => phase.executorRoleIds)),
  };
}
