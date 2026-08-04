<script lang="ts">
  import {
    addToast,
    messagesState,
  } from '../stores/messages.svelte';
  import Icon from './Icon.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import type {
    PlanItemDto,
    SessionPlanDto,
    SessionGoalDto,
  } from '../shared/rust-backend-types';
  import type { IconName } from '../lib/icons';
  import {
    applyCurrentGoalResponse,
    ensureGoalState,
    getGoalState,
    refreshCurrentGoal,
  } from '../stores/goal-store.svelte';
  import { RustDaemonClient } from '../shared/rust-daemon-client';
  import { resolveAgentBaseUrl } from '../web/agent-api';
  import { readStoredAccessProfile } from '../shared/access-profile';

  const currentSessionId = $derived(messagesState.currentSessionId);
  const currentWorkspaceId = $derived(messagesState.currentWorkspaceId);
  const currentWorkspacePath = $derived(messagesState.currentWorkspacePath);

  let goalRequestScope = '';
  let goalDrawerExpanded = $state(false);
  let planDrawerExpanded = $state(false);
  let observedPlanId = '';
  let observedActivePlanItemId = '';
  let isEditingGoal = $state(false);
  let goalObjectiveDraft = $state('');
  let goalBudgetDraft = $state('');
  let observedBudgetGoalRevision = '';
  let goalActionLoading = $state<'save' | 'pause' | 'resume' | 'clear' | null>(null);
  let planClearLoading = $state(false);
  let goalClockNow = $state(Date.now());
  let goalClockObservedAt = $state(Date.now());

  $effect(() => {
    ensureGoalState(currentSessionId, currentWorkspaceId, currentWorkspacePathValue());
  });

  const goalState = $derived(getGoalState(currentSessionId, currentWorkspaceId));
  const currentGoal = $derived<SessionGoalDto | null>(goalState.response?.goal ?? null);
  const currentPlan = $derived<SessionPlanDto | null>(goalState.response?.plan ?? null);
  const allowedGoalActions = $derived(goalState.response?.allowedActions ?? null);
  const currentGoalTimeSeconds = $derived.by(() => {
    if (!currentGoal) return 0;
    const settledMillis = typeof currentGoal.timeUsedMillis === 'number'
      && Number.isFinite(currentGoal.timeUsedMillis)
      ? Math.max(0, currentGoal.timeUsedMillis)
      : Math.max(0, currentGoal.timeUsedSeconds) * 1000;
    const timingStartedAt = currentGoal.timingStartedAt;
    const serverObservedAt = goalState.response?.observedAt;
    const runningMillis = typeof timingStartedAt === 'number'
      && Number.isFinite(timingStartedAt)
      && timingStartedAt > 0
      && typeof serverObservedAt === 'number'
      && Number.isFinite(serverObservedAt)
      ? Math.max(0, serverObservedAt - timingStartedAt)
        + Math.max(0, goalClockNow - goalClockObservedAt)
      : 0;
    return Math.floor((settledMillis + runningMillis) / 1000);
  });
  const currentPlanItems = $derived<PlanItemDto[]>(
    Array.isArray(currentPlan?.items) ? currentPlan.items : []
  );

  $effect(() => {
    const timingStartedAt = currentGoal?.timingStartedAt;
    const serverObservedAt = goalState.response?.observedAt;
    const localObservedAt = Date.now();
    goalClockNow = localObservedAt;
    goalClockObservedAt = localObservedAt;
    if (
      typeof timingStartedAt !== 'number'
      || !Number.isFinite(timingStartedAt)
      || timingStartedAt <= 0
      || typeof serverObservedAt !== 'number'
      || !Number.isFinite(serverObservedAt)
    ) {
      return;
    }
    const timer = window.setInterval(() => {
      goalClockNow = Date.now();
    }, 1000);
    return () => window.clearInterval(timer);
  });

  $effect(() => {
    if (!isEditingGoal) {
      goalObjectiveDraft = currentGoal?.objective ?? '';
    }
    if (currentGoal?.status === 'budget_limited') {
      const revisionKey = `${currentGoal.goalId}:${currentGoal.controlRevision}`;
      if (observedBudgetGoalRevision !== revisionKey) {
        observedBudgetGoalRevision = revisionKey;
        goalBudgetDraft = '';
      }
    } else {
      observedBudgetGoalRevision = '';
      goalBudgetDraft = '';
    }
  });

  $effect(() => {
    const sessionId = currentSessionIdValue();
    const workspaceId = currentWorkspaceIdValue();
    const workspacePath = currentWorkspacePathValue();
    const scope = sessionId ? `${sessionScopeKey(workspaceId, sessionId)}:${workspacePath}` : '';
    if (goalRequestScope === scope) {
      return;
    }
    goalRequestScope = scope;
    ensureGoalState(sessionId, workspaceId, workspacePath);
    void refreshCurrentGoal(sessionId, workspaceId, workspacePath);
  });

  const hasCurrentPlan = $derived(currentPlanItems.length > 0);
  const currentPlanPaused = $derived(currentPlan?.state === 'paused');
  const planSummary = $derived.by(() => buildPlanSummary(currentPlanItems));
  const currentPlanBlocked = $derived(planSummary.blocked > 0);
  const planProgressPercent = $derived.by(() => {
    if (planSummary.total <= 0) return 0;
    return Math.min(100, Math.max(0, Math.round((planSummary.completed / planSummary.total) * 100)));
  });
  const planFinished = $derived(
    planSummary.total > 0
    && (
      planSummary.completed === planSummary.total
      || currentPlan?.state === 'completed'
      || currentPlan?.state === 'canceled'
      || currentGoal?.status === 'complete'
    ),
  );
  $effect(() => {
    const planId = currentPlan?.planId ?? '';
    const activeItemId = currentPlanItems.find((item) => item.status === 'in_progress')?.itemId ?? '';
    if (!planId) {
      observedPlanId = '';
      observedActivePlanItemId = '';
      planDrawerExpanded = false;
      return;
    }
    if (planFinished) {
      planDrawerExpanded = false;
    } else if (observedPlanId !== planId || observedActivePlanItemId !== activeItemId) {
      planDrawerExpanded = true;
    }
    observedPlanId = planId;
    observedActivePlanItemId = activeItemId;
  });

  function createClient(): RustDaemonClient {
    return new RustDaemonClient(resolveAgentBaseUrl());
  }

  function currentSessionIdValue(): string | null {
    if (typeof window !== 'undefined') {
      const routeSessionId = new URL(window.location.href).searchParams.get('sessionId')?.trim() || '';
      if (routeSessionId) return routeSessionId;
    }
    const sessionId = currentSessionId?.trim();
    return sessionId || null;
  }

  function currentWorkspaceIdValue(): string {
    if (typeof window !== 'undefined') {
      const routeWorkspaceId = new URL(window.location.href).searchParams.get('workspaceId')?.trim() || '';
      if (routeWorkspaceId) return routeWorkspaceId;
    }
    const stateWorkspaceId = typeof messagesState.currentWorkspaceId === 'string'
      ? messagesState.currentWorkspaceId.trim()
      : '';
    return stateWorkspaceId;
  }

  function currentWorkspacePathValue(): string {
    if (typeof window !== 'undefined') {
      const routeWorkspacePath = new URL(window.location.href).searchParams.get('workspacePath')?.trim() || '';
      if (routeWorkspacePath) return routeWorkspacePath;
    }
    const stateWorkspacePath = typeof currentWorkspacePath === 'string'
      ? currentWorkspacePath.trim()
      : '';
    return stateWorkspacePath;
  }

  function sessionScopeKey(workspaceId: string, sessionId: string): string {
    return workspaceId ? `${workspaceId}\u0000${sessionId}` : `session:${sessionId}`;
  }

  function buildPlanSummary(items: PlanItemDto[]) {
    return {
      total: items.length,
      completed: items.filter((item) => item.status === 'completed').length,
      running: items.filter((item) => item.status === 'in_progress').length,
      pending: items.filter((item) => item.status === 'pending').length,
      blocked: items.filter((item) => item.status === 'blocked').length,
      canceled: items.filter((item) => item.status === 'canceled').length,
    };
  }

  function planItemStatusLabel(status: PlanItemDto['status']): string {
    switch (status) {
      case 'completed': return i18n.t('goalPanel.plan.status.completed');
      case 'in_progress': return i18n.t('goalPanel.plan.status.inProgress');
      case 'pending': return i18n.t('goalPanel.plan.status.pending');
      case 'blocked': return i18n.t('goalPanel.plan.status.blocked');
      case 'canceled': return i18n.t('goalPanel.plan.status.canceled');
      default: return status;
    }
  }

  function planItemStatusIcon(status: PlanItemDto['status']): IconName {
    if (status === 'in_progress' && currentPlanPaused) return 'pause';
    switch (status) {
      case 'completed': return 'check-circle';
      case 'in_progress': return 'loader';
      case 'pending': return 'circle';
      case 'blocked': return 'alert-triangle';
      case 'canceled': return 'x-circle';
      default: return 'circle';
    }
  }

  function planItemMeta(item: PlanItemDto): string {
    return planItemStatusLabel(item.status);
  }

  function goalStatusLabel(goal: SessionGoalDto): string {
    if (goal.status === 'active' && goal.continuation.phase === 'waiting') {
      return i18n.t('goalPanel.goal.statusWaiting');
    }
    switch (goal.status) {
      case 'active': return i18n.t('goalPanel.goal.statusActive');
      case 'paused': return i18n.t('goalPanel.goal.statusPaused');
      case 'blocked': return i18n.t('goalPanel.goal.statusBlocked');
      case 'usage_limited': return i18n.t('goalPanel.goal.statusUsageLimited');
      case 'budget_limited': return i18n.t('goalPanel.goal.statusBudgetLimited');
      case 'complete': return i18n.t('goalPanel.goal.statusComplete');
      default: return goal.status;
    }
  }

  function goalStatusIcon(goal: SessionGoalDto): IconName {
    if (goal.status === 'active' && goal.continuation.phase === 'waiting') return 'clock';
    switch (goal.status) {
      case 'complete': return 'check-circle';
      case 'paused': return 'pause';
      case 'blocked':
      case 'usage_limited':
      case 'budget_limited': return 'alert-triangle';
      default: return 'target';
    }
  }

  function goalCanEdit(goal: SessionGoalDto): boolean {
    return allowedGoalActions?.canEdit ?? goal.status !== 'complete';
  }

  function goalCanPause(goal: SessionGoalDto): boolean {
    return allowedGoalActions?.canPause
      ?? (!currentPlanPaused && !currentPlanBlocked && goal.status === 'active');
  }

  function goalCanResume(goal: SessionGoalDto): boolean {
    return allowedGoalActions?.canResume
      ?? (goal.status === 'paused' || goal.status === 'blocked');
  }

  function goalResumeBudgetValid(goal: SessionGoalDto): boolean {
    return goal.status !== 'budget_limited'
      || Number.parseInt(goalBudgetDraft, 10) > goal.tokensUsed;
  }

  function goalBudgetLabel(tokensUsed: number, tokenBudget?: number | null): string {
    const used = Number.isFinite(tokensUsed) ? Math.max(0, Math.round(tokensUsed)) : 0;
    if (!tokenBudget || tokenBudget <= 0) {
      return `${used.toLocaleString()} tokens`;
    }
    return `${used.toLocaleString()} / ${Math.round(tokenBudget).toLocaleString()} tokens`;
  }

  function goalTimeLabel(seconds: number): string {
    const value = Number.isFinite(seconds) ? Math.max(0, Math.round(seconds)) : 0;
    if (value < 60) return `${value}s`;
    const minutes = Math.floor(value / 60);
    const remain = value % 60;
    return remain > 0 ? `${minutes}m ${remain}s` : `${minutes}m`;
  }

  function formatGoalDateTime(timestamp?: number): string {
    if (typeof timestamp !== 'number' || !Number.isFinite(timestamp) || timestamp <= 0) {
      return '--';
    }
    const date = new Date(timestamp);
    const month = String(date.getMonth() + 1).padStart(2, '0');
    const day = String(date.getDate()).padStart(2, '0');
    const hours = String(date.getHours()).padStart(2, '0');
    const minutes = String(date.getMinutes()).padStart(2, '0');
    return `${month}-${day} ${hours}:${minutes}`;
  }

  function goalActionRequest() {
    const newTokenBudget = currentGoal?.status === 'budget_limited'
      ? Number.parseInt(goalBudgetDraft, 10)
      : undefined;
    return {
      sessionId: currentSessionIdValue() ?? '',
      workspaceId: currentWorkspaceIdValue(),
      workspacePath: currentWorkspacePathValue(),
      goalId: currentGoal?.goalId ?? '',
      expectedRevision: currentGoal?.controlRevision ?? 0,
      ...(currentPlan ? { expectedPlanRevision: currentPlan.revision } : {}),
      ...(Number.isFinite(newTokenBudget) ? { newTokenBudget } : {}),
    };
  }

  async function runGoalAction(
    action: 'save' | 'pause' | 'resume' | 'clear',
    task: () => Promise<void>,
  ) {
    if (goalActionLoading) return;
    goalActionLoading = action;
    try {
      await task();
    } finally {
      if (goalActionLoading === action) {
        goalActionLoading = null;
      }
    }
  }

  async function refreshGoalAfterMutation(): Promise<void> {
    const request = goalActionRequest();
    if (!request.sessionId) return;
    await refreshCurrentGoal(request.sessionId, request.workspaceId, request.workspacePath);
  }

  function startEditGoal(): void {
    if (!currentGoal || !goalCanEdit(currentGoal)) return;
    goalObjectiveDraft = currentGoal.objective;
    isEditingGoal = true;
    goalDrawerExpanded = true;
  }

  function cancelEditGoal(): void {
    goalObjectiveDraft = currentGoal?.objective ?? '';
    isEditingGoal = false;
  }

  async function saveGoalObjective(): Promise<void> {
    const objective = goalObjectiveDraft.trim();
    if (!currentGoal || !goalCanEdit(currentGoal) || !objective) return;
    await runGoalAction('save', async () => {
      await createClient().updateCurrentGoal({
        ...goalActionRequest(),
        objective,
      });
      await refreshGoalAfterMutation();
      isEditingGoal = false;
      addToast('success', i18n.t('goalPanel.action.goalUpdated'));
    }).catch((err) => {
      console.warn('[GoalRunDrawers] goal update failed:', err);
      addToast('error', i18n.t('goalPanel.action.goalUpdateFailed'));
    });
  }

  async function pauseGoal(): Promise<void> {
    if (!currentGoal || !goalCanPause(currentGoal)) return;
    await runGoalAction('pause', async () => {
      await createClient().pauseCurrentGoal(goalActionRequest());
      await refreshGoalAfterMutation();
      addToast('info', i18n.t('goalPanel.action.goalPaused'));
    }).catch((err) => {
      console.warn('[GoalRunDrawers] goal pause failed:', err);
      addToast('error', i18n.t('goalPanel.action.goalPauseFailed'));
    });
  }

  async function resumeGoal(): Promise<void> {
    if (!currentGoal || !goalCanResume(currentGoal)) return;
    if (!goalResumeBudgetValid(currentGoal)) return;
    await runGoalAction('resume', async () => {
      await createClient().resumeCurrentGoal({
        ...goalActionRequest(),
        accessProfile: readStoredAccessProfile(),
      });
      await refreshGoalAfterMutation();
      addToast('success', i18n.t('goalPanel.action.goalResumed'));
    }).catch((err) => {
      console.warn('[GoalRunDrawers] goal resume failed:', err);
      addToast('error', i18n.t('goalPanel.action.goalResumeFailed'));
    });
  }

  async function clearGoal(): Promise<void> {
    if (!currentGoal) return;
    await runGoalAction('clear', async () => {
      const response = await createClient().clearCurrentGoal(goalActionRequest());
      applyCurrentGoalResponse(response);
      isEditingGoal = false;
      addToast('info', i18n.t('goalPanel.action.goalCleared'));
    }).catch((err) => {
      console.warn('[GoalRunDrawers] goal clear failed:', err);
      addToast('error', i18n.t('goalPanel.action.goalClearFailed'));
    });
  }

  async function clearPlan(): Promise<void> {
    if (!hasCurrentPlan || planClearLoading) return;
    planClearLoading = true;
    try {
      const response = await createClient().clearCurrentPlan(goalActionRequest());
      applyCurrentGoalResponse(response);
      planDrawerExpanded = false;
    } catch (err) {
      console.warn('[GoalRunDrawers] plan clear failed:', err);
      addToast('error', i18n.t('goalPanel.action.planClearFailed'));
    } finally {
      planClearLoading = false;
    }
  }

