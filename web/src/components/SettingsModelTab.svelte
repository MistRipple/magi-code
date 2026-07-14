<script lang="ts">
  import {
    resolveModelConfigTabStatus,
  } from '../shared/model-governance';
  import { resolveAgentIndicatorVariant } from '../lib/agent-status-indicator';
  import type { AgentBinding } from '../shared/types/registry-types';
  import type { RoleTemplate } from '../shared/types/role-templates';
  import { i18n } from '../stores/i18n.svelte';
  import Icon from './Icon.svelte';
  import ModelConfigForm from './ModelConfigForm.svelte';

  let {
    modelConfigTab = $bindable(),
    orchConfig = $bindable(),
    compConfig = $bindable(),
    imageConfig = $bindable(),
    workerConfigs = $bindable(),
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
    renameEngineDisplay
  } = $props<{
    modelConfigTab: string;
    orchConfig: any;
    compConfig: any;
    imageConfig: any;
    workerConfigs: Record<string, any>;
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
    fetchModelList: (type: 'orch' | 'comp' | 'image' | 'worker') => void;
    selectModel: (type: string, model: string) => void;
    saveModelConfig: (type: 'orch' | 'comp' | 'image' | 'worker') => void;
    testModelConnection: (type: 'orch' | 'comp' | 'image' | 'worker') => void;
    getStatusClass: (status: string) => string;
    getStatusText: (status: string) => string;
    getWorkerDisplayName: (workerId: string) => string;
    getAgentColor: (templateId: string, colorToken?: string) => any;
    deleteEngine: (engineId: string) => void;
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

  // --- 统一 tab 滚动状态检测 ---
  let tabbarWrapperEl: HTMLElement | undefined = $state();
  let canScrollLeft = $state(false);
  let canScrollRight = $state(false);

  function updateScrollState() {
    const el = tabbarWrapperEl?.querySelector('.tabbar-scroll') as HTMLElement | null;
    if (!el) return;
    canScrollLeft = el.scrollLeft > 2;
    canScrollRight = el.scrollLeft + el.clientWidth < el.scrollWidth - 2;
  }

  /** 切换 tab 时自动滚入可视区 */
  function scrollTabIntoView(tabId: string) {
    requestAnimationFrame(() => {
      const btn = tabbarWrapperEl?.querySelector(`.role-tab[data-tab-id="${CSS.escape(tabId)}"]`) as HTMLElement | null;
      btn?.scrollIntoView({ behavior: 'smooth', inline: 'nearest', block: 'nearest' });
      setTimeout(updateScrollState, 200);
    });
  }

  $effect(() => {
    workerModelTabs;
    modelConfigTab;
    requestAnimationFrame(updateScrollState);
  });

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
      <div
        class="tabbar-wrapper"
        bind:this={tabbarWrapperEl}
        class:can-scroll-left={canScrollLeft}
        class:can-scroll-right={canScrollRight}
      >
        <div class="tabbar-scroll" onscroll={updateScrollState}>
          <div class="tabbar-track" role="tablist">
            <!-- 主模型 -->
            <button
              type="button"
              class="role-tab"
              class:active={modelConfigTab === 'orch'}
              role="tab"
              aria-selected={modelConfigTab === 'orch'}
              data-tab-id="orch"
              onclick={() => selectTab('orch')}
            >
              <span
                class="role-tab-status {getStatusClass(resolveModelConfigTabStatus('orch', modelStatuses))}"
                title={getStatusText(resolveModelConfigTabStatus('orch', modelStatuses))}
              ></span>
              <span class="role-tab-name">{i18n.t('settings.model.orchestratorModel')}</span>
            </button>

            <!-- 辅助模型 -->
            <button
              type="button"
              class="role-tab"
              class:active={modelConfigTab === 'comp'}
              role="tab"
              aria-selected={modelConfigTab === 'comp'}
              data-tab-id="comp"
              onclick={() => selectTab('comp')}
            >
              <span
                class="role-tab-status {getStatusClass(resolveModelConfigTabStatus('comp', modelStatuses))}"
                title={getStatusText(resolveModelConfigTabStatus('comp', modelStatuses))}
              ></span>
              <span class="role-tab-name">{i18n.t('settings.model.auxiliaryModel')}</span>
            </button>

            <!-- 图片生成模型 -->
            <button
              type="button"
              class="role-tab"
              class:active={modelConfigTab === 'image'}
              role="tab"
              aria-selected={modelConfigTab === 'image'}
              data-tab-id="image"
              onclick={() => selectTab('image')}
            >
              <span
                class="role-tab-status {getStatusClass(resolveModelConfigTabStatus('imageGeneration', modelStatuses))}"
                title={getStatusText(resolveModelConfigTabStatus('imageGeneration', modelStatuses))}
              ></span>
              <span class="role-tab-name">{i18n.t('settings.model.imageGenerationModel')}</span>
            </button>

            <!-- 代理引擎 -->
            {#each workerModelTabs as workerTab (workerTab)}
              {@const workerStatus = resolveModelConfigTabStatus(workerTab, modelStatuses)}
              {@const workerIndicatorVariant = resolveAgentIndicatorVariant(workerStatus)}
              {@const workerColor = getAgentColor(workerTab)}
              {@const isActive = modelConfigTab === workerTab}
              {@const isEditing = editingTab === workerTab}
              <button
                type="button"
                class="role-tab role-tab--worker"
                class:active={isActive}
                class:editing={isEditing}
                role="tab"
                aria-selected={isActive}
                data-tab-id={workerTab}
                style="--worker-brand-color: {workerColor.color}"
                onclick={() => { if (!isEditing) selectTab(workerTab); }}
                ondblclick={(e) => { e.stopPropagation(); startRename(workerTab); }}
                title={isEditing ? '' : i18n.t('settings.model.renameEngineHint')}
              >
                <span
                  class="role-tab-status worker-dot"
                  class:brand={workerIndicatorVariant === 'brand'}
                  class:disabled={workerIndicatorVariant === 'disabled'}
                  class:warning={workerIndicatorVariant === 'warning'}
                  class:error={workerIndicatorVariant === 'error'}
                  title={getStatusText(workerStatus)}
                ></span>
                {#if isEditing}
                  <input
                    bind:this={renameInputEl}
                    class="role-tab-rename-input"
                    type="text"
                    bind:value={editingName}
                    onkeydown={onRenameKeydown}
                    onblur={commitRename}
                    onclick={(e) => e.stopPropagation()}
                    onmousedown={(e) => e.stopPropagation()}
                  />
                {:else}
                  <span class="role-tab-name">{getWorkerDisplayName(workerTab)}</span>
                  <!-- svelte-ignore a11y_click_events_have_key_events -->
                  <span
                    class="role-tab-delete"
                    role="button"
                    tabindex="-1"
                    title={i18n.t('settings.model.deleteEngine')}
                    onclick={(e) => {
                      e.stopPropagation();
                      deleteEngine(workerTab);
                    }}
                  >×</span>
                {/if}
              </button>
            {/each}

            <!-- + 新增引擎 -->
            <button
              type="button"
              class="role-tab role-tab--add"
              title={i18n.t('settings.model.addEngine')}
              onclick={openAddEngineDialog}
            >
              <Icon name="plus" size={12} />
            </button>
          </div>
        </div>
      </div>

      <div class="tab-content-area">
        {#if modelConfigTab === 'orch'}
          <ModelConfigForm
            formType="orch"
            statusKey="orch"
            bind:config={orchConfig}
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
            bind:config={compConfig}
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
            bind:config={imageConfig}
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
        {:else if workerConfigs[modelConfigTab]}
          <ModelConfigForm
            formType="worker"
            statusKey={modelConfigTab}
            bind:config={workerConfigs[modelConfigTab]}
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

        <div class="engine-usage-row engine-usage-row--system">
          <div class="engine-avatar engine-avatar--image" aria-hidden="true">
            <Icon name="sparkles" size={15} />
            <span
              class="model-status-dot {getStatusClass(resolveModelConfigTabStatus('imageGeneration', modelStatuses))}"
              title={getStatusText(resolveModelConfigTabStatus('imageGeneration', modelStatuses))}
            ></span>
          </div>
          <div class="engine-identity">
            <span class="engine-name">{i18n.t('settings.model.imageGenerationModel')}</span>
            {#if imageConfig?.model}
              <span class="engine-model-tag">{imageConfig.model}</span>
            {/if}
          </div>
          <div class="engine-consumers">
            <span class="engine-system-note">{i18n.t('settings.model.imageGenerationSystemUsage')}</span>
          </div>
        </div>

        <div class="engine-usage-row engine-usage-row--system">
          <div class="engine-avatar engine-avatar--auxiliary" aria-hidden="true">
            <Icon name="sparkles" size={15} />
            <span
              class="model-status-dot {getStatusClass(resolveModelConfigTabStatus('comp', modelStatuses))}"
              title={getStatusText(resolveModelConfigTabStatus('comp', modelStatuses))}
            ></span>
          </div>
          <div class="engine-identity">
            <span class="engine-name">{i18n.t('settings.model.auxiliaryModel')}</span>
            {#if compConfig?.model}
              <span class="engine-model-tag">{compConfig.model}</span>
            {/if}
          </div>
          <div class="engine-consumers">
            <span class="engine-system-note">{i18n.t('settings.model.auxiliarySystemUsage')}</span>
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
  </div>
</div>

<style>
  .apple-manager {
    container: settings-model / inline-size;
  }

  /* ===== 统一 tab 条（沿用「角色」tab underline 风格） ===== */
  .tabbar-wrapper {
    position: relative;
    margin-bottom: var(--space-4);
    --fade-w: 24px;
  }
  .tabbar-wrapper::before,
  .tabbar-wrapper::after {
    content: '';
    position: absolute;
    top: 0;
    bottom: 0;
    width: var(--fade-w);
    pointer-events: none;
    z-index: 1;
    opacity: 0;
    transition: opacity var(--transition-fast);
  }
  .tabbar-wrapper::before {
    left: 0;
    background: linear-gradient(to right, var(--background), transparent);
  }
  .tabbar-wrapper::after {
    right: 0;
    background: linear-gradient(to left, var(--background), transparent);
  }
  .tabbar-wrapper.can-scroll-left::before { opacity: 1; }
  .tabbar-wrapper.can-scroll-right::after { opacity: 1; }

  .tabbar-scroll {
    overflow-x: auto;
    overflow-y: hidden;
    scrollbar-width: none;
    scroll-behavior: smooth;
    -webkit-overflow-scrolling: touch;
  }
  .tabbar-scroll::-webkit-scrollbar { height: 0; }

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

  .role-tab-name {
    font-variant-numeric: tabular-nums;
  }

  /* 状态指示点：5px 圆点，沿用角色 tab 视觉 */
  .role-tab-status {
    width: 5px;
    height: 5px;
    border-radius: 50%;
    flex-shrink: 0;
    margin-left: 1px;
    background: var(--ind-foreground-soft, var(--foreground-muted));
  }
  .role-tab-status.success { background: var(--success, #34c759); }
  .role-tab-status.checking { background: var(--warning, #d97706); }
  .role-tab-status.warning { background: var(--warning, #d97706); }
  .role-tab-status.error { background: var(--error, #ff3b30); }
  .role-tab-status.disabled { background: color-mix(in srgb, var(--ind-foreground-soft) 55%, transparent); }

  /* 代理引擎 tab 的状态点支持品牌色变体 */
  .role-tab-status.worker-dot.brand { background: var(--worker-brand-color); }

  /* 删除按钮：hover 浮出 */
  .role-tab-delete {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 16px;
    height: 16px;
    border-radius: 50%;
    font-size: 13px;
    line-height: 1;
    color: var(--ind-foreground-muted);
    opacity: 0;
    transition: opacity 0.15s ease, background 0.15s ease, color 0.15s ease;
    cursor: pointer;
    margin-left: 2px;
  }
  .role-tab--worker:hover .role-tab-delete { opacity: 0.7; }
  .role-tab-delete:hover {
    opacity: 1 !important;
    background: color-mix(in srgb, var(--error, #ff3b30) 12%, transparent);
    color: var(--error, #ff3b30);
  }

  /* + 新增引擎按钮 */
  .role-tab--add {
    color: var(--ind-foreground-soft, var(--ind-foreground-muted));
    padding: 7px 9px 9px;
  }
  .role-tab--add:hover {
    color: var(--ind-tab-accent);
  }

  /* Inline rename input */
  .role-tab-rename-input {
    background: transparent;
    border: none;
    outline: none;
    font-family: inherit;
    font-size: 13px;
    font-weight: 600;
    color: var(--ind-foreground);
    padding: 0;
    margin: 0;
    width: 9ch;
    min-width: 4ch;
    border-bottom: 1px dashed var(--ind-tab-accent);
    border-radius: 0;
    letter-spacing: -0.005em;
  }
  .role-tab.editing { cursor: text; }

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
  .model-status-dot.disabled { background: var(--foreground-subtle, #94a3b8); }

  .worker-dot {
    width: 7px;
    height: 7px;
    border-radius: var(--radius-full);
    flex-shrink: 0;
    background: var(--foreground-subtle, #94a3b8);
  }
  .worker-dot.brand { background: var(--worker-brand-color); }
  .worker-dot.disabled { background: var(--foreground-subtle, #94a3b8); }
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
  .engine-avatar--auxiliary {
    color: var(--ind-foreground-secondary, var(--foreground));
  }
  .engine-avatar--image {
    color: var(--warning, #d97706);
    background: color-mix(in srgb, var(--warning, #d97706) 11%, var(--ind-bg-control, var(--surface-2)));
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
    .role-tab--worker .role-tab-delete { opacity: 1; }
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
    .role-tab--worker .role-tab-delete { opacity: 1; }
  }
</style>
