<script lang="ts">
  import type { ConversationDisplayMode } from '../shared/settings-bootstrap';
  import { i18n } from '../stores/i18n.svelte';
  import Icon from './Icon.svelte';
  import ConversationDisplayPreference from './ConversationDisplayPreference.svelte';

  let {
    userRules = $bindable(),
    SAFEGUARD_CATEGORIES,
    getRulesForCategory,
    toggleSafeguardRule,
    updateSafeguardRuleAction,
    removeCustomRule,
    newCustomRule = $bindable(),
    addCustomRule,
    userRulesSaveStatus,
    safeguardSaveStatus,
    safeguardAuditCount,
    safeguardAuditPersistenceHealthy,
    conversationDisplayMode,
    conversationDisplaySaveStatus,
    saveConversationDisplayMode,
  } = $props<{
    userRules: string;
    SAFEGUARD_CATEGORIES: any[];
    getRulesForCategory: (cat: any) => any[];
    toggleSafeguardRule: (index: number) => void;
    updateSafeguardRuleAction: (index: number, action: string) => void;
    removeCustomRule: (index: number) => void;
    newCustomRule: string;
    addCustomRule: () => void;
    userRulesSaveStatus: string;
    safeguardSaveStatus: string;
    safeguardAuditCount: number;
    safeguardAuditPersistenceHealthy: boolean;
    conversationDisplayMode: ConversationDisplayMode;
    conversationDisplaySaveStatus: 'idle' | 'saving' | 'saved' | 'error';
    saveConversationDisplayMode: (mode: ConversationDisplayMode) => void;
  }>();

  const SAFEGUARD_ACTIONS = [
    'require_approval_in_restricted',
    'hard_block',
    'audit_only',
  ];

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

  const safeguardStatusText = $derived.by(() => {
    switch (safeguardSaveStatus) {
      case 'saving':
        return i18n.t('settings.safeguard.status.saving');
      case 'saved':
        return i18n.t('settings.safeguard.status.saved');
      case 'error':
        return i18n.t('settings.safeguard.status.error');
      default:
        return '';
    }
  });

  function getSafeguardActionLabel(action: string | undefined): string {
    switch (action) {
      case 'hard_block':
        return i18n.t('settings.safeguard.action.hardBlock');
      case 'audit_only':
        return i18n.t('settings.safeguard.action.auditOnly');
      default:
        return i18n.t('settings.safeguard.action.requireApproval');
    }
  }

  function getSafeguardRuleTitle(rule: any): string {
    const status = rule.enabled
      ? i18n.t('settings.tools.clickToDisable')
      : i18n.t('settings.tools.clickToEnable');
    return `${getSafeguardActionLabel(rule.action)}\n${status}`;
  }
</script>

<div class="apple-manager">
<div class="apple-scroller-proxy">
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

