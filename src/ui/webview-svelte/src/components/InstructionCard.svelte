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
  }

  let { content, targetWorker, isStreaming = false }: Props = $props();

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
    <MarkdownContent {content} {isStreaming} />
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
