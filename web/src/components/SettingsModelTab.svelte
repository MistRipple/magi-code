<script lang="ts">
  import {
    resolveModelConfigTabStatus,
  } from '../shared/model-governance';
  import { resolveAgentIndicatorVariant } from '../lib/agent-status-indicator';
  import type { AgentBinding } from '../shared/types/registry-types';
  import type { RoleTemplate } from '../shared/types/role-templates';
  import type { VisionBuiltinTextModelRule } from '../shared/settings-bootstrap';
  import { i18n } from '../stores/i18n.svelte';
  import Icon from './Icon.svelte';
  import ModelConfigForm from './ModelConfigForm.svelte';

  let {
    modelConfigTab = $bindable(),
    orchConfig = $bindable(),
    compConfig = $bindable(),
    visionConfig = $bindable(),
    visionBuiltinTextModelRules,
    imageConfig = $bindable(),
    workerConfigs = $bindable(),
    modelConfigBaselines,
    workerModelTabs,
    modelStatuses,
    saveStatus,
    testStatus,
    fetchingModels,
    keyVisible = $bindable(),
    modelDropdownOpen,
    dropdownPosition,
    modelLists,
    roleTemplates,
    registryAgents,
    getBaseUrlPlaceholder,
    shouldRecommendStandardUrlMode,
    openModelDropdown,
    closeModelDropdown,
    fetchModelList,
    selectModel,
    saveModelConfig,
    testModelConnection,
    getStatusClass,
    getStatusText,
    getWorkerDisplayName,
    getAgentColor,
    deleteEngine,
    openAddEngineDialog,
    renameEngineDisplay,
    openWebModelSettings
  } = $props<{
    modelConfigTab: string;
    orchConfig: any;
    compConfig: any;
    visionConfig: any;
    visionBuiltinTextModelRules: VisionBuiltinTextModelRule[];
    imageConfig: any;
    workerConfigs: Record<string, any>;
    modelConfigBaselines: Record<string, any>;
    workerModelTabs: string[];
    modelStatuses: Record<string, { status?: string }>;
    saveStatus: Record<string, string>;
    testStatus: Record<string, string>;
    fetchingModels: Record<string, boolean>;
    keyVisible: Record<string, boolean>;
    modelDropdownOpen: Record<string, boolean>;
    dropdownPosition: any;
    modelLists: Record<string, string[]>;
    roleTemplates: RoleTemplate[];
    registryAgents: AgentBinding[];
    getBaseUrlPlaceholder: () => string;
    shouldRecommendStandardUrlMode: (baseUrl: string) => boolean;
    openModelDropdown: (type: string, target: HTMLElement) => void;
    closeModelDropdown: (key: string) => void;
    fetchModelList: (type: 'orch' | 'comp' | 'vision' | 'image' | 'worker', statusKey: string) => void;
    selectModel: (type: string, model: string) => void;
    saveModelConfig: (type: 'orch' | 'comp' | 'vision' | 'image' | 'worker', statusKey: string) => void;
    testModelConnection: (type: 'orch' | 'comp' | 'vision' | 'image' | 'worker', statusKey: string) => void;
    getStatusClass: (status: string) => string;
    getStatusText: (status: string) => string;
    getWorkerDisplayName: (workerId: string) => string;
    getAgentColor: (templateId: string, colorToken?: string) => any;
    deleteEngine: (engineId: string) => void;
    /** 打开 GPT Web 的设置（登录、可用性、工具通道）。它依赖内置浏览器的会话，所以设置在「浏览器」页。 */
    openWebModelSettings: () => void;
    openAddEngineDialog: () => void;
    renameEngineDisplay: (engineId: string, newName: string) => void;
  }>();

  // 角色 displayName 解析（带 i18n fallback）
  function resolveTemplateDisplayName(templateId: string): string {
    const tmpl = roleTemplates.find((t: RoleTemplate) => t.templateId === templateId);
    if (!tmpl) return templateId;
    const key = tmpl.i18n?.displayNameKey || `roleTemplate.${tmpl.templateId}.displayName`;
    const translated = i18n.t(key);
    return translated !== key ? translated : tmpl.displayName;
  }

  // 反向 lookup：每个引擎服务于哪些角色
  // engineId 空串 = 继承编排模型；非空 = 显式绑定到该 engine。
  const inheritedConsumers = $derived(
    registryAgents.filter((a: AgentBinding) => !a.engineId)
  );

  function consumersOf(engineId: string): AgentBinding[] {
    return registryAgents.filter(
      (a: AgentBinding) => a.engineId === engineId,
    );
  }

  // --- 左侧列表（窄屏时横向排列）：切换时把当前项滚入可视区 ---
  let railEl: HTMLElement | undefined = $state();

  function scrollTabIntoView(tabId: string) {
    requestAnimationFrame(() => {
      const btn = railEl?.querySelector(`[data-tab-id="${CSS.escape(tabId)}"]`) as HTMLElement | null;
      btn?.scrollIntoView({ behavior: 'smooth', inline: 'nearest', block: 'nearest' });
    });
  }

  type RailItem = { id: string; name: string; sub: string; statusClass: string; statusText: string };

  function railStatusKey(tabId: string): string {
    return tabId === 'image' ? 'imageGeneration' : tabId;
  }

  function tabStatus(tabId: string): { statusClass: string; statusText: string } {
    const status = resolveModelConfigTabStatus(railStatusKey(tabId), modelStatuses);
    return { statusClass: getStatusClass(status), statusText: getStatusText(status) };
  }

  function railItem(id: string, name: string, model: string | undefined): RailItem {
    const status = tabStatus(id);
    return { id, name, sub: model?.trim() || status.statusText, ...status };
  }

  const roleRailItems = $derived<RailItem[]>([
    { ...railItem('orch', i18n.t('settings.model.orchestratorModel'), undefined), sub: i18n.t('settings.model.sessionModelSelection') },
    railItem('comp', i18n.t('settings.model.auxiliaryModel'), compConfig?.model),
    railItem('image', i18n.t('settings.model.imageGenerationModel'), imageConfig?.model),
    railItem('vision', i18n.t('settings.model.visionModel'), visionConfig?.model),
  ]);

  const workerRailItems = $derived<RailItem[]>(
    workerModelTabs.map((id: string) => railItem(id, getWorkerDisplayName(id), workerConfigs[id]?.model)),
  );

  // 引擎很多时左侧列表默认只显示前几个（当前选中的引擎始终可见），其余「展开全部」；
  // 页面只有一个滚动区，不在列表里再嵌一层滚动。超过阈值时提供搜索。
  const ENGINE_COLLAPSED_COUNT = 6;
  let engineQuery = $state('');
  let enginesExpanded = $state(false);
  const hasManyEngines = $derived(workerRailItems.length > ENGINE_COLLAPSED_COUNT);
  const visibleWorkerRailItems = $derived.by(() => {
    const query = hasManyEngines ? engineQuery.trim().toLowerCase() : '';
    if (query) {
      return workerRailItems.filter((item) => (
        `${item.name} ${item.sub}`.toLowerCase().includes(query)
      ));
    }
    if (!hasManyEngines || enginesExpanded) return workerRailItems;
    const head = workerRailItems.slice(0, ENGINE_COLLAPSED_COUNT);
    const active = workerRailItems.find((item) => item.id === modelConfigTab);
    return active && !head.includes(active) ? [...head, active] : head;
  });

  function tabTitle(tabId: string): string {
    return roleRailItems.find((item) => item.id === tabId)?.name ?? getWorkerDisplayName(tabId);
  }

  // --- Inline rename 状态机 ---
  let editingTab = $state<string | null>(null);
  let editingName = $state('');
  let renameInputEl: HTMLInputElement | undefined = $state();

  function startRename(engineId: string) {
    if (engineId === 'orch' || engineId === 'comp' || engineId === 'image') return;
    editingTab = engineId;
    editingName = getWorkerDisplayName(engineId);
    requestAnimationFrame(() => {
      renameInputEl?.focus();
      renameInputEl?.select();
    });
  }

  function commitRename() {
    if (!editingTab) return;
    const trimmed = editingName.trim();
    if (trimmed) {
      renameEngineDisplay(editingTab, trimmed);
    }
    editingTab = null;
    editingName = '';
  }

  function cancelRename() {
    editingTab = null;
    editingName = '';
  }

  function onRenameKeydown(e: KeyboardEvent) {
    if (e.key === 'Enter') {
      e.preventDefault();
      commitRename();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      cancelRename();
    }
  }

  function selectTab(tabId: string) {
    modelConfigTab = tabId;
    scrollTabIntoView(tabId);
  }
