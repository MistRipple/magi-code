<script lang="ts">
  // Props
  interface Props {
    name: string;
    id?: string;
    input?: unknown;
    output?: unknown;
    error?: string;
    status?: 'pending' | 'running' | 'success' | 'error';
    duration?: number;
    initialExpanded?: boolean;
  }

  let {
    name,
    id,
    input,
    output,
    error,
    status = 'success',
    duration,
    initialExpanded = false
  }: Props = $props();

  // 折叠状态
  let collapsed = $state(true);

  // 初始化
  $effect(() => {
    collapsed = !initialExpanded;
  });

  // 格式化内容
  function formatContent(content: unknown): string {
    if (!content) return '';
    if (typeof content === 'string') return content.trim();
    try {
      return JSON.stringify(content, null, 2);
    } catch {
      return String(content).trim();
    }
  }

  // 状态信息
  const statusInfo = $derived(() => {
    const map: Record<string, { class: string; text: string }> = {
      pending: { class: 'pending', text: '等待中' },
      running: { class: 'running', text: '执行中' },
      success: { class: 'success', text: '成功' },
      error: { class: 'error', text: '失败' },
    };
    return map[status] || { class: 'success', text: '完成' };
  });

  // 检查是否有内容
  const hasInput = $derived(!!input && !!formatContent(input));
  const hasOutput = $derived(!!output && !!formatContent(output));
  const hasError = $derived(!!error && !!error.trim());
  const hasContent = $derived(hasInput || hasOutput || hasError);

  function toggle() {
    collapsed = !collapsed;
  }
</script>

{#if hasContent}
  <div 
    class="tool-call"
    class:collapsed
    data-status={statusInfo().class}
  >
    <button class="tool-header" onclick={toggle}>
      <span class="chevron">
        <svg width="12" height="12" viewBox="0 0 16 16" fill="currentColor">
          <path d="M4.646 1.646a.5.5 0 0 1 .708 0l6 6a.5.5 0 0 1 0 .708l-6 6a.5.5 0 0 1-.708-.708L10.293 8 4.646 2.354a.5.5 0 0 1 0-.708z"/>
        </svg>
      </span>
      
      <span class="tool-icon">🔧</span>
      
      <span class="tool-title">
        <span class="tool-name">{name || '工具调用'}</span>
        {#if id}
          <span class="tool-id">#{id}</span>
        {/if}
      </span>
      
      <span class="tool-status status-{statusInfo().class}">
        {#if status === 'running'}
          <span class="spinner"></span>
        {/if}
        {statusInfo().text}
      </span>
    </button>
    
    {#if !collapsed}
      <div class="tool-content">
        {#if hasInput}
          <div class="tool-section">
            <div class="section-label">输入</div>
            <pre class="section-content">{formatContent(input)}</pre>
          </div>
        {/if}
        
        {#if hasOutput}
          <div class="tool-section">
            <div class="section-label">输出</div>
            <pre class="section-content">{formatContent(output)}</pre>
          </div>
        {/if}
        
        {#if hasError}
          <div class="tool-section error">
            <div class="section-label">错误</div>
            <pre class="section-content">{error}</pre>
          </div>
        {/if}
        
        {#if duration}
          <div class="tool-meta">
            耗时: <strong>{(duration / 1000).toFixed(2)}s</strong>
          </div>
        {/if}
      </div>
    {/if}
  </div>
{/if}

<style>
  .tool-call {
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    margin: var(--spacing-sm) 0;
    overflow: hidden;
  }

  .tool-header {
    display: flex;
    align-items: center;
    gap: var(--spacing-sm);
    width: 100%;
    padding: var(--spacing-sm) var(--spacing-md);
    background: var(--code-bg);
    text-align: left;
    cursor: pointer;
    transition: background var(--transition-fast);
  }

  .tool-header:hover {
    background: var(--vscode-list-hoverBackground, rgba(255,255,255,0.05));
  }

  .chevron {
    display: flex;
    transition: transform var(--transition-fast);
  }

  .collapsed .chevron {
    transform: rotate(0deg);
  }

  .tool-call:not(.collapsed) .chevron {
    transform: rotate(90deg);
  }

  .tool-icon {
    font-size: 14px;
  }

  .tool-title {
    flex: 1;
    display: flex;
    align-items: center;
    gap: var(--spacing-xs);
  }

  .tool-name {
    font-weight: 500;
  }

  .tool-id {
    font-size: var(--font-size-sm);
    color: var(--vscode-descriptionForeground, #888);
    opacity: 0.7;
  }

  .tool-status {
    display: flex;
    align-items: center;
    gap: 4px;
    font-size: var(--font-size-sm);
    padding: 2px 8px;
    border-radius: var(--radius-sm);
  }

  .status-pending { color: var(--warning); }
  .status-running { color: var(--info); }
  .status-success { color: var(--success); }
  .status-error { color: var(--error); }

  .spinner {
    width: 12px;
    height: 12px;
    border: 2px solid currentColor;
    border-top-color: transparent;
    border-radius: 50%;
    animation: spin 1s linear infinite;
  }

  @keyframes spin {
    to { transform: rotate(360deg); }
  }

  .tool-content {
    padding: var(--spacing-md);
    border-top: 1px solid var(--border);
  }

  .tool-section {
    margin-bottom: var(--spacing-md);
  }

  .tool-section:last-child {
    margin-bottom: 0;
  }

  .section-label {
    font-size: var(--font-size-sm);
    color: var(--vscode-descriptionForeground, #888);
    margin-bottom: var(--spacing-xs);
  }

  .section-content {
    font-family: var(--font-mono);
    font-size: var(--font-size-sm);
    background: var(--code-bg);
    padding: var(--spacing-sm);
    border-radius: var(--radius-sm);
    overflow-x: auto;
    margin: 0;
    white-space: pre-wrap;
    word-break: break-word;
  }

  .tool-section.error .section-content {
    color: var(--error);
  }

  .tool-meta {
    font-size: var(--font-size-sm);
    color: var(--vscode-descriptionForeground, #888);
  }
</style>

