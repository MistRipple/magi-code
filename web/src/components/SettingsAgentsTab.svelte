<script lang="ts">
  import type { AgentBinding, ModelEngine } from '../shared/types/registry-types';
  import type { RoleTemplate } from '../shared/types/role-templates';
  import { isAgentBindingOperational, resolveSelectableRegistryEngines } from '../shared/model-governance';
  import { i18n } from '../stores/i18n.svelte';
  import Icon from './Icon.svelte';
  import EnginePicker from './EnginePicker.svelte';

  let {
    roleTemplates,
    registryAgents,
    registryEngines,
    inheritModelLabel = '',
    modelStatuses = {},
    getAgentColor,
    getWorkerDisplayName,
    updateRoleEngine,
  } = $props<{
    roleTemplates: RoleTemplate[];
    registryAgents: AgentBinding[];
    registryEngines: ModelEngine[];
    inheritModelLabel?: string;
    modelStatuses?: Record<string, { status?: string }>;
    getAgentColor: (templateId: string, colorToken?: string) => { color: string; muted: string };
    getWorkerDisplayName: (workerId: string) => string;
    updateRoleEngine: (templateId: string, engineId: string) => void;
  }>();

  type RoleStatus = 'bound' | 'inherit' | 'error';

  type RoleAtom = {
    template: RoleTemplate;
    agent: AgentBinding | undefined;
    status: RoleStatus;
    engineId: string;
    displayName: string;
    description: string;
    color: string;
    muted: string;
  };

  const selectableEngines = $derived(resolveSelectableRegistryEngines(registryEngines));

  function resolveLocalizedTemplateDisplayName(tmpl: RoleTemplate): string {
    const key = tmpl.i18n?.displayNameKey || `roleTemplate.${tmpl.templateId}.displayName`;
    const translated = i18n.t(key);
    return translated !== key ? translated : tmpl.displayName;
  }

  function formatLocalizedFallbackList(items: string[]): string {
    const normalized = items.map((item) => item.trim()).filter(Boolean);
    if (normalized.length === 0) return '';
    return new Intl.ListFormat(i18n.locale, { style: 'short', type: 'conjunction' }).format(normalized);
  }

  function resolveLocalizedTemplateDescription(tmpl: RoleTemplate): string {
    const key = tmpl.i18n?.descriptionKey || `roleTemplate.${tmpl.templateId}.description`;
    const translated = i18n.t(key);
    if (translated !== key) return translated;
    if (tmpl.description) return tmpl.description;
    const localizedFocus = tmpl.profile.focus.map((item, index) => (
      resolveLocalizedListPhrase(tmpl, 'focus', index, item)
    ));
    return formatLocalizedFallbackList(localizedFocus);
  }

  function resolveLocalizedRolePositioning(tmpl: RoleTemplate): string {
    const key = `roleTemplate.${tmpl.templateId}.role`;
    const translated = i18n.t(key);
    return translated !== key ? translated : tmpl.profile.role;
  }

  function resolveLocalizedListPhrase(
    tmpl: RoleTemplate,
    kind: 'focus' | 'constraints' | 'outputPreferences' | 'ownerships',
    index: number,
    raw: string,
  ): string {
    const key = `roleTemplate.${tmpl.templateId}.${kind}.${index}`;
    const translated = i18n.t(key);
    return translated !== key ? translated : raw;
  }

  const atoms = $derived<RoleAtom[]>(
    (roleTemplates as RoleTemplate[]).map((tmpl: RoleTemplate): RoleAtom => {
      // 故意触达 i18n.locale，使 displayName / description 在切换语言时重新求值
      void i18n.locale;
      const agent = (registryAgents as AgentBinding[]).find((a) => a.templateId === tmpl.templateId);
      const isExplicit = Boolean(agent?.engineId);
      const isOperational = agent ? isAgentBindingOperational(agent, registryEngines) : true;
      const status: RoleStatus = !isExplicit ? 'inherit' : isOperational ? 'bound' : 'error';
      const pair = getAgentColor(tmpl.templateId, tmpl.defaultUI?.colorToken);
      return {
        template: tmpl,
        agent,
        status,
        engineId: isExplicit ? (agent!.engineId as string) : '',
        displayName: resolveLocalizedTemplateDisplayName(tmpl),
        description: resolveLocalizedTemplateDescription(tmpl),
        color: pair.color,
        muted: pair.muted,
      };
    })
  );

  let selectedKey = $state<string | null>(null);

  $effect(() => {
    if (atoms.length === 0) {
      if (selectedKey !== null) selectedKey = null;
      return;
    }
    if (selectedKey === null || !atoms.some((a: RoleAtom) => a.template.templateId === selectedKey)) {
      selectedKey = atoms[0].template.templateId;
    }
  });

  const selected = $derived<RoleAtom | null>(
    atoms.find((a: RoleAtom) => a.template.templateId === selectedKey) ?? null
  );

  function statusTooltip(status: RoleStatus): string {
    if (status === 'bound') return i18n.t('settings.agents.statusBound');
    if (status === 'error') return i18n.t('settings.agents.statusError');
    return i18n.t('settings.agents.statusInherit');
  }

  function insightLabel(kind: 'decision' | 'contract' | 'risk' | 'constraint'): string {
    switch (kind) {
      case 'decision': return i18n.t('settings.agents.insightDecision');
      case 'contract': return i18n.t('settings.agents.insightContract');
      case 'risk': return i18n.t('settings.agents.insightRisk');
      case 'constraint': return i18n.t('settings.agents.insightConstraint');
    }
  }

  function onTabKeydown(event: KeyboardEvent, idx: number) {
    if (
      event.key !== 'ArrowLeft'
      && event.key !== 'ArrowRight'
      && event.key !== 'ArrowUp'
      && event.key !== 'ArrowDown'
      && event.key !== 'Home'
      && event.key !== 'End'
    ) {
      return;
    }
    event.preventDefault();
    let next = idx;
    if (event.key === 'ArrowLeft' || event.key === 'ArrowUp') next = (idx - 1 + atoms.length) % atoms.length;
    else if (event.key === 'ArrowRight' || event.key === 'ArrowDown') next = (idx + 1) % atoms.length;
    else if (event.key === 'Home') next = 0;
    else if (event.key === 'End') next = atoms.length - 1;
    selectedKey = atoms[next].template.templateId;
  }
