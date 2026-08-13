<script lang="ts">
  import { i18n } from '../stores/i18n.svelte';
  import Icon from './Icon.svelte';

  type FormType = 'orch' | 'comp' | 'vision' | 'image' | 'worker';

  let {
    formType,
    statusKey,
    config = $bindable(),
    baselineConfig,
    keyVisible = $bindable(),
    showModelField = true,
    showAdvancedOptions = true,
    description = null,
    saveStatus,
    testStatus,
    fetchingModels,
    modelDropdownOpen,
    dropdownPosition,
    modelLists,
    getBaseUrlPlaceholder,
    shouldRecommendStandardUrlMode,
    openModelDropdown,
    closeModelDropdown,
    fetchModelList,
    selectModel,
    saveModelConfig,
    testModelConnection
  } = $props<{
    formType: FormType;
    statusKey: string;
    config: any;
    baselineConfig: any;
    keyVisible: Record<string, boolean>;
    showModelField?: boolean;
    showAdvancedOptions?: boolean;
    description?: string | null;
    saveStatus: Record<string, string>;
    testStatus: Record<string, string>;
    fetchingModels: Record<string, boolean>;
    modelDropdownOpen: Record<string, boolean>;
    dropdownPosition: any;
    modelLists: Record<string, string[]>;
    getBaseUrlPlaceholder: () => string;
    shouldRecommendStandardUrlMode: (baseUrl: string) => boolean;
    openModelDropdown: (type: string, target: HTMLElement) => void;
    closeModelDropdown: (key: string) => void;
    fetchModelList: (type: FormType, statusKey: string) => void;
    selectModel: (type: string, model: string) => void;
    saveModelConfig: (type: FormType, statusKey: string) => void;
    testModelConnection: (type: FormType, statusKey: string) => void;
  }>();

  const keyVisibleKey = $derived(formType);

  function editableConfigSnapshot(value: any): string {
    const normalized = {
      baseUrl: String(value?.baseUrl ?? ''),
      urlMode: value?.urlMode === 'full' ? 'full' : 'standard',
      ...(formType !== 'image'
        ? { apiProtocol: String(value?.apiProtocol ?? 'openai_chat') }
        : {}),
      apiKey: String(value?.apiKey ?? ''),
      ...(showModelField ? { model: String(value?.model ?? '') } : {}),
      ...(showAdvancedOptions
        ? { reasoningEffort: String(value?.reasoningEffort ?? 'medium') }
        : {})
    };
    return JSON.stringify(normalized);
  }

  const isDirty = $derived(editableConfigSnapshot(config) !== editableConfigSnapshot(baselineConfig));

  // --- 下拉外部点击关闭 ---
  // 模型下拉用 position: fixed 渲染到 .model-combobox 之外的 stacking context，
  // 这里用两个 ref 锁定边界：只有点击落在 combobox 或 dropdown 内才视为内部交互，
  // 其余 pointerdown 一律关闭本表单的下拉。
  let comboboxEl: HTMLDivElement | undefined = $state();
  let dropdownEl: HTMLDivElement | undefined = $state();

  function portalToBody(node: HTMLElement) {
    document.body.appendChild(node);
    return {
      destroy() {
        node.remove();
      }
    };
  }

  $effect(() => {
    if (!modelDropdownOpen[statusKey]) return;
    function handlePointerDown(event: PointerEvent) {
      const target = event.target as Node | null;
      if (!target) return;
      if (comboboxEl?.contains(target)) return;
      if (dropdownEl?.contains(target)) return;
      closeModelDropdown(statusKey);
    }
    function handleScroll(event: Event) {
      const target = event.target;
      if (target instanceof Node && (comboboxEl?.contains(target) || dropdownEl?.contains(target))) {
        return;
      }
      closeModelDropdown(statusKey);
    }
    window.addEventListener('pointerdown', handlePointerDown, true);
    window.addEventListener('scroll', handleScroll, true);
    return () => {
      window.removeEventListener('pointerdown', handlePointerDown, true);
      window.removeEventListener('scroll', handleScroll, true);
    };
  });

  const currentSaveStatus = $derived(saveStatus[statusKey]);
  const currentTestStatus = $derived(testStatus[statusKey]);
  const isSaving = $derived(currentSaveStatus === 'saving');
  const isTesting = $derived(currentTestStatus === 'testing');
  const saveDisabled = $derived(isSaving || !isDirty);
  const showSavedLabel = $derived(currentSaveStatus === 'saved' && !isDirty);
  const showProtocolField = $derived(formType !== 'image');

  function handleModelListAction(event: MouseEvent) {
    const button = event.currentTarget as HTMLElement | null;
    const input = button?.parentElement?.querySelector('input') ?? null;
    const hasModels = Array.isArray(modelLists[statusKey]) && modelLists[statusKey].length > 0;
    if (input) {
      openModelDropdown(statusKey, input);
    }
    if (!hasModels) {
      void fetchModelList(formType, statusKey);
    }
  }

  function modelListActionTitle(): string {
    if (Array.isArray(modelLists[statusKey]) && modelLists[statusKey].length > 0) {
      return i18n.t('settings.model.openModelList');
    }
    return i18n.t('settings.model.fetchModelList');
  }

  function protocolEndpointPath(protocol: string): string {
    if (protocol === 'openai_responses') return '/v1/responses';
    if (protocol === 'anthropic_messages') return '/v1/messages';
    return '/v1/chat/completions';
  }

  function protocolEndpointLabel(): string {
    const baseUrl = String(config.baseUrl ?? '').trim();
    if (config.urlMode === 'full') {
      return baseUrl || i18n.t('settings.model.protocol.fullEndpoint');
    }
    return protocolEndpointPath(config.apiProtocol);
  }

  function protocolBehaviorKey(protocol: string): string {
    if (protocol === 'openai_responses') return 'settings.model.protocol.openaiResponsesBehavior';
    if (protocol === 'anthropic_messages') return 'settings.model.protocol.anthropicMessagesBehavior';
    return 'settings.model.protocol.openaiChatBehavior';
  }
