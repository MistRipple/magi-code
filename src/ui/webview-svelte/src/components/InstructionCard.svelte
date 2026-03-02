<script lang="ts">
  import Icon from './Icon.svelte';
  import MarkdownContent from './MarkdownContent.svelte';

  // Worker 类型定义
  type WorkerType = 'claude' | 'codex' | 'gemini' | 'default';

  interface Props {
    /** 指令内容（支持 Markdown） */
    content: string;
    /** 目标 Worker 名称 */
    targetWorker?: string;
    /** 是否流式输出中 */
    isStreaming?: boolean;
    /** 指令元数据（用于结构化渲染） */
    metadata?: Record<string, unknown>;
  }

  interface LaneTask {
    taskId: string;
    title: string;
    status: 'pending' | 'waiting_deps' | 'running' | 'completed' | 'failed' | 'skipped' | 'cancelled';
    dependsOn: string[];
    isCurrent: boolean;
  }

  interface LaneViewModel {
    laneIndex: number;
    laneTotal: number;
    currentTaskId?: string;
    tasks: LaneTask[];
  }

  let { content, targetWorker, isStreaming = false, metadata }: Props = $props();

  // Worker 类型和颜色映射
  const workerColorMap: Record<WorkerType, { colorVar: string; icon: string; label: string }> = {
    claude: { colorVar: '--color-claude', icon: '🧠', label: 'Claude' },
    codex: { colorVar: '--color-codex', icon: '⚡', label: 'Codex' },
    gemini: { colorVar: '--color-gemini', icon: '✨', label: 'Gemini' },
    default: { colorVar: '--primary', icon: '🤖', label: 'Worker' },
  };

  // 解析 worker 类型
  function getWorkerType(name: string | undefined): WorkerType {
    if (!name) return 'default';
    const lower = name.toLowerCase();
    if (lower.includes('claude')) return 'claude';
    if (lower.includes('codex')) return 'codex';
    if (lower.includes('gemini')) return 'gemini';
    return 'default';
  }

  const workerType = $derived(getWorkerType(targetWorker));
  const workerConfig = $derived(workerColorMap[workerType]);

  // 显示的 Worker 名称
  const displayWorkerName = $derived(
    targetWorker ? targetWorker.charAt(0).toUpperCase() + targetWorker.slice(1) : null
  );

  function toSafeInt(value: unknown): number | undefined {
    if (typeof value !== 'number' || !Number.isFinite(value)) return undefined;
    const normalized = Math.floor(value);
    return normalized > 0 ? normalized : undefined;
  }

  function parseLaneTasks(raw: unknown): LaneTask[] {
    if (!Array.isArray(raw)) return [];
    const list: LaneTask[] = [];
    for (const item of raw) {
      if (!item || typeof item !== 'object') continue;
      const record = item as Record<string, unknown>;
      const taskId = typeof record.taskId === 'string' ? record.taskId.trim() : '';
      const title = typeof record.title === 'string' ? record.title.trim() : '';
      const status = typeof record.status === 'string' ? record.status : '';
      if (!taskId || !title) continue;
      if (!['pending', 'waiting_deps', 'running', 'completed', 'failed', 'skipped', 'cancelled'].includes(status)) {
        continue;
      }
      const dependsOn = Array.isArray(record.dependsOn)
        ? record.dependsOn.filter((dep): dep is string => typeof dep === 'string' && dep.trim().length > 0)
        : [];
      list.push({
        taskId,
        title,
        status: status as LaneTask['status'],
        dependsOn,
        isCurrent: Boolean(record.isCurrent),
      });
    }
    return list;
  }

  const laneView = $derived.by((): LaneViewModel | null => {
    if (!metadata) return null;
    const laneIndex = toSafeInt(metadata.laneIndex);
    const laneTotal = toSafeInt(metadata.laneTotal);
    const laneTasks = parseLaneTasks(metadata.laneTasks);
    if (!laneIndex || !laneTotal || laneTasks.length === 0) {
      return null;
    }
    const currentTaskId = typeof metadata.laneCurrentTaskId === 'string'
      ? metadata.laneCurrentTaskId.trim()
      : undefined;
    return {
      laneIndex,
      laneTotal,
      currentTaskId,
      tasks: laneTasks,
    };
  });

  function isCompletedStatus(status: LaneTask['status']): boolean {
    return status === 'completed';
  }

  function isPendingStatus(status: LaneTask['status']): boolean {
    return status === 'pending' || status === 'waiting_deps';
  }

  function statusLabel(status: LaneTask['status']): string {
    switch (status) {
      case 'running':
        return '进行中';
      case 'completed':
        return '已完成';
      case 'failed':
        return '失败';
      case 'skipped':
        return '已跳过';
      case 'cancelled':
        return '已取消';
      case 'waiting_deps':
        return '等待依赖';
      case 'pending':
      default:
        return '待执行';
    }
  }

  const currentTask = $derived(
    laneView?.tasks.find(task =>
      task.isCurrent || (laneView.currentTaskId ? task.taskId === laneView.currentTaskId : false)
    )
  );
  const pendingTasks = $derived(
    laneView ? laneView.tasks.filter(task => isPendingStatus(task.status)) : []
  );
  const completedTasks = $derived(
    laneView ? laneView.tasks.filter(task => isCompletedStatus(task.status)) : []
  );
  const otherTasks = $derived(
    laneView
      ? laneView.tasks.filter(task =>
        task !== currentTask
        && !isPendingStatus(task.status)
        && !isCompletedStatus(task.status)
      )
      : []
  );
