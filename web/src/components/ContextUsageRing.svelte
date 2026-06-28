<script lang="ts">
  import { i18n } from '../stores/i18n.svelte';
  import {
    buildRingDetailItems,
    buildRingTooltip,
    resolveRingView,
    type ContextRingTone,
  } from '../lib/context-usage-ring';

  interface Props {
    usageRatio?: number | null;
    tokenUsed?: number | null;
    remainingTokens?: number | null;
    tokenLimit?: number | null;
    warningLevel?: ContextRingTone | null;
  }

  let {
    usageRatio = null,
    tokenUsed = null,
    remainingTokens = null,
    tokenLimit = null,
    warningLevel = null,
  }: Props = $props();

  // 上下文窗口占用率圆环：只负责统计详情，不承载模型切换行为。
  const input = $derived({
    usageRatio,
    tokenUsed,
    remainingTokens,
    tokenLimit,
    warningLevel,
  });

  const view = $derived(resolveRingView(input));
  const tooltip = $derived(buildRingTooltip(input, (key, params) => i18n.t(key, params)));
  const detailItems = $derived(buildRingDetailItems(input, (key, params) => i18n.t(key, params)));
  const popoverId = `context-usage-ring-${Math.random().toString(36).slice(2)}`;

  let rootEl = $state<HTMLSpanElement | null>(null);
  let pinned = $state(false);

  function toggleDetails(event: MouseEvent) {
    event.stopPropagation();
    pinned = !pinned;
  }

  function handleKeydown(event: KeyboardEvent) {
    if (event.key === 'Escape') {
      pinned = false;
    }
  }

  $effect(() => {
    if (!pinned || !rootEl) return;
    const root = rootEl;
    const handleWindowClick = (event: MouseEvent) => {
      const path = typeof event.composedPath === 'function' ? event.composedPath() : [];
      if (path.includes(root)) return;
      const target = event.target as Node | null;
      if (target && root.contains(target)) return;
      pinned = false;
    };
    window.addEventListener('click', handleWindowClick);
    return () => {
      window.removeEventListener('click', handleWindowClick);
    };
  });
</script>

<span class="ia-context-ring-wrap" bind:this={rootEl}>
  <button
    type="button"
    class="ia-context-ring tone-{view.tone}"
    class:empty={!view.hasData}
    data-testid="context-usage-ring"
    onclick={toggleDetails}
    onkeydown={handleKeydown}
    title={tooltip}
    aria-label={tooltip}
    aria-expanded={pinned}
    aria-describedby={popoverId}
  >
    <svg viewBox="0 0 18 18" width="18" height="18" aria-hidden="true">
      <circle class="ring-track" cx="9" cy="9" r={view.geometry.radius} fill="none" stroke-width="2" />
      {#if view.hasData}
        <circle
          class="ring-fill"
          cx="9"
          cy="9"
          r={view.geometry.radius}
          fill="none"
          stroke-width="2"
          stroke-linecap="round"
          stroke-dasharray={view.geometry.circumference}
          stroke-dashoffset={view.geometry.dashOffset}
          transform="rotate(-90 9 9)"
        />
      {/if}
    </svg>
    <span class="ring-label">{view.labelText}</span>
  </button>

  <div
    id={popoverId}
    class="ia-context-popover"
    class:visible={pinned}
    role="tooltip"
  >
    <div class="context-popover-title">
      <span>{i18n.t('input.contextRing.label')}</span>
      <strong>{view.labelText}</strong>
    </div>
    {#if view.hasData}
      <div class="context-popover-list">
        {#each detailItems as item (item.key)}
          <span>{item.text}</span>
        {/each}
      </div>
    {:else}
      <div class="context-popover-empty">{i18n.t('input.contextRing.empty')}</div>
    {/if}
  </div>
</span>

<style>
  .ia-context-ring-wrap {
    display: inline-flex;
    flex-shrink: 0;
    position: relative;
  }

  .ia-context-ring {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    height: 24px;
    padding: 0 6px 0 4px;
    border: 0;
    border-radius: var(--radius-full);
    background: var(--surface-2);
    color: var(--foreground-muted);
    font-size: 11px;
    line-height: 1;
    font-family: inherit;
    user-select: none;
    cursor: pointer;
    flex-shrink: 0;
    transition:
      background var(--transition-fast),
      color var(--transition-fast),
      box-shadow var(--transition-fast);
  }

  .ia-context-ring:hover,
  .ia-context-ring:focus-visible,
  .ia-context-ring[aria-expanded="true"] {
    background: var(--surface-hover);
    box-shadow: inset 0 0 0 1px var(--border);
  }

  .ia-context-ring:focus-visible {
    outline: 2px solid var(--primary);
    outline-offset: 2px;
  }

  .ia-context-ring svg {
    flex-shrink: 0;
  }

  .ring-track {
    stroke: var(--border);
  }

  .ring-fill {
    stroke: var(--primary);
    transition: stroke-dashoffset var(--transition-fast);
  }

  .ring-label {
    font-variant-numeric: tabular-nums;
    color: var(--foreground);
  }

  .ia-context-ring.empty .ring-label {
    color: var(--foreground-muted);
  }

  .ia-context-popover {
    position: absolute;
    right: 0;
    bottom: calc(100% + 8px);
    z-index: var(--z-popover);
    display: flex;
    flex-direction: column;
    gap: 8px;
    width: min(220px, calc(100vw - 24px));
    padding: 10px;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--background);
    color: var(--foreground);
    box-shadow: 0 16px 36px rgba(0, 0, 0, 0.28);
    font-size: 12px;
    line-height: 1.35;
    opacity: 0;
    visibility: hidden;
    pointer-events: none;
    transform: translateY(4px);
    transition:
      opacity var(--transition-fast),
      transform var(--transition-fast),
      visibility var(--transition-fast);
  }

  .ia-context-ring-wrap:hover .ia-context-popover,
  .ia-context-ring-wrap:focus-within .ia-context-popover,
  .ia-context-popover.visible {
    opacity: 1;
    visibility: visible;
    pointer-events: auto;
    transform: translateY(0);
  }

  .context-popover-title {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    color: var(--foreground-muted);
  }

  .context-popover-title strong {
    color: var(--foreground);
    font-size: 13px;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }

  .context-popover-list {
    display: flex;
    flex-direction: column;
    gap: 4px;
    color: var(--foreground);
  }

  .context-popover-empty {
    color: var(--foreground-muted);
  }

  .tone-notice .ring-fill { stroke: var(--warning, #d6a700); }
  .tone-notice .ring-label { color: var(--warning, #d6a700); }
  .tone-warning .ring-fill { stroke: var(--warning, #e08600); }
  .tone-warning .ring-label { color: var(--warning, #e08600); }
  .tone-danger .ring-fill { stroke: var(--error, #d64545); }
  .tone-danger .ring-label { color: var(--error, #d64545); }
</style>
