<script lang="ts">
  import { getState } from '../stores/messages.svelte';
  import { ensureArray } from '../lib/utils';
  import type { Toast } from '../types/message';
  import Icon from './Icon.svelte';
  import { i18n } from '../stores/i18n.svelte';

  const appState = getState();

  // Toast 列表
  const toasts = $derived(ensureArray(appState.toasts) as Toast[]);

  // 自动关闭定时器（默认 5 秒，可被单条 toast 覆盖）
  const AUTO_DISMISS_MS = 5000;

  // 跟踪已设置定时器的 toast ID
  const activeTimers = new Map<string, ReturnType<typeof setTimeout>>();

  // 移除 toast
  function removeToast(id: string) {
    // 清除定时器
    const timer = activeTimers.get(id);
    if (timer) {
      clearTimeout(timer);
      activeTimers.delete(id);
    }
    appState.toasts = (ensureArray(appState.toasts) as Toast[]).filter(t => t.id !== id);
  }

  // 为新增的 toast 设置自动关闭
  $effect(() => {
    const currentToasts = toasts;
    const currentIds = new Set(currentToasts.map(t => t.id));

    // 为新 toast 设置定时器
    for (const toast of currentToasts) {
      if (!activeTimers.has(toast.id)) {
        const dismissAfter = typeof toast.duration === 'number' && Number.isFinite(toast.duration) && toast.duration > 0
          ? toast.duration
          : AUTO_DISMISS_MS;
        const timer = setTimeout(() => {
          activeTimers.delete(toast.id);
          appState.toasts = (ensureArray(appState.toasts) as Toast[]).filter(t => t.id !== toast.id);
        }, dismissAfter);
        activeTimers.set(toast.id, timer);
      }
    }

    // 清理已移除 toast 的定时器
    for (const [id, timer] of activeTimers) {
      if (!currentIds.has(id)) {
        clearTimeout(timer);
        activeTimers.delete(id);
      }
    }
  });
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
      <button class="toast-close" onclick={() => removeToast(toast.id)} title={i18n.t('toastContainer.closeNotification')}>
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
    display: -webkit-box;
    -webkit-line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
    word-break: break-word;
  }

  .toast-message {
    font-size: var(--text-sm);
    color: var(--foreground-muted);
    line-height: var(--leading-normal);
    display: -webkit-box;
    -webkit-line-clamp: 3;
    -webkit-box-orient: vertical;
    overflow: hidden;
    word-break: break-word;
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
