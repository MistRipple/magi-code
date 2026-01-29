<script lang="ts">
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
</script>

<div class="subtask-card" class:failed={card.status === 'failed'}>
  <div class="subtask-card-header">
    <div class="subtask-card-title">{card.title || '子任务总结'}</div>
    <span class="subtask-card-status">{statusText}</span>
  </div>

  <div class="subtask-card-overview">
    <div class="overview-item">
      <div class="overview-label">执行者</div>
      <div class="overview-value">{executor}</div>
    </div>
    <div class="overview-item">
      <div class="overview-label">耗时</div>
      <div class="overview-value">{card.duration || '-'}</div>
    </div>
    <div class="overview-item">
      <div class="overview-label">状态</div>
      <div class="overview-value status-{card.status || 'completed'}">{statusText}</div>
    </div>
    {#if typeof card.toolCount === 'number'}
      <div class="overview-item">
        <div class="overview-label">工具调用</div>
        <div class="overview-value">{card.toolCount} 次</div>
      </div>
    {/if}
  </div>

  {#if card.description}
    <div class="subtask-section">
      <div class="subtask-section-title">任务描述</div>
      <div class="subtask-section-body">{card.description}</div>
    </div>
  {/if}

  {#if card.error}
    <div class="subtask-section error">
      <div class="subtask-section-title">错误信息</div>
      <div class="subtask-section-body">{card.error}</div>
    </div>
  {/if}

  {#if card.changes && card.changes.length > 0}
    <div class="subtask-section">
      <div class="subtask-section-title">文件变更 ({card.changes.length})</div>
      <ul class="file-list">
        {#each card.changes as file}
          <li>{file}</li>
        {/each}
      </ul>
    </div>
  {/if}

  {#if card.verification && card.verification.length > 0}
    <div class="subtask-section">
      <div class="subtask-section-title">后续验证建议</div>
      <ul class="verification-list">
        {#each card.verification as item}
          <li>{item}</li>
        {/each}
      </ul>
    </div>
  {/if}
</div>

<style>
  .subtask-card {
    border: 1px solid var(--border);
    background: var(--surface-1);
    border-radius: var(--radius-lg);
    padding: var(--space-4);
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }
  .subtask-card.failed {
    border-color: color-mix(in srgb, var(--error) 40%, var(--border));
  }
  .subtask-card-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
  }
  .subtask-card-title {
    font-size: var(--text-base);
    font-weight: 600;
  }
  .subtask-card-status {
    font-size: var(--text-xs);
    padding: 2px 8px;
    border-radius: var(--radius-full);
    background: var(--surface-2);
    color: var(--foreground-muted);
  }
  .subtask-card.failed .subtask-card-status {
    background: color-mix(in srgb, var(--error) 12%, var(--surface-2));
    color: var(--error);
  }

  .subtask-card-overview {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(120px, 1fr));
    gap: var(--space-3);
  }
  .overview-item {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: var(--space-2);
    border-radius: var(--radius-md);
    background: var(--surface-2);
  }
  .overview-label {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
  }
  .overview-value {
    font-size: var(--text-sm);
    font-weight: 600;
  }
  .overview-value.status-failed { color: var(--error); }
  .overview-value.status-completed { color: var(--success); }

  .subtask-section {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }
  .subtask-section-title {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .subtask-section-body {
    font-size: var(--text-sm);
    color: var(--foreground);
    line-height: 1.5;
    white-space: pre-wrap;
  }
  .subtask-section.error .subtask-section-body {
    color: var(--error);
  }
  .file-list,
  .verification-list {
    margin: 0;
    padding-left: 18px;
    font-size: var(--text-sm);
    color: var(--foreground);
  }
  .file-list li,
  .verification-list li {
    margin-bottom: 4px;
  }
</style>
