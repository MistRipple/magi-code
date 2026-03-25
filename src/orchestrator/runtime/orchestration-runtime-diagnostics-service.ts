import type { Mission, MissionStorageManager } from '../mission';
import type { PlanLedgerService, PlanRecord, PlanStatus } from '../plan-ledger';
import type { RuntimeTerminationDecisionTraceEntry, RuntimeTerminationSnapshot } from '../core/orchestration-control-plane-types';
import {
  FileKnowledgeGovernanceAuditStore,
  type KnowledgeGovernanceAuditQuery,
  type KnowledgeGovernanceAuditRecord,
} from '../../knowledge/knowledge-governance-audit-store';
import { OrchestrationReadModelService } from './orchestration-read-model-service';
import {
  OrchestrationTimelineStore,
  type OrchestrationTimelineEvent,
} from './orchestration-timeline-store';
import type {
  OrchestrationRuntimeAssignmentSummary,
  OrchestrationRuntimeDiagnosticsQuery,
  OrchestrationRuntimeDiagnosticsSnapshot,
  OrchestrationRuntimeFailureRootCause,
  OrchestrationRuntimeKnowledgeAuditEntry,
  OrchestrationRuntimeKnowledgeAuditView,
  OrchestrationRuntimeMissionSummary,
  OrchestrationRuntimeOpsView,
  OrchestrationRuntimePlanSummary,
  OrchestrationRuntimeRecoverySummary,
  OrchestrationRuntimeStateDiffEntry,
  OrchestrationRuntimeTimelineEntry,
} from './orchestration-runtime-diagnostics-types';

function normalizeString(value: unknown): string | undefined {
  if (typeof value !== 'string') {
    return undefined;
  }
  const normalized = value.trim();
  return normalized || undefined;
}

function normalizeStringArray(value: unknown): string[] {
  if (!Array.isArray(value)) {
    return [];
  }
  const seen = new Set<string>();
  const normalized: string[] = [];
  for (const item of value) {
    if (typeof item !== 'string') {
      continue;
    }
    const next = item.trim();
    if (!next || seen.has(next)) {
      continue;
    }
    seen.add(next);
    normalized.push(next);
  }
  return normalized;
}

export class OrchestrationRuntimeDiagnosticsService {
  private readonly knowledgeAuditStore: FileKnowledgeGovernanceAuditStore;

  constructor(
    private readonly missionStorage: MissionStorageManager,
    private readonly planLedger: PlanLedgerService,
    private readonly readModelService: OrchestrationReadModelService,
    private readonly timelineStore: OrchestrationTimelineStore,
    workspaceRoot: string,
  ) {
    this.knowledgeAuditStore = new FileKnowledgeGovernanceAuditStore(workspaceRoot);
  }

