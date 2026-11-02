<script lang="ts">
  /**
   * 代理列表的一行：两行主信息 + 需要决策时的一行诊断。
   *
   * 第一行是谁、在做什么状态、多久；第二行是它的目标。模型、排队原因、失败详情不再平铺：
   * 模型放进悬停提示，排队原因与失败详情只在真的发生时多出一行。
   */
  import Icon from './Icon.svelte';
  import { getAgentBrandInfo, getAgentRunColor } from '../lib/agent-colors';
  import { agentModelLabel } from '../lib/agent-model-display';
  import { agentDurationSeconds, formatAgentDuration } from '../lib/active-agent-center';
  import { resolveAgentDisplayName } from '../lib/agent-role-utils';
  import type { IconName } from '../lib/icons';
  import type { AgentProjectionDto, TaskStatus } from '../shared/rust-backend-types';
  import { i18n } from '../stores/i18n.svelte';
  import { getEnabledAgents, getState } from '../stores/messages.svelte';
  import { uiClockState } from '../stores/ui-clock.svelte';

  interface Props {
    agent: AgentProjectionDto;
    selected?: boolean;
    /** 该次运行的任务投影，用来解析实际使用的模型。 */
    modelSelection?: Parameters<typeof agentModelLabel>[0];
    onOpen: (agent: AgentProjectionDto, label: string) => void;
  }

  let { agent, selected = false, modelSelection, onOpen }: Props = $props();

  const enabledAgents = $derived(getEnabledAgents());
  const registrySnapshot = $derived(getState().settingsRegistrySnapshot);
  const visual = $derived({
    ...getAgentBrandInfo(agent.role, (key) => i18n.t(key)),
    ...getAgentRunColor(agent.agentRunId),
  });
  const name = $derived(agent.displayName.trim() || visual.label);
  const role = $derived(
    resolveAgentDisplayName(agent.role, enabledAgents, registrySnapshot, (key) => i18n.t(key)) || visual.label,
  );
  const model = $derived(agentModelLabel(modelSelection, (key) => i18n.t(key)) || agent.model || '');

  const tone = $derived.by(() => {
    if (agent.lifecycle === 'degraded' || agent.status === 'failed') return 'attention';
    if (agent.status === 'running') return 'running';
    if (agent.status === 'pending') return 'pending';
    return 'completed';
  });

  const icon = $derived.by((): { name: IconName; spinning: boolean } => {
    if (agent.lifecycle === 'degraded') return { name: 'alert-triangle', spinning: false };
    switch (agent.status) {
      case 'pending': return { name: 'taskPending', spinning: false };
      case 'running': return { name: 'loader', spinning: true };
      case 'completed': return { name: 'check-circle', spinning: false };
      case 'failed': return { name: 'x-circle', spinning: false };
      case 'killed': return { name: 'stop', spinning: false };
      default: return { name: 'circle', spinning: false };
    }
  });

  const statusText = $derived.by(() => {
    if (agent.lifecycle === 'degraded') return i18n.t('activeAgentCenter.status.degraded');
    const labels: Record<TaskStatus, string> = {
      pending: i18n.t('activeAgentCenter.status.pending'),
      running: i18n.t('activeAgentCenter.status.running'),
      completed: i18n.t('activeAgentCenter.status.completed'),
      failed: i18n.t('activeAgentCenter.status.failed'),
      killed: i18n.t('activeAgentCenter.status.killed'),
    };
    return labels[agent.status];
  });

  function translated(prefix: string, value: string | null | undefined): string {
    if (!value) return '';
    const text = i18n.t(`${prefix}.${value}`);
    return text === `${prefix}.${value}` ? value : text;
  }

  const duration = $derived(formatAgentDuration(agentDurationSeconds(agent, uiClockState.now)));
  const diagnostic = $derived.by(() => {
    if (agent.queueReason) return agent.queueReason;
    if (!agent.failureCode && !agent.failureStage) return '';
    return [
      translated('activeAgentCenter.failureCode', agent.failureCode) || i18n.t('activeAgentCenter.status.failed'),
      translated('activeAgentCenter.failureStage', agent.failureStage),
      agent.failureMessage ?? '',
    ].filter(Boolean).join(' · ');
  });
