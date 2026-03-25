<script lang="ts">
  import { listAgentDirectory, type DirectoryEntry } from './agent-api';

  interface Props {
    onSelect: (path: string, name: string) => void;
    onCancel: () => void;
    disabled?: boolean;
  }

  const { onSelect, onCancel, disabled = false }: Props = $props();

  // 当前浏览路径
  let currentPath = $state('');
  let entries = $state<DirectoryEntry[]>([]);
  let loading = $state(true);
  let error = $state('');
  // 选中的目录路径
  let selectedPath = $state('');
  // 手动输入路径
  let manualPathInput = $state('');
  let showManualInput = $state(false);

  // 初始加载：用户 home 目录
  $effect(() => {
    void loadDirectory();
  });

  async function loadDirectory(dirPath?: string): Promise<void> {
    loading = true;
    error = '';
    selectedPath = '';
    try {
      const result = await listAgentDirectory(dirPath);
      if (result.error) {
        error = result.error;
        return;
      }
      currentPath = result.path;
      entries = result.entries;
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    } finally {
      loading = false;
    }
  }

  // 进入子目录
  function enterDirectory(entry: DirectoryEntry): void {
    void loadDirectory(entry.path);
  }

  // 返回上级目录
  function goUp(): void {
    // 利用 path 自身计算父目录
    const parts = currentPath.replace(/\/$/, '').split('/');
    if (parts.length <= 1) {
      // 已经在根目录
      void loadDirectory('/');
      return;
    }
    parts.pop();
    const parent = parts.join('/') || '/';
    void loadDirectory(parent);
  }

  // 选中一个目录（不进入）
  function selectEntry(entry: DirectoryEntry): void {
    selectedPath = entry.path;
  }

  // 双击进入
  function handleDblClick(entry: DirectoryEntry): void {
    enterDirectory(entry);
  }

  // 确认选择当前选中的目录
  function confirmSelection(): void {
    const targetPath = selectedPath || currentPath;
    if (!targetPath) return;
    const name = targetPath.split('/').filter(Boolean).pop() || targetPath;
    onSelect(targetPath, name);
  }

  // 选择当前目录本身
  function selectCurrentDir(): void {
    if (!currentPath) return;
    const name = currentPath.split('/').filter(Boolean).pop() || currentPath;
    onSelect(currentPath, name);
  }

  // 切换手动输入模式
  function toggleManualInput(): void {
    showManualInput = !showManualInput;
    if (showManualInput) {
      manualPathInput = currentPath;
    }
  }

  // 手动路径跳转
  function goToManualPath(): void {
    const target = manualPathInput.trim();
    if (!target) return;
    showManualInput = false;
    void loadDirectory(target);
  }

  function handleManualInputKeydown(e: KeyboardEvent): void {
    if (e.key === 'Enter') {
      goToManualPath();
    }
    if (e.key === 'Escape') {
      showManualInput = false;
    }
  }
</script>

