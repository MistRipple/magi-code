<script lang="ts">
  import FileName from './FileName.svelte';
  import ToolCardMeta from './ToolCardMeta.svelte';
  import Icon from './Icon.svelte';
  import { getContext, onDestroy } from 'svelte';
  import Disclosure from './Disclosure.svelte';
  import DiagramRenderer from './DiagramRenderer.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { highlightCode } from '../lib/code-highlighter';
  import { parseCodeBlockDiagramPayload } from '../lib/diagram-payload';
  import {
    dispatchFilePreviewEvent,
    FILE_PREVIEW_SCOPE_CONTEXT,
    type FilePreviewScopeReader,
    normalizeFileReferenceTarget,
  } from '../lib/file-reference';
  import { vscode } from '../lib/vscode-bridge';
  import { desktopContextMenu } from '../lib/desktop-context-menu-contract';

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
    isStreaming = false,
  }: Props = $props();

  const contentId = $props.id();

  // 状态
  let collapsed = $state(false);
  let copied = $state(false);
  let copyRevision = $state(0);
  let copyResetTimer: ReturnType<typeof setTimeout> | undefined;
  onDestroy(() => clearTimeout(copyResetTimer));

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
  const diagramPayload = $derived(parseCodeBlockDiagramPayload(language, trimmedCode));
  const readFilePreviewScope = getContext<FilePreviewScopeReader | undefined>(FILE_PREVIEW_SCOPE_CONTEXT);

  function currentFilePreviewScope() {
    return readFilePreviewScope?.() ?? {};
  }

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

    const codeToHighlight = trimmedCode;
    highlightedHtml = safeEscape(codeToHighlight);

    if (isStreaming || diagramPayload) {
      // 流式期间：只转义，不高亮。保证内容绝对可见且流畅。
      return;
    }

    // 超长代码块跳过高亮，避免主线程阻塞
    if (codeToHighlight.length > 50000 || !language.trim()) return;

    let cancelled = false;
    const timer = setTimeout(() => {
      void highlightCode(codeToHighlight, language).then((result) => {
        if (!cancelled && result !== null) highlightedHtml = result;
      }).catch((error) => {
        console.warn('[CodeBlock] 高亮失败:', error);
      });
    }, 0);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  });

  function toggle() {
    collapsed = !collapsed;
  }

  async function copyCode() {
    try {
      await navigator.clipboard.writeText(trimmedCode);
      copied = true;
      copyRevision += 1;
      clearTimeout(copyResetTimer);
      copyResetTimer = setTimeout(() => { copied = false; }, 2500);
    } catch (e) {
      console.error('复制失败:', e);
    }
  }

  function previewFilepathTarget() {
    const target = filepath.trim();
    if (!target) return;
    const normalizedTarget = normalizeFileReferenceTarget(target) ?? target;
    const scope = currentFilePreviewScope();
    if (dispatchFilePreviewEvent({ filepath: normalizedTarget, ...scope })) {
      return;
    }
    vscode.postMessage({ type: 'openFile', filepath: normalizedTarget, ...scope });
  }

  function previewFilepathClick(event: MouseEvent) {
    event.preventDefault();
    event.stopPropagation();
    previewFilepathTarget();
  }

  function previewFilepathKeydown(event: KeyboardEvent) {
    if (event.key !== 'Enter' && event.key !== ' ') {
      return;
    }
    event.preventDefault();
    event.stopPropagation();
    previewFilepathTarget();
  }
</script>

