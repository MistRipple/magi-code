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

export function buildDispatchLaneCardData(
  lane: DispatchGroupLane,
  dispatchWaveId?: string,
): WorkerTaskCardData {
  const description = normalizeCardText(lane.description);
  // P1 身份契约：worker 身份只认 jumpTarget.workerTabId（即 roleId）。
  // lane.worker 保留为兼容字段，不再参与卡片身份。
  const workerTabId = normalizeCardText(lane.jumpTarget?.workerTabId);
  const worker = workerTabId;
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
    workerTabId: workerTabId || undefined,
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
