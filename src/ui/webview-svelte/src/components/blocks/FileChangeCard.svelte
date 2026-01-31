<script lang="ts">
  import type { ContentBlock } from '../../types/message';
  import Icon from '../Icon.svelte';

  interface Props {
    block: ContentBlock;
  }

  let { block }: Props = $props();
  const change = $derived(block.fileChange);
  let isExpanded = $state(false);

  const changeLabel = $derived.by(() => {
    if (!change) return '';
    switch (change.changeType) {
      case 'create':
        return '新建';
      case 'delete':
        return '删除';
      default:
        return '修改';
    }
  });
</script>

{#if change}
  <div class="file-change-card">
    <div class="file-change-header">
      <div class="file-change-meta">
        <span class="change-badge {change.changeType}">{changeLabel}</span>
        <span class="file-path">{change.filePath}</span>
      </div>
      <div class="file-change-stats">
        {#if typeof change.additions === 'number'}
          <span class="stat additions">+{change.additions}</span>
        {/if}
        {#if typeof change.deletions === 'number'}
          <span class="stat deletions">-{change.deletions}</span>
        {/if}
        {#if change.diff}
          <button class="toggle-btn" onclick={() => (isExpanded = !isExpanded)} aria-label="切换 diff 展示">
            <Icon name={isExpanded ? 'chevron-up' : 'chevron-down'} size={14} />
          </button>
        {/if}
      </div>
    </div>

    {#if change.diff && isExpanded}
      <pre class="diff-block"><code>{change.diff}</code></pre>
    {/if}
  </div>
{/if}

<style>
  .file-change-card {
    background: var(--surface-1);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: var(--space-3) var(--space-4);
    margin: var(--space-2) 0;
  }

  .file-change-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
  }

  .file-change-meta {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
  }

  .file-path {
    font-size: var(--text-sm);
    color: var(--foreground);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 320px;
  }

  .change-badge {
    font-size: var(--text-xs);
    padding: 2px 8px;
    border-radius: 999px;
    background: var(--surface-2);
    color: var(--foreground-muted);
    text-transform: uppercase;
    letter-spacing: 0.03em;
  }
  .change-badge.create { color: var(--success); background: color-mix(in oklab, var(--success) 20%, transparent); }
  .change-badge.delete { color: var(--error); background: color-mix(in oklab, var(--error) 20%, transparent); }
  .change-badge.modify { color: var(--warning); background: color-mix(in oklab, var(--warning) 20%, transparent); }

  .file-change-stats {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }

  .stat {
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }
  .stat.additions { color: var(--success); }
  .stat.deletions { color: var(--error); }

  .toggle-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 24px;
    height: 24px;
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

  .diff-block {
    margin-top: var(--space-3);
    background: var(--surface-2);
    border-radius: var(--radius-md);
    padding: var(--space-3);
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    overflow-x: auto;
  }
</style>
