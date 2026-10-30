<script lang="ts">
  import FileName from '../components/FileName.svelte';
  import { onMount, untrack } from 'svelte';
  import Icon from '../components/Icon.svelte';
  import FileTypeIcon from '../components/FileTypeIcon.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import {
    RUNTIME_CONNECTION_EVENT,
    type AgentConnectionEventDetail,
    listAgentDirectory,
    resolveAgentFileRevealTarget,
    type WorkspaceDirectoryEntry as DirectoryEntry,
  } from './agent-api';

  interface Props {
    rootPath: string;
    workspaceId: string;
    sourceFolderId?: string;
    sessionId?: string;
    /** 是否显示隐藏文件；由侧栏标题行的开关持有，树只负责按它加载。 */
    showHidden?: boolean;
    selectedFilePath?: string | null;
    onFileSelect?: (selection: { pathRef: string; displayPath: string; name: string }) => void;
  }

  let { rootPath, workspaceId, sourceFolderId = '', sessionId = '', showHidden = false, selectedFilePath = null, onFileSelect }: Props = $props();

  let expandedDirPaths = $state<Set<string>>(new Set());
  let dirCache = $state<Map<string, DirectoryEntry[]>>(new Map());
  let loadingDirPaths = $state<Set<string>>(new Set());
  let dirErrors = $state<Map<string, string>>(new Map());
  let loadedTreeKey = $state('');
  let selectedPathRef = $state('');
  let treeElement = $state<HTMLElement | null>(null);
  let treeGeneration = 0;
  let selectionGeneration = 0;
  const directoryRequests = new Map<string, { promise: Promise<void>; refreshQueued: boolean }>();
  const ROOT_DIRECTORY_KEY = '__workspace_root__';

  const rootEntries = $derived(dirCache.get(ROOT_DIRECTORY_KEY) ?? []);
  const rootLoading = $derived(loadingDirPaths.has(ROOT_DIRECTORY_KEY));
  const rootError = $derived(dirErrors.get(ROOT_DIRECTORY_KEY) ?? '');

  $effect(() => {
    const nextRoot = rootPath?.trim() || '';
    const nextWorkspaceId = workspaceId?.trim() || '';
    const nextTreeKey = `${nextWorkspaceId}\u0000${sessionId}\u0000${sourceFolderId}\u0000${nextRoot}`;
    if (nextTreeKey === loadedTreeKey) {
      return;
    }
    resetTree(nextTreeKey);
    if (nextRoot || sessionId) {
      void loadDirectory(undefined, { force: true });
    }
  });

  function currentTreeKey(): string {
    return `${workspaceId?.trim() || ''}\u0000${sessionId}\u0000${sourceFolderId}\u0000${rootPath?.trim() || ''}`;
  }

  function resetTree(nextTreeKey = currentTreeKey()): void {
    treeGeneration += 1;
    directoryRequests.clear();
    selectionGeneration += 1;
    loadedTreeKey = nextTreeKey;
    expandedDirPaths = new Set();
    dirCache = new Map();
    loadingDirPaths = new Set();
    dirErrors = new Map();
    selectedPathRef = '';
  }

  $effect(() => {
    const filePath = selectedFilePath?.trim() || '';
    const nextWorkspaceId = workspaceId?.trim() || '';
    const nextRootPath = rootPath?.trim() || '';
    const currentSelectionGeneration = ++selectionGeneration;
    selectedPathRef = '';
    if (!filePath || (!sessionId && (!nextWorkspaceId || !nextRootPath))) {
      return;
    }
    void revealActiveFile(filePath, currentSelectionGeneration, nextWorkspaceId, nextRootPath);
  });

  async function revealActiveFile(
    filePath: string,
    currentSelectionGeneration: number,
    nextWorkspaceId: string,
    nextRootPath: string,
  ): Promise<void> {
    try {
      const response = await resolveAgentFileRevealTarget(filePath, nextWorkspaceId ? {
        scope: 'workspace', workspaceId: nextWorkspaceId, workspacePath: nextRootPath, sourceFolderId,
        ...(sessionId ? { sessionId } : {}),
      } : { scope: 'personal', sessionId });
      if (currentSelectionGeneration !== selectionGeneration || currentTreeKey() !== loadedTreeKey) {
        return;
      }
      for (const directoryPathRef of response.ancestorPathRefs ?? []) {
        if (currentSelectionGeneration !== selectionGeneration) {
          return;
        }
        expandedDirPaths = new Set(expandedDirPaths).add(directoryPathRef);
        await loadDirectory(directoryPathRef);
        if (dirErrors.has(directoryPathRef)) {
          return;
        }
      }
      if (currentSelectionGeneration !== selectionGeneration) {
        return;
      }
      selectedPathRef = response.targetPathRef;
      await scrollSelectedFileIntoView();
    } catch (error) {
      if (currentSelectionGeneration === selectionGeneration) {
        console.debug('[ProjectFileTree] active file is not revealable:', error);
      }
    }
  }

  async function scrollSelectedFileIntoView(): Promise<void> {
    await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
    treeElement?.querySelector<HTMLElement>('[aria-selected="true"]')?.scrollIntoView({
      block: 'nearest',
      behavior: 'auto',
    });
  }

  function loadDirectory(pathRef?: string, options: { force?: boolean } = {}): Promise<void> {
    const cacheKey = pathRef || ROOT_DIRECTORY_KEY;
    const existing = directoryRequests.get(cacheKey);
    if (existing) {
      existing.refreshQueued ||= options.force === true;
      return existing.promise;
    }
    if (!options.force && dirCache.has(cacheKey)) return Promise.resolve();

    const requestGeneration = treeGeneration;
    const requestTreeKey = loadedTreeKey;
    const requestWorkspaceId = workspaceId;
    const requestSessionId = sessionId;
    const requestSourceFolderId = sourceFolderId;
    const isCurrent = () => requestGeneration === treeGeneration && requestTreeKey === loadedTreeKey;
    const entry = { promise: Promise.resolve(), refreshQueued: false };
    loadingDirPaths = new Set(loadingDirPaths).add(cacheKey);
    entry.promise = (async () => {
      do {
        entry.refreshQueued = false;
        const nextErrors = new Map(dirErrors);
        nextErrors.delete(cacheKey);
        dirErrors = nextErrors;
        try {
          const result = await listAgentDirectory(pathRef || '', showHidden, requestWorkspaceId, requestSessionId, requestSourceFolderId);
          if (!isCurrent()) return;
          // 读取期间发生刷新或切换隐藏文件条件时，先补读，不能提交已过期的结果。
          if (entry.refreshQueued) continue;
          dirCache = new Map(dirCache).set(cacheKey, [...(result.entries ?? [])].sort(compareEntries));
        } catch (error) {
          if (!isCurrent()) return;
          if (entry.refreshQueued) continue;
          console.warn('[ProjectFileTree] directory load failed:', error);
          dirErrors = new Map(dirErrors).set(cacheKey, i18n.t('web.projectFilesLoadFailed'));
        }
      } while (entry.refreshQueued && isCurrent());
    })().finally(() => {
      if (!isCurrent()) return;
      if (directoryRequests.get(cacheKey) === entry) directoryRequests.delete(cacheKey);
      const nextLoading = new Set(loadingDirPaths);
      nextLoading.delete(cacheKey);
      loadingDirPaths = nextLoading;
    });
    directoryRequests.set(cacheKey, entry);
    return entry.promise;
  }

  function compareEntries(a: DirectoryEntry, b: DirectoryEntry): number {
    if (a.isDirectory !== b.isDirectory) {
      return a.isDirectory ? -1 : 1;
    }
    return a.name.localeCompare(b.name, undefined, { sensitivity: 'base' });
  }

  function toggleDirectory(pathRef: string): void {
    const nextExpanded = new Set(expandedDirPaths);
    if (nextExpanded.has(pathRef)) {
      nextExpanded.delete(pathRef);
      expandedDirPaths = nextExpanded;
      return;
    }
    nextExpanded.add(pathRef);
    expandedDirPaths = nextExpanded;
    void loadDirectory(pathRef);
  }

  /**
   * 重新读取根目录和所有已展开目录，保留用户的展开状态；
   * 目录已不存在（如切分支后）的展开项在重读失败后被收起。
   */
  export async function refresh(): Promise<void> {
    if (!rootPath?.trim() && !sessionId) return;
    const treeKey = loadedTreeKey;
    const directories = [undefined, ...expandedDirPaths];
    await Promise.all(directories.map((pathRef) => loadDirectory(pathRef, { force: true })));
    if (treeKey !== loadedTreeKey) return;
    const stillValid = new Set([...expandedDirPaths].filter((pathRef) => !dirErrors.has(pathRef)));
    if (stillValid.size !== expandedDirPaths.size) {
      expandedDirPaths = stillValid;
    }
  }

  // 工作区内容变更（如切分支）后刷新文件树，避免停留在旧分支的目录结构。
  onMount(() => {
    const handleWorkspaceContentChanged = () => void refresh();
    const handleConnected = (event: Event) => {
      if ((event as CustomEvent<AgentConnectionEventDetail>).detail?.status === 'connected') void refresh();
    };
    window.addEventListener('magi:workspaceContentChanged', handleWorkspaceContentChanged);
    window.addEventListener('focus', handleWorkspaceContentChanged);
    window.addEventListener(RUNTIME_CONNECTION_EVENT, handleConnected);
    return () => {
      window.removeEventListener('magi:workspaceContentChanged', handleWorkspaceContentChanged);
      window.removeEventListener('focus', handleWorkspaceContentChanged);
      window.removeEventListener(RUNTIME_CONNECTION_EVENT, handleConnected);
    };
  });

  // 隐藏文件开关变化时按新条件重读，展开状态不丢。
  let appliedShowHidden = untrack(() => showHidden);
  $effect(() => {
    const next = showHidden;
    if (next === appliedShowHidden) return;
    appliedShowHidden = next;
    untrack(() => void refresh());
  });

  function handleEntryClick(entry: DirectoryEntry): void {
    if (entry.isDirectory) {
      toggleDirectory(entry.pathRef);
      return;
    }
    onFileSelect?.({
      pathRef: entry.pathRef,
      displayPath: entry.displayPath,
      name: entry.name,
    });
  }

