<script lang="ts">
  import { onMount } from 'svelte';
  import { listAgentDirectory, type DirectoryEntry } from './agent-api';
  import Icon from '../components/Icon.svelte';

  interface Props {
    title?: string;
    onSelect: (path: string, name: string) => void;
    onCancel: () => void;
    disabled?: boolean;
  }

  const { title, onSelect, onCancel, disabled = false }: Props = $props();

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

  function buildPathFromSegments(segments: string[], upToIndex: number): string {
    const isWindows = currentPath.includes('\\') || (segments.length > 0 && /^[A-Za-z]:$/.test(segments[0]));
    const joined = segments.slice(0, upToIndex + 1).join('/');
    if (isWindows) {
      return joined;
    }
    return '/' + joined;
  }

  function navigateToSegment(index: number): void {
    const segments = getPathSegments(currentPath);
    const targetPath = buildPathFromSegments(segments, index);
    void loadDirectory(targetPath);
  }

  function navigateToRoot(): void {
    const isWindows = currentPath.includes('\\') || (/^[A-Za-z]:/.test(currentPath));
    if (isWindows) {
      const segments = getPathSegments(currentPath);
      if (segments.length > 0) {
        void loadDirectory(segments[0] + '/');
      }
    } else {
      void loadDirectory('/');
    }
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

  const breadcrumbSegments = $derived(getPathSegments(currentPath));

  const selectedBasename = $derived(
    selectedPath ? getPathBasename(selectedPath) : ''
  );
</script>

<div class="mac-finder-container">
  <!-- ── 仿 Apple 标题栏与工具栏 ── -->
  <div class="mac-glass-header">
    {#if title}
      <div class="mac-titlebar">
        <h2 class="mac-title">{title}</h2>
        <button class="mac-close-btn" onclick={onCancel} aria-label="Close" title="关闭">
          <Icon name="close" size={18} />
        </button>
      </div>
    {/if}

    <div class="mac-toolbar">
      <div class="mac-nav-group">
        <button class="mac-icon-btn" onclick={goUp} disabled={!canGoUp} title="返回上级">
          <Icon name="chevron-up" size={16} />
        </button>
      </div>

      <div class="mac-breadcrumbs-wrapper">
        {#if currentPath}
          <div class="mac-breadcrumbs">
            <button class="mac-crumb-btn" onclick={navigateToRoot} disabled={loading} title="/">
              <Icon name="model" size={12} />
            </button>
            {#each breadcrumbSegments as segment, i (i)}
              <span class="mac-crumb-sep"><Icon name="chevron-right" size={10} /></span>
              {#if i === breadcrumbSegments.length - 1}
                <span class="mac-crumb-text current">{segment}</span>
              {:else}
                <button class="mac-crumb-btn" onclick={() => navigateToSegment(i)} disabled={loading}>
                  {segment}
                </button>
              {/if}
            {/each}
          </div>
        {:else}
          <div class="mac-breadcrumbs">
            <span class="mac-crumb-text current">正在定位目录...</span>
          </div>
        {/if}
      </div>

      <div class="mac-actions-group">
        <button class="mac-icon-btn" onclick={toggleManualInput} disabled={loading} title="手动输入路径" class:active={showManualInput}>
          <Icon name="pencil" size={14} />
        </button>
      </div>
    </div>
  </div>

  <!-- ── 手动输入路径栏 ── -->
  {#if showManualInput}
    <div class="mac-path-editor">
      <div class="mac-input-wrapper">
        <div class="mac-input-icon"><Icon name="pencil" size={12} /></div>
        <input
          class="mac-path-input"
          type="text"
          bind:value={manualPathInput}
          onkeydown={handleManualInputKeydown}
          placeholder="输入完整路径，按 Enter 跳转"
        />
      </div>
      <button class="apple-action-btn primary" onclick={goToManualPath} disabled={loading || !manualPathInput.trim()}>
        跳转
      </button>
    </div>
  {/if}

  <!-- ── 目录列表 ── -->
  <div class="mac-list-area">
    {#if loading}
      <div class="mac-empty-state">
        <div class="mac-spinner"></div>
        <div class="mac-empty-text">读取目录内容中…</div>
      </div>
    {:else if error}
      <div class="mac-empty-state error">
        <Icon name="close" size={24} />
        <div class="mac-empty-text">{error}</div>
        <div class="mac-empty-actions">
          <button class="apple-action-btn secondary" onclick={retryLoad}>重试</button>
          <button class="apple-action-btn" onclick={toggleManualInput}>手动输入</button>
        </div>
      </div>
    {:else if entries.length === 0}
      <div class="mac-empty-state">
        <div class="mac-empty-icon"><Icon name="folder" size={32} /></div>
        <div class="mac-empty-text">当前文件夹为空</div>
      </div>
    {:else}
      <div class="mac-list">
        {#each entries as entry (entry.path)}
          <button
            class="mac-list-item"
            class:selected={selectedPath === entry.path}
            class:is-file={!entry.isDirectory}
            type="button"
            disabled={!entry.isDirectory}
            onclick={() => { if (entry.isDirectory) selectEntry(entry); }}
            ondblclick={() => { if (entry.isDirectory) handleDblClick(entry); }}
          >
            <div class="mac-item-icon" class:is-file={!entry.isDirectory}>
              <Icon name={entry.isDirectory ? 'folder' : 'document'} size={16} />
            </div>
            <span class="mac-item-name">{entry.name}</span>
            {#if entry.isDirectory && entry.hasChildren !== false}
              <div class="mac-item-chevron"><Icon name="chevron-right" size={12} /></div>
            {/if}
          </button>
        {/each}
      </div>
    {/if}
  </div>

  <!-- ── 底部栏 ── -->
  <div class="mac-glass-footer">
    <div class="mac-footer-left">
      <label class="mac-checkbox-label">
        <input type="checkbox" checked={showHidden} onchange={toggleShowHidden} />
        <span class="mac-checkbox-box"></span>
        <span class="mac-checkbox-text">显示隐藏文件</span>
      </label>
      {#if selectedBasename}
        <div class="mac-selected-hint">
          已选：<strong>{selectedBasename}</strong>
        </div>
      {/if}
    </div>
    
    <div class="mac-footer-right">
      <button class="apple-action-btn secondary" onclick={onCancel} disabled={disabled}>取消</button>
      {#if selectedPath}
        <button class="apple-action-btn primary" onclick={confirmSelection} disabled={disabled || loading}>确认选择</button>
      {:else}
        <button class="apple-action-btn primary" onclick={selectCurrentDir} disabled={disabled || loading || !hasLoaded}>选择当前目录</button>
      {/if}
    </div>
  </div>
</div>

<style>
  /* 全局容器 */
  .mac-finder-container {
    display: flex;
    flex-direction: column;
    height: 520px;
    min-width: 0;
    background: var(--background);
    border-radius: inherit;
    overflow: hidden;
  }

  /* 顶部玻璃态标题栏 */
  .mac-glass-header {
    flex-shrink: 0;
    background: var(--glass-bg);
    backdrop-filter: blur(20px);
    -webkit-backdrop-filter: blur(20px);
    border-bottom: 1px solid rgba(var(--foreground-rgb, 100, 100, 100), 0.1);
    box-shadow: 0 1px 3px rgba(0, 0, 0, 0.02);
    z-index: 10;
  }

  .mac-titlebar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 12px 16px 8px;
  }

  .mac-title {
    margin: 0;
    font-size: 14px;
    font-weight: 600;
    color: var(--foreground);
    letter-spacing: -0.01em;
  }

  .mac-close-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 24px;
    height: 24px;
    border-radius: 6px;
    border: none;
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    transition: all 0.2s ease;
  }

  .mac-close-btn:hover {
    background: rgba(var(--foreground-rgb, 100, 100, 100), 0.08);
    color: var(--foreground);
  }

  .mac-toolbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 8px 16px 12px;
  }

  .mac-nav-group, .mac-actions-group {
    display: flex;
    align-items: center;
    gap: 4px;
  }

  .mac-icon-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
    border-radius: 6px;
    border: 1px solid transparent;
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    transition: all 0.2s ease;
  }

  .mac-icon-btn:hover:not(:disabled) {
    background: rgba(var(--foreground-rgb, 100, 100, 100), 0.06);
    color: var(--foreground);
  }

  .mac-icon-btn:disabled {
    opacity: 0.3;
    cursor: not-allowed;
  }

  .mac-icon-btn.active {
    background: rgba(var(--primary-rgb, 0, 122, 255), 0.1);
    color: var(--primary);
  }

  /* 面包屑导航 */
  .mac-breadcrumbs-wrapper {
    flex: 1;
    min-width: 0;
    height: 30px;
    display: flex;
    align-items: center;
    background: rgba(var(--foreground-rgb, 100, 100, 100), 0.04);
    border: 1px solid rgba(var(--foreground-rgb, 100, 100, 100), 0.08);
    border-radius: 8px;
    padding: 0 10px;
    box-shadow: inset 0 1px 2px rgba(0,0,0,0.02);
  }

  .mac-breadcrumbs {
    display: flex;
    align-items: center;
    gap: 4px;
    overflow-x: auto;
    scrollbar-width: none;
    -ms-overflow-style: none;
    white-space: nowrap;
    mask-image: linear-gradient(to right, transparent 0, black 0, black calc(100% - 10px), transparent 100%);
    -webkit-mask-image: linear-gradient(to right, transparent 0, black 0, black calc(100% - 10px), transparent 100%);
  }

  .mac-breadcrumbs::-webkit-scrollbar { display: none; }

  .mac-crumb-btn {
    display: inline-flex;
    align-items: center;
    padding: 2px 6px;
    border-radius: 4px;
    border: none;
    background: transparent;
    color: var(--foreground);
    font-size: 13px;
    font-weight: 500;
    cursor: pointer;
    transition: background 0.15s;
  }

  .mac-crumb-btn:hover:not(:disabled) {
    background: rgba(var(--foreground-rgb, 100, 100, 100), 0.08);
  }

  .mac-crumb-btn:disabled {
    opacity: 0.5;
  }

  .mac-crumb-sep {
    color: var(--foreground-muted);
    opacity: 0.5;
    display: flex;
    align-items: center;
  }

  .mac-crumb-text.current {
    padding: 2px 6px;
    color: var(--foreground);
    font-size: 13px;
    font-weight: 600;
  }

  /* 手动路径输入 */
  .mac-path-editor {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px 16px;
    background: rgba(var(--foreground-rgb, 100, 100, 100), 0.02);
    border-bottom: 1px solid rgba(var(--foreground-rgb, 100, 100, 100), 0.08);
  }

  .mac-input-wrapper {
    flex: 1;
    position: relative;
    display: flex;
    align-items: center;
  }

  .mac-input-icon {
    position: absolute;
    left: 10px;
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--foreground-muted);
  }

  .mac-path-input {
    width: 100%;
    height: 32px;
    padding: 0 12px 0 32px;
    border-radius: 6px;
    border: 1px solid rgba(var(--foreground-rgb, 100, 100, 100), 0.15);
    background: var(--background);
    color: var(--foreground);
    font-size: 13px;
    font-family: var(--font-mono, monospace);
    transition: border-color 0.2s, box-shadow 0.2s;
  }

  .mac-path-input:focus {
    outline: none;
    border-color: var(--primary);
    box-shadow: 0 0 0 2px rgba(var(--primary-rgb, 0, 122, 255), 0.2);
  }

  /* 列表区域 */
  .mac-list-area {
    flex: 1;
    overflow-y: auto;
    position: relative;
  }

  .mac-list {
    padding: 8px;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .mac-list-item {
    display: flex;
    align-items: center;
    gap: 10px;
    width: 100%;
    padding: 6px 12px;
    border-radius: 6px;
    border: none;
    background: transparent;
    color: var(--foreground);
    font-size: 13px;
    text-align: left;
    cursor: pointer;
    user-select: none;
    transition: background 0.15s;
  }

  .mac-list-item:hover {
    background: rgba(var(--foreground-rgb, 100, 100, 100), 0.05);
  }

  .mac-list-item.selected {
    background: var(--primary);
    color: var(--primary-foreground);
    box-shadow: 0 2px 6px rgba(var(--primary-rgb, 0, 122, 255), 0.3);
  }

  .mac-list-item.is-file {
    opacity: 0.6;
    cursor: default;
  }

  .mac-list-item.is-file:hover {
    background: transparent;
  }

  .mac-item-icon {
    display: flex;
    align-items: center;
    justify-content: center;
    /* Apple 风格蓝色文件夹 */
    color: var(--primary);
    filter: drop-shadow(0 1px 1px rgba(0,0,0,0.1));
  }

  .mac-item-icon.is-file {
    color: var(--foreground-muted);
    filter: none;
  }

  .mac-list-item.selected .mac-item-icon {
    color: var(--primary-foreground);
    filter: none;
  }

  .mac-item-name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 500;
  }

  .mac-item-chevron {
    color: var(--foreground-muted);
    opacity: 0.4;
  }

  .mac-list-item.selected .mac-item-chevron {
    color: var(--primary-foreground);
    opacity: 0.8;
  }

  /* 状态视图 */
  .mac-empty-state {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    height: 100%;
    padding: 20px;
    color: var(--foreground-muted);
    text-align: center;
  }

  .mac-empty-icon {
    opacity: 0.3;
    margin-bottom: 12px;
  }

  .mac-empty-text {
    font-size: 13px;
    max-width: 260px;
    line-height: 1.5;
  }

  .mac-empty-actions {
    display: flex;
    gap: 8px;
    margin-top: 16px;
  }

  .mac-empty-state.error {
    color: var(--error);
  }

  /* 底部玻璃态区域 */
  .mac-glass-footer {
    flex-shrink: 0;
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 12px 16px;
    background: var(--glass-bg);
    backdrop-filter: blur(20px);
    -webkit-backdrop-filter: blur(20px);
    border-top: 1px solid rgba(var(--foreground-rgb, 100, 100, 100), 0.1);
  }

  .mac-footer-left, .mac-footer-right {
    display: flex;
    align-items: center;
    gap: 12px;
  }

  .mac-checkbox-label {
    display: flex;
    align-items: center;
    gap: 6px;
    cursor: pointer;
    font-size: 12px;
    color: var(--foreground-muted);
    user-select: none;
  }

  .mac-checkbox-label input {
    display: none;
  }

  .mac-checkbox-box {
    width: 14px;
    height: 14px;
    border-radius: 4px;
    border: 1px solid rgba(var(--foreground-rgb, 100, 100, 100), 0.3);
    position: relative;
    transition: all 0.2s;
  }

  .mac-checkbox-label input:checked + .mac-checkbox-box {
    background: var(--primary);
    border-color: var(--primary);
  }

  .mac-checkbox-label input:checked + .mac-checkbox-box::after {
    content: '';
    position: absolute;
    top: 2px;
    left: 4px;
    width: 3px;
    height: 6px;
    border: solid var(--primary-foreground);
    border-width: 0 1.5px 1.5px 0;
    transform: rotate(45deg);
  }

  .mac-selected-hint {
    font-size: 12px;
    color: var(--foreground-muted);
    border-left: 1px solid rgba(var(--foreground-rgb, 100, 100, 100), 0.1);
    padding-left: 12px;
  }

  .mac-selected-hint strong {
    color: var(--foreground);
    font-weight: 600;
  }

  /* Apple风格按钮重用 */
  .apple-action-btn {
    height: 30px;
    padding: 0 14px;
    border-radius: 6px;
    font-size: 13px;
    font-weight: 500;
    cursor: pointer;
    border: 1px solid transparent;
    transition: all 0.2s ease;
  }

  .apple-action-btn.primary {
    background: var(--primary);
    color: var(--primary-foreground);
    box-shadow: 0 1px 3px rgba(var(--primary-rgb, 0, 122, 255), 0.3);
  }

  .apple-action-btn.primary:hover:not(:disabled) {
    filter: brightness(1.1);
  }

  .apple-action-btn.secondary {
    background: rgba(var(--foreground-rgb, 100, 100, 100), 0.06);
    color: var(--foreground);
    border-color: rgba(var(--foreground-rgb, 100, 100, 100), 0.1);
  }

  .apple-action-btn.secondary:hover:not(:disabled) {
    background: rgba(var(--foreground-rgb, 100, 100, 100), 0.1);
  }

  .apple-action-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  /* 加载动画 */
  .mac-spinner {
    width: 24px;
    height: 24px;
    border: 2px solid rgba(var(--foreground-rgb, 100, 100, 100), 0.1);
    border-top-color: var(--primary);
    border-radius: 50%;
    animation: mac-spin 1s linear infinite;
    margin-bottom: 12px;
  }

  @keyframes mac-spin {
    to { transform: rotate(360deg); }
  }
</style>
