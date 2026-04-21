import type { TaskSemanticStatus } from '../shared/task-status-semantics';
import type { DispatchGroupLane } from '../types/message';

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
}

function normalizeCardText(value: unknown): string {
  return typeof value === 'string' ? value.trim() : '';
}

export function buildDispatchLaneCardData(
  lane: DispatchGroupLane,
  dispatchWaveId?: string,
): WorkerTaskCardData {
  const description = normalizeCardText(lane.description);
  const title = normalizeCardText(lane.title)
    || description
    || normalizeCardText(lane.worker);
  const liveActivity = normalizeCardText(lane.liveActivity);

  return {
    title,
    ...(description ? {
      instruction: description,
      description,
    } : {}),
    worker: normalizeCardText(lane.worker) || undefined,
    workerTabId: normalizeCardText(lane.jumpTarget?.workerTabId) || undefined,
    status: lane.status,
    startedAt: lane.startedAt,
    dispatchWaveId,
    laneId: lane.laneId,
    ...(liveActivity ? { liveActivity } : {}),
    ...(typeof lane.toolUseCount === 'number' && Number.isFinite(lane.toolUseCount) && lane.toolUseCount > 0
      ? { toolUseCount: Math.floor(lane.toolUseCount) }
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
