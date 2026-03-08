import * as fs from 'fs';
import * as path from 'path';
import { EventEmitter } from 'events';
import { logger, LogCategory } from '../../logging';
import type { UnifiedSessionManager } from '../../session/unified-session-manager';
import type { UnifiedTodo } from '../../todo/types';
import type {
  CreatePlanDraftInput,
  DispatchPlanItemInput,
  PlanAttemptCompleteInput,
  PlanAttemptRecord,
  PlanAttemptScope,
  PlanAttemptStartInput,
  PlanAttemptStatus,
  PlanAttemptTerminalStatus,
  PlanIndexEntry,
  PlanItem,
  PlanItemStatus,
  PlanLedgerSnapshot,
  PlanRecord,
  PlanStatus,
  PlanTodoStatus,
} from './types';

type MissionTerminalStatus = 'completed' | 'failed' | 'cancelled';

interface NormalizedAttemptSelector {
  scope: PlanAttemptScope;
  targetId: string;
  assignmentId?: string;
  todoId?: string;
}

interface AttemptTransitionOptions {
  reason?: string;
  error?: string;
  evidenceIds?: string[];
  metadata?: Record<string, string | number | boolean | null>;
}

interface PlanLedgerEventRecord {
  timestamp: number;
  reason: string;
  sessionId: string;
  planId: string;
  missionId?: string;
  status: PlanStatus;
  version: number;
  itemTotal: number;
  completedItems: number;
  failedItems: number;
  attemptTotal: number;
  inflightAttempts: number;
  failedAttempts: number;
  timeoutAttempts: number;
}

export interface PlanLedgerUpdateEvent {
  sessionId: string;
  planId: string;
  reason: string;
  record: PlanRecord;
}

const TERMINAL_PLAN_STATUSES = new Set<PlanStatus>([
  'completed',
  'failed',
  'cancelled',
  'rejected',
  'superseded',
]);

const PLAN_ATTEMPT_TERMINAL_STATUSES = new Set<PlanAttemptStatus>([
  'succeeded',
  'failed',
  'timeout',
  'cancelled',
]);

const ATTEMPT_TRANSITIONS: Record<PlanAttemptStatus, PlanAttemptStatus[]> = {
  created: ['inflight'],
  inflight: ['succeeded', 'failed', 'timeout', 'cancelled'],
  succeeded: [],
  failed: [],
  timeout: [],
  cancelled: [],
};

export class PlanLedgerService extends EventEmitter {
  private readonly writeQueues = new Map<string, Promise<void>>();
  private readonly indexCache = new Map<string, PlanIndexEntry[]>();
  private readonly planCache = new Map<string, Map<string, PlanRecord>>();
  private readonly sessionCacheAccessOrder = new Map<string, number>();
  private static readonly EVENTS_ROTATE_MAX_BYTES = 5 * 1024 * 1024;
  private static readonly EVENTS_ROTATE_KEEP_FILES = 5;
  private static readonly CACHE_MAX_SESSION_COUNT = 32;
  private static readonly PLAN_CACHE_MAX_PER_SESSION = 200;

  constructor(
    private readonly sessionManager: UnifiedSessionManager,
  ) {
    super();
  }

  async createDraft(input: CreatePlanDraftInput): Promise<PlanRecord> {
    return this.runWithSessionQueue(input.sessionId, async () => {
      this.ensurePlansDir(input.sessionId);

      const index = this.loadIndex(input.sessionId);
      const latestForTurn = index
        .filter((entry) => entry.turnId === input.turnId)
        .sort((a, b) => b.version - a.version)[0];

      const now = Date.now();
      const planId = this.generatePlanId();
      const summary = (input.summary || input.prompt).trim() || '未命名计划';
      const record: PlanRecord = {
        planId,
        sessionId: input.sessionId,
        missionId: input.missionId,
        turnId: input.turnId,
        version: latestForTurn ? latestForTurn.version + 1 : 1,
        parentPlanId: latestForTurn?.planId,
        mode: input.mode,
        status: 'draft',
        source: 'orchestrator',
        promptDigest: this.buildPromptDigest(input.prompt),
        summary,
        analysis: input.analysis?.trim() || undefined,
        acceptanceCriteria: this.normalizeStringArray(input.acceptanceCriteria),
        constraints: this.normalizeStringArray(input.constraints),
        riskLevel: input.riskLevel,
        formattedPlan: input.formattedPlan?.trim() || undefined,
        items: [],
        attempts: [],
        links: {
          assignmentIds: [],
          todoIds: [],
        },
        createdAt: now,
        updatedAt: now,
      };

      if (latestForTurn) {
        this.markSuperseded(input.sessionId, latestForTurn.planId);
      }

      this.persistPlan(record);
      this.emitUpdated(record, 'draft-created');
      return record;
    });
  }

  async markAwaitingConfirmation(sessionId: string, planId: string, formattedPlan?: string): Promise<PlanRecord | null> {
    return this.runWithSessionQueue(sessionId, async () => {
      const record = this.loadPlan(sessionId, planId);
      if (!record || TERMINAL_PLAN_STATUSES.has(record.status)) {
        return null;
      }
      record.status = 'awaiting_confirmation';
      if (formattedPlan && formattedPlan.trim()) {
        record.formattedPlan = formattedPlan.trim();
      }
      record.updatedAt = Date.now();
      this.persistPlan(record);
      this.emitUpdated(record, 'awaiting-confirmation');
      return record;
    });
  }

  async approve(sessionId: string, planId: string, reviewer = 'system:auto', reason?: string): Promise<PlanRecord | null> {
    return this.runWithSessionQueue(sessionId, async () => {
      const record = this.loadPlan(sessionId, planId);
      if (!record || TERMINAL_PLAN_STATUSES.has(record.status)) {
        return null;
      }
      record.status = 'approved';
      record.review = {
        status: 'approved',
        reviewer,
        reason: reason?.trim() || undefined,
        reviewedAt: Date.now(),
      };
      record.updatedAt = Date.now();
      this.persistPlan(record);
      this.emitUpdated(record, 'approved');
      return record;
    });
  }

  async reject(sessionId: string, planId: string, reviewer = 'user', reason?: string): Promise<PlanRecord | null> {
    return this.runWithSessionQueue(sessionId, async () => {
      const record = this.loadPlan(sessionId, planId);
      if (!record || TERMINAL_PLAN_STATUSES.has(record.status)) {
        return null;
      }
      record.status = 'rejected';
      record.review = {
        status: 'rejected',
        reviewer,
        reason: reason?.trim() || '用户拒绝执行计划',
        reviewedAt: Date.now(),
      };
      record.updatedAt = Date.now();
      this.persistPlan(record);
      this.emitUpdated(record, 'rejected');
      return record;
    });
  }

