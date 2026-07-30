<script lang="ts">
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
    runtimeAssignmentNeedsAttention,
    shouldShowRuntimeBudget,
    shouldShowRuntimePanel,
    shouldShowRuntimePhase,
  } from '../lib/runtime-state-panel';
  import { mergeCurrentRuntimeTimelineEntries } from '../lib/runtime-timeline';
  import { resolveToolDisplayName } from '../lib/tool-display-name';

  interface Props {
    runtimeState: OrchestratorRuntimeState | null;
    conversationRecords?: OrchestrationRuntimeTimelineEntry[];
    conversationStartedAt?: number | null;
    isProcessing?: boolean;
    processingStartedAt?: number | null;
  }

  let {
    runtimeState,
    conversationRecords = [],
    conversationStartedAt = null,
    isProcessing = false,
    processingStartedAt = null,
  }: Props = $props();
  let isPanelExpanded = $state(false);
  let panelRef: HTMLElement | undefined = $state();

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
      isPanelExpanded = false;
    }
    function handleKeydown(event: KeyboardEvent): void {
      if (event.key === 'Escape') {
        isPanelExpanded = false;
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
    if (!isProcessing) {
      return false;
    }
    const status = runtimeState?.status;
    return !status
      || status === 'idle'
      || status === 'completed'
      || status === 'failed'
      || status === 'cancelled';
  });

  const canonicalFailureActive = $derived(
    !canonicalProcessingActive
    && conversationRecords.some((item) => item.kind === 'error'),
  );
  const canonicalInterruptionActive = $derived(
    !canonicalProcessingActive
    && conversationRecords.some((item) => (
      item.type === 'session.turn.interrupted' && item.kind === 'warning'
    )),
  );

  const effectiveStatus = $derived.by(() => (
    canonicalProcessingActive
      ? 'running'
      : canonicalFailureActive
        ? 'failed'
        : canonicalInterruptionActive
          ? 'paused'
          : runtimeState?.status
  ));
  const effectivePhase = $derived.by(() => (
    canonicalProcessingActive
      ? 'running'
      : canonicalFailureActive
        ? 'failed'
        : canonicalInterruptionActive
          ? 'paused'
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
    attentionAssignmentCount: assignmentSummaries.filter((item) => (
      runtimeAssignmentNeedsAttention(item.status)
    )).length,
  }));
  const phaseVisible = $derived(shouldShowRuntimePhase(effectiveStatus, effectivePhase));

  function formatTimestamp(timestamp: number): string {
    if (!Number.isFinite(timestamp)) return '--';
    return new Date(timestamp).toLocaleTimeString();
  }

  function formatDateTime(timestamp: number): string {
    if (!Number.isFinite(timestamp)) return '--';
    return new Date(timestamp).toLocaleString();
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
    return [statusLabel, versionLabel].filter(Boolean).join(' · ') || '--';
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
    return summary.join(' · ');
  }

  function formatTimelineTypeLabel(type: string): string {
    const normalized = typeof type === 'string' ? type.trim().toLowerCase() : '';
    if (!normalized) return '--';
    switch (normalized) {
      case 'task.dispatched':
        return '任务已派发';
      case 'task.running':
        return '任务执行中';
      case 'task.completed':
        return '任务已完成';
      case 'task.failed':
        return '任务失败';
      case 'task.status.changed':
        return '任务状态更新';
      case 'mission.execution.overview':
        return '执行概览';
      case 'knowledge.context.selected':
        return '知识按需决策';
      case 'knowledge.learning.extraction':
        return '自动经验抽取';
      case 'mission.resume.dispatch.created':
        return '恢复调度已创建';
      case 'worker.reported':
        return '执行者上报';
      case 'worker.tool.observed':
      case 'task.tool.invoked':
      case 'session.turn.tool.invoked':
      case 'tool.call.finished':
      case 'tool.invoked':
        return '工具调用';
      case 'session.turn.failed':
        return '本轮执行失败';
      case 'session.turn.interrupted':
        return '本轮执行中断';
      case 'session.turn.queue_failed':
        return '任务排队失败';
      case 'model.retry.runtime':
        return '模型重试';
      case 'worker.skill_dispatch.observed':
      case 'worker.skill_dispatch.applied':
        return '技能调度';
      case 'worker.executor.observed':
        return '执行器状态';
      case 'governance.decision.applied':
        return '决策已应用';
      case 'system.runtime.maintenance.status':
        return '运行态维护';
      default:
        return normalized
          .split('.')
          .map((part) => formatRuntimeTokenLabel(part))
          .filter(Boolean)
          .join(' · ') || '运行事件';
    }
  }

  function formatTimelineSummary(
    item: Pick<OrchestrationRuntimeTimelineEntry, 'type' | 'summary' | 'source'>,
  ): string {
    const toolName = item.source ? resolveToolDisplayName(item.source, i18n) : '';
    switch (item.type) {
      case 'session.turn.processing':
        return i18n.t('runtimeDiagnostics.record.processing');
      case 'session.tool.running':
        return i18n.t('runtimeDiagnostics.record.toolRunning', { tool: toolName });
      case 'session.tool.succeeded':
        return i18n.t('runtimeDiagnostics.record.toolSucceeded', { tool: toolName });
      case 'session.tool.failed': {
        const reason = item.summary === item.source
          ? ''
          : formatHumanizedRuntimeText(item.summary);
        return reason
          ? i18n.t('runtimeDiagnostics.record.toolFailedWithReason', { tool: toolName, reason })
          : i18n.t('runtimeDiagnostics.record.toolFailed', { tool: toolName });
      }
      case 'session.model.failed':
        return i18n.t('runtimeDiagnostics.record.modelFailed', {
          reason: sanitizeRuntimeDisplayText(item.summary),
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

  function resolveRuntimeRecordKind(
    item: Pick<OrchestrationRuntimeTimelineEntry, 'type' | 'kind'>,
  ): NonNullable<OrchestrationRuntimeTimelineEntry['kind']> {
    const type = item.type.trim().toLowerCase();
    if (type.includes('interrupted')) {
      return item.kind === 'warning' ? 'warning' : 'error';
    }
    if (type.includes('failed') || type.includes('blocked')) {
      return 'error';
    }
    return item.kind || 'progress';
  }

  function formatRuntimeRecordKind(
    item: Pick<OrchestrationRuntimeTimelineEntry, 'type' | 'kind'>,
  ): string {
    switch (resolveRuntimeRecordKind(item)) {
      case 'success': return '已完成';
      case 'warning': return '需处理';
      case 'error': return '错误';
      default: return '进行中';
    }
  }

  function formatRuntimeTokenLabel(token: string): string {
    switch (token) {
      case 'task': return '任务';
      case 'mission': return '执行组';
      case 'worker': return '执行者';
      case 'tool': return '工具';
      case 'governance': return '决策';
      case 'decision': return '决策';
      case 'system': return '系统';
      case 'runtime': return '运行态';
      case 'execution': return '执行';
      case 'overview': return '概览';
      case 'status': return '状态';
      case 'changed': return '更新';
      case 'dispatched': return '已派发';
      case 'reported': return '上报';
      case 'observed': return '已观测';
      case 'applied': return '已应用';
      case 'resume': return '恢复';
      case 'dispatch': return '调度';
      case 'created': return '已创建';
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
    isPanelExpanded = !isPanelExpanded;
  }
</script>

{#if panelVisible}
  <section
    bind:this={panelRef}
    class="runtime-diagnostics runtime-diagnostics--{statusModifier}"
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
    <div class="runtime-diagnostics__content">
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
              <div class="metric-card__sub">
                {i18n.t('runtimeDiagnostics.tokens', { value: formatTokens(snap.budgetState.tokenUsed) })}
                {#if snap.budgetState.tokenLimit != null}
                  · {i18n.t('runtimeDiagnostics.tokenLimit', { value: formatTokens(snap.budgetState.tokenLimit) })}
                {/if}
                {#if snap.budgetState.remainingTokens != null}
                  · {i18n.t('runtimeDiagnostics.remainingTokens', { value: formatTokens(snap.budgetState.remainingTokens) })}
                {/if}
                {#if snap.budgetState.errorRate != null && snap.budgetState.errorRate > 0}
                  · {i18n.t('runtimeDiagnostics.errorRate', { rate: Math.round(snap.budgetState.errorRate * 100) })}
                {/if}
              </div>
              {#if snap.budgetState.usageRatio != null}
                <div
                  class="metric-card__sub"
                  class:metric-card__sub--notice={resolveBudgetTone(snap.budgetState.warningLevel) === 'notice'}
                  class:metric-card__sub--warn={resolveBudgetTone(snap.budgetState.warningLevel) === 'warning'}
                  class:metric-card__sub--danger={resolveBudgetTone(snap.budgetState.warningLevel) === 'danger'}
                >
                  {resolveBudgetToneLabel(snap.budgetState.warningLevel)}
                  · {i18n.t('runtimeDiagnostics.usageRatio', { value: formatUsageRatio(snap.budgetState.usageRatio) })}
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
          {#each recentTimeline as item}
            <div
              class="runtime-diagnostics__ops-item runtime-diagnostics__ops-item--{resolveRuntimeRecordKind(item)}"
            >
              <div class="runtime-diagnostics__ops-title-row">
                <span class="runtime-diagnostics__record-kind">
                  {formatRuntimeRecordKind(item)}
                </span>
                <span class="runtime-diagnostics__ops-title">{formatTimelineSummary(item)}</span>
                <span class="runtime-diagnostics__ops-time">{formatTimestamp(item.timestamp)}</span>
              </div>
              {#if item.detail}
                <pre class="runtime-diagnostics__record-detail">{item.detail}</pre>
              {/if}
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

<style>
  .runtime-diagnostics {
    --runtime-status-color: var(--vscode-editorWidget-border, var(--border));
    margin: 6px 12px 0;
    border: 1px solid var(--vscode-editorWidget-border, var(--border));
    border-left: 2px solid var(--runtime-status-color);
    border-radius: 8px;
    background: var(--vscode-editorWidget-background, var(--surface-2));
    color: var(--vscode-foreground, var(--foreground));
    overflow: visible;
    position: relative;
    z-index: 12;
  }

  .runtime-diagnostics--expanded {
    border-bottom-left-radius: 0;
    border-bottom-right-radius: 0;
  }

  /* 左侧状态线贯穿标题栏和展开内容。 */
  .runtime-diagnostics--completed { --runtime-status-color: var(--success); }
  .runtime-diagnostics--failed    { --runtime-status-color: var(--vscode-editorError-foreground, var(--error)); }
  .runtime-diagnostics--cancelled { --runtime-status-color: var(--vscode-editorWidget-border, var(--border)); }
  .runtime-diagnostics--idle      { --runtime-status-color: var(--vscode-editorWidget-border, var(--border)); }
  .runtime-diagnostics--running   { --runtime-status-color: var(--vscode-progressBar-background, var(--info)); }
  .runtime-diagnostics--waiting   { --runtime-status-color: var(--vscode-editorWarning-foreground, var(--warning)); }
  .runtime-diagnostics--paused    { --runtime-status-color: var(--vscode-editorWarning-foreground, var(--warning)); }
  .runtime-diagnostics--blocked   { --runtime-status-color: var(--vscode-editorWarning-foreground, var(--warning)); }

  .runtime-diagnostics__summary-button {
    width: 100%;
    cursor: pointer;
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
    font-weight: 600;
  }

  .summary__badge {
    font-size: 10px;
    font-weight: 500;
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
    font-size: 11px;
    opacity: 0.72;
  }

  .summary__meta {
    padding-left: 6px;
    border-left: 1px solid var(--border-subtle);
    color: var(--foreground-muted);
    font-size: 10px;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }

  .summary__time {
    margin-left: auto;
    font-size: 11px;
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
    border: 1px solid var(--vscode-editorWidget-border, var(--border));
    border-top: 0;
    border-left: 2px solid var(--runtime-status-color);
    border-radius: 0 0 8px 8px;
    background: var(--vscode-editorWidget-background, var(--surface-2));
    box-shadow: 0 14px 36px rgba(0, 0, 0, 0.34);
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
    border-bottom: 1px solid var(--border-subtle);
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
    font-size: 11px;
    opacity: 0.8;
    min-width: 0;
  }

  :global(.metric-card__icon) {
    opacity: 0.9;
  }

  .metric-card__title {
    font-size: 11px;
    min-width: 0;
    overflow-wrap: anywhere;
  }

  .metric-card__value {
    font-size: 16px;
    font-weight: 600;
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
    font-size: 11px;
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
    border-bottom: 1px solid var(--border-subtle);
    border-radius: 0;
    background: transparent;
  }

  .runtime-diagnostics__label {
    font-size: 11px;
    opacity: 0.8;
    margin-bottom: 2px;
  }

  .runtime-diagnostics__record-detail {
    margin: 3px 0 0;
    padding: 0;
    border: 0;
    background: transparent;
    color: var(--vscode-foreground, var(--foreground));
    font-family: var(--vscode-editor-font-family, ui-monospace, monospace);
    font-size: 11px;
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
    font-size: 11px;
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
    border-top: 1px solid color-mix(in srgb, var(--border-subtle) 72%, transparent);
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

  .runtime-diagnostics__ops-title {
    font-size: 12px;
    font-weight: 500;
    line-height: 1.4;
    min-width: 0;
    overflow-wrap: anywhere;
    word-break: break-word;
  }

  .runtime-diagnostics__ops-time {
    font-size: 11px;
    opacity: 0.7;
    white-space: nowrap;
    font-variant-numeric: tabular-nums;
  }

  .runtime-diagnostics__ops-sub {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 11px;
    opacity: 0.78;
    line-height: 1.5;
    min-width: 0;
    overflow-wrap: anywhere;
    word-break: break-word;
  }

  .runtime-diagnostics__ops-item--success {
    border-left: 2px solid var(--success);
    padding-left: 8px;
  }

  .runtime-diagnostics__ops-item--warning {
    border-left: 2px solid var(--vscode-editorWarning-foreground, var(--warning));
    padding-left: 8px;
  }

  .runtime-diagnostics__ops-item--error {
    border-left: 2px solid var(--vscode-inputValidation-errorBorder, var(--error));
    padding-left: 8px;
  }

  .runtime-diagnostics__record-kind {
    flex: 0 0 auto;
    font-weight: 600;
    font-size: 11px;
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

  @media (max-width: 640px) {
    .runtime-diagnostics {
      margin: 6px 8px 0;
    }

    .runtime-diagnostics__summary-button {
      padding: 7px 9px;
      gap: 6px;
      flex-wrap: wrap;
    }

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