</script>

{#if currentGoal || hasCurrentPlan}
<div class="goal-run-drawers">
  {#if hasCurrentPlan}
    <section class="run-drawer plan-panel" data-testid="plan-card" aria-label={i18n.t('goalPanel.plan.title')}>
      <div class="run-drawer-header">
        <button
          type="button"
          class="run-drawer-toggle"
          aria-expanded={planDrawerExpanded}
          onclick={() => planDrawerExpanded = !planDrawerExpanded}
        >
          <span class="drawer-leading-icon drawer-leading-icon--plan"><Icon name="list" size={14} /></span>
          <span class="run-drawer-title">{i18n.t('goalPanel.plan.title')}</span>
          <span class="run-progress-count">
            {i18n.t('goalPanel.progress.completedCount', {
              completed: planSummary.completed,
              total: planSummary.total,
            })}
          </span>
          {#if currentPlanBlocked}
            <span class="plan-running plan-running--blocked">{i18n.t('goalPanel.plan.state.blocked')}</span>
          {:else if currentPlanPaused}
            <span class="plan-running">{i18n.t('goalPanel.plan.state.paused')}</span>
          {:else if planSummary.running > 0}
            <span class="plan-running">{i18n.t('goalPanel.plan.runningCount', { count: planSummary.running })}</span>
          {/if}
          <Icon name={planDrawerExpanded ? 'chevron-down' : 'chevron-right'} size={13} class="drawer-chevron" />
        </button>
        {#if currentGoal && goalCanResume(currentGoal)}
          <div class="goal-actions">
            <button
              type="button"
              class="plan-resume-action"
              disabled={goalActionLoading !== null || !goalResumeBudgetValid(currentGoal)}
              onclick={resumeGoal}
              title={i18n.t('goalPanel.action.resumeBlockedPlan')}
            >
              <Icon name={goalActionLoading === 'resume' ? 'loader' : 'play'} size={12} class={goalActionLoading === 'resume' ? 'spinning' : ''} />
              {i18n.t('goalPanel.action.resumeBlockedPlan')}
            </button>
          </div>
        {:else if planSummary.total > 0 && planSummary.completed === planSummary.total}
          <div class="goal-actions">
            <button
              type="button"
              class="icon-action icon-action--danger"
              disabled={planClearLoading}
              onclick={clearPlan}
              title={i18n.t('goalPanel.action.clearPlanTitle')}
              aria-label={i18n.t('goalPanel.action.clearPlanTitle')}
            >
              <Icon name={planClearLoading ? 'loader' : 'trash'} size={13} class={planClearLoading ? 'spinning' : ''} />
            </button>
          </div>
        {/if}
      </div>

      {#if planDrawerExpanded}
        {#if currentPlanBlocked}
          <div class="plan-blocked-hint">
            <Icon name="alert-triangle" size={14} />
            <span>{i18n.t('goalPanel.plan.blockedHint')}</span>
          </div>
        {/if}
        <div class="run-progress-bar plan-progress-bar" aria-hidden="true">
          <span style="width: {planProgressPercent}%"></span>
        </div>
        <div class="run-list plan-list" role="list">
          {#each currentPlanItems as item (item.itemId)}
            {@const planIcon = planItemStatusIcon(item.status)}
            <div class="run-row run-row--plan run-row--{item.status}" role="listitem">
              <span class="run-row-icon status-icon--{item.status}" aria-label={planItemStatusLabel(item.status)}>
                <Icon name={planIcon} size={15} class={item.status === 'in_progress' && !currentPlanPaused ? 'spinning' : ''} />
              </span>
              <span class="run-row-main">
                <span class="run-row-title">{item.title}</span>
                <span class="run-row-meta">{planItemMeta(item)}</span>
              </span>
            </div>
          {/each}
        </div>
      {/if}
    </section>
  {/if}

  {#if currentGoal}
    <section
      class="run-drawer goal-panel goal-panel--{currentGoal.status}"
      data-testid="goal-card"
      aria-label={i18n.t('goalPanel.goal.current')}
    >
      <div class="run-drawer-header">
        <button
          type="button"
          class="run-drawer-toggle goal-drawer-toggle"
          aria-expanded={goalDrawerExpanded}
          onclick={() => goalDrawerExpanded = !goalDrawerExpanded}
        >
          <span class="drawer-leading-icon goal-status-icon"><Icon name={goalStatusIcon(currentGoal)} size={14} /></span>
          <span class="goal-heading">
            <span class="goal-status-title">{goalStatusLabel(currentGoal)}</span>
            <span class="goal-objective">{currentGoal.objective}</span>
          </span>
          <span class="goal-meta">{goalTimeLabel(currentGoalTimeSeconds)}</span>
          <Icon name={goalDrawerExpanded ? 'chevron-down' : 'chevron-right'} size={13} class="drawer-chevron" />
        </button>
        <div class="goal-actions">
          {#if goalCanEdit(currentGoal)}
            <button
              type="button"
              class="icon-action"
              disabled={goalActionLoading !== null}
              onclick={startEditGoal}
              title={i18n.t('goalPanel.action.editGoalTitle')}
              aria-label={i18n.t('goalPanel.action.editGoalTitle')}
            >
              <Icon name="pencil" size={13} />
            </button>
          {/if}
          {#if goalCanResume(currentGoal)}
            <button
              type="button"
              class="icon-action"
              disabled={goalActionLoading !== null || !goalResumeBudgetValid(currentGoal)}
              onclick={resumeGoal}
              title={i18n.t('goalPanel.action.resumeGoalTitle')}
              aria-label={i18n.t('goalPanel.action.resumeGoalTitle')}
            >
              <Icon name={goalActionLoading === 'resume' ? 'loader' : 'play'} size={13} class={goalActionLoading === 'resume' ? 'spinning' : ''} />
            </button>
          {:else if goalCanPause(currentGoal)}
            <button
              type="button"
              class="icon-action"
              disabled={goalActionLoading !== null}
              onclick={pauseGoal}
              title={i18n.t('goalPanel.action.pauseGoalTitle')}
              aria-label={i18n.t('goalPanel.action.pauseGoalTitle')}
            >
              <Icon name={goalActionLoading === 'pause' ? 'loader' : 'pause'} size={13} class={goalActionLoading === 'pause' ? 'spinning' : ''} />
            </button>
          {/if}
          <button
            type="button"
            class="icon-action icon-action--danger"
            disabled={goalActionLoading !== null}
            onclick={clearGoal}
            title={i18n.t('goalPanel.action.clearGoalTitle')}
            aria-label={i18n.t('goalPanel.action.clearGoalTitle')}
          >
            <Icon name={goalActionLoading === 'clear' ? 'loader' : 'trash'} size={13} class={goalActionLoading === 'clear' ? 'spinning' : ''} />
          </button>
        </div>
      </div>
      {#if goalDrawerExpanded}
        {#if isEditingGoal}
          <form class="goal-edit-form" onsubmit={(event) => { event.preventDefault(); void saveGoalObjective(); }}>
            <input
              class="goal-edit-input"
              bind:value={goalObjectiveDraft}
              maxlength="4000"
              aria-label={i18n.t('goalPanel.action.editGoalTitle')}
            />
            <button
              type="submit"
              class="goal-edit-button"
              disabled={goalActionLoading !== null || !goalObjectiveDraft.trim()}
            >
              {goalActionLoading === 'save' ? i18n.t('common.loading') : i18n.t('common.save')}
            </button>
            <button
              type="button"
              class="goal-edit-button goal-edit-button--ghost"
              disabled={goalActionLoading !== null}
              onclick={cancelEditGoal}
            >
              {i18n.t('common.cancel')}
            </button>
          </form>
        {:else}
          <div class="goal-detail">
            <p class="goal-detail-objective-text">{currentGoal.objective}</p>
            {#if currentGoal.status === 'budget_limited'}
              <label class="goal-budget-resume-field">
                <span>{i18n.t('goalPanel.goal.newBudget')}</span>
                <input
                  type="number"
                  min={currentGoal.tokensUsed + 1}
                  step="1"
                  bind:value={goalBudgetDraft}
                />
              </label>
            {/if}
            <div class="goal-stat-strip">
              <span class="goal-detail-item">
                <span class="goal-detail-label">{i18n.t('goalPanel.goal.elapsed')}</span>
                <strong>{goalTimeLabel(currentGoalTimeSeconds)}</strong>
              </span>
              <span class="goal-detail-item">
                <span class="goal-detail-label">{i18n.t('goalPanel.goal.budget')}</span>
                <strong>{goalBudgetLabel(currentGoal.tokensUsed, currentGoal.tokenBudget)}</strong>
              </span>
              <span class="goal-detail-item">
                <span class="goal-detail-label">{i18n.t('goalPanel.goal.updatedAtShort')}</span>
                <strong>{formatGoalDateTime(currentGoal.updatedAt)}</strong>
              </span>
            </div>
            <span class="goal-created-at">
              {i18n.t('goalPanel.goal.createdAt')} {formatGoalDateTime(currentGoal.createdAt)}
            </span>
          </div>
        {/if}
      {/if}
    </section>
  {/if}

</div>
{/if}

<style>
  .goal-run-drawers {
    display: flex;
    flex-direction: column;
    gap: 8px;
    width: 100%;
    padding: 0 var(--space-4);
    box-sizing: border-box;
    position: relative;
    z-index: 0;
  }

  .run-drawer {
    display: flex;
    flex-direction: column;
    gap: 8px;
    min-width: 0;
    padding: 10px 12px;
    border: 1px solid color-mix(in srgb, var(--border) 78%, transparent);
    border-radius: 8px;
    background: color-mix(in srgb, var(--surface-1) 72%, var(--background));
    box-sizing: border-box;
  }

  .goal-panel {
    --goal-tone: var(--primary);
    order: 3;
    width: 100%;
    padding: 9px 10px;
    border-color: color-mix(in srgb, var(--goal-tone) 24%, var(--border));
    border-left: 2px solid var(--goal-tone);
    background: color-mix(in srgb, var(--vscode-input-background) 94%, var(--background));
  }

  .goal-panel--paused {
    --goal-tone: var(--foreground-muted);
  }

  .goal-panel--blocked,
  .goal-panel--usage_limited,
  .goal-panel--budget_limited {
    --goal-tone: var(--warning);
  }

  .goal-panel--complete {
    --goal-tone: var(--success);
  }

  .plan-panel {
    order: 1;
  }

  .run-drawer-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
    min-width: 0;
  }

  .run-drawer-toggle {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    flex: 1 1 auto;
    padding: 0;
    border: 0;
    background: transparent;
    color: var(--foreground);
    font: inherit;
    text-align: left;
    cursor: pointer;
  }

  .run-drawer-toggle:focus-visible,
  .icon-action:focus-visible,
  .goal-edit-button:focus-visible,
  .goal-edit-input:focus-visible {
    outline: 2px solid color-mix(in srgb, var(--primary) 58%, transparent);
    outline-offset: 2px;
  }

  .goal-budget-resume-field {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(120px, 180px);
    align-items: center;
    gap: 10px;
    color: var(--foreground-muted);
    font-size: 12px;
  }

  .goal-budget-resume-field input {
    min-width: 0;
    height: 28px;
    padding: 0 8px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--vscode-input-background);
    color: var(--foreground);
    font: inherit;
  }

  .run-drawer-toggle:focus-visible {
    border-radius: 4px;
  }

  .run-drawer-toggle > :global(svg) {
    flex: 0 0 auto;
    color: var(--foreground-muted);
  }

  .drawer-leading-icon {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 24px;
    height: 24px;
    flex: 0 0 24px;
    border-radius: 6px;
    background: color-mix(in srgb, var(--primary) 10%, transparent);
    color: var(--primary);
  }

  .drawer-leading-icon :global(svg) {
    color: inherit;
  }

  .drawer-leading-icon--plan {
    background: color-mix(in srgb, var(--success) 10%, transparent);
    color: color-mix(in srgb, var(--success) 82%, var(--foreground));
  }

  .run-drawer-title {
    flex: 0 0 auto;
    color: var(--foreground);
    font-size: var(--text-sm);
    font-weight: var(--font-semibold);
    white-space: nowrap;
  }

  .goal-drawer-toggle {
    min-height: 28px;
  }

  .goal-status-icon {
    background: color-mix(in srgb, var(--goal-tone) 11%, transparent);
    color: var(--goal-tone);
  }

  .goal-heading {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr);
    align-items: center;
    gap: 7px;
    min-width: 0;
    flex: 1 1 auto;
  }

  .goal-status-title {
    color: var(--goal-tone);
    font-size: var(--text-2xs);
    font-weight: var(--font-semibold);
    white-space: nowrap;
  }

  :global(.drawer-chevron) {
    margin-left: auto;
    opacity: 0.55;
    transition: opacity var(--transition-fast);
  }

  .run-drawer-toggle:hover :global(.drawer-chevron) {
    opacity: 0.85;
  }

  .goal-objective {
    min-width: 0;
    overflow: hidden;
    color: var(--foreground);
    font-size: var(--text-sm);
    font-weight: var(--font-semibold);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .goal-meta {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--space-2);
    min-width: 0;
  }

  .goal-meta {
    flex: 0 0 auto;
    color: var(--foreground-muted);
    font-size: var(--text-2xs);
    white-space: nowrap;
  }

  .goal-actions {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    flex: 0 0 auto;
  }

  .plan-resume-action {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    min-height: 26px;
    padding: 0 8px;
    border: 1px solid color-mix(in srgb, var(--warning) 28%, var(--border));
    border-radius: var(--radius-sm);
    background: color-mix(in srgb, var(--warning) 8%, transparent);
    color: color-mix(in srgb, var(--warning) 84%, var(--foreground));
    font-size: var(--text-xs);
    cursor: pointer;
  }

  .plan-resume-action:hover:not(:disabled) {
    background: color-mix(in srgb, var(--warning) 14%, transparent);
  }

  .plan-resume-action:disabled {
    cursor: not-allowed;
    opacity: 0.55;
  }

  .icon-action {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 26px;
    height: 26px;
    padding: 0;
    border: 0;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    transition:
      background var(--transition-fast),
      color var(--transition-fast);
  }

  .icon-action:hover:not(:disabled) {
    background: color-mix(in srgb, var(--surface-hover) 80%, transparent);
    color: var(--foreground);
  }

  .icon-action--danger:hover:not(:disabled) {
    color: var(--error);
  }

  .icon-action:disabled {
    cursor: not-allowed;
    opacity: 0.55;
  }

  .goal-edit-form {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto auto;
    gap: var(--space-2);
    padding: 2px 0 1px 32px;
  }

  .goal-detail {
    display: flex;
    flex-direction: column;
    gap: 8px;
    max-height: min(36vh, 320px);
    padding: 3px 0 1px 32px;
    min-width: 0;
    overflow-y: auto;
    overscroll-behavior: contain;
    scrollbar-gutter: stable;
  }

  .goal-detail-objective-text {
    margin: 0;
    min-width: 0;
    color: var(--foreground);
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    line-height: 1.5;
    overflow-wrap: anywhere;
  }

  .goal-detail-label {
    color: var(--foreground-muted);
    font-size: var(--text-2xs);
    font-weight: var(--font-medium);
    line-height: var(--leading-tight);
  }

  .goal-stat-strip {
    display: grid;
    grid-template-columns: repeat(4, minmax(0, 1fr));
    min-width: 0;
    border-top: 1px solid color-mix(in srgb, var(--border) 70%, transparent);
    border-bottom: 1px solid color-mix(in srgb, var(--border) 70%, transparent);
  }

  .goal-detail-item {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
    padding: 7px 10px;
    border-right: 1px solid color-mix(in srgb, var(--border) 70%, transparent);
  }

  .goal-detail-item:first-child {
    padding-left: 0;
  }

  .goal-detail-item:last-child {
    padding-right: 0;
    border-right: 0;
  }

  .goal-detail-item strong {
    min-width: 0;
    overflow: hidden;
    color: var(--foreground);
    font-size: var(--text-xs);
    font-weight: var(--font-semibold);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .goal-created-at {
    color: var(--foreground-muted);
    font-size: var(--text-2xs);
  }

  .goal-edit-input {
    min-width: 0;
    height: 30px;
    padding: 0 var(--space-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--background);
    color: var(--foreground);
    font: inherit;
    font-size: var(--text-xs);
  }

  .goal-edit-input:focus {
    border-color: color-mix(in srgb, var(--primary) 48%, var(--border));
    outline: none;
  }

  .goal-edit-button {
    height: 30px;
    padding: 0 var(--space-3);
    border: 1px solid color-mix(in srgb, var(--primary) 40%, var(--border));
    border-radius: var(--radius-sm);
    background: var(--primary);
    color: var(--primary-foreground);
    font-size: var(--text-2xs);
    font-weight: var(--font-semibold);
    cursor: pointer;
  }

  .goal-edit-button--ghost {
    border-color: var(--border);
    background: transparent;
    color: var(--foreground-muted);
  }

  .goal-edit-button:disabled {
    cursor: not-allowed;
    opacity: 0.55;
  }

  .run-progress-count {
    color: var(--foreground-muted);
    font-size: var(--text-2xs);
    white-space: nowrap;
  }

  .plan-running {
    flex: 0 0 auto;
    color: var(--primary);
    font-size: var(--text-2xs);
    font-weight: var(--font-medium);
    white-space: nowrap;
  }

  .plan-running--blocked {
    color: var(--warning);
  }

  .plan-blocked-hint {
    display: flex;
    align-items: flex-start;
    gap: 7px;
    padding: 8px 9px;
    border: 1px solid color-mix(in srgb, var(--warning) 26%, transparent);
    border-radius: var(--radius-md);
    background: color-mix(in srgb, var(--warning) 7%, transparent);
    color: color-mix(in srgb, var(--warning) 78%, var(--foreground));
    font-size: var(--text-xs);
    line-height: 1.45;
  }

  .plan-blocked-hint :global(svg) {
    flex: 0 0 auto;
    margin-top: 1px;
  }

  .run-progress-bar {
    overflow: hidden;
    width: 100%;
    height: 3px;
    border-radius: var(--radius-full);
    background: color-mix(in srgb, var(--border) 48%, transparent);
  }

  .run-progress-bar span {
    display: block;
    height: 100%;
    border-radius: inherit;
    background: var(--primary);
    transition: width var(--transition-normal);
  }

  .plan-progress-bar span {
    background: var(--success);
  }

  .run-list {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
  }

  .plan-list {
    max-height: min(32vh, 280px);
    overflow-y: auto;
    overscroll-behavior: contain;
    scrollbar-gutter: stable;
  }

  .run-row {
    display: grid;
    grid-template-columns: 22px minmax(0, 1fr);
    align-items: center;
    gap: var(--space-2);
    min-height: 34px;
    padding: var(--space-1);
    border: 1px solid transparent;
    border-radius: var(--radius-sm);
    color: var(--foreground);
  }

  .run-row--plan {
    grid-template-columns: 24px minmax(0, 1fr);
  }

  .run-row--in_progress {
    background: color-mix(in srgb, var(--primary) 7%, transparent);
  }

  .run-row-icon {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 22px;
    flex-shrink: 0;
  }

  .status-icon--in_progress { color: var(--primary); }
  .status-icon--completed { color: var(--success); }
  .status-icon--pending { color: var(--foreground-muted); }

  .run-row-main {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }

  .run-row-title,
  .run-row-meta {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .run-row-title {
    color: var(--foreground);
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    line-height: var(--leading-tight);
  }

  .run-row-meta {
    color: var(--foreground-muted);
    font-size: var(--text-2xs);
    line-height: var(--leading-tight);
  }

  :global(.spinning) {
    animation: spin 1s linear infinite;
  }

  @keyframes spin {
    from { transform: rotate(0deg); }
    to { transform: rotate(360deg); }
  }

  @media (max-width: 640px) {
    .goal-run-drawers {
      gap: 6px;
      padding: 0 10px;
    }

    .run-drawer {
      padding: 9px 10px;
    }

    .run-drawer-header {
      gap: 6px;
    }

    .run-progress-count,
    .plan-running,
    .goal-meta {
      display: none;
    }

    .goal-heading {
      display: block;
    }

    .goal-status-title {
      display: none;
    }

    .goal-actions {
      gap: 0;
    }

    .icon-action {
      width: 28px;
      height: 28px;
    }

    .goal-edit-form {
      grid-template-columns: minmax(0, 1fr) auto auto;
      padding-left: 0;
    }

    .goal-detail {
      padding-left: 0;
    }

    .goal-stat-strip {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }

    .goal-detail-item:nth-child(2) {
      border-right: 0;
    }

    .goal-detail-item:nth-child(n + 3) {
      border-top: 1px solid color-mix(in srgb, var(--border) 70%, transparent);
    }

    .goal-detail-item:nth-child(3) {
      padding-left: 0;
    }
  }
</style>