  async query(input: OrchestrationRuntimeDiagnosticsQuery): Promise<OrchestrationRuntimeDiagnosticsSnapshot | null> {
    const sessionId = normalizeString(input.sessionId);
    if (!sessionId) {
      return null;
    }

    const sessionEvents = this.timelineStore.replay({ sessionId });
    const missionId = normalizeString(input.missionId)
      || this.resolveLatestMissionId(sessionEvents, {
        requestId: normalizeString(input.requestId),
        planId: normalizeString(input.planId),
      });
    const mission = await this.resolveMission(sessionId, missionId);
    const missionProjection = mission
      ? this.readModelService.toMissionProjection(mission)
      : null;
    const plan = this.resolvePlan(sessionId, normalizeString(input.planId), missionProjection?.missionId);
    const requestId = normalizeString(input.requestId)
      || this.resolveLatestRequestId(sessionEvents, {
        missionId: missionProjection?.missionId,
        planId: plan?.planId,
      });
    const batchId = normalizeString(input.batchId)
      || this.resolveLatestBatchId(sessionEvents, {
        requestId,
        missionId: missionProjection?.missionId,
        planId: plan?.planId,
      });
    const scopedEvents = this.selectScopedEvents(sessionEvents, {
      requestId,
      missionId: missionProjection?.missionId,
      planId: plan?.planId,
      batchId,
    });

    const runtimeReason = normalizeString(input.liveRuntimeReason)
      || this.resolveRuntimeReason(scopedEvents)
      || normalizeString(plan?.runtime.termination.reason)
      || (this.hasFailureSignal(scopedEvents, missionProjection?.failureReason) ? 'failed' : 'completed');
    const finalStatus = this.resolveFinalStatus({
      explicit: input.liveFinalStatus,
      events: scopedEvents,
      planStatus: plan?.status,
      runtimeReason,
    });
    const failureRootCause = this.resolveFailureRootCause(scopedEvents, mission, plan);
    const liveErrors = normalizeStringArray(input.liveErrors);
    const errors = liveErrors.length > 0
      ? liveErrors
      : this.resolveErrorsFromFailure(failureRootCause, missionProjection?.failureReason);
    const failureReason = normalizeString(input.liveFailureReason)
      || (finalStatus === 'failed'
        ? failureRootCause?.summary || missionProjection?.failureReason || normalizeString(plan?.runtime.replan.reason)
        : undefined);
    const updatedAt = this.resolveUpdatedAt({
      events: scopedEvents,
      plan,
      mission,
      explicitUpdatedAt: input.updatedAt,
    });

    if (!missionProjection && !plan && scopedEvents.length === 0 && !input.liveRuntimeReason && !input.liveFinalStatus) {
      return null;
    }

    const recentTimeline = scopedEvents
      .slice(-this.normalizeLimit(input.maxTimelineEvents, 18, 4, 40))
      .map((event) => this.mapTimelineEntry(event));
    const recentStateDiffs = this.extractStateDiffs(
      scopedEvents,
      this.normalizeLimit(input.maxStateDiffs, 12, 4, 32),
    );
    const assignments = this.mapAssignmentSummaries(mission);
    const knowledgeAudit = this.buildKnowledgeAuditView({
      sessionId,
      ...(requestId ? { requestId } : {}),
      ...(missionProjection?.missionId ? { missionId: missionProjection.missionId } : {}),
      limit: this.normalizeLimit(input.maxKnowledgeAuditEntries, 8, 1, 24),
    });
    const opsView: OrchestrationRuntimeOpsView = {
      scope: {
        sessionId,
        ...(requestId ? { requestId } : {}),
        ...(missionProjection?.missionId ? { missionId: missionProjection.missionId } : {}),
        ...(plan?.planId ? { planId: plan.planId } : {}),
        ...(batchId ? { batchId } : {}),
      },
      timelinePath: this.timelineStore.getStoragePath(sessionId),
      eventCount: scopedEvents.length,
      diffCount: scopedEvents.reduce((sum, event) => sum + (event.diffs?.length || 0), 0),
      ...(missionProjection ? { mission: this.mapMissionSummary(missionProjection) } : {}),
      ...(plan ? { plan: this.mapPlanSummary(plan) } : {}),
      recentTimeline,
      recentStateDiffs,
      assignments,
      ...(failureRootCause ? { failureRootCause } : {}),
      recovery: this.buildRecoverySummary(mission, plan),
      knowledgeAudit,
    };

    return {
      sessionId,
      ...(requestId ? { requestId } : {}),
      runtimeReason,
      finalStatus,
      ...(failureReason ? { failureReason } : {}),
      errors,
      runtimeSnapshot: this.cloneRuntimeSnapshot(input.liveRuntimeSnapshot),
      runtimeDecisionTrace: this.cloneDecisionTrace(input.liveRuntimeDecisionTrace),
      updatedAt,
      opsView,
    };
  }

  private async resolveMission(sessionId: string, missionId?: string): Promise<Mission | null> {
    const normalizedMissionId = normalizeString(missionId);
    if (normalizedMissionId) {
      return this.missionStorage.load(normalizedMissionId);
    }
    return this.missionStorage.getLatestBySession(sessionId);
  }

  private resolvePlan(sessionId: string, planId?: string, missionId?: string): PlanRecord | null {
    const normalizedPlanId = normalizeString(planId);
    if (normalizedPlanId) {
      return this.planLedger.getPlan(sessionId, normalizedPlanId);
    }
    const normalizedMissionId = normalizeString(missionId);
    if (normalizedMissionId) {
      return this.planLedger.getLatestPlanByMission(sessionId, normalizedMissionId, { includeTerminal: true });
    }
    return this.planLedger.getLatestPlan(sessionId);
  }

