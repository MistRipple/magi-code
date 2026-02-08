<script lang="ts">
  import hljs from 'highlight.js';
  import MermaidRenderer from './MermaidRenderer.svelte';

  // Props
  interface Props {
    code: string;
    language?: string;
    filepath?: string;
    showLineNumbers?: boolean;
    showCopyButton?: boolean;
    isStreaming?: boolean;
  }

  let {
    code,
    language = '',
    filepath = '',
    showLineNumbers = false,
    showCopyButton = true,
    isStreaming = false
  }: Props = $props();

  // 检测是否是 Mermaid 代码
  const isMermaid = $derived(language.toLowerCase() === 'mermaid');

  // 状态
  let collapsed = $state(false);
  let copied = $state(false);
  // 移除直接 DOM 引用，改用数据驱动
  // let codeRef: HTMLElement | null = $state(null);

  // 语言名称映射
  const LANG_NAMES: Record<string, string> = {
    js: 'JavaScript', javascript: 'JavaScript',
    ts: 'TypeScript', typescript: 'TypeScript',
    py: 'Python', python: 'Python',
    sh: 'Shell', bash: 'Bash',
    json: 'JSON', yaml: 'YAML', yml: 'YAML',
    html: 'HTML', css: 'CSS', scss: 'SCSS',
    md: 'Markdown', markdown: 'Markdown',
  };

  const langName = $derived(
    language ? (LANG_NAMES[language.toLowerCase()] || language.toUpperCase()) : 'Code'
  );

  // 🔧 优化：保留代码缩进，仅移除末尾空格和开头的首个换行符
  const trimmedCode = $derived(code.trimEnd().replace(/^\n/, ''));
  const lines = $derived(trimmedCode.split('\n'));

  // 🔧 计算高亮 HTML
  // 策略：
  // 1. 流式传输期间 (isStreaming=true): 为避免 JSON 等格式不完整导致解析错误或不显示，且为了性能，直接显示转义后的纯文本。
  // 2. 传输完成 (isStreaming=false): 执行完整的高亮逻辑。
  let highlightedHtml = $state('');

  // 简易转义函数（如果 markdown-utils 中没有导出，可以在这里定义）
  function safeEscape(str: string) {
    return str
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;")
      .replace(/'/g, "&#039;");
  }

  $effect(() => {
    // 只有非折叠状态才处理内容
    if (collapsed) return;

    if (isStreaming) {
      // 流式期间：只转义，不高亮。保证内容绝对可见且流畅。
      highlightedHtml = safeEscape(trimmedCode);
    } else {
      // 非流式：尝试高亮
      // 超长代码块跳过高亮，避免主线程阻塞
      if (trimmedCode.length > 50000) {
        highlightedHtml = safeEscape(trimmedCode);
        return;
      }
      // 使用 setTimeout 宏任务，避免阻塞主线程（虽然 hljs 是同步的）
      const timer = setTimeout(() => {
        try {
          if (language && hljs.getLanguage(language)) {
            highlightedHtml = hljs.highlight(trimmedCode, { language }).value;
          } else {
            highlightedHtml = safeEscape(trimmedCode);
          }
        } catch (e) {
          console.warn('[CodeBlock] 高亮失败:', e);
          highlightedHtml = safeEscape(trimmedCode);
        }
      }, 0);
      return () => clearTimeout(timer);
    }
  });

  function toggle() {
    collapsed = !collapsed;
  }

  async function copyCode() {
    try {
      await navigator.clipboard.writeText(trimmedCode);
      copied = true;
      setTimeout(() => { copied = false; }, 2000);
    } catch (e) {
      console.error('复制失败:', e);
    }
  }
</script>

<div class="code-block" class:collapsed>
  {#if isMermaid && !isStreaming}
    <!-- Mermaid 图表渲染 (非流式状态下) -->
    <MermaidRenderer code={trimmedCode} />
  {:else}
    <!-- 普通代码块 (或流式中的 Mermaid) -->
    <div class="code-header">
      <button class="header-left" onclick={toggle}>
        <span class="chevron">
          <svg width="12" height="12" viewBox="0 0 16 16" fill="currentColor">
            <path d="M4.646 1.646a.5.5 0 0 1 .708 0l6 6a.5.5 0 0 1 0 .708l-6 6a.5.5 0 0 1-.708-.708L10.293 8 4.646 2.354a.5.5 0 0 1 0-.708z"/>
          </svg>
        </span>

        <span class="code-icon">
          <svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor">
            <path d="M5.854 4.854a.5.5 0 1 0-.708-.708l-3.5 3.5a.5.5 0 0 0 0 .708l3.5 3.5a.5.5 0 0 0 .708-.708L2.707 8l3.147-3.146zm4.292 0a.5.5 0 0 1 .708-.708l3.5 3.5a.5.5 0 0 1 0 .708l-3.5 3.5a.5.5 0 0 1-.708-.708L13.293 8l-3.147-3.146z"/>
          </svg>
        </span>

        <span class="code-title">
          <span class="lang-name">{langName}</span>
          {#if isMermaid && isStreaming}
            <span class="streaming-badge">生成中...</span>
          {/if}
          {#if filepath}
            <span class="filepath" title={filepath}>{filepath}</span>
          {/if}
        </span>
      </button>

      {#if showCopyButton}
        <button class="copy-btn" onclick={copyCode} class:copied>
          {#if copied}
            <svg width="12" height="12" viewBox="0 0 16 16" fill="currentColor">
              <path d="M12.736 3.97a.733.733 0 0 1 1.047 0c.286.289.29.756.01 1.05L7.88 12.01a.733.733 0 0 1-1.065.02L3.217 8.384a.757.757 0 0 1 0-1.06.733.733 0 0 1 1.047 0l3.052 3.093 5.4-6.425z"/>
            </svg>
            <span>已复制</span>
          {:else}
            <svg width="12" height="12" viewBox="0 0 16 16" fill="currentColor">
              <path d="M4 2a2 2 0 0 1 2-2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V2zm2-1a1 1 0 0 0-1 1v8a1 1 0 0 0 1 1h8a1 1 0 0 0 1-1V2a1 1 0 0 0-1-1H6zM2 5a1 1 0 0 0-1 1v8a1 1 0 0 0 1 1h8a1 1 0 0 0 1-1v-1h1v1a2 2 0 0 1-2 2H2a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h1v1H2z"/>
            </svg>
            <span>复制</span>
          {/if}
        </button>
      {/if}
    </div>

    {#if !collapsed}
      <div class="code-content">
        {#if showLineNumbers}
          <div class="line-numbers">
            {#each lines as _, i}
              <span class="line-num">{i + 1}</span>
            {/each}
          </div>
        {/if}
        <!-- 🔧 改用 {@html} 渲染，彻底避免 DOM 操作冲突 -->
        <pre class="code-pre"><code
          class="code-text {language ? `language-${language}` : ''}"
        >{@html highlightedHtml}</code></pre>
      </div>
    {/if}
  {/if}
</div>

<style>
  .code-block {
    border: 1px solid var(--code-border);
    border-radius: var(--radius-md);
    margin: var(--spacing-sm) 0;
    overflow: hidden;
    background: var(--code-bg);
    
    /* 🔧 头部高度 */
    --header-height: 36px;
  }

  .code-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    height: var(--header-height);
    padding: 0 var(--spacing-sm);
    background: var(--code-header-bg, rgba(255, 255, 255, 0.03));
    border-bottom: 1px solid var(--code-border);
    user-select: none;
  }

  .header-left {
    display: flex;
    align-items: center;
    gap: var(--spacing-sm);
    flex: 1;
    height: 100%;
    background: transparent;
    border: none;
    padding: 0;
    color: var(--foreground);
    font-family: inherit;
    cursor: pointer;
    overflow: hidden;
    outline: none;
  }

  .header-left:hover .chevron {
    color: var(--foreground);
  }

  /* 折叠图标 */
  .chevron {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 16px;
    height: 16px;
    transition: transform var(--transition-fast);
    color: var(--foreground-muted, #888);
  }

  .code-block.collapsed .chevron {
    transform: rotate(0deg);
  }

  .code-block:not(.collapsed) .chevron {
    transform: rotate(90deg);
  }

  .code-icon {
    display: flex;
    align-items: center;
    color: var(--foreground-muted);
  }

  .code-title {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: var(--font-size-xs);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .lang-name {
    font-weight: 600;
    color: var(--accent-color, #646cff);
    text-transform: uppercase;
  }

  .filepath {
    opacity: 0.6;
    font-family: var(--font-mono);
  }

  .streaming-badge {
    font-size: 10px;
    background: var(--accent-color);
    color: white;
    padding: 0 4px;
    border-radius: 4px;
    animation: pulse 1.5s infinite;
  }

  .copy-btn {
    display: flex;
    align-items: center;
    gap: 6px;
    background: transparent;
    border: 1px solid transparent;
    border-radius: 4px;
    padding: 4px 8px;
    color: var(--foreground-muted);
    font-size: var(--font-size-xs);
    cursor: pointer;
    transition: all 0.2s;
  }

  .copy-btn:hover {
    background: rgba(255, 255, 255, 0.1);
    color: var(--foreground);
  }

  .copy-btn.copied {
    color: var(--success-color, #4caf50);
    border-color: var(--success-color);
    background: rgba(76, 175, 80, 0.1);
  }

  /* 
   * 🔧 核心修复：内容区域布局
   * 1. 在父容器统一字号、行高、字体。
   * 2. 子元素 (行号、代码) 全部 inherit，确保严格对齐。
   */
  .code-content {
    display: flex;
    flex-direction: row;
    align-items: flex-start;
    background: var(--code-bg);
    position: relative;
    
    /* 统一定义排版属性 */
    font-family: var(--font-mono);
    font-size: var(--font-size-sm, 12px); /* 使用明确的小字号 */
    line-height: 1.5;
  }

  .line-numbers {
    display: flex;
    flex-direction: column;
    padding: var(--spacing-sm) 0;
    width: 40px;
    min-width: 40px;
    border-right: 1px solid var(--border);
    text-align: right;
    user-select: none;
    background: rgba(0, 0, 0, 0.02);
    flex-shrink: 0;
    
    /* 强制继承父级排版 */
    font-family: inherit;
    font-size: inherit;
    line-height: inherit;
  }

  .line-num {
    display: block;
    padding-right: 8px;
    color: var(--foreground-muted);
    opacity: 0.5;
    white-space: nowrap;
    font-variant-numeric: tabular-nums;
  }

  .code-pre {
    flex: 1;
    margin: 0 !important;
    padding: var(--spacing-sm) var(--spacing-md) !important;
    overflow-x: auto;
    background: transparent !important;
    min-width: 0;
    border: none !important;
  }

  .code-text {
    display: block;
    margin: 0 !important;
    padding: 0 !important;
    background: transparent !important;
    white-space: pre;
    color: var(--code-fg, inherit);
    border: none !important;
    
    /* 🔧 关键：强制继承，覆盖全局 code { font-size: 0.9em } */
    font-family: inherit !important;
    font-size: inherit !important;
    line-height: inherit !important;
  }

  @keyframes pulse {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.6; }
  }
</style>
