<script lang="ts">
  import { i18n } from '../stores/i18n.svelte';
  import { getState } from '../stores/messages.svelte';
  import { getTaskProjectionState } from '../stores/task-projection-store.svelte';
  import { ensureArray } from '../lib/utils';

  interface Props {
    activeTopTab: 'thread' | 'tasks' | 'edits' | 'knowledge';
    onTabChange: (tab: 'thread' | 'tasks' | 'edits' | 'knowledge') => void;
  }

  let { activeTopTab, onTabChange }: Props = $props();

  const appState = getState();
  const currentSessionId = $derived(appState.currentSessionId);
  const currentWorkspaceId = $derived(appState.currentWorkspaceId);
  const taskProjection = $derived(getTaskProjectionState(currentSessionId, currentWorkspaceId));

  // 任务和变更的徽章数量
  const tasksBadge = $derived.by(() => {
    const projection = taskProjection.projection;
    return projection?.progress_summary?.total_tasks ?? 0;
  });
  // 失败任务优先级高于总数显示。
  const attentionCount = $derived.by(() => {
    const projection = taskProjection.projection;
    if (!projection) return 0;
    const ids = new Set<string>(projection.failed_tasks ?? []);
    return ids.size;
  });
  const editsBadge = $derived(ensureArray(appState.edits).length);
</script>

<div class="tab-bar tab-bar--top">
  <button class="tab-item" class:active={activeTopTab === 'thread'} onclick={() => onTabChange('thread')}>
    {i18n.t('topTabs.thread')}
  </button>
  <button class="tab-item" class:active={activeTopTab === 'tasks'} onclick={() => onTabChange('tasks')}>
    {i18n.t('topTabs.tasks')}
    {#if attentionCount > 0}
      <span class="badge badge--warning" title={i18n.t('topTabs.attentionTitle', { count: attentionCount })}>
        {attentionCount}
      </span>
    {:else if tasksBadge > 0}
      <span class="badge {activeTopTab === 'tasks' ? 'badge--primary' : 'badge--muted'}">{tasksBadge}</span>
    {/if}
  </button>
  <button class="tab-item" class:active={activeTopTab === 'edits'} onclick={() => onTabChange('edits')}>
    {i18n.t('topTabs.edits')}
    {#if editsBadge > 0}
      <span class="badge {activeTopTab === 'edits' ? 'badge--primary' : 'badge--muted'}">{editsBadge}</span>
    {/if}
  </button>
  <button class="tab-item" class:active={activeTopTab === 'knowledge'} onclick={() => onTabChange('knowledge')}>
    {i18n.t('topTabs.knowledge')}
  </button>
</div>

<style>
  /* ============================================
     TopTabs - 顶部导航栏
     设计参考: Apple HIG 胶囊切换器 (Segmented Control)
     ============================================ */
  .tab-bar.tab-bar--top {
    border-bottom: none;
    background: transparent;
    padding: 6px 16px;
    gap: 3px;
    justify-content: center;
  }

  .tab-item {
    padding: 6px 13px;
    border-radius: var(--radius-full);
    height: 28px;
    background: transparent;
    transition: background var(--transition-fast), color var(--transition-fast), box-shadow var(--transition-fast);
  }

  .tab-item::after {
    display: none; /* 移除旧版下划线 */
  }

  .tab-item:hover {
    background: color-mix(in srgb, var(--surface-hover) 72%, transparent);
    color: var(--foreground);
  }

  .tab-item.active {
    background: color-mix(in srgb, var(--surface-active) 55%, transparent);
    color: var(--foreground);
    font-weight: var(--font-semibold);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--border) 72%, transparent);
  }

  .badge--muted {
    background: var(--surface-3);
    color: var(--foreground-muted);
  }

  .badge--primary {
    background: var(--primary);
    color: var(--primary-foreground);
  }

  .badge--warning {
    background: var(--warning, #f59e0b);
    color: #fff;
  }

  @media (max-width: 768px) {
    .tab-bar.tab-bar--top {
      padding: 2px 0;
      justify-content: flex-start;
      flex-wrap: nowrap;
    }

    .tab-item {
      padding: 5px 10px;
      font-size: var(--text-xs);
      height: 26px;
      flex-shrink: 0;
    }
  }
</style>
