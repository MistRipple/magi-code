<script lang="ts">
  import Icon from './Icon.svelte';
  import MarkdownContent from './MarkdownContent.svelte';
  import { i18n } from '../stores/i18n.svelte';

  interface Props {
    content: string;
    title: string;
    triggerLabel: string;
    triggerTitle?: string;
    maxWidth?: number;
    maxHeight?: number;
  }

  let {
    content,
    title,
    triggerLabel,
    triggerTitle,
    maxWidth = 680,
    maxHeight = 520,
  }: Props = $props();

  let rootEl = $state<HTMLSpanElement | null>(null);
  let triggerEl = $state<HTMLButtonElement | null>(null);
  let popover = $state<{ top: number; left: number; width: number; maxHeight: number } | null>(null);
  let copySuccess = $state(false);

  const normalizedContent = $derived(typeof content === 'string' ? content.trim() : '');

  function stopPropagation(event: Event) {
    event.stopPropagation();
  }

  function updatePopoverPosition() {
    if (!triggerEl) return;
    const rect = triggerEl.getBoundingClientRect();
    const viewportWidth = window.innerWidth;
    const viewportHeight = window.innerHeight;
    const width = Math.min(maxWidth, Math.max(280, viewportWidth - 24));
    const effectiveMaxHeight = Math.min(maxHeight, Math.max(220, viewportHeight - 24));
    const left = Math.max(12, Math.min(viewportWidth - width - 12, rect.left + rect.width - width));
    const preferredTop = rect.bottom + 8;
    const fallbackTop = rect.top - effectiveMaxHeight - 8;
    const top = preferredTop + effectiveMaxHeight <= viewportHeight - 12
      ? preferredTop
      : Math.max(12, fallbackTop);
    popover = { top, left, width, maxHeight: effectiveMaxHeight };
  }

  function togglePopover(event: MouseEvent) {
    event.stopPropagation();
    if (popover) {
      popover = null;
      return;
    }
    updatePopoverPosition();
  }

  function closePopover(event?: Event) {
    event?.stopPropagation();
    popover = null;
  }

  async function copyContent(event: MouseEvent) {
    event.stopPropagation();
    if (!normalizedContent) return;
    try {
      await navigator.clipboard.writeText(normalizedContent);
      copySuccess = true;
      setTimeout(() => {
        copySuccess = false;
      }, 2000);
    } catch (error) {
      console.error('复制详情内容失败:', error);
    }
  }

  $effect(() => {
    const root = rootEl;
    if (!popover || !root) return;

    const handleWindowClick = (event: MouseEvent) => {
      const path = typeof event.composedPath === 'function' ? event.composedPath() : [];
      if (path.includes(root)) return;
      const target = event.target as Node | null;
      if (target && root.contains(target)) return;
      popover = null;
    };
    const handleViewportChange = () => {
      popover = null;
    };
    const handleKeydown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        popover = null;
      }
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

<span class="markdown-detail-popover-root" bind:this={rootEl}>
  <button
    class="markdown-detail-trigger"
    bind:this={triggerEl}
    onclick={togglePopover}
    type="button"
    title={triggerTitle || triggerLabel}
  >
    {triggerLabel}
  </button>

  {#if popover}
    <div
      class="markdown-detail-popover"
      role="dialog"
      aria-modal="false"
      aria-label={title}
      tabindex="-1"
      style={`top:${popover.top}px;left:${popover.left}px;width:${popover.width}px;max-height:${popover.maxHeight}px;`}
      onclick={stopPropagation}
      onkeydown={stopPropagation}
    >
      <div class="markdown-detail-header">
        <span class="markdown-detail-title">{title}</span>
        <div class="markdown-detail-actions">
          <button
            class="markdown-detail-action-btn"
            onclick={copyContent}
            type="button"
            title={copySuccess ? i18n.t('detailPopover.copied') : i18n.t('detailPopover.copy')}
          >
            <Icon name={copySuccess ? 'check' : 'copy'} size={12} />
          </button>
          <button
            class="markdown-detail-action-btn"
            onclick={closePopover}
            type="button"
            title={i18n.t('detailPopover.close')}
          >
            <Icon name="close" size={12} />
          </button>
        </div>
      </div>
      <div class="markdown-detail-body">
        <MarkdownContent content={normalizedContent} />
      </div>
    </div>
  {/if}
</span>

<style>
  .markdown-detail-popover-root {
    display: inline-flex;
    align-items: center;
    flex-shrink: 0;
  }

  .markdown-detail-trigger,
  .markdown-detail-action-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    cursor: pointer;
    transition: all var(--transition-fast);
  }

  .markdown-detail-trigger {
    padding: 0;
    border: none;
    background: transparent;
    color: var(--primary);
    font-size: 11px;
    line-height: 1.4;
    text-decoration: underline;
    text-underline-offset: 2px;
  }

  .markdown-detail-trigger:hover {
    color: color-mix(in srgb, var(--primary) 82%, white);
  }

  .markdown-detail-trigger:focus-visible {
    outline: 2px solid var(--primary);
    outline-offset: 2px;
    border-radius: var(--radius-xs);
  }

  .markdown-detail-action-btn {
    width: 24px;
    height: 24px;
    border: 1px solid var(--border);
    background: var(--surface-2);
    color: var(--foreground-muted);
    border-radius: var(--radius-sm);
  }

  .markdown-detail-action-btn:hover {
    background: var(--surface-hover);
    color: var(--foreground);
  }

  .markdown-detail-popover {
    position: fixed;
    z-index: var(--z-popover);
    background: var(--vscode-editor-background, #1e1e1e);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    box-shadow: 0 18px 40px rgba(0, 0, 0, 0.35);
    padding: var(--space-3);
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }

  .markdown-detail-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
  }

  .markdown-detail-title {
    font-size: var(--text-sm);
    font-weight: var(--font-semibold);
    color: var(--foreground);
  }

  .markdown-detail-actions {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }

  .markdown-detail-body {
    overflow: auto;
    min-height: 0;
    color: var(--foreground);
  }

  .markdown-detail-body :global(.markdown) {
    font-size: var(--text-sm);
    line-height: 1.6;
    color: var(--foreground);
  }

  .markdown-detail-body :global(h1),
  .markdown-detail-body :global(h2),
  .markdown-detail-body :global(h3),
  .markdown-detail-body :global(h4),
  .markdown-detail-body :global(h5),
  .markdown-detail-body :global(h6) {
    margin-top: 0;
  }
</style>
