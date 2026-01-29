<script lang="ts">
  import { getState } from '../stores/messages.svelte';
  import { vscode } from '../lib/vscode-bridge';
  import { ensureArray } from '../lib/utils';
  import WorkerBadge from './WorkerBadge.svelte';

  const appState = getState();

  // 变更列表
  const edits = $derived(ensureArray(appState.edits));

  function getContributors(edit: any): string[] {
    if (Array.isArray(edit?.contributors) && edit.contributors.length > 0) {
      return edit.contributors;
    }
    if (edit?.workerId) {
      return [edit.workerId];
    }
    return [];
  }

  // 打开文件
  function openFile(filePath: string) {
    vscode.postMessage({ type: 'openFile', filepath: filePath });
  }

  function approveChange(filePath: string) {
    vscode.postMessage({ type: 'approveChange', filePath });
  }

  function revertChange(filePath: string) {
    vscode.postMessage({ type: 'revertChange', filePath });
  }

  function viewDiff(filePath: string) {
    vscode.postMessage({ type: 'viewDiff', filePath });
  }

  function approveAllChanges() {
    vscode.postMessage({ type: 'approveAllChanges' });
  }

  function revertAllChanges() {
    vscode.postMessage({ type: 'revertAllChanges' });
  }

  // 获取文件类型图标
  function getFileIcon(filePath: string): string {
    const ext = filePath.split('.').pop()?.toLowerCase() || '';
    const iconMap: Record<string, string> = {
      ts: 'M0 0h16v16H0z M1 3h14v10H1z M4 7h3v1H4z M8 7h4v1H8z',
      js: 'M0 0h16v16H0z M1 3h14v10H1z',
      svelte: 'M8 0a8 8 0 1 0 0 16A8 8 0 0 0 8 0z',
      vue: 'M8 0L0 14h16L8 0z',
      css: 'M0 0h16v16H0z M2 4h12v8H2z',
      json: 'M2 2v12h12V2H2zm11 11H3V3h10v10z',
    };
    return iconMap[ext] || 'M14 4.5V14a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V2a2 2 0 0 1 2-2h5.5L14 4.5z';
  }
</script>