  async markExecuting(sessionId: string, planId: string): Promise<PlanRecord | null> {
    return this.runWithSessionQueue(sessionId, async () => {
      const record = this.loadPlan(sessionId, planId);
      if (!record || TERMINAL_PLAN_STATUSES.has(record.status)) {
        return null;
      }
      record.status = 'executing';
      record.updatedAt = Date.now();
      this.persistPlan(record);
      this.emitUpdated(record, 'executing');
      return record;
    });
  }

  async bindMission(sessionId: string, planId: string, missionId: string): Promise<PlanRecord | null> {
    return this.runWithSessionQueue(sessionId, async () => {
      const record = this.loadPlan(sessionId, planId);
      if (!record) {
        return null;
      }
      record.missionId = missionId;
      record.updatedAt = Date.now();
      this.persistPlan(record);
      this.emitUpdated(record, 'mission-bound');
      return record;
    });
  }

  async startAttempt(sessionId: string, planId: string, input: PlanAttemptStartInput): Promise<PlanRecord | null> {
    return this.runWithSessionQueue(sessionId, async () => {
      const record = this.loadPlan(sessionId, planId);
      if (!record || TERMINAL_PLAN_STATUSES.has(record.status)) {
        return null;
      }

      const normalized = this.normalizeAttemptInput(input);
      const now = Date.now();
      let updated = false;

      let attempt = this.findLatestAttempt(record, normalized, ['created', 'inflight']);
      if (attempt) {
        if (attempt.status === 'created') {
          updated = this.applyAttemptTransition(attempt, 'inflight', {
            reason: normalized.reason,
            metadata: normalized.metadata,
          }) || updated;
        } else {
          updated = this.mergeAttemptAnnotations(attempt, {
            reason: normalized.reason,
            metadata: normalized.metadata,
            updatedAt: now,
          }) || updated;
        }
      } else {
        const sequence = this.nextAttemptSequence(record, normalized);
        attempt = {
          attemptId: this.generateAttemptId(normalized.scope, normalized.targetId, sequence),
          scope: normalized.scope,
          targetId: normalized.targetId,
          assignmentId: normalized.assignmentId,
          todoId: normalized.todoId,
          sequence,
          status: 'created',
          reason: normalized.reason,
          evidenceIds: [],
          metadata: normalized.metadata,
          createdAt: now,
          updatedAt: now,
        };
        updated = this.applyAttemptTransition(attempt, 'inflight', {
          reason: normalized.reason,
          metadata: normalized.metadata,
        }) || updated;
        record.attempts.push(attempt);
        updated = true;
      }

      if (!updated) {
        return record;
      }

      record.updatedAt = now;
      this.persistPlan(record);
      this.emitUpdated(record, 'attempt-started');
      return record;
    });
  }

  async completeLatestAttempt(sessionId: string, planId: string, input: PlanAttemptCompleteInput): Promise<PlanRecord | null> {
    return this.runWithSessionQueue(sessionId, async () => {
      const record = this.loadPlan(sessionId, planId);
      if (!record) {
        return null;
      }

      const normalized = this.normalizeAttemptInput(input);
      const now = Date.now();
      let attempt = this.findLatestAttempt(record, normalized, ['created', 'inflight']);
      let updated = false;

      if (!attempt) {
        const latestAny = this.findLatestAttempt(record, normalized);
        if (latestAny && PLAN_ATTEMPT_TERMINAL_STATUSES.has(latestAny.status)) {
          const merged = this.mergeAttemptAnnotations(latestAny, {
            reason: normalized.reason,
            error: input.error,
            evidenceIds: input.evidenceIds,
            metadata: normalized.metadata,
            updatedAt: now,
          });
          if (latestAny.status !== input.status) {
            logger.warn('计划账本.Attempt.忽略无inflight的终态跳转', {
              attemptId: latestAny.attemptId,
              currentStatus: latestAny.status,
              incomingStatus: input.status,
              scope: normalized.scope,
              targetId: normalized.targetId,
            }, LogCategory.ORCHESTRATOR);
          }
          if (merged) {
            record.updatedAt = now;
            this.persistPlan(record);
            this.emitUpdated(record, 'attempt-terminal-duplicate-merged');
          }
          return record;
        }

        if (!latestAny) {
          const sequence = this.nextAttemptSequence(record, normalized);
          attempt = {
            attemptId: this.generateAttemptId(normalized.scope, normalized.targetId, sequence),
            scope: normalized.scope,
            targetId: normalized.targetId,
            assignmentId: normalized.assignmentId,
            todoId: normalized.todoId,
            sequence,
            status: 'created',
            reason: normalized.reason || 'late-terminal-event:auto-started',
            evidenceIds: [],
            metadata: {
              ...(normalized.metadata || {}),
              autoStarted: true,
            },
            createdAt: now,
            updatedAt: now,
          };
          updated = this.applyAttemptTransition(attempt, 'inflight', {
            reason: attempt.reason,
            metadata: attempt.metadata,
          }) || updated;
          record.attempts.push(attempt);
          updated = true;
        } else {
          attempt = latestAny;
        }
      }

      if (attempt.status === 'created') {
        updated = this.applyAttemptTransition(attempt, 'inflight', {
          reason: normalized.reason,
          metadata: normalized.metadata,
        }) || updated;
      }

      const transitioned = this.applyAttemptTransition(attempt, input.status, {
        reason: normalized.reason,
        error: input.error,
        evidenceIds: input.evidenceIds,
        metadata: normalized.metadata,
      });
      updated = transitioned || updated;

      if (!updated) {
        return record;
      }

      record.updatedAt = now;
      this.persistPlan(record);
      this.emitUpdated(record, `attempt-${input.status}`);
      return record;
    });
  }

  async upsertDispatchItem(sessionId: string, planId: string, input: DispatchPlanItemInput): Promise<PlanRecord | null> {
    return this.runWithSessionQueue(sessionId, async () => {
      const record = this.loadPlan(sessionId, planId);
      if (!record || TERMINAL_PLAN_STATUSES.has(record.status)) {
        return null;
      }

      const now = Date.now();
      const normalizedItemId = input.itemId.trim();
      if (!normalizedItemId) {
        return record;
      }

      const existingIndex = record.items.findIndex((item) => item.itemId === normalizedItemId);
      if (existingIndex >= 0) {
        const existing = record.items[existingIndex];
        record.items[existingIndex] = {
          ...existing,
          title: input.title.trim() || existing.title,
          owner: input.worker,
          category: input.category || existing.category,
          dependsOn: this.normalizeStringArray(input.dependsOn, existing.dependsOn),
          scopeHints: this.normalizeStringArray(input.scopeHints, existing.scopeHints),
          targetFiles: this.normalizeStringArray(input.targetFiles, existing.targetFiles),
          requiresModification: input.requiresModification ?? existing.requiresModification,
          updatedAt: now,
        };
      } else {
        const item: PlanItem = {
          itemId: normalizedItemId,
          title: input.title.trim() || normalizedItemId,
          owner: input.worker,
          category: input.category,
          dependsOn: this.normalizeStringArray(input.dependsOn),
          scopeHints: this.normalizeStringArray(input.scopeHints),
          targetFiles: this.normalizeStringArray(input.targetFiles),
          requiresModification: input.requiresModification,
          status: 'pending',
          progress: 0,
          assignmentId: normalizedItemId,
          todoIds: [],
          todoStatuses: {},
          createdAt: now,
          updatedAt: now,
        };
        record.items.push(item);
      }

      this.addUnique(record.links.assignmentIds, normalizedItemId);
      record.updatedAt = now;
      if (record.status === 'draft') {
        record.status = 'approved';
      }
      this.persistPlan(record);
      this.emitUpdated(record, 'dispatch-item-upserted');
      return record;
    });
  }

