<script lang="ts">
  import { fade, scale } from 'svelte/transition';
  import { onMount, tick } from 'svelte';
  import type { Snippet } from 'svelte';
  import Icon from './Icon.svelte';

  interface Props {
    title?: string;
    onClose?: () => void;
    closeOnEscape?: boolean;
    closeOnBackdrop?: boolean;
    size?: 'sm' | 'md' | 'lg' | 'xl';
    modalClass?: string;
    bodyClass?: string;
    showHeader?: boolean;
    children?: Snippet;
    header?: Snippet;
    footer?: Snippet;
  }

  let {
    title,
    onClose,
    closeOnEscape = true,
    closeOnBackdrop = false,
    size = 'md',
    modalClass = '',
    bodyClass = '',
    showHeader = true,
    children,
    header,
    footer,
  }: Props = $props();

  let dialogEl: HTMLDivElement | undefined = $state();
  let overlayEl: HTMLDivElement | undefined = $state();
  let previouslyFocused: HTMLElement | null = null;

  function handleBackdropClick(e: MouseEvent) {
    if (e.target === overlayEl && closeOnBackdrop && onClose) {
      onClose();
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape' && closeOnEscape && onClose) {
      e.stopPropagation();
      onClose();
    }
    
    // Simple Focus Trap
    if (e.key === 'Tab' && dialogEl) {
      const focusableElements = dialogEl.querySelectorAll<HTMLElement>(
        'a[href], button:not([disabled]), textarea:not([disabled]), input:not([disabled]), select:not([disabled]), [tabindex]:not([tabindex="-1"])'
      );
      if (focusableElements.length === 0) {
        e.preventDefault();
        return;
      }
      const first = focusableElements[0];
      const last = focusableElements[focusableElements.length - 1];

      if (e.shiftKey && document.activeElement === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    }
  }

  onMount(() => {
    previouslyFocused = document.activeElement as HTMLElement;
    tick().then(() => {
      if (dialogEl) {
        // Focus first element or dialog itself
        const focusable = dialogEl.querySelector<HTMLElement>(
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
      // Restore focus when modal unmounts
      if (previouslyFocused && typeof previouslyFocused.focus === 'function') {
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
  role="presentation"
  onclick={handleBackdropClick}
  onkeydown={handleKeydown}
  transition:fade={{ duration: 150 }}
>
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div
    bind:this={dialogEl}
    class="modal-dialog modal-dialog--{size}"
    role="dialog"
    aria-modal="true"
    aria-labelledby={title ? 'modal-title' : undefined}
    tabindex="-1"
    onclick={(e) => e.stopPropagation()}
    transition:scale={{ duration: 150, start: 0.95 }}
  >
    {#if showHeader}
      <div class="modal-header">
        {#if header}
          {@render header()}
        {:else}
          <div class="modal-title" id="modal-title">{title}</div>
          {#if onClose}
            <button class="modal-close" type="button" onclick={onClose} aria-label="Close" title="Close">
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
    width: 320px;
  }
  .modal-dialog--md {
    width: 480px;
  }
  .modal-dialog--lg {
    width: 640px;
  }
  .modal-dialog--xl {
    width: 800px;
  }
  
  .modal-overlay {
    /* Using higher z-index to ensure it sits on top */
    z-index: 10000;
  }
</style>
