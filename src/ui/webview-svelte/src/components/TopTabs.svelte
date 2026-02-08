<script lang="ts">
  import { getState } from '../stores/messages.svelte';
  import { ensureArray } from '../lib/utils';

  interface Props {
    activeTopTab: 'thread' | 'tasks' | 'edits' | 'knowledge';
    onTabChange: (tab: 'thread' | 'tasks' | 'edits' | 'knowledge') => void;
  }

  let { activeTopTab, onTabChange }: Props = $props();

  const appState = getState();

  // 任务和变更的徽章数量
  const tasksBadge = $derived(ensureArray(appState.tasks).length);
  const editsBadge = $derived(ensureArray(appState.edits).length);
</script>

<div class="tt-bar">
  <button class="tt-tab" class:active={activeTopTab === 'thread'} onclick={() => onTabChange('thread')}>
    对话
  </button>
  <button class="tt-tab" class:active={activeTopTab === 'tasks'} onclick={() => onTabChange('tasks')}>
    任务
    {#if tasksBadge > 0}
      <span class="tt-badge">{tasksBadge}</span>
    {/if}
  </button>
  <button class="tt-tab" class:active={activeTopTab === 'edits'} onclick={() => onTabChange('edits')}>
    变更
    {#if editsBadge > 0}
      <span class="tt-badge">{editsBadge}</span>
    {/if}
  </button>
  <button class="tt-tab" class:active={activeTopTab === 'knowledge'} onclick={() => onTabChange('knowledge')}>
    知识
  </button>
</div>

<style>
  /* ============================================
     TopTabs - 顶部导航栏
     设计参考: Cursor/Linear 极简下划线 Tab
     ============================================ */
  .tt-bar {
    display: flex;
    padding: 0 var(--space-3);
    background: var(--background);
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
  }

  .tt-tab {
    display: inline-flex;
    align-items: center;
    gap: var(--space-1);
    padding: 6px var(--space-3);
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    color: var(--foreground-muted);
    background: transparent;
    border: none;
    border-bottom: 2px solid transparent;
    cursor: pointer;
    transition: color var(--transition-fast), border-color var(--transition-fast);
    white-space: nowrap;
  }

  .tt-tab:hover {
    color: var(--foreground);
  }

  .tt-tab.active {
    color: var(--foreground);
    border-bottom-color: var(--primary);
  }

  .tt-badge {
    font-size: 10px;
    min-width: 16px;
    height: 16px;
    line-height: 16px;
    text-align: center;
    padding: 0 4px;
    background: var(--surface-3);
    color: var(--foreground-muted);
    border-radius: var(--radius-full);
    font-variant-numeric: tabular-nums;
  }

  .tt-tab.active .tt-badge {
    background: var(--primary);
    color: white;
  }
</style>
