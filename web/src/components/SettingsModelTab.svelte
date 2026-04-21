<script lang="ts">
  import { resolveModelConfigTabStatus } from '../shared/model-governance';
  import { resolveAgentIndicatorVariant } from '../lib/agent-status-indicator';
  import { i18n } from '../stores/i18n.svelte';
  import Icon from './Icon.svelte';
  import Toggle from './Toggle.svelte';

  let {
    modelConfigTab = $bindable(),
    orchConfig,
    compConfig,
    workerConfigs,
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
      // 等滚动完再更新遮罩
      setTimeout(updateScrollState, 200);
    });
  }

  $effect(() => {
    // tab 列表变化或选中变化时更新状态
    workerModelTabs;
    workerModelTab;
    requestAnimationFrame(updateScrollState);
  });

  function handleModelListAction(
    type: "orch" | "comp" | "worker",
    event: MouseEvent,
  ) {
    const button = event.currentTarget as HTMLElement | null;
    const input = button?.parentElement?.querySelector("input") ?? null;
    const key = type === "worker" ? workerModelTab : type;
    const hasModels = Array.isArray(modelLists[key]) && modelLists[key].length > 0;

    if (hasModels && input) {
      openModelDropdown(key, input);
      return;
    }
    void fetchModelList(type);
  }
