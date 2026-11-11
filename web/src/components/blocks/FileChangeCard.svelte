<script lang="ts">
  import type { ConversationPresentationRole } from '../../lib/conversation-presentation';
  import FileName from '../FileName.svelte';
  import Disclosure from '../Disclosure.svelte';
  import type { ContentBlock } from '../../types/message';
  import type { FilePreviewScope } from '../../lib/file-reference';
  import Icon from '../Icon.svelte';
  import ToolCardMeta from '../ToolCardMeta.svelte';
  import FileSpan from '../FileSpan.svelte';
  import type { IconName } from '../../lib/icons';
  import { i18n } from '../../stores/i18n.svelte';
  import { requestOpenHtmlFileInBrowser } from '../../lib/browser-navigation';
  import { openCodeTab } from '../../stores/right-pane.svelte';
  import DiffCodeBlock from './DiffCodeBlock.svelte';
  import { desktopContextMenu } from '../../lib/desktop-context-menu-contract';

  interface Props {
    block: ContentBlock;
    presentationRole?: ConversationPresentationRole;
    filePreviewScope?: FilePreviewScope;
  }

  let { block, filePreviewScope = undefined }: Props = $props();
  const change = $derived(block.fileChange);

  // 文件变更保留可见摘要，diff 默认折叠；后续更新不覆盖用户的手动选择。
  let collapsed = $state(true);
  const contentId = $props.id();

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

  const hasDiff = $derived(typeof change?.diff === 'string' && change.diff.trim().length > 0);

  const emptyDiffNote = $derived.by(() => {
    if (!change) return i18n.t('fileChangeCard.noDiff');
    const additions = typeof change.additions === 'number' ? change.additions : 0;
    const deletions = typeof change.deletions === 'number' ? change.deletions : 0;
    if (additions > 0 || deletions > 0) {
      return i18n.t('fileChangeCard.noDiffDetail');
    }
    return i18n.t('fileChangeCard.noTextChange');
  });

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
  const contextFileScope = $derived({
    sourceFolderId: change?.sourceFolderId ?? filePreviewScope?.sourceFolderId,
    sessionId: filePreviewScope?.sessionId ?? change?.sessionId,
    workspaceId: filePreviewScope?.workspaceId ?? change?.workspaceId,
    workspacePath: filePreviewScope?.workspacePath ?? change?.workspacePath,
  });

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

  function previewInRightPane(filepath: string) {
    if (!filepath) return;
    if (requestOpenHtmlFileInBrowser(filepath, contextFileScope)) return;
    const sessionId = filePreviewScope?.sessionId ?? change?.sessionId;
    const workspaceId = filePreviewScope?.workspaceId ?? change?.workspaceId;
    const workspacePath = filePreviewScope?.workspacePath ?? change?.workspacePath;
    openCodeTab(sessionId, filepath, {
      sourceFolderId: contextFileScope.sourceFolderId,
      sessionId,
      workspaceId,
      workspacePath,
      diff: change?.diff ?? null,
      isChangeDiff: Boolean(change?.diff),
      contentKind: change?.contentKind,
      size: change?.size,
      mime: change?.mime,
      symlinkTarget: change?.symlinkTarget,
      headSummary: change?.headSummary,
      tailSummary: change?.tailSummary,
    });
  }
</script>

{#if change}
  <div class="message-card tool-call" class:collapsed>
    <div class="tool-header tool-header-with-actions">
      <span class="tool-icon">
        <Icon name={changeIcon} size={14} />
      </span>

      <span class="tool-title">
        <span class="tool-name">{changeLabel}</span>
        {#if change.changeType === 'rename' && change.oldPath}
          <button
            type="button"
            class="rename-path"
            title={displayPath}
            use:desktopContextMenu={{
              kind: 'file',
              filePath: change.filePath,
              open: () => previewInRightPane(change.filePath),
              fileScope: contextFileScope,
            }}
            onclick={() => previewInRightPane(change.filePath)}
          ><FileName name={displayPath ?? ""} /></button>
        {:else}
          <FileSpan
            filepath={change.filePath}
            showIcon={false}
            clickable={true}
            onClick={previewInRightPane}
            filePreviewScope={contextFileScope}
          />
        {/if}
      </span>

      <button type="button" class="tool-header-disclosure" onclick={toggle} aria-label={`${changeLabel} ${displayPath}`} aria-expanded={!collapsed} aria-controls={contentId}>
        <ToolCardMeta status={change.error ? 'error' : 'success'} label={i18n.t(change.error ? 'terminalSession.status.error' : 'terminalSession.status.success')} expanded={!collapsed}>

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

        </ToolCardMeta>
      </button>
    </div>

    <Disclosure expanded={!collapsed}>
      <div class="tool-content" id={contentId}>
        {#if change.error}
          <div class="empty-diff-note error">{i18n.t('fileChangeCard.previewUnavailable')}</div>
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
          <DiffCodeBlock embedded diff={change.diff} ariaLabel={change.filePath} />
        {:else}
          <div class="empty-diff-note">
            {emptyDiffNote}
          </div>
        {/if}
      </div>
    </Disclosure>
  </div>
{/if}

<style>
  /* 复用 ToolCall 卡片容器样式 */
  .tool-call {
    margin: var(--space-2, 8px) 0;
    overflow: hidden;
  }


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
    border: 0;
    padding: 0;
    background: transparent;
    min-width: 0;
    overflow: hidden;
    color: var(--foreground);
    font-size: var(--text-sm, 13px);
    font-family: var(--font-mono);
    text-overflow: ellipsis;
    white-space: nowrap;
    cursor: pointer;
  }

  .rename-path:hover {
    color: var(--info);
    text-decoration: underline;
  }

  .stats-badge {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: var(--text-xs, 11px);
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    padding: 0;
    border-radius: 4px;
    background: transparent;
    white-space: nowrap;
    flex-shrink: 0;
  }

  .stats-value {
    font-weight: var(--font-semibold);
  }

  .stat-add {
    color: var(--success);
  }

  .stat-del {
    color: var(--error);
  }

  .stat-meta {
    color: var(--foreground-muted);
    font-weight: var(--font-medium);
  }

  .non-text-card {
    display: flex;
    flex-direction: column;
    gap: var(--space-3, 12px);
    margin: 0;
    padding: 12px var(--tool-card-padding) 14px;
    border: 0;
    border-radius: 0;
    background: transparent;
  }

  .non-text-title {
    font-size: var(--text-sm, 13px);
    font-weight: var(--font-semibold);
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
    font-size: var(--text-xs);
    font-weight: var(--font-semibold);
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }

  .non-text-snippet {
    margin: 0;
    padding: var(--space-2, 8px);
    border: 0;
    border-radius: 4px;
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
    padding: 0;
    border: 0;
    border-radius: 0;
    color: var(--error);
    background: transparent;
  }

  .tool-content {
    border-top: var(--magi-hairline) solid var(--border);
    background: transparent;
  }

  .empty-diff-note {
    margin: var(--space-3, 12px);
    color: var(--foreground-muted);
    font-size: var(--text-sm, 13px);
    line-height: 1.5;
  }

</style>
