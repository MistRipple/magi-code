import { t } from '../../i18n';
import type { WorkerSlot } from '../../types';
import type { MessageHub, SubTaskCardPayload } from './message-hub';
import { DispatchBatch, type DispatchEntry, type DispatchStatus } from './dispatch-batch';

export interface DispatchPresentationAdapterDeps {
  messageHub: MessageHub;
  getActiveBatch: () => DispatchBatch | null;
  getCurrentTurnId: () => string | null;
  getActiveRoundRequestId?: () => string | undefined;
}

interface WorkerLanePresentationState {
  entries: DispatchEntry[];
  focusTaskId: string;
  focusEntry: DispatchEntry | null;
  lanePayload: Pick<
    SubTaskCardPayload,
    'dispatchWaveId' | 'laneId' | 'workerCardId' | 'laneIndex' | 'laneTotal' | 'laneTaskIds' | 'laneCurrentTaskId' | 'laneTasks' | 'laneTaskCards' | 'timelineAnchorTimestamp'
  >;
}

function normalizePositiveTimestamp(value: number | undefined): number | undefined {
  return typeof value === 'number' && Number.isFinite(value) && value > 0
    ? Math.floor(value)
    : undefined;
}

export class DispatchPresentationAdapter {
  constructor(
    private readonly deps: DispatchPresentationAdapterDeps,
  ) {}

  reportTodoProgress(assignmentId: string, summary: string): void {
    const activeBatch = this.deps.getActiveBatch();
    const entry = activeBatch?.getEntry(assignmentId);
    if (entry && entry.status === 'running') {
      this.emitSubTaskCard({
        id: assignmentId,
        title: entry.taskContract.taskTitle,
        status: 'running',
        worker: entry.worker,
        summary,
        requestId: entry.requestId || activeBatch?.requestId,
      });
    }
  }

  emitSubTaskCard(payload: SubTaskCardPayload): void {
    const activeBatch = this.deps.getActiveBatch();
    const entry = activeBatch?.getEntry(payload.id);
    const rawSessionId = typeof payload.sessionId === 'string' ? payload.sessionId.trim() : '';
    const rawMissionId = typeof payload.missionId === 'string' ? payload.missionId.trim() : '';
    const rawTurnId = typeof payload.turnId === 'string' ? payload.turnId.trim() : '';
    const rawRequestId = typeof payload.requestId === 'string' ? payload.requestId.trim() : '';
    const rawDispatchWaveId = typeof payload.dispatchWaveId === 'string' ? payload.dispatchWaveId.trim() : '';
    const rawAnchorTimestamp = typeof payload.timelineAnchorTimestamp === 'number'
      && Number.isFinite(payload.timelineAnchorTimestamp)
      && payload.timelineAnchorTimestamp > 0
      ? Math.floor(payload.timelineAnchorTimestamp)
      : 0;
    const rawRoundRequestId = typeof this.deps.getActiveRoundRequestId === 'function'
      ? (this.deps.getActiveRoundRequestId()?.trim() || '')
      : '';
    const sessionId = rawSessionId || entry?.trace?.sessionId || activeBatch?.trace?.sessionId || undefined;
    const missionId = rawMissionId
      || entry?.trace?.missionId
      || this.resolveBatchMissionId(activeBatch)
      || this.deps.getCurrentTurnId()
      || undefined;
    // dispatchWaveId 是 Worker 卡片跨波次隔离的核心依据。
    // 如果 batch 不存在（极端边界：batch 已释放后仍发送更新），
    // 用 missionId + timestamp 合成唯一标识，确保不会与其他波次合并。
    const dispatchWaveId = rawDispatchWaveId
      || activeBatch?.id
      || (missionId ? `${missionId}-fallback-${Date.now()}` : undefined);
    const turnId = rawTurnId || this.deps.getCurrentTurnId() || undefined;
    const requestId = rawRequestId || rawRoundRequestId || activeBatch?.requestId || undefined;
    const laneState = activeBatch && entry
      ? this.buildWorkerLaneState(activeBatch, entry.worker, entry.taskId)
      : null;
    const requestAnchorTimestamp = normalizePositiveTimestamp(activeBatch?.timelineAnchorTimestamp) || 0;
    const timelineAnchorTimestamp = rawAnchorTimestamp
      || requestAnchorTimestamp
      || laneState?.lanePayload.timelineAnchorTimestamp
      || undefined;
    this.deps.messageHub.subTaskCard({
      ...payload,
      ...(sessionId ? { sessionId } : {}),
      ...(missionId ? { missionId } : {}),
      ...(dispatchWaveId ? { dispatchWaveId } : {}),
      ...(turnId ? { turnId } : {}),
      ...(requestId ? { requestId } : {}),
      ...(timelineAnchorTimestamp ? { timelineAnchorTimestamp } : {}),
      ...(laneState?.lanePayload || {}),
    });
  }