  private resolveLatestMissionId(
    events: OrchestrationTimelineEvent[],
    scope: { requestId?: string; planId?: string },
  ): string | undefined {
    for (let index = events.length - 1; index >= 0; index -= 1) {
      const event = events[index];
      if (scope.requestId && event.trace.requestId !== scope.requestId) {
        continue;
      }
      if (scope.planId && event.trace.planId !== scope.planId) {
        continue;
      }
      const missionId = normalizeString(event.trace.missionId);
      if (missionId) {
        return missionId;
      }
    }
    return undefined;
  }

  private resolveLatestRequestId(
    events: OrchestrationTimelineEvent[],
    scope: { missionId?: string; planId?: string },
  ): string | undefined {
    for (let index = events.length - 1; index >= 0; index -= 1) {
      const event = events[index];
      if (scope.missionId && event.trace.missionId !== scope.missionId) {
        continue;
      }
      if (scope.planId && event.trace.planId !== scope.planId && event.type !== 'dispatch.batch.created') {
        continue;
      }
      const requestId = normalizeString(event.trace.requestId);
      if (requestId) {
        return requestId;
      }
    }
    return undefined;
  }

  private resolveLatestBatchId(
    events: OrchestrationTimelineEvent[],
    scope: { requestId?: string; missionId?: string; planId?: string },
  ): string | undefined {
    for (let index = events.length - 1; index >= 0; index -= 1) {
      const event = events[index];
      if (scope.requestId && event.trace.requestId !== scope.requestId) {
        continue;
      }
      if (scope.missionId && event.trace.missionId !== scope.missionId) {
        continue;
      }
      if (scope.planId && event.trace.planId !== scope.planId && event.type !== 'dispatch.batch.created') {
        continue;
      }
      const batchId = normalizeString(event.trace.batchId);
      if (batchId) {
        return batchId;
      }
    }
    return undefined;
  }

  private selectScopedEvents(
    events: OrchestrationTimelineEvent[],
    scope: { requestId?: string; missionId?: string; planId?: string; batchId?: string },
  ): OrchestrationTimelineEvent[] {
    const scopeValues = [
      ['requestId', scope.requestId],
      ['missionId', scope.missionId],
      ['planId', scope.planId],
      ['batchId', scope.batchId],
    ].filter((entry): entry is [keyof OrchestrationTimelineEvent['trace'], string] => Boolean(entry[1]));
    if (scopeValues.length === 0) {
      return events;
    }
    const filtered = events.filter((event) => scopeValues.some(([key, value]) => event.trace[key] === value));
    return filtered.length > 0 ? filtered : events;
  }

  private resolveRuntimeReason(events: OrchestrationTimelineEvent[]): string | undefined {
    for (let index = events.length - 1; index >= 0; index -= 1) {
      const event = events[index];
      const payload = event.payload || {};
      const runtimeReason = normalizeString(payload.runtimeReason);
      if (runtimeReason) {
        return runtimeReason;
      }
    }
    return undefined;
  }

  private hasFailureSignal(events: OrchestrationTimelineEvent[], missionFailureReason?: string): boolean {
    if (normalizeString(missionFailureReason)) {
      return true;
    }
    return events.some((event) => {
      if (event.type === 'todo.failed') {
        return true;
      }
      const payload = event.payload || {};
      return payload.status === 'failed' || payload.finalStatus === 'failed';
    });
  }

