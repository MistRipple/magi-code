<script lang="ts">
  import { i18n } from '../stores/i18n.svelte';
  import Icon from './Icon.svelte';
  import Toggle from './Toggle.svelte';
  import SafeguardAuditPanel from './SafeguardAuditPanel.svelte';

  let {
    SAFEGUARD_CATEGORIES,
    getRulesForCategory,
    toggleSafeguardRule,
    updateSafeguardRuleAction,
    removeCustomRule,
    newCustomRule = $bindable(),
    addCustomRule,
    safeguardSaveStatus,
    safeguardAuditCount,
    safeguardAuditPersistenceHealthy,
    setSafeguardAuditCount,
  } = $props<{
    SAFEGUARD_CATEGORIES: any[];
    getRulesForCategory: (cat: any) => any[];
    toggleSafeguardRule: (index: number) => void;
    updateSafeguardRuleAction: (index: number, action: string) => void;
    removeCustomRule: (index: number) => void;
    newCustomRule: string;
    addCustomRule: () => void;
    safeguardSaveStatus: string;
    safeguardAuditCount: number;
    safeguardAuditPersistenceHealthy: boolean;
    setSafeguardAuditCount: (count: number) => void;
  }>();

  let view = $state<'rules' | 'audit'>('rules');

  const SAFEGUARD_ACTIONS = [
    'require_approval_in_restricted',
    'hard_block',
    'audit_only',
  ];

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
</script>