  async bindAssignmentTodos(sessionId: string, planId: string, assignmentId: string, todos: UnifiedTodo[]): Promise<PlanRecord | null> {
    return this.runWithSessionQueue(sessionId, async () => {
      const record = this.loadPlan(sessionId, planId);
      if (!record || TERMINAL_PLAN_STATUSES.has(record.status)) {
        return null;
      }

      const normalizedAssignmentId = this.normalizeIdentifier(assignmentId);
      if (!normalizedAssignmentId) {
        return record;
      }

      const item = this.findOrCreateItemByAssignment(record, normalizedAssignmentId);
      item.assignmentId = normalizedAssignmentId;

      for (const todo of todos) {
        const todoId = typeof todo.id === 'string' ? todo.id.trim() : '';
        if (!todoId) continue;
        this.addUnique(item.todoIds, todoId);
        this.addUnique(record.links.todoIds, todoId);
        if (!item.todoStatuses[todoId]) {
          item.todoStatuses[todoId] = this.mapTodoStatus(todo.status);
        }
      }

      item.progress = this.computeItemProgress(item);
      item.status = this.computeItemStatus(item);
      item.updatedAt = Date.now();
      record.updatedAt = item.updatedAt;
      this.persistPlan(record);
      this.emitUpdated(record, 'assignment-todos-bound');
      return record;
    });
  }

  async updateAssignmentStatus(
    sessionId: string,
    planId: string,
    assignmentId: string,
    status: 'pending' | 'running' | 'completed' | 'failed' | 'cancelled',
  ): Promise<PlanRecord | null> {
    return this.runWithSessionQueue(sessionId, async () => {
      const record = this.loadPlan(sessionId, planId);
      if (!record || TERMINAL_PLAN_STATUSES.has(record.status)) {
        return null;
      }

      const normalizedAssignmentId = this.normalizeIdentifier(assignmentId);
      if (!normalizedAssignmentId) {
        return record;
      }

      const item = this.findItemByAssignment(record, normalizedAssignmentId);
      if (!item) {
        return record;
      }

      item.status = this.mapAssignmentStatus(status);
      if (item.status === 'completed') {
        item.progress = 100;
      } else if (item.status === 'failed' || item.status === 'cancelled') {
        item.progress = Math.max(item.progress, 1);
      }
      item.updatedAt = Date.now();
      record.updatedAt = item.updatedAt;
      record.status = this.computePlanStatus(record, record.status === 'executing' ? 'executing' : undefined);
      this.persistPlan(record);
      this.emitUpdated(record, 'assignment-status-updated');
      return record;
    });
  }

  async updateTodoStatus(
    sessionId: string,
    planId: string,
    assignmentId: string,
    todoId: string,
    status: PlanTodoStatus,
  ): Promise<PlanRecord | null> {
    return this.runWithSessionQueue(sessionId, async () => {
      const record = this.loadPlan(sessionId, planId);
      if (!record || TERMINAL_PLAN_STATUSES.has(record.status)) {
        return null;
      }

      const normalizedTodoId = todoId.trim();
      if (!normalizedTodoId) {
        return record;
      }

      const normalizedAssignmentId = this.normalizeIdentifier(assignmentId);
      if (!normalizedAssignmentId) {
        return record;
      }

      const item = this.findItemByAssignment(record, normalizedAssignmentId);
      if (!item) {
        return record;
      }

      this.addUnique(item.todoIds, normalizedTodoId);
      this.addUnique(record.links.todoIds, normalizedTodoId);
      item.todoStatuses[normalizedTodoId] = status;
      item.progress = this.computeItemProgress(item);
      item.status = this.computeItemStatus(item);
      item.updatedAt = Date.now();

      record.updatedAt = item.updatedAt;
      if (record.status === 'approved' || record.status === 'awaiting_confirmation') {
        record.status = 'executing';
      }
      record.status = this.computePlanStatus(record, record.status);
      this.persistPlan(record);
      this.emitUpdated(record, 'todo-status-updated');
      return record;
    });
  }

  async finalizeByMissionStatus(sessionId: string, missionId: string, missionStatus: MissionTerminalStatus): Promise<PlanRecord | null> {
    return this.runWithSessionQueue(sessionId, async () => {
      const index = this.loadIndex(sessionId);
      const matched = index
        .filter((entry) => entry.missionId === missionId)
        .sort((a, b) => b.updatedAt - a.updatedAt)[0];
      if (!matched) {
        return null;
      }

      const record = this.loadPlan(sessionId, matched.planId);
      if (!record) {
        return null;
      }
      if (TERMINAL_PLAN_STATUSES.has(record.status)) {
        return record;
      }

      const attemptTerminated = this.finalizeInflightAttemptsInRecord(
        record,
        this.mapMissionStatusToAttemptTerminalStatus(missionStatus),
        `mission-finalized:${missionStatus}`,
      );
      const inferredStatus = this.mapMissionStatusToPlanStatus(missionStatus, record);
      if (!attemptTerminated && inferredStatus === record.status) {
        return record;
      }
      record.status = inferredStatus;
      record.updatedAt = Date.now();
      this.persistPlan(record);
      this.emitUpdated(record, 'mission-finalized');
      return record;
    });
  }

  async finalize(sessionId: string, planId: string, status: 'completed' | 'failed' | 'cancelled'): Promise<PlanRecord | null> {
    return this.runWithSessionQueue(sessionId, async () => {
      const record = this.loadPlan(sessionId, planId);
      if (!record || TERMINAL_PLAN_STATUSES.has(record.status)) {
        return record;
      }
      const attemptTerminated = this.finalizeInflightAttemptsInRecord(
        record,
        this.mapMissionStatusToAttemptTerminalStatus(status),
        `plan-finalized:${status}`,
      );
      const nextStatus = this.mapMissionStatusToPlanStatus(status, record);
      if (!attemptTerminated && nextStatus === record.status) {
        return record;
      }
      record.status = nextStatus;
      record.updatedAt = Date.now();
      this.persistPlan(record);
      this.emitUpdated(record, 'finalized');
      return record;
    });
  }

