<script lang="ts" generics="T extends string">
  import { i18n } from '../stores/i18n.svelte';
  import type { HTMLAttributes } from 'svelte/elements';

  let {
    value = $bindable(), options, label, id, disabled = false, onchange, ...rest
  }: {
    value: T;
    options: ReadonlyArray<{ value: T; label: string; disabled?: boolean }>;
    label: string;
    id?: string;
    disabled?: boolean;
    onchange?: (value: T) => void;
  } & Omit<HTMLAttributes<HTMLDivElement>, 'onchange'> = $props();

  let field: HTMLDivElement;
  let query = $state('');
  const searchable = $derived(options.length > 10);
  const filtered = $derived(options.filter(option => !searchable || !query.trim()
    || option.label.toLocaleLowerCase().includes(query.trim().toLocaleLowerCase())));
  // Keep the current value represented while filtering candidates.
  // Searching must never select the first result or change the owner's draft.
  const visibleOptions = $derived(options.filter(option => option.value === value || filtered.includes(option)));
  function choose(next: T): void {
    if (onchange) {
      // A controlled owner may ask for confirmation or reject a save. Native controls
      // change before `change`; restore the owner's value until it accepts the choice.
      const select = field.querySelector('select');
      if (select) select.value = value;
      onchange(next);
    } else value = next;
  }
</script>

<div bind:this={field!} class="select-field" data-control-kind="select" {...rest}>
  {#if searchable}
    <input class="form-input select-field-search" type="search" bind:value={query}
      placeholder={i18n.t('selection.search')} aria-label={i18n.t('selection.searchFor', { label })} {disabled} />
    {#if filtered.length === 0}<span class="select-field-empty" role="status">{i18n.t('selection.empty')}</span>{/if}
  {/if}
  <select {id} class="form-input" aria-label={label} {disabled} {value}
    onchange={(event) => choose(event.currentTarget.value as T)}>
    {#each visibleOptions as option (option.value)}
      <option value={option.value} disabled={option.disabled}>{option.label}</option>
    {/each}
  </select>
</div>

<style>
  .select-field { min-width: 0; display: flex; flex-direction: column; gap: 6px; }
  .select-field-search { height: var(--btn-height-sm); }
  .select-field-empty { color: var(--foreground-muted); font-size: var(--text-xs); }
</style>