  private resolveFinalStatus(input: {
    explicit?: 'completed' | 'failed' | 'cancelled' | 'paused';
    events: OrchestrationTimelineEvent[];
    planStatus?: PlanStatus;
    runtimeReason?: string;
  }): 'completed' | 'failed' | 'cancelled' | 'paused' {
    if (input.explicit) {
      return input.explicit;
    }
    for (let index = input.events.length - 1; index >= 0; index -= 1) {
      const payload = input.events[index].payload || {};
      const finalStatus = payload.finalStatus;
      if (finalStatus === 'completed' || finalStatus === 'failed' || finalStatus === 'cancelled' || finalStatus === 'paused') {
        return finalStatus;
      }
    }

    // 运行终止原因比计划状态更接近“本次请求已如何结束”的事实。
    // 若 runtimeReason 已明确收敛到终态，不应再被 executing/awaiting_confirmation 之类的计划快照覆盖。
    if (input.runtimeReason === 'interrupted' || input.runtimeReason === 'cancelled' || input.runtimeReason === 'external_abort') {
      return 'cancelled';
    }
    if (input.runtimeReason === 'stalled' || input.runtimeReason === 'budget_exceeded' || input.runtimeReason === 'external_wait_timeout' || input.runtimeReason === 'upstream_model_error') {
      return 'paused';
    }
    if (input.runtimeReason === 'failed') {
      return 'failed';
    }
    if (input.runtimeReason === 'completed') {
      return 'completed';
    }

    switch (input.planStatus) {
      case 'completed':
        return 'completed';
      case 'failed':
        return 'failed';
      case 'cancelled':
        return 'cancelled';
      case 'executing':
      case 'partially_completed':
      case 'awaiting_confirmation':
        return 'paused';
      default:
        return 'completed';
    }
  }

  private resolveFailureRootCause(
    events: OrchestrationTimelineEvent[],
    mission: Mission | null,
    plan: PlanRecord | null,
  ): OrchestrationRuntimeFailureRootCause | undefined {
    for (let index = events.length - 1; index >= 0; index -= 1) {
      const event = events[index];
      const payload = event.payload || {};
      const eventMarkedFailed = event.type === 'todo.failed'
        || payload.status === 'failed'
        || payload.finalStatus === 'failed';
      if (!eventMarkedFailed) {
        continue;
      }
      const error = normalizeString(payload.error);
      const summary = error
        || normalizeString(payload.summary)
        || mission?.failureReason
        || normalizeString(plan?.runtime.replan.reason)
        || event.summary;
      return {
        summary,
        eventType: event.type,
        eventId: event.eventId,
        occurredAt: event.timestamp,
        ...(normalizeString(event.trace.assignmentId) ? { assignmentId: event.trace.assignmentId } : {}),
        ...(normalizeString(event.trace.todoId) ? { todoId: event.trace.todoId } : {}),
        ...(normalizeString(event.trace.verificationId) ? { verificationId: event.trace.verificationId } : {}),
        ...(error ? { error } : {}),
      };
    }

    const fallback = mission?.failureReason || normalizeString(plan?.runtime.replan.reason);
    if (!fallback) {
      return undefined;
    }
    return {
      summary: fallback,
      occurredAt: mission?.updatedAt || plan?.updatedAt || Date.now(),
    };
  }

  private resolveErrorsFromFailure(
    failureRootCause: OrchestrationRuntimeFailureRootCause | undefined,
    missionFailureReason?: string,
  ): string[] {
    const fallback = failureRootCause?.error || failureRootCause?.summary || normalizeString(missionFailureReason);
    return fallback ? [fallback] : [];
  }

  private resolveUpdatedAt(input: {
    events: OrchestrationTimelineEvent[];
    plan: PlanRecord | null;
    mission: Mission | null;
    explicitUpdatedAt?: number;
  }): number {
    const explicit = typeof input.explicitUpdatedAt === 'number' && Number.isFinite(input.explicitUpdatedAt)
      ? input.explicitUpdatedAt
      : 0;
    const eventUpdatedAt = input.events.length > 0 ? input.events[input.events.length - 1].timestamp : 0;
    const planUpdatedAt = typeof input.plan?.updatedAt === 'number' ? input.plan.updatedAt : 0;
    const missionUpdatedAt = typeof input.mission?.updatedAt === 'number' ? input.mission.updatedAt : 0;
    return Math.max(explicit, eventUpdatedAt, planUpdatedAt, missionUpdatedAt, Date.now());
  }

  private mapMissionSummary(mission: Awaited<ReturnType<OrchestrationReadModelService['getMissionProjection']>> extends infer T ? Exclude<T, null> : never): OrchestrationRuntimeMissionSummary {
    return {
      missionId: mission.missionId,
      title: mission.title || mission.goal || mission.prompt,
      status: mission.status,
      deliveryStatus: mission.deliveryStatus,
      updatedAt: mission.updatedAt,
      ...(mission.failureReason ? { failureReason: mission.failureReason } : {}),
    };
  }

