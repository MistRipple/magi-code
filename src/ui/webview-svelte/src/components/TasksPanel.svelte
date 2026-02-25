<script lang="ts">
  import { getState } from '../stores/messages.svelte';
  import { ensureArray } from '../lib/utils';
  import type { Task, SubTaskItem } from '../types/message';
  import { vscode } from '../lib/vscode-bridge';
  import Icon from './Icon.svelte';

  const appState = getState();

  // 数据源：task.subTasks 是唯一的 todo 数据源（来自 stateUpdate 全量同步）
  // missionPlanMap 按 missionId 存储 Worker 分组元信息（workerId、responsibility）
  const tasks = $derived(ensureArray(appState.tasks) as Task[]);
  const missionPlanMap = $derived(appState.missionPlan);

  // 折叠状态
  let expandedTasks = $state<Set<string>>(new Set());

  // 概览统计
  const stats = $derived.by(() => {
    const total = tasks.length;
    const completed = tasks.filter(t => t.status === 'completed').length;
    const running = tasks.filter(t => t.status === 'running').length;
    const failed = tasks.filter(t => t.status === 'failed').length;
    const pending = total - completed - running - failed;
    const progress = total > 0 ? Math.round((completed / total) * 100) : 0;
    return { total, completed, running, failed, pending, progress };
  });

  // 将 task.subTasks 按 assignmentId 分组，并关联 missionPlan 的 Worker 元信息
  const enrichedTasks = $derived.by(() => {
    return tasks.map(task => {
      const taskId = task.id;
      const subTasks = task.subTasks;

      // 按 assignmentId 分组 subTasks
      const groupMap = new Map<string, { workerId: string; responsibility: string; todos: SubTaskItem[] }>();
      for (const st of subTasks) {
        const aid = st.assignmentId || '_default';
        if (!groupMap.has(aid)) {
          groupMap.set(aid, { workerId: st.assignedWorker || '', responsibility: '', todos: [] });
        }
        groupMap.get(aid)!.todos.push(st);
      }

      // 用 missionPlan 的 assignment 补充 Worker 元信息（responsibility）
      const plan = missionPlanMap.get(taskId) || missionPlanMap.get(task.missionId);
      if (plan?.assignments) {
        for (const a of plan.assignments) {
          const group = groupMap.get(a.id);
          if (group) {
            group.workerId = group.workerId || a.workerId;
            group.responsibility = a.responsibility || '';
          }
        }
      }

      // 转换为数组
      const workerGroups = Array.from(groupMap.entries()).map(([aid, g]) => ({
        id: aid,
        workerId: g.workerId,
        responsibility: g.responsibility,
        todos: g.todos,
      }));

      // 统计
      const todoTotal = subTasks.length;
      const todoCompleted = subTasks.filter(t => t.status === 'completed' || t.status === 'skipped').length;
      const todoProgress = todoTotal > 0 ? Math.round((todoCompleted / todoTotal) * 100) : task.progress;

      return { ...task, workerGroups, todoTotal, todoCompleted, todoProgress };
    });
  });

  const workerColors: Record<string, string> = {
    claude: 'var(--color-claude)',
    codex: 'var(--color-codex)',
    gemini: 'var(--color-gemini)',
  };

  function toggleExpand(taskId: string) {
    const next = new Set(expandedTasks);
    if (next.has(taskId)) next.delete(taskId);
    else next.add(taskId);
    expandedTasks = next;
  }

  // 自动展开 running 任务（仅在有新增 running task 需要展开时才写入，避免高频 stateUpdate 触发无效渲染）
  $effect(() => {
    const runningIds = tasks.filter(t => t.status === 'running').map(t => t.id);
    const needsExpand = runningIds.some(id => !expandedTasks.has(id));
    if (needsExpand) {
      const next = new Set(expandedTasks);
      for (const id of runningIds) next.add(id);
      expandedTasks = next;
    }
  });

  function startTask(taskId: string) {
    vscode.postMessage({ type: 'startTask', taskId });
  }

  function deleteTask(taskId: string) {
    vscode.postMessage({ type: 'deleteTask', taskId });
  }
</script>