<!-- 安全防护 section -->
  <div class="settings-section" style="border-bottom: none;">
    <div class="settings-section-header">
      <div class="settings-section-title">{i18n.t('settings.safeguard.title')}</div>
      {#if safeguardStatusText}
        <div class="rules-save-status" class:error={safeguardSaveStatus === 'error'}>
          {#if safeguardSaveStatus === 'saving'}
            <Icon name="refresh" size={13} />
          {:else if safeguardSaveStatus === 'saved'}
            <Icon name="check" size={13} />
          {:else}
            <Icon name="close" size={13} />
          {/if}
          <span>{safeguardStatusText}</span>
        </div>
      {/if}
    </div>
    <div class="settings-section-desc">{i18n.t('settings.safeguard.desc')}</div>
    <div class="safeguard-policy-note">
      {i18n.t('settings.safeguard.policyNote')}
      <div class="safeguard-audit-summary" class:unhealthy={!safeguardAuditPersistenceHealthy}>
        {#if safeguardAuditPersistenceHealthy}
          {i18n.t('settings.safeguard.auditSummary', { count: safeguardAuditCount })}
        {:else}
          {i18n.t('settings.safeguard.auditUnavailable')}
        {/if}
      </div>
    </div>
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
                aria-pressed={rule.enabled}
                aria-label={`${rule.pattern}: ${getSafeguardActionLabel(rule.action)}`}
                onclick={() => toggleSafeguardRule(index)}
                onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); toggleSafeguardRule(index); } }}
                title={getSafeguardRuleTitle(rule)}
              >
                <span class="safeguard-badge-text">{rule.pattern}</span>
                <select
                  class="safeguard-action-select"
                  data-action={rule.action}
                  value={rule.action}
                  aria-label={i18n.t('settings.safeguard.actionLabel')}
                  onclick={(e) => e.stopPropagation()}
                  onkeydown={(e) => e.stopPropagation()}
                  onchange={(e) => updateSafeguardRuleAction(index, e.currentTarget.value)}
                >
                  {#each SAFEGUARD_ACTIONS as action}
                    <option value={action}>{getSafeguardActionLabel(action)}</option>
                  {/each}
                </select>
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
                class="form-input safeguard-add-input"
                bind:value={newCustomRule}
                placeholder={i18n.t('settings.safeguard.addPlaceholder')}
                onkeydown={(e) => e.key === 'Enter' && addCustomRule()}
              />
              <button class="btn btn--primary btn--sm" onclick={addCustomRule}>
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

  .safeguard-policy-note {
    margin-top: 8px;
    padding: 8px 10px;
    border: 1px solid var(--border);
    border-radius: 8px;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    line-height: 1.5;
  }

  .safeguard-audit-summary {
    margin-top: 4px;
    color: var(--foreground);
  }

  .safeguard-audit-summary.unhealthy {
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

  /* 规则默认全部启用，启用态用淡色底；实心大面积主色会让整页像一堵蓝墙，
     真正需要注意的是被停用的规则，所以停用态用虚线 + 删除线区分。 */
  .safeguard-badge.enabled {
    background: color-mix(in srgb, var(--primary) 12%, transparent);
    color: var(--foreground);
    border-color: color-mix(in srgb, var(--primary) 40%, var(--border));
  }

  .safeguard-badge.enabled:hover {
    background: color-mix(in srgb, var(--primary) 20%, transparent);
  }

  .safeguard-badge.disabled {
    background: transparent;
    color: var(--foreground-muted);
    border-color: var(--border);
    border-style: dashed;
  }

  .safeguard-badge.disabled .safeguard-badge-text {
    text-decoration: line-through;
  }

  .safeguard-badge.disabled:hover {
    border-color: var(--foreground);
    color: var(--foreground);
  }

  .safeguard-action-select {
    appearance: none;
    font-family: var(--font-sans, sans-serif);
    font-size: 10px;
    line-height: 1;
    padding: 2px 16px 2px 6px;
    border-radius: 6px;
    border: 0;
    background-color: color-mix(in srgb, var(--foreground) 10%, transparent);
    /* 下拉箭头：select 去掉原生外观后需要自己给出可点击的提示。 */
    background-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='8' height='5' viewBox='0 0 8 5'%3E%3Cpath d='M0.5 0.5 4 4 7.5 0.5' fill='none' stroke='%23888' stroke-width='1.2' stroke-linecap='round' stroke-linejoin='round'/%3E%3C/svg%3E");
    background-repeat: no-repeat;
    background-position: right 5px center;
    color: inherit;
    white-space: nowrap;
    cursor: pointer;
  }

  /* 动作强度一眼可辨：阻断最强用危险色，审计只记录用弱化色。 */
  .safeguard-action-select[data-action='hard_block'] {
    background-color: color-mix(in srgb, var(--danger) 18%, transparent);
    color: var(--danger);
  }

  .safeguard-action-select[data-action='audit_only'] {
    color: var(--foreground-muted);
  }

  .safeguard-action-select:focus {
    outline: 1px solid currentColor;
    outline-offset: 1px;
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
    background: var(--surface-hover);
  }

  .safeguard-add-row {
    display: flex;
    align-items: stretch;
    gap: 8px;
    margin-top: 8px;
  }

  /* 添加按钮与输入框等高：btn--sm 自带固定高度，这里让它随行拉伸。 */
  .safeguard-add-row > button {
    height: auto;
    flex-shrink: 0;
  }

  .safeguard-add-input {
    flex: 1;
    font-family: var(--font-mono, monospace);
  }



  /* Forms override for rules tab handled globally */
</style>
