<script lang="ts">
  import type {
    OrchestratorRuntimeState,
    OrchestratorRuntimeDecisionTraceEntry,
  } from '../types/message';
  import Icon from './Icon.svelte';
  import type { IconName } from '../lib/icons';
  import { i18n } from '../stores/i18n.svelte';

  interface Props {
    runtimeState: OrchestratorRuntimeState | null;
  }

  let { runtimeState }: Props = $props();

  type KnowledgeAuditEntry =
    NonNullable<NonNullable<OrchestratorRuntimeState['opsView']>['knowledgeAudit']>['recentEntries'][number];

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
      .filter((item, index, arr) => arr.indexOf(item) === index);
  });

  const opsView = $derived.by(() => runtimeState?.opsView || null);
  const knowledgeAudit = $derived.by(() => opsView?.knowledgeAudit || null);

  const scopeEntries = $derived.by(() => {
    const scope = opsView?.scope;
    if (!scope) {
      return [] as Array<{ label: string; value: string }>;
    }
    const entries: Array<{ label: string; value: string }> = [];
    if (scope.sessionId) {
      entries.push({ label: i18n.t('runtimeDiagnostics.scope.session'), value: scope.sessionId });
    }
    if (scope.requestId) {
      entries.push({ label: i18n.t('runtimeDiagnostics.scope.request'), value: scope.requestId });
    }
    if (scope.missionId) {
      entries.push({ label: i18n.t('runtimeDiagnostics.scope.mission'), value: scope.missionId });
    }
    if (scope.planId) {
      entries.push({ label: i18n.t('runtimeDiagnostics.scope.plan'), value: scope.planId });
    }
    if (scope.batchId) {
      entries.push({ label: i18n.t('runtimeDiagnostics.scope.batch'), value: scope.batchId });
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
    return [
      { label: i18n.t('runtimeDiagnostics.auditPath'), value: knowledgeAudit.auditPath },
      { label: i18n.t('runtimeDiagnostics.auditEvents'), value: String(knowledgeAudit.eventCount || 0) },
    ];
  });

  const recentTimeline = $derived.by(() => Array.isArray(opsView?.recentTimeline) ? opsView.recentTimeline : []);
  const recentStateDiffs = $derived.by(() => Array.isArray(opsView?.recentStateDiffs) ? opsView.recentStateDiffs : []);
  const assignmentSummaries = $derived.by(() => Array.isArray(runtimeState?.assignments) ? runtimeState.assignments : []);
  const failureRootCause = $derived.by(() => opsView?.failureRootCause || null);

  const summaryEntries = $derived.by(() => {
    if (!runtimeState) {
      return [] as Array<{ label: string; value: string }>;
    }
    const entries: Array<{ label: string; value: string }> = [
      { label: i18n.t('runtimeState.summary.phase'), value: runtimeState.phase },
      { label: i18n.t('runtimeState.summary.lastEventAt'), value: formatDateTime(runtimeState.lastEventAt) },
    ];
    if (runtimeState.startedAt) {
      entries.push({ label: i18n.t('runtimeState.summary.startedAt'), value: formatDateTime(runtimeState.startedAt) });
    }
    if (runtimeState.statusReason) {
      entries.push({ label: i18n.t('runtimeState.summary.reason'), value: runtimeState.statusReason });
    }
    if (runtimeState.chain?.chainId) {
      entries.push({
        label: i18n.t('runtimeState.summary.chain'),
        value: `${runtimeState.chain.chainId} · ${runtimeState.chain.status}`,
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
      entries.push({ label: i18n.t('runtimeDiagnostics.recovery.acceptanceSummary'), value: recovery.acceptanceSummary });
    }
    if (recovery.reviewState) {
      entries.push({ label: i18n.t('runtimeDiagnostics.recovery.reviewState'), value: recovery.reviewState });
    }
    return entries;
  });

  // 状态图标
  const statusIcon = $derived.by((): IconName => {
    switch (runtimeState?.status) {
      case 'idle': return 'circle';
      case 'running': return 'loader';
      case 'waiting': return 'clock';
      case 'paused': return 'taskPending';
      case 'completed': return 'taskComplete';
      case 'failed': return 'taskFailed';
      case 'cancelled': return 'stop';
      default: return 'loader';
    }
  });

  // 状态翻译文本
  const statusLabel = $derived.by(() => {
    switch (runtimeState?.status) {
      case 'idle': return i18n.t('runtimeState.status.idle');
      case 'running': return i18n.t('runtimeState.status.running');
      case 'waiting': return i18n.t('runtimeState.status.waiting');
      case 'paused': return i18n.t('runtimeState.status.paused');
      case 'completed': return i18n.t('runtimeState.status.completed');
      case 'failed': return i18n.t('runtimeState.status.failed');
      case 'cancelled': return i18n.t('runtimeState.status.cancelled');
      default: return i18n.t('runtimeState.status.idle');
    }
  });

  // 状态对应的 CSS modifier
  const statusModifier = $derived.by(() => {
    switch (runtimeState?.status) {
      case 'idle': return 'idle';
      case 'running': return 'running';
      case 'waiting': return 'waiting';
      case 'paused': return 'paused';
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

  function formatDuration(ms: number | undefined): string {
    if (!ms || !Number.isFinite(ms)) return '--';
    if (ms < 1000) return `${ms}ms`;
    const s = Math.round(ms / 1000);
    if (s < 60) return `${s}s`;
    const m = Math.floor(s / 60);
    return `${m}m${s % 60}s`;
  }

  function formatTokens(n: number | undefined): string {
    if (!n || !Number.isFinite(n)) return '--';
    if (n < 1000) return `${n}`;
    return `${(n / 1000).toFixed(1)}k`;
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

  function formatTodoStats(item: {
    completedTodos: number;
    todoTotal: number;
    runningTodos: number;
    failedTodos: number;
  }): string {
    return i18n.t('runtimeDiagnostics.todoStats', {
      completed: item.completedTodos,
      total: item.todoTotal,
      running: item.runningTodos,
      failed: item.failedTodos,
    });
  }

  function formatChangedKeys(keys: string[]): string {
    if (!Array.isArray(keys) || keys.length === 0) {
      return '--';
    }
    return keys.join(', ');
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
        return purpose;
    }
  }

  function formatKnowledgeAuditScope(entry: KnowledgeAuditEntry): string {
    const scopes: string[] = [];
    if (entry.requestId) {
      scopes.push(`${i18n.t('runtimeDiagnostics.scope.request')}: ${entry.requestId}`);
    }
    if (entry.missionId) {
      scopes.push(`${i18n.t('runtimeDiagnostics.scope.mission')}: ${entry.missionId}`);
    }
    if (entry.assignmentId) {
      scopes.push(`${i18n.t('runtimeDiagnostics.scope.assignment')}: ${entry.assignmentId}`);
    }
    if (entry.todoId) {
      scopes.push(`${i18n.t('runtimeDiagnostics.scope.todo')}: ${entry.todoId}`);
    }
    if (entry.workerId) {
      scopes.push(`${i18n.t('runtimeDiagnostics.scope.worker')}: ${entry.workerId}`);
    }
    if (scopes.length === 0 && entry.sessionId) {
      scopes.push(`${i18n.t('runtimeDiagnostics.scope.session')}: ${entry.sessionId}`);
    }
    return scopes.length > 0 ? scopes.join(' · ') : '--';
  }
</script>

{#if runtimeState}
  <details class="runtime-diagnostics runtime-diagnostics--{statusModifier}">
    <summary>
      <Icon name={statusIcon} size={13} class="summary__icon" />
      <span class="summary__title">{i18n.t('runtimeState.title')}</span>
      <span class="summary__badge summary__badge--{statusModifier}">{statusLabel}</span>
      <span class="summary__phase">{runtimeState.phase}</span>
      <span class="summary__time">{formatTimestamp(runtimeState.lastEventAt)}</span>
    </summary>
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

      {#if runtimeState.runtimeSnapshot}
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
              <div class="metric-card__value">{formatDuration(snap.budgetState.elapsedMs)}</div>
              <div class="metric-card__sub">
                {i18n.t('runtimeDiagnostics.tokens', { value: formatTokens(snap.budgetState.tokenUsed) })}
                {#if snap.budgetState.errorRate != null && snap.budgetState.errorRate > 0}
                  · {i18n.t('runtimeDiagnostics.errorRate', { rate: Math.round(snap.budgetState.errorRate * 100) })}
                {/if}
              </div>
            </div>
          {/if}
        </div>
      {/if}

      {#if runtimeState.status === 'failed' && (failureReason || failureErrors.length > 0)}
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
                <div class="runtime-diagnostics__ops-item">
                  <div class="runtime-diagnostics__ops-title-row">
                    <span class="runtime-diagnostics__ops-title">{formatKnowledgePurpose(item.purpose)}</span>
                    <span class="runtime-diagnostics__ops-time">{formatTimestamp(item.timestamp)}</span>
                  </div>
                  <div class="runtime-diagnostics__ops-sub">
                    {i18n.t('runtimeDiagnostics.consumer')}: {item.consumer || '--'}
                    · {i18n.t('runtimeDiagnostics.resultKind')}: {item.resultKind}
                    · {i18n.t('runtimeDiagnostics.references')}: {item.referenceCount}
                  </div>
                  <div class="runtime-diagnostics__ops-sub">{formatKnowledgeAuditScope(item)}</div>
                </div>
              {/each}
            </div>
          {:else}
            <div class="runtime-diagnostics__empty">{i18n.t('runtimeDiagnostics.noKnowledgeAudit')}</div>
          {/if}
        {:else}
          <div class="runtime-diagnostics__empty">{i18n.t('runtimeDiagnostics.noKnowledgeAudit')}</div>
        {/if}
      </div>

      {#if failureRootCause}
        <div class="runtime-diagnostics__block runtime-diagnostics__block--failure">
          <div class="runtime-diagnostics__label">{i18n.t('runtimeDiagnostics.failureRootCauseTitle')}</div>
          <div class="runtime-diagnostics__failure-reason">{failureRootCause.summary}</div>
          <div class="runtime-diagnostics__meta-line">
            {formatDateTime(failureRootCause.occurredAt)}
            {#if failureRootCause.eventType}
              · {failureRootCause.eventType}
            {/if}
            {#if failureRootCause.assignmentId}
              · {failureRootCause.assignmentId}
            {/if}
            {#if failureRootCause.todoId}
              · {failureRootCause.todoId}
            {/if}
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

      <div class="runtime-diagnostics__block">
        <div class="runtime-diagnostics__label">{i18n.t('runtimeDiagnostics.assignmentTitle')}</div>
        {#if assignmentSummaries.length > 0}
          <div class="runtime-diagnostics__ops-list">
            {#each assignmentSummaries as item}
              <div class="runtime-diagnostics__ops-item">
                <div class="runtime-diagnostics__ops-title-row">
                  <span class="runtime-diagnostics__ops-title">{item.title}</span>
                  <span class="runtime-diagnostics__ops-time">{item.workerId || '--'} · {item.status}</span>
                </div>
                <div class="runtime-diagnostics__ops-sub">{formatTodoStats(item)}</div>
              </div>
            {/each}
          </div>
        {:else}
          <div class="runtime-diagnostics__empty">{i18n.t('runtimeDiagnostics.noAssignments')}</div>
        {/if}
      </div>

      <div class="runtime-diagnostics__block">
        <div class="runtime-diagnostics__label">{i18n.t('runtimeDiagnostics.timelineTitle')}</div>
        {#if recentTimeline.length > 0}
          <div class="runtime-diagnostics__ops-list">
            {#each recentTimeline as item}
              <div class="runtime-diagnostics__ops-item">
                <div class="runtime-diagnostics__ops-title-row">
                  <span class="runtime-diagnostics__ops-title">{item.summary}</span>
                  <span class="runtime-diagnostics__ops-time">{formatTimestamp(item.timestamp)}</span>
                </div>
                <div class="runtime-diagnostics__ops-sub">
                  {item.type}
                  {#if item.diffCount > 0}
                    · Δ{item.diffCount}
                  {/if}
                </div>
              </div>
            {/each}
          </div>
        {:else}
          <div class="runtime-diagnostics__empty">{i18n.t('runtimeDiagnostics.noTimeline')}</div>
        {/if}
      </div>

      <div class="runtime-diagnostics__block">
        <div class="runtime-diagnostics__label">{i18n.t('runtimeDiagnostics.stateDiffTitle')}</div>
        {#if recentStateDiffs.length > 0}
          <div class="runtime-diagnostics__ops-list">
            {#each recentStateDiffs as item}
              <div class="runtime-diagnostics__ops-item">
                <div class="runtime-diagnostics__ops-title-row">
                  <span class="runtime-diagnostics__ops-title">{item.entityType}:{item.entityId}</span>
                  <span class="runtime-diagnostics__ops-time">{formatTimestamp(item.timestamp)}</span>
                </div>
                <div class="runtime-diagnostics__ops-sub">
                  {i18n.t('runtimeDiagnostics.changedKeys')}: {formatChangedKeys(item.changedKeys)}
                </div>
                {#if item.beforeSummary || item.afterSummary}
                  <div class="runtime-diagnostics__ops-sub">
                    {item.beforeSummary || '--'} -> {item.afterSummary || '--'}
                  </div>
                {/if}
              </div>
            {/each}
          </div>
        {:else}
          <div class="runtime-diagnostics__empty">{i18n.t('runtimeDiagnostics.noStateDiffs')}</div>
        {/if}
      </div>

      <div class="runtime-diagnostics__block">
        <div class="runtime-diagnostics__label">{i18n.t('runtimeDiagnostics.decisionTrace')}</div>
        {#if recentTrace.length > 0}
          <div class="trace-list">
            {#each recentTrace as item}
              <div class="trace-item">
                <span class="trace-item__round">R{item.round}</span>
                <span class="trace-item__phase {phaseClass(item.phase)}">{phaseLabel(item.phase)}</span>
                <span class="trace-item__arrow">→</span>
                <span class="trace-item__action {actionClass(item.action)}">{item.action}</span>
                {#if item.requiredTotal > 0}
                  <span class="trace-item__meta">({item.requiredTotal})</span>
                {/if}
                {#if item.reason || item.note}
                  <span class="trace-item__note">{item.reason || item.note}</span>
                {/if}
              </div>
            {/each}
          </div>
        {:else}
          <div class="runtime-diagnostics__empty">{i18n.t('runtimeDiagnostics.noTrace')}</div>
        {/if}
      </div>
    </div>
  </details>
{/if}

<style>
  .runtime-diagnostics {
    margin: 8px 12px 0;
    border: 1px solid var(--vscode-editorWidget-border, var(--border));
    border-radius: 8px;
    background: var(--vscode-editorWidget-background, var(--surface-2));
    color: var(--vscode-foreground, var(--foreground));
    overflow: visible;
    border-left: 3px solid var(--vscode-editorWidget-border, var(--border));
    position: relative;
  }

  /* 卡片左边框根据终态着色 */
  .runtime-diagnostics--completed { border-left-color: var(--success); }
  .runtime-diagnostics--failed    { border-left-color: var(--vscode-editorError-foreground, var(--error)); }
  .runtime-diagnostics--cancelled { border-left-color: var(--vscode-editorWidget-border, var(--border)); }
  .runtime-diagnostics--idle      { border-left-color: var(--vscode-editorWidget-border, var(--border)); }
  .runtime-diagnostics--running   { border-left-color: var(--vscode-progressBar-background, var(--info)); }
  .runtime-diagnostics--waiting   { border-left-color: var(--vscode-editorWarning-foreground, var(--warning)); }
  .runtime-diagnostics--paused    { border-left-color: var(--vscode-editorWarning-foreground, var(--warning)); }

  .runtime-diagnostics > summary {
    cursor: pointer;
    padding: 8px 10px;
    font-size: 12px;
    user-select: none;
    display: flex;
    align-items: center;
    gap: 6px;
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
    top: 100%;
    left: -1px;
    right: -1px;
    z-index: 20;
    border: 1px solid var(--vscode-editorWidget-border, var(--border));
    border-top: 1px solid var(--vscode-editorWidget-border, var(--border));
    border-radius: 0 0 8px 8px;
    background: var(--vscode-editorWidget-background, var(--surface-2));
    box-shadow: 0 4px 12px rgba(0, 0, 0, 0.35);
    padding: 8px 10px;
    display: flex;
    flex-direction: column;
    gap: 10px;
    max-height: 440px;
    overflow-y: auto;
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
  }

  .metric-card__header {
    display: flex;
    align-items: center;
    gap: 4px;
    font-size: 11px;
    opacity: 0.8;
  }

  :global(.metric-card__icon) {
    opacity: 0.9;
  }

  .metric-card__title {
    font-size: 11px;
  }

  .metric-card__value {
    font-size: 16px;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }

  .metric-card__value--warn {
    color: var(--vscode-editorWarning-foreground, var(--warning));
  }

  .metric-card__sub {
    font-size: 11px;
    opacity: 0.7;
  }

  .metric-card__sub--ok {
    color: var(--success);
    opacity: 1;
  }

  .metric-card__sub--warn {
    color: var(--vscode-editorWarning-foreground, var(--warning));
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

  .runtime-diagnostics__block {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .runtime-diagnostics__block--failure {
    border: 1px solid color-mix(in srgb, var(--error) 28%, transparent);
    border-radius: 8px;
    padding: 10px 12px;
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
  }

  .runtime-diagnostics__kv-label {
    font-size: 11px;
    opacity: 0.75;
  }

  .runtime-diagnostics__kv-value {
    font-size: 12px;
    font-family: var(--vscode-editor-font-family, monospace);
    word-break: break-all;
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
    word-break: break-word;
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
</style>
