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
  }

  let {
    code,
    language = '',
    filepath = '',
    showLineNumbers = false,
    showCopyButton = true
  }: Props = $props();

  // 检测是否是 Mermaid 代码
  const isMermaid = $derived(language.toLowerCase() === 'mermaid');

  // 调试日志
  $effect(() => {
    if (isMermaid) {
      console.log('[CodeBlock] Mermaid detected, language:', language, 'code length:', code?.length);
    }
  });

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

  const trimmedCode = $derived(code.trim());
  const lines = $derived(trimmedCode.split('\n'));

  // 代码高亮
  $effect(() => {
    if (codeRef && trimmedCode && !collapsed) {
      try {
        hljs.highlightElement(codeRef);
      } catch (e) {
        console.warn('[CodeBlock] 高亮失败:', e);
      }
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
  {#if isMermaid}
    <!-- Mermaid 图表渲染 -->
    <MermaidRenderer code={trimmedCode} />
  {:else}
    <!-- 普通代码块 -->
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
  }

  .code-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    background: rgba(0, 0, 0, 0.2);
  }

  .header-left {
    display: flex;
    align-items: center;
    gap: var(--spacing-sm);
    padding: var(--spacing-xs) var(--spacing-md);
    flex: 1;
    text-align: left;
    cursor: pointer;
  }

  .header-left:hover {
    background: rgba(255, 255, 255, 0.05);
  }

  .chevron {
    display: flex;
    transition: transform var(--transition-fast);
    color: var(--vscode-descriptionForeground, #888);
  }

  .collapsed .chevron {
    transform: rotate(0deg);
  }

  .code-block:not(.collapsed) .chevron {
    transform: rotate(90deg);
  }

  .code-icon {
    display: flex;
    color: var(--vscode-descriptionForeground, #888);
  }

  .code-title {
    display: flex;
    align-items: center;
    gap: var(--spacing-sm);
    font-size: var(--font-size-sm);
  }

  .lang-name {
    font-weight: 500;
  }

  .filepath {
    color: var(--vscode-descriptionForeground, #888);
    font-size: 11px;
    max-width: 200px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .copy-btn {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: var(--spacing-xs) var(--spacing-sm);
    margin-right: var(--spacing-sm);
    font-size: 11px;
    color: var(--vscode-descriptionForeground, #888);
    border-radius: var(--radius-sm);
    transition: all var(--transition-fast);
  }

  .copy-btn:hover {
    background: rgba(255, 255, 255, 0.1);
    color: var(--foreground);
  }

  .copy-btn.copied {
    color: var(--success);
  }

  .code-content {
    display: flex;
    overflow-x: auto;
  }

  .line-numbers {
    display: flex;
    flex-direction: column;
    padding: var(--spacing-sm);
    padding-right: var(--spacing-sm);
    border-right: 1px solid var(--border);
    text-align: right;
    user-select: none;
  }

  .line-num {
    font-family: var(--font-mono);
    font-size: var(--font-size-sm);
    line-height: 1.5;
    color: var(--vscode-editorLineNumber-foreground, #858585);
  }

  .code-pre {
    flex: 1;
    margin: 0;
    padding: var(--spacing-sm) var(--spacing-md);
    overflow-x: auto;
  }

  .code-text {
    font-family: var(--font-mono);
    font-size: var(--font-size-sm);
    line-height: 1.5;
  }
</style>

