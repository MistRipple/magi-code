<script lang="ts">
  import {
    resolveModelConfigTabStatus,
  } from '../shared/model-governance';
  import { resolveAgentIndicatorVariant } from '../lib/agent-status-indicator';
  import { i18n } from '../stores/i18n.svelte';
  import Icon from './Icon.svelte';
  import ModelConfigForm from './ModelConfigForm.svelte';

  let {
    modelConfigTab = $bindable(),
    orchConfig = $bindable(),
    compConfig = $bindable(),
    workerConfigs = $bindable(),
    workerModelTabs,
    workerModelTab = $bindable(),
    modelStatuses,
    saveStatus,
    testStatus,
    fetchingModels,
    keyVisible = $bindable(),
    modelDropdownOpen,
    dropdownPosition,
    modelLists,
    getBaseUrlPlaceholder,
    shouldRecommendStandardUrlMode,
    getOpenAiProtocolValue,
    setOpenAiProtocolValue,
    openModelDropdown,
    fetchModelList,
    selectModel,
    saveModelConfig,
    testModelConnection,
    getStatusClass,
    getStatusText,
    getWorkerDisplayName,
    handleWorkerEnabledToggle,
    getAgentColor,
    deleteEngine,
    openAddEngineDialog
  } = $props<{
    modelConfigTab: string;
    orchConfig: any;
    compConfig: any;
    workerConfigs: Record<string, any>;
    workerModelTabs: string[];
    workerModelTab: string;
    modelStatuses: Record<string, { status?: string }>;
    saveStatus: Record<string, string>;
    testStatus: Record<string, string>;
    fetchingModels: Record<string, boolean>;
    keyVisible: Record<string, boolean>;
    modelDropdownOpen: Record<string, boolean>;
    dropdownPosition: any;
    modelLists: Record<string, string[]>;
    getBaseUrlPlaceholder: (provider: string) => string;
    shouldRecommendStandardUrlMode: (provider: any, baseUrl: string) => boolean;
    getOpenAiProtocolValue: (config: any) => 'responses' | 'chat';
    setOpenAiProtocolValue: (config: any, value: unknown) => void;
    openModelDropdown: (type: string, target: HTMLElement) => void;
    fetchModelList: (type: 'orch' | 'comp' | 'worker') => void;
    selectModel: (type: 'orch' | 'comp' | 'worker', model: string) => void;
    saveModelConfig: (type: 'orch' | 'comp' | 'worker') => void;
    testModelConnection: (type: 'orch' | 'comp' | 'worker') => void;
    getStatusClass: (status: string) => string;
    getStatusText: (status: string) => string;
    getWorkerDisplayName: (workerId: string) => string;
    handleWorkerEnabledToggle: (workerId: string, enabled: boolean) => void;
    getAgentColor: (templateId: string, colorToken?: string) => any;
    deleteEngine: (engineId: string) => void;
    openAddEngineDialog: () => void;
  }>();

  // --- Worker tabs 滚动状态检测 ---
  let workerTabsWrapperEl: HTMLElement | undefined = $state();
  let canScrollLeft = $state(false);
  let canScrollRight = $state(false);

  function updateScrollState() {
    const el = workerTabsWrapperEl?.querySelector('.worker-model-tabs') as HTMLElement | null;
    if (!el) return;
    canScrollLeft = el.scrollLeft > 2;
    canScrollRight = el.scrollLeft + el.clientWidth < el.scrollWidth - 2;
  }

  /** 切换 worker tab 时自动滚入可视区 */
  function scrollWorkerTabIntoView(tabId: string) {
    requestAnimationFrame(() => {
      const btn = workerTabsWrapperEl?.querySelector(`.worker-model-tab[data-tab-id="${CSS.escape(tabId)}"]`) as HTMLElement | null;
      btn?.scrollIntoView({ behavior: 'smooth', inline: 'nearest', block: 'nearest' });
      setTimeout(updateScrollState, 200);
    });
  }

  $effect(() => {
    workerModelTabs;
    workerModelTab;
    requestAnimationFrame(updateScrollState);
  });
</script>

