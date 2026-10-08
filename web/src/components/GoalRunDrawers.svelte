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
  import { uiClockState, retainUiClock } from '../stores/ui-clock.svelte';

  interface Props {
    /** 当前显示的抽屉数（目标 + 计划），供上层计算排队、提问卡的叠层层级。 */
    count?: number;
  }
  let { count = $bindable(0) }: Props = $props();

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
  let goalNoteEl = $state<HTMLElement | null>(null);
  let goalNoteExpanded = $state(false);
  let goalNoteOverflows = $state(false);
  let goalEvidenceExpanded = $state(false);
  let goalEditEl = $state<HTMLTextAreaElement | null>(null);
  let editingGoalId = '';
  let goalClockObservedAt = $state(Date.now());

  $effect(() => {
    ensureGoalState(currentSessionId, currentWorkspaceId, currentWorkspacePathValue());
  });

  const goalState = $derived(getGoalState(currentSessionId, currentWorkspaceId));
  const currentGoal = $derived<SessionGoalDto | null>(goalState.response?.goal ?? null);
  // 叠层层级（紧挨输入框为 1）：目标抽屉最贴近输入框，计划抽屉在它上面。
  const goalDockLevel = 1;
  const planDockLevel = $derived(1 + (currentGoal ? 1 : 0));
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
        + Math.max(0, uiClockState.now - goalClockObservedAt)
      : 0;
    return Math.floor((settledMillis + runningMillis) / 1000);
  });
  const currentPlanItems = $derived<PlanItemDto[]>(
    Array.isArray(currentPlan?.items) ? currentPlan.items : []
  );
  const goalNote = $derived(currentGoal ? goalNoteOf(currentGoal) : null);
  const GOAL_OBJECTIVE_MAX_CHARS = 4000;
  const goalDraftChanged = $derived(
    !!currentGoal && goalObjectiveDraft.trim() !== currentGoal.objective.trim(),
  );
  const goalDraftSavable = $derived(goalDraftChanged && goalObjectiveDraft.trim().length > 0);

  // 编辑框随内容增高（有上限，超出后内部滚动）；进入编辑时聚焦并把光标放到末尾。
  $effect(() => {
    const el = goalEditEl;
    if (!el) return;
    void goalObjectiveDraft;
    el.style.height = 'auto';
    el.style.height = `${el.scrollHeight}px`;
  });
  $effect(() => {
    const el = goalEditEl;
    if (!el) return;
    el.focus();
    el.setSelectionRange(el.value.length, el.value.length);
  });
  // 目标被清除、换成别的目标、或不再允许编辑时，退出编辑态，避免把旧草稿提交到别的目标上。
  $effect(() => {
    if (!isEditingGoal) return;
    if (!currentGoal || currentGoal.goalId !== editingGoalId || !goalCanEdit(currentGoal)) {
      isEditingGoal = false;
    }
  });
  const goalBudgetPct = $derived(
    currentGoal ? goalBudgetPercent(currentGoal.tokensUsed, currentGoal.tokenBudget) : null,
  );

  // 折叠状态下检测说明是否被截断，只有真被截断才显示“展开全文”。
  $effect(() => {
    const el = goalNoteEl;
    if (!el) {
      goalNoteOverflows = false;
      return;
    }
    void goalNote?.text;
    const measure = () => {
      if (!goalNoteExpanded) goalNoteOverflows = el.scrollHeight > el.clientHeight + 1;
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    return () => observer.disconnect();
  });

  $effect(() => {
    const timingStartedAt = currentGoal?.timingStartedAt;
    const serverObservedAt = goalState.response?.observedAt;
    const localObservedAt = Date.now();
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
    return retainUiClock();
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
  $effect(() => {
    count = (currentGoal ? 1 : 0) + (hasCurrentPlan ? 1 : 0);
  });
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

  /** 展开区里解释“为什么是这个状态”的说明：模型写的文字（阻塞原因、完成总结）会折叠，固定说明不折叠。 */
  interface GoalNote {
    text: string;
    /** 模型写的长文字：限制行数，点击展开全文。 */
    collapsible: boolean;
    meta: string | null;
    evidence: string[];
  }

  function goalNoteOf(goal: SessionGoalDto): GoalNote | null {
    switch (goal.status) {
      case 'blocked': {
        const reason = goal.blocker?.reason?.trim();
        if (!reason) return null;
        const turns = goal.blocker?.consecutiveTurns ?? 0;
        return {
          text: reason,
          collapsible: true,
          meta: turns > 1 ? i18n.t('goalPanel.note.blockedTurns', { count: turns }) : null,
          evidence: [],
        };
      }
      case 'complete': {
        const summary = goal.completion?.summary?.trim();
        if (!summary) return null;
        return {
          text: summary,
          collapsible: true,
          meta: null,
          evidence: goal.completion?.evidenceRefs ?? [],
        };
      }
      case 'usage_limited':
        return { text: i18n.t('goalPanel.note.usageLimited'), collapsible: false, meta: null, evidence: [] };
      case 'budget_limited':
        return { text: i18n.t('goalPanel.note.budgetLimited'), collapsible: false, meta: null, evidence: [] };
      case 'active':
        if (goal.continuation.phase !== 'waiting') return null;
        return {
          text: goal.continuation.reason === 'resume_requested'
            ? i18n.t('goalPanel.note.resumeRequested')
            : i18n.t('goalPanel.note.waiting'),
          collapsible: false,
          meta: null,
          evidence: [],
        };
      default:
        return null;
    }
  }

  function goalCanEdit(goal: SessionGoalDto): boolean {
    return currentGoal?.goalId === goal.goalId && allowedGoalActions?.canEdit === true;
  }

  function goalCanPause(goal: SessionGoalDto): boolean {
    return currentGoal?.goalId === goal.goalId && allowedGoalActions?.canPause === true;
  }

  function goalCanResume(goal: SessionGoalDto): boolean {
    return currentGoal?.goalId === goal.goalId && allowedGoalActions?.canResume === true;
  }

  function goalResumeBudgetValid(goal: SessionGoalDto): boolean {
    return goal.status !== 'budget_limited'
      || Number.parseInt(goalBudgetDraft, 10) > goal.tokensUsed;
  }

  function goalBudgetLabel(tokensUsed: number, tokenBudget?: number | null): string {
    const used = Number.isFinite(tokensUsed) ? Math.max(0, Math.round(tokensUsed)) : 0;
    if (!tokenBudget || tokenBudget <= 0) {
      return used.toLocaleString();
    }
    return `${used.toLocaleString()} / ${Math.round(tokenBudget).toLocaleString()}`;
  }

  function goalBudgetPercent(tokensUsed: number, tokenBudget?: number | null): number | null {
    if (!tokenBudget || tokenBudget <= 0 || !Number.isFinite(tokensUsed)) return null;
    return Math.min(100, Math.max(0, Math.round((tokensUsed / tokenBudget) * 100)));
  }

  function goalTimeLabel(seconds: number): string {
    const value = Number.isFinite(seconds) ? Math.max(0, Math.round(seconds)) : 0;
    if (value < 60) return i18n.t('goalPanel.time.seconds', { s: value });
    if (value < 3600) {
      const m = Math.floor(value / 60);
      const s = value % 60;
      return s > 0
        ? i18n.t('goalPanel.time.minutesSeconds', { m, s })
        : i18n.t('goalPanel.time.minutes', { m });
    }
    const h = Math.floor(value / 3600);
    const m = Math.floor((value % 3600) / 60);
    return m > 0
      ? i18n.t('goalPanel.time.hoursMinutes', { h, m })
      : i18n.t('goalPanel.time.hours', { h });
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
    const workspaceId = currentWorkspaceIdValue();
    const workspacePath = currentWorkspacePathValue();
    const workspaceScoped = Boolean(workspaceId || workspacePath);
    return {
      sessionId: currentSessionIdValue() ?? '',
      scope: workspaceScoped ? 'workspace' as const : 'personal' as const,
      ...(workspaceId ? { workspaceId } : {}),
      ...(workspacePath ? { workspacePath } : {}),
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
    if (isEditingGoal) {
      goalEditEl?.focus();
      return;
    }
    goalObjectiveDraft = currentGoal.objective;
    editingGoalId = currentGoal.goalId;
    isEditingGoal = true;
    goalDrawerExpanded = true;
  }

  function cancelEditGoal(): void {
    goalObjectiveDraft = currentGoal?.objective ?? '';
    isEditingGoal = false;
  }

  /** Enter 保存、Shift+Enter 换行、Esc 取消；输入法组词期间的回车不触发。 */
  function handleGoalEditKeydown(event: KeyboardEvent): void {
    if (event.isComposing || event.keyCode === 229) return;
    if (event.key === 'Escape') {
      event.preventDefault();
      event.stopPropagation();
      if (goalActionLoading === null) cancelEditGoal();
      return;
    }
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault();
      void saveGoalObjective();
    }
  }

  async function saveGoalObjective(): Promise<void> {
    const objective = goalObjectiveDraft.trim();
    if (!currentGoal || !goalCanEdit(currentGoal) || !objective) return;
    // 没改动就直接退出编辑，不产生一次无意义的目标修订。
    if (!goalDraftChanged) {
      cancelEditGoal();
      return;
    }
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
    <section class="run-drawer plan-panel dock-card" style="--dock-level: {planDockLevel}" data-testid="plan-card" aria-label={i18n.t('goalPanel.plan.title')}>
      <div class="run-drawer-header dock-header">
        <button
          type="button"
          class="run-drawer-toggle"
          aria-expanded={planDrawerExpanded}
          onclick={() => planDrawerExpanded = !planDrawerExpanded}
        >
          <span class="drawer-leading-icon dock-lead" style="--dock-tone: var(--success)"><Icon name="list" size={14} /></span>
          <span class="run-drawer-title dock-title">{i18n.t('goalPanel.plan.title')}</span>
          <span class="run-progress-count dock-meta">
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
              class="plan-resume-action dock-btn dock-btn--warn"
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
              class="icon-action dock-icon-btn dock-icon-btn--danger"
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
      class="run-drawer goal-panel dock-card goal-panel--{currentGoal.status}"
      style="--dock-level: {goalDockLevel}"
      data-testid="goal-card"
      aria-label={i18n.t('goalPanel.goal.current')}
    >
      <div class="run-drawer-header dock-header">
        <button
          type="button"
          class="run-drawer-toggle goal-drawer-toggle"
          aria-expanded={goalDrawerExpanded}
          onclick={() => goalDrawerExpanded = !goalDrawerExpanded}
        >
          <span class="drawer-leading-icon goal-status-icon dock-lead" style="--dock-tone: var(--goal-tone)"><Icon name={goalStatusIcon(currentGoal)} size={14} /></span>
          <span class="goal-heading" class:expanded={goalDrawerExpanded}>
            <span class="goal-status-title">
              {goalDrawerExpanded
                ? i18n.t('goalPanel.goal.expandedTitle', { status: goalStatusLabel(currentGoal) })
                : goalStatusLabel(currentGoal)}
            </span>
            {#if !goalDrawerExpanded}
              <span class="goal-objective">{currentGoal.objective}</span>
            {/if}
          </span>
          <span class="goal-meta dock-meta">{goalTimeLabel(currentGoalTimeSeconds)}</span>
          <Icon name={goalDrawerExpanded ? 'chevron-down' : 'chevron-right'} size={13} class="drawer-chevron" />
        </button>
        <div class="goal-actions">
          {#if goalCanEdit(currentGoal)}
            <button
              type="button"
              class="icon-action dock-icon-btn"
              class:active={isEditingGoal}
              aria-pressed={isEditingGoal}
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
              class="icon-action dock-icon-btn"
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
              class="icon-action dock-icon-btn"
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
            class="icon-action dock-icon-btn dock-icon-btn--danger"
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
        <div class="goal-detail">
          {#if isEditingGoal}
            <form class="goal-edit-form" onsubmit={(event) => { event.preventDefault(); void saveGoalObjective(); }}>
              <textarea
                class="goal-edit-input"
                bind:this={goalEditEl}
                bind:value={goalObjectiveDraft}
                maxlength={GOAL_OBJECTIVE_MAX_CHARS}
                rows="2"
                readonly={goalActionLoading === 'save'}
                placeholder={i18n.t('goalPanel.edit.placeholder')}
                aria-label={i18n.t('goalPanel.action.editGoalTitle')}
                onkeydown={handleGoalEditKeydown}
              ></textarea>
              <div class="goal-edit-footer">
                <span class="goal-edit-hint">
                  {i18n.t('goalPanel.edit.hint')}
                  {#if goalObjectiveDraft.length >= GOAL_OBJECTIVE_MAX_CHARS * 0.9}
                    · {i18n.t('goalPanel.edit.counter', { count: goalObjectiveDraft.length, max: GOAL_OBJECTIVE_MAX_CHARS })}
                  {/if}
                </span>
                <span class="goal-edit-buttons">
                  <button
                    type="button"
                    class="goal-edit-button goal-edit-button--ghost"
                    disabled={goalActionLoading !== null}
                    onclick={cancelEditGoal}
                  >
                    {i18n.t('common.cancel')}
                  </button>
                  <button
                    type="submit"
                    class="goal-edit-button"
                    disabled={goalActionLoading !== null || !goalDraftSavable}
                  >
                    {goalActionLoading === 'save' ? i18n.t('common.loading') : i18n.t('common.save')}
                  </button>
                </span>
              </div>
            </form>
          {:else}
            <p class="goal-detail-objective-text">{currentGoal.objective}</p>
          {/if}
          {#if goalNote}
            <div class="goal-note goal-note--{currentGoal.status}">
              <p
                class="goal-note-text"
                class:clamped={goalNote.collapsible && !goalNoteExpanded}
                bind:this={goalNoteEl}
              >{goalNote.text}</p>
              {#if goalNote.collapsible && (goalNoteOverflows || goalNoteExpanded)}
                <button type="button" class="goal-note-toggle" onclick={() => goalNoteExpanded = !goalNoteExpanded}>
                  {goalNoteExpanded ? i18n.t('goalPanel.note.collapse') : i18n.t('goalPanel.note.expand')}
                </button>
              {/if}
              {#if goalNote.meta}
                <span class="goal-note-meta">{goalNote.meta}</span>
              {/if}
              {#if goalNote.evidence.length > 0}
                <button
                  type="button"
                  class="goal-note-toggle"
                  aria-expanded={goalEvidenceExpanded}
                  onclick={() => goalEvidenceExpanded = !goalEvidenceExpanded}
                >
                  {i18n.t('goalPanel.note.evidenceCount', { count: goalNote.evidence.length })}
                </button>
                {#if goalEvidenceExpanded}
                  <ul class="goal-evidence-list">
                    {#each goalNote.evidence as ref (ref)}
                      <li>{ref}</li>
                    {/each}
                  </ul>
                {/if}
              {/if}
            </div>
          {/if}
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
          <div class="goal-metrics">
            <span class="goal-metric">
              <span class="goal-detail-label">{i18n.t('goalPanel.goal.elapsed')}</span>
              <strong>{goalTimeLabel(currentGoalTimeSeconds)}</strong>
            </span>
            <span class="goal-metric">
              <span class="goal-detail-label">{i18n.t('goalPanel.goal.createdAt')}</span>
              <strong>{formatGoalDateTime(currentGoal.createdAt)}</strong>
            </span>
            <span class="goal-metric goal-metric--budget">
              <span class="goal-detail-label">{i18n.t('goalPanel.goal.budget')}</span>
              <strong>{goalBudgetLabel(currentGoal.tokensUsed, currentGoal.tokenBudget)}</strong>
              {#if goalBudgetPct !== null}
                <span
                  class="goal-budget-bar"
                  class:warn={goalBudgetPct >= 90}
                  role="progressbar"
                  aria-valuemin="0"
                  aria-valuemax="100"
                  aria-valuenow={goalBudgetPct}
                ><span style="width: {goalBudgetPct}%"></span></span>
              {/if}
            </span>
          </div>
        </div>
      {/if}
    </section>
  {/if}

</div>
{/if}

<style>
  /* 卡片的外形（上圆角、无下边线、缩进台阶、背景退后）由全局 .dock-card 统一提供。 */
  .goal-run-drawers {
    display: flex;
    flex-direction: column;
    width: 100%;
    position: relative;
    z-index: 0;
  }

  .run-drawer {
    min-width: 0;
  }

  .goal-panel {
    --goal-tone: var(--primary);
    order: 3;
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


  .drawer-leading-icon :global(svg) {
    color: inherit;
  }


  .run-drawer-title {
    flex: 0 0 auto;
  }

  .goal-drawer-toggle {
    min-height: 28px;
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
  }

  .goal-actions {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    flex: 0 0 auto;
  }








  .goal-edit-form {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    min-width: 0;
  }

  .goal-detail {
    display: flex;
    flex-direction: column;
    gap: 8px;
    max-height: min(36vh, 320px);
    /* 全局滚动条宽 5px：始终预留这 5px 并让它落在卡片内边距里，
       无论是否出现滚动条，内容左右留白都对称。 */
    margin-right: -5px;
    padding: 3px 0 1px;
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

  .goal-note {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 4px;
    min-width: 0;
    padding: 6px 10px;
    border-radius: var(--radius-sm);
    background: color-mix(in srgb, var(--goal-tone) 7%, transparent);
  }

  .goal-note-text {
    margin: 0;
    min-width: 0;
    max-width: 100%;
    color: var(--foreground);
    font-size: var(--text-xs);
    line-height: 1.5;
    overflow-wrap: anywhere;
    white-space: pre-wrap;
  }

  .goal-note-text.clamped {
    display: -webkit-box;
    line-clamp: 3;
    -webkit-line-clamp: 3;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }

  .goal-note-meta {
    color: var(--foreground-muted);
    font-size: var(--text-2xs);
  }

  .goal-note-toggle {
    padding: 0;
    border: 0;
    background: transparent;
    color: var(--primary);
    font: inherit;
    font-size: var(--text-2xs);
    cursor: pointer;
  }

  .goal-note-toggle:hover {
    text-decoration: underline;
  }

  .goal-evidence-list {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin: 0;
    padding: 0;
    max-width: 100%;
    list-style: none;
    color: var(--foreground-muted);
    font-family: var(--font-mono, monospace);
    font-size: var(--text-2xs);
    overflow-wrap: anywhere;
  }

  .goal-metrics {
    display: grid;
    grid-template-columns: auto auto minmax(0, 1fr);
    min-width: 0;
    border-top: 1px solid color-mix(in srgb, var(--border) 70%, transparent);
    border-bottom: 1px solid color-mix(in srgb, var(--border) 70%, transparent);
  }

  .goal-metric {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
    padding: 7px 14px;
    border-right: 1px solid color-mix(in srgb, var(--border) 70%, transparent);
  }

  .goal-metric:first-child {
    padding-left: 0;
  }

  .goal-metric:last-child {
    padding-right: 0;
    border-right: 0;
  }

  .goal-metric strong {
    min-width: 0;
    overflow: hidden;
    color: var(--foreground);
    font-size: var(--text-xs);
    font-weight: var(--font-semibold);
    font-variant-numeric: tabular-nums;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .goal-budget-bar {
    display: block;
    height: 3px;
    overflow: hidden;
    border-radius: 999px;
    background: color-mix(in srgb, var(--foreground) 10%, transparent);
  }

  .goal-budget-bar > span {
    display: block;
    height: 100%;
    border-radius: inherit;
    background: var(--primary);
    transition: width var(--transition-fast);
  }

  .goal-budget-bar.warn > span {
    background: var(--warning);
  }

  .goal-edit-input {
    box-sizing: border-box;
    width: 100%;
    min-width: 0;
    min-height: 56px;
    max-height: 160px;
    padding: 6px var(--space-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--background);
    color: var(--foreground);
    font: inherit;
    font-size: var(--text-sm);
    line-height: 1.5;
    resize: none;
    overflow-y: auto;
  }

  .goal-edit-input[readonly] {
    opacity: 0.7;
  }

  .goal-edit-footer {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
    min-width: 0;
  }

  .goal-edit-hint {
    min-width: 0;
    color: var(--foreground-muted);
    font-size: var(--text-2xs);
    line-height: 1.4;
  }

  .goal-edit-buttons {
    display: inline-flex;
    flex: 0 0 auto;
    gap: var(--space-2);
  }

  .icon-action.active {
    background: color-mix(in srgb, var(--primary) 14%, transparent);
    color: var(--primary);
  }

  .goal-edit-input:focus {
    border-color: color-mix(in srgb, var(--primary) 48%, var(--border));
    outline: none;
  }

  .goal-edit-button {
    height: 26px;
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
    flex: 0 0 auto;
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

    /* 收起时状态标题隐藏、只剩一列目标文字。保持 grid 让目标文字成为网格项（块级）：
       内联元素上的 overflow / text-overflow 不生效，长目标会冲出卡片。
       展开后完整目标在详情里，标题行只留状态标题。 */
    .goal-heading:not(.expanded) {
      grid-template-columns: minmax(0, 1fr);
    }

    .goal-heading:not(.expanded) .goal-status-title {
      display: none;
    }

    .goal-actions {
      gap: 0;
    }

    .icon-action {
      width: 28px;
      height: 28px;
    }

    /* 窄屏：执行时间与创建时间并排一行，Token 用量独占下一行（进度条随之拉满）。 */
    .goal-metrics {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }

    .goal-metric:nth-child(2) {
      padding-right: 0;
      border-right: 0;
    }

    .goal-metric--budget {
      grid-column: 1 / -1;
      padding-left: 0;
      border-top: 1px solid color-mix(in srgb, var(--border) 70%, transparent);
    }
  }
</style>
