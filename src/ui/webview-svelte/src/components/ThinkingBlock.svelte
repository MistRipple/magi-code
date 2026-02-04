<script lang="ts">
  import { untrack } from 'svelte';
  import Icon from './Icon.svelte';
  import MarkdownContent from './MarkdownContent.svelte';

  // Props
  interface Props {
    thinking: Array<string | { content: string }>;
    isStreaming?: boolean;
    initialExpanded?: boolean;
  }

  let {
    thinking,
    isStreaming = false,
    initialExpanded
  }: Props = $props();

  // 🔧 折叠状态：流式期间自动展开，完成后根据 initialExpanded 决定
  // 使用 $effect 监听 isStreaming 变化，实现动态展开/折叠
  let collapsed = $state(untrack(() => isStreaming ? false : (initialExpanded !== undefined ? !initialExpanded : true)));

  // 🔧 当流式开始时自动展开面板
  $effect(() => {
    if (isStreaming && collapsed) {
      collapsed = false;
    }
  });

  // 提取思考内容
  const thinkingContent = $derived(
    thinking
      .map(t => typeof t === 'string' ? t : t.content)
      .join('\n\n')
      .trim()
  );

  // 生成摘要
  const summary = $derived.by(() => {
    if (!thinkingContent) return '正在思考...';
    const plain = thinkingContent
      .replace(/[#*_`~\[\]()]/g, '')
      .replace(/\s+/g, ' ')
      .trim();
    const firstSentence = plain.split(/[。！？.!?]/)[0];
    return firstSentence.length <= 50 ? firstSentence : plain.substring(0, 50) + '...';
  });

  function toggle() {
    collapsed = !collapsed;
  }
</script>

<div
  class="thinking-block"
  class:collapsed
  class:streaming={isStreaming}
>
  <button class="thinking-header" onclick={toggle}>
    <span class="chevron">
      <Icon name="chevron-right" size={12} />
    </span>

    <span class="thinking-icon">
      <Icon name="clock" size={14} />
    </span>

    <span class="thinking-title">
      <span class="title-text">思考过程</span>
      <span class="thinking-summary">{summary}</span>
    </span>

    <span class="thinking-badge">{thinking.length} 步</span>
  </button>

  {#if !collapsed}
    <div class="thinking-content">
      <div class="thinking-body">
        <MarkdownContent content={thinkingContent} {isStreaming} />
      </div>
    </div>
  {/if}
</div>

<style>
  .thinking-block {
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    margin: var(--space-2, 8px) 0;
    background: rgba(139, 92, 246, 0.05);
    overflow: hidden;
  }

  .thinking-block.streaming {
    border-color: #a855f7;
    box-shadow: 0 0 0 1px rgba(168, 85, 247, 0.2);
  }

  .thinking-header {
    display: flex;
    align-items: center;
    gap: var(--space-2, 8px);
    width: 100%;
    padding: var(--space-2, 8px) var(--space-3, 12px);
    background: transparent;
    border: none;
    text-align: left;
    cursor: pointer;
    transition: background var(--transition-fast);
  }

  .thinking-header:hover {
    background: rgba(139, 92, 246, 0.1);
  }

  .chevron {
    display: flex;
    transition: transform var(--transition-fast);
    color: var(--foreground-muted, #888);
  }

  .collapsed .chevron { transform: rotate(0deg); }
  .thinking-block:not(.collapsed) .chevron { transform: rotate(90deg); }

  .thinking-icon {
    display: flex;
    color: #a855f7;
  }

  .thinking-title {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 2px;
    overflow: hidden;
    min-width: 0;
  }

  .title-text {
    font-weight: 500;
    font-size: var(--text-sm, 13px);
    color: var(--foreground);
  }

  .thinking-summary {
    font-size: var(--text-xs, 11px);
    color: var(--foreground-muted, #888);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .thinking-badge {
    font-size: var(--text-xs, 11px);
    padding: 2px 8px;
    background: rgba(139, 92, 246, 0.2);
    color: #a78bfa;
    border-radius: var(--radius-full);
    white-space: nowrap;
    font-weight: 500;
  }

  .thinking-content {
    padding: var(--space-3, 12px);
    border-top: 1px solid var(--border);
    background: rgba(139, 92, 246, 0.02);
    max-height: 400px;
    overflow-y: auto;
    animation: expandContent 0.2s ease-out;
  }

  @keyframes expandContent {
    from { opacity: 0; transform: translateY(-8px); }
    to { opacity: 1; transform: translateY(0); }
  }

  .thinking-body {
    font-size: var(--text-sm, 13px);
    line-height: 1.6;
    color: var(--foreground-muted, #aaa);
  }

  /* 流式动画 */
  .streaming .thinking-badge {
    animation: pulse 1.5s ease-in-out infinite;
  }

  .streaming .thinking-icon {
    animation: spin 2s linear infinite;
  }

  @keyframes pulse {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.5; }
  }

  @keyframes spin {
    to { transform: rotate(360deg); }
  }
</style>
