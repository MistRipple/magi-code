<script lang="ts">
  import { onMount } from 'svelte';
  import type {
    AgentProjectionDto,
    AgentRunProjectionDto,
    TaskStatus,
  } from '../shared/rust-backend-types';
  import {
    agentDurationSeconds,
    buildActiveAgentSummary,
    formatAgentDuration,
    groupActiveAgents,
    shouldPinAgentProjection,
    shouldShowActiveAgentCenter,
  } from '../lib/active-agent-center';
  import { getAgentVisualInfo } from '../lib/agent-colors';
  import { resolveAgentDisplayName } from '../lib/agent-role-utils';
  import type { IconName } from '../lib/icons';
  import { i18n } from '../stores/i18n.svelte';
  import {
    ensureAgentRunState,
    fetchAgentRunProjection,
    getAgentRunState,
    selectAgentRun,
  } from '../stores/agent-run-store.svelte';
  import {
    getEnabledAgents,
    getState,
    messagesState,
  } from '../stores/messages.svelte';
  import { openAgentTab } from '../stores/right-pane.svelte';
  import Icon from './Icon.svelte';

  const MOBILE_BREAKPOINT = 768;
  const STORAGE_PREFIX = 'magi.active-agent-center.v1';

  interface PersistedAgentCenterState {
    pinnedRootTaskId: string;
    dismissedRootTaskId: string;
  }

  let expanded = $state(false);
  let completedExpanded = $state(false);
  let mobile = $state(typeof window !== 'undefined' && window.innerWidth <= MOBILE_BREAKPOINT);
  let nowMs = $state(Date.now());
  let pinnedProjection: AgentRunProjectionDto | null = $state(null);
  let pinnedRootTaskId = $state('');
  let dismissedRootTaskId = $state('');
  let activeScopeKey = '';
  let lastAutoOpenedRootTaskId = '';
  let centerRoot: HTMLDivElement | undefined = $state();

  const enabledAgents = $derived(getEnabledAgents());
  const appState = getState();
  const registrySnapshot = $derived(appState.settingsRegistrySnapshot);
  const currentSessionId = $derived(messagesState.currentSessionId);
  const currentWorkspaceId = $derived(messagesState.currentWorkspaceId);
  const currentWorkspacePath = $derived(messagesState.currentWorkspacePath);
  const agentRunState = $derived(getAgentRunState(currentSessionId, currentWorkspaceId));
  const currentProjection = $derived(agentRunState.projection);
  const agentGroups = $derived.by(() => groupActiveAgents(pinnedProjection?.agents ?? []));
  const summary = $derived.by(() => buildActiveAgentSummary(agentGroups));
  const visible = $derived(shouldShowActiveAgentCenter(agentGroups));

  function normalizedScopePart(value: string | null | undefined): string {
    return typeof value === 'string' ? value.trim() : '';
  }

  function agentCenterScopeKey(): string {
    const sessionId = normalizedScopePart(currentSessionId);
    if (!sessionId) return '';
    const workspace = normalizedScopePart(currentWorkspaceId)
      || normalizedScopePart(currentWorkspacePath);
    return workspace ? `${workspace}\u0000${sessionId}` : `session:${sessionId}`;
  }

  function storageKey(scopeKey: string): string {
    return `${STORAGE_PREFIX}:${scopeKey}`;
  }

  function pinnedProjectionRootId(): string {
    return pinnedProjection?.root_task.task_id?.trim() ?? '';
  }

  function readPersistedState(scopeKey: string): PersistedAgentCenterState {
    if (!scopeKey || typeof localStorage === 'undefined') {
      return { pinnedRootTaskId: '', dismissedRootTaskId: '' };
    }
    try {
      const value = JSON.parse(localStorage.getItem(storageKey(scopeKey)) ?? '{}') as Partial<PersistedAgentCenterState>;
      return {
        pinnedRootTaskId: normalizedScopePart(value.pinnedRootTaskId),
        dismissedRootTaskId: normalizedScopePart(value.dismissedRootTaskId),
      };
    } catch {
      localStorage.removeItem(storageKey(scopeKey));
      return { pinnedRootTaskId: '', dismissedRootTaskId: '' };
    }
  }

  function persistState(scopeKey: string): void {
    if (!scopeKey || typeof localStorage === 'undefined') return;
    const value: PersistedAgentCenterState = {
      pinnedRootTaskId,
      dismissedRootTaskId,
    };
    localStorage.setItem(storageKey(scopeKey), JSON.stringify(value));
  }

  $effect(() => {
    ensureAgentRunState(currentSessionId, currentWorkspaceId, currentWorkspacePath);
  });

  $effect(() => {
    const scopeKey = agentCenterScopeKey();
    if (scopeKey === activeScopeKey) return;
    activeScopeKey = scopeKey;
    pinnedProjection = null;
    pinnedRootTaskId = '';
    dismissedRootTaskId = '';
    if (!scopeKey) return;

    const persisted = readPersistedState(scopeKey);
    pinnedRootTaskId = persisted.pinnedRootTaskId;
    dismissedRootTaskId = persisted.dismissedRootTaskId;
    const sessionId = normalizedScopePart(currentSessionId);
    if (pinnedRootTaskId && pinnedRootTaskId !== dismissedRootTaskId && sessionId) {
      void fetchAgentRunProjection(
        sessionId,
        pinnedRootTaskId,
        currentWorkspaceId,
        currentWorkspacePath,
      );
    }
  });

  $effect(() => {
    const candidate = currentProjection;
    const scopeKey = agentCenterScopeKey();
    const rootTaskId = candidate?.root_task.task_id?.trim() ?? '';
    const agents = candidate?.agents ?? [];
    if (!candidate || !scopeKey || !shouldPinAgentProjection(
      rootTaskId,
      agents.length,
      dismissedRootTaskId,
    )) {
      return;
    }
    pinnedProjection = candidate;
    pinnedRootTaskId = rootTaskId;
    if (dismissedRootTaskId && dismissedRootTaskId !== rootTaskId) {
      dismissedRootTaskId = '';
    }
    persistState(scopeKey);
  });

  $effect(() => {
    const rootTaskId = visible ? pinnedProjectionRootId() : '';
    if (!rootTaskId) {
      expanded = false;
      lastAutoOpenedRootTaskId = '';
      return;
    }
    if (rootTaskId !== lastAutoOpenedRootTaskId) {
      lastAutoOpenedRootTaskId = rootTaskId;
      completedExpanded = false;
      expanded = !mobile;
    }
  });

  onMount(() => {
    const durationTimer = window.setInterval(() => {
      nowMs = Date.now();
    }, 1_000);
    const updateViewport = () => {
      mobile = window.innerWidth <= MOBILE_BREAKPOINT;
    };
    const closeOnOutsidePointer = (event: PointerEvent) => {
      if (!expanded || mobile || centerRoot?.contains(event.target as Node)) {
        return;
      }
      expanded = false;
    };
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        expanded = false;
      }
    };
    updateViewport();
    window.addEventListener('resize', updateViewport);
    document.addEventListener('pointerdown', closeOnOutsidePointer);
    document.addEventListener('keydown', closeOnEscape);
    return () => {
      window.clearInterval(durationTimer);
      window.removeEventListener('resize', updateViewport);
      document.removeEventListener('pointerdown', closeOnOutsidePointer);
      document.removeEventListener('keydown', closeOnEscape);
    };
  });

  function roleColorToken(role: string): string | undefined {
    const normalizedRole = role.trim();
    if (!normalizedRole) return undefined;
    return enabledAgents.find((agent) => agent.templateId.trim() === normalizedRole)?.colorToken;
  }

  function agentVisual(agent: AgentProjectionDto) {
    return getAgentVisualInfo(agent.role, roleColorToken(agent.role));
  }

  function agentDisplayName(agent: AgentProjectionDto): string {
    return agent.displayName.trim() || agentVisual(agent).label;
  }

  function agentRoleLabel(agent: AgentProjectionDto): string {
    return resolveAgentDisplayName(
      agent.role,
      enabledAgents,
      registrySnapshot,
      (key) => i18n.t(key),
    ) || agentVisual(agent).label;
  }

  function statusIcon(agent: AgentProjectionDto): { name: IconName; spinning: boolean } {
    if (agent.lifecycle === 'degraded') return { name: 'alert-triangle', spinning: false };
    switch (agent.status) {
      case 'pending': return { name: 'taskPending', spinning: false };
      case 'running': return { name: 'loader', spinning: true };
      case 'completed': return { name: 'check-circle', spinning: false };
      case 'failed': return { name: 'x-circle', spinning: false };
      case 'killed': return { name: 'stop', spinning: false };
      default: return { name: 'circle', spinning: false };
    }
  }

  function statusTone(agent: AgentProjectionDto): string {
    if (agent.lifecycle === 'degraded' || agent.status === 'failed') return 'attention';
    if (agent.status === 'running') return 'running';
    if (agent.status === 'pending') return 'pending';
    return 'completed';
  }

  function statusLabel(agent: AgentProjectionDto): string {
    if (agent.lifecycle === 'degraded') return i18n.t('activeAgentCenter.status.degraded');
    const labels: Record<TaskStatus, string> = {
      pending: i18n.t('activeAgentCenter.status.pending'),
      running: i18n.t('activeAgentCenter.status.running'),
      completed: i18n.t('activeAgentCenter.status.completed'),
      failed: i18n.t('activeAgentCenter.status.failed'),
      killed: i18n.t('activeAgentCenter.status.killed'),
    };
    return labels[agent.status];
  }

  function durationLabel(agent: AgentProjectionDto): string {
    return formatAgentDuration(agentDurationSeconds(agent, nowMs));
  }

  function summaryLabel(): string {
    if (summary.attentionCount > 0) {
      return i18n.t('activeAgentCenter.summaryAttention', {
        active: summary.activeCount,
        attention: summary.attentionCount,
      });
    }
    if (summary.activeCount > 0) {
      return i18n.t('activeAgentCenter.summaryRunning', { count: summary.activeCount });
    }
    return i18n.t('activeAgentCenter.summaryCompleted', { count: summary.completedCount });
  }

  async function openAgent(agent: AgentProjectionDto): Promise<void> {
    const sessionId = currentSessionId?.trim() ?? '';
    if (!sessionId) return;
    const rootTaskId = pinnedProjection?.root_task.task_id?.trim() ?? '';
    const loadedRootTaskId = agentRunState.projection?.root_task.task_id?.trim() ?? '';
    if (rootTaskId && rootTaskId !== loadedRootTaskId) {
      await fetchAgentRunProjection(
        sessionId,
        rootTaskId,
        currentWorkspaceId,
        currentWorkspacePath,
      );
    }
    const visual = agentVisual(agent);
    selectAgentRun(sessionId, agent.agentRunId, currentWorkspaceId, currentWorkspacePath);
    openAgentTab(sessionId, agent.agentRunId, {
      label: agentDisplayName(agent),
      accentToken: visual.color,
      workspaceId: currentWorkspaceId,
      workspacePath: currentWorkspacePath,
    });
    if (mobile) {
      expanded = false;
    }
  }

  function clearAndClose(): void {
    const scopeKey = agentCenterScopeKey();
    const rootTaskId = pinnedProjection?.root_task.task_id?.trim() ?? pinnedRootTaskId;
    if (!scopeKey || !rootTaskId) return;
    dismissedRootTaskId = rootTaskId;
    pinnedRootTaskId = '';
    pinnedProjection = null;
    completedExpanded = false;
    expanded = false;
    persistState(scopeKey);
  }
