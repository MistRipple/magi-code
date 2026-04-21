<script lang="ts">
  import { getState, requestMessageJump, setCurrentBottomTab, setCurrentTopTab } from '../stores/messages.svelte';
  import { ensureArray } from '../lib/utils';
  import type { ActivePlanState, PlanLedgerRecord, Message } from '../types/message';
  import Icon from './Icon.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import type { TaskStatus, AssignmentLeaseDto, RunnerStatusResponseDto } from '../shared/rust-backend-types';
  import type { IconName } from '../lib/icons';
  import {
    getTaskGraphState,
    getTaskKindLabel,
    getTaskStatusModifier,
    refreshTaskProjection,
  } from '../stores/task-graph-store.svelte';
  import { RustDaemonClient } from '../shared/rust-daemon-client';
  import { resolveAgentBaseUrl } from '../web/agent-api';

  const appState = getState();

  const appPayload = $derived((appState.appState || {}) as Record<string, unknown>);
  const activePlanState = $derived((appPayload.activePlan || null) as ActivePlanState | null);
  const planHistory = $derived(ensureArray(appPayload.planHistory) as PlanLedgerRecord[]);
  const threadMessages = $derived(ensureArray(appState.threadMessages) as Message[]);

  // ─── Task Graph Projection (new Task-based view) ─────────────────
  const taskGraph = getTaskGraphState();
  const hasTaskProjection = $derived(taskGraph.projection !== null);
  const projectionProgress = $derived.by(() => {
    const p = taskGraph.projection?.progress_summary;
    if (!p || p.total_tasks === 0) return null;
    const percent = Math.round((p.completed_tasks / p.total_tasks) * 100);
    return { ...p, percent };
  });
  const workpackageSummaries = $derived(taskGraph.projection?.workpackage_summaries ?? []);

  // Track expanded task graph nodes
  let expandedGraphNodes = $state<Set<string>>(new Set());

  function toggleGraphNode(taskId: string) {
    const next = new Set(expandedGraphNodes);
    if (next.has(taskId)) next.delete(taskId);
    else next.add(taskId);
    expandedGraphNodes = next;
  }

  // Auto-expand running workpackages
  $effect(() => {
    const runningIds = taskGraph.projection?.running_tasks ?? [];
    const wpIds = workpackageSummaries
      .filter(wp => wp.status === 'Running')
      .map(wp => wp.task_id);
    const allRunning = [...runningIds, ...wpIds];
    const needsExpand = allRunning.some(id => !expandedGraphNodes.has(id));
    if (needsExpand) {
      const next = new Set(expandedGraphNodes);
      for (const id of allRunning) next.add(id);
      expandedGraphNodes = next;
    }
  });

  function getProjectionStatusIcon(status: TaskStatus): { name: IconName; spinning: boolean } {
    switch (status) {
      case 'Running': return { name: 'loader', spinning: true };
      case 'Completed': return { name: 'check-circle', spinning: false };
      case 'Failed': return { name: 'x-circle', spinning: false };
      case 'Cancelled': case 'Skipped': return { name: 'skip-forward', spinning: false };
      case 'Blocked': return { name: 'alert-circle', spinning: false };
      case 'AwaitingApproval': return { name: 'shield', spinning: false };
      case 'Verifying': return { name: 'check-circle', spinning: true };
      case 'Repairing': return { name: 'wrench', spinning: true };
      default: return { name: 'circleOutline', spinning: false };
    }
  }

  function formatLeaseRemaining(lease: AssignmentLeaseDto): string {
    const remaining = lease.expires_at - Date.now();
    if (remaining <= 0) return 'expired';
    const seconds = Math.floor(remaining / 1000);
    if (seconds < 60) return `${seconds}s`;
    const minutes = Math.floor(seconds / 60);
    return `${minutes}m${seconds % 60}s`;
  }

  // ─── Runner Controls ─────────────────────────────────────────────
  function createClient(): RustDaemonClient {
    return new RustDaemonClient(resolveAgentBaseUrl());
  }

  let runnerStatus = $state<RunnerStatusResponseDto | null>(null);
  let runnerLoading = $state(false);
  let runnerError = $state<string | null>(null);
  let runnerPollTimer = $state<ReturnType<typeof setInterval> | null>(null);

  const rootTaskId = $derived(taskGraph.rootTaskId);

  async function pollRunnerStatus() {
    if (!rootTaskId) return;
    try {
      const client = createClient();
      runnerStatus = await client.getRunnerStatus(rootTaskId);
      runnerError = null;
    } catch {
      // Non-critical: runner may not exist yet
    }
  }

  async function handleStartRunner() {
    if (!rootTaskId || runnerLoading) return;
    runnerLoading = true;
    runnerError = null;
    try {
      const client = createClient();
      await client.startRunner({ rootTaskId });
      await pollRunnerStatus();
      startRunnerPolling();
    } catch (err) {
      runnerError = err instanceof Error ? err.message : String(err);
    } finally {
      runnerLoading = false;
    }
  }

  async function handleStopRunner() {
    if (!rootTaskId || runnerLoading) return;
    runnerLoading = true;
    runnerError = null;
    try {
      const client = createClient();
      await client.stopRunner({ rootTaskId });
      await pollRunnerStatus();
      stopRunnerPolling();
    } catch (err) {
      runnerError = err instanceof Error ? err.message : String(err);
    } finally {
      runnerLoading = false;
    }
  }

  function startRunnerPolling() {
    stopRunnerPolling();
    runnerPollTimer = setInterval(() => { pollRunnerStatus(); }, 3000);
  }

  function stopRunnerPolling() {
    if (runnerPollTimer !== null) {
      clearInterval(runnerPollTimer);
      runnerPollTimer = null;
    }
  }

  const isRunnerActive = $derived(
    runnerStatus !== null && (runnerStatus.status === 'Running' || runnerStatus.status === 'running'),
  );

  // Auto-poll runner status when root task is set and auto-start polling if runner is running
  $effect(() => {
    if (rootTaskId) {
      pollRunnerStatus().then(() => {
        if (isRunnerActive) {
          startRunnerPolling();
        }
      });
    }
    return () => { stopRunnerPolling(); };
  });

  // ─── Decision Task Approval ──────────────────────────────────────
  let decisionLoading = $state<Set<string>>(new Set());

  async function approveDecision(taskId: string) {
    const next = new Set(decisionLoading);
    next.add(taskId);
    decisionLoading = next;
    try {
      const client = createClient();
      await client.updateTaskStatus(taskId, 'Completed');
      await refreshTaskProjection();
    } catch (err) {
      console.error('Failed to approve decision:', err);
    } finally {
      const after = new Set(decisionLoading);
      after.delete(taskId);
      decisionLoading = after;
    }
  }

  async function rejectDecision(taskId: string) {
    const next = new Set(decisionLoading);
    next.add(taskId);
    decisionLoading = next;
    try {
      const client = createClient();
      await client.updateTaskStatus(taskId, 'Failed');
      await refreshTaskProjection();
    } catch (err) {
      console.error('Failed to reject decision:', err);
    } finally {
      const after = new Set(decisionLoading);
      after.delete(taskId);
      decisionLoading = after;
    }
  }

  // ─── Pending decisions from projection ───────────────────────────
  // pending_decisions is an array of task IDs; we need to identify Decision+Ready tasks
  // The projection root_task has kind info, but pending_decisions are just IDs.
  // We display them in the attention section with approve/reject buttons.

  let showPlanLedger = $state(true);

  const activePlanRecord = $derived.by(() => {
    const activePlanId = activePlanState?.planId;
    if (!activePlanId) {
      return null;
    }
    return planHistory.find((plan) => plan?.planId === activePlanId) || null;
  });

  const archivedPlans = $derived.by(() => {
    const activePlanId = activePlanState?.planId;
    return planHistory
      .filter((plan) => !!plan && plan.planId !== activePlanId)
      .slice(0, 6);
  });

  const activePlanProgress = $derived.by(() => {
    const plan = activePlanRecord;
    if (!plan || !Array.isArray(plan.items) || plan.items.length === 0) {
      return { total: 0, completed: 0, percent: 0 };
    }
    const total = plan.items.length;
    const completed = plan.items.filter((item) => item.status === 'completed' || item.status === 'skipped').length;
    return {
      total,
      completed,
      percent: Math.round((completed / total) * 100),
    };
  });

  function getPlanStatusLabel(status: string): string {
    // 后端 status 使用 snake_case（如 awaiting_confirmation），i18n key 使用 camelCase（如 awaitingConfirmation）
    const camelStatus = status.replace(/_([a-z])/g, (_, c) => c.toUpperCase());
    const key = `tasks.planStatus.${camelStatus}`;
    const label = i18n.t(key);
    // 如果 key 没有匹配到翻译（返回原 key），则使用 status 或 '未知'
    return label !== key ? label : (status || i18n.t('tasks.planStatus.unknown'));
  }

  function getPlanStatusClass(status: string): string {
    if (status === 'completed') return 'is-completed';
    if (status === 'failed' || status === 'rejected') return 'is-failed';
    if (status === 'executing') return 'is-running';
    if (status === 'partially_completed') return 'is-partial';
    if (status === 'cancelled' || status === 'superseded') return 'is-cancelled';
    return 'is-pending';
  }

  function formatTimestamp(timestamp?: number): string {
    if (typeof timestamp !== 'number' || !Number.isFinite(timestamp) || timestamp <= 0) {
      return '--';
    }
    const date = new Date(timestamp);
    const hours = String(date.getHours()).padStart(2, '0');
    const minutes = String(date.getMinutes()).padStart(2, '0');
    return `${hours}:${minutes}`;
  }

  function normalizeAnchorText(raw: unknown): string {
    if (typeof raw !== 'string') {
      return '';
    }
    return raw.replace(/\s+/g, ' ').trim();
  }

  function extractUserInputText(message: Message): string {
    const content = normalizeAnchorText(message.content);
    if (content) {
      return content;
    }

    const blocks = Array.isArray(message.blocks) ? message.blocks : [];
    const text = blocks
      .filter((block) => block?.type === 'text' || block?.type === 'thinking')
      .map((block) => (typeof block?.content === 'string' ? block.content : ''))
      .join(' ');
    return normalizeAnchorText(text);
  }

  function isPrimaryUserInput(message: Message): boolean {
    if (message.type !== 'user_input') {
      return false;
    }
    return message?.metadata?.isSupplementary !== true;
  }

  function getTemporalAnchorScore(messageTimestamp: number, anchorTimestamp: number): number {
    const delta = anchorTimestamp - messageTimestamp;
    const isFuture = delta < -2000;
    return Math.abs(delta) + (isFuture ? 200000 : 0);
  }

  function matchUserInputByPromptDigest(messages: Message[], plan: PlanLedgerRecord): Message | null {
    const normalizedDigest = normalizeAnchorText(plan.promptDigest);
    if (!normalizedDigest || normalizedDigest === 'empty') {
      return null;
    }

    const hasEllipsis = normalizedDigest.endsWith('...');
    const digestPrefix = hasEllipsis ? normalizeAnchorText(normalizedDigest.slice(0, -3)) : normalizedDigest;
    if (!digestPrefix) {
      return null;
    }

    const anchorTs = Number.isFinite(plan.createdAt) ? plan.createdAt : Date.now();
    let bestMatch: Message | null = null;
    let bestScore = Number.POSITIVE_INFINITY;

    for (const message of messages) {
      const text = extractUserInputText(message);
      if (!text) {
        continue;
      }

      const exact = text === digestPrefix;
      const prefix = text.startsWith(digestPrefix);
      const include = !hasEllipsis && text.includes(digestPrefix);
      if (!exact && !prefix && !include) {
        continue;
      }

      const textScore = exact ? 0 : prefix ? 1 : 2;
      const score = textScore * 100000 + getTemporalAnchorScore(message.timestamp, anchorTs);
      if (score < bestScore) {
        bestScore = score;
        bestMatch = message;
      }
    }

    return bestMatch;
  }

  function matchUserInputByTimestamp(messages: Message[], anchorTimestamp: number): Message | null {
    let bestMatch: Message | null = null;
    let bestScore = Number.POSITIVE_INFINITY;

    for (const message of messages) {
      const score = getTemporalAnchorScore(message.timestamp, anchorTimestamp);
      if (score < bestScore) {
        bestScore = score;
        bestMatch = message;
      }
    }

    return bestMatch;
  }

  function resolvePlanAnchorMessageId(plan: PlanLedgerRecord): string | null {
    const userInputs = threadMessages.filter((message) => isPrimaryUserInput(message) && Number.isFinite(message.timestamp));
    if (userInputs.length === 0) {
      return null;
    }

    const anchorTs = Number.isFinite(plan.createdAt) ? plan.createdAt : Date.now();
    const normalizedTurnId = typeof plan.turnId === 'string' ? plan.turnId.trim() : '';
    if (normalizedTurnId) {
      const byTurn = userInputs.filter((message) => {
        const metadataTurnId = typeof message?.metadata?.turnId === 'string'
          ? message.metadata.turnId.trim()
          : '';
        return metadataTurnId === normalizedTurnId && message.type === 'user_input';
      });
      if (byTurn.length > 0) {
        const digestMatch = matchUserInputByPromptDigest(byTurn, plan);
        if (digestMatch?.id) {
          return digestMatch.id;
        }
        const byTurnTime = matchUserInputByTimestamp(byTurn, anchorTs);
        if (byTurnTime?.id) {
          return byTurnTime.id;
        }
      }
    }

    const digestMatch = matchUserInputByPromptDigest(userInputs, plan);
    if (digestMatch?.id) {
      return digestMatch.id;
    }

    return matchUserInputByTimestamp(userInputs, anchorTs)?.id || null;
  }

  function jumpToPlanConversation(plan: PlanLedgerRecord): void {
    setCurrentTopTab('thread');
    setCurrentBottomTab('thread');
    const anchorMessageId = resolvePlanAnchorMessageId(plan);
    if (!anchorMessageId) {
      return;
    }
    requestMessageJump(anchorMessageId);
  }
