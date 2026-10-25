<script lang="ts">
  import { onMount, tick } from 'svelte';
  import type { Snippet } from 'svelte';
  import Icon from './Icon.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { hasOverlay } from '../lib/overlay-stack';

  interface Props {
    title?: string;
    onClose: () => void;
    busy?: boolean;
    pageClass?: string;
    bodyClass?: string;
    showHeader?: boolean;
    children?: Snippet;
    header?: Snippet;
    footer?: Snippet;
  }
  let { title, onClose, busy = false, pageClass = '', bodyClass = '', showHeader = true, children, header, footer }: Props = $props();
  let page: HTMLElement;
  let back: HTMLButtonElement;
  onMount(() => {
    const previous = document.activeElement as HTMLElement | null;
    void tick().then(() => back?.focus({ preventScroll: true }));
    const keydown = (event: KeyboardEvent) => {
      if (event.key !== 'Escape' || hasOverlay() || !page.checkVisibility()) return;
      event.preventDefault();
      event.stopImmediatePropagation();
      if (!busy) onClose();
    };
    window.addEventListener('keydown', keydown, true);
    return () => {
      window.removeEventListener('keydown', keydown, true);
      void tick().then(() => { if (previous?.isConnected) previous.focus({ preventScroll: true }); });
    };
  });
</script>

<section bind:this={page!} class="settings-detail-page settings-frame {pageClass}" aria-label={title} data-settings-detail-page>
  <div class="detail-back-row">
    <button bind:this={back!} type="button" class="btn btn--secondary btn--sm" onclick={onClose} disabled={busy}>
      <Icon name="chevron-left" size={14} />{i18n.t('overlay.backKeepDraft')}
    </button>
  </div>
  {#if showHeader}
    <header class="detail-header">
      {#if header}{@render header()}{:else}<h3>{title}</h3>{/if}
    </header>
  {/if}
  <div class="detail-body {bodyClass}">{@render children?.()}</div>
  {#if footer}<footer class="detail-footer">{@render footer()}</footer>{/if}
</section>

<style>
  .settings-detail-page { width: 100%; min-width: 0; box-sizing: border-box; color: var(--foreground); }
  .detail-back-row { flex-shrink: 0; padding: 16px 0; }
  .detail-header { flex-shrink: 0; display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: 16px; padding-bottom: 20px; }
  .detail-header h3 { margin: 0; font-size: var(--text-lg); }
  .detail-body { min-width: 0; }
  .detail-footer { flex-shrink: 0; display: flex; flex-wrap: wrap; justify-content: flex-end; gap: 8px; padding: 20px 0 0; }
</style>
