<script lang="ts">
  import FileName from './FileName.svelte';
  import FileTypeIcon from './FileTypeIcon.svelte';
  import type { FilePreviewScope } from '../lib/file-reference';
  import { desktopContextMenu } from '../lib/desktop-context-menu-contract';

  interface Props {
    filepath: string;
    showIcon?: boolean;
    clickable?: boolean;
    onClick?: (filepath: string) => void;
    filePreviewScope?: FilePreviewScope;
  }

  let {
    filepath,
    showIcon = true,
    clickable = true,
    onClick,
    filePreviewScope = undefined,
  }: Props = $props();

  function lastPathSeparatorIndex(path: string): number {
    return Math.max(path.lastIndexOf('/'), path.lastIndexOf('\\'));
  }

  // 获取文件名
  const filename = $derived.by(() => {
    const separatorIndex = lastPathSeparatorIndex(filepath);
    return separatorIndex >= 0 ? filepath.slice(separatorIndex + 1) || filepath : filepath;
  });

  // 获取目录路径
  const directory = $derived.by(() => {
    const separatorIndex = lastPathSeparatorIndex(filepath);
    return separatorIndex >= 0 ? filepath.slice(0, separatorIndex) : '';
  });

  function handleClick(e: MouseEvent) {
    if (clickable && onClick) {
      e.stopPropagation();
      onClick(filepath);
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    if (clickable && onClick && (e.key === 'Enter' || e.key === ' ')) {
      e.preventDefault();
      e.stopPropagation();
      onClick(filepath);
    }
  }

  function openFile() {
    if (clickable && onClick) {
      onClick(filepath);
    }
  }
</script>

{#if clickable}
  <button 
    class="filespan" 
    class:clickable
    use:desktopContextMenu={{ kind: 'file', filePath: filepath, open: openFile, fileScope: filePreviewScope }}
    onclick={handleClick}
    onkeydown={handleKeydown}
    title={filepath}
  >
    {#if showIcon}
      <span class="filespan-icon">
        <FileTypeIcon path={filepath} size={14} />
      </span>
    {/if}
    <span class="filespan-name"><FileName name={filename} /></span>
    {#if directory}
      <span class="filespan-dir">{directory}</span>
    {/if}
  </button>
{:else}
  <span
    class="filespan"
    title={filepath}
    use:desktopContextMenu={{ kind: 'file', filePath: filepath, fileScope: filePreviewScope }}
  >
    {#if showIcon}
      <span class="filespan-icon">
        <FileTypeIcon path={filepath} size={14} />
      </span>
    {/if}
    <span class="filespan-name"><FileName name={filename} /></span>
  </span>
{/if}

<style>
  .filespan {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    font-family: var(--font-mono);
    font-size: var(--text-sm);
    color: var(--foreground);
    background: var(--surface-2);
    padding: 2px 6px;
    border-radius: var(--radius-sm);
    border: none;
    max-width: 100%;
    overflow: hidden;
  }

  .filespan.clickable {
    cursor: pointer;
    transition: all var(--transition-fast);
  }

  .filespan.clickable:hover {
    background: var(--surface-hover);
    color: var(--info);
  }

  .filespan-icon {
    display: flex;
    flex-shrink: 0;
    color: var(--foreground-muted);
  }

  .filespan-name {
    display: flex;
    min-width: 0;
    color: var(--info);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .filespan-dir {
    min-width: 0;
    flex: 0 1 auto;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    direction: rtl;
    text-align: left;
  }
</style>
