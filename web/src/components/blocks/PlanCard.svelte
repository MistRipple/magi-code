<script lang="ts">
  import type { ContentBlock } from '../../types/message';
  import Icon from '../Icon.svelte';
  import { i18n } from '../../stores/i18n.svelte';

  interface Props {
    block: ContentBlock;
  }

  let { block }: Props = $props();
  const plan = $derived(block.plan);
  let isExpanded = $state(false);
</script>

{#if plan}
  <div class="plan-card" data-status={plan.riskLevel ?? 'default'}>
    <div class="plan-header">
      <span class="plan-title">
        <Icon name="note" size={14} />
        <span>{i18n.t('planCard.title')}</span>
      </span>
      <button class="toggle-btn" onclick={() => (isExpanded = !isExpanded)}>
        <Icon name={isExpanded ? 'chevron-up' : 'chevron-down'} size={14} />
      </button>
    </div>

    <div class="plan-body">
      <div class="plan-goal">{plan.goal}</div>

      {#if plan.analysis}
        <div class="plan-section">
          <div class="section-title">{i18n.t('planCard.analysis')}</div>
          <div class="section-body">{plan.analysis}</div>
        </div>
      {/if}

      {#if plan.constraints && plan.constraints.length > 0}
        <div class="plan-section">
          <div class="section-title">{i18n.t('planCard.constraints')}</div>
          <ul>
            {#each plan.constraints as item}
              <li>{item}</li>
            {/each}
          </ul>
        </div>
      {/if}

      {#if plan.acceptanceCriteria && plan.acceptanceCriteria.length > 0}
        <div class="plan-section">
          <div class="section-title">{i18n.t('planCard.acceptanceCriteria')}</div>
          <ul>
            {#each plan.acceptanceCriteria as item}
              <li>{item}</li>
            {/each}
          </ul>
        </div>
      {/if}

      {#if plan.riskLevel}
        <div class="plan-section">
          <div class="section-title">{i18n.t('planCard.riskLevel')}</div>
          <div class="risk-badge {plan.riskLevel}">{plan.riskLevel}</div>
        </div>
      {/if}

      {#if plan.riskFactors && plan.riskFactors.length > 0}
        <div class="plan-section">
          <div class="section-title">{i18n.t('planCard.riskFactors')}</div>
          <ul>
            {#each plan.riskFactors as item}
              <li>{item}</li>
            {/each}
          </ul>
        </div>
      {/if}

      {#if plan.rawJson && isExpanded}
        <div class="plan-raw">
          <div class="section-title">{i18n.t('planCard.rawData')}</div>
          <pre><code>{plan.rawJson}</code></pre>
        </div>
      {/if}
    </div>
  </div>
{/if}

<style>
  /* 与 tool-card 风格统一：左 3px 状态色条 + 中性卡身
     accent 色由 riskLevel 驱动（low=success / medium=warning / high=error），
     无 riskLevel 时降级为 info（informational 默认态） */
  .plan-card {
    position: relative;
    background: var(--surface-1);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    /* padding-left 让出 3px 给 ::before accent 条 */
    padding: var(--space-4) var(--space-4) var(--space-4) calc(var(--space-4) + 3px);
    margin: var(--space-2) 0;
  }

  .plan-card::before {
    content: '';
    position: absolute;
    left: 0;
    top: 0;
    bottom: 0;
    width: 3px;
    background: var(--info);
    pointer-events: none;
    transition: background var(--transition-fast);
  }

  .plan-card[data-status='low']::before { background: var(--success); }
  .plan-card[data-status='medium']::before { background: var(--warning); }
  .plan-card[data-status='high']::before { background: var(--error); }

  .plan-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: var(--space-3);
  }

  .plan-title {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    font-weight: 600;
    color: var(--foreground);
  }

  .toggle-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 26px;
    height: 26px;
    border-radius: 6px;
    border: 1px solid var(--border);
    background: var(--surface-2);
    color: var(--foreground-muted);
    cursor: pointer;
  }

  .toggle-btn:hover {
    color: var(--foreground);
    border-color: var(--primary);
  }

  .plan-goal {
    font-size: var(--text-base);
    color: var(--foreground);
    margin-bottom: var(--space-3);
  }

  .plan-section {
    margin-bottom: var(--space-3);
  }

  .section-title {
    font-size: var(--text-xs);
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--foreground-muted);
    margin-bottom: var(--space-1);
  }

  .section-body,
  li {
    font-size: var(--text-sm);
    color: var(--foreground);
  }

  ul {
    margin: 0;
    padding-left: var(--space-4);
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .risk-badge {
    display: inline-flex;
    align-items: center;
    padding: 2px 8px;
    border-radius: 999px;
    font-size: var(--text-xs);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }

  .risk-badge.low {
    color: var(--success);
    background: color-mix(in oklab, var(--success) 20%, transparent);
  }

  .risk-badge.medium {
    color: var(--warning);
    background: color-mix(in oklab, var(--warning) 20%, transparent);
  }

  .risk-badge.high {
    color: var(--error);
    background: color-mix(in oklab, var(--error) 20%, transparent);
  }

  .plan-raw pre {
    background: var(--surface-2);
    border-radius: var(--radius-md);
    padding: var(--space-3);
    font-size: var(--text-xs);
    overflow-x: auto;
  }
</style>
