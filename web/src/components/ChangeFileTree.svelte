<script lang="ts">
  import FileTypeIcon from './FileTypeIcon.svelte';
  import Icon from './Icon.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import type { Edit, EditType } from '../types/message';

  interface Props {
    edits: readonly Edit[];
    workspacePath?: string;
    activeFilePath?: string | null;
    changeMutationPending?: boolean;
    onOpen: (edit: Edit) => void | Promise<void>;
    onApprove: (edit: Edit) => void;
    onRevert: (edit: Edit) => void;
  }

  interface DirectoryNode {
    kind: 'directory';
    key: string;
    name: string;
    children: TreeNode[];
    statusKinds: EditType[];
  }

  interface FileNode {
    kind: 'file';
    key: string;
    name: string;
    relativePath: string;
    edit: Edit;
  }

  type TreeNode = DirectoryNode | FileNode;

  let {
    edits,
    workspacePath = '',
    activeFilePath = null,
    changeMutationPending = false,
    onOpen,
    onApprove,
    onRevert,
  }: Props = $props();

  let expandedDirectoryKeys = $state<Set<string>>(new Set());
  let knownDirectoryKeys = new Set<string>();
  let appliedTreeSignature = '';

  const tree = $derived(buildTree(edits, workspacePath));
  const treeSignature = $derived(edits.map((edit) => (
    `${displayPathForEdit(edit, workspacePath)}::${edit.type ?? 'modify'}`
  )).join('\u0000'));
  $effect(() => {
    const signature = treeSignature;
    if (signature === appliedTreeSignature) return;

    const directoryKeys = new Set(collectDirectoryKeys(tree));
    if (!appliedTreeSignature) {
      expandedDirectoryKeys = directoryKeys;
    } else {
      const nextExpandedKeys = new Set(
        [...expandedDirectoryKeys].filter((key) => directoryKeys.has(key)),
      );
      for (const key of directoryKeys) {
        if (!knownDirectoryKeys.has(key)) nextExpandedKeys.add(key);
      }
      expandedDirectoryKeys = nextExpandedKeys;
    }

    knownDirectoryKeys = directoryKeys;
    appliedTreeSignature = signature;
  });

  function normalizePath(value: string): string {
    return value
      .trim()
      .replaceAll('\\', '/')
      .replace(/\/+/gu, '/')
      .replace(/^\.\//u, '')
      .replace(/\/$/u, '');
  }

  function displayPathForPath(filePath: string, basePath: string): string {
    const normalizedPath = normalizePath(filePath);
    const normalizedBase = normalizePath(basePath);
    if (
      normalizedBase
      && (normalizedPath === normalizedBase || normalizedPath.startsWith(`${normalizedBase}/`))
    ) {
      return normalizedPath.slice(normalizedBase.length).replace(/^\/+/, '');
    }
    return normalizedPath.replace(/^\/+/, '');
  }

  function displayPathForEdit(edit: Edit, fallbackWorkspacePath: string): string {
    return displayPathForPath(edit.filePath, edit.workspacePath || fallbackWorkspacePath);
  }

  function editKind(edit: Edit): EditType {
    return edit.type ?? 'modify';
  }

  function addStatusKind(directory: DirectoryNode, kind: EditType): void {
    if (!directory.statusKinds.includes(kind)) {
      directory.statusKinds.push(kind);
    }
  }

  function buildTree(sourceEdits: readonly Edit[], rootPath: string): DirectoryNode {
    const root: DirectoryNode = {
      kind: 'directory',
      key: '__change_tree_root__',
      name: '',
      children: [],
      statusKinds: [],
    };
    const directories = new Map<string, DirectoryNode>();
    directories.set('', root);

    const latestEditsByPath = new Map<string, Edit>();
    for (const edit of sourceEdits) {
      const relativePath = displayPathForEdit(edit, rootPath);
      if (!relativePath) continue;
      const existing = latestEditsByPath.get(relativePath);
      if (!existing || (edit.updatedAt ?? 0) >= (existing.updatedAt ?? 0)) {
        latestEditsByPath.set(relativePath, edit);
      }
    }

    for (const [relativePath, edit] of latestEditsByPath) {
      const segments = relativePath.split('/').filter(Boolean);
      const kind = editKind(edit);
      let parent = root;
      let parentKey = '';
      addStatusKind(root, kind);

      for (const segment of segments.slice(0, -1)) {
        parentKey = parentKey ? `${parentKey}/${segment}` : segment;
        let directory = directories.get(parentKey);
        if (!directory) {
          directory = {
            kind: 'directory',
            key: parentKey,
            name: segment,
            children: [],
            statusKinds: [],
          };
          directories.set(parentKey, directory);
          parent.children.push(directory);
        }
        addStatusKind(directory, kind);
        parent = directory;
      }

      const fileName = segments[segments.length - 1];
      parent.children.push({
        kind: 'file',
        key: relativePath,
        name: fileName,
        relativePath,
        edit,
      });
    }

    sortTree(root);
    return root;
  }

  function sortTree(directory: DirectoryNode): void {
    directory.children.sort((left, right) => {
      if (left.kind !== right.kind) return left.kind === 'directory' ? -1 : 1;
      return left.name.localeCompare(right.name, undefined, { sensitivity: 'base' });
    });
    for (const child of directory.children) {
      if (child.kind === 'directory') sortTree(child);
    }
  }

  function collectDirectoryKeys(directory: DirectoryNode): string[] {
    const keys: string[] = [];
    for (const child of directory.children) {
      if (child.kind !== 'directory') continue;
      keys.push(child.key, ...collectDirectoryKeys(child));
    }
    return keys;
  }

  function statusClass(kinds: readonly EditType[]): string {
    return kinds.length === 1 ? kinds[0] : 'mixed';
  }

  function statusGlyph(kind: EditType): string {
    switch (kind) {
      case 'add': return '+';
      case 'delete': return '−';
      case 'rename': return '↗';
      default: return '•';
    }
  }

  function statusLabel(kind: EditType): string {
    switch (kind) {
      case 'add': return i18n.t('edits.tree.status.add');
      case 'delete': return i18n.t('edits.tree.status.delete');
      case 'rename': return i18n.t('edits.tree.status.rename');
      default: return i18n.t('edits.tree.status.modify');
    }
  }

  function editTitle(edit: Edit): string {
    return edit.type === 'rename' && edit.oldPath
      ? `${edit.oldPath} → ${edit.filePath}`
      : edit.filePath;
  }

  function fileStats(edit: Edit): string {
    if (edit.contentKind && edit.contentKind !== 'text') {
      return formatSize(edit.size);
    }
    const additions = Math.max(0, edit.additions ?? 0);
    const deletions = Math.max(0, edit.deletions ?? 0);
    if (additions === 0 && deletions === 0) return '';
    return `+${additions} −${deletions}`;
  }

  function textLineStats(edit: Edit): { additions: number; deletions: number } | null {
    if (edit.contentKind && edit.contentKind !== 'text') return null;
    const additions = Math.max(0, edit.additions ?? 0);
    const deletions = Math.max(0, edit.deletions ?? 0);
    return additions === 0 && deletions === 0 ? null : { additions, deletions };
  }

  function formatSize(size?: number): string {
    if (typeof size !== 'number' || !Number.isFinite(size) || size < 0) return '';
    if (size < 1024) return `${size} B`;
    if (size < 1024 * 1024) return `${(size / 1024).toFixed(1)} KB`;
    if (size < 1024 * 1024 * 1024) return `${(size / (1024 * 1024)).toFixed(1)} MB`;
    return `${(size / (1024 * 1024 * 1024)).toFixed(1)} GB`;
  }

  function contentKindLabel(kind?: string): string {
    switch (kind) {
      case 'binary': return i18n.t('edits.kind.binary');
      case 'large_text': return i18n.t('edits.kind.largeText');
      case 'symlink': return i18n.t('edits.kind.symlink');
      case 'special': return i18n.t('edits.kind.special');
      default: return i18n.t('edits.kind.text');
    }
  }

  function editActorPresentation(edit: Edit): { kind: 'mainline' | 'agent' | 'external'; label: string; title: string } | null {
    if (edit.sourceKind === 'tool') {
      return edit.workerId?.trim()
        ? { kind: 'agent', label: i18n.t('edits.actor.agent'), title: i18n.t('edits.actor.agentTitle') }
        : { kind: 'mainline', label: i18n.t('edits.actor.mainline'), title: i18n.t('edits.actor.mainlineTitle') };
    }
    if (edit.sourceKind === 'watcher' || edit.sourceKind === 'external') {
      return { kind: 'external', label: i18n.t('edits.actor.external'), title: i18n.t('edits.actor.externalTitle') };
    }
    return null;
  }

  function baseName(filePath: string): string {
    return normalizePath(filePath).split('/').pop() || filePath;
  }

  function isActiveFile(edit: Edit, relativePath: string): boolean {
    if (!activeFilePath) return false;
    return displayPathForPath(
      activeFilePath,
      edit.workspacePath || workspacePath,
    ) === relativePath;
  }

  function toggleDirectory(key: string): void {
    const next = new Set(expandedDirectoryKeys);
    if (next.has(key)) next.delete(key);
    else next.add(key);
    expandedDirectoryKeys = next;
  }

  function openFile(edit: Edit): void {
    void onOpen(edit);
  }

  function handleFileKeydown(event: KeyboardEvent, edit: Edit): void {
    if (event.target !== event.currentTarget) return;
    if (event.key !== 'Enter' && event.key !== ' ') return;
    event.preventDefault();
    openFile(edit);
  }
</script>

<div class="change-file-tree" role="tree" aria-label={i18n.t('edits.section.pendingChanges')}>
  {#each tree.children as node (node.key)}
    {@render treeNode(node, 0)}
  {/each}
</div>

{#snippet treeNode(node: TreeNode, depth: number)}
  {#if node.kind === 'directory'}
    {@const expanded = expandedDirectoryKeys.has(node.key)}
    <div class="change-tree-node" role="treeitem" aria-expanded={expanded} aria-selected="false">
      <button
        type="button"
        class="change-tree-row change-tree-row--directory"
        style={`--change-tree-depth: ${depth}`}
        onclick={() => toggleDirectory(node.key)}
        title={node.name}
      >
        <span class="change-tree-chevron" class:expanded aria-hidden="true">
          <Icon name="chevronDown" size={11} />
        </span>
        <Icon name="folder" size={14} class="change-tree-folder-icon" />
        <span class="change-tree-directory-name">{node.name}</span>
        <span
          class={`change-tree-status change-tree-status--${statusClass(node.statusKinds)}`}
          title={i18n.t('edits.tree.directoryChanged')}
          aria-hidden="true"
        >•</span>
      </button>
      {#if expanded}
        <div class="change-tree-children" role="group">
          {#each node.children as child (child.key)}
            {@render treeNode(child, depth + 1)}
          {/each}
        </div>
      {/if}
    </div>
  {:else}
    {@const active = isActiveFile(node.edit, node.relativePath)}
    {@const kind = editKind(node.edit)}
    {@const stats = fileStats(node.edit)}
    {@const lineStats = textLineStats(node.edit)}
    {@const contentKind = node.edit.contentKind ?? 'text'}
    {@const actor = editActorPresentation(node.edit)}
    {@const oldName = node.edit.oldPath ? baseName(node.edit.oldPath) : ''}
    <div
      class="change-tree-row change-tree-row--file"
      class:active
      class:change-tree-row--error={node.edit.hasError}
      style={`--change-tree-depth: ${depth}`}
      role="treeitem"
      tabindex="0"
      aria-selected={active}
      title={editTitle(node.edit)}
      onclick={openFile.bind(null, node.edit)}
      onkeydown={(event) => handleFileKeydown(event, node.edit)}
    >
      <span class="change-tree-chevron-spacer" aria-hidden="true"></span>
      <FileTypeIcon path={node.edit.filePath} size={15} />
      <span class="change-tree-file-name">
        {#if kind === 'rename' && oldName}
          <span class="change-tree-old-name">{oldName}</span>
          <span class="change-tree-rename-arrow">→</span>
        {/if}
        <span>{node.name}</span>
      </span>
      <div class="change-tree-file-meta">
        <span class="change-tree-file-tags">
          {#if actor}
            <span class="change-tree-tag change-tree-tag--{actor.kind}" title={actor.title}>{actor.label}</span>
          {/if}
          {#if contentKind !== 'text'}
            <span class="change-tree-tag" title={contentKindLabel(contentKind)}>{contentKindLabel(contentKind)}</span>
          {/if}
          {#if node.edit.hasError}
            <span class="change-tree-tag change-tree-tag--error" title={i18n.t('edits.row.errorTitle')}>{i18n.t('edits.row.error')}</span>
          {/if}
        </span>
        <span class={`change-tree-status change-tree-status--${kind}`} title={statusLabel(kind)}>{statusGlyph(kind)}</span>
        {#if lineStats}
          <span
            class="change-tree-file-stats change-tree-file-stats--lines"
            aria-label={stats}
          >
            <span class="stat-add">+{lineStats.additions}</span>
            <span class="stat-del">−{lineStats.deletions}</span>
          </span>
        {:else}
          <span class="change-tree-file-stats" aria-label={stats || undefined}>{stats}</span>
        {/if}
        <div class="change-tree-actions">
          <button
            type="button"
            class="change-tree-action change-tree-action--approve"
            disabled={changeMutationPending}
            title={i18n.t('edits.actions.approveChange')}
            aria-label={i18n.t('edits.actions.approveChange')}
            onclick={(event) => { event.stopPropagation(); onApprove(node.edit); }}
          >
            <Icon name="check" size={13} />
          </button>
          <button
            type="button"
            class="change-tree-action change-tree-action--revert"
            disabled={changeMutationPending || node.edit.revertible !== true}
            title={node.edit.revertible === true ? i18n.t('edits.actions.revertChange') : i18n.t('edits.actions.revertUnavailable')}
            aria-label={node.edit.revertible === true ? i18n.t('edits.actions.revertChange') : i18n.t('edits.actions.revertUnavailable')}
            onclick={(event) => { event.stopPropagation(); onRevert(node.edit); }}
          >
            <Icon name="undo" size={13} />
          </button>
        </div>
      </div>
    </div>
  {/if}
{/snippet}

<style>
  .change-file-tree {
    container-type: inline-size;
    overflow: hidden;
    padding: 3px 0;
    border: 1px solid color-mix(in srgb, var(--border-subtle) 76%, transparent);
    border-radius: var(--radius-md);
    background: color-mix(in srgb, var(--background) 62%, var(--surface-1));
  }

  .change-tree-row {
    position: relative;
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    min-width: 0;
    min-height: 34px;
    padding: 5px 8px 5px calc(8px + var(--change-tree-depth, 0) * 18px);
    border: 0;
    background: transparent;
    color: var(--foreground);
    text-align: left;
    transition: background var(--transition-fast), color var(--transition-fast);
  }

  .change-tree-row--directory {
    cursor: pointer;
    font-weight: var(--font-medium);
  }

  .change-tree-row--directory:hover,
  .change-tree-row--directory:focus-visible {
    background: color-mix(in srgb, var(--surface-hover) 78%, transparent);
  }

  .change-tree-row--file {
    cursor: pointer;
    outline: none;
  }

  .change-tree-row--file:focus-visible {
    box-shadow: inset 0 0 0 1px var(--primary);
  }

  .change-tree-row--file:hover,
  .change-tree-row--file.active {
    background: color-mix(in srgb, var(--surface-hover) 78%, transparent);
  }

  .change-tree-row--file.active {
    color: var(--foreground);
    font-weight: var(--font-semibold);
  }

  .change-tree-row--error {
    background: color-mix(in srgb, var(--error) 8%, transparent);
  }

  .change-tree-chevron,
  .change-tree-chevron-spacer {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 12px;
    height: 12px;
    flex: 0 0 12px;
    color: var(--foreground-muted);
  }

  .change-tree-chevron {
    transform: rotate(-90deg);
    transition: transform var(--transition-fast);
  }

  .change-tree-chevron.expanded {
    transform: rotate(0deg);
  }

  :global(.change-tree-folder-icon) {
    flex: 0 0 auto;
    color: var(--foreground-muted);
  }

  .change-tree-directory-name,
  .change-tree-file-name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .change-tree-directory-name {
    flex: 1 1 auto;
  }

  .change-tree-file-name {
    display: inline-flex;
    flex: 1 1 auto;
    gap: 4px;
    font-size: var(--text-sm);
  }

  .change-tree-old-name,
  .change-tree-rename-arrow {
    color: var(--foreground-muted);
    font-weight: var(--font-normal);
  }

  .change-tree-file-meta {
    display: inline-flex;
    width: 172px;
    flex: 0 0 172px;
    align-items: center;
    justify-content: flex-start;
    gap: 4px;
    margin-left: auto;
  }

  .change-tree-file-tags {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    width: auto;
    max-width: 72px;
    min-width: 0;
    overflow: hidden;
  }

  .change-tree-tag {
    display: inline-flex;
    flex: 0 0 auto;
    align-items: center;
    padding: 1px 4px;
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-sm);
    background: color-mix(in srgb, var(--surface-2) 60%, transparent);
    color: var(--foreground-muted);
    font-size: var(--text-2xs);
    font-weight: var(--font-medium);
    line-height: 1.35;
  }

  .change-tree-tag--error {
    border-color: color-mix(in srgb, var(--error) 35%, var(--border-subtle));
    background: color-mix(in srgb, var(--error) 18%, transparent);
    color: var(--error);
  }

  .change-tree-tag--mainline {
    border-color: color-mix(in srgb, var(--info) 42%, var(--border-subtle));
    background: color-mix(in srgb, var(--info) 16%, transparent);
    color: var(--info);
  }

  .change-tree-tag--external {
    border-color: color-mix(in srgb, var(--warning) 44%, var(--border-subtle));
    background: color-mix(in srgb, var(--warning) 16%, transparent);
    color: var(--warning);
  }

  .change-tree-tag--agent {
    border-color: color-mix(in srgb, var(--color-orchestrator) 42%, var(--border-subtle));
    background: color-mix(in srgb, var(--color-orchestrator) 16%, transparent);
    color: var(--color-orchestrator);
  }

  .change-tree-file-stats {
    width: auto;
    color: var(--foreground-muted);
    font-size: var(--text-2xs);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }

  .change-tree-file-stats--lines {
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }

  .stat-add { color: var(--success); }
  .stat-del { color: var(--error); }

  .change-tree-status {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 20px;
    height: 20px;
    flex: 0 0 20px;
    border: 1px solid currentColor;
    border-radius: 4px;
    font-size: 14px;
    font-weight: var(--font-semibold);
    line-height: 1;
  }

  .change-tree-row--directory .change-tree-status {
    width: auto;
    height: auto;
    margin-left: auto;
    border: 0;
    font-size: 16px;
  }

  .change-tree-status--add { color: var(--success); }
  .change-tree-status--modify { color: var(--warning); }
  .change-tree-status--delete { color: var(--error); }
  .change-tree-status--rename { color: var(--info); }
  .change-tree-status--mixed { color: var(--warning); }

  .change-tree-actions {
    display: inline-flex;
    width: 44px;
    justify-content: flex-start;
    gap: 2px;
    opacity: 0;
    transition: opacity var(--transition-fast);
  }

  .change-tree-row--file:hover .change-tree-actions,
  .change-tree-row--file:focus-within .change-tree-actions {
    opacity: 1;
  }

  .change-tree-action {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 22px;
    border: 1px solid transparent;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
  }

  .change-tree-action:hover:not(:disabled) {
    border-color: var(--border-subtle);
    background: var(--surface-hover);
    color: var(--foreground);
  }

  .change-tree-action--approve:hover:not(:disabled) { color: var(--success); }
  .change-tree-action--revert:hover:not(:disabled) { color: var(--error); }

  .change-tree-action:disabled {
    cursor: not-allowed;
    opacity: 0.42;
  }

  @media (hover: none) {
    .change-tree-actions {
      opacity: 1;
    }
  }

  @container (max-width: 520px) {
    .change-tree-file-meta {
      width: auto;
      flex-basis: auto;
    }

    .change-tree-file-tags {
      display: none;
    }
  }
</style>
