<script lang="ts">
  import { untrack } from 'svelte';
  import Icon from './Icon.svelte';
  import FileSpan from './FileSpan.svelte';
  import MermaidRenderer from './MermaidRenderer.svelte';
  import WaitResultCard from './WaitResultCard.svelte';
  import MarkdownContent from './MarkdownContent.svelte';
  import { vscode } from '../lib/vscode-bridge';
  import type { IconName } from '../lib/icons';
  import type { StandardizedToolResult } from '../types/message';

  interface ErrorDiagnosis {
    category: 'model_input' | 'context_stale' | 'permission' | 'role_constraint' | 'runtime';
    categoryLabel: string;
    ownerLabel: string;
    hint: string;
  }

  // Props
  interface Props {
    name: string;
    id?: string;
    input?: unknown;
    output?: unknown;
    error?: string;
    standardized?: StandardizedToolResult;
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
    standardized,
    status = 'success',
    duration,
    initialExpanded = false,
    filepath,
    onOpenFile
  }: Props = $props();

  // 折叠状态 - Mermaid / wait_for_workers 卡片默认展开，其他由 initialExpanded 控制
  let collapsed = $state(untrack(() => !initialExpanded && name !== 'mermaid_diagram' && name !== 'wait_for_workers'));
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
      'file_view': 'eye',
      'file_create': 'file-plus',
      'file_edit': 'pencil',
      'file_insert': 'plus',
      'grep_search': 'search',
      'file_remove': 'trash',
      'web_search': 'search',
      'web_fetch': 'globe',
      'mermaid_diagram': 'git-branch',
      'codebase_retrieval': 'search',
      'dispatch_task': 'tools',
      'send_worker_message': 'send',
      'wait_for_workers': 'hourglass',
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

  // 文件变更工具：diff 面板由 FileChangeCard 展示，ToolCall 仅渲染紧凑 header
  const isFileMutationTool = $derived(
    name === 'file_edit' || name === 'file_create' || name === 'file_insert' || name === 'file_remove'
  );

  // 目录/文件只读工具：只需紧凑 header
  const isCompactReadOnlyTool = $derived(name === 'file_view' || name === 'list_files');
  // 仅 view 类工具支持点击整行 header 打开文件
  const isHeaderOpenableTool = $derived(name === 'file_view' || name === 'view');

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

  // 检查是否为 wait_for_workers 工具输出
  const isWaitForWorkersTool = $derived(name === 'wait_for_workers');
  const waitResultData = $derived.by(() => {
    if (!isWaitForWorkersTool || !output) return null;
    try {
      const data = typeof output === 'string' ? JSON.parse(output) : output;
      if (data && Array.isArray(data.results) && typeof data.wait_status === 'string') {
        return data as {
          results: Array<{ task_id: string; worker: string; status: 'completed' | 'failed' | 'skipped' | 'cancelled'; summary: string; modified_files: string[]; errors?: string[] }>;
          wait_status: 'completed' | 'timeout';
          timed_out: boolean;
          pending_task_ids: string[];
          waited_ms: number;
          audit?: any;
        };
      }
    } catch {
      // 解析失败时回退到原始展示
    }
    return null;
  });

  // 获取工具显示名
  function getToolDisplayName(toolName: string): string {
    if (!toolName || typeof toolName !== 'string') return 'tool';

    // 文件操作工具直接使用语义化显示名
    const displayNameMap: Record<string, string> = {
      'launch-process': 'execute',
      'read-process': 'read process',
      'write-process': 'write process',
      'kill-process': 'kill process',
      'list-processes': 'list processes',
      'file_view': 'view',
      'file_create': 'create',
      'file_edit': 'edit',
      'file_insert': 'insert',
      'grep_search': 'search',
      'file_remove': 'remove',
      'web_search': 'web search',
      'web_fetch': 'web fetch',
      'mermaid_diagram': 'diagram',
      'codebase_retrieval': 'retrieval',
      'dispatch_task': 'dispatch',
      'send_worker_message': 'message',
      'wait_for_workers': 'wait results',
      'list_files': 'list files',
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
      case 'file_view':
      case 'file_create':
      case 'file_edit':
      case 'file_insert':
      case 'list_files': {
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
      case 'file_remove': {
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
  const toolDisplayName = $derived(getToolDisplayName(name));
  const toolSummary = $derived(getToolSummary(name, input));

  // 判断输出内容是否包含 markdown 格式（标题、表格、列表等）
  const outputText = $derived(formatContent(output));
  const isMarkdownOutput = $derived.by(() => {
    if (!outputText || outputText.length < 20) return false;
    // 检测常见 markdown 标记：标题、表格、列表、引用、加粗、分隔线、代码块
    return /^#{1,4}\s|^\|.+\|$|^\s*[-*]\s|^\s*\d+\.\s|^>\s|^---$|```|\*\*[^*]+\*\*/m.test(outputText);
  });

  function detectErrorDiagnosis(errorText?: string, toolResult?: StandardizedToolResult): ErrorDiagnosis | null {
    const rawMessage = `${toolResult?.message || ''}\n${errorText || ''}`.trim();
    if (!rawMessage) return null;

    const errorCode = (toolResult?.errorCode || '').toLowerCase();
    // 只取消息前 300 字符做关键词匹配，避免工具输出正文中的常见词（如 authorization、timeout）
    // 导致误分类。后端结构化错误前缀（如 "Tool blocked:", "Command rejected:"）都在开头。
    const messageHead = rawMessage.slice(0, 300).toLowerCase();
    /** 匹配 errorCode 或消息头部 */
    const matches = (...patterns: string[]): boolean =>
      patterns.some((pattern) => errorCode.includes(pattern) || messageHead.includes(pattern));
    /** 仅匹配 errorCode（不匹配消息内容，用于宽泛关键词如 authorization） */
    const codeMatches = (...patterns: string[]): boolean =>
      patterns.some((pattern) => errorCode.includes(pattern));

    if (matches('file_context_stale', '[file_context_stale]')) {
      return {
        category: 'context_stale',
        categoryLabel: '上下文过期',
        ownerLabel: '归因：流程上下文',
        hint: '先对目标文件执行一次 file_view，再基于最新内容重新生成 old_str 与行锚点。',
      };
    }

    if (matches(
      'tool_rejected',
      'command rejected',
      '参数解析失败',
      'path is required',
      'old_str_1 is required',
      'old_str and new_str are identical',
      'old_str appears multiple times',
      'old_str not found',
      'no match found close',
    )) {
      return {
        category: 'model_input',
        categoryLabel: '参数不匹配',
        ownerLabel: '归因：LLM 参数',
        hint: '工具参数或锚点与当前文件不匹配，需要重新读取文件并生成新的编辑参数。',
      };
    }

    // 编排者角色约束（dispatch_task 引导）— 与用户权限无关，是系统架构层面的职责划分
    if (matches('编排者', 'dispatch_task 委派', '深度模式下编排者')) {
      return {
        category: 'role_constraint',
        categoryLabel: '角色约束',
        ownerLabel: '归因：编排者职责边界',
        hint: '编排者不执行此操作，应通过 dispatch_task 委派给 Worker。',
      };
    }

    // 用户权限拦截（Ask 模式弹窗拒绝 / 权限开关关闭）
    // 仅匹配 errorCode，不对 message 做子串匹配 — 'authorization' 在代码中过于常见，易误判
    if (codeMatches('tool_blocked') || messageHead.includes('user denied tool authorization')) {
      return {
        category: 'permission',
        categoryLabel: '权限拦截',
        ownerLabel: '归因：权限策略',
        hint: '当前操作被授权策略拦截，需要授权放行后再执行。',
      };
    }

    return {
      category: 'runtime',
      categoryLabel: '执行异常',
      ownerLabel: '归因：工具执行链',
      hint: '工具进入执行阶段后失败，请检查执行器日志或外部环境（保存、格式化、进程改写）。',
    };
  }

  const errorDiagnosis = $derived.by(() => detectErrorDiagnosis(error, standardized));

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

  // 从工具参数中提取文件路径
  const toolFilepath = $derived.by(() => {
    if (filepath) return filepath;
    if (!input || typeof input !== 'object') return undefined;
    const args = input as Record<string, unknown>;

    const pathCandidates = [args.path, args.filepath, args.filePath];
    for (const candidate of pathCandidates) {
      if (typeof candidate === 'string' && candidate.trim().length > 0) {
        return candidate.trim();
      }
    }

    return undefined;
  });

  // 处理文件点击
  function handleOpenFile() {
    if (toolFilepath) {
      // 优先使用传入的回调
      if (onOpenFile) {
        onOpenFile(toolFilepath);
      } else {
        // 后备：直接发消息给 VSCode 桥
        vscode.postMessage({
          type: 'openFile',
          filepath: toolFilepath
        });
      }
    }
  }
</script>

{#snippet headerContent()}
  <span class="tool-icon">
    <Icon name={toolIcon} size={14} />
  </span>

  <span class="tool-title">
    <span class="tool-name">{toolDisplayName}</span>
    {#if status === 'error' && errorDiagnosis}
      <span class="error-tag error-{errorDiagnosis.category}" title={errorDiagnosis.ownerLabel}>
        {errorDiagnosis.categoryLabel}
      </span>
    {/if}
    {#if toolFilepath}
      <FileSpan filepath={toolFilepath} showIcon={false} clickable={true} onClick={handleOpenFile} />
    {:else if toolSummary}
      <span class="tool-summary" title={toolSummary}>{toolSummary}</span>
    {/if}
  </span>

  <span class="tool-status status-{statusInfo.class}">
    {#if status === 'running' || status === 'pending'}
      <span class="status-dot pulsing"></span>
    {:else}
      <span class="status-dot"></span>
    {/if}
  </span>
{/snippet}

{#if isFileMutationTool && status === 'success'}
  <!-- 文件变更工具完成：由 FileChangeCard 全权展示 -->
{:else}
  {@const isCompactMutation = isFileMutationTool && (status === 'running' || status === 'pending')}
  {@const isExpandable = hasContent && !isCompactReadOnlyTool && !isCompactMutation}
  {#if isExpandable || isCompactReadOnlyTool || isCompactMutation}
    <div
      class="tool-call"
      class:collapsed={isExpandable && collapsed}
      class:has-error={isExpandable && hasError}
      class:file-mutation={isCompactMutation || isCompactReadOnlyTool}
      data-status={statusInfo.class}
    >
      {#if isExpandable}
        <button class="tool-header" onclick={toggle}>
          <span class="chevron">
            <Icon name="chevron-right" size={12} />
          </span>
          {@render headerContent()}
        </button>
      {:else}
        <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
        <div
          class="tool-header"
          class:file-mutation-header={isCompactMutation || isCompactReadOnlyTool}
          class:clickable={isHeaderOpenableTool && !!toolFilepath}
          onclick={isHeaderOpenableTool && toolFilepath ? handleOpenFile : undefined}
          onkeydown={(e) => {
            if (isHeaderOpenableTool && toolFilepath && (e.key === 'Enter' || e.key === ' ')) {
              e.preventDefault();
              handleOpenFile();
            }
          }}
          role={isHeaderOpenableTool && toolFilepath ? "button" : undefined}
          tabindex={isHeaderOpenableTool && toolFilepath ? 0 : undefined}
        >
          {@render headerContent()}
        </div>
      {/if}

      {#if isExpandable && !collapsed}
        <div class="tool-content">
          {#if hasInput && !isMermaidTool && !isWaitForWorkersTool}
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
              {:else if isWaitForWorkersTool && waitResultData}
                <WaitResultCard data={waitResultData} />
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
                {#if errorDiagnosis}
                  <span class="diagnosis-owner">{errorDiagnosis.ownerLabel}</span>
                {/if}
              </div>
              <pre class="section-content error-content">{error}</pre>
              {#if errorDiagnosis}
                <div class="error-hint">{errorDiagnosis.hint}</div>
              {/if}
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

  /* 文件变更工具：紧凑 header-only 卡片，不可展开 */
  .tool-call.file-mutation {
    border: none;
    background: transparent;
    margin: var(--space-1, 4px) 0;
  }

  .file-mutation-header {
    cursor: default;
    padding: var(--space-1, 4px) 0;
    opacity: 0.85;
  }

  .file-mutation-header:hover {
    background: transparent;
    opacity: 1;
  }

  .file-mutation-header.clickable {
    cursor: pointer;
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
    flex-shrink: 0;
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

  .error-tag {
    display: inline-flex;
    align-items: center;
    padding: 1px 6px;
    border-radius: 999px;
    border: 1px solid transparent;
    font-size: 10px;
    line-height: 1.4;
    font-weight: 500;
    white-space: nowrap;
    flex-shrink: 0;
  }

  .error-model_input {
    color: var(--warning);
    border-color: rgba(245, 158, 11, 0.45);
    background: rgba(245, 158, 11, 0.12);
  }

  .error-context_stale {
    color: var(--info);
    border-color: rgba(59, 130, 246, 0.45);
    background: rgba(59, 130, 246, 0.12);
  }

  .error-permission {
    color: var(--warning);
    border-color: rgba(234, 179, 8, 0.45);
    background: rgba(234, 179, 8, 0.12);
  }

  .error-role_constraint {
    color: var(--info);
    border-color: rgba(139, 92, 246, 0.45);
    background: rgba(139, 92, 246, 0.12);
  }

  .error-runtime {
    color: var(--error);
    border-color: rgba(239, 68, 68, 0.45);
    background: rgba(239, 68, 68, 0.12);
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

  .diagnosis-owner {
    font-size: var(--text-xs, 11px);
    color: var(--foreground-muted);
  }

  .error-hint {
    margin-top: var(--space-2, 8px);
    padding: var(--space-2, 8px);
    border-radius: var(--radius-sm);
    border: 1px dashed var(--border);
    color: var(--foreground-muted);
    font-size: var(--text-xs, 11px);
    line-height: 1.5;
    background: var(--surface-1, rgba(255,255,255,0.02));
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
