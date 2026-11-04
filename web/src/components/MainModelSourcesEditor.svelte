<script lang="ts">
  import ModelConnectionFields from './ModelConnectionFields.svelte';
  import Icon from './Icon.svelte';
  import { confirmMessage } from '../stores/confirm-dialog.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { fetchAgentModelList, saveAgentMainModelSources } from '../web/agent-api';
  import type { MainModelCatalogEntry, MainModelSource, MainModelSourcesConfig } from '../shared/main-model-sources';

  type MainEditorConfig = Record<string, unknown> & {
    sources?: MainModelSource[];
    revision?: number;
  };
  let { config = $bindable(), getBaseUrlPlaceholder, onSaved }: {
    config: MainEditorConfig;
    getBaseUrlPlaceholder: () => string;
    onSaved: (config: MainModelSourcesConfig) => void;
  } = $props();

  const emptySource = (id: string, name: string): MainModelSource => ({
    id, name, baseUrl: '', apiKey: '', apiProtocol: 'openai_responses', models: [],
  });
  let activeId = $state('');
  let draft = $state<MainModelSource | null>(null);
  let baseline = $state('');
  let newSource = $state(false);
  let busy = $state(false);
  let fetching = $state(false);
  let message = $state('');
  let modelIdDraft = $state('');
  let modelNameDraft = $state('');
  let keyVisible = $state<Record<string, boolean>>({});

  const sources = $derived(Array.isArray(config.sources) ? config.sources as MainModelSource[] : []);
  const dirty = $derived(draft !== null && JSON.stringify(draft) !== baseline);

  function cloneSource(source: MainModelSource): MainModelSource {
    return { ...source, models: source.models.map((model) => ({ ...model })) };
  }

  function selectSaved(source: MainModelSource): void {
    draft = cloneSource(source);
    activeId = source.id;
    baseline = JSON.stringify(draft);
    newSource = false;
    message = '';
    modelIdDraft = '';
    modelNameDraft = '';
  }

  async function changeSource(source: MainModelSource): Promise<void> {
    if (dirty && !(await confirmMessage(i18n.t('settings.model.sources.discardDraft'), {
      title: i18n.t('settings.model.sources.title'),
      confirmLabel: i18n.t('settings.model.unsavedClose.discard'),
    }))) return;
    selectSaved(source);
  }

  async function addSource(): Promise<void> {
    if (dirty && !(await confirmMessage(i18n.t('settings.model.sources.discardDraft'), {
      title: i18n.t('settings.model.sources.title'),
      confirmLabel: i18n.t('settings.model.unsavedClose.discard'),
    }))) return;
    let index = 1;
    while (sources.some((source) => source.id === `source-${index}`)) index += 1;
    const source = emptySource(`source-${index}`, i18n.t('settings.model.sources.newName', { count: index }));
    draft = source;
    activeId = source.id;
    baseline = '';
    newSource = true;
    message = '';
    modelIdDraft = '';
    modelNameDraft = '';
  }

  async function save(): Promise<void> {
    if (!draft || !dirty || busy) return;
    busy = true;
    message = '';
    try {
      const next = newSource ? [...sources, cloneSource(draft)]
        : sources.map((source) => source.id === draft?.id ? cloneSource(draft) : source);
      const saved = await saveAgentMainModelSources({ revision: config.revision || 0, sources: next });
      config = { ...config, ...saved };
      onSaved(saved);
      const current = saved.sources.find((source) => source.id === draft?.id);
      if (current) selectSaved(current);
      else { draft = null; activeId = ''; baseline = ''; newSource = false; }
    } catch (error) {
      message = error instanceof Error ? error.message : i18n.t('settings.model.sources.saveFailed');
    } finally { busy = false; }
  }

  async function remove(source: MainModelSource): Promise<void> {
    if (busy || !(await confirmMessage(i18n.t('settings.model.sources.deleteConfirm', { name: source.name }), {
      title: i18n.t('settings.model.sources.delete'),
      confirmLabel: i18n.t('settings.model.sources.delete'),
      tone: 'danger',
    }))) return;
    busy = true;
    message = '';
    try {
      const saved = await saveAgentMainModelSources({
        revision: config.revision || 0,
        sources: sources.filter((entry) => entry.id !== source.id),
      });
      config = { ...config, ...saved };
      onSaved(saved);
      if (activeId === source.id) {
        draft = null;
        activeId = '';
        baseline = '';
        newSource = false;
      }
    } catch (error) {
      message = error instanceof Error ? error.message : i18n.t('settings.model.sources.saveFailed');
    } finally { busy = false; }
  }

  async function fetchModels(): Promise<void> {
    if (!draft || fetching) return;
    fetching = true;
    message = '';
    try {
      const result = await fetchAgentModelList({
        baseUrl: draft.baseUrl,
        apiKey: draft.apiKey,
        apiProtocol: draft.apiProtocol,
      }, 'orch');
      const names = result.modelDisplayNames ?? {};
      draft.models = result.models.map((id) => ({ id, displayName: names[id] || id }));
    } catch (error) {
      message = error instanceof Error ? error.message : i18n.t('settings.model.sources.fetchFailed');
    } finally { fetching = false; }
  }

  function addManualModel(): void {
    if (!draft) return;
    const id = modelIdDraft.trim();
    if (!id || draft.models.some((model) => model.id === id)) return;
    const entry: MainModelCatalogEntry = { id, displayName: modelNameDraft.trim() || id };
    draft.models = [...draft.models, entry];
    modelIdDraft = '';
    modelNameDraft = '';
  }

  function updateModel(index: number, patch: Partial<MainModelCatalogEntry>): void {
    if (!draft) return;
    draft.models = draft.models.map((model, current) => current === index ? { ...model, ...patch } : model);
  }
