<script lang="ts">
  import {
    getCurrentSessionId,
    messagesState,
  } from '../stores/messages.svelte';
  import { vscode } from '../lib/vscode-bridge';
  import { ensureArray } from '../lib/utils';
  import {
    changeDiffRevision,
    getRightPaneState,
    openCodeTab,
    rightPaneState,
    type CodeTabPayload,
  } from '../stores/right-pane.svelte';
  import type { Edit } from '../types/message';
  import Icon from './Icon.svelte';
  import ChangeFileTree from './ChangeFileTree.svelte';
  import GitRepositoryPanel from './GitRepositoryPanel.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { requestOpenHtmlFileInBrowser } from '../lib/browser-navigation';
  import {
    getAgentChangeDiff,
    isWebAgentMode,
  } from '../web/agent-api';

  const isWebMode = isWebAgentMode();
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
  const displayedCurrentEdits = $derived(
    currentRoundEdits.length > 0 ? currentRoundEdits : edits
  );
  const currentRoundTotals = $derived.by(() => displayedCurrentEdits.reduce(
    (totals, edit) => ({
      additions: totals.additions + Math.max(0, edit.additions ?? 0),
      deletions: totals.deletions + Math.max(0, edit.deletions ?? 0),
    }),
    { additions: 0, deletions: 0 },
  ));
  const allEditsRevertible = $derived(edits.every((edit) => edit.revertible === true));
  const currentRoundRevertible = $derived(
    currentRoundEdits.length > 0 && currentRoundEdits.every((edit) => edit.revertible === true)
  );

  function editScope(edit?: Edit): { scope: 'workspace'; sessionId?: string; workspaceId: string; workspacePath: string } {
    return {
      scope: 'workspace',
      sessionId: edit?.sessionId?.trim() || getCurrentSessionId() || undefined,
      workspaceId: edit?.workspaceId?.trim() || messagesState.currentWorkspaceId?.trim() || '',
      workspacePath: edit?.workspacePath?.trim() || messagesState.currentWorkspacePath?.trim() || '',
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

  const activeCodeFilePath = $derived.by(() => {
    const pane = getRightPaneState(rightPaneState.activeScopeKey);
    if (!pane.activeTabId) return '';
    const tab = pane.openTabs.find((item) => item.id === pane.activeTabId);
    return tab?.kind === 'code' ? (tab.payload as CodeTabPayload).filepath : '';
  });

  function approveChange(edit: Edit) {
    if (changeMutationPending) return;
    vscode.postMessage({ type: 'approveChange', filePath: edit.filePath, ...editScope(edit) });
  }
  function revertChange(edit: Edit) {
    if (changeMutationPending || edit.revertible !== true) return;
    const confirmed = window.confirm(i18n.t('edits.confirm.revertChange', { file: edit.filePath }));
    if (!confirmed) return;
    vscode.postMessage({ type: 'revertChange', filePath: edit.filePath, ...editScope(edit) });
  }

  function approveAllChanges() {
    if (changeMutationPending || edits.length === 0) return;
    vscode.postMessage({ type: 'approveAllChanges', ...editScope(currentRoundEdits[0] ?? earlierPendingEdits[0]) });
  }
  function revertAllChanges() {
    if (changeMutationPending || edits.length === 0 || !allEditsRevertible) return;
    const confirmed = window.confirm(i18n.t('edits.confirm.revertAll', { count: edits.length }));
    if (!confirmed) return;
    vscode.postMessage({ type: 'revertAllChanges', ...editScope(currentRoundEdits[0] ?? earlierPendingEdits[0]) });
  }
  function revertCurrentRound() {
    if (changeMutationPending || !latestExecutionGroupId || !currentRoundRevertible) return;
    const confirmed = window.confirm(i18n.t('edits.confirm.revertRound', { count: currentRoundEdits.length }));
    if (!confirmed) return;
    vscode.postMessage({
      type: 'revertExecutionGroup',
      executionGroupId: latestExecutionGroupId,
      ...editScope(currentRoundEdits[0]),
    });
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
      changeRevision: changeDiffRevision(detail),
      originalContent: detail.originalContent ?? null,
      currentContent: detail.previewContent ?? null,
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
    requestOpenHtmlFileInBrowser(detail.filePath);
  }

</script>

<div class="panel-content-scrollable edits-panel">
  <GitRepositoryPanel />
  <div class="changes-section-label">{i18n.t('edits.section.pendingChanges')}</div>
  {#if edits.length === 0}
    <div class="empty-state">
      <Icon name="file-edit" size={32} />
      <div class="empty-text">{i18n.t('edits.empty.title')}</div>
      <div class="empty-hint">{i18n.t('edits.empty.hint')}</div>
    </div>
  {:else}
    <div class="edits-main">
      {#if hasGroups}
        <div class="group-section">
          <div class="group-header">
            <span class="group-label">{i18n.t('edits.group.earlierPending')}</span>
            <span class="group-count">{i18n.t('edits.group.earlierPendingCount', { count: earlierPendingEdits.length })}</span>
          </div>
          <ChangeFileTree
            edits={earlierPendingEdits}
            workspacePath={messagesState.currentWorkspacePath || ''}
            activeFilePath={activeCodeFilePath}
            changeMutationPending={changeMutationPending}
            onOpen={viewDiff}
            onApprove={approveChange}
            onRevert={revertChange}
          />
        </div>
      {/if}

      <div class="group-section">
        {#if displayedCurrentEdits.length > 0}
          <div class="group-header current-round">
            <span class="group-label">{i18n.t('edits.group.currentRound')}</span>
            <span class="group-count">{i18n.t('edits.group.currentRoundCount', { count: displayedCurrentEdits.length })}</span>
            <span
              class="group-diff-summary"
              aria-label={i18n.t('edits.group.currentRoundSummary', {
                count: displayedCurrentEdits.length,
                additions: currentRoundTotals.additions,
                deletions: currentRoundTotals.deletions,
              })}
            >
              <span class="stat-add">+{currentRoundTotals.additions}</span>
              <span class="stat-del">-{currentRoundTotals.deletions}</span>
            </span>
            <div class="group-actions">
              <button
                type="button"
                class="group-action approve"
                disabled={changeMutationPending}
                title={i18n.t('edits.actions.approveAllTitle')}
                onclick={approveAllChanges}
              >
                <Icon name="check" size={12} />
                <span>{i18n.t('edits.actions.approveAll')}</span>
              </button>
              <button
                type="button"
                class="group-action revert"
                disabled={changeMutationPending || !allEditsRevertible}
                title={allEditsRevertible ? i18n.t('edits.actions.revertAllTitle') : i18n.t('edits.actions.revertUnavailable')}
                onclick={revertAllChanges}
              >
                <Icon name="undo" size={12} />
                <span>{i18n.t('edits.actions.revertAll')}</span>
              </button>
              {#if currentRoundEdits.length > 0 && latestExecutionGroupId}
                <button
                  type="button"
                  class="group-action revert"
                  disabled={changeMutationPending || !currentRoundRevertible}
                  title={currentRoundRevertible ? i18n.t('edits.group.revertRoundTitle') : i18n.t('edits.actions.revertUnavailable')}
                  onclick={revertCurrentRound}
                >
                  <Icon name="undo" size={12} />
                  <span>{i18n.t('edits.group.revertRound')}</span>
                </button>
              {/if}
            </div>
          </div>
          <ChangeFileTree
            edits={displayedCurrentEdits}
            workspacePath={messagesState.currentWorkspacePath || ''}
            activeFilePath={activeCodeFilePath}
            changeMutationPending={changeMutationPending}
            onOpen={viewDiff}
            onApprove={approveChange}
            onRevert={revertChange}
          />
        {/if}
      </div>
    </div>
  {/if}
</div>

<style>
  .edits-panel {
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

  .changes-section-label {
    flex: 0 0 auto;
    padding: 4px calc(var(--space-2) + var(--space-1)) 0;
    color: var(--foreground-muted);
    font-size: var(--text-2xs);
    font-weight: var(--font-semibold);
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

  .group-actions {
    display: inline-flex;
    flex: 0 0 auto;
    align-items: center;
    gap: 2px;
    margin-left: auto;
  }

  .group-action {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 4px;
    height: 24px;
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

  .group-action.approve {
    border-color: color-mix(in srgb, var(--primary) 28%, transparent);
    background: var(--primary-muted);
    color: var(--primary);
  }

  .group-action.approve:hover:not(:disabled) {
    border-color: color-mix(in srgb, var(--primary) 44%, transparent);
    background: color-mix(in srgb, var(--primary) 22%, transparent);
  }

  .group-action.revert:hover:not(:disabled) {
    border-color: color-mix(in srgb, var(--error) 35%, transparent);
    background: color-mix(in srgb, var(--error) 10%, transparent);
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

  .group-diff-summary {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    color: var(--foreground-muted);
    font-size: var(--text-2xs);
    font-variant-numeric: tabular-nums;
    font-weight: var(--font-semibold);
  }

  .stat-add { color: var(--success); }
  .stat-del { color: var(--error); }
  @media (max-width: 768px) {
    .edits-panel {
      padding: var(--space-2);
    }

    .group-header.current-round {
      flex-wrap: wrap;
    }

    .group-actions {
      margin-left: auto;
    }

    .group-action {
      padding-inline: 6px;
    }

  }
</style>
