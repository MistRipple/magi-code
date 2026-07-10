<script lang="ts">
  import {
    addToast,
    getEnabledAgents,
    getState,
    messagesState,
  } from '../stores/messages.svelte';
  import Icon from './Icon.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import type {
    AgentRunProjectionDto,
    GoalTodoItemDto,
    SessionGoalDto,
    TaskDto,
    TaskStatus,
  } from '../shared/rust-backend-types';
  import type { IconName } from '../lib/icons';
  import {
    getRunnerUserStateLabel,
    getRunnerUserStateTone,
    getRunnerUserStateTooltip,
    getTaskDisplayTitle,
    getTaskStatusLabel,
    isUserVisibleTaskKind,
  } from '../lib/task-labels';
  import { resolveAgentDisplayName } from '../lib/agent-role-utils';
  import {
    ensureAgentRunState,
    clearAgentRunProjection,
    fetchAgentRunProjection,
    getAgentRunState,
    getAgentRunStatusModifier,
    refreshAgentRunProjection,
  } from '../stores/agent-run-store.svelte';
  import {
    ensureGoalState,
    getGoalState,
    refreshCurrentGoal,
  } from '../stores/goal-store.svelte';
  import { RustDaemonClient } from '../shared/rust-daemon-client';
  import { resolveAgentBaseUrl } from '../web/agent-api';

  interface AgentRunAttentionSummary {
    title: string;
    hint: string;
  }

  const appState = getState();
  const enabledAgents = $derived(getEnabledAgents());
  const registrySnapshot = $derived(appState.settingsRegistrySnapshot);

  const currentSessionId = $derived(messagesState.currentSessionId);
  const currentWorkspaceId = $derived(messagesState.currentWorkspaceId);
  const currentWorkspacePath = $derived(messagesState.currentWorkspacePath);
  const agentRunState = $derived(getAgentRunState(currentSessionId, currentWorkspaceId));
  const hasAgentRunProjection = $derived(agentRunState.projection !== null);
  const hasAgentRunActivity = $derived(Boolean(agentRunState.rootTaskId || agentRunState.projection));

  let goalRequestScope = '';
  let goalDrawerExpanded = $state(false);
  let todoDrawerExpanded = $state(true);
  let agentRunDrawerExpanded = $state(true);
  let isEditingGoal = $state(false);
  let goalObjectiveDraft = $state('');
  let goalActionLoading = $state<'save' | 'pause' | 'resume' | 'clear' | null>(null);
  let runActionLoading = $state<'stop' | 'resume' | 'restart' | 'archive' | null>(null);

  $effect(() => {
    ensureAgentRunState(currentSessionId, currentWorkspaceId, currentWorkspacePathValue());
  });

  $effect(() => {
    ensureGoalState(currentSessionId, currentWorkspaceId, currentWorkspacePathValue());
  });

  const goalState = $derived(getGoalState(currentSessionId, currentWorkspaceId));
  const currentGoal = $derived<SessionGoalDto | null>(goalState.response?.goal ?? null);
  const currentGoalTodos = $derived<GoalTodoItemDto[]>(
    Array.isArray(goalState.response?.todoItems) ? goalState.response.todoItems : []
  );

  $effect(() => {
    if (!isEditingGoal) {
      goalObjectiveDraft = currentGoal?.objective ?? '';
    }
  });

  $effect(() => {
    const sessionId = currentSessionIdValue() ?? agentRunState.projection?.sessionId ?? null;
    const workspaceId = currentWorkspaceIdValue() || agentRunState.projection?.workspaceId || '';
    const workspacePath = currentWorkspacePathValue() || agentRunState.projection?.workspacePath || '';
    const scope = sessionId ? `${sessionScopeKey(workspaceId, sessionId)}:${workspacePath}` : '';
    if (goalRequestScope === scope) {
      return;
    }
    goalRequestScope = scope;
    ensureGoalState(sessionId, workspaceId, workspacePath);
    const refresh = () => void refreshCurrentGoal(sessionId, workspaceId, workspacePath);
    refresh();
    const timer = setInterval(refresh, 3000);
    return () => clearInterval(timer);
  });

  const agentRunTasks = $derived(agentRunState.projection?.tasks ?? []);
  const hasGoalTodos = $derived(currentGoalTodos.length > 0);
  const todoSummary = $derived.by(() => buildGoalTodoSummary(currentGoalTodos));
  const todoProgressPercent = $derived.by(() => {
    if (todoSummary.total <= 0) return 0;
    return Math.min(100, Math.max(0, Math.round((todoSummary.completed / todoSummary.total) * 100)));
  });
  const runUnitById = $derived.by(() => new Map(agentRunTasks.map((task) => [task.task_id, task])));
  const activeAgentRunTasks = $derived.by(() => (
    agentRunTasks.filter((task) => task.status !== 'killed')
  ));
  const childrenByParentId = $derived.by(() => {
    const grouped = new Map<string, TaskDto[]>();
    for (const task of activeAgentRunTasks) {
      if (!task.parent_task_id) continue;
      const siblings = grouped.get(task.parent_task_id) ?? [];
      siblings.push(task);
      grouped.set(task.parent_task_id, siblings);
    }
    return grouped;
  });
  const userVisibleTasks = $derived.by(() => (
    activeAgentRunTasks
      .filter((task) => isUserVisibleTaskKind(task.kind))
      .filter((task) => !isCoordinationEnvelopeRoot(task, agentRunState.projection))
      .slice()
      .sort((left, right) => {
        if (left.created_at !== right.created_at) return left.created_at - right.created_at;
        return left.task_id.localeCompare(right.task_id);
      })
  ));
  const displayedAgentRunTasks = $derived.by(() => {
    if (userVisibleTasks.length > 0) return userVisibleTasks;
    const rootTask = agentRunState.projection?.root_task;
    return rootTask && rootTask.status !== 'killed' ? [rootTask] : [];
  });
  const runSummary = $derived.by(() => buildRunSummary(agentRunState.projection, userVisibleTasks));
  const canResumeAgentRun = $derived.by(() => {
    const projection = agentRunState.projection;
    return projection?.runner_status === 'error'
      && projection.has_recoverable_chain === true
      && (projection.recoverable_branch_count ?? 0) > 0;
  });
  const canRestartAgentRun = $derived.by(() => {
    const status = agentRunState.projection?.runner_status;
    return status === 'completed' || status === 'error' || status === 'killed' || status === 'idle';
  });
  const canArchiveAgentRun = $derived.by(() => canRestartAgentRun);
  const attentionTasks = $derived.by(() => {
    const projection = agentRunState.projection;
    if (!projection) return [];
    const seen = new Set<string>();
    return (projection.failed_tasks ?? [])
      .filter((id) => {
        if (seen.has(id)) return false;
        seen.add(id);
        return true;
      })
      .map((id) => runUnitById.get(id))
      .filter((task): task is TaskDto => Boolean(task));
  });
  const attentionSummary = $derived.by(() => buildAgentRunAttentionSummary(
    agentRunState.projection,
    attentionTasks,
    canResumeAgentRun,
  ));
  const runnerBlockedReason = $derived(attentionSummary?.title ?? null);
  const progressPercent = $derived.by(() => {
    if (runSummary.total <= 0) return 0;
    return Math.min(100, Math.max(0, Math.round((runSummary.completed / runSummary.total) * 100)));
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

  function currentRootTaskId(): string | null {
    return agentRunState.projection?.root_task.task_id ?? null;
  }

  function getTaskExecutorDisplayName(task: TaskDto): string {
    const roleId = task.executor_binding?.target_role?.trim() ?? '';
    if (!roleId) return '';
    return resolveAgentDisplayName(roleId, enabledAgents, registrySnapshot, (key) => i18n.t(key)) || roleId;
  }

  function getTaskPerformerLabel(task: TaskDto): string {
    const executorName = getTaskExecutorDisplayName(task);
    if (executorName) return executorName;
    switch (task.kind) {
      case 'local_workflow': return i18n.t('goalPanel.performer.localWorkflow');
      case 'remote_agent': return i18n.t('goalPanel.performer.remoteAgent');
      case 'monitor_mcp': return 'MCP';
      case 'in_process_teammate': return i18n.t('goalPanel.performer.teammate');
      case 'dream': return i18n.t('goalPanel.performer.background');
      default: return i18n.t('goalPanel.performer.agent');
    }
  }

  function isCoordinationEnvelopeRoot(
    task: TaskDto,
    projection: AgentRunProjectionDto | null,
  ): boolean {
    if (!projection || task.task_id !== projection.root_task.task_id) {
      return false;
    }
    return (childrenByParentId.get(task.task_id) ?? [])
      .filter((child) => child.status !== 'killed')
      .some((child) => isUserVisibleTaskKind(child.kind));
  }

  function buildRunSummary(
    projection: AgentRunProjectionDto | null,
    visibleTasks: TaskDto[],
  ) {
    if (visibleTasks.length > 0) {
      return {
        total: visibleTasks.length,
        completed: visibleTasks.filter((task) => task.status === 'completed').length,
      };
    }
    const progress = projection?.progress_summary;
    return {
      total: progress?.total_tasks ?? 0,
      completed: progress?.completed_tasks ?? 0,
    };
  }

  function buildGoalTodoSummary(items: GoalTodoItemDto[]) {
    return {
      total: items.length,
      completed: items.filter((item) => item.status === 'completed').length,
      running: items.filter((item) => item.status === 'in_progress').length,
      pending: items.filter((item) => item.status === 'pending').length,
    };
  }

  function goalTodoStatusLabel(status: GoalTodoItemDto['status']): string {
    switch (status) {
      case 'completed': return i18n.t('goalPanel.todo.status.completed');
      case 'in_progress': return i18n.t('goalPanel.todo.status.inProgress');
      case 'pending': return i18n.t('goalPanel.todo.status.pending');
      default: return status;
    }
  }

  function goalTodoStatusIcon(status: GoalTodoItemDto['status']): IconName {
    switch (status) {
      case 'completed': return 'check-circle';
      case 'in_progress': return 'loader';
      case 'pending': return 'circle';
      default: return 'circle';
    }
  }

  function goalTodoMeta(todo: GoalTodoItemDto): string {
    if (todo.status !== 'in_progress') {
      return goalTodoStatusLabel(todo.status);
    }
    const activeForm = todo.activeForm.trim();
    return activeForm && activeForm !== todo.content.trim()
      ? activeForm
      : goalTodoStatusLabel(todo.status);
  }

  function buildAgentRunAttentionSummary(
    projection: AgentRunProjectionDto | null,
    failedTasks: TaskDto[],
    canResume: boolean,
  ): AgentRunAttentionSummary | null {
    if (!projection) return null;
    const failedCount = failedTasks.length;
    if (projection.runner_status !== 'error' && failedCount === 0) return null;

    const rootTaskId = projection.root_task.task_id;
    const rootFailed = failedTasks.some((task) => task.task_id === rootTaskId);
    const agentFailedCount = failedTasks
      .filter((task) => task.kind === 'local_agent' && task.task_id !== rootTaskId)
      .length;

    let title = i18n.t('goalPanel.attention.executionIncomplete');
    if (rootFailed && agentFailedCount > 0) {
      title = i18n.t('goalPanel.attention.mainAndAgentsIncomplete', { count: agentFailedCount });
    } else if (rootFailed) {
      title = i18n.t('goalPanel.attention.mainIncomplete');
    } else if (agentFailedCount > 0 && agentFailedCount === failedCount) {
      title = i18n.t('goalPanel.attention.agentsIncomplete', { count: agentFailedCount });
    } else if (failedCount > 0) {
      title = i18n.t('goalPanel.attention.tasksIncomplete', { count: failedCount });
    }

    return {
      title,
      hint: canResume
        ? i18n.t('goalPanel.attention.resumeHint')
        : i18n.t('goalPanel.attention.restartHint'),
    };
  }

  function goalStatusLabel(status: string): string {
    switch (status) {
      case 'active': return i18n.t('goalPanel.goal.statusActive');
      case 'paused': return i18n.t('goalPanel.goal.statusPaused');
      case 'blocked': return i18n.t('goalPanel.goal.statusBlocked');
      case 'usage_limited': return i18n.t('goalPanel.goal.statusUsageLimited');
      case 'budget_limited': return i18n.t('goalPanel.goal.statusBudgetLimited');
      case 'complete': return i18n.t('goalPanel.goal.statusComplete');
      case 'cleared': return i18n.t('goalPanel.goal.statusCleared');
      default: return status;
    }
  }

  function goalStatusIcon(status: string): IconName {
    switch (status) {
      case 'complete': return 'check-circle';
      case 'paused': return 'pause';
      case 'blocked':
      case 'usage_limited':
      case 'budget_limited': return 'alert-triangle';
      default: return 'target';
    }
  }

  function goalCanEdit(goal: SessionGoalDto): boolean {
    return goal.status !== 'complete' && goal.status !== 'cleared';
  }

  function goalCanPause(goal: SessionGoalDto): boolean {
    return goal.status === 'active' || goal.status === 'usage_limited' || goal.status === 'budget_limited';
  }

  function goalCanResume(goal: SessionGoalDto): boolean {
    return goal.status === 'paused' || goal.status === 'blocked';
  }

  function goalBudgetLabel(tokensUsed: number, tokenBudget?: number | null): string {
    const used = Number.isFinite(tokensUsed) ? Math.max(0, Math.round(tokensUsed)) : 0;
    if (!tokenBudget || tokenBudget <= 0) {
      return `${used.toLocaleString()} tokens`;
    }
    return `${used.toLocaleString()} / ${Math.round(tokenBudget).toLocaleString()} tokens`;
  }

  function goalRemainingTokenLabel(tokensUsed: number, tokenBudget?: number | null): string {
    if (!tokenBudget || tokenBudget <= 0) {
      return i18n.t('common.unlimited');
    }
    const used = Number.isFinite(tokensUsed) ? Math.max(0, Math.round(tokensUsed)) : 0;
    const remaining = Math.max(0, Math.round(tokenBudget) - used);
    return `${remaining.toLocaleString()} tokens`;
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
    return {
      sessionId: currentSessionIdValue() ?? '',
      workspaceId: currentWorkspaceIdValue(),
      workspacePath: currentWorkspacePathValue(),
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
    await runGoalAction('resume', async () => {
      await createClient().resumeCurrentGoal(goalActionRequest());
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
      await createClient().clearCurrentGoal(goalActionRequest());
      await refreshGoalAfterMutation();
      isEditingGoal = false;
      addToast('info', i18n.t('goalPanel.action.goalCleared'));
    }).catch((err) => {
      console.warn('[GoalRunDrawers] goal clear failed:', err);
      addToast('error', i18n.t('goalPanel.action.goalClearFailed'));
    });
  }

  function getAgentRunStatusIcon(status: TaskStatus): { name: IconName; spinning: boolean } {
    switch (status) {
      case 'running': return { name: 'loader', spinning: true };
      case 'completed': return { name: 'check-circle', spinning: false };
      case 'failed': return { name: 'x-circle', spinning: false };
      case 'killed': return { name: 'skip-forward', spinning: false };
      case 'pending': return { name: 'circleOutline', spinning: false };
      default: return { name: 'circleOutline', spinning: false };
    }
  }

  async function runAgentRunAction(
    action: 'stop' | 'resume' | 'restart' | 'archive',
    task: () => Promise<void>,
  ) {
    if (runActionLoading) return;
    runActionLoading = action;
    try {
      await task();
    } finally {
      if (runActionLoading === action) {
        runActionLoading = null;
      }
    }
  }

  function reportAgentRunActionFailure(labelKey: string, error: unknown): void {
    console.warn('[GoalRunDrawers] agent run action failed:', error);
    addToast('error', i18n.t(labelKey));
  }

  async function stopCurrentAgentRun() {
    const sessionId = currentSessionIdValue();
    const rootTaskId = currentRootTaskId();
    if (!sessionId || !rootTaskId) return;
    await runAgentRunAction('stop', async () => {
      const client = createClient();
      await client.interruptAgentRun({
        taskId: rootTaskId,
        sessionId,
        workspaceId: currentWorkspaceIdValue(),
        workspacePath: currentWorkspacePathValue(),
      });
      await refreshAgentRunProjection(sessionId, currentWorkspaceIdValue(), currentWorkspacePathValue());
      addToast('info', i18n.t('goalPanel.action.stopped'));
    }).catch((err) => {
      reportAgentRunActionFailure('goalPanel.action.stopFailed', err);
    });
  }

  async function resumeCurrentAgentRun() {
    const sessionId = currentSessionIdValue();
    const rootTaskId = currentRootTaskId();
    if (!sessionId || !rootTaskId) return;
    await runAgentRunAction('resume', async () => {
      const client = createClient();
      await client.continueSession({
        sessionId,
        workspaceId: currentWorkspaceIdValue(),
        workspacePath: currentWorkspacePathValue(),
      });
      await refreshAgentRunProjection(sessionId, currentWorkspaceIdValue(), currentWorkspacePathValue());
      addToast('success', i18n.t('goalPanel.action.resumed'));
    }).catch((err) => {
      reportAgentRunActionFailure('goalPanel.action.resumeFailed', err);
    });
  }

  async function restartCurrentAgentRun() {
    const sessionId = currentSessionIdValue();
    const rootTaskId = currentRootTaskId();
    if (!sessionId || !rootTaskId) return;
    await runAgentRunAction('restart', async () => {
      const client = createClient();
      const result = await client.restartAgentRun({
        taskId: rootTaskId,
        sessionId,
        workspaceId: currentWorkspaceIdValue(),
        workspacePath: currentWorkspacePathValue(),
      });
      if (result.rootTaskId) {
        await fetchAgentRunProjection(sessionId, result.rootTaskId, currentWorkspaceIdValue(), currentWorkspacePathValue());
      } else {
        await refreshAgentRunProjection(sessionId, currentWorkspaceIdValue(), currentWorkspacePathValue());
      }
      addToast('success', i18n.t('goalPanel.action.restarted'));
    }).catch((err) => {
      reportAgentRunActionFailure('goalPanel.action.restartFailed', err);
    });
  }

  async function archiveCurrentAgentRun() {
    const sessionId = currentSessionIdValue();
    const rootTaskId = currentRootTaskId();
    if (!sessionId || !rootTaskId) return;
    await runAgentRunAction('archive', async () => {
      const client = createClient();
      await client.archiveAgentRun({
        taskId: rootTaskId,
        sessionId,
        workspaceId: currentWorkspaceIdValue(),
        workspacePath: currentWorkspacePathValue(),
      });
      clearAgentRunProjection(sessionId, rootTaskId, currentWorkspaceIdValue());
      addToast('info', i18n.t('goalPanel.action.archived'));
    }).catch((err) => {
      reportAgentRunActionFailure('goalPanel.action.archiveFailed', err);
    });
  }
</script>

{#if currentGoal || hasGoalTodos || hasAgentRunProjection || agentRunState.error || (agentRunState.loading && hasAgentRunActivity)}
<div class="goal-run-drawers">
  {#if hasGoalTodos}
    <section class="run-drawer todo-panel" data-testid="todo-card" aria-label={i18n.t('goalPanel.todo.title')}>
      <div class="run-drawer-header">
        <button
          type="button"
          class="run-drawer-toggle"
          aria-expanded={todoDrawerExpanded}
          onclick={() => todoDrawerExpanded = !todoDrawerExpanded}
        >
          <span class="drawer-leading-icon drawer-leading-icon--todo"><Icon name="list" size={14} /></span>
          <span class="run-drawer-title">{i18n.t('goalPanel.todo.title')}</span>
          <span class="run-progress-count">
            {i18n.t('goalPanel.progress.completedCount', {
              completed: todoSummary.completed,
              total: todoSummary.total,
            })}
          </span>
          {#if todoSummary.running > 0}
            <span class="todo-running">{i18n.t('goalPanel.todo.runningCount', { count: todoSummary.running })}</span>
          {/if}
          <Icon name="chevron-right" size={13} class={todoDrawerExpanded ? 'drawer-chevron drawer-chevron--open' : 'drawer-chevron'} />
        </button>
      </div>

      {#if todoDrawerExpanded}
        <div class="run-progress-bar todo-progress-bar" aria-hidden="true">
          <span style="width: {todoProgressPercent}%"></span>
        </div>
        <div class="run-list todo-list" role="list">
          {#each currentGoalTodos as todo, index (`${index}:${todo.content}`)}
            {@const todoIcon = goalTodoStatusIcon(todo.status)}
            <div class="run-row run-row--todo run-row--{todo.status}" role="listitem">
              <span class="run-row-icon status-icon--{todo.status}" aria-label={goalTodoStatusLabel(todo.status)}>
                <Icon name={todoIcon} size={15} class={todo.status === 'in_progress' ? 'spinning' : ''} />
              </span>
              <span class="run-row-main">
                <span class="run-row-title">{todo.content}</span>
                <span class="run-row-meta">{goalTodoMeta(todo)}</span>
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
          <span class="drawer-leading-icon goal-status-icon"><Icon name={goalStatusIcon(currentGoal.status)} size={14} /></span>
          <span class="goal-heading">
            <span class="goal-status-title">{goalStatusLabel(currentGoal.status)}</span>
            <span class="goal-objective">{currentGoal.objective}</span>
          </span>
          <span class="goal-meta">{goalTimeLabel(currentGoal.timeUsedSeconds)}</span>
          <Icon name="chevron-right" size={13} class={goalDrawerExpanded ? 'drawer-chevron drawer-chevron--open' : 'drawer-chevron'} />
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
              disabled={goalActionLoading !== null}
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
            <div class="goal-stat-strip">
              <span class="goal-detail-item">
                <span class="goal-detail-label">{i18n.t('goalPanel.goal.elapsed')}</span>
                <strong>{goalTimeLabel(currentGoal.timeUsedSeconds)}</strong>
              </span>
              <span class="goal-detail-item">
                <span class="goal-detail-label">{i18n.t('goalPanel.goal.budget')}</span>
                <strong>{goalBudgetLabel(currentGoal.tokensUsed, currentGoal.tokenBudget)}</strong>
              </span>
              <span class="goal-detail-item">
                <span class="goal-detail-label">{i18n.t('goalPanel.goal.remaining')}</span>
                <strong>{goalRemainingTokenLabel(currentGoal.tokensUsed, currentGoal.tokenBudget)}</strong>
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

  {#if hasAgentRunProjection}
    {@const projection = agentRunState.projection}
    {#if projection}
      <section class="run-drawer agent-run-panel" aria-label={i18n.t('goalPanel.progress.title')}>
        <div class="run-drawer-header">
          <button
            type="button"
            class="run-drawer-toggle"
            aria-expanded={agentRunDrawerExpanded}
            onclick={() => agentRunDrawerExpanded = !agentRunDrawerExpanded}
          >
            <Icon name="list" size={14} />
            <span class="run-drawer-title">{i18n.t('goalPanel.progress.title')}</span>
            {#if runSummary.total > 0}
              <span class="run-progress-count">
                {i18n.t('goalPanel.progress.completedCount', {
                  completed: runSummary.completed,
                  total: runSummary.total,
                })}
              </span>
            {/if}
            <Icon name="chevron-right" size={13} class={agentRunDrawerExpanded ? 'drawer-chevron drawer-chevron--open' : 'drawer-chevron'} />
          </button>

          <div class="run-actions">
            <span
              class="status-badge status-badge--{getRunnerUserStateTone(projection.runner_status)}"
              title={getRunnerUserStateTooltip(projection.runner_status, runnerBlockedReason) ?? ''}
            >
              {getRunnerUserStateLabel(projection.runner_status)}
            </span>
            {#if projection.runner_status === 'running'}
              <button
                type="button"
                class="run-action"
                disabled={runActionLoading !== null}
                onclick={stopCurrentAgentRun}
                title={i18n.t('goalPanel.action.stopTitle')}
              >
                <Icon name={runActionLoading === 'stop' ? 'loader' : 'stop'} size={12} class={runActionLoading === 'stop' ? 'spinning' : ''} />
                <span>{i18n.t('goalPanel.action.stop')}</span>
              </button>
            {:else if canResumeAgentRun}
              <button
                type="button"
                class="run-action"
                disabled={runActionLoading !== null}
                onclick={resumeCurrentAgentRun}
                title={i18n.t('goalPanel.action.resumeTitle')}
              >
                <Icon name={runActionLoading === 'resume' ? 'loader' : 'play'} size={12} class={runActionLoading === 'resume' ? 'spinning' : ''} />
                <span>{i18n.t('goalPanel.action.resume')}</span>
              </button>
            {/if}
            {#if canRestartAgentRun}
              <button
                type="button"
                class="run-action"
                disabled={runActionLoading !== null}
                onclick={restartCurrentAgentRun}
                title={i18n.t('goalPanel.action.restartTitle')}
              >
                <Icon name={runActionLoading === 'restart' ? 'loader' : 'refresh'} size={12} class={runActionLoading === 'restart' ? 'spinning' : ''} />
                <span>{i18n.t('goalPanel.action.restart')}</span>
              </button>
            {/if}
            {#if canArchiveAgentRun}
              <button
                type="button"
                class="run-action run-action--quiet"
                disabled={runActionLoading !== null}
                onclick={archiveCurrentAgentRun}
                title={i18n.t('goalPanel.action.archiveTitle')}
              >
                <Icon name={runActionLoading === 'archive' ? 'loader' : 'eye-slash'} size={12} class={runActionLoading === 'archive' ? 'spinning' : ''} />
                <span>{i18n.t('goalPanel.action.archive')}</span>
              </button>
            {/if}
          </div>
        </div>

        {#if agentRunDrawerExpanded}
          {#if runSummary.total > 0}
            <div class="run-progress-bar" aria-hidden="true">
              <span style="width: {progressPercent}%"></span>
            </div>
          {/if}

          {#if attentionSummary}
            <div class="run-attention">
              <Icon name="alert-triangle" size={13} />
              <span>
                <strong>{attentionSummary.title}</strong>
                <em>{attentionSummary.hint}</em>
              </span>
            </div>
          {/if}

          {#if displayedAgentRunTasks.length > 0}
            <div class="run-list" role="list">
              {#each displayedAgentRunTasks as task (task.task_id)}
                {@const statusIcon = getAgentRunStatusIcon(task.status)}
                {@const performerLabel = getTaskPerformerLabel(task)}
                <div class="run-row run-row--{getAgentRunStatusModifier(task.status)}" role="listitem">
                  <span class="run-row-icon status-icon--{getAgentRunStatusModifier(task.status)}" aria-label={getTaskStatusLabel(task.status)}>
                    {#if statusIcon.spinning}
                      <Icon name={statusIcon.name} size={15} class="spinning" />
                    {:else}
                      <Icon name={statusIcon.name} size={15} />
                    {/if}
                  </span>
                  <span class="run-row-main">
                    <span class="run-row-title">{getTaskDisplayTitle(task)}</span>
                    <span class="run-row-meta">{performerLabel} · {getTaskStatusLabel(task.status)}</span>
                  </span>
                </div>
              {/each}
            </div>
          {/if}
        {/if}
      </section>
    {/if}
  {/if}

  {#if agentRunState.error}
    <div class="run-error">{i18n.t('goalPanel.projectionLoadFailed')}</div>
  {/if}

  {#if agentRunState.loading && hasAgentRunActivity && !hasAgentRunProjection}
    <div class="run-loading" role="status" aria-live="polite">
      <Icon name="loader" size={16} class="spinning" />
      <span>{i18n.t('common.loading')}</span>
    </div>
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

  .goal-panel--paused,
  .goal-panel--cleared {
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

  .todo-panel {
    order: 1;
  }

  .agent-run-panel {
    order: 2;
    background: color-mix(in srgb, var(--background) 88%, var(--surface-1));
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
  .run-action:focus-visible,
  .goal-edit-button:focus-visible,
  .goal-edit-input:focus-visible {
    outline: 2px solid color-mix(in srgb, var(--primary) 58%, transparent);
    outline-offset: 2px;
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

  .drawer-leading-icon--todo {
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

  .drawer-chevron {
    margin-left: auto;
    transform: rotate(0deg);
    opacity: 0.55;
    transition: transform var(--transition-fast), opacity var(--transition-fast);
  }

  .run-drawer-toggle:hover .drawer-chevron {
    opacity: 0.85;
  }

  .drawer-chevron--open {
    transform: rotate(90deg);
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

  .goal-meta,
  .run-actions {
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

  .todo-running {
    flex: 0 0 auto;
    color: var(--primary);
    font-size: var(--text-2xs);
    font-weight: var(--font-medium);
    white-space: nowrap;
  }

  .run-actions {
    justify-content: flex-end;
    flex: 0 0 auto;
  }

  .run-action {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    height: 26px;
    padding: 0 var(--space-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--foreground);
    font-size: var(--text-2xs);
    font-weight: var(--font-medium);
    cursor: pointer;
    transition:
      background var(--transition-fast),
      border-color var(--transition-fast),
      color var(--transition-fast);
  }

  .run-action:hover:not(:disabled) {
    border-color: color-mix(in srgb, var(--primary) 30%, var(--border));
    color: var(--primary);
  }

  .run-action--quiet {
    color: var(--foreground-muted);
  }

  .run-action:disabled {
    opacity: 0.55;
    cursor: not-allowed;
  }

  .status-badge {
    flex-shrink: 0;
    padding: 2px 8px;
    border: 1px solid transparent;
    border-radius: var(--radius-full);
    font-size: var(--text-2xs);
    white-space: nowrap;
  }

  .status-badge--running {
    color: var(--primary);
    background: var(--primary-muted);
    border-color: color-mix(in srgb, var(--primary) 30%, var(--border));
  }

  .status-badge--completed {
    color: var(--success);
    background: var(--success-muted);
    border-color: color-mix(in srgb, var(--success) 32%, var(--border));
  }

  .status-badge--failed {
    color: var(--error);
    background: var(--error-muted);
    border-color: color-mix(in srgb, var(--error) 32%, var(--border));
  }

  .status-badge--pending,
  .status-badge--killed,
  .status-badge--unknown {
    color: var(--foreground-muted);
    background: transparent;
    border-color: var(--border);
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

  .todo-progress-bar span {
    background: var(--success);
  }

  .run-attention {
    display: flex;
    align-items: flex-start;
    gap: var(--space-2);
    min-height: 30px;
    padding: var(--space-1) var(--space-2);
    border: 1px solid color-mix(in srgb, var(--error) 24%, var(--border));
    border-radius: var(--radius-sm);
    background: var(--background);
    color: var(--foreground-muted);
    font-size: var(--text-2xs);
    line-height: 1.45;
  }

  .run-attention :global(svg) {
    flex: 0 0 auto;
    margin-top: 2px;
    color: var(--error);
  }

  .run-attention span {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
  }

  .run-attention strong {
    color: var(--foreground);
    font-size: var(--text-xs);
    font-style: normal;
    font-weight: var(--font-semibold);
  }

  .run-attention em {
    color: var(--foreground-muted);
    font-size: var(--text-2xs);
    font-style: normal;
  }

  .run-list {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
  }

  .todo-list,
  .agent-run-panel .run-list {
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

  .run-row--failed {
    border-color: color-mix(in srgb, var(--error) 24%, transparent);
  }

  .run-row--completed {
    opacity: 0.76;
  }

  .run-row--todo {
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

  .status-icon--running { color: var(--primary); }
  .status-icon--in_progress { color: var(--primary); }
  .status-icon--completed { color: var(--success); }
  .status-icon--failed { color: var(--error); }
  .status-icon--pending,
  .status-icon--killed,
  .status-icon--unknown { color: var(--foreground-muted); }

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

  .run-loading,
  .run-error {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius-md);
    font-size: var(--text-xs);
  }

  .run-loading {
    color: var(--foreground-muted);
    background: var(--surface-1);
  }

  .run-error {
    color: var(--error);
    background: var(--error-muted);
    border: 1px solid color-mix(in srgb, var(--error) 32%, var(--border));
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
    .todo-running,
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

    .run-actions {
      flex-wrap: nowrap;
      gap: 3px;
    }

    .run-action span,
    .status-badge {
      display: none;
    }

    .run-action {
      width: 28px;
      padding: 0;
      justify-content: center;
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
