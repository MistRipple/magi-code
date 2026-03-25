import * as fs from 'fs';
import * as path from 'path';
import { EventEmitter } from 'events';
import { logger, LogCategory } from '../../logging';
import { atomicWriteFile } from '../../utils/atomic-write';
import { CoalescedAsyncTaskQueue, SerialAsyncTaskQueue } from '../../utils/async-task-queue';
import type { UnifiedSessionManager } from '../../session/unified-session-manager';
import type { UnifiedTodo } from '../../todo/types';
import type { AcceptanceCriterion } from '../mission/types';
import type {
  PlanAcceptanceSummary,
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
  PlanMutationOptions,
  PlanRecord,
  PlanRuntimeReplanState,
  PlanRuntimeReviewState,
  PlanRuntimePhaseState,
  PlanRuntimeState,
  PlanRuntimeTerminationState,
  PlanRuntimeVersion,
  PlanRuntimeWaitState,
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
  schemaVersion: number;
  runtimeVersion: PlanRuntimeVersion;
  revision: number;
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

interface SchemaVersionResolution {
  sourceVersion: number;
  targetVersion: number;
  supported: boolean;
  shouldMigrate: boolean;
  sourceDeclared: boolean;
  reason?: 'too_old' | 'too_new';
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

const PLAN_STATUS_TRANSITIONS: Record<PlanStatus, PlanStatus[]> = {
  draft: ['awaiting_confirmation', 'approved', 'executing', 'rejected', 'failed', 'cancelled', 'superseded', 'completed'],
  awaiting_confirmation: ['approved', 'rejected', 'executing', 'failed', 'cancelled', 'superseded', 'completed'],
  approved: ['executing', 'failed', 'cancelled', 'superseded', 'completed'],
  rejected: [],
  executing: ['partially_completed', 'completed', 'failed', 'cancelled'],
  partially_completed: ['completed', 'failed', 'cancelled'],
  completed: [],
  failed: [],
  cancelled: [],
  superseded: [],
};

const RUNTIME_REVIEW_TRANSITIONS: Record<PlanRuntimeReviewState['state'], PlanRuntimeReviewState['state'][]> = {
  idle: ['running', 'accepted', 'rejected'],
  running: ['idle', 'accepted', 'rejected'],
  accepted: ['idle', 'running'],
  rejected: ['idle', 'running'],
};

const RUNTIME_REPLAN_TRANSITIONS: Record<PlanRuntimeReplanState['state'], PlanRuntimeReplanState['state'][]> = {
  none: ['required', 'awaiting_confirmation', 'applied'],
  required: ['none', 'awaiting_confirmation', 'applied'],
  awaiting_confirmation: ['none', 'required', 'applied'],
  applied: ['none', 'required', 'awaiting_confirmation'],
};

const RUNTIME_WAIT_TRANSITIONS: Record<PlanRuntimeWaitState['state'], PlanRuntimeWaitState['state'][]> = {
  none: ['external_waiting'],
  external_waiting: ['none'],
};

export class PlanLedgerService extends EventEmitter {
  private static readonly CURRENT_SCHEMA_VERSION = 2;
  private static readonly MIN_SUPPORTED_SCHEMA_VERSION = 1;
  private readonly sessionMutationQueues = new Map<string, Promise<void>>();
  private readonly indexCache = new Map<string, PlanIndexEntry[]>();
  private readonly planCache = new Map<string, Map<string, PlanRecord>>();
  private readonly sessionCacheAccessOrder = new Map<string, number>();
  private readonly snapshotPersistQueue = new CoalescedAsyncTaskQueue((targetPath, error) => {
    logger.warn('计划账本.异步快照写入失败', {
      targetPath,
      error: error instanceof Error ? error.message : String(error),
    }, LogCategory.ORCHESTRATOR);
  });
  private readonly eventAppendQueue = new SerialAsyncTaskQueue((targetPath, error) => {
    logger.warn('计划账本.events.异步追加失败', {
      targetPath,
      error: error instanceof Error ? error.message : String(error),
    }, LogCategory.ORCHESTRATOR);
  });
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
      const runtimeVersion = this.resolveRuntimeVersion(input.mode);
      const acceptanceCriteria = this.normalizeAcceptanceCriteria(input.acceptanceCriteria);
      const record: PlanRecord = {
        planId,
        sessionId: input.sessionId,
        missionId: input.missionId,
        turnId: input.turnId,
        schemaVersion: PlanLedgerService.CURRENT_SCHEMA_VERSION,
        runtimeVersion,
        revision: 1,
        version: latestForTurn ? latestForTurn.version + 1 : 1,
        parentPlanId: latestForTurn?.planId,
        mode: input.mode,
        status: 'draft',
        source: 'orchestrator',
        promptDigest: this.buildPromptDigest(input.prompt),
        summary,
        analysis: input.analysis?.trim() || undefined,
        constraints: this.normalizeStringArray(input.constraints),
        riskLevel: input.riskLevel,
        runtime: this.createInitialRuntimeState(acceptanceCriteria, runtimeVersion, now),
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

      this.persistPlan(record, { preserveRevision: true });
      this.emitUpdated(record, 'draft-created');
      return record;
    });
  }

  async markAwaitingConfirmation(
    sessionId: string,
    planId: string,
    formattedPlan?: string,
    options?: PlanMutationOptions,
  ): Promise<PlanRecord | null> {
    return this.runWithSessionQueue(sessionId, async () => {
      const record = this.loadPlan(sessionId, planId);
      if (!record || TERMINAL_PLAN_STATUSES.has(record.status)) {
        return null;
      }
      if (!this.canMutateWithExpectedRevision(record, options, 'markAwaitingConfirmation')) {
        return null;
      }
      if (!this.tryTransitionPlanStatus(record, 'awaiting_confirmation', 'markAwaitingConfirmation', options?.auditReason)) {
        return null;
      }
      if (formattedPlan && formattedPlan.trim()) {
        record.formattedPlan = formattedPlan.trim();
      }
      record.updatedAt = Date.now();
      this.persistPlan(record);
      this.emitUpdated(record, 'awaiting-confirmation');
      return record;
    });
  }

  async approve(
    sessionId: string,
    planId: string,
    reviewer = 'system:auto',
    reason?: string,
    options?: PlanMutationOptions,
  ): Promise<PlanRecord | null> {
    return this.runWithSessionQueue(sessionId, async () => {
      const record = this.loadPlan(sessionId, planId);
      if (!record || TERMINAL_PLAN_STATUSES.has(record.status)) {
        return null;
      }
      if (!this.canMutateWithExpectedRevision(record, options, 'approve')) {
        return null;
      }
      if (!this.tryTransitionPlanStatus(record, 'approved', 'approve', options?.auditReason)) {
        return null;
      }
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

  async reject(
    sessionId: string,
    planId: string,
    reviewer = 'user',
    reason?: string,
    options?: PlanMutationOptions,
  ): Promise<PlanRecord | null> {
    return this.runWithSessionQueue(sessionId, async () => {
      const record = this.loadPlan(sessionId, planId);
      if (!record || TERMINAL_PLAN_STATUSES.has(record.status)) {
        return null;
      }
      if (!this.canMutateWithExpectedRevision(record, options, 'reject')) {
        return null;
      }
      if (!this.tryTransitionPlanStatus(record, 'rejected', 'reject', options?.auditReason)) {
        return null;
      }
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

  async markExecuting(sessionId: string, planId: string, options?: PlanMutationOptions): Promise<PlanRecord | null> {
    return this.runWithSessionQueue(sessionId, async () => {
      const record = this.loadPlan(sessionId, planId);
      if (!record || TERMINAL_PLAN_STATUSES.has(record.status)) {
        return null;
      }
      if (!this.canMutateWithExpectedRevision(record, options, 'markExecuting')) {
        return null;
      }
      if (!this.tryTransitionPlanStatus(record, 'executing', 'markExecuting', options?.auditReason)) {
        return null;
      }
      record.updatedAt = Date.now();
      this.persistPlan(record);
      this.emitUpdated(record, 'executing');
      return record;
    });
  }

  async bindMission(
    sessionId: string,
    planId: string,
    missionId: string,
    options?: PlanMutationOptions,
  ): Promise<PlanRecord | null> {
    return this.runWithSessionQueue(sessionId, async () => {
      const record = this.loadPlan(sessionId, planId);
      if (!record) {
        return null;
      }
      if (!this.canMutateWithExpectedRevision(record, options, 'bindMission')) {
        return null;
      }
      record.missionId = missionId;
      record.updatedAt = Date.now();
      this.persistPlan(record);
      this.emitUpdated(record, 'mission-bound');
      return record;
    });
  }

  /**
   * 更新计划摘要（由编排者在首次 worker_dispatch 时通过 mission_title 提供语义化标题）
   */
  async updateSummary(
    sessionId: string,
    planId: string,
    summary: string,
    options?: PlanMutationOptions,
  ): Promise<PlanRecord | null> {
    const trimmed = summary.trim();
    if (!trimmed) {
      return null;
    }
    return this.runWithSessionQueue(sessionId, async () => {
      const record = this.loadPlan(sessionId, planId);
      if (!record || TERMINAL_PLAN_STATUSES.has(record.status)) {
        return null;
      }
      if (!this.canMutateWithExpectedRevision(record, options, 'updateSummary')) {
        return null;
      }
      record.summary = trimmed;
      record.updatedAt = Date.now();
      this.persistPlan(record);
      this.emitUpdated(record, 'summary-updated');
      return record;
    });
  }

  async startAttempt(
    sessionId: string,
    planId: string,
    input: PlanAttemptStartInput,
    options?: PlanMutationOptions,
  ): Promise<PlanRecord | null> {
    return this.runWithSessionQueue(sessionId, async () => {
      const record = this.loadPlan(sessionId, planId);
      if (!record || TERMINAL_PLAN_STATUSES.has(record.status)) {
        return null;
      }
      if (!this.canMutateWithExpectedRevision(record, options, 'startAttempt')) {
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

  async completeLatestAttempt(
    sessionId: string,
    planId: string,
    input: PlanAttemptCompleteInput,
    options?: PlanMutationOptions,
  ): Promise<PlanRecord | null> {
    return this.runWithSessionQueue(sessionId, async () => {
      const record = this.loadPlan(sessionId, planId);
      if (!record) {
        return null;
      }
      if (!this.canMutateWithExpectedRevision(record, options, 'completeLatestAttempt')) {
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

  async upsertDispatchItem(
    sessionId: string,
    planId: string,
    input: DispatchPlanItemInput,
    options?: PlanMutationOptions,
  ): Promise<PlanRecord | null> {
    return this.runWithSessionQueue(sessionId, async () => {
      const record = this.loadPlan(sessionId, planId);
      if (!record || TERMINAL_PLAN_STATUSES.has(record.status)) {
        return null;
      }
      if (!this.canMutateWithExpectedRevision(record, options, 'upsertDispatchItem')) {
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
      if (record.status === 'draft' && !this.tryTransitionPlanStatus(record, 'approved', 'upsertDispatchItem:auto-approve', options?.auditReason)) {
        return null;
      }
      this.persistPlan(record);
      this.emitUpdated(record, 'dispatch-item-upserted');
      return record;
    });
  }

  async bindAssignmentTodos(
    sessionId: string,
    planId: string,
    assignmentId: string,
    todos: UnifiedTodo[],
    options?: PlanMutationOptions,
  ): Promise<PlanRecord | null> {
    return this.runWithSessionQueue(sessionId, async () => {
      const record = this.loadPlan(sessionId, planId);
      if (!record || TERMINAL_PLAN_STATUSES.has(record.status)) {
        return null;
      }
      if (!this.canMutateWithExpectedRevision(record, options, 'bindAssignmentTodos')) {
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
    options?: PlanMutationOptions,
  ): Promise<PlanRecord | null> {
    return this.runWithSessionQueue(sessionId, async () => {
      const record = this.loadPlan(sessionId, planId);
      if (!record || TERMINAL_PLAN_STATUSES.has(record.status)) {
        return null;
      }
      if (!this.canMutateWithExpectedRevision(record, options, 'updateAssignmentStatus')) {
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
      const nextPlanStatus = this.computePlanStatus(record, record.status === 'executing' ? 'executing' : undefined);
      if (!this.tryTransitionPlanStatus(record, nextPlanStatus, 'updateAssignmentStatus:computePlanStatus', options?.auditReason)) {
        return null;
      }
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
    options?: PlanMutationOptions,
  ): Promise<PlanRecord | null> {
    return this.runWithSessionQueue(sessionId, async () => {
      const record = this.loadPlan(sessionId, planId);
      if (!record || TERMINAL_PLAN_STATUSES.has(record.status)) {
        return null;
      }
      if (!this.canMutateWithExpectedRevision(record, options, 'updateTodoStatus')) {
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
      if ((record.status === 'approved' || record.status === 'awaiting_confirmation')
        && !this.tryTransitionPlanStatus(record, 'executing', 'updateTodoStatus:auto-executing', options?.auditReason)) {
        return null;
      }
      const nextPlanStatus = this.computePlanStatus(record, record.status);
      if (!this.tryTransitionPlanStatus(record, nextPlanStatus, 'updateTodoStatus:computePlanStatus', options?.auditReason)) {
        return null;
      }
      this.persistPlan(record);
      this.emitUpdated(record, 'todo-status-updated');
      return record;
    });
  }

  async finalize(
    sessionId: string,
    planId: string,
    status: 'completed' | 'failed' | 'cancelled',
    options?: PlanMutationOptions,
  ): Promise<PlanRecord | null> {
    return this.runWithSessionQueue(sessionId, async () => {
      const record = this.loadPlan(sessionId, planId);
      if (!record || TERMINAL_PLAN_STATUSES.has(record.status)) {
        return record;
      }
      if (!this.canMutateWithExpectedRevision(record, options, 'finalize')) {
        return null;
      }
      // 填充 termination runtime 状态
      record.runtime.termination = {
        reason: `plan-finalized:${status}`,
        updatedAt: Date.now(),
      };
      // 同步更新 acceptance summary
      if (status === 'completed') {
        record.runtime.acceptance.summary = 'passed';
        record.runtime.acceptance.updatedAt = Date.now();
      } else if (status === 'failed') {
        record.runtime.acceptance.summary = 'failed';
        record.runtime.acceptance.updatedAt = Date.now();
      }
      this.applyMissionTerminalStatus(record, status, `plan-finalized:${status}`, 'finalized');
      return record;
    });
  }

  /**
   * 更新 PlanRuntimeState 的子字段
   *
   * 用于在主执行链路的关键节点推进 runtime 状态，
   * 确保 review/wait/replan/termination 字段反映实际运行时状态。
   */
  async updateRuntimeState(
    sessionId: string,
    planId: string,
    patch: {
      review?: Partial<PlanRuntimeReviewState>;
      replan?: Partial<PlanRuntimeReplanState>;
      wait?: Partial<PlanRuntimeWaitState>;
      phase?: Partial<PlanRuntimePhaseState>;
      termination?: Partial<PlanRuntimeTerminationState>;
      acceptance?: { summary?: PlanAcceptanceSummary; criteria?: AcceptanceCriterion[] };
    },
    options?: PlanMutationOptions,
  ): Promise<PlanRecord | null> {
    return this.runWithSessionQueue(sessionId, async () => {
      const record = this.loadPlan(sessionId, planId);
      if (!record) {
        return null;
      }
      if (!this.canMutateWithExpectedRevision(record, options, 'updateRuntimeState')) {
        return null;
      }

      const now = Date.now();

      if (patch.review) {
        if (patch.review.state
          && !this.canTransitionRuntimeReview(record.runtime.review.state, patch.review.state, record, options?.auditReason)) {
          return null;
        }
        record.runtime.review = {
          ...record.runtime.review,
          ...patch.review,
          lastReviewedAt: patch.review.state === 'running' || patch.review.state === 'accepted' || patch.review.state === 'rejected'
            ? now
            : record.runtime.review.lastReviewedAt,
        };
      }

      if (patch.replan) {
        if (patch.replan.state
          && !this.canTransitionRuntimeReplan(record.runtime.replan.state, patch.replan.state, record, options?.auditReason)) {
          return null;
        }
        const nextReplan: PlanRuntimeReplanState = {
          ...record.runtime.replan,
          ...patch.replan,
          updatedAt: now,
        };
        if (patch.replan.state === 'none' && patch.replan.reason === undefined) {
          nextReplan.reason = undefined;
        }
        record.runtime.replan = nextReplan;
      }

      if (patch.wait) {
        if (patch.wait.state
          && !this.canTransitionRuntimeWait(record.runtime.wait.state, patch.wait.state, record, options?.auditReason)) {
          return null;
        }
        const nextWait: PlanRuntimeWaitState = {
          ...record.runtime.wait,
          ...patch.wait,
          updatedAt: now,
        };
        if (patch.wait.state === 'none' && patch.wait.reasonCode === undefined) {
          nextWait.reasonCode = undefined;
        }
        record.runtime.wait = nextWait;
      }

      if (patch.phase) {
        const nextPhase: PlanRuntimePhaseState = {
          ...record.runtime.phase,
          ...patch.phase,
          remainingPhases: Array.isArray(patch.phase.remainingPhases)
            ? patch.phase.remainingPhases
                .map((item) => (typeof item === 'string' ? item.trim() : ''))
                .filter((item) => item.length > 0)
            : [...record.runtime.phase.remainingPhases],
          updatedAt: now,
        };
        if (nextPhase.continuationIntent === 'stop') {
          nextPhase.nextIndex = undefined;
          nextPhase.nextTitle = undefined;
          nextPhase.remainingPhases = [];
        }
        record.runtime.phase = nextPhase;
      }

      if (patch.termination) {
        record.runtime.termination = {
          ...record.runtime.termination,
          ...patch.termination,
          updatedAt: now,
        };
      }

      if (patch.acceptance) {
        let acceptanceTouched = false;
        if (Array.isArray(patch.acceptance.criteria)) {
          record.runtime.acceptance.criteria = this.normalizeAcceptanceCriteria(patch.acceptance.criteria);
          acceptanceTouched = true;
        }
        if (patch.acceptance.summary) {
          record.runtime.acceptance.summary = patch.acceptance.summary;
          acceptanceTouched = true;
        } else if (acceptanceTouched) {
          record.runtime.acceptance.summary = this.computeAcceptanceSummary(record.runtime.acceptance.criteria);
        }
        if (acceptanceTouched) {
          record.runtime.acceptance.updatedAt = now;
        }
      }

      record.updatedAt = now;
      this.persistPlan(record);
      this.emitUpdated(record, 'runtime-state-updated');
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

        const updated = this.applyMissionTerminalStatus(
          record,
          missionTerminalStatus,
          `reconciled:${missionTerminalStatus}`,
          'reconciled-with-mission',
        );
        if (!updated) {
          continue;
        }
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

  getLatestPlanByMission(
    sessionId: string,
    missionId: string,
    options?: { includeTerminal?: boolean },
  ): PlanRecord | null {
    const normalizedMissionId = missionId.trim();
    if (!normalizedMissionId) {
      return null;
    }
    const includeTerminal = options?.includeTerminal === true;
    const index = this.loadIndex(sessionId)
      .filter((entry) => (entry.missionId || '').trim() === normalizedMissionId)
      .sort((a, b) => b.updatedAt - a.updatedAt);
    for (const entry of index) {
      if (!includeTerminal && TERMINAL_PLAN_STATUSES.has(entry.status)) {
        continue;
      }
      const plan = this.loadPlan(sessionId, entry.planId);
      if (!plan) {
        continue;
      }
      if (!includeTerminal && TERMINAL_PLAN_STATUSES.has(plan.status)) {
        continue;
      }
      return plan;
    }
    return null;
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
    const acceptanceDescriptions = this.getAcceptanceDescriptions(plan.runtime.acceptance.criteria);
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
    if (acceptanceDescriptions.length > 0) {
      lines.push('');
      lines.push('### 验收');
      for (const item of acceptanceDescriptions) {
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

  private applyMissionTerminalStatus(
    record: PlanRecord,
    status: MissionTerminalStatus,
    attemptReason: string,
    updateReason: string,
  ): boolean {
    const attemptTerminated = this.finalizeInflightAttemptsInRecord(
      record,
      this.mapMissionStatusToAttemptTerminalStatus(status),
      attemptReason,
    );
    const nextStatus = this.mapMissionStatusToPlanStatus(status, record);
    if (!attemptTerminated && nextStatus === record.status) {
      return false;
    }
    if (!this.tryTransitionPlanStatus(record, nextStatus, updateReason, 'mission-terminal-sync')) {
      return false;
    }
    record.updatedAt = Date.now();
    this.persistPlan(record);
    this.emitUpdated(record, updateReason);
    return true;
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

  private persistPlan(record: PlanRecord, options?: { preserveRevision?: boolean }): void {
    const preserveRevision = options?.preserveRevision === true;
    if (preserveRevision) {
      record.revision = this.normalizeRevision(record.revision);
    } else {
      record.revision = this.normalizeRevision(record.revision) + 1;
    }
    this.ensurePlansDir(record.sessionId);
    const planFile = this.getPlanFilePath(record.sessionId, record.planId);

    const index = this.loadIndex(record.sessionId);
    const entry = this.toIndexEntry(record);
    const existingIdx = index.findIndex((item) => item.planId === record.planId);
    if (existingIdx >= 0) {
      index[existingIdx] = entry;
    } else {
      index.push(entry);
    }
    index.sort((a, b) => b.updatedAt - a.updatedAt);
    this.touchSessionCache(record.sessionId);
    this.indexCache.set(record.sessionId, this.cloneIndexEntries(index));
    const sessionPlanCache = this.getPlanCacheForSession(record.sessionId);
    this.setPlanCacheRecord(sessionPlanCache, record.planId, this.clonePlanRecord(record));
    this.prunePlanCacheForSession(sessionPlanCache);

    const planPayload = JSON.stringify(record, null, 2);
    const indexPayload = JSON.stringify(index, null, 2);
    const planFilePath = planFile;
    const indexPath = this.getIndexPath(record.sessionId);
    this.snapshotPersistQueue.schedule(planFilePath, async () => {
      await atomicWriteFile(planFilePath, planPayload);
    });
    this.snapshotPersistQueue.schedule(indexPath, async () => {
      await atomicWriteFile(indexPath, indexPayload);
    });
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
      const parsed = JSON.parse(raw) as Record<string, unknown>;
      const schemaResolution = this.resolveSchemaVersion(parsed?.schemaVersion);
      if (!schemaResolution.supported) {
        logger.warn('计划账本.schema.版本不受支持', {
          sessionId,
          planId,
          schemaVersion: schemaResolution.sourceVersion,
          minSupportedSchemaVersion: PlanLedgerService.MIN_SUPPORTED_SCHEMA_VERSION,
          currentSchemaVersion: PlanLedgerService.CURRENT_SCHEMA_VERSION,
          reason: schemaResolution.reason,
        }, LogCategory.ORCHESTRATOR);
        return null;
      }
      const normalized = this.normalizePlanRecord(parsed);
      if (JSON.stringify(parsed) !== JSON.stringify(normalized)) {
        this.persistPlan(normalized, { preserveRevision: true });
        if (schemaResolution.shouldMigrate) {
          const migrationReason = `schema-migrated:${schemaResolution.sourceVersion}->${schemaResolution.targetVersion}`;
          this.appendEventRecord(normalized, migrationReason);
          logger.info('计划账本.schema.在线迁移完成', {
            sessionId,
            planId,
            fromSchemaVersion: schemaResolution.sourceVersion,
            toSchemaVersion: schemaResolution.targetVersion,
            sourceDeclared: schemaResolution.sourceDeclared,
          }, LogCategory.ORCHESTRATOR);
        }
      }
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
    if (!this.tryTransitionPlanStatus(existing, 'superseded', 'markSuperseded', 'supersede-previous-plan')) {
      return;
    }
    existing.updatedAt = Date.now();
    this.persistPlan(existing);
    this.emitUpdated(existing, 'superseded');
  }

  private canMutateWithExpectedRevision(
    record: PlanRecord,
    options: PlanMutationOptions | undefined,
    op: string,
  ): boolean {
    const expectedRevision = this.normalizePositiveInteger(options?.expectedRevision);
    if (expectedRevision === undefined) {
      return true;
    }
    if (record.revision === expectedRevision) {
      return true;
    }
    this.recordAudit(record, 'revision_conflict', {
      op,
      expectedRevision,
      actualRevision: record.revision,
      auditReason: options?.auditReason,
    });
    logger.warn('计划账本.CAS.修订号冲突', {
      sessionId: record.sessionId,
      planId: record.planId,
      op,
      expectedRevision,
      actualRevision: record.revision,
      auditReason: options?.auditReason,
    }, LogCategory.ORCHESTRATOR);
    return false;
  }

  private tryTransitionPlanStatus(
    record: PlanRecord,
    nextStatus: PlanStatus,
    op: string,
    auditReason?: string,
  ): boolean {
    const current = record.status;
    if (current === nextStatus) {
      return true;
    }
    const allowed = PLAN_STATUS_TRANSITIONS[current] || [];
    if (allowed.includes(nextStatus)) {
      record.status = nextStatus;
      return true;
    }
    this.recordAudit(record, 'invalid_plan_status_transition', {
      op,
      from: current,
      to: nextStatus,
      auditReason,
    });
    logger.warn('计划账本.非法计划状态迁移', {
      sessionId: record.sessionId,
      planId: record.planId,
      op,
      from: current,
      to: nextStatus,
      auditReason,
    }, LogCategory.ORCHESTRATOR);
    return false;
  }

  private canTransitionRuntimeReview(
    from: PlanRuntimeReviewState['state'],
    to: PlanRuntimeReviewState['state'],
    record: PlanRecord,
    auditReason?: string,
  ): boolean {
    if (from === to) {
      return true;
    }
    const allowed = RUNTIME_REVIEW_TRANSITIONS[from] || [];
    if (allowed.includes(to)) {
      return true;
    }
    this.recordAudit(record, 'invalid_runtime_review_transition', { from, to, auditReason });
    logger.warn('计划账本.非法runtime.review迁移', {
      sessionId: record.sessionId,
      planId: record.planId,
      from,
      to,
      auditReason,
    }, LogCategory.ORCHESTRATOR);
    return false;
  }

  private canTransitionRuntimeReplan(
    from: PlanRuntimeReplanState['state'],
    to: PlanRuntimeReplanState['state'],
    record: PlanRecord,
    auditReason?: string,
  ): boolean {
    if (from === to) {
      return true;
    }
    const allowed = RUNTIME_REPLAN_TRANSITIONS[from] || [];
    if (allowed.includes(to)) {
      return true;
    }
    this.recordAudit(record, 'invalid_runtime_replan_transition', { from, to, auditReason });
    logger.warn('计划账本.非法runtime.replan迁移', {
      sessionId: record.sessionId,
      planId: record.planId,
      from,
      to,
      auditReason,
    }, LogCategory.ORCHESTRATOR);
    return false;
  }

  private canTransitionRuntimeWait(
    from: PlanRuntimeWaitState['state'],
    to: PlanRuntimeWaitState['state'],
    record: PlanRecord,
    auditReason?: string,
  ): boolean {
    if (from === to) {
      return true;
    }
    const allowed = RUNTIME_WAIT_TRANSITIONS[from] || [];
    if (allowed.includes(to)) {
      return true;
    }
    this.recordAudit(record, 'invalid_runtime_wait_transition', { from, to, auditReason });
    logger.warn('计划账本.非法runtime.wait迁移', {
      sessionId: record.sessionId,
      planId: record.planId,
      from,
      to,
      auditReason,
    }, LogCategory.ORCHESTRATOR);
    return false;
  }

  private recordAudit(
    record: PlanRecord,
    code: string,
    payload?: Record<string, unknown>,
  ): void {
    const payloadText = payload ? JSON.stringify(payload) : '';
    this.appendEventRecord(record, payloadText ? `audit:${code}:${payloadText}` : `audit:${code}`);
  }

  private normalizePlanRecord(record: Record<string, unknown>): PlanRecord {
    const sourceRecord = record as Partial<PlanRecord> & {
      acceptanceCriteria?: unknown;
      runtime?: Partial<PlanRuntimeState> & {
        acceptance?: {
          criteria?: unknown;
          summary?: unknown;
          updatedAt?: unknown;
        };
        review?: {
          round?: unknown;
          state?: unknown;
          lastReviewedAt?: unknown;
        };
        replan?: {
          state?: unknown;
          reason?: unknown;
          updatedAt?: unknown;
        };
        wait?: {
          state?: unknown;
          reasonCode?: unknown;
          updatedAt?: unknown;
        };
        termination?: {
          snapshotId?: unknown;
          reason?: unknown;
          updatedAt?: unknown;
        };
      };
    };
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

    const runtimeVersion = this.normalizeRuntimeVersion(sourceRecord.runtimeVersion, sourceRecord.mode);
    const legacyAcceptance = this.normalizeStringArray(sourceRecord.acceptanceCriteria as string[] | undefined);
    const normalizedAcceptanceCriteria = this.normalizeAcceptanceCriteria(
      sourceRecord.runtime?.acceptance?.criteria ?? legacyAcceptance,
    );
    const acceptanceSummary = this.normalizeAcceptanceSummary(
      sourceRecord.runtime?.acceptance?.summary,
      normalizedAcceptanceCriteria,
    );
    const reviewState = this.normalizeRuntimeReviewState(sourceRecord.runtime?.review?.state, sourceRecord.review?.status);
    const reviewRound = this.normalizePositiveInteger(sourceRecord.runtime?.review?.round)
      ?? (sourceRecord.review ? 1 : 0);
    const reviewUpdatedAt = this.normalizeTimestamp(sourceRecord.runtime?.review?.lastReviewedAt)
      ?? this.normalizeTimestamp(sourceRecord.review?.reviewedAt);
    const schemaResolution = this.resolveSchemaVersion(sourceRecord.schemaVersion);
    const now = Date.now();

    return {
      ...sourceRecord,
      planId: this.normalizeIdentifier(sourceRecord.planId) || this.generatePlanId(),
      sessionId: this.normalizeIdentifier(sourceRecord.sessionId) || 'unknown-session',
      missionId: this.normalizeIdentifier(sourceRecord.missionId) || undefined,
      turnId: this.normalizeIdentifier(sourceRecord.turnId) || 'unknown-turn',
      schemaVersion: schemaResolution.supported
        ? schemaResolution.targetVersion
        : PlanLedgerService.CURRENT_SCHEMA_VERSION,
      runtimeVersion,
      revision: this.normalizeRevision(sourceRecord.revision),
      version: this.normalizePositiveInteger(sourceRecord.version) || 1,
      parentPlanId: this.normalizeIdentifier(sourceRecord.parentPlanId) || undefined,
      mode: sourceRecord.mode === 'deep' ? 'deep' : 'standard',
      status: this.normalizePlanStatus(sourceRecord.status),
      source: 'orchestrator',
      promptDigest: typeof sourceRecord.promptDigest === 'string' && sourceRecord.promptDigest.trim()
        ? sourceRecord.promptDigest.trim()
        : 'empty',
      summary: typeof sourceRecord.summary === 'string' && sourceRecord.summary.trim()
        ? sourceRecord.summary.trim()
        : '未命名计划',
      analysis: typeof sourceRecord.analysis === 'string' && sourceRecord.analysis.trim()
        ? sourceRecord.analysis.trim()
        : undefined,
      constraints: this.normalizeStringArray(sourceRecord.constraints),
      riskLevel: this.normalizeRiskLevel(sourceRecord.riskLevel),
      review: this.normalizePlanReview(sourceRecord.review),
      runtime: {
        acceptance: {
          criteria: normalizedAcceptanceCriteria,
          summary: acceptanceSummary,
          updatedAt: this.normalizeTimestamp(sourceRecord.runtime?.acceptance?.updatedAt) || now,
        },
        review: {
          round: reviewRound,
          state: reviewState,
          lastReviewedAt: reviewUpdatedAt,
        },
        replan: {
          state: this.normalizeRuntimeReplanState(sourceRecord.runtime?.replan?.state),
          reason: typeof sourceRecord.runtime?.replan?.reason === 'string' && sourceRecord.runtime.replan.reason.trim()
            ? sourceRecord.runtime.replan.reason.trim()
            : undefined,
          updatedAt: this.normalizeTimestamp(sourceRecord.runtime?.replan?.updatedAt),
        },
        wait: {
          state: this.normalizeRuntimeWaitState(sourceRecord.runtime?.wait?.state),
          reasonCode: typeof sourceRecord.runtime?.wait?.reasonCode === 'string' && sourceRecord.runtime.wait.reasonCode.trim()
            ? sourceRecord.runtime.wait.reasonCode.trim()
            : undefined,
          updatedAt: this.normalizeTimestamp(sourceRecord.runtime?.wait?.updatedAt),
        },
        phase: {
          state: this.normalizeRuntimePhaseState(sourceRecord.runtime?.phase?.state),
          currentIndex: this.normalizePositiveInteger(sourceRecord.runtime?.phase?.currentIndex),
          currentTitle: typeof sourceRecord.runtime?.phase?.currentTitle === 'string' && sourceRecord.runtime.phase.currentTitle.trim()
            ? sourceRecord.runtime.phase.currentTitle.trim()
            : undefined,
          nextIndex: this.normalizePositiveInteger(sourceRecord.runtime?.phase?.nextIndex),
          nextTitle: typeof sourceRecord.runtime?.phase?.nextTitle === 'string' && sourceRecord.runtime.phase.nextTitle.trim()
            ? sourceRecord.runtime.phase.nextTitle.trim()
            : undefined,
          remainingPhases: this.normalizeStringArray(sourceRecord.runtime?.phase?.remainingPhases),
          continuationIntent: this.normalizeRuntimePhaseContinuationIntent(sourceRecord.runtime?.phase?.continuationIntent),
          updatedAt: this.normalizeTimestamp(sourceRecord.runtime?.phase?.updatedAt),
        },
        termination: {
          snapshotId: this.normalizeIdentifier(sourceRecord.runtime?.termination?.snapshotId) || undefined,
          reason: typeof sourceRecord.runtime?.termination?.reason === 'string' && sourceRecord.runtime.termination.reason.trim()
            ? sourceRecord.runtime.termination.reason.trim()
            : undefined,
          updatedAt: this.normalizeTimestamp(sourceRecord.runtime?.termination?.updatedAt),
        },
      },
      formattedPlan: typeof sourceRecord.formattedPlan === 'string' && sourceRecord.formattedPlan.trim()
        ? sourceRecord.formattedPlan.trim()
        : undefined,
      items: normalizedItems,
      attempts: normalizedAttempts,
      links: {
        assignmentIds: this.normalizeStringArray(sourceRecord.links?.assignmentIds),
        todoIds: this.normalizeStringArray(sourceRecord.links?.todoIds),
      },
      createdAt: this.normalizeTimestamp(sourceRecord.createdAt) || now,
      updatedAt: this.normalizeTimestamp(sourceRecord.updatedAt) || now,
    };
  }

  private toIndexEntry(record: PlanRecord): PlanIndexEntry {
    return {
      planId: record.planId,
      sessionId: record.sessionId,
      missionId: record.missionId,
      turnId: record.turnId,
      schemaVersion: record.schemaVersion,
      runtimeVersion: record.runtimeVersion,
      revision: record.revision,
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

  private normalizeAcceptanceCriteria(input: unknown): AcceptanceCriterion[] {
    if (!Array.isArray(input)) {
      return [];
    }

    const normalized = input
      .map((entry, index) => this.normalizeAcceptanceCriterion(entry, index))
      .filter((entry): entry is AcceptanceCriterion => Boolean(entry));

    const seen = new Set<string>();
    return normalized.filter((entry) => {
      const key = this.buildAcceptanceCriterionDedupKey(entry);
      if (!key || seen.has(key)) {
        return false;
      }
      seen.add(key);
      return true;
    });
  }

  private buildAcceptanceCriterionDedupKey(entry: AcceptanceCriterion): string {
    const normalizedId = this.normalizeIdentifier(entry.id);
    if (normalizedId) {
      return `id:${normalizedId}`;
    }
    const description = entry.description.trim().toLowerCase();
    const scope = typeof entry.scope === 'string' ? entry.scope.trim().toLowerCase() : '';
    const owner = typeof entry.owner === 'string' ? entry.owner.trim().toLowerCase() : '';
    return `tuple:${description}::${scope}::${owner}`;
  }

  private normalizeAcceptanceCriterion(input: unknown, index: number): AcceptanceCriterion | null {
    if (typeof input === 'string') {
      return this.buildAcceptanceCriterion(input, index);
    }
    if (!input || typeof input !== 'object') {
      return null;
    }

    const candidate = input as Partial<AcceptanceCriterion>;
    const description = typeof candidate.description === 'string' ? candidate.description.trim() : '';
    if (!description) {
      return null;
    }

    return {
      id: this.normalizeIdentifier(candidate.id) || `acceptance-${index + 1}`,
      description,
      verifiable: candidate.verifiable !== false,
      verificationMethod: candidate.verificationMethod === 'manual' || candidate.verificationMethod === 'test'
        ? candidate.verificationMethod
        : 'auto',
      status: candidate.status === 'passed' || candidate.status === 'failed' ? candidate.status : 'pending',
      evidence: this.normalizeAcceptanceEvidence(candidate.evidence),
      owner: this.normalizeAcceptanceOwner(candidate.owner),
      scope: typeof candidate.scope === 'string' && candidate.scope.trim().length > 0
        ? candidate.scope.trim()
        : undefined,
      lastBatchId: this.normalizeIdentifier(candidate.lastBatchId),
      lastWorkerId: this.normalizeAcceptanceOwner(candidate.lastWorkerId),
      reviewHistory: this.normalizeAcceptanceReviewHistory(candidate.reviewHistory),
      verificationSpec: candidate.verificationSpec,
    };
  }

  private buildAcceptanceCriterion(description: string, index: number): AcceptanceCriterion | null {
    const normalized = description.trim();
    if (!normalized) {
      return null;
    }
    return {
      id: `acceptance-${index + 1}`,
      description: normalized,
      verifiable: true,
      verificationMethod: 'auto',
      status: 'pending',
    };
  }

  private normalizeAcceptanceEvidence(evidence: unknown): string[] | undefined {
    const normalized = this.normalizeStringArray(
      Array.isArray(evidence) ? evidence.filter((item): item is string => typeof item === 'string') : undefined,
    );
    return normalized.length > 0 ? normalized : undefined;
  }

  private normalizeAcceptanceOwner(value: unknown): AcceptanceCriterion['owner'] | undefined {
    const normalized = typeof value === 'string' ? value.trim() : '';
    if (normalized === 'claude' || normalized === 'codex' || normalized === 'gemini' || normalized === 'orchestrator') {
      return normalized;
    }
    return undefined;
  }

  private normalizeAcceptanceReviewHistory(
    input: unknown,
  ): AcceptanceCriterion['reviewHistory'] | undefined {
    if (!Array.isArray(input)) {
      return undefined;
    }
    const normalized = input
      .map((entry) => {
        if (!entry || typeof entry !== 'object') {
          return null;
        }
        const candidate = entry as {
          status?: string;
          reviewer?: string;
          detail?: string;
          reviewedAt?: unknown;
          round?: unknown;
          batchId?: unknown;
          workerId?: unknown;
        };
        const status: 'pending' | 'passed' | 'failed' =
          candidate.status === 'passed' || candidate.status === 'failed' || candidate.status === 'pending'
          ? candidate.status
          : 'pending';
        const reviewer = typeof candidate.reviewer === 'string' && candidate.reviewer.trim().length > 0
          ? candidate.reviewer.trim()
          : 'system';
        const reviewedAt = this.normalizeTimestamp(candidate.reviewedAt);
        if (reviewedAt === undefined) {
          return null;
        }
        const detail = typeof candidate.detail === 'string' && candidate.detail.trim().length > 0
          ? candidate.detail.trim()
          : undefined;
        const round = this.normalizePositiveInteger(candidate.round);
        const batchId = this.normalizeIdentifier(candidate.batchId);
        const workerId = this.normalizeAcceptanceOwner(candidate.workerId);
        return {
          status,
          reviewer,
          detail,
          reviewedAt,
          round,
          batchId,
          workerId,
        };
      })
      .filter((entry): entry is NonNullable<typeof entry> => Boolean(entry));
    return normalized.length > 0 ? normalized : undefined;
  }

  private createInitialRuntimeState(
    acceptanceCriteria: AcceptanceCriterion[],
    runtimeVersion: PlanRuntimeVersion,
    now: number,
  ): PlanRuntimeState {
    return {
      acceptance: {
        criteria: acceptanceCriteria,
        summary: this.computeAcceptanceSummary(acceptanceCriteria),
        updatedAt: now,
      },
      review: {
        round: 0,
        state: runtimeVersion === 'deep_v1' ? 'idle' : 'accepted',
      },
      replan: {
        state: 'none',
      },
      wait: {
        state: 'none',
      },
      phase: {
        state: runtimeVersion === 'deep_v1' ? 'running' : 'idle',
        currentIndex: runtimeVersion === 'deep_v1' ? 1 : undefined,
        currentTitle: runtimeVersion === 'deep_v1' ? 'Phase 1' : undefined,
        remainingPhases: [],
        continuationIntent: 'stop',
      },
      termination: {},
    };
  }

  private computeAcceptanceSummary(criteria: AcceptanceCriterion[]): PlanAcceptanceSummary {
    if (criteria.length === 0) {
      return 'pending';
    }
    const passedCount = criteria.filter((item) => item.status === 'passed').length;
    const failedCount = criteria.filter((item) => item.status === 'failed').length;
    if (failedCount > 0) {
      return 'failed';
    }
    if (passedCount === criteria.length) {
      return 'passed';
    }
    if (passedCount > 0) {
      return 'partial';
    }
    return 'pending';
  }

  private normalizeAcceptanceSummary(input: unknown, criteria: AcceptanceCriterion[]): PlanAcceptanceSummary {
    if (input === 'pending' || input === 'partial' || input === 'passed' || input === 'failed') {
      return input;
    }
    return this.computeAcceptanceSummary(criteria);
  }

  private getAcceptanceDescriptions(criteria: AcceptanceCriterion[]): string[] {
    return criteria
      .map((item) => item.description.trim())
      .filter((item) => item.length > 0);
  }

  private resolveRuntimeVersion(mode: 'standard' | 'deep'): PlanRuntimeVersion {
    return mode === 'deep' ? 'deep_v1' : 'classic';
  }

  private resolveSchemaVersion(input: unknown): SchemaVersionResolution {
    const declared = this.normalizePositiveInteger(input);
    if (declared === undefined) {
      return {
        sourceVersion: PlanLedgerService.MIN_SUPPORTED_SCHEMA_VERSION,
        targetVersion: PlanLedgerService.CURRENT_SCHEMA_VERSION,
        supported: true,
        shouldMigrate: true,
        sourceDeclared: false,
      };
    }
    if (declared < PlanLedgerService.MIN_SUPPORTED_SCHEMA_VERSION) {
      return {
        sourceVersion: declared,
        targetVersion: declared,
        supported: false,
        shouldMigrate: false,
        sourceDeclared: true,
        reason: 'too_old',
      };
    }
    if (declared > PlanLedgerService.CURRENT_SCHEMA_VERSION) {
      return {
        sourceVersion: declared,
        targetVersion: declared,
        supported: false,
        shouldMigrate: false,
        sourceDeclared: true,
        reason: 'too_new',
      };
    }
    return {
      sourceVersion: declared,
      targetVersion: PlanLedgerService.CURRENT_SCHEMA_VERSION,
      supported: true,
      shouldMigrate: declared !== PlanLedgerService.CURRENT_SCHEMA_VERSION,
      sourceDeclared: true,
    };
  }

  private normalizeRuntimeVersion(input: unknown, mode: unknown): PlanRuntimeVersion {
    if (input === 'classic' || input === 'deep_v1') {
      return input;
    }
    return mode === 'deep' ? 'deep_v1' : 'classic';
  }

  private normalizeRevision(input: unknown): number {
    return this.normalizePositiveInteger(input) || 1;
  }

  private normalizePositiveInteger(input: unknown): number | undefined {
    return Number.isFinite(input) && Number(input) > 0 ? Math.floor(Number(input)) : undefined;
  }

  private normalizeTimestamp(input: unknown): number | undefined {
    return Number.isFinite(input) && Number(input) > 0 ? Number(input) : undefined;
  }

  private normalizePlanStatus(input: unknown): PlanStatus {
    const allowed: PlanStatus[] = [
      'draft',
      'awaiting_confirmation',
      'approved',
      'rejected',
      'executing',
      'partially_completed',
      'completed',
      'failed',
      'cancelled',
      'superseded',
    ];
    return allowed.includes(input as PlanStatus) ? input as PlanStatus : 'draft';
  }

  private normalizeRiskLevel(input: unknown): PlanRecord['riskLevel'] {
    return input === 'low' || input === 'medium' || input === 'high' || input === 'critical'
      ? input
      : undefined;
  }

  private normalizePlanReview(input: unknown): PlanRecord['review'] {
    if (!input || typeof input !== 'object') {
      return undefined;
    }
    const candidate = input as Partial<NonNullable<PlanRecord['review']>>;
    if (candidate.status !== 'approved' && candidate.status !== 'rejected' && candidate.status !== 'skipped') {
      return undefined;
    }
    const reviewedAt = this.normalizeTimestamp(candidate.reviewedAt);
    if (!reviewedAt) {
      return undefined;
    }
    return {
      status: candidate.status,
      reviewer: typeof candidate.reviewer === 'string' && candidate.reviewer.trim() ? candidate.reviewer.trim() : undefined,
      reason: typeof candidate.reason === 'string' && candidate.reason.trim() ? candidate.reason.trim() : undefined,
      reviewedAt,
    };
  }

  private normalizeRuntimeReviewState(
    input: unknown,
    reviewStatus?: NonNullable<PlanRecord['review']>['status'],
  ): PlanRuntimeState['review']['state'] {
    if (input === 'idle' || input === 'running' || input === 'accepted' || input === 'rejected') {
      return input;
    }
    if (reviewStatus === 'approved') {
      return 'accepted';
    }
    if (reviewStatus === 'rejected') {
      return 'rejected';
    }
    return 'idle';
  }

  private normalizeRuntimeReplanState(input: unknown): PlanRuntimeState['replan']['state'] {
    return input === 'required' || input === 'awaiting_confirmation' || input === 'applied' ? input : 'none';
  }

  private normalizeRuntimeWaitState(input: unknown): PlanRuntimeState['wait']['state'] {
    return input === 'external_waiting' ? 'external_waiting' : 'none';
  }

  private normalizeRuntimePhaseState(input: unknown): PlanRuntimeState['phase']['state'] {
    return input === 'running'
      || input === 'awaiting_next_phase'
      || input === 'completed'
      ? input
      : 'idle';
  }

  private normalizeRuntimePhaseContinuationIntent(input: unknown): PlanRuntimeState['phase']['continuationIntent'] {
    return input === 'continue' ? 'continue' : 'stop';
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
      const event: PlanLedgerEventRecord = {
        timestamp: Date.now(),
        reason,
        sessionId: record.sessionId,
        planId: record.planId,
        missionId: record.missionId,
        schemaVersion: record.schemaVersion,
        runtimeVersion: record.runtimeVersion,
        revision: record.revision,
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
      const eventsFilePath = this.getEventsFilePath(record.sessionId, record.planId);
      const line = `${JSON.stringify(event)}\n`;
      this.eventAppendQueue.enqueue(eventsFilePath, async () => {
        await this.rotateEventsFileIfNeededAsync(record.sessionId, record.planId);
        await fs.promises.mkdir(path.dirname(eventsFilePath), { recursive: true });
        await fs.promises.appendFile(eventsFilePath, line, 'utf-8');
      });
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
      constraints: [...record.constraints],
      links: {
        assignmentIds: [...record.links.assignmentIds],
        todoIds: [...record.links.todoIds],
      },
      review: record.review ? { ...record.review } : undefined,
      runtime: {
        acceptance: {
          ...record.runtime.acceptance,
          criteria: record.runtime.acceptance.criteria.map((criterion) => ({
            ...criterion,
            evidence: Array.isArray(criterion.evidence)
              ? [...criterion.evidence]
              : undefined,
            verificationSpec: criterion.verificationSpec ? { ...criterion.verificationSpec } : undefined,
            reviewHistory: Array.isArray(criterion.reviewHistory)
              ? criterion.reviewHistory.map((entry) => ({ ...entry }))
              : undefined,
          })),
        },
        review: { ...record.runtime.review },
        replan: { ...record.runtime.replan },
        wait: { ...record.runtime.wait },
        phase: {
          ...record.runtime.phase,
          remainingPhases: [...record.runtime.phase.remainingPhases],
        },
        termination: { ...record.runtime.termination },
      },
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
      this.sessionMutationQueues.delete(oldestSessionId);
    }
  }

  private async rotateEventsFileIfNeededAsync(sessionId: string, planId: string): Promise<void> {
    const eventsFilePath = this.getEventsFilePath(sessionId, planId);
    try {
      await fs.promises.access(eventsFilePath, fs.constants.F_OK);
    } catch {
      return;
    }

    const stats = await fs.promises.stat(eventsFilePath);
    if (stats.size < PlanLedgerService.EVENTS_ROTATE_MAX_BYTES) {
      return;
    }

    const rotateSuffix = `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
    const rotatedFilePath = path.join(this.getPlansDir(sessionId), `${planId}.events.${rotateSuffix}.jsonl`);
    await fs.promises.rename(eventsFilePath, rotatedFilePath);
    await this.pruneRotatedEventFilesAsync(sessionId, planId);
  }

  private async pruneRotatedEventFilesAsync(sessionId: string, planId: string): Promise<void> {
    const plansDir = this.getPlansDir(sessionId);
    try {
      await fs.promises.access(plansDir, fs.constants.F_OK);
    } catch {
      return;
    }

    const prefix = `${planId}.events.`;
    const suffix = '.jsonl';
    const rotatedFiles = (await fs.promises.readdir(plansDir))
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
        await fs.promises.unlink(path.join(plansDir, staleFile.fileName));
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

  async flushPendingWrites(): Promise<void> {
    await Promise.all([
      this.snapshotPersistQueue.flushAll(),
      this.eventAppendQueue.flushAll(),
    ]);
  }

  private runWithSessionQueue<T>(sessionId: string, operation: () => Promise<T>): Promise<T> {
    this.touchSessionCache(sessionId);
    const previous = this.sessionMutationQueues.get(sessionId) || Promise.resolve();
    const next = previous.then(operation, operation);
    const queueTail = next.then(
      () => undefined,
      () => undefined,
    );
    this.sessionMutationQueues.set(
      sessionId,
      queueTail,
    );
    void next.finally(() => {
      if (this.sessionMutationQueues.get(sessionId) === queueTail) {
        this.sessionMutationQueues.delete(sessionId);
      }
    });
    return next;
  }
}
