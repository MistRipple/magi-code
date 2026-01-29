<script lang="ts">
  import { setCurrentBottomTab } from '../stores/messages.svelte';
  import Icon from './Icon.svelte';

  interface SummaryCard {
    title?: string;
    status?: 'completed' | 'failed';
    description?: string;
    executor?: string;
    agent?: string;
    duration?: string;
    changes?: string[];
    verification?: string[];
    error?: string;
    toolCount?: number;
  }

  interface Props {
    card: SummaryCard;
  }

  let { card }: Props = $props();

  const statusText = $derived(card.status === 'failed' ? '失败' : '完成');
  const executor = $derived(card.executor || card.agent || '未知');

  // Worker 类型和颜色映射
  type WorkerType = 'claude' | 'codex' | 'gemini' | 'default';

  const workerColorMap: Record<WorkerType, { colorVar: string; icon: string; label: string }> = {
    claude: { colorVar: '--color-claude', icon: '🧠', label: 'Claude' },
    codex: { colorVar: '--color-codex', icon: '⚡', label: 'Codex' },
    gemini: { colorVar: '--color-gemini', icon: '✨', label: 'Gemini' },
    default: { colorVar: '--foreground-muted', icon: '🤖', label: 'Worker' },
  };

  // 解析 worker 类型
  function getWorkerType(name: string): WorkerType {
    const lower = name.toLowerCase();
    if (lower.includes('claude')) return 'claude';
    if (lower.includes('codex')) return 'codex';
    if (lower.includes('gemini')) return 'gemini';
    return 'default';
  }

  const workerType = $derived(getWorkerType(executor));
  const workerConfig = $derived(workerColorMap[workerType]);

  // 点击跳转到对应的 worker tab
  function handleCardClick() {
    if (workerType !== 'default') {
      setCurrentBottomTab(workerType);
    }
  }

  // 是否可点击
  const isClickable = $derived(workerType !== 'default');
</script>

<button
  class="worker-progress-card"
  class:failed={card.status === 'failed'}
  class:clickable={isClickable}
  style="--worker-color: var({workerConfig.colorVar})"
  onclick={handleCardClick}
  title={isClickable ? `点击查看 ${workerConfig.label} 详情` : ''}
>
  <!-- 卡片头部：worker 图标 + 标题 + 状态 -->
  <div class="card-header">
    <div class="worker-info">
      <span class="worker-icon">{workerConfig.icon}</span>
      <span class="worker-name">{executor}</span>
    </div>
    <div class="card-meta">
      {#if card.duration}
        <span class="duration">{card.duration}</span>
      {/if}
      <span class="status-badge" class:failed={card.status === 'failed'}>
        {statusText}
      </span>
      {#if isClickable}
        <span class="jump-hint">
          <Icon name="chevron-right" size={14} />
        </span>
      {/if}
    </div>
  </div>

  <!-- 任务描述 -->
  {#if card.title || card.description}
    <div class="card-body">
      {card.title || card.description}
    </div>
  {/if}

  <!-- 统计信息行 -->
  <div class="card-stats">
    {#if typeof card.toolCount === 'number'}
      <span class="stat-item">
        <Icon name="tool" size={12} />
        {card.toolCount} 次调用
      </span>
    {/if}
    {#if card.changes && card.changes.length > 0}
      <span class="stat-item">
        <Icon name="file" size={12} />
        {card.changes.length} 文件变更
      </span>
    {/if}
  </div>

  <!-- 错误信息（如果有） -->
  {#if card.error}
    <div class="card-error">
      <Icon name="x-circle" size={14} />
      {card.error}
    </div>
  {/if}
</button>

<style>
  /* Worker 进度卡片 - 使用 worker 颜色作为边框和微色背景 */
  .worker-progress-card {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    padding: var(--space-3) var(--space-4);
    border-radius: var(--radius-md);
    /* 使用 worker 颜色 */
    background: color-mix(in srgb, var(--worker-color) 8%, var(--surface-1));
    border: 1px solid color-mix(in srgb, var(--worker-color) 30%, var(--border));
    border-left: 3px solid var(--worker-color);
    /* 按钮重置 */
    text-align: left;
    cursor: default;
    font-family: inherit;
    font-size: inherit;
    color: inherit;
    width: 100%;
    transition: all var(--transition-fast);
  }

  .worker-progress-card.clickable {
    cursor: pointer;
  }

  .worker-progress-card.clickable:hover {
    background: color-mix(in srgb, var(--worker-color) 12%, var(--surface-1));
    border-color: color-mix(in srgb, var(--worker-color) 50%, var(--border));
  }

  .worker-progress-card.failed {
    --worker-color: var(--error);
  }

  /* 卡片头部 */
  .card-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
  }

  .worker-info {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }

  .worker-icon {
    font-size: var(--text-base);
  }

  .worker-name {
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--worker-color);
    text-transform: uppercase;
    letter-spacing: 0.5px;
  }

  .card-meta {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }

  .duration {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  .status-badge {
    font-size: var(--text-xs);
    padding: 2px 8px;
    border-radius: var(--radius-full);
    background: color-mix(in srgb, var(--success) 15%, transparent);
    color: var(--success);
    font-weight: 500;
  }

  .status-badge.failed {
    background: color-mix(in srgb, var(--error) 15%, transparent);
    color: var(--error);
  }

  .jump-hint {
    display: flex;
    align-items: center;
    color: var(--foreground-muted);
    opacity: 0;
    transition: opacity var(--transition-fast);
  }

  .worker-progress-card.clickable:hover .jump-hint {
    opacity: 1;
  }

  /* 卡片内容 */
  .card-body {
    font-size: var(--text-sm);
    color: var(--foreground);
    line-height: 1.5;
  }

  /* 统计信息 */
  .card-stats {
    display: flex;
    align-items: center;
    gap: var(--space-4);
  }

  .stat-item {
    display: flex;
    align-items: center;
    gap: var(--space-1);
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }

  /* 错误信息 */
  .card-error {
    display: flex;
    align-items: flex-start;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius-sm);
    background: color-mix(in srgb, var(--error) 10%, transparent);
    color: var(--error);
    font-size: var(--text-sm);
    line-height: 1.4;
  }
</style>
