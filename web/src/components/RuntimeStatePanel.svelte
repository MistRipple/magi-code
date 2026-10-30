<script lang="ts">
  import Disclosure from './Disclosure.svelte';
  import {
    mapTaskSemanticStatusToDisplayStatus,
    resolveTaskSemanticStatus,
  } from '../shared/task-status-semantics';
  import type {
    OrchestrationRuntimeTimelineEntry,
    OrchestratorRuntimeState,
  } from '../types/message';
  import Icon from './Icon.svelte';
  import type { IconName } from '../lib/icons';
  import { i18n } from '../stores/i18n.svelte';
  import {
    resolveRuntimeTaskProgress,
    resolveRuntimePanelStatus,
    runtimeAssignmentIsActive,
    shouldShowRuntimeBudget,
    shouldShowRuntimePanel,
    shouldShowRuntimePhase,
  } from '../lib/runtime-state-panel';
  import { mergeCurrentRuntimeTimelineEntries } from '../lib/runtime-timeline';
  import { resolveToolDisplayName } from '../lib/tool-display-name';
  import type { TurnStage } from '../shared/protocol/processing-state';

  interface Props {
    runtimeState: OrchestratorRuntimeState | null;
    conversationRecords?: OrchestrationRuntimeTimelineEntry[];
    conversationStartedAt?: number | null;
    isProcessing?: boolean;
    processingStartedAt?: number | null;
    turnStage?: TurnStage | null;
  }

  let {
    runtimeState,
    conversationRecords = [],
    conversationStartedAt = null,
    isProcessing = false,
    processingStartedAt = null,
    turnStage = null,
  }: Props = $props();
  let isPanelExpanded = $state(false);
  let expandedRecordIds = $state<Set<string>>(new Set());
  let panelRef: HTMLElement | undefined = $state();

  function closePanel(): void {
    isPanelExpanded = false;
    expandedRecordIds = new Set();
  }

  // 展开后按 popover 行为闭合：点击面板外部或按 ESC 即收起。
  $effect(() => {
    if (!isPanelExpanded) {
      return;
    }
    function handleOutsideMouseDown(event: MouseEvent): void {
      const target = event.target as Node | null;
      if (!target) {
        return;
      }
      if (panelRef && panelRef.contains(target)) {
        return;
      }
      closePanel();
    }
    function handleKeydown(event: KeyboardEvent): void {
      if (event.key === 'Escape') {
        closePanel();
      }
    }
    document.addEventListener('mousedown', handleOutsideMouseDown, true);
    document.addEventListener('keydown', handleKeydown);
    return () => {
      document.removeEventListener('mousedown', handleOutsideMouseDown, true);
      document.removeEventListener('keydown', handleKeydown);
    };
  });
  const opsView = $derived.by(() => runtimeState?.opsView || null);
  const executionGroupSummary = $derived.by(() => opsView?.executionGroup || null);
  const planSummary = $derived.by(() => opsView?.plan || null);

  const recentTimeline = $derived.by(() => (
    mergeCurrentRuntimeTimelineEntries({
      runtimeEntries: Array.isArray(opsView?.recentTimeline) ? opsView.recentTimeline : [],
      conversationEntries: Array.isArray(conversationRecords) ? conversationRecords : [],
      isProcessing,
      processingStartedAt,
      currentTurnStartedAt: conversationStartedAt,
    }).filter((item) => Boolean(formatTimelineSummary(item)))
  ));

  $effect(() => {
    const visibleRecordIds = new Set(recentTimeline.map((item) => item.eventId));
    const retainedRecordIds = new Set(
      [...expandedRecordIds].filter((eventId) => visibleRecordIds.has(eventId)),
    );
    if (retainedRecordIds.size !== expandedRecordIds.size) {
      expandedRecordIds = retainedRecordIds;
    }
  });
  const assignmentSummaries = $derived.by(() => Array.isArray(runtimeState?.assignments) ? runtimeState.assignments : []);
  const activeWorkerSummary = $derived.by(() => {
    const names = assignmentSummaries
      .map((item) => item.workerId)
      .filter((item): item is string => typeof item === 'string' && item.trim().length > 0)
      .filter((item) => !isGeneratedRuntimeIdentifier(item))
      .map((item) => formatWorkerName(item))
      .filter((item, index, arr) => item && arr.indexOf(item) === index);
    return names.slice(0, 4).join('、');
  });
  const summaryEntries = $derived.by(() => {
    if (!runtimeState) {
      return [] as Array<{ label: string; value: string }>;
    }
    const entries: Array<{ label: string; value: string }> = [];
    if (executionGroupSummary?.title) {
      entries.unshift({
        label: i18n.t('runtimeState.summary.executionGroup'),
        value: executionGroupSummary.title,
      });
    }
    if (planSummary?.planId) {
      entries.push({
        label: i18n.t('runtimeState.summary.plan'),
        value: formatPlanSummaryLabel(planSummary.status, planSummary.version),
      });
    }
    if (activeWorkerSummary) {
      entries.push({
        label: i18n.t('runtimeState.summary.activeWorkers'),
        value: activeWorkerSummary,
      });
    }
    if (runtimeState.startedAt) {
      entries.push({ label: i18n.t('runtimeState.summary.startedAt'), value: formatDateTime(runtimeState.startedAt) });
    }
    const statusReason = sanitizeRuntimeDisplayText(runtimeState.statusReason);
    if (statusReason) {
      entries.push({ label: i18n.t('runtimeState.summary.reason'), value: statusReason });
    }
    return entries;
  });

  const recoveryEntries = $derived.by(() => {
    const recovery = opsView?.recovery;
    if (!recovery) {
      return [] as Array<{ label: string; value: string }>;
    }
    const entries: Array<{ label: string; value: string }> = [];
    const continuationPolicy = formatContinuationPolicy(recovery.continuationPolicy);
    if (continuationPolicy) {
      entries.push({ label: i18n.t('runtimeDiagnostics.recovery.continuationPolicy'), value: continuationPolicy });
    }
    const continuationReason = sanitizeRuntimeDisplayText(recovery.continuationReason);
    if (continuationReason) {
      entries.push({ label: i18n.t('runtimeDiagnostics.recovery.continuationReason'), value: continuationReason });
    }
    const waitState = formatRecoveryState(recovery.waitState);
    if (waitState) {
      entries.push({ label: i18n.t('runtimeDiagnostics.recovery.waitState'), value: waitState });
    }
    const replanState = formatRecoveryState(recovery.replanState);
    if (replanState) {
      entries.push({ label: i18n.t('runtimeDiagnostics.recovery.replanState'), value: replanState });
    }
    const terminationReason = sanitizeRuntimeDisplayText(recovery.terminationReason);
    if (terminationReason) {
      entries.push({ label: i18n.t('runtimeDiagnostics.recovery.terminationReason'), value: terminationReason });
    }
    if (recovery.acceptanceSummary) {
      const acceptanceSummary = sanitizeRuntimeDisplayText(String(recovery.acceptanceSummary));
      if (acceptanceSummary) {
        entries.push({ label: i18n.t('runtimeDiagnostics.recovery.acceptanceSummary'), value: acceptanceSummary });
      }
    }
    const reviewState = formatRecoveryState(recovery.reviewState);
    if (reviewState) {
      entries.push({ label: i18n.t('runtimeDiagnostics.recovery.reviewState'), value: reviewState });
    }
    if (recovery.latestRecoveryAt) {
      entries.push({
        label: i18n.t('runtimeDiagnostics.recovery.latestRecoveryAt'),
        value: formatDateTime(recovery.latestRecoveryAt),
      });
    }
    if (recovery.snapshotStorage) {
      entries.push({
        label: i18n.t('runtimeDiagnostics.recovery.snapshotStorage'),
        value: formatSnapshotStorageLabel(recovery.snapshotStorage),
      });
    }
    if (typeof recovery.snapshotDirtyFileCount === 'number' && recovery.snapshotDirtyFileCount > 0) {
      entries.push({
        label: i18n.t('runtimeDiagnostics.recovery.snapshotDirtyFileCount'),
        value: String(recovery.snapshotDirtyFileCount),
      });
    }
    if (typeof recovery.snapshotPendingChangeCount === 'number' && recovery.snapshotPendingChangeCount > 0) {
      entries.push({
        label: i18n.t('runtimeDiagnostics.recovery.snapshotPendingChangeCount'),
        value: String(recovery.snapshotPendingChangeCount),
      });
    }
    if (typeof recovery.restoredWorkerBranchCount === 'number' && recovery.restoredWorkerBranchCount > 0) {
      entries.push({
        label: i18n.t('runtimeDiagnostics.recovery.restoredWorkerBranchCount'),
        value: String(recovery.restoredWorkerBranchCount),
      });
    }
    if (typeof recovery.restoredWorkerSessionCount === 'number' && recovery.restoredWorkerSessionCount > 0) {
      entries.push({
        label: i18n.t('runtimeDiagnostics.recovery.restoredWorkerSessionCount'),
        value: String(recovery.restoredWorkerSessionCount),
      });
    }
    if (typeof recovery.pendingTaskCount === 'number' && recovery.pendingTaskCount > 0) {
      entries.push({ label: i18n.t('runtimeDiagnostics.recovery.pendingTaskCount'), value: String(recovery.pendingTaskCount) });
    }
    if (typeof recovery.runningTaskCount === 'number' && recovery.runningTaskCount > 0) {
      entries.push({ label: i18n.t('runtimeDiagnostics.recovery.runningTaskCount'), value: String(recovery.runningTaskCount) });
    }
    if (typeof recovery.completedTaskCount === 'number' && recovery.completedTaskCount > 0) {
      entries.push({ label: i18n.t('runtimeDiagnostics.recovery.completedTaskCount'), value: String(recovery.completedTaskCount) });
    }
    if (typeof recovery.cancelledTaskCount === 'number' && recovery.cancelledTaskCount > 0) {
      entries.push({ label: i18n.t('runtimeDiagnostics.recovery.cancelledTaskCount'), value: String(recovery.cancelledTaskCount) });
    }
    return entries;
  });

  const canonicalProcessingActive = $derived.by(() => {
    return isProcessing && turnStage !== null && turnStage !== 'done';
  });

  const effectiveStatus = $derived(resolveRuntimePanelStatus({
    status: runtimeState?.status,
    isProcessing,
  }));
  const effectivePhase = $derived.by(() => (
    canonicalProcessingActive
      ? turnStage || 'running'
      : runtimeState?.phase
  ));
  const effectiveLastEventAt = $derived.by(() => (
    canonicalProcessingActive
      ? (processingStartedAt || runtimeState?.lastEventAt || Date.now())
      : runtimeState?.lastEventAt
  ));
  const summaryTimeLabel = $derived.by(() => (
    typeof effectiveLastEventAt === 'number' && Number.isFinite(effectiveLastEventAt) && effectiveLastEventAt > 0
      ? `${i18n.t('runtimeState.summary.lastEventShort')} ${formatTimestamp(effectiveLastEventAt)}`
      : ''
  ));

  // 状态图标
  const statusIcon = $derived.by((): IconName => {
    switch (effectiveStatus) {
      case 'idle': return 'circle';
      case 'running': return 'loader';
      case 'waiting': return 'clock';
      case 'paused': return 'taskPending';
      case 'blocked': return 'taskPending';
      case 'completed': return 'taskComplete';
      case 'failed': return 'taskFailed';
      case 'cancelled': return 'stop';
      default: return 'loader';
    }
  });

  // 状态翻译文本
  const statusLabel = $derived.by(() => {
    switch (effectiveStatus) {
      case 'idle': return i18n.t('runtimeState.status.idle');
      case 'running': return i18n.t('runtimeState.status.running');
      case 'waiting': return i18n.t('runtimeState.status.waiting');
      case 'paused': return i18n.t('runtimeState.status.paused');
      case 'blocked': return i18n.t('runtimeState.status.blocked');
      case 'completed': return i18n.t('runtimeState.status.completed');
      case 'failed': return i18n.t('runtimeState.status.failed');
      case 'cancelled': return i18n.t('runtimeState.status.cancelled');
      default: return i18n.t('runtimeState.status.idle');
    }
  });

  // 状态对应的 CSS modifier
  const statusModifier = $derived.by(() => {
    switch (effectiveStatus) {
      case 'idle': return 'idle';
      case 'running': return 'running';
      case 'waiting': return 'waiting';
      case 'paused': return 'paused';
      case 'blocked': return 'blocked';
      case 'completed': return 'completed';
      case 'failed': return 'failed';
      case 'cancelled': return 'cancelled';
      default: return 'idle';
    }
  });

  const taskProgress = $derived(resolveRuntimeTaskProgress(runtimeState?.runtimeSnapshot));
  const visibleMetrics = $derived.by(() => {
    const snapshot = runtimeState?.runtimeSnapshot;
    if (!snapshot) return false;
    return Boolean(
      (taskProgress && taskProgress.total > 0)
      || ((snapshot.reviewState?.total ?? 0) > 0)
      || ((snapshot.blockerState?.open ?? 0) > 0)
      || ((snapshot.blockerState?.externalWaitOpen ?? 0) > 0)
      || shouldShowRuntimeBudget(snapshot.budgetState?.warningLevel),
    );
  });
  const panelVisible = $derived(shouldShowRuntimePanel({
    status: effectiveStatus,
    isProcessing: canonicalProcessingActive,
    activeAssignmentCount: assignmentSummaries.filter((item) => (
      runtimeAssignmentIsActive(item.status)
    )).length,
  }));
  const phaseVisible = $derived(shouldShowRuntimePhase(effectiveStatus, effectivePhase));

  $effect(() => {
    if (!panelVisible && isPanelExpanded) {
      closePanel();
    }
  });

  function formatTimestamp(timestamp: number): string {
    if (!Number.isFinite(timestamp)) return '--';
    return new Date(timestamp).toLocaleTimeString(i18n.locale, { hour12: false });
  }

  function formatDateTime(timestamp: number): string {
    if (!Number.isFinite(timestamp)) return '--';
    return new Date(timestamp).toLocaleString(i18n.locale);
  }

  function formatRuntimePhase(phase: string | undefined): string {
    const normalized = typeof phase === 'string' ? phase.trim() : '';
    if (!normalized) return '--';
    switch (normalized) {
      case 'clarify': return i18n.t('runtimeDiagnostics.phase.clarify');
      case 'design': return i18n.t('runtimeDiagnostics.phase.design');
      case 'architecture': return i18n.t('runtimeDiagnostics.phase.architecture');
      case 'frontend_implement': return i18n.t('runtimeDiagnostics.phase.frontendImplement');
      case 'backend_implement': return i18n.t('runtimeDiagnostics.phase.backendImplement');
      case 'integration': return i18n.t('runtimeDiagnostics.phase.integration');
      case 'verify': return i18n.t('runtimeDiagnostics.phase.verify');
      case 'review': return i18n.t('runtimeDiagnostics.phase.review');
      case 'document': return i18n.t('runtimeDiagnostics.phase.document');
      case 'deploy': return i18n.t('runtimeDiagnostics.phase.deploy');
      case 'summarize': return i18n.t('runtimeDiagnostics.phase.summarize');
      case 'analysis': return i18n.t('runtimeDiagnostics.phase.analysis');
      case 'planning': return i18n.t('runtimeDiagnostics.phase.planning');
      case 'running': return i18n.t('runtimeDiagnostics.phase.running');
      case 'pending': return i18n.t('runtimeDiagnostics.phase.pending');
      case 'preparing': return i18n.t('runtimeDiagnostics.phase.preparing');
      case 'streaming': return i18n.t('runtimeDiagnostics.phase.streaming');
      case 'finalizing': return i18n.t('runtimeDiagnostics.phase.finalizing');
      case 'done': return i18n.t('runtimeDiagnostics.phase.done');
      case 'waiting': return i18n.t('runtimeDiagnostics.phase.waiting');
      case 'blocked': return i18n.t('runtimeState.status.blocked');
      case 'reviewing': return i18n.t('runtimeDiagnostics.phase.reviewing');
      case 'summary': return i18n.t('runtimeDiagnostics.phase.summary');
      case 'idle': return i18n.t('runtimeState.status.idle');
      case 'tool': return i18n.t('runtimeDiagnostics.phase.tool');
      case 'handoff': return i18n.t('runtimeDiagnostics.phase.handoff');
      case 'finalize': return i18n.t('runtimeDiagnostics.phase.finalize');
      case 'no_tool': return i18n.t('runtimeDiagnostics.phase.noTool');
      default: return normalized;
    }
  }

  function formatPlanStatus(status: string | undefined): string {
    const normalized = typeof status === 'string' ? status.trim() : '';
    if (!normalized) return '--';
    const camelStatus = normalized.replace(/_([a-z])/g, (_, c) => c.toUpperCase());
    const key = `tasks.planStatus.${camelStatus}`;
    const label = i18n.t(key);
    return label !== key ? label : normalized;
  }

  function formatPlanSummaryLabel(status?: string, version?: number): string {
    const statusLabel = formatPlanStatus(status);
    const versionLabel = typeof version === 'number' && Number.isFinite(version)
      ? i18n.t('runtimeState.summary.planVersion', { version })
      : '';
    return [statusLabel, versionLabel].filter(Boolean).join(', ') || '--';
  }

  function formatSnapshotStorageLabel(storage: string | undefined): string {
    switch (storage) {
      case 'ghost_commit':
        return i18n.t('runtimeDiagnostics.recovery.storage.ghostCommit');
      case 'head_commit':
        return i18n.t('runtimeDiagnostics.recovery.storage.headCommit');
      default:
        return '';
    }
  }

  function formatContinuationPolicy(policy: string | undefined): string {
    switch (policy) {
      case 'resumable':
        return i18n.t('runtimeDiagnostics.recovery.continuation.resumable');
      case 'none':
        return i18n.t('runtimeDiagnostics.recovery.continuation.none');
      default:
        return formatHumanizedRuntimeText(policy);
    }
  }

  function formatRecoveryState(state: string | undefined): string {
    const normalized = typeof state === 'string' ? state.trim() : '';
    if (!normalized) return '';
    const assignmentStatus = formatAssignmentStatus(normalized);
    if (assignmentStatus && assignmentStatus !== normalized) {
      return assignmentStatus;
    }
    switch (normalized) {
      case 'ready':
        return i18n.t('runtimeDiagnostics.recovery.state.ready');
      case 'consumed':
      case 'worker_resumed':
        return i18n.t('runtimeDiagnostics.recovery.state.consumed');
      default:
        return formatHumanizedRuntimeText(normalized);
    }
  }

  function formatWorkerName(workerId: string | undefined): string {
    const normalized = typeof workerId === 'string' ? workerId.trim() : '';
    if (!normalized) return '--';
    const parts = normalized.split('-').filter(Boolean);
    if (parts.length === 0) return normalized;
    return parts.map((part) => part.charAt(0).toUpperCase() + part.slice(1)).join(' ');
  }

  function isGeneratedRuntimeIdentifier(value: string | undefined): boolean {
    const normalized = typeof value === 'string' ? value.trim().toLowerCase() : '';
    if (!normalized) return false;
    return /\b(task|session|worker|mission|chain|recovery|assignment|request|batch|execution[_-]?group|snapshot|tool[_-]?call)[-_][a-z0-9_-]*\d{4,}/.test(normalized)
      || /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(normalized)
      || /^[a-f0-9]{12,}$/.test(normalized)
      || /\d{10,}/.test(normalized)
      || normalized.startsWith('task_failed:');
  }

  function formatAssignmentMeta(item: { assignmentId?: string; workerId?: string; status: string }): string {
    return formatAssignmentStatus(item.status);
  }

  function formatAssignmentStatus(status: string | undefined): string {
    const displayStatus = mapTaskSemanticStatusToDisplayStatus(resolveTaskSemanticStatus({ status }));
    switch (displayStatus) {
      case 'running':
        return i18n.t('runtimeState.status.running');
      case 'completed':
        return i18n.t('runtimeState.status.completed');
      case 'failed':
        return i18n.t('runtimeState.status.failed');
      case 'cancelled':
        return i18n.t('runtimeState.status.cancelled');
      case 'awaiting_approval':
        return i18n.t('runtimeDiagnostics.assignmentStatus.awaitingApproval');
      case 'review_required':
        return i18n.t('runtimeDiagnostics.assignmentStatus.reviewRequired');
      case 'blocked':
        return i18n.t('runtimeDiagnostics.assignmentStatus.blocked');
      case 'pending':
        return i18n.t('runtimeDiagnostics.assignmentStatus.pending');
      default:
        return (typeof status === 'string' ? status.trim() : '') || '--';
    }
  }

  function formatDuration(ms: number | undefined): string {
    if (!ms || !Number.isFinite(ms)) return '--';
    if (ms < 1000) return `${ms}ms`;
    const s = Math.round(ms / 1000);
    if (s < 60) return `${s}s`;
    const m = Math.floor(s / 60);
    return `${m}m${s % 60}s`;
  }

  function formatTokens(n: number | undefined): string {
    if (n == null || !Number.isFinite(n)) return '--';
    if (n < 1000) return `${n}`;
    return `${(n / 1000).toFixed(1)}k`;
  }

  function formatUsageRatio(ratio: number | undefined): string {
    if (ratio == null || !Number.isFinite(ratio)) return '--';
    return `${Math.round(ratio * 100)}%`;
  }

  function resolveBudgetTone(level: string | undefined): 'normal' | 'notice' | 'warning' | 'danger' {
    switch (level) {
      case 'notice':
      case 'warning':
      case 'danger':
        return level;
      default:
        return 'normal';
    }
  }

  function resolveBudgetToneLabel(level: string | undefined): string {
    switch (resolveBudgetTone(level)) {
      case 'notice': return i18n.t('runtimeDiagnostics.budgetLevel.notice');
      case 'warning': return i18n.t('runtimeDiagnostics.budgetLevel.warning');
      case 'danger': return i18n.t('runtimeDiagnostics.budgetLevel.danger');
      default: return i18n.t('runtimeDiagnostics.budgetLevel.normal');
    }
  }

  function resolveBudgetFillClass(level: string | undefined): string {
    switch (resolveBudgetTone(level)) {
      case 'notice': return 'progress-bar__fill--notice';
      case 'warning': return 'progress-bar__fill--warning';
      case 'danger': return 'progress-bar__fill--danger';
      default: return '';
    }
  }

  function formatAssignmentRuntimeSummary(item: {
    completedTaskCount: number;
    taskTotal: number;
    runningTaskCount: number;
    failedTaskCount: number;
    blockedTaskCount?: number;
    awaitingApprovalTaskCount?: number;
    reviewRequiredTaskCount?: number;
  }): string {
    const completedTaskCount = item.completedTaskCount;
    const totalTaskCount = item.taskTotal;
    const runningTaskCount = item.runningTaskCount;
    const failedTaskCount = item.failedTaskCount;
    const blockedTaskCount = item.blockedTaskCount || 0;
    const awaitingApprovalTaskCount = item.awaitingApprovalTaskCount || 0;
    const reviewRequiredTaskCount = item.reviewRequiredTaskCount || 0;
    const summary: string[] = [
      i18n.t('runtimeDiagnostics.taskStats', {
        completed: completedTaskCount,
        total: totalTaskCount,
        running: runningTaskCount,
        failed: failedTaskCount,
      }),
    ];
    if (blockedTaskCount > 0) {
      summary.push(`${i18n.t('runtimeDiagnostics.assignmentStatus.blocked')} ${blockedTaskCount}`);
    }
    if (awaitingApprovalTaskCount > 0) {
      summary.push(`${i18n.t('runtimeDiagnostics.assignmentStatus.awaitingApproval')} ${awaitingApprovalTaskCount}`);
    }
    if (reviewRequiredTaskCount > 0) {
      summary.push(`${i18n.t('runtimeDiagnostics.assignmentStatus.reviewRequired')} ${reviewRequiredTaskCount}`);
    }
    return summary.join(', ');
  }

  function formatTimelineTypeLabel(type: string): string {
    const normalized = typeof type === 'string' ? type.trim().toLowerCase() : '';
    if (!normalized) return '--';
    switch (normalized) {
      case 'task.dispatched':
        return i18n.t('runtimeDiagnostics.event.taskDispatched');
      case 'task.running':
        return i18n.t('runtimeDiagnostics.event.taskRunning');
      case 'task.completed':
        return i18n.t('runtimeDiagnostics.event.taskCompleted');
      case 'task.failed':
        return i18n.t('runtimeDiagnostics.event.taskFailed');
      case 'task.status.changed':
        return i18n.t('runtimeDiagnostics.event.taskChanged');
      case 'mission.execution.overview':
        return i18n.t('runtimeDiagnostics.event.overview');
      case 'knowledge.context.selected':
        return i18n.t('runtimeDiagnostics.event.context');
      case 'knowledge.learning.extraction':
        return i18n.t('runtimeDiagnostics.event.learning');
      case 'mission.resume.dispatch.created':
        return i18n.t('runtimeDiagnostics.event.resume');
      case 'worker.reported':
        return i18n.t('runtimeDiagnostics.event.worker');
      case 'worker.tool.observed':
      case 'task.tool.invoked':
      case 'session.turn.tool.invoked':
      case 'tool.call.finished':
      case 'tool.invoked':
        return i18n.t('runtimeDiagnostics.event.tool');
      case 'session.turn.failed':
        return i18n.t('runtimeDiagnostics.event.turnFailed');
      case 'session.turn.interrupted':
        return i18n.t('runtimeDiagnostics.event.interrupted');
      case 'session.turn.queue_failed':
        return i18n.t('runtimeDiagnostics.event.queueFailed');
      case 'model.retry.runtime':
        return i18n.t('runtimeDiagnostics.event.retry');
      case 'worker.skill_dispatch.observed':
      case 'worker.skill_dispatch.applied':
        return i18n.t('runtimeDiagnostics.event.skill');
      case 'worker.executor.observed':
        return i18n.t('runtimeDiagnostics.event.executor');
      case 'governance.decision.applied':
        return i18n.t('runtimeDiagnostics.event.decision');
      case 'system.runtime.maintenance.status':
        return i18n.t('runtimeDiagnostics.event.maintenance');
      default:
        return normalized
          .split('.')
          .map((part) => formatRuntimeTokenLabel(part))
          .filter(Boolean)
          .join(', ') || i18n.t('runtimeDiagnostics.event.default');
    }
  }

  function formatTimelineSummary(
    item: Pick<OrchestrationRuntimeTimelineEntry, 'type' | 'summary' | 'source' | 'detail'>,
  ): string {
    const toolName = item.source ? resolveToolDisplayName(item.source, i18n) : '';
    switch (item.type) {
      case 'session.turn.processing':
        return i18n.t('runtimeDiagnostics.record.processing');
      case 'session.tool.running':
        return appendRuntimeRecordSummary(
          i18n.t('runtimeDiagnostics.record.toolRunning', { tool: toolName }),
          item.summary === item.source ? '' : sanitizeRuntimeDisplayText(item.summary),
        );
      case 'session.tool.succeeded':
        return appendRuntimeRecordSummary(
          i18n.t('runtimeDiagnostics.record.toolSucceeded', { tool: toolName }),
          item.summary === item.source ? '' : sanitizeRuntimeDisplayText(item.summary),
        );
      case 'session.tool.failed': {
        const reason = formatRuntimeRecordPreview(item.detail)
          || (item.summary === item.source ? '' : sanitizeRuntimeDisplayText(item.summary));
        return reason
          ? i18n.t('runtimeDiagnostics.record.toolFailedWithReason', { tool: toolName, reason })
          : i18n.t('runtimeDiagnostics.record.toolFailed', { tool: toolName });
      }
      case 'session.model.failed':
        return i18n.t('runtimeDiagnostics.record.modelFailed', {
          reason: formatRuntimeRecordPreview(item.detail) || sanitizeRuntimeDisplayText(item.summary),
        });
      case 'session.turn.interrupted':
        return i18n.t('runtimeDiagnostics.record.interrupted');
      default:
        break;
    }
    const typeLabel = formatTimelineTypeLabel(item.type);
    const cleanedSummary = formatHumanizedRuntimeText(item.summary);
    if (!cleanedSummary || cleanedSummary === typeLabel) {
      return typeLabel;
    }
    const normalizedType = typeof item.type === 'string' ? item.type.trim() : '';
    if (normalizedType && cleanedSummary.toLowerCase().startsWith(normalizedType.toLowerCase())) {
      const rest = formatHumanizedRuntimeText(cleanedSummary.slice(normalizedType.length));
      return rest ? `${typeLabel}：${rest}` : typeLabel;
    }
    return cleanedSummary;
  }

  function appendRuntimeRecordSummary(label: string, summary: string): string {
    return summary ? `${label}: ${summary}` : label;
  }

  function formatRuntimeRecordPreview(value: unknown): string {
    const preview = typeof value === 'string'
      ? value.replace(/\s+/g, ' ').trim()
      : '';
    if (!preview) return '';
    return preview.length > 220 ? `${preview.slice(0, 220).trimEnd()}…` : preview;
  }

  function hasRuntimeRecordDetail(item: OrchestrationRuntimeTimelineEntry): boolean {
    return typeof item.detail === 'string' && item.detail.trim().length > 0;
  }

  function isRuntimeRecordExpanded(eventId: string): boolean {
    return expandedRecordIds.has(eventId);
  }

  function toggleRuntimeRecord(eventId: string): void {
    const next = new Set(expandedRecordIds);
    if (next.has(eventId)) {
      next.delete(eventId);
    } else {
      next.add(eventId);
    }
    expandedRecordIds = next;
  }

  function resolveRuntimeRecordKind(
    item: Pick<OrchestrationRuntimeTimelineEntry, 'kind'>,
  ): NonNullable<OrchestrationRuntimeTimelineEntry['kind']> {
    return item.kind || 'progress';
  }

  function formatRuntimeRecordKind(
    item: Pick<OrchestrationRuntimeTimelineEntry, 'type' | 'kind'>,
  ): string {
    switch (resolveRuntimeRecordKind(item)) {
      case 'success': return i18n.t('runtimeDiagnostics.recordKind.success');
      case 'warning': return i18n.t('runtimeDiagnostics.recordKind.warning');
      case 'error': return i18n.t('runtimeDiagnostics.recordKind.error');
      default: return i18n.t('runtimeDiagnostics.recordKind.progress');
    }
  }

  function formatRuntimeTokenLabel(token: string): string {
    switch (token) {
      case 'task': return i18n.t('runtimeDiagnostics.token.task');
      case 'mission': return i18n.t('runtimeDiagnostics.token.mission');
      case 'worker': return i18n.t('runtimeDiagnostics.token.worker');
      case 'tool': return i18n.t('runtimeDiagnostics.token.tool');
      case 'governance': return i18n.t('runtimeDiagnostics.token.decision');
      case 'decision': return i18n.t('runtimeDiagnostics.token.decision');
      case 'system': return i18n.t('runtimeDiagnostics.token.system');
      case 'runtime': return i18n.t('runtimeDiagnostics.token.runtime');
      case 'execution': return i18n.t('runtimeDiagnostics.token.execution');
      case 'overview': return i18n.t('runtimeDiagnostics.token.overview');
      case 'status': return i18n.t('runtimeDiagnostics.token.status');
      case 'changed': return i18n.t('runtimeDiagnostics.token.changed');
      case 'dispatched': return i18n.t('runtimeDiagnostics.token.dispatched');
      case 'reported': return i18n.t('runtimeDiagnostics.token.reported');
      case 'observed': return i18n.t('runtimeDiagnostics.token.observed');
      case 'applied': return i18n.t('runtimeDiagnostics.token.applied');
      case 'resume': return i18n.t('runtimeDiagnostics.token.resume');
      case 'dispatch': return i18n.t('runtimeDiagnostics.token.dispatch');
      case 'created': return i18n.t('runtimeDiagnostics.token.created');
      default:
        return token
          .replace(/[_-]/g, ' ')
          .replace(/\b\w/g, (char) => char.toUpperCase());
    }
  }

  function formatHumanizedRuntimeText(value: unknown): string {
    const raw = typeof value === 'string' ? value.trim() : '';
    if (!raw || isGeneratedRuntimeIdentifier(raw)) {
      return '';
    }
    const withoutIdentifiers = stripRuntimeIdentifiers(raw)
      .replace(/\s+/g, ' ')
      .replace(/^[\s:：,，;；·-]+|[\s:：,，;；·-]+$/g, '')
      .trim();
    if (!withoutIdentifiers || isGeneratedRuntimeIdentifier(withoutIdentifiers)) {
      return '';
    }
    return withoutIdentifiers
      .replace(/[_-]/g, ' ')
      .replace(/\b[a-z]/g, (char) => char.toUpperCase());
  }

  function sanitizeRuntimeDisplayText(value: unknown): string {
    const raw = typeof value === 'string' ? value.trim() : '';
    if (!raw || isGeneratedRuntimeIdentifier(raw)) {
      return '';
    }
    const sanitized = stripRuntimeIdentifiers(raw)
      .replace(/\s+/g, ' ')
      .replace(/^[\s:：,，;；·-]+|[\s:：,，;；·-]+$/g, '')
      .trim();
    return isGeneratedRuntimeIdentifier(sanitized) ? '' : sanitized;
  }

  function stripRuntimeIdentifiers(value: string): string {
    return value
      .replace(/\b(task|session|worker|mission|chain|recovery|assignment|request|batch|execution[_-]?group|snapshot|tool[_-]?call)[-_:][a-z0-9_-]*\d{4,}\b/gi, '')
      .replace(/\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b/gi, '')
      .replace(/\b[a-f0-9]{12,}\b/gi, '')
      .replace(/\b\d{10,}\b/g, '');
  }

  function togglePanel(): void {
    if (isPanelExpanded) {
      closePanel();
      return;
    }
    expandedRecordIds = new Set();
    isPanelExpanded = true;
  }
