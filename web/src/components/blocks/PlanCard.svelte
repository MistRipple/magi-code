<script lang="ts">
  import type { ContentBlock } from '../../types/message';
  import type { FilePreviewScope } from '../../lib/file-reference';
  import Icon from '../Icon.svelte';
  import MarkdownContent from '../MarkdownContent.svelte';
  import { i18n } from '../../stores/i18n.svelte';

  interface Props {
    block: ContentBlock;
    filePreviewScope?: FilePreviewScope;
  }

  let { block, filePreviewScope = undefined }: Props = $props();
  const plan = $derived(block.plan);
</script>

{#if plan}
  <div class="plan-card">
    <div class="plan-header">
      <span class="plan-title">
        <Icon name="note" size={14} />
        <span>{i18n.t('planCard.title')}</span>
      </span>
    </div>

    <div class="plan-body">
      <div class="plan-goal"><MarkdownContent content={plan.goal} {filePreviewScope} /></div>

      {#if plan.analysis}
        <div class="plan-section">
          <div class="section-title">{i18n.t('planCard.analysis')}</div>
          <div class="section-body"><MarkdownContent content={plan.analysis} {filePreviewScope} /></div>
        </div>
      {/if}

      {#if plan.constraints && plan.constraints.length > 0}
        <div class="plan-section">
          <div class="section-title">{i18n.t('planCard.constraints')}</div>
          <ul>
            {#each plan.constraints as item}
              <li><MarkdownContent content={item} {filePreviewScope} /></li>
            {/each}
          </ul>
        </div>
      {/if}

      {#if plan.acceptanceCriteria && plan.acceptanceCriteria.length > 0}
        <div class="plan-section">
          <div class="section-title">{i18n.t('planCard.acceptanceCriteria')}</div>
          <ul>
            {#each plan.acceptanceCriteria as item}
              <li><MarkdownContent content={item} {filePreviewScope} /></li>
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
              <li><MarkdownContent content={item} {filePreviewScope} /></li>
            {/each}
          </ul>
        </div>
      {/if}

    </div>
  </div>
{/if}

<style>
  /* 中性卡身，无左侧 accent 条；padding 由 plan-header（全局 SSoT）和 plan-body 各自管理 */
  .plan-card {
    background: var(--surface-1);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    margin: var(--space-2) 0;
  }

  /* plan-header 的 display/align/gap/padding/min-height/font 由全局 SSoT
     (styles/tool-card.css) 提供，组件层不再重复声明，确保与其他卡片 header 一致 */

  .plan-title {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }

  /* plan-body：承载所有 header 以下内容的 padding */
  .plan-body {
    padding: var(--space-2) var(--space-4) var(--space-4);
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

</style>
