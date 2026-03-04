import type { WorkerSlot } from '../../types';

interface ResumeExecutionContext {
  sessionId: string;
  sourceMissionId: string;
  resumePrompt?: string;
  workerSessionBySlot: Map<WorkerSlot, string>;
  createdAt: number;
}

export class DispatchResumeContextStore {
  private missionWorkerSessions = new Map<string, Map<WorkerSlot, string>>();
  private activeResumeContexts = new Map<string, ResumeExecutionContext>();

  constructor(private readonly maxMissionSessionRecords: number) {}

  activate(
    currentSessionId: string,
    sourceMissionId: string,
    resumePrompt?: string,
  ): { ok: true; workerCount: number } | { ok: false } {
    const workerSessions = this.missionWorkerSessions.get(sourceMissionId);
    if (!workerSessions || workerSessions.size === 0) {
      return { ok: false };
    }

    this.activeResumeContexts.set(currentSessionId, {
      sessionId: currentSessionId,
      sourceMissionId,
      resumePrompt,
      workerSessionBySlot: new Map(workerSessions),
      createdAt: Date.now(),
    });

    return { ok: true, workerCount: workerSessions.size };
  }

  clear(currentSessionId?: string): void {
    if (!currentSessionId) {
      this.activeResumeContexts.clear();
      return;
    }
    this.activeResumeContexts.delete(currentSessionId);
  }

  getForWorker(
    currentSessionId: string,
    worker: WorkerSlot,
  ): { resumeSessionId?: string; resumePrompt?: string } {
    const context = this.activeResumeContexts.get(currentSessionId);
    if (!context) {
      return {};
    }
    const resumeSessionId = context.workerSessionBySlot.get(worker);
    if (!resumeSessionId) {
      return {};
    }
    return {
      resumeSessionId,
      resumePrompt: context.resumePrompt,
    };
  }

  recordWorkerSession(
    missionId: string,
    worker: WorkerSlot,
    workerSessionId: string,
  ): void {
    if (!missionId || !workerSessionId) {
      return;
    }

    const existing = this.missionWorkerSessions.get(missionId) || new Map<WorkerSlot, string>();
    existing.set(worker, workerSessionId);
    this.missionWorkerSessions.set(missionId, existing);

    if (this.missionWorkerSessions.size > this.maxMissionSessionRecords) {
      const oldestMissionId = this.missionWorkerSessions.keys().next().value as string | undefined;
      if (oldestMissionId) {
        this.missionWorkerSessions.delete(oldestMissionId);
      }
    }
  }

  dispose(): void {
    this.activeResumeContexts.clear();
    this.missionWorkerSessions.clear();
  }
}

