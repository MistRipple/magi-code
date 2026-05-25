<script lang="ts">
  import type { ModelEngine } from '../shared/types/registry-types';
  import Icon from './Icon.svelte';
  import { i18n } from '../stores/i18n.svelte';

  let {
    value = '',
    engines = [],
    inheritModelLabel = '',
    getDisplayName,
    modelStatuses = {},
    error = false,
    disabled = false,
    onchange,
  } = $props<{
    value?: string;
    engines?: ModelEngine[];
    inheritModelLabel?: string;
    getDisplayName: (engineId: string) => string;
    modelStatuses?: Record<string, { status?: string }>;
    error?: boolean;
    disabled?: boolean;
    onchange: (engineId: string) => void;
  }>();

  type OptionKind = 'inherit' | 'engine';
  type Option = { id: string; kind: OptionKind; engine?: ModelEngine };

  const options = $derived<Option[]>([
    { id: '', kind: 'inherit' },
    ...engines.map((e: ModelEngine) => ({ id: e.id, kind: 'engine' as const, engine: e })),
  ]);

  const currentIdx = $derived(options.findIndex((o: Option) => o.id === value));

  const triggerLabel = $derived.by(() => {
    if (!value) return i18n.t('settings.agents.inheritOrchestrator');
    return getDisplayName(value);
  });

  const triggerSubLabel = $derived.by(() => {
    if (value) return '';
    const m = inheritModelLabel?.trim();
    if (!m) return i18n.t('settings.agents.inheritUnconfigured');
    return i18n.t('settings.agents.inheritWillUse', { model: m });
  });

  function shortBaseUrl(raw: unknown): string {
    if (typeof raw !== 'string') return '';
    const trimmed = raw.trim();
    if (!trimmed) return '';
    return trimmed.replace(/^https?:\/\//i, '').replace(/\/+$/, '');
  }

  type StatusToken = 'connected' | 'configured' | 'not_configured' | 'error' | 'checking';
  function statusOf(engineId: string): StatusToken {
    const s = modelStatuses?.[engineId]?.status;
    if (s === 'connected' || s === 'configured' || s === 'error' || s === 'checking') return s;
    return 'not_configured';
  }

  let open = $state(false);
  let activeIdx = $state(-1);
  let dropUp = $state(false);
  let triggerEl: HTMLButtonElement | undefined = $state();
  let popupEl: HTMLDivElement | undefined = $state();

  function toggle() {
    if (disabled) return;
    open = !open;
    if (open) {
      activeIdx = currentIdx >= 0 ? currentIdx : 0;
      queueMicrotask(maybeFlip);
    }
  }

  function close() {
    if (!open) return;
    open = false;
  }

  function pick(idx: number) {
    const opt = options[idx];
    if (!opt) return;
    if (opt.id !== value) onchange(opt.id);
    open = false;
    triggerEl?.focus();
  }

  function onTriggerKey(ev: KeyboardEvent) {
    if (disabled) return;
    if (!open) {
      if (ev.key === 'Enter' || ev.key === ' ' || ev.key === 'ArrowDown' || ev.key === 'ArrowUp') {
        ev.preventDefault();
        toggle();
      }
      return;
    }
    onListKey(ev);
  }

  function onListKey(ev: KeyboardEvent) {
    if (!open) return;
    const last = options.length - 1;
    if (ev.key === 'Escape') { ev.preventDefault(); close(); triggerEl?.focus(); return; }
    if (ev.key === 'ArrowDown') { ev.preventDefault(); activeIdx = activeIdx >= last ? 0 : activeIdx + 1; return; }
    if (ev.key === 'ArrowUp') { ev.preventDefault(); activeIdx = activeIdx <= 0 ? last : activeIdx - 1; return; }
    if (ev.key === 'Home') { ev.preventDefault(); activeIdx = 0; return; }
    if (ev.key === 'End') { ev.preventDefault(); activeIdx = last; return; }
    if (ev.key === 'Enter') { ev.preventDefault(); pick(activeIdx); return; }
    if (ev.key === 'Tab') { close(); return; }
  }

  function onDocPointer(ev: MouseEvent) {
    if (!open) return;
    const t = ev.target as Node | null;
    if (!t) return;
    if (triggerEl?.contains(t) || popupEl?.contains(t)) return;
    close();
  }

  function maybeFlip() {
    if (!triggerEl) return;
    const r = triggerEl.getBoundingClientRect();
    const below = window.innerHeight - r.bottom;
    const above = r.top;
    // 期望 popup 高度上限约 260；下方不够、上方更宽时上翻
    dropUp = below < 220 && above > below;
  }

  $effect(() => {
    if (!open) return;
    document.addEventListener('mousedown', onDocPointer, true);
    window.addEventListener('resize', maybeFlip);
    window.addEventListener('scroll', maybeFlip, true);
    return () => {
      document.removeEventListener('mousedown', onDocPointer, true);
      window.removeEventListener('resize', maybeFlip);
      window.removeEventListener('scroll', maybeFlip, true);
    };
  });
</script>

<div class="engine-picker" class:err={error} class:disabled class:open>
  <button
    type="button"
    class="engine-picker-trigger"
    bind:this={triggerEl}
    onclick={toggle}
    onkeydown={onTriggerKey}
    aria-haspopup="listbox"
    aria-expanded={open}
    {disabled}
  >
    <Icon name="model" size={12} class="trigger-icon-pre" />
    <span class="trigger-label-stack">
      <span class="trigger-label">{triggerLabel}</span>
      {#if triggerSubLabel}
        <span class="trigger-sublabel">{triggerSubLabel}</span>
      {/if}
    </span>
    <Icon name="chevron-down" size={10} class="trigger-icon-suf" />
  </button>

  {#if open}
    <div
      bind:this={popupEl}
      class="engine-picker-popup"
      class:drop-up={dropUp}
      role="listbox"
      tabindex="-1"
      onkeydown={onListKey}
    >
      <div class="group-label">{i18n.t('settings.agents.groupDefault')}</div>
      {#each options as opt, i (opt.id || '__inherit__')}
        {#if i === 1}
          <div class="group-sep" role="separator"></div>
          <div class="group-label">{i18n.t('settings.agents.groupCustom')}</div>
        {/if}
        {@const isSelected = opt.id === value}
        {@const isActive = i === activeIdx}
        {@const engineStatus = opt.kind === 'engine' ? statusOf(opt.id) : null}
        <button
          type="button"
          class="engine-option"
          class:selected={isSelected}
          class:active={isActive}
          role="option"
          aria-selected={isSelected}
          tabindex="-1"
          onmouseenter={() => (activeIdx = i)}
          onclick={() => pick(i)}
        >
          {#if opt.kind === 'inherit'}
            <span class="opt-dot opt-dot--inherit" aria-hidden="true"></span>
            <span class="opt-stack">
              <span class="opt-name">{i18n.t('settings.agents.inheritOrchestrator')}</span>
              {#if inheritModelLabel?.trim()}
                <span class="opt-sub">{i18n.t('settings.agents.inheritWillUse', { model: inheritModelLabel })}</span>
              {:else}
                <span class="opt-sub">{i18n.t('settings.agents.inheritUnconfigured')}</span>
              {/if}
            </span>
          {:else}
            <span class="opt-dot opt-dot--status-{engineStatus}" aria-hidden="true"></span>
            <span class="opt-stack">
              <span class="opt-name">{getDisplayName(opt.id)}</span>
              {#if opt.engine?.llm?.baseUrl}
                <span class="opt-sub">{shortBaseUrl(opt.engine.llm.baseUrl)}</span>
              {/if}
            </span>
          {/if}
          {#if isSelected}
            <Icon name="check" size={10} class="opt-check" />
          {/if}
        </button>
      {/each}
    </div>
  {/if}
</div>

<style>
  .engine-picker {
    position: relative;
    max-width: 320px;
    width: 100%;
    align-self: flex-start;
  }

  /* ---------- Trigger ---------- */
  .engine-picker-trigger {
    width: 100%;
    min-height: 34px;
    background: var(--ind-bg-control);
    border: 1px solid var(--ind-border-control);
    border-radius: 8px;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 10px;
    box-sizing: border-box;
    cursor: pointer;
    text-align: left;
    color: var(--ind-foreground);
    font-family: inherit;
    transition: background 0.18s ease, border-color 0.18s ease, box-shadow 0.18s ease;
  }
  .engine-picker-trigger:hover {
    background: var(--ind-bg-control-hover);
    border-color: var(--ind-border-control-strong);
  }
  .engine-picker.open .engine-picker-trigger {
    border-color: var(--ind-tab-accent);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--ind-tab-accent) 18%, transparent);
  }
  .engine-picker.err .engine-picker-trigger {
    border-color: color-mix(in srgb, var(--error, #ff3b30) 36%, var(--ind-border-control));
    background: color-mix(in srgb, var(--error, #ff3b30) 8%, var(--ind-bg-control));
  }
  .engine-picker.disabled .engine-picker-trigger {
    cursor: not-allowed;
    opacity: 0.55;
  }
  .engine-picker-trigger:focus-visible {
    outline: none;
    border-color: var(--ind-tab-accent);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--ind-tab-accent) 30%, transparent);
  }
  :global(.engine-picker-trigger > .trigger-icon-pre),
  :global(.engine-picker-trigger > .trigger-icon-suf) {
    opacity: 0.56;
    flex-shrink: 0;
  }
  :global(.engine-picker-trigger > .trigger-icon-suf) {
    margin-left: auto;
    transition: transform 0.18s ease;
  }
  .engine-picker.open :global(.engine-picker-trigger > .trigger-icon-suf) {
    transform: rotate(180deg);
  }

  .trigger-label-stack {
    display: flex;
    flex-direction: column;
    min-width: 0;
    gap: 1px;
    flex: 1;
  }
  .trigger-label {
    font-size: 12px;
    font-weight: 500;
    color: var(--ind-foreground);
    line-height: 1.25;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .trigger-sublabel {
    font-size: 10.5px;
    color: var(--ind-foreground-muted);
    line-height: 1.3;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  /* ---------- Popup ---------- */
  .engine-picker-popup {
    position: absolute;
    top: calc(100% + 6px);
    left: 0;
    right: 0;
    z-index: 50;
    background: var(--ind-bg-elevated, var(--background));
    border: 1px solid var(--ind-border-separator);
    border-radius: 10px;
    padding: 4px;
    max-height: 260px;
    overflow-y: auto;
    box-shadow:
      0 8px 24px -8px rgba(0, 0, 0, 0.18),
      0 2px 6px -2px rgba(0, 0, 0, 0.10);
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .engine-picker-popup.drop-up {
    top: auto;
    bottom: calc(100% + 6px);
  }

  .group-label {
    font-size: 10.5px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--ind-foreground-muted);
    padding: 6px 10px 4px;
  }
  .group-sep {
    height: 1px;
    background: var(--ind-border-separator);
    margin: 3px 6px;
  }

  /* ---------- Options ---------- */
  .engine-option {
    display: flex;
    align-items: center;
    gap: 9px;
    padding: 7px 10px;
    border: none;
    border-radius: 6px;
    background: transparent;
    color: var(--ind-foreground);
    font-family: inherit;
    cursor: pointer;
    text-align: left;
    transition: background 0.12s ease;
  }
  .engine-option.active {
    background: color-mix(in srgb, var(--ind-tab-accent) 12%, transparent);
  }
  .engine-option.selected {
    color: var(--ind-foreground);
  }
  .engine-option.selected.active {
    background: color-mix(in srgb, var(--ind-tab-accent) 16%, transparent);
  }
  :global(.engine-option > .opt-check) {
    opacity: 0.72;
    margin-left: auto;
    flex-shrink: 0;
    color: var(--ind-tab-accent);
  }

  .opt-dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    flex-shrink: 0;
    margin-top: 2px;
  }
  .opt-dot--inherit {
    background: color-mix(in srgb, var(--ind-foreground-soft) 55%, transparent);
    border: 1px dashed color-mix(in srgb, var(--ind-foreground-soft) 70%, transparent);
    width: 9px;
    height: 9px;
    box-sizing: border-box;
    background: transparent;
  }
  .opt-dot--status-connected { background: var(--success, #34c759); }
  .opt-dot--status-configured { background: color-mix(in srgb, var(--success, #34c759) 65%, transparent); }
  .opt-dot--status-checking { background: var(--warning, #ff9500); }
  .opt-dot--status-error { background: var(--error, #ff3b30); }
  .opt-dot--status-not_configured {
    background: transparent;
    border: 1px solid color-mix(in srgb, var(--ind-foreground-soft) 60%, transparent);
    width: 9px;
    height: 9px;
    box-sizing: border-box;
  }

  .opt-stack {
    display: flex;
    flex-direction: column;
    min-width: 0;
    gap: 1px;
    flex: 1;
  }
  .opt-name {
    font-size: 12px;
    font-weight: 500;
    line-height: 1.3;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .opt-sub {
    font-size: 10.5px;
    color: var(--ind-foreground-muted);
    line-height: 1.3;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
</style>
