<script lang="ts">
  import {
    applyPendingChangesProjection,
    getCurrentSessionId,
    messagesState,
  } from '../stores/messages.svelte';
  import { vscode } from '../lib/vscode-bridge';
  import { ensureArray } from '../lib/utils';
  import { openCodeTab } from '../stores/right-pane.svelte';
  import type { Edit } from '../types/message';
  import type { IconName } from '../lib/icons';
  import Icon from './Icon.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import {
    getAgentChangeDiff,
    getAgentPendingChanges,
    isWebAgentMode,
  } from '../web/agent-api';

  const isWebMode = isWebAgentMode();
  const changeRefreshIntervalMs = 1000;

  const edits = $derived(ensureArray(messagesState.edits) as Edit[]);

  // ─── 按执行分组展示 ───
  // 最新执行分组 ID：取 edits 列表中最后一个有 executionGroupId 的值（后端已按 timestamp 排序）
  const latestExecutionGroupId = $derived.by(() => {
    if (edits.length === 0) return null;
    for (let i = edits.length - 1; i >= 0; i--) {
      if (edits[i].executionGroupId) return edits[i].executionGroupId!;
    }
    return null;
  });

  const currentRoundEdits = $derived(
    latestExecutionGroupId ? edits.filter(e => e.executionGroupId === latestExecutionGroupId) : []
  );

  const earlierPendingEdits = $derived(
    latestExecutionGroupId ? edits.filter(e => e.executionGroupId !== latestExecutionGroupId) : edits
  );

  const hasGroups = $derived(earlierPendingEdits.length > 0 && currentRoundEdits.length > 0);
  const allEditsRevertible = $derived(edits.every((edit) => edit.revertible === true));
  const currentRoundRevertible = $derived(
    currentRoundEdits.length > 0 && currentRoundEdits.every((edit) => edit.revertible === true)
  );

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

  function editScope(edit?: Edit): { sessionId?: string; workspaceId?: string; workspacePath?: string } {
    return {
      sessionId: edit?.sessionId?.trim() || getCurrentSessionId() || undefined,
      workspaceId: edit?.workspaceId?.trim() || messagesState.currentWorkspaceId?.trim() || undefined,
      workspacePath: edit?.workspacePath?.trim() || messagesState.currentWorkspacePath?.trim() || undefined,
    };
  }

  function normalizeScopePart(value?: string | null): string {
    return typeof value === 'string' ? value.trim() : '';
  }

  function scopeMatchesActiveChangeMutation(scope: ReturnType<typeof editScope>): boolean {
    const status = messagesState.changeMutationStatus;
    if (!status?.isMutating) {
      return false;
    }
    const statusSessionId = normalizeScopePart(status.sessionId);
    const statusWorkspaceId = normalizeScopePart(status.workspaceId);
    const statusWorkspacePath = normalizeScopePart(status.workspacePath);
    const scopeSessionId = normalizeScopePart(scope.sessionId);
    const scopeWorkspaceId = normalizeScopePart(scope.workspaceId);
    const scopeWorkspacePath = normalizeScopePart(scope.workspacePath);
    if (statusSessionId && scopeSessionId && statusSessionId !== scopeSessionId) return false;
    if (statusWorkspaceId && scopeWorkspaceId && statusWorkspaceId !== scopeWorkspaceId) return false;
    if (statusWorkspacePath && scopeWorkspacePath && statusWorkspacePath !== scopeWorkspacePath) return false;
    return Boolean(statusSessionId || statusWorkspaceId || statusWorkspacePath);
  }

  const changeMutationPending = $derived.by(() => (
    scopeMatchesActiveChangeMutation(editScope(currentRoundEdits[0] ?? earlierPendingEdits[0] ?? edits[0]))
  ));

  $effect(() => {
    const sessionId = messagesState.currentSessionId?.trim() || '';
    const workspaceId = messagesState.currentWorkspaceId?.trim() || '';
    const workspacePath = messagesState.currentWorkspacePath?.trim() || '';
    if (!isWebMode || !sessionId || (!workspaceId && !workspacePath)) {
      return;
    }

    let disposed = false;
    let inFlight = false;
    let errorReported = false;
    const refresh = async () => {
      if (disposed || inFlight || messagesState.changeMutationStatus?.isMutating) {
        return;
      }
      inFlight = true;
      try {
        const payload = await getAgentPendingChanges({ sessionId, workspaceId, workspacePath });
        if (!disposed) {
          applyPendingChangesProjection(payload);
        }
        errorReported = false;
      } catch (error) {
        if (!disposed && !errorReported) {
          console.warn('[EditsPanel] 刷新变更列表失败:', error);
          errorReported = true;
        }
      } finally {
        inFlight = false;
      }
    };

    void refresh();
    const timer = window.setInterval(() => {
      void refresh();
    }, changeRefreshIntervalMs);
    return () => {
      disposed = true;
      window.clearInterval(timer);
    };
  });

  function approveChange(edit: Edit) {
    if (changeMutationPending) return;
    vscode.postMessage({ type: 'approveChange', filePath: edit.filePath, ...editScope(edit) });
  }
  function revertChange(edit: Edit) {
    if (changeMutationPending || edit.revertible !== true) return;
    vscode.postMessage({ type: 'revertChange', filePath: edit.filePath, ...editScope(edit) });
  }

  function approveAllChanges() {
    if (changeMutationPending || edits.length === 0) return;
    vscode.postMessage({ type: 'approveAllChanges', ...editScope(currentRoundEdits[0] ?? earlierPendingEdits[0]) });
  }
  function revertAllChanges() {
    if (changeMutationPending || edits.length === 0 || !allEditsRevertible) return;
    vscode.postMessage({ type: 'revertAllChanges', ...editScope(currentRoundEdits[0] ?? earlierPendingEdits[0]) });
  }
  function revertCurrentRound() {
    if (changeMutationPending || !latestExecutionGroupId || !currentRoundRevertible) return;
    vscode.postMessage({
      type: 'revertExecutionGroup',
      executionGroupId: latestExecutionGroupId,
      ...editScope(currentRoundEdits[0]),
    });
  }

  function editTitle(edit: Edit): string {
    return edit.type === 'rename' && edit.oldPath
      ? `${edit.oldPath} → ${edit.filePath}`
      : edit.filePath;
  }

  /**
   * 为 add/delete 类型变更合成 unified diff，让 RightPane 始终走 diff 视图。
   * - 后端已生成 diff：直接复用（modify 走这条）
   * - add 且仅有 previewContent：合成 `@@ -0,0 +1,N @@` + 全 `+` 行
   * - delete 且仅有 originalContent：合成 `@@ -1,N +0,0 @@` + 全 `-` 行
   * - 其他情况返回 null，调用方仍可能用 content 走源码视图兜底
   */
  function synthesizeDiff(edit: Edit): string | null {
    if (typeof edit.diff === 'string' && edit.diff.trim().length > 0) {
      return edit.diff;
    }
    if (edit.type === 'add' && typeof edit.previewContent === 'string' && edit.previewContent.length > 0) {
      const rawLines = edit.previewContent.split('\n');
      const effectiveLen = rawLines.length > 0 && rawLines[rawLines.length - 1] === ''
        ? rawLines.length - 1
        : rawLines.length;
      const body = rawLines.slice(0, effectiveLen).map((l) => `+${l}`).join('\n');
      return `@@ -0,0 +1,${effectiveLen} @@\n${body}`;
    }
    if (edit.type === 'delete' && typeof edit.originalContent === 'string' && edit.originalContent.length > 0) {
      const rawLines = edit.originalContent.split('\n');
      const effectiveLen = rawLines.length > 0 && rawLines[rawLines.length - 1] === ''
        ? rawLines.length - 1
        : rawLines.length;
      const body = rawLines.slice(0, effectiveLen).map((l) => `-${l}`).join('\n');
      return `@@ -1,${effectiveLen} +0,0 @@\n${body}`;
    }
    return null;
  }

  function hasInlineChangeDetail(edit: Edit): boolean {
    return (
      (typeof edit.diff === 'string' && edit.diff.trim().length > 0)
      || (typeof edit.previewContent === 'string' && edit.previewContent.length > 0)
      || (typeof edit.originalContent === 'string' && edit.originalContent.length > 0)
    );
  }

  async function loadChangeDetail(edit: Edit, scope: ReturnType<typeof editScope>): Promise<Edit> {
    if (hasInlineChangeDetail(edit)) {
      return edit;
    }
    if (edit.contentKind && edit.contentKind !== 'text' && edit.contentKind !== 'large_text') {
      return edit;
    }
    try {
      const detail = await getAgentChangeDiff(edit.filePath, scope);
      return {
        ...edit,
        diff: typeof detail.diff === 'string' ? detail.diff : edit.diff,
        originalContent:
          typeof detail.originalContent === 'string'
            ? detail.originalContent
            : edit.originalContent,
        previewContent:
          typeof detail.currentContent === 'string'
            ? detail.currentContent
            : edit.previewContent,
      };
    } catch (error) {
      console.warn('[EditsPanel] change detail load failed:', error);
      return edit;
    }
  }

  /**
   * 点击文件行：
   * - Web 模式：把变更推到全局右侧 RightPane 的 code tab（携带 diff 与文件元信息），由 RightPane 负责展示与切换
   * - VS Code host：沿用 host 的 diff 编辑器（postMessage 给 extension）
   * EditsPanel 自身不再承担 diff 预览职责，避免与 RightPane 双轨实现
   */
  async function viewDiff(edit: Edit) {
    if (!isWebMode) {
      vscode.postMessage({
        type: 'viewDiff',
        filePath: edit.filePath,
        ...editScope(edit),
        diff: edit.diff || '',
        originalContent: edit?.originalContent,
        previewContent:
          (typeof edit.previewContent === 'string' && edit.previewContent.length > 0)
            ? edit.previewContent
            : (typeof edit.originalContent === 'string' ? edit.originalContent : ''),
        previewAbsolutePath: edit?.previewAbsolutePath,
        previewCanOpenWorkspaceFile: edit?.previewCanOpenWorkspaceFile,
        contentKind: edit?.contentKind ?? 'text',
        size: edit?.size,
        mime: edit?.mime,
        symlinkTarget: edit?.symlinkTarget,
        headSummary: edit?.headSummary,
        tailSummary: edit?.tailSummary,
      });
      return;
    }
    const scope = editScope(edit);
    const detail = await loadChangeDetail(edit, scope);
    const diff = synthesizeDiff(detail);
    openCodeTab(scope.sessionId, detail.filePath, {
      ...scope,
      diff,
      isChangeDiff: Boolean(diff),
      content: diff
        ? null
        : (
          typeof detail.previewContent === 'string' && detail.previewContent.length > 0
            ? detail.previewContent
            : (typeof detail.originalContent === 'string' ? detail.originalContent : null)
        ),
      contentKind: detail.contentKind,
      size: detail.size,
      mime: detail.mime,
      symlinkTarget: detail.symlinkTarget,
      headSummary: detail.headSummary,
      tailSummary: detail.tailSummary,
    });
  }

  function getEditKey(edit: Edit): string {
    return `${edit.filePath}::${edit.executionGroupId ?? 'none'}::${edit.snapshotId ?? 'na'}`;
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

  function editActorLabel(edit: Edit): string | null {
    if (edit.workerId?.trim()) return i18n.t('edits.actor.agent');
    if (edit.sourceKind === 'tool') return i18n.t('edits.actor.mainline');
    if (edit.sourceKind === 'watcher' || edit.sourceKind === 'external') return i18n.t('edits.actor.external');
    return null;
  }

  function editActorTitle(edit: Edit): string {
    if (edit.workerId?.trim()) return i18n.t('edits.actor.agentTitle');
    if (edit.sourceKind === 'tool') return i18n.t('edits.actor.mainlineTitle');
    return i18n.t('edits.actor.externalTitle');
  }
</script>

{#snippet fileRow(edit: Edit)}
  {@const { name } = splitPath(edit.filePath)}
  {@const oldName = edit.oldPath ? splitPath(edit.oldPath).name : ''}
  {@const kind = edit.contentKind ?? 'text'}
  {@const isText = kind === 'text'}
  <div
    class="file-row"
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
      {#if edit.hasError}
        <span class="file-error-tag" title={i18n.t('edits.row.errorTitle')}>{i18n.t('edits.row.error')}</span>
      {/if}
      {#if editActorLabel(edit)}
        <span class="file-actor-tag" title={editActorTitle(edit)}>{editActorLabel(edit)}</span>
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
      <button class="action-icon approve" type="button" disabled={changeMutationPending} title={i18n.t('edits.actions.approveChange')} onclick={(e) => { e.stopPropagation(); approveChange(edit); }}>
        <Icon name="check" size={14} />
      </button>
      <button class="action-icon revert" type="button" disabled={changeMutationPending || edit.revertible !== true} title={edit.revertible === true ? i18n.t('edits.actions.revertChange') : i18n.t('edits.actions.revertUnavailable')} onclick={(e) => { e.stopPropagation(); revertChange(edit); }}>
        <Icon name="undo" size={14} />
      </button>
    </div>
  </div>
{/snippet}

<div class="panel-content-scrollable edits-panel">
  {#if edits.length === 0}
    <div class="empty-state">
      <Icon name="file-edit" size={32} />
      <div class="empty-text">{i18n.t('edits.empty.title')}</div>
      <div class="empty-hint">{i18n.t('edits.empty.hint')}</div>
    </div>
  {:else}
    <div class="edits-main">
      {#if edits.length >= 2}
        <div class="edits-toolbar">
          <button
            type="button"
            class="toolbar-btn approve"
            disabled={changeMutationPending}
            title={i18n.t('edits.actions.approveAllTitle')}
            onclick={approveAllChanges}
          >
            <Icon name="check" size={13} />
            <span>{i18n.t('edits.actions.approveAll')}</span>
          </button>
          <button
            type="button"
            class="toolbar-btn revert"
            disabled={changeMutationPending || !allEditsRevertible}
            title={allEditsRevertible ? i18n.t('edits.actions.revertAllTitle') : i18n.t('edits.actions.revertUnavailable')}
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
            <span class="group-label">{i18n.t('edits.group.earlierPending')}</span>
            <span class="group-count">{i18n.t('edits.group.earlierPendingCount', { count: earlierPendingEdits.length })}</span>
          </div>
          <div class="file-list">
            {#each earlierPendingEdits as edit (getEditKey(edit))}
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
                disabled={changeMutationPending || !currentRoundRevertible}
                title={currentRoundRevertible ? i18n.t('edits.group.revertRoundTitle') : i18n.t('edits.actions.revertUnavailable')}
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
  {/if}
</div>

<style>
  .edits-panel {
    --edits-card-bg: color-mix(in srgb, var(--surface-1) 88%, var(--background));
    --edits-card-border: color-mix(in srgb, var(--border-subtle) 82%, transparent);
    --edits-row-bg: color-mix(in srgb, var(--background) 62%, var(--surface-1));
    --edits-row-bg-hover: color-mix(in srgb, var(--surface-hover) 86%, var(--surface-1));
    --edits-row-border: color-mix(in srgb, var(--border-subtle) 76%, transparent);
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
    --edits-row-bg: #f9fafb;
    --edits-row-bg-hover: #eef1f5;
    --edits-row-border: #e2e6ed;
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

  .edits-main {
    flex: 1 1 auto;
    min-width: 0;
    min-height: 0;
    overflow: auto;
    padding: var(--space-2);
  }

  .edits-main::-webkit-scrollbar {
    width: 10px;
    height: 10px;
  }

  .edits-main::-webkit-scrollbar-track {
    background: transparent;
  }

  .edits-main::-webkit-scrollbar-thumb {
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

  .file-row {
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
  .file-error-tag,
  .file-actor-tag {
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

  .file-actor-tag {
    background: color-mix(in srgb, var(--info) 13%, transparent);
    color: var(--foreground-muted);
    border-color: color-mix(in srgb, var(--info) 28%, var(--edits-card-border));
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

  .action-icon:hover:not(:disabled) {
    border-color: var(--edits-card-border);
    background: var(--surface-hover);
    color: var(--foreground);
  }

  .action-icon:disabled {
    cursor: not-allowed;
    opacity: 0.45;
  }

  .action-icon.approve:hover:not(:disabled) { color: var(--success); }
  .action-icon.revert:hover:not(:disabled) { color: var(--error); }

  @media (hover: none) {
    .file-actions {
      opacity: 1;
    }
  }

  @media (max-width: 768px) {
    .edits-panel {
      padding: var(--space-2);
    }

    .file-row {
      grid-template-columns: 3px 18px minmax(0, 1fr) auto auto;
      gap: 6px;
      min-height: 44px;
      padding-inline: 6px;
    }
  }
</style>
