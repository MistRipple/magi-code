<script lang="ts">
  import { onMount } from 'svelte';
  import { listAgentDirectory, type DirectoryEntry } from './agent-api';

  interface Props {
    onSelect: (path: string, name: string) => void;
    onCancel: () => void;
    disabled?: boolean;
  }

  const { onSelect, onCancel, disabled = false }: Props = $props();

  let currentPath = $state('');
  let parentPath = $state('');
  let entries = $state<DirectoryEntry[]>([]);
  let loading = $state(true);
  let error = $state('');
  let selectedPath = $state('');
  let manualPathInput = $state('');
  let showManualInput = $state(false);
  let showHidden = $state(false);
  let hasLoaded = $state(false);
  let requestToken = 0;

  onMount(() => {
    void loadDirectory();
  });

  async function loadDirectory(dirPath?: string): Promise<void> {
    const token = ++requestToken;
    loading = true;
    error = '';
    selectedPath = '';

    try {
      const result = await listAgentDirectory(dirPath, showHidden);
      if (token !== requestToken) {
        return;
      }
      if (result.error) {
        error = result.error;
        return;
      }
      currentPath = result.path;
      parentPath = result.parent;
      entries = result.entries;
      manualPathInput = result.path;
      hasLoaded = true;
    } catch (err) {
      if (token !== requestToken) {
        return;
      }
      error = err instanceof Error ? err.message : String(err);
    } finally {
      if (token === requestToken) {
        loading = false;
      }
    }
  }

  function enterDirectory(entry: DirectoryEntry): void {
    void loadDirectory(entry.path);
  }

  function toggleShowHidden(): void {
    showHidden = !showHidden;
    void loadDirectory(currentPath || undefined);
  }

  function goUp(): void {
    const resolvedCurrent = currentPath.trim();
    const resolvedParent = parentPath.trim();
    if (!resolvedCurrent || !resolvedParent || resolvedCurrent === resolvedParent) {
      return;
    }
    void loadDirectory(resolvedParent);
  }

  function selectEntry(entry: DirectoryEntry): void {
    selectedPath = entry.path;
  }

  function handleDblClick(entry: DirectoryEntry): void {
    enterDirectory(entry);
  }

  function getPathSegments(rawPath: string): string[] {
    return rawPath
      .replace(/\\/g, '/')
      .split('/')
      .filter(Boolean);
  }

  function getPathBasename(rawPath: string): string {
    const normalized = rawPath.trim();
    if (!normalized) {
      return '';
    }
    const segments = getPathSegments(normalized);
    if (segments.length > 0) {
      return segments[segments.length - 1];
    }
    return normalized === '/' ? '/' : normalized.replace(/\/+$/, '');
  }

  function getPathDisplay(rawPath: string): string {
    const normalized = rawPath.trim();
    return normalized || '正在定位目录';
  }

  function confirmSelection(): void {
    const targetPath = selectedPath || currentPath;
    if (!targetPath) {
      return;
    }
    onSelect(targetPath, getPathBasename(targetPath));
  }

  function selectCurrentDir(): void {
    if (!currentPath) {
      return;
    }
    onSelect(currentPath, getPathBasename(currentPath));
  }

  function toggleManualInput(): void {
    showManualInput = !showManualInput;
    if (showManualInput) {
      manualPathInput = currentPath;
    }
  }

  function goToManualPath(): void {
    const target = manualPathInput.trim();
    if (!target) {
      return;
    }
    showManualInput = false;
    void loadDirectory(target);
  }

  function handleManualInputKeydown(event: KeyboardEvent): void {
    if (event.key === 'Enter') {
      goToManualPath();
    }
    if (event.key === 'Escape') {
      showManualInput = false;
    }
  }

  function retryLoad(): void {
    void loadDirectory(currentPath || undefined);
  }

  const canGoUp = $derived(
    !loading
      && !!currentPath.trim()
      && !!parentPath.trim()
      && currentPath.trim() !== parentPath.trim()
  );

  const currentSelectionPath = $derived(selectedPath || currentPath);
</script>

