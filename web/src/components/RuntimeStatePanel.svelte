<script lang="ts">
  import {
    mapTaskSemanticStatusToDisplayStatus,
    resolveTaskSemanticStatus,
  } from '../shared/task-status-semantics';
  import type {
    OrchestratorRuntimeState,
    OrchestratorRuntimeDecisionTraceEntry,
  } from '../types/message';
  import Icon from './Icon.svelte';
  import type { IconName } from '../lib/icons';
  import { i18n } from '../stores/i18n.svelte';

  interface Props {
    runtimeState: OrchestratorRuntimeState | null;
    isProcessing?: boolean;
    processingStartedAt?: number | null;
  }

  /** knowledgeAudit 运行时结构（后端类型为 unknown，这里给出前端期望的形状） */
  interface KnowledgeAuditView {
    recentEntries?: KnowledgeAuditEntry[];
    eventCount?: number;
  }

  /** knowledgeAudit.recentEntries 中的单条记录 */
  interface KnowledgeAuditEntry {
    timestamp?: number;
    kind?: string;
    summary?: string;
    purpose?: string;
    consumer?: string;
    resultKind?: string;
    referenceCount?: number;
    [key: string]: unknown;
  }

  let {
    runtimeState,
    isProcessing = false,
    processingStartedAt = null,
  }: Props = $props();
  let isPanelExpanded = $state(false);
  let panelRef: HTMLElement | undefined = $state();
  type DiagnosticsSectionKey = 'timeline' | 'stateDiff' | 'decisionTrace' | null;
  let expandedSection = $state<DiagnosticsSectionKey>(null);

  // 展开后按 popover 行为闭合：点击面板外部或按 ESC 即收起；同时折叠内部子区，避免下次再展开时残留状态。
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
      expandedSection = null;
    }
    function handleKeydown(event: KeyboardEvent): void {
      if (event.key === 'Escape') {
        isPanelExpanded = false;
        expandedSection = null;
      }
    }
    document.addEventListener('mousedown', handleOutsideMouseDown, true);
    document.addEventListener('keydown', handleKeydown);
    return () => {
      document.removeEventListener('mousedown', handleOutsideMouseDown, true);
      document.removeEventListener('keydown', handleKeydown);
    };
  });
  const isTimelineExpanded = $derived(expandedSection === 'timeline');
  const isStateDiffExpanded = $derived(expandedSection === 'stateDiff');
  const isDecisionTraceExpanded = $derived(expandedSection === 'decisionTrace');

  const recentTrace = $derived.by(() => {
    const trace = runtimeState?.runtimeDecisionTrace;
    if (!Array.isArray(trace) || trace.length === 0) {
      return [] as OrchestratorRuntimeDecisionTraceEntry[];
    }
    return trace.slice(-8);
  });

  const failureReason = $derived.by(() => {
    const raw = runtimeState?.failureReason;
    return typeof raw === 'string' && raw.trim().length > 0 ? raw.trim() : '';
  });

  const failureErrors = $derived.by(() => {
    const errors = runtimeState?.errors;
    if (!Array.isArray(errors)) {
      return [] as string[];
    }
    return errors
      .filter((item): item is string => typeof item === 'string' && item.trim().length > 0)
      .map((item) => item.trim())
      .filter((item) => !isGeneratedRuntimeIdentifier(item))
      .filter((item, index, arr) => arr.indexOf(item) === index);
  });

  const opsView = $derived.by(() => runtimeState?.opsView || null);
  const knowledgeAudit = $derived.by(() => (opsView?.knowledgeAudit || null) as KnowledgeAuditView | null);
  const executionGroupSummary = $derived.by(() => opsView?.executionGroup || null);
  const planSummary = $derived.by(() => opsView?.plan || null);

  const scopeEntries = $derived.by(() => {
    const scope = opsView?.scope;
    if (!scope) {
      return [] as Array<{ label: string; value: string }>;
    }
    const entries: Array<{ label: string; value: string }> = [];
    if (executionGroupSummary?.title) {
      entries.push({
        label: i18n.t('runtimeState.summary.executionGroup'),
        value: executionGroupSummary.title,
      });
    }
    if (planSummary?.planId || scope.planId) {
      entries.push({
        label: i18n.t('runtimeState.summary.plan'),
        value: formatPlanSummaryLabel(planSummary?.status, planSummary?.version),
      });
    }
    return entries;
  });

  const knowledgeAuditEntries = $derived.by(() => (
    Array.isArray(knowledgeAudit?.recentEntries) ? knowledgeAudit.recentEntries : []
  ));

  const knowledgeAuditSummaryEntries = $derived.by(() => {
    if (!knowledgeAudit) {
      return [] as Array<{ label: string; value: string }>;
    }
    const entries: Array<{ label: string; value: string }> = [];
    if (typeof knowledgeAudit.eventCount === 'number' && knowledgeAudit.eventCount > 0) {
      entries.push({ label: i18n.t('runtimeDiagnostics.auditEvents'), value: String(knowledgeAudit.eventCount) });
    }
    return entries;
  });

  const recentTimeline = $derived.by(() => (
    Array.isArray(opsView?.recentTimeline)
      ? opsView.recentTimeline.filter((item) => Boolean(formatTimelineSummary(item)))
      : []
  ));
  const recentStateDiffs = $derived.by(() => (
    Array.isArray(opsView?.recentStateDiffs)
      ? opsView.recentStateDiffs.filter((item) => hasReadableStateDiff(item))
      : []
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
  const failureRootCause = $derived.by(() => opsView?.failureRootCause || null);

  const summaryEntries = $derived.by(() => {
    if (!runtimeState) {
      return [] as Array<{ label: string; value: string }>;
    }
    const entries: Array<{ label: string; value: string }> = [
      { label: i18n.t('runtimeState.summary.phase'), value: formatRuntimePhase(runtimeState.phase) },
      { label: i18n.t('runtimeState.summary.lastEventAt'), value: formatDateTime(runtimeState.lastEventAt) },
    ];
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
    if (runtimeState.statusReason) {
      entries.push({ label: i18n.t('runtimeState.summary.reason'), value: runtimeState.statusReason });
    }
    if (runtimeState.chain?.chainId) {
      entries.push({
        label: i18n.t('runtimeState.summary.chain'),
        value: formatChainSummary(runtimeState.chain.status, runtimeState.chain.attempt),
      });
    }
    if (runtimeState.canResume) {
      entries.push({ label: i18n.t('runtimeState.summary.resume'), value: i18n.t('runtimeState.resume.ready') });
    }
    return entries;
  });

  const recoveryEntries = $derived.by(() => {
    const recovery = opsView?.recovery;
    if (!recovery) {
      return [] as Array<{ label: string; value: string }>;
    }
    const entries: Array<{ label: string; value: string }> = [];
    if (recovery.continuationPolicy) {
      entries.push({ label: i18n.t('runtimeDiagnostics.recovery.continuationPolicy'), value: recovery.continuationPolicy });
    }
    if (recovery.continuationReason) {
      entries.push({ label: i18n.t('runtimeDiagnostics.recovery.continuationReason'), value: recovery.continuationReason });
    }
    if (recovery.waitState) {
      entries.push({ label: i18n.t('runtimeDiagnostics.recovery.waitState'), value: recovery.waitState });
    }
    if (recovery.replanState) {
      entries.push({ label: i18n.t('runtimeDiagnostics.recovery.replanState'), value: recovery.replanState });
    }
    if (recovery.terminationReason) {
      entries.push({ label: i18n.t('runtimeDiagnostics.recovery.terminationReason'), value: recovery.terminationReason });
    }
    if (recovery.acceptanceSummary) {
      entries.push({ label: i18n.t('runtimeDiagnostics.recovery.acceptanceSummary'), value: String(recovery.acceptanceSummary) });
    }
    if (recovery.reviewState) {
      entries.push({ label: i18n.t('runtimeDiagnostics.recovery.reviewState'), value: recovery.reviewState });
    }
    if (recovery.latestSnapshotId) {
      entries.push({
        label: i18n.t('runtimeDiagnostics.recovery.latestSnapshotId'),
        value: formatNamedReference(
          formatSnapshotStorageLabel(recovery.snapshotStorage),
          recovery.latestSnapshotId,
        ),
      });
    }
    if (recovery.latestSnapshotCreatedAt) {
      entries.push({
        label: i18n.t('runtimeDiagnostics.recovery.latestSnapshotCreatedAt'),
        value: formatDateTime(recovery.latestSnapshotCreatedAt),
      });
    }
    if (recovery.snapshotStorage) {
      entries.push({
        label: i18n.t('runtimeDiagnostics.recovery.snapshotStorage'),
        value: formatSnapshotStorageDetail(recovery.snapshotStorage, recovery.snapshotBaseRef),
      });
    }
    if (typeof recovery.snapshotDirtyFileCount === 'number') {
      entries.push({
        label: i18n.t('runtimeDiagnostics.recovery.snapshotDirtyFileCount'),
        value: String(recovery.snapshotDirtyFileCount),
      });
    }
    if (typeof recovery.snapshotPendingChangeCount === 'number') {
      entries.push({
        label: i18n.t('runtimeDiagnostics.recovery.snapshotPendingChangeCount'),
        value: String(recovery.snapshotPendingChangeCount),
      });
    }
    if (typeof recovery.restoredWorkerBranchCount === 'number') {
      entries.push({
        label: i18n.t('runtimeDiagnostics.recovery.restoredWorkerBranchCount'),
        value: String(recovery.restoredWorkerBranchCount),
      });
    }
    if (typeof recovery.restoredWorkerSessionCount === 'number') {
      entries.push({
        label: i18n.t('runtimeDiagnostics.recovery.restoredWorkerSessionCount'),
        value: String(recovery.restoredWorkerSessionCount),
      });
    }
    if (typeof recovery.pendingTaskCount === 'number') {
      entries.push({ label: i18n.t('runtimeDiagnostics.recovery.pendingTaskCount'), value: String(recovery.pendingTaskCount) });
    }
    if (typeof recovery.runningTaskCount === 'number') {
      entries.push({ label: i18n.t('runtimeDiagnostics.recovery.runningTaskCount'), value: String(recovery.runningTaskCount) });
    }
    if (typeof recovery.completedTaskCount === 'number') {
      entries.push({ label: i18n.t('runtimeDiagnostics.recovery.completedTaskCount'), value: String(recovery.completedTaskCount) });
    }
    if (typeof recovery.cancelledTaskCount === 'number') {
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

  const effectiveStatus = $derived.by(() => (
    canonicalProcessingActive ? 'running' : runtimeState?.status
  ));
  const effectivePhase = $derived.by(() => (
    canonicalProcessingActive ? 'running' : runtimeState?.phase
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

  // 任务进度计算
  const taskProgress = $derived.by(() => {
    const snap = runtimeState?.runtimeSnapshot;
    if (!snap) return null;
    const total = snap.requiredTotal ?? 0;
    const failed = snap.failedRequired ?? 0;
    const running = snap.runningOrPendingRequired ?? 0;
    const completed = Math.max(0, total - failed - running);
    const percent = total > 0 ? Math.round((completed / total) * 100) : 0;
    return { completed, failed, running, total, percent };
  });

  function formatTimestamp(timestamp: number): string {
    if (!Number.isFinite(timestamp)) return '--';
    return new Date(timestamp).toLocaleTimeString();
  }

  function formatDateTime(timestamp: number): string {
    if (!Number.isFinite(timestamp)) return '--';
    return new Date(timestamp).toLocaleString();
  }

  function shortenIdentifier(value: string | undefined): string {
    const normalized = typeof value === 'string' ? value.trim() : '';
    if (!normalized) return '--';
    if (normalized.length <= 20) return normalized;
    return `${normalized.slice(0, 8)}…${normalized.slice(-6)}`;
  }

  function formatNamedReference(title: string | undefined | null, id?: string | null): string {
    const normalizedTitle = typeof title === 'string' ? title.trim() : '';
    const normalizedId = typeof id === 'string' ? id.trim() : '';
    const readableId = normalizedId && !isGeneratedRuntimeIdentifier(normalizedId) ? normalizedId : '';
    if (normalizedTitle && readableId) {
      return `${normalizedTitle} (${shortenIdentifier(readableId)})`;
    }
    return normalizedTitle || (readableId ? shortenIdentifier(readableId) : '--');
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

  function formatChainSummary(status: string | undefined, attempt: number | undefined): string {
    const parts: string[] = [];
    if (typeof attempt === 'number' && Number.isFinite(attempt)) {
      parts.push(i18n.t('runtimeState.summary.chainAttempt', { attempt }));
    }
    if (typeof status === 'string' && status.trim()) {
      parts.push(formatAssignmentStatus(status));
    }
    return parts.join(' · ') || '--';
  }

  function formatSnapshotStorageLabel(storage: string | undefined): string {
    switch (storage) {
      case 'ghost_commit':
        return i18n.t('runtimeDiagnostics.recovery.storage.ghostCommit');
      case 'head_commit':
        return i18n.t('runtimeDiagnostics.recovery.storage.headCommit');
      default:
        return '--';
    }
  }

  function formatSnapshotStorageDetail(storage: string | undefined, baseRef?: string): string {
    const storageLabel = formatSnapshotStorageLabel(storage);
    const baseLabel = typeof baseRef === 'string' && baseRef.trim()
      ? `${i18n.t('runtimeDiagnostics.recovery.snapshotBaseRef')}: ${shortenIdentifier(baseRef)}`
      : '';
    return [storageLabel, baseLabel].filter(Boolean).join(' · ') || '--';
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
    return /\b(task|session|worker|mission|chain|recovery|assignment|request|batch|execution[_-]?group)[-_][a-z0-9_-]*\d{4,}/.test(normalized)
      || /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(normalized)
      || /\d{10,}/.test(normalized)
      || normalized.startsWith('task_failed:');
  }

  function resolveTaskRuntimeIdentifier(entry: Record<string, unknown>): string {
    const taskId = typeof entry.taskId === 'string' ? entry.taskId.trim() : '';
    if (taskId) {
      return taskId;
    }
    return '';
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

  function resolveCacheTone(health: string | undefined): 'normal' | 'notice' | 'warning' | 'danger' {
    switch (health) {
      case 'healthy':
        return 'normal';
      case 'cooling':
      case 'cold':
        return 'notice';
      case 'degraded':
        return 'danger';
      default:
        return 'warning';
    }
  }

  function resolveCacheToneLabel(health: string | undefined): string {
    switch (health) {
      case 'healthy': return i18n.t('runtimeDiagnostics.cacheHealth.healthy');
      case 'cooling': return i18n.t('runtimeDiagnostics.cacheHealth.cooling');
      case 'cold': return i18n.t('runtimeDiagnostics.cacheHealth.cold');
      case 'degraded': return i18n.t('runtimeDiagnostics.cacheHealth.degraded');
      default: return i18n.t('runtimeDiagnostics.cacheHealth.unknown');
    }
  }

  function resolveCacheFillClass(health: string | undefined): string {
    switch (resolveCacheTone(health)) {
      case 'notice': return 'progress-bar__fill--notice';
      case 'warning': return 'progress-bar__fill--warning';
      case 'danger': return 'progress-bar__fill--danger';
      default: return '';
    }
  }

  function resolveCacheModeLabel(mode: string | undefined): string {
    switch (mode) {
      case 'cache_control': return i18n.t('runtimeDiagnostics.cacheMode.cacheControl');
      case 'cache_editing': return i18n.t('runtimeDiagnostics.cacheMode.cacheEditing');
      case 'disabled': return i18n.t('runtimeDiagnostics.cacheMode.disabled');
      default: return i18n.t('runtimeDiagnostics.cacheMode.unsupported');
    }
  }

  function resolveCacheResetReasonLabel(reason: string | undefined): string {
    switch (reason) {
      case 'micro_compaction': return i18n.t('runtimeDiagnostics.cacheReset.microCompaction');
      case 'idle_micro_compaction': return i18n.t('runtimeDiagnostics.cacheReset.idleMicroCompaction');
      case 'manual_compaction': return i18n.t('runtimeDiagnostics.cacheReset.manualCompaction');
      case 'session_reset': return i18n.t('runtimeDiagnostics.cacheReset.sessionReset');
      default: return '';
    }
  }

  function resolveCacheBreakReasonLabel(reason: string | undefined): string {
    switch (reason) {
      case 'cache_read_miss': return i18n.t('runtimeDiagnostics.cacheBreak.cacheReadMiss');
      case 'cache_read_drop': return i18n.t('runtimeDiagnostics.cacheBreak.cacheReadDrop');
      case 'idle_expired': return i18n.t('runtimeDiagnostics.cacheBreak.idleExpired');
      default: return '';
    }
  }

  // 决策轨迹 phase → 文字标签
  function phaseLabel(phase: string): string {
    switch (phase) {
      case 'tool': return i18n.t('runtimeDiagnostics.phase.tool');
      case 'handoff': return i18n.t('runtimeDiagnostics.phase.handoff');
      case 'finalize': return i18n.t('runtimeDiagnostics.phase.finalize');
      case 'no_tool': return i18n.t('runtimeDiagnostics.phase.noTool');
      default: return phase;
    }
  }

  // 决策轨迹 phase → 样式类
  function phaseClass(phase: string): string {
    switch (phase) {
      case 'tool': return 'phase--tool';
      case 'handoff': return 'phase--handoff';
      case 'finalize': return 'phase--finalize';
      case 'no_tool': return 'phase--idle';
      default: return '';
    }
  }

  // 决策轨迹 action → 样式类
  function actionClass(action: string): string {
    switch (action) {
      case 'continue':
      case 'continue_with_prompt': return 'action--continue';
      case 'handoff': return 'action--handoff';
      case 'terminate': return 'action--terminate';
      case 'fallback': return 'action--fallback';
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
      i18n.t('runtimeDiagnostics.todoStats', {
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

  function formatChangedKeys(keys: string[]): string {
    if (!Array.isArray(keys) || keys.length === 0) {
      return '--';
    }
    const labels = keys
      .map((key) => formatRuntimeFieldLabel(key))
      .filter((label) => label.length > 0)
      .filter((label, index, arr) => arr.indexOf(label) === index);
    if (labels.length === 0) {
      return '状态已更新';
    }
    const visibleLabels = labels.slice(0, 4);
    return labels.length > visibleLabels.length
      ? `${visibleLabels.join('、')} 等 ${labels.length} 项`
      : visibleLabels.join('、');
  }

  function formatKnowledgePurpose(purpose: string): string {
    switch (purpose) {
      case 'project_context':
        return i18n.t('runtimeDiagnostics.knowledgePurpose.projectContext');
      case 'knowledge_index':
        return i18n.t('runtimeDiagnostics.knowledgePurpose.knowledgeIndex');
      case 'tool_query':
        return i18n.t('runtimeDiagnostics.knowledgePurpose.toolQuery');
      case 'knowledge_api':
        return i18n.t('runtimeDiagnostics.knowledgePurpose.knowledgeApi');
      case 'ui_panel':
        return i18n.t('runtimeDiagnostics.knowledgePurpose.uiPanel');
      default:
        return formatHumanizedRuntimeText(purpose) || '知识记录';
    }
  }

  function formatKnowledgeAuditScope(entry: KnowledgeAuditEntry): string {
    const scopes: string[] = [];
    appendReadableScope(scopes, i18n.t('runtimeDiagnostics.scope.request'), entry.requestId);
    appendReadableScope(scopes, i18n.t('runtimeDiagnostics.scope.executionGroup'), entry.executionGroupId);
    appendReadableScope(scopes, i18n.t('runtimeDiagnostics.scope.assignment'), entry.assignmentId);
    appendReadableScope(scopes, i18n.t('runtimeDiagnostics.scope.task'), resolveTaskRuntimeIdentifier(entry));
    appendReadableScope(scopes, i18n.t('runtimeDiagnostics.scope.worker'), entry.workerId);
    appendReadableScope(scopes, i18n.t('runtimeDiagnostics.scope.session'), entry.sessionId);
    return scopes.length > 0 ? scopes.join(' · ') : '';
  }

  function appendReadableScope(scopes: string[], label: string, value: unknown): void {
    const normalized = typeof value === 'string' ? value.trim() : '';
    if (!normalized || isGeneratedRuntimeIdentifier(normalized)) {
      return;
    }
    scopes.push(`${label}: ${shortenIdentifier(normalized)}`);
  }

  function formatKnowledgeAuditMeta(entry: KnowledgeAuditEntry): string {
    const parts: string[] = [];
    const consumer = formatKnowledgeConsumer(entry.consumer);
    if (consumer) {
      parts.push(`${i18n.t('runtimeDiagnostics.consumer')}: ${consumer}`);
    }
    const resultKind = formatKnowledgeResultKind(entry.resultKind);
    if (resultKind) {
      parts.push(`${i18n.t('runtimeDiagnostics.resultKind')}: ${resultKind}`);
    }
    if (typeof entry.referenceCount === 'number' && Number.isFinite(entry.referenceCount)) {
      parts.push(`${i18n.t('runtimeDiagnostics.references')}: ${entry.referenceCount}`);
    }
    return parts.join(' · ');
  }

  function formatKnowledgeConsumer(consumer: unknown): string {
    const normalized = typeof consumer === 'string' ? consumer.trim() : '';
    if (!normalized || isGeneratedRuntimeIdentifier(normalized)) {
      return '';
    }
    switch (normalized) {
      case 'prompt':
      case 'prompt_context':
      case 'prompt-context':
        return '提示词上下文';
      case 'runtime':
      case 'orchestrator':
        return '任务编排';
      case 'ui':
      case 'ui_panel':
      case 'ui-panel':
        return '运行态面板';
      default:
        return formatHumanizedRuntimeText(normalized);
    }
  }

  function formatKnowledgeResultKind(resultKind: unknown): string {
    const normalized = typeof resultKind === 'string' ? resultKind.trim() : '';
    if (!normalized) {
      return '';
    }
    switch (normalized) {
      case 'hit':
      case 'hits':
      case 'matched':
        return '已命中';
      case 'miss':
      case 'empty':
        return '未命中';
      case 'error':
      case 'failed':
        return '查询失败';
      default:
        return formatHumanizedRuntimeText(normalized);
    }
  }

  function formatTimelineTypeLabel(type: string): string {
    const normalized = typeof type === 'string' ? type.trim() : '';
    if (!normalized) return '--';
    switch (normalized) {
      case 'task.dispatched':
        return '任务已派发';
      case 'task.status.changed':
        return '任务状态更新';
      case 'mission.execution.overview':
        return '执行概览';
      case 'mission.resume.dispatch.created':
        return '恢复调度已创建';
      case 'worker.reported':
        return '执行者上报';
      case 'worker.tool.observed':
      case 'tool.invoked':
        return '工具调用';
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

  function formatTimelineSummary(item: { type: string; summary: string }): string {
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

  function formatStateDiffEntityLabel(item: { entityType: string; entityId: string }): string {
    const entityType = item.entityType || '--';
    const typeLabel = formatRuntimeEntityTypeLabel(entityType);
    return isGeneratedRuntimeIdentifier(item.entityId)
      ? typeLabel
      : `${typeLabel} · ${shortenIdentifier(item.entityId)}`;
  }

  function formatStateSummary(value: string | undefined): string {
    return formatHumanizedRuntimeText(value);
  }

  function hasReadableStateDiff(item: { entityType: string; entityId: string; changedKeys: string[]; beforeSummary?: string; afterSummary?: string }): boolean {
    return formatStateDiffEntityLabel(item) !== '--'
      || formatChangedKeys(item.changedKeys) !== '--'
      || Boolean(formatStateSummary(item.beforeSummary))
      || Boolean(formatStateSummary(item.afterSummary));
  }

  function formatRuntimeEntityTypeLabel(entityType: string | undefined): string {
    const normalized = typeof entityType === 'string' ? entityType.trim() : '';
    if (!normalized) return '--';
    switch (normalized) {
      case 'task':
        return '任务';
      case 'mission':
      case 'execution_group':
        return '执行组';
      case 'assignment':
        return '任务分配';
      case 'worker':
        return '执行者';
      case 'session':
        return '会话';
      case 'plan':
        return '计划';
      case 'recovery':
        return '恢复状态';
      default:
        return formatHumanizedRuntimeText(normalized) || '--';
    }
  }

  function formatRuntimeFieldLabel(key: string): string {
    const normalized = typeof key === 'string' ? key.trim() : '';
    if (!normalized || isInternalRuntimeField(normalized)) {
      return '';
    }
    switch (normalized) {
      case 'status':
      case 'current_status':
      case 'root_task_status':
        return '状态';
      case 'phase':
      case 'current_phase':
        return '阶段';
      case 'title':
      case 'task_title':
        return '标题';
      case 'goal':
        return '目标';
      case 'updated_at':
      case 'last_update':
        return '更新时间';
      case 'failed_dispatch_count':
        return '失败次数';
      case 'active_task_ids':
        return '活动任务';
      case 'active_branches':
        return '活动分支';
      default:
        return formatHumanizedRuntimeText(normalized);
    }
  }

  function isInternalRuntimeField(key: string): boolean {
    return /(^|_)(id|ids|ref|refs)$/.test(key)
      || key === 'event_id'
      || key === 'request_id'
      || key === 'session_id'
      || key === 'task_id'
      || key === 'worker_id'
      || key === 'assignment_id'
      || key === 'mission_id';
  }

  function formatDecisionAction(action: string): string {
    switch (action) {
      case 'continue':
        return '继续执行';
      case 'continue_with_prompt':
        return '补充约束后继续';
      case 'terminate':
        return '结束本轮';
      case 'handoff':
        return '交接处理';
      case 'fallback':
        return '改用备选路径';
      default:
        return formatHumanizedRuntimeText(action) || '决策更新';
    }
  }

  function formatDecisionDetail(item: OrchestratorRuntimeDecisionTraceEntry): string {
    const parts = [
      formatDecisionReason(item.reason),
      formatHumanizedRuntimeText(item.note),
    ].filter((part) => part.length > 0);
    return parts.join(' · ');
  }

  function formatDecisionReason(reason: string | undefined): string {
    const normalized = typeof reason === 'string' ? reason.trim() : '';
    if (!normalized) return '';
    switch (normalized) {
      case 'completed':
        return '已完成';
      case 'failed':
        return '执行失败';
      case 'cancelled':
        return '已取消';
      case 'governance_pause':
        return '等待治理检查';
      case 'stalled':
        return '进展停滞';
      case 'budget_exceeded':
        return '预算已耗尽';
      case 'external_wait_timeout':
        return '外部等待超时';
      case 'external_abort':
        return '外部中止';
      case 'upstream_model_error':
        return '上游模型连续失败';
      case 'interrupted':
        return '执行中断';
      case 'unknown':
        return '原因未知';
      default:
        return formatHumanizedRuntimeText(normalized);
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
    const withoutIdentifiers = raw
      .replace(/\b(task|session|worker|mission|chain|recovery|assignment|request|batch|execution[_-]?group)[-_:][a-z0-9_-]*\d{4,}\b/gi, '')
      .replace(/\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b/gi, '')
      .replace(/\b\d{10,}\b/g, '')
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

  function togglePanel(): void {
    isPanelExpanded = !isPanelExpanded;
    if (!isPanelExpanded) {
      expandedSection = null;
    }
  }

  function toggleSection(section: Exclude<DiagnosticsSectionKey, null>): void {
    expandedSection = expandedSection === section ? null : section;
  }
</script>

{#if runtimeState || canonicalProcessingActive}
  <section bind:this={panelRef} class="runtime-diagnostics runtime-diagnostics--{statusModifier}">
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
      <span class="summary__phase">{formatRuntimePhase(effectivePhase)}</span>
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

      {#if runtimeState?.runtimeSnapshot}
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

          {#if snap.reviewState}
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

          {#if snap.blockerState}
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

          {#if snap.budgetState}
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

          {#if snap.cacheState}
            <div class="metric-card">
              <div class="metric-card__header">
                <Icon name="database" size={12} class="metric-card__icon" />
                <span class="metric-card__title">{i18n.t('runtimeDiagnostics.cache')}</span>
              </div>
              <div
                class="metric-card__value"
                class:metric-card__value--notice={resolveCacheTone(snap.cacheState.health) === 'notice'}
                class:metric-card__value--warn={resolveCacheTone(snap.cacheState.health) === 'warning' || resolveCacheTone(snap.cacheState.health) === 'danger'}
              >
                {resolveCacheModeLabel(snap.cacheState.mode)}
              </div>
              {#if snap.cacheState.cacheReadRatio != null}
                <div class="progress-bar">
                  <div
                    class={`progress-bar__fill ${resolveCacheFillClass(snap.cacheState.health)}`}
                    style="width: {Math.max(0, Math.min(100, Math.round((snap.cacheState.cacheReadRatio ?? 0) * 100)))}%"
                  ></div>
                </div>
              {/if}
              <div class="metric-card__sub">
                {resolveCacheToneLabel(snap.cacheState.health)}
                {#if snap.cacheState.cacheReadTokens != null}
                  · {i18n.t('runtimeDiagnostics.cacheReadTokens', { value: formatTokens(snap.cacheState.cacheReadTokens) })}
                {/if}
                {#if snap.cacheState.cacheWriteTokens != null}
                  · {i18n.t('runtimeDiagnostics.cacheWriteTokens', { value: formatTokens(snap.cacheState.cacheWriteTokens) })}
                {/if}
                {#if snap.cacheState.cacheReadRatio != null}
                  · {i18n.t('runtimeDiagnostics.usageRatio', { value: formatUsageRatio(snap.cacheState.cacheReadRatio) })}
                {/if}
              </div>
              {#if snap.cacheState.baselineCacheReadTokens != null}
                <div class="metric-card__sub">
                  {i18n.t('runtimeDiagnostics.cacheBaseline', { value: formatTokens(snap.cacheState.baselineCacheReadTokens) })}
                </div>
              {/if}
              {#if snap.cacheState.lastResetReason || snap.cacheState.lastBreakReason}
                <div class="metric-card__sub metric-card__sub--notice">
                  {#if snap.cacheState.lastResetReason}
                    {i18n.t('runtimeDiagnostics.cacheResetTitle')}: {resolveCacheResetReasonLabel(snap.cacheState.lastResetReason)}
                  {/if}
                  {#if snap.cacheState.lastBreakReason}
                    {#if snap.cacheState.lastResetReason} · {/if}
                    {i18n.t('runtimeDiagnostics.cacheBreakTitle')}: {resolveCacheBreakReasonLabel(snap.cacheState.lastBreakReason)}
                  {/if}
                </div>
              {/if}
            </div>
          {/if}
        </div>
      {/if}

      <div class="runtime-diagnostics__block">
        <div class="runtime-diagnostics__label">{i18n.t('runtimeDiagnostics.assignmentTitle')}</div>
        {#if assignmentSummaries.length > 0}
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
        {:else}
          <div class="runtime-diagnostics__empty">{i18n.t('runtimeDiagnostics.noAssignments')}</div>
        {/if}
      </div>

      {#if recentTimeline.length > 0 || recentStateDiffs.length > 0 || recentTrace.length > 0}
      <div class="runtime-diagnostics__section-stack">
        {#if recentTimeline.length > 0}
        <section class="runtime-diagnostics__section-toggle">
          <button
            type="button"
            class="runtime-diagnostics__section-summary"
            class:runtime-diagnostics__section-summary--expanded={isTimelineExpanded}
            aria-expanded={isTimelineExpanded}
            onclick={() => toggleSection('timeline')}
          >
            <span class="runtime-diagnostics__section-title">
              <Icon name={isTimelineExpanded ? 'chevron-down' : 'chevron-right'} size={12} class="runtime-diagnostics__section-icon" />
              <span>{i18n.t('runtimeDiagnostics.timelineTitle')}</span>
            </span>
            <span class="runtime-diagnostics__section-count">{recentTimeline.length}</span>
          </button>
          {#if isTimelineExpanded}
          <div class="runtime-diagnostics__section-body">
            <div class="runtime-diagnostics__ops-list">
              {#each recentTimeline as item}
                <div class="runtime-diagnostics__ops-item">
                  <div class="runtime-diagnostics__ops-title-row">
                    <span class="runtime-diagnostics__ops-title">{formatTimelineSummary(item)}</span>
                    <span class="runtime-diagnostics__ops-time">{formatTimestamp(item.timestamp)}</span>
                  </div>
                  <div class="runtime-diagnostics__ops-sub">
                    {formatTimelineTypeLabel(item.type)}
                    {#if item.diffCount > 0}
                      · {item.diffCount} 项变更
                    {/if}
                  </div>
                </div>
              {/each}
            </div>
          </div>
          {/if}
        </section>
        {/if}

        {#if recentStateDiffs.length > 0}
        <section class="runtime-diagnostics__section-toggle">
          <button
            type="button"
            class="runtime-diagnostics__section-summary"
            class:runtime-diagnostics__section-summary--expanded={isStateDiffExpanded}
            aria-expanded={isStateDiffExpanded}
            onclick={() => toggleSection('stateDiff')}
          >
            <span class="runtime-diagnostics__section-title">
              <Icon name={isStateDiffExpanded ? 'chevron-down' : 'chevron-right'} size={12} class="runtime-diagnostics__section-icon" />
              <span>{i18n.t('runtimeDiagnostics.stateDiffTitle')}</span>
            </span>
            <span class="runtime-diagnostics__section-count">{recentStateDiffs.length}</span>
          </button>
          {#if isStateDiffExpanded}
          <div class="runtime-diagnostics__section-body">
            <div class="runtime-diagnostics__ops-list">
              {#each recentStateDiffs as item}
                {@const beforeSummary = formatStateSummary(item.beforeSummary)}
                {@const afterSummary = formatStateSummary(item.afterSummary)}
                <div class="runtime-diagnostics__ops-item">
                  <div class="runtime-diagnostics__ops-title-row">
                    <span class="runtime-diagnostics__ops-title">{formatStateDiffEntityLabel(item)}</span>
                    <span class="runtime-diagnostics__ops-time">{formatTimestamp(item.timestamp)}</span>
                  </div>
                  <div class="runtime-diagnostics__ops-sub">
                    {i18n.t('runtimeDiagnostics.changedKeys')}: {formatChangedKeys(item.changedKeys)}
                  </div>
                  {#if beforeSummary || afterSummary}
                    <div class="runtime-diagnostics__ops-sub">
                      {beforeSummary || '--'} → {afterSummary || '--'}
                    </div>
                  {/if}
                </div>
              {/each}
            </div>
          </div>
          {/if}
        </section>
        {/if}

        {#if recentTrace.length > 0}
        <section class="runtime-diagnostics__section-toggle">
          <button
            type="button"
            class="runtime-diagnostics__section-summary"
            class:runtime-diagnostics__section-summary--expanded={isDecisionTraceExpanded}
            aria-expanded={isDecisionTraceExpanded}
            onclick={() => toggleSection('decisionTrace')}
          >
            <span class="runtime-diagnostics__section-title">
              <Icon name={isDecisionTraceExpanded ? 'chevron-down' : 'chevron-right'} size={12} class="runtime-diagnostics__section-icon" />
              <span>{i18n.t('runtimeDiagnostics.decisionTrace')}</span>
            </span>
            <span class="runtime-diagnostics__section-count">{recentTrace.length}</span>
          </button>
          {#if isDecisionTraceExpanded}
          <div class="runtime-diagnostics__section-body">
            <div class="trace-list">
              {#each recentTrace as item}
                {@const decisionDetail = formatDecisionDetail(item)}
                <div class="trace-item">
                  <span class="trace-item__round">R{item.round}</span>
                  <span class="trace-item__phase {phaseClass(item.phase)}">{phaseLabel(item.phase)}</span>
                  <span class="trace-item__arrow">→</span>
                  <span class="trace-item__action {actionClass(item.action)}">{formatDecisionAction(item.action)}</span>
                  {#if item.requiredTotal > 0}
                    <span class="trace-item__meta">({item.requiredTotal})</span>
                  {/if}
                  {#if decisionDetail}
                    <span class="trace-item__note">{decisionDetail}</span>
                  {/if}
                </div>
              {/each}
            </div>
          </div>
          {/if}
        </section>
        {/if}
      </div>
      {/if}

      {#if runtimeState?.status === 'failed' && (failureReason || failureErrors.length > 0)}
        <div class="runtime-diagnostics__block runtime-diagnostics__block--failure">
          <div class="runtime-diagnostics__label">{i18n.t('runtimeDiagnostics.failureTitle')}</div>
          {#if failureReason}
            <div class="runtime-diagnostics__failure-reason">{failureReason}</div>
          {/if}
          {#if failureErrors.length > 0}
            <ul class="runtime-diagnostics__failure-list">
              {#each failureErrors as item}
                <li>{item}</li>
              {/each}
            </ul>
          {/if}
        </div>
      {/if}

      {#if failureRootCause}
        <div class="runtime-diagnostics__block runtime-diagnostics__block--failure">
          <div class="runtime-diagnostics__label">{i18n.t('runtimeDiagnostics.failureRootCauseTitle')}</div>
          <div class="runtime-diagnostics__failure-reason">{failureRootCause.summary}</div>
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

      {#if scopeEntries.length > 0}
        <div class="runtime-diagnostics__block">
          <div class="runtime-diagnostics__label">{i18n.t('runtimeDiagnostics.scopeTitle')}</div>
          <div class="runtime-diagnostics__kv-grid">
            {#each scopeEntries as item}
              <div class="runtime-diagnostics__kv-item">
                <div class="runtime-diagnostics__kv-label">{item.label}</div>
                <div class="runtime-diagnostics__kv-value">{item.value}</div>
              </div>
            {/each}
          </div>
        </div>
      {/if}

      {#if knowledgeAuditSummaryEntries.length > 0 || knowledgeAuditEntries.length > 0}
      <div class="runtime-diagnostics__block">
        <div class="runtime-diagnostics__label">{i18n.t('runtimeDiagnostics.knowledgeAuditTitle')}</div>
        {#if knowledgeAuditSummaryEntries.length > 0}
          <div class="runtime-diagnostics__kv-grid">
            {#each knowledgeAuditSummaryEntries as item}
              <div class="runtime-diagnostics__kv-item">
                <div class="runtime-diagnostics__kv-label">{item.label}</div>
                <div class="runtime-diagnostics__kv-value">{item.value}</div>
              </div>
            {/each}
          </div>
          {#if knowledgeAuditEntries.length > 0}
            <div class="runtime-diagnostics__ops-list">
              {#each knowledgeAuditEntries as item}
                {@const knowledgeMeta = formatKnowledgeAuditMeta(item)}
                {@const knowledgeScope = formatKnowledgeAuditScope(item)}
                <div class="runtime-diagnostics__ops-item">
                  <div class="runtime-diagnostics__ops-title-row">
                    <span class="runtime-diagnostics__ops-title">{formatKnowledgePurpose(item.purpose ?? '')}</span>
                    <span class="runtime-diagnostics__ops-time">{formatTimestamp(item.timestamp ?? 0)}</span>
                  </div>
                  {#if knowledgeMeta}
                  <div class="runtime-diagnostics__ops-sub">
                    {knowledgeMeta}
                  </div>
                  {/if}
                  {#if knowledgeScope}
                    <div class="runtime-diagnostics__ops-sub">{knowledgeScope}</div>
                  {/if}
                </div>
              {/each}
            </div>
          {/if}
        {/if}
      </div>
      {/if}
    </div>
    {/if}
  </section>
{/if}

<style>
  .runtime-diagnostics {
    margin: 8px 12px 0;
    border: 1px solid var(--vscode-editorWidget-border, var(--border));
    border-radius: 10px;
    background: var(--vscode-editorWidget-background, var(--surface-2));
    color: var(--vscode-foreground, var(--foreground));
    overflow: visible;
    border-left: 3px solid var(--vscode-editorWidget-border, var(--border));
    position: relative;
    z-index: 12;
  }

  /* 卡片左边框根据终态着色 */
  .runtime-diagnostics--completed { border-left-color: var(--success); }
  .runtime-diagnostics--failed    { border-left-color: var(--vscode-editorError-foreground, var(--error)); }
  .runtime-diagnostics--cancelled { border-left-color: var(--vscode-editorWidget-border, var(--border)); }
  .runtime-diagnostics--idle      { border-left-color: var(--vscode-editorWidget-border, var(--border)); }
  .runtime-diagnostics--running   { border-left-color: var(--vscode-progressBar-background, var(--info)); }
  .runtime-diagnostics--waiting   { border-left-color: var(--vscode-editorWarning-foreground, var(--warning)); }
  .runtime-diagnostics--paused    { border-left-color: var(--vscode-editorWarning-foreground, var(--warning)); }
  .runtime-diagnostics--blocked   { border-left-color: var(--vscode-editorWarning-foreground, var(--warning)); }

  .runtime-diagnostics__summary-button {
    width: 100%;
    cursor: pointer;
    padding: 11px 14px;
    font-size: 12px;
    user-select: none;
    display: flex;
    align-items: center;
    gap: 8px;
    border: 0;
    background: transparent;
    color: inherit;
    text-align: left;
    border-radius: 10px;
  }

  .runtime-diagnostics__summary-button:hover {
    background: color-mix(in srgb, var(--vscode-editor-background, var(--assistant-message-bg)) 55%, transparent);
  }

  .runtime-diagnostics__summary-button--expanded {
    border-bottom: 1px solid var(--vscode-editorWidget-border, var(--border));
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
    font-weight: 500;
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
    opacity: 0.65;
    font-family: var(--vscode-editor-font-family, monospace);
  }

  .summary__time {
    margin-left: auto;
    font-size: 11px;
    opacity: 0.5;
    font-variant-numeric: tabular-nums;
  }

  .runtime-diagnostics__content {
    position: absolute;
    top: calc(100% + 6px);
    left: -1px;
    right: -1px;
    z-index: 24;
    padding: 14px;
    display: flex;
    flex-direction: column;
    gap: 14px;
    max-height: min(70vh, 680px);
    overflow-y: auto;
    border: 1px solid var(--vscode-editorWidget-border, var(--border));
    border-radius: 10px;
    background: color-mix(in srgb, var(--vscode-editorWidget-background, var(--surface-2)) 96%, black 4%);
    box-shadow: 0 14px 36px rgba(0, 0, 0, 0.34);
    backdrop-filter: blur(10px);
    pointer-events: auto;
  }

  .metrics-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(110px, 1fr));
    gap: 8px;
  }

  .metric-card {
    padding: 8px;
    border-radius: 6px;
    background: var(--vscode-editor-background, var(--assistant-message-bg));
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
    gap: 6px;
    padding: 12px;
    border-radius: 10px;
    background: color-mix(in srgb, var(--vscode-editor-background, var(--assistant-message-bg)) 88%, transparent);
    border: 1px solid color-mix(in srgb, var(--vscode-editorWidget-border, var(--border)) 92%, transparent);
  }

  .runtime-diagnostics__block--failure {
    border: 1px solid color-mix(in srgb, var(--error) 28%, transparent);
    background: color-mix(in srgb, var(--vscode-editorError-foreground, var(--error)) 8%, transparent);
  }

  .runtime-diagnostics__label {
    font-size: 11px;
    opacity: 0.8;
    margin-bottom: 2px;
  }

  .runtime-diagnostics__failure-reason {
    font-size: 13px;
    line-height: 1.5;
    color: var(--vscode-foreground, var(--foreground));
    word-break: break-word;
  }

  .runtime-diagnostics__failure-list {
    margin: 0;
    padding-left: 18px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    color: var(--foreground-muted);
    font-size: 12px;
    line-height: 1.5;
  }

  .runtime-diagnostics__meta-line {
    font-size: 11px;
    opacity: 0.75;
    line-height: 1.5;
  }

  .runtime-diagnostics__kv-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
    gap: 8px;
  }

  .runtime-diagnostics__kv-item {
    padding: 8px;
    border-radius: 6px;
    background: var(--vscode-editor-background, var(--assistant-message-bg));
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
    gap: 6px;
  }

  .runtime-diagnostics__ops-item {
    padding: 8px;
    border-radius: 6px;
    background: var(--vscode-editor-background, var(--assistant-message-bg));
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
    font-size: 11px;
    opacity: 0.78;
    line-height: 1.5;
    min-width: 0;
    overflow-wrap: anywhere;
    word-break: break-word;
  }

  .runtime-diagnostics__section-stack {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  .runtime-diagnostics__section-toggle {
    border: 1px solid var(--vscode-editorWidget-border, var(--border));
    border-radius: 10px;
    background: color-mix(in srgb, var(--vscode-editor-background, var(--assistant-message-bg)) 88%, transparent);
    overflow: hidden;
  }

  .runtime-diagnostics__section-summary {
    cursor: pointer;
    width: 100%;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    padding: 11px 14px;
    font-size: 12px;
    font-weight: 500;
    border: 0;
    background: transparent;
    color: inherit;
    text-align: left;
  }

  .runtime-diagnostics__section-summary:hover {
    background: color-mix(in srgb, var(--vscode-editor-background, var(--assistant-message-bg)) 70%, transparent);
  }

  .runtime-diagnostics__section-summary--expanded {
    border-bottom: 1px solid var(--vscode-editorWidget-border, var(--border));
    background: color-mix(in srgb, var(--vscode-editor-background, var(--assistant-message-bg)) 74%, transparent);
  }

  .runtime-diagnostics__section-title {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }

  :global(.runtime-diagnostics__section-icon) {
    opacity: 0.72;
    flex-shrink: 0;
  }

  .runtime-diagnostics__section-count {
    font-size: 11px;
    opacity: 0.7;
    font-variant-numeric: tabular-nums;
    flex-shrink: 0;
  }

  .runtime-diagnostics__section-body {
    padding: 12px 14px 14px;
    background: color-mix(in srgb, var(--vscode-editorWidget-background, var(--surface-2)) 95%, transparent);
  }

  .trace-list {
    display: flex;
    flex-direction: column;
    gap: 3px;
  }

  .trace-item {
    display: flex;
    align-items: center;
    gap: 5px;
    font-size: 11px;
    line-height: 1.4;
    padding: 2px 0;
  }

  .trace-item__round {
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    min-width: 24px;
    opacity: 0.7;
  }

  .trace-item__phase {
    font-size: 10px;
    font-weight: 500;
    padding: 1px 5px;
    border-radius: 3px;
    min-width: 42px;
    text-align: center;
  }

  .phase--tool {
    background: color-mix(in srgb, var(--info) 12%, transparent);
    color: var(--info);
  }
  .phase--handoff {
    background: color-mix(in srgb, var(--warning) 12%, transparent);
    color: var(--warning);
  }
  .phase--finalize {
    background: color-mix(in srgb, var(--success) 12%, transparent);
    color: var(--success);
  }
  .phase--idle {
    background: color-mix(in srgb, var(--foreground-muted) 12%, transparent);
    color: var(--foreground-muted);
  }

  .trace-item__arrow {
    opacity: 0.55;
    font-size: 11px;
  }

  .trace-item__action {
    font-weight: 500;
    padding: 1px 5px;
    border-radius: 3px;
    font-size: 10px;
  }

  .action--continue {
    background: color-mix(in srgb, var(--info) 20%, transparent);
    color: var(--info);
  }

  .action--handoff {
    background: color-mix(in srgb, var(--warning) 15%, transparent);
    color: var(--warning);
  }

  .action--terminate {
    background: color-mix(in srgb, var(--success) 20%, transparent);
    color: var(--success);
  }

  .action--fallback {
    background: color-mix(in srgb, var(--error) 15%, transparent);
    color: var(--error);
  }

  .trace-item__meta {
    opacity: 0.65;
    font-size: 10px;
    font-variant-numeric: tabular-nums;
  }

  .trace-item__note {
    opacity: 0.65;
    font-size: 10px;
    margin-left: 2px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 160px;
  }

  .runtime-diagnostics__empty {
    font-size: 11px;
    opacity: 0.7;
  }

  @media (max-width: 640px) {
    .runtime-diagnostics {
      margin: 8px 8px 0;
    }

    .runtime-diagnostics__summary-button {
      padding: 10px 12px;
      gap: 6px;
      align-items: flex-start;
    }

    .summary__time {
      width: 100%;
      margin-left: 0;
      padding-left: 21px;
    }

    .runtime-diagnostics__content {
      left: 0;
      right: 0;
      padding: 12px;
      max-height: min(72vh, 560px);
      gap: 12px;
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

    .runtime-diagnostics__section-summary {
      padding: 10px 12px;
      align-items: flex-start;
    }

    .runtime-diagnostics__section-title {
      min-width: 0;
      flex: 1;
    }

    .trace-item {
      flex-wrap: wrap;
      align-items: flex-start;
    }

    .trace-item__note {
      white-space: normal;
      max-width: 100%;
      margin-left: 0;
    }
  }
</style>
