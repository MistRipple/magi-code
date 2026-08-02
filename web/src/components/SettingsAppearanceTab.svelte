<script lang="ts">
  import { onMount, tick } from 'svelte';
  import Icon from './Icon.svelte';
  import Toggle from './Toggle.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import {
    activateAppearanceTheme,
    createAppearanceTheme,
    deleteAppearanceTheme,
    downloadThemePackage,
    exportAppearanceTheme,
    importAppearanceTheme,
    updateAppearanceTheme,
    uploadAppearanceAsset,
  } from '../appearance/client';
  import {
    applyAppearanceSnapshot,
    previewAppearanceTheme,
    refreshAppearanceRuntime,
    resolveAppearanceAssetUrl,
    restoreActiveAppearance,
    subscribeAppearanceRuntime,
    type AppearanceRuntimeSnapshot,
  } from '../appearance/runtime';
  import type {
    AppearanceSnapshot,
    ThemeMaterial,
    ThemePack,
    ThemeRecord,
    ThemeScheme,
  } from '../appearance/contract';

  let runtime = $state<AppearanceRuntimeSnapshot>({
    library: null,
    activeTheme: null,
    mode: 'dark',
    previewing: false,
  });
  let editorOpen = $state(false);
  let editingThemeId = $state<string | null>(null);
  let draft = $state<ThemePack | null>(null);
  let busyAction = $state('');
  let errorMessage = $state('');
  let pendingDelete = $state<ThemeRecord | null>(null);
  let deleteDialogElement = $state<HTMLDivElement | null>(null);
  let importInput = $state<HTMLInputElement | null>(null);
  let wallpaperInput = $state<HTMLInputElement | null>(null);
  let wallpaperUrls = $state<Record<string, string>>({});
  let wallpaperLoadSequence = 0;

  const themes = $derived(runtime.library?.themes ?? []);
  const visibleThemes = $derived(themes.filter((record) => record.pack.id !== 'builtin.system'));
  const wallpaperAssetIds = $derived([
    ...new Set(themes.flatMap((record) => record.pack.wallpaper?.assetId ?? [])),
  ]);
  const activeThemeId = $derived(runtime.library?.activeThemeId ?? '');
  const followsSystemAppearance = $derived(activeThemeId === 'builtin.system');
  const isEditingExisting = $derived(Boolean(editingThemeId));
  const editorTitle = $derived(isEditingExisting
    ? i18n.t('appearance.editTheme')
    : i18n.t('appearance.createTheme'));

  onMount(() => {
    const unsubscribe = subscribeAppearanceRuntime((snapshot) => {
      runtime = snapshot;
    });
    if (!runtime.library) {
      void refreshAppearanceRuntime().catch((error) => {
        errorMessage = error instanceof Error ? error.message : String(error);
      });
    }
    return unsubscribe;
  });

  $effect(() => {
    const currentDraft = draft;
    if (!editorOpen || !currentDraft || !canPreview(currentDraft)) return;
    void previewAppearanceTheme(currentDraft).catch((error) => {
      errorMessage = error instanceof Error ? error.message : String(error);
    });
  });

  $effect(() => {
    const assetIds = wallpaperAssetIds;
    const sequence = ++wallpaperLoadSequence;
    if (assetIds.length === 0) {
      wallpaperUrls = {};
      return;
    }
    void Promise.all(assetIds.map(async (assetId) => [
      assetId,
      await resolveAppearanceAssetUrl(assetId),
    ] as const)).then((entries) => {
      if (sequence === wallpaperLoadSequence) wallpaperUrls = Object.fromEntries(entries);
    }).catch((error) => {
      if (sequence === wallpaperLoadSequence) {
        errorMessage = error instanceof Error ? error.message : String(error);
      }
    });
  });

  function schemeForCard(record: ThemeRecord): ThemeScheme {
    return record.pack.schemes[runtime.mode]
      ?? record.pack.schemes.dark
      ?? record.pack.schemes.light
      ?? { accent: '#3B82F6', background: '#0F141B', foreground: '#E5E7EB', contrast: 60 };
  }

  function sourceLabel(record: ThemeRecord): string {
    if (record.source === 'builtin') return i18n.t('appearance.sourceBuiltin');
    if (record.source === 'imported') return i18n.t('appearance.sourceImported');
    return i18n.t('appearance.sourceCreated');
  }

  function materialLabel(material: ThemeMaterial): string {
    return i18n.t(`appearance.material.${material}`);
  }

  function clonePack(pack: ThemePack): ThemePack {
    return {
      ...pack,
      schemes: {
        ...(pack.schemes.light ? { light: { ...pack.schemes.light } } : {}),
        ...(pack.schemes.dark ? { dark: { ...pack.schemes.dark } } : {}),
      },
      ...(pack.wallpaper ? { wallpaper: { ...pack.wallpaper } } : {}),
    };
  }

  function fixedCustomPack(pack: ThemePack): ThemePack | null {
    const targetMode = pack.schemePolicy === 'adaptive' ? runtime.mode : pack.schemePolicy;
    const scheme = pack.schemes[targetMode];
    if (!scheme) return null;
    return {
      ...clonePack(pack),
      schemePolicy: targetMode,
      schemes: targetMode === 'light'
        ? { light: cloneScheme(scheme) }
        : { dark: cloneScheme(scheme) },
    };
  }

  function userThemeId(baseName: string): string {
    const slug = baseName
      .toLowerCase()
      .normalize('NFKD')
      .replace(/[^a-z0-9]+/g, '-')
      .replace(/^-+|-+$/g, '')
      .slice(0, 32) || 'theme';
    return `user.${slug}-${Date.now().toString(36)}`;
  }

  function beginCreate(base?: ThemeRecord): void {
    const source = base?.pack ?? runtime.activeTheme;
    if (!source) return;
    const next = fixedCustomPack(source);
    if (!next) return;
    next.id = userThemeId(next.name);
    next.name = i18n.t('appearance.copyName', { name: next.name });
    next.author = undefined;
    editingThemeId = null;
    draft = next;
    errorMessage = '';
    editorOpen = true;
  }

  function beginEdit(record: ThemeRecord): void {
    if (!record.editable) {
      beginCreate(record);
      return;
    }
    editingThemeId = record.pack.id;
    draft = clonePack(record.pack);
    errorMessage = '';
    editorOpen = true;
  }

  async function closeEditor(): Promise<void> {
    editorOpen = false;
    editingThemeId = null;
    draft = null;
    errorMessage = '';
    await restoreActiveAppearance();
  }

  async function runAction(action: string, operation: () => Promise<void>): Promise<void> {
    if (busyAction) return;
    busyAction = action;
    errorMessage = '';
    try {
      await operation();
    } catch (error) {
      errorMessage = error instanceof Error ? error.message : String(error);
    } finally {
      busyAction = '';
    }
  }

  async function applyTheme(themeId: string): Promise<void> {
    const library = runtime.library;
    if (!library || themeId === library.activeThemeId) return;
    await runAction(`apply:${themeId}`, async () => {
      const snapshot = await activateAppearanceTheme(themeId, library.revision);
      await applyAppearanceSnapshot(snapshot);
    });
  }

  async function toggleSystemAppearance(enabled: boolean): Promise<void> {
    const targetThemeId = enabled
      ? 'builtin.system'
      : runtime.mode === 'light' ? 'builtin.light' : 'builtin.dark';
    await applyTheme(targetThemeId);
  }

  async function saveDraft(): Promise<void> {
    const library = runtime.library;
    const pack = draft;
    if (!library || !pack) return;
    await runAction('save', async () => {
      let snapshot: AppearanceSnapshot;
      if (editingThemeId) {
        snapshot = await updateAppearanceTheme(editingThemeId, pack, library.revision);
      } else {
        snapshot = await createAppearanceTheme(pack, library.revision);
      }
      if (snapshot.activeThemeId !== pack.id) {
        snapshot = await activateAppearanceTheme(pack.id, snapshot.revision);
      }
      editorOpen = false;
      editingThemeId = null;
      draft = null;
      await applyAppearanceSnapshot(snapshot);
    });
  }

  async function removeTheme(record: ThemeRecord): Promise<void> {
    const library = runtime.library;
    if (!library || !record.editable) return;
    errorMessage = '';
    pendingDelete = record;
    await tick();
    deleteDialogElement?.focus();
  }

  async function confirmRemoveTheme(): Promise<void> {
    const record = pendingDelete;
    const library = runtime.library;
    if (!record || !library || !record.editable) return;
    await runAction(`delete:${record.pack.id}`, async () => {
      let snapshot = runtime.library!;
      if (snapshot.activeThemeId === record.pack.id) {
        snapshot = await activateAppearanceTheme('builtin.system', snapshot.revision);
      }
      snapshot = await deleteAppearanceTheme(record.pack.id, snapshot.revision);
      await applyAppearanceSnapshot(snapshot);
      pendingDelete = null;
    });
  }

  async function exportTheme(record: ThemeRecord): Promise<void> {
    if (!record.editable) return;
    await runAction(`export:${record.pack.id}`, async () => {
      downloadThemePackage(await exportAppearanceTheme(record.pack.id));
    });
  }

  async function importTheme(file: File): Promise<void> {
    const library = runtime.library;
    if (!library) return;
    await runAction('import', async () => {
      const snapshot = await importAppearanceTheme(file, library.revision, 'duplicate');
      await applyAppearanceSnapshot(snapshot);
    });
  }

  async function handleImportChange(event: Event): Promise<void> {
    const input = event.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    input.value = '';
    if (file) await importTheme(file);
  }

  async function handleWallpaperChange(event: Event): Promise<void> {
    const input = event.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    input.value = '';
    if (!file || !draft) return;
    await runAction('wallpaper', async () => {
      const asset = await uploadAppearanceAsset(file);
      draft = {
        ...draft!,
        wallpaper: {
          assetId: asset.assetId,
          focusX: 0.5,
          focusY: 0.5,
          dim: 0.2,
          blur: 0,
        },
      };
    });
  }

  function cloneScheme(scheme: ThemeScheme): ThemeScheme {
    return { ...scheme };
  }

  function canPreview(pack: ThemePack): boolean {
    const required = pack.schemePolicy === 'adaptive'
      ? [pack.schemes.light, pack.schemes.dark]
      : [pack.schemes[pack.schemePolicy]];
    return required.every((scheme) => Boolean(
      scheme
      && /^#[0-9a-f]{6}$/i.test(scheme.accent)
      && /^#[0-9a-f]{6}$/i.test(scheme.background)
      && /^#[0-9a-f]{6}$/i.test(scheme.foreground),
    ));
  }
