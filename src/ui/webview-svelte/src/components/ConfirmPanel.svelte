<script lang="ts">
  import Icon from './Icon.svelte';

  type ConfirmType = 'info' | 'warning' | 'danger';

  interface Props {
    type?: ConfirmType;
    title: string;
    message: string;
    confirmText?: string;
    cancelText?: string;
    onConfirm: () => void;
    onCancel: () => void;
  }

  let {
    type = 'info',
    title,
    message,
    confirmText = '确认',
    cancelText = '取消',
    onConfirm,
    onCancel
  }: Props = $props();

  // 类型配置
  const typeConfig: Record<ConfirmType, { icon: string; color: string; btnClass: string }> = {
    info: { icon: 'info', color: 'var(--info)', btnClass: 'btn--primary' },
    warning: { icon: 'warning', color: 'var(--warning)', btnClass: 'btn--warning' },
    danger: { icon: 'warning', color: 'var(--error)', btnClass: 'btn--danger' }
  };

  const config = $derived(typeConfig[type]);
</script>

<div class="confirm-panel confirm-panel--{type}">
  <div class="confirm-icon" style="color: {config.color}">
    <Icon name={config.icon} size={24} />
  </div>
  
  <div class="confirm-content">
    <div class="confirm-title">{title}</div>
    <div class="confirm-message">{message}</div>
  </div>
  
  <div class="confirm-actions">
    <button class="btn btn--ghost" onclick={onCancel}>
      {cancelText}
    </button>
    <button class="btn {config.btnClass}" onclick={onConfirm}>
      {confirmText}
    </button>
  </div>
</div>

<style>
  .confirm-panel {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    padding: var(--space-4);
    background: var(--surface-1);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    margin: var(--space-3) 0;
  }

  .confirm-panel--warning { border-color: var(--warning); }
  .confirm-panel--danger { border-color: var(--error); }

  .confirm-icon {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 48px;
    height: 48px;
    border-radius: var(--radius-full);
    background: color-mix(in srgb, currentColor 10%, transparent);
    margin: 0 auto;
  }

  .confirm-content { text-align: center; }

  .confirm-title {
    font-size: var(--text-base);
    font-weight: 600;
    color: var(--foreground);
    margin-bottom: var(--space-1);
  }

  .confirm-message {
    font-size: var(--text-sm);
    color: var(--foreground-muted);
    line-height: 1.5;
  }

  .confirm-actions {
    display: flex;
    justify-content: center;
    gap: var(--space-2);
    margin-top: var(--space-2);
  }

  .btn {
    padding: var(--space-2) var(--space-4);
    border-radius: var(--radius-md);
    font-size: var(--text-sm);
    font-weight: 500;
    cursor: pointer;
    transition: all var(--transition-fast);
    border: 1px solid transparent;
  }

  .btn--ghost {
    background: transparent;
    color: var(--foreground-muted);
    border-color: var(--border);
  }
  .btn--ghost:hover { background: var(--surface-hover); color: var(--foreground); }

  .btn--primary { background: var(--info); color: white; }
  .btn--primary:hover { opacity: 0.9; }

  .btn--warning { background: var(--warning); color: black; }
  .btn--warning:hover { opacity: 0.9; }

  .btn--danger { background: var(--error); color: white; }
  .btn--danger:hover { opacity: 0.9; }
</style>

