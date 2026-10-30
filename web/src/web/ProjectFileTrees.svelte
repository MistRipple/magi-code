<script lang="ts">
  import Icon from '../components/Icon.svelte';
  import ProjectFileTree from './ProjectFileTree.svelte';
  import type { AgentWorkspaceSourceFolder } from './agent-api';

  type Selection = { pathRef: string; displayPath: string; name: string; sourceFolderId?: string };
  interface Props {
    rootPath: string;
    workspaceId: string;
    sourceFolders?: AgentWorkspaceSourceFolder[];
    sessionId?: string;
    showHidden?: boolean;
    selectedFilePath?: string | null;
    selectedSourceFolderId?: string;
    onFileSelect?: (selection: Selection) => void;
  }
  let { rootPath, workspaceId, sourceFolders = [], sessionId = '', showHidden = false,
    selectedFilePath = null, selectedSourceFolderId = '', onFileSelect }: Props = $props();
  let trees = $state<Record<string, { refresh: () => Promise<void> }>>({});
  let personalTree = $state<{ refresh: () => Promise<void> } | null>(null);

  export async function refresh(): Promise<void> {
    const current = new Set(sourceFolders.map((folder) => folder.sourceFolderId));
    await Promise.all([
      ...Object.entries(trees).filter(([id]) => current.has(id)).map(([, tree]) => tree?.refresh()),
      personalTree?.refresh(),
    ]);
  }
</script>

{#if workspaceId && sourceFolders.length}
  {#each sourceFolders as folder (folder.sourceFolderId)}
    <details class="project-source-tree" open data-source-folder-id={folder.sourceFolderId}>
      <summary>
        <span class="source-tree-heading">
          <span class="source-tree-chevron"><Icon name="chevronDown" size={12} /></span>
          <span class="source-tree-copy">
            <bdi>{folder.name || folder.path.split(/[\\/]/).filter(Boolean).at(-1) || folder.path}</bdi>
            <span class="source-tree-path" dir="ltr">{folder.path}</span>
          </span>
        </span>
      </summary>
      <ProjectFileTree
        bind:this={trees[folder.sourceFolderId]}
        rootPath={folder.path}
        {workspaceId}
        sourceFolderId={folder.sourceFolderId}
        {sessionId}
        {showHidden}
        selectedFilePath={!selectedSourceFolderId || selectedSourceFolderId === folder.sourceFolderId ? selectedFilePath : null}
        onFileSelect={(selection) => onFileSelect?.({ ...selection, sourceFolderId: folder.sourceFolderId })}
      />
    </details>
  {/each}
{:else}
  <ProjectFileTree bind:this={personalTree} {rootPath} {workspaceId} {sessionId} {showHidden} {selectedFilePath} {onFileSelect} />
{/if}

<style>
  .project-source-tree { min-width: 0; margin-bottom: var(--space-2); }
  summary { padding: 6px; color: var(--foreground); font-size: var(--text-xs); cursor: pointer; list-style: none; border-radius: var(--radius-sm); }
  summary::-webkit-details-marker { display: none; }
  .source-tree-heading { display: flex; align-items: start; gap: 8px; min-inline-size: 0; }
  .source-tree-chevron { display: flex; padding-block-start: 3px; transform: rotate(-90deg); }
  details[open] .source-tree-chevron { transform: none; }
  details:dir(rtl):not([open]) .source-tree-chevron { transform: rotate(90deg); }
  .source-tree-copy { display: flex; flex-direction: column; gap: 4px; min-inline-size: 0; line-height: 1.5; overflow-wrap: anywhere; }
  .source-tree-path { color: var(--foreground-muted); unicode-bidi: isolate; }
  summary:hover, summary:active { background: var(--surface-hover); }
  @media (hover: none), (pointer: coarse) { summary { min-block-size: 44px; padding: 8px; } }
</style>
