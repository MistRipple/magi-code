<script lang="ts">
  import Icon from '../components/Icon.svelte';
  import MarkdownContent from '../components/MarkdownContent.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import {
    isKnownBinaryFile,
    isMarkdownFile,
    isWordFile,
  } from '../lib/file-preview-utils';
  import type { EditContentKind } from '../types/message';

  interface Props {
    filePath: string;
    workspaceRoot: string;
    content: string | null;
    loading: boolean;
    error: string;
    contentKind?: EditContentKind;
    size?: number;
    mime?: string;
    symlinkTarget?: string;
    headSummary?: string;
    tailSummary?: string;
    onClose?: () => void;
  }

  let {
    filePath,
    workspaceRoot,
    content,
    loading,
    error,
    contentKind = 'text',
    size,
    mime,
    symlinkTarget,
    headSummary,
    tailSummary,
    onClose,
  }: Props = $props();
  let markdownMode = $state<'rendered' | 'raw'>('rendered');

  const displayPath = $derived(getDisplayPath(filePath, workspaceRoot));
  const markdownFile = $derived(isMarkdownFile(filePath));
  const wordFile = $derived(isWordFile(filePath));
  const binaryFile = $derived(contentKind === 'binary' || isKnownBinaryFile(filePath));
  const largeTextFile = $derived(contentKind === 'large_text');
  const symlinkFile = $derived(contentKind === 'symlink');
  const specialFile = $derived(contentKind === 'special');
  const previewContent = $derived(content ?? '');
  const truncatedContent = $derived(
    previewContent.length > 500_000 ? previewContent.slice(0, 100_000) : previewContent
  );
  const isLargeFile = $derived(previewContent.length > 500_000);
  const sourceLines = $derived(truncatedContent.split('\n'));
  const codePreviewMode = $derived(
    !loading && !error && !wordFile && !binaryFile && !largeTextFile && !symlinkFile && !specialFile && !!previewContent && (!markdownFile || markdownMode === 'raw')
  );

  function formatSize(value?: number): string {
    if (typeof value !== 'number' || !Number.isFinite(value) || value < 0) {
      return '-';
    }
    if (value < 1024) return `${value} B`;
    if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KB`;
    if (value < 1024 * 1024 * 1024) return `${(value / (1024 * 1024)).toFixed(1)} MB`;
    return `${(value / (1024 * 1024 * 1024)).toFixed(1)} GB`;
  }

  function getDisplayPath(path: string, root: string): string {
    const normalizedPath = path.replace(/\\/g, '/');
    const normalizedRoot = root.replace(/\\/g, '/').replace(/\/+$/, '');
    if (normalizedRoot && normalizedPath.startsWith(`${normalizedRoot}/`)) {
      return normalizedPath.slice(normalizedRoot.length + 1);
    }
    return path;
  }
</script>

<aside class="file-preview-panel" aria-label={i18n.t('web.filePreviewTitle')}>
  <header class="file-preview-header">
    <div class="file-preview-title-group">
      <div class="file-preview-title">
        <Icon name={markdownFile ? 'file-text' : wordFile ? 'document' : 'file'} size={14} />
        <span>{i18n.t('web.filePreviewTitle')}</span>
      </div>
      <div class="file-preview-path" title={filePath}>{displayPath}</div>
    </div>
    <button
      type="button"
      class="file-preview-close"
      onclick={() => onClose?.()}
      title={i18n.t('common.close')}
      aria-label={i18n.t('common.close')}
    >
      <Icon name="close" size={14} />
    </button>
  </header>

  {#if markdownFile && !loading && !error && !wordFile && !binaryFile && !largeTextFile && !symlinkFile && !specialFile && previewContent}
    <div class="file-preview-tabs" role="tablist" aria-label={i18n.t('web.filePreviewTitle')}>
      <button
        type="button"
        class="file-preview-tab"
        class:active={markdownMode === 'rendered'}
        onclick={() => markdownMode = 'rendered'}
      >
        {i18n.t('web.filePreviewRendered')}
      </button>
      <button
        type="button"
        class="file-preview-tab"
        class:active={markdownMode === 'raw'}
        onclick={() => markdownMode = 'raw'}
      >
        {i18n.t('web.filePreviewRaw')}
      </button>
    </div>
  {/if}

  <div class="file-preview-body" class:file-preview-body--code={codePreviewMode}>
    {#if loading}
      <div class="file-preview-state">{i18n.t('web.filePreviewLoading')}</div>
    {:else if error}
      <div class="file-preview-state file-preview-state--error">
        {i18n.t('web.filePreviewError', { message: error })}
      </div>
    {:else if wordFile}
      <div class="file-preview-state">
        <Icon name="document" size={22} />
        <span>{i18n.t('web.filePreviewUnsupportedWord')}</span>
      </div>
    {:else if binaryFile}
      <div class="file-preview-state file-preview-state--metadata">
        <Icon name="file" size={22} />
        <span>{i18n.t('web.filePreviewUnsupportedBinary')}</span>
        <span class="file-preview-meta-line">{i18n.t('edits.nonText.size')}: {formatSize(size)}</span>
        {#if mime}
          <span class="file-preview-meta-line">{i18n.t('edits.nonText.mime')}: {mime}</span>
        {/if}
      </div>
    {:else if largeTextFile}
      <div class="file-preview-large-text">
        <div class="file-preview-notice">{i18n.t('edits.nonText.largeTextTitle')} · {i18n.t('edits.nonText.size')}: {formatSize(size)}</div>
        {#if headSummary}
          <div class="file-preview-summary-section">
            <div class="file-preview-summary-title">{i18n.t('edits.nonText.head')}</div>
            <pre class="file-preview-summary-content">{headSummary}</pre>
          </div>
        {/if}
        {#if tailSummary}
          <div class="file-preview-summary-section">
            <div class="file-preview-summary-title">{i18n.t('edits.nonText.tail')}</div>
            <pre class="file-preview-summary-content">{tailSummary}</pre>
          </div>
        {/if}
      </div>
    {:else if symlinkFile}
      <div class="file-preview-state file-preview-state--metadata">
        <Icon name="file" size={22} />
        <span>{i18n.t('edits.nonText.symlinkTitle')}</span>
        <span class="file-preview-meta-line">{i18n.t('edits.nonText.target')}: {symlinkTarget ?? '-'}</span>
      </div>
    {:else if specialFile}
      <div class="file-preview-state file-preview-state--metadata">
        <Icon name="file" size={22} />
        <span>{i18n.t('edits.nonText.specialTitle')}</span>
        <span class="file-preview-meta-line">{i18n.t('edits.nonText.specialHint')}</span>
      </div>
    {:else if !previewContent}
      <div class="file-preview-state">{i18n.t('edits.preview.empty')}</div>
    {:else}
      {#if isLargeFile}
        <div class="file-preview-notice">{i18n.t('web.filePreviewLargeFile')}</div>
      {/if}
      {#if markdownFile && markdownMode === 'rendered'}
        <div class="file-preview-markdown">
          <MarkdownContent content={truncatedContent} />
        </div>
      {:else}
        <div class="file-preview-source" aria-label={displayPath}>
          {#each sourceLines as line, index}
            <div class="file-preview-source-row">
              <span class="file-preview-source-line-number" aria-hidden="true">{index + 1}</span>
              <code class="file-preview-source-line">{line || ' '}</code>
            </div>
          {/each}
        </div>
      {/if}
    {/if}
  </div>
</aside>

<style>
  .file-preview-panel {
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
    height: 100%;
    background: color-mix(in srgb, var(--background) 94%, var(--surface-1));
  }

  .file-preview-header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: var(--space-3);
    padding: var(--space-3) var(--space-4);
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
  }

  .file-preview-title-group {
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .file-preview-title {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    color: var(--foreground);
    font-size: var(--text-sm);
    font-weight: var(--font-semibold);
  }

  .file-preview-path {
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
  }

  .file-preview-close {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
    padding: 0;
    border: none;
    border-radius: var(--radius-md);
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    transition: background var(--transition-fast), color var(--transition-fast);
    flex-shrink: 0;
  }

  .file-preview-close:hover {
    background: var(--surface-hover);
    color: var(--foreground);
  }

  .file-preview-tabs {
    display: inline-flex;
    gap: 2px;
    padding: 8px var(--space-4) 0;
    flex-shrink: 0;
  }

  .file-preview-tab {
    padding: 4px 10px;
    border: none;
    border-radius: var(--radius-full);
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    font-size: var(--text-xs);
    transition: background var(--transition-fast), color var(--transition-fast);
  }

  .file-preview-tab:hover,
  .file-preview-tab.active {
    background: color-mix(in srgb, var(--surface-selected) 72%, transparent);
    color: var(--foreground);
  }

  .file-preview-body {
    min-height: 0;
    flex: 1;
    overflow: auto;
    padding: var(--space-4);
  }

  .file-preview-body--code {
    display: flex;
    flex-direction: column;
    overflow: hidden;
    padding: 0;
  }

  .file-preview-source {
    min-height: 0;
    flex: 1;
    overflow: auto;
    padding: var(--space-4) 0;
    background: transparent;
    color: var(--foreground);
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    line-height: 1.6;
  }

  .file-preview-source-row {
    display: grid;
    grid-template-columns: 46px minmax(0, 1fr);
    align-items: start;
    min-width: 0;
  }

  .file-preview-source-line-number {
    position: sticky;
    left: 0;
    z-index: 1;
    padding: 0 10px 0 var(--space-2);
    background: transparent;
    color: var(--foreground-muted);
    font-variant-numeric: tabular-nums;
    opacity: 0.46;
    text-align: right;
    user-select: none;
  }

  .file-preview-source-line {
    min-width: 0;
    padding: 0 var(--space-4) 0 var(--space-3);
    background: transparent !important;
    border: none !important;
    box-shadow: none !important;
    color: inherit;
    font: inherit;
    overflow-wrap: anywhere;
    tab-size: 2;
    white-space: pre-wrap;
  }

  .file-preview-markdown {
    max-width: 880px;
    color: var(--foreground);
    line-height: 1.65;
  }

  .file-preview-state {
    min-height: 180px;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: var(--space-3);
    color: var(--foreground-muted);
    text-align: center;
    font-size: var(--text-sm);
    line-height: 1.5;
  }

  .file-preview-state--error {
    color: var(--error);
  }

  .file-preview-state--metadata {
    padding: var(--space-4);
  }

  .file-preview-meta-line {
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }

  .file-preview-large-text {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }

  .file-preview-summary-section {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .file-preview-summary-title {
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    font-weight: var(--font-semibold);
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }

  .file-preview-summary-content {
    margin: 0;
    padding: var(--space-3);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: color-mix(in srgb, var(--surface-1) 82%, var(--background));
    color: var(--foreground);
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    line-height: 1.6;
    max-height: 260px;
    overflow: auto;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  .file-preview-notice {
    margin-bottom: var(--space-3);
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius-md);
    border: 1px solid color-mix(in srgb, var(--warning, #f59e0b) 30%, var(--border));
    background: color-mix(in srgb, var(--warning, #f59e0b) 10%, transparent);
    color: var(--foreground);
    font-size: var(--text-xs);
  }

</style>
