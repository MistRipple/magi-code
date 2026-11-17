<script lang="ts">
  import type { ConversationDisplayMode } from '../shared/settings-bootstrap';
  import { i18n } from '../stores/i18n.svelte';
  import Icon from './Icon.svelte';
  import SelectField from './SelectField.svelte';
  import { LOCALES, isSupportedLocale } from '../i18n/locales';
  import { switchLocale } from '../lib/locale-switch';
  import ConversationDisplayPreference from './ConversationDisplayPreference.svelte';

  let {
    userRules = $bindable(),
    userRulesSaveStatus,
    conversationDisplayMode,
    conversationDisplaySaveStatus,
    saveConversationDisplayMode,
  }: {
    userRules: string;
    userRulesSaveStatus: string;
    conversationDisplayMode: ConversationDisplayMode;
    conversationDisplaySaveStatus: 'idle' | 'saving' | 'saved' | 'error';
    saveConversationDisplayMode: (mode: ConversationDisplayMode) => void;
  } = $props();

  const userRulesStatusText = $derived.by(() => {
    switch (userRulesSaveStatus) {
      case 'saving':
        return i18n.t('settings.profile.autoSaving');
      case 'saved':
        return i18n.t('settings.profile.autoSaved');
      case 'error':
        return i18n.t('settings.profile.autoSaveFailed');
      default:
        return '';
    }
  });
</script>

<div class="apple-manager">
<div class="apple-scroller-proxy">
<div class="settings-section">
  <div class="settings-section-header">
    <div class="settings-section-title">{i18n.t('settings.locale.label')}</div>
    <div class="settings-section-actions">
      <SelectField value={i18n.locale} label={i18n.t('settings.locale.label')}
        options={LOCALES.map(locale => ({ value: locale.code, label: locale.nativeName }))}
        onchange={(code) => { if (isSupportedLocale(code)) void switchLocale(code); }} />
    </div>
  </div>
</div>

<ConversationDisplayPreference
  mode={conversationDisplayMode}
  saveStatus={conversationDisplaySaveStatus}
  onChange={saveConversationDisplayMode}
/>

<!-- 用户自定义规则 -->
<div class="settings-section">
  <div class="settings-section-header">
    <div class="settings-section-title">{i18n.t('settings.profile.userRules')}</div>
    {#if userRulesStatusText}
      <div class="rules-save-status" class:error={userRulesSaveStatus === 'error'}>
        {#if userRulesSaveStatus === 'saving'}
          <Icon name="refresh" size={13} />
        {:else if userRulesSaveStatus === 'saved'}
          <Icon name="check" size={13} />
        {:else}
          <Icon name="close" size={13} />
        {/if}
        <span>{userRulesStatusText}</span>
      </div>
    {/if}
  </div>
  <div class="settings-section-desc">{i18n.t('settings.profile.userRulesDesc')}</div>
  <div class="profile-editor">
    <div class="profile-field">
      <textarea
        class="form-textarea user-rules-textarea"
        bind:value={userRules}
        placeholder={i18n.t('settings.profile.userRulesPlaceholder')}
      ></textarea>
    </div>
  </div>
</div>

</div>
</div>

<style>
  .profile-editor { display: flex; flex-direction: column; gap: var(--space-4); margin-top: var(--space-4); }
  .profile-field { display: flex; flex-direction: column; gap: var(--space-2); }
  .user-rules-textarea { min-height: 140px; width: 100%; box-sizing: border-box; }

  .rules-save-status {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    white-space: nowrap;
  }

  .rules-save-status.error {
    color: var(--danger);
  }
</style>
