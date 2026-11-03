<script lang="ts">
  import type { Snippet } from 'svelte';
  import SelectField from './SelectField.svelte';
  import Icon from './Icon.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import type { MainModelProtocol } from '../shared/main-model-sources';

  let {
    config = $bindable(),
    statusKey,
    keyVisible = $bindable(),
    getBaseUrlPlaceholder,
    showProtocol = true,
    children,
  }: {
    config: { baseUrl: string; apiKey: string; apiProtocol: MainModelProtocol };
    statusKey: string;
    keyVisible: Record<string, boolean>;
    getBaseUrlPlaceholder: () => string;
    showProtocol?: boolean;
    children?: Snippet;
  } = $props();

  const fieldId = (field: string) => `model-connection-${statusKey}-${field}`;
  const protocolEndpoint = $derived(config.apiProtocol === 'openai_responses'
    ? '/v1/responses'
    : config.apiProtocol === 'anthropic_messages' ? '/v1/messages' : '/v1/chat/completions');
</script>

<section class="settings-card" aria-label={i18n.t('settings.model.card.connection')}>
  <div class="settings-card-head">
    <h4 class="settings-section-title">{i18n.t('settings.model.card.connection')}</h4>
  </div>
  <div class="settings-card-body model-config-grid">
    {#if children}
      <div class="connection-identity">{@render children()}</div>
    {/if}
    <div class="llm-config-field connection-address">
      <label class="form-label" for={fieldId('base-url')}>{i18n.t('settings.model.field.baseUrl')}</label>
      <input id={fieldId('base-url')} type="text" class="form-input" bind:value={config.baseUrl} placeholder={getBaseUrlPlaceholder()} />
    </div>
    {#if showProtocol}
      <div class="llm-config-field">
        <label class="form-label" for={fieldId('protocol')}>{i18n.t('settings.model.field.apiProtocol')}</label>
        <SelectField id={fieldId('protocol')} bind:value={config.apiProtocol}
          label={i18n.t('settings.model.field.apiProtocol')}
          options={[
            { value: 'openai_responses', label: i18n.t('settings.model.protocol.openaiResponses') },
            { value: 'openai_chat', label: i18n.t('settings.model.protocol.openaiChat') },
            { value: 'anthropic_messages', label: i18n.t('settings.model.protocol.anthropicMessages') },
          ]} />
        <div class="protocol-endpoint" aria-live="polite">
          <span>{i18n.t('settings.model.protocol.endpoint')}</span><code>POST {protocolEndpoint}</code>
        </div>
      </div>
    {/if}
    <div class="llm-config-field">
      <label class="form-label" for={fieldId('api-key')}>{i18n.t('settings.model.field.apiKey')}</label>
      <div class="api-key-wrapper">
        <input id={fieldId('api-key')} type={keyVisible[statusKey] ? 'text' : 'password'} class="form-input"
          bind:value={config.apiKey} placeholder={config.apiProtocol === 'anthropic_messages' ? 'sk-ant-...' : 'sk-...'} autocomplete="off" />
        <button type="button" class="api-key-toggle" onclick={() => keyVisible[statusKey] = !keyVisible[statusKey]}
          aria-label={keyVisible[statusKey] ? i18n.t('input.hideKey') : i18n.t('input.showKey')}
          title={keyVisible[statusKey] ? i18n.t('input.hideKey') : i18n.t('input.showKey')}>
          <Icon name={keyVisible[statusKey] ? 'eye-slash' : 'eye'} size={14} />
        </button>
      </div>
    </div>
  </div>
</section>

<style>
  .connection-address, .connection-identity { grid-column: 1 / -1; min-width: 0; }
  .protocol-endpoint { display: inline-flex; align-items: baseline; flex-wrap: wrap; gap: var(--space-2); min-width: 0; color: var(--foreground-muted); font-size: var(--text-xs); }
  .protocol-endpoint code { color: var(--foreground); font-family: var(--font-mono); font-size: var(--text-xs); overflow-wrap: anywhere; }
  .api-key-wrapper { position: relative; }
  .api-key-wrapper .form-input { padding-right: 32px; }
  .api-key-toggle { position: absolute; right: 4px; top: 50%; transform: translateY(-50%); display: flex; align-items: center; justify-content: center; width: 24px; height: 24px; padding: 0; border: none; border-radius: var(--radius-sm); background: transparent; color: var(--foreground-muted); cursor: pointer; transition: all var(--transition-fast); opacity: .6; }
  .api-key-toggle:hover { background: var(--secondary); color: var(--foreground); opacity: 1; }
</style>
