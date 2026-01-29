<script lang="ts">
  import { getState } from '../stores/messages.svelte';
  import { ensureArray } from '../lib/utils';

  const appState = getState();

  // 任务列表
  const tasks = $derived(ensureArray(appState.tasks));
</script>

<div class="tasks-panel">
  <div class="tasks-content">
    {#if tasks.length === 0}
      <div class="empty-state">
        <svg class="empty-icon" viewBox="0 0 16 16">
          <path d="M2 2.5a.5.5 0 0 1 .5-.5h11a.5.5 0 0 1 0 1h-11a.5.5 0 0 1-.5-.5zm0 4a.5.5 0 0 1 .5-.5h11a.5.5 0 0 1 0 1h-11a.5.5 0 0 1-.5-.5zm0 4a.5.5 0 0 1 .5-.5h11a.5.5 0 0 1 0 1h-11a.5.5 0 0 1-.5-.5zm0 4a.5.5 0 0 1 .5-.5h11a.5.5 0 0 1 0 1h-11a.5.5 0 0 1-.5-.5z"/>
        </svg>
        <div class="empty-text">暂无任务</div>
        <div class="empty-hint">执行任务后会在此显示进度</div>
      </div>
    {:else}
      <div class="tasks-list">
        {#each tasks as task}
          <div class="task-item" class:completed={task.status === 'completed'} class:failed={task.status === 'failed'}>
            <div class="task-status">
              {#if task.status === 'running'}
                <svg class="status-icon spinning" viewBox="0 0 16 16">
                  <path d="M11.534 7h3.932a.25.25 0 0 1 .192.41l-1.966 2.36a.25.25 0 0 1-.384 0l-1.966-2.36a.25.25 0 0 1 .192-.41z"/>
                  <path d="M8 3c-1.552 0-2.94.707-3.857 1.818a.5.5 0 1 1-.771-.636A6.002 6.002 0 0 1 13.917 7H12.9A5.002 5.002 0 0 0 8 3z"/>
                </svg>
              {:else if task.status === 'completed'}
                <svg class="status-icon success" viewBox="0 0 16 16">
                  <path d="M10.97 4.97a.75.75 0 0 1 1.07 1.05l-3.99 4.99a.75.75 0 0 1-1.08.02L4.324 8.384a.75.75 0 1 1 1.06-1.06l2.094 2.093 3.473-4.425z"/>
                </svg>
              {:else if task.status === 'failed'}
                <svg class="status-icon error" viewBox="0 0 16 16">
                  <path d="M4.646 4.646a.5.5 0 0 1 .708 0L8 7.293l2.646-2.647a.5.5 0 0 1 .708.708L8.707 8l2.647 2.646a.5.5 0 0 1-.708.708L8 8.707l-2.646 2.647a.5.5 0 0 1-.708-.708L7.293 8 4.646 5.354a.5.5 0 0 1 0-.708z"/>
                </svg>
              {:else}
                <svg class="status-icon pending" viewBox="0 0 16 16">
                  <circle cx="8" cy="8" r="3" fill="currentColor"/>
                </svg>
              {/if}
            </div>
            <div class="task-info">
              <div class="task-name">{task.name || '未命名任务'}</div>
              {#if task.description}
                <div class="task-desc">{task.description}</div>
              {/if}
            </div>
          </div>
        {/each}
      </div>
    {/if}
  </div>
</div>

<style>
  .tasks-panel {
    height: 100%;
    overflow-y: auto;
    padding: var(--space-4);
  }

  .empty-state {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    padding: var(--space-8) var(--space-5);
    color: var(--foreground-muted);
    text-align: center;
  }

  .empty-icon {
    width: var(--icon-2xl);
    height: var(--icon-2xl);
    fill: currentColor;
    opacity: 0.3;
    margin-bottom: var(--space-4);
  }

  .empty-text {
    font-size: var(--text-base);
    font-weight: var(--font-medium);
    color: var(--foreground);
    margin-bottom: var(--space-2);
  }

  .empty-hint {
    font-size: var(--text-sm);
    opacity: 0.7;
  }

  .tasks-list {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  .task-item {
    display: flex;
    align-items: flex-start;
    gap: var(--space-3);
    padding: var(--space-3) var(--space-4);
    background: var(--surface-1);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    transition: all var(--transition-fast);
  }

  .task-item:hover {
    background: var(--surface-hover);
  }

  .task-item.completed {
    opacity: 0.6;
  }

  .task-item.failed {
    border-color: var(--error-muted);
    background: var(--error-muted);
  }

  .task-status {
    flex-shrink: 0;
    width: var(--icon-lg);
    height: var(--icon-lg);
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .status-icon {
    width: var(--icon-md);
    height: var(--icon-md);
    fill: currentColor;
  }

  .status-icon.spinning {
    animation: spin 1s linear infinite;
    color: var(--primary);
  }

  .status-icon.success {
    color: var(--success);
  }

  .status-icon.error {
    color: var(--error);
  }

  .status-icon.pending {
    color: var(--foreground-muted);
  }

  @keyframes spin {
    from { transform: rotate(0deg); }
    to { transform: rotate(360deg); }
  }

  .task-info {
    flex: 1;
    min-width: 0;
  }

  .task-name {
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    color: var(--foreground);
    line-height: var(--leading-tight);
  }

  .task-desc {
    font-size: var(--text-sm);
    color: var(--foreground-muted);
    margin-top: var(--space-1);
    line-height: var(--leading-normal);
  }
</style>
