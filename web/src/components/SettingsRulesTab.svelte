<script lang="ts">
  import { i18n } from '../stores/i18n.svelte';
  import Icon from './Icon.svelte';

  let {
    userRules = $bindable(),
    SAFEGUARD_CATEGORIES,
    getRulesForCategory,
    toggleSafeguardRule,
    removeCustomRule,
    newCustomRule = $bindable(),
    addCustomRule,
    userRulesSaveStatus,
  } = $props<{
    userRules: string;
    SAFEGUARD_CATEGORIES: any[];
    getRulesForCategory: (cat: any) => any[];
    toggleSafeguardRule: (index: number) => void;
    removeCustomRule: (index: number) => void;
    newCustomRule: string;
    addCustomRule: () => void;
    userRulesSaveStatus: string;
  }>();

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
        class="profile-textarea user-rules-textarea"
        bind:value={userRules}
        placeholder={i18n.t('settings.profile.userRulesPlaceholder')}
      ></textarea>
    </div>
  </div>
</div>

<!-- 安全防护 section -->
<div class="settings-section" style="border-bottom: none;">
  <div class="settings-section-header">
    <div class="settings-section-title">{i18n.t('settings.safeguard.title')}</div>
  </div>
  <div class="settings-section-desc">{i18n.t('settings.safeguard.desc')}</div>
  <div class="safeguard-categories">
    {#each SAFEGUARD_CATEGORIES as category}
      {@const categoryRules = getRulesForCategory(category)}
      {#if categoryRules.length > 0 || category === 'custom'}
        <div class="safeguard-category">
          <div class="safeguard-category-label">{i18n.t(`settings.safeguard.category.${category}`)}</div>
          <div class="safeguard-badges">
            {#each categoryRules as { rule, index } (rule.pattern)}
              <div
                role="button" tabindex="0"
                class="safeguard-badge"
                class:enabled={rule.enabled}
                class:disabled={!rule.enabled}
                onclick={() => toggleSafeguardRule(index)}
                onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); toggleSafeguardRule(index); } }}
                title={rule.enabled ? i18n.t('settings.tools.clickToDisable') : i18n.t('settings.tools.clickToEnable')}
              >
                <span class="safeguard-badge-text">{rule.pattern}</span>
                {#if category === 'custom'}
                  <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
                  <div role="button" tabindex="0" class="safeguard-badge-remove" onclick={(e) => { e.stopPropagation(); removeCustomRule(index); }} onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); e.stopPropagation(); removeCustomRule(index); } }} title={i18n.t('settings.tools.delete')}>×</div>
                {/if}
              </div>
            {/each}
          </div>
          {#if category === 'custom'}
            <div class="safeguard-add-row">
              <input
                type="text"
                class="safeguard-add-input"
                bind:value={newCustomRule}
                placeholder={i18n.t('settings.safeguard.addPlaceholder')}
                onkeydown={(e) => e.key === 'Enter' && addCustomRule()}
              />
              <button class="apple-action-btn" onclick={addCustomRule}>
                <Icon name="plus" size={14} />
                {i18n.t('settings.safeguard.add')}
              </button>
            </div>
          {/if}
        </div>
      {/if}
    {/each}
  </div>
</div>

</div>
</div>

<style>
  .profile-editor { display: flex; flex-direction: column; gap: var(--space-4); margin-top: var(--space-4); }
  .profile-field { display: flex; flex-direction: column; gap: var(--space-2); }
  .profile-textarea {
    padding: var(--space-3);
    font-size: var(--text-sm);
  }
  .user-rules-textarea { resize: none; min-height: 140px; width: 100%; box-sizing: border-box; }

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

  /* ── 安全防护 ── */
  .safeguard-categories {
    display: flex;
    flex-direction: column;
    gap: 12px;
    margin-top: 8px;
  }

  .safeguard-category-label {
    font-size: 12px;
    font-weight: var(--font-semibold);
    color: var(--foreground);
    margin-bottom: 6px;
  }

  .safeguard-badges {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  .safeguard-badge {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 3px 10px;
    border-radius: 12px;
    font-size: 12px;
    font-family: var(--font-mono, monospace);
    cursor: pointer;
    user-select: none;
    transition: all 0.15s ease;
    border: 1px solid transparent;
  }

  .safeguard-badge.enabled {
    background: var(--primary);
    color: var(--primary-foreground);
    border-color: var(--primary);
  }

  .safeguard-badge.enabled:hover {
    opacity: 0.85;
  }

  .safeguard-badge.disabled {
    background: transparent;
    color: var(--foreground-muted);
    border-color: var(--border);
  }

  .safeguard-badge.disabled:hover {
    border-color: var(--foreground);
    color: var(--foreground);
  }

  .safeguard-badge-remove {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 14px;
    height: 14px;
    border-radius: 50%;
    font-size: 11px;
    line-height: 1;
    cursor: pointer;
    opacity: 0.7;
  }

  .safeguard-badge-remove:hover {
    opacity: 1;
    background: rgba(255, 255, 255, 0.2);
  }

  .safeguard-badge.disabled .safeguard-badge-remove:hover {
    background: var(--surface-hover);
  }

  .safeguard-add-row {
    display: flex;
    gap: 8px;
    margin-top: 8px;
  }

  .safeguard-add-input {
    flex: 1;
    padding: 4px 10px;
    font-size: 12px;
    font-family: var(--font-mono, monospace);
  }



  /* Forms override for rules tab handled globally */
</style>