{#if diagramPayload && !isStreaming}
  <DiagramRenderer payload={diagramPayload} />
{:else}
  <div class="message-card code-block" class:collapsed>
    <!-- 普通代码块（或流式中的图表代码） -->
    <div class="code-header">
      <button class="header-left" onclick={toggle} aria-expanded={!collapsed} aria-controls={contentId}>

        <span class="code-icon">
          <Icon name="code" size={14} />
        </span>

        <span class="code-title">
          <span class="lang-name">{langName}</span>
          {#if diagramPayload && isStreaming}
            <span class="streaming-badge">{i18n.t('codeBlock.streaming')}</span>
          {/if}
          {#if filepath}
            <span
              class="filepath filepath-clickable"
              title={filepath}
              role="button"
              tabindex="0"
              use:desktopContextMenu={{
                kind: 'file',
                filePath: filepath,
                open: previewFilepathTarget,
                fileScope: currentFilePreviewScope(),
              }}
              onclick={previewFilepathClick}
              onkeydown={previewFilepathKeydown}
            ><FileName name={filepath} /></span>
          {/if}
        </span>
      </button>

      <div class="code-actions">
        {#if showCopyButton}
          <button class="copy-btn" onclick={copyCode} class:copied aria-label={i18n.t(copied ? 'codeBlock.copied' : 'codeBlock.copy')} title={i18n.t(copied ? 'codeBlock.copied' : 'codeBlock.copy')}>
            {#key copyRevision}<Icon name={copied ? 'check' : 'copy'} size={14} />{/key}
          </button>
        {/if}
        <button class="code-disclosure" onclick={toggle} aria-expanded={!collapsed} aria-controls={contentId} aria-label={langName}>
          <ToolCardMeta expanded={!collapsed} />
        </button>
      </div>
    </div>

    <Disclosure expanded={!collapsed}>
      <div class="code-content" id={contentId}>
        {#if showLineNumbers}
          <div class="line-numbers" aria-hidden="true">
            {#each lines as _, i}
              <span class="line-num">{i + 1}</span>
            {/each}
          </div>
        {/if}
        <!-- 🔧 改用 {@html} 渲染，彻底避免 DOM 操作冲突 -->
        <pre
          class="code-pre"
          use:desktopContextMenu={{
            kind: 'code',
            content: trimmedCode,
            filePath: filepath || undefined,
            openFile: filepath ? previewFilepathTarget : undefined,
            fileScope: filepath ? currentFilePreviewScope() : undefined,
          }}
        ><code
          class="code-text {language ? `language-${language}` : ''}"
        >{@html highlightedHtml}</code></pre>
      </div>
    </Disclosure>
  </div>
{/if}

<style>
  .code-block {
    position: relative;

    margin: var(--spacing-sm) 0;
    overflow: hidden;
    box-shadow: none;
  }

  /* Draw the contour above the contents so the code surface cannot erase its sides. */
  .code-block::after {
    content: '';
    position: absolute;
    inset: 0;
    border-radius: inherit;
    box-shadow: inset 0 0 0 var(--magi-hairline) var(--tool-card-outline);
    pointer-events: none;
  }

  .code-header {
    justify-content: space-between;
    background: transparent;
    box-shadow: inset 0 calc(-1 * var(--magi-hairline)) var(--tool-card-outline);
    user-select: none;
  }

  .header-left {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    flex: 1;
    align-self: stretch;
    background: transparent;
    border: none;
    padding: 0;
    color: var(--foreground);
    font-family: inherit;
    cursor: pointer;
    overflow: hidden;
    outline: none;
  }

  .code-actions { display:flex; align-items:center; gap:8px; flex:none; }
  .code-disclosure { position:relative; display:flex; align-items:center; min-height:26px; padding:0; border:0; background:transparent; cursor:pointer; color:var(--foreground-muted); }
  .code-disclosure::before { content:''; position:absolute; inset:0 -6px; }
  .header-left:focus-visible,.code-disclosure:focus-visible,.copy-btn:focus-visible { outline:0; box-shadow:inset 0 0 0 1px var(--primary); border-radius:4px; }
  .code-icon {
    display: flex;
    align-items: center;
    color: var(--foreground-muted);
  }

  .code-title {
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 8px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .filepath {
    display: inline-flex;
    min-width: 0;
    overflow: hidden;
    opacity: 0.6;
    font-family: var(--font-mono);
    font-size: var(--text-xs);
  }

  .filepath-clickable {
    cursor: pointer;
  }

  .filepath-clickable:hover {
    color: var(--info);
    opacity: 1;
    text-decoration: underline;
  }

  .streaming-badge {
    font-size: var(--text-xs);
    background: var(--primary);
    color: var(--primary-foreground);
    padding: 0 4px;
    border-radius: 4px;
    animation: pulse 1.5s infinite;
  }

  .copy-btn {
    display: flex;
    align-items: center;
    gap: 6px;
    background: transparent;
    border: 0;
    border-radius: 4px;
    padding: 0;
    width: 26px;
    height: 26px;
    justify-content: center;
    color: var(--foreground-muted);
    font-size: var(--font-size-xs);
    cursor: pointer;
    transition: color var(--transition-fast), background-color var(--transition-fast);
  }

  .copy-btn:hover {
    background: var(--code-copy-hover-bg);
    color: var(--foreground);
  }

  .copy-btn.copied {
    color: var(--success);
    background: transparent;
  }

  .copy-btn.copied :global(svg) {
    animation: magi-feedback-pop 500ms var(--magi-spring-bouncy),
      magi-feedback-fade 160ms 2340ms ease-out forwards;
  }

  /* Line numbers and code inherit one typography and the same vertical padding. */
  .code-content {
    min-width: 0;
    display: flex;
    flex-direction: row;
    align-items: stretch;
    background: transparent;
    position: relative;
    
    /* 统一定义排版属性 */
    font-family: var(--font-mono);
    font-size: var(--code-panel-font-size);
    line-height: var(--code-panel-line-height);
    tab-size: 2;
  }

  .line-numbers {
    display: flex;
    flex-direction: column;
    padding: var(--code-panel-padding-block) 0 var(--code-panel-padding-block) var(--tool-card-padding);
    min-width: calc(2ch + var(--tool-card-padding));
    text-align: right;
    user-select: none;
    background: transparent;
    flex-shrink: 0;
    
    /* 强制继承父级排版 */
    font-family: inherit;
    font-size: inherit;
    line-height: inherit;
  }

  .line-num {
    display: block;
    color: var(--foreground-muted);
    white-space: nowrap;
    font-variant-numeric: tabular-nums;
  }

  .code-pre {
    flex: 1;
    margin: 0;
    padding: var(--code-panel-padding-block) var(--tool-card-padding);
    overflow-x: auto;
    background: transparent;
    min-width: 0;
    border: none;
    border-radius: 0;
  }

  .code-text {
    display: block;
    margin: 0;
    padding: 0;
    background: transparent;
    white-space: pre;
    color: var(--code-fg, inherit);
    border: none;
    
    /* Semantic code panels own their typography; inline Markdown code stays separate. */
    font-family: inherit;
    font-size: inherit;
    line-height: inherit;
  }

  @keyframes pulse {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.6; }
  }

</style>
