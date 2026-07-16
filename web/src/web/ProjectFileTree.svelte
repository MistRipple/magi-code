<script lang="ts">
  import { onMount } from 'svelte';
  import Icon from '../components/Icon.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { listAgentDirectory, type WorkspaceDirectoryEntry as DirectoryEntry } from './agent-api';
  import { isMarkdownFile, isWordFile } from '../lib/file-preview-utils';
  import type { IconName } from '../lib/icons';

  interface Props {
    rootPath: string;
    workspaceId: string;
    title?: string;
    titlePath?: string;
    selectedFilePath?: string | null;
    onFileSelect?: (selection: { pathRef: string; displayPath: string; name: string }) => void;
  }

  let { rootPath, workspaceId, title = '', titlePath = '', selectedFilePath = null, onFileSelect }: Props = $props();

  let expandedDirPaths = $state<Set<string>>(new Set());
  let dirCache = $state<Map<string, DirectoryEntry[]>>(new Map());
  let loadingDirPaths = $state<Set<string>>(new Set());
  let dirErrors = $state<Map<string, string>>(new Map());
  let showHidden = $state(false);
  let loadedRootPath = $state('');
  const ROOT_DIRECTORY_KEY = '__workspace_root__';

  const rootEntries = $derived(dirCache.get(ROOT_DIRECTORY_KEY) ?? []);
  const rootLoading = $derived(loadingDirPaths.has(ROOT_DIRECTORY_KEY));
  const rootError = $derived(dirErrors.get(ROOT_DIRECTORY_KEY) ?? '');

  $effect(() => {
    const nextRoot = rootPath?.trim() || '';
    if (!nextRoot || nextRoot === loadedRootPath) {
      return;
    }
    resetTree(nextRoot);
    void loadDirectory(undefined, { force: true });
  });

  function resetTree(nextRoot = rootPath): void {
    loadedRootPath = nextRoot;
    expandedDirPaths = new Set();
    dirCache = new Map();
    loadingDirPaths = new Set();
    dirErrors = new Map();
  }

  async function loadDirectory(pathRef?: string, options: { force?: boolean } = {}): Promise<void> {
    const cacheKey = pathRef || ROOT_DIRECTORY_KEY;
    if (loadingDirPaths.has(cacheKey)) {
      return;
    }
    if (!options.force && dirCache.has(cacheKey)) {
      return;
    }

    loadingDirPaths = new Set(loadingDirPaths).add(cacheKey);
    const nextErrors = new Map(dirErrors);
    nextErrors.delete(cacheKey);
    dirErrors = nextErrors;

    try {
      const result = await listAgentDirectory(pathRef || '', showHidden, workspaceId);
      const entries = [...(result.entries ?? [])].sort(compareEntries);
      const nextCache = new Map(dirCache);
      nextCache.set(cacheKey, entries);
      dirCache = nextCache;
    } catch (error) {
      console.warn('[ProjectFileTree] directory load failed:', error);
      const nextErrorMap = new Map(dirErrors);
      nextErrorMap.set(cacheKey, i18n.t('web.projectFilesLoadFailed'));
      dirErrors = nextErrorMap;
    } finally {
      const nextLoading = new Set(loadingDirPaths);
      nextLoading.delete(cacheKey);
      loadingDirPaths = nextLoading;
    }
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

  function refreshRoot(): void {
    resetTree(rootPath);
    void loadDirectory(undefined, { force: true });
  }

  // 工作区内容变更（如切分支）后刷新文件树，避免停留在旧分支的目录结构。
  onMount(() => {
    const handleWorkspaceContentChanged = () => refreshRoot();
    window.addEventListener('magi:workspaceContentChanged', handleWorkspaceContentChanged);
    return () => window.removeEventListener('magi:workspaceContentChanged', handleWorkspaceContentChanged);
  });

  function toggleHiddenFiles(): void {
    showHidden = !showHidden;
    refreshRoot();
  }

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

  function getEntryIcon(entry: DirectoryEntry): IconName {
    if (entry.isDirectory) return 'folder';
    if (isMarkdownFile(entry.displayPath)) return 'file-text';
    if (isWordFile(entry.displayPath)) return 'document';
    return 'file';
  }
</script>

<div class="project-file-tree" data-workspace-id={workspaceId}>
  <div class="file-tree-heading-row">
    <div class="file-tree-heading" title={titlePath || rootPath}>
      <Icon name="folder" size={12} />
      <span>{title || i18n.t('web.projectFiles')}</span>
    </div>
    <div class="file-tree-toolbar">
      <button
        type="button"
        class="file-tree-tool"
        onclick={refreshRoot}
        title={i18n.t('web.projectFilesRefresh')}
        aria-label={i18n.t('web.projectFilesRefresh')}
        disabled={!rootPath || rootLoading}
      >
        <Icon name="refresh" size={12} />
      </button>
      <button
        type="button"
        class="file-tree-tool"
        class:active={showHidden}
        onclick={toggleHiddenFiles}
        title={i18n.t('web.projectFilesShowHidden')}
        aria-label={i18n.t('web.projectFilesShowHidden')}
        disabled={!rootPath || rootLoading}
      >
        <Icon name={showHidden ? 'eye' : 'eye-slash'} size={12} />
      </button>
    </div>
  </div>

  {#if !rootPath}
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
    aria-selected={!entry.isDirectory && entry.pathRef === selectedFilePath}
  >
    <button
      type="button"
      class="file-tree-row"
      class:selected={!entry.isDirectory && entry.pathRef === selectedFilePath}
      style={`--tree-depth: ${depth}`}
      title={entry.displayPath}
      onclick={() => handleEntryClick(entry)}
    >
      <span class="file-tree-chevron" class:file-tree-chevron--expanded={expanded} aria-hidden="true">
        {#if entry.isDirectory}
          <Icon name="chevronDown" size={9} />
        {/if}
      </span>
      <Icon name={getEntryIcon(entry)} size={12} class="file-tree-entry-icon" />
      <span class="file-tree-name">{entry.name}</span>
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

  .file-tree-heading-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
    min-height: 28px;
    flex-shrink: 0;
  }

  .file-tree-heading {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    color: var(--foreground);
    font-size: var(--text-sm);
    font-weight: var(--font-semibold);
  }

  .file-tree-heading span {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .file-tree-heading :global(svg) {
    flex-shrink: 0;
    color: var(--foreground-muted);
  }

  .file-tree-toolbar {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: var(--space-1);
    flex-shrink: 0;
  }

  .file-tree-tool {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 24px;
    height: 24px;
    padding: 0;
    border: none;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    transition: background var(--transition-fast), color var(--transition-fast);
  }

  .file-tree-tool:hover:not(:disabled),
  .file-tree-tool.active {
    background: color-mix(in srgb, var(--surface-hover) 72%, transparent);
    color: var(--foreground);
  }

  .file-tree-tool:disabled {
    opacity: 0.45;
    cursor: not-allowed;
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

  .file-tree-name {
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
</style>
