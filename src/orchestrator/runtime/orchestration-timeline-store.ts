import fs from 'fs';
import path from 'path';
import { logger, LogCategory } from '../../logging';
import { SerialAsyncTaskQueue } from '../../utils/async-task-queue';
import {
  normalizeOrchestrationTraceLinks,
  type OrchestrationTraceLinks,
} from '../trace/types';

export type OrchestrationTimelineEntityType =
  | 'request'
  | 'batch'
  | 'plan'
  | 'assignment'
  | 'todo'
  | 'verification'
  | 'mission'
  | 'runtime';

export interface OrchestrationStateDiff {
  entityType: OrchestrationTimelineEntityType;
  entityId: string;
  before?: Record<string, unknown>;
  after?: Record<string, unknown>;
}

export interface OrchestrationTimelineEvent {
  schemaVersion: 'orchestration-timeline.v1';
  eventId: string;
  seq: number;
  timestamp: number;
  type: string;
  summary: string;
  trace: OrchestrationTraceLinks;
  payload?: Record<string, unknown>;
  diffs?: OrchestrationStateDiff[];
}

export interface AppendOrchestrationTimelineEventInput {
  type: string;
  summary: string;
  trace?: Partial<OrchestrationTraceLinks> | null;
  payload?: Record<string, unknown>;
  diffs?: OrchestrationStateDiff[];
  timestamp?: number;
}

export interface OrchestrationTimelineReplayQuery {
  sessionId: string;
  requestId?: string;
  planId?: string;
  missionId?: string;
  batchId?: string;
  assignmentId?: string;
  todoId?: string;
  verificationId?: string;
}

export class OrchestrationTimelineStore {
  private readonly baseDir: string;
  private readonly sequenceCache = new Map<string, number>();
  private readonly appendQueue = new SerialAsyncTaskQueue((filePath, error) => {
    logger.warn('编排时间轴.异步写入失败', {
      filePath,
      error: error instanceof Error ? error.message : String(error),
    }, LogCategory.ORCHESTRATOR);
  });

  constructor(workspaceRoot: string) {
    this.baseDir = path.join(workspaceRoot, '.magi', 'observability', 'timeline');
  }

  append(input: AppendOrchestrationTimelineEventInput): OrchestrationTimelineEvent | null {
    const trace = normalizeOrchestrationTraceLinks(input.trace);
    const sessionId = trace?.sessionId;
    if (!trace || !sessionId) {
      logger.warn('编排时间轴.写入跳过_缺少sessionId', {
        type: input.type,
        trace,
      }, LogCategory.ORCHESTRATOR);
      return null;
    }

    try {
      const filePath = this.getTimelineFilePath(sessionId);
      const seq = this.nextSequence(sessionId, filePath);
      const timestamp = typeof input.timestamp === 'number' && Number.isFinite(input.timestamp)
        ? Math.floor(input.timestamp)
        : Date.now();
      const event: OrchestrationTimelineEvent = {
        schemaVersion: 'orchestration-timeline.v1',
        eventId: `timeline_${timestamp}_${seq}_${Math.random().toString(36).slice(2, 8)}`,
        seq,
        timestamp,
        type: input.type.trim(),
        summary: input.summary.trim(),
        trace,
        payload: input.payload && Object.keys(input.payload).length > 0 ? input.payload : undefined,
        diffs: Array.isArray(input.diffs) && input.diffs.length > 0 ? input.diffs : undefined,
      };
      const line = `${JSON.stringify(event)}\n`;
      this.appendQueue.enqueue(filePath, async () => {
        await fs.promises.mkdir(path.dirname(filePath), { recursive: true });
        await fs.promises.appendFile(filePath, line, 'utf8');
      });
      return event;
    } catch (error) {
      logger.warn('编排时间轴.写入失败', {
        type: input.type,
        error: error instanceof Error ? error.message : String(error),
      }, LogCategory.ORCHESTRATOR);
      return null;
    }
  }

  replay(query: OrchestrationTimelineReplayQuery): OrchestrationTimelineEvent[] {
    const sessionId = typeof query.sessionId === 'string' ? query.sessionId.trim() : '';
    if (!sessionId) {
      return [];
    }

    const filePath = this.getTimelineFilePath(sessionId);
    if (!fs.existsSync(filePath)) {
      return [];
    }

    try {
      const content = fs.readFileSync(filePath, 'utf8');
      return content
        .split('\n')
        .map((line) => line.trim())
        .filter(Boolean)
        .map((line) => JSON.parse(line) as OrchestrationTimelineEvent)
        .filter((event) => this.matchesReplayQuery(event, query))
        .sort((left, right) => (left.seq - right.seq) || (left.timestamp - right.timestamp));
    } catch (error) {
      logger.warn('编排时间轴.回放读取失败', {
        sessionId,
        error: error instanceof Error ? error.message : String(error),
      }, LogCategory.ORCHESTRATOR);
      return [];
    }
  }

  getStoragePath(sessionId: string): string {
    const normalizedSessionId = typeof sessionId === 'string' ? sessionId.trim() : '';
    return this.getTimelineFilePath(normalizedSessionId);
  }

  async flushPendingWrites(): Promise<void> {
    await this.appendQueue.flushAll();
  }

  private nextSequence(sessionId: string, filePath: string): number {
    const cached = this.sequenceCache.get(sessionId);
    if (typeof cached === 'number' && Number.isFinite(cached)) {
      const next = cached + 1;
      this.sequenceCache.set(sessionId, next);
      return next;
    }

    const initial = this.readLastSequence(filePath) + 1;
    this.sequenceCache.set(sessionId, initial);
    return initial;
  }

  private readLastSequence(filePath: string): number {
    if (!fs.existsSync(filePath)) {
      return 0;
    }
    try {
      const content = fs.readFileSync(filePath, 'utf8');
      const lines = content
        .split('\n')
        .map((line) => line.trim())
        .filter(Boolean);
      if (lines.length === 0) {
        return 0;
      }
      const last = JSON.parse(lines[lines.length - 1]) as Partial<OrchestrationTimelineEvent>;
      return typeof last.seq === 'number' && Number.isFinite(last.seq) ? Math.max(0, Math.floor(last.seq)) : 0;
    } catch {
      return 0;
    }
  }

  private matchesReplayQuery(
    event: OrchestrationTimelineEvent,
    query: OrchestrationTimelineReplayQuery,
  ): boolean {
    const checks: Array<[keyof OrchestrationTimelineReplayQuery, string | undefined]> = [
      ['requestId', query.requestId],
      ['planId', query.planId],
      ['missionId', query.missionId],
      ['batchId', query.batchId],
      ['assignmentId', query.assignmentId],
      ['todoId', query.todoId],
      ['verificationId', query.verificationId],
    ];
    for (const [key, expectedValue] of checks) {
      const normalizedExpected = typeof expectedValue === 'string' ? expectedValue.trim() : '';
      if (!normalizedExpected) {
        continue;
      }
      const actual = event.trace[key];
      if (actual !== normalizedExpected) {
        return false;
      }
    }
    return true;
  }

  private getTimelineFilePath(sessionId: string): string {
    return path.join(this.baseDir, `${this.toSafeFileSegment(sessionId || 'unknown-session')}.jsonl`);
  }

  private toSafeFileSegment(value: string): string {
    return value.replace(/[^a-zA-Z0-9._-]/g, '_');
  }
}
