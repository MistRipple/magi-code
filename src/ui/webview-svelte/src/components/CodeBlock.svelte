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
  let codeRef: HTMLElement | null = $state(null);

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

  // 代码高亮 (带防抖优化)
  $effect(() => {
    if (!codeRef || !trimmedCode || collapsed) return;

    // 🔧 防抖：避免在流式输出时每字符触发高亮导致 UI 卡顿
    const timer = setTimeout(() => {
      try {
        if (codeRef) {
          // 重置 class 以便重新高亮 (如果语言变化)
          codeRef.className = `code-text ${language ? `language-${language}` : ''}`;
          // 移除可能存在的 hljs 属性
          codeRef.removeAttribute('data-highlighted');
          hljs.highlightElement(codeRef);
        }
      } catch (e) {
        console.warn('[CodeBlock] 高亮失败:', e);
      }
    }, 80); // 80ms 延迟，平衡性能与视觉响应

    return () => clearTimeout(timer);
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
        <pre class="code-pre"><code
          bind:this={codeRef}
          class="code-text {language ? `language-${language}` : ''}"
        >{trimmedCode}</code></pre>
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
    /* 🔧 定义局部变量确保行号和代码严格对齐 */
    --code-line-height: 1.5;
    --code-font-size: var(--font-size-sm, 12px);
  }

  .code-content {
    display: flex;
    flex-direction: row;
    align-items: flex-start;
  }

  .line-numbers {
    display: flex;
    flex-direction: column;
    padding: var(--spacing-sm);
    padding-right: var(--spacing-sm);
    border-right: 1px solid var(--border);
    text-align: right;
    user-select: none;
    background: rgba(0, 0, 0, 0.1); /* 微弱底色区分行号区域 */
  }

  .line-num {
    font-family: var(--font-mono);
    font-size: var(--code-font-size) !important;
    line-height: var(--code-line-height) !important;
    /* 🔧 强制高度：使用 calc 确保像素级对齐 */
    height: calc(var(--code-font-size) * var(--code-line-height));
    color: var(--vscode-editorLineNumber-foreground, #858585);
    white-space: nowrap;
    display: flex; /* 确保垂直居中 */
    align-items: center;
    justify-content: flex-end;
  }

  .code-pre {
    flex: 1;
    /* 🔧 增强防御性：防止 MarkdownContent 的 global(pre) 干扰对齐 */
    margin: 0 !important;
    padding: var(--spacing-sm) var(--spacing-md) !important;
    overflow-x: auto;
    background: transparent !important;
  }

  .code-text {
    font-family: var(--font-mono);
    font-size: var(--code-font-size) !important;
    line-height: var(--code-line-height) !important;
    /* 🔧 修复行号对齐问题：重置可能由 hljs 引入的内边距和外边距 */
    padding: 0 !important;
    margin: 0 !important;
    background: transparent !important;
    display: block;
    white-space: pre; /* 确保不换行，与行号一一对应 */
  }
</style>

