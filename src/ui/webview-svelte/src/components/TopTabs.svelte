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

<div class="top-tabs">
  <button
    class="top-tab"
    class:active={activeTopTab === 'thread'}
    onclick={() => onTabChange('thread')}
  >
    对话
  </button>
  <button
    class="top-tab"
    class:active={activeTopTab === 'tasks'}
    onclick={() => onTabChange('tasks')}
  >
    任务
    {#if tasksBadge > 0}
      <span class="badge">{tasksBadge}</span>
    {/if}
  </button>
  <button
    class="top-tab"
    class:active={activeTopTab === 'edits'}
    onclick={() => onTabChange('edits')}
  >
    变更
    {#if editsBadge > 0}
      <span class="badge">{editsBadge}</span>
    {/if}
  </button>
  <button
    class="top-tab"
    class:active={activeTopTab === 'knowledge'}
    onclick={() => onTabChange('knowledge')}
  >
    知识
  </button>
</div>

<style>
  .top-tabs {
    display: flex;
    gap: var(--space-1);
    padding: 0 var(--space-4);
    background: var(--background);
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
  }

  .top-tab {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-3) var(--space-4);
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    color: var(--foreground-muted);
    background: transparent;
    border: none;
    border-bottom: 2px solid transparent;
    cursor: pointer;
    transition: all var(--transition-fast);
    position: relative;
  }

  .top-tab:hover {
    color: var(--foreground);
    background: var(--surface-1);
  }

  .top-tab.active {
    color: var(--primary);
    border-bottom-color: var(--primary);
  }

  .badge {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: 18px;
    height: 18px;
    padding: 0 var(--space-2);
    font-size: var(--text-xs);
    font-weight: var(--font-semibold);
    color: white;
    background: var(--primary);
    border-radius: var(--radius-full);
    line-height: 1;
  }
</style>
