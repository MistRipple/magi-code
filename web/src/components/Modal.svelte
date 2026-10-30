<script lang="ts">
  import { fade, scale } from 'svelte/transition';
  import { onMount, tick } from 'svelte';
  import type { Snippet } from 'svelte';
  import Icon from './Icon.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { topOverlay, registerOverlay } from '../lib/overlay-stack';

  interface Props {
    title?: string;
    onClose?: () => void;
    closeOnEscape?: boolean;
    closeOnBackdrop?: boolean;
    /** 表单确认窗口不允许点蒙层丢失草稿。 */
    form?: boolean;
    size?: 'sm' | 'md' | 'lg' | 'xl';
    modalClass?: string;
    bodyClass?: string;
    showHeader?: boolean;
    initialFocusSelector?: string;
    children?: Snippet;
    header?: Snippet;
    footer?: Snippet;
  }

  let {
    title,
    onClose,
    closeOnEscape = true,
    closeOnBackdrop = false,
    form = false,
    size = 'md',
    modalClass = '',
    bodyClass = '',
    showHeader = true,
    initialFocusSelector,
    children,
    header,
    footer,
  }: Props = $props();

  let dialogEl: HTMLDivElement | undefined = $state();
  let overlayEl: HTMLDivElement | undefined = $state();
  const layer = Symbol('overlay');
  const titleId = $props.id();
  let reducedMotion = $state(false);
  let previouslyFocused: HTMLElement | null = null;

  function handleBackdropClick(e: MouseEvent) {
    if (e.target === overlayEl && !form && closeOnBackdrop && onClose) {
      onClose();
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    if (!topOverlay(layer)) return;
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopImmediatePropagation();
      if (closeOnEscape) onClose?.();
    }
    
    // Simple Focus Trap
    if (e.key === 'Tab' && dialogEl) {
      const focusableElements = [...dialogEl.querySelectorAll<HTMLElement>(
        'a[href], button:not([disabled]), textarea:not([disabled]), input:not([disabled]), select:not([disabled]), [tabindex]:not([tabindex="-1"])'
      )].filter((element) => element.getClientRects().length && !element.closest('[inert]'));
      if (focusableElements.length === 0) {
        e.preventDefault();
        return;
      }
      const first = focusableElements[0];
      const last = focusableElements[focusableElements.length - 1];

      if (e.shiftKey && (document.activeElement === first || !dialogEl.contains(document.activeElement))) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && (document.activeElement === last || document.activeElement === dialogEl || !dialogEl.contains(document.activeElement))) {
        e.preventDefault();
        first.focus();
      }
    }
  }

  onMount(() => {
    reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
    const unregister = registerOverlay(layer);
    window.addEventListener('keydown', handleKeydown, true);
    previouslyFocused = document.activeElement as HTMLElement;
    tick().then(() => {
      if (dialogEl && topOverlay(layer)) {
        // Focus first element or dialog itself
        const focusable = (initialFocusSelector ? dialogEl.querySelector<HTMLElement>(initialFocusSelector) : null) ?? dialogEl.querySelector<HTMLElement>(
          'button:not([disabled]), input:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])'
        );
        if (focusable) {
          focusable.focus();
        } else {
          dialogEl.focus();
        }
      }
    });

    return () => {
      const wasTop = topOverlay(layer);
      unregister();
      window.removeEventListener('keydown', handleKeydown, true);
      // Restore focus when modal unmounts
      if (wasTop && previouslyFocused?.isConnected && typeof previouslyFocused.focus === 'function') {
        previouslyFocused.focus();
      }
    };
  });
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<!-- svelte-ignore a11y_click_events_have_key_events -->
<div
  bind:this={overlayEl}
  class="modal-overlay {modalClass}"
  data-overlay-kind="modal"
  data-form={form}
  role="presentation"
  onclick={handleBackdropClick}
  transition:fade={{ duration: reducedMotion ? 0 : 150 }}
>
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div
    bind:this={dialogEl}
    class="modal-dialog modal-dialog--{size}"
    data-magi-surface="window"
    role="dialog"
    aria-modal="true"
    aria-labelledby={showHeader && (title || header) ? titleId : undefined}
    aria-label={!showHeader ? title : undefined}
    tabindex="-1"
    onclick={(e) => e.stopPropagation()}
    transition:scale={{ duration: reducedMotion ? 0 : 150, start: 0.95 }}
  >
    {#if showHeader}
      <div class="modal-header" id={titleId}>
        {#if header}
          {@render header()}
        {:else}
          <div class="modal-title">{title}</div>
          {#if onClose}
            <button class="modal-close" type="button" onclick={onClose} aria-label={i18n.t('common.close')} title={i18n.t('common.close')}>
              <Icon name="close" size={20} />
            </button>
          {/if}
        {/if}
      </div>
    {/if}

    <div class="modal-body {bodyClass}">
      {@render children?.()}
    </div>

    {#if footer}
      <div class="modal-footer">
        {@render footer()}
      </div>
    {/if}
  </div>
</div>

<style>
  .modal-dialog--sm {
    width: 400px;
  }
  .modal-dialog--md {
    width: 520px;
  }
  .modal-dialog--lg {
    width: 720px;
  }
  .modal-dialog--xl {
    width: 800px;
  }
  
  .modal-overlay {
    /* Modal 通用对话框：高于设置主面板（--z-modal），用于设置内的二级确认弹窗 */
    z-index: var(--z-modal-secondary);
  }
</style>
