<script lang="ts">
  import Icon from './Icon.svelte';
  import FileSpan from './FileSpan.svelte';

  type OperationType = 'create' | 'edit' | 'delete' | 'read' | 'move' | 'copy';

  interface Props {
    operation: OperationType;
    filepath: string;
    targetPath?: string;  // 用于 move/copy 操作
    status?: 'pending' | 'success' | 'error';
    message?: string;
    onOpenFile?: (filepath: string) => void;
  }

  let {
    operation,
    filepath,
    targetPath,
    status = 'success',
    message,
    onOpenFile
  }: Props = $props();

  // 操作配置
  const operationConfig: Record<OperationType, { icon: string; label: string; color: string }> = {
    create: { icon: 'file-plus', label: '创建文件', color: 'var(--success)' },
    edit: { icon: 'file-edit', label: '编辑文件', color: 'var(--info)' },
    delete: { icon: 'file-minus', label: '删除文件', color: 'var(--error)' },
    read: { icon: 'file-text', label: '读取文件', color: 'var(--foreground-muted)' },
    move: { icon: 'folder', label: '移动文件', color: 'var(--warning)' },
    copy: { icon: 'copy', label: '复制文件', color: 'var(--info)' }
  };

  const config = $derived(operationConfig[operation] || operationConfig.read);

  // 状态图标
  const statusIcon = $derived(() => {
    switch (status) {
      case 'pending': return 'loader';
      case 'success': return 'check';
      case 'error': return 'close';
      default: return 'check';
    }
  });

  function handleOpenFile() {
    if (onOpenFile) {
      onOpenFile(filepath);
    }
  }
</script>

<div class="file-operation" class:error={status === 'error'} class:pending={status === 'pending'}>
  <div class="operation-icon" style="color: {config.color}">
    <Icon name={config.icon} size={16} />
  </div>
  
  <div class="operation-content">
    <div class="operation-header">
      <span class="operation-label">{config.label}</span>
      <span class="operation-status status-{status}">
        <Icon name={statusIcon()} size={12} />
      </span>
    </div>
    
    <div class="operation-files">
      <FileSpan {filepath} clickable={!!onOpenFile} onClick={handleOpenFile} />
      {#if targetPath}
        <span class="arrow">→</span>
        <FileSpan filepath={targetPath} clickable={!!onOpenFile} onClick={() => onOpenFile?.(targetPath)} />
      {/if}
    </div>
    
    {#if message}
      <div class="operation-message">{message}</div>
    {/if}
  </div>
</div>

<style>
  .file-operation {
    display: flex;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    margin: var(--space-2) 0;
  }

  .file-operation.error {
    border-color: var(--error);
    background: rgba(239, 68, 68, 0.05);
  }

  .file-operation.pending {
    border-color: var(--info);
  }

  .operation-icon {
    display: flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
    width: 24px;
    height: 24px;
  }

  .operation-content {
    flex: 1;
    min-width: 0;
  }

  .operation-header {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    margin-bottom: var(--space-1);
  }

  .operation-label {
    font-size: var(--text-xs);
    font-weight: 500;
    color: var(--foreground-muted);
    text-transform: uppercase;
    letter-spacing: 0.5px;
  }

  .operation-status {
    display: flex;
    align-items: center;
  }

  .status-pending { color: var(--info); animation: spin 1s linear infinite; }
  .status-success { color: var(--success); }
  .status-error { color: var(--error); }

  @keyframes spin { to { transform: rotate(360deg); } }

  .operation-files {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    flex-wrap: wrap;
  }

  .arrow {
    color: var(--foreground-muted);
    font-size: var(--text-sm);
  }

  .operation-message {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    margin-top: var(--space-1);
  }

  .error .operation-message {
    color: var(--error);
  }
</style>