</script>

<div class="project-file-tree" data-workspace-id={workspaceId} data-source-folder-id={sourceFolderId || undefined} bind:this={treeElement}>
  {#if !rootPath && !sessionId}
    <div class="file-tree-empty">{i18n.t('web.projectFilesNoWorkspace')}</div>
  {:else if rootLoading && rootEntries.length === 0}
    <div class="file-tree-empty">{i18n.t('common.loading')}</div>
  {:else if rootError}
    <div class="file-tree-error">{rootError}</div>
  {:else if rootEntries.length === 0}
    <div class="file-tree-empty">{i18n.t('web.projectFilesEmpty')}</div>
  {:else}
    <div class="file-tree-list" role="tree" aria-label={i18n.t('web.projectFiles')}>
      {#each rootEntries as entry (entry.pathRef)}
        {@render treeNode(entry, 0)}
      {/each}
    </div>
  {/if}
</div>

{#snippet treeNode(entry: DirectoryEntry, depth: number)}
  {@const expanded = expandedDirPaths.has(entry.pathRef)}
  {@const children = dirCache.get(entry.pathRef) ?? []}
  {@const loading = loadingDirPaths.has(entry.pathRef)}
  {@const error = dirErrors.get(entry.pathRef) ?? ''}
  <div
    class="file-tree-node"
    role="treeitem"
    aria-expanded={entry.isDirectory ? expanded : undefined}
    aria-selected={!entry.isDirectory && entry.pathRef === selectedPathRef}
  >
    <button
      type="button"
      class="file-tree-row"
      class:selected={!entry.isDirectory && entry.pathRef === selectedPathRef}
      style={`--tree-depth: ${depth}`}
      title={entry.displayPath}
      onclick={() => handleEntryClick(entry)}
    >
      <span class="file-tree-chevron" class:file-tree-chevron--expanded={expanded} aria-hidden="true">
        {#if entry.isDirectory}
          <Icon name="chevronDown" size={9} />
        {/if}
      </span>
      {#if entry.isDirectory}
        <Icon name="folder" size={12} class="file-tree-entry-icon file-tree-folder-icon" />
      {:else}
        <FileTypeIcon path={entry.displayPath} size={14} />
      {/if}
      <span class="file-tree-name"><FileName name={entry.name} preserveExtension={!entry.isDirectory} /></span>
    </button>

    {#if entry.isDirectory && expanded}
      <div class="file-tree-children" role="group">
        {#if loading && children.length === 0}
          <div class="file-tree-status" style={`--tree-depth: ${depth + 1}`}>{i18n.t('common.loading')}</div>
        {:else if error}
          <div class="file-tree-status file-tree-status--error" style={`--tree-depth: ${depth + 1}`}>{error}</div>
        {:else if children.length === 0}
          <div class="file-tree-status" style={`--tree-depth: ${depth + 1}`}>{i18n.t('web.projectFilesEmpty')}</div>
        {:else}
          {#each children as child (child.pathRef)}
            {@render treeNode(child, depth + 1)}
          {/each}
        {/if}
      </div>
    {/if}
  </div>
{/snippet}

<style>
  .project-file-tree {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    min-height: 0;
  }

  .file-tree-list {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-height: 0;
    flex: 1;
  }

  .file-tree-node {
    min-width: 0;
  }

  .file-tree-row {
    display: flex;
    align-items: center;
    gap: 5px;
    width: 100%;
    min-width: 0;
    height: 24px;
    padding: 0 6px 0 calc(6px + var(--tree-depth) * 14px);
    border: none;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    text-align: left;
    font-size: var(--text-xs);
    line-height: 1;
    transition: background var(--transition-fast), color var(--transition-fast);
  }

  .file-tree-row:hover {
    background: color-mix(in srgb, var(--surface-hover) 64%, transparent);
    color: var(--foreground);
  }

  .file-tree-row.selected {
    background: color-mix(in srgb, var(--surface-selected) 78%, transparent);
    color: var(--foreground);
  }

  .file-tree-chevron {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 10px;
    height: 10px;
    flex-shrink: 0;
    color: var(--foreground-muted);
    transform: rotate(-90deg);
    transition: transform var(--transition-fast);
  }

  .file-tree-chevron--expanded {
    transform: rotate(0deg);
  }

  :global(.file-tree-entry-icon) {
    flex-shrink: 0;
    color: var(--foreground-muted);
  }

  :global(.file-tree-row.selected .file-type-icon) {
    filter: saturate(1.12) brightness(1.08);
  }

  .file-tree-name {
    display: flex;
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .file-tree-status,
  .file-tree-empty,
  .file-tree-error {
    padding: 5px 6px 5px calc(6px + var(--tree-depth, 0) * 14px);
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    line-height: 1.4;
  }

  .file-tree-error,
  .file-tree-status--error {
    color: var(--error);
  }
  .file-tree-row:active { background: var(--surface-active, var(--surface-hover)); }
  @media (hover: none), (pointer: coarse) {
    .file-tree-row { height: auto; min-block-size: 44px; padding-block: 8px; line-height: 1.5; }
  }
</style>
