<script lang="ts">
  import { getState } from '../stores/messages.svelte';
  import { ensureArray } from '../lib/utils';

  const appState = getState();

  // 任务列表
  const tasks = $derived(ensureArray(appState.tasks));
  const missionPlan = $derived(appState.missionPlan);

  const todoStatusLabels: Record<string, string> = {
    pending: '待执行',
    blocked: '阻塞',
    in_progress: '执行中',
    completed: '已完成',
    failed: '失败',
    skipped: '跳过',
  };

  const assignmentStatusLabels: Record<string, string> = {
    pending: '待执行',
    running: '执行中',
    completed: '已完成',
    failed: '失败',
  };
</script>

<div class="tasks-panel">
  <div class="tasks-content">
    {#if tasks.length === 0 && (!missionPlan || missionPlan.assignments.length === 0)}
      <div class="empty-state">
        <svg class="empty-icon" viewBox="0 0 16 16">
          <path d="M2 2.5a.5.5 0 0 1 .5-.5h11a.5.5 0 0 1 0 1h-11a.5.5 0 0 1-.5-.5zm0 4a.5.5 0 0 1 .5-.5h11a.5.5 0 0 1 0 1h-11a.5.5 0 0 1-.5-.5zm0 4a.5.5 0 0 1 .5-.5h11a.5.5 0 0 1 0 1h-11a.5.5 0 0 1-.5-.5zm0 4a.5.5 0 0 1 .5-.5h11a.5.5 0 0 1 0 1h-11a.5.5 0 0 1-.5-.5z"/>
        </svg>
        <div class="empty-text">暂无任务</div>
        <div class="empty-hint">执行任务后会在此显示进度</div>
      </div>
    {:else}
      {#if tasks.length > 0}
        <div class="section-title">任务</div>
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
                <div class="task-name">{task.name || task.prompt || '未命名任务'}</div>
                {#if task.description || task.prompt}
                  <div class="task-desc">{task.description || task.prompt}</div>
                {/if}
              </div>
            </div>
          {/each}
        </div>
      {/if}

      {#if missionPlan && missionPlan.assignments.length > 0}
        <div class="section-title">Worker Todo</div>
        <div class="assignment-list">
          {#each missionPlan.assignments as assignment}
            <div class="assignment-card">
              <div class="assignment-header">
                <div class="assignment-title">{assignment.responsibility}</div>
                <div class="assignment-meta">
                  <span class="assignment-worker">{assignment.workerId}</span>
                  <span class="assignment-status status-{assignment.status || 'pending'}">
                    {assignmentStatusLabels[assignment.status || 'pending'] || assignment.status || '待执行'}
                  </span>
                </div>
              </div>
              {#if typeof assignment.progress === 'number'}
                <div class="assignment-progress">
                  <div class="progress-bar" style="width: {assignment.progress}%"></div>
                </div>
              {/if}
              {#if assignment.todos.length > 0}
                <div class="todo-list">
                  {#each assignment.todos as todo}
                    <div class="todo-item" class:completed={todo.status === 'completed'} class:failed={todo.status === 'failed'}>
                      <div class="todo-main">
                        <span class="todo-status status-{todo.status}">
                          {todoStatusLabels[todo.status] || todo.status}
                        </span>
                        <span class="todo-content">{todo.content || '未命名 Todo'}</span>
                      </div>
                      <div class="todo-meta">
                        <span class="todo-type">{todo.type}</span>
                        <span class="todo-priority">P{todo.priority}</span>
                        {#if todo.outOfScope}
                          <span class="todo-flag">超范围</span>
                        {/if}
                        {#if todo.approvalStatus}
                          <span class="todo-approval">{todo.approvalStatus}</span>
                        {/if}
                      </div>
                      {#if todo.expectedOutput}
                        <div class="todo-output">预期: {todo.expectedOutput}</div>
                      {/if}
                    </div>
                  {/each}
                </div>
              {:else}
                <div class="todo-empty">暂无 Todo</div>
              {/if}
            </div>
          {/each}
        </div>
      {/if}
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

  .section-title {
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    color: var(--foreground-muted);
    text-transform: uppercase;
    letter-spacing: 0.04em;
    margin: var(--space-2) 0 var(--space-3);
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

  .assignment-list {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }

  .assignment-card {
    border: 1px solid var(--border);
    background: var(--surface-1);
    border-radius: var(--radius-md);
    padding: var(--space-3);
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  .assignment-header {
    display: flex;
    justify-content: space-between;
    gap: var(--space-3);
    align-items: center;
  }

  .assignment-title {
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    color: var(--foreground);
  }

  .assignment-meta {
    display: flex;
    gap: var(--space-2);
    align-items: center;
    font-size: var(--text-xs);
  }

  .assignment-worker {
    padding: 2px 6px;
    border-radius: var(--radius-full);
    background: var(--surface-2);
    color: var(--foreground-muted);
  }

  .assignment-status {
    padding: 2px 6px;
    border-radius: var(--radius-full);
    background: var(--surface-2);
    color: var(--foreground-muted);
    text-transform: uppercase;
    font-size: 10px;
  }

  .assignment-progress {
    height: 6px;
    background: var(--surface-2);
    border-radius: var(--radius-full);
    overflow: hidden;
  }

  .progress-bar {
    height: 100%;
    background: var(--primary);
  }

  .todo-list {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    margin-top: var(--space-2);
  }

  .todo-item {
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius-md);
    background: var(--surface-2);
    border: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .todo-item.completed {
    opacity: 0.7;
  }

  .todo-item.failed {
    border-color: var(--error-muted);
    background: var(--error-muted);
  }

  .todo-main {
    display: flex;
    gap: var(--space-2);
    align-items: center;
    font-size: var(--text-sm);
  }

  .todo-status {
    padding: 2px 6px;
    border-radius: var(--radius-full);
    background: var(--surface-3);
    font-size: 10px;
    text-transform: uppercase;
  }

  .todo-content {
    color: var(--foreground);
  }

  .todo-meta {
    display: flex;
    gap: var(--space-2);
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  .todo-flag {
    color: var(--warning);
  }

  .todo-approval {
    color: var(--info);
  }

  .todo-output {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  .todo-empty {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    padding: var(--space-2) 0;
  }
</style>
