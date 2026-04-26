<script lang="ts">
  import type { TimelineRenderItem } from '../types/message';
  import MessageList from './MessageList.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { messagesState } from '../stores/messages.svelte';
  import { getTaskGraphState, getTaskStatusModifier } from '../stores/task-graph-store.svelte';
  import type { TaskDto, TaskStatus } from '../shared/rust-backend-types';
  import Icon from './Icon.svelte';

  // Props
  interface Props {
    workerName?: string;
    renderItems: TimelineRenderItem[];
    isActive?: boolean;
  }

  let { workerName, renderItems, isActive = false }: Props = $props();

  const currentSessionId = $derived(messagesState.currentSessionId);
  const taskGraph = $derived(getTaskGraphState(currentSessionId));
  const projection = $derived(taskGraph.projection);
  const projectionTasks = $derived(projection?.tasks ?? []);

  // 根据 workerName（roleId）匹配该 Worker 当前绑定的任务
  const boundTasks = $derived.by((): TaskDto[] => {
    if (!workerName) return [];
    return projectionTasks.filter((task) =>
      task.executor_binding?.target_role === workerName
    );
  });

  // 当前活跃任务：优先 Running，其次 Ready/Blocked/Verifying/Repairing/AwaitingApproval
  const activeTask = $derived.by((): TaskDto | null => {
    const priority: TaskStatus[] = [
      'Running', 'Ready', 'Verifying', 'Repairing', 'Blocked', 'AwaitingApproval',
    ];
    for (const status of priority) {
      const found = boundTasks.find((t) => t.status === status);
      if (found) return found;
    }
    return boundTasks[0] ?? null;
  });

  // 历史完成任务（同一 Worker 下已完成的）
  const completedTasks = $derived(
    boundTasks.filter((t) => t.status === 'Completed' || t.status === 'Skipped')
  );

  const emptyState = $derived({
    icon: 'message-square',
    title: i18n.t('agentTab.empty.title'),
    hint: i18n.t('agentTab.empty.hint'),
  });

  function getTaskStatusLabel(status: TaskStatus): string {
    switch (status) {
      case 'Draft': return '待规划';
      case 'Ready': return '待执行';
      case 'Running': return '执行中';
      case 'Blocked': return '已暂停';
      case 'AwaitingApproval': return '等待确认';
      case 'Verifying': return '验证中';
      case 'Repairing': return '修复中';
      case 'Completed': return '已完成';
      case 'Failed': return '失败';
      case 'Cancelled': return '已取消';
      case 'Skipped': return '已跳过';
      default: return status;
    }
  }

  function getTaskKindLabel(kind: TaskDto['kind']): string {
    switch (kind) {
      case 'Objective': return '目标';
      case 'Phase': return '阶段';
      case 'WorkPackage': return '工作包';
      case 'Action': return '步骤';
      case 'Validation': return '验证';
      case 'Repair': return '修复';
      case 'Decision': return '决策';
      default: return kind;
    }
  }

  const activeTaskStatusIcon = $derived.by(() => {
    if (!activeTask) return { name: 'circleOutline' as const, spinning: false };
    switch (activeTask.status) {
      case 'Running': return { name: 'loader' as const, spinning: true };
      case 'Completed': return { name: 'check-circle' as const, spinning: false };
      case 'Failed': return { name: 'x-circle' as const, spinning: false };
      case 'Cancelled':
      case 'Skipped': return { name: 'skip-forward' as const, spinning: false };
      case 'Blocked': return { name: 'alert-circle' as const, spinning: false };
      case 'AwaitingApproval': return { name: 'shield' as const, spinning: false };
      case 'Verifying': return { name: 'check-circle' as const, spinning: true };
      case 'Repairing': return { name: 'wrench' as const, spinning: true };
      default: return { name: 'circleOutline' as const, spinning: false };
    }
  });
</script>