</script>

{#snippet agentRow(agent: AgentProjectionDto)}
  {@const visual = agentVisual(agent)}
  {@const icon = statusIcon(agent)}
  <button
    type="button"
    class:selected={agentRunState.selectedAgentRunId === agent.agentRunId}
    class="agent-row agent-row--{statusTone(agent)}"
    style="--agent-color: {visual.color}; --agent-muted: {visual.muted};"
    onclick={() => openAgent(agent)}
  >
    <span class="agent-avatar">
      <Icon name={visual.icon} size={14} />
      <span class="agent-status-dot agent-status-dot--{statusTone(agent)}"></span>
    </span>
    <span class="agent-copy">
      <span class="agent-title-line">
        <strong>{agentDisplayName(agent)}</strong>
        <span>{agentRoleLabel(agent)}</span>
      </span>
      <span class="agent-goal">{agent.goal}</span>
    </span>
    <span class="agent-status agent-status--{statusTone(agent)}">
      <Icon name={icon.name} size={12} class={icon.spinning ? 'spinning' : ''} />
      <span>{statusLabel(agent)} · {durationLabel(agent)}</span>
    </span>
  </button>
{/snippet}

{#if visible}
  <div class="active-agent-center" bind:this={centerRoot}>
    <button
      type="button"
      class:has-attention={summary.attentionCount > 0}
      class="agent-center-trigger"
      aria-expanded={expanded}
      aria-label={i18n.t('activeAgentCenter.title')}
      title={i18n.t('activeAgentCenter.title')}
      onclick={() => expanded = !expanded}
    >
      <Icon name="bot" size={15} />
      {#if summary.triggerCount > 0}
        <span>{summary.triggerCount}</span>
      {/if}
      <i></i>
    </button>

    {#if expanded}
      <button
        type="button"
        class="mobile-backdrop"
        aria-label={i18n.t('common.close')}
        onclick={() => expanded = false}
      ></button>
      <section class="agent-center-panel" aria-label={i18n.t('activeAgentCenter.title')}>
        <header class="panel-header">
          <span class="panel-heading">
            <strong>{i18n.t('activeAgentCenter.title')}</strong>
            <small>{summaryLabel()}</small>
          </span>
          <span class="panel-actions">
            <button
              type="button"
              class="panel-clear"
              title={i18n.t('activeAgentCenter.clearAndClose')}
              onclick={clearAndClose}
            >
              <Icon name="trash" size={12} />
              {i18n.t('activeAgentCenter.clearAndClose')}
            </button>
            <button
              type="button"
              class="panel-close"
              aria-label={i18n.t('activeAgentCenter.collapse')}
              title={i18n.t('activeAgentCenter.collapse')}
              onclick={() => expanded = false}
            >
              <Icon name={mobile ? 'chevron-down' : 'chevron-up'} size={14} />
            </button>
          </span>
        </header>

        <div class="panel-content">
          {#if agentGroups.running.length > 0}
            <section class="agent-group">
              <div class="group-heading">
                <span>{i18n.t('activeAgentCenter.running')}</span>
                <small>{agentGroups.running.length}</small>
              </div>
              {#each agentGroups.running as agent (agent.agentRunId)}
                {@render agentRow(agent)}
              {/each}
            </section>
          {/if}

          {#if agentGroups.attention.length > 0}
            <section class="agent-group agent-group--attention">
              <div class="group-heading">
                <span>{i18n.t('activeAgentCenter.attention')}</span>
                <small>{agentGroups.attention.length}</small>
              </div>
              {#each agentGroups.attention as agent (agent.agentRunId)}
                {@render agentRow(agent)}
              {/each}
            </section>
          {/if}

          {#if agentGroups.completed.length > 0}
            <section class="agent-group">
              <button
                type="button"
                class="group-heading group-heading--toggle"
                aria-expanded={completedExpanded}
                onclick={() => completedExpanded = !completedExpanded}
              >
                <span>{i18n.t('activeAgentCenter.completed')}</span>
                <span class="group-completed-meta">
                  <small>{agentGroups.completed.length}</small>
                  <Icon name="chevron-right" size={12} class={completedExpanded ? 'completed-chevron completed-chevron--open' : 'completed-chevron'} />
                </span>
              </button>
              {#if completedExpanded}
                {#each agentGroups.completed as agent (agent.agentRunId)}
                  {@render agentRow(agent)}
                {/each}
              {/if}
            </section>
          {/if}
        </div>
      </section>
    {/if}
  </div>
{/if}

<style>
  .active-agent-center {
    position: absolute;
    top: 12px;
    right: 16px;
    z-index: var(--z-sticky);
  }

  .agent-center-trigger {
    position: relative;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
    min-width: 34px;
    height: 32px;
    padding: 0 9px;
    border: 1px solid color-mix(in srgb, var(--border) 88%, transparent);
    border-radius: var(--radius-md);
    background: color-mix(in srgb, var(--background) 94%, transparent);
    color: var(--foreground);
    box-shadow: var(--shadow-md);
    backdrop-filter: blur(14px);
    -webkit-backdrop-filter: blur(14px);
    cursor: pointer;
  }

  .agent-center-trigger:hover {
    background: color-mix(in srgb, var(--surface-3) 72%, var(--background));
  }

  .agent-center-trigger > span {
    font-size: var(--text-xs);
    font-weight: var(--font-semibold);
  }

  .agent-center-trigger > i {
    position: absolute;
    top: 4px;
    right: 4px;
    width: 6px;
    height: 6px;
    border: 1.5px solid var(--background);
    border-radius: 50%;
    background: var(--success);
  }

  .agent-center-trigger.has-attention > i {
    background: var(--error);
  }

  .agent-center-panel {
    position: absolute;
    top: 40px;
    right: 0;
    width: min(334px, calc(100vw - 40px));
    max-height: min(600px, calc(100vh - 150px));
    display: flex;
    flex-direction: column;
    overflow: hidden;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--dropdown-bg);
    box-shadow: var(--shadow-xl);
  }

  .panel-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    min-height: 46px;
    padding: 0 9px 0 12px;
    border-bottom: 1px solid var(--border);
  }

  .panel-heading {
    display: flex;
    align-items: baseline;
    gap: 7px;
    min-width: 0;
  }

  .panel-heading strong {
    font-size: var(--text-base);
    font-weight: var(--font-semibold);
  }

  .panel-heading small,
  .group-heading small {
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    font-weight: var(--font-normal);
  }

  .panel-close {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 27px;
    height: 27px;
    border: 0;
    border-radius: var(--radius-md);
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
  }

  .panel-actions {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    flex-shrink: 0;
  }

  .panel-clear {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 27px;
    padding: 0 7px;
    border: 0;
    border-radius: var(--radius-md);
    background: transparent;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    cursor: pointer;
  }

  .panel-clear:hover {
    background: color-mix(in srgb, var(--error) 9%, transparent);
    color: var(--error);
  }

  .panel-close:hover {
    background: var(--surface-hover);
    color: var(--foreground);
  }

  .panel-content {
    min-height: 0;
    overflow-y: auto;
    padding: 6px;
    scrollbar-width: thin;
  }

  .agent-group + .agent-group {
    margin-top: 7px;
  }

  .agent-group--attention {
    padding: 3px;
    border: 1px solid color-mix(in srgb, var(--error) 22%, transparent);
    border-radius: var(--radius-md);
    background: color-mix(in srgb, var(--error) 5%, transparent);
  }

  .group-heading {
    display: flex;
    align-items: center;
    justify-content: space-between;
    width: 100%;
    height: 26px;
    padding: 0 6px;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    font-weight: var(--font-semibold);
  }

  .group-heading--toggle {
    border: 0;
    background: transparent;
    cursor: pointer;
    text-align: left;
  }

  .group-heading--toggle:hover {
    color: var(--foreground);
  }

  .group-completed-meta {
    display: inline-flex;
    align-items: center;
    gap: 5px;
  }

  :global(.completed-chevron) {
    transition: transform var(--transition-fast);
  }

  :global(.completed-chevron--open) {
    transform: rotate(90deg);
  }

  .agent-row {
    display: grid;
    grid-template-columns: 28px minmax(0, 1fr) auto;
    gap: 9px;
    align-items: center;
    width: 100%;
    min-height: 52px;
    padding: 7px;
    border: 0;
    border-radius: var(--radius-md);
    background: transparent;
    color: var(--foreground);
    text-align: left;
    cursor: pointer;
  }

  .agent-row:hover {
    background: var(--surface-hover);
  }

  .agent-row.selected {
    background: var(--surface-selected);
  }

  .agent-avatar {
    position: relative;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
    border-radius: var(--radius-md);
    background: var(--agent-muted);
    color: var(--agent-color);
  }

  .agent-status-dot {
    position: absolute;
    right: -1px;
    bottom: -1px;
    width: 7px;
    height: 7px;
    border: 2px solid var(--dropdown-bg);
    border-radius: 50%;
    background: var(--foreground-muted);
  }

  .agent-status-dot--running { background: var(--success); }
  .agent-status-dot--pending { background: var(--warning); }
  .agent-status-dot--attention { background: var(--error); }

  .agent-copy {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
  }

  .agent-title-line {
    display: flex;
    align-items: baseline;
    gap: 6px;
    min-width: 0;
  }

  .agent-title-line strong {
    overflow: hidden;
    color: var(--foreground);
    font-size: var(--text-sm);
    font-weight: var(--font-semibold);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .agent-title-line span,
  .agent-goal {
    color: var(--foreground-muted);
    font-size: var(--text-xs);
  }

  .agent-title-line span {
    flex-shrink: 0;
  }

  .agent-goal {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .agent-status {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    color: var(--foreground-muted);
    font-size: var(--text-2xs);
    white-space: nowrap;
  }

  .agent-status--attention { color: color-mix(in srgb, var(--error) 78%, var(--foreground)); }
  .agent-status--running { color: color-mix(in srgb, var(--success) 78%, var(--foreground)); }
  .agent-status--pending { color: color-mix(in srgb, var(--warning) 78%, var(--foreground)); }

  :global(.spinning) {
    animation: agent-center-spin 0.9s linear infinite;
  }

  .mobile-backdrop {
    display: none;
  }

  @keyframes agent-center-spin {
    to { transform: rotate(360deg); }
  }

  @media (max-width: 768px) {
    .active-agent-center {
      top: 8px;
      right: 10px;
    }

    .agent-center-trigger {
      min-width: 32px;
      height: 30px;
      padding: 0 8px;
    }

    .mobile-backdrop {
      position: fixed;
      inset: 0;
      z-index: var(--z-overlay-preview);
      display: block;
      border: 0;
      background: var(--overlay);
    }

    .agent-center-panel {
      position: fixed;
      top: auto;
      right: 8px;
      bottom: 8px;
      left: 8px;
      z-index: calc(var(--z-overlay-preview) + 1);
      width: auto;
      max-height: min(72vh, 620px);
      border-radius: var(--radius-lg);
    }

    .panel-header {
      min-height: 48px;
    }

    .agent-row {
      min-height: 56px;
    }
  }
</style>
