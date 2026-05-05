import type { TaskDto, TaskProjectionDto } from '../shared/rust-backend-types';

function hasRootTask(projection: TaskProjectionDto, rootTaskId?: string | null): boolean {
  const explicitRootTaskId = typeof rootTaskId === 'string' ? rootTaskId.trim() : '';
  return Boolean(explicitRootTaskId || projection.root_task.task_id.trim());
}

function hasPendingUserAction(projection: TaskProjectionDto): boolean {
  return projection.pending_decisions.length > 0 || projection.blocked_tasks.length > 0;
}

export function isTaskProjectionAcceptingIntake(
  projection: TaskProjectionDto | null | undefined,
  rootTaskId?: string | null,
): boolean {
  if (!projection || projection.execution_mode !== 'deep' || !hasRootTask(projection, rootTaskId)) {
    return false;
  }
  if (projection.runner_status === 'running' || projection.runner_status === 'blocked') {
    return true;
  }
  if (projection.runner_status === 'completed' || projection.runner_status === 'idle') {
    return false;
  }
  return hasPendingUserAction(projection);
}

export function isTaskIntakeTargetTask(task: TaskDto, projection: TaskProjectionDto): boolean {
  if (task.task_id === projection.root_task.task_id) {
    return true;
  }
  if (task.kind === 'Decision') {
    return true;
  }
  switch (task.status) {
    case 'AwaitingApproval':
    case 'Blocked':
    case 'Failed':
    case 'Repairing':
    case 'Verifying':
    case 'Running':
    case 'Ready':
      return true;
    default:
      return false;
  }
}
