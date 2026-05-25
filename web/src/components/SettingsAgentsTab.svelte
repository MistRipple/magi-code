<script lang="ts">
  import type { AgentBinding, ModelEngine } from '../shared/types/registry-types';
  import type { RoleTemplate } from '../shared/types/role-templates';
  import { isAgentBindingOperational, resolveSelectableRegistryEngines } from '../shared/model-governance';
  import { i18n } from '../stores/i18n.svelte';
  import Icon from './Icon.svelte';

  let {
    roleTemplates,
    registryAgents,
    registryEngines,
    getAgentColor,
    getWorkerDisplayName,
    updateRoleEngine,
  } = $props<{
    roleTemplates: RoleTemplate[];
    registryAgents: AgentBinding[];
    registryEngines: ModelEngine[];
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

  function resolveLocalizedTemplateDescription(tmpl: RoleTemplate): string {
    const key = tmpl.i18n?.descriptionKey || `roleTemplate.${tmpl.templateId}.description`;
    const translated = i18n.t(key);
    return translated !== key ? translated : (tmpl.description || tmpl.profile.focus.join('，'));
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
    if (event.key !== 'ArrowLeft' && event.key !== 'ArrowRight' && event.key !== 'Home' && event.key !== 'End') {
      return;
    }
    event.preventDefault();
    let next = idx;
    if (event.key === 'ArrowLeft') next = (idx - 1 + atoms.length) % atoms.length;
    else if (event.key === 'ArrowRight') next = (idx + 1) % atoms.length;
    else if (event.key === 'Home') next = 0;
    else if (event.key === 'End') next = atoms.length - 1;
    selectedKey = atoms[next].template.templateId;
  }
</script>

<div class="settings-tab-inner scroll-proxy">
  <div class="agents-scroll-panel settings-scroll-panel">
    <!-- ============ Top: Horizontal Tab Bar ============ -->
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
              <Icon name="bot" size={10} />
            </span>
            <span class="role-tab-name">{atom.displayName}</span>
            <span
              class="role-tab-status status-{atom.status}"
              title={statusTooltip(atom.status)}
              aria-label={statusTooltip(atom.status)}
            ></span>
          </button>
        {/each}
      </div>
    </div>

    <!-- ============ Detail Panel ============ -->
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

        <!-- Header -->
        <div class="detail-head">
          <div class="detail-avatar" style="background: {selected.muted}; color: {selected.color}">
            <Icon name="bot" size={14} />
          </div>
          <div class="detail-title-stack">
            <div class="detail-title">{selected.displayName}</div>
            {#if positioning}
              <div class="detail-kicker">{positioning}</div>
            {/if}
          </div>
        </div>

        {#if selected.description}
          <p class="detail-description">{selected.description}</p>
        {/if}

        <!-- Engine binding -->
        <section class="detail-section">
          <div class="section-title">{i18n.t('settings.agents.sectionEngine')}</div>
          <div class="engine-binding-row" class:err={selected.status === 'error'}>
            <Icon name="model" size={12} class="icon-pre" />
            <select
              class="engine-select"
              value={selected.engineId}
              onchange={(e) => updateRoleEngine(tmpl.templateId, (e.target as HTMLSelectElement).value)}
            >
              <option value="">{i18n.t('settings.agents.inheritOrchestrator')}</option>
              {#each selectableEngines as eng}
                <option value={eng.id}>{getWorkerDisplayName(eng.id)}</option>
              {/each}
            </select>
            <Icon name="chevron-down" size={10} class="icon-suf" />
          </div>
          {#if selected.status === 'error'}
            <div class="binding-hint err">{i18n.t('settings.agents.engineDisabledHint')}</div>
          {:else if selected.status === 'inherit'}
            <div class="binding-hint">{i18n.t('settings.agents.inheritOrchestratorHint')}</div>
          {/if}
        </section>

        <!-- ============ Multi-column grid: 5 secondary sections tile across width ============ -->
        <div class="detail-grid">
          <!-- focus -->
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

          <!-- constraints -->
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

          <!-- Output preferences (optional) -->
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

          <!-- ownerships -->
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

          <!-- insights -->
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
      {/if}
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
  /* padding-inline 与其他 settings tab 对齐：父 .scroll-content 提供 20px，叠加这里的 20px 让内容左基线落到 x=529（= SettingsModelTab 的 .settings-section padding 后的内容起点） */
  .settings-scroll-panel { flex: 1; min-height: 0; overflow-y: auto; padding: 0 20px 4px; scrollbar-width: none; display: flex; flex-direction: column; gap: 14px; }
  .settings-scroll-panel::-webkit-scrollbar { width: 0; }

  /* ---------- Horizontal Tab Bar ---------- */
  .agents-tabbar {
    position: relative;
    overflow-x: auto;
    overflow-y: hidden;
    scrollbar-width: none;
  }
  .agents-tabbar::-webkit-scrollbar { height: 0; }
  .tabbar-track {
    display: flex;
    align-items: stretch;
    gap: 2px;
    min-width: max-content;
    border-bottom: 1px solid var(--ind-border-separator);
  }

  .role-tab {
    position: relative;
    display: inline-flex;
    align-items: center;
    gap: 7px;
    padding: 7px 11px 9px;
    border: none;
    background: transparent;
    color: var(--ind-foreground-muted);
    font-family: inherit;
    font-size: 13px;
    font-weight: 500;
    letter-spacing: -0.005em;
    cursor: pointer;
    transition: color 0.15s ease;
    white-space: nowrap;
  }
  .role-tab:hover {
    color: var(--ind-foreground-secondary);
  }
  .role-tab.active {
    color: var(--ind-foreground);
    font-weight: 600;
  }
  .role-tab.active::after {
    content: '';
    position: absolute;
    left: 11px;
    right: 11px;
    bottom: -1px;
    height: 2px;
    background: var(--ind-tab-accent);
    border-radius: 2px;
  }
  .role-tab:focus-visible {
    outline: 2px solid color-mix(in srgb, var(--ind-tab-accent) 60%, transparent);
    outline-offset: -3px;
    border-radius: 4px;
  }

  .role-tab-avatar {
    width: 18px; height: 18px;
    border-radius: 5px;
    display: inline-flex; align-items: center; justify-content: center;
    flex-shrink: 0;
    opacity: 0.72;
    transition: opacity 0.15s ease;
  }
  .role-tab.active .role-tab-avatar { opacity: 1; }
  .role-tab:hover .role-tab-avatar { opacity: 0.9; }

  .role-tab-name {
    font-variant-numeric: tabular-nums;
  }

  .role-tab-status {
    width: 5px; height: 5px;
    border-radius: 50%;
    flex-shrink: 0;
    margin-left: 1px;
  }
  .role-tab-status.status-bound { background: var(--success, #34c759); }
  .role-tab-status.status-inherit { background: color-mix(in srgb, var(--ind-foreground-soft) 55%, transparent); }
  .role-tab-status.status-error { background: var(--error, #ff3b30); }

  /* ---------- Detail Panel (plain flow, share scroll-content padding) ---------- */
  .agents-detail {
    display: flex;
    flex-direction: column;
    gap: 18px;
    min-width: 0;
  }

  .detail-empty {
    color: var(--ind-foreground-muted);
    font-size: 12px;
    text-align: center;
    padding: 24px 0;
  }

  .detail-head {
    display: grid;
    grid-template-columns: 32px minmax(0, 1fr);
    gap: 12px;
    align-items: center;
  }
  .detail-avatar {
    width: 32px; height: 32px;
    border-radius: 9px;
    display: flex; align-items: center; justify-content: center;
    flex-shrink: 0;
  }
  .detail-title-stack {
    display: flex;
    flex-direction: column;
    min-width: 0;
    gap: 2px;
  }
  .detail-title {
    font-size: 15px;
    font-weight: 650;
    color: var(--ind-foreground);
    letter-spacing: -0.014em;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .detail-kicker {
    font-size: 12px;
    font-weight: 500;
    color: var(--ind-foreground-muted);
    letter-spacing: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .detail-description {
    font-size: 13px;
    line-height: 1.55;
    color: var(--ind-foreground-secondary);
    margin: 0;
    text-wrap: pretty;
  }

  .detail-section {
    display: flex;
    flex-direction: column;
    gap: 8px;
    min-width: 0;
  }

  /* Multi-column grid: tile the 5 secondary sections across the available width.
   * 240px minmax gives 3 columns at ≥800px wide, 2 cols at ~520-800, 1 col below.
   * row-gap matches the main agents-detail gap (18px); col-gap a bit more for breathing room. */
  .detail-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
    gap: 18px 28px;
    align-items: start;
  }
  /* 与 SettingsModelTab.settings-section-title 对齐：13px / 700，常规文字色（非 uppercase microcopy） */
  .section-title {
    font-size: 13px;
    font-weight: 700;
    letter-spacing: 0;
    text-transform: none;
    color: var(--ind-foreground);
  }

  /* Engine binding */
  .engine-binding-row {
    width: 100%;
    height: 34px;
    background: var(--ind-bg-control);
    border: 1px solid var(--ind-border-control);
    border-radius: 8px;
    display: flex;
    align-items: center;
    padding: 0 10px;
    gap: 6px;
    transition: background 0.18s ease, border-color 0.18s ease;
    box-sizing: border-box;
    max-width: 320px;
    align-self: flex-start;
  }
  .engine-binding-row:hover { background: var(--ind-bg-control-hover); border-color: var(--ind-border-control-strong); }
  .engine-binding-row.err {
    border-color: color-mix(in srgb, var(--error, #ff3b30) 36%, var(--ind-border-control));
    background: color-mix(in srgb, var(--error, #ff3b30) 8%, var(--ind-bg-control));
  }
  :global(.icon-pre), :global(.icon-suf) { opacity: 0.56; flex-shrink: 0; }
  :global(.icon-suf) { margin-left: auto; }

  /* 与 SettingsModelTab 字号一致：subtitle / label 12px，input 12px */
  .engine-select {
    flex: 1;
    min-width: 0;
    width: 100%;
    height: 100%;
    background: transparent;
    border: none;
    font-size: 12px;
    font-weight: 500;
    color: var(--ind-foreground);
    outline: none;
    cursor: pointer;
    padding: 0;
    appearance: none;
  }
  .engine-select option { color: var(--foreground); background: var(--background); }

  .binding-hint {
    font-size: 11px;
    line-height: 1.5;
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
    gap: 5px;
  }
  .detail-list li {
    position: relative;
    padding-left: 14px;
    font-size: 12px;
    line-height: 1.55;
    color: var(--ind-foreground-secondary);
    text-wrap: pretty;
  }
  .detail-list li::before {
    content: '';
    position: absolute;
    left: 4px;
    top: 9px;
    width: 4px;
    height: 4px;
    border-radius: 50%;
    background: var(--ind-foreground-soft);
  }

  /* Chips */
  .chip-row {
    display: flex;
    flex-wrap: wrap;
    gap: 5px;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    height: 24px;
    padding: 0 10px;
    border-radius: 6px;
    font-size: 11.5px;
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
  @container agents-tab (max-width: 560px) {
    .role-tab {
      padding: 9px 10px 8px;
    }
    .role-tab-avatar {
      display: none;
    }
  }
</style>