</script>

<div class="settings-tab-inner scroll-proxy">
  <div class="agents-scroll-panel settings-scroll-panel">
    <div class="agents-shell">
      <div class="agents-tabbar" role="tablist" aria-label={i18n.t('settings.agents.listTitle')}>
        <div class="tabbar-track">
          {#each atoms as atom, idx (atom.template.templateId)}
            {@const isSelected = atom.template.templateId === selectedKey}
            <button
              type="button"
              class="role-tab"
              class:active={isSelected}
              role="tab"
              id="agent-tab-{atom.template.templateId}"
              aria-selected={isSelected}
              aria-controls="agent-tabpanel"
              tabindex={isSelected ? 0 : -1}
              onclick={() => (selectedKey = atom.template.templateId)}
              onkeydown={(e) => onTabKeydown(e, idx)}
            >
              <span class="role-tab-avatar" style="background: {atom.muted}; color: {atom.color}" aria-hidden="true">
                <Icon name="bot" size={11} />
              </span>
              <span class="role-tab-copy">
                <span class="role-tab-name">{atom.displayName}</span>
                <span class="role-tab-subtitle">{statusTooltip(atom.status)}</span>
              </span>
              <span
                class="role-tab-status status-{atom.status}"
                title={statusTooltip(atom.status)}
                aria-label={statusTooltip(atom.status)}
              ></span>
            </button>
          {/each}
        </div>
      </div>

      <div
        id="agent-tabpanel"
        class="agents-detail"
        role="tabpanel"
        aria-labelledby={selected ? `agent-tab-${selected.template.templateId}` : undefined}
      >
        {#if !selected}
          <div class="detail-empty">{i18n.t('settings.agents.detailEmpty')}</div>
        {:else}
          {@const tmpl = selected.template}
          {@const positioning = resolveLocalizedRolePositioning(tmpl)}

          <div class="detail-layout">
            <div class="detail-primary">
              <header class="detail-header">
                <div class="detail-avatar" style="background: {selected.muted}; color: {selected.color}">
                  <Icon name="bot" size={18} />
                </div>
                <div class="detail-title-stack">
                  <div class="detail-title-row">
                    <span class="detail-title">{selected.displayName}</span>
                    <span class="detail-status-pill status-{selected.status}">{statusTooltip(selected.status)}</span>
                  </div>
                  {#if positioning}
                    <div class="detail-kicker">{positioning}</div>
                  {/if}
                  {#if selected.description}
                    <p class="detail-description">{selected.description}</p>
                  {/if}
                </div>
              </header>

              <section class="detail-section engine-row">
                <div class="section-title">{i18n.t('settings.agents.sectionEngine')}</div>
                <EnginePicker
                  value={selected.engineId}
                  engines={selectableEngines}
                  inheritModelLabel={inheritModelLabel}
                  getDisplayName={getWorkerDisplayName}
                  modelStatuses={modelStatuses}
                  error={selected.status === 'error'}
                  onchange={(engineId: string) => updateRoleEngine(tmpl.templateId, engineId)}
                />
                {#if selected.status === 'error'}
                  <div class="binding-hint err">{i18n.t('settings.agents.engineDisabledHint')}</div>
                {:else if selected.status === 'inherit'}
                  <div class="binding-hint">{i18n.t('settings.agents.inheritOrchestratorHint')}</div>
                {/if}
              </section>
            </div>

            <div class="detail-masonry">
              {#if tmpl.profile.focus.length > 0}
                <section class="detail-section">
                  <div class="section-title">{i18n.t('settings.agents.sectionFocus')}</div>
                  <ul class="detail-list">
                    {#each tmpl.profile.focus as item, i}
                      <li>{resolveLocalizedListPhrase(tmpl, 'focus', i, item)}</li>
                    {/each}
                  </ul>
                </section>
              {/if}

              {#if tmpl.profile.constraints.length > 0}
                <section class="detail-section">
                  <div class="section-title">{i18n.t('settings.agents.sectionConstraints')}</div>
                  <ul class="detail-list">
                    {#each tmpl.profile.constraints as item, i}
                      <li>{resolveLocalizedListPhrase(tmpl, 'constraints', i, item)}</li>
                    {/each}
                  </ul>
                </section>
              {/if}

              {#if tmpl.profile.outputPreferences && tmpl.profile.outputPreferences.length > 0}
                <section class="detail-section">
                  <div class="section-title">{i18n.t('settings.agents.sectionOutput')}</div>
                  <ul class="detail-list">
                    {#each tmpl.profile.outputPreferences as item, i}
                      <li>{resolveLocalizedListPhrase(tmpl, 'outputPreferences', i, item)}</li>
                    {/each}
                  </ul>
                </section>
              {/if}

              {#if tmpl.ownerships.length > 0}
                <section class="detail-section">
                  <div class="section-title">{i18n.t('settings.agents.sectionOwnerships')}</div>
                  <div class="chip-row">
                    {#each tmpl.ownerships as item, i}
                      <span class="chip">{resolveLocalizedListPhrase(tmpl, 'ownerships', i, item)}</span>
                    {/each}
                  </div>
                </section>
              {/if}

              {#if tmpl.insightPreferences.length > 0}
                <section class="detail-section">
                  <div class="section-title">{i18n.t('settings.agents.sectionInsights')}</div>
                  <div class="chip-row">
                    {#each tmpl.insightPreferences as kind}
                      <span class="chip chip-insight chip-insight-{kind}">{insightLabel(kind)}</span>
                    {/each}
                  </div>
                </section>
              {/if}
            </div>
          </div>
        {/if}
      </div>
    </div>
  </div>
</div>

<style>
  .settings-tab-inner {
    container-type: inline-size;
    container-name: agents-tab;
    /* 覆盖 settings.css 默认值：本 tab 自己用 .settings-scroll-panel 承担滚动 */
    height: 100%;
    width: 100%;
    overflow: hidden;
  }

  .scroll-proxy { min-height: 0; }
  .settings-scroll-panel {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 0 20px 4px;
    scrollbar-width: none;
    display: flex;
    flex-direction: column;
  }
  .settings-scroll-panel::-webkit-scrollbar { width: 0; }

  .agents-shell {
    display: grid;
    grid-template-columns: 220px minmax(0, 1fr);
    gap: 22px;
    min-height: 100%;
    align-items: stretch;
  }

  .agents-tabbar {
    position: relative;
    overflow: hidden;
    scrollbar-width: none;
    border-right: 1px solid var(--ind-border-separator);
    padding: 8px 14px 8px 0;
  }
  .agents-tabbar::-webkit-scrollbar { height: 0; }
  .tabbar-track {
    display: flex;
    flex-direction: column;
    gap: 5px;
    min-width: 0;
  }

  .role-tab {
    position: relative;
    display: grid;
    grid-template-columns: 28px minmax(0, 1fr) 7px;
    align-items: center;
    gap: 10px;
    width: 100%;
    min-height: 48px;
    padding: 7px 9px;
    border: none;
    border-radius: 8px;
    background: transparent;
    color: var(--ind-foreground-muted);
    font-family: inherit;
    font-size: 13px;
    font-weight: 500;
    letter-spacing: -0.005em;
    cursor: pointer;
    transition: background 0.15s ease, color 0.15s ease;
    text-align: left;
  }
  .role-tab:hover {
    background: var(--ind-bg-control);
    color: var(--ind-foreground-secondary);
  }
  .role-tab.active {
    background: var(--ind-bg-control-hover);
    color: var(--ind-foreground);
    font-weight: 600;
  }
  .role-tab.active::before {
    content: '';
    position: absolute;
    left: 0;
    top: 9px;
    bottom: 9px;
    width: 2px;
    background: var(--ind-tab-accent);
    border-radius: 2px;
  }
  .role-tab:focus-visible {
    outline: 2px solid color-mix(in srgb, var(--ind-tab-accent) 60%, transparent);
    outline-offset: -3px;
    border-radius: 4px;
  }

  .role-tab-avatar {
    width: 28px; height: 28px;
    border-radius: 8px;
    display: inline-flex; align-items: center; justify-content: center;
    flex-shrink: 0;
    opacity: 0.78;
    transition: opacity 0.15s ease;
  }
  .role-tab.active .role-tab-avatar { opacity: 1; }
  .role-tab:hover .role-tab-avatar { opacity: 0.9; }

  .role-tab-copy {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }

  .role-tab-name {
    font-variant-numeric: tabular-nums;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .role-tab-subtitle {
    font-size: 11.5px;
    font-weight: 500;
    line-height: 1.2;
    color: var(--ind-foreground-soft);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .role-tab-status {
    width: 6px; height: 6px;
    border-radius: 50%;
    flex-shrink: 0;
  }
  .role-tab-status.status-bound { background: var(--success, #34c759); }
  .role-tab-status.status-inherit { background: color-mix(in srgb, var(--ind-foreground-soft) 55%, transparent); }
  .role-tab-status.status-error { background: var(--error, #ff3b30); }

  /* ---------- Detail Panel ---------- */
  .agents-detail {
    display: flex;
    min-height: 0;
    min-width: 0;
    padding: 8px 0 16px;
  }

  .detail-empty {
    color: var(--ind-foreground-muted);
    font-size: 13px;
    text-align: center;
    padding: 32px 0;
  }

  .detail-layout {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 22px;
    align-items: start;
    align-content: start;
    width: 100%;
    min-width: 0;
  }

  .detail-primary {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(240px, 300px);
    gap: 24px;
    align-items: start;
    min-width: 0;
    padding-bottom: 18px;
    border-bottom: 1px solid var(--ind-border-separator);
  }

  .detail-header {
    display: grid;
    grid-template-columns: 44px minmax(0, 1fr);
    gap: 14px;
    align-items: start;
  }
  .detail-avatar {
    width: 44px; height: 44px;
    border-radius: 12px;
    display: flex; align-items: center; justify-content: center;
    flex-shrink: 0;
    box-shadow: inset 0 0 0 1px color-mix(in srgb, currentColor 12%, transparent);
  }
  .detail-title-row {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 10px;
    min-width: 0;
  }

  .engine-row {
    gap: 9px;
    padding: 0;
    margin: 0;
    border-bottom: none;
  }

  .detail-masonry {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    column-gap: 34px;
    row-gap: 0;
    align-items: start;
    min-width: 0;
  }
  .detail-title-stack {
    display: flex;
    flex-direction: column;
    min-width: 0;
    gap: 6px;
  }
  .detail-title {
    font-size: 18px;
    font-weight: 650;
    color: var(--ind-foreground);
    letter-spacing: -0.018em;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .detail-status-pill {
    display: inline-flex;
    align-items: center;
    height: 22px;
    padding: 0 8px;
    border-radius: 999px;
    border: 1px solid var(--ind-border-control);
    font-size: 11.5px;
    font-weight: 600;
    line-height: 1;
    color: var(--ind-foreground-secondary);
    background: var(--ind-bg-control);
    white-space: nowrap;
  }
  .detail-status-pill.status-bound {
    color: var(--success, #34c759);
    border-color: color-mix(in srgb, var(--success, #34c759) 28%, var(--ind-border-control));
    background: color-mix(in srgb, var(--success, #34c759) 8%, var(--ind-bg-control));
  }
  .detail-status-pill.status-inherit {
    color: var(--ind-foreground-muted);
  }
  .detail-status-pill.status-error {
    color: var(--error, #ff3b30);
    border-color: color-mix(in srgb, var(--error, #ff3b30) 34%, var(--ind-border-control));
    background: color-mix(in srgb, var(--error, #ff3b30) 8%, var(--ind-bg-control));
  }
  .detail-kicker {
    font-size: 13px;
    font-weight: 500;
    color: var(--ind-foreground-muted);
    letter-spacing: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .detail-description {
    font-size: 13.5px;
    line-height: 1.58;
    color: var(--ind-foreground-secondary);
    margin: 0;
    text-wrap: pretty;
  }

  .detail-section {
    display: flex;
    flex-direction: column;
    gap: 9px;
    min-width: 0;
    padding: 0 0 18px;
    margin: 0 0 18px;
    border-bottom: 1px solid var(--ind-border-separator);
  }
  .detail-masonry .detail-section:nth-last-child(-n + 2) {
    margin-bottom: 0;
    border-bottom: none;
  }

  /* 与 SettingsModelTab.settings-section-title 对齐：14px / 700，常规文字色（非 uppercase microcopy） */
  .section-title {
    font-size: 13px;
    font-weight: 700;
    letter-spacing: 0;
    text-transform: none;
    color: var(--ind-foreground);
  }

  /* Engine binding hint */
  .binding-hint {
    font-size: 12px;
    line-height: 1.55;
    color: var(--ind-foreground-muted);
    padding-left: 2px;
  }
  .binding-hint.err { color: var(--error, #ff3b30); }

  /* Lists */
  .detail-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 7px;
  }
  .detail-list li {
    position: relative;
    padding-left: 16px;
    font-size: 13px;
    line-height: 1.55;
    color: var(--ind-foreground-secondary);
    text-wrap: pretty;
  }
  .detail-list li::before {
    content: '';
    position: absolute;
    left: 4px;
    top: 10px;
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: var(--ind-foreground-soft);
  }

  /* Chips */
  .chip-row {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    height: 28px;
    padding: 0 12px;
    border-radius: 7px;
    font-size: 12.5px;
    font-weight: 500;
    color: var(--ind-foreground-secondary);
    background: color-mix(in srgb, var(--ind-foreground) 5%, transparent);
    border: 1px solid var(--ind-border-separator);
    letter-spacing: -0.005em;
  }
  .chip-insight-decision { color: var(--ind-tab-accent); border-color: color-mix(in srgb, var(--ind-tab-accent) 24%, transparent); background: color-mix(in srgb, var(--ind-tab-accent) 8%, transparent); }
  .chip-insight-contract { color: #5856d6; border-color: color-mix(in srgb, #5856d6 24%, transparent); background: color-mix(in srgb, #5856d6 8%, transparent); }
  .chip-insight-risk { color: var(--error, #ff3b30); border-color: color-mix(in srgb, var(--error, #ff3b30) 24%, transparent); background: color-mix(in srgb, var(--error, #ff3b30) 8%, transparent); }
  .chip-insight-constraint { color: var(--warning, #ff9500); border-color: color-mix(in srgb, var(--warning, #ff9500) 28%, transparent); background: color-mix(in srgb, var(--warning, #ff9500) 9%, transparent); }

  /* ---------- Responsive ---------- */
  @container agents-tab (max-width: 760px) {
    .agents-shell {
      grid-template-columns: 1fr;
      gap: 16px;
    }
    .agents-tabbar {
      border-right: none;
      border-bottom: 1px solid var(--ind-border-separator);
      padding: 0 0 10px;
      overflow-x: auto;
    }
    .tabbar-track {
      flex-direction: row;
      min-width: max-content;
      gap: 4px;
    }
    .role-tab {
      flex: 0 0 auto;
      width: auto;
      min-height: 38px;
      grid-template-columns: 20px max-content 6px;
      padding: 7px 10px;
      white-space: nowrap;
    }
    .role-tab-copy {
      min-width: max-content;
    }
    .role-tab-name {
      overflow: visible;
      text-overflow: clip;
    }
    .role-tab.active::before {
      left: 10px;
      right: 10px;
      top: auto;
      bottom: -10px;
      width: auto;
      height: 2px;
    }
    .role-tab-subtitle {
      display: none;
    }
    .role-tab-avatar {
      width: 20px;
      height: 20px;
      border-radius: 6px;
    }
    .detail-layout {
      grid-template-columns: 1fr;
      gap: 22px;
    }
    .detail-primary {
      grid-template-columns: 1fr;
      gap: 18px;
    }
    .engine-row {
      padding-top: 16px;
      border-top: 1px solid var(--ind-border-separator);
    }
  }

  @container agents-tab (max-width: 560px) {
    .detail-masonry {
      grid-template-columns: 1fr;
    }
    .detail-masonry .detail-section {
      margin-bottom: 18px;
      border-bottom: 1px solid var(--ind-border-separator);
    }
    .detail-masonry .detail-section:last-child {
      margin-bottom: 0;
      border-bottom: none;
    }
    .role-tab {
      grid-template-columns: max-content 6px;
      padding: 9px 10px 8px;
    }
    .role-tab-avatar {
      display: none;
    }
  }
</style>