</script>

<div class="apple-manager settings-tab-inner">
  <div class="apple-scroller-proxy">
    <div class="settings-section">
      <div class="model-workbench">
        <div class="model-rail" role="tablist" aria-orientation="vertical" aria-label={i18n.t('settings.zone.quickStart')} bind:this={railEl}>
          <div class="rail-group-label">{i18n.t('settings.model.tabGroup.roles')}</div>
          {#each roleRailItems as item (item.id)}
            <button
              type="button"
              class="rail-item"
              class:active={modelConfigTab === item.id}
              role="tab"
              aria-selected={modelConfigTab === item.id}
              data-tab-id={item.id}
              onclick={() => selectTab(item.id)}
            >
              <span class="rail-dot {item.statusClass}" title={item.statusText}></span>
              <span class="rail-copy">
                <span class="rail-name">{item.name}</span>
                <span class="rail-sub">{item.sub}</span>
              </span>
            </button>
          {/each}

          <div class="rail-group-label rail-group-label--engines">{i18n.t('settings.model.tabGroup.engines')}</div>
          {#if hasManyEngines}
            <input
              type="search"
              class="rail-search"
              bind:value={engineQuery}
              placeholder={i18n.t('settings.model.searchEngines')}
              aria-label={i18n.t('settings.model.searchEngines')}
            />
          {/if}
          <div class="rail-engines">
          {#each visibleWorkerRailItems as item (item.id)}
            {@const isActive = modelConfigTab === item.id}
            {@const isEditing = editingTab === item.id}
            <button
              type="button"
              class="rail-item rail-item--engine"
              class:active={isActive}
              class:editing={isEditing}
              role="tab"
              aria-selected={isActive}
              data-tab-id={item.id}
              onclick={() => { if (!isEditing) selectTab(item.id); }}
              ondblclick={(e) => { e.stopPropagation(); startRename(item.id); }}
              onkeydown={(e) => {
                if (isEditing) return;
                if (e.key === 'F2') { e.preventDefault(); startRename(item.id); }
                else if (e.key === 'Delete') { e.preventDefault(); deleteEngine(item.id); }
              }}
              title={isEditing ? '' : i18n.t('settings.model.renameEngineHint')}
            >
              <span class="rail-dot {item.statusClass}" title={item.statusText}></span>
              {#if isEditing}
                <input
                  bind:this={renameInputEl}
                  class="rail-rename-input"
                  type="text"
                  bind:value={editingName}
                  onkeydown={onRenameKeydown}
                  onblur={commitRename}
                  onclick={(e) => e.stopPropagation()}
                  onmousedown={(e) => e.stopPropagation()}
                />
              {:else}
                <span class="rail-copy">
                  <span class="rail-name">{item.name}</span>
                  <span class="rail-sub">{item.sub}</span>
                </span>
                <!-- svelte-ignore a11y_click_events_have_key_events -->
                <span
                  class="rail-delete"
                  role="button"
                  tabindex="-1"
                  title={i18n.t('settings.model.deleteEngine')}
                  onclick={(e) => {
                    e.stopPropagation();
                    deleteEngine(item.id);
                  }}
                >×</span>
              {/if}
            </button>
          {:else}
            <div class="rail-empty">{i18n.t('settings.model.noEngineMatch')}</div>
          {/each}
          </div>
          {#if hasManyEngines && !engineQuery.trim()}
            <button type="button" class="rail-more" onclick={() => { enginesExpanded = !enginesExpanded; }}>
              {enginesExpanded
                ? i18n.t('settings.model.collapseEngines')
                : i18n.t('settings.model.showAllEngines', { count: workerRailItems.length })}
            </button>
          {/if}

          <button type="button" class="rail-add" onclick={openAddEngineDialog}>
            <Icon name="plus" size={12} />
            <span>{i18n.t('settings.model.addEngine')}</span>
          </button>
        </div>

      <div class="tab-content-area">
        {#if modelConfigTab === 'orch'}
          <ModelConfigForm
            formType="orch"
            statusKey="orch"
            title={tabTitle(modelConfigTab)}
            statusClass={tabStatus(modelConfigTab).statusClass}
            statusLabel={tabStatus(modelConfigTab).statusText}
            bind:config={orchConfig}
            baselineConfig={modelConfigBaselines.orch}
            bind:keyVisible
            showModelField={false}
            showAdvancedOptions={false}
            description={i18n.t('settings.model.orchestratorDesc')}
            {saveStatus}
            {testStatus}
            {fetchingModels}
            {modelDropdownOpen}
            {dropdownPosition}
            {modelLists}
            {getBaseUrlPlaceholder}
            {shouldRecommendStandardUrlMode}
            {openModelDropdown}
            {closeModelDropdown}
            {fetchModelList}
            {selectModel}
            {saveModelConfig}
            {testModelConnection}
          />
        {:else if modelConfigTab === 'comp'}
          <ModelConfigForm
            formType="comp"
            statusKey="comp"
            title={tabTitle(modelConfigTab)}
            statusClass={tabStatus(modelConfigTab).statusClass}
            statusLabel={tabStatus(modelConfigTab).statusText}
            bind:config={compConfig}
            baselineConfig={modelConfigBaselines.comp}
            bind:keyVisible
            showAdvancedOptions={false}
            description={i18n.t('settings.model.auxiliaryDesc')}
            {saveStatus}
            {testStatus}
            {fetchingModels}
            {modelDropdownOpen}
            {dropdownPosition}
            {modelLists}
            {getBaseUrlPlaceholder}
            {shouldRecommendStandardUrlMode}
            {openModelDropdown}
            {closeModelDropdown}
            {fetchModelList}
            {selectModel}
            {saveModelConfig}
            {testModelConnection}
          />
        {:else if modelConfigTab === 'image'}
          <ModelConfigForm
            formType="image"
            statusKey="image"
            title={tabTitle(modelConfigTab)}
            statusClass={tabStatus(modelConfigTab).statusClass}
            statusLabel={tabStatus(modelConfigTab).statusText}
            bind:config={imageConfig}
            baselineConfig={modelConfigBaselines.image}
            bind:keyVisible
            showAdvancedOptions={false}
            description={i18n.t('settings.model.imageGenerationDesc')}
            {saveStatus}
            {testStatus}
            {fetchingModels}
            {modelDropdownOpen}
            {dropdownPosition}
            {modelLists}
            {getBaseUrlPlaceholder}
            {shouldRecommendStandardUrlMode}
            {openModelDropdown}
            {closeModelDropdown}
            {fetchModelList}
            {selectModel}
            {saveModelConfig}
            {testModelConnection}
          />
        {:else if modelConfigTab === 'vision'}
          <ModelConfigForm
            formType="vision"
            statusKey="vision"
            title={tabTitle(modelConfigTab)}
            statusClass={tabStatus(modelConfigTab).statusClass}
            statusLabel={tabStatus(modelConfigTab).statusText}
            bind:config={visionConfig}
            baselineConfig={modelConfigBaselines.vision}
            {visionBuiltinTextModelRules}
            bind:keyVisible
            showAdvancedOptions={false}
            description={i18n.t('settings.model.visionDesc')}
            {saveStatus}
            {testStatus}
            {fetchingModels}
            {modelDropdownOpen}
            {dropdownPosition}
            {modelLists}
            {getBaseUrlPlaceholder}
            {shouldRecommendStandardUrlMode}
            {openModelDropdown}
            {closeModelDropdown}
            {fetchModelList}
            {selectModel}
            {saveModelConfig}
            {testModelConnection}
          />
        {:else if workerConfigs[modelConfigTab]}
          <ModelConfigForm
            formType="worker"
            statusKey={modelConfigTab}
            title={tabTitle(modelConfigTab)}
            statusClass={tabStatus(modelConfigTab).statusClass}
            statusLabel={tabStatus(modelConfigTab).statusText}
            bind:config={workerConfigs[modelConfigTab]}
            baselineConfig={modelConfigBaselines[modelConfigTab]}
            bind:keyVisible
            showAdvancedOptions={true}
            description={null}
            {saveStatus}
            {testStatus}
            {fetchingModels}
            {modelDropdownOpen}
            {dropdownPosition}
            {modelLists}
            {getBaseUrlPlaceholder}
            {shouldRecommendStandardUrlMode}
            {openModelDropdown}
            {closeModelDropdown}
            {fetchModelList}
            {selectModel}
            {saveModelConfig}
            {testModelConnection}
          />
        {:else}
          <div class="llm-config-empty">
            <div class="llm-config-empty-inner">
              <Icon name="plus" size={24} />
              <p>{i18n.t('settings.model.noWorkerConfig')}</p>
            </div>
          </div>
        {/if}
      </div>
      </div>
    </div>

    <div class="settings-section engine-usage-section">
      <div class="settings-section-header">
        <div class="settings-section-title">{i18n.t('settings.model.engineUsageTitle')}</div>
        <div class="settings-section-subtitle">{i18n.t('settings.model.engineUsageSubtitle')}</div>
      </div>

      <div class="engine-usage-list">
        <div class="engine-usage-row engine-usage-row--system">
          <div class="engine-avatar engine-avatar--primary" aria-hidden="true">
            <Icon name="chat" size={15} />
            <span
              class="model-status-dot {getStatusClass(resolveModelConfigTabStatus('orch', modelStatuses))}"
              title={getStatusText(resolveModelConfigTabStatus('orch', modelStatuses))}
            ></span>
          </div>
          <div class="engine-identity">
            <span class="engine-name">{i18n.t('settings.model.orchestratorModel')}</span>
            <span class="engine-model-tag">{i18n.t('settings.model.sessionModelSelection')}</span>
          </div>
          <div class="engine-consumers engine-consumers--stacked">
            <span class="engine-system-note">{i18n.t('settings.model.orchestratorSystemUsage')}</span>
            {#if inheritedConsumers.length > 0}
              <div class="consumer-chip-list">
                {#each inheritedConsumers as agent (agent.templateId)}
                  {@const color = getAgentColor(agent.templateId)}
                  <span
                    class="consumer-chip"
                    style="background: {color.muted}; color: {color.color}"
                  >{resolveTemplateDisplayName(agent.templateId)}</span>
                {/each}
              </div>
            {/if}
          </div>
        </div>

        {#if workerModelTabs.length > 0}
          {#each workerModelTabs as workerId (workerId)}
            {@const consumers = consumersOf(workerId)}
            {@const workerStatus = resolveModelConfigTabStatus(workerId, modelStatuses)}
            {@const indicatorVariant = resolveAgentIndicatorVariant(workerStatus)}
            {@const workerColor = getAgentColor(workerId)}
            <div class="engine-usage-row">
              <div
                class="engine-avatar"
                style="background: {workerColor.muted}; color: {workerColor.color}"
                aria-hidden="true"
              >
                <Icon name="bot" size={15} />
                <span
                  class="worker-dot"
                  class:brand={indicatorVariant === 'brand'}
                  class:disabled={indicatorVariant === 'disabled'}
                  class:warning={indicatorVariant === 'warning'}
                  class:error={indicatorVariant === 'error'}
                  style="--worker-brand-color: {workerColor.color}"
                  title={getStatusText(workerStatus)}
                ></span>
              </div>
              <div class="engine-identity">
                <span class="engine-name">{getWorkerDisplayName(workerId)}</span>
                {#if workerConfigs[workerId]?.model}
                  <span class="engine-model-tag">{workerConfigs[workerId].model}</span>
                {/if}
              </div>
              <div class="engine-consumers">
                {#if consumers.length > 0}
                  {#each consumers as agent (agent.templateId)}
                    {@const color = getAgentColor(agent.templateId)}
                    <span
                      class="consumer-chip"
                      style="background: {color.muted}; color: {color.color}"
                    >{resolveTemplateDisplayName(agent.templateId)}</span>
                  {/each}
                {:else}
                  <span class="engine-empty">{i18n.t('settings.model.engineIdle')}</span>
                {/if}
              </div>
            </div>
          {/each}
        {/if}
      </div>
    </div>

    <div class="settings-section web-model-pointer">
      <div class="settings-section-header">
        <div class="settings-section-title">{i18n.t('settings.model.webModelTitle')}</div>
        <div class="settings-section-subtitle">{i18n.t('settings.model.webModelSubtitle')}</div>
      </div>
      <button type="button" class="web-model-pointer-button" onclick={openWebModelSettings}>
        <Icon name="globe" size={14} />
        <span>{i18n.t('settings.model.webModelOpen')}</span>
      </button>
    </div>
  </div>
</div>

<style>
  .web-model-pointer {
    margin-top: var(--space-4);
  }

  .web-model-pointer-button {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 28px;
    padding: 0 12px;
    border: 1px solid var(--border);
    border-radius: 999px;
    background: transparent;
    color: var(--foreground);
    cursor: pointer;
    font-size: var(--text-xs);
  }

  .web-model-pointer-button:hover {
    background: color-mix(in srgb, var(--foreground) 7%, transparent);
  }

  .apple-manager {
    container: settings-model / inline-size;
  }

  /* ===== 左侧列表 + 右侧详情 ===== */
  .model-workbench {
    display: grid;
    grid-template-columns: 208px minmax(0, 1fr);
    gap: var(--space-5, 20px);
    align-items: start;
  }

  .model-rail {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }

  .rail-engines {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .rail-more {
    padding: 5px var(--space-3);
    border: none;
    background: transparent;
    color: var(--primary);
    font-family: inherit;
    font-size: var(--text-xs);
    text-align: left;
    cursor: pointer;
  }
  .rail-more:hover { text-decoration: underline; }

  .rail-search {
    margin: 0 0 var(--space-1);
    padding: 5px var(--space-3);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--vscode-input-background, transparent);
    color: var(--foreground);
    font-family: inherit;
    font-size: var(--text-xs);
    outline: none;
  }
  .rail-search:focus { border-color: var(--primary); }

  .rail-empty {
    padding: var(--space-2) var(--space-3);
    color: var(--foreground-muted);
    font-size: var(--text-xs);
  }

  .rail-group-label {
    padding: var(--space-2) var(--space-3) var(--space-1);
    color: var(--foreground-muted);
    font-size: 11px;
    font-weight: var(--font-semibold);
    letter-spacing: 0.04em;
  }
  .rail-group-label--engines { margin-top: var(--space-2); }

  .rail-item {
    position: relative;
    display: flex;
    align-items: center;
    gap: var(--space-3);
    width: 100%;
    min-width: 0;
    padding: 7px var(--space-3);
    border: 1px solid transparent;
    border-radius: var(--radius-md);
    background: transparent;
    color: var(--foreground-muted);
    font-family: inherit;
    text-align: left;
    cursor: pointer;
    transition: background var(--transition-fast), color var(--transition-fast), border-color var(--transition-fast);
  }
  .rail-item:hover { background: var(--surface-hover, color-mix(in srgb, var(--foreground) 6%, transparent)); color: var(--foreground); }
  .rail-item.active {
    background: color-mix(in srgb, var(--primary) 10%, transparent);
    border-color: color-mix(in srgb, var(--primary) 28%, transparent);
    color: var(--foreground);
  }
  .rail-item:focus-visible { outline: 2px solid color-mix(in srgb, var(--primary) 60%, transparent); outline-offset: -2px; }

  .rail-copy { display: flex; flex-direction: column; gap: 1px; min-width: 0; flex: 1; }
  .rail-name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: var(--text-sm); font-weight: var(--font-medium); }
  .rail-sub { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--foreground-muted); font-size: 11px; font-family: var(--font-mono, monospace); }
  .rail-item.active .rail-name { font-weight: var(--font-semibold); }

  .rail-dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    flex-shrink: 0;
    background: var(--foreground-muted);
  }
  .rail-dot.success { background: var(--success, #16a34a); }
  .rail-dot.checking, .rail-dot.warning { background: var(--warning, #d97706); }
  .rail-dot.error { background: var(--error, #dc2626); }
  .rail-dot.disabled { background: color-mix(in srgb, var(--foreground-muted) 55%, transparent); }

  .rail-delete {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 18px;
    height: 18px;
    border-radius: 50%;
    flex-shrink: 0;
    font-size: 14px;
    line-height: 1;
    color: var(--foreground-muted);
    opacity: 0;
    transition: opacity var(--transition-fast), background var(--transition-fast), color var(--transition-fast);
    cursor: pointer;
  }
  .rail-item--engine:hover .rail-delete, .rail-item--engine.active .rail-delete { opacity: 0.7; }
  .rail-delete:hover {
    opacity: 1 !important;
    background: color-mix(in srgb, var(--error, #ff3b30) 12%, transparent);
    color: var(--error, #ff3b30);
  }

  .rail-add {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-top: var(--space-1);
    padding: 7px var(--space-3);
    border: 1px dashed var(--border);
    border-radius: var(--radius-md);
    background: transparent;
    color: var(--foreground-muted);
    font-family: inherit;
    font-size: var(--text-sm);
    cursor: pointer;
    transition: color var(--transition-fast), border-color var(--transition-fast);
  }
  .rail-add:hover { color: var(--primary); border-color: color-mix(in srgb, var(--primary) 50%, var(--border)); }

  .rail-rename-input {
    flex: 1;
    min-width: 0;
    padding: 0;
    border: none;
    border-bottom: 1px dashed var(--primary);
    outline: none;
    background: transparent;
    color: var(--foreground);
    font-family: inherit;
    font-size: var(--text-sm);
    font-weight: var(--font-semibold);
  }

  /* 窄容器：列表改为横向滑动的一排，详情在下方整宽显示。 */
  @container settings-model (max-width: 720px) {
    .model-workbench { grid-template-columns: minmax(0, 1fr); gap: var(--space-3); }
    .model-rail {
      flex-direction: row;
      align-items: stretch;
      overflow-x: auto;
      padding-bottom: var(--space-1);
      scrollbar-width: none;
    }
    .model-rail::-webkit-scrollbar { height: 0; }
    .rail-engines { display: contents; }
    .rail-search, .rail-more { display: none; }
    .rail-group-label { align-self: center; padding: 0 var(--space-1); white-space: nowrap; }
    .rail-group-label--engines { margin-top: 0; padding-left: var(--space-3); }
    .rail-item, .rail-add { width: auto; flex: 0 0 auto; }
    .rail-sub { max-width: 14ch; }
  }

  .tab-content-area {
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
  }

  /* 概览列表内的连接状态 */
  .model-status-dot {
    width: 7px;
    height: 7px;
    border-radius: var(--radius-full);
    background: var(--foreground-muted);
    flex-shrink: 0;
  }
  .model-status-dot.success { background: var(--success, #16a34a); }
  .model-status-dot.checking { background: var(--warning, #d97706); }
  .model-status-dot.warning { background: var(--warning, #d97706); }
  .model-status-dot.error { background: var(--error, #dc2626); }
  .model-status-dot.disabled { background: var(--foreground-muted, #94a3b8); }

  .worker-dot {
    width: 7px;
    height: 7px;
    border-radius: var(--radius-full);
    flex-shrink: 0;
    background: var(--foreground-muted, #94a3b8);
  }
  .worker-dot.brand { background: var(--worker-brand-color); }
  .worker-dot.disabled { background: var(--foreground-muted, #94a3b8); }
  .worker-dot.warning { background: var(--warning, #d97706); }
  .worker-dot.error { background: var(--error, #dc2626); }

  .llm-config-empty {
    display: flex;
    align-items: center;
    justify-content: center;
    flex: 1;
    color: var(--foreground-muted);
    padding: var(--space-6) 0;
  }
  .llm-config-empty-inner {
    text-align: center;
  }
  .llm-config-empty-inner p {
    margin-top: 8px;
  }

  /* ===== 引擎用途概览 ===== */
  .engine-usage-section {
    margin-top: var(--space-4);
  }
  .engine-usage-section .settings-section-header {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    justify-content: flex-start;
    gap: 4px;
    margin-bottom: var(--space-3);
  }
  .settings-section-subtitle {
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    line-height: 1.5;
  }
  .engine-usage-list {
    display: flex;
    flex-direction: column;
    background: var(--ind-bg-control, var(--surface-2));
    border: 1px solid var(--ind-border-control, var(--border));
    border-radius: 8px;
    overflow: hidden;
  }
  .engine-usage-row {
    display: grid;
    grid-template-columns: 34px minmax(130px, 176px) minmax(0, 1fr);
    column-gap: 12px;
    align-items: center;
    padding: 12px 14px;
  }
  .engine-usage-row + .engine-usage-row {
    border-top: 1px solid var(--ind-border-separator, var(--border-subtle, var(--border)));
  }
  .engine-avatar {
    position: relative;
    width: 34px;
    height: 34px;
    border-radius: 8px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    color: var(--ind-foreground-secondary, var(--foreground));
    background: var(--ind-bg-control-hover, var(--surface-3));
    box-shadow: inset 0 0 0 1px color-mix(in srgb, currentColor 10%, transparent);
  }
  .engine-avatar--primary {
    color: var(--ind-tab-accent, var(--info));
    background: color-mix(in srgb, var(--ind-tab-accent, var(--info)) 11%, var(--ind-bg-control, var(--surface-2)));
  }
  .engine-avatar > .model-status-dot,
  .engine-avatar > .worker-dot {
    position: absolute;
    right: -2px;
    bottom: -2px;
    border: 2px solid var(--ind-bg-control, var(--surface-2));
    box-sizing: content-box;
  }
  .engine-identity {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .engine-name {
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--ind-foreground, var(--foreground));
    line-height: 1.35;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .engine-model-tag {
    font-size: var(--text-xs);
    color: var(--ind-foreground-soft, var(--foreground-muted));
    font-family: var(--font-mono, ui-monospace, SFMono-Regular, monospace);
    line-height: 1.35;
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    align-self: flex-start;
  }
  .engine-consumers {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: center;
    align-content: center;
    min-width: 0;
  }
  .engine-consumers--stacked {
    flex-direction: column;
    flex-wrap: nowrap;
    align-items: flex-start;
    gap: 8px;
  }
  .consumer-chip-list {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: center;
  }
  .consumer-chip {
    display: inline-flex;
    align-items: center;
    min-height: 22px;
    padding: 3px 9px;
    border-radius: var(--radius-full);
    font-size: var(--text-xs);
    font-weight: var(--font-medium);
    white-space: nowrap;
    line-height: 1;
    box-shadow: inset 0 0 0 1px color-mix(in srgb, currentColor 10%, transparent);
  }
  .engine-empty,
  .engine-system-note {
    font-size: var(--text-xs);
    color: var(--ind-foreground-muted, var(--foreground-muted));
    line-height: 1.5;
  }

  @container settings-model (max-width: 640px) {
    .rail-item--engine .rail-delete { opacity: 1; }
    .engine-usage-row {
      grid-template-columns: 34px minmax(0, 1fr);
      column-gap: 12px;
      row-gap: 8px;
      align-items: center;
    }
    .engine-avatar { grid-row: 1 / span 2; align-self: start; }
    .engine-identity { grid-column: 2; }
    .engine-consumers {
      grid-column: 2;
    }
  }

  @media (max-width: 768px) {
    .rail-item--engine .rail-delete { opacity: 1; }
  }
</style>
