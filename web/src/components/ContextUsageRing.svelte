<script lang="ts">
  import { i18n } from '../stores/i18n.svelte';
  import {
    buildRingDetailItems,
    buildRingTooltip,
    resolveRingView,
    type ContextRingTone,
  } from '../lib/context-usage-ring';
  import Icon from './Icon.svelte';

  interface Props {
    model?: string;
    usageRatio?: number | null;
    tokenUsed?: number | null;
    remainingTokens?: number | null;
    tokenLimit?: number | null;
    warningLevel?: ContextRingTone | null;
    lastCompactionReason?: string | null;
    originalTokenEstimate?: number | null;
    compactedTokenEstimate?: number | null;
    measurement?: 'estimated' | 'authoritative' | null;
    onSaveContextWindow?: (contextWindowTokens: number) => Promise<void> | void;
  }

  let {
    model = '',
    usageRatio = null,
    tokenUsed = null,
    remainingTokens = null,
    tokenLimit = null,
    warningLevel = null,
    lastCompactionReason = null,
    originalTokenEstimate = null,
    compactedTokenEstimate = null,
    measurement = null,
    onSaveContextWindow,
  }: Props = $props();

  // 上下文窗口占用率圆环：只负责统计详情，不承载模型切换行为。
  const input = $derived({
    usageRatio,
    tokenUsed,
    remainingTokens,
    tokenLimit,
    warningLevel,
    lastCompactionReason,
    originalTokenEstimate,
    compactedTokenEstimate,
    measurement,
  });

  const view = $derived(resolveRingView(input));
  const tooltip = $derived(buildRingTooltip(input, (key, params) => i18n.t(key, params)));
  const detailItems = $derived(buildRingDetailItems(input, (key, params) => i18n.t(key, params)));
  const popoverId = `context-usage-ring-${Math.random().toString(36).slice(2)}`;

  let rootEl = $state<HTMLSpanElement | null>(null);
  let pinned = $state(false);
  let editing = $state(false);
  let draftWindowValue = $state('');
  let draftWindowUnit = $state<'K' | 'M'>('K');
  let saving = $state(false);
  let saveError = $state('');

  function beginEditing() {
    const tokens = tokenLimit ?? 256_000;
    if (tokens >= 1_000_000 && tokens % 1_000_000 === 0) {
      draftWindowValue = String(tokens / 1_000_000);
      draftWindowUnit = 'M';
    } else {
      draftWindowValue = String(tokens / 1_000);
      draftWindowUnit = 'K';
    }
    saveError = '';
    editing = true;
  }

  function cancelEditing() {
    editing = false;
    saveError = '';
  }

  async function saveContextWindow() {
    if (!onSaveContextWindow || saving) return;
    const numericValue = Number(draftWindowValue);
    const multiplier = draftWindowUnit === 'M' ? 1_000_000 : 1_000;
    const tokens = Math.round(numericValue * multiplier);
    if (!Number.isFinite(tokens) || tokens < 16_000 || tokens > 10_000_000) {
      saveError = i18n.t('input.contextRing.invalidWindow');
      return;
    }
    saving = true;
    saveError = '';
    try {
      await onSaveContextWindow(tokens);
      editing = false;
    } catch (error) {
      console.warn('[ContextUsageRing] 保存模型上下文窗口失败:', error);
      saveError = error instanceof Error && error.message.trim()
        ? error.message
        : i18n.t('input.contextRing.saveFailed');
    } finally {
      saving = false;
    }
  }

  function toggleDetails(event: MouseEvent) {
    event.stopPropagation();
    pinned = !pinned;
  }

  function handleKeydown(event: KeyboardEvent) {
    if (event.key === 'Escape') {
      if (editing) {
        cancelEditing();
      } else {
        pinned = false;
      }
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
    class:estimating={measurement === 'estimated'}
    data-testid="context-usage-ring"
    onclick={toggleDetails}
    onkeydown={handleKeydown}
    title={i18n.t('input.contextRing.label')}
    aria-label={tooltip}
    aria-expanded={pinned}
    aria-controls={popoverId}
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
    {#if onSaveContextWindow && model}
      <div class="context-window-editor">
        {#if editing}
          <div class="context-window-input-row">
            <input
              type="number"
              min={draftWindowUnit === 'M' ? 0.016 : 16}
              max={draftWindowUnit === 'M' ? 10 : 10000}
              step={draftWindowUnit === 'M' ? 0.1 : 1}
              bind:value={draftWindowValue}
              disabled={saving}
              aria-label={i18n.t('input.contextRing.edit')}
              onkeydown={(event) => {
                if (event.key === 'Enter') void saveContextWindow();
              }}
            />
            <select
              bind:value={draftWindowUnit}
              disabled={saving}
              aria-label={i18n.t('input.contextRing.windowUnit')}
            >
              <option value="K">K</option>
              <option value="M">M</option>
            </select>
            <button
              type="button"
              class="context-window-icon-button"
              disabled={saving}
              title={i18n.t('input.contextRing.save')}
              aria-label={i18n.t('input.contextRing.save')}
              onclick={() => void saveContextWindow()}
            ><Icon name={saving ? 'loader' : 'check'} size={13} class={saving ? 'spinning' : ''} /></button>
            <button
              type="button"
              class="context-window-icon-button"
              disabled={saving}
              title={i18n.t('input.contextRing.cancel')}
              aria-label={i18n.t('input.contextRing.cancel')}
              onclick={cancelEditing}
            ><Icon name="x" size={13} /></button>
          </div>
          {#if saveError}<span class="context-window-error">{saveError}</span>{/if}
        {:else}
          <button
            type="button"
            class="context-window-edit-button"
            onclick={beginEditing}
          >
            <Icon name="edit" size={12} />
            <span>{i18n.t('input.contextRing.edit')}</span>
          </button>
        {/if}
      </div>
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
    justify-content: center;
    width: 24px;
    height: 24px;
    padding: 0;
    border: 0;
    border-radius: var(--radius-full);
    background: var(--surface-2);
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

  .ia-context-ring.estimating .ring-fill {
    animation: context-ring-estimating 1.4s ease-in-out infinite;
  }

  @keyframes context-ring-estimating {
    0%, 100% { opacity: 0.55; }
    50% { opacity: 1; }
  }

  @media (prefers-reduced-motion: reduce) {
    .ia-context-ring.estimating .ring-fill {
      animation: none;
    }
  }

  .ia-context-popover {
    position: absolute;
    right: 0;
    bottom: calc(100% + 8px);
    z-index: var(--z-popover);
    display: flex;
    flex-direction: column;
    gap: 8px;
    width: max-content;
    max-width: min(260px, calc(100vw - 24px));
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

  .context-window-editor {
    padding-top: 8px;
    border-top: 1px solid var(--border);
  }

  .context-window-edit-button,
  .context-window-icon-button {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border: 0;
    background: transparent;
    color: var(--foreground-muted);
    font: inherit;
    cursor: pointer;
  }

  .context-window-edit-button {
    gap: 5px;
    padding: 2px 0;
  }

  .context-window-edit-button:hover,
  .context-window-icon-button:hover:not(:disabled) {
    color: var(--foreground);
  }

  .context-window-input-row {
    display: grid;
    grid-template-columns: minmax(72px, 1fr) 44px 24px 24px;
    align-items: center;
    gap: 4px;
  }

  .context-window-input-row input,
  .context-window-input-row select {
    min-width: 0;
    height: 28px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--surface-1);
    color: var(--foreground);
    font: inherit;
  }

  .context-window-input-row input {
    padding: 0 7px;
    font-variant-numeric: tabular-nums;
  }

  .context-window-input-row select {
    padding: 0 4px;
  }

  .context-window-icon-button {
    width: 24px;
    height: 24px;
    padding: 0;
    border-radius: var(--radius-sm);
  }

  .context-window-icon-button:hover:not(:disabled) {
    background: var(--surface-hover);
  }

  .context-window-icon-button:disabled {
    cursor: default;
    opacity: 0.55;
  }

  .context-window-error {
    display: block;
    margin-top: 5px;
    color: var(--error);
  }

  :global(.spinning) {
    animation: context-window-spin 0.9s linear infinite;
  }

  @keyframes context-window-spin {
    to { transform: rotate(360deg); }
  }

  .tone-notice .ring-fill { stroke: var(--warning, #d6a700); }
  .tone-warning .ring-fill { stroke: var(--warning, #e08600); }
  .tone-danger .ring-fill { stroke: var(--error, #d64545); }
</style>
