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
    userRulesResetStatus,
    resetUserRules,
    userRulesSaveStatus,
    saveUserRules
  } = $props<{
    userRules: string;
    SAFEGUARD_CATEGORIES: any[];
    getRulesForCategory: (cat: any) => any[];
    toggleSafeguardRule: (index: number) => void;
    removeCustomRule: (index: number) => void;
    newCustomRule: string;
    addCustomRule: () => void;
    userRulesResetStatus: string;
    resetUserRules: () => void;
    userRulesSaveStatus: string;
    saveUserRules: () => void;
  }>();
</script>

<div class="apple-manager">
<div class="apple-scroller-proxy">
<!-- 用户自定义规则 -->
<div class="settings-section">
  <div class="settings-section-header">
    <div class="settings-section-title">{i18n.t('settings.profile.userRules')}</div>
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

<!-- 保存/重置 -->
<div class="settings-section profile-save-section" style="border-bottom: none; background: transparent; box-shadow: none; padding-top: 0;">
  <div class="apple-dashboard-bar" style="display: flex; justify-content: flex-end; align-items: center;">
    <div class="settings-section-actions">
      <button
        class="apple-action-btn danger"
        class:saving={userRulesResetStatus === 'saving'}
        onclick={resetUserRules}
        disabled={userRulesResetStatus === 'saving' || userRulesSaveStatus === 'saving'}
      >
        {#if userRulesResetStatus === 'saving'}
          <Icon name="refresh" size={14} />
          {i18n.t('settings.profile.processing')}
        {:else if userRulesResetStatus === 'saved'}
          <Icon name="check" size={14} />
          {i18n.t('settings.profile.resetDone')}
        {:else if userRulesResetStatus === 'error'}
          <Icon name="close" size={14} />
          {i18n.t('settings.profile.resetFailed')}
        {:else}
          {i18n.t('settings.profile.resetAll')}
        {/if}
      </button>
      <button
        class="apple-action-btn primary"
        class:saving={userRulesSaveStatus === 'saving'}
        onclick={saveUserRules}
        disabled={userRulesSaveStatus === 'saving' || userRulesResetStatus === 'saving'}
      >
        {#if userRulesSaveStatus === 'saving'}
          <Icon name="refresh" size={14} />
          {i18n.t('settings.profile.savingProfile')}
        {:else if userRulesSaveStatus === 'saved'}
          <Icon name="check" size={14} />
          {i18n.t('settings.profile.savedProfile')}
        {:else if userRulesSaveStatus === 'error'}
          <Icon name="close" size={14} />
          {i18n.t('settings.profile.saveFailed')}
        {:else}
          {i18n.t('settings.profile.saveAll')}
        {/if}
      </button>
    </div>
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