  emitWorkerLaneInstructionCard(
    entry: DispatchEntry,
    worker: WorkerSlot,
    batch: DispatchBatch | null,
    preferredTaskId?: string,
  ): void {
    const turnId = this.deps.getCurrentTurnId() || undefined;
    const sessionId = entry.trace?.sessionId || batch?.trace?.sessionId || undefined;
    if (!batch) {
      this.deps.messageHub.workerInstruction(worker, entry.taskContract.taskTitle, {
        assignmentId: entry.taskId,
        requestId: entry.requestId,
        ...(sessionId ? { sessionId } : {}),
        ...(turnId ? { turnId } : {}),
      });
      return;
    }

    const laneState = this.buildWorkerLaneState(batch, worker, preferredTaskId || entry.taskId);
    const focusEntry = laneState.focusEntry || entry;
    const missionId = this.resolveBatchMissionId(batch) || batch.id;

    this.deps.messageHub.workerInstruction(
      worker,
      this.buildWorkerLaneInstructionContent(laneState.entries, laneState.focusTaskId),
      {
        assignmentId: focusEntry.taskId,
        ...(sessionId ? { sessionId } : {}),
        missionId,
        requestId: batch.requestId,
        ...(turnId ? { turnId } : {}),
        ...laneState.lanePayload,
      },
    );
  }

  mapDispatchStatusToInitialCardStatus(
    status: DispatchStatus,
  ): 'pending' | 'running' | 'completed' | 'failed' | 'cancelled' | 'skipped' {
    switch (status) {
      case 'waiting_deps':
      case 'pending':
        return 'pending';
      case 'completed':
        return 'completed';
      case 'failed':
        return 'failed';
      case 'skipped':
        return 'skipped';
      case 'cancelled':
        return 'cancelled';
      case 'running':
      default:
        return 'running';
    }
  }

  private resolveWorkerLaneFocusTaskId(entries: DispatchEntry[], preferredTaskId?: string): string {
    const runningEntry = entries.find(item => item.status === 'running');
    if (runningEntry) {
      return runningEntry.taskId;
    }

    if (preferredTaskId && entries.some(item => item.taskId === preferredTaskId)) {
      return preferredTaskId;
    }

    const nextPendingEntry = entries.find(item => item.status === 'pending' || item.status === 'waiting_deps');
    if (nextPendingEntry) {
      return nextPendingEntry.taskId;
    }

    return entries[entries.length - 1]?.taskId || preferredTaskId || '';
  }

  private getWorkerLaneEntries(batch: DispatchBatch, worker: WorkerSlot): DispatchEntry[] {
    return batch
      .getEntries()
      .filter(entry => entry.worker === worker)
      .sort((a, b) => a.createdAt - b.createdAt);
  }

  private getWorkerLaneId(dispatchWaveId: string, worker: WorkerSlot): string {
    return `${dispatchWaveId}:${worker}`;
  }

  private resolveBatchMissionId(batch: DispatchBatch | null): string | undefined {
    const missionId = typeof batch?.trace?.missionId === 'string'
      ? batch.trace.missionId.trim()
      : '';
    return missionId || undefined;
  }

  private getWorkerCardId(dispatchWaveId: string, worker: WorkerSlot): string {
    return `worker-lane-instruction-${dispatchWaveId}-${worker}`;
  }

  private buildWorkerLaneState(
    batch: DispatchBatch,
    worker: WorkerSlot,
    preferredTaskId?: string,
  ): WorkerLanePresentationState {
    const laneEntries = this.getWorkerLaneEntries(batch, worker);
    const laneTaskIds = laneEntries.map((item) => item.taskId);
    const focusTaskId = this.resolveWorkerLaneFocusTaskId(laneEntries, preferredTaskId);
    const focusEntry = laneEntries.find((item) => item.taskId === focusTaskId) || null;
    const currentLaneIndex = laneTaskIds.indexOf(focusTaskId);
    const laneTotal = Math.max(1, laneEntries.length);
    const laneIndex = currentLaneIndex >= 0 ? currentLaneIndex + 1 : laneTotal;
    const dispatchWaveId = batch.id;
    const laneAnchorTimestamp = normalizePositiveTimestamp(batch.timelineAnchorTimestamp) || 0;

    return {
      entries: laneEntries,
      focusTaskId,
      focusEntry,
      lanePayload: {
        dispatchWaveId,
        laneId: this.getWorkerLaneId(dispatchWaveId, worker),
        workerCardId: this.getWorkerCardId(dispatchWaveId, worker),
        laneIndex,
        laneTotal,
        laneTaskIds,
        laneCurrentTaskId: focusTaskId,
        ...(laneAnchorTimestamp > 0 ? { timelineAnchorTimestamp: laneAnchorTimestamp } : {}),
        laneTasks: laneEntries.map((item) => ({
          taskId: item.taskId,
          title: item.taskContract.taskTitle,
          status: item.status,
          dependsOn: item.taskContract.dependsOn,
          isCurrent: item.taskId === focusTaskId,
        })),
        laneTaskCards: this.buildWorkerLaneTaskCards(laneEntries),
      },
    };
  }