</script>

        <!-- 模型配置 Tab -->
        <div class="apple-manager">
          <div class="apple-scroller-proxy">
        <div class="settings-section">
          <div class="model-config-stack">
            <div class="segmented-control" style="max-width: 400px; margin: 0 auto 24px 0;">
              <button class="segmented-control__option" class:active={modelConfigTab === 'orch'} onclick={() => modelConfigTab = 'orch'}>
                <span class="model-status-dot {getStatusClass(resolveModelConfigTabStatus('orch', modelStatuses, workerConfigs))}" title={getStatusText(resolveModelConfigTabStatus('orch', modelStatuses, workerConfigs))}></span>
                {i18n.t('settings.model.orchestratorModel')}
              </button>
              <button class="segmented-control__option" class:active={modelConfigTab === 'comp'} onclick={() => modelConfigTab = 'comp'}>
                <span class="model-status-dot {getStatusClass(resolveModelConfigTabStatus('comp', modelStatuses, workerConfigs))}" title={getStatusText(resolveModelConfigTabStatus('comp', modelStatuses, workerConfigs))}></span>
                {i18n.t('settings.model.auxiliaryModel')}
              </button>
            </div>

            {#if modelConfigTab === 'orch'}
              <!-- svelte-ignore a11y_label_has_associated_control -->
              <div class="llm-config-form">
                  <div class="llm-config-field-row url-mode-row">
                    <div class="llm-config-field">
                      <label class="llm-config-label">{i18n.t('settings.model.field.baseUrl')}</label>
                      <input
                        type="text"
                        class="llm-config-input"
                        bind:value={orchConfig.baseUrl}
                        placeholder={getBaseUrlPlaceholder(orchConfig.provider)}
                      >
                    </div>
                    <div class="llm-config-field llm-config-field--compact">
                      <label class="llm-config-label">{i18n.t('settings.model.field.urlMode')}</label>
                      <div class="segmented-control">
                        <button
                          type="button"
                          class="segmented-control__option"
                          class:active={orchConfig.urlMode === 'standard'}
                          onclick={() => orchConfig.urlMode = 'standard'}
                        >
                          {i18n.t('settings.model.urlMode.standard')}
                        </button>
                        <button
                          type="button"
                          class="segmented-control__option"
                          class:active={orchConfig.urlMode === 'full'}
                          onclick={() => orchConfig.urlMode = 'full'}
                        >
                          {i18n.t('settings.model.urlMode.full')}
                        </button>
                      </div>
                      {#if shouldRecommendStandardUrlMode(orchConfig.provider, orchConfig.baseUrl)}
                        <div class="llm-config-hint">
                          {i18n.t('settings.model.urlMode.standardRecommended')}
                        </div>
                      {/if}
                    </div>
                  </div>
                  <div class="llm-config-field">
                    <label class="llm-config-label">{i18n.t('settings.model.field.apiKey')}</label>
                    <div class="api-key-wrapper">
                      <input type={keyVisible.orch ? 'text' : 'password'} class="llm-config-input api-key-input" bind:value={orchConfig.apiKey} placeholder="sk-ant-...">
                      <button type="button" class="api-key-toggle" onclick={() => keyVisible.orch = !keyVisible.orch} title={keyVisible.orch ? i18n.t('input.hideKey') : i18n.t('input.showKey')}>
                        <Icon name={keyVisible.orch ? 'eye-slash' : 'eye'} size={14} />
                      </button>
                    </div>
                  </div>
                  <div class="llm-config-field-row has-thinking" class:has-level={orchConfig.provider === 'openai'}>
                    <div class="llm-config-field">
                      <label class="llm-config-label">{i18n.t('settings.model.field.model')}</label>
                      <div class="model-combobox">
                        <input type="text" class="llm-config-input" bind:value={orchConfig.model}
                          onfocus={(e) => { if (modelLists.orch.length > 0) openModelDropdown('orch', e.currentTarget); }}
                        >
                        {#if !orchConfig.model}
                          <button
                            class="model-fetch-btn"
                            onclick={(event) => handleModelListAction('orch', event)}
                            disabled={fetchingModels.orch || (modelLists.orch.length === 0 && (!orchConfig.baseUrl || !orchConfig.apiKey))}
                            aria-label={modelLists.orch.length > 0 ? i18n.t('settings.model.openModelList') : i18n.t('settings.model.fetchModelList')}
                            title={modelLists.orch.length > 0 ? i18n.t('settings.model.openModelList') : i18n.t('settings.model.fetchModelList')}
                          >
                            {#if fetchingModels.orch}
                              <Icon name="refresh" size={12} />
                            {:else if modelLists.orch.length > 0}
                              <Icon name="chevron-down" size={12} />
                            {:else}
                              <Icon name="download" size={12} />
                            {/if}
                          </button>
                        {/if}
                        {#if modelDropdownOpen.orch && modelLists.orch.length > 0}
                          <div class="model-dropdown" style="top: {dropdownPosition.top}px; left: {dropdownPosition.left}px; width: {dropdownPosition.width}px;">
                            {#each modelLists.orch as m}
                              <button class="model-dropdown-item" class:selected={orchConfig.model === m} onclick={() => selectModel('orch', m)}>{m}</button>
                            {/each}
                          </div>
                        {/if}
                      </div>
                    </div>
                    <div class="llm-config-field">
                      <label class="llm-config-label">{i18n.t('settings.model.field.provider')}</label>
                      <select class="llm-config-select" bind:value={orchConfig.provider}>
                        <option value="openai">{i18n.t('settings.model.provider.openai')}</option>
                        <option value="anthropic">{i18n.t('settings.model.provider.anthropic')}</option>
                      </select>
                    </div>
                    {#if orchConfig.provider === 'openai'}
                    <div class="llm-config-field">
                      <label class="llm-config-label">{i18n.t('settings.model.field.protocol')}</label>
                      <select class="llm-config-select" bind:value={orchConfig.openaiProtocol}>
                        <option value="responses">{i18n.t('settings.model.protocol.responses')}</option>
                        <option value="chat">{i18n.t('settings.model.protocol.chat')}</option>
                      </select>
                    </div>
                    <div class="llm-config-field">
                      <label class="llm-config-label">{i18n.t('settings.model.field.level')}</label>
                      <select class="llm-config-select" bind:value={orchConfig.reasoningEffort}>
                        <option value="low">{i18n.t('settings.model.reasoning.low')}</option>
                        <option value="medium">{i18n.t('settings.model.reasoning.medium')}</option>
                        <option value="high">{i18n.t('settings.model.reasoning.high')}</option>
                        <option value="xhigh">{i18n.t('settings.model.reasoning.xhigh')}</option>
                      </select>
                    </div>
                    {/if}
                    <div class="llm-config-field inline-toggle" style="align-items: center; flex-direction: row; gap: 8px;">
                      <label class="llm-config-label" style="margin: 0;">{i18n.t('settings.model.field.thinking')}</label>
                      <Toggle
                        size="small"
                        checked={orchConfig.thinking}
                        title={orchConfig.thinking ? i18n.t('settings.model.disableThinking') : i18n.t('settings.model.enableThinking')}
                        onchange={() => {
                          orchConfig.thinking = !orchConfig.thinking;
                        }}
                      />
                    </div>
                  </div>
                  <div class="apple-dashboard-bar" style="display: flex; justify-content: space-between; align-items: center; margin-top: 24px;">
                    <span style="font-size: 12px; color: var(--foreground-muted);">{i18n.t('settings.model.orchestratorDesc')}</span>
                    <div class="settings-section-actions">
                      <button
                        class="apple-action-btn secondary"
                        class:testing={testStatus.orch === 'testing'}
                        class:success={testStatus.orch === 'success'}
                        class:error={testStatus.orch === 'error'}
                        onclick={() => testModelConnection('orch')}
                        disabled={testStatus.orch === 'testing'}
                      >
                        {#if testStatus.orch === 'testing'}
                          <Icon name="refresh" size={14} />
                          {i18n.t('settings.model.testing')}
                        {:else if testStatus.orch === 'success'}
                          <Icon name="check" size={14} />
                          {i18n.t('settings.model.testSuccess')}
                        {:else if testStatus.orch === 'error'}
                          <Icon name="close" size={14} />
                          {i18n.t('settings.model.testFailed')}
                        {:else}
                          <Icon name="check" size={14} />
                          {i18n.t('settings.model.testConnection')}
                        {/if}
                      </button>
                      <button
                        class="apple-action-btn primary"
                        class:saving={saveStatus.orch === 'saving'}
                        onclick={() => saveModelConfig('orch')}
                        disabled={saveStatus.orch === 'saving'}
                      >
                        {#if saveStatus.orch === 'saving'}
                          <Icon name="refresh" size={14} />
                          {i18n.t('settings.model.saving')}
                        {:else if saveStatus.orch === 'saved'}
                          <Icon name="check" size={14} />
                          {i18n.t('settings.model.saved')}
                        {:else}
                          {i18n.t('settings.model.saveConfig')}
                        {/if}
                      </button>
                    </div>
                  </div>
                </div>
            {:else if modelConfigTab === 'comp'}
              <!-- svelte-ignore a11y_label_has_associated_control -->
              <div class="llm-config-form">
                  <div class="llm-config-field-row url-mode-row">
                    <div class="llm-config-field">
                      <label class="llm-config-label">{i18n.t('settings.model.field.baseUrl')}</label>
                      <input
                        type="text"
                        class="llm-config-input"
                        bind:value={compConfig.baseUrl}
                        placeholder={getBaseUrlPlaceholder(compConfig.provider)}
                      >
                    </div>
                    <div class="llm-config-field llm-config-field--compact">
                      <label class="llm-config-label">{i18n.t('settings.model.field.urlMode')}</label>
                      <div class="segmented-control">
                        <button
                          type="button"
                          class="segmented-control__option"
                          class:active={compConfig.urlMode === 'standard'}
                          onclick={() => compConfig.urlMode = 'standard'}
                        >
                          {i18n.t('settings.model.urlMode.standard')}
                        </button>
                        <button
                          type="button"
                          class="segmented-control__option"
                          class:active={compConfig.urlMode === 'full'}
                          onclick={() => compConfig.urlMode = 'full'}
                        >
                          {i18n.t('settings.model.urlMode.full')}
                        </button>
                      </div>
                      {#if shouldRecommendStandardUrlMode(compConfig.provider, compConfig.baseUrl)}
                        <div class="llm-config-hint">
                          {i18n.t('settings.model.urlMode.standardRecommended')}
                        </div>
                      {/if}
                    </div>
                  </div>
                  <div class="llm-config-field">
                    <label class="llm-config-label">{i18n.t('settings.model.field.apiKey')}</label>
                    <div class="api-key-wrapper">
                      <input type={keyVisible.comp ? 'text' : 'password'} class="llm-config-input api-key-input" bind:value={compConfig.apiKey} placeholder="sk-ant-...">
                      <button type="button" class="api-key-toggle" onclick={() => keyVisible.comp = !keyVisible.comp} title={keyVisible.comp ? i18n.t('input.hideKey') : i18n.t('input.showKey')}>
                        <Icon name={keyVisible.comp ? 'eye-slash' : 'eye'} size={14} />
                      </button>
                    </div>
                  </div>
                  <div class="llm-config-field-row">
                    <div class="llm-config-field">
                      <label class="llm-config-label">{i18n.t('settings.model.field.model')}</label>
                      <div class="model-combobox">
                        <input type="text" class="llm-config-input" bind:value={compConfig.model}
                          onfocus={(e) => { if (modelLists.comp.length > 0) openModelDropdown('comp', e.currentTarget); }}
                        >
                        {#if !compConfig.model}
                          <button
                            class="model-fetch-btn"
                            onclick={(event) => handleModelListAction('comp', event)}
                            disabled={fetchingModels.comp || (modelLists.comp.length === 0 && (!compConfig.baseUrl || !compConfig.apiKey))}
                            aria-label={modelLists.comp.length > 0 ? i18n.t('settings.model.openModelList') : i18n.t('settings.model.fetchModelList')}
                            title={modelLists.comp.length > 0 ? i18n.t('settings.model.openModelList') : i18n.t('settings.model.fetchModelList')}
                          >
                            {#if fetchingModels.comp}
                              <Icon name="refresh" size={12} />
                            {:else if modelLists.comp.length > 0}
                              <Icon name="chevron-down" size={12} />
                            {:else}
                              <Icon name="download" size={12} />
                            {/if}
                          </button>
                        {/if}
                        {#if modelDropdownOpen.comp && modelLists.comp.length > 0}
                          <div class="model-dropdown" style="top: {dropdownPosition.top}px; left: {dropdownPosition.left}px; width: {dropdownPosition.width}px;">
                            {#each modelLists.comp as m}
                              <button class="model-dropdown-item" class:selected={compConfig.model === m} onclick={() => selectModel('comp', m)}>{m}</button>
                            {/each}
                          </div>
                        {/if}
                      </div>
                    </div>
                    <div class="llm-config-field">
                      <label class="llm-config-label">{i18n.t('settings.model.field.provider')}</label>
                      <select class="llm-config-select" bind:value={compConfig.provider}>
                        <option value="openai">{i18n.t('settings.model.provider.openai')}</option>
                        <option value="anthropic">{i18n.t('settings.model.provider.anthropic')}</option>
                      </select>
                    </div>
                    {#if compConfig.provider === 'openai'}
                    <div class="llm-config-field">
                      <label class="llm-config-label">{i18n.t('settings.model.field.protocol')}</label>
                      <select class="llm-config-select" bind:value={compConfig.openaiProtocol}>
                        <option value="responses">{i18n.t('settings.model.protocol.responses')}</option>
                        <option value="chat">{i18n.t('settings.model.protocol.chat')}</option>
                      </select>
                    </div>
                    {/if}
                  </div>
                  <div class="apple-dashboard-bar" style="display: flex; justify-content: space-between; align-items: center; margin-top: 24px;">
                    <span style="font-size: 12px; color: var(--foreground-muted);">{i18n.t('settings.model.auxiliaryDesc')}</span>
                    <div class="settings-section-actions">
                      <button
                        class="apple-action-btn secondary"
                        class:testing={testStatus.comp === 'testing'}
                        class:success={testStatus.comp === 'success'}
                        class:error={testStatus.comp === 'error'}
                        onclick={() => testModelConnection('comp')}
                        disabled={testStatus.comp === 'testing'}
                      >
                        {#if testStatus.comp === 'testing'}
                          <Icon name="refresh" size={14} />
                          {i18n.t('settings.model.testing')}
                        {:else if testStatus.comp === 'success'}
                          <Icon name="check" size={14} />
                          {i18n.t('settings.model.testSuccess')}
                        {:else if testStatus.comp === 'error'}
                          <Icon name="close" size={14} />
                          {i18n.t('settings.model.testFailed')}
                        {:else}
                          <Icon name="check" size={14} />
                          {i18n.t('settings.model.testConnection')}
                        {/if}
                      </button>
                      <button
                        class="apple-action-btn primary"
                        class:saving={saveStatus.comp === 'saving'}
                        onclick={() => saveModelConfig('comp')}
                        disabled={saveStatus.comp === 'saving'}
                      >
                        {#if saveStatus.comp === 'saving'}
                          <Icon name="refresh" size={14} />
                          {i18n.t('settings.model.saving')}
                        {:else if saveStatus.comp === 'saved'}
                          <Icon name="check" size={14} />
                          {i18n.t('settings.model.saved')}
                        {:else}
                          {i18n.t('settings.model.saveConfig')}
                        {/if}
                      </button>
                    </div>
                  </div>
                </div>
            {/if}
          </div>
        </div>
        <div class="settings-section">
            <div class="settings-section-header">
              <div class="settings-section-title">{i18n.t('settings.model.workerModel')}</div>
            </div>
            <div class="worker-tabs-scroll-wrapper" bind:this={workerTabsWrapperEl} class:can-scroll-left={canScrollLeft} class:can-scroll-right={canScrollRight}>
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
                    onclick={() => { workerModelTab = workerTab; scrollWorkerTabIntoView(workerTab); }}
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
                    <span class="worker-tab-delete" role="button" tabindex="-1" title={i18n.t('settings.model.deleteEngine')} onclick={(e) => { e.stopPropagation(); deleteEngine(workerTab); }}>×</span>
                  </button>
                {/each}
                <button class="worker-model-tab worker-model-tab--add" title={i18n.t('settings.model.addEngine')} onclick={openAddEngineDialog}>
                  <Icon name="plus" size={12} />
                </button>
              </div>
            </div>
            <!-- svelte-ignore a11y_label_has_associated_control -->
            {#if workerConfigs[workerModelTab]}
            <div class="llm-config-form">
              <div class="llm-config-field-row worker-url-mode-row">
                <div class="llm-config-field">
                  <label class="llm-config-label">{i18n.t('settings.model.field.baseUrl')}</label>
                  <input
                    type="text"
                    class="llm-config-input"
                    bind:value={workerConfigs[workerModelTab].baseUrl}
                    placeholder={getBaseUrlPlaceholder(workerConfigs[workerModelTab].provider)}
                  >
                </div>
                <div class="llm-config-field llm-config-field--compact">
                  <label class="llm-config-label">{i18n.t('settings.model.field.urlMode')}</label>
                  <div class="segmented-control">
                    <button
                      type="button"
                      class="segmented-control__option"
                      class:active={workerConfigs[workerModelTab].urlMode === 'standard'}
                      onclick={() => workerConfigs[workerModelTab].urlMode = 'standard'}
                    >
                      {i18n.t('settings.model.urlMode.standard')}
                    </button>
                    <button
                      type="button"
                      class="segmented-control__option"
                      class:active={workerConfigs[workerModelTab].urlMode === 'full'}
                      onclick={() => workerConfigs[workerModelTab].urlMode = 'full'}
                    >
                      {i18n.t('settings.model.urlMode.full')}
                    </button>
                  </div>
                  {#if shouldRecommendStandardUrlMode(workerConfigs[workerModelTab].provider, workerConfigs[workerModelTab].baseUrl)}
                    <div class="llm-config-hint">
                      {i18n.t('settings.model.urlMode.standardRecommended')}
                    </div>
                  {/if}
                </div>
                <div class="llm-config-field inline-toggle" style="align-items: center; flex-direction: row; gap: 8px;">
                  <label class="llm-config-label" style="margin: 0;">{i18n.t('settings.model.enable')}</label>
                  <Toggle
                    size="small"
                    checked={workerConfigs[workerModelTab].enabled}
                    onchange={() => handleWorkerEnabledToggle(workerModelTab, !workerConfigs[workerModelTab].enabled)}
                  />
                </div>
              </div>
              <div class="llm-config-field">
                <label class="llm-config-label">{i18n.t('settings.model.field.apiKey')}</label>
                <div class="api-key-wrapper">
                  <input type={keyVisible.worker ? 'text' : 'password'} class="llm-config-input api-key-input" bind:value={workerConfigs[workerModelTab].apiKey} placeholder="sk-ant-...">
                  <button type="button" class="api-key-toggle" onclick={() => keyVisible.worker = !keyVisible.worker} title={keyVisible.worker ? i18n.t('input.hideKey') : i18n.t('input.showKey')}>
                    <Icon name={keyVisible.worker ? 'eye-slash' : 'eye'} size={14} />
                  </button>
                </div>
              </div>
              <div class="llm-config-field-row has-thinking" class:has-level={workerConfigs[workerModelTab].provider === 'openai'}>
                <div class="llm-config-field">
                  <label class="llm-config-label">{i18n.t('settings.model.field.model')}</label>
                  <div class="model-combobox">
                    <input type="text" class="llm-config-input" bind:value={workerConfigs[workerModelTab].model}
                      onfocus={(e) => { if (modelLists[workerModelTab]?.length > 0) openModelDropdown(workerModelTab, e.currentTarget); }}
                    >
                    {#if !workerConfigs[workerModelTab].model}
                      <button
                        class="model-fetch-btn"
                        onclick={(event) => handleModelListAction('worker', event)}
                        disabled={fetchingModels[workerModelTab] || ((!modelLists[workerModelTab] || modelLists[workerModelTab].length === 0) && (!workerConfigs[workerModelTab].baseUrl || !workerConfigs[workerModelTab].apiKey))}
                        aria-label={(modelLists[workerModelTab]?.length ?? 0) > 0 ? i18n.t('settings.model.openModelList') : i18n.t('settings.model.fetchModelList')}
                        title={(modelLists[workerModelTab]?.length ?? 0) > 0 ? i18n.t('settings.model.openModelList') : i18n.t('settings.model.fetchModelList')}
                      >
                        {#if fetchingModels[workerModelTab]}
                          <Icon name="refresh" size={12} />
                        {:else if (modelLists[workerModelTab]?.length ?? 0) > 0}
                          <Icon name="chevron-down" size={12} />
                        {:else}
                          <Icon name="download" size={12} />
                        {/if}
                      </button>
                    {/if}
                    {#if modelDropdownOpen[workerModelTab] && modelLists[workerModelTab]?.length > 0}
                      <div class="model-dropdown" style="top: {dropdownPosition.top}px; left: {dropdownPosition.left}px; width: {dropdownPosition.width}px;">
                        {#each modelLists[workerModelTab] as m}
                          <button class="model-dropdown-item" class:selected={workerConfigs[workerModelTab].model === m} onclick={() => selectModel(workerModelTab, m)}>{m}</button>
                        {/each}
                      </div>
                    {/if}
                  </div>
                </div>
                <div class="llm-config-field">
                  <label class="llm-config-label">{i18n.t('settings.model.field.provider')}</label>
                  <select class="llm-config-select" bind:value={workerConfigs[workerModelTab].provider}>
                    <option value="openai">{i18n.t('settings.model.provider.openai')}</option>
                    <option value="anthropic">{i18n.t('settings.model.provider.anthropic')}</option>
                  </select>
                </div>
                {#if workerConfigs[workerModelTab].provider === 'openai'}
                <div class="llm-config-field">
                  <label class="llm-config-label">{i18n.t('settings.model.field.protocol')}</label>
                  <select class="llm-config-select" bind:value={workerConfigs[workerModelTab].openaiProtocol}>
                    <option value="responses">{i18n.t('settings.model.protocol.responses')}</option>
                    <option value="chat">{i18n.t('settings.model.protocol.chat')}</option>
                  </select>
                </div>
                <div class="llm-config-field">
                  <label class="llm-config-label">{i18n.t('settings.model.field.level')}</label>
                  <select class="llm-config-select" bind:value={workerConfigs[workerModelTab].reasoningEffort}>
                    <option value="low">{i18n.t('settings.model.reasoning.low')}</option>
                    <option value="medium">{i18n.t('settings.model.reasoning.medium')}</option>
                    <option value="high">{i18n.t('settings.model.reasoning.high')}</option>
                    <option value="xhigh">{i18n.t('settings.model.reasoning.xhigh')}</option>
                  </select>
                </div>
                {/if}
                <div class="llm-config-field inline-toggle" style="align-items: center; flex-direction: row; gap: 8px;">
                  <label class="llm-config-label" style="margin: 0;">{i18n.t('settings.model.field.thinking')}</label>
                  <Toggle
                    size="small"
                    checked={workerConfigs[workerModelTab].thinking}
                    title={workerConfigs[workerModelTab].thinking ? i18n.t('settings.model.disableThinking') : i18n.t('settings.model.enableThinking')}
                    onchange={() => {
                      workerConfigs[workerModelTab].thinking = !workerConfigs[workerModelTab].thinking;
                    }}
                  />
                </div>
              </div>
              <div class="apple-dashboard-bar" style="display: flex; justify-content: flex-end; align-items: center; margin-top: 24px;">
                <div class="settings-section-actions">
                  <button
                    class="apple-action-btn secondary"
                    class:testing={testStatus[workerModelTab] === 'testing'}
                    class:success={testStatus[workerModelTab] === 'success'}
                    class:error={testStatus[workerModelTab] === 'error'}
                    onclick={() => testModelConnection('worker')}
                    disabled={testStatus[workerModelTab] === 'testing'}
                  >
                    {#if testStatus[workerModelTab] === 'testing'}
                      <Icon name="refresh" size={14} />
                      {i18n.t('settings.model.testing')}
                    {:else if testStatus[workerModelTab] === 'success'}
                      <Icon name="check" size={14} />
                      {i18n.t('settings.model.testSuccess')}
                    {:else if testStatus[workerModelTab] === 'error'}
                      <Icon name="close" size={14} />
                      {i18n.t('settings.model.testFailed')}
                    {:else}
                      <Icon name="check" size={14} />
                      {i18n.t('settings.model.testConnection')}
                    {/if}
                  </button>
                  <button
                    class="apple-action-btn primary"
                    class:saving={saveStatus[workerModelTab] === 'saving'}
                    onclick={() => saveModelConfig('worker')}
                    disabled={saveStatus[workerModelTab] === 'saving'}
                  >
                    {#if saveStatus[workerModelTab] === 'saving'}
                      <Icon name="refresh" size={14} />
                      {i18n.t('settings.model.saving')}
                    {:else if saveStatus[workerModelTab] === 'saved'}
                      <Icon name="check" size={14} />
                      {i18n.t('settings.model.saved')}
                    {:else}
                      {i18n.t('settings.model.saveConfig')}
                    {/if}
                  </button>
                </div>
              </div>
            </div>
            {:else}
              <div class="llm-config-form" style="display:flex;align-items:center;justify-content:center;flex:1;color:var(--foreground-muted);">
                <div style="text-align:center;">
                  <Icon name="plus" size={24} />
                  <p style="margin-top:8px;">{i18n.t('settings.model.noWorkerConfig')}</p>
                </div>
              </div>
            {/if}
          </div>
        </div>
        </div>

<style>
  .llm-config-field-row { display: grid; grid-template-columns: 1fr auto; gap: var(--space-3); }
  .llm-config-field-row.has-thinking { grid-template-columns: 1fr auto 72px; }
  /* OpenAI 专属行：两个字段平分宽度 */
  .llm-config-field-row--openai { grid-template-columns: 1fr 1fr; }

  .worker-tabs-scroll-wrapper {
    position: relative;
    margin-bottom: var(--space-4);
    /* 左右渐变遮罩 */
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
  .worker-model-tab { display: flex; align-items: center; gap: var(--space-2); height: var(--btn-height-sm); padding: 0 var(--space-3); font-size: var(--text-sm); background: transparent; border: 1px solid var(--border); border-radius: var(--radius-sm); color: var(--foreground-muted); cursor: pointer; transition: all var(--transition-fast); }
  .worker-model-tab:hover { background: var(--surface-hover); color: var(--foreground); }
  .worker-model-tab.active { background: var(--primary); border-color: var(--primary); color: var(--primary-foreground); }
  .worker-model-tab--add { padding: 0 var(--space-2); min-width: var(--btn-height-sm); justify-content: center; border-style: dashed; }
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
  .worker-tab-status { font-size: var(--text-xs); opacity: 0.8; }
  .worker-tab-delete { display: inline-flex; align-items: center; justify-content: center; width: 16px; height: 16px; border-radius: var(--radius-full); font-size: 13px; line-height: 1; color: var(--foreground-muted); opacity: 0; transition: opacity var(--transition-fast), background var(--transition-fast); cursor: pointer; margin-left: 2px; }
  .worker-model-tab:hover .worker-tab-delete { opacity: 0.7; }
  .worker-tab-delete:hover { opacity: 1 !important; background: var(--danger-bg, rgba(220,38,38,0.15)); color: var(--danger, #dc2626); }
  .worker-model-tab.active .worker-tab-delete { color: var(--primary-foreground); }
  .worker-model-tab.active .worker-tab-delete:hover { background: rgba(255,255,255,0.25); color: var(--primary-foreground); }
  /* 模型配置表单已重用 settings-section 样式 */

  

  
  
  

  /* 模型配置顶部 Tab 切换 */
  .model-config-stack { display: flex; flex-direction: column; gap: var(--space-4); }
  .model-config-tabs { display: flex; gap: var(--space-1); border-bottom: 1px solid var(--border); margin-bottom: var(--space-4); }
  .model-config-tab {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    height: var(--btn-height-md);
    padding: 0 var(--space-4);
    font-size: var(--text-sm);
    background: transparent;
    border: none;
    border-bottom: 2px solid transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    transition: all var(--transition-fast);
  }
  .model-config-tab:hover { color: var(--foreground); }
  .model-config-tab.active { color: var(--primary); border-bottom-color: var(--primary); }
  .model-status-text { font-size: var(--text-xs); color: inherit; opacity: 0.8; }
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

  .llm-config-form { display: flex; flex-direction: column; gap: var(--space-3); }
  .llm-config-field { display: flex; flex-direction: column; gap: var(--space-2); }
  .llm-config-field-row { display: grid; grid-template-columns: 1fr 96px; gap: var(--space-3); }
  .llm-config-field-row.has-thinking { grid-template-columns: 1fr 96px 72px; }
  .llm-config-field-row.has-thinking.has-level { grid-template-columns: 1fr 96px 88px 88px 72px; }
  .llm-config-field-row.url-mode-row { grid-template-columns: minmax(0, 1fr) 180px; align-items: end; }
  .llm-config-field-row.worker-url-mode-row { grid-template-columns: minmax(0, 1fr) 180px 88px; align-items: end; }
  .llm-config-label { font-size: var(--text-sm); color: var(--foreground-muted); }
  .llm-config-field--compact { min-width: 0; }
  .llm-config-hint {
    margin-top: var(--space-2);
    font-size: var(--text-xs);
    line-height: 1.4;
    color: var(--foreground-muted);
  }

  .llm-config-input, .llm-config-select {
    height: var(--btn-height-md);
    padding: 0 var(--space-3);
    font-size: var(--text-sm);
    width: 100%;
    box-sizing: border-box;
  }

  .llm-config-input:focus, .llm-config-select:focus { border-color: var(--primary); }

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

  .api-key-wrapper { position: relative; }
  .api-key-wrapper .api-key-input { padding-right: 32px; }
  .api-key-toggle {
    position: absolute;
    right: 4px;
    top: 50%;
    transform: translateY(-50%);
    display: flex;
    align-items: center;
    justify-content: center;
    width: 24px;
    height: 24px;
    padding: 0;
    border: none;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    transition: all var(--transition-fast);
    opacity: 0.6;
  }
  .api-key-toggle:hover { background: var(--secondary); color: var(--foreground); opacity: 1; }

  .model-combobox { position: relative; }
  .model-combobox .llm-config-input { padding-right: 32px; }
  .model-fetch-btn {
    position: absolute;
    right: 4px;
    top: 50%;
    transform: translateY(-50%);
    display: flex;
    align-items: center;
    justify-content: center;
    width: 24px;
    height: 24px;
    padding: 0;
    border: none;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    transition: all var(--transition-fast);
  }
  .model-fetch-btn:hover:not(:disabled) { background: var(--secondary); color: var(--foreground); }
  .model-fetch-btn:disabled { opacity: 0.4; cursor: not-allowed; }
  .model-fetch-btn :global(svg) { animation: none; }
  .model-combobox:has(.model-fetch-btn:disabled) .model-fetch-btn :global(svg) { animation: none; }
  /* fetchingModels 状态下的旋转动画，由 Icon name="refresh" 触发 */

  .model-dropdown {
    position: fixed;
    z-index: 10000;
    max-height: 200px;
    overflow-y: auto;
    background: var(--vscode-input-background, var(--surface-2));
    border: 1px solid var(--border);
    border-top: none;
    border-radius: 0 0 var(--radius-sm) var(--radius-sm);
    box-shadow: 0 4px 12px rgba(0,0,0,0.3);
  }
  .model-dropdown-item {
    display: block;
    width: 100%;
    padding: 6px var(--space-3);
    font-size: var(--text-sm);
    text-align: left;
    border: none;
    background: transparent;
    color: var(--foreground);
    cursor: pointer;
    transition: background var(--transition-fast);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .model-dropdown-item:hover { background: var(--secondary); }
  .model-dropdown-item.selected { color: var(--primary); background: var(--primary-muted, rgba(var(--primary-rgb, 100,149,237), 0.1)); }

  
  
  
  
  @keyframes spin {
    from { transform: rotate(0deg); }
    to { transform: rotate(360deg); }
  }
  .llm-config-toggle-btn {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    height: var(--btn-height-md);
    padding: 0;
    background: transparent;
    border: none;
    color: var(--foreground);
    font-size: var(--text-sm);
    cursor: pointer;
    justify-content: flex-start;
    width: 100%;
  }
  .inline-toggle { display: flex; flex-direction: column; gap: var(--space-2); align-items: flex-start; }
  .toggle-switch { width: 32px; height: 18px; background: var(--secondary); border-radius: var(--radius-full); position: relative; transition: background var(--transition-fast); cursor: pointer; flex-shrink: 0; }
  .toggle-switch::after { content: ''; position: absolute; top: 2px; left: 2px; width: 14px; height: 14px; background: var(--primary-foreground); border-radius: var(--radius-full); transition: transform var(--transition-fast); }
  .toggle-switch.active { background: var(--primary); }
  .toggle-switch.active::after { transform: translateX(14px); }



  @media (max-width: 768px) {
    .llm-config-field-row,
    .llm-config-field-row.has-thinking,
    .llm-config-field-row.has-thinking.has-level,
    .llm-config-field-row.url-mode-row,
    .llm-config-field-row.worker-url-mode-row {
      grid-template-columns: 1fr;
    }
  }

</style>
