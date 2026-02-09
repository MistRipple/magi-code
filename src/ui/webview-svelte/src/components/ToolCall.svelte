<script lang="ts">
  import { untrack } from 'svelte';
  import Icon from './Icon.svelte';
  import FileSpan from './FileSpan.svelte';
  import MermaidRenderer from './MermaidRenderer.svelte';
  import MarkdownContent from './MarkdownContent.svelte';
  import { vscode } from '../lib/vscode-bridge';
  import type { IconName } from '../lib/icons';

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

  // 折叠状态 - Mermaid 卡片默认展开，其他由 initialExpanded 控制
  let collapsed = $state(untrack(() => !initialExpanded && name !== 'mermaid_diagram'));
  let copySuccess = $state(false);

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

  // 获取工具图标（基于当前项目实际工具名）
  function getToolIcon(toolName: string): IconName {
    if (!toolName || typeof toolName !== 'string') {
      vscode.postMessage({
        type: 'uiError',
        component: 'ToolCall',
        detail: { toolName, id, status },
        stack: new Error('ToolCall: invalid toolName').stack,
      });
      throw new Error('ToolCall: invalid toolName');
    }

    const iconMap: Record<string, IconName> = {
      // ToolManager 内置工具
      'launch-process': 'terminal',
      'read-process': 'terminal',
      'write-process': 'terminal',
      'kill-process': 'terminal',
      'list-processes': 'terminal',
      'text_editor': 'file-edit',
      'grep_search': 'search',
      'remove_files': 'file-minus',
      'web_search': 'search',
      'web_fetch': 'globe',
      'mermaid_diagram': 'git-branch',
      'codebase_retrieval': 'search',
      'dispatch_task': 'tools',
      'plan_mission': 'list',
      'send_worker_message': 'send',
      'report_progress': 'clock',
    };

    if (iconMap[toolName]) {
      return iconMap[toolName];
    }

    const lowerName = toolName.toLowerCase();
    if (lowerName.includes('search') || lowerName.includes('retrieval')) return 'search';
    if (lowerName.includes('read') || lowerName.includes('view')) return 'file-text';
    if (lowerName.includes('write') || lowerName.includes('edit')) return 'file-edit';
    if (lowerName.includes('delete') || lowerName.includes('remove')) return 'file-minus';
    if (lowerName.includes('web') || lowerName.includes('fetch') || lowerName.includes('browser')) return 'globe';
    if (lowerName.includes('mermaid')) return 'git-branch';
    if (lowerName.includes('mcp')) return 'plug';
    return 'tool';
  }

  // 状态信息
  const statusInfo = $derived.by(() => {
    const map: Record<string, { class: string }> = {
      pending: { class: 'pending' },
      running: { class: 'running' },
      success: { class: 'success' },
      error: { class: 'error' },
    };
    return map[status] || { class: 'success' };
  });

  // 检查是否有内容
  const hasInput = $derived(!!input && !!formatContent(input));
  const hasOutput = $derived(!!output && !!formatContent(output));
  const hasError = $derived(!!error && !!error.trim());
  const hasContent = $derived(hasInput || hasOutput || hasError);

  // 检查是否为 Mermaid 工具输出
  const isMermaidTool = $derived(name === 'mermaid_diagram');
  const mermaidData = $derived.by(() => {
    if (!isMermaidTool || !output) return null;
    try {
      const data = typeof output === 'string' ? JSON.parse(output) : output;
      if (data && (data.type === 'mermaid' || data.type === 'mermaid_diagram') && data.code) {
        return {
          code: data.code as string,
          title: (data.title || '') as string,
          diagramType: (data.diagramType || '') as string,
        };
      }
    } catch {
      // 忽略解析错误
    }
    return null;
  });

  // 获取工具显示名
  function getToolDisplayName(toolName: string, toolInput?: unknown): string {
    if (!toolName || typeof toolName !== 'string') return 'tool';

    // text_editor 根据子命令动态显示
    if (toolName === 'text_editor' && toolInput && typeof toolInput === 'object') {
      const cmd = (toolInput as Record<string, unknown>).command;
      if (typeof cmd === 'string') return cmd;
    }

    const displayNameMap: Record<string, string> = {
      'launch-process': 'execute',
      'read-process': 'read process',
      'write-process': 'write process',
      'kill-process': 'kill process',
      'list-processes': 'list processes',
      'text_editor': 'edit',
      'grep_search': 'search',
      'remove_files': 'remove',
      'web_search': 'web search',
      'web_fetch': 'web fetch',
      'mermaid_diagram': 'diagram',
      'codebase_retrieval': 'retrieval',
      'dispatch_task': 'dispatch',
      'plan_mission': 'plan',
      'send_worker_message': 'message',
      'report_progress': 'progress',
    };

    return displayNameMap[toolName] ?? toolName;
  }

  // 从工具参数中提取语义摘要
  function getToolSummary(toolName: string, toolInput: unknown): string {
    if (!toolInput || typeof toolInput !== 'object') return '';
    const args = toolInput as Record<string, unknown>;
    switch (toolName) {
      case 'execute_shell':
        return typeof args.command === 'string' ? args.command : '';
      case 'text_editor': {
        const p = typeof args.path === 'string' ? args.path : '';
        return p;
      }
      case 'grep_search':
        return typeof args.pattern === 'string' ? args.pattern : '';
      case 'read_file':
      case 'write_file':
      case 'edit_file':
      case 'delete_file':
        return typeof args.path === 'string' ? args.path : '';
      case 'remove_files': {
        const paths = args.paths;
        if (Array.isArray(paths) && paths.length > 0) {
          return paths.length === 1 ? String(paths[0]) : `${paths[0]} 等 ${paths.length} 个文件`;
        }
        return typeof args.path === 'string' ? args.path : '';
      }
      case 'web_fetch':
        return typeof args.url === 'string' ? args.url : '';
      case 'web_search':
        return typeof args.query === 'string' ? args.query : '';
      case 'mermaid_diagram':
        return typeof args.title === 'string' ? args.title : '';
      case 'lsp_query': {
        const action = typeof args.action === 'string' ? args.action : '';
        const fp = typeof args.filePath === 'string' ? args.filePath : '';
        return action && fp ? `${action} ${fp}` : action || fp;
      }
      case 'list_files':
        return typeof args.path === 'string' ? args.path : '';
      default:
        // MCP 或其他未知工具：尝试提取常见字段
        return (typeof args.command === 'string' ? args.command : '')
          || (typeof args.path === 'string' ? args.path : '')
          || (typeof args.query === 'string' ? args.query : '')
          || (typeof args.url === 'string' ? args.url : '');
    }
  }

  const toolIcon = $derived(getToolIcon(name));
  const toolDisplayName = $derived(getToolDisplayName(name, input));
  const toolSummary = $derived(getToolSummary(name, input));

  // 判断输出内容是否包含 markdown 格式（标题、表格、列表等）
  const outputText = $derived(formatContent(output));
  const isMarkdownOutput = $derived.by(() => {
    if (!outputText || outputText.length < 20) return false;
    // 检测常见 markdown 标记：标题、表格、列表、引用、加粗、分隔线、代码块
    return /^#{1,4}\s|^\|.+\|$|^\s*[-*]\s|^\s*\d+\.\s|^>\s|^---$|```|\*\*[^*]+\*\*/m.test(outputText);
  });

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
    data-status={statusInfo.class}
  >
    <button class="tool-header" onclick={toggle}>
      <span class="chevron">
        <Icon name="chevron-right" size={12} />
      </span>

      <span class="tool-icon">
        <Icon name={toolIcon} size={14} />
      </span>

      <span class="tool-title">
        <span class="tool-name">{toolDisplayName}</span>
        {#if filepath}
          <FileSpan {filepath} showIcon={false} clickable={!!onOpenFile} onClick={handleOpenFile} />
        {:else if toolSummary}
          <span class="tool-summary" title={toolSummary}>{toolSummary}</span>
        {/if}
      </span>

      <span class="tool-status status-{statusInfo.class}">
        {#if status === 'running'}
          <span class="status-dot pulsing"></span>
        {:else}
          <span class="status-dot"></span>
        {/if}
      </span>
    </button>

    {#if !collapsed}
      <div class="tool-content">
        {#if hasInput && !isMermaidTool}
          <div class="tool-section">
            <div class="section-header">
              <span class="section-label">输入</span>
            </div>
            <pre class="section-content">{formatContent(input)}</pre>
          </div>
        {/if}

        {#if hasOutput}
          <div class="tool-section">
            {#if isMermaidTool && mermaidData}
              <MermaidRenderer
                code={mermaidData?.code || ''}
                title={mermaidData?.title}
                diagramType={mermaidData?.diagramType}
              />
            {:else}
              <div class="section-header">
                <span class="section-label">输出</span>
                <button class="copy-btn" onclick={copyOutput} title={copySuccess ? '已复制' : '复制输出'}>
                  <Icon name={copySuccess ? 'check' : 'copy'} size={12} />
                </button>
              </div>
              {#if isMarkdownOutput}
                <div class="markdown-output">
                  <MarkdownContent content={outputText} />
                </div>
              {:else}
                <pre class="section-content">{formatContent(output)}</pre>
              {/if}
            {/if}
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

  .tool-summary {
    font-size: var(--text-xs, 11px);
    color: var(--foreground-muted);
    opacity: 0.8;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    min-width: 0;
    flex: 1;
  }

  .tool-status {
    display: flex;
    align-items: center;
    flex-shrink: 0;
  }

  .status-dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: currentColor;
  }

  .status-dot.pulsing {
    animation: pulse 1.5s ease-in-out infinite;
  }

  .status-pending { color: var(--warning); }
  .status-running { color: var(--info); }
  .status-success { color: var(--success); }
  .status-error { color: var(--error); }

  @keyframes pulse {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.3; }
  }

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

  .markdown-output {
    font-size: var(--text-sm, 13px);
    background: var(--code-bg, rgba(0,0,0,0.2));
    padding: var(--space-2, 8px) var(--space-3, 12px);
    border-radius: var(--radius-sm);
    max-height: 400px;
    overflow-y: auto;
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