  async reconcileByMissions(
    sessionId: string,
    missions: Array<{ id: string; status: string }>,
  ): Promise<number> {
    return this.runWithSessionQueue(sessionId, async () => {
      if (!Array.isArray(missions) || missions.length === 0) {
        return 0;
      }

      const missionTerminalStatusMap = new Map<string, MissionTerminalStatus>();
      for (const mission of missions) {
        const missionId = typeof mission?.id === 'string' ? mission.id.trim() : '';
        if (!missionId) {
          continue;
        }
        const terminalStatus = this.normalizeMissionTerminalStatus(mission.status);
        if (!terminalStatus) {
          continue;
        }
        missionTerminalStatusMap.set(missionId, terminalStatus);
      }

      if (missionTerminalStatusMap.size === 0) {
        return 0;
      }

      const index = this.loadIndex(sessionId).sort((a, b) => b.updatedAt - a.updatedAt);
      let reconciled = 0;

      for (const entry of index) {
        if (TERMINAL_PLAN_STATUSES.has(entry.status)) {
          continue;
        }
        const missionId = typeof entry.missionId === 'string' ? entry.missionId.trim() : '';
        if (!missionId) {
          continue;
        }
        const missionTerminalStatus = missionTerminalStatusMap.get(missionId);
        if (!missionTerminalStatus) {
          continue;
        }

        const record = this.loadPlan(sessionId, entry.planId);
        if (!record || TERMINAL_PLAN_STATUSES.has(record.status)) {
          continue;
        }

        const nextStatus = this.mapMissionStatusToPlanStatus(missionTerminalStatus, record);
        const attemptsTerminated = this.finalizeInflightAttemptsInRecord(
          record,
          this.mapMissionStatusToAttemptTerminalStatus(missionTerminalStatus),
          `reconciled:${missionTerminalStatus}`,
        );
        if (nextStatus === record.status && !attemptsTerminated) {
          continue;
        }

        record.status = nextStatus;
        record.updatedAt = Date.now();
        this.persistPlan(record);
        this.emitUpdated(record, 'reconciled-with-mission');
        reconciled += 1;
      }

      return reconciled;
    });
  }

  getPlan(sessionId: string, planId: string): PlanRecord | null {
    return this.loadPlan(sessionId, planId);
  }

  listPlans(sessionId: string, limit = 20): PlanRecord[] {
    const index = this.loadIndex(sessionId)
      .sort((a, b) => b.updatedAt - a.updatedAt)
      .slice(0, Math.max(1, limit));
    return index
      .map((entry) => this.loadPlan(sessionId, entry.planId))
      .filter((plan): plan is PlanRecord => Boolean(plan));
  }

  getActivePlan(sessionId: string): PlanRecord | null {
    const index = this.loadIndex(sessionId)
      .sort((a, b) => b.updatedAt - a.updatedAt);
    for (const entry of index) {
      if (TERMINAL_PLAN_STATUSES.has(entry.status)) {
        continue;
      }
      const plan = this.loadPlan(sessionId, entry.planId);
      if (plan) {
        return plan;
      }
    }
    return null;
  }

  getLatestPlan(sessionId: string): PlanRecord | null {
    const latest = this.loadIndex(sessionId)
      .sort((a, b) => b.updatedAt - a.updatedAt)[0];
    if (!latest) {
      return null;
    }
    return this.loadPlan(sessionId, latest.planId);
  }

  getSnapshot(sessionId: string, limit = 20): PlanLedgerSnapshot {
    return {
      activePlan: this.getActivePlan(sessionId),
      plans: this.listPlans(sessionId, limit),
    };
  }

  buildActivePlanState(sessionId: string): { planId: string; formattedPlan: string; updatedAt: number; review?: { status: 'approved' | 'rejected' | 'skipped'; summary: string } } | undefined {
    const activePlan = this.getActivePlan(sessionId);
    if (!activePlan) {
      return undefined;
    }
    const review = activePlan.review
      ? {
          status: activePlan.review.status,
          summary: activePlan.review.reason || '',
        }
      : undefined;

    return {
      planId: activePlan.planId,
      formattedPlan: activePlan.formattedPlan || this.formatPlanForDisplay(activePlan),
      updatedAt: activePlan.updatedAt,
      review,
    };
  }

  formatPlanForDisplay(plan: PlanRecord): string {
    const lines: string[] = [];
    lines.push(`## 计划摘要`);
    lines.push(plan.summary || '未命名计划');
    if (plan.analysis) {
      lines.push('');
      lines.push(`### 分析`);
      lines.push(plan.analysis);
    }
    if (plan.constraints.length > 0) {
      lines.push('');
      lines.push('### 约束');
      for (const item of plan.constraints) {
        lines.push(`- ${item}`);
      }
    }
    if (plan.acceptanceCriteria.length > 0) {
      lines.push('');
      lines.push('### 验收');
      for (const item of plan.acceptanceCriteria) {
        lines.push(`- ${item}`);
      }
    }
    if (plan.items.length > 0) {
      lines.push('');
      lines.push('### 任务分解');
      for (const item of plan.items) {
        lines.push(`1. [${item.owner}] ${item.title}`);
      }
    }
    return lines.join('\n');
  }

  private mapMissionStatusToPlanStatus(status: MissionTerminalStatus, record: PlanRecord): PlanStatus {
    if (status === 'cancelled') {
      return 'cancelled';
    }

    const itemTotal = record.items.length;
    if (itemTotal === 0) {
      return status === 'completed' ? 'completed' : 'failed';
    }

    const completedItems = record.items.filter((item) => item.status === 'completed' || item.status === 'skipped').length;
    const failedItems = record.items.filter((item) => item.status === 'failed' || item.status === 'cancelled').length;

    if (status === 'completed') {
      return failedItems > 0 ? 'partially_completed' : 'completed';
    }

    // mission failed
    return completedItems > 0 ? 'partially_completed' : 'failed';
  }

  private mapMissionStatusToAttemptTerminalStatus(status: MissionTerminalStatus): PlanAttemptTerminalStatus {
    if (status === 'completed') {
      return 'cancelled';
    }
    if (status === 'cancelled') {
      return 'cancelled';
    }
    return 'failed';
  }

  private computePlanStatus(record: PlanRecord, fallback?: PlanStatus): PlanStatus {
    const total = record.items.length;
    if (total === 0) {
      return fallback || record.status;
    }

    let completed = 0;
    let failed = 0;
    let running = 0;
    let pending = 0;

    for (const item of record.items) {
      if (item.status === 'completed' || item.status === 'skipped') {
        completed += 1;
      } else if (item.status === 'failed' || item.status === 'cancelled') {
        failed += 1;
      } else if (item.status === 'running') {
        running += 1;
      } else {
        pending += 1;
      }
    }

    if (failed > 0 && completed > 0) {
      return 'partially_completed';
    }
    if (failed > 0 && running === 0 && pending === 0) {
      return 'failed';
    }
    if (completed === total) {
      return 'completed';
    }
    if (running > 0 || completed > 0) {
      return 'executing';
    }
    return fallback || record.status;
  }