</script>

<button
  type="button"
  class="agent-row agent-row--{tone}"
  class:selected
  style="--agent-color: {visual.color}; --agent-muted: {visual.muted};"
  title={model ? `${name} · ${model}` : name}
  data-agent-run-id={agent.agentRunId}
  onclick={() => onOpen(agent, name)}
>
  <span class="agent-avatar" aria-hidden="true">
    <Icon name={visual.icon} size={14} />
    <span class="agent-status-dot agent-status-dot--{tone}"></span>
  </span>
  <span class="agent-copy">
    <span class="agent-line">
      <span class="agent-name">
        <strong>{name}</strong>
        <span class="agent-role">{role}</span>
      </span>
      <span class="agent-status agent-status--{tone}">
        <Icon name={icon.name} size={12} class={icon.spinning ? 'spinning' : ''} />
        <span>{statusText}</span>
        <span class="agent-duration">{duration}</span>
      </span>
    </span>
    {#if agent.goal}<span class="agent-goal">{agent.goal}</span>{/if}
    {#if diagnostic}<span class="agent-diagnostic agent-diagnostic--{tone}">{diagnostic}</span>{/if}
  </span>
</button>

<style>
  .agent-row {
    display: flex;
    align-items: flex-start;
    gap: var(--space-4);
    box-sizing: border-box;
    width: 100%;
    min-height: 44px;
    padding: var(--space-3) var(--space-4);
    border: none;
    border-radius: var(--radius-lg);
    background: transparent;
    color: var(--foreground);
    font: inherit;
    text-align: start;
    cursor: pointer;
    transition: background var(--transition-fast);
  }

  .agent-row:hover {
    background: var(--surface-hover);
  }

  .agent-row.selected {
    background: var(--surface-2);
    box-shadow: inset 0 0 0 1px var(--border-subtle);
  }

  .agent-row:focus-visible {
    outline: none;
    box-shadow: inset 0 0 0 2px var(--primary);
  }

  .agent-avatar {
    position: relative;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    flex: 0 0 auto;
    width: 28px;
    height: 28px;
    border-radius: var(--radius-md);
    background: var(--agent-muted);
    color: var(--agent-color);
  }

  .agent-status-dot {
    position: absolute;
    right: -2px;
    bottom: -2px;
    width: 8px;
    height: 8px;
    border: 1.5px solid var(--background);
    border-radius: 50%;
    background: var(--foreground-muted);
  }

  .agent-status-dot--running { background: var(--success); }
  .agent-status-dot--pending { background: var(--warning); }
  .agent-status-dot--attention { background: var(--error); }

  .agent-copy {
    display: flex;
    flex: 1;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }

  .agent-line {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: var(--space-3);
    min-width: 0;
  }

  .agent-name {
    display: flex;
    align-items: baseline;
    gap: var(--space-3);
    min-width: 0;
  }

  .agent-name strong {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-sm);
    font-weight: var(--font-semibold);
  }

  .agent-role {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
  }

  .agent-status {
    display: inline-flex;
    flex: 0 0 auto;
    align-items: center;
    gap: var(--space-2);
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }

  .agent-status--running { color: var(--success); }
  .agent-status--pending { color: var(--warning); }
  .agent-status--attention { color: var(--error); }

  .agent-duration {
    color: var(--foreground-muted);
  }

  .agent-goal,
  .agent-diagnostic {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
  }

  .agent-diagnostic--attention {
    color: var(--error);
  }

  @media (hover: none), (pointer: coarse) {
    .agent-row {
      min-height: 48px;
    }
  }
</style>