  private mapPlanSummary(plan: PlanRecord): OrchestrationRuntimePlanSummary {
    return {
      planId: plan.planId,
      status: plan.status,
      mode: plan.mode,
      revision: plan.revision,
      version: plan.version,
      updatedAt: plan.updatedAt,
      acceptanceSummary: plan.runtime.acceptance.summary,
      waitState: plan.runtime.wait.state,
      replanState: plan.runtime.replan.state,
      ...(normalizeString(plan.runtime.termination.reason) ? { terminationReason: plan.runtime.termination.reason } : {}),
    };
  }

  private mapAssignmentSummaries(mission: Mission | null): OrchestrationRuntimeAssignmentSummary[] {
    if (!mission) {
      return [];
    }
    return mission.assignments
      .map((assignment): OrchestrationRuntimeAssignmentSummary => {
        const todoTotal = assignment.todos.length;
        const completedTodos = assignment.todos.filter((todo) => todo.status === 'completed' || todo.status === 'skipped').length;
        const failedTodos = assignment.todos.filter((todo) => todo.status === 'failed' || todo.status === 'cancelled' || todo.status === 'blocked').length;
        const runningTodos = assignment.todos.filter((todo) => todo.status === 'running' || todo.status === 'pending' || todo.status === 'ready').length;
        return {
          assignmentId: assignment.id,
          workerId: assignment.workerId,
          title: assignment.shortTitle || assignment.responsibility,
          status: assignment.status,
          progress: assignment.progress,
          todoTotal,
          completedTodos,
          failedTodos,
          runningTodos,
          trace: assignment.trace,
        };
      })
      .sort((left, right) => {
        const leftUpdatedAt = Math.max(left.progress, 0);
        const rightUpdatedAt = Math.max(right.progress, 0);
        if (leftUpdatedAt !== rightUpdatedAt) {
          return rightUpdatedAt - leftUpdatedAt;
        }
        return left.assignmentId.localeCompare(right.assignmentId);
      });
  }

  private buildRecoverySummary(
    mission: Mission | null,
    plan: PlanRecord | null,
  ): OrchestrationRuntimeRecoverySummary | undefined {
    if (!mission && !plan) {
      return undefined;
    }
    return {
      ...(mission?.continuationPolicy ? { continuationPolicy: mission.continuationPolicy } : {}),
      ...(normalizeString(mission?.continuationReason) ? { continuationReason: mission?.continuationReason } : {}),
      ...(plan ? {
        waitState: plan.runtime.wait.state,
        waitReasonCode: plan.runtime.wait.reasonCode,
        replanState: plan.runtime.replan.state,
        replanReason: plan.runtime.replan.reason,
        terminationReason: plan.runtime.termination.reason,
        acceptanceSummary: plan.runtime.acceptance.summary,
        reviewState: plan.runtime.review.state,
      } : {}),
    };
  }

  private buildKnowledgeAuditView(
    query: KnowledgeGovernanceAuditQuery,
  ): OrchestrationRuntimeKnowledgeAuditView {
    return {
      auditPath: this.knowledgeAuditStore.getStoragePath(),
      eventCount: this.knowledgeAuditStore.count(query),
      recentEntries: this.knowledgeAuditStore.query(query).map((record) => this.mapKnowledgeAuditEntry(record)),
    };
  }

  private mapKnowledgeAuditEntry(
    record: KnowledgeGovernanceAuditRecord,
  ): OrchestrationRuntimeKnowledgeAuditEntry {
    return {
      eventId: record.eventId,
      timestamp: record.timestamp,
      purpose: record.purpose,
      resultKind: record.resultKind,
      referenceCount: record.referenceCount,
      ...(normalizeString(record.consumer) ? { consumer: record.consumer } : {}),
      ...(normalizeString(record.sessionId) ? { sessionId: record.sessionId } : {}),
      ...(normalizeString(record.requestId) ? { requestId: record.requestId } : {}),
      ...(normalizeString(record.missionId) ? { missionId: record.missionId } : {}),
      ...(normalizeString(record.assignmentId) ? { assignmentId: record.assignmentId } : {}),
      ...(normalizeString(record.todoId) ? { todoId: record.todoId } : {}),
      ...(normalizeString(record.workerId) ? { workerId: record.workerId } : {}),
    };
  }

