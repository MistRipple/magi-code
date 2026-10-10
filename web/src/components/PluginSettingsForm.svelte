<script lang="ts">
  import { onMount } from 'svelte';
  import { i18n } from '../stores/i18n.svelte';
  import {
    readPluginSettings,
    writePluginSettings,
  } from '../web/plugin-api';
  import type { PluginManifest } from '../shared/app-server-protocol.generated';

  interface Props {
    pluginId: string;
    manifest: PluginManifest;
    scope: string;
  }

  interface SchemaProperty {
    key: string;
    title: string;
    description: string;
    type: string;
    defaultValue?: unknown;
    enumValues: string[];
  }

  let { pluginId, manifest, scope }: Props = $props();
  let value = $state<Record<string, unknown>>({});
  let version = $state(0);
  let loading = $state(true);
  let saving = $state(false);
  let error = $state('');
  let status = $state('');

  const properties = $derived.by<SchemaProperty[]>(() => {
    const schema = manifest.settingsSchema;
    if (!schema || typeof schema !== 'object' || Array.isArray(schema)) return [];
    const raw = (schema as Record<string, unknown>).properties;
    if (!raw || typeof raw !== 'object' || Array.isArray(raw)) return [];
    return Object.entries(raw as Record<string, unknown>).flatMap(([key, item]) => {
      if (!item || typeof item !== 'object' || Array.isArray(item)) return [];
      const property = item as Record<string, unknown>;
      const type = typeof property.type === 'string' ? property.type : 'string';
      const enumValues = Array.isArray(property.enum)
        ? property.enum.filter((entry): entry is string => typeof entry === 'string')
        : [];
      return [{
        key,
        title: typeof property.title === 'string' && property.title.trim() ? property.title : key,
        description: typeof property.description === 'string' ? property.description : '',
        type,
        defaultValue: property.default,
        enumValues,
      }];
    });
  });

  function initialValue(): Record<string, unknown> {
    const schema = manifest.settingsSchema;
    const defaults: Record<string, unknown> = {};
    for (const property of properties) {
      if (property.defaultValue !== undefined) defaults[property.key] = property.defaultValue;
    }
    if (schema && typeof schema === 'object' && !Array.isArray(schema)) {
      const required = (schema as Record<string, unknown>).required;
      if (Array.isArray(required)) {
        for (const key of required) {
          if (typeof key === 'string' && defaults[key] === undefined) defaults[key] = defaultForType(properties.find((item) => item.key === key)?.type);
        }
      }
    }
    return defaults;
  }

  function defaultForType(type: string | undefined): unknown {
    if (type === 'boolean') return false;
    if (type === 'number' || type === 'integer') return 0;
    if (type === 'array') return [];
    if (type === 'object') return {};
    return '';
  }

  function displayValue(property: SchemaProperty): string | number {
    const current = value[property.key];
    if (property.type === 'number' || property.type === 'integer') {
      return typeof current === 'number' ? current : 0;
    }
    if (property.type === 'array' || property.type === 'object') {
      return JSON.stringify(current ?? defaultForType(property.type), null, 2);
    }
    return typeof current === 'string' ? current : '';
  }

  function updateValue(property: SchemaProperty, raw: string): void {
    let next: unknown = raw;
    if (property.type === 'number' || property.type === 'integer') {
      const parsed = property.type === 'integer' ? Number.parseInt(raw, 10) : Number.parseFloat(raw);
      next = Number.isFinite(parsed) ? parsed : 0;
    } else if (property.type === 'array' || property.type === 'object') {
      try { next = JSON.parse(raw); } catch { next = raw; }
    }
    value = { ...value, [property.key]: next };
  }

  async function refresh(): Promise<void> {
    loading = true;
    error = '';
    try {
      const resource = await readPluginSettings(pluginId, scope) as { version?: unknown; value?: unknown };
      version = typeof resource.version === 'number' ? resource.version : 0;
      value = resource.value && typeof resource.value === 'object' && !Array.isArray(resource.value)
        ? { ...initialValue(), ...(resource.value as Record<string, unknown>) }
        : initialValue();
    } catch (cause) {
      error = cause instanceof Error ? cause.message : String(cause);
    } finally {
      loading = false;
    }
  }

  async function save(): Promise<void> {
    if (saving) return;
    saving = true;
    error = '';
    status = '';
    try {
      const resource = await writePluginSettings(pluginId, scope, version, value) as { version?: unknown; value?: unknown };
      version = typeof resource.version === 'number' ? resource.version : version + 1;
      status = i18n.t('settings.plugins.settings.saved');
    } catch (cause) {
      error = cause instanceof Error ? cause.message : String(cause);
    } finally {
      saving = false;
    }
  }

  onMount(() => { void refresh(); });