</script>

<div class="tasks-panel">
  <!-- ═══ Runner Controls Bar ═══ -->
  {#if hasTaskProjection && rootTaskId}
    <div class="runner-bar">
      <div class="runner-bar-left">
        <span class="runner-label">Runner</span>
        {#if runnerStatus}
          <span class="runner-status runner-status--{runnerStatus.status?.toLowerCase() ?? 'unknown'}">
            {runnerStatus.status}
          </span>
          {#if runnerStatus.cycleCount > 0}
            <span class="runner-cycles">Cycle {runnerStatus.cycleCount}</span>
          {/if}
        {:else}
          <span class="runner-status runner-status--stopped">Stopped</span>
        {/if}
        {#if runnerError}
          <span class="runner-error-text" title={runnerError}>Error</span>
        {/if}
      </div>
      <div class="runner-bar-right">
        {#if isRunnerActive}
          <button
            class="btn-icon btn-icon--xs btn-icon--error"
            title="Stop Runner"
            disabled={runnerLoading}
            onclick={() => handleStopRunner()}
          >
            <Icon name="stop" size={12} />
          </button>
        {:else}
          <button
            class="btn-icon btn-icon--xs"
            title="Start Runner"
            disabled={runnerLoading}
            onclick={() => handleStartRunner()}
          >
            <Icon name="play" size={12} />
          </button>
        {/if}
        {#if runnerLoading}
          <Icon name="loader" size={14} class="spinning" />
        {/if}
      </div>
    </div>
  {/if}

  {#if hasTaskProjection}
    <!-- ═══ Task Projection View (new Task Graph) ═══ -->
    {@const proj = taskGraph.projection}
    {#if proj}
      <!-- Projection overview bar -->
      <div class="tg-overview-bar">
        <div class="tg-overview-header">
          <span class="tg-overview-title">{proj.root_task.title}</span>
          <span class="tg-status-badge tg-status--{getTaskStatusModifier(proj.aggregate_status)}">
            {proj.display_status || proj.aggregate_status}
          </span>
        </div>
        {#if proj.current_phase}
          <span class="tg-phase-label">{proj.current_phase}</span>
        {/if}
        {#if projectionProgress}
          <div class="tg-progress-wrap">
            <span class="tg-progress-label">{projectionProgress.completed_tasks}/{projectionProgress.total_tasks}</span>
            <div class="tg-progress-bar">
              <div class="tg-progress-fill" style="width: {projectionProgress.percent}%"></div>
            </div>
          </div>
          <div class="tg-progress-stats">
            {#if projectionProgress.running_tasks > 0}
              <span class="tg-stat tg-stat--running">{projectionProgress.running_tasks} running</span>
            {/if}
            {#if projectionProgress.failed_tasks > 0}
              <span class="tg-stat tg-stat--failed">{projectionProgress.failed_tasks} failed</span>
            {/if}
            {#if projectionProgress.blocked_tasks > 0}
              <span class="tg-stat tg-stat--blocked">{projectionProgress.blocked_tasks} blocked</span>
            {/if}
          </div>
        {/if}
        {#if proj.validation_summary}
          <div class="tg-validation-summary">{proj.validation_summary}</div>
        {/if}
      </div>

      <!-- Work Package list -->
      {#if workpackageSummaries.length > 0}
        <div class="tg-wp-list">
          {#each workpackageSummaries as wp (wp.task_id)}
            {@const isWpExpanded = expandedGraphNodes.has(wp.task_id)}
            {@const statusIcon = getProjectionStatusIcon(wp.status)}
            {@const lease = taskGraph.leases.get(wp.task_id)}
            <div class="tg-wp-card tg-wp--{getTaskStatusModifier(wp.status)}">
              <div
                class="tg-wp-header"
                role="button"
                tabindex="0"
                onclick={() => toggleGraphNode(wp.task_id)}
                onkeydown={(e) => e.key === 'Enter' && toggleGraphNode(wp.task_id)}
              >
                <span class="tg-wp-chevron" class:expanded={isWpExpanded}>
                  <Icon name="chevron-right" size={12} />
                </span>
                <span class="tg-kind-badge">{getTaskKindLabel('WorkPackage')}</span>
                <span class="tg-wp-status-icon tg-status-icon--{getTaskStatusModifier(wp.status)}">
                  {#if statusIcon.spinning}
                    <Icon name={statusIcon.name} size={14} class="spinning" />
                  {:else}
                    <Icon name={statusIcon.name} size={14} />
                  {/if}
                </span>
                <span class="tg-wp-title">{wp.title}</span>
                <span class="tg-wp-count">{wp.completed_children}/{wp.total_children}</span>
                {#if lease}
                  <span class="tg-lease-badge" title="Worker: {lease.worker_id}, Role: {lease.role}">
                    {lease.worker_id} ({formatLeaseRemaining(lease)})
                  </span>
                {/if}
              </div>
              {#if wp.total_children > 0}
                <div class="tg-wp-progress-bar">
                  <div class="tg-wp-progress-fill" style="width: {wp.total_children > 0 ? Math.round((wp.completed_children / wp.total_children) * 100) : 0}%"></div>
                </div>
              {/if}
            </div>
          {/each}
        </div>
      {/if}

      <!-- Blocked / pending decisions -->
      {#if (proj.blocked_tasks?.length ?? 0) > 0 || (proj.pending_decisions?.length ?? 0) > 0}
        <div class="tg-attention-section">
          {#if (proj.blocked_tasks?.length ?? 0) > 0}
            <div class="tg-attention-item tg-attention--blocked">
              <Icon name="alert-circle" size={12} />
              <span>{proj.blocked_tasks.length} blocked task{proj.blocked_tasks.length > 1 ? 's' : ''}</span>
            </div>
          {/if}
          {#if (proj.pending_decisions?.length ?? 0) > 0}
            {#each proj.pending_decisions as decisionId (decisionId)}
              {@const isDecLoading = decisionLoading.has(decisionId)}
              <div class="tg-attention-item tg-attention--decision">
                <Icon name="alert-circle" size={12} />
                <span class="tg-decision-id">Decision: {decisionId.slice(0, 8)}...</span>
                <div class="tg-decision-actions">
                  <button
                    class="tg-decision-btn tg-decision-btn--approve"
                    title="Approve decision"
                    disabled={isDecLoading}
                    onclick={() => approveDecision(decisionId)}
                  >
                    {#if isDecLoading}
                      <Icon name="loader" size={12} class="spinning" />
                    {:else}
                      <Icon name="check" size={12} />
                    {/if}
                    Approve
                  </button>
                  <button
                    class="tg-decision-btn tg-decision-btn--reject"
                    title="Reject decision"
                    disabled={isDecLoading}
                    onclick={() => rejectDecision(decisionId)}
                  >
                    <Icon name="x-circle" size={12} />
                    Reject
                  </button>
                </div>
              </div>
            {/each}
          {/if}
        </div>
      {/if}

      {#if taskGraph.error}
        <div class="tg-error">{taskGraph.error}</div>
      {/if}
    {/if}
  {/if}

  {#if activePlanState || archivedPlans.length > 0}
    <div class="plan-ledger-card">
      <button
        type="button"
        class="plan-ledger-toggle"
        aria-expanded={showPlanLedger}
        onclick={() => showPlanLedger = !showPlanLedger}
      >
        <span class="plan-ledger-title-wrap">
          <span class="plan-ledger-title">{i18n.t('tasks.planLedger.title')}</span>
          {#if activePlanState}
            <span class="plan-ledger-badge">{i18n.t('tasks.planLedger.currentPlan')}</span>
          {:else}
            <span class="plan-ledger-count">{i18n.t('tasks.planLedger.historyCount', { count: archivedPlans.length })}</span>
          {/if}
        </span>
        <span class="plan-ledger-chevron" class:expanded={showPlanLedger}>
          <Icon name="chevron-right" size={12} />
        </span>
      </button>

      {#if showPlanLedger}
        {#if activePlanState}
          <div class="plan-ledger-current">
            <div class="plan-ledger-summary">
              <span>{activePlanRecord?.summary || i18n.t('tasks.planLedger.executingFallback')}</span>
              {#if activePlanRecord}
                <span class="plan-status {getPlanStatusClass(activePlanRecord.status)}">
                  {getPlanStatusLabel(activePlanRecord.status)}
                </span>
              {/if}
            </div>
            <div class="plan-ledger-meta">
              {#if activePlanRecord}
                <span>{i18n.t('tasks.planLedger.modeLabel', { mode: activePlanRecord.mode === 'deep' ? i18n.t('tasks.planLedger.modeDeep') : i18n.t('tasks.planLedger.modeShallow') })}</span>
                <span>{i18n.t('tasks.planLedger.versionLabel', { version: activePlanRecord.version })}</span>
                <span>{i18n.t('tasks.planLedger.updatedLabel', { time: formatTimestamp(activePlanRecord.updatedAt) })}</span>
              {:else}
                <span>{i18n.t('tasks.planLedger.updatedLabel', { time: formatTimestamp(activePlanState.updatedAt) })}</span>
              {/if}
            </div>
            {#if activePlanProgress.total > 0}
              <div class="plan-ledger-progress-wrap">
                <span class="plan-ledger-progress-label">{activePlanProgress.completed}/{activePlanProgress.total}</span>
                <div class="plan-ledger-progress">
                  <div class="plan-ledger-progress-fill" style="width: {activePlanProgress.percent}%"></div>
                </div>
              </div>
            {/if}
          </div>
        {/if}

        {#if archivedPlans.length > 0}
          <div class="plan-history-list">
            {#each archivedPlans as plan (plan.planId)}
              <button
                type="button"
                class="plan-history-item clickable"
                title={i18n.t('tasks.planLedger.jumpTitle')}
                onclick={() => jumpToPlanConversation(plan)}
              >
                <div class="plan-history-main">
                  <span class="plan-history-summary">{plan.summary || i18n.t('tasks.planLedger.unnamedPlan')}</span>
                  <span class="plan-status {getPlanStatusClass(plan.status)}">
                    {getPlanStatusLabel(plan.status)}
                  </span>
                </div>
                <div class="plan-history-meta">
                  <span>{plan.mode === 'deep' ? i18n.t('tasks.planLedger.modeDeep') : i18n.t('tasks.planLedger.modeShallow')}</span>
                  <span>v{plan.version}</span>
                  <span>{formatTimestamp(plan.updatedAt)}</span>
                </div>
              </button>
            {/each}
          </div>
        {/if}
      {/if}
    </div>
  {/if}

  {#if !hasTaskProjection}
    <div class="empty-state">
      <div class="empty-icon-wrap">
        <Icon name="circleOutline" size={32} class="empty-icon" />
      </div>
      <div class="empty-text">{i18n.t('tasks.empty.title')}</div>
      {#if activePlanState || archivedPlans.length > 0}
        <div class="empty-hint">{i18n.t('tasks.empty.hintWithPlan')}</div>
      {:else}
        <div class="empty-hint">{i18n.t('tasks.empty.hintNoPlan')}</div>
      {/if}
    </div>
  {/if}
</div>

<style>
  /* ========== 面板容器 ========== */
  .tasks-panel {
    height: 100%;
    min-height: 0; /* flex 布局防溢出 */
    overflow-y: auto;
    padding: var(--space-4);
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }

  .plan-ledger-card {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    padding: var(--space-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--surface-1);
  }

  .plan-ledger-toggle {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
    width: 100%;
    border: none;
    background: transparent;
    color: inherit;
    padding: var(--space-1) var(--space-2);
    border-radius: var(--radius-sm);
    cursor: pointer;
    text-align: left;
  }

  .plan-ledger-toggle:hover {
    background: var(--surface-hover);
  }

  .plan-ledger-title-wrap {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
  }

  .plan-ledger-title {
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
  }

  .plan-ledger-badge {
    font-size: var(--text-2xs);
    color: var(--primary);
    background: var(--primary-muted);
    border: 1px solid color-mix(in srgb, var(--primary) 30%, var(--border));
    border-radius: 999px;
    padding: 2px 8px;
  }

  .plan-ledger-count {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  .plan-ledger-chevron {
    display: inline-flex;
    align-items: center;
    color: var(--foreground-muted);
    transition: transform var(--transition-fast);
  }

  .plan-ledger-chevron.expanded {
    transform: rotate(90deg);
  }

  .plan-ledger-current {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    padding: var(--space-1) var(--space-2) var(--space-2);
  }

  .plan-ledger-summary {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
    font-size: var(--text-sm);
    color: var(--foreground);
  }

  .plan-ledger-meta {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  .plan-status {
    font-size: var(--text-xs);
    border-radius: 999px;
    padding: 2px 8px;
    border: 1px solid transparent;
    white-space: nowrap;
  }

  .plan-status.is-running {
    color: var(--primary);
    background: var(--primary-muted);
    border-color: color-mix(in srgb, var(--primary) 30%, var(--border));
  }

  .plan-status.is-completed {
    color: var(--success);
    background: var(--success-muted);
    border-color: color-mix(in srgb, var(--success) 32%, var(--border));
  }

  .plan-status.is-failed {
    color: var(--error);
    background: var(--error-muted);
    border-color: color-mix(in srgb, var(--error) 32%, var(--border));
  }

  .plan-status.is-partial {
    color: var(--warning);
    background: var(--warning-muted);
    border-color: color-mix(in srgb, var(--warning) 30%, var(--border));
  }

  .plan-status.is-cancelled {
    color: var(--foreground-muted);
    background: var(--surface-2);
    border-color: var(--border);
  }

  .plan-status.is-pending {
    color: var(--foreground-muted);
    background: var(--surface-2);
    border-color: var(--border);
  }

  .plan-ledger-progress-wrap {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }

  .plan-ledger-progress-label {
    min-width: 50px;
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  .plan-ledger-progress {
    flex: 1;
    height: 6px;
    border-radius: 999px;
    background: var(--surface-3);
    overflow: hidden;
  }

  .plan-ledger-progress-fill {
    height: 100%;
    border-radius: inherit;
    background: var(--primary);
    transition: width 200ms ease;
  }

  .plan-history-list {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    font-size: var(--text-xs);
  }

  .plan-history-item {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: var(--space-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--surface-2);
  }

  .plan-history-item.clickable {
    width: 100%;
    text-align: left;
    color: inherit;
    cursor: pointer;
    transition: background var(--transition-fast), border-color var(--transition-fast);
  }

  .plan-history-item.clickable:hover {
    background: var(--surface-hover);
    border-color: color-mix(in srgb, var(--primary) 28%, var(--border));
  }

  .plan-history-main {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
  }

  .plan-history-summary {
    font-size: var(--text-sm);
    color: var(--foreground);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .plan-history-meta {
    display: flex;
    gap: var(--space-2);
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  /* ========== 空状态 ========== */
  .empty-state {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    padding: var(--space-8) var(--space-5);
    color: var(--foreground-muted);
    text-align: center;
    width: 100%;
    box-sizing: border-box;
  }

  .empty-icon-wrap {
    opacity: 0.2;
    margin-bottom: var(--space-4);
  }

  .empty-text {
    font-size: var(--text-base);
    font-weight: var(--font-medium);
    color: var(--foreground);
    margin-bottom: var(--space-2);
  }

  .empty-hint {
    font-size: var(--text-sm);
    opacity: 0.6;
  }

  :global(.spinning) {
    animation: spin 1s linear infinite;
  }

  @keyframes spin {
    from { transform: rotate(0deg); }
    to { transform: rotate(360deg); }
  }

  /* ========== Runner Controls Bar ========== */
  .runner-bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
    padding: var(--space-2) var(--space-3);
    background: var(--surface-1);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
  }

  .runner-bar-left {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
  }

  .runner-bar-right {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    flex-shrink: 0;
  }

  .runner-label {
    font-size: var(--text-xs);
    font-weight: var(--font-medium);
    color: var(--foreground-muted);
  }

  .runner-status {
    font-size: var(--text-2xs);
    border-radius: 999px;
    padding: 2px 8px;
    border: 1px solid transparent;
    white-space: nowrap;
  }

  .runner-status--running {
    color: var(--primary);
    background: var(--primary-muted);
    border-color: color-mix(in srgb, var(--primary) 30%, var(--border));
  }

  .runner-status--stopped {
    color: var(--foreground-muted);
    background: var(--surface-2);
    border-color: var(--border);
  }

  .runner-status--completed {
    color: var(--success);
    background: var(--success-muted);
    border-color: color-mix(in srgb, var(--success) 32%, var(--border));
  }

  .runner-status--error,
  .runner-status--failed {
    color: var(--error);
    background: var(--error-muted);
    border-color: color-mix(in srgb, var(--error) 32%, var(--border));
  }

  .runner-status--unknown {
    color: var(--foreground-muted);
    background: var(--surface-2);
    border-color: var(--border);
  }

  .runner-cycles {
    font-size: var(--text-2xs);
    color: var(--foreground-muted);
    font-variant-numeric: tabular-nums;
  }

  .runner-error-text {
    font-size: var(--text-2xs);
    color: var(--error);
  }

  /* ========== Task Graph Overview ========== */
  .tg-overview-bar {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    padding: var(--space-3) var(--space-4);
    background: var(--surface-1);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
  }

  .tg-overview-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
  }

  .tg-overview-title {
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    color: var(--foreground);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .tg-status-badge {
    font-size: var(--text-2xs);
    border-radius: 999px;
    padding: 2px 8px;
    border: 1px solid transparent;
    white-space: nowrap;
    flex-shrink: 0;
  }

  .tg-status--running {
    color: var(--primary);
    background: var(--primary-muted);
    border-color: color-mix(in srgb, var(--primary) 30%, var(--border));
  }

  .tg-status--completed {
    color: var(--success);
    background: var(--success-muted);
    border-color: color-mix(in srgb, var(--success) 32%, var(--border));
  }

  .tg-status--failed {
    color: var(--error);
    background: var(--error-muted);
    border-color: color-mix(in srgb, var(--error) 32%, var(--border));
  }

  .tg-status--blocked {
    color: var(--warning);
    background: var(--warning-muted);
    border-color: color-mix(in srgb, var(--warning) 30%, var(--border));
  }

  .tg-status--cancelled,
  .tg-status--skipped {
    color: var(--foreground-muted);
    background: var(--surface-2);
    border-color: var(--border);
  }

  .tg-status--draft,
  .tg-status--ready,
  .tg-status--unknown {
    color: var(--foreground-muted);
    background: var(--surface-2);
    border-color: var(--border);
  }

  .tg-status--awaiting {
    color: var(--warning);
    background: var(--warning-muted);
    border-color: color-mix(in srgb, var(--warning) 30%, var(--border));
  }

  .tg-status--verifying {
    color: var(--primary);
    background: var(--primary-muted);
    border-color: color-mix(in srgb, var(--primary) 30%, var(--border));
  }

  .tg-status--repairing {
    color: var(--warning);
    background: var(--warning-muted);
    border-color: color-mix(in srgb, var(--warning) 30%, var(--border));
  }

  .tg-phase-label {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    font-style: italic;
  }

  .tg-progress-wrap {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }

  .tg-progress-label {
    min-width: 50px;
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    font-variant-numeric: tabular-nums;
  }

  .tg-progress-bar {
    flex: 1;
    height: 6px;
    border-radius: 999px;
    background: var(--surface-3);
    overflow: hidden;
  }

  .tg-progress-fill {
    height: 100%;
    border-radius: inherit;
    background: var(--primary);
    transition: width 200ms ease;
  }

  .tg-progress-stats {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
    font-size: var(--text-xs);
  }

  .tg-stat {
    display: inline-flex;
    align-items: center;
    gap: var(--space-1);
  }

  .tg-stat--running { color: var(--primary); }
  .tg-stat--failed { color: var(--error); }
  .tg-stat--blocked { color: var(--warning); }

  .tg-validation-summary {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    padding: var(--space-1) 0;
  }

  /* ========== Work Package List ========== */
  .tg-wp-list {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  .tg-wp-card {
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--surface-1);
    overflow: hidden;
    transition: border-color var(--transition-fast);
  }

  .tg-wp-card:hover {
    border-color: color-mix(in srgb, var(--foreground) 20%, var(--border));
  }

  .tg-wp--running {
    border-color: color-mix(in srgb, var(--primary) 40%, var(--border));
  }

  .tg-wp--completed {
    opacity: 0.6;
  }

  .tg-wp--failed {
    border-color: color-mix(in srgb, var(--error) 30%, var(--border));
  }

  .tg-wp-header {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    cursor: pointer;
    user-select: none;
  }

  .tg-wp-header:hover {
    background: var(--surface-hover);
  }

  .tg-wp-chevron {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 16px;
    height: 16px;
    flex-shrink: 0;
    color: var(--foreground-muted);
    transition: transform var(--transition-fast);
  }

  .tg-wp-chevron.expanded {
    transform: rotate(90deg);
  }

  .tg-kind-badge {
    font-size: var(--text-2xs);
    font-weight: var(--font-medium);
    color: var(--foreground-muted);
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: 1px 5px;
    flex-shrink: 0;
    letter-spacing: 0.03em;
  }

  .tg-wp-status-icon {
    display: flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
    width: 18px;
    height: 18px;
  }

  .tg-status-icon--running { color: var(--primary); }
  .tg-status-icon--completed { color: var(--success); }
  .tg-status-icon--failed { color: var(--error); }
  .tg-status-icon--blocked { color: var(--warning); }
  .tg-status-icon--cancelled,
  .tg-status-icon--skipped { color: var(--foreground-muted); }
  .tg-status-icon--draft,
  .tg-status-icon--ready { color: var(--foreground-muted); }
  .tg-status-icon--awaiting { color: var(--warning); }
  .tg-status-icon--verifying { color: var(--primary); }
  .tg-status-icon--repairing { color: var(--warning); }
  .tg-status-icon--unknown { color: var(--foreground-muted); }

  .tg-wp-title {
    flex: 1;
    min-width: 0;
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    color: var(--foreground);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .tg-wp-count {
    font-size: var(--text-2xs);
    color: var(--foreground-muted);
    padding: 1px 5px;
    background: var(--surface-3);
    border-radius: var(--radius-full);
    flex-shrink: 0;
    font-variant-numeric: tabular-nums;
  }

  .tg-lease-badge {
    font-size: var(--text-2xs);
    color: var(--primary);
    background: var(--primary-muted);
    border: 1px solid color-mix(in srgb, var(--primary) 30%, var(--border));
    border-radius: 999px;
    padding: 1px 6px;
    flex-shrink: 0;
    white-space: nowrap;
  }

  .tg-wp-progress-bar {
    height: 2px;
    background: var(--surface-2);
  }

  .tg-wp-progress-fill {
    height: 100%;
    background: var(--primary);
    transition: width 200ms ease;
  }

  /* ========== Attention Section (blocked / decisions) ========== */
  .tg-attention-section {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  .tg-attention-item {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius-md);
    font-size: var(--text-xs);
  }

  .tg-attention--blocked {
    color: var(--warning);
    background: var(--warning-muted);
    border: 1px solid color-mix(in srgb, var(--warning) 30%, var(--border));
  }

  .tg-attention--decision {
    color: var(--primary);
    background: var(--primary-muted);
    border: 1px solid color-mix(in srgb, var(--primary) 30%, var(--border));
  }

  .tg-decision-id {
    flex: 1;
    min-width: 0;
    font-family: var(--font-mono, monospace);
    font-size: var(--text-2xs);
  }

  .tg-decision-actions {
    display: flex;
    align-items: center;
    gap: var(--space-1);
    flex-shrink: 0;
  }

  .tg-decision-btn {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: var(--text-2xs);
    padding: 2px 8px;
    border-radius: var(--radius-sm);
    border: 1px solid transparent;
    cursor: pointer;
    white-space: nowrap;
    transition: background var(--transition-fast), border-color var(--transition-fast);
  }

  .tg-decision-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .tg-decision-btn--approve {
    color: var(--success);
    background: var(--success-muted);
    border-color: color-mix(in srgb, var(--success) 32%, var(--border));
  }

  .tg-decision-btn--approve:hover:not(:disabled) {
    background: color-mix(in srgb, var(--success) 20%, var(--surface-1));
  }

  .tg-decision-btn--reject {
    color: var(--error);
    background: var(--error-muted);
    border-color: color-mix(in srgb, var(--error) 32%, var(--border));
  }

  .tg-decision-btn--reject:hover:not(:disabled) {
    background: color-mix(in srgb, var(--error) 20%, var(--surface-1));
  }

  .tg-error {
    font-size: var(--text-xs);
    color: var(--error);
    padding: var(--space-2) var(--space-3);
    background: var(--error-muted);
    border: 1px solid color-mix(in srgb, var(--error) 32%, var(--border));
    border-radius: var(--radius-md);
  }

</style>
