<script lang="ts">
  import { getState } from '../stores/messages.svelte';
  import { vscode } from '../lib/vscode-bridge';
  import { ensureArray } from '../lib/utils';
  import type { Edit } from '../types/message';
  import Icon from './Icon.svelte';
  import WorkerBadge from './WorkerBadge.svelte';

  const appState = getState();

  const edits = $derived(ensureArray(appState.edits) as Edit[]);

  // 统计汇总
  const totalAdditions = $derived(edits.reduce((s, e) => s + (e.additions ?? 0), 0));
  const totalDeletions = $derived(edits.reduce((s, e) => s + (e.deletions ?? 0), 0));
  const addedCount = $derived(edits.filter(e => e.type === 'add').length);
  const modifiedCount = $derived(edits.filter(e => e.type === 'modify').length);
  const deletedCount = $derived(edits.filter(e => e.type === 'delete').length);

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
  function getFileIconName(filePath: string): 'file-plus' | 'file-minus' | 'file-edit' | 'file-text' {
    const edit = edits.find(e => e.filePath === filePath);
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

  function openFile(filePath: string) { vscode.postMessage({ type: 'openFile', filepath: filePath }); }
  function approveChange(filePath: string) { vscode.postMessage({ type: 'approveChange', filePath }); }
  function revertChange(filePath: string) { vscode.postMessage({ type: 'revertChange', filePath }); }
  function viewDiff(filePath: string) { vscode.postMessage({ type: 'viewDiff', filePath }); }
  function approveAllChanges() { vscode.postMessage({ type: 'approveAllChanges' }); }
  function revertAllChanges() { vscode.postMessage({ type: 'revertAllChanges' }); }
</script>

<div class="edits-panel">
  {#if edits.length === 0}
    <div class="empty-state">
      <Icon name="file-edit" size={32} />
      <div class="empty-text">暂无变更</div>
      <div class="empty-hint">Worker 产生的代码变更会在此显示</div>
    </div>
  {:else}
    <!-- 顶部统计条 -->
    <div class="summary-bar">
      <div class="summary-left">
        <span class="summary-count">{edits.length} 个文件</span>
        {#if addedCount > 0}<span class="summary-chip add">{addedCount} 新增</span>{/if}
        {#if modifiedCount > 0}<span class="summary-chip modify">{modifiedCount} 修改</span>{/if}
        {#if deletedCount > 0}<span class="summary-chip del">{deletedCount} 删除</span>{/if}
      </div>
      <div class="summary-right">
        <span class="stat-add">+{totalAdditions}</span>
        <span class="stat-del">-{totalDeletions}</span>
      </div>
    </div>

    <!-- 批量操作 -->
    <div class="bulk-actions">
      <button class="bulk-btn approve" onclick={approveAllChanges} title="批准所有变更">
        <Icon name="check-circle" size={13} />
        <span>全部批准</span>
      </button>
      <button class="bulk-btn revert" onclick={revertAllChanges} title="还原所有变更">
        <Icon name="undo" size={13} />
        <span>全部还原</span>
      </button>
    </div>

    <!-- 文件列表 -->
    <div class="file-list">
      {#each edits as edit (edit.filePath)}
        {@const { dir, name } = splitPath(edit.filePath)}
        {@const blocks = getChangeBlocks(edit.additions ?? 0, edit.deletions ?? 0)}
        {@const contributors = getContributors(edit)}
        <div class="file-row" role="button" tabindex="0" onclick={() => viewDiff(edit.filePath)} onkeydown={(e) => e.key === 'Enter' && viewDiff(edit.filePath)}>
          <!-- 变更类型指示器 -->
          <div class="type-indicator" class:add={edit.type === 'add'} class:modify={edit.type === 'modify'} class:del={edit.type === 'delete'}></div>

          <!-- 文件图标 -->
          <div class="file-icon" class:add={edit.type === 'add'} class:modify={edit.type === 'modify'} class:del={edit.type === 'delete'}>
            <Icon name={getFileIconName(edit.filePath)} size={14} />
          </div>

          <!-- 文件名与路径 -->
          <div class="file-info">
            <span class="file-name">{name}</span>
            {#if dir}
              <span class="file-dir">{dir}</span>
            {/if}
          </div>

          <!-- Worker 贡献者 -->
          {#if contributors.length > 0}
            <div class="file-workers">
              {#each contributors as worker}
                <WorkerBadge {worker} size="sm" />
              {/each}
            </div>
          {/if}

          <!-- 增删统计 + 比例条 -->
          <div class="file-stats">
            {#if edit.additions}<span class="stat-add">+{edit.additions}</span>{/if}
            {#if edit.deletions}<span class="stat-del">-{edit.deletions}</span>{/if}
            <div class="change-blocks">
              {#each blocks as block}
                <span class="block" class:add={block === 'add'} class:del={block === 'del'} class:neutral={block === 'neutral'}></span>
              {/each}
            </div>
          </div>

          <!-- hover 操作按钮 -->
          <div class="file-actions">
            <button class="action-icon" title="打开文件" onclick={(e) => { e.stopPropagation(); openFile(edit.filePath); }}>
              <Icon name="file-text" size={14} />
            </button>
            <button class="action-icon approve" title="批准变更" onclick={(e) => { e.stopPropagation(); approveChange(edit.filePath); }}>
              <Icon name="check" size={14} />
            </button>
            <button class="action-icon revert" title="还原变更" onclick={(e) => { e.stopPropagation(); revertChange(edit.filePath); }}>
              <Icon name="undo" size={14} />
            </button>
          </div>
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  .edits-panel {
    height: 100%;
    min-height: 0; /* flex 布局防溢出 */
    overflow-y: auto;
    padding: var(--space-3);
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
    background: var(--surface-1);
    border-radius: var(--radius-md);
    border: 1px solid var(--border);
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
  }

  .bulk-btn {
    display: inline-flex;
    align-items: center;
    gap: var(--space-1);
    padding: 4px 10px;
    border-radius: var(--radius-sm);
    border: 1px solid var(--border);
    background: var(--surface-1);
    color: var(--foreground-muted);
    cursor: pointer;
    font-size: var(--text-xs);
    transition: all var(--transition-fast);
  }

  .bulk-btn:hover {
    background: var(--surface-hover);
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
    background: var(--border);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    overflow: hidden;
  }

  .file-row {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    background: var(--surface-1);
    cursor: pointer;
    transition: background var(--transition-fast);
    position: relative;
  }

  .file-row:hover {
    background: var(--surface-hover);
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

  .file-row:hover .file-actions {
    opacity: 1;
  }

  .action-icon {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 24px;
    height: 24px;
    border-radius: var(--radius-sm);
    border: none;
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    transition: all var(--transition-fast);
  }

  .action-icon:hover {
    background: var(--surface-active);
    color: var(--foreground);
  }

  .action-icon.approve:hover { color: var(--success); }
  .action-icon.revert:hover { color: var(--error); }
</style>