</script>

<div class="appearance-tab settings-tab-inner">
  <div class="appearance-toolbar">
    <div>
      <div class="toolbar-title">{i18n.t('appearance.libraryTitle')}</div>
      <div class="toolbar-subtitle">{i18n.t('appearance.libraryDescription')}</div>
    </div>
    <div class="toolbar-actions">
      <input
        bind:this={importInput}
        class="sr-only"
        type="file"
        accept=".zip,.magi-theme.zip,application/zip"
        onchange={handleImportChange}
      />
      <button class="btn btn--secondary btn--sm" type="button" onclick={() => importInput?.click()} disabled={Boolean(busyAction)}>
        <Icon name="download" size={14} />
        {i18n.t('appearance.import')}
      </button>
      <button class="btn btn--primary btn--sm" type="button" onclick={() => beginCreate()} disabled={!runtime.activeTheme || Boolean(busyAction)}>
        <Icon name="plus" size={14} />
        {i18n.t('appearance.create')}
      </button>
    </div>
  </div>

  {#if errorMessage && !editorOpen}
    <div class="inline-alert inline-alert--error" role="alert">{errorMessage}</div>
  {/if}

  {#if runtime.library}
    <div class="system-appearance-row">
      <span class="system-appearance-icon" aria-hidden="true"><Icon name="monitor" size={16} /></span>
      <div class="system-appearance-copy">
        <strong>{i18n.t('appearance.followSystem')}</strong>
        <span>
          {i18n.t('appearance.followSystemDescription')}
          · {i18n.t('appearance.currentMode', { mode: i18n.t(`appearance.mode.${runtime.mode}`) })}
        </span>
      </div>
      <Toggle
        checked={followsSystemAppearance}
        disabled={Boolean(busyAction)}
        ariaLabel={i18n.t('appearance.followSystem')}
        onchange={(enabled) => void toggleSystemAppearance(enabled)}
      />
    </div>
  {/if}

  {#if !runtime.library}
    <div class="loading-state"><Icon name="loader" size={18} /><span>{i18n.t('common.loading')}</span></div>
  {:else}
  <div class="theme-grid" aria-label={i18n.t('appearance.libraryTitle')}>
    {#each visibleThemes as record (record.pack.id)}
      {@const colors = schemeForCard(record)}
      {@const wallpaper = record.pack.wallpaper}
      {@const wallpaperUrl = wallpaper ? wallpaperUrls[wallpaper.assetId] : ''}
      <article class="theme-card" class:active={record.pack.id === activeThemeId}>
        <button
          class="theme-preview"
          type="button"
          style:background={colors.background}
          aria-label={i18n.t('appearance.applyTheme', { name: record.pack.name })}
          onclick={() => void applyTheme(record.pack.id)}
          disabled={Boolean(busyAction)}
        >
          {#if wallpaper && wallpaperUrl}
            <span
              class="preview-wallpaper"
              aria-hidden="true"
              style:background-image={`url("${wallpaperUrl}")`}
              style:background-position={`${wallpaper.focusX * 100}% ${wallpaper.focusY * 100}%`}
              style:filter={`blur(${wallpaper.blur}px)`}
            ></span>
            <span class="preview-wallpaper-dim" aria-hidden="true" style:opacity={wallpaper.dim}></span>
          {/if}
          <span class="preview-sidebar" style:background={`color-mix(in srgb, ${colors.foreground} 8%, ${colors.background})`}></span>
          <span class="preview-line preview-line--wide" style:background={colors.foreground}></span>
          <span class="preview-line" style:background={colors.foreground}></span>
          <span class="preview-accent" style:background={colors.accent}></span>
          {#if record.pack.id === activeThemeId}
            <span class="active-mark"><Icon name="check" size={12} /></span>
          {/if}
        </button>
        <div class="theme-card-body">
          <div class="theme-meta">
            <strong title={record.pack.name}>{record.pack.name}</strong>
            <span>{sourceLabel(record)} · {materialLabel(record.pack.material)}</span>
          </div>
          <div class="theme-actions">
            <button
              class="btn-icon btn-icon--sm"
              type="button"
              title={record.editable ? i18n.t('appearance.editTheme') : i18n.t('appearance.createFrom')}
              onclick={() => beginEdit(record)}
              disabled={Boolean(busyAction)}
            >
              <Icon name={record.editable ? 'edit' : 'copy'} size={13} />
            </button>
            {#if record.editable}
              <button class="btn-icon btn-icon--sm" type="button" title={i18n.t('appearance.export')} onclick={() => void exportTheme(record)} disabled={Boolean(busyAction)}>
                <Icon name="download" size={13} />
              </button>
              <button class="btn-icon btn-icon--sm btn-icon--danger" type="button" title={i18n.t('appearance.delete')} onclick={() => void removeTheme(record)} disabled={Boolean(busyAction)}>
                <Icon name="trash" size={13} />
              </button>
            {/if}
          </div>
        </div>
      </article>
    {/each}
  </div>
  {/if}

  {#if editorOpen && draft}
    <div class="editor-overlay" role="presentation">
      <section class="theme-editor" data-magi-surface="window" aria-label={editorTitle}>
        <header class="editor-header">
          <div>
            <h3>{editorTitle}</h3>
            <p>{i18n.t('appearance.previewHint')}</p>
          </div>
          <button class="btn-icon btn-icon--sm" type="button" title={i18n.t('common.cancel')} onclick={() => void closeEditor()} disabled={Boolean(busyAction)}>
            <Icon name="close" size={14} />
          </button>
        </header>

        <div class="editor-content">
          <div class="editor-section editor-section--identity">
            <label>
              <span class="form-label">{i18n.t('appearance.name')}</span>
              <input class="form-input" type="text" maxlength="60" bind:value={draft.name} />
            </label>
            <label>
              <span class="form-label">{i18n.t('appearance.description')}</span>
              <input class="form-input" type="text" maxlength="160" bind:value={draft.description} />
            </label>
          </div>

          <div class="editor-section">
            <div class="section-label">{i18n.t('appearance.materialTitle')}</div>
            <div class="material-options">
              {#each ['clear', 'translucent', 'immersive'] as material}
                <button type="button" class:active={draft.material === material} onclick={() => draft!.material = material as ThemeMaterial}>
                  <span>{i18n.t(`appearance.material.${material}`)}</span>
                  <small>{i18n.t(`appearance.material.${material}Desc`)}</small>
                </button>
              {/each}
            </div>
          </div>

          {#if draft.schemes.light}
            <div class="editor-section color-section">
              <div class="section-label">{i18n.t('appearance.themeColors')}</div>
              <div class="color-grid">
                <label><span class="form-label">{i18n.t('appearance.background')}</span><input class="control-color" type="color" bind:value={draft.schemes.light.background} /></label>
                <label><span class="form-label">{i18n.t('appearance.foreground')}</span><input class="control-color" type="color" bind:value={draft.schemes.light.foreground} /></label>
                <label><span class="form-label">{i18n.t('appearance.accent')}</span><input class="control-color" type="color" bind:value={draft.schemes.light.accent} /></label>
              </div>
              <label class="range-row"><span class="form-label">{i18n.t('appearance.contrast')}</span><input class="control-range" type="range" min="20" max="90" bind:value={draft.schemes.light.contrast} /><output>{draft.schemes.light.contrast}</output></label>
            </div>
          {/if}

          {#if draft.schemes.dark}
            <div class="editor-section color-section">
              <div class="section-label">{i18n.t('appearance.themeColors')}</div>
              <div class="color-grid">
                <label><span class="form-label">{i18n.t('appearance.background')}</span><input class="control-color" type="color" bind:value={draft.schemes.dark.background} /></label>
                <label><span class="form-label">{i18n.t('appearance.foreground')}</span><input class="control-color" type="color" bind:value={draft.schemes.dark.foreground} /></label>
                <label><span class="form-label">{i18n.t('appearance.accent')}</span><input class="control-color" type="color" bind:value={draft.schemes.dark.accent} /></label>
              </div>
              <label class="range-row"><span class="form-label">{i18n.t('appearance.contrast')}</span><input class="control-range" type="range" min="20" max="90" bind:value={draft.schemes.dark.contrast} /><output>{draft.schemes.dark.contrast}</output></label>
            </div>
          {/if}

          <div class="editor-section wallpaper-section">
            <div class="wallpaper-heading">
              <div><div class="section-label">{i18n.t('appearance.wallpaper')}</div><small>{i18n.t('appearance.wallpaperHint')}</small></div>
              <div class="wallpaper-actions">
                <input bind:this={wallpaperInput} class="sr-only" type="file" accept="image/png,image/jpeg,image/webp" onchange={handleWallpaperChange} />
                <button class="btn btn--secondary btn--sm" type="button" onclick={() => wallpaperInput?.click()} disabled={Boolean(busyAction)}>{i18n.t('appearance.chooseImage')}</button>
                {#if draft.wallpaper}<button class="btn btn--ghost-danger btn--sm" type="button" onclick={() => draft!.wallpaper = undefined}>{i18n.t('appearance.removeImage')}</button>{/if}
              </div>
            </div>
            {#if draft.wallpaper}
              <label class="range-row"><span class="form-label">{i18n.t('appearance.focusHorizontal')}</span><input class="control-range" type="range" min="0" max="1" step="0.01" bind:value={draft.wallpaper.focusX} /><output>{Math.round(draft.wallpaper.focusX * 100)}%</output></label>
              <label class="range-row"><span class="form-label">{i18n.t('appearance.focusVertical')}</span><input class="control-range" type="range" min="0" max="1" step="0.01" bind:value={draft.wallpaper.focusY} /><output>{Math.round(draft.wallpaper.focusY * 100)}%</output></label>
              <label class="range-row"><span class="form-label">{i18n.t('appearance.dim')}</span><input class="control-range" type="range" min="0" max="0.85" step="0.01" bind:value={draft.wallpaper.dim} /><output>{Math.round(draft.wallpaper.dim * 100)}%</output></label>
              <label class="range-row"><span class="form-label">{i18n.t('appearance.blur')}</span><input class="control-range" type="range" min="0" max="24" step="1" bind:value={draft.wallpaper.blur} /><output>{draft.wallpaper.blur}px</output></label>
            {/if}
          </div>
        </div>

        {#if errorMessage}<div class="inline-alert inline-alert--error editor-error" role="alert">{errorMessage}</div>{/if}
        <footer class="editor-footer">
          <button class="btn btn--secondary btn--sm" type="button" onclick={() => void closeEditor()} disabled={Boolean(busyAction)}>{i18n.t('common.cancel')}</button>
          <button class="btn btn--primary btn--sm" type="button" onclick={() => void saveDraft()} disabled={Boolean(busyAction) || !draft.name.trim() || !canPreview(draft)}>
            {busyAction === 'save' ? i18n.t('common.loading') : i18n.t('appearance.saveAndApply')}
          </button>
        </footer>
      </section>
    </div>
  {/if}

  {#if pendingDelete}
    <div class="confirm-overlay" role="presentation">
      <div bind:this={deleteDialogElement} class="confirm-dialog" data-magi-surface="critical" role="alertdialog" tabindex="-1" aria-labelledby="appearance-delete-title" aria-describedby="appearance-delete-description">
        <div class="confirm-icon"><Icon name="trash" size={18} /></div>
        <div class="confirm-copy">
          <h3 id="appearance-delete-title">{i18n.t('appearance.deleteTitle')}</h3>
          <p id="appearance-delete-description">{i18n.t('appearance.deleteConfirm', { name: pendingDelete.pack.name })}</p>
        </div>
        {#if errorMessage}<div class="inline-alert inline-alert--error confirm-error" role="alert">{errorMessage}</div>{/if}
        <footer class="confirm-actions">
          <button class="btn btn--secondary btn--sm" type="button" onclick={() => { pendingDelete = null; errorMessage = ''; }} disabled={Boolean(busyAction)}>{i18n.t('common.cancel')}</button>
          <button class="btn btn--danger btn--sm" type="button" onclick={() => void confirmRemoveTheme()} disabled={Boolean(busyAction)}>
            {busyAction.startsWith('delete:') ? i18n.t('common.loading') : i18n.t('appearance.delete')}
          </button>
        </footer>
      </div>
    </div>
  {/if}
</div>

<style>
  .appearance-tab { position: relative; gap: 18px; padding: 2px 4px 20px; color: var(--foreground); }
  .appearance-toolbar, .theme-card-body, .editor-header, .editor-footer, .wallpaper-heading, .toolbar-actions, .theme-actions, .wallpaper-actions { display: flex; align-items: center; }
  .appearance-toolbar { justify-content: space-between; gap: 16px; flex-wrap: wrap; }
  .toolbar-title { font-size: var(--text-lg); font-weight: var(--font-semibold); }
  .toolbar-subtitle { margin-top: 4px; color: var(--foreground-muted); font-size: var(--text-sm); }
  .toolbar-actions, .theme-actions, .wallpaper-actions { gap: 8px; }
  .system-appearance-row { display: grid; grid-template-columns: 34px minmax(0, 1fr) auto; align-items: center; gap: 11px; padding: 12px 2px; border-block: 1px solid var(--border-subtle); }
  .system-appearance-icon { width: 32px; height: 32px; display: grid; place-items: center; border-radius: var(--radius-md); background: var(--primary-muted); color: var(--primary); }
  .system-appearance-copy { min-width: 0; display: flex; flex-direction: column; gap: 3px; }
  .system-appearance-copy strong { font-size: var(--text-sm); }
  .system-appearance-copy span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--foreground-muted); font-size: var(--text-xs); }
  .theme-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(190px, 1fr)); gap: 12px; padding-bottom: 10px; }
  .theme-card { min-width: 0; overflow: hidden; border: 1px solid var(--border); border-radius: var(--radius-lg); background: var(--surface-1); }
  .theme-card.active { border-color: color-mix(in srgb, var(--primary) 72%, var(--border)); box-shadow: 0 0 0 1px color-mix(in srgb, var(--primary) 45%, transparent); }
  .theme-preview { position: relative; display: block; width: 100%; height: 104px; overflow: hidden; border: 0; border-bottom: 1px solid var(--border-subtle); cursor: pointer; }
  .preview-wallpaper { position: absolute; inset: -24px; background-repeat: no-repeat; background-size: cover; }
  .preview-wallpaper-dim { position: absolute; inset: 0; background: #000; }
  .preview-sidebar { position: absolute; z-index: 1; inset: 0 auto 0 0; width: 30%; }
  .preview-line { position: absolute; z-index: 1; left: 40%; top: 46%; width: 35%; height: 5px; border-radius: 2px; opacity: .62; }
  .preview-line--wide { top: 31%; width: 48%; opacity: .9; }
  .preview-accent { position: absolute; z-index: 1; left: 40%; top: 64%; width: 25%; height: 9px; border-radius: 3px; }
  .active-mark { position: absolute; z-index: 2; top: 8px; right: 8px; width: 22px; height: 22px; display: grid; place-items: center; border-radius: 50%; background: var(--primary); color: white; box-shadow: var(--shadow-sm); }
  .theme-card-body { justify-content: space-between; gap: 8px; min-height: 54px; padding: 9px 10px; }
  .theme-meta { min-width: 0; display: flex; flex-direction: column; gap: 3px; }
  .theme-meta strong { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: var(--text-sm); }
  .theme-meta span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--foreground-muted); font-size: var(--text-2xs); }
  .editor-overlay { position: absolute; inset: 0; z-index: 4; display: flex; justify-content: flex-end; background: color-mix(in srgb, var(--overlay) 65%, transparent); }
  .theme-editor { width: min(680px, 100%); height: 100%; display: flex; flex-direction: column; border-left: 1px solid var(--border); box-shadow: var(--shadow-xl); }
  .editor-header { justify-content: space-between; gap: 12px; padding: 17px 20px; border-bottom: 1px solid var(--border); }
  .editor-header h3 { margin: 0; font-size: var(--text-lg); }
  .editor-header p { margin: 4px 0 0; color: var(--foreground-muted); font-size: var(--text-xs); }
  .editor-content { flex: 1; min-height: 0; overflow: auto; padding: 0 20px; }
  .editor-section { padding: 17px 0; border-bottom: 1px solid var(--border-subtle); }
  .editor-section:last-child { border-bottom: 0; }
  .editor-section--identity { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; }
  label { min-width: 0; }
  .editor-section .form-label, .section-label { display: block; margin-bottom: 7px; font-size: var(--text-xs); }
  .material-options { display: grid; grid-template-columns: repeat(3, 1fr); gap: 8px; }
  .material-options button { min-width: 0; padding: 10px; text-align: left; border: 1px solid var(--border); border-radius: var(--radius-md); background: var(--surface-1); color: var(--foreground); cursor: pointer; }
  .material-options button.active { border-color: var(--primary); background: var(--primary-muted); }
  .material-options span, .material-options small { display: block; }
  .material-options small { margin-top: 4px; color: var(--foreground-muted); line-height: 1.35; }
  .color-grid { display: grid; grid-template-columns: repeat(3, 1fr); gap: 10px; }
  .color-grid label { display: flex; align-items: center; justify-content: space-between; gap: 8px; height: 34px; padding: 0 9px; border: 1px solid var(--border); border-radius: var(--radius-md); background: var(--surface-1); }
  .color-grid .form-label { margin: 0; }
  .range-row { display: grid; grid-template-columns: 112px 1fr 48px; align-items: center; gap: 10px; margin-top: 13px; }
  .range-row .form-label { margin: 0; }
  .range-row output { color: var(--foreground-muted); font-size: var(--text-xs); text-align: right; }
  .wallpaper-heading { justify-content: space-between; gap: 16px; }
  .wallpaper-heading small { color: var(--foreground-muted); }
  .editor-error { margin: 0 20px 12px; }
  .editor-footer { justify-content: flex-end; gap: 9px; padding: 13px 20px; border-top: 1px solid var(--border); }
  .confirm-overlay { position: absolute; inset: 0; z-index: 5; display: grid; place-items: center; padding: 20px; background: color-mix(in srgb, var(--overlay) 72%, transparent); }
  .confirm-dialog { width: min(400px, 100%); display: grid; grid-template-columns: 38px 1fr; gap: 12px; box-sizing: border-box; border: 1px solid var(--border); border-radius: var(--radius-lg); padding: 18px; box-shadow: var(--shadow-xl); color: var(--foreground); }
  .confirm-icon { width: 34px; height: 34px; display: grid; place-items: center; border-radius: var(--radius-md); background: var(--error-muted); color: var(--error); }
  .confirm-copy { min-width: 0; }
  .confirm-copy h3 { margin: 0; font-size: var(--text-base); }
  .confirm-copy p { margin: 7px 0 0; overflow-wrap: anywhere; color: var(--foreground-muted); font-size: var(--text-sm); line-height: 1.5; }
  .confirm-error { grid-column: 1 / -1; }
  .confirm-actions { grid-column: 1 / -1; display: flex; justify-content: flex-end; gap: 8px; margin-top: 4px; }
  @media (max-width: 720px) {
    .theme-grid { grid-template-columns: repeat(auto-fill, minmax(160px, 1fr)); }
    .editor-section--identity, .material-options, .color-grid { grid-template-columns: 1fr; }
    .wallpaper-heading { align-items: flex-start; flex-direction: column; }
    .range-row { grid-template-columns: 92px 1fr 42px; }
  }
</style>
