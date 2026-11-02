<script lang="ts">
  /**
   * 对话顶部的代理状态条：常驻在顶栏右侧，不占流式布局，所以代理出现和消失时对话内容不会跳动。
   *
   * 有代理在跑时显示转圈和“进行中”的数量，有异常时带警示点；没有代理时是弱化的静态入口。
   * 点击打开右栏的代理列表，而不是在对话上方弹出一个遮住消息的浮层。
   */
  import Icon from './Icon.svelte';
  import { summarizeActiveAgentRun } from '../lib/active-agent-center';
  import { activeAgentRun } from '../stores/active-agent-run.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { AGENTS_TAB_ID, getRightPaneState, rightPaneState } from '../stores/right-pane.svelte';

  const summary = $derived(activeAgentRun.summary);
  const outcome = $derived(activeAgentRun.outcome);
  const working = $derived(summary.activeCount + summary.queuedCount);
  const needsAttention = $derived(
    summary.attentionCount > 0
      || outcome === 'recoverable'
      || outcome === 'failed'
      || outcome === 'degraded'
      || outcome === 'killed',
  );
  const pane = $derived(getRightPaneState(rightPaneState.activeScopeKey));
  const open = $derived(!pane.collapsed && pane.activeTabId === AGENTS_TAB_ID);
  const label = $derived(
    summary.triggerCount > 0
      ? summarizeActiveAgentRun(summary, outcome, (key, vars) => i18n.t(key, vars))
      : i18n.t('rightPane.agents.title'),
  );
</script>

<button
  type="button"
  class="btn-icon btn-icon--lg header-btn agent-chip"
  class:btn-icon--active={open}
  class:agent-chip--working={working > 0}
  class:agent-chip--attention={needsAttention}
  title={label}
  aria-label={label}
  aria-pressed={open}
  data-testid="agent-status-chip"
  data-agent-count={summary.triggerCount}
  onclick={() => activeAgentRun.openList()}
>
  <Icon name={summary.activeCount > 0 ? 'loader' : 'bot'} size={14} class={summary.activeCount > 0 ? 'spinning' : ''} />
  {#if working > 0}<span class="agent-chip-count">{working}</span>{/if}
  {#if needsAttention}<i class="agent-chip-dot" aria-hidden="true"></i>{/if}
</button>

<style>
  .agent-chip {
    position: relative;
    width: auto;
    min-width: 32px;
    gap: var(--space-2);
    padding: 0 var(--space-3);
    color: var(--foreground-muted);
  }

  .agent-chip--working {
    color: var(--foreground);
  }

  .agent-chip-count {
    font-size: var(--text-xs);
    font-weight: var(--font-semibold);
    font-variant-numeric: tabular-nums;
  }

  .agent-chip-dot {
    position: absolute;
    top: 5px;
    right: 5px;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--foreground-muted);
  }

  .agent-chip--attention .agent-chip-dot {
    background: var(--error);
  }
</style>
