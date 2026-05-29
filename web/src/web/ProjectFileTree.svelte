<script lang="ts">
  import { onMount } from 'svelte';
  import Icon from '../components/Icon.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { listAgentDirectory, type DirectoryEntry } from './agent-api';
  import { isMarkdownFile, isWordFile } from '../lib/file-preview-utils';
  import type { IconName } from '../lib/icons';

  interface Props {
    rootPath: string;
    workspaceId: string;
    title?: string;
    titlePath?: string;
    selectedFilePath?: string | null;
    onFileSelect?: (path: string) => void;
  }

  let { rootPath, workspaceId, title = '', titlePath = '', selectedFilePath = null, onFileSelect }: Props = $props();

  let expandedDirPaths = $state<Set<string>>(new Set());
  let dirCache = $state<Map<string, DirectoryEntry[]>>(new Map());
  let loadingDirPaths = $state<Set<string>>(new Set());
  let dirErrors = $state<Map<string, string>>(new Map());
  let showHidden = $state(false);
  let loadedRootPath = $state('');

  const rootEntries = $derived(dirCache.get(rootPath) ?? []);
  const rootLoading = $derived(loadingDirPaths.has(rootPath));
  const rootError = $derived(dirErrors.get(rootPath) ?? '');

  $effect(() => {
    const nextRoot = rootPath?.trim() || '';
    if (!nextRoot || nextRoot === loadedRootPath) {
      return;
    }
    resetTree(nextRoot);
    void loadDirectory(nextRoot, { force: true });
  });

  function resetTree(nextRoot = rootPath): void {
    loadedRootPath = nextRoot;
    expandedDirPaths = new Set();
    dirCache = new Map();
    loadingDirPaths = new Set();
    dirErrors = new Map();
  }

  async function loadDirectory(path: string, options: { force?: boolean } = {}): Promise<void> {
    if (!path || loadingDirPaths.has(path)) {
      return;
    }
    if (!options.force && dirCache.has(path)) {
      return;
    }

    loadingDirPaths = new Set(loadingDirPaths).add(path);
    const nextErrors = new Map(dirErrors);
    nextErrors.delete(path);
    dirErrors = nextErrors;

    try {
      const result = await listAgentDirectory(path, showHidden);
      const entries = [...(result.entries ?? [])].sort(compareEntries);
      const nextCache = new Map(dirCache);
      nextCache.set(path, entries);
      dirCache = nextCache;
    } catch (error) {
      const nextErrorMap = new Map(dirErrors);
      nextErrorMap.set(path, error instanceof Error ? error.message : String(error));
      dirErrors = nextErrorMap;
    } finally {
      const nextLoading = new Set(loadingDirPaths);
      nextLoading.delete(path);
      loadingDirPaths = nextLoading;
    }
  }

  function compareEntries(a: DirectoryEntry, b: DirectoryEntry): number {
    if (a.isDirectory !== b.isDirectory) {
      return a.isDirectory ? -1 : 1;
    }
    return a.name.localeCompare(b.name, undefined, { sensitivity: 'base' });
  }

  function toggleDirectory(path: string): void {
    const nextExpanded = new Set(expandedDirPaths);
    if (nextExpanded.has(path)) {
      nextExpanded.delete(path);
      expandedDirPaths = nextExpanded;
      return;
    }
    nextExpanded.add(path);
    expandedDirPaths = nextExpanded;
    void loadDirectory(path);
  }

  function refreshRoot(): void {
    resetTree(rootPath);
    void loadDirectory(rootPath, { force: true });
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
      toggleDirectory(entry.path);
      return;
    }
    onFileSelect?.(entry.path);
  }

  function getEntryIcon(entry: DirectoryEntry): IconName {
    if (entry.isDirectory) return 'folder';
    if (isMarkdownFile(entry.path)) return 'file-text';
    if (isWordFile(entry.path)) return 'document';
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
      {#each rootEntries as entry (entry.path)}
        {@render treeNode(entry, 0)}
      {/each}
    </div>
  {/if}
</div>

{#snippet treeNode(entry: DirectoryEntry, depth: number)}
  {@const expanded = expandedDirPaths.has(entry.path)}
  {@const children = dirCache.get(entry.path) ?? []}
  {@const loading = loadingDirPaths.has(entry.path)}
  {@const error = dirErrors.get(entry.path) ?? ''}
  <div
    class="file-tree-node"
    role="treeitem"
    aria-expanded={entry.isDirectory ? expanded : undefined}
    aria-selected={!entry.isDirectory && entry.path === selectedFilePath}
  >
    <button
      type="button"
      class="file-tree-row"
      class:selected={!entry.isDirectory && entry.path === selectedFilePath}
      style={`--tree-depth: ${depth}`}
      title={entry.path}
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
          {#each children as child (child.path)}
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
