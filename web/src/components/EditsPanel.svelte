<script lang="ts">
  import { onMount } from 'svelte';
  import { getCurrentSessionId, getState } from '../stores/messages.svelte';
  import { vscode } from '../lib/vscode-bridge';
  import { ensureArray } from '../lib/utils';
  import type { Edit } from '../types/message';
  import type { IconName } from '../lib/icons';
  import Icon from './Icon.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { isWebAgentMode } from '../web/agent-api';

  const appState = getState();
  const isWebMode = isWebAgentMode();

  const edits = $derived(ensureArray(appState.edits) as Edit[]);
  let isCompactViewport = $state(false);
  let previewOpen = $state(false);
  let selectedPreviewKey = $state('');
  const useDockedPreview = $derived(isWebMode && !isCompactViewport);
  const selectedPreviewEdit = $derived(
    selectedPreviewKey
      ? edits.find((edit) => getEditKey(edit) === selectedPreviewKey) ?? null
      : null
  );
  const previewTitle = $derived(selectedPreviewEdit?.filePath ?? '');
  const previewDisplayTitle = $derived(selectedPreviewEdit?.type === 'rename' && selectedPreviewEdit.oldPath
    ? `${selectedPreviewEdit.oldPath} → ${selectedPreviewEdit.filePath}`
    : previewTitle);
  const previewContent = $derived(selectedPreviewEdit?.diff ?? '');
  const previewContentKind = $derived(selectedPreviewEdit?.contentKind ?? 'text');
  const previewHasDiff = $derived(previewContentKind === 'text');
  const previewError = $derived.by(() => {
    if (!selectedPreviewEdit) {
      return '';
    }
    if (selectedPreviewEdit.error) {
      return selectedPreviewEdit.error;
    }
    if (previewHasDiff && !previewContent) {
      return i18n.t('fileChangeCard.noDiffDetail');
    }
    return '';
  });

  onMount(() => {
    if (!isWebMode || typeof window === 'undefined') {
      return;
    }
    const media = window.matchMedia('(max-width: 1120px)');
    const updateViewport = () => {
      isCompactViewport = media.matches;
    };
    updateViewport();
    media.addEventListener('change', updateViewport);
    return () => media.removeEventListener('change', updateViewport);
  });

  // ─── 按执行分组分组 ───
  // 最新执行分组 ID：取 edits 列表中最后一个有 executionGroupId 的值（后端已按 timestamp 排序）
  const latestExecutionGroupId = $derived.by(() => {
    if (edits.length === 0) return null;
    for (let i = edits.length - 1; i >= 0; i--) {
      if (edits[i].executionGroupId) return edits[i].executionGroupId!;
    }
    return null;
  });

  // 本轮变更
  const currentRoundEdits = $derived(
    latestExecutionGroupId ? edits.filter(e => e.executionGroupId === latestExecutionGroupId) : []
  );

  // 统一暂存（非本轮）
  const stagedEdits = $derived(
    latestExecutionGroupId ? edits.filter(e => e.executionGroupId !== latestExecutionGroupId) : edits
  );

  // 是否有两组分组（只有同时存在统一暂存和本轮变更才分组显示）
  const hasGroups = $derived(stagedEdits.length > 0 && currentRoundEdits.length > 0);

  // 拆分文件名和目录
  function splitPath(filePath: string): { dir: string; name: string } {
    const lastSlash = filePath.lastIndexOf('/');
    if (lastSlash === -1) return { dir: '', name: filePath };
    return { dir: filePath.substring(0, lastSlash + 1), name: filePath.substring(lastSlash + 1) };
  }

  // 文件类型图标名
  function getFileIconName(edit: Edit): IconName {
    if (edit?.type === 'add') return 'file-plus';
    if (edit?.type === 'delete') return 'file-minus';
    if (edit?.type === 'modify') return 'file-edit';
    if (edit?.type === 'rename') return 'git-branch';
    return 'file-text';
  }

  function closePreview(): void {
    previewOpen = false;
    if (!useDockedPreview) {
      selectedPreviewKey = '';
    }
  }

  function approveChange(filePath: string) {
    const sessionId = getCurrentSessionId() || undefined;
    vscode.postMessage({ type: 'approveChange', filePath, sessionId });
  }
  function revertChange(filePath: string) {
    const sessionId = getCurrentSessionId() || undefined;
    vscode.postMessage({ type: 'revertChange', filePath, sessionId });
  }

  let pendingBatch = $state<'approveAll' | 'revertAll' | 'revertRound' | null>(null);
  function approveAllChanges() {
    if (pendingBatch || edits.length === 0) return;
    pendingBatch = 'approveAll';
    const sessionId = getCurrentSessionId() || undefined;
    vscode.postMessage({ type: 'approveAllChanges', sessionId });
    setTimeout(() => { pendingBatch = null; }, 1500);
  }
  function revertAllChanges() {
    if (pendingBatch || edits.length === 0) return;
    pendingBatch = 'revertAll';
    const sessionId = getCurrentSessionId() || undefined;
    vscode.postMessage({ type: 'revertAllChanges', sessionId });
    setTimeout(() => { pendingBatch = null; }, 1500);
  }
  function revertCurrentRound() {
    if (pendingBatch || !latestExecutionGroupId) return;
    pendingBatch = 'revertRound';
    const sessionId = getCurrentSessionId() || undefined;
    vscode.postMessage({
      type: 'revertExecutionGroup',
      executionGroupId: latestExecutionGroupId,
      sessionId,
    });
    setTimeout(() => { pendingBatch = null; }, 1500);
  }

  function selectEdit(edit: Edit, openFloatingPreview: boolean): void {
    selectedPreviewKey = getEditKey(edit);
    previewOpen = openFloatingPreview && !useDockedPreview;
  }

  function editTitle(edit: Edit): string {
    return edit.type === 'rename' && edit.oldPath
      ? `${edit.oldPath} → ${edit.filePath}`
      : edit.filePath;
  }

  function viewDiff(edit: Edit) {
    if (!isWebMode) {
      vscode.postMessage({
        type: 'viewDiff',
        filePath: edit.filePath,
        sessionId: getCurrentSessionId() || undefined,
        diff: edit.diff || '',
        originalContent: edit?.originalContent,
        previewContent: resolveEditPreviewContent(edit),
        previewAbsolutePath: edit?.previewAbsolutePath,
        previewCanOpenWorkspaceFile: edit?.previewCanOpenWorkspaceFile,
        contentKind: edit?.contentKind ?? 'text',
        size: edit?.size,
        mime: edit?.mime,
        error: edit?.error,
        symlinkTarget: edit?.symlinkTarget,
        headSummary: edit?.headSummary,
        tailSummary: edit?.tailSummary,
      });
      return;
    }
    selectEdit(edit, true);
  }

  function getEditKey(edit: Edit): string {
    return `${edit.filePath}::${edit.executionGroupId ?? 'none'}::${edit.snapshotId ?? 'na'}`;
  }

  function getPreviewLines(content: string): string[] {
    return content.split('\n');
  }

  function getDiffLineClass(line: string): string {
    if (line.startsWith('+++') || line.startsWith('---') || line.startsWith('@@')) {
      return 'meta';
    }
    if (line.startsWith('+')) return 'add';
    if (line.startsWith('-')) return 'del';
    return 'context';
  }

  function getDiffLinePrefix(line: string): string {
    const lineClass = getDiffLineClass(line);
    if (lineClass === 'add') return '+';
    if (lineClass === 'del') return '-';
    return '';
  }

  function getDiffLineText(line: string): string {
    const lineClass = getDiffLineClass(line);
    if (lineClass === 'add' || lineClass === 'del') {
      return line.slice(1) || ' ';
    }
    return line || ' ';
  }

  function resolveEditPreviewContent(edit: Edit | null): string {
    if (!edit) {
      return '';
    }
    if (typeof edit.previewContent === 'string' && edit.previewContent.length > 0) {
      return edit.previewContent;
    }
    if (typeof edit.originalContent === 'string' && edit.originalContent.length > 0) {
      return edit.originalContent;
    }
    return '';
  }

  function formatSize(size?: number): string {
    if (typeof size !== 'number' || !Number.isFinite(size) || size < 0) {
      return '-';
    }
    if (size < 1024) return `${size} B`;
    if (size < 1024 * 1024) return `${(size / 1024).toFixed(1)} KB`;
    if (size < 1024 * 1024 * 1024) return `${(size / (1024 * 1024)).toFixed(1)} MB`;
    return `${(size / (1024 * 1024 * 1024)).toFixed(1)} GB`;
  }

  function contentKindLabel(kind?: string): string {
    switch (kind) {
      case 'binary':
        return i18n.t('edits.kind.binary');
      case 'large_text':
        return i18n.t('edits.kind.largeText');
      case 'symlink':
        return i18n.t('edits.kind.symlink');
      case 'special':
        return i18n.t('edits.kind.special');
      default:
        return i18n.t('edits.kind.text');
    }
  }

  $effect(() => {
    if (edits.length === 0) {
      selectedPreviewKey = '';
      previewOpen = false;
      return;
    }
    const selectedStillExists = selectedPreviewKey
      ? edits.some((edit) => getEditKey(edit) === selectedPreviewKey)
      : false;
    if (selectedStillExists) {
      if (!useDockedPreview && selectedPreviewKey && !previewOpen) {
        previewOpen = true;
      }
      return;
    }
    if (useDockedPreview) {
      selectEdit(edits[0], false);
      return;
    }
    if (selectedPreviewKey || previewOpen) {
      selectedPreviewKey = '';
      previewOpen = false;
    }
  });
