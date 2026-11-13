<script lang="ts">
  /**
   * 右栏里的代理列表：当前会话这一轮运行的全部代理，按状态分组。
   *
   * 数据与操作都来自 `activeAgentRun`（和恢复横幅同源）；
   * 这里只负责展示。会话继续由输入区承担，不混进列表。
   */
  import AgentRow from '../AgentRow.svelte';
  import Disclosure from '../Disclosure.svelte';
  import Icon from '../Icon.svelte';
  import { summarizeActiveAgentRun } from '../../lib/active-agent-center';
  import type { AgentProjectionDto } from '../../shared/rust-backend-types';
  import { activeAgentRun } from '../../stores/active-agent-run.svelte';
  import { i18n } from '../../stores/i18n.svelte';
  import { retainUiClock } from '../../stores/ui-clock.svelte';

  let completedExpanded = $state(false);

  const groups = $derived(activeAgentRun.groups);
  const summary = $derived(activeAgentRun.summary);
  const empty = $derived(summary.triggerCount === 0);
  const canArchive = $derived(
    activeAgentRun.outcome === 'completed' && activeAgentRun.availableActions.includes('archive'),
  );

  // 有代理在运行或排队时耗时每秒刷新；只在这个视图挂载且确有进行中的代理时注册时钟。
  $effect(() => {
    if (summary.activeCount + summary.queuedCount === 0) return;
    return retainUiClock();
  });

  function modelSelection(agent: AgentProjectionDto) {
    return activeAgentRun.tasks.find((task) => task.task_id === agent.agentRunId)?.executor_binding?.model_selection;
  }

  function open(agent: AgentProjectionDto, label: string): void {
    activeAgentRun.openAgent(agent, label);
  }
</script>

<div class="agent-list" data-testid="right-pane-agent-list">
  <header class="agent-list-header">
    <span class="agent-list-heading">
      <strong>{i18n.t('rightPane.agents.title')}</strong>
      {#if !empty}<small>{summarizeActiveAgentRun(summary, activeAgentRun.outcome, (key, vars) => i18n.t(key, vars))}</small>{/if}
    </span>
    {#if canArchive}
      <button
        type="button"
        class="btn btn--secondary btn--sm"
        disabled={activeAgentRun.actionInFlight !== null}
        onclick={() => void activeAgentRun.runAction('archive')}
      >
        <Icon name="eye-slash" size={12} />
        {i18n.t('activeAgentCenter.action.archive')}
      </button>
    {/if}
  </header>

  {#if activeAgentRun.actionError}
    <p class="agent-list-action-error" role="alert">{activeAgentRun.actionError}</p>
  {/if}

  {#if empty}
    <div class="agent-list-empty">
      <Icon name="bot" size={22} />
      <span>{i18n.t('rightPane.agents.empty')}</span>
      <small>{i18n.t('rightPane.agents.emptyHint')}</small>
    </div>
  {:else}
    {#each [
      { key: 'running', agents: groups.running, label: i18n.t('activeAgentCenter.running') },
      { key: 'queued', agents: groups.queued, label: i18n.t('activeAgentCenter.queued') },
      { key: 'attention', agents: groups.attention, label: i18n.t('activeAgentCenter.attention') },
    ] as group (group.key)}
      {#if group.agents.length > 0}
        <section class="agent-group" aria-label={group.label}>
          <div class="group-heading"><span>{group.label}</span><small>{group.agents.length}</small></div>
          {#each group.agents as agent (agent.agentRunId)}
            <AgentRow
              {agent}
              selected={activeAgentRun.selectedAgentRunId === agent.agentRunId}
              modelSelection={modelSelection(agent)}
              onOpen={open}
            />
          {/each}
        </section>
      {/if}
    {/each}

    {#if groups.completed.length > 0}
      <section class="agent-group" aria-label={i18n.t('activeAgentCenter.completed')}>
        <button
          type="button"
          class="group-heading group-heading--toggle"
          aria-expanded={completedExpanded}
          onclick={() => completedExpanded = !completedExpanded}
        >
          <span>{i18n.t('activeAgentCenter.completed')}</span>
          <span class="group-completed-meta">
            <small>{groups.completed.length}</small>
            <Icon name="chevron-right" size={12} class={completedExpanded ? 'completed-chevron completed-chevron--open' : 'completed-chevron'} />
          </span>
        </button>
        <Disclosure expanded={completedExpanded}>
          {#each groups.completed as agent (agent.agentRunId)}
            <AgentRow
              {agent}
              selected={activeAgentRun.selectedAgentRunId === agent.agentRunId}
              modelSelection={modelSelection(agent)}
              onOpen={open}
            />
          {/each}
        </Disclosure>
      </section>
    {/if}
  {/if}
</div>

<style>
  .agent-list-action-error {
    margin: 0;
    color: var(--error);
    font-size: var(--text-xs);
    overflow-wrap: anywhere;
  }
  .agent-list {
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
    box-sizing: border-box;
    width: 100%;
    max-width: 720px;
    height: 100%;
    margin: 0 auto;
    overflow-y: auto;
    padding: var(--space-6) var(--space-5);
  }

  .agent-list-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-4);
  }

  .agent-list-heading {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }

  .agent-list-heading strong {
    font-size: var(--text-sm);
    font-weight: var(--font-semibold);
  }

  .agent-list-heading small {
    color: var(--foreground-muted);
    font-size: var(--text-xs);
  }

  .agent-group {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  .group-heading {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0 var(--space-4);
    border: none;
    background: transparent;
    color: var(--foreground-muted);
    font: inherit;
    font-size: var(--text-xs);
    font-weight: var(--font-medium);
  }

  .group-heading--toggle {
    min-height: 32px;
    border-radius: var(--radius-md);
    cursor: pointer;
  }

  .group-heading--toggle:hover {
    background: var(--surface-hover);
  }

  .group-completed-meta {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
  }

  :global(.completed-chevron) {
    transition: transform var(--transition-fast);
  }

  :global(.completed-chevron--open) {
    transform: rotate(90deg);
  }

  .agent-list-empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-8) var(--space-5);
    color: var(--foreground-muted);
    text-align: center;
  }

  .agent-list-empty small {
    font-size: var(--text-xs);
  }
</style>
