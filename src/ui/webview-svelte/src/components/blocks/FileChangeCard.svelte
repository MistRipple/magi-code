<script lang="ts">
  import hljs from 'highlight.js';
  import type { ContentBlock } from '../../types/message';
  import Icon from '../Icon.svelte';
  import FileSpan from '../FileSpan.svelte';
  import type { IconName } from '../../lib/icons';

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
      case 'create': return 'create';
      case 'delete': return 'delete';
      default: return 'edit';
    }
  });

  const changeIcon = $derived.by((): IconName => {
    if (!change) return 'file-text';
    switch (change.changeType) {
      case 'create': return 'file-plus';
      case 'delete': return 'trash';
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
    if (!change) return '没有可展示的 diff。';
    const additions = typeof change.additions === 'number' ? change.additions : 0;
    const deletions = typeof change.deletions === 'number' ? change.deletions : 0;
    if (additions > 0 || deletions > 0) {
      return '本次有文本变更，但后端未返回 diff 详情。';
    }
    return '本次操作最终未产生文本变更。';
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

  /** 统计徽标文本 */
  const statsBadge = $derived.by(() => {
    if (!change) return '';
    const parts: string[] = [];
    if (typeof change.additions === 'number' && change.additions > 0) parts.push(`+${change.additions}`);
    if (typeof change.deletions === 'number' && change.deletions > 0) parts.push(`-${change.deletions}`);
    return parts.join(' ');
  });
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
        <FileSpan filepath={change.filePath} showIcon={false} clickable={false} />
      </span>

      {#if statsBadge}
        <span class="stats-badge">{statsBadge}</span>
      {/if}

      <span class="tool-status status-success">
        <span class="status-dot"></span>
      </span>
    </button>

    {#if !collapsed}
      <div class="tool-content">
        {#if hasDiff}
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

  .stats-badge {
    font-size: var(--text-xs, 11px);
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    padding: 1px 6px;
    border-radius: 4px;
    background: var(--surface-2, rgba(0,0,0,0.15));
    color: var(--foreground-muted);
    white-space: nowrap;
    flex-shrink: 0;
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
    from { opacity: 0; max-height: 0; transform: translateY(-8px); }
    to { opacity: 1; max-height: 500px; transform: translateY(0); }
  }

  /* Diff 内容 */
  .diff-container {
    font-family: var(--font-mono);
    font-size: var(--text-xs, 11px);
    line-height: 1.5;
    overflow-x: auto;
    max-height: 400px;
    overflow-y: auto;
  }

  .diff-line {
    display: flex;
    padding: 0 var(--space-3, 12px);
    min-height: 20px;
  }

  .diff-prefix {
    flex-shrink: 0;
    width: 16px;
    text-align: center;
    user-select: none;
  }

  .diff-content {
    flex: 1;
    white-space: pre;
    padding-left: var(--space-2, 8px);
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
</style>