</script>

{#snippet fileRow(edit: Edit)}
  {@const { name } = splitPath(edit.filePath)}
  {@const oldName = edit.oldPath ? splitPath(edit.oldPath).name : ''}
  {@const kind = edit.contentKind ?? 'text'}
  {@const isText = kind === 'text'}
  <div
    class="file-row"
    class:selected={selectedPreviewKey === getEditKey(edit)}
    role="button"
    tabindex="0"
    onclick={() => viewDiff(edit)}
    onkeydown={(e) => e.key === 'Enter' && viewDiff(edit)}
    title={editTitle(edit)}
  >
    <div class="type-indicator" class:add={edit.type === 'add'} class:modify={edit.type === 'modify'} class:del={edit.type === 'delete'} class:rename={edit.type === 'rename'}></div>
    <div class="file-icon" class:add={edit.type === 'add'} class:modify={edit.type === 'modify'} class:del={edit.type === 'delete'} class:rename={edit.type === 'rename'}>
      <Icon name={getFileIconName(edit)} size={14} />
    </div>
    <div class="file-info">
      {#if edit.type === 'rename' && oldName}
        <span class="file-name rename-name"><span class="old-path">{oldName}</span><span class="rename-arrow">→</span><span>{name}</span></span>
      {:else}
        <span class="file-name">{name}</span>
      {/if}
      {#if !isText}
        <span class="file-kind-tag" title={contentKindLabel(kind)}>{contentKindLabel(kind)}</span>
      {/if}
      {#if edit.error}
        <span class="file-error-tag" title={edit.error}>{i18n.t('edits.row.error')}</span>
      {/if}
    </div>
    <div class="file-stats">
      {#if isText}
        <span class="stat-add">+{edit.additions ?? 0}</span>
        <span class="stat-del">-{edit.deletions ?? 0}</span>
      {:else}
        <span class="stat-meta">{formatSize(edit.size)}</span>
      {/if}
    </div>
    <div class="file-actions">
      <button class="action-icon approve" title={i18n.t('edits.actions.approveChange')} onclick={(e) => { e.stopPropagation(); approveChange(edit.filePath); }}>
        <Icon name="check" size={14} />
      </button>
      <button class="action-icon revert" title={i18n.t('edits.actions.revertChange')} onclick={(e) => { e.stopPropagation(); revertChange(edit.filePath); }}>
        <Icon name="undo" size={14} />
      </button>
    </div>
  </div>
{/snippet}

{#snippet diffContent()}
  <div class="diff-reader-header">
    <div class="diff-file-title" title={previewDisplayTitle}>{selectedPreviewEdit?.type === 'rename' && selectedPreviewEdit.oldPath ? previewDisplayTitle : splitPath(previewTitle).name}</div>
    {#if selectedPreviewEdit}
      <div class="diff-file-stats" aria-label="变更行数">
        {#if previewHasDiff}
          <span class="stat-add">+{selectedPreviewEdit.additions ?? 0}</span>
          <span class="stat-del">-{selectedPreviewEdit.deletions ?? 0}</span>
        {:else}
          <span class="stat-meta">{contentKindLabel(previewContentKind)}</span>
          <span class="stat-meta">{formatSize(selectedPreviewEdit.size)}</span>
        {/if}
      </div>
    {/if}
  </div>
  <div class="diff-reader-body">
    {#if previewError}
      <div class="preview-empty error">{previewError}</div>
    {:else if previewContentKind === 'binary'}
      <div class="preview-non-text" role="note" aria-live="polite">
        <div class="preview-non-text-title">{i18n.t('edits.nonText.binaryTitle')}</div>
        <div class="preview-non-text-meta">
          <span>{i18n.t('edits.nonText.size')}: {formatSize(selectedPreviewEdit?.size)}</span>
          {#if selectedPreviewEdit?.mime}
            <span>{i18n.t('edits.nonText.mime')}: {selectedPreviewEdit.mime}</span>
          {/if}
        </div>
        <div class="preview-non-text-hint">{i18n.t('edits.nonText.binaryHint')}</div>
      </div>
    {:else if previewContentKind === 'large_text'}
      <div class="preview-non-text" role="note" aria-live="polite">
        <div class="preview-non-text-title">{i18n.t('edits.nonText.largeTextTitle')}</div>
        <div class="preview-non-text-meta">
          <span>{i18n.t('edits.nonText.size')}: {formatSize(selectedPreviewEdit?.size)}</span>
        </div>
        {#if selectedPreviewEdit?.headSummary}
          <div class="preview-non-text-section">
            <div class="preview-non-text-section-label">{i18n.t('edits.nonText.head')}</div>
            <pre class="preview-non-text-snippet">{selectedPreviewEdit.headSummary}</pre>
          </div>
        {/if}
        {#if selectedPreviewEdit?.tailSummary}
          <div class="preview-non-text-section">
            <div class="preview-non-text-section-label">{i18n.t('edits.nonText.tail')}</div>
            <pre class="preview-non-text-snippet">{selectedPreviewEdit.tailSummary}</pre>
          </div>
        {/if}
      </div>
    {:else if previewContentKind === 'symlink'}
      <div class="preview-non-text" role="note" aria-live="polite">
        <div class="preview-non-text-title">{i18n.t('edits.nonText.symlinkTitle')}</div>
        <div class="preview-non-text-meta">
          <span>{i18n.t('edits.nonText.target')}: {selectedPreviewEdit?.symlinkTarget ?? '-'}</span>
        </div>
      </div>
    {:else if previewContentKind === 'special'}
      <div class="preview-non-text" role="note" aria-live="polite">
        <div class="preview-non-text-title">{i18n.t('edits.nonText.specialTitle')}</div>
        <div class="preview-non-text-hint">{i18n.t('edits.nonText.specialHint')}</div>
      </div>
    {:else if !previewContent}
      <div class="preview-empty">{i18n.t('edits.preview.empty')}</div>
    {:else}
      <div class="preview-diff">
        {#each getPreviewLines(previewContent) as line, index}
          <div class="preview-diff-line {getDiffLineClass(line)}">
            <span class="preview-line-number">{index + 1}</span>
            <span class="preview-line-prefix" aria-hidden="true">{getDiffLinePrefix(line)}</span>
            <code>{getDiffLineText(line)}</code>
          </div>
        {/each}
      </div>
    {/if}
  </div>
{/snippet}

<div class="panel-content-scrollable edits-panel" class:web-mode={isWebMode} class:compact-web={isWebMode && isCompactViewport}>
  {#if edits.length === 0}
    <div class="empty-state">
      <Icon name="file-edit" size={32} />
      <div class="empty-text">{i18n.t('edits.empty.title')}</div>
      <div class="empty-hint">{i18n.t('edits.empty.hint')}</div>
    </div>
  {:else if !useDockedPreview && previewOpen && selectedPreviewEdit}
    <section class="compact-preview" aria-label={previewTitle}>
      <div class="compact-preview-nav">
        <button class="compact-back" type="button" onclick={closePreview}>
          <Icon name="chevron-right" size={15} class="back-chevron" />
          <span>{i18n.t('topTabs.edits')}</span>
        </button>
      </div>
      <div class="diff-reader compact-reader active">
        {@render diffContent()}
      </div>
    </section>
  {:else}
    <div class="edits-shell" class:has-docked-preview={useDockedPreview}>
      <div class="edits-main">
        {#if edits.length >= 2}
          <div class="edits-toolbar">
            <button
              type="button"
              class="toolbar-btn approve"
              disabled={!!pendingBatch}
              title={i18n.t('edits.actions.approveAllTitle')}
              onclick={approveAllChanges}
            >
              <Icon name="check" size={13} />
              <span>{i18n.t('edits.actions.approveAll')}</span>
            </button>
            <button
              type="button"
              class="toolbar-btn revert"
              disabled={!!pendingBatch}
              title={i18n.t('edits.actions.revertAllTitle')}
              onclick={revertAllChanges}
            >
              <Icon name="undo" size={13} />
              <span>{i18n.t('edits.actions.revertAll')}</span>
            </button>
          </div>
        {/if}

        {#if hasGroups}
          <div class="group-section">
            <div class="group-header">
              <span class="group-label">{i18n.t('edits.group.staged')}</span>
              <span class="group-count">{i18n.t('edits.group.stagedCount', { count: stagedEdits.length })}</span>
            </div>
            <div class="file-list">
              {#each stagedEdits as edit (getEditKey(edit))}
                {@render fileRow(edit)}
              {/each}
            </div>
          </div>
        {/if}

        <div class="group-section">
          {#if hasGroups || currentRoundEdits.length > 0}
            <div class="group-header current-round">
              <span class="group-label">{i18n.t('edits.group.currentRound')}</span>
              <span class="group-count">{i18n.t('edits.group.currentRoundCount', { count: currentRoundEdits.length })}</span>
              {#if currentRoundEdits.length > 0 && latestExecutionGroupId}
                <button
                  type="button"
                  class="group-action"
                  disabled={!!pendingBatch}
                  title={i18n.t('edits.group.revertRoundTitle')}
                  onclick={revertCurrentRound}
                >
                  <Icon name="undo" size={12} />
                  <span>{i18n.t('edits.group.revertRound')}</span>
                </button>
              {/if}
            </div>
            <div class="file-list">
              {#each currentRoundEdits as edit (getEditKey(edit))}
                {@render fileRow(edit)}
              {/each}
            </div>
          {:else}
            <div class="file-list">
              {#each edits as edit (getEditKey(edit))}
                {@render fileRow(edit)}
              {/each}
            </div>
          {/if}
        </div>
      </div>
      {#if useDockedPreview}
        <aside class="diff-reader" class:active={!!selectedPreviewEdit}>
          {#if selectedPreviewEdit}
            {@render diffContent()}
          {:else}
            <div class="preview-empty state-hint">{i18n.t('edits.preview.selectFile')}</div>
          {/if}
        </aside>
      {/if}
    </div>
  {/if}
</div>

<style>
  .edits-panel {
    --edits-card-bg: color-mix(in srgb, var(--surface-1) 88%, var(--background));
    --edits-card-border: color-mix(in srgb, var(--border-subtle) 82%, transparent);
    --edits-card-shadow: 0 1px 0 color-mix(in srgb, var(--foreground) 4%, transparent);
    --edits-row-bg: color-mix(in srgb, var(--background) 62%, var(--surface-1));
    --edits-row-bg-hover: color-mix(in srgb, var(--surface-hover) 86%, var(--surface-1));
    --edits-row-border: color-mix(in srgb, var(--border-subtle) 76%, transparent);
    --edits-line-number-bg: color-mix(in srgb, var(--surface-2) 54%, transparent);
    --diff-add-line-bg: #dafbe1;
    --diff-add-gutter-bg: #aceebb;
    --diff-add-accent: #1a7f37;
    --diff-del-line-bg: #ffebe9;
    --diff-del-gutter-bg: #ffd7d5;
    --diff-del-accent: #cf222e;
    --diff-meta-line-bg: rgba(84, 112, 153, 0.08);
    --diff-meta-gutter-bg: rgba(84, 112, 153, 0.12);
    --diff-meta-fg: #57606a;
    --diff-line-number-width: 36px;
    --diff-prefix-width: 16px;
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
    overflow: hidden;
    padding: var(--space-2);
    background: transparent;
  }

  .edits-panel * {
    box-sizing: border-box;
  }

  :global(.theme-light) .edits-panel,
  :global(body.vscode-light) .edits-panel,
  :global(:root.theme-light) .edits-panel {
    --edits-card-bg: #f6f8fa;
    --edits-card-border: #d7dce5;
    --edits-card-shadow:
      0 1px 2px rgba(15, 23, 42, 0.06),
      0 4px 12px rgba(15, 23, 42, 0.04);
    --edits-row-bg: #f9fafb;
    --edits-row-bg-hover: #eef1f5;
    --edits-row-border: #e2e6ed;
    --edits-line-number-bg: #eef1f5;
    --diff-add-line-bg: #dafbe1;
    --diff-add-gutter-bg: #aceebb;
    --diff-add-accent: #1a7f37;
    --diff-del-line-bg: #ffebe9;
    --diff-del-gutter-bg: #ffd7d5;
    --diff-del-accent: #cf222e;
    --diff-meta-line-bg: rgba(84, 112, 153, 0.08);
    --diff-meta-gutter-bg: rgba(84, 112, 153, 0.12);
    --diff-meta-fg: #57606a;
  }

  :global(.theme-dark) .edits-panel,
  :global(body.vscode-dark) .edits-panel,
  :global(:root.theme-dark) .edits-panel {
    --diff-add-line-bg: color-mix(in srgb, #52b87a 20%, var(--background));
    --diff-add-gutter-bg: color-mix(in srgb, #52b87a 30%, var(--background));
    --diff-add-accent: #52b87a;
    --diff-del-line-bg: color-mix(in srgb, #f6806d 18%, var(--background));
    --diff-del-gutter-bg: color-mix(in srgb, #f6806d 28%, var(--background));
    --diff-del-accent: #f6806d;
    --diff-meta-line-bg: color-mix(in srgb, var(--foreground) 5%, var(--background));
    --diff-meta-gutter-bg: color-mix(in srgb, var(--foreground) 8%, var(--background));
    --diff-meta-fg: color-mix(in srgb, var(--foreground) 54%, var(--foreground-muted));
  }

  .empty-state {
    display: flex;
    flex: 1;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: var(--space-2);
    width: 100%;
    min-height: 0;
    box-sizing: border-box;
    padding: var(--space-8) var(--space-5);
    color: var(--foreground-muted);
    text-align: center;
  }

  .empty-text {
    color: var(--foreground);
    font-size: var(--text-base);
    font-weight: var(--font-medium);
  }

  .empty-hint {
    font-size: var(--text-sm);
    opacity: 0.6;
  }

  .edits-shell,
  .edits-shell.has-docked-preview {
    display: grid;
    grid-template-columns: minmax(220px, 340px) minmax(0, 1fr);
    gap: 0;
    width: 100%;
    height: 100%;
    min-height: 0;
    overflow: hidden;
    border: 1px solid var(--edits-card-border);
    border-radius: var(--radius-lg);
    background: var(--edits-card-bg);
    box-shadow: var(--edits-card-shadow);
  }

  .compact-preview {
    display: flex;
    flex: 1 1 auto;
    flex-direction: column;
    width: 100%;
    height: 100%;
    min-width: 0;
    min-height: 0;
    overflow: hidden;
    border: 1px solid var(--edits-card-border);
    border-radius: var(--radius-lg);
    background: var(--edits-card-bg);
    box-shadow: var(--edits-card-shadow);
  }

  .compact-preview-nav {
    display: flex;
    flex: 0 0 auto;
    align-items: center;
    min-height: 40px;
    padding: 0 var(--space-2);
    border-bottom: 1px solid var(--edits-card-border);
    background: color-mix(in srgb, var(--surface-1) 72%, var(--background));
  }

  .compact-back {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    min-height: 28px;
    padding: 0 8px 0 6px;
    border: 1px solid transparent;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    font-size: var(--text-xs);
    font-weight: var(--font-medium);
  }

  .compact-back:hover {
    border-color: var(--edits-card-border);
    background: var(--surface-hover);
    color: var(--foreground);
  }

  .compact-back :global(.back-chevron) {
    transform: rotate(180deg);
  }

  .compact-reader {
    flex: 1 1 auto;
    min-width: 0;
    min-height: 0;
    height: auto;
  }

  .edits-main {
    min-width: 0;
    min-height: 0;
    overflow: auto;
    padding: var(--space-2);
    border-right: 1px solid var(--edits-card-border);
    background: color-mix(in srgb, var(--surface-1) 52%, var(--background));
  }

  .edits-main::-webkit-scrollbar,
  .diff-reader-body::-webkit-scrollbar {
    width: 10px;
    height: 10px;
  }

  .edits-main::-webkit-scrollbar-track,
  .diff-reader-body::-webkit-scrollbar-track {
    background: transparent;
  }

  .edits-main::-webkit-scrollbar-thumb,
  .diff-reader-body::-webkit-scrollbar-thumb {
    border: 2px solid transparent;
    border-radius: var(--radius-full);
    background: var(--scrollbar-thumb);
    background-clip: padding-box;
  }

  .edits-toolbar {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
    margin: 0 0 var(--space-2);
    padding: var(--space-2);
    border: 1px solid var(--edits-row-border);
    border-radius: var(--radius-md);
    background: var(--edits-row-bg);
  }

  .toolbar-btn {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 28px;
    padding: 0 10px;
    border: 1px solid var(--edits-card-border);
    border-radius: var(--radius-sm);
    background: var(--edits-card-bg);
    color: var(--foreground);
    cursor: pointer;
    font-size: var(--text-xs);
    font-weight: var(--font-medium);
    transition: background var(--transition-fast), color var(--transition-fast), border-color var(--transition-fast);
  }

  .toolbar-btn:hover:not(:disabled) {
    background: var(--surface-hover);
  }

  .toolbar-btn:disabled {
    cursor: not-allowed;
    opacity: 0.55;
  }

  .toolbar-btn.approve:hover:not(:disabled) {
    border-color: color-mix(in srgb, var(--success) 50%, var(--edits-card-border));
    color: var(--success);
  }

  .toolbar-btn.revert:hover:not(:disabled) {
    border-color: color-mix(in srgb, var(--error) 50%, var(--edits-card-border));
    color: var(--error);
  }

  .group-action {
    margin-left: auto;
    display: inline-flex;
    align-items: center;
    gap: 4px;
    height: 22px;
    padding: 0 8px;
    border: 1px solid transparent;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    font-size: var(--text-2xs);
    font-weight: var(--font-medium);
    transition: background var(--transition-fast), color var(--transition-fast), border-color var(--transition-fast);
  }

  .group-action:hover:not(:disabled) {
    border-color: color-mix(in srgb, var(--error) 40%, var(--edits-card-border));
    background: var(--surface-hover);
    color: var(--error);
  }

  .group-action:disabled {
    cursor: not-allowed;
    opacity: 0.5;
  }

  .group-section {
    margin: 0 0 var(--space-2);
  }

  .group-section:last-child {
    margin-bottom: 0;
  }

  .group-header {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-height: 28px;
    margin: 0 0 4px;
    padding: 0 var(--space-1);
  }

  .group-label {
    color: var(--foreground-muted);
    font-size: var(--text-2xs);
    font-weight: var(--font-semibold);
  }

  .group-count {
    color: var(--foreground-muted);
    font-size: var(--text-2xs);
    opacity: 0.72;
  }

  .file-list {
    display: flex;
    flex-direction: column;
    gap: 1px;
    overflow: hidden;
    border: 1px solid var(--edits-row-border);
    border-radius: var(--radius-md);
    background: var(--edits-row-border);
  }

  .file-row,
  .edits-panel.web-mode .file-row {
    display: grid;
    grid-template-columns: 3px 20px minmax(0, 1fr) auto auto;
    grid-template-areas: "indicator icon info stats actions";
    align-items: center;
    gap: var(--space-2);
    min-height: 42px;
    padding: 0 var(--space-2);
    background: var(--edits-row-bg);
    cursor: pointer;
    transition: background var(--transition-fast), box-shadow var(--transition-fast);
  }

  .file-row:hover {
    background: var(--edits-row-bg-hover);
  }

  .file-row.selected {
    background: color-mix(in srgb, var(--info-muted) 20%, var(--edits-row-bg));
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--info) 28%, transparent);
  }

  .type-indicator {
    grid-area: indicator;
    width: 3px;
    height: 22px;
    border-radius: 2px;
    background: var(--foreground-muted);
    opacity: 0.28;
  }

  .type-indicator.add { background: var(--success); opacity: 1; }
  .type-indicator.modify { background: var(--warning); opacity: 1; }
  .type-indicator.del { background: var(--error); opacity: 1; }
  .type-indicator.rename { background: var(--info); opacity: 1; }

  .file-icon {
    grid-area: icon;
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--foreground-muted);
  }

  .file-icon.add { color: var(--success); }
  .file-icon.modify { color: var(--warning); }
  .file-icon.del { color: var(--error); }
  .file-icon.rename { color: var(--info); }

  .file-info {
    grid-area: info;
    min-width: 0;
    overflow: hidden;
  }

  .file-name {
    display: block;
    overflow: hidden;
    color: var(--foreground);
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .rename-name {
    display: flex;
    align-items: center;
    gap: 4px;
  }

  .old-path,
  .rename-arrow {
    color: var(--foreground-muted);
    font-weight: var(--font-normal);
  }

  .file-stats {
    grid-area: stats;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    font-weight: var(--font-semibold);
  }

  .stat-add { color: var(--success); }
  .stat-del { color: var(--error); }
  .stat-meta {
    color: var(--foreground-muted);
    font-weight: var(--font-medium);
  }

  .file-kind-tag,
  .file-error-tag {
    display: inline-flex;
    align-items: center;
    margin-left: 6px;
    padding: 1px 6px;
    border: 1px solid var(--edits-card-border);
    border-radius: var(--radius-sm);
    background: color-mix(in srgb, var(--surface-2) 60%, transparent);
    color: var(--foreground-muted);
    font-size: var(--text-2xs);
    font-weight: var(--font-medium);
    line-height: 1.4;
    vertical-align: middle;
  }

  .file-error-tag {
    background: color-mix(in srgb, var(--error) 18%, transparent);
    color: var(--error);
    border-color: color-mix(in srgb, var(--error) 35%, var(--edits-card-border));
  }

  .preview-non-text {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    padding: var(--space-5);
    color: var(--foreground);
  }

  .preview-non-text-title {
    font-size: var(--text-base);
    font-weight: var(--font-semibold);
  }

  .preview-non-text-meta {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-3);
    color: var(--foreground-muted);
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
  }

  .preview-non-text-hint {
    color: var(--foreground-muted);
    font-size: var(--text-sm);
    line-height: 1.6;
  }

  .preview-non-text-section {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .preview-non-text-section-label {
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    font-weight: var(--font-semibold);
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }

  .preview-non-text-snippet {
    margin: 0;
    padding: var(--space-3);
    border: 1px solid var(--edits-card-border);
    border-radius: var(--radius-sm);
    background: color-mix(in srgb, var(--background) 92%, var(--surface-1));
    color: var(--foreground);
    font-family: var(--font-mono);
    font-size: 12px;
    line-height: 1.55;
    max-height: 240px;
    overflow: auto;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  .file-actions {
    grid-area: actions;
    display: inline-flex;
    gap: 2px;
    opacity: 0;
    transition: opacity var(--transition-fast);
  }

  .file-row:hover .file-actions,
  .file-row:focus-within .file-actions {
    opacity: 1;
  }

  .action-icon {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 24px;
    height: 24px;
    border: 1px solid transparent;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    transition: background var(--transition-fast), color var(--transition-fast), border-color var(--transition-fast);
  }

  .action-icon:hover {
    border-color: var(--edits-card-border);
    background: var(--surface-hover);
    color: var(--foreground);
  }

  .action-icon.approve:hover { color: var(--success); }
  .action-icon.revert:hover { color: var(--error); }

  .diff-reader {
    position: relative;
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
    height: 100%;
    overflow: hidden;
    background: color-mix(in srgb, var(--background) 92%, var(--surface-1));
  }

  .diff-reader:not(.active) {
    align-items: center;
    justify-content: center;
  }

  .diff-reader-header {
    display: flex;
    flex: 0 0 auto;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
    min-height: 44px;
    min-width: 0;
    padding: 0 var(--space-3);
    border-bottom: 1px solid var(--edits-card-border);
    background: color-mix(in srgb, var(--surface-1) 72%, var(--background));
  }

  .diff-file-title {
    min-width: 0;
    overflow: hidden;
    color: var(--foreground);
    font-size: var(--text-sm);
    font-weight: var(--font-semibold);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .diff-file-stats {
    display: inline-flex;
    flex: 0 0 auto;
    align-items: center;
    gap: var(--space-2);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    font-weight: var(--font-semibold);
  }

  .diff-reader-body {
    flex: 1 1 auto;
    min-width: 0;
    min-height: 0;
    overflow: auto;
    background: color-mix(in srgb, var(--background) 94%, var(--surface-1));
    scrollbar-width: thin;
    scrollbar-color: var(--scrollbar-thumb) transparent;
  }

  .preview-diff {
    width: 100%;
    min-width: 0;
    min-height: 100%;
    background: transparent;
  }

  .preview-diff-line {
    display: grid;
    grid-template-columns: var(--diff-line-number-width) var(--diff-prefix-width) minmax(0, 1fr);
    align-items: start;
    min-width: 0;
    width: 100%;
    font-family: var(--font-mono);
    font-size: 12px;
    line-height: 1.55;
  }

  .preview-line-number {
    position: sticky;
    left: 0;
    z-index: 1;
    min-width: 0;
    padding: 2px 8px;
    background: var(--edits-line-number-bg);
    color: var(--foreground-muted);
    font-variant-numeric: tabular-nums;
    text-align: right;
    user-select: none;
  }

  .preview-line-prefix {
    position: sticky;
    left: var(--diff-line-number-width);
    z-index: 1;
    min-width: 0;
    padding: 2px 0;
    border-right: 1px solid var(--edits-card-border);
    background: var(--edits-line-number-bg);
    color: transparent;
    font-weight: var(--font-semibold);
    text-align: center;
    user-select: none;
  }

  .preview-diff-line code {
    display: block;
    min-width: 0;
    margin: 0;
    padding: 2px 12px 2px 6px;
    background: transparent !important;
    border: none !important;
    box-shadow: none !important;
    color: var(--foreground);
    overflow-wrap: anywhere;
    tab-size: 4;
    white-space: pre-wrap;
  }

  .preview-diff-line.meta {
    background: var(--diff-meta-line-bg);
  }

  .preview-diff-line.meta .preview-line-number,
  .preview-diff-line.meta .preview-line-prefix {
    background: var(--diff-meta-gutter-bg);
    color: var(--diff-meta-fg);
  }

  .preview-diff-line.meta code {
    color: var(--diff-meta-fg);
  }

  .preview-diff-line.add {
    background: var(--diff-add-line-bg);
  }

  .preview-diff-line.add .preview-line-number,
  .preview-diff-line.add .preview-line-prefix {
    background: var(--diff-add-gutter-bg);
    color: var(--diff-add-accent);
  }

  .preview-diff-line.add .preview-line-number {
    box-shadow: inset 2px 0 0 var(--diff-add-accent);
  }

  .preview-diff-line.del {
    background: var(--diff-del-line-bg);
  }

  .preview-diff-line.del .preview-line-number,
  .preview-diff-line.del .preview-line-prefix {
    background: var(--diff-del-gutter-bg);
    color: var(--diff-del-accent);
  }

  .preview-diff-line.del .preview-line-number {
    box-shadow: inset 2px 0 0 var(--diff-del-accent);
  }

  .preview-empty {
    padding: var(--space-8) var(--space-4);
    color: var(--foreground-muted);
    text-align: center;
  }

  .preview-empty.state-hint {
    max-width: 240px;
    font-size: var(--text-sm);
    line-height: 1.6;
  }

  .preview-empty.error {
    color: var(--error);
  }

  @media (hover: none) {
    .file-actions {
      opacity: 1;
    }
  }

  @media (max-width: 1120px) {
    .compact-web {
      padding: var(--space-2);
    }

    .edits-shell,
    .edits-shell.has-docked-preview {
      display: block;
      border: none;
      border-radius: 0;
      background: transparent;
      box-shadow: none;
    }

    .edits-main {
      height: 100%;
      border-right: none;
      background: transparent;
    }

    .compact-preview .preview-diff {
      width: 100%;
      min-width: 100%;
    }

    .compact-preview .diff-reader-header {
      min-height: auto;
      padding: 8px var(--space-2);
    }

    .compact-preview .diff-file-title {
      white-space: normal;
      overflow-wrap: anywhere;
      line-height: 1.35;
    }

    .compact-preview .preview-diff-line {
      width: 100%;
      min-width: 0;
    }

    .compact-preview .preview-line-number,
    .compact-preview .preview-line-prefix {
      position: static;
      left: auto;
    }

    .compact-preview .preview-diff-line code {
      flex: 1 1 0;
      min-width: 0;
      white-space: pre-wrap;
      overflow-wrap: anywhere;
      word-break: break-word;
    }
  }

  @media (max-width: 768px) {
    .edits-panel {
      padding: var(--space-2);
      --diff-line-number-width: 30px;
      --diff-prefix-width: 14px;
    }

    .file-row,
    .edits-panel.web-mode .file-row {
      grid-template-columns: 3px 18px minmax(0, 1fr) auto auto;
      gap: 6px;
      min-height: 44px;
      padding-inline: 6px;
    }

    .preview-diff-line {
      font-size: 11.5px;
    }

    .preview-line-number {
      padding-inline: 4px;
    }
  }
</style>
