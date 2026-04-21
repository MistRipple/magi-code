<script lang="ts">
  import { i18n } from '../stores/i18n.svelte';
  import { getState } from '../stores/messages.svelte';
  import { getTaskGraphState } from '../stores/task-graph-store.svelte';
  import { ensureArray } from '../lib/utils';

  interface Props {
    activeTopTab: 'thread' | 'tasks' | 'edits' | 'knowledge';
    onTabChange: (tab: 'thread' | 'tasks' | 'edits' | 'knowledge') => void;
  }

  let { activeTopTab, onTabChange }: Props = $props();

  const appState = getState();
  const taskGraph = getTaskGraphState();

  // 任务和变更的徽章数量
  const tasksBadge = $derived.by(() => {
    const projection = taskGraph.projection;
    return projection?.progress_summary?.total_tasks ?? 0;
  });
  const editsBadge = $derived(ensureArray(appState.edits).length);
</script>

<div class="tab-bar tab-bar--top">
  <button class="tab-item" class:active={activeTopTab === 'thread'} onclick={() => onTabChange('thread')}>
    {i18n.t('topTabs.thread')}
  </button>
  <button class="tab-item" class:active={activeTopTab === 'tasks'} onclick={() => onTabChange('tasks')}>
    {i18n.t('topTabs.tasks')}
    {#if tasksBadge > 0}
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
     设计参考: Cursor/Linear 极简下划线 Tab
     ============================================ */
  .badge--muted {
    background: var(--surface-3);
    color: var(--foreground-muted);
  }

  .badge--primary {
    background: var(--primary);
    color: var(--primary-foreground);
  }
</style>
