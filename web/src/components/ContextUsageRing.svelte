<script lang="ts">
  import { tick } from 'svelte';
  import { constrainPanelWidth } from '../lib/panel-width';
  import SelectField from './SelectField.svelte';
  import type { ContextUsageBreakdown } from '../lib/context-usage-breakdown';
  import { i18n } from '../stores/i18n.svelte';
  import {
    buildRingDetailItems,
    buildRingTooltip,
    formatRingPercent,
    formatRingTokens,
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
    configuredTokenLimit?: number | null;
    warningLevel?: ContextRingTone | null;
    lastCompactionReason?: string | null;
    originalTokenEstimate?: number | null;
    compactedTokenEstimate?: number | null;
    measurement?: 'estimated' | 'authoritative' | null;
    contextBreakdown?: ContextUsageBreakdown | null;
    responseReserveTokens?: number | null;
    recoveryBufferTokens?: number | null;
    proactiveThresholdTokens?: number | null;
    onSaveContextWindow?: (contextWindowTokens: number) => Promise<void> | void;
  }

  let {
    model = '',
    usageRatio = null,
    tokenUsed = null,
    remainingTokens = null,
    tokenLimit = null,
    configuredTokenLimit = null,
    warningLevel = null,
    lastCompactionReason = null,
    originalTokenEstimate = null,
    compactedTokenEstimate = null,
    measurement = null,
    contextBreakdown = null,
    responseReserveTokens = null,
    recoveryBufferTokens = null,
    proactiveThresholdTokens = null,
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
  const compactionItems = $derived(buildRingDetailItems(input, (key, params) => i18n.t(key, params))
    .filter(item => item.key === 'compaction'));
  const breakdownRows = $derived.by(() => {
    const categories = [
      ['messages', 'input.contextRing.category.messages', contextBreakdown?.conversationTokens ?? 0],
      ['images', 'input.contextRing.category.images', contextBreakdown?.imageTokens ?? 0],
      ['system', 'input.contextRing.category.system', contextBreakdown?.systemInstructionTokens ?? 0],
      ['project', 'input.contextRing.category.project', contextBreakdown?.projectContextTokens ?? 0],
      ['skills', 'input.contextRing.category.skills', contextBreakdown?.skillTokens ?? 0],
      ['references', 'input.contextRing.category.references', contextBreakdown?.contextReferenceTokens ?? 0],
      ['builtin-tools', 'input.contextRing.category.builtinTools', contextBreakdown?.builtinToolTokens ?? 0],
      ['mcp-tools', 'input.contextRing.category.mcpTools', contextBreakdown?.mcpToolTokens ?? 0],
      ['skill-tools', 'input.contextRing.category.skillTools', contextBreakdown?.skillToolTokens ?? 0],
      ['other-tools', 'input.contextRing.category.otherTools', contextBreakdown?.otherToolTokens ?? 0],
    ] as const;
    const total = categories.reduce((sum, [, , tokens]) => sum + Math.max(0, tokens), 0);
    return categories
      .filter(([, , tokens]) => tokens > 0)
      .map(([key, labelKey, tokens]) => ({
        key,
        label: i18n.t(labelKey),
        tokens,
        percent: formatRingPercent(tokens / total),
        width: tokens / total * 100,
      }));
  });
  const hasBreakdown = $derived(breakdownRows.length > 0);
  const popoverId = `context-usage-ring-${Math.random().toString(36).slice(2)}`;

  let rootEl = $state<HTMLSpanElement | null>(null);
  let ringButton = $state<HTMLButtonElement | null>(null);
  let windowInput = $state<HTMLInputElement | null>(null);
  let editButton = $state<HTMLButtonElement | null>(null);
  let pinned = $state(false);
  let editing = $state(false);
  let draftWindowValue = $state('');
  let draftWindowUnit = $state<'K' | 'M'>('K');
  let saving = $state(false);
  let saveError = $state('');

  function beginEditing() {
    const tokens = configuredTokenLimit ?? tokenLimit ?? 256_000;
    if (tokens >= 1_000_000 && tokens % 1_000_000 === 0) {
      draftWindowValue = String(tokens / 1_000_000);
      draftWindowUnit = 'M';
    } else {
      draftWindowValue = String(tokens / 1_000);
      draftWindowUnit = 'K';
    }
    saveError = '';
    editing = true;
    void tick().then(() => { windowInput?.focus(); windowInput?.select(); });
  }

  function cancelEditing() {
    if (saving) return;
    editing = false;
    saveError = '';
    void tick().then(() => editButton?.focus());
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
      void tick().then(() => editButton?.focus());
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
      event.preventDefault();
      event.stopPropagation();
      if (saving) return;
      if (editing) {
        cancelEditing();
      } else {
        pinned = false;
        ringButton?.focus();
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
    bind:this={ringButton}
    onclick={toggleDetails}
    onkeydown={handleKeydown}
    title={i18n.t('input.contextRing.label')}
    aria-label={tooltip}
    aria-expanded={pinned}
    aria-haspopup="dialog"
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
    use:constrainPanelWidth={{ align: 'end', active: pinned }}
    class:visible={pinned}
    data-magi-surface="popover"
    role="dialog"
    aria-label={i18n.t('input.contextRing.label')}
    tabindex="-1"
    onkeydown={handleKeydown}
  >
    <div class="context-popover-title">
      <span>{i18n.t('input.contextRing.label')}</span>
      {#if view.hasData}<strong class="context-usage-percent tone-{view.tone}">{view.labelText}</strong>{/if}
    </div>
    {#if view.hasData}
      <div class="context-usage-summary">
        <strong>{formatRingTokens(tokenUsed)}</strong>
        <span>/ {formatRingTokens(tokenLimit)}</span>
      </div>
      <div class="context-usage-track tone-{view.tone}" aria-hidden="true">
        <span style:width={`${view.clampedRatio * 100}%`}></span>
      </div>
      <div class="context-usage-remaining">{i18n.t('input.contextRing.remaining', { value: formatRingTokens(remainingTokens) })}</div>
      {#if measurement === 'estimated'}<p class="context-note">{i18n.t('input.contextRing.estimated')}</p>{/if}
      {#if hasBreakdown}
        <div class="context-breakdown-section">
          <div class="context-breakdown-heading">{i18n.t('input.contextRing.breakdown')}</div>
          <div class="context-breakdown-bar" role="img" aria-label={i18n.t('input.contextRing.breakdown')}>
            {#each breakdownRows as row (row.key)}
              <span class="context-breakdown-segment category-{row.key}" style={`width:${row.width}%`}></span>
            {/each}
          </div>
          <div class="context-breakdown-list">
            {#each breakdownRows as row (row.key)}
              <div class="context-breakdown-row">
                <span class="context-breakdown-swatch category-{row.key}" aria-hidden="true"></span>
                <span class="context-breakdown-name">{row.label}</span>
                <span class="context-breakdown-tokens">{formatRingTokens(row.tokens)}</span>
                <span class="context-breakdown-percent">{row.percent}%</span>
              </div>
            {/each}
          </div>
        </div>
      {/if}
      {#each compactionItems as item (item.key)}<p class="context-note">{item.text}</p>{/each}
      {#if responseReserveTokens != null || recoveryBufferTokens != null || proactiveThresholdTokens != null}
        <div class="context-budget-notes">
          <div class="context-breakdown-heading">{i18n.t('input.contextRing.budget')}</div>
          <dl class="context-budget-list">
            {#if responseReserveTokens != null}<div><dt>{i18n.t('input.contextRing.outputReserve')}</dt><dd>{formatRingTokens(responseReserveTokens)}</dd></div>{/if}
            {#if recoveryBufferTokens != null}<div><dt>{i18n.t('input.contextRing.recoveryBuffer')}</dt><dd>{formatRingTokens(recoveryBufferTokens)}</dd></div>{/if}
            {#if proactiveThresholdTokens != null}<div><dt>{i18n.t('input.contextRing.compactionThreshold')}</dt><dd>{formatRingTokens(proactiveThresholdTokens)}</dd></div>{/if}
          </dl>
        </div>
      {/if}
    {:else}
      <div class="context-popover-empty">{i18n.t('input.contextRing.empty')}</div>
    {/if}
    {#if onSaveContextWindow && model}
      <div class="context-window-editor">
        {#if editing}
          <label class="context-window-label" for={`${popoverId}-window`}>{i18n.t('input.contextRing.configuredWindow')}</label>
          <div class="context-window-input-row">
            <input
              id={`${popoverId}-window`}
              bind:this={windowInput}
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
            <SelectField bind:value={draftWindowUnit} disabled={saving}
              label={i18n.t('input.contextRing.windowUnit')}
              options={[{ value: 'K', label: 'K' }, { value: 'M', label: 'M' }]} />
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
          {#if saveError}<span class="context-window-error" role="alert">{saveError}</span>{/if}
        {:else}
          <div class="context-window-setting">
            <span>{i18n.t('input.contextRing.configuredWindow')}</span>
            <strong>{formatRingTokens(configuredTokenLimit ?? tokenLimit)}</strong>
            <button type="button" class="context-window-icon-button" bind:this={editButton}
              title={i18n.t('input.contextRing.edit')} aria-label={i18n.t('input.contextRing.edit')} onclick={beginEditing}>
              <Icon name="edit" size={13} />
            </button>
          </div>
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

  .ia-context-popover {
    position: absolute;
    right: 0;
    bottom: calc(100% + 8px);
    z-index: var(--z-popover);
    display: flex;
    flex-direction: column;
    gap: 6px;
    width: min(300px, var(--panel-available-width, calc(100vw - 24px)));
    box-sizing: border-box;
    max-height: min(70vh, 560px);
    overflow: auto;
    padding: var(--space-4);
    border: 0;
    border-radius: calc(var(--radius-sm) + var(--space-4));
    background: var(--background);
    color: var(--foreground);
    box-shadow: inset 0 0 0 var(--magi-hairline) var(--magi-border-faint), var(--shadow-lg);
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

  .context-popover-title { display: flex; align-items: center; justify-content: space-between; gap: 8px; color: var(--foreground-muted); }
  .context-usage-percent { color: var(--primary); font-weight: var(--font-semibold); font-variant-numeric: tabular-nums; }
  .context-usage-summary { display: flex; align-items: baseline; flex-wrap: wrap; gap: 5px; color: var(--foreground-muted); font-variant-numeric: tabular-nums; }
  .context-usage-summary strong { color: var(--foreground); font-size: 18px; line-height: 1.25; font-weight: var(--font-semibold); }
  .context-usage-track { height: 5px; overflow: hidden; border-radius: var(--radius-full); background: var(--surface-3); }
  .context-usage-track > span { display: block; height: 100%; border-radius: inherit; background: var(--primary); transition: width var(--transition-fast); }
  .context-usage-remaining { color: var(--foreground-muted); font-variant-numeric: tabular-nums; }
  .context-note { margin: 0; color: var(--foreground-muted); font-size: var(--text-xs); overflow-wrap: anywhere; }

  .context-breakdown-bar {
    display: flex;
    width: 100%;
    height: 8px;
    overflow: hidden;
    border-radius: 999px;
    background: var(--surface-3);
  }

  .context-breakdown-segment {
    display: block;
    flex: 0 0 auto;
    height: 100%;
  }

  .context-breakdown-section,
  .context-budget-notes {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .context-breakdown-heading {
    color: var(--foreground-muted);
    font-weight: var(--font-semibold);
  }

  .context-breakdown-list {
    display: flex;
    flex-direction: column;
    gap: 3px;
  }

  .context-breakdown-row { display: grid; grid-template-columns: 7px minmax(0, 1fr) auto auto; align-items: baseline; gap: 7px; min-height: 20px; }
  .context-breakdown-swatch { width: 7px; height: 7px; border-radius: 2px; align-self: center; }
  .context-breakdown-name { overflow-wrap: anywhere; }
  .context-breakdown-tokens, .context-breakdown-percent { color: var(--foreground-muted); text-align: right; font-variant-numeric: tabular-nums; }
  .context-breakdown-percent { min-width: 3ch; }
  .context-breakdown-section, .context-budget-notes { margin-top: 2px; padding-top: 8px; border-top: var(--magi-hairline) solid var(--border); }
  .context-budget-list { display: flex; flex-direction: column; gap: 5px; margin: 0; }
  .context-budget-list > div { display: flex; justify-content: space-between; align-items: baseline; gap: 12px; }
  .context-budget-list dt { color: var(--foreground-muted); overflow-wrap: anywhere; }
  .context-budget-list dd { margin: 0; flex-shrink: 0; font-variant-numeric: tabular-nums; }

  .category-messages { background: #3b82f6; }
  .category-images { background: #22d3ee; }
  .category-system { background: #f97316; }
  .category-project { background: #14b8a6; }
  .category-skills { background: #eab308; }
  .category-references { background: #8b5cf6; }
  .category-builtin-tools { background: #ec4899; }
  .category-mcp-tools { background: #22c55e; }
  .category-skill-tools { background: #c084fc; }
  .category-other-tools { background: #78716c; }

  .context-popover-empty {
    color: var(--foreground-muted);
  }

  .context-window-editor {
    padding-top: 8px;
    border-top: var(--magi-hairline) solid var(--border);
  }

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

  .context-window-label { display: block; margin-bottom: 6px; color: var(--foreground-muted); }
  .context-window-setting { display: flex; align-items: center; gap: 8px; color: var(--foreground-muted); }
  .context-window-setting strong { margin-left: auto; color: var(--foreground); font-weight: var(--font-medium); font-variant-numeric: tabular-nums; }

  .context-window-input-row {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 52px 28px 28px;
    align-items: center;
    gap: 4px;
  }

  .context-window-input-row input {
    min-width: 0;
    height: 28px;
    border: var(--magi-hairline) solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--surface-1);
    color: var(--foreground);
    font: inherit;
  }

  .context-window-input-row :global(.select-field select) { height: 28px; min-height: 28px; padding: 0 7px; font-size: inherit; }

  .context-window-input-row input {
    padding: 0 7px;
    font-variant-numeric: tabular-nums;
  }

  .context-window-icon-button {
    width: 28px;
    height: 28px;
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

  .context-usage-percent:is(.tone-notice, .tone-warning) { color: var(--warning); }
  .context-usage-percent.tone-danger { color: var(--error); }
  .context-usage-track:is(.tone-notice, .tone-warning) > span { background: var(--warning); }
  .context-usage-track.tone-danger > span { background: var(--error); }

  .tone-notice .ring-fill { stroke: var(--warning, #d6a700); }
  .tone-warning .ring-fill { stroke: var(--warning, #e08600); }
  .tone-danger .ring-fill { stroke: var(--error, #d64545); }
</style>