<div class="fp">
  <div class="fp-toolbar">
    <div class="fp-location">{getPathDisplay(currentPath)}</div>
    <div class="fp-toolbar-actions">
      <button class="fp-icon-btn" type="button" onclick={goUp} disabled={!canGoUp} title="返回上级目录">
        <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">
          <path d="M8 12V4M4.5 7.5 8 4l3.5 3.5"/>
        </svg>
      </button>
      <button class="fp-icon-btn" type="button" onclick={toggleManualInput} disabled={loading} title="手动输入路径">
        <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">
          <path d="M9.5 2.5l4 4M2 14l1-4L11.5 1.5l4 4L7 14l-4 1z"/>
        </svg>
      </button>
    </div>
  </div>

  {#if showManualInput}
    <div class="fp-path-editor">
      <input
        class="fp-path-input"
        type="text"
        bind:value={manualPathInput}
        onkeydown={handleManualInputKeydown}
        placeholder="输入完整路径，按 Enter 跳转"
      />
      <button class="fp-inline-btn" type="button" onclick={goToManualPath} disabled={loading || !manualPathInput.trim()}>
        跳转
      </button>
      <button class="fp-inline-btn fp-inline-btn--ghost" type="button" onclick={() => { showManualInput = false; }}>
        取消
      </button>
    </div>
  {/if}

  <div class="fp-list">
    {#if loading}
      <div class="fp-state">
        <div class="fp-state-text">正在读取目录...</div>
      </div>
    {:else if error}
      <div class="fp-state fp-state--error">
        <div class="fp-state-text">{error}</div>
        <div class="fp-state-actions">
          <button class="fp-inline-btn" type="button" onclick={retryLoad}>重试</button>
          <button class="fp-inline-btn fp-inline-btn--ghost" type="button" onclick={toggleManualInput}>手动输入</button>
        </div>
      </div>
    {:else if entries.length === 0}
      <div class="fp-state">
        <div class="fp-state-text">当前目录下没有子目录</div>
      </div>
    {:else}
      {#each entries as entry (entry.path)}
        <button
          class="fp-item"
          class:fp-item--selected={selectedPath === entry.path}
          type="button"
          onclick={() => selectEntry(entry)}
          ondblclick={() => handleDblClick(entry)}
        >
          <svg class="fp-item-icon" width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round">
            <path d="M1.5 3.5V12a1.5 1.5 0 0 0 1.5 1.5h10a1.5 1.5 0 0 0 1.5-1.5V5.5A1.5 1.5 0 0 0 13 4H8.5L7 2.5H3A1.5 1.5 0 0 0 1.5 3.5z"/>
          </svg>
          <span class="fp-item-name">{entry.name}</span>
          {#if entry.hasChildren}
            <svg class="fp-item-chevron" width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
              <path d="M6 3l5 5-5 5"/>
            </svg>
          {/if}
        </button>
      {/each}
    {/if}
  </div>

  <div class="fp-footer">
    <div class="fp-footer-top">
      <div class="fp-selection">
        <svg class="fp-selection-icon" width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round">
          <path d="M1.5 3.5V12a1.5 1.5 0 0 0 1.5 1.5h10a1.5 1.5 0 0 0 1.5-1.5V5.5A1.5 1.5 0 0 0 13 4H8.5L7 2.5H3A1.5 1.5 0 0 0 1.5 3.5z"/>
        </svg>
        <span class="fp-selection-path">{getPathDisplay(currentSelectionPath)}</span>
      </div>
      <label class="fp-hidden-toggle">
        <input type="checkbox" checked={showHidden} onchange={toggleShowHidden} />
        <span>显示隐藏目录</span>
      </label>
    </div>
    <div class="fp-footer-actions">
      <button class="modal-btn secondary" type="button" onclick={onCancel} disabled={disabled}>取消</button>
      {#if selectedPath}
        <button class="modal-btn primary" type="button" onclick={confirmSelection} disabled={disabled || loading}>确认选择</button>
      {:else}
        <button class="modal-btn primary" type="button" onclick={selectCurrentDir} disabled={disabled || loading || !hasLoaded}>选择当前目录</button>
      {/if}
    </div>
  </div>
</div>

<style>
  .fp {
    display: flex;
    flex-direction: column;
    height: 460px;
    min-width: 0;
  }

  /* ── 顶栏：当前路径 + 操作按钮 ── */

  .fp-toolbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    padding: 12px 16px;
    border-bottom: 1px solid var(--border, #273142);
    flex-shrink: 0;
  }

  .fp-location {
    flex: 1;
    min-width: 0;
    color: var(--foreground-muted, #98a2b3);
    font-size: 12px;
    line-height: 1.4;
    font-family: var(--font-mono, monospace);
    word-break: break-all;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .fp-toolbar-actions {
    display: flex;
    align-items: center;
    gap: 4px;
    flex-shrink: 0;
  }

  .fp-icon-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
    border: 1px solid var(--border, #273142);
    border-radius: var(--radius-md, 6px);
    background: transparent;
    color: var(--foreground-muted, #98a2b3);
    cursor: pointer;
    flex-shrink: 0;
  }

  .fp-icon-btn:hover:not(:disabled) {
    color: var(--foreground, #e5e7eb);
    background: var(--surface-hover, rgba(255, 255, 255, 0.06));
  }

  .fp-icon-btn:disabled {
    opacity: 0.3;
    cursor: not-allowed;
  }

  /* ── 手动输入路径 ── */

  .fp-path-editor {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 8px 16px;
    border-bottom: 1px solid var(--border, #273142);
    flex-shrink: 0;
  }

  .fp-path-input {
    flex: 1;
    min-width: 0;
    height: 32px;
    padding: 0 10px;
    border: 1px solid var(--border, #273142);
    border-radius: var(--radius-md, 6px);
    background: var(--surface-1, #11161d);
    color: var(--foreground, #e5e7eb);
    font-size: 12px;
    font-family: var(--font-mono, monospace);
    outline: none;
  }

  .fp-path-input:focus {
    border-color: var(--info, #3b82f6);
  }

  .fp-inline-btn {
    height: 32px;
    padding: 0 10px;
    border: 1px solid var(--border, #273142);
    border-radius: var(--radius-md, 6px);
    background: var(--surface-2, rgba(255, 255, 255, 0.04));
    color: var(--foreground, #e5e7eb);
    font-size: 12px;
    cursor: pointer;
    white-space: nowrap;
  }

  .fp-inline-btn:hover:not(:disabled) {
    background: var(--surface-hover, rgba(255, 255, 255, 0.06));
  }

  .fp-inline-btn:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .fp-inline-btn--ghost {
    border-color: transparent;
    background: transparent;
    color: var(--foreground-muted, #98a2b3);
  }

  .fp-inline-btn--ghost:hover:not(:disabled) {
    background: var(--surface-hover, rgba(255, 255, 255, 0.06));
  }

  /* ── 目录列表 ── */

  .fp-list {
    flex: 1;
    overflow-y: auto;
    overscroll-behavior: contain;
    -webkit-overflow-scrolling: touch;
  }

  .fp-item {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 7px 16px;
    border: none;
    border-bottom: 1px solid color-mix(in srgb, var(--border, #273142) 50%, transparent);
    background: transparent;
    color: var(--foreground, #e5e7eb);
    font-size: 13px;
    text-align: left;
    cursor: pointer;
    user-select: none;
  }

  .fp-item:last-child {
    border-bottom: none;
  }

  .fp-item:hover {
    background: var(--surface-hover, rgba(255, 255, 255, 0.04));
  }

  .fp-item--selected {
    background: color-mix(in srgb, var(--info, #3b82f6) 12%, transparent);
  }

  .fp-item--selected:hover {
    background: color-mix(in srgb, var(--info, #3b82f6) 16%, transparent);
  }

  .fp-item-icon {
    flex-shrink: 0;
    color: var(--foreground-muted, #98a2b3);
    opacity: 0.6;
  }

  .fp-item--selected .fp-item-icon {
    color: var(--info, #3b82f6);
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
    opacity: 0.35;
  }

  .fp-item:hover .fp-item-chevron {
    opacity: 0.7;
  }

  /* ── 空/错误状态 ── */

  .fp-state {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 8px;
    min-height: 120px;
    padding: 24px 20px;
    text-align: center;
  }

  .fp-state-text {
    max-width: 380px;
    color: var(--foreground-muted, #98a2b3);
    font-size: 13px;
    line-height: 1.5;
    word-break: break-word;
  }

  .fp-state-actions {
    display: flex;
    gap: 6px;
    margin-top: 4px;
  }

  .fp-state--error .fp-state-text {
    color: var(--error, #ef4444);
  }

  /* ── 底栏：选中路径 + 操作按钮 ── */

  .fp-footer {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 12px 16px;
    border-top: 1px solid var(--border, #273142);
    flex-shrink: 0;
  }

  .fp-footer-top {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
  }

  .fp-selection {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    flex: 1;
  }

  .fp-selection-icon {
    flex-shrink: 0;
    color: var(--info, #3b82f6);
    opacity: 0.8;
  }

  .fp-selection-path {
    flex: 1;
    min-width: 0;
    font-size: 12px;
    color: var(--foreground, #e5e7eb);
    font-family: var(--font-mono, monospace);
    word-break: break-all;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .fp-footer-actions {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: 8px;
  }

  .fp-hidden-toggle {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-shrink: 0;
    font-size: 12px;
    color: var(--foreground-muted, #98a2b3);
    cursor: pointer;
    user-select: none;
    white-space: nowrap;
  }

  .fp-hidden-toggle input[type="checkbox"] {
    width: 14px;
    height: 14px;
    margin: 0;
    accent-color: var(--info, #3b82f6);
    cursor: pointer;
  }
</style>