<div class="agent-tab">
  {#if activeTask}
    <div class="agent-task-binding">
      <div class="atb-header">
        <span class="atb-kind">{getTaskKindLabel(activeTask.kind)}</span>
        <span class="atb-status atb-status--{getTaskStatusModifier(activeTask.status)}">
          {#if activeTaskStatusIcon.spinning}
            <Icon name={activeTaskStatusIcon.name} size={12} class="spinning" />
          {:else}
            <Icon name={activeTaskStatusIcon.name} size={12} />
          {/if}
          {getTaskStatusLabel(activeTask.status)}
        </span>
      </div>
      <div class="atb-title">{activeTask.title}</div>
      {#if activeTask.goal && activeTask.goal !== activeTask.title}
        <div class="atb-goal">{activeTask.goal}</div>
      {/if}

      {#if activeTask.output_refs.length > 0 || activeTask.evidence_refs.length > 0}
        <div class="atb-outputs">
          {#if activeTask.output_refs.length > 0}
            <div class="atb-output-group">
              <span class="atb-output-label">产出</span>
              <div class="atb-output-list">
                {#each activeTask.output_refs as ref}
                  <span class="atb-output-chip">{ref}</span>
                {/each}
              </div>
            </div>
          {/if}
          {#if activeTask.evidence_refs.length > 0}
            <div class="atb-output-group">
              <span class="atb-output-label">证据</span>
              <div class="atb-output-list">
                {#each activeTask.evidence_refs as ref}
                  <span class="atb-output-chip">{ref}</span>
                {/each}
              </div>
            </div>
          {/if}
        </div>
      {/if}

      {#if activeTask.status === 'Blocked' && activeTask.decision_payload}
        <div class="atb-blocked">
          <Icon name="alert-circle" size={12} />
          <span>{activeTask.decision_payload.blocked_reason || '任务被阻塞'}</span>
        </div>
      {:else if activeTask.status === 'Blocked'}
        <div class="atb-blocked">
          <Icon name="alert-circle" size={12} />
          <span>任务被阻塞，等待外部条件</span>
        </div>
      {/if}
    </div>
  {:else if boundTasks.length > 0}
    <div class="agent-task-binding agent-task-binding--idle">
      <div class="atb-header">
        <span class="atb-kind">Worker</span>
        <span class="atb-status atb-status--idle">
          <Icon name="circleOutline" size={12} />
          空闲
        </span>
      </div>
      <div class="atb-title">{workerName || '未命名 Worker'}</div>
      <div class="atb-goal">当前没有活跃任务绑定</div>
    </div>
  {/if}

  {#if completedTasks.length > 0}
    <div class="agent-completed-tasks">
      <div class="atb-section-label">已完成 ({completedTasks.length})</div>
      {#each completedTasks as task (task.task_id)}
        <div class="atb-completed-row">
          <Icon name="check-circle" size={12} />
          <span class="atb-completed-title">{task.title}</span>
          <span class="atb-completed-kind">{getTaskKindLabel(task.kind)}</span>
        </div>
      {/each}
    </div>
  {/if}

  <div class="agent-message-list">
    <!-- 复用 MessageList 组件，displayContext='worker' 标识 Worker 面板 -->
    <!-- Worker 面板中的生命周期卡片与执行流统一按语义时间轴渲染 -->
    <MessageList workerName={workerName} {renderItems} {emptyState} displayContext="worker" {isActive} />
  </div>
</div>

<style>
  .agent-tab {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
    overflow: hidden;
  }

  .agent-task-binding {
    flex-shrink: 0;
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    padding: var(--space-3) var(--space-4);
    border-bottom: 1px solid var(--border);
    background: var(--surface-1);
  }

  .agent-task-binding--idle {
    opacity: 0.7;
  }

  .atb-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
  }

  .atb-kind {
    font-size: var(--text-2xs);
    font-weight: var(--font-medium);
    color: var(--foreground-muted);
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: 1px 6px;
  }

  .atb-status {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: var(--text-2xs);
    border-radius: 999px;
    padding: 2px 8px;
    border: 1px solid transparent;
    white-space: nowrap;
  }

  .atb-status--running {
    color: var(--primary);
    background: var(--primary-muted);
    border-color: color-mix(in srgb, var(--primary) 30%, var(--border));
  }

  .atb-status--completed {
    color: var(--success);
    background: var(--success-muted);
    border-color: color-mix(in srgb, var(--success) 32%, var(--border));
  }

  .atb-status--failed {
    color: var(--error);
    background: var(--error-muted);
    border-color: color-mix(in srgb, var(--error) 32%, var(--border));
  }

  .atb-status--blocked,
  .atb-status--awaiting-approval {
    color: var(--warning);
    background: var(--warning-muted);
    border-color: color-mix(in srgb, var(--warning) 30%, var(--border));
  }

  .atb-status--verifying,
  .atb-status--repairing {
    color: var(--primary);
    background: var(--primary-muted);
    border-color: color-mix(in srgb, var(--primary) 30%, var(--border));
  }

  .atb-status--idle,
  .atb-status--ready,
  .atb-status--draft {
    color: var(--foreground-muted);
    background: var(--surface-2);
    border-color: var(--border);
  }

  .atb-title {
    font-size: var(--text-sm);
    font-weight: var(--font-semibold);
    color: var(--foreground);
    line-height: var(--leading-tight);
  }

  .atb-goal {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    line-height: var(--leading-normal);
  }

  .atb-outputs {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
    margin-top: var(--space-1);
  }

  .atb-output-group {
    display: flex;
    align-items: flex-start;
    gap: var(--space-2);
  }

  .atb-output-label {
    font-size: var(--text-2xs);
    font-weight: var(--font-medium);
    color: var(--foreground-muted);
    flex-shrink: 0;
    padding-top: 2px;
  }

  .atb-output-list {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    min-width: 0;
  }

  .atb-output-chip {
    font-size: var(--text-2xs);
    color: var(--foreground-muted);
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: 1px 5px;
    max-width: 200px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .atb-blocked {
    display: inline-flex;
    align-items: center;
    gap: var(--space-1);
    font-size: var(--text-xs);
    color: var(--warning);
    background: var(--warning-muted);
    border: 1px solid color-mix(in srgb, var(--warning) 30%, var(--border));
    border-radius: var(--radius-md);
    padding: var(--space-2) var(--space-3);
    margin-top: var(--space-1);
  }

  .agent-completed-tasks {
    flex-shrink: 0;
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
    padding: var(--space-2) var(--space-4);
    border-bottom: 1px solid var(--border);
    background: var(--surface-1);
  }

  .atb-section-label {
    font-size: var(--text-2xs);
    font-weight: var(--font-medium);
    color: var(--foreground-muted);
    letter-spacing: 0.04em;
  }

  .atb-completed-row {
    display: flex;
    align-items: center;
    gap: var(--space-1);
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  .atb-completed-title {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .atb-completed-kind {
    font-size: var(--text-2xs);
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: 1px 5px;
  }

  .agent-message-list {
    flex: 1;
    min-height: 0;
    overflow: hidden;
  }

  :global(.spinning) {
    animation: spin 1s linear infinite;
  }

  @keyframes spin {
    from { transform: rotate(0deg); }
    to { transform: rotate(360deg); }
  }
</style>
