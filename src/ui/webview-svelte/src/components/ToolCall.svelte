<script lang="ts">
  import Icon from './Icon.svelte';
  import FileSpan from './FileSpan.svelte';
  import { vscode } from '../lib/vscode-bridge';

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
    filepath?: string;
    onOpenFile?: (filepath: string) => void;
  }

  let {
    name,
    id,
    input,
    output,
    error,
    status = 'success',
    duration,
    initialExpanded = false,
    filepath,
    onOpenFile
  }: Props = $props();

  // 折叠状态
  let collapsed = $state(true);
  let copySuccess = $state(false);

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

  // 获取工具图标
  function getToolIcon(toolName: string): string {
    if (!toolName || typeof toolName !== 'string') {
      vscode.postMessage({
        type: 'uiError',
        component: 'ToolCall',
        detail: { toolName, id, status },
        stack: new Error('ToolCall: invalid toolName').stack,
      });
      throw new Error('ToolCall: invalid toolName');
    }
    const iconMap: Record<string, string> = {
      'read_file': 'file-text',
      'write_file': 'file-plus',
      'edit_file': 'file-edit',
      'delete_file': 'file-minus',
      'list_files': 'folder',
      'search': 'search',
      'execute': 'terminal',
      'bash': 'terminal',
      'shell': 'terminal',
      'git': 'git-branch',
      'browser': 'globe',
      'fetch': 'download',
      'mcp': 'plug',
    };
    const lowerName = toolName.toLowerCase();
    for (const [key, icon] of Object.entries(iconMap)) {
      if (lowerName.includes(key)) return icon;
    }
    return 'tool';
  }

  // 状态信息
  const statusInfo = $derived(() => {
    const map: Record<string, { class: string; text: string; icon: string }> = {
      pending: { class: 'pending', text: '等待中', icon: 'clock' },
      running: { class: 'running', text: '执行中', icon: 'loader' },
      success: { class: 'success', text: '成功', icon: 'check' },
      error: { class: 'error', text: '失败', icon: 'close' },
    };
    return map[status] || { class: 'success', text: '完成', icon: 'check' };
  });

  // 检查是否有内容
  const hasInput = $derived(!!input && !!formatContent(input));
  const hasOutput = $derived(!!output && !!formatContent(output));
  const hasError = $derived(!!error && !!error.trim());
  const hasContent = $derived(hasInput || hasOutput || hasError);

  const toolIcon = $derived(getToolIcon(name));

  function toggle() {
    collapsed = !collapsed;
  }

  async function copyOutput() {
    const content = formatContent(output);
    if (!content) return;
    try {
      await navigator.clipboard.writeText(content);
      copySuccess = true;
      setTimeout(() => { copySuccess = false; }, 2000);
    } catch (e) {
      console.error('复制失败:', e);
    }
  }

  function handleOpenFile() {
    if (filepath && onOpenFile) {
      onOpenFile(filepath);
    }
  }
</script>

