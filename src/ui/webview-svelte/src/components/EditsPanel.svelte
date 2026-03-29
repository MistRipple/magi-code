<script lang="ts">
  import { onMount } from 'svelte';
  import { getCurrentSessionId, getState } from '../stores/messages.svelte';
  import { vscode } from '../lib/vscode-bridge';
  import { ensureArray } from '../lib/utils';
  import type { Edit } from '../types/message';
  import Icon from './Icon.svelte';
  import WorkerBadge from './WorkerBadge.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { isWebAgentMode } from '../web/agent-api';

  const appState = getState();
  const isWebMode = isWebAgentMode();

  const edits = $derived(ensureArray(appState.edits) as Edit[]);
  let isCompactViewport = $state(false);
  let previewOpen = $state(false);
  let previewMode = $state<'diff' | 'file'>('diff');
  let previewTitle = $state('');
  let previewContent = $state('');
  let previewLanguage = $state('text');
  let previewLoading = $state(false);
  let previewError = $state('');
  let selectedPreviewFilePath = $state('');
  const useDockedPreview = $derived(isWebMode && !isCompactViewport);
  const selectedPreviewEdit = $derived(
    selectedPreviewFilePath
      ? edits.find((edit) => edit.filePath === selectedPreviewFilePath) ?? null
      : null
  );

  onMount(() => {
    if (!isWebMode || typeof window === 'undefined') {
      return;
    }
    const media = window.matchMedia('(max-width: 960px)');
    const updateViewport = () => {
      isCompactViewport = media.matches;
    };
    updateViewport();
    media.addEventListener('change', updateViewport);
    return () => media.removeEventListener('change', updateViewport);
  });

  // 统计汇总
  const totalAdditions = $derived(edits.reduce((s, e) => s + (e.additions ?? 0), 0));
  const totalDeletions = $derived(edits.reduce((s, e) => s + (e.deletions ?? 0), 0));
  const addedCount = $derived(edits.filter(e => e.type === 'add').length);
  const modifiedCount = $derived(edits.filter(e => e.type === 'modify').length);
  const deletedCount = $derived(edits.filter(e => e.type === 'delete').length);

  // ─── 按轮次（missionId）分组 ───
  // 最新轮次 missionId：取 edits 列表中最后一个有 missionId 的值（后端已按 timestamp 排序）
  const latestMissionId = $derived.by(() => {
    if (edits.length === 0) return null;
    for (let i = edits.length - 1; i >= 0; i--) {
      if (edits[i].missionId) return edits[i].missionId!;
    }
    return null;
  });

  // 本轮变更
  const currentRoundEdits = $derived(
    latestMissionId ? edits.filter(e => e.missionId === latestMissionId) : []
  );

  // 统一暂存（非本轮）
  const stagedEdits = $derived(
    latestMissionId ? edits.filter(e => e.missionId !== latestMissionId) : edits
  );

  // 是否有两组分组（只有同时存在统一暂存和本轮变更才分组显示）
  const hasGroups = $derived(stagedEdits.length > 0 && currentRoundEdits.length > 0);

  function getContributors(edit: Edit): string[] {
    if (Array.isArray(edit?.contributors) && edit.contributors.length > 0) return edit.contributors;
    if (edit?.workerId) return [edit.workerId];
    return [];
  }

  // 拆分文件名和目录
  function splitPath(filePath: string): { dir: string; name: string } {
    const lastSlash = filePath.lastIndexOf('/');
    if (lastSlash === -1) return { dir: '', name: filePath };
    return { dir: filePath.substring(0, lastSlash + 1), name: filePath.substring(lastSlash + 1) };
  }

  // 文件类型图标名
  function getFileIconName(edit: Edit): 'file-plus' | 'file-minus' | 'file-edit' | 'file-text' {
    if (edit?.type === 'add') return 'file-plus';
    if (edit?.type === 'delete') return 'file-minus';
    if (edit?.type === 'modify') return 'file-edit';
    return 'file-text';
  }

  // 增删比例条（5 格小方块，类似 GitHub）
  function getChangeBlocks(additions: number, deletions: number): ('add' | 'del' | 'neutral')[] {
    const total = additions + deletions;
    if (total === 0) return ['neutral', 'neutral', 'neutral', 'neutral', 'neutral'];
    const addBlocks = Math.round((additions / total) * 5);
    const delBlocks = 5 - addBlocks;
    return [
      ...Array(addBlocks).fill('add') as 'add'[],
      ...Array(delBlocks).fill('del') as 'del'[],
    ];
  }

  function closePreview(): void {
    previewOpen = false;
    previewError = '';
    previewLoading = false;
    if (useDockedPreview) {
      selectedPreviewFilePath = '';
      previewTitle = '';
      previewContent = '';
    }
  }

  async function openFile(filePath: string) {
    const edit = edits.find((candidate) => candidate.filePath === filePath) ?? null;
    const resolvedPreviewContent = resolveEditPreviewContent(edit);
    const resolvedPreviewLanguage = inferPreviewLanguage(filePath);

    if (!isWebMode) {
      vscode.postMessage({
        type: 'openFile',
        filepath: filePath,
        sessionId: getCurrentSessionId() || undefined,
        previewContent: resolvedPreviewContent,
        previewAbsolutePath: edit?.previewAbsolutePath,
        previewCanOpenWorkspaceFile: edit?.previewCanOpenWorkspaceFile,
      });
      return;
    }
    selectedPreviewFilePath = filePath;
    previewOpen = !useDockedPreview;
    previewMode = 'file';
    previewTitle = filePath;
    previewContent = resolvedPreviewContent;
    previewLanguage = resolvedPreviewLanguage;
    previewError = '';
    previewLoading = false;
    if (!resolvedPreviewContent) {
      previewError = i18n.t('edits.preview.empty');
    }
  }
  function approveChange(filePath: string) { vscode.postMessage({ type: 'approveChange', filePath }); }
  function revertChange(filePath: string) { vscode.postMessage({ type: 'revertChange', filePath }); }
  async function viewDiff(filePath: string) {
    const edit = edits.find((candidate) => candidate.filePath === filePath) ?? null;
    const diff = edit?.diff || '';

    if (!isWebMode) {
      vscode.postMessage({
        type: 'viewDiff',
        filePath,
        sessionId: getCurrentSessionId() || undefined,
        diff,
        originalContent: edit?.originalContent,
        previewContent: resolveEditPreviewContent(edit),
        previewAbsolutePath: edit?.previewAbsolutePath,
        previewCanOpenWorkspaceFile: edit?.previewCanOpenWorkspaceFile,
      });
      return;
    }
    selectedPreviewFilePath = filePath;
    previewOpen = !useDockedPreview;
    previewMode = 'diff';
    previewTitle = filePath;
    previewContent = diff;
    previewLanguage = 'diff';
    previewError = '';
    previewLoading = false;
    if (!diff) {
      previewError = i18n.t('fileChangeCard.noDiffDetail');
    }
  }
  function approveAllChanges() { vscode.postMessage({ type: 'approveAllChanges' }); }
  function revertAllChanges() { vscode.postMessage({ type: 'revertAllChanges' }); }
  function revertMission() {
    if (!latestMissionId) return;
    vscode.postMessage({ type: 'revertMission', missionId: latestMissionId });
  }

  function getEditKey(edit: Edit): string {
    return `${edit.filePath}::${edit.missionId ?? 'none'}::${edit.snapshotId ?? 'na'}`;
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

  function inferPreviewLanguage(filePath: string): string {
    const ext = filePath.split('.').pop()?.toLowerCase() || '';
    const languageMap: Record<string, string> = {
      ts: 'typescript',
      tsx: 'typescript',
      js: 'javascript',
      jsx: 'javascript',
      json: 'json',
      md: 'markdown',
      css: 'css',
      scss: 'scss',
      html: 'html',
      vue: 'vue',
      svelte: 'svelte',
      py: 'python',
      go: 'go',
      java: 'java',
      sh: 'bash',
      yml: 'yaml',
      yaml: 'yaml',
    };
    return languageMap[ext] || 'text';
  }
</script>

{#snippet fileRow(edit: Edit)}
  {@const { dir, name } = splitPath(edit.filePath)}
  {@const blocks = getChangeBlocks(edit.additions ?? 0, edit.deletions ?? 0)}
  {@const contributors = getContributors(edit)}
  <div
    class="file-row"
    class:selected={selectedPreviewFilePath === edit.filePath}
    role="button"
    tabindex="0"
    onclick={() => viewDiff(edit.filePath)}
    onkeydown={(e) => e.key === 'Enter' && viewDiff(edit.filePath)}
  >
    <div class="type-indicator" class:add={edit.type === 'add'} class:modify={edit.type === 'modify'} class:del={edit.type === 'delete'}></div>
    <div class="file-icon" class:add={edit.type === 'add'} class:modify={edit.type === 'modify'} class:del={edit.type === 'delete'}>
      <Icon name={getFileIconName(edit)} size={14} />
    </div>
    <div class="file-info">
      <span class="file-name">{name}</span>
      {#if dir}<span class="file-dir">{dir}</span>{/if}
    </div>
    {#if contributors.length > 0}
      <div class="file-workers">
        {#each contributors as worker}
          <WorkerBadge {worker} size="sm" />
        {/each}
      </div>
    {/if}
    <div class="file-stats">
      {#if edit.additions}<span class="stat-add">+{edit.additions}</span>{/if}
      {#if edit.deletions}<span class="stat-del">-{edit.deletions}</span>{/if}
      <div class="change-blocks">
        {#each blocks as block}
          <span class="block" class:add={block === 'add'} class:del={block === 'del'} class:neutral={block === 'neutral'}></span>
        {/each}
      </div>
    </div>
    <div class="file-actions">
      <button class="action-icon" title={i18n.t('edits.actions.openFile')} onclick={(e) => { e.stopPropagation(); openFile(edit.filePath); }}>
        <Icon name="file-text" size={14} />
      </button>
      <button class="action-icon approve" title={i18n.t('edits.actions.approveChange')} onclick={(e) => { e.stopPropagation(); approveChange(edit.filePath); }}>
        <Icon name="check" size={14} />
      </button>
      <button class="action-icon revert" title={i18n.t('edits.actions.revertChange')} onclick={(e) => { e.stopPropagation(); revertChange(edit.filePath); }}>
        <Icon name="undo" size={14} />
      </button>
    </div>
  </div>
{/snippet}

<div class="edits-panel" class:web-mode={isWebMode} class:compact-web={isWebMode && isCompactViewport}>
  {#if edits.length === 0}
    <div class="empty-state">
      <Icon name="file-edit" size={32} />
      <div class="empty-text">{i18n.t('edits.empty.title')}</div>
      <div class="empty-hint">{i18n.t('edits.empty.hint')}</div>
    </div>
  {:else}
    <div class="edits-shell" class:has-docked-preview={useDockedPreview}>
      <div class="edits-main">
        <!-- 顶部统计条 -->
        <div class="summary-bar">
          <div class="summary-left">
            <span class="summary-count">{i18n.t('edits.summary.fileCount', { count: edits.length })}</span>
            {#if addedCount > 0}<span class="summary-chip add">{i18n.t('edits.summary.added', { count: addedCount })}</span>{/if}
            {#if modifiedCount > 0}<span class="summary-chip modify">{i18n.t('edits.summary.modified', { count: modifiedCount })}</span>{/if}
            {#if deletedCount > 0}<span class="summary-chip del">{i18n.t('edits.summary.deleted', { count: deletedCount })}</span>{/if}
          </div>
          <div class="summary-right">
            <span class="stat-add">+{totalAdditions}</span>
            <span class="stat-del">-{totalDeletions}</span>
          </div>
        </div>

        <!-- 批量操作 -->
        <div class="bulk-actions">
          <button class="bulk-btn approve" onclick={approveAllChanges} title={i18n.t('edits.actions.approveAllTitle')}>
            <Icon name="check-circle" size={13} />
            <span>{i18n.t('edits.actions.approveAll')}</span>
          </button>
          <button class="bulk-btn revert" onclick={revertAllChanges} title={i18n.t('edits.actions.revertAllTitle')}>
            <Icon name="undo" size={13} />
            <span>{i18n.t('edits.actions.revertAll')}</span>
          </button>
        </div>

        {#if hasGroups}
          <!-- 统一暂存（历史轮次） -->
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

        <!-- 本轮变更 / 全部变更 -->
        <div class="group-section">
          {#if hasGroups || currentRoundEdits.length > 0}
            <div class="group-header current-round">
              <span class="group-label">{i18n.t('edits.group.currentRound')}</span>
              <span class="group-count">{i18n.t('edits.group.currentRoundCount', { count: currentRoundEdits.length })}</span>
              <button
                class="revert-round-btn"
                onclick={revertMission}
                disabled={appState.isProcessing}
                title={appState.isProcessing ? i18n.t('edits.group.revertRoundTitleDisabled') : i18n.t('edits.group.revertRoundTitle')}
              >
                <Icon name="undo" size={12} />
                <span>{i18n.t('edits.group.revertRound')}</span>
              </button>
            </div>
            <div class="file-list">
              {#each currentRoundEdits as edit (getEditKey(edit))}
                {@render fileRow(edit)}
              {/each}
            </div>
          {:else}
            <!-- 没有 missionId 的情况（兼容旧数据）：全部扁平显示 -->
            <div class="file-list">
              {#each edits as edit (getEditKey(edit))}
                {@render fileRow(edit)}
              {/each}
            </div>
          {/if}
        </div>
      </div>
      {#if useDockedPreview}
        <aside class="preview-sidepane" class:active={!!previewTitle}>
          {#if previewTitle}
            <div class="preview-header">
              <div class="preview-header-copy">
                <div class="preview-title">{previewTitle}</div>
                <div class="preview-subtitle">
                  {previewMode === 'diff' ? i18n.t('edits.preview.diffTitle') : i18n.t('edits.preview.fileTitle')}
                </div>
                {#if selectedPreviewEdit}
                  <div class="preview-meta">
                    <span class="preview-meta-chip">
                      {selectedPreviewEdit.type === 'add'
                        ? i18n.t('edits.summary.added', { count: 1 })
                        : selectedPreviewEdit.type === 'delete'
                          ? i18n.t('edits.summary.deleted', { count: 1 })
                          : i18n.t('edits.summary.modified', { count: 1 })}
                    </span>
                    <span class="preview-meta-chip stat-add">+{selectedPreviewEdit.additions ?? 0}</span>
                    <span class="preview-meta-chip stat-del">-{selectedPreviewEdit.deletions ?? 0}</span>
                  </div>
                {/if}
              </div>
              <div class="preview-toolbar">
                {#if selectedPreviewEdit}
                  <button
                    class="preview-action"
                    type="button"
                    title={i18n.t('edits.preview.diffTitle')}
                    aria-label={i18n.t('edits.preview.diffTitle')}
                    onclick={() => viewDiff(selectedPreviewEdit.filePath)}
                  >
                    <Icon name="file-edit" size={14} />
                  </button>
                  <button
                    class="preview-action"
                    type="button"
                    title={i18n.t('edits.actions.openFile')}
                    aria-label={i18n.t('edits.actions.openFile')}
                    onclick={() => openFile(selectedPreviewEdit.filePath)}
                  >
                    <Icon name="file-text" size={14} />
                  </button>
                  <button
                    class="preview-action approve"
                    type="button"
                    title={i18n.t('edits.actions.approveChange')}
                    aria-label={i18n.t('edits.actions.approveChange')}
                    onclick={() => approveChange(selectedPreviewEdit.filePath)}
                  >
                    <Icon name="check" size={14} />
                  </button>
                  <button
                    class="preview-action revert"
                    type="button"
                    title={i18n.t('edits.actions.revertChange')}
                    aria-label={i18n.t('edits.actions.revertChange')}
                    onclick={() => revertChange(selectedPreviewEdit.filePath)}
                  >
                    <Icon name="undo" size={14} />
                  </button>
                {/if}
                <button class="preview-close" type="button" onclick={closePreview} title={i18n.t('common.close')}>
                  <Icon name="close" size={16} />
                </button>
              </div>
            </div>
            <div class="preview-body">
              {#if previewLoading}
                <div class="preview-empty">{i18n.t('edits.preview.loading')}</div>
              {:else if previewError}
                <div class="preview-empty error">{previewError}</div>
              {:else if !previewContent}
                <div class="preview-empty">{i18n.t('edits.preview.empty')}</div>
              {:else if previewMode === 'diff'}
                <div class="preview-diff">
                  {#each getPreviewLines(previewContent) as line, index}
                    <div class="preview-diff-line {getDiffLineClass(line)}">
                      <span class="preview-line-number">{index + 1}</span>
                      <code>{line || ' '}</code>
                    </div>
                  {/each}
                </div>
              {:else}
                <div class="preview-file">
                  {#each getPreviewLines(previewContent) as line, index}
                    <div class="preview-file-line">
                      <span class="preview-line-number">{index + 1}</span>
                      <code class:wrap={previewLanguage === 'markdown'}>{line || ' '}</code>
                    </div>
                  {/each}
                </div>
              {/if}
            </div>
          {:else}
            <div class="preview-empty state-hint">{i18n.t('edits.preview.selectFile')}</div>
          {/if}
        </aside>
      {/if}
    </div>
  {/if}
</div>

{#if previewOpen && !useDockedPreview}
  <div class="preview-overlay" role="presentation" onclick={(event) => {
    if (event.target === event.currentTarget) {
      closePreview();
    }
  }}>
    <div class="preview-modal" role="dialog" aria-modal="true" aria-label={previewTitle}>
      <div class="preview-header">
        <div class="preview-header-copy">
          <div class="preview-title">{previewTitle}</div>
          <div class="preview-subtitle">
            {previewMode === 'diff' ? i18n.t('edits.preview.diffTitle') : i18n.t('edits.preview.fileTitle')}
          </div>
        </div>
        <button class="preview-close" type="button" onclick={closePreview} title={i18n.t('common.close')}>
          <Icon name="close" size={16} />
        </button>
      </div>

      <div class="preview-body">
        {#if previewLoading}
          <div class="preview-empty">{i18n.t('edits.preview.loading')}</div>
        {:else if previewError}
          <div class="preview-empty error">{previewError}</div>
        {:else if !previewContent}
          <div class="preview-empty">{i18n.t('edits.preview.empty')}</div>
        {:else if previewMode === 'diff'}
          <div class="preview-diff">
            {#each getPreviewLines(previewContent) as line, index}
              <div class="preview-diff-line {getDiffLineClass(line)}">
                <span class="preview-line-number">{index + 1}</span>
                <code>{line || ' '}</code>
              </div>
            {/each}
          </div>
        {:else}
          <div class="preview-file">
            {#each getPreviewLines(previewContent) as line, index}
              <div class="preview-file-line">
                <span class="preview-line-number">{index + 1}</span>
                <code class:wrap={previewLanguage === 'markdown'}>{line || ' '}</code>
              </div>
            {/each}
          </div>
        {/if}
      </div>
      {#if isWebMode && selectedPreviewEdit}
        <div class="preview-footer-actions">
          <button class="footer-action secondary" type="button" onclick={() => viewDiff(selectedPreviewEdit.filePath)}>
            <Icon name="file-edit" size={14} />
            <span>{i18n.t('edits.preview.diffTitle')}</span>
          </button>
          <button class="footer-action secondary" type="button" onclick={() => openFile(selectedPreviewEdit.filePath)}>
            <Icon name="file-text" size={14} />
            <span>{i18n.t('edits.actions.openFile')}</span>
          </button>
          <button class="footer-action approve" type="button" onclick={() => approveChange(selectedPreviewEdit.filePath)}>
            <Icon name="check" size={14} />
            <span>{i18n.t('edits.actions.approveChange')}</span>
          </button>
          <button class="footer-action revert" type="button" onclick={() => revertChange(selectedPreviewEdit.filePath)}>
            <Icon name="undo" size={14} />
            <span>{i18n.t('edits.actions.revertChange')}</span>
          </button>
        </div>
      {/if}
    </div>
  </div>
{/if}

<style>
  .edits-panel {
    --edits-card-bg: color-mix(in srgb, var(--surface-1) 88%, var(--background));
    --edits-card-bg-strong: color-mix(in srgb, var(--surface-2) 84%, var(--background));
    --edits-card-border: color-mix(in srgb, var(--border-subtle) 82%, transparent);
    --edits-card-shadow: 0 1px 0 color-mix(in srgb, var(--foreground) 4%, transparent);
    --edits-row-bg: color-mix(in srgb, var(--background) 62%, var(--surface-1));
    --edits-row-bg-hover: color-mix(in srgb, var(--surface-hover) 86%, var(--surface-1));
    --edits-row-border: color-mix(in srgb, var(--border-subtle) 76%, transparent);
    --edits-header-bg: color-mix(in srgb, var(--surface-1) 76%, var(--background));
    --edits-line-number-bg: color-mix(in srgb, var(--surface-2) 78%, transparent);
    --edits-overlay-bg: rgba(10, 16, 28, 0.52);
    height: 100%;
    min-height: 0; /* flex 布局防溢出 */
    overflow-y: auto;
    padding: var(--space-3);
    background: transparent;
    scrollbar-width: thin;
    scrollbar-color: var(--scrollbar-thumb) transparent;
  }

  :global(.theme-light) .edits-panel,
  :global(body.vscode-light) .edits-panel,
  :global(:root.theme-light) .edits-panel {
    --edits-card-bg: color-mix(in srgb, white 90%, var(--surface-1));
    --edits-card-bg-strong: color-mix(in srgb, white 78%, var(--surface-2));
    --edits-card-border: color-mix(in srgb, var(--border-subtle) 88%, rgba(15, 23, 42, 0.05));
    --edits-card-shadow:
      0 1px 0 rgba(255, 255, 255, 0.75),
      0 10px 24px rgba(15, 23, 42, 0.04);
    --edits-row-bg: color-mix(in srgb, white 92%, var(--surface-1));
    --edits-row-bg-hover: color-mix(in srgb, white 82%, var(--surface-hover));
    --edits-row-border: color-mix(in srgb, var(--border-subtle) 88%, rgba(15, 23, 42, 0.06));
    --edits-header-bg: color-mix(in srgb, white 80%, var(--surface-1));
    --edits-line-number-bg: color-mix(in srgb, white 76%, var(--surface-2));
    --edits-overlay-bg: rgba(15, 23, 42, 0.24);
  }

  .edits-panel::-webkit-scrollbar {
    width: 10px;
  }

  .edits-panel::-webkit-scrollbar-track {
    background: transparent;
  }

  .edits-panel::-webkit-scrollbar-thumb {
    background: var(--scrollbar-thumb);
    border-radius: var(--radius-full);
    border: 2px solid transparent;
    background-clip: padding-box;
  }

  .edits-panel::-webkit-scrollbar-thumb:hover {
    background: var(--scrollbar-thumb-hover);
    background-clip: padding-box;
  }

  .edits-shell {
    min-height: 100%;
  }

  .edits-main {
    min-width: 0;
  }

  .edits-panel.web-mode .summary-bar,
  .edits-panel.web-mode .bulk-actions,
  .edits-panel.web-mode .group-section {
    max-width: none;
  }

  .edits-shell.has-docked-preview {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(360px, 420px);
    gap: var(--space-3);
    align-items: start;
  }

  .preview-sidepane {
    position: sticky;
    top: var(--space-3);
    min-height: 560px;
    max-height: calc(100vh - 120px);
    display: flex;
    flex-direction: column;
    background: var(--edits-card-bg);
    border: 1px solid var(--edits-card-border);
    border-radius: var(--radius-xl);
    box-shadow: var(--edits-card-shadow);
    overflow: hidden;
  }

  .preview-sidepane:not(.active) {
    align-items: center;
    justify-content: center;
  }

  /* 空状态 */
  .empty-state {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    padding: var(--space-8) var(--space-5);
    color: var(--foreground-muted);
    text-align: center;
    gap: var(--space-2);
  }

  .empty-text {
    font-size: var(--text-base);
    font-weight: var(--font-medium);
    color: var(--foreground);
  }

  .empty-hint {
    font-size: var(--text-sm);
    opacity: 0.6;
  }

  /* 统计条 */
  .summary-bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--space-2) var(--space-3);
    margin-bottom: var(--space-2);
    position: sticky;
    top: 0;
    z-index: 2;
    background: var(--edits-card-bg);
    backdrop-filter: blur(10px);
    border-radius: var(--radius-lg);
    border: 1px solid var(--edits-card-border);
    box-shadow: var(--edits-card-shadow);
  }

  .summary-left {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    flex-wrap: wrap;
  }

  .summary-count {
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    color: var(--foreground);
  }

  .summary-chip {
    font-size: var(--text-2xs);
    padding: 1px 6px;
    border-radius: var(--radius-full);
    font-weight: var(--font-medium);
  }

  .summary-chip.add { color: var(--success); background: var(--success-muted); }
  .summary-chip.modify { color: var(--warning); background: var(--warning-muted); }
  .summary-chip.del { color: var(--error); background: var(--error-muted); }

  .summary-right {
    display: flex;
    gap: var(--space-2);
    font-size: var(--text-xs);
    font-weight: var(--font-semibold);
    font-variant-numeric: tabular-nums;
  }

  .stat-add { color: var(--success); }
  .stat-del { color: var(--error); }

  /* 批量操作 */
  .bulk-actions {
    display: flex;
    gap: var(--space-2);
    margin-bottom: var(--space-2);
    flex-wrap: wrap;
  }

  .bulk-btn {
    display: inline-flex;
    align-items: center;
    gap: var(--space-1);
    min-height: 30px;
    padding: 6px 10px;
    border-radius: var(--radius-sm);
    border: 1px solid var(--edits-card-border);
    background: var(--edits-card-bg);
    color: var(--foreground);
    cursor: pointer;
    font-size: var(--text-xs);
    transition: all var(--transition-fast);
    box-shadow: var(--edits-card-shadow);
  }

  .bulk-btn:hover {
    background: var(--edits-row-bg-hover);
    color: var(--foreground);
  }

  .bulk-btn.approve:hover {
    color: var(--success);
    border-color: var(--success);
  }

  .bulk-btn.revert:hover {
    color: var(--error);
    border-color: var(--error);
  }

  /* 文件列表 */
  .file-list {
    display: flex;
    flex-direction: column;
    gap: 1px;
    background: var(--edits-row-border);
    border: 1px solid var(--edits-card-border);
    border-radius: var(--radius-md);
    overflow: hidden;
  }

  .file-row {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    background: var(--edits-row-bg);
    cursor: pointer;
    transition: background var(--transition-fast);
    position: relative;
  }

  .file-row:hover {
    background: var(--edits-row-bg-hover);
  }

  .file-row.selected {
    background: color-mix(in srgb, var(--info-muted) 28%, var(--edits-row-bg));
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--info) 30%, transparent);
  }

  .edits-panel.web-mode .file-row {
    display: grid;
    grid-template-columns: auto auto minmax(0, 1fr) auto auto;
    grid-template-areas:
      "indicator icon info stats actions"
      "indicator icon workers workers actions";
    align-items: center;
    column-gap: var(--space-2);
    row-gap: 6px;
    padding-block: 12px;
  }

  .edits-panel.web-mode .type-indicator {
    grid-area: indicator;
  }

  .edits-panel.web-mode .file-icon {
    grid-area: icon;
  }

  /* 左侧变更类型彩条 */
  .type-indicator {
    width: 3px;
    height: 20px;
    border-radius: 2px;
    flex-shrink: 0;
    background: var(--foreground-muted);
    opacity: 0.3;
  }

  .type-indicator.add { background: var(--success); opacity: 1; }
  .type-indicator.modify { background: var(--warning); opacity: 1; }
  .type-indicator.del { background: var(--error); opacity: 1; }

  /* 文件图标 */
  .file-icon {
    flex-shrink: 0;
    color: var(--foreground-muted);
    display: flex;
    align-items: center;
  }

  .file-icon.add { color: var(--success); }
  .file-icon.modify { color: var(--warning); }
  .file-icon.del { color: var(--error); }

  /* 文件名 */
  .file-info {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: baseline;
    gap: var(--space-2);
    overflow: hidden;
  }

  .edits-panel.web-mode .file-info {
    grid-area: info;
    flex-direction: column;
    align-items: flex-start;
    gap: 2px;
  }

  .edits-panel.web-mode .file-name {
    width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .edits-panel.web-mode .file-dir {
    width: 100%;
  }

  .file-name {
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    color: var(--foreground);
    white-space: nowrap;
    flex-shrink: 0;
  }

  .file-dir {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    opacity: 0.7;
  }

  /* Worker 标识 */
  .file-workers {
    display: flex;
    gap: var(--space-1);
    flex-shrink: 0;
  }

  .edits-panel.web-mode .file-workers {
    grid-area: workers;
    min-width: 0;
    flex-wrap: wrap;
  }

  /* 增删统计 */
  .file-stats {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    font-size: var(--text-xs);
    font-weight: var(--font-medium);
    font-variant-numeric: tabular-nums;
    flex-shrink: 0;
  }

  .edits-panel.web-mode .file-stats {
    grid-area: stats;
    align-self: start;
    padding-top: 2px;
  }

  /* GitHub 风格增删比例条 */
  .change-blocks {
    display: flex;
    gap: 1px;
  }

  .change-blocks .block {
    width: 7px;
    height: 7px;
    border-radius: 1px;
  }

  .block.add { background: var(--success); }
  .block.del { background: var(--error); }
  .block.neutral { background: var(--surface-3); }

  /* hover 操作按钮 */
  .file-actions {
    display: flex;
    gap: var(--space-1);
    opacity: 0;
    transition: opacity var(--transition-fast);
    flex-shrink: 0;
  }

  .edits-panel.web-mode .file-actions {
    grid-area: actions;
    opacity: 1;
    align-self: center;
  }

  .file-row:hover .file-actions {
    opacity: 1;
  }

  :global(.theme-light) .file-row,
  :global(body.vscode-light) .file-row,
  :global(:root.theme-light) .file-row {
    box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.65);
  }

  .action-icon {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 24px;
    height: 24px;
    border-radius: var(--radius-sm);
    border: none;
    background: color-mix(in srgb, var(--surface-2) 72%, transparent);
    color: var(--foreground-muted);
    cursor: pointer;
    transition: all var(--transition-fast);
    border: 1px solid transparent;
  }

  .action-icon:hover {
    background: color-mix(in srgb, var(--surface-active) 90%, var(--surface-1));
    border-color: var(--edits-card-border);
    color: var(--foreground);
  }

  .action-icon.approve:hover { color: var(--success); }
  .action-icon.revert:hover { color: var(--error); }

  /* ─── 轮次分组 ─── */
  .group-section {
    margin-bottom: var(--space-3);
    padding: var(--space-2);
    background: var(--edits-card-bg);
    border: 1px solid var(--edits-card-border);
    border-radius: var(--radius-lg);
    box-shadow: var(--edits-card-shadow);
  }

  .group-section:last-child {
    margin-bottom: 0;
  }

  .group-header {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-2);
    margin-bottom: var(--space-2);
    border-radius: var(--radius-md);
    background: var(--edits-header-bg);
    border: 1px solid color-mix(in srgb, var(--edits-card-border) 85%, transparent);
  }

  .group-label {
    font-size: var(--text-xs);
    font-weight: var(--font-semibold);
    color: var(--foreground);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .group-header.current-round .group-label {
    color: var(--info);
  }

  .group-count {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    opacity: 0.9;
  }

  .revert-round-btn {
    display: inline-flex;
    align-items: center;
    gap: var(--space-1);
    margin-left: auto;
    min-height: 28px;
    padding: 4px 9px;
    border-radius: var(--radius-sm);
    border: 1px solid var(--edits-card-border);
    background: var(--edits-card-bg-strong);
    color: var(--foreground);
    cursor: pointer;
    font-size: var(--text-2xs);
    transition: all var(--transition-fast);
  }

  .revert-round-btn:hover:not(:disabled) {
    color: var(--error);
    border-color: var(--error);
    background: var(--error-muted);
  }

  .revert-round-btn:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  @media (hover: none) {
    .file-actions {
      opacity: 1;
    }
  }

  @media (max-width: 768px) {
    .edits-panel {
      padding: var(--space-2);
    }

    .edits-panel.web-mode .file-row,
    .file-row {
      align-items: flex-start;
      flex-wrap: wrap;
      row-gap: var(--space-2);
    }

    .edits-panel.web-mode .file-row {
      display: flex;
    }

    .file-info {
      width: calc(100% - 52px);
      min-width: 0;
      flex-direction: column;
      align-items: flex-start;
      gap: 0;
    }

    .file-name,
    .file-dir {
      white-space: normal;
      word-break: break-all;
    }

    .file-workers,
    .file-stats,
    .file-actions {
      margin-left: 24px;
    }
  }

  @media (max-width: 960px) {
    .edits-shell.has-docked-preview {
      grid-template-columns: 1fr;
    }

    .preview-sidepane {
      display: none;
    }
  }

  .preview-overlay {
    position: fixed;
    inset: 0;
    z-index: 80;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: var(--space-4);
    background: var(--edits-overlay-bg);
    backdrop-filter: blur(8px);
  }

  .preview-modal {
    width: min(1100px, 100%);
    max-height: min(82vh, 920px);
    display: flex;
    flex-direction: column;
    background: var(--edits-card-bg);
    border: 1px solid var(--edits-card-border);
    border-radius: var(--radius-xl);
    box-shadow:
      0 18px 48px rgba(0, 0, 0, 0.28),
      0 1px 0 rgba(255, 255, 255, 0.06) inset;
    overflow: hidden;
  }

  .preview-header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: var(--space-3);
    padding: var(--space-4);
    border-bottom: 1px solid var(--edits-card-border);
    background: var(--edits-header-bg);
  }

  .preview-header-copy {
    min-width: 0;
  }

  .preview-title {
    color: var(--foreground);
    font-size: var(--text-base);
    font-weight: var(--font-semibold);
    word-break: break-all;
  }

  .preview-subtitle {
    margin-top: 4px;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
  }

  .preview-meta {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
    margin-top: var(--space-2);
  }

  .preview-meta-chip {
    display: inline-flex;
    align-items: center;
    min-height: 24px;
    padding: 0 8px;
    border-radius: var(--radius-full);
    border: 1px solid var(--edits-card-border);
    background: var(--edits-card-bg);
    color: var(--foreground-muted);
    font-size: var(--text-2xs);
    font-weight: var(--font-medium);
  }

  .preview-close {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 36px;
    height: 36px;
    border: 1px solid var(--edits-card-border);
    border-radius: var(--radius-md);
    background: var(--edits-card-bg-strong);
    color: var(--foreground-muted);
    cursor: pointer;
  }

  .preview-close:hover {
    color: var(--foreground);
    background: var(--surface-hover);
  }

  .preview-toolbar {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    margin-left: auto;
  }

  .preview-action {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 34px;
    height: 34px;
    border-radius: var(--radius-md);
    border: 1px solid var(--edits-card-border);
    background: var(--edits-card-bg-strong);
    color: var(--foreground-muted);
    cursor: pointer;
    transition: all var(--transition-fast);
  }

  .preview-action:hover {
    background: var(--edits-row-bg-hover);
    color: var(--foreground);
  }

  .preview-action.approve:hover { color: var(--success); }
  .preview-action.revert:hover { color: var(--error); }

  .preview-body {
    min-height: 0;
    overflow: auto;
    padding: var(--space-3);
    background: color-mix(in srgb, var(--background) 86%, var(--surface-1));
    scrollbar-width: thin;
    scrollbar-color: var(--scrollbar-thumb) transparent;
  }

  .preview-diff,
  .preview-file {
    border: 1px solid var(--edits-card-border);
    border-radius: var(--radius-lg);
    overflow: hidden;
    background: var(--edits-card-bg-strong);
    box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.04);
  }

  .preview-diff-line,
  .preview-file-line {
    display: grid;
    grid-template-columns: 56px 1fr;
    align-items: stretch;
    min-width: 0;
    font-family: var(--font-mono);
    font-size: 12px;
    line-height: 1.55;
    border-top: 1px solid color-mix(in srgb, var(--edits-card-border) 78%, transparent);
  }

  .preview-diff-line:first-child,
  .preview-file-line:first-child {
    border-top: none;
  }

  .preview-line-number {
    padding: 8px 10px;
    color: var(--foreground-muted);
    text-align: right;
    background: var(--edits-line-number-bg);
    border-right: 1px solid var(--edits-card-border);
    user-select: none;
  }

  .preview-diff-line code,
  .preview-file-line code {
    margin: 0;
    padding: 8px 12px;
    white-space: pre;
    overflow-x: auto;
    background: transparent;
    color: var(--foreground);
  }

  .preview-file-line code.wrap {
    white-space: pre-wrap;
    word-break: break-word;
  }

  .preview-diff-line.meta code {
    color: var(--info);
    background: color-mix(in srgb, var(--info-muted) 42%, transparent);
  }

  .preview-diff-line.add code {
    color: var(--success);
    background: color-mix(in srgb, var(--success-muted) 40%, transparent);
  }

  .preview-diff-line.del code {
    color: var(--error);
    background: color-mix(in srgb, var(--error-muted) 42%, transparent);
  }

  .preview-empty {
    padding: var(--space-8) var(--space-4);
    text-align: center;
    color: var(--foreground-muted);
  }

  .preview-empty.state-hint {
    max-width: 240px;
    font-size: var(--text-sm);
    line-height: 1.6;
  }

  .preview-empty.error {
    color: var(--error);
  }

  .preview-footer-actions {
    display: none;
  }

  .footer-action {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: var(--space-1);
    min-height: 40px;
    padding: 0 12px;
    border-radius: var(--radius-md);
    border: 1px solid var(--edits-card-border);
    background: var(--edits-card-bg);
    color: var(--foreground);
    font-size: var(--text-xs);
    font-weight: var(--font-medium);
    cursor: pointer;
    transition: all var(--transition-fast);
  }

  .footer-action.secondary:hover {
    background: var(--edits-row-bg-hover);
  }

  .footer-action.approve {
    color: var(--success);
    border-color: color-mix(in srgb, var(--success) 28%, var(--edits-card-border));
    background: color-mix(in srgb, var(--success-muted) 66%, var(--edits-card-bg));
  }

  .footer-action.revert {
    color: var(--error);
    border-color: color-mix(in srgb, var(--error) 28%, var(--edits-card-border));
    background: color-mix(in srgb, var(--error-muted) 66%, var(--edits-card-bg));
  }

  @media (max-width: 768px) {
    .preview-overlay {
      padding: 0;
      align-items: stretch;
    }

    .preview-modal {
      width: 100%;
      max-height: none;
      height: 100%;
      border-radius: 0;
    }

    .preview-header {
      padding: var(--space-3);
    }

    .preview-diff-line,
    .preview-file-line {
      grid-template-columns: 44px 1fr;
      font-size: 11px;
    }

    .preview-footer-actions {
      display: grid;
      grid-template-columns: repeat(2, minmax(0, 1fr));
      gap: var(--space-2);
      padding: var(--space-3);
      border-top: 1px solid var(--edits-card-border);
      background: var(--edits-card-bg);
      box-shadow: 0 -8px 24px rgba(0, 0, 0, 0.08);
    }
  }
</style>
