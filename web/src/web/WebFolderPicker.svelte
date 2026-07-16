<script lang="ts">
  import { onMount, tick } from 'svelte';
  import {
    browseAgentDirectory,
    resolveAgentPath,
    type DirectoryPathNode,
    type DirectoryPickerEntry,
  } from './agent-api';
  import Icon from '../components/Icon.svelte';
  import { i18n } from '../stores/i18n.svelte';

  interface Props {
    title?: string;
    onSelect: (selection: { pathRef: string; displayPath: string; name: string }) => void;
    onCancel: () => void;
    disabled?: boolean;
  }

  const {
    title,
    onSelect,
    onCancel,
    disabled = false,
  }: Props = $props();

  let currentPathRef = $state('');
  let currentDisplayPath = $state('');
  let parentPathRef = $state('');
  let breadcrumbs = $state<DirectoryPathNode[]>([]);
  let roots = $state<DirectoryPathNode[]>([]);
  let entries = $state<DirectoryPickerEntry[]>([]);
  let loading = $state(true);
  let error = $state('');
  let selectedPathRef = $state('');
  let manualPathInput = $state('');
  let manualPathInputElement = $state<HTMLInputElement>();
  let showManualInput = $state(false);
  let showHidden = $state(false);
  let hasLoaded = $state(false);
  let requestToken = 0;
  const directoryLoadFailedText = () => i18n.t('web.folderPickerLoadFailed');

  onMount(() => {
    void loadDirectory();
  });

  async function loadDirectory(pathRef?: string): Promise<void> {
    const token = ++requestToken;
    loading = true;
    error = '';
    selectedPathRef = '';

    try {
      const result = await browseAgentDirectory({ pathRef, showHidden });
      if (token !== requestToken) {
        return;
      }
      if (result.error) {
        console.warn('[WebFolderPicker] directory browse returned error:', result.error);
        error = directoryLoadFailedText();
        return;
      }
      currentPathRef = result.pathRef;
      currentDisplayPath = result.displayPath;
      parentPathRef = result.parentPathRef?.trim() || '';
      breadcrumbs = result.breadcrumbs;
      roots = result.roots;
      entries = result.entries;
      manualPathInput = result.displayPath;
      hasLoaded = true;
    } catch (err) {
      if (token !== requestToken) {
        return;
      }
      console.warn('[WebFolderPicker] directory browse failed:', err);
      error = directoryLoadFailedText();
    } finally {
      if (token === requestToken) {
        loading = false;
      }
    }
  }

  function enterDirectory(entry: DirectoryPickerEntry): void {
    void loadDirectory(entry.pathRef);
  }

  function toggleShowHidden(): void {
    showHidden = !showHidden;
    void loadDirectory(currentPathRef || undefined);
  }

  function goUp(): void {
    if (!currentPathRef || !parentPathRef || currentPathRef === parentPathRef) {
      return;
    }
    void loadDirectory(parentPathRef);
  }

  function selectEntry(entry: DirectoryPickerEntry): void {
    selectedPathRef = entry.pathRef;
  }

  function handleDblClick(entry: DirectoryPickerEntry): void {
    enterDirectory(entry);
  }

  function navigateToNode(node: DirectoryPathNode): void {
    void loadDirectory(node.pathRef);
  }

  function handleRootChange(event: Event): void {
    const pathRef = (event.currentTarget as HTMLSelectElement).value;
    if (pathRef) void loadDirectory(pathRef);
  }

  function confirmSelection(): void {
    const selectedEntry = entries.find((entry) => entry.pathRef === selectedPathRef);
    if (!selectedEntry) {
      return;
    }
    onSelect(selectedEntry);
  }

  function selectCurrentDir(): void {
    if (!currentPathRef) {
      return;
    }
    const current = breadcrumbs.at(-1);
    onSelect({
      pathRef: currentPathRef,
      displayPath: currentDisplayPath,
      name: current?.name || currentDisplayPath,
    });
  }

  async function toggleManualInput(): Promise<void> {
    showManualInput = !showManualInput;
    if (showManualInput) {
      manualPathInput = currentDisplayPath;
      await tick();
      manualPathInputElement?.focus();
      manualPathInputElement?.select();
    }
  }

  async function goToManualPath(): Promise<void> {
    const target = manualPathInput.trim();
    if (!target) {
      return;
    }
    loading = true;
    error = '';
    try {
      const resolved = await resolveAgentPath(target, currentPathRef || undefined);
      if (resolved.kind !== 'directory') {
        error = directoryLoadFailedText();
        return;
      }
      showManualInput = false;
      await loadDirectory(resolved.pathRef);
    } catch (err) {
      console.warn('[WebFolderPicker] manual path resolve failed:', err);
      error = directoryLoadFailedText();
    } finally {
      loading = false;
    }
  }

  function handleManualInputKeydown(event: KeyboardEvent): void {
    if (event.key === 'Enter') {
      void goToManualPath();
    }
    if (event.key === 'Escape') {
      event.stopPropagation();
      showManualInput = false;
    }
  }

  function retryLoad(): void {
    void loadDirectory(currentPathRef || undefined);
  }

  const canGoUp = $derived(
    !loading
      && !!currentPathRef
      && !!parentPathRef
      && currentPathRef !== parentPathRef
  );
  const selectedBasename = $derived(
    entries.find((entry) => entry.pathRef === selectedPathRef)?.name || ''
  );
