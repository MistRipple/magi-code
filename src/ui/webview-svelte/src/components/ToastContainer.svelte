<script lang="ts">
  import { getState } from '../stores/messages.svelte';
  import { ensureArray } from '../lib/utils';
  import Icon from './Icon.svelte';

  const appState = getState();

  // Toast 列表
  const toasts = $derived(ensureArray(appState.toasts));

  // 移除 toast
  function removeToast(id: string) {
    appState.toasts = ensureArray(appState.toasts).filter(t => t.id !== id);
  }
</script>

<div class="toast-container">
  {#each toasts as toast (toast.id)}
    <div class="toast" class:success={toast.type === 'success'} class:error={toast.type === 'error'} class:warning={toast.type === 'warning'}>
      <div class="toast-icon">
        {#if toast.type === 'success'}
          <Icon name="check" size={16} />
        {:else if toast.type === 'error'}
          <Icon name="close" size={16} />
        {:else if toast.type === 'warning'}
          <Icon name="warning" size={16} />
        {:else}
          <Icon name="info" size={16} />
        {/if}
      </div>
      <div class="toast-content">
        {#if toast.title}
          <div class="toast-title">{toast.title}</div>
        {/if}
        <div class="toast-message">{toast.message}</div>
      </div>
      <button class="toast-close" onclick={() => removeToast(toast.id)} title="关闭通知">
        <Icon name="close" size={12} />
      </button>
    </div>
  {/each}
</div>

<style>
  .toast-container {
    position: fixed;
    bottom: var(--space-4);
    right: var(--space-4);
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    z-index: var(--z-toast);
    pointer-events: none;
  }

  .toast {
    display: flex;
    align-items: flex-start;
    gap: var(--space-3);
    padding: var(--space-3) var(--space-4);
    background: var(--background);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-lg);
    pointer-events: auto;
    animation: slideIn var(--duration-normal) var(--ease-out);
    max-width: 320px;
  }

  @keyframes slideIn {
    from {
      opacity: 0;
      transform: translateX(16px);
    }
    to {
      opacity: 1;
      transform: translateX(0);
    }
  }

  .toast.success { border-left: 3px solid var(--success); }
  .toast.error { border-left: 3px solid var(--error); }
  .toast.warning { border-left: 3px solid var(--warning); }

  .toast-icon {
    flex-shrink: 0;
    width: var(--icon-md);
    height: var(--icon-md);
    margin-top: var(--space-1);
  }

  .toast.success .toast-icon { color: var(--success); }
  .toast.error .toast-icon { color: var(--error); }
  .toast.warning .toast-icon { color: var(--warning); }

  .toast-content {
    flex: 1;
    min-width: 0;
  }

  .toast-title {
    font-size: var(--text-sm);
    font-weight: var(--font-semibold);
    color: var(--foreground);
    margin-bottom: var(--space-1);
  }

  .toast-message {
    font-size: var(--text-sm);
    color: var(--foreground-muted);
    line-height: var(--leading-normal);
  }

  .toast-close {
    flex-shrink: 0;
    width: var(--btn-height-xs);
    height: var(--btn-height-xs);
    padding: 0;
    background: transparent;
    border: none;
    color: var(--foreground-muted);
    cursor: pointer;
    border-radius: var(--radius-sm);
    display: flex;
    align-items: center;
    justify-content: center;
    transition: all var(--transition-fast);
  }

  .toast-close:hover {
    background: var(--surface-hover);
    color: var(--foreground);
  }
</style>