  private computeItemProgress(item: PlanItem): number {
    if (item.todoIds.length === 0) {
      if (item.status === 'completed') return 100;
      if (item.status === 'failed' || item.status === 'cancelled') return Math.max(item.progress, 1);
      return item.progress;
    }
    const terminal = new Set<PlanTodoStatus>(['completed', 'failed', 'skipped', 'cancelled']);
    const doneCount = item.todoIds
      .map((todoId) => item.todoStatuses[todoId] || 'pending')
      .filter((status) => terminal.has(status)).length;
    return Math.min(100, Math.round((doneCount / item.todoIds.length) * 100));
  }

  private computeItemStatus(item: PlanItem): PlanItemStatus {
    const statuses = item.todoIds.map((todoId) => item.todoStatuses[todoId] || 'pending');
    if (statuses.length === 0) {
      return item.status;
    }
    if (statuses.some((status) => status === 'failed')) {
      return 'failed';
    }
    if (statuses.every((status) => status === 'completed' || status === 'skipped' || status === 'cancelled')) {
      return 'completed';
    }
    if (statuses.some((status) => status === 'in_progress' || status === 'running')) {
      return 'running';
    }
    return 'pending';
  }

  private mapAssignmentStatus(status: 'pending' | 'running' | 'completed' | 'failed' | 'cancelled'): PlanItemStatus {
    switch (status) {
      case 'running':
        return 'running';
      case 'completed':
        return 'completed';
      case 'failed':
        return 'failed';
      case 'cancelled':
        return 'cancelled';
      default:
        return 'pending';
    }
  }

  private mapTodoStatus(status: string): PlanTodoStatus {
    switch (status) {
      case 'in_progress':
        return 'in_progress';
      case 'running':
        return 'running';
      case 'completed':
        return 'completed';
      case 'failed':
        return 'failed';
      case 'skipped':
        return 'skipped';
      case 'blocked':
        return 'blocked';
      case 'cancelled':
        return 'cancelled';
      default:
        return 'pending';
    }
  }

  private normalizeAttemptStatus(status: unknown): PlanAttemptStatus {
    if (status === 'created' || status === 'inflight' || status === 'succeeded'
      || status === 'failed' || status === 'timeout' || status === 'cancelled') {
      return status;
    }
    return 'created';
  }

  private normalizeAttemptScope(scope: unknown): PlanAttemptScope {
    if (scope === 'assignment' || scope === 'todo') {
      return scope;
    }
    return 'orchestrator';
  }

  private findItemByAssignment(record: PlanRecord, assignmentId: unknown): PlanItem | null {
    const normalized = this.normalizeIdentifier(assignmentId);
    if (!normalized) {
      return null;
    }
    return record.items.find((item) => item.assignmentId === normalized || item.itemId === normalized) || null;
  }

  private findOrCreateItemByAssignment(record: PlanRecord, assignmentId: string): PlanItem {
    const normalizedAssignmentId = this.normalizeIdentifier(assignmentId);
    const existing = this.findItemByAssignment(record, normalizedAssignmentId);
    if (existing) {
      return existing;
    }
    if (!normalizedAssignmentId) {
      throw new Error('findOrCreateItemByAssignment requires non-empty assignmentId');
    }
    const now = Date.now();
    const item: PlanItem = {
      itemId: normalizedAssignmentId,
      title: normalizedAssignmentId,
      owner: 'orchestrator',
      dependsOn: [],
      status: 'pending',
      progress: 0,
      assignmentId: normalizedAssignmentId,
      todoIds: [],
      todoStatuses: {},
      createdAt: now,
      updatedAt: now,
    };
    record.items.push(item);
    this.addUnique(record.links.assignmentIds, normalizedAssignmentId);
    return item;
  }

  private normalizeIdentifier(value: unknown): string {
    return typeof value === 'string' ? value.trim() : '';
  }

  private normalizeAttemptInput(input: PlanAttemptStartInput): NormalizedAttemptSelector & {
    reason?: string;
    metadata?: Record<string, string | number | boolean | null>;
  } {
    const scope: PlanAttemptScope = input.scope;
    const assignmentId = this.normalizeIdentifier(input.assignmentId);
    const todoId = this.normalizeIdentifier(input.todoId);
    const explicitTargetId = this.normalizeIdentifier(input.targetId);

    const targetId = explicitTargetId
      || (scope === 'todo' ? todoId : '')
      || (scope === 'assignment' ? assignmentId : '')
      || 'orchestrator';

    const normalized: NormalizedAttemptSelector & {
      reason?: string;
      metadata?: Record<string, string | number | boolean | null>;
    } = {
      scope,
      targetId,
      assignmentId: assignmentId || undefined,
      todoId: todoId || undefined,
      reason: typeof input.reason === 'string' && input.reason.trim() ? input.reason.trim() : undefined,
      metadata: this.normalizeAttemptMetadata(input.metadata),
    };

    return normalized;
  }

  private normalizeAttemptMetadata(
    metadata?: Record<string, string | number | boolean | null>,
  ): Record<string, string | number | boolean | null> | undefined {
    if (!metadata || typeof metadata !== 'object') {
      return undefined;
    }
    const normalizedEntries = Object.entries(metadata)
      .filter(([key]) => typeof key === 'string' && key.trim().length > 0)
      .map(([key, value]) => [key.trim(), value] as const)
      .filter(([, value]) => (
        typeof value === 'string'
        || typeof value === 'number'
        || typeof value === 'boolean'
        || value === null
      ));
    if (normalizedEntries.length === 0) {
      return undefined;
    }
    return Object.fromEntries(normalizedEntries);
  }

  private normalizeAttemptEvidenceIds(evidenceIds?: string[]): string[] {
    return this.normalizeStringArray(evidenceIds);
  }

  private findLatestAttempt(
    record: PlanRecord,
    selector: NormalizedAttemptSelector,
    statuses?: PlanAttemptStatus[],
  ): PlanAttemptRecord | null {
    const statusSet = statuses ? new Set(statuses) : null;
    let latest: PlanAttemptRecord | null = null;

    for (const attempt of record.attempts) {
      if (attempt.scope !== selector.scope) {
        continue;
      }
      if (attempt.targetId !== selector.targetId) {
        continue;
      }
      if (selector.assignmentId && attempt.assignmentId !== selector.assignmentId) {
        continue;
      }
      if (selector.todoId && attempt.todoId !== selector.todoId) {
        continue;
      }
      if (statusSet && !statusSet.has(attempt.status)) {
        continue;
      }
      if (!latest || attempt.sequence > latest.sequence || attempt.updatedAt > latest.updatedAt) {
        latest = attempt;
      }
    }

    return latest;
  }