</script>

<div class="mac-finder-container">
  <!-- ── 仿 Apple 标题栏与工具栏 ── -->
  <div class="mac-glass-header">
    {#if title}
      <div class="mac-titlebar">
        <h2 class="mac-title">{title}</h2>
        <button class="mac-close-btn" onclick={onCancel} aria-label={i18n.t('common.close')} title={i18n.t('common.close')}>
          <Icon name="close" size={18} />
        </button>
      </div>
    {/if}

    <div class="mac-toolbar">
      <div class="mac-nav-group">
        <button class="mac-icon-btn" onclick={goUp} disabled={!canGoUp} title={i18n.t('web.folderPickerGoUp')}>
          <Icon name="chevron-up" size={16} />
        </button>
        {#if roots.length > 1}
          <select class="mac-root-select" aria-label={i18n.t('web.folderPickerManualPath')} onchange={handleRootChange} disabled={loading}>
            {#each roots as root (root.pathRef)}
              <option value={root.pathRef}>{root.name}</option>
            {/each}
          </select>
        {/if}
      </div>

      <div class="mac-breadcrumbs-wrapper" class:editing={showManualInput}>
        {#if showManualInput}
          <input
            class="mac-path-input"
            type="text"
            bind:this={manualPathInputElement}
            bind:value={manualPathInput}
            onkeydown={handleManualInputKeydown}
            placeholder={i18n.t('web.folderPickerManualPathPlaceholder')}
            aria-label={i18n.t('web.folderPickerManualPath')}
          />
        {:else if currentPathRef}
          <div class="mac-breadcrumbs">
            {#each breadcrumbs as crumb, i (crumb.pathRef)}
              {#if i > 0}<span class="mac-crumb-sep"><Icon name="chevron-right" size={10} /></span>{/if}
              {#if i === breadcrumbs.length - 1}
                <span class="mac-crumb-text current">{crumb.name}</span>
              {:else}
                <button class="mac-crumb-btn" onclick={() => navigateToNode(crumb)} disabled={loading} title={crumb.displayPath}>
                  {crumb.name}
                </button>
              {/if}
            {/each}
          </div>
        {:else}
          <div class="mac-breadcrumbs">
            <span class="mac-crumb-text current">{i18n.t('web.folderPickerLocating')}</span>
          </div>
        {/if}
      </div>

      <div class="mac-actions-group">
        <button
          class="mac-icon-btn"
          onclick={toggleManualInput}
          disabled={loading}
          title={showManualInput ? i18n.t('common.close') : i18n.t('web.folderPickerManualPath')}
          aria-label={showManualInput ? i18n.t('common.close') : i18n.t('web.folderPickerManualPath')}
          class:active={showManualInput}
        >
          <Icon name={showManualInput ? 'close' : 'pencil'} size={14} />
        </button>
      </div>
    </div>
  </div>

  <!-- ── 目录列表 ── -->
  <div class="mac-list-area">
    {#if loading}
      <div class="mac-empty-state">
        <div class="mac-spinner"></div>
        <div class="mac-empty-text">{i18n.t('web.folderPickerLoading')}</div>
      </div>
    {:else if error}
      <div class="mac-empty-state error">
        <Icon name="close" size={24} />
        <div class="mac-empty-text">{error}</div>
        <div class="mac-empty-actions">
          <button class="apple-action-btn secondary" onclick={retryLoad}>{i18n.t('web.folderPickerRetry')}</button>
          <button class="apple-action-btn" onclick={toggleManualInput}>{i18n.t('web.folderPickerManualPath')}</button>
        </div>
      </div>
    {:else if entries.length === 0}
      <div class="mac-empty-state">
        <div class="mac-empty-icon"><Icon name="folder" size={32} /></div>
        <div class="mac-empty-text">{i18n.t('web.folderPickerEmpty')}</div>
      </div>
    {:else}
      <div class="mac-list">
        {#each entries as entry (entry.pathRef)}
          <button
            class="mac-list-item"
            class:selected={selectedPathRef === entry.pathRef}
            type="button"
            onclick={() => selectEntry(entry)}
            ondblclick={() => handleDblClick(entry)}
          >
            <div class="mac-item-icon">
              <Icon name="folder" size={16} />
            </div>
            <span class="mac-item-name">{entry.name}</span>
            <div class="mac-item-chevron"><Icon name="chevron-right" size={12} /></div>
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
        <span class="mac-checkbox-text">{i18n.t('web.folderPickerShowHidden')}</span>
      </label>
      {#if selectedBasename}
        <div class="mac-selected-hint">
          {i18n.t('web.folderPickerSelected')}<strong>{selectedBasename}</strong>
        </div>
      {/if}
    </div>
    
    <div class="mac-footer-right">
      <button class="apple-action-btn secondary" onclick={onCancel} disabled={disabled}>{i18n.t('web.folderPickerCancel')}</button>
      {#if selectedPathRef}
        <button class="apple-action-btn primary" onclick={confirmSelection} disabled={disabled || loading}>{i18n.t('web.folderPickerConfirm')}</button>
      {:else}
        <button class="apple-action-btn primary" onclick={selectCurrentDir} disabled={disabled || loading || !hasLoaded}>{i18n.t('web.folderPickerSelectCurrent')}</button>
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

  .mac-nav-group {
    min-width: 0;
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

  .mac-root-select {
    appearance: none;
    height: 28px;
    min-width: 64px;
    max-width: 112px;
    padding: 0 26px 0 10px;
    border: 1px solid rgba(var(--foreground-rgb, 100, 100, 100), 0.12);
    border-radius: 6px;
    background: rgba(var(--foreground-rgb, 100, 100, 100), 0.04);
    background-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='10' height='6' viewBox='0 0 10 6'%3E%3Cpath fill='%23888' d='M1 1l4 4 4-4' stroke='%23888' stroke-width='1.2' stroke-linecap='round' stroke-linejoin='round'/%3E%3C/svg%3E");
    background-repeat: no-repeat;
    background-position: right 9px center;
    color: var(--foreground);
    font: inherit;
    font-size: 12px;
    font-weight: 500;
    cursor: pointer;
    text-overflow: ellipsis;
    transition: background-color 0.15s ease, border-color 0.15s ease;
  }

  .mac-root-select:hover:not(:disabled) {
    background-color: rgba(var(--foreground-rgb, 100, 100, 100), 0.08);
    border-color: rgba(var(--foreground-rgb, 100, 100, 100), 0.18);
  }

  .mac-root-select:focus-visible {
    outline: none;
    border-color: var(--primary);
    box-shadow: 0 0 0 2px rgba(var(--primary-rgb, 0, 122, 255), 0.16);
  }

  .mac-root-select:disabled {
    opacity: 0.45;
    cursor: not-allowed;
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

  .mac-breadcrumbs-wrapper.editing {
    padding: 0;
    border-color: var(--primary);
    box-shadow: 0 0 0 2px rgba(var(--primary-rgb, 0, 122, 255), 0.16);
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

  .mac-path-input {
    width: 100%;
    height: 100%;
    min-width: 0;
    padding: 0 10px;
    border: none;
    border-radius: inherit;
    background: transparent;
    color: var(--foreground);
    font-size: 13px;
    font-family: var(--font-mono, monospace);
  }

  .mac-path-input:focus {
    outline: none;
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

  .mac-item-icon {
    display: flex;
    align-items: center;
    justify-content: center;
    /* Apple 风格蓝色文件夹 */
    color: var(--primary);
    filter: drop-shadow(0 1px 1px rgba(0,0,0,0.1));
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