</script>

<!-- svelte-ignore a11y_label_has_associated_control -->
<div class="llm-config-form">
  <div class="llm-config-field-row url-mode-row">
    <div class="llm-config-field">
      <label class="form-label">{i18n.t('settings.model.field.baseUrl')}</label>
      <input
        type="text"
        class="form-input"
        bind:value={config.baseUrl}
        placeholder={getBaseUrlPlaceholder()}
      />
    </div>
    <div class="llm-config-field llm-config-field--compact">
      <label class="form-label">{i18n.t('settings.model.field.urlMode')}</label>
      <div class="ui-segmented url-mode-switch">
        <button
          type="button"
          class="ui-segmented__option"
          class:active={config.urlMode === 'standard'}
          onclick={() => { config.urlMode = 'standard'; }}
        >
          {i18n.t('settings.model.urlMode.standard')}
        </button>
        <button
          type="button"
          class="ui-segmented__option"
          class:active={config.urlMode === 'full'}
          onclick={() => { config.urlMode = 'full'; }}
        >
          {i18n.t('settings.model.urlMode.full')}
        </button>
      </div>
      {#if shouldRecommendStandardUrlMode(config.baseUrl)}
        <div class="llm-config-hint">
          {i18n.t('settings.model.urlMode.standardRecommended')}
        </div>
      {/if}
    </div>
  </div>

  {#if showProtocolField}
    <div class="llm-config-field protocol-field">
      <label class="form-label">{i18n.t('settings.model.field.apiProtocol')}</label>
      <div class="protocol-control-row">
        <select
          class="form-input protocol-select"
          bind:value={config.apiProtocol}
          aria-label={i18n.t('settings.model.field.apiProtocol')}
          title={i18n.t('settings.model.protocolHint')}
        >
          <option value="openai_responses">{i18n.t('settings.model.protocol.openaiResponses')}</option>
          <option value="openai_chat">{i18n.t('settings.model.protocol.openaiChat')}</option>
          <option value="anthropic_messages">{i18n.t('settings.model.protocol.anthropicMessages')}</option>
        </select>
        <div
          class="protocol-endpoint"
          aria-live="polite"
          title={`POST ${protocolEndpointLabel()}`}
        >
          <span class="protocol-endpoint__label">{i18n.t('settings.model.protocol.endpoint')}</span>
          <code>POST {protocolEndpointLabel()}</code>
        </div>
      </div>
      <div class="protocol-behavior" role="note">
        {i18n.t(protocolBehaviorKey(config.apiProtocol))}
      </div>
    </div>
  {/if}

  <div
    class="llm-config-field-row credentials-row"
    class:has-level={showAdvancedOptions}
    class:key-only={!showModelField && !showAdvancedOptions}
  >
    <div class="llm-config-field">
      <label class="form-label">{i18n.t('settings.model.field.apiKey')}</label>
      <div class="api-key-wrapper">
        <input
          type={keyVisible[keyVisibleKey] ? 'text' : 'password'}
          class="form-input api-key-input"
          bind:value={config.apiKey}
          placeholder="sk-ant-..."
        />
        <button
          type="button"
          class="api-key-toggle"
          onclick={() => (keyVisible[keyVisibleKey] = !keyVisible[keyVisibleKey])}
          title={keyVisible[keyVisibleKey] ? i18n.t('input.hideKey') : i18n.t('input.showKey')}
        >
          <Icon name={keyVisible[keyVisibleKey] ? 'eye-slash' : 'eye'} size={14} />
        </button>
      </div>
    </div>

    {#if showModelField}
      <div class="llm-config-field">
        <label class="form-label">{i18n.t('settings.model.field.model')}</label>
        <div class="model-combobox" bind:this={comboboxEl}>
          <input
            type="text"
            class="form-input"
            bind:value={config.model}
            onfocus={(e) => {
              if ((modelLists[statusKey]?.length ?? 0) > 0) openModelDropdown(statusKey, e.currentTarget);
            }}
          />
          <button
            class="model-fetch-btn"
            type="button"
            onclick={handleModelListAction}
            disabled={fetchingModels[statusKey]}
            aria-label={modelListActionTitle()}
            title={modelListActionTitle()}
          >
            {#if fetchingModels[statusKey]}
              <Icon name="refresh" size={12} />
            {:else if (modelLists[statusKey]?.length ?? 0) > 0}
              <Icon name="chevron-down" size={12} />
            {:else}
              <Icon name="download" size={12} />
            {/if}
          </button>
          {#if modelDropdownOpen[statusKey] && (modelLists[statusKey]?.length ?? 0) > 0}
            <div
              bind:this={dropdownEl}
              use:portalToBody
              class="model-dropdown"
              data-magi-surface="popover"
              style="top: {dropdownPosition.top}px; left: {dropdownPosition.left}px; width: {dropdownPosition.width}px;"
            >
              {#each modelLists[statusKey] as m}
                <button
                  class="model-dropdown-item"
                  class:selected={config.model === m}
                  onclick={() => { selectModel(statusKey, m); }}
                >
                  {m}
                </button>
              {/each}
            </div>
          {/if}
        </div>
      </div>
    {/if}

    {#if showAdvancedOptions}
      <div class="llm-config-field">
        <label class="form-label">{i18n.t('settings.model.field.level')}</label>
        <select class="form-input" bind:value={config.reasoningEffort}>
          <option value="low">{i18n.t('settings.model.reasoning.low')}</option>
          <option value="medium">{i18n.t('settings.model.reasoning.medium')}</option>
          <option value="high">{i18n.t('settings.model.reasoning.high')}</option>
          <option value="xhigh">{i18n.t('settings.model.reasoning.xhigh')}</option>
        </select>
      </div>
    {/if}
  </div>

  <div
    class="apple-dashboard-bar model-form-actions"
    class:model-form-actions--buttons-only={!description}
  >
    {#if description}
      <span class="model-form-action-desc">
        {description}
        {#if isDirty}
          <span class="model-form-dirty-tag" title={i18n.t('settings.model.unsavedChanges')}>
            {i18n.t('settings.model.unsaved')}
          </span>
        {/if}
      </span>
    {:else if isDirty}
      <span class="model-form-action-desc">
        <span class="model-form-dirty-tag" title={i18n.t('settings.model.unsavedChanges')}>
          {i18n.t('settings.model.unsaved')}
        </span>
      </span>
    {/if}
    <div class="settings-section-actions">
      <button
        class="btn btn--secondary btn--sm"
        class:is-testing={currentTestStatus === 'testing'}
        class:is-success={currentTestStatus === 'success'}
        class:is-error={currentTestStatus === 'error'}
        onclick={() => testModelConnection(formType, statusKey)}
        disabled={isTesting || isSaving}
      >
        {#if currentTestStatus === 'testing'}
          <Icon name="refresh" size={14} />
          {i18n.t('settings.model.testing')}
        {:else if currentTestStatus === 'success'}
          <Icon name="check" size={14} />
          {i18n.t('settings.model.testSuccess')}
        {:else if currentTestStatus === 'error'}
          <Icon name="close" size={14} />
          {i18n.t('settings.model.testFailed')}
        {:else}
          <Icon name="check" size={14} />
          {i18n.t('settings.model.testConnection')}
        {/if}
      </button>
      <button
        class="btn btn--primary btn--sm"
        class:is-saving={isSaving}
        onclick={() => saveModelConfig(formType, statusKey)}
        disabled={saveDisabled}
      >
        {#if isSaving}
          <Icon name="refresh" size={14} />
          {i18n.t('settings.model.saving')}
        {:else if showSavedLabel}
          <Icon name="check" size={14} />
          {i18n.t('settings.model.saved')}
        {:else}
          {i18n.t('settings.model.saveConfig')}
        {/if}
      </button>
    </div>
  </div>
</div>

<style>
  .llm-config-form {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }

  .llm-config-field {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  .llm-config-field-row {
    display: grid;
    grid-template-columns: 1fr;
    gap: var(--space-3);
  }
  .llm-config-field-row.credentials-row {
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
  }
  .llm-config-field-row.credentials-row.has-level {
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr) 96px;
  }
  .llm-config-field-row.credentials-row.key-only {
    grid-template-columns: minmax(0, 1fr);
  }
  .llm-config-field-row.url-mode-row {
    grid-template-columns: minmax(0, 1fr) 180px;
    align-items: end;
  }

  .llm-config-field--compact {
    min-width: 0;
  }

  .llm-config-hint {
    margin-top: var(--space-2);
    font-size: var(--text-xs);
    line-height: 1.4;
    color: var(--foreground-muted);
  }

  .protocol-field {
    gap: var(--space-2);
  }

  .protocol-control-row {
    display: grid;
    grid-template-columns: minmax(230px, 300px) minmax(0, 1fr);
    gap: var(--space-3);
    align-items: center;
  }

  .protocol-select {
    min-width: 0;
  }

  .protocol-endpoint {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
    min-height: var(--btn-height-md);
    padding: 0 var(--space-3);
    border: 1px solid color-mix(in srgb, var(--border) 78%, transparent);
    border-radius: var(--radius-sm);
    background: color-mix(in srgb, var(--surface-2) 78%, transparent);
  }

  .protocol-endpoint__label {
    flex: 0 0 auto;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
  }

  .protocol-endpoint code {
    min-width: 0;
    overflow: hidden;
    color: var(--foreground);
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .protocol-behavior {
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    line-height: 1.45;
  }

  .url-mode-switch { min-width: 0; }

  .api-key-wrapper {
    position: relative;
  }
  .api-key-wrapper .form-input {
    padding-right: 32px;
  }
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
  .api-key-toggle:hover {
    background: var(--secondary);
    color: var(--foreground);
    opacity: 1;
  }

  .model-combobox {
    position: relative;
  }
  .model-combobox .form-input {
    padding-right: 32px;
  }
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
  .model-fetch-btn:hover {
    background: var(--secondary);
    color: var(--foreground);
  }
  .model-fetch-btn:disabled {
    cursor: wait;
    opacity: 0.6;
  }

  .model-dropdown {
    position: fixed;
    z-index: var(--z-popover);
    max-height: 200px;
    overflow-y: auto;
    border: 1px solid var(--border);
    border-top: none;
    border-radius: 0 0 var(--radius-sm) var(--radius-sm);
    box-shadow: 0 4px 12px rgba(0, 0, 0, 0.3);
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
  .model-dropdown-item:hover {
    background: var(--secondary);
  }
  .model-dropdown-item.selected {
    color: var(--primary);
    background: var(--primary-muted, rgba(var(--primary-rgb, 100, 149, 237), 0.1));
  }

  .model-form-actions {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: var(--space-3);
    margin-top: 24px;
  }

  .model-form-actions--buttons-only {
    justify-content: flex-end;
  }

  .model-form-actions :global(.settings-section-actions) {
    flex: 0 0 auto;
  }

  .model-form-action-desc {
    min-width: 0;
    font-size: 12px;
    line-height: 1.5;
    color: var(--foreground-muted);
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    flex-wrap: wrap;
  }

  .model-form-dirty-tag {
    display: inline-flex;
    align-items: center;
    padding: 1px 8px;
    font-size: var(--text-xs);
    font-weight: var(--font-medium);
    line-height: 1.5;
    color: var(--warning, #d97706);
    background: var(--warning-bg, rgba(217, 119, 6, 0.12));
    border: 1px solid var(--warning-border, rgba(217, 119, 6, 0.3));
    border-radius: var(--radius-full);
  }

  @container settings-model (max-width: 640px) {
    .model-form-actions {
      flex-direction: column;
      align-items: stretch;
      gap: var(--space-3);
      margin-top: var(--space-4);
    }
    .model-form-actions :global(.settings-section-actions) {
      width: 100%;
      display: grid;
      grid-template-columns: repeat(2, minmax(0, 1fr));
      gap: var(--space-2);
    }
    .model-form-actions :global(.btn) {
      width: 100%;
      justify-content: center;
    }
    .model-form-actions--buttons-only {
      align-items: stretch;
    }
    .llm-config-field-row,
    .llm-config-field-row.credentials-row,
    .llm-config-field-row.credentials-row.has-level,
    .llm-config-field-row.credentials-row.key-only,
    .llm-config-field-row.url-mode-row {
      grid-template-columns: 1fr;
    }
    .protocol-control-row {
      grid-template-columns: 1fr;
    }
  }

  @media (max-width: 768px) {
    .model-form-actions {
      flex-direction: column;
      align-items: stretch;
      gap: var(--space-3);
      margin-top: var(--space-4);
    }
    .model-form-actions :global(.settings-section-actions) {
      width: 100%;
      display: grid;
      grid-template-columns: repeat(2, minmax(0, 1fr));
      gap: var(--space-2);
    }
    .model-form-actions :global(.btn) {
      width: 100%;
      justify-content: center;
    }
    .model-form-actions--buttons-only {
      align-items: stretch;
    }
    .llm-config-field-row,
    .llm-config-field-row.credentials-row,
    .llm-config-field-row.credentials-row.has-level,
    .llm-config-field-row.credentials-row.key-only,
    .llm-config-field-row.url-mode-row {
      grid-template-columns: 1fr;
    }
    .protocol-control-row {
      grid-template-columns: 1fr;
    }
  }
</style>
