<script lang="ts">
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

  // 折叠状态
  let collapsed = $state(true);

  // 初始化折叠状态
  $effect(() => {
    // 流式时默认展开，否则使用 initialExpanded 或默认折叠
    if (isStreaming) {
      collapsed = false;
    } else if (initialExpanded !== undefined) {
      collapsed = !initialExpanded;
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
  const summary = $derived(() => {
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
      <svg width="12" height="12" viewBox="0 0 16 16" fill="currentColor">
        <path d="M4.646 1.646a.5.5 0 0 1 .708 0l6 6a.5.5 0 0 1 0 .708l-6 6a.5.5 0 0 1-.708-.708L10.293 8 4.646 2.354a.5.5 0 0 1 0-.708z"/>
      </svg>
    </span>
    
    <span class="thinking-icon">💭</span>
    
    <span class="thinking-title">
      <span class="title-text">思考过程</span>
      <span class="thinking-summary">{summary()}</span>
    </span>
    
    <span class="thinking-badge">{thinking.length} 步</span>
  </button>
  
  {#if !collapsed}
    <div class="thinking-content">
      <div class="thinking-body">
        {thinkingContent}
      </div>
    </div>
  {/if}
</div>

<style>
  .thinking-block {
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    margin: var(--spacing-sm) 0;
    background: rgba(139, 92, 246, 0.05);
    overflow: hidden;
  }

  .thinking-block.streaming {
    border-color: var(--info);
  }

  .thinking-header {
    display: flex;
    align-items: center;
    gap: var(--spacing-sm);
    width: 100%;
    padding: var(--spacing-sm) var(--spacing-md);
    background: transparent;
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
    color: var(--vscode-descriptionForeground, #888);
  }

  .collapsed .chevron {
    transform: rotate(0deg);
  }

  .thinking-block:not(.collapsed) .chevron {
    transform: rotate(90deg);
  }

  .thinking-icon {
    font-size: 14px;
  }

  .thinking-title {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 2px;
    overflow: hidden;
  }

  .title-text {
    font-weight: 500;
    font-size: var(--font-size-sm);
  }

  .thinking-summary {
    font-size: 11px;
    color: var(--vscode-descriptionForeground, #888);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .thinking-badge {
    font-size: 11px;
    padding: 2px 6px;
    background: rgba(139, 92, 246, 0.2);
    color: #a78bfa;
    border-radius: var(--radius-sm);
    white-space: nowrap;
  }

  .thinking-content {
    padding: var(--spacing-md);
    border-top: 1px solid var(--border);
  }

  .thinking-body {
    font-size: var(--font-size-sm);
    line-height: 1.6;
    color: var(--vscode-descriptionForeground, #aaa);
    white-space: pre-wrap;
    word-break: break-word;
  }

  /* 流式动画 */
  .streaming .thinking-badge {
    animation: pulse 1.5s ease-in-out infinite;
  }

  @keyframes pulse {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.5; }
  }
</style>

