import type { TaskSemanticStatus } from '../shared/task-status-semantics';
import type { DispatchGroupLane, WorkerLaneProgressSummary, WorkerLaneTaskItem } from '../types/message';

export type CardWorkerStatus = TaskSemanticStatus;

export interface WorkerTaskCardData {
  title?: string;
  instruction?: string;
  status?: CardWorkerStatus;
  description?: string;
  executor?: string;
  agent?: string;
  worker?: string;
  workerTabId?: string;
  duration?: number | string;
  startedAt?: number;
  toolCount?: number;
  sessionId?: string;
  isResumed?: boolean;
  dispatchWaveId?: string;
  laneId?: string;
  waveIndex?: number;
  laneIndex?: number;
  laneTotal?: number;
  liveActivity?: string;
  toolUseCount?: number;
  progressSummary?: WorkerLaneProgressSummary;
  taskQueue?: WorkerLaneTaskItem[];
  summary?: string;
  fileChangeCount?: number;
}

function normalizeCardText(value: unknown): string {
  return typeof value === 'string' ? value.trim() : '';
}

const WORKER_LANE_STATUS_PRIORITY: Record<DispatchGroupLane['status'], number> = {
  failed: 80,
  blocked: 70,
  awaiting_approval: 60,
  review_required: 50,
  running: 40,
  pending: 30,
  cancelled: 20,
  completed: 10,
};

function mergeWorkerLaneStatus(
  left: DispatchGroupLane['status'],
  right: DispatchGroupLane['status'],
): DispatchGroupLane['status'] {
  return WORKER_LANE_STATUS_PRIORITY[right] > WORKER_LANE_STATUS_PRIORITY[left] ? right : left;
}

function laneTaskFallback(lane: DispatchGroupLane, index: number): WorkerLaneTaskItem {
  return {
    title: normalizeCardText(lane.title) || normalizeCardText(lane.description) || normalizeCardText(lane.worker) || '任务',
    status: lane.status,
    isCurrent: lane.status === 'running' || lane.status === 'pending',
    seq: index,
  };
}

function normalizeLaneTaskQueue(lane: DispatchGroupLane, index: number): WorkerLaneTaskItem[] {
  const fallback = laneTaskFallback(lane, index);
  const tasks = Array.isArray(lane.tasks) && lane.tasks.length > 0
    ? lane.tasks
    : [fallback];
  return tasks.map((task, taskIndex) => ({
    ...task,
    title: normalizeCardText(task.title) || fallback.title,
    status: task.status || lane.status,
    seq: typeof task.seq === 'number' && Number.isFinite(task.seq)
      ? task.seq
      : index * 100 + taskIndex,
  }));
}

function summarizeTaskQueue(tasks: WorkerLaneTaskItem[]): WorkerLaneProgressSummary {
  return {
    totalTaskCount: tasks.length,
    completedTaskCount: tasks.filter((task) => task.status === 'completed').length,
    blockedTaskCount: tasks.filter((task) => task.status === 'blocked').length,
    awaitingApprovalTaskCount: tasks.filter((task) => task.status === 'awaiting_approval').length,
    reviewRequiredTaskCount: tasks.filter((task) => task.status === 'review_required').length,
  };
}

export function mergeDispatchLanesByWorkerTab(lanes: DispatchGroupLane[]): DispatchGroupLane[] {
  const grouped = new Map<string, DispatchGroupLane>();
  for (const [index, lane] of lanes.entries()) {
    const workerTabId = normalizeCardText(lane.jumpTarget?.workerTabId);
    const workerId = workerTabId || normalizeCardText(lane.worker);
    if (!workerId) {
      continue;
    }
    const tasks = normalizeLaneTaskQueue(lane, index);
    const existing = grouped.get(workerId);
    if (!existing) {
      grouped.set(workerId, {
        ...lane,
        laneId: `worker-role:${workerId}`,
        worker: workerId,
        title: normalizeCardText(lane.title) || tasks[0]?.title || workerId,
        description: tasks.length > 1 ? 'worker_task_queue' : lane.description,
        tasks,
        progressSummary: summarizeTaskQueue(tasks),
        jumpTarget: { workerTabId: workerId },
      });
      continue;
    }

    const nextTasks = [...(existing.tasks ?? []), ...tasks];
    grouped.set(workerId, {
      ...existing,
      laneVersion: Math.max(existing.laneVersion, lane.laneVersion),
      title: normalizeCardText(existing.title) || normalizeCardText(lane.title) || nextTasks[0]?.title || workerId,
      description: nextTasks.length > 1 ? 'worker_task_queue' : (existing.description || lane.description),
      status: mergeWorkerLaneStatus(existing.status, lane.status),
      startedAt: Math.min(existing.startedAt ?? lane.startedAt ?? 0, lane.startedAt ?? existing.startedAt ?? 0) || undefined,
      endedAt: Math.max(existing.endedAt ?? lane.endedAt ?? 0, lane.endedAt ?? existing.endedAt ?? 0) || undefined,
      liveActivity: normalizeCardText(lane.liveActivity) || existing.liveActivity,
      toolUseCount: (existing.toolUseCount ?? 0) + (lane.toolUseCount ?? 0) || undefined,
      tasks: nextTasks,
      progressSummary: summarizeTaskQueue(nextTasks),
      summary: existing.summary || lane.summary,
      fileChangeCount: (existing.fileChangeCount ?? 0) + (lane.fileChangeCount ?? 0) || undefined,
    });
  }
  return [...grouped.values()];
}

export function buildDispatchLaneCardData(
  lane: DispatchGroupLane,
  dispatchWaveId?: string,
): WorkerTaskCardData {
  const description = normalizeCardText(lane.description);
  const workerTabId = normalizeCardText(lane.jumpTarget?.workerTabId);
  const worker = workerTabId || normalizeCardText(lane.worker);
  const title = normalizeCardText(lane.title)
    || description
    || worker;
  const liveActivity = normalizeCardText(lane.liveActivity);

  return {
    title,
    ...(description ? {
      instruction: description,
      description,
    } : {}),
    worker: worker || undefined,
    workerTabId: workerTabId || worker || undefined,
    status: lane.status,
    startedAt: lane.startedAt,
    dispatchWaveId,
    laneId: lane.laneId,
    ...(liveActivity ? { liveActivity } : {}),
    ...(typeof lane.toolUseCount === 'number' && Number.isFinite(lane.toolUseCount) && lane.toolUseCount > 0
      ? { toolUseCount: Math.floor(lane.toolUseCount) }
      : {}),
    ...(lane.progressSummary ? { progressSummary: lane.progressSummary } : {}),
    ...(Array.isArray(lane.tasks) && lane.tasks.length > 0 ? { taskQueue: lane.tasks } : {}),
    ...(normalizeCardText(lane.summary) ? { summary: normalizeCardText(lane.summary) } : {}),
    ...(typeof lane.fileChangeCount === 'number' && Number.isFinite(lane.fileChangeCount) && lane.fileChangeCount > 0
      ? { fileChangeCount: Math.floor(lane.fileChangeCount) }
      : {}),
  };
}



export function resolveDispatchLaneMessageTimestamp(
  lane: DispatchGroupLane,
  fallback: number,
): number {
  if (typeof lane.endedAt === 'number' && Number.isFinite(lane.endedAt) && lane.endedAt > 0) {
    return Math.floor(lane.endedAt);
  }
  if (typeof lane.startedAt === 'number' && Number.isFinite(lane.startedAt) && lane.startedAt > 0) {
    return Math.floor(lane.startedAt);
  }
  return fallback;
}
