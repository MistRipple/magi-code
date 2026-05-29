<script lang="ts">
  import { i18n } from '../stores/i18n.svelte';
  import { resolveModelApiProtocol } from '../shared/model-governance';
  import Icon from './Icon.svelte';

  type FormType = 'orch' | 'comp' | 'worker';

  let {
    formType,
    statusKey,
    config = $bindable(),
    keyVisible = $bindable(),
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
    keyVisible: Record<string, boolean>;
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
    fetchModelList: (type: FormType) => void;
    selectModel: (type: string, model: string) => void;
    saveModelConfig: (type: FormType) => void;
    testModelConnection: (type: FormType) => void;
  }>();

  const keyVisibleKey = $derived(formType);

  // --- 脏态检测 ---
  // 父组件会异步重新赋值 config（首次加载 / 切换 provider 时），proxy 引用整体替换，
  // 单纯比较快照无法分辨「外部数据装入」与「用户编辑」。因此用 userHasEdited 作为闸门：
  // 仅在表单收到 input 事件（来自用户键入或切换）后才进入脏态比对，避免冷启动误报。
  function snapshot(value: any): string {
    try {
      return JSON.stringify($state.snapshot(value));
    } catch {
      return '';
    }
  }

  let baseline = $state(snapshot(config));
  let userHasEdited = $state(false);

  $effect(() => {
    // 用户未编辑前，保持 baseline 与外部数据同步，避免父组件异步装入时误判脏态
    if (!userHasEdited) {
      baseline = snapshot(config);
    }
  });

  $effect(() => {
    if (saveStatus[statusKey] === 'saved') {
      baseline = snapshot(config);
      userHasEdited = false;
    }
  });

  const isDirty = $derived(userHasEdited && snapshot(config) !== baseline);

  function markUserEdited() {
    userHasEdited = true;
  }

  // --- 下拉外部点击关闭 ---
  // 模型下拉用 position: fixed 渲染到 .model-combobox 之外的 stacking context，
  // 这里用两个 ref 锁定边界：只有点击落在 combobox 或 dropdown 内才视为内部交互，
  // 其余 pointerdown 一律关闭本表单的下拉。
  let comboboxEl: HTMLDivElement | undefined = $state();
  let dropdownEl: HTMLDivElement | undefined = $state();

  $effect(() => {
    if (!modelDropdownOpen[statusKey]) return;
    function handlePointerDown(event: PointerEvent) {
      const target = event.target as Node | null;
      if (!target) return;
      if (comboboxEl?.contains(target)) return;
      if (dropdownEl?.contains(target)) return;
      closeModelDropdown(statusKey);
    }
    window.addEventListener('pointerdown', handlePointerDown, true);
    return () => window.removeEventListener('pointerdown', handlePointerDown, true);
  });

  const currentSaveStatus = $derived(saveStatus[statusKey]);
  const currentTestStatus = $derived(testStatus[statusKey]);
  const isSaving = $derived(currentSaveStatus === 'saving');
  const isTesting = $derived(currentTestStatus === 'testing');
  const saveDisabled = $derived(isSaving || !isDirty);
  const showSavedLabel = $derived(currentSaveStatus === 'saved' && !isDirty);
  const resolvedProtocol = $derived(resolveModelApiProtocol(config));
  const showResolvedProtocol = $derived(
    Boolean(String(config.model || '').trim()) || config.urlMode === 'full',
  );

  function handleModelListAction(event: MouseEvent) {
    const button = event.currentTarget as HTMLElement | null;
    const input = button?.parentElement?.querySelector('input') ?? null;
    const hasModels = Array.isArray(modelLists[statusKey]) && modelLists[statusKey].length > 0;
    if (input) {
      openModelDropdown(statusKey, input);
    }
    if (!hasModels) {
      void fetchModelList(formType);
    }
  }

  function modelListActionTitle(): string {
    if (Array.isArray(modelLists[statusKey]) && modelLists[statusKey].length > 0) {
      return i18n.t('settings.model.openModelList');
    }
    return i18n.t('settings.model.fetchModelList');
  }
</script>