</script>

<div
  class="instruction-card"
  style="--worker-color: var({workerConfig.colorVar})"
>
  <!-- 卡片头部：图标 + 标题 + Worker 名称 -->
  <div class="card-header">
    <div class="header-left">
      <span class="header-icon">
        <Icon name="target" size={14} />
      </span>
      <span class="header-title">任务说明</span>
    </div>
    {#if displayWorkerName}
      <div class="worker-tag">
        <span class="worker-icon">{workerConfig.icon}</span>
        <span class="worker-name">{displayWorkerName}</span>
      </div>
    {/if}
  </div>

  <!-- 内容区 -->
  <div class="card-content">
    {#if laneView}
      <div class="lane-progress">
        <span class="progress-label">执行队列</span>
        <span class="progress-value">{laneView.laneIndex}/{laneView.laneTotal}</span>
      </div>

      {#if currentTask}
        <div class="task-row current">
          <span class="task-title">{currentTask.title}</span>
          <span class="task-status status-running">{statusLabel(currentTask.status)}</span>
        </div>
      {/if}

      {#if pendingTasks.length > 0}
        <div class="task-group">
          <div class="group-title">待执行</div>
          {#each pendingTasks as task (`pending-${task.taskId}`)}
            <div class="task-row pending">
              <span class="task-title">{task.title}</span>
              <span class="task-status status-pending">{statusLabel(task.status)}</span>
            </div>
          {/each}
        </div>
      {/if}

      {#if otherTasks.length > 0}
        <div class="task-group">
          <div class="group-title">处理中断</div>
          {#each otherTasks as task (`other-${task.taskId}`)}
            <div class="task-row neutral">
              <span class="task-title">{task.title}</span>
              <span class="task-status status-neutral">{statusLabel(task.status)}</span>
            </div>
          {/each}
        </div>
      {/if}

      {#if completedTasks.length > 0}
        <details class="task-group completed-group">
          <summary class="group-title">已完成 {completedTasks.length} 项</summary>
          {#each completedTasks as task (`done-${task.taskId}`)}
            <div class="task-row done">
              <span class="task-title">{task.title}</span>
              <span class="task-status status-done">{statusLabel(task.status)}</span>
            </div>
          {/each}
        </details>
      {/if}
    {:else}
      <MarkdownContent {content} {isStreaming} />
    {/if}
  </div>
</div>

<style>
  /* 任务说明卡片 - 使用 primary 颜色边框和浅色背景 */
  .instruction-card {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    padding: var(--space-3) var(--space-4);
    border-radius: var(--radius-md);
    /* 使用 primary 颜色作为基调 */
    background: color-mix(in srgb, var(--worker-color) 8%, var(--surface-1));
    border: 1px solid color-mix(in srgb, var(--worker-color) 30%, var(--border));
    border-left: 3px solid var(--worker-color);
    /* 平滑过渡 */
    transition: all var(--transition-fast);
  }

  /* 卡片头部 */
  .card-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
  }

  .header-left {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }

  .header-icon {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 20px;
    height: 20px;
    border-radius: var(--radius-sm);
    background: color-mix(in srgb, var(--worker-color) 15%, transparent);
    color: var(--worker-color);
  }

  .header-title {
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--foreground);
    text-transform: uppercase;
    letter-spacing: 0.5px;
  }

  /* Worker 标签 */
  .worker-tag {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: var(--text-xs);
    padding: 2px 8px;
    border-radius: var(--radius-sm);
    background: color-mix(in srgb, var(--worker-color) 12%, transparent);
    color: var(--worker-color);
    border: 1px solid color-mix(in srgb, var(--worker-color) 25%, transparent);
  }

  .worker-icon {
    font-size: 12px;
  }

  .worker-name {
    font-weight: 500;
  }

  /* 卡片内容 */
  .card-content {
    font-size: var(--text-sm);
    color: var(--foreground);
    line-height: 1.6;
  }

  .lane-progress {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: var(--space-2);
    font-size: var(--text-xs);
  }

  .progress-label {
    color: var(--muted-foreground);
    font-weight: 500;
  }

  .progress-value {
    font-weight: 700;
    color: var(--worker-color);
  }

  .task-group {
    margin-top: var(--space-2);
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
  }

  .group-title {
    font-size: var(--text-xs);
    color: var(--muted-foreground);
    font-weight: 600;
    margin-bottom: 2px;
  }

  .task-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
    padding: 4px 8px;
    border-radius: var(--radius-sm);
    border: 1px solid color-mix(in srgb, var(--border) 80%, transparent);
    background: color-mix(in srgb, var(--surface-1) 92%, var(--worker-color) 8%);
  }

  .task-row.current {
    border-color: color-mix(in srgb, var(--worker-color) 42%, var(--border));
    background: color-mix(in srgb, var(--worker-color) 16%, var(--surface-1));
  }

  .task-row.done {
    opacity: 0.88;
  }

  .task-title {
    flex: 1;
    min-width: 0;
    font-size: var(--text-xs);
    line-height: 1.4;
  }

  .task-status {
    font-size: 11px;
    font-weight: 600;
    white-space: nowrap;
    padding: 1px 6px;
    border-radius: 10px;
    border: 1px solid transparent;
  }

  .status-running {
    color: color-mix(in srgb, var(--worker-color) 86%, var(--foreground));
    border-color: color-mix(in srgb, var(--worker-color) 48%, transparent);
    background: color-mix(in srgb, var(--worker-color) 16%, transparent);
  }

  .status-pending {
    color: var(--muted-foreground);
    border-color: color-mix(in srgb, var(--muted-foreground) 30%, transparent);
    background: color-mix(in srgb, var(--surface-2) 88%, transparent);
  }

  .status-done {
    color: var(--success);
    border-color: color-mix(in srgb, var(--success) 40%, transparent);
    background: color-mix(in srgb, var(--success) 14%, transparent);
  }

  .status-neutral {
    color: var(--warning);
    border-color: color-mix(in srgb, var(--warning) 40%, transparent);
    background: color-mix(in srgb, var(--warning) 12%, transparent);
  }

  .completed-group > summary {
    cursor: pointer;
    user-select: none;
    list-style: none;
  }

  .completed-group > summary::-webkit-details-marker {
    display: none;
  }

  /* 对内容区的 Markdown 样式微调 */
  .card-content :global(p:first-child) {
    margin-top: 0;
  }

  .card-content :global(p:last-child) {
    margin-bottom: 0;
  }

  .card-content :global(ul),
  .card-content :global(ol) {
    margin: var(--space-2) 0;
    padding-left: var(--space-4);
  }

  .card-content :global(li) {
    margin: var(--space-1) 0;
  }

  .card-content :global(code) {
    font-size: 0.85em;
    padding: 1px 4px;
    border-radius: 3px;
    background: var(--code-bg);
  }
</style>