<div class="edits-panel">
  <div class="edits-content">
    {#if edits.length === 0}
      <div class="empty-state">
        <svg class="empty-icon" viewBox="0 0 16 16">
          <path d="M12.854.146a.5.5 0 0 0-.707 0L10.5 1.793 14.207 5.5l1.647-1.646a.5.5 0 0 0 0-.708l-3-3zm.646 6.061L9.793 2.5 3.293 9H3.5a.5.5 0 0 1 .5.5v.5h.5a.5.5 0 0 1 .5.5v.5h.5a.5.5 0 0 1 .5.5v.5h.5a.5.5 0 0 1 .5.5v.207l6.5-6.5zm-7.468 7.468A.5.5 0 0 1 6 13.5V13h-.5a.5.5 0 0 1-.5-.5V12h-.5a.5.5 0 0 1-.5-.5V11h-.5a.5.5 0 0 1-.5-.5V10h-.5a.499.499 0 0 1-.175-.032l-.179.178a.5.5 0 0 0-.11.168l-2 5a.5.5 0 0 0 .65.65l5-2a.5.5 0 0 0 .168-.11l.178-.178z"/>
        </svg>
        <div class="empty-text">暂无变更</div>
        <div class="empty-hint">代码变更会在此显示</div>
      </div>
    {:else}
      <div class="edits-actions">
        <button class="action-btn approve-all" onclick={approveAllChanges}>全部批准</button>
        <button class="action-btn revert-all" onclick={revertAllChanges}>全部还原</button>
      </div>
      <div class="edits-list">
        {#each edits as edit}
          <div class="edit-item">
            <button class="edit-main" onclick={() => openFile(edit.filePath)}>
              <div class="edit-icon">
                <svg viewBox="0 0 16 16">
                  <path d={getFileIcon(edit.filePath)}/>
                </svg>
              </div>
              <div class="edit-info">
                <div class="edit-path">{edit.filePath}</div>
                {#if edit.type}
                  <div class="edit-type" class:added={edit.type === 'add'} class:modified={edit.type === 'modify'} class:deleted={edit.type === 'delete'}>
                    {edit.type === 'add' ? '新增' : edit.type === 'modify' ? '修改' : '删除'}
                  </div>
                {/if}
              </div>
              <div class="edit-stats">
                {#if edit.additions}
                  <span class="stat-add">+{edit.additions}</span>
                {/if}
                {#if edit.deletions}
                  <span class="stat-del">-{edit.deletions}</span>
                {/if}
              </div>
            </button>
            <div class="edit-actions">
              <div class="edit-workers">
                {#each getContributors(edit) as worker}
                  <WorkerBadge worker={worker} size="sm" />
                {/each}
              </div>
              <button class="action-btn diff" onclick={() => viewDiff(edit.filePath)}>对比</button>
              <button class="action-btn approve" onclick={() => approveChange(edit.filePath)}>批准</button>
              <button class="action-btn revert" onclick={() => revertChange(edit.filePath)}>还原</button>
            </div>
          </div>
        {/each}
      </div>
    {/if}
  </div>
</div>

<style>
  .edits-panel {
    height: 100%;
    overflow-y: auto;
    padding: var(--space-4);
  }

  .empty-state {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    padding: var(--space-8) var(--space-5);
    color: var(--foreground-muted);
    text-align: center;
  }

  .empty-icon {
    width: var(--icon-2xl);
    height: var(--icon-2xl);
    fill: currentColor;
    opacity: 0.3;
    margin-bottom: var(--space-4);
  }

  .empty-text {
    font-size: var(--text-base);
    font-weight: var(--font-medium);
    color: var(--foreground);
    margin-bottom: var(--space-2);
  }

  .empty-hint {
    font-size: var(--text-sm);
    opacity: 0.7;
  }

  .edits-list {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  .edits-actions {
    display: flex;
    gap: var(--space-2);
    margin-bottom: var(--space-3);
  }

  .action-btn {
    padding: 6px 10px;
    border-radius: var(--radius-md);
    border: 1px solid var(--border);
    background: var(--surface-1);
    color: var(--foreground);
    cursor: pointer;
    font-size: var(--text-xs);
  }

  .action-btn:hover {
    background: var(--surface-hover);
    border-color: var(--primary);
  }

  .action-btn.approve-all,
  .action-btn.approve {
    color: var(--success);
    border-color: color-mix(in srgb, var(--success) 40%, var(--border));
  }

  .action-btn.revert-all,
  .action-btn.revert {
    color: var(--error);
    border-color: color-mix(in srgb, var(--error) 40%, var(--border));
  }

  .edit-item {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    background: var(--surface-1);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    text-align: left;
    transition: all var(--transition-fast);
  }

  .edit-main {
    flex: 1;
    display: flex;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-3) var(--space-4);
    background: transparent;
    border: none;
    cursor: pointer;
    text-align: left;
  }

  .edit-item:hover {
    background: var(--surface-hover);
    border-color: var(--primary);
  }

  .edit-actions {
    display: flex;
    gap: var(--space-2);
    padding-right: var(--space-3);
    align-items: center;
    flex-wrap: wrap;
  }

  .edit-workers {
    display: flex;
    gap: var(--space-2);
    flex-wrap: wrap;
  }

  .edit-icon {
    flex-shrink: 0;
    width: var(--icon-lg);
    height: var(--icon-lg);
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--foreground-muted);
  }

  .edit-icon svg {
    width: var(--icon-md);
    height: var(--icon-md);
    fill: currentColor;
  }

  .edit-info {
    flex: 1;
    min-width: 0;
  }

  .edit-path {
    font-size: var(--text-sm);
    color: var(--foreground);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .edit-type {
    font-size: var(--text-xs);
    margin-top: var(--space-1);
  }

  .edit-type.added { color: var(--success); }
  .edit-type.modified { color: var(--warning); }
  .edit-type.deleted { color: var(--error); }

  .edit-stats {
    display: flex;
    gap: var(--space-2);
    font-size: var(--text-xs);
    font-weight: var(--font-medium);
  }

  .stat-add { color: var(--success); }
  .stat-del { color: var(--error); }
</style>