  private mapTimelineEntry(event: OrchestrationTimelineEvent): OrchestrationRuntimeTimelineEntry {
    return {
      eventId: event.eventId,
      seq: event.seq,
      timestamp: event.timestamp,
      type: event.type,
      summary: event.summary,
      diffCount: event.diffs?.length || 0,
      trace: event.trace,
    };
  }

  private extractStateDiffs(
    events: OrchestrationTimelineEvent[],
    limit: number,
  ): OrchestrationRuntimeStateDiffEntry[] {
    const flattened: OrchestrationRuntimeStateDiffEntry[] = [];
    for (const event of events) {
      for (const diff of event.diffs || []) {
        flattened.push({
          eventId: event.eventId,
          timestamp: event.timestamp,
          entityType: diff.entityType,
          entityId: diff.entityId,
          changedKeys: this.resolveChangedKeys(diff.before, diff.after),
          beforeSummary: this.summarizeState(diff.before),
          afterSummary: this.summarizeState(diff.after),
        });
      }
    }
    return flattened.slice(-limit);
  }

  private resolveChangedKeys(
    before?: Record<string, unknown>,
    after?: Record<string, unknown>,
  ): string[] {
    const keys = new Set<string>([
      ...Object.keys(before || {}),
      ...Object.keys(after || {}),
    ]);
    return Array.from(keys)
      .filter((key) => JSON.stringify(before?.[key]) !== JSON.stringify(after?.[key]))
      .sort((left, right) => left.localeCompare(right));
  }

  private summarizeState(value?: Record<string, unknown>): string | undefined {
    if (!value || typeof value !== 'object') {
      return undefined;
    }
    const preferredKeys = ['status', 'summary', 'runtimeReason', 'phase', 'requestId', 'missionId'];
    const parts: string[] = [];
    for (const key of preferredKeys) {
      const item = value[key];
      const formatted = this.formatScalar(item);
      if (formatted) {
        parts.push(`${key}=${formatted}`);
      }
    }
    if (parts.length > 0) {
      return parts.join(' · ');
    }
    const keys = Object.keys(value).slice(0, 4);
    if (keys.length === 0) {
      return undefined;
    }
    return keys
      .map((key) => `${key}=${this.formatScalar(value[key]) || 'object'}`)
      .join(' · ');
  }

  private formatScalar(value: unknown): string | undefined {
    if (typeof value === 'string') {
      const normalized = value.trim();
      return normalized || undefined;
    }
    if (typeof value === 'number' && Number.isFinite(value)) {
      return String(value);
    }
    if (typeof value === 'boolean') {
      return value ? 'true' : 'false';
    }
    return undefined;
  }

  private cloneRuntimeSnapshot(input?: RuntimeTerminationSnapshot | null): RuntimeTerminationSnapshot | null {
    if (!input) {
      return null;
    }
    return {
      ...input,
      progressVector: input.progressVector ? { ...input.progressVector } : undefined,
      reviewState: input.reviewState ? { ...input.reviewState } : undefined,
      blockerState: input.blockerState ? { ...input.blockerState } : undefined,
      budgetState: input.budgetState ? { ...input.budgetState } : undefined,
      sourceEventIds: Array.isArray(input.sourceEventIds) ? [...input.sourceEventIds] : undefined,
    };
  }

  private cloneDecisionTrace(input?: RuntimeTerminationDecisionTraceEntry[]): RuntimeTerminationDecisionTraceEntry[] {
    if (!Array.isArray(input)) {
      return [];
    }
    return input.map((entry) => ({
      ...entry,
      candidates: Array.isArray(entry.candidates) ? [...entry.candidates] : undefined,
      gateState: entry.gateState ? { ...entry.gateState } : undefined,
    }));
  }

  private normalizeLimit(value: unknown, fallback: number, min: number, max: number): number {
    const parsed = Number(value);
    if (!Number.isFinite(parsed)) {
      return fallback;
    }
    return Math.max(min, Math.min(max, Math.floor(parsed)));
  }
}