{#if hasContent}
  <div
    class="tool-call"
    class:collapsed
    class:has-error={hasError}
    data-status={statusInfo().class}
  >
    <button class="tool-header" onclick={toggle}>
      <span class="chevron">
        <Icon name="chevron-right" size={12} />
      </span>

      <span class="tool-icon">
        <Icon name={toolIcon} size={14} />
      </span>

      <span class="tool-title">
        <span class="tool-name">{name || '工具调用'}</span>
        {#if filepath}
          <FileSpan {filepath} showIcon={false} clickable={!!onOpenFile} onClick={handleOpenFile} />
        {:else if id}
          <span class="tool-id">#{id}</span>
        {/if}
      </span>

      <span class="tool-status status-{statusInfo().class}">
        {#if status === 'running'}
          <span class="spinner"></span>
        {:else}
          <Icon name={statusInfo().icon} size={12} />
        {/if}
        {statusInfo().text}
      </span>
    </button>

    {#if !collapsed}
      <div class="tool-content">
        {#if hasInput}
          <div class="tool-section">
            <div class="section-header">
              <span class="section-label">输入</span>
            </div>
            <pre class="section-content">{formatContent(input)}</pre>
          </div>
        {/if}

        {#if hasOutput}
          <div class="tool-section">
            <div class="section-header">
              <span class="section-label">输出</span>
              <button class="copy-btn" onclick={copyOutput} title={copySuccess ? '已复制' : '复制输出'}>
                <Icon name={copySuccess ? 'check' : 'copy'} size={12} />
              </button>
            </div>
            <pre class="section-content">{formatContent(output)}</pre>
          </div>
        {/if}

        {#if hasError}
          <div class="tool-section error">
            <div class="section-header">
              <span class="section-label">错误</span>
            </div>
            <pre class="section-content error-content">{error}</pre>
          </div>
        {/if}

        {#if duration}
          <div class="tool-meta">
            <Icon name="clock" size={12} />
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
    margin: var(--space-2, 8px) 0;
    overflow: hidden;
    background: var(--surface-1, rgba(255,255,255,0.02));
  }

  .tool-call.has-error {
    border-color: var(--error);
  }

  .tool-header {
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

  .tool-header:hover {
    background: var(--surface-hover, rgba(255,255,255,0.05));
  }

  .chevron {
    display: flex;
    color: var(--foreground-muted);
    transition: transform var(--transition-fast);
  }

  .collapsed .chevron { transform: rotate(0deg); }
  .tool-call:not(.collapsed) .chevron { transform: rotate(90deg); }

  .tool-icon {
    display: flex;
    color: var(--info);
  }

  .tool-title {
    flex: 1;
    display: flex;
    align-items: center;
    gap: var(--space-2, 8px);
    min-width: 0;
    overflow: hidden;
  }

  .tool-name {
    font-weight: 500;
    font-size: var(--text-sm, 13px);
    white-space: nowrap;
  }

  .tool-id {
    font-size: var(--text-xs, 11px);
    color: var(--foreground-muted);
    opacity: 0.7;
  }

  .tool-status {
    display: flex;
    align-items: center;
    gap: 4px;
    font-size: var(--text-xs, 11px);
    padding: 2px 8px;
    border-radius: var(--radius-full);
    background: rgba(255,255,255,0.05);
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

  @keyframes spin { to { transform: rotate(360deg); } }

  .tool-content {
    padding: var(--space-3, 12px);
    border-top: 1px solid var(--border);
    background: var(--surface-2, rgba(0,0,0,0.1));
    animation: slideDown 0.2s ease-out;
    transform-origin: top;
  }

  @keyframes slideDown {
    from { opacity: 0; max-height: 0; transform: translateY(-8px); }
    to { opacity: 1; max-height: 500px; transform: translateY(0); }
  }

  .tool-section { margin-bottom: var(--space-3, 12px); }
  .tool-section:last-child { margin-bottom: 0; }

  .section-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: var(--space-1, 4px);
  }

  .section-label {
    font-size: var(--text-xs, 11px);
    color: var(--foreground-muted);
    text-transform: uppercase;
    letter-spacing: 0.5px;
  }

  .copy-btn {
    display: flex;
    align-items: center;
    padding: 2px 6px;
    background: transparent;
    border: none;
    color: var(--foreground-muted);
    cursor: pointer;
    border-radius: var(--radius-sm);
    transition: all var(--transition-fast);
  }

  .copy-btn:hover {
    background: var(--surface-hover);
    color: var(--foreground);
  }

  .section-content {
    font-family: var(--font-mono);
    font-size: var(--text-xs, 11px);
    background: var(--code-bg, rgba(0,0,0,0.2));
    padding: var(--space-2, 8px);
    border-radius: var(--radius-sm);
    overflow-x: auto;
    margin: 0;
    white-space: pre-wrap;
    word-break: break-word;
    max-height: 300px;
    overflow-y: auto;
  }

  .error-content {
    color: var(--error);
    background: rgba(239, 68, 68, 0.1);
  }

  .tool-meta {
    display: flex;
    align-items: center;
    gap: 4px;
    font-size: var(--text-xs, 11px);
    color: var(--foreground-muted);
    margin-top: var(--space-2, 8px);
    padding-top: var(--space-2, 8px);
    border-top: 1px dashed var(--border);
  }
</style>
