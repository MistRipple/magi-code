<script lang="ts">
  import type { ConversationDisplayMode } from '../shared/settings-bootstrap';
  import { i18n } from '../stores/i18n.svelte';
  import Icon from './Icon.svelte';

  interface Props {
    mode: ConversationDisplayMode;
    saveStatus?: 'idle' | 'saving' | 'saved' | 'error';
    onChange: (mode: ConversationDisplayMode) => void;
  }

  let { mode, saveStatus = 'idle', onChange }: Props = $props();

  const options: Array<{
    value: ConversationDisplayMode;
    icon: 'list' | 'chat';
    titleKey: string;
    descriptionKey: string;
  }> = [
    {
      value: 'original',
      icon: 'list',
      titleKey: 'settings.conversationDisplay.original',
      descriptionKey: 'settings.conversationDisplay.originalDesc',
    },
    {
      value: 'summary',
      icon: 'chat',
      titleKey: 'settings.conversationDisplay.summary',
      descriptionKey: 'settings.conversationDisplay.summaryDesc',
    },
  ];
</script>

<section class="conversation-display-preference settings-section">
  <div class="settings-section-header">
    <div class="settings-section-title">{i18n.t('settings.conversationDisplay.title')}</div>
    {#if saveStatus !== 'idle'}
      <span class="conversation-display-status" class:error={saveStatus === 'error'}>
        {#if saveStatus === 'saving'}
          <Icon name="refresh" size={13} />
        {:else if saveStatus === 'saved'}
          <Icon name="check" size={13} />
        {:else}
          <Icon name="close" size={13} />
        {/if}
        {i18n.t(`settings.conversationDisplay.status.${saveStatus}`)}
      </span>
    {/if}
  </div>
  <div class="settings-section-desc">{i18n.t('settings.conversationDisplay.description')}</div>

  <div class="conversation-display-options" role="radiogroup" aria-label={i18n.t('settings.conversationDisplay.title')}>
    {#each options as option}
      <button
        type="button"
        class="conversation-display-option"
        class:selected={mode === option.value}
        role="radio"
        aria-checked={mode === option.value}
        onclick={() => onChange(option.value)}
      >
        <span class="conversation-display-option-icon"><Icon name={option.icon} size={16} /></span>
        <span class="conversation-display-option-copy">
          <strong>{i18n.t(option.titleKey)}</strong>
          <span>{i18n.t(option.descriptionKey)}</span>
        </span>
        <span class="conversation-display-option-check" aria-hidden="true">
          {#if mode === option.value}<Icon name="check" size={14} />{/if}
        </span>
      </button>
    {/each}
  </div>
</section>

<style>
  .conversation-display-preference {
    padding-bottom: var(--space-5);
    border-bottom: 1px solid var(--border-subtle, var(--border));
  }

  .conversation-display-status {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
  }

  .conversation-display-status.error {
    color: var(--danger);
  }

  .conversation-display-options {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: var(--space-3);
    margin-top: var(--space-4);
  }

  .conversation-display-option {
    display: flex;
    align-items: flex-start;
    min-width: 0;
    gap: var(--space-3);
    padding: var(--space-3);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--surface-1, transparent);
    color: var(--foreground);
    text-align: left;
    cursor: pointer;
    transition: border-color var(--transition-fast), background var(--transition-fast), box-shadow var(--transition-fast);
  }

  .conversation-display-option:hover {
    border-color: color-mix(in srgb, var(--primary) 45%, var(--border));
    background: var(--surface-hover, rgba(255, 255, 255, 0.04));
  }

  .conversation-display-option.selected {
    border-color: var(--primary);
    box-shadow: 0 0 0 1px color-mix(in srgb, var(--primary) 35%, transparent);
  }

  .conversation-display-option:focus-visible {
    outline: 1px solid var(--primary);
    outline-offset: 2px;
  }

  .conversation-display-option-icon {
    display: grid;
    place-items: center;
    width: 30px;
    height: 30px;
    flex: 0 0 30px;
    border-radius: var(--radius-sm);
    background: color-mix(in srgb, var(--primary) 10%, transparent);
    color: var(--primary);
  }

  .conversation-display-option-copy {
    display: flex;
    flex-direction: column;
    min-width: 0;
    gap: 4px;
  }

  .conversation-display-option-copy strong {
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
  }

  .conversation-display-option-copy span {
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    line-height: 1.5;
  }

  .conversation-display-option-check {
    display: grid;
    place-items: center;
    width: 20px;
    height: 20px;
    flex: 0 0 20px;
    margin-left: auto;
    border: 1px solid var(--border);
    border-radius: 50%;
    color: var(--primary);
  }

  .selected .conversation-display-option-check {
    border-color: var(--primary);
    background: var(--primary);
    color: var(--primary-foreground, white);
  }

  @media (max-width: 640px) {
    .conversation-display-options {
      grid-template-columns: 1fr;
    }
  }
</style>