</script>

<section class="plugin-settings-form">
  {#if loading}
    <p class="settings-hint">{i18n.t('settings.plugins.settings.loading')}</p>
  {:else if error}
    <p class="settings-hint plugin-settings-error">{error}</p>
    <button type="button" class="btn btn--secondary btn--sm" onclick={() => void refresh()}>{i18n.t('settings.plugins.settings.retry')}</button>
  {:else}
    {#each properties as property (property.key)}
      <div class="form-field">
        <label class="form-label" for={`plugin-setting-${pluginId}-${property.key}`}>{property.title}</label>
        {#if property.description}<span class="settings-hint">{property.description}</span>{/if}
        {#if property.type === 'boolean'}
          <label class="plugin-settings-checkbox">
            <input
              id={`plugin-setting-${pluginId}-${property.key}`}
              type="checkbox"
              checked={value[property.key] === true}
              onchange={(event) => value = { ...value, [property.key]: event.currentTarget.checked }}
            />
            <span>{i18n.t('settings.plugins.settings.enabled')}</span>
          </label>
        {:else if property.enumValues.length > 0}
          <select
            class="form-input"
            id={`plugin-setting-${pluginId}-${property.key}`}
            value={String(value[property.key] ?? property.enumValues[0] ?? '')}
            onchange={(event) => updateValue(property, event.currentTarget.value)}
          >
            {#each property.enumValues as option (option)}<option value={option}>{option}</option>{/each}
          </select>
        {:else if property.type === 'array' || property.type === 'object'}
          <textarea
            class="form-input plugin-settings-json"
            id={`plugin-setting-${pluginId}-${property.key}`}
            value={String(displayValue(property))}
            oninput={(event) => updateValue(property, event.currentTarget.value)}
          ></textarea>
        {:else}
          <input
            class="form-input"
            id={`plugin-setting-${pluginId}-${property.key}`}
            type={property.type === 'number' || property.type === 'integer' ? 'number' : 'text'}
            value={String(displayValue(property))}
            oninput={(event) => updateValue(property, event.currentTarget.value)}
          />
        {/if}
      </div>
    {/each}
    {#if properties.length === 0}
      <p class="settings-hint">{i18n.t('settings.plugins.settings.empty')}</p>
    {:else}
      <div class="plugin-settings-actions">
        <button type="button" class="btn btn--primary btn--sm" onclick={() => void save()} disabled={saving}>{i18n.t('settings.plugins.settings.save')}</button>
        {#if status}<span class="settings-hint">{status}</span>{/if}
      </div>
    {/if}
  {/if}
</section>

<style>
  .plugin-settings-form { display: grid; gap: 16px; max-width: 720px; }
  .plugin-settings-form .form-field { display: grid; gap: 6px; }
  .plugin-settings-checkbox { display: inline-flex; gap: 8px; align-items: center; color: var(--text-secondary); }
  .plugin-settings-json { min-height: 120px; resize: vertical; font-family: var(--font-mono, monospace); }
  .plugin-settings-actions { display: flex; gap: 10px; align-items: center; }
  .plugin-settings-error { color: var(--text-danger, #d33); }
</style>