<div class="tasks-panel">
  {#if enrichedTasks.length === 0}
    <!-- 空状态 -->
    <div class="empty-state">
      <div class="empty-icon-wrap">
        <Icon name="circleOutline" size={32} class="empty-icon" />
      </div>
      <div class="empty-text">暂无任务</div>
      <div class="empty-hint">执行任务后会在此显示进度</div>
    </div>
  {:else}
    <!-- 概览统计条 -->
    {#if stats.total > 0}
      <div class="overview-bar">
        <div class="overview-stats">
          <span class="overview-label">{stats.completed}/{stats.total} 已完成</span>
          {#if stats.running > 0}
            <span class="overview-dot running"></span>
            <span class="overview-running">{stats.running} 执行中</span>
          {/if}
          {#if stats.failed > 0}
            <span class="overview-dot failed"></span>
            <span class="overview-failed">{stats.failed} 失败</span>
          {/if}
        </div>
        <div class="overview-progress">
          <div class="overview-progress-fill" style="width: {stats.progress}%"></div>
        </div>
      </div>
    {/if}

    <!-- 任务列表 -->
    <div class="tasks-list">
      {#each enrichedTasks as task, taskIdx (task.id)}
        {@const isExpanded = expandedTasks.has(task.id)}
        {@const hasChildren = task.todoTotal > 0}
        <div class="task-card" class:is-running={task.status === 'running'} class:is-done={task.status === 'completed'} class:is-failed={task.status === 'failed'} class:is-cancelled={task.status === 'cancelled'}>
          <!-- 任务头部 -->
          <div class="task-header" role="button" tabindex="0" onclick={() => hasChildren && toggleExpand(task.id)} onkeydown={(e) => e.key === 'Enter' && hasChildren && toggleExpand(task.id)}>
            <!-- 折叠箭头 -->
            <span class="task-chevron" class:expanded={isExpanded} class:hidden={!hasChildren}>
              <Icon name="chevron-right" size={12} />
            </span>
            <!-- 任务序号 -->
            <span class="task-seq">{taskIdx + 1}.</span>
            <!-- 状态图标 -->
            <span class="task-status-icon status-{task.status}">
              {#if task.status === 'running'}
                <Icon name="loader" size={14} class="spinning" />
              {:else if task.status === 'completed'}
                <Icon name="check-circle" size={14} />
              {:else if task.status === 'failed'}
                <Icon name="x-circle" size={14} />
              {:else if task.status === 'cancelled'}
                <Icon name="skip-forward" size={14} />
              {:else}
                <Icon name="circleOutline" size={14} />
              {/if}
            </span>
            <!-- 任务信息 -->
            <div class="task-body">
              <span class="task-name">{task.name || task.prompt || '未命名任务'}</span>
              {#if task.todoTotal > 0}
                <span class="task-count">{task.todoCompleted}/{task.todoTotal}</span>
              {/if}
            </div>
            <!-- 操作按钮（hover 显示） -->
            <div class="task-actions">
              {#if task.status === 'pending' || task.status === 'paused'}
                <button class="action-btn" title="开始" onclick={(e) => { e.stopPropagation(); startTask(task.id); }}>
                  <Icon name="play" size={12} />
                </button>
              {/if}
              {#if task.status !== 'running'}
                <button class="action-btn danger" title="删除" onclick={(e) => { e.stopPropagation(); deleteTask(task.id); }}>
                  <Icon name="trash" size={12} />
                </button>
              {/if}
            </div>
          </div>

          <!-- 进度条（有子任务时显示） -->
          {#if task.todoTotal > 0}
            <div class="task-progress-bar">
              <div class="task-progress-fill" style="width: {task.todoProgress}%"></div>
            </div>
          {/if}

          <!-- 展开的子任务/Todo -->
          {#if isExpanded && hasChildren}
            <div class="task-children">
              {#each task.workerGroups as group (group.id)}
                <!-- Worker 分组标题（多 Worker 时显示） -->
                {#if task.workerGroups.length > 1}
                  <div class="worker-group-label">
                    <span class="worker-dot" style="background: {workerColors[group.workerId] || 'var(--foreground-muted)'}"></span>
                    <span>{group.workerId}</span>
                    <span class="worker-responsibility">{group.responsibility || ''}</span>
                  </div>
                {/if}
                {#each group.todos as todo, todoIdx (todo.id)}
                  {@const seqNum = todoIdx + 1}
                  <div class="todo-row" class:is-done={todo.status === 'completed' || todo.status === 'skipped'} class:is-failed={todo.status === 'failed'} class:is-running={todo.status === 'running' || todo.status === 'in_progress'}>
                    <span class="todo-seq">{seqNum}.</span>
                    <span class="todo-check">
                      {#if todo.status === 'completed' || todo.status === 'skipped'}
                        <Icon name="check" size={12} />
                      {:else if todo.status === 'running' || todo.status === 'in_progress'}
                        <Icon name="loader" size={12} class="spinning" />
                      {:else if todo.status === 'failed'}
                        <Icon name="x-circle" size={12} />
                      {:else if todo.status === 'blocked'}
                        <Icon name="alert-circle" size={12} />
                      {:else}
                        <span class="todo-circle"></span>
                      {/if}
                    </span>
                    <span class="todo-text">{todo.description || '未命名'}</span>
                    {#if task.workerGroups.length <= 1 && group.workerId}
                      <span class="todo-worker" style="color: {workerColors[group.workerId] || 'var(--foreground-muted)'}">{group.workerId}</span>
                    {/if}
                  </div>
                {/each}
              {/each}
            </div>
          {/if}
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  /* ========== 面板容器 ========== */
  .tasks-panel {
    height: 100%;
    min-height: 0; /* flex 布局防溢出 */
    overflow-y: auto;
    padding: var(--space-4);
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }

  /* ========== 空状态 ========== */
  .empty-state {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    padding: var(--space-8) var(--space-5);
    color: var(--foreground-muted);
    text-align: center;
  }

  .empty-icon-wrap {
    opacity: 0.2;
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
    opacity: 0.6;
  }

  /* ========== 概览统计条 ========== */
  .overview-bar {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    padding: var(--space-3) var(--space-4);
    background: var(--surface-1);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
  }

  .overview-stats {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  .overview-label {
    font-weight: var(--font-medium);
    color: var(--foreground);
  }

  .overview-dot {
    width: 6px;
    height: 6px;
    border-radius: var(--radius-full);
    flex-shrink: 0;
  }

  .overview-dot.running { background: var(--primary); }
  .overview-dot.failed { background: var(--error); }

  .overview-running { color: var(--primary); }
  .overview-failed { color: var(--error); }

  .overview-progress {
    height: 3px;
    background: var(--surface-3);
    border-radius: var(--radius-full);
    overflow: hidden;
  }

  .overview-progress-fill {
    height: 100%;
    background: var(--success);
    border-radius: var(--radius-full);
    transition: width var(--transition-normal);
  }

  /* ========== 任务列表 ========== */
  .tasks-list {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  /* ========== 任务卡片 ========== */
  .task-card {
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--surface-1);
    transition: all var(--transition-fast);
    overflow: hidden;
  }

  .task-card:hover {
    border-color: color-mix(in srgb, var(--foreground) 20%, var(--border));
  }

  .task-card.is-running {
    border-color: color-mix(in srgb, var(--primary) 40%, var(--border));
  }

  .task-card.is-done {
    opacity: 0.6;
  }

  .task-card.is-failed {
    border-color: color-mix(in srgb, var(--error) 30%, var(--border));
  }

  .task-card.is-cancelled {
    opacity: 0.5;
  }

  /* ========== 任务头部（可点击） ========== */
  .task-header {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-3) var(--space-3);
    width: 100%;
    border: none;
    background: none;
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
    user-select: none;
  }

  .task-header:hover .task-actions {
    opacity: 1;
  }

  /* ========== 折叠箭头 ========== */
  .task-chevron {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 16px;
    height: 16px;
    flex-shrink: 0;
    color: var(--foreground-muted);
    transition: transform var(--transition-fast);
  }

  .task-chevron.expanded {
    transform: rotate(90deg);
  }

  .task-chevron.hidden {
    visibility: hidden;
  }

  /* ========== 任务序号 ========== */
  .task-seq {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    min-width: 18px;
    flex-shrink: 0;
    font-weight: var(--font-medium);
    font-variant-numeric: tabular-nums;
  }

  /* ========== 状态图标 ========== */
  .task-status-icon {
    display: flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
    width: 18px;
    height: 18px;
  }

  .task-status-icon.status-running { color: var(--primary); }
  .task-status-icon.status-completed { color: var(--success); }
  .task-status-icon.status-failed { color: var(--error); }
  .task-status-icon.status-cancelled { color: var(--foreground-muted); }
  .task-status-icon.status-pending { color: var(--foreground-muted); }
  .task-status-icon.status-paused { color: var(--warning); }

  :global(.spinning) {
    animation: spin 1s linear infinite;
  }

  @keyframes spin {
    from { transform: rotate(0deg); }
    to { transform: rotate(360deg); }
  }

  /* ========== 任务体 ========== */
  .task-body {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }

  .task-name {
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    color: var(--foreground);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .task-count {
    font-size: var(--text-2xs);
    color: var(--foreground-muted);
    padding: 1px 5px;
    background: var(--surface-3);
    border-radius: var(--radius-full);
    flex-shrink: 0;
  }

  /* ========== 操作按钮 ========== */
  .task-actions {
    display: flex;
    align-items: center;
    gap: var(--space-1);
    opacity: 0;
    transition: opacity var(--transition-fast);
    flex-shrink: 0;
  }

  .action-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 22px;
    border: none;
    background: var(--surface-3);
    color: var(--foreground-muted);
    border-radius: var(--radius-sm);
    cursor: pointer;
    transition: all var(--transition-fast);
  }

  .action-btn:hover {
    background: var(--surface-active);
    color: var(--foreground);
  }

  .action-btn.danger:hover {
    background: var(--error-muted);
    color: var(--error);
  }

  /* ========== 进度条 ========== */
  .task-progress-bar {
    height: 2px;
    background: var(--surface-2);
  }

  .task-progress-fill {
    height: 100%;
    background: var(--primary);
    transition: width var(--transition-normal);
  }

  /* ========== 子任务区域 ========== */
  .task-children {
    padding: 0 var(--space-3) var(--space-3);
    display: flex;
    flex-direction: column;
    gap: 1px;
  }

  /* ========== Worker 分组 ========== */
  .worker-group-label {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    font-size: var(--text-2xs);
    color: var(--foreground-muted);
    padding: var(--space-2) 0 var(--space-1);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }

  .worker-dot {
    width: 7px;
    height: 7px;
    border-radius: var(--radius-full);
    flex-shrink: 0;
  }

  .worker-responsibility {
    opacity: 0.7;
    font-style: italic;
    text-transform: none;
    letter-spacing: normal;
  }

  /* ========== Todo 行（Checklist 风格） ========== */
  .todo-row {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-1) var(--space-2);
    border-radius: var(--radius-xs);
    font-size: var(--text-sm);
    color: var(--foreground);
    transition: background var(--transition-fast);
  }

  .todo-row:hover {
    background: var(--surface-hover);
  }

  .todo-row.is-done {
    opacity: 0.5;
  }

  .todo-row.is-done .todo-text {
    text-decoration: line-through;
  }

  .todo-row.is-failed {
    color: var(--error);
  }

  .todo-row.is-running {
    color: var(--primary);
  }

  .todo-seq {
    font-size: var(--text-2xs);
    color: var(--foreground-muted);
    min-width: 18px;
    flex-shrink: 0;
    font-variant-numeric: tabular-nums;
  }

  .todo-check {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 16px;
    height: 16px;
    flex-shrink: 0;
    color: inherit;
  }

  .todo-row.is-done .todo-check {
    color: var(--success);
  }

  .todo-circle {
    width: 8px;
    height: 8px;
    border: 1.5px solid var(--foreground-muted);
    border-radius: var(--radius-full);
  }

  .todo-text {
    flex: 1;
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .todo-worker {
    font-size: var(--text-2xs);
    flex-shrink: 0;
    font-weight: var(--font-medium);
  }

</style>