<script lang="ts">
  import SelectField from './SelectField.svelte';
  import { resolveAgentModelReasoning } from '../web/agent-api';
  import type { ModelReasoningCapability } from '../shared/rust-backend-types';
  import { i18n } from '../stores/i18n.svelte';
  import type { VisionBuiltinTextModelRule } from '../shared/settings-bootstrap';
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
    visionBuiltinTextModelRules = [],
    saveStatus,
    testStatus,
    fetchingModels,
    modelListOpen,
    modelListPosition,
    modelLists,
    modelDisplayNames,
    getBaseUrlPlaceholder,
    openModelList,
    closeModelList,
    fetchModelList,
    selectModel,
    saveModelConfig,
    testModelConnection
  }: {
    formType: FormType;
    statusKey: string;
    config: any;
    baselineConfig: any;
    keyVisible: Record<string, boolean>;
    showModelField?: boolean;
    showAdvancedOptions?: boolean;
    description?: string | null;
    visionBuiltinTextModelRules?: VisionBuiltinTextModelRule[];
    saveStatus: Record<string, string>;
    testStatus: Record<string, string>;
    fetchingModels: Record<string, boolean>;
    modelListOpen: Record<string, boolean>;
    modelListPosition: any;
    modelLists: Record<string, string[]>;
    modelDisplayNames: Record<string, Record<string, string>>;
    getBaseUrlPlaceholder: () => string;
    openModelList: (type: string, target: HTMLElement) => void;
    closeModelList: (key: string) => void;
    fetchModelList: (type: FormType, statusKey: string) => void;
    selectModel: (type: string, model: string) => void;
    saveModelConfig: (type: FormType, statusKey: string) => void;
    testModelConnection: (type: FormType, statusKey: string) => void;
  } = $props();

  let reasoningCapability = $state<ModelReasoningCapability | null>(null);
  let reasoningStatus = $state<'loading' | 'ready' | 'failed'>('loading');
  let reasoningRetry = $state(0);
  let reasoningAdjusted = $state(false);
  $effect(() => {
    void modelLists[statusKey];
    void reasoningRetry;
    reasoningCapability = null;
    reasoningStatus = 'loading';
    reasoningAdjusted = false;
    if (fetchingModels[statusKey]) return;
    const raw = { baseUrl: config?.baseUrl, apiKey: config?.apiKey, apiProtocol: config?.apiProtocol };
    const model = String(config?.model ?? '');
    let cancelled = false;
    if (formType !== 'image' && model) void resolveAgentModelReasoning(raw, model).then(value => {
      if (!cancelled) {
        reasoningCapability = value;
        reasoningStatus = 'ready';
        if (value.source !== 'unknown' && config.reasoningEffort != null && !value.supportedEfforts.includes(config.reasoningEffort)) {
          config.reasoningEffort = null;
          reasoningAdjusted = true;
        }
      }
    }).catch(() => { if (!cancelled) reasoningStatus = 'failed'; });
    return () => { cancelled = true; };
  });
  const keyVisibleKey = $derived(formType);
  // 同一时刻只渲染一个表单，但 statusKey 仍按引擎区分，避免切换引擎时 label 指向旧控件。
  const fieldId = (name: string) => `model-config-${statusKey}-${name}`;

  function editableConfigSnapshot(value: any): string {
    const normalized = {
      baseUrl: String(value?.baseUrl ?? ''),
      ...(formType !== 'image'
        ? { apiProtocol: String(value?.apiProtocol ?? 'openai_responses') }
        : {}),
      apiKey: String(value?.apiKey ?? ''),
      ...(showModelField ? { model: String(value?.model ?? '') } : {}),
      ...(showAdvancedOptions
        ? { reasoningEffort: value?.reasoningEffort ?? null }
        : {}),
      ...(formType === 'vision'
        ? {
            contextWindowTokens: Math.floor(Number(value?.contextWindowTokens) || 128000),
            textModelRules: Array.isArray(value?.textModelRules)
              ? value.textModelRules.map((rule: any) => ({
                  matchMode: rule?.matchMode === 'regex' ? 'regex' : 'exact',
                  pattern: String(rule?.pattern ?? ''),
                }))
              : [],
          }
        : {}),
    };
    return JSON.stringify(normalized);
  }

  function escapeRegexDelimiter(pattern: string): string {
    return pattern.replaceAll('/', '\\/');
  }

  function unescapeRegexDelimiter(pattern: string): string {
    return pattern.replaceAll('\\/', '/');
  }

  function escapeExactRule(pattern: string): string {
    return pattern.replaceAll('\\', '\\\\').replaceAll(',', '\\,').replaceAll('，', '\\，');
  }

  function unescapeExactRule(pattern: string): string {
    let result = '';
    for (let index = 0; index < pattern.length; index += 1) {
      const char = pattern[index];
      const next = pattern[index + 1];
      if (char === '\\' && (next === '\\' || next === ',' || next === '，')) {
        result += next;
        index += 1;
      } else {
        result += char;
      }
    }
    return result;
  }

  function serializeVisionRules(rules: any[]): string {
    return (Array.isArray(rules) ? rules : [])
      .map((rule) => {
        const pattern = String(rule?.pattern ?? '').trim();
        if (!pattern) return '';
        return rule?.matchMode === 'regex'
          ? `/${escapeRegexDelimiter(pattern)}/`
          : escapeExactRule(pattern);
      })
      .filter(Boolean)
      .join(', ');
  }

  function analyzeVisionRulesText(value: string): {
    rules: Array<{ matchMode: 'exact' | 'regex'; pattern: string }>;
    syntaxInvalid: boolean;
    regexInvalid: boolean;
  } {
    const entries: string[] = [];
    let current = '';
    let inRegex = false;
    let escaped = false;
    for (const char of value) {
      if (escaped) {
        current += char;
        escaped = false;
        continue;
      }
      if (char === '\\') {
        current += char;
        escaped = true;
        continue;
      }
      if (char === '/' && (inRegex || current.trim().length === 0)) {
        inRegex = !inRegex;
        current += char;
        continue;
      }
      if (!inRegex && (char === ',' || char === '，' || char === '\n')) {
        if (current.trim()) entries.push(current.trim());
        current = '';
        continue;
      }
      current += char;
    }
    if (current.trim()) entries.push(current.trim());

    let syntaxInvalid = inRegex;
    let regexInvalid = false;
    const rules: Array<{ matchMode: 'exact' | 'regex'; pattern: string }> = [];
    for (const entry of entries) {
      if (entry.startsWith('/')) {
        if (entry.length < 2 || !entry.endsWith('/')) {
          syntaxInvalid = true;
          continue;
        }
        const pattern = unescapeRegexDelimiter(entry.slice(1, -1)).trim();
        if (!pattern) {
          syntaxInvalid = true;
          continue;
        }
        try {
          new RegExp(pattern);
        } catch {
          regexInvalid = true;
          continue;
        }
        rules.push({ matchMode: 'regex', pattern });
        continue;
      }
      const pattern = unescapeExactRule(entry).trim();
      if (pattern) rules.push({ matchMode: 'exact', pattern });
    }
    return { rules, syntaxInvalid, regexInvalid };
  }

  let visionRulesText = $state('');
  let syncedVisionRulesSignature = '';
  const visionRuleAnalysis = $derived(analyzeVisionRulesText(visionRulesText));
  const visionRulesTextDirty = $derived(
    formType === 'vision'
    && (visionRuleAnalysis.syntaxInvalid || visionRuleAnalysis.regexInvalid)
    && visionRulesText.trim() !== serializeVisionRules(baselineConfig?.textModelRules ?? []),
  );
  const isDirty = $derived(
    editableConfigSnapshot(config) !== editableConfigSnapshot(baselineConfig)
    || visionRulesTextDirty,
  );

  $effect(() => {
    if (formType !== 'vision') return;
    const signature = serializeVisionRules(config?.textModelRules ?? []);
    if (signature !== syncedVisionRulesSignature) {
      visionRulesText = signature;
      syncedVisionRulesSignature = signature;
    }
  });

  // --- 下拉外部点击关闭 ---
  // 模型下拉用 position: fixed 渲染到 .model-combobox 之外的 stacking context，
  // 这里用两个 ref 锁定边界：只有点击落在 combobox 或选项列表 内才视为内部交互，
  // 其余 pointerdown 一律关闭本表单的下拉。
  let comboboxEl: HTMLDivElement | undefined = $state();
  let modelListEl: HTMLDivElement | undefined = $state();
  let modelQuery = $state('');
  const filteredModels = $derived((modelLists[statusKey] ?? []).filter(model =>
    `${modelDisplayNames[statusKey]?.[model] ?? ''} ${model}`.toLocaleLowerCase().includes(modelQuery.trim().toLocaleLowerCase())));
  $effect(() => { if (!modelListOpen[statusKey] || (modelLists[statusKey]?.length ?? 0) <= 10) modelQuery = ''; });

  function portalToBody(node: HTMLElement) {
    document.body.appendChild(node);
    return {
      destroy() {
        node.remove();
      }
    };
  }

  $effect(() => {
    if (!modelListOpen[statusKey]) return;
    function handlePointerDown(event: PointerEvent) {
      const target = event.target as Node | null;
      if (!target) return;
      if (comboboxEl?.contains(target)) return;
      if (modelListEl?.contains(target)) return;
      closeModelList(statusKey);
    }
    function handleScroll(event: Event) {
      const target = event.target;
      if (target instanceof Node && (comboboxEl?.contains(target) || modelListEl?.contains(target))) {
        return;
      }
      closeModelList(statusKey);
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
  const saveDisabled = $derived(
    isSaving
    || !isDirty
    || (formType === 'vision' && (visionRuleAnalysis.syntaxInvalid || visionRuleAnalysis.regexInvalid)),
  );
  const showSavedLabel = $derived(currentSaveStatus === 'saved' && !isDirty);
  const showProtocolField = $derived(formType !== 'image');

  function handleModelListAction(event: MouseEvent) {
    const button = event.currentTarget as HTMLElement | null;
    const input = button?.parentElement?.querySelector('input') ?? null;
    const hasModels = Array.isArray(modelLists[statusKey]) && modelLists[statusKey].length > 0;
    if (input) {
      openModelList(statusKey, input);
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
    return protocolEndpointPath(config.apiProtocol);
  }

  function visionContextWindowInK(): number {
    const tokens = Number(config?.contextWindowTokens);
    return Number.isFinite(tokens) && tokens > 0 ? tokens / 1000 : 128;
  }

  function handleVisionContextWindowInput(event: Event): void {
    const input = event.currentTarget as HTMLInputElement;
    if (!Number.isFinite(input.valueAsNumber)) return;
    config.contextWindowTokens = Math.round(input.valueAsNumber * 1000);
  }

  function handleVisionRulesInput(value: string) {
    visionRulesText = value;
    const analysis = analyzeVisionRulesText(value);
    config.textModelRules = analysis.rules;
    syncedVisionRulesSignature = serializeVisionRules(analysis.rules);
  }

  const exactVisionRuleCount = $derived(
    (config?.textModelRules ?? []).filter((rule: any) => rule?.matchMode !== 'regex').length,
  );
  const regexVisionRuleCount = $derived(
    (config?.textModelRules ?? []).filter((rule: any) => rule?.matchMode === 'regex').length,
  );
  const duplicateVisionRuleCount = $derived.by(() => {
    const seen = new Set<string>();
    let duplicates = 0;
    for (const rule of config?.textModelRules ?? []) {
      const mode = rule?.matchMode === 'regex' ? 'regex' : 'exact';
      const pattern = String(rule?.pattern ?? '').trim();
      const key = `${mode}:${mode === 'exact' ? pattern.toLocaleLowerCase() : pattern}`;
      if (seen.has(key)) duplicates += 1;
      else seen.add(key);
    }
    return duplicates;
  });

</script>

<div class="llm-config-form">
  {#if description}
    <p class="llm-config-lead">{description}</p>
  {/if}

  <section class="settings-card" aria-label={i18n.t('settings.model.card.connection')}>
    <div class="settings-card-head">
      <h4 class="settings-section-title">{i18n.t('settings.model.card.connection')}</h4>
    </div>
    <div class="settings-card-body model-config-grid">
      <div class="llm-config-field connection-address">
        <label class="form-label" for={fieldId('base-url')}>{i18n.t('settings.model.field.baseUrl')}</label>
        <input
          id={fieldId('base-url')}
          type="text"
          class="form-input"
          bind:value={config.baseUrl}
          placeholder={getBaseUrlPlaceholder()}
        />
      </div>

      {#if showProtocolField}
        <div class="llm-config-field">
          <label class="form-label" for={fieldId('protocol')}>{i18n.t('settings.model.field.apiProtocol')}</label>
          <SelectField id={fieldId('protocol')} bind:value={config.apiProtocol}
            label={i18n.t('settings.model.field.apiProtocol')}
            options={[{ value: 'openai_responses', label: i18n.t('settings.model.protocol.openaiResponses') }, { value: 'openai_chat', label: i18n.t('settings.model.protocol.openaiChat') }, { value: 'anthropic_messages', label: i18n.t('settings.model.protocol.anthropicMessages') }]} />
          <div class="protocol-endpoint" aria-live="polite">
            <span>{i18n.t('settings.model.protocol.endpoint')}</span>
            <code>POST {protocolEndpointLabel()}</code>
          </div>
        </div>
      {/if}

      <div class="llm-config-field">
        <label class="form-label" for={fieldId('api-key')}>{i18n.t('settings.model.field.apiKey')}</label>
        <div class="api-key-wrapper">
          <input
            id={fieldId('api-key')}
            type={keyVisible[keyVisibleKey] ? 'text' : 'password'}
            class="form-input"
            bind:value={config.apiKey}
            placeholder={config.apiProtocol === 'anthropic_messages' ? 'sk-ant-...' : 'sk-...'}
            autocomplete="off"
          />
          <button
            type="button"
            class="api-key-toggle"
            onclick={() => (keyVisible[keyVisibleKey] = !keyVisible[keyVisibleKey])}
            aria-label={keyVisible[keyVisibleKey] ? i18n.t('input.hideKey') : i18n.t('input.showKey')}
            title={keyVisible[keyVisibleKey] ? i18n.t('input.hideKey') : i18n.t('input.showKey')}
          >
            <Icon name={keyVisible[keyVisibleKey] ? 'eye-slash' : 'eye'} size={14} />
          </button>
        </div>
      </div>
    </div>
  </section>

  {#if showModelField || showAdvancedOptions || formType === 'vision'}
    <section class="settings-card" aria-label={i18n.t('settings.model.card.model')}>
      <div class="settings-card-head">
        <h4 class="settings-section-title">{i18n.t('settings.model.card.model')}</h4>
      </div>
      <div class="settings-card-body model-config-grid model-row">
        {#if showModelField}
          <div class="llm-config-field">
            <label class="form-label" for={fieldId('model')}>{i18n.t('settings.model.field.model')}</label>
            <div class="model-combobox" bind:this={comboboxEl}>
              <input
                id={fieldId('model')}
                type="text"
                class="form-input"
                bind:value={config.model}
                onfocus={(e) => {
                  if ((modelLists[statusKey]?.length ?? 0) > 0) openModelList(statusKey, e.currentTarget);
                }}
              />
              <button
                class="btn btn--sm btn--secondary model-fetch-btn"
                type="button"
                onclick={handleModelListAction}
                disabled={fetchingModels[statusKey]}
                aria-label={modelListActionTitle()}
                title={modelListActionTitle()}
              >
                {#if fetchingModels[statusKey]}
                  <Icon name="refresh" size={14} class="spinning" />
                {:else if (modelLists[statusKey]?.length ?? 0) > 0}
                  <Icon name="chevron-down" size={14} />
                {:else}
                  <Icon name="download" size={14} />
                {/if}
                {modelListActionTitle()}
              </button>
              {#if modelListOpen[statusKey] && (modelLists[statusKey]?.length ?? 0) > 0}
                <div
                  bind:this={modelListEl}
                  use:portalToBody
                  class="model-select-list"
                  data-magi-surface="popover"
                  style="top: {modelListPosition.top}px; left: {modelListPosition.left}px; width: {modelListPosition.width}px;"
                >
                  {#if modelLists[statusKey].length > 10}
                    <input class="form-input model-select-search" type="search" bind:value={modelQuery}
                      aria-label={i18n.t('selection.searchFor', { label: i18n.t('settings.model.field.model') })}
                      placeholder={i18n.t('selection.search')} />
                  {/if}
                  <div role="listbox" aria-label={i18n.t('settings.model.field.model')}>
                    {#each filteredModels as m}
                      <button
                        type="button"
                        class="model-select-option"
                        role="option"
                        aria-selected={config.model === m}
                        class:selected={config.model === m}
                        title={m}
                        data-model-id={m}
                        onclick={() => { selectModel(statusKey, m); }}
                      >
                        {modelDisplayNames[statusKey]?.[m] || m}
                      </button>
                    {:else}
                      <p class="select-empty" role="status">{i18n.t('selection.empty')}</p>
                    {/each}
                  </div>
                </div>
              {/if}
            </div>
          </div>
        {/if}

        {#if showAdvancedOptions && config?.model}
          <div class="llm-config-field">
            <span class="form-label" id={fieldId('level-label')}>{i18n.t(formType === 'worker' ? 'settings.model.reasoning.workerDefault' : 'settings.model.field.level')}</span>
            <div class="reasoning-control" role="group" aria-labelledby={fieldId('level-label')}>
              {#if reasoningStatus === 'loading'}
                <div class="reasoning-status" role="status"><Icon name="loader" size={14} class="spinning" />{i18n.t('settings.model.reasoning.loading')}</div>
              {:else if reasoningStatus === 'failed'}
                <div class="reasoning-status reasoning-status--error">
                  <span role="alert">{i18n.t('settings.model.reasoning.failed')}</span>
                  <button type="button" class="btn btn--sm btn--secondary" onclick={() => reasoningRetry++}>{i18n.t('input.mainModelPicker.retry')}</button>
                </div>
                <p class="settings-hint">{i18n.t('settings.model.reasoning.preserved', {
                  effort: i18n.t(config.reasoningEffort == null ? 'input.mainModelPicker.reasoning.providerDefault' : `input.mainModelPicker.reasoning.${config.reasoningEffort}`),
                })}</p>
              {:else if reasoningCapability?.supportedEfforts.length}
                <SelectField id={fieldId('level')} value={config.reasoningEffort == null ? '' : reasoningCapability.supportedEfforts.includes(config.reasoningEffort) ? config.reasoningEffort : reasoningCapability.defaultEffort ?? ''}
                  onchange={(value) => { config.reasoningEffort = value || null; reasoningAdjusted = false; }}
                  label={i18n.t(formType === 'worker' ? 'settings.model.reasoning.workerDefault' : 'settings.model.field.level')}
                  options={[{value:'',label:i18n.t('input.mainModelPicker.reasoning.providerDefault')}, ...reasoningCapability.supportedEfforts.map(value => ({value,label:i18n.t(`input.mainModelPicker.reasoning.${value}`)}))]} />
              {:else}
                <div class="reasoning-status">{i18n.t(reasoningCapability?.source === 'unknown' ? 'input.mainModelPicker.reasoning.unknown' : 'input.mainModelPicker.reasoning.unsupported')}</div>
              {/if}
            </div>
          </div>
        {/if}
        {#if formType === 'vision'}
          <div class="llm-config-field vision-context-window-field">
            <label class="form-label vision-context-window-label" for="vision-context-window">
              {i18n.t('settings.model.field.contextWindow')}
              <span
                class="vision-context-window-help"
                title={i18n.t('settings.model.visionContextWindowHint')}
                aria-label={i18n.t('settings.model.visionContextWindowHint')}
              >
                <Icon name="info" size={12} />
              </span>
            </label>
            <div class="vision-context-window-input">
              <input
                id="vision-context-window"
                type="number"
                class="form-input"
                min="16"
                max="10000"
                step="1"
                value={visionContextWindowInK()}
                oninput={handleVisionContextWindowInput}
              />
              <span aria-hidden="true">K</span>
            </div>
          </div>
        {/if}

        {#if showAdvancedOptions && config?.model}
          <div class="model-reasoning-notes">
            {#if reasoningAdjusted}
              <p class="settings-notice" role="status">{i18n.t('settings.model.reasoning.adjusted')}</p>
            {/if}
            {#if reasoningCapability?.supportedEfforts.length}
              <p class="settings-hint">{i18n.t('settings.model.reasoning.cost')} {#if config.reasoningEffort == null}{i18n.t('settings.model.reasoning.defaultHint')}{/if}</p>
            {/if}
            <p class="settings-hint">{i18n.t(formType === 'worker' ? 'settings.model.reasoning.workerScope' : 'settings.model.reasoning.savedScope')}</p>
          </div>
        {/if}
      </div>
    </section>
  {/if}

  {#if formType === 'vision'}
    <section class="settings-card" aria-labelledby="vision-routing-title">
      <div class="settings-card-head">
        <div class="settings-card-title">
          <h4 class="settings-section-title" id="vision-routing-title">
            {i18n.t('settings.model.visionRoutingRules')}
          </h4>
          <div class="settings-section-desc">{i18n.t('settings.model.visionRoutingRulesHint')}</div>
        </div>
      </div>
      <div class="settings-card-body">
        <details class="vision-builtin-rules">
          <summary>
            <span class="vision-rules-summary__title">
              <Icon name="shield" size={13} />
              {i18n.t('settings.model.builtinVisionRules')}
              <span class="vision-rule-count">{visionBuiltinTextModelRules.length}</span>
            </span>
            <span class="vision-rules-summary__meta">
              {i18n.t('settings.model.managedByMagi')}
              <Icon name="chevron-down" size={12} />
            </span>
          </summary>
          <div class="vision-builtin-rule-list">
            {#each visionBuiltinTextModelRules as rule (rule.id)}
              <span
                class="vision-builtin-rule"
                title={rule.examples.length > 0
                  ? i18n.t('settings.model.visionRuleExamples', { examples: rule.examples.join(', ') })
                  : rule.displayName}
              >
                {rule.displayName}
              </span>
            {/each}
          </div>
        </details>

        <div class="vision-custom-rules">
          <div class="vision-custom-rules__header">
            <label class="form-label" for="vision-custom-rules">
              {i18n.t('settings.model.customVisionRules')}
            </label>
            <span class="vision-custom-rules__count">
              {i18n.t('settings.model.customVisionRulesCount', {
                names: exactVisionRuleCount,
                regex: regexVisionRuleCount,
              })}
            </span>
          </div>
          <textarea
            id="vision-custom-rules"
            class="form-input vision-rules-input"
            rows="3"
            value={visionRulesText}
            oninput={(event) => handleVisionRulesInput(event.currentTarget.value)}
            placeholder={i18n.t('settings.model.customVisionRulesPlaceholder')}
            spellcheck={false}
          ></textarea>
          <div class="llm-config-hint vision-rules-syntax-hint">
            {@html i18n.t('settings.model.customVisionRulesHint')}
          </div>
          {#if visionRuleAnalysis.syntaxInvalid}
            <div class="vision-rule-feedback vision-rule-feedback--error">
              <Icon name="warning" size={12} />
              {i18n.t('settings.model.customVisionRulesSyntaxInvalid')}
            </div>
          {:else if visionRuleAnalysis.regexInvalid}
            <div class="vision-rule-feedback vision-rule-feedback--error">
              <Icon name="warning" size={12} />
              {i18n.t('settings.model.customVisionRulesInvalid')}
            </div>
          {:else if duplicateVisionRuleCount > 0}
            <div class="vision-rule-feedback vision-rule-feedback--warning">
              <Icon name="info" size={12} />
              {i18n.t('settings.model.customVisionRulesDuplicate', { count: duplicateVisionRuleCount })}
            </div>
          {/if}
        </div>
      </div>
    </section>
  {/if}

  <div class="apple-dashboard-bar model-form-actions">
    <span class="model-form-action-desc" role="status">
      {#if isDirty}
        {i18n.t('settings.model.unsavedChanges')}
      {:else if showSavedLabel}
        {i18n.t('settings.model.saved')}
      {/if}
    </span>
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
    container: model-config / inline-size;
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }

  .llm-config-field {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  .model-config-grid {
    align-items: start;
  }

  .connection-address,
  .model-reasoning-notes {
    grid-column: 1 / -1;
  }

  .model-reasoning-notes :global(.settings-hint:first-child) {
    margin-top: 0;
  }

  @container model-config (min-width: 640px) {
    .model-config-grid {
      grid-template-columns: repeat(2, minmax(0, 1fr));
      column-gap: var(--space-4);
    }
  }

  .llm-config-form,
  .llm-config-field {
    min-width: 0;
  }

  .llm-config-form :global(.settings-section-title) {
    margin: 0;
  }

  .llm-config-lead {
    margin: var(--space-2) 0 0;
    font-size: var(--text-xs);
    line-height: 1.5;
    color: var(--foreground-muted);
  }

  .llm-config-hint {
    margin-top: var(--space-2);
    font-size: var(--text-xs);
    line-height: 1.4;
    color: var(--foreground-muted);
  }

  .reasoning-control {
    min-width: 0;
  }

  .reasoning-status {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--space-2) var(--space-3);
    min-height: var(--btn-height-lg);
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    overflow-wrap: anywhere;
  }

  .reasoning-status--error > span {
    color: var(--error);
  }

  .vision-context-window-field {
    min-width: 0;
  }

  .vision-context-window-label,
  .vision-context-window-help {
    display: inline-flex;
    align-items: center;
  }

  .vision-context-window-label {
    gap: 5px;
  }

  .vision-context-window-help {
    color: var(--foreground-muted);
    cursor: help;
  }

  .vision-context-window-input {
    position: relative;
    min-width: 0;
  }

  .vision-context-window-input .form-input {
    padding-right: 30px;
    font-variant-numeric: tabular-nums;
  }

  .vision-context-window-input > span {
    position: absolute;
    top: 50%;
    right: 10px;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    font-weight: var(--font-medium);
    transform: translateY(-50%);
    pointer-events: none;
  }

  .vision-builtin-rules {
    border-top: var(--magi-hairline) solid var(--border-subtle);
    border-bottom: var(--magi-hairline) solid var(--border-subtle);
  }

  .vision-builtin-rules summary {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
    min-height: 36px;
    padding: 0 2px;
    list-style: none;
    cursor: pointer;
  }

  .vision-builtin-rules summary::-webkit-details-marker {
    display: none;
  }

  .vision-rules-summary__title,
  .vision-rules-summary__meta {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
  }

  .vision-rules-summary__title {
    color: var(--foreground);
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
  }

  .vision-rules-summary__meta {
    color: var(--foreground-muted);
    font-size: var(--text-xs);
  }

  .vision-rules-summary__meta :global(svg) {
    transition: transform var(--transition-fast);
  }

  .vision-builtin-rules[open] .vision-rules-summary__meta :global(svg) {
    transform: rotate(180deg);
  }

  .vision-rule-count {
    min-width: 20px;
    padding: 0 5px;
    border-radius: var(--radius-full);
    color: var(--foreground-muted);
    background: var(--surface-2);
    font-size: var(--text-xs);
    font-weight: var(--font-medium);
    line-height: 18px;
    text-align: center;
  }

  .vision-builtin-rule-list {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    padding: 2px 0 10px;
  }

  .vision-builtin-rule {
    padding: 3px 7px;
    border: var(--magi-hairline) solid var(--border-subtle);
    border-radius: var(--radius-sm);
    color: var(--foreground-muted);
    background: color-mix(in srgb, var(--surface-2) 68%, transparent);
    font-size: var(--text-xs);
    line-height: 1.35;
  }

  .vision-custom-rules {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  .vision-custom-rules__header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
  }

  .vision-custom-rules__count {
    color: var(--foreground-muted);
    font-size: var(--text-xs);
  }

  .vision-rules-input {
    min-height: 76px;
    resize: vertical;
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    line-height: 1.55;
  }

  .vision-rules-syntax-hint {
    margin-top: 0;
  }

  .vision-rules-syntax-hint :global(code) {
    color: var(--foreground);
    font-family: var(--font-mono);
  }

  .vision-rule-feedback {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: var(--text-xs);
    line-height: 1.4;
  }

  .vision-rule-feedback--error {
    color: var(--error);
  }

  .vision-rule-feedback--warning {
    color: var(--warning);
  }

  @media (max-width: 760px) {
    .vision-rules-summary__meta {
      max-width: 45%;
      text-align: right;
    }
  }

  .protocol-endpoint {
    display: inline-flex;
    align-items: baseline;
    flex-wrap: wrap;
    gap: var(--space-2);
    min-width: 0;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
  }

  .protocol-endpoint code {
    color: var(--foreground);
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    overflow-wrap: anywhere;
  }

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
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
  }

  .model-combobox .form-input {
    flex: 1;
    min-width: 0;
  }

  .model-fetch-btn {
    flex: 0 0 auto;
    max-width: 100%;
  }

  .model-select-list {
    position: fixed;
    z-index: var(--z-popover);
    max-height: 264px;
    overflow-y: auto;
    border: var(--magi-hairline) solid var(--border);
    border-top: none;
    border-radius: 0 0 var(--radius-sm) var(--radius-sm);
    box-shadow: 0 4px 12px rgba(0, 0, 0, 0.3);
  }
  .model-select-search { margin: 4px; width: calc(100% - 8px); }
  .select-empty { margin: 8px; color: var(--foreground-muted); font-size: var(--text-xs); }
  .model-select-option {
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
  .model-select-option:hover {
    background: var(--secondary);
  }
  .model-select-option.selected {
    color: var(--primary);
    background: var(--primary-muted, rgba(var(--primary-rgb, 100, 149, 237), 0.1));
  }

  .model-form-actions {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-3) 0;
    border-top: var(--magi-hairline) solid var(--border-subtle);
  }

  .model-form-action-desc {
    min-width: 0;
    flex: 1;
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    overflow-wrap: anywhere;
  }

  .vision-custom-rules__header,
  .vision-builtin-rules summary {
    flex-wrap: wrap;
  }

  @container settings-model (max-width: 480px) {
    .model-form-actions {
      align-items: stretch;
      flex-direction: column;
    }
    .model-form-actions :global(.settings-section-actions) {
      display: grid;
      grid-template-columns: repeat(2, minmax(0, 1fr));
      width: 100%;
    }
    .model-form-actions :global(.btn) {
      width: 100%;
      justify-content: center;
    }
  }
</style>