  private buildWorkerLaneTaskCards(
    laneEntries: DispatchEntry[],
  ): NonNullable<SubTaskCardPayload['laneTaskCards']> {
    return laneEntries.map((item) => {
      const duration = typeof item.startedAt === 'number' && typeof item.completedAt === 'number'
        && Number.isFinite(item.startedAt) && Number.isFinite(item.completedAt) && item.completedAt >= item.startedAt
        ? Math.floor(item.completedAt - item.startedAt)
        : undefined;
      const primaryError = Array.isArray(item.result?.errors) && item.result?.errors.length > 0
        ? item.result.errors[0]
        : undefined;
      return {
        taskId: item.taskId,
        title: item.taskContract.taskTitle,
        worker: item.worker,
        status: item.status,
        ...(typeof item.result?.summary === 'string' && item.result.summary.trim()
          ? { summary: item.result.summary }
          : {}),
        ...(typeof item.result?.fullSummary === 'string' && item.result.fullSummary.trim()
          ? { fullSummary: item.result.fullSummary }
          : {}),
        ...(typeof primaryError === 'string' && primaryError.trim()
          ? { error: primaryError }
          : {}),
        ...(Array.isArray(item.result?.modifiedFiles) && item.result.modifiedFiles.length > 0
          ? { modifiedFiles: item.result.modifiedFiles }
          : {}),
        ...(typeof duration === 'number' ? { duration } : {}),
      };
    });
  }

  private buildWorkerLaneInstructionContent(entries: DispatchEntry[], currentTaskId: string): string {
    const current = entries.find(entry => entry.taskId === currentTaskId);
    const currentIndex = entries.findIndex(entry => entry.taskId === currentTaskId);
    const laneIndex = currentIndex >= 0 ? currentIndex + 1 : Math.max(entries.length, 1);
    const laneTotal = Math.max(entries.length, 1);
    const list = entries.length > 0 ? entries.map(item => ({
      taskId: item.taskId,
      taskTitle: item.taskContract.taskTitle,
      status: item.status,
      dependsOn: item.taskContract.dependsOn,
    })) : [{
      taskId: currentTaskId,
      taskTitle: current?.taskContract.taskTitle || t('dispatch.lane.unknownTask'),
      status: 'running' as const,
      dependsOn: [] as string[],
    }];

    const lines = [
      t('dispatch.lane.header'),
      t('dispatch.lane.currentTask', { task: current?.taskContract.taskTitle || t('dispatch.lane.unknownTask') }),
      t('dispatch.lane.progress', { laneIndex, laneTotal }),
      t('dispatch.lane.description'),
      '',
      t('dispatch.lane.taskList'),
      ...list.map((item, index) => {
        const dependsText = item.dependsOn.length > 0
          ? t('dispatch.lane.dependsOn', { dependsOn: item.dependsOn.join(', ') })
          : '';
        return `${index + 1}. [${this.getWorkerLaneTaskStatusLabel(item.status)}] ${item.taskTitle}${dependsText}`;
      }),
    ];

    return lines.join('\n');
  }

  private getWorkerLaneTaskStatusLabel(status: DispatchStatus): string {
    switch (status) {
      case 'completed':
        return t('dispatch.lane.status.completed');
      case 'failed':
        return t('dispatch.lane.status.failed');
      case 'skipped':
        return t('dispatch.lane.status.skipped');
      case 'cancelled':
        return t('dispatch.lane.status.cancelled');
      case 'waiting_deps':
        return t('dispatch.lane.status.waitingDeps');
      case 'running':
        return t('dispatch.lane.status.running');
      case 'pending':
      default:
        return t('dispatch.lane.status.pending');
    }
  }
}