  private nextAttemptSequence(record: PlanRecord, selector: NormalizedAttemptSelector): number {
    let maxSequence = 0;
    for (const attempt of record.attempts) {
      if (attempt.scope !== selector.scope || attempt.targetId !== selector.targetId) {
        continue;
      }
      if (selector.assignmentId && attempt.assignmentId !== selector.assignmentId) {
        continue;
      }
      if (selector.todoId && attempt.todoId !== selector.todoId) {
        continue;
      }
      if (attempt.sequence > maxSequence) {
        maxSequence = attempt.sequence;
      }
    }
    return maxSequence + 1;
  }

  private generateAttemptId(scope: PlanAttemptScope, targetId: string, sequence: number): string {
    const safeTarget = targetId.replace(/[^a-zA-Z0-9_-]/g, '_').slice(0, 48) || 'target';
    return `attempt-${scope}-${safeTarget}-${sequence}-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
  }

  private mergeAttemptAnnotations(
    attempt: PlanAttemptRecord,
    options: {
      reason?: string;
      error?: string;
      evidenceIds?: string[];
      metadata?: Record<string, string | number | boolean | null>;
      updatedAt?: number;
    },
  ): boolean {
    let updated = false;
    const now = options.updatedAt || Date.now();

    if (options.reason && options.reason !== attempt.reason) {
      attempt.reason = options.reason;
      updated = true;
    }
    if (options.error && options.error !== attempt.error) {
      attempt.error = options.error;
      updated = true;
    }
    if (options.evidenceIds && options.evidenceIds.length > 0) {
      const mergedEvidenceIds = this.normalizeAttemptEvidenceIds([
        ...attempt.evidenceIds,
        ...options.evidenceIds,
      ]);
      if (mergedEvidenceIds.join('|') !== attempt.evidenceIds.join('|')) {
        attempt.evidenceIds = mergedEvidenceIds;
        updated = true;
      }
    }
    if (options.metadata && Object.keys(options.metadata).length > 0) {
      const nextMetadata = {
        ...(attempt.metadata || {}),
        ...options.metadata,
      };
      const oldKey = JSON.stringify(attempt.metadata || {});
      const nextKey = JSON.stringify(nextMetadata);
      if (oldKey !== nextKey) {
        attempt.metadata = nextMetadata;
        updated = true;
      }
    }
    if (updated) {
      attempt.updatedAt = now;
    }
    return updated;
  }

  private applyAttemptTransition(
    attempt: PlanAttemptRecord,
    nextStatus: PlanAttemptStatus,
    options: AttemptTransitionOptions = {},
  ): boolean {
    if (attempt.status === nextStatus) {
      return this.mergeAttemptAnnotations(attempt, {
        reason: options.reason,
        error: options.error,
        evidenceIds: options.evidenceIds,
        metadata: options.metadata,
      });
    }

    const legalNext = ATTEMPT_TRANSITIONS[attempt.status] || [];
    if (!legalNext.includes(nextStatus)) {
      logger.warn('计划账本.Attempt.非法状态转移', {
        attemptId: attempt.attemptId,
        from: attempt.status,
        to: nextStatus,
      }, LogCategory.ORCHESTRATOR);
      return false;
    }

    const now = Date.now();
    attempt.status = nextStatus;
    if (nextStatus === 'inflight') {
      attempt.startedAt = attempt.startedAt || now;
    }
    if (PLAN_ATTEMPT_TERMINAL_STATUSES.has(nextStatus)) {
      attempt.endedAt = now;
    }
    attempt.updatedAt = now;
    this.mergeAttemptAnnotations(attempt, {
      reason: options.reason,
      error: options.error,
      evidenceIds: options.evidenceIds,
      metadata: options.metadata,
      updatedAt: now,
    });
    return true;
  }

  private finalizeInflightAttemptsInRecord(
    record: PlanRecord,
    status: PlanAttemptTerminalStatus,
    reason: string,
  ): boolean {
    let changed = false;
    for (const attempt of record.attempts) {
      if (attempt.status !== 'inflight' && attempt.status !== 'created') {
        continue;
      }
      if (attempt.status === 'created') {
        changed = this.applyAttemptTransition(attempt, 'inflight', {
          reason,
        }) || changed;
      }
      changed = this.applyAttemptTransition(attempt, status, {
        reason,
      }) || changed;
    }
    return changed;
  }

  private emitUpdated(record: PlanRecord, reason: string): void {
    this.appendEventRecord(record, reason);
    const event: PlanLedgerUpdateEvent = {
      sessionId: record.sessionId,
      planId: record.planId,
      reason,
      record,
    };
    this.emit('updated', event);
  }

  private persistPlan(record: PlanRecord): void {
    this.ensurePlansDir(record.sessionId);
    const planFile = this.getPlanFilePath(record.sessionId, record.planId);
    fs.writeFileSync(planFile, JSON.stringify(record, null, 2), 'utf-8');

    const index = this.loadIndex(record.sessionId);
    const entry = this.toIndexEntry(record);
    const existingIdx = index.findIndex((item) => item.planId === record.planId);
    if (existingIdx >= 0) {
      index[existingIdx] = entry;
    } else {
      index.push(entry);
    }
    index.sort((a, b) => b.updatedAt - a.updatedAt);
    fs.writeFileSync(this.getIndexPath(record.sessionId), JSON.stringify(index, null, 2), 'utf-8');
    this.touchSessionCache(record.sessionId);
    this.indexCache.set(record.sessionId, this.cloneIndexEntries(index));
    const sessionPlanCache = this.getPlanCacheForSession(record.sessionId);
    this.setPlanCacheRecord(sessionPlanCache, record.planId, this.clonePlanRecord(record));
    this.prunePlanCacheForSession(sessionPlanCache);
  }

  private loadPlan(sessionId: string, planId: string): PlanRecord | null {
    const sessionPlanCache = this.getPlanCacheForSession(sessionId);
    const cached = sessionPlanCache.get(planId);
    if (cached) {
      this.touchSessionCache(sessionId);
      this.setPlanCacheRecord(sessionPlanCache, planId, cached);
      return this.clonePlanRecord(cached);
    }

    const filePath = this.getPlanFilePath(sessionId, planId);
    if (!fs.existsSync(filePath)) {
      return null;
    }
    try {
      const raw = fs.readFileSync(filePath, 'utf-8');
      const parsed = JSON.parse(raw) as PlanRecord;
      const normalized = this.normalizePlanRecord(parsed);
      this.touchSessionCache(sessionId);
      this.setPlanCacheRecord(sessionPlanCache, planId, this.clonePlanRecord(normalized));
      this.prunePlanCacheForSession(sessionPlanCache);
      return this.clonePlanRecord(normalized);
    } catch (error) {
      logger.warn('计划账本.加载失败', {
        sessionId,
        planId,
        error: error instanceof Error ? error.message : String(error),
      }, LogCategory.ORCHESTRATOR);
      return null;
    }
  }

  private markSuperseded(sessionId: string, planId: string): void {
    const existing = this.loadPlan(sessionId, planId);
    if (!existing || TERMINAL_PLAN_STATUSES.has(existing.status)) {
      return;
    }
    existing.status = 'superseded';
    existing.updatedAt = Date.now();
    this.persistPlan(existing);
    this.emitUpdated(existing, 'superseded');
  }

  private normalizePlanRecord(record: PlanRecord): PlanRecord {
    const normalizedItems = (Array.isArray(record.items) ? record.items : []).map((item) => ({
      ...item,
      dependsOn: this.normalizeStringArray(item.dependsOn),
      scopeHints: this.normalizeStringArray(item.scopeHints),
      targetFiles: this.normalizeStringArray(item.targetFiles),
      todoIds: this.normalizeStringArray(item.todoIds),
      todoStatuses: item.todoStatuses && typeof item.todoStatuses === 'object'
        ? item.todoStatuses
        : {},
    }));

    const normalizedAttempts = (Array.isArray(record.attempts) ? record.attempts : [])
      .filter((attempt): attempt is PlanAttemptRecord => Boolean(attempt) && typeof attempt === 'object')
      .map((attempt): PlanAttemptRecord => {
        const scope = this.normalizeAttemptScope(attempt.scope);
        const targetId = this.normalizeIdentifier(attempt.targetId) || 'orchestrator';
        return {
          attemptId: this.normalizeIdentifier(attempt.attemptId)
            || `attempt-legacy-${scope}-${targetId}-${Number.isFinite(attempt.sequence) ? attempt.sequence : 1}`,
          scope,
          targetId,
          assignmentId: this.normalizeIdentifier(attempt.assignmentId) || undefined,
          todoId: this.normalizeIdentifier(attempt.todoId) || undefined,
          sequence: Number.isFinite(attempt.sequence) && attempt.sequence > 0 ? Math.floor(attempt.sequence) : 1,
          status: this.normalizeAttemptStatus(attempt.status),
          reason: typeof attempt.reason === 'string' && attempt.reason.trim() ? attempt.reason.trim() : undefined,
          error: typeof attempt.error === 'string' && attempt.error.trim() ? attempt.error.trim() : undefined,
          evidenceIds: this.normalizeAttemptEvidenceIds(attempt.evidenceIds),
          metadata: this.normalizeAttemptMetadata(attempt.metadata),
          createdAt: Number.isFinite(attempt.createdAt) ? attempt.createdAt : Date.now(),
          startedAt: Number.isFinite(attempt.startedAt) ? attempt.startedAt : undefined,
          endedAt: Number.isFinite(attempt.endedAt) ? attempt.endedAt : undefined,
          updatedAt: Number.isFinite(attempt.updatedAt) ? attempt.updatedAt : Date.now(),
        };
      });

    return {
      ...record,
      acceptanceCriteria: this.normalizeStringArray(record.acceptanceCriteria),
      constraints: this.normalizeStringArray(record.constraints),
      items: normalizedItems,
      attempts: normalizedAttempts,
      links: {
        assignmentIds: this.normalizeStringArray(record.links?.assignmentIds),
        todoIds: this.normalizeStringArray(record.links?.todoIds),
      },
    };
  }

  private toIndexEntry(record: PlanRecord): PlanIndexEntry {
    return {
      planId: record.planId,
      sessionId: record.sessionId,
      missionId: record.missionId,
      turnId: record.turnId,
      version: record.version,
      status: record.status,
      mode: record.mode,
      summary: record.summary,
      createdAt: record.createdAt,
      updatedAt: record.updatedAt,
    };
  }

  private loadIndex(sessionId: string): PlanIndexEntry[] {
    this.touchSessionCache(sessionId);
    const cached = this.indexCache.get(sessionId);
    if (cached) {
      return this.cloneIndexEntries(cached);
    }

    const filePath = this.getIndexPath(sessionId);
    if (!fs.existsSync(filePath)) {
      this.indexCache.set(sessionId, []);
      return [];
    }
    try {
      const raw = fs.readFileSync(filePath, 'utf-8');
      const parsed = JSON.parse(raw) as PlanIndexEntry[];
      if (!Array.isArray(parsed)) {
        this.indexCache.set(sessionId, []);
        return [];
      }
      const normalized = parsed.filter((entry) => !!entry && typeof entry.planId === 'string');
      this.indexCache.set(sessionId, this.cloneIndexEntries(normalized));
      return this.cloneIndexEntries(normalized);
    } catch (error) {
      logger.warn('计划账本.index.加载失败', {
        sessionId,
        error: error instanceof Error ? error.message : String(error),
      }, LogCategory.ORCHESTRATOR);
      this.indexCache.set(sessionId, []);
      return [];
    }
  }

  private ensurePlansDir(sessionId: string): void {
    const plansDir = this.getPlansDir(sessionId);
    if (!fs.existsSync(plansDir)) {
      fs.mkdirSync(plansDir, { recursive: true });
    }
  }

  private getPlansDir(sessionId: string): string {
    return this.sessionManager.getPlansDir(sessionId);
  }

  private getIndexPath(sessionId: string): string {
    return path.join(this.getPlansDir(sessionId), 'index.json');
  }

  private getPlanFilePath(sessionId: string, planId: string): string {
    return path.join(this.getPlansDir(sessionId), `${planId}.json`);
  }

  private getEventsFilePath(sessionId: string, planId: string): string {
    return path.join(this.getPlansDir(sessionId), `${planId}.events.jsonl`);
  }

  private generatePlanId(): string {
    return `plan-${Date.now()}-${Math.random().toString(36).slice(2, 9)}`;
  }

  private buildPromptDigest(prompt: string): string {
    const normalized = prompt.replace(/\s+/g, ' ').trim();
    if (!normalized) {
      return 'empty';
    }
    return normalized.length > 240 ? `${normalized.slice(0, 240)}...` : normalized;
  }

  private normalizeStringArray(values?: string[] | readonly string[], fallback: string[] = []): string[] {
    const result = (Array.isArray(values) ? values : fallback)
      .filter((item): item is string => typeof item === 'string')
      .map((item) => item.trim())
      .filter((item) => item.length > 0);
    return Array.from(new Set(result));
  }

  private addUnique(target: string[], value: string): void {
    const normalized = value.trim();
    if (!normalized) {
      return;
    }
    if (!target.includes(normalized)) {
      target.push(normalized);
    }
  }

  private appendEventRecord(record: PlanRecord, reason: string): void {
    try {
      this.rotateEventsFileIfNeeded(record.sessionId, record.planId);
      const event: PlanLedgerEventRecord = {
        timestamp: Date.now(),
        reason,
        sessionId: record.sessionId,
        planId: record.planId,
        missionId: record.missionId,
        status: record.status,
        version: record.version,
        itemTotal: record.items.length,
        completedItems: record.items.filter((item) => item.status === 'completed' || item.status === 'skipped').length,
        failedItems: record.items.filter((item) => item.status === 'failed' || item.status === 'cancelled').length,
        attemptTotal: record.attempts.length,
        inflightAttempts: record.attempts.filter((attempt) => attempt.status === 'created' || attempt.status === 'inflight').length,
        failedAttempts: record.attempts.filter((attempt) => attempt.status === 'failed' || attempt.status === 'cancelled').length,
        timeoutAttempts: record.attempts.filter((attempt) => attempt.status === 'timeout').length,
      };
      fs.appendFileSync(this.getEventsFilePath(record.sessionId, record.planId), `${JSON.stringify(event)}\n`, 'utf-8');
    } catch (error) {
      logger.warn('计划账本.events.追加失败', {
        sessionId: record.sessionId,
        planId: record.planId,
        error: error instanceof Error ? error.message : String(error),
      }, LogCategory.ORCHESTRATOR);
    }
  }

  private normalizeMissionTerminalStatus(status: string): MissionTerminalStatus | null {
    if (status === 'completed' || status === 'failed' || status === 'cancelled') {
      return status;
    }
    return null;
  }

  private getPlanCacheForSession(sessionId: string): Map<string, PlanRecord> {
    this.touchSessionCache(sessionId);
    const existing = this.planCache.get(sessionId);
    if (existing) {
      return existing;
    }
    const created = new Map<string, PlanRecord>();
    this.planCache.set(sessionId, created);
    return created;
  }

  private cloneIndexEntries(entries: PlanIndexEntry[]): PlanIndexEntry[] {
    return entries.map((entry) => ({ ...entry }));
  }

  private clonePlanRecord(record: PlanRecord): PlanRecord {
    return {
      ...record,
      acceptanceCriteria: [...record.acceptanceCriteria],
      constraints: [...record.constraints],
      links: {
        assignmentIds: [...record.links.assignmentIds],
        todoIds: [...record.links.todoIds],
      },
      review: record.review ? { ...record.review } : undefined,
      items: record.items.map((item) => ({
        ...item,
        dependsOn: [...item.dependsOn],
        scopeHints: item.scopeHints ? [...item.scopeHints] : undefined,
        targetFiles: item.targetFiles ? [...item.targetFiles] : undefined,
        todoIds: [...item.todoIds],
        todoStatuses: { ...item.todoStatuses },
      })),
      attempts: record.attempts.map((attempt) => ({
        ...attempt,
        evidenceIds: [...attempt.evidenceIds],
        metadata: attempt.metadata ? { ...attempt.metadata } : undefined,
      })),
    };
  }

  private setPlanCacheRecord(target: Map<string, PlanRecord>, planId: string, record: PlanRecord): void {
    if (target.has(planId)) {
      target.delete(planId);
    }
    target.set(planId, record);
  }

  private prunePlanCacheForSession(cache: Map<string, PlanRecord>): void {
    while (cache.size > PlanLedgerService.PLAN_CACHE_MAX_PER_SESSION) {
      const firstKey = cache.keys().next().value as string | undefined;
      if (!firstKey) {
        break;
      }
      cache.delete(firstKey);
    }
    this.pruneSessionCachesIfNeeded();
  }

  private touchSessionCache(sessionId: string): void {
    if (this.sessionCacheAccessOrder.has(sessionId)) {
      this.sessionCacheAccessOrder.delete(sessionId);
    }
    this.sessionCacheAccessOrder.set(sessionId, Date.now());
    this.pruneSessionCachesIfNeeded();
  }

  private pruneSessionCachesIfNeeded(): void {
    while (this.sessionCacheAccessOrder.size > PlanLedgerService.CACHE_MAX_SESSION_COUNT) {
      const oldestSessionId = this.sessionCacheAccessOrder.keys().next().value as string | undefined;
      if (!oldestSessionId) {
        break;
      }
      this.sessionCacheAccessOrder.delete(oldestSessionId);
      this.indexCache.delete(oldestSessionId);
      this.planCache.delete(oldestSessionId);
      this.writeQueues.delete(oldestSessionId);
    }
  }

  private rotateEventsFileIfNeeded(sessionId: string, planId: string): void {
    const eventsFilePath = this.getEventsFilePath(sessionId, planId);
    if (!fs.existsSync(eventsFilePath)) {
      return;
    }

    const stats = fs.statSync(eventsFilePath);
    if (stats.size < PlanLedgerService.EVENTS_ROTATE_MAX_BYTES) {
      return;
    }

    const rotateSuffix = `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
    const rotatedFilePath = path.join(this.getPlansDir(sessionId), `${planId}.events.${rotateSuffix}.jsonl`);
    fs.renameSync(eventsFilePath, rotatedFilePath);
    this.pruneRotatedEventFiles(sessionId, planId);
  }

