<script lang="ts">
  import Icon from './Icon.svelte';

  interface Props {
    filepath: string;
    showIcon?: boolean;
    clickable?: boolean;
    onClick?: (filepath: string) => void;
  }

  let {
    filepath,
    showIcon = true,
    clickable = true,
    onClick
  }: Props = $props();

  // 获取文件名
  const filename = $derived(filepath.split('/').pop() || filepath);
  
  // 获取目录路径
  const directory = $derived(() => {
    const parts = filepath.split('/');
    parts.pop();
    return parts.join('/');
  });

  // 获取文件扩展名图标
  function getFileIcon(path: string): string {
    const ext = path.split('.').pop()?.toLowerCase();
    const iconMap: Record<string, string> = {
      'ts': 'typescript',
      'tsx': 'react',
      'js': 'javascript',
      'jsx': 'react',
      'svelte': 'svelte',
      'vue': 'vue',
      'py': 'python',
      'go': 'go',
      'rs': 'rust',
      'java': 'java',
      'md': 'markdown',
      'json': 'json',
      'css': 'css',
      'scss': 'sass',
      'html': 'html',
      'yaml': 'yaml',
      'yml': 'yaml',
      'toml': 'settings',
      'sh': 'terminal',
      'bash': 'terminal',
      'sql': 'database',
    };
    return iconMap[ext || ''] || 'file';
  }

  const fileIcon = $derived(getFileIcon(filepath));

  function handleClick() {
    if (clickable && onClick) {
      onClick(filepath);
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      handleClick();
    }
  }
</script>

{#if clickable}
  <button 
    class="filespan" 
    class:clickable
    onclick={handleClick}
    onkeydown={handleKeydown}
    title={filepath}
  >
    {#if showIcon}
      <span class="filespan-icon">
        <Icon name={fileIcon} size={14} />
      </span>
    {/if}
    <span class="filespan-name">{filename}</span>
    {#if directory()}
      <span class="filespan-dir">{directory()}</span>
    {/if}
  </button>
{:else}
  <span class="filespan" title={filepath}>
    {#if showIcon}
      <span class="filespan-icon">
        <Icon name={fileIcon} size={14} />
      </span>
    {/if}
    <span class="filespan-name">{filename}</span>
  </span>
{/if}

<style>
  .filespan {
    display: inline-flex;
    align-items: center;
    gap: var(--space-1);
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
    color: var(--info);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .filespan-dir {
    color: var(--foreground-muted);
    font-size: 0.85em;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    direction: rtl;
    text-align: left;
  }
</style>

