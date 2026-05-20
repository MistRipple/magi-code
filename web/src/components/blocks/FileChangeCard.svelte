<script lang="ts">
  import hljs from 'highlight.js';
  import type { ContentBlock } from '../../types/message';
  import Icon from '../Icon.svelte';
  import FileSpan from '../FileSpan.svelte';
  import type { IconName } from '../../lib/icons';
  import { i18n } from '../../stores/i18n.svelte';
  import { openCodeTab } from '../../stores/right-pane.svelte';
  import { messagesState } from '../../stores/messages.svelte';

  interface Props {
    block: ContentBlock;
  }

  /** 解析后的 diff 行 */
  interface DiffLine {
    type: 'add' | 'delete' | 'context';
    content: string;
  }

  let { block }: Props = $props();
  const change = $derived(block.fileChange);

  // 默认折叠，与 ToolCall 保持一致
  let collapsed = $state(true);

  function toggle() {
    collapsed = !collapsed;
  }

  const changeLabel = $derived.by(() => {
    if (!change) return '';
    switch (change.changeType) {
      case 'create': return i18n.t('fileChangeCard.label.create');
      case 'delete': return i18n.t('fileChangeCard.label.delete');
      case 'rename': return i18n.t('fileChangeCard.label.rename');
      default: return i18n.t('fileChangeCard.label.edit');
    }
  });

  const changeIcon = $derived.by((): IconName => {
    if (!change) return 'file-text';
    switch (change.changeType) {
      case 'create': return 'file-plus';
      case 'delete': return 'trash';
      case 'rename': return 'git-branch';
      default: return 'pencil';
    }
  });

  /** 从文件路径推断 hljs 语言标识 */
  const EXT_LANG_MAP: Record<string, string> = {
    ts: 'typescript', tsx: 'typescript', js: 'javascript', jsx: 'javascript',
    py: 'python', rb: 'ruby', go: 'go', rs: 'rust', java: 'java',
    cpp: 'cpp', c: 'c', cs: 'csharp', kt: 'kotlin', swift: 'swift',
    html: 'xml', vue: 'xml', svelte: 'xml', xml: 'xml', svg: 'xml',
    css: 'css', scss: 'scss', less: 'less',
    json: 'json', yaml: 'yaml', yml: 'yaml', toml: 'ini',
    md: 'markdown', sh: 'bash', bash: 'bash', zsh: 'bash',
    sql: 'sql', graphql: 'graphql', dockerfile: 'dockerfile',
  };

  const diffLanguage = $derived.by(() => {
    if (!change?.filePath) return '';
    const ext = change.filePath.split('.').pop()?.toLowerCase() ?? '';
    return EXT_LANG_MAP[ext] ?? '';
  });

  /** 解析 unified diff 文本为结构化行数据（过滤 hunk 头信息） */
  const diffLines = $derived.by((): DiffLine[] => {
    if (!change?.diff) return [];
    return change.diff.split('\n')
      .filter(line => !line.startsWith('---') && !line.startsWith('+++') && !line.startsWith('@@'))
      .map(line => {
        if (line.startsWith('+')) return { type: 'add' as const, content: line.slice(1) };
        if (line.startsWith('-')) return { type: 'delete' as const, content: line.slice(1) };
        return { type: 'context' as const, content: line.startsWith(' ') ? line.slice(1) : line };
      });
  });

  const hasDiff = $derived(diffLines.length > 0);

  const emptyDiffNote = $derived.by(() => {
    if (!change) return i18n.t('fileChangeCard.noDiff');
    const additions = typeof change.additions === 'number' ? change.additions : 0;
    const deletions = typeof change.deletions === 'number' ? change.deletions : 0;
    if (additions > 0 || deletions > 0) {
      return i18n.t('fileChangeCard.noDiffDetail');
    }
    return i18n.t('fileChangeCard.noTextChange');
  });

  /** 对代码行做 hljs 语法高亮，返回 HTML 字符串数组（与 diffLines 一一对应） */
  const highlightedLines = $derived.by((): string[] => {
    if (!diffLines.length) return [];
    const lang = diffLanguage;
    const hasLang = lang && hljs.getLanguage(lang);
    if (!hasLang) {
      return diffLines.map(l => escapeHtml(l.content));
    }
    const codeTexts = diffLines.map(l => l.content);
    try {
      const fullHighlighted = hljs.highlight(codeTexts.join('\n'), { language: lang }).value;
      return fullHighlighted.split('\n');
    } catch {
      return codeTexts.map(t => escapeHtml(t));
    }
  });

  function escapeHtml(str: string): string {
    return str
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;')
      .replace(/'/g, '&#039;');
  }

  const additionsCount = $derived.by(() =>
    change && typeof change.additions === 'number' && change.additions > 0 ? change.additions : 0
  );
  const deletionsCount = $derived.by(() =>
    change && typeof change.deletions === 'number' && change.deletions > 0 ? change.deletions : 0
  );
  const hasStatsBadge = $derived(additionsCount > 0 || deletionsCount > 0);
  const kind = $derived(change?.contentKind ?? 'text');
  const isText = $derived(kind === 'text');
  const displayPath = $derived(change?.changeType === 'rename' && change.oldPath
    ? `${change.oldPath} → ${change.filePath}`
    : change?.filePath);

  function formatSize(size?: number): string {
    if (typeof size !== 'number' || !Number.isFinite(size) || size < 0) {
      return '-';
    }
    if (size < 1024) return `${size} B`;
    if (size < 1024 * 1024) return `${(size / 1024).toFixed(1)} KB`;
    if (size < 1024 * 1024 * 1024) return `${(size / (1024 * 1024)).toFixed(1)} MB`;
    return `${(size / (1024 * 1024 * 1024)).toFixed(1)} GB`;
  }

  function kindLabel(value: string): string {
    switch (value) {
      case 'binary': return i18n.t('edits.kind.binary');
      case 'large_text': return i18n.t('edits.kind.largeText');
      case 'symlink': return i18n.t('edits.kind.symlink');
      case 'special': return i18n.t('edits.kind.special');
      default: return i18n.t('edits.kind.text');
    }
  }

  /**
   * 把当前 file change 推到右侧 RightPane 的 code tab。
   * 优先带上 diff（unified 文本）；二进制/特殊类型且无 diff 时仅传 filepath，让 RightPane 显示空态。
   */
  function previewInRightPane(filepath: string) {
    if (!filepath) return;
    openCodeTab(messagesState.currentSessionId, filepath, {
      diff: change?.diff ?? null,
    });
  }
</script>

{#if change}
  <div class="tool-call" class:collapsed>
    <button class="tool-header" onclick={toggle}>
      <span class="chevron">
        <Icon name="chevron-right" size={12} />
      </span>

      <span class="tool-icon">
        <Icon name={changeIcon} size={14} />
      </span>

      <span class="tool-title">
        <span class="tool-name">{changeLabel}</span>
        {#if change.changeType === 'rename' && change.oldPath}
          <span class="rename-path" title={displayPath}>{displayPath}</span>
        {:else}
          <FileSpan filepath={change.filePath} showIcon={false} clickable={true} onClick={previewInRightPane} />
        {/if}
      </span>

      {#if hasStatsBadge && isText}
        <span class="stats-badge">
          {#if additionsCount > 0}
            <span class="stats-value stat-add">+{additionsCount}</span>
          {/if}
          {#if deletionsCount > 0}
            <span class="stats-value stat-del">-{deletionsCount}</span>
          {/if}
        </span>
      {:else if !isText}
        <span class="stats-badge">
          <span class="stats-value stat-meta">{kindLabel(kind)}</span>
          <span class="stats-value stat-meta">{formatSize(change.size)}</span>
        </span>
      {/if}

      <span class="tool-status status-success">
        <span class="status-dot"></span>
      </span>
    </button>

    {#if !collapsed}
      <div class="tool-content">
        {#if change.error}
          <div class="empty-diff-note error">{change.error}</div>
        {:else if !isText}
          <div class="non-text-card">
            <div class="non-text-title">{kindLabel(kind)}</div>
            <div class="non-text-meta">
              <span>{i18n.t('edits.nonText.size')}: {formatSize(change.size)}</span>
              {#if change.mime}
                <span>{i18n.t('edits.nonText.mime')}: {change.mime}</span>
              {/if}
              {#if change.symlinkTarget}
                <span>{i18n.t('edits.nonText.target')}: {change.symlinkTarget}</span>
              {/if}
            </div>
            {#if change.headSummary}
              <div class="non-text-section">
                <div class="non-text-section-label">{i18n.t('edits.nonText.head')}</div>
                <pre class="non-text-snippet">{change.headSummary}</pre>
              </div>
            {/if}
            {#if change.tailSummary}
              <div class="non-text-section">
                <div class="non-text-section-label">{i18n.t('edits.nonText.tail')}</div>
                <pre class="non-text-snippet">{change.tailSummary}</pre>
              </div>
            {/if}
            {#if kind === 'binary'}
              <div class="non-text-hint">{i18n.t('edits.nonText.binaryHint')}</div>
            {:else if kind === 'special'}
              <div class="non-text-hint">{i18n.t('edits.nonText.specialHint')}</div>
            {/if}
          </div>
        {:else if hasDiff}
          <div class="diff-container">
            {#each diffLines as line, i}
              <div class="diff-line {line.type}">
                <span class="diff-prefix">{line.type === 'add' ? '+' : line.type === 'delete' ? '-' : ' '}</span>
                <span class="diff-content">{@html highlightedLines[i] ?? ''}</span>
              </div>
            {/each}
          </div>
        {:else}
          <div class="empty-diff-note">
            {emptyDiffNote}
          </div>
        {/if}
      </div>
    {/if}
  </div>
{/if}

<style>
  /* 复用 ToolCall 卡片容器样式 */
  .tool-call {
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    margin: var(--space-2, 8px) 0;
    overflow: hidden;
    background: var(--surface-1, rgba(255,255,255,0.02));
  }

  /* header 高度/padding/字号/accent 条/chevron 等共享规范见 styles/tool-card.css */

  /* tool-icon 中性化：accent 条承担状态色，图标用 muted 避免三层颜色冲突 */
  .tool-icon {
    display: flex;
    color: var(--foreground-muted);
  }

  .tool-title {
    flex: 1;
    display: flex;
    align-items: center;
    gap: var(--space-2, 8px);
    min-width: 0;
    overflow: hidden;
  }

  .rename-path {
    min-width: 0;
    overflow: hidden;
    color: var(--foreground);
    font-size: var(--text-sm, 13px);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .stats-badge {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: var(--text-xs, 11px);
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    padding: 1px 6px;
    border-radius: 4px;
    background: var(--surface-2, rgba(0,0,0,0.15));
    white-space: nowrap;
    flex-shrink: 0;
  }

  .stats-value {
    font-weight: 600;
  }

  .stat-add {
    color: var(--success);
  }

  .stat-del {
    color: var(--error);
  }

  .stat-meta {
    color: var(--foreground-muted);
    font-weight: 500;
  }

  .non-text-card {
    display: flex;
    flex-direction: column;
    gap: var(--space-3, 12px);
    margin: var(--space-3, 12px);
    padding: var(--space-3, 12px);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--surface-1, rgba(255, 255, 255, 0.02));
  }

  .non-text-title {
    font-size: var(--text-sm, 13px);
    font-weight: 600;
    color: var(--foreground);
  }

  .non-text-meta {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-3, 12px);
    color: var(--foreground-muted);
    font-size: var(--text-xs, 12px);
    font-variant-numeric: tabular-nums;
  }

  .non-text-section {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .non-text-section-label {
    color: var(--foreground-muted);
    font-size: var(--text-2xs, 11px);
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }

  .non-text-snippet {
    margin: 0;
    padding: var(--space-2, 8px);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--surface-2, rgba(0, 0, 0, 0.1));
    color: var(--foreground);
    font-family: var(--font-mono);
    font-size: var(--text-xs, 11px);
    line-height: 1.5;
    max-height: 200px;
    overflow: auto;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  .non-text-hint {
    color: var(--foreground-muted);
    font-size: var(--text-xs, 12px);
    line-height: 1.5;
  }

  .empty-diff-note.error {
    border-color: color-mix(in srgb, var(--error) 40%, var(--border));
    color: var(--error);
    background: color-mix(in srgb, var(--error) 10%, var(--surface-1));
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

  .status-success { color: var(--success); }

  .tool-content {
    border-top: 1px solid var(--border);
    background: var(--surface-2, rgba(0,0,0,0.1));
    animation: slideDown 0.2s ease-out;
    transform-origin: top;
  }

  .empty-diff-note {
    margin: var(--space-3, 12px);
    padding: var(--space-3, 12px);
    border: 1px dashed var(--border);
    border-radius: var(--radius-sm);
    color: var(--foreground-muted);
    font-size: var(--text-sm, 13px);
    line-height: 1.5;
    background: var(--surface-1, rgba(255,255,255,0.02));
  }

  @keyframes slideDown {
    from { opacity: 0; transform: translateY(-8px); }
    to { opacity: 1; transform: translateY(0); }
  }

  /* Diff 内容 */
  .diff-container {
    font-family: var(--font-mono);
    font-size: var(--text-xs, 11px);
    line-height: 1.5;
    overflow-x: hidden;
    max-height: min(60vh, 640px);
    overflow-y: auto;
  }

  .diff-line {
    display: grid;
    grid-template-columns: 20px minmax(0, 1fr);
    align-items: start;
    min-width: 0;
    min-height: 20px;
    padding: 0 var(--space-3, 12px);
  }

  .diff-prefix {
    text-align: center;
    user-select: none;
  }

  .diff-content {
    min-width: 0;
    padding-left: var(--space-2, 8px);
    background: transparent !important;
    border: none !important;
    box-shadow: none !important;
    overflow-wrap: anywhere;
    white-space: pre-wrap;
  }

  /* 行类型着色 */
  .diff-line.add {
    background: color-mix(in oklab, var(--success) 15%, transparent);
    color: var(--foreground);
  }
  .diff-line.add .diff-prefix { color: var(--success); }

  .diff-line.delete {
    background: color-mix(in oklab, var(--error) 15%, transparent);
    color: var(--foreground);
  }
  .diff-line.delete .diff-prefix { color: var(--error); }

  .diff-line.context {
    color: var(--foreground-muted);
  }

  :global(.theme-light) .diff-line.add,
  :global(:root.theme-light) .diff-line.add {
    background: #dafbe1;
  }

  :global(.theme-light) .diff-line.delete,
  :global(:root.theme-light) .diff-line.delete {
    background: #ffeef0;
  }
</style>