</script>

<div class="llm-config-form">
  <p class="llm-config-lead">{i18n.t('settings.model.orchestratorDesc')}</p>

  <section class="settings-card" aria-labelledby="main-model-sources-title">
    <div class="settings-card-head">
      <div class="settings-card-title">
        <h4 class="settings-section-title" id="main-model-sources-title">{i18n.t('settings.model.sources.title')}</h4>
      </div>
      <div class="settings-section-actions">
        <button class="btn btn--primary btn--sm" type="button" onclick={() => void addSource()} disabled={busy || dirty}>
          <Icon name="plus" size={14} />{i18n.t('settings.model.sources.add')}
        </button>
      </div>
    </div>
    <div class="settings-card-body">
      {#if sources.length > 0}
        <div class="source-list" role="list" aria-label={i18n.t('settings.model.sources.title')}>
          {#each sources as source (source.id)}
            <div class="settings-row source-row" role="listitem">
              <button type="button" class="source-row-select" class:selected={activeId === source.id}
                onclick={() => void changeSource(source)} disabled={busy || (dirty && activeId === source.id)}>
                <span class="settings-row-copy">
                  <span class="settings-row-title">{source.name}</span>
                  <span class="settings-row-desc">{source.baseUrl} · {source.models.length} {i18n.t('settings.model.sources.models')}</span>
                </span>
              </button>
              <div class="source-row-actions">
                <button type="button" class="btn btn--secondary btn--sm" onclick={() => void changeSource(source)} disabled={busy || (dirty && activeId === source.id)}>
                  {i18n.t('settings.model.sources.edit')}
                </button>
                <button type="button" class="btn btn--error btn--sm" onclick={() => void remove(source)} disabled={busy}>
                  {i18n.t('settings.model.sources.delete')}
                </button>
              </div>
            </div>
          {/each}
        </div>
      {:else}
        <div class="settings-empty">{i18n.t('settings.model.sources.empty')}</div>
      {/if}
      {#if !draft && sources.length > 0}
        <p class="settings-hint source-select-hint">{i18n.t('settings.model.sources.selectHint')}</p>
      {/if}
    </div>
  </section>

  {#if draft}
    <div class="source-editor">
      <div class="source-editor-heading">
        <h4 class="settings-section-title">{newSource ? i18n.t('settings.model.sources.add') : draft.name}</h4>
        <button type="button" class="btn btn--secondary btn--sm" onclick={() => { draft = null; activeId = ''; baseline = ''; newSource = false; message = ''; }} disabled={busy}>
          {i18n.t('common.cancel')}
        </button>
      </div>
      <ModelConnectionFields bind:config={draft} statusKey={`source-${draft.id}`} bind:keyVisible {getBaseUrlPlaceholder}>
        <div class="llm-config-field">
          <label class="form-label" for="main-source-name">{i18n.t('settings.model.sources.name')}</label>
          <input id="main-source-name" class="form-input" bind:value={draft.name} maxlength="128" />
        </div>
      </ModelConnectionFields>

      <section class="settings-card" aria-labelledby="source-model-catalog-title">
        <div class="settings-card-head">
          <div class="settings-card-title">
            <h4 class="settings-section-title" id="source-model-catalog-title">{i18n.t('settings.model.sources.modelCatalog')}</h4>
            <div class="settings-section-desc">{i18n.t('settings.model.sources.modelCatalogHint')}</div>
          </div>
          <div class="settings-section-actions">
            <button type="button" class="btn btn--secondary btn--sm" onclick={() => void fetchModels()} disabled={fetching || busy || !draft.baseUrl.trim()}>
              {#if fetching}<Icon name="refresh" size={13} class="spinning" />{/if}
              {i18n.t(fetching ? 'settings.model.sources.fetching' : 'settings.model.sources.fetchModels')}
            </button>
          </div>
        </div>
        <div class="settings-card-body">
          {#if draft.models.length > 0}
            <div class="source-model-list">
              {#each draft.models as model, index (`${draft.id}:${model.id}`)}
                <div class="settings-row source-model-row">
                  <div class="source-model-fields">
                    <input class="form-input source-model-id" aria-label={i18n.t('settings.model.sources.modelId')} bind:value={model.id} oninput={(event) => updateModel(index, { id: event.currentTarget.value })} />
                    <input class="form-input" aria-label={i18n.t('settings.model.sources.modelName')} bind:value={model.displayName} oninput={(event) => updateModel(index, { displayName: event.currentTarget.value })} />
                  </div>
                  <button type="button" class="btn btn--error btn--sm" aria-label={i18n.t('settings.model.sources.removeModel', { model: model.id })}
                    onclick={() => { if (draft) draft.models = draft.models.filter((_, current) => current !== index); }}>
                    <Icon name="close" size={13} />
                  </button>
                </div>
              {/each}
            </div>
          {:else}
            <div class="settings-empty">{i18n.t('settings.model.sources.noModels')}</div>
          {/if}
          <div class="source-add-model">
            <input class="form-input" bind:value={modelIdDraft} onkeydown={(event) => { if (event.key === 'Enter') { event.preventDefault(); addManualModel(); } }}
              placeholder={i18n.t('settings.model.sources.modelId')} aria-label={i18n.t('settings.model.sources.modelId')} />
            <input class="form-input" bind:value={modelNameDraft} placeholder={i18n.t('settings.model.sources.modelName')} aria-label={i18n.t('settings.model.sources.modelName')} />
            <button type="button" class="btn btn--secondary btn--sm" onclick={addManualModel} disabled={!modelIdDraft.trim() || draft.models.some((model) => model.id === modelIdDraft.trim())}>
              <Icon name="plus" size={13} />{i18n.t('settings.model.sources.addModel')}
            </button>
          </div>
        </div>
      </section>
    </div>
    <div class="model-form-actions">
      <span class="model-form-action-desc" role="status">{dirty ? i18n.t('settings.model.unsavedChanges') : ''}</span>
      <div class="settings-section-actions">
        <button type="button" class="btn btn--primary btn--sm" onclick={() => void save()} disabled={!dirty || busy || !draft.name.trim() || !draft.baseUrl.trim()}>
          {#if busy}<Icon name="refresh" size={13} class="spinning" />{/if}
          {i18n.t(busy ? 'settings.model.saving' : 'settings.model.saveConfig')}
        </button>
      </div>
    </div>
  {/if}
  {#if message}<div class="settings-notice settings-notice--error" role="alert">{message}</div>{/if}
</div>

<style>
  .source-row { display: flex; align-items: center; gap: var(--space-3); }
  .source-row-select { display: flex; flex: 1 1 auto; min-width: 0; border: 0; padding: 0; color: inherit; background: transparent; text-align: start; cursor: pointer; }
  .source-row-select.selected .settings-row-title { color: var(--primary); }
  .source-row .settings-row-title { overflow-wrap: anywhere; }
  .source-row-actions { display: flex; align-items: center; flex: 0 0 auto; gap: var(--space-2); }
  .source-row .settings-row-desc { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; direction: ltr; text-align: start; }
  .source-select-hint { margin: 0; }
  .source-editor { display: flex; flex-direction: column; gap: var(--space-3); min-width: 0; }
  .source-editor-heading { display: flex; flex-wrap: wrap; align-items: center; justify-content: space-between; gap: var(--space-3); min-width: 0; }
  .source-editor-heading h4 { flex: 1; min-width: 0; overflow-wrap: anywhere; }
  .source-model-list { max-height: min(36vh, 360px); overflow-y: auto; }
  .source-model-row { display: flex; align-items: center; gap: var(--space-2); }
  .source-model-fields { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1fr); gap: var(--space-2); flex: 1 1 auto; min-width: 0; }
  .source-model-id { direction: ltr; }
  .source-add-model { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1fr) auto; gap: var(--space-2); }
  @container model-config (max-width: 580px) {
    .source-add-model { grid-template-columns: minmax(0, 1fr) minmax(0, 1fr); }
    .source-add-model .btn { grid-column: 1 / -1; justify-self: start; }
  }
  @container model-config (max-width: 480px) {
    .source-row { align-items: flex-start; flex-direction: column; }
    .source-row-actions { align-self: flex-end; }
    .source-model-fields, .source-add-model { grid-template-columns: minmax(0, 1fr); }
  }
</style>