  private pruneRotatedEventFiles(sessionId: string, planId: string): void {
    const plansDir = this.getPlansDir(sessionId);
    if (!fs.existsSync(plansDir)) {
      return;
    }

    const prefix = `${planId}.events.`;
    const suffix = '.jsonl';
    const rotatedFiles = fs.readdirSync(plansDir)
      .map((fileName) => {
        if (!fileName.startsWith(prefix) || !fileName.endsWith(suffix)) {
          return null;
        }
        const middle = fileName.slice(prefix.length, fileName.length - suffix.length);
        const tsMatch = /^(\d+)(?:-[a-z0-9]+)?$/i.exec(middle);
        if (!tsMatch) {
          return null;
        }
        const ts = Number(tsMatch[1]);
        return {
          fileName,
          timestamp: Number.isFinite(ts) ? ts : 0,
        };
      })
      .filter((item): item is { fileName: string; timestamp: number } => item !== null)
      .sort((a, b) => b.timestamp - a.timestamp);

    const staleFiles = rotatedFiles.slice(PlanLedgerService.EVENTS_ROTATE_KEEP_FILES);
    for (const staleFile of staleFiles) {
      try {
        fs.unlinkSync(path.join(plansDir, staleFile.fileName));
      } catch (error) {
        logger.warn('计划账本.events.历史轮转文件清理失败', {
          sessionId,
          planId,
          fileName: staleFile.fileName,
          error: error instanceof Error ? error.message : String(error),
        }, LogCategory.ORCHESTRATOR);
      }
    }
  }

  private runWithSessionQueue<T>(sessionId: string, operation: () => Promise<T>): Promise<T> {
    this.touchSessionCache(sessionId);
    const previous = this.writeQueues.get(sessionId) || Promise.resolve();
    const next = previous.then(operation, operation);
    const queueTail = next.then(
      () => undefined,
      () => undefined,
    );
    this.writeQueues.set(
      sessionId,
      queueTail,
    );
    void next.finally(() => {
      if (this.writeQueues.get(sessionId) === queueTail) {
        this.writeQueues.delete(sessionId);
      }
    });
    return next;
  }
}
