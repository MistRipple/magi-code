<script lang="ts">
  import { getState } from '../stores/messages.svelte';
  import { ensureArray } from '../lib/utils';
  import type { WorkerSessionState, Task } from '../types/message';
  import { vscode } from '../lib/vscode-bridge';
  import Icon from './Icon.svelte';

  const appState = getState();

  // 数据源
  const tasks = $derived(ensureArray(appState.tasks) as (Task & Record<string, any>)[]);
  const missionPlan = $derived(appState.missionPlan);
  const workerSessions = $derived(appState.workerSessions);
  const workerSessionList = $derived(
    workerSessions ? Array.from(workerSessions.values()) as WorkerSessionState[] : []
  );

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

  // 将 task 和 missionPlan assignment 合并
  const enrichedTasks = $derived.by(() => {
    return tasks.map(task => {
      const taskId = task.id || '';
      // 从 missionPlan 找到对应的 assignments
      const assignments = missionPlan?.assignments?.filter(
        (a: any) => a.missionId === taskId || a.taskId === taskId
      ) || [];
      // 如果没匹配到，用 task 自身的 subTasks
      const subTasks = (task as any).subTasks || [];
      // 获取关联的 worker sessions
      const sessions = workerSessionList.filter(
        s => assignments.some((a: any) => a.id === s.assignmentId)
      );
      // 计算 todo 总进度
      const allTodos = assignments.flatMap((a: any) => ensureArray(a.todos));
      const todoTotal = allTodos.length || subTasks.length;
      const todoCompleted = allTodos.length > 0
        ? allTodos.filter((t: any) => t.status === 'completed' || t.status === 'skipped').length
        : subTasks.filter((s: any) => s.status === 'completed' || s.status === 'skipped').length;
      const todoProgress = todoTotal > 0 ? Math.round((todoCompleted / todoTotal) * 100) : ((task as any).progress ?? 0);

      return { ...task, assignments, subTasks, sessions, todoTotal, todoCompleted, todoProgress };
    });
  });

  // 独立的 missionPlan assignments（没有关联到任何 task 的）
  const standaloneAssignments = $derived.by(() => {
    if (!missionPlan?.assignments?.length) return [];
    const taskIds = new Set(tasks.map(t => t.id));
    return missionPlan.assignments.filter(
      (a: any) => !taskIds.has(a.missionId) && !taskIds.has(a.taskId)
    );
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

  // 自动展开 running 任务
  $effect(() => {
    const runningIds = tasks.filter(t => t.status === 'running').map(t => t.id).filter(Boolean) as string[];
    if (runningIds.length > 0) {
      const next = new Set(expandedTasks);
      for (const id of runningIds) next.add(id);
      expandedTasks = next;
    }
  });

  function startTask(taskId?: string) {
    if (!taskId) return;
    vscode.postMessage({ type: 'startTask', taskId });
  }

  function deleteTask(taskId?: string) {
    if (!taskId) return;
    vscode.postMessage({ type: 'deleteTask', taskId });
  }
</script>

<div class="tasks-panel">
  {#if enrichedTasks.length === 0 && standaloneAssignments.length === 0}
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
      {#each enrichedTasks as task (task.id)}
        {@const isExpanded = expandedTasks.has(task.id || '')}
        {@const hasChildren = task.todoTotal > 0 || task.assignments.length > 0}
        <div class="task-card" class:is-running={task.status === 'running'} class:is-done={task.status === 'completed'} class:is-failed={task.status === 'failed'} class:is-cancelled={task.status === 'cancelled'}>
          <!-- 任务头部 -->
          <div class="task-header" role="button" tabindex="0" onclick={() => hasChildren && toggleExpand(task.id || '')} onkeydown={(e) => e.key === 'Enter' && hasChildren && toggleExpand(task.id || '')}>
            <!-- 折叠箭头 -->
            <span class="task-chevron" class:expanded={isExpanded} class:hidden={!hasChildren}>
              <Icon name="chevron-right" size={12} />
            </span>
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
              <span class="task-name">{task.name || (task as any).prompt || '未命名任务'}</span>
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
              {#each task.assignments as assignment (assignment.id)}
                <!-- Worker 分组标题 -->
                {#if task.assignments.length > 1}
                  <div class="worker-group-label">
                    <span class="worker-dot" style="background: {workerColors[assignment.workerId] || 'var(--foreground-muted)'}"></span>
                    <span>{assignment.workerId}</span>
                    <span class="worker-responsibility">{assignment.responsibility || ''}</span>
                  </div>
                {/if}
                {#each (ensureArray(assignment.todos) as any[]) as todo, todoIdx (todo.id)}
                  {#if !todo.parentId}
                    {@const seqNum = (ensureArray(assignment.todos) as any[]).filter((t: any) => !t.parentId).indexOf(todo) + 1}
                    {@const children = (ensureArray(assignment.todos) as any[]).filter((t: any) => t.parentId === todo.id)}
                    <div class="todo-row" class:is-done={todo.status === 'completed' || todo.status === 'skipped'} class:is-failed={todo.status === 'failed'} class:is-running={todo.status === 'in_progress'}>
                      <span class="todo-seq">{seqNum}.</span>
                      <span class="todo-check">
                        {#if todo.status === 'completed' || todo.status === 'skipped'}
                          <Icon name="check" size={12} />
                        {:else if todo.status === 'in_progress'}
                          <Icon name="loader" size={12} class="spinning" />
                        {:else if todo.status === 'failed'}
                          <Icon name="x-circle" size={12} />
                        {:else if todo.status === 'blocked'}
                          <Icon name="alert-circle" size={12} />
                        {:else}
                          <span class="todo-circle"></span>
                        {/if}
                      </span>
                      <span class="todo-text">{todo.content || '未命名'}</span>
                      {#if task.assignments.length <= 1 && assignment.workerId}
                        <span class="todo-worker" style="color: {workerColors[assignment.workerId] || 'var(--foreground-muted)'}">{assignment.workerId}</span>
                      {/if}
                      {#if todo.priority !== undefined && todo.priority <= 1}
                        <span class="todo-priority-badge">P{todo.priority}</span>
                      {/if}
                      {#if todo.outOfScope}
                        <span class="todo-flag">超范围</span>
                      {/if}
                    </div>
                    {#each children as child, childIdx (child.id)}
                      <div class="todo-row todo-child" class:is-done={child.status === 'completed' || child.status === 'skipped'} class:is-failed={child.status === 'failed'} class:is-running={child.status === 'in_progress'}>
                        <span class="todo-seq sub">{seqNum}.{childIdx + 1}</span>
                        <span class="todo-check">
                          {#if child.status === 'completed' || child.status === 'skipped'}
                            <Icon name="check" size={12} />
                          {:else if child.status === 'in_progress'}
                            <Icon name="loader" size={12} class="spinning" />
                          {:else if child.status === 'failed'}
                            <Icon name="x-circle" size={12} />
                          {:else}
                            <span class="todo-circle"></span>
                          {/if}
                        </span>
                        <span class="todo-text">{child.content || '未命名'}</span>
                        {#if child.outOfScope}
                          <span class="todo-flag">超范围</span>
                        {/if}
                      </div>
                    {/each}
                  {/if}
                {/each}
              {/each}
              <!-- 没有 assignment 但有 subTasks 的情况 -->
              {#if task.assignments.length === 0 && task.subTasks.length > 0}
                {#each task.subTasks as sub, subIdx (sub.id || sub.name)}
                  <div class="todo-row" class:is-done={sub.status === 'completed'} class:is-failed={sub.status === 'failed'} class:is-running={sub.status === 'running'}>
                    <span class="todo-seq">{subIdx + 1}.</span>
                    <span class="todo-check">
                      {#if sub.status === 'completed'}
                        <Icon name="check" size={12} />
                      {:else if sub.status === 'running'}
                        <Icon name="loader" size={12} class="spinning" />
                      {:else if sub.status === 'failed'}
                        <Icon name="x-circle" size={12} />
                      {:else}
                        <span class="todo-circle"></span>
                      {/if}
                    </span>
                    <span class="todo-text">{sub.name || sub.content || '未命名'}</span>
                  </div>
                {/each}
              {/if}
            </div>
          {/if}
        </div>
      {/each}
    </div>

    <!-- 独立 Worker Todo（未关联到 Task 的 Assignments） -->
    {#if standaloneAssignments.length > 0}
      <div class="section-divider">
        <span>Worker 任务</span>
      </div>
      <div class="tasks-list">
        {#each standaloneAssignments as assignment (assignment.id)}
          {@const todos = ensureArray(assignment.todos) as any[]}
          {@const doneCount = todos.filter((t: any) => t.status === 'completed' || t.status === 'skipped').length}
          {@const isExpanded = expandedTasks.has(assignment.id)}
          <div class="task-card" class:is-running={assignment.status === 'running'} class:is-done={assignment.status === 'completed'}>
            <div class="task-header" role="button" tabindex="0" onclick={() => todos.length > 0 && toggleExpand(assignment.id)} onkeydown={(e) => e.key === 'Enter' && todos.length > 0 && toggleExpand(assignment.id)}>
              <span class="task-chevron" class:expanded={isExpanded} class:hidden={todos.length === 0}>
                <Icon name="chevron-right" size={12} />
              </span>
              <span class="worker-dot" style="background: {workerColors[assignment.workerId] || 'var(--foreground-muted)'}"></span>
              <div class="task-body">
                <span class="task-name">{assignment.responsibility || '未命名'}</span>
                {#if todos.length > 0}
                  <span class="task-count">{doneCount}/{todos.length}</span>
                {/if}
              </div>
            </div>
            {#if todos.length > 0}
              <div class="task-progress-bar">
                <div class="task-progress-fill" style="width: {todos.length > 0 ? Math.round((doneCount / todos.length) * 100) : 0}%"></div>
              </div>
            {/if}
            {#if isExpanded && todos.length > 0}
              <div class="task-children">
                {#each todos as todo, todoIdx (todo.id)}
                  {#if !todo.parentId}
                    {@const seqNum = todos.filter((t: any) => !t.parentId).indexOf(todo) + 1}
                    {@const children = todos.filter((t: any) => t.parentId === todo.id)}
                    <div class="todo-row" class:is-done={todo.status === 'completed' || todo.status === 'skipped'} class:is-failed={todo.status === 'failed'} class:is-running={todo.status === 'in_progress'}>
                      <span class="todo-seq">{seqNum}.</span>
                      <span class="todo-check">
                        {#if todo.status === 'completed' || todo.status === 'skipped'}
                          <Icon name="check" size={12} />
                        {:else if todo.status === 'in_progress'}
                          <Icon name="loader" size={12} class="spinning" />
                        {:else if todo.status === 'failed'}
                          <Icon name="x-circle" size={12} />
                        {:else}
                          <span class="todo-circle"></span>
                        {/if}
                      </span>
                      <span class="todo-text">{todo.content || '未命名'}</span>
                    </div>
                    {#each children as child, childIdx (child.id)}
                      <div class="todo-row todo-child" class:is-done={child.status === 'completed' || child.status === 'skipped'} class:is-failed={child.status === 'failed'} class:is-running={child.status === 'in_progress'}>
                        <span class="todo-seq sub">{seqNum}.{childIdx + 1}</span>
                        <span class="todo-check">
                          {#if child.status === 'completed' || child.status === 'skipped'}
                            <Icon name="check" size={12} />
                          {:else if child.status === 'in_progress'}
                            <Icon name="loader" size={12} class="spinning" />
                          {:else if child.status === 'failed'}
                            <Icon name="x-circle" size={12} />
                          {:else}
                            <span class="todo-circle"></span>
                          {/if}
                        </span>
                        <span class="todo-text">{child.content || '未命名'}</span>
                        {#if child.outOfScope}
                          <span class="todo-flag">超范围</span>
                        {/if}
                      </div>
                    {/each}
                  {/if}
                {/each}
              </div>
            {/if}
          </div>
        {/each}
      </div>
    {/if}
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

  .todo-row.todo-child {
    padding-left: var(--space-6);
  }

  .todo-seq {
    font-size: var(--text-2xs);
    color: var(--foreground-muted);
    min-width: 18px;
    flex-shrink: 0;
    font-variant-numeric: tabular-nums;
  }

  .todo-seq.sub {
    min-width: 24px;
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

  .todo-priority-badge {
    font-size: 9px;
    padding: 0 4px;
    background: var(--warning-muted);
    color: var(--warning);
    border-radius: var(--radius-xs);
    font-weight: var(--font-semibold);
    flex-shrink: 0;
  }

  .todo-flag {
    font-size: 9px;
    padding: 0 4px;
    background: var(--warning-muted);
    color: var(--warning);
    border-radius: var(--radius-xs);
    flex-shrink: 0;
  }

  /* ========== 分隔线 ========== */
  .section-divider {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    margin: var(--space-3) 0 var(--space-1);
  }

  .section-divider::before,
  .section-divider::after {
    content: '';
    flex: 1;
    height: 1px;
    background: var(--border);
  }
</style>