<div class="apple-manager">
<div class="apple-scroller-proxy">
  <div class="safeguard-tabs" role="tablist" aria-label={i18n.t('settings.zone.safeguard')}>
    <button type="button" role="tab" class="safeguard-tab" class:active={view === 'rules'} aria-selected={view === 'rules'} data-testid="safeguard-tab-rules" onclick={() => { view = 'rules'; }}>
      {i18n.t('settings.safeguard.tab.rules')}
    </button>
    <button type="button" role="tab" class="safeguard-tab" class:active={view === 'audit'} aria-selected={view === 'audit'} data-testid="safeguard-tab-audit" onclick={() => { view = 'audit'; }}>
      {i18n.t('settings.safeguard.tab.audit')}
      {#if safeguardAuditCount > 0}<span class="safeguard-tab-count">{safeguardAuditCount}</span>{/if}
    </button>
  </div>

  {#if view === 'rules'}
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
      <div class="safeguard-policy-note">{i18n.t('settings.safeguard.policyNote')}</div>

      <div class="safeguard-categories">
        {#each SAFEGUARD_CATEGORIES as category}
          {@const categoryRules = getRulesForCategory(category)}
          {#if categoryRules.length > 0 || category === 'custom'}
            <div class="safeguard-category">
              <div class="safeguard-category-label">{i18n.t(`settings.safeguard.category.${category}`)}</div>
              {#if categoryRules.length > 0}
                <ul class="safeguard-rules">
                  {#each categoryRules as { rule, index } (rule.pattern)}
                    <li class="safeguard-rule" class:safeguard-rule--off={!rule.enabled}>
                      <Toggle
                        size="small"
                        checked={rule.enabled}
                        ariaLabel={rule.pattern}
                        title={rule.enabled ? i18n.t('settings.tools.clickToDisable') : i18n.t('settings.tools.clickToEnable')}
                        onchange={() => toggleSafeguardRule(index)}
                      />
                      <code class="safeguard-rule-pattern">{rule.pattern}</code>
                      <select
                        class="safeguard-action-select"
                        data-action={rule.action}
                        value={rule.action}
                        aria-label={i18n.t('settings.safeguard.actionLabel')}
                        onchange={(e) => updateSafeguardRuleAction(index, e.currentTarget.value)}
                      >
                        {#each SAFEGUARD_ACTIONS as action}
                          <option value={action}>{getSafeguardActionLabel(action)}</option>
                        {/each}
                      </select>
                      {#if category === 'custom'}
                        <button
                          type="button"
                          class="btn-icon btn-icon--sm btn-icon--danger"
                          title={i18n.t('settings.tools.delete')}
                          aria-label={i18n.t('settings.tools.delete')}
                          onclick={() => removeCustomRule(index)}
                        >
                          <Icon name="delete" size={12} />
                        </button>
                      {/if}
                    </li>
                  {/each}
                </ul>
              {/if}
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
  {:else}
    <div class="settings-section" style="border-bottom: none;">
      <SafeguardAuditPanel persistenceHealthy={safeguardAuditPersistenceHealthy} onCountChange={setSafeguardAuditCount} />
    </div>
  {/if}
</div>
</div>

<style>
  .rules-save-status {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    white-space: nowrap;
  }

  .rules-save-status.error { color: var(--danger); }

  .safeguard-tabs { display: flex; gap: var(--space-1); border-bottom: 1px solid var(--border-subtle, var(--border)); margin: 0 var(--space-4); }
  .safeguard-tab {
    position: relative;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 8px 12px 10px;
    border: none;
    background: transparent;
    color: var(--foreground-muted);
    font-size: var(--text-sm);
    cursor: pointer;
  }
  .safeguard-tab:hover { color: var(--foreground); }
  .safeguard-tab.active { color: var(--foreground); font-weight: var(--font-semibold); }
  .safeguard-tab.active::after { content: ''; position: absolute; left: 12px; right: 12px; bottom: -1px; height: 2px; border-radius: 2px; background: var(--primary); }
  .safeguard-tab:focus-visible { outline: 2px solid color-mix(in srgb, var(--primary) 60%, transparent); outline-offset: -3px; border-radius: 4px; }
  .safeguard-tab-count { padding: 0 6px; border-radius: 9px; background: color-mix(in srgb, var(--foreground) 10%, transparent); font-size: 11px; font-weight: normal; font-variant-numeric: tabular-nums; }

  .safeguard-policy-note {
    margin-top: 8px;
    padding: 8px 10px;
    border: 1px solid var(--border);
    border-radius: 8px;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    line-height: 1.5;
  }

  .safeguard-categories { display: flex; flex-direction: column; gap: 16px; margin-top: 12px; }

  .safeguard-category-label {
    font-size: 12px;
    font-weight: var(--font-semibold);
    color: var(--foreground);
    margin-bottom: 6px;
  }

  /* 一行一条规则：开关 · 规则 · 动作。开关是唯一的启停控件，动作下拉与之分开，没有嵌套交互。 */
  .safeguard-rules {
    list-style: none;
    margin: 0;
    padding: 0;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    overflow: hidden;
  }

  .safeguard-rule {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    min-height: 40px;
    padding: 4px var(--space-3);
  }

  .safeguard-rule + .safeguard-rule { border-top: 1px solid var(--border-subtle, var(--border)); }

  .safeguard-rule-pattern {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--font-mono, monospace);
    font-size: 12px;
  }

  /* 停用的规则：划掉并淡化，需要注意的是被关掉的，而不是默认开着的。 */
  .safeguard-rule--off .safeguard-rule-pattern {
    color: var(--foreground-muted);
    text-decoration: line-through;
  }

  .safeguard-action-select {
    flex-shrink: 0;
    appearance: none;
    padding: 3px 22px 3px 8px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background-color: transparent;
    background-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='8' height='5' viewBox='0 0 8 5'%3E%3Cpath d='M0.5 0.5 4 4 7.5 0.5' fill='none' stroke='%23888' stroke-width='1.2' stroke-linecap='round' stroke-linejoin='round'/%3E%3C/svg%3E");
    background-repeat: no-repeat;
    background-position: right 7px center;
    color: var(--foreground);
    font-family: var(--font-sans, sans-serif);
    font-size: 12px;
    cursor: pointer;
  }

  .safeguard-action-select:hover { border-color: var(--foreground-muted); }
  .safeguard-action-select:focus-visible { outline: 1px solid var(--primary); outline-offset: 1px; }

  /* 动作强度一眼可辨：阻断最强用危险色，审计只记录用弱化色。 */
  .safeguard-action-select[data-action='hard_block'] { color: var(--danger); border-color: color-mix(in srgb, var(--danger) 45%, var(--border)); }
  .safeguard-action-select[data-action='audit_only'] { color: var(--foreground-muted); }

  .safeguard-add-row { display: flex; align-items: stretch; gap: 8px; margin-top: 8px; }
  /* 添加按钮与输入框等高：btn--sm 自带固定高度，这里让它随行拉伸。 */
  .safeguard-add-row > button { height: auto; flex-shrink: 0; }
  .safeguard-add-input { flex: 1; font-family: var(--font-mono, monospace); }
</style>