<!-- svelte-ignore a11y_label_has_associated_control -->
<div class="llm-config-form" oninput={markUserEdited} onchange={markUserEdited}>
  <div
    class="llm-config-field-row url-mode-row"
  >
    <div class="llm-config-field">
      <label class="llm-config-label">{i18n.t('settings.model.field.baseUrl')}</label>
      <input
        type="text"
        class="llm-config-input"
        bind:value={config.baseUrl}
        placeholder={getBaseUrlPlaceholder()}
      />
    </div>
    <div class="llm-config-field llm-config-field--compact">
      <label class="llm-config-label">{i18n.t('settings.model.field.urlMode')}</label>
      <div class="segmented-control">
        <button
          type="button"
          class="segmented-control__option"
          class:active={config.urlMode === 'standard'}
          onclick={() => { config.urlMode = 'standard'; markUserEdited(); }}
        >
          {i18n.t('settings.model.urlMode.standard')}
        </button>
        <button
          type="button"
          class="segmented-control__option"
          class:active={config.urlMode === 'full'}
          onclick={() => { config.urlMode = 'full'; markUserEdited(); }}
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

  <div
    class="llm-config-field-row credentials-row"
    class:has-level={showAdvancedOptions}
  >
    <div class="llm-config-field">
      <label class="llm-config-label">{i18n.t('settings.model.field.apiKey')}</label>
      <div class="api-key-wrapper">
        <input
          type={keyVisible[keyVisibleKey] ? 'text' : 'password'}
          class="llm-config-input api-key-input"
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

    <div class="llm-config-field">
      <div class="llm-config-label-row">
        <label class="llm-config-label">{i18n.t('settings.model.field.model')}</label>
        {#if showResolvedProtocol}
          <span
            class="model-protocol-chip"
            title={i18n.t('settings.model.protocolHint')}
          >
            {i18n.t('settings.model.protocolResolved', {
              protocol: resolvedProtocol === 'anthropic_messages'
                ? i18n.t('settings.model.protocol.anthropic')
                : i18n.t('settings.model.protocol.openai'),
            })}
          </span>
        {/if}
      </div>
      <div class="model-combobox" bind:this={comboboxEl}>
        <input
          type="text"
          class="llm-config-input"
          bind:value={config.model}
          onfocus={(e) => {
            if ((modelLists[statusKey]?.length ?? 0) > 0) openModelDropdown(statusKey, e.currentTarget);
          }}
        />
        <button
          class="model-fetch-btn"
          onclick={handleModelListAction}
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
            class="model-dropdown"
            style="top: {dropdownPosition.top}px; left: {dropdownPosition.left}px; width: {dropdownPosition.width}px;"
          >
            {#each modelLists[statusKey] as m}
              <button
                class="model-dropdown-item"
                class:selected={config.model === m}
                onclick={() => { selectModel(statusKey, m); markUserEdited(); }}
              >
                {m}
              </button>
            {/each}
          </div>
        {/if}
      </div>
    </div>

    {#if showAdvancedOptions}
      <div class="llm-config-field">
        <label class="llm-config-label">{i18n.t('settings.model.field.level')}</label>
        <select class="llm-config-select" bind:value={config.reasoningEffort}>
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
        class="apple-action-btn secondary"
        class:testing={currentTestStatus === 'testing'}
        class:success={currentTestStatus === 'success'}
        class:error={currentTestStatus === 'error'}
        onclick={() => testModelConnection(formType)}
        disabled={isTesting}
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
        class="apple-action-btn primary"
        class:saving={isSaving}
        onclick={() => saveModelConfig(formType)}
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
  .llm-config-field-row.url-mode-row {
    grid-template-columns: minmax(0, 1fr) 180px;
    align-items: end;
  }

  .llm-config-label {
    font-size: var(--text-sm);
    color: var(--foreground-muted);
  }

  .llm-config-label-row {
    min-width: 0;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
  }

  .model-protocol-chip {
    min-width: 0;
    max-width: 160px;
    padding: 1px 7px;
    border: 1px solid var(--border);
    border-radius: var(--radius-full);
    color: var(--foreground-muted);
    background: var(--surface-2);
    font-size: var(--text-xs);
    line-height: 1.5;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
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

  .llm-config-input,
  .llm-config-select {
    height: var(--btn-height-md);
    padding: 0 var(--space-3);
    font-size: var(--text-sm);
    width: 100%;
    box-sizing: border-box;
  }

  .llm-config-input:focus,
  .llm-config-select:focus {
    border-color: var(--primary);
  }

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

  .api-key-wrapper {
    position: relative;
  }
  .api-key-wrapper .api-key-input {
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
  .model-combobox .llm-config-input {
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

  .model-dropdown {
    position: fixed;
    z-index: var(--z-popover);
    max-height: 200px;
    overflow-y: auto;
    background: var(--vscode-input-background, var(--surface-2));
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
    .model-form-actions :global(.apple-action-btn) {
      width: 100%;
      justify-content: center;
    }
    .model-form-actions--buttons-only {
      align-items: stretch;
    }
    .llm-config-field-row,
    .llm-config-field-row.credentials-row,
    .llm-config-field-row.credentials-row.has-level,
    .llm-config-field-row.url-mode-row {
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
    .model-form-actions :global(.apple-action-btn) {
      width: 100%;
      justify-content: center;
    }
    .model-form-actions--buttons-only {
      align-items: stretch;
    }
    .llm-config-field-row,
    .llm-config-field-row.credentials-row,
    .llm-config-field-row.credentials-row.has-level,
    .llm-config-field-row.url-mode-row {
      grid-template-columns: 1fr;
    }
  }
</style>
