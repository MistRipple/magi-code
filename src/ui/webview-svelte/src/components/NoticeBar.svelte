<script lang="ts">
  import Icon from './Icon.svelte';

  type NoticeType = 'info' | 'success' | 'warning' | 'error' | 'loading';

  interface Props {
    type?: NoticeType;
    message: string;
    dismissable?: boolean;
    onDismiss?: () => void;
  }

  let {
    type = 'info',
    message,
    dismissable = false,
    onDismiss
  }: Props = $props();

  // 类型配置
  const typeConfig: Record<NoticeType, { icon: string; color: string }> = {
    info: { icon: 'info', color: 'var(--info)' },
    success: { icon: 'check', color: 'var(--success)' },
    warning: { icon: 'warning', color: 'var(--warning)' },
    error: { icon: 'close', color: 'var(--error)' },
    loading: { icon: 'loader', color: 'var(--info)' }
  };

  const config = $derived(typeConfig[type]);
</script>

<div class="notice-bar notice-bar--{type}" role="status">
  <span class="notice-icon" class:spinning={type === 'loading'} style="color: {config.color}">
    <Icon name={config.icon} size={14} />
  </span>
  <span class="notice-message">{message}</span>
  {#if dismissable}
    <button class="notice-dismiss" onclick={onDismiss} title="关闭">
      <Icon name="close" size={12} />
    </button>
  {/if}
</div>

<style>
  .notice-bar {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius-md);
    font-size: var(--text-sm);
    line-height: 1.4;
    background: var(--surface-2);
    border: 1px solid var(--border);
    margin: var(--space-2) 0;
  }

  .notice-bar--info { border-left: 3px solid var(--info); }
  .notice-bar--success { border-left: 3px solid var(--success); }
  .notice-bar--warning { border-left: 3px solid var(--warning); }
  .notice-bar--error { border-left: 3px solid var(--error); }
  .notice-bar--loading { border-left: 3px solid var(--info); }

  .notice-icon {
    display: flex;
    flex-shrink: 0;
  }

  .notice-icon.spinning {
    animation: spin 1s linear infinite;
  }

  @keyframes spin { to { transform: rotate(360deg); } }

  .notice-message {
    flex: 1;
    color: var(--foreground);
  }

  .notice-dismiss {
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 4px;
    background: transparent;
    border: none;
    color: var(--foreground-muted);
    cursor: pointer;
    border-radius: var(--radius-sm);
    transition: all var(--transition-fast);
  }

  .notice-dismiss:hover {
    background: var(--surface-hover);
    color: var(--foreground);
  }
</style>

