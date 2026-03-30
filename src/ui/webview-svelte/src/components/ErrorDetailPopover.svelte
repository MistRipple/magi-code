<script lang="ts">
  import Icon from './Icon.svelte';
  import { i18n } from '../stores/i18n.svelte';

  interface Props {
    text: string;
    maxInlineChars?: number;
  }

  let {
    text,
    maxInlineChars = 80,
  }: Props = $props();

  let rootEl = $state<HTMLSpanElement | null>(null);
  let triggerEl = $state<HTMLButtonElement | null>(null);
  let popover = $state<{ top: number; left: number; width: number; maxHeight: number } | null>(null);
  let copySuccess = $state(false);

  const normalizedText = $derived(typeof text === 'string' ? text.trim() : '');
  const inlineText = $derived(normalizedText.replace(/\s+/g, ' ').trim());
  const shouldShowMore = $derived.by(() => normalizedText.includes('\n') || inlineText.length > maxInlineChars);

  function updatePopoverPosition() {
    if (!triggerEl) return;
    const rect = triggerEl.getBoundingClientRect();
    const width = Math.min(560, window.innerWidth - 24);
    const maxHeight = Math.min(360, window.innerHeight - 24);
    const left = Math.max(12, Math.min(window.innerWidth - width - 12, rect.left));
    const preferredTop = rect.bottom + 8;
    const fallbackTop = rect.top - maxHeight - 8;
    const top = preferredTop + maxHeight <= window.innerHeight - 12
      ? preferredTop
      : Math.max(12, fallbackTop);
    popover = { top, left, width, maxHeight };
  }

  function togglePopover(event: MouseEvent) {
    event.stopPropagation();
    if (popover) {
      popover = null;
      return;
    }
    updatePopoverPosition();
  }

  function closePopover() {
    popover = null;
  }

  async function copyText(event: MouseEvent) {
    event.stopPropagation();
    if (!normalizedText) return;
    try {
      await navigator.clipboard.writeText(normalizedText);
      copySuccess = true;
      setTimeout(() => { copySuccess = false; }, 2000);
    } catch (error) {
      console.error('复制错误详情失败:', error);
    }
  }

  $effect(() => {
    if (!popover || !rootEl) return;
    const handleWindowClick = (event: MouseEvent) => {
      const path = typeof event.composedPath === 'function' ? event.composedPath() : [];
      if (rootEl && path.includes(rootEl)) return;
      const target = event.target as Node | null;
      if (target && rootEl?.contains(target)) return;
      closePopover();
    };
    const handleViewportChange = () => closePopover();
    const handleKeydown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') closePopover();
    };
    window.addEventListener('click', handleWindowClick);
    window.addEventListener('resize', handleViewportChange);
    window.addEventListener('keydown', handleKeydown);
    return () => {
      window.removeEventListener('click', handleWindowClick);
      window.removeEventListener('resize', handleViewportChange);
      window.removeEventListener('keydown', handleKeydown);
    };
  });
</script>

<span class="error-detail" bind:this={rootEl}>
  <span class="error-summary" title={normalizedText}>{inlineText}</span>
  {#if shouldShowMore}
    <button class="error-more-btn" bind:this={triggerEl} onclick={togglePopover} type="button" title={i18n.t('errorDetail.more')}>
      {i18n.t('errorDetail.more')}
    </button>
  {/if}

  {#if popover}
    <div class="error-popover" style={`top:${popover.top}px;left:${popover.left}px;width:${popover.width}px;max-height:${popover.maxHeight}px;`}>
      <div class="error-popover-header">
        <span class="error-popover-title">{i18n.t('errorDetail.title')}</span>
        <div class="error-popover-actions">
          <button class="error-popover-btn" onclick={copyText} type="button" title={copySuccess ? i18n.t('errorDetail.copied') : i18n.t('errorDetail.copy')}>
            <Icon name={copySuccess ? 'check' : 'copy'} size={12} />
          </button>
          <button class="error-popover-btn" onclick={() => closePopover()} type="button" title={i18n.t('errorDetail.close')}>
            <Icon name="close" size={12} />
          </button>
        </div>
      </div>
      <pre class="error-popover-body">{normalizedText}</pre>
    </div>
  {/if}
</span>

<style>
  .error-detail { display: inline-flex; align-items: center; gap: 6px; min-width: 0; max-width: 100%; }
  .error-summary { min-width: 0; max-width: 100%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: inherit; font: inherit; }
  .error-more-btn, .error-popover-btn { display: inline-flex; align-items: center; justify-content: center; flex-shrink: 0; cursor: pointer; transition: all var(--transition-fast); }
  .error-more-btn { padding: 0; border: none; background: transparent; color: var(--primary); font-size: inherit; line-height: 1.4; text-decoration: underline; text-underline-offset: 2px; }
  .error-popover-btn { width: 24px; height: 24px; }
  .error-more-btn:hover { color: var(--primary-hover); }
  .error-more-btn:focus-visible { outline: 2px solid var(--primary); outline-offset: 2px; border-radius: var(--radius-xs); }
  .error-popover-btn { border: 1px solid var(--border); background: var(--surface-2); color: var(--foreground-muted); border-radius: var(--radius-sm); }
  .error-popover-btn:hover { background: var(--surface-hover); color: var(--foreground); }
  .error-popover { position: fixed; z-index: var(--z-popover); background: var(--background); border: 1px solid var(--border); border-radius: var(--radius-md); box-shadow: 0 18px 40px rgba(0, 0, 0, 0.35); padding: var(--space-3); display: flex; flex-direction: column; gap: var(--space-3); }
  .error-popover-header { display: flex; align-items: center; justify-content: space-between; gap: var(--space-3); }
  .error-popover-title { font-size: var(--text-sm); font-weight: var(--font-semibold); color: var(--foreground); }
  .error-popover-actions { display: flex; align-items: center; gap: var(--space-2); }
  .error-popover-body { margin: 0; overflow: auto; white-space: pre-wrap; word-break: break-word; font-family: var(--font-mono); font-size: var(--text-xs); line-height: 1.5; color: var(--foreground); }
</style>