<div class="fp">
  <!-- 路径导航栏 -->
  <div class="fp-nav">
    <button class="fp-icon-btn" type="button" onclick={goUp} disabled={loading} title="返回上级目录">
      <svg width="16" height="16" viewBox="0 0 16 16" fill="currentColor">
        <path d="M7.646 4.646a.5.5 0 0 1 .708 0l6 6a.5.5 0 0 1-.708.708L8 5.707l-5.646 5.647a.5.5 0 0 1-.708-.708l6-6z"/>
      </svg>
    </button>
    {#if showManualInput}
      <input
        class="fp-path-input"
        type="text"
        bind:value={manualPathInput}
        onkeydown={handleManualInputKeydown}
        placeholder="输入路径，按 Enter 跳转"
      />
      <button class="fp-icon-btn" type="button" onclick={() => { showManualInput = false; }} title="取消">
        <svg width="16" height="16" viewBox="0 0 16 16" fill="currentColor">
          <path d="M4.646 4.646a.5.5 0 0 1 .708 0L8 7.293l2.646-2.647a.5.5 0 0 1 .708.708L8.707 8l2.647 2.646a.5.5 0 0 1-.708.708L8 8.707l-2.646 2.647a.5.5 0 0 1-.708-.708L7.293 8 4.646 5.354a.5.5 0 0 1 0-.708z"/>
        </svg>
      </button>
    {:else}
      <button class="fp-path" type="button" onclick={toggleManualInput} title="点击编辑路径">
        {currentPath || '...'}
      </button>
    {/if}
  </div>

  <!-- 目录列表 -->
  <div class="fp-list">
    {#if loading}
      <div class="fp-empty">加载中…</div>
    {:else if error}
      <div class="fp-empty fp-empty--error">{error}</div>
    {:else if entries.length === 0}
      <div class="fp-empty">此目录为空</div>
    {:else}
      {#each entries as entry (entry.path)}
        <button
          class="fp-item"
          class:fp-item--selected={selectedPath === entry.path}
          type="button"
          onclick={() => selectEntry(entry)}
          ondblclick={() => handleDblClick(entry)}
        >
          <svg class="fp-item-icon" width="16" height="16" viewBox="0 0 16 16" fill="currentColor">
            <path d="M.54 3.87.5 3a2 2 0 0 1 2-2h3.672a2 2 0 0 1 1.414.586l.828.828A2 2 0 0 0 9.828 3H13.5a2 2 0 0 1 2 2v7a2 2 0 0 1-2 2H2a2 2 0 0 1-2-2V4.172a2 2 0 0 1 .554-1.382l-.014-.03z"/>
          </svg>
          <span class="fp-item-name">{entry.name}</span>
          {#if entry.hasChildren}
            <svg class="fp-item-chevron" width="16" height="16" viewBox="0 0 16 16" fill="currentColor">
              <path d="M4.646 1.646a.5.5 0 0 1 .708 0l6 6a.5.5 0 0 1 0 .708l-6 6a.5.5 0 0 1-.708-.708L10.293 8 4.646 2.354a.5.5 0 0 1 0-.708z"/>
            </svg>
          {/if}
        </button>
      {/each}
    {/if}
  </div>

  <!-- 底部操作栏 -->
  <div class="fp-footer">
    <span class="fp-selection">
      {#if selectedPath}
        {selectedPath.split('/').filter(Boolean).pop()}
      {:else}
        {currentPath.split('/').filter(Boolean).pop() || '/'}
      {/if}
    </span>
    <div class="fp-actions">
      <button class="modal-btn secondary" type="button" onclick={onCancel} disabled={disabled}>取消</button>
      {#if selectedPath}
        <button class="modal-btn primary" type="button" onclick={confirmSelection} disabled={disabled || loading}>选择</button>
      {:else}
        <button class="modal-btn primary" type="button" onclick={selectCurrentDir} disabled={disabled || loading}>选择当前目录</button>
      {/if}
    </div>
  </div>
</div>

<style>
  .fp {
    display: flex;
    flex-direction: column;
    height: 420px;
    min-width: 0;
  }

  .fp-nav {
    display: flex;
    align-items: center;
    gap: var(--space-1, 4px);
    padding: var(--space-2, 8px) var(--space-3, 12px);
    border-bottom: 1px solid var(--border, #273142);
    flex-shrink: 0;
  }

  .fp-icon-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
    border: none;
    border-radius: var(--radius-md, 6px);
    background: transparent;
    color: var(--foreground-muted, #98a2b3);
    cursor: pointer;
    flex-shrink: 0;
  }

  .fp-icon-btn:hover:not(:disabled) {
    color: var(--foreground, #e5e7eb);
    background: var(--surface-2, rgba(255, 255, 255, 0.04));
  }

  .fp-icon-btn:disabled {
    opacity: 0.35;
    cursor: not-allowed;
  }

  .fp-path {
    flex: 1;
    min-width: 0;
    padding: var(--space-1, 4px) var(--space-2, 8px);
    border: 1px solid transparent;
    border-radius: var(--radius-md, 6px);
    background: transparent;
    color: var(--foreground-muted, #98a2b3);
    font-size: var(--text-sm, 13px);
    font-family: var(--font-mono, monospace);
    text-align: left;
    cursor: pointer;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .fp-path:hover {
    background: var(--surface-2, rgba(255, 255, 255, 0.04));
  }

  .fp-path-input {
    flex: 1;
    min-width: 0;
    height: 28px;
    padding: 0 var(--space-2, 8px);
    border: 1px solid var(--primary, #2563eb);
    border-radius: var(--radius-md, 6px);
    background: var(--surface-1, #11161d);
    color: var(--foreground, #e5e7eb);
    font-size: var(--text-sm, 13px);
    font-family: var(--font-mono, monospace);
    outline: none;
  }

  .fp-list {
    flex: 1;
    overflow-y: auto;
    padding: var(--space-1, 4px) 0;
  }

  .fp-item {
    display: flex;
    align-items: center;
    gap: var(--space-2, 8px);
    width: 100%;
    padding: 6px var(--space-3, 12px);
    border: none;
    background: transparent;
    color: var(--foreground, #e5e7eb);
    font-size: var(--text-sm, 13px);
    text-align: left;
    cursor: pointer;
    user-select: none;
  }

  .fp-item:hover {
    background: var(--surface-2, rgba(255, 255, 255, 0.04));
  }

  .fp-item--selected {
    background: color-mix(in srgb, var(--primary, #2563eb) 15%, transparent);
  }

  .fp-item--selected:hover {
    background: color-mix(in srgb, var(--primary, #2563eb) 20%, transparent);
  }

  .fp-item-icon {
    flex-shrink: 0;
    color: var(--foreground-muted, #98a2b3);
    opacity: 0.6;
  }

  .fp-item--selected .fp-item-icon {
    color: var(--primary, #2563eb);
    opacity: 1;
  }

  .fp-item-name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .fp-item-chevron {
    flex-shrink: 0;
    color: var(--foreground-muted, #98a2b3);
    opacity: 0.4;
  }

  .fp-item:hover .fp-item-chevron {
    opacity: 0.7;
  }

  .fp-empty {
    padding: var(--space-6, 24px) var(--space-4, 16px);
    text-align: center;
    color: var(--foreground-muted, #98a2b3);
    font-size: var(--text-sm, 13px);
  }

  .fp-empty--error {
    color: var(--error, #ef4444);
  }

  .fp-footer {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3, 12px);
    padding: var(--space-2, 8px) var(--space-3, 12px);
    border-top: 1px solid var(--border, #273142);
    flex-shrink: 0;
  }

  .fp-selection {
    flex: 1;
    min-width: 0;
    font-size: var(--text-sm, 13px);
    color: var(--foreground, #e5e7eb);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .fp-actions {
    display: flex;
    gap: var(--space-2, 8px);
    flex-shrink: 0;
  }
</style>