</script>

<div class="runtime-diagnostics-slot" class:runtime-diagnostics-slot--visible={panelVisible}>
  {#if panelVisible}
  <section
    bind:this={panelRef}
    class="runtime-diagnostics"
    class:runtime-diagnostics--expanded={isPanelExpanded}
  >
    <button
      type="button"
      class="runtime-diagnostics__summary-button"
      class:runtime-diagnostics__summary-button--expanded={isPanelExpanded}
      aria-expanded={isPanelExpanded}
      onclick={togglePanel}
    >
      <Icon name={isPanelExpanded ? 'chevron-down' : 'chevron-right'} size={13} class="summary__chevron" />
      <Icon name={statusIcon} size={13} class="summary__icon" />
      <span class="summary__title">{i18n.t('runtimeState.title')}</span>
      <span class="summary__badge summary__badge--{statusModifier}">{statusLabel}</span>
      {#if phaseVisible}
        <span class="summary__phase">{formatRuntimePhase(effectivePhase)}</span>
      {/if}
      {#if taskProgress && taskProgress.total > 0}
        <span class="summary__meta">{taskProgress.completed}/{taskProgress.total}</span>
      {/if}
      {#if assignmentSummaries.length > 0}
        <span class="summary__meta">{i18n.t('runtimeState.summary.agentCount', { count: assignmentSummaries.length })}</span>
      {/if}
      {#if summaryTimeLabel}
        <span class="summary__time">{summaryTimeLabel}</span>
      {/if}
    </button>
    {#if isPanelExpanded}
    <div class="runtime-diagnostics__content" data-magi-surface="popover">
      {#if summaryEntries.length > 0}
        <div class="runtime-diagnostics__block">
          <div class="runtime-diagnostics__label">{i18n.t('runtimeState.summary.title')}</div>
          <div class="runtime-diagnostics__kv-grid">
            {#each summaryEntries as item}
              <div class="runtime-diagnostics__kv-item">
                <div class="runtime-diagnostics__kv-label">{item.label}</div>
                <div class="runtime-diagnostics__kv-value">{item.value}</div>
              </div>
            {/each}
          </div>
        </div>
      {/if}

      {#if runtimeState?.runtimeSnapshot && visibleMetrics}
        {@const snap = runtimeState.runtimeSnapshot}
        <div class="metrics-grid">
          {#if taskProgress && taskProgress.total > 0}
            <div class="metric-card">
              <div class="metric-card__header">
                <Icon name="list" size={12} class="metric-card__icon" />
                <span class="metric-card__title">{i18n.t('runtimeDiagnostics.taskProgress')}</span>
              </div>
              <div class="metric-card__value">{taskProgress.completed}/{taskProgress.total}</div>
              <div class="progress-bar">
                <div class="progress-bar__fill" style="width: {taskProgress.percent}%"></div>
              </div>
              {#if taskProgress.failed > 0}
                <div class="metric-card__sub metric-card__sub--warn">{i18n.t('runtimeDiagnostics.failedCount', { count: taskProgress.failed })}</div>
              {/if}
            </div>
          {/if}

          {#if snap.reviewState && (snap.reviewState.total ?? 0) > 0}
            <div class="metric-card">
              <div class="metric-card__header">
                <Icon name="check-circle" size={12} class="metric-card__icon" />
                <span class="metric-card__title">{i18n.t('runtimeDiagnostics.review')}</span>
              </div>
              <div class="metric-card__value">
                {snap.reviewState.accepted ?? 0}/{snap.reviewState.total ?? 0}
              </div>
              <div class="metric-card__sub"
                   class:metric-card__sub--ok={(snap.reviewState.accepted ?? 0) >= (snap.reviewState.total ?? 0) && (snap.reviewState.total ?? 0) > 0}>
                {#if (snap.reviewState.accepted ?? 0) >= (snap.reviewState.total ?? 0) && (snap.reviewState.total ?? 0) > 0}
                  {i18n.t('runtimeDiagnostics.allPassed')}
                {:else}
                  {i18n.t('runtimeDiagnostics.inProgress')}
                {/if}
              </div>
            </div>
          {/if}

          {#if snap.blockerState && ((snap.blockerState.open ?? 0) > 0 || (snap.blockerState.externalWaitOpen ?? 0) > 0)}
            <div class="metric-card">
              <div class="metric-card__header">
                <Icon name={(snap.blockerState.open ?? 0) > 0 ? 'alert-triangle' : 'check-circle'} size={12} class="metric-card__icon" />
                <span class="metric-card__title">{i18n.t('runtimeDiagnostics.blocker')}</span>
              </div>
              <div class="metric-card__value"
                   class:metric-card__value--warn={(snap.blockerState.open ?? 0) > 0}>
                {snap.blockerState.open ?? 0}
              </div>
              {#if (snap.blockerState.externalWaitOpen ?? 0) > 0}
                <div class="metric-card__sub metric-card__sub--warn">
                  {i18n.t('runtimeDiagnostics.externalWait', { count: snap.blockerState.externalWaitOpen ?? 0 })}
                </div>
              {:else}
                <div class="metric-card__sub metric-card__sub--ok">{i18n.t('runtimeDiagnostics.noBlocker')}</div>
              {/if}
            </div>
          {/if}

          {#if snap.budgetState && shouldShowRuntimeBudget(snap.budgetState.warningLevel)}
            <div class="metric-card">
              <div class="metric-card__header">
                <Icon name="clock" size={12} class="metric-card__icon" />
                <span class="metric-card__title">{i18n.t('runtimeDiagnostics.budget')}</span>
              </div>
              <div
                class="metric-card__value"
                class:metric-card__value--notice={resolveBudgetTone(snap.budgetState.warningLevel) === 'notice'}
                class:metric-card__value--warn={resolveBudgetTone(snap.budgetState.warningLevel) === 'warning' || resolveBudgetTone(snap.budgetState.warningLevel) === 'danger'}
              >
                {formatDuration(snap.budgetState.elapsedMs)}
              </div>
              {#if snap.budgetState.usageRatio != null}
                <div class="progress-bar">
                  <div
                    class={`progress-bar__fill ${resolveBudgetFillClass(snap.budgetState.warningLevel)}`}
                    style="width: {Math.max(0, Math.min(100, Math.round((snap.budgetState.usageRatio ?? 0) * 100)))}%"
                  ></div>
                </div>
              {/if}
              <div class="metric-card__sub ui-meta">
                <span>{i18n.t('runtimeDiagnostics.tokens', { value: formatTokens(snap.budgetState.tokenUsed) })}</span>
                {#if snap.budgetState.tokenLimit != null}
                  <span>{i18n.t('runtimeDiagnostics.tokenLimit', { value: formatTokens(snap.budgetState.tokenLimit) })}</span>
                {/if}
                {#if snap.budgetState.remainingTokens != null}
                  <span>{i18n.t('runtimeDiagnostics.remainingTokens', { value: formatTokens(snap.budgetState.remainingTokens) })}</span>
                {/if}
                {#if snap.budgetState.errorRate != null && snap.budgetState.errorRate > 0}
                  <span>{i18n.t('runtimeDiagnostics.errorRate', { rate: Math.round(snap.budgetState.errorRate * 100) })}</span>
                {/if}
              </div>
              {#if snap.budgetState.usageRatio != null}
                <div
                  class="metric-card__sub ui-meta"
                  class:metric-card__sub--notice={resolveBudgetTone(snap.budgetState.warningLevel) === 'notice'}
                  class:metric-card__sub--warn={resolveBudgetTone(snap.budgetState.warningLevel) === 'warning'}
                  class:metric-card__sub--danger={resolveBudgetTone(snap.budgetState.warningLevel) === 'danger'}
                >
                  <span>{resolveBudgetToneLabel(snap.budgetState.warningLevel)}</span>
                  <span>{i18n.t('runtimeDiagnostics.usageRatio', { value: formatUsageRatio(snap.budgetState.usageRatio) })}</span>
                </div>
              {/if}
            </div>
          {/if}

        </div>
      {/if}

      {#if assignmentSummaries.length > 0}
      <div class="runtime-diagnostics__block runtime-diagnostics__block--assignments">
        <div class="runtime-diagnostics__label">{i18n.t('runtimeDiagnostics.assignmentTitle')}</div>
        <div class="runtime-diagnostics__ops-list">
          {#each assignmentSummaries as item}
            <div class="runtime-diagnostics__ops-item">
              <div class="runtime-diagnostics__ops-title-row">
                <span class="runtime-diagnostics__ops-title">{item.title}</span>
                <span class="runtime-diagnostics__ops-time">{formatAssignmentMeta(item)}</span>
              </div>
              <div class="runtime-diagnostics__ops-sub">{formatAssignmentRuntimeSummary(item)}</div>
            </div>
          {/each}
        </div>
      </div>
      {/if}

      {#if recentTimeline.length > 0}
      <div class="runtime-diagnostics__block runtime-diagnostics__block--records">
        <div class="runtime-diagnostics__label">{i18n.t('runtimeDiagnostics.keyRecords')}</div>
        <div class="runtime-diagnostics__ops-list">
          {#each recentTimeline as item (item.eventId)}
            {@const hasDetail = hasRuntimeRecordDetail(item)}
            {@const recordExpanded = hasDetail && isRuntimeRecordExpanded(item.eventId)}
            <div
              class="runtime-diagnostics__ops-item runtime-diagnostics__ops-item--{resolveRuntimeRecordKind(item)}"
            >
              <button
                type="button"
                class="runtime-diagnostics__ops-title-row runtime-diagnostics__record-toggle"
                class:runtime-diagnostics__record-toggle--expandable={hasDetail}
                aria-expanded={hasDetail ? recordExpanded : undefined}
                disabled={!hasDetail}
                title={formatTimelineSummary(item)}
                onclick={() => toggleRuntimeRecord(item.eventId)}
              >
                <span class="runtime-diagnostics__record-chevron" class:runtime-diagnostics__record-chevron--hidden={!hasDetail}>
                  <Icon name={recordExpanded ? 'chevron-down' : 'chevron-right'} size={11} />
                </span>
                <span class="runtime-diagnostics__record-kind">
                  {formatRuntimeRecordKind(item)}
                </span>
                <span class="runtime-diagnostics__ops-title">{formatTimelineSummary(item)}</span>
                <span class="runtime-diagnostics__ops-time">{formatTimestamp(item.timestamp)}</span>
              </button>
              <Disclosure expanded={recordExpanded}>
                <pre class="runtime-diagnostics__record-detail">{item.detail}</pre>
              </Disclosure>
            </div>
          {/each}
        </div>
      </div>
      {/if}

      {#if recoveryEntries.length > 0}
        <div class="runtime-diagnostics__block">
          <div class="runtime-diagnostics__label">{i18n.t('runtimeDiagnostics.recoveryTitle')}</div>
          <div class="runtime-diagnostics__kv-grid">
            {#each recoveryEntries as item}
              <div class="runtime-diagnostics__kv-item">
                <div class="runtime-diagnostics__kv-label">{item.label}</div>
                <div class="runtime-diagnostics__kv-value">{item.value}</div>
              </div>
            {/each}
          </div>
        </div>
      {/if}

    </div>
    {/if}
  </section>
  {/if}
</div>

<style>
  .runtime-diagnostics-slot {
    container-type: inline-size;
    container-name: runtime-panel;
    position: absolute;
    inset: 0 0 auto;
    height: 0;
    min-height: 0;
    overflow: visible;
    pointer-events: none;
    z-index: 12;
  }

  /* 收起态卡片需要进入主布局，不能覆盖消息列表的首条消息。展开内容仍通过
     runtime-diagnostics__content 作为浮层显示，不额外推动消息列表。 */
  .runtime-diagnostics-slot--visible {
    position: relative;
    flex: 0 0 42px;
    height: 42px;
    padding-top: 6px;
    box-sizing: border-box;
  }

  .runtime-diagnostics {
    margin: 0 12px;
    border: var(--magi-hairline) solid var(--vscode-editorWidget-border, var(--border));
    border-radius: 8px;
    background: var(--vscode-editorWidget-background, var(--surface-2));
    color: var(--vscode-foreground, var(--foreground));
    overflow: visible;
    position: relative;
    z-index: 1;
    pointer-events: auto;
    height: 36px;
  }

  .runtime-diagnostics--expanded {
    border-bottom-left-radius: 0;
    border-bottom-right-radius: 0;
  }

  .runtime-diagnostics__summary-button {
    width: 100%;
    cursor: pointer;
    height: 34px;
    min-height: 34px;
    padding: 7px 10px;
    font-size: 12px;
    user-select: none;
    display: flex;
    align-items: center;
    gap: 6px;
    border: 0;
    background: transparent;
    color: inherit;
    text-align: left;
    border-radius: 8px;
    overflow: hidden;
  }

  .runtime-diagnostics__summary-button:hover {
    background: color-mix(in srgb, var(--vscode-editor-background, var(--assistant-message-bg)) 55%, transparent);
  }

  .runtime-diagnostics__summary-button--expanded {
    border-bottom-left-radius: 0;
    border-bottom-right-radius: 0;
  }

  :global(.summary__chevron) {
    opacity: 0.72;
    flex-shrink: 0;
  }

  :global(.summary__icon) {
    opacity: 0.9;
    flex-shrink: 0;
  }

  .summary__title {
    font-weight: var(--font-semibold);
    flex: 0 0 auto;
  }

  .summary__badge {
    font-size: var(--text-xs);
    font-weight: var(--font-medium);
    padding: 1px 6px;
    border-radius: 3px;
  }

  .summary__badge--completed {
    background: color-mix(in srgb, var(--success) 18%, transparent);
    color: var(--success);
  }
  .summary__badge--idle {
    background: color-mix(in srgb, var(--foreground-muted) 18%, transparent);
    color: var(--foreground-muted);
  }
  .summary__badge--running {
    background: color-mix(in srgb, var(--info) 18%, transparent);
    color: var(--info);
  }
  .summary__badge--waiting {
    background: color-mix(in srgb, var(--warning) 18%, transparent);
    color: var(--warning);
  }
  .summary__badge--failed {
    background: color-mix(in srgb, var(--error) 18%, transparent);
    color: var(--error);
  }
  .summary__badge--cancelled {
    background: color-mix(in srgb, var(--foreground-muted) 18%, transparent);
    color: var(--foreground-muted);
  }
  .summary__badge--paused {
    background: color-mix(in srgb, var(--warning) 18%, transparent);
    color: var(--warning);
  }
  .summary__badge--blocked {
    background: color-mix(in srgb, var(--warning) 18%, transparent);
    color: var(--warning);
  }

  .summary__phase {
    font-size: var(--text-xs);
    opacity: 0.72;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .summary__meta {
    padding-left: 6px;
    border-left: var(--magi-hairline) solid var(--border-subtle);
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }

  .summary__time {
    margin-left: auto;
    font-size: var(--text-xs);
    opacity: 0.5;
    font-variant-numeric: tabular-nums;
  }

  .runtime-diagnostics__content {
    position: absolute;
    top: calc(100% - 1px);
    left: -1px;
    right: -1px;
    z-index: 24;
    padding: 6px 12px 10px;
    display: flex;
    flex-direction: column;
    gap: 0;
    max-height: min(60vh, 560px);
    overflow-y: auto;
    border: var(--magi-hairline) solid var(--vscode-editorWidget-border, var(--border));
    border-top: 0;
    border-radius: 0 0 8px 8px;
    box-shadow: var(--shadow-lg);
    pointer-events: auto;
  }

  .runtime-diagnostics__content > :last-child {
    border-bottom: 0;
  }

  .metrics-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(110px, 1fr));
    gap: 0 14px;
    padding: 4px 0 8px;
    border-bottom: var(--magi-hairline) solid var(--border-subtle);
  }

  .metric-card {
    padding: 7px 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
  }

  .metric-card__header {
    display: flex;
    align-items: center;
    gap: 4px;
    font-size: var(--text-xs);
    opacity: 0.8;
    min-width: 0;
  }

  :global(.metric-card__icon) {
    opacity: 0.9;
  }

  .metric-card__title {
    font-size: var(--text-xs);
    min-width: 0;
    overflow-wrap: anywhere;
  }

  .metric-card__value {
    font-size: 16px;
    font-weight: var(--font-semibold);
    font-variant-numeric: tabular-nums;
    line-height: 1.3;
    min-width: 0;
    overflow-wrap: anywhere;
    word-break: break-word;
  }

  .metric-card__value--warn {
    color: var(--vscode-editorWarning-foreground, var(--warning));
  }

  .metric-card__value--notice {
    color: var(--info);
  }

  .metric-card__sub {
    font-size: var(--text-xs);
    opacity: 0.7;
    min-width: 0;
    overflow-wrap: anywhere;
    word-break: break-word;
  }

  .metric-card__sub--ok {
    color: var(--success);
    opacity: 1;
  }

  .metric-card__sub--warn {
    color: var(--vscode-editorWarning-foreground, var(--warning));
    opacity: 1;
  }

  .metric-card__sub--notice {
    color: var(--info);
    opacity: 1;
  }

  .metric-card__sub--danger {
    color: var(--error);
    opacity: 1;
  }

  .progress-bar {
    height: 4px;
    border-radius: 2px;
    background: var(--vscode-editorWidget-border, var(--border));
    overflow: hidden;
  }

  .progress-bar__fill {
    height: 100%;
    border-radius: 2px;
    background: var(--info);
    transition: width 0.3s ease;
  }

  .progress-bar__fill--notice {
    background: var(--info);
  }

  .progress-bar__fill--warning {
    background: var(--vscode-editorWarning-foreground, var(--warning));
  }

  .progress-bar__fill--danger {
    background: var(--error);
  }

  .runtime-diagnostics__block {
    display: flex;
    flex-direction: column;
    gap: 5px;
    padding: 9px 0;
    border: 0;
    border-bottom: var(--magi-hairline) solid var(--border-subtle);
    border-radius: 0;
    background: transparent;
  }

  .runtime-diagnostics__label {
    font-size: var(--text-xs);
    opacity: 0.8;
    margin-bottom: 2px;
  }

  .runtime-diagnostics__record-detail {
    margin: 3px 0 0 19px;
    padding: 7px 9px;
    border: 0;
    border-left: var(--magi-hairline) solid var(--border-subtle);
    background: color-mix(in srgb, var(--vscode-editor-background, var(--surface-1)) 68%, transparent);
    color: var(--vscode-foreground, var(--foreground));
    font-family: var(--vscode-editor-font-family, ui-monospace, monospace);
    font-size: var(--text-xs);
    line-height: 1.5;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    user-select: text;
  }

  .runtime-diagnostics__kv-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
    gap: 6px 18px;
  }

  .runtime-diagnostics__kv-item {
    padding: 2px 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
  }

  .runtime-diagnostics__kv-label {
    font-size: var(--text-xs);
    opacity: 0.75;
  }

  .runtime-diagnostics__kv-value {
    font-size: 12px;
    font-family: var(--vscode-editor-font-family, monospace);
    min-width: 0;
    overflow-wrap: anywhere;
    word-break: break-word;
  }

  .runtime-diagnostics__ops-list {
    display: flex;
    flex-direction: column;
    gap: 0;
  }

  .runtime-diagnostics__ops-item {
    padding: 7px 0;
    border-top: var(--magi-hairline) solid color-mix(in srgb, var(--border-subtle) 72%, transparent);
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
  }

  .runtime-diagnostics__ops-title-row {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 8px;
  }

  .runtime-diagnostics__record-toggle {
    width: 100%;
    margin: 0;
    padding: 0;
    border: 0;
    background: transparent;
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: default;
  }

  .runtime-diagnostics__record-toggle:disabled {
    opacity: 1;
  }

  .runtime-diagnostics__record-toggle--expandable {
    cursor: pointer;
  }

  .runtime-diagnostics__record-toggle--expandable:hover .runtime-diagnostics__ops-title {
    color: var(--vscode-textLink-foreground, var(--accent));
  }

  .runtime-diagnostics__record-chevron {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    flex: 0 0 11px;
    height: 17px;
    opacity: 0.72;
  }

  .runtime-diagnostics__record-chevron--hidden {
    visibility: hidden;
  }

  .runtime-diagnostics__ops-title {
    font-size: 12px;
    font-weight: var(--font-medium);
    line-height: 1.4;
    min-width: 0;
    overflow-wrap: anywhere;
    word-break: break-word;
  }

  .runtime-diagnostics__record-toggle .runtime-diagnostics__ops-title {
    flex: 1 1 auto;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .runtime-diagnostics__ops-time {
    font-size: var(--text-xs);
    opacity: 0.7;
    white-space: nowrap;
    font-variant-numeric: tabular-nums;
  }

  .runtime-diagnostics__ops-sub {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: var(--text-xs);
    opacity: 0.78;
    line-height: 1.5;
    min-width: 0;
    overflow-wrap: anywhere;
    word-break: break-word;
  }

  .runtime-diagnostics__record-kind {
    flex: 0 0 auto;
    font-weight: var(--font-semibold);
    font-size: var(--text-xs);
  }

  .runtime-diagnostics__ops-item--success .runtime-diagnostics__record-kind {
    color: var(--success);
  }

  .runtime-diagnostics__ops-item--warning .runtime-diagnostics__record-kind {
    color: var(--vscode-editorWarning-foreground, var(--warning));
  }

  .runtime-diagnostics__ops-item--error .runtime-diagnostics__record-kind {
    color: var(--vscode-editorError-foreground, var(--error));
  }

  @container runtime-panel (max-width: 640px) {
    .runtime-diagnostics {
      margin: 6px 8px 0;
    }

    .runtime-diagnostics__summary-button {
      padding: 7px 9px;
      gap: 6px;
      flex-wrap: nowrap;
    }

    .summary__phase,
    .summary__meta,
    .summary__time {
      display: none;
    }

    .runtime-diagnostics__content {
      left: 0;
      right: 0;
      padding: 5px 10px 8px;
      max-height: min(52vh, 420px);
      gap: 0;
    }

    .metrics-grid,
    .runtime-diagnostics__kv-grid {
      grid-template-columns: 1fr;
    }

    .runtime-diagnostics__ops-title-row {
      flex-direction: column;
      align-items: flex-start;
      gap: 4px;
    }

    .runtime-diagnostics__ops-time {
      white-space: normal;
    }

  }
</style>