<div class="apple-manager">
  <div class="apple-scroller-proxy">
    <div class="settings-section">
      <div class="model-config-stack">
        <div class="segmented-control model-primary-tabs">
          <button
            class="segmented-control__option"
            class:active={modelConfigTab === 'orch'}
            onclick={() => (modelConfigTab = 'orch')}
          >
            <span
              class="model-status-dot {getStatusClass(resolveModelConfigTabStatus('orch', modelStatuses, workerConfigs))}"
              title={getStatusText(resolveModelConfigTabStatus('orch', modelStatuses, workerConfigs))}
            ></span>
            {i18n.t('settings.model.orchestratorModel')}
          </button>
          <button
            class="segmented-control__option"
            class:active={modelConfigTab === 'comp'}
            onclick={() => (modelConfigTab = 'comp')}
          >
            <span
              class="model-status-dot {getStatusClass(resolveModelConfigTabStatus('comp', modelStatuses, workerConfigs))}"
              title={getStatusText(resolveModelConfigTabStatus('comp', modelStatuses, workerConfigs))}
            ></span>
            {i18n.t('settings.model.auxiliaryModel')}
          </button>
        </div>

        {#if modelConfigTab === 'orch'}
          <ModelConfigForm
            formType="orch"
            statusKey="orch"
            bind:config={orchConfig}
            bind:keyVisible
            showAdvancedOptions={true}
            description={i18n.t('settings.model.orchestratorDesc')}
            {saveStatus}
            {testStatus}
            {fetchingModels}
            {modelDropdownOpen}
            {dropdownPosition}
            {modelLists}
            {getBaseUrlPlaceholder}
            {shouldRecommendStandardUrlMode}
            {getOpenAiProtocolValue}
            {setOpenAiProtocolValue}
            {openModelDropdown}
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
            {getOpenAiProtocolValue}
            {setOpenAiProtocolValue}
            {openModelDropdown}
            {fetchModelList}
            {selectModel}
            {saveModelConfig}
            {testModelConnection}
          />
        {/if}
      </div>
    </div>

    <div class="settings-section">
      <div class="settings-section-header">
        <div class="settings-section-title">{i18n.t('settings.model.workerModel')}</div>
      </div>
      <div
        class="worker-tabs-scroll-wrapper"
        bind:this={workerTabsWrapperEl}
        class:can-scroll-left={canScrollLeft}
        class:can-scroll-right={canScrollRight}
      >
        <div class="worker-model-tabs" onscroll={updateScrollState}>
          {#each workerModelTabs as workerTab (workerTab)}
            {@const workerStatus = resolveModelConfigTabStatus(workerTab, modelStatuses, workerConfigs)}
            {@const workerIndicatorVariant = resolveAgentIndicatorVariant(workerStatus)}
            {@const workerColor = getAgentColor(workerTab)}
            <button
              class="worker-model-tab"
              class:active={workerModelTab === workerTab}
              data-tab-id={workerTab}
              style="--worker-brand-color: {workerColor.color}"
              onclick={() => {
                workerModelTab = workerTab;
                scrollWorkerTabIntoView(workerTab);
              }}
            >
              <span
                class="worker-dot"
                class:brand={workerIndicatorVariant === 'brand'}
                class:disabled={workerIndicatorVariant === 'disabled'}
                class:warning={workerIndicatorVariant === 'warning'}
                class:error={workerIndicatorVariant === 'error'}
                title={getStatusText(workerStatus)}
              ></span>
              {getWorkerDisplayName(workerTab)}
              <!-- svelte-ignore a11y_click_events_have_key_events -->
              <span
                class="worker-tab-delete"
                role="button"
                tabindex="-1"
                title={i18n.t('settings.model.deleteEngine')}
                onclick={(e) => {
                  e.stopPropagation();
                  deleteEngine(workerTab);
                }}
              >×</span>
            </button>
          {/each}
          <button
            class="worker-model-tab worker-model-tab--add"
            title={i18n.t('settings.model.addEngine')}
            onclick={openAddEngineDialog}
          >
            <Icon name="plus" size={12} />
          </button>
        </div>
      </div>

      {#if workerConfigs[workerModelTab]}
        <ModelConfigForm
          formType="worker"
          statusKey={workerModelTab}
          bind:config={workerConfigs[workerModelTab]}
          bind:keyVisible
          showAdvancedOptions={true}
          showEnabledToggle={true}
          description={null}
          {saveStatus}
          {testStatus}
          {fetchingModels}
          {modelDropdownOpen}
          {dropdownPosition}
          {modelLists}
          {getBaseUrlPlaceholder}
          {shouldRecommendStandardUrlMode}
          {getOpenAiProtocolValue}
          {setOpenAiProtocolValue}
          {openModelDropdown}
          {fetchModelList}
          {selectModel}
          {saveModelConfig}
          {testModelConnection}
          {handleWorkerEnabledToggle}
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

<style>
  .apple-manager {
    container: settings-model / inline-size;
    --model-toggle-column-width: 100px;
  }

  .model-config-stack {
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
  }

  .model-primary-tabs {
    width: min(100%, 400px);
    margin: 0 auto 24px 0;
  }

  .model-status-dot {
    width: 8px;
    height: 8px;
    border-radius: var(--radius-full);
    background: var(--foreground-muted);
    flex-shrink: 0;
  }
  .model-status-dot.success { background: var(--success, #16a34a); }
  .model-status-dot.checking { background: var(--warning, #d97706); }
  .model-status-dot.warning { background: var(--warning, #d97706); }
  .model-status-dot.error { background: var(--error, #dc2626); }
  .model-status-dot.disabled { background: var(--foreground-subtle, #94a3b8); }

  .segmented-control {
    display: grid;
    grid-template-columns: 1fr 1fr;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    overflow: hidden;
    min-width: 0;
    height: var(--btn-height-md);
    background: var(--surface-2);
  }
  .segmented-control__option {
    border: none;
    background: transparent;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    font-weight: var(--font-medium);
    cursor: pointer;
    transition: all var(--transition-fast);
    min-width: 0;
    padding: 0 var(--space-2);
    white-space: nowrap;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
  }
  .segmented-control__option + .segmented-control__option {
    border-left: 1px solid var(--border);
  }
  .segmented-control__option:hover {
    color: var(--foreground);
    background: var(--surface-3);
  }
  .segmented-control__option.active {
    background: var(--primary);
    color: var(--primary-foreground);
  }

  .worker-tabs-scroll-wrapper {
    position: relative;
    margin-bottom: var(--space-4);
    --fade-w: 24px;
  }
  .worker-tabs-scroll-wrapper::before,
  .worker-tabs-scroll-wrapper::after {
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
  .worker-tabs-scroll-wrapper::before {
    left: 0;
    background: linear-gradient(to right, var(--background), transparent);
  }
  .worker-tabs-scroll-wrapper::after {
    right: 0;
    background: linear-gradient(to left, var(--background), transparent);
  }
  .worker-tabs-scroll-wrapper.can-scroll-left::before { opacity: 1; }
  .worker-tabs-scroll-wrapper.can-scroll-right::after { opacity: 1; }

  .worker-model-tabs {
    display: flex;
    gap: var(--space-2);
    overflow-x: auto;
    flex-wrap: nowrap;
    scroll-behavior: smooth;
    -webkit-overflow-scrolling: touch;
    scrollbar-width: none;
    padding-bottom: 2px;
  }
  .worker-model-tabs::-webkit-scrollbar { display: none; }

  .worker-model-tab {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    height: var(--btn-height-sm);
    padding: 0 var(--space-3);
    font-size: var(--text-sm);
    background: transparent;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    color: var(--foreground-muted);
    cursor: pointer;
    transition: all var(--transition-fast);
    flex: 0 0 auto;
    white-space: nowrap;
  }
  .worker-model-tab:hover {
    background: var(--surface-hover);
    color: var(--foreground);
  }
  .worker-model-tab.active {
    background: var(--primary);
    border-color: var(--primary);
    color: var(--primary-foreground);
  }
  .worker-model-tab--add {
    padding: 0 var(--space-2);
    min-width: var(--btn-height-sm);
    justify-content: center;
    border-style: dashed;
  }

  .worker-dot {
    width: 8px;
    height: 8px;
    border-radius: var(--radius-full);
    flex-shrink: 0;
    background: var(--foreground-subtle, #94a3b8);
  }
  .worker-dot.brand { background: var(--worker-brand-color); }
  .worker-dot.disabled { background: var(--foreground-subtle, #94a3b8); }
  .worker-dot.warning { background: var(--warning, #d97706); }
  .worker-dot.error { background: var(--error, #dc2626); }

  .worker-tab-delete {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 16px;
    height: 16px;
    border-radius: var(--radius-full);
    font-size: 13px;
    line-height: 1;
    color: var(--foreground-muted);
    opacity: 0.5;
    transition: opacity var(--transition-fast), background var(--transition-fast);
    cursor: pointer;
    margin-left: 2px;
  }
  .worker-model-tab:hover .worker-tab-delete { opacity: 1; }
  .worker-tab-delete:hover {
    background: var(--danger-bg, rgba(220, 38, 38, 0.15));
    color: var(--danger, #dc2626);
  }
  .worker-model-tab.active .worker-tab-delete { color: var(--primary-foreground); opacity: 0.8; }
  .worker-model-tab.active .worker-tab-delete:hover {
    background: rgba(255, 255, 255, 0.25);
    color: var(--primary-foreground);
  }

  .llm-config-empty {
    display: flex;
    align-items: center;
    justify-content: center;
    flex: 1;
    color: var(--foreground-muted);
  }
  .llm-config-empty-inner {
    text-align: center;
  }
  .llm-config-empty-inner p {
    margin-top: 8px;
  }

  @container settings-model (max-width: 640px) {
    .model-primary-tabs {
      width: 100%;
      margin: 0 0 var(--space-4) 0;
    }
    .worker-model-tab {
      min-width: max-content;
    }
    .worker-tab-delete {
      opacity: 1;
    }
  }

  @media (max-width: 768px) {
    .model-primary-tabs {
      width: 100%;
      max-width: none;
      margin: 0 0 var(--space-4) 0;
    }
    .worker-model-tab {
      min-width: max-content;
    }
    .worker-tab-delete {
      opacity: 1;
    }
  }
</style>
