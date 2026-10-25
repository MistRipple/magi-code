<script lang="ts">
  import { i18n } from '../stores/i18n.svelte';
  import { confirmMessage } from '../stores/confirm-dialog.svelte';
  import {
    applyPluginCandidate,
    authorizeAndActivatePlugin,
    deactivatePlugin,
    disablePlugin,
    fetchPluginCenter,
    installBundledPlugin,
    installFromPluginCenter,
    listBundledPlugins,
    listInstalledPlugins,
    previewPluginCandidate,
    savePluginCenterConfig,
    uninstallPlugin,
    type PluginCandidate,
  } from '../web/plugin-api';
  import PluginDetailPane from './PluginDetailPane.svelte';
  import PluginGuidePage from './PluginGuidePage.svelte';
  import Icon from './Icon.svelte';
  import SettingsDetailPage from './SettingsDetailPage.svelte';
  import { loadViewDraft, saveViewDraft, clearViewDraft } from '../lib/view-drafts';
  import { openSettings } from '../stores/shell-ui.svelte';
  import { buildStoreItems, filterStoreItems, paginate, storeGroup, type StoreFilter, type StoreGroup, type StoreItem } from '../lib/plugin-store';
  import { pluginRegistryRevision } from '../stores/plugin-registry.svelte';
  import type { PluginCandidatePreview, PluginCenterProjection, PluginList, PluginManifest, PluginPermission } from '../shared/app-server-protocol.generated';

  let list = $state<PluginList | null>(null);
  let bundledPlugins = $state<PluginManifest[]>([]);
  let address = $state('');
  let busy = $state(false);
  let message = $state('');
  let now = $state(Date.now());
  let center = $state<PluginCenterProjection | null>(null);
  let centerUrl = $state('');
  let centerKeyId = $state('');
  let centerPublicKey = $state('');
  let centerConfigOpen = $state(false);
  let centerSynced = false;
  let centerDraftBaseline = $state('');
  let listRequestSeq = 0;
  let centerRequestSeq = 0;
  let guideOpen = $state(false);
  /** 已通过准备阶段、等待用户确认的候选包；确认前不写入任何安装状态。 */
  let staged = $state<{ candidate: PluginCandidate; preview: PluginCandidatePreview } | null>(null);
  const plugins = $derived(list?.plugins ?? []);
  const anyDraining = $derived(plugins.some((plugin) => plugin.draining.length > 0));

  async function refresh(): Promise<void> {
    const requestSeq = ++listRequestSeq;
    const [installed, bundled] = await Promise.all([listInstalledPlugins(), listBundledPlugins()]);
    if (requestSeq !== listRequestSeq) return;
    list = installed;
    bundledPlugins = bundled;
    await refreshCenter();
  }

  async function refreshCenter(): Promise<void> {
    const requestSeq = ++centerRequestSeq;
    try {
      const next = await fetchPluginCenter();
      if (requestSeq !== centerRequestSeq) return;
      center = next;
      // 只在首次读取和保存后同步输入框，避免用户正在输入时被周期刷新覆盖。
      if (!centerSynced) {
        centerSynced = true;
        centerUrl = center.config.url;
        const key = center.config.trustedKeys[0];
        centerKeyId = key?.keyId ?? '';
        centerPublicKey = key?.publicKey ?? '';
        centerDraftBaseline = JSON.stringify(center.config);
        const restored = loadViewDraft('plugin-sources', centerDraftBaseline, { centerUrl, centerKeyId, centerPublicKey, address }, 'memory');
        ({ centerUrl, centerKeyId, centerPublicKey, address } = restored);
      }
    } catch (error) {
      if (requestSeq !== centerRequestSeq) return;
      // 插件中心不可用不应阻止管理已安装插件。
      message = describe(error);
    }
  }

  async function saveCenterConfig(): Promise<void> {
    await run(async () => {
      await savePluginCenterConfig({
        url: centerUrl.trim(),
        trustedKeys: centerKeyId.trim() && centerPublicKey.trim()
          ? [{ keyId: centerKeyId.trim(), publicKey: centerPublicKey.trim() }]
          : [],
      });
      centerConfigOpen = false;
      clearViewDraft('plugin-sources');
      centerSynced = false;
    });
  }

  function describe(error: unknown): string {
    return error instanceof Error ? error.message : String(error);
  }

  // 首次进入以及注册表在别处变化（其他客户端、窗口重新获得焦点）后重新读取 daemon 事实。
  $effect(() => {
    void pluginRegistryRevision();
    void refresh().catch((error) => { message = describe(error); });
  });

  // 停用与升级会等待在途运行，期间持续读取 daemon 的排空状态并倒计时，而不是让界面静止。
  $effect(() => {
    if (!busy && !anyDraining) return;
    const timer = window.setInterval(() => {
      now = Date.now();
      void refresh().catch(() => undefined);
    }, 1000);
    return () => window.clearInterval(timer);
  });

  /** 执行一次改变状态的操作并刷新 daemon 事实；成功由界面状态本身体现，只有失败才提示。 */
  async function run(operation: () => Promise<void>): Promise<void> {
    busy = true;
    message = '';
    try {
      await operation();
      await refresh();
    } catch (error) {
      message = describe(error);
      await refresh().catch(() => undefined);
    } finally {
      busy = false;
    }
  }

  async function readArchive(event: Event): Promise<ArrayBuffer | null> {
    const input = event.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    input.value = '';
    return file ? await file.arrayBuffer() : null;
  }

  async function stage(candidate: PluginCandidate): Promise<void> {
    busy = true;
    message = '';
    staged = null;
    try {
      const preview = await previewPluginCandidate(candidate);
      if (preview.operation === 'unchanged') message = i18n.t('settings.plugins.candidate.unchanged');
      else staged = { candidate, preview };
    } catch (error) {
      message = describe(error);
    } finally {
      busy = false;
    }
  }

  async function stageFile(event: Event): Promise<void> {
    const archive = await readArchive(event);
    if (archive) await stage({ source: 'local:browser-upload', archive });
  }

  async function stageAddress(): Promise<void> {
    const source = address.trim();
    if (!source) return;
    await stage({ source: source.includes(':') ? source : `address:${source}` });
  }

  async function confirmStaged(): Promise<void> {
    const pending = staged;
    if (!pending) return;
    await run(async () => {
      await applyPluginCandidate(pending.candidate, pending.preview);
      staged = null;
      address = '';
    });
  }

  function permissionText(permission: PluginPermission): string {
    return `${permission.kind}: ${permission.scope}${permission.targets.length ? `, ${permission.targets.join(', ')}` : ''}`;
  }

  const hasCatalog = $derived(Boolean(center && center.entries.length > 0));

  /** 第一方插件的名称与说明随界面语言翻译；其他插件显示清单原文。 */
  function manifestText(manifest: PluginManifest, field: 'name' | 'description'): string {
    const key = `settings.plugins.builtin.${manifest.id}.${field}`;
    const text = i18n.t(key);
    return text === key ? manifest[field] : text;
  }

  // ---- 商店：左侧列表（合并已安装、内置与插件中心，搜索 + 过滤 + 分页），右侧是所选插件的详情 ----
  const PAGE_SIZE = 20;
  const GROUPS: StoreGroup[] = ['updatable', 'installed', 'available'];
  let filter = $state<StoreFilter>('all');
  let query = $state('');
  let page = $state(1);
  let selectedId = $state<string | null>(null);
  let sourcesOpen = $state(false);

  const items = $derived(buildStoreItems(plugins, bundledPlugins, center?.entries ?? [], manifestText));
  const filtered = $derived(filterStoreItems(items, filter, query));
  const view = $derived(paginate(filtered, page, PAGE_SIZE));
  /** 默认选中当前页第一项；所选插件不在当前页（翻页、过滤后）时回到第一项。 */
  const selected = $derived<StoreItem | null>(view.rows.find((entry) => entry.id === selectedId) ?? view.rows[0] ?? null);
  const groups = $derived(
    GROUPS.map((group) => ({
      group,
      total: filtered.filter((entry) => storeGroup(entry) === group).length,
      rows: view.rows.filter((entry) => storeGroup(entry) === group),
    })).filter((entry) => entry.rows.length > 0),
  );
  const counts = $derived({
    all: items.length,
    installed: items.filter((entry) => entry.installed).length,
    available: items.filter((entry) => !entry.installed).length,
    updatable: items.filter((entry) => entry.updatable).length,
  });

  // 过滤条件变化后回到第一页。
  $effect(() => { void filter; void query; page = 1; });

  async function uninstallItem(item: StoreItem): Promise<void> {
    const { id, name } = item;
    if (!(await confirmMessage(i18n.t('overlay.uninstallBody', { name }), {
      title: i18n.t('settings.plugins.uninstall'), confirmLabel: i18n.t('settings.plugins.uninstall'), tone: 'danger',
    }))) return;
    await run(() => uninstallPlugin(id));
  }

  async function installItem(item: StoreItem): Promise<void> {
    // 内置包优先：第一方插件只接受内置来源，且离线可装。
    if (item.bundled) {
      await run(() => installBundledPlugin(item.id));
    } else if (item.catalog) {
      await run(() => installFromPluginCenter(item.id, item.catalog!.version));
    }
  }

  async function updateFromCenter(item: StoreItem): Promise<void> {
    if (!item.catalog) return;
    await run(() => installFromPluginCenter(item.id, item.catalog!.version));
  }

  $effect(() => {
    if (centerSynced && centerDraftBaseline) saveViewDraft('plugin-sources', centerDraftBaseline, { centerUrl, centerKeyId, centerPublicKey, address }, 'memory');
  });

  function closeSources(): void {
    sourcesOpen = false;
  }

  async function submitAddress(): Promise<void> {
    closeSources();
    await stageAddress();
  }

  async function submitFile(event: Event): Promise<void> {
    closeSources();
    await stageFile(event);
  }
</script>

<div hidden={sourcesOpen || guideOpen}>
<div class="apple-manager">
<div class="apple-scroller-proxy">
  {#if message}
    <div class="settings-notice plugins-status" data-testid="plugin-message" role="status"><Icon name="info" size={14} /><span>{message}</span></div>
  {/if}

  {#if staged}
    {@const preview = staged.preview}
    <div class="settings-section" data-testid="plugin-candidate" data-operation={preview.operation}>
      <div class="settings-card settings-card--accent">
        <div class="settings-card-head">
          <div class="settings-icon-tile" aria-hidden="true"><Icon name="download" size={15} /></div>
          <div class="settings-card-title">
            <div class="settings-card-title-line">
              <span class="settings-card-name">{manifestText(preview.manifest, 'name')}</span>
              <span class="settings-chip">
                {preview.installedVersion
                  ? i18n.t('settings.plugins.candidate.versionChange', { from: preview.installedVersion, to: preview.manifest.version })
                  : `v${preview.manifest.version}`}
              </span>
              <span class="settings-chip settings-chip--accent">{preview.operation === 'upgrade' ? i18n.t('settings.plugins.upgrade') : i18n.t('settings.plugins.installBundled')}</span>
            </div>
            <span class="settings-row-desc">{manifestText(preview.manifest, 'description') || preview.manifest.id}</span>
          </div>
        </div>
        <div class="settings-card-body">
          <div class="plugin-perm" data-testid="candidate-added-permissions">
            <span class="settings-row-title">{i18n.t('settings.plugins.candidate.added')}</span>
            <div class="settings-chips">
              {#each preview.addedPermissions as permission}
                <span class="settings-chip settings-mono">{permissionText(permission)}</span>
              {:else}
                <span class="settings-row-desc">{i18n.t('settings.plugins.candidate.noneAdded')}</span>
              {/each}
            </div>
          </div>
          {#if preview.removedPermissions.length > 0}
            <div class="plugin-perm">
              <span class="settings-row-title">{i18n.t('settings.plugins.candidate.removed')}</span>
              <div class="settings-chips">
                {#each preview.removedPermissions as permission}
                  <span class="settings-chip settings-mono">{permissionText(permission)}</span>
                {/each}
              </div>
            </div>
          {/if}
          {#if preview.dataSchemaFrom !== undefined}
            <div class="settings-notice" class:settings-notice--error={preview.dataSchemaBlocked} role={preview.dataSchemaBlocked ? 'alert' : undefined}>
              {preview.dataSchemaBlocked
                ? i18n.t('settings.plugins.candidate.dataBlocked', { from: preview.dataSchemaFrom, to: preview.manifest.dataSchemaVersion })
                : preview.dataConversion
                  ? i18n.t('settings.plugins.candidate.dataConverts', { from: preview.dataSchemaFrom, to: preview.manifest.dataSchemaVersion })
                  : i18n.t('settings.plugins.candidate.dataChanged', { from: preview.dataSchemaFrom, to: preview.manifest.dataSchemaVersion })}
            </div>
          {/if}
          <div class="settings-row-desc">
            {preview.operation === 'upgrade' ? i18n.t('settings.plugins.candidate.upgradeHint') : i18n.t('settings.plugins.candidate.installHint')}
            <span class="settings-mono" title={preview.digest}>{i18n.t('settings.plugins.digest', { digest: preview.digest.slice(0, 12) })}</span>
          </div>
        </div>
        <div class="settings-card-foot">
          <button class="btn btn--sm btn--secondary" data-action="cancel-candidate" onclick={() => { staged = null; }} disabled={busy}>{i18n.t('settings.plugins.candidate.cancel')}</button>
          <button class="btn btn--sm btn--primary" data-action="confirm-candidate" onclick={() => void confirmStaged()} disabled={busy || preview.dataSchemaBlocked}>
            {preview.operation === 'upgrade' ? i18n.t('settings.plugins.candidate.confirmUpgrade') : i18n.t('settings.plugins.candidate.confirmInstall')}
          </button>
        </div>
      </div>
    </div>
  {/if}

  <div class="settings-section store-section">
    <div class="settings-section-header">
      <div>
        <div class="settings-section-title">{i18n.t('settings.plugins.store.title')}</div>
        <div class="settings-section-desc">{i18n.t('settings.plugins.store.desc')}</div>
      </div>
      <div class="settings-section-actions">
        <button class="btn btn--sm btn--secondary" data-testid="plugin-guide-toggle" onclick={() => { guideOpen = true; }}>
          <Icon name="file-text" size={13} />
          <span>{i18n.t('settings.plugins.guide.toggle')}</span>
        </button>
        <button class="btn btn--sm btn--secondary" data-testid="plugin-sources-open" onclick={() => { sourcesOpen = true; }}>
          <Icon name="plus" size={13} />
          <span>{i18n.t('settings.plugins.store.sources')}</span>
        </button>
      </div>
    </div>

    {#if center?.error}
      <div class="settings-notice settings-notice--warn store-center-error" data-testid="plugin-center-error" role="status">
        <Icon name="alert-triangle" size={14} />
        <span>{i18n.t('settings.plugins.center.loadFailed', { reason: center.error })}</span>
        <button class="plugin-link" onclick={() => void refreshCenter()}>{i18n.t('settings.plugins.center.retry')}</button>
      </div>
    {/if}

    <div class="store-toolbar">
      <input class="form-input store-search" data-testid="plugin-search" type="search" bind:value={query} placeholder={i18n.t('settings.plugins.store.search')} />
      <div class="settings-subtabs store-filters" role="tablist">
        {#each [['all', 'filter.all'], ['installed', 'filter.installed'], ['available', 'filter.available'], ['updatable', 'filter.updatable']] as [id, key] (id)}
          {#if id !== 'updatable' || counts.updatable > 0}
            <button type="button" role="tab" class="settings-subtab" class:active={filter === id} aria-selected={filter === id} data-testid={`plugin-filter-${id}`} onclick={() => { filter = id as StoreFilter; }}>
              {i18n.t(`settings.plugins.store.${key}`)} <span class="store-count">{counts[id as StoreFilter]}</span>
            </button>
          {/if}
        {/each}
      </div>
    </div>

    <div class="store" data-testid="plugin-store">
      <aside class="store-list">
        <div class="store-scroll" data-testid="plugin-list">
          {#each groups as entry (entry.group)}
            <div class="store-group" data-group={entry.group}>
              <span>{i18n.t(`settings.plugins.store.group.${entry.group}`)}</span>
              <span class="store-count">{entry.total}</span>
            </div>
            {#each entry.rows as item (item.id)}
              <button
                type="button"
                class="store-item"
                class:selected={selected?.id === item.id}
                aria-current={selected?.id === item.id ? 'true' : undefined}
                data-testid="plugin-row"
                data-plugin-id={item.id}
                data-status={item.status}
                data-group={entry.group}
                onclick={() => { selectedId = item.id; }}
              >
                <span class="settings-icon-tile" aria-hidden="true"><Icon name="plug" size={15} /></span>
                <span class="store-item-copy">
                  <span class="store-item-name">{item.name}</span>
                  <span class="store-item-desc">{item.description}</span>
                </span>
                <span class="store-item-state" data-state={entry.group === 'updatable' ? 'updatable' : item.status}>
                  {#if entry.group === 'updatable'}
                    <Icon name="download" size={14} />
                  {:else if item.status === 'working'}
                    <Icon name="refresh" size={14} />
                  {:else if item.installed}
                    <Icon name="check-circle" size={14} />
                  {:else}
                    <Icon name="plus" size={14} />
                  {/if}
                  <span class="store-item-version">v{item.version}</span>
                </span>
              </button>
            {/each}
          {:else}
            <div class="settings-empty">
              <span>{items.length === 0 ? i18n.t('settings.plugins.empty') : i18n.t('settings.plugins.store.noMatch')}</span>
            </div>
          {/each}
        </div>

        {#if view.pages > 1}
          <div class="store-pager" data-testid="plugin-pager">
            <span class="settings-row-desc">{i18n.t('settings.plugins.store.pageSummary', { from: (view.page - 1) * PAGE_SIZE + 1, to: Math.min(view.page * PAGE_SIZE, filtered.length), total: filtered.length })}</span>
            <div class="store-pager-actions">
              <button class="btn btn--sm btn--secondary" data-testid="plugin-page-prev" onclick={() => { page = view.page - 1; }} disabled={view.page <= 1}>{i18n.t('settings.plugins.store.prev')}</button>
              <span class="settings-row-desc">{view.page} / {view.pages}</span>
              <button class="btn btn--sm btn--secondary" data-testid="plugin-page-next" onclick={() => { page = view.page + 1; }} disabled={view.page >= view.pages}>{i18n.t('settings.plugins.store.next')}</button>
            </div>
          </div>
        {/if}
      </aside>

      <section class="store-detail" data-testid="plugin-detail">
        {#if selected}
          {#key selected.id}
            <PluginDetailPane
              item={selected}
              {now}
              {busy}
              onInstall={() => void installItem(selected)}
              onUpdateFromCenter={() => void updateFromCenter(selected)}
              onUpgradeFile={(event) => void stageFile(event)}
              onUninstall={() => void uninstallItem(selected)}
              onActivate={() => void run(() => authorizeAndActivatePlugin(selected.id, selected.installed!.manifest))}
              onDeactivate={() => void run(() => deactivatePlugin(selected.id))}
              onDisable={() => void run(() => disablePlugin(selected.id))}
              onOpenSettings={openSettings}
            />
          {/key}
        {:else}
          <div class="settings-empty"><span>{i18n.t('settings.plugins.detail.empty')}</span></div>
        {/if}
      </section>
    </div>

    {#if list?.lifecycle}
      <p class="settings-hint">
        {i18n.t('settings.plugins.drainHint', {
          drain: Math.round(list.lifecycle.drainTimeoutMs / 1000),
          settle: Math.round(list.lifecycle.cancelSettleTimeoutMs / 1000),
        })}
      </p>
    {/if}
  </div>
</div>
</div>

</div>

{#if sourcesOpen}
  <SettingsDetailPage title={i18n.t('settings.plugins.store.sources')} {busy} onClose={closeSources}>
    {#snippet header()}
      <div class="sources-header">
        <h3>{i18n.t('settings.plugins.store.sources')}</h3>
      </div>
    {/snippet}
    <p class="settings-hint" role="status">{i18n.t('overlay.draftMemory')}</p>
    <div class="sources-body">
      <section>
        <h4>{i18n.t('settings.plugins.center.title')}</h4>
        <div class="settings-row-desc">{i18n.t('settings.plugins.section.availableDesc')}</div>
        <div class="settings-inline-form">
          <input class="form-input" data-testid="plugin-center-url" bind:value={centerUrl} placeholder={i18n.t('settings.plugins.center.urlPlaceholder')} disabled={busy} />
          <button class="btn btn--sm btn--primary" data-testid="plugin-center-save" disabled={busy} onclick={() => void saveCenterConfig()}>{i18n.t('settings.plugins.center.save')}</button>
        </div>
        {#if center}
          <div class="settings-row-desc">
            <span class="settings-chip" class:settings-chip--ok={center.verified} class:settings-chip--warn={!center.verified && Boolean(center.config.url)}>
              {center.verified ? i18n.t('settings.plugins.center.verified') : i18n.t('settings.plugins.center.unverified')}
            </span>
          </div>
        {/if}
        {#if center && hasCatalog && !center.verified}
          <div class="settings-notice settings-notice--warn" data-testid="plugin-center-unsigned"><Icon name="alert-triangle" size={14} /><span>{i18n.t('settings.plugins.center.unsigned')}</span></div>
        {/if}
        {#if center?.error}
          <div class="settings-notice settings-notice--error" role="alert"><Icon name="alert-circle" size={14} /><span>{center.error}</span></div>
        {/if}
        <button class="plugin-link" data-testid="plugin-center-advanced" aria-expanded={centerConfigOpen} onclick={() => { centerConfigOpen = !centerConfigOpen; }}>
          {i18n.t('settings.plugins.center.advanced')}
        </button>
        {#if centerConfigOpen}
          <div class="settings-row-desc">{i18n.t('settings.plugins.center.advancedHint')}</div>
          <div class="settings-inline-form">
            <input class="form-input" bind:value={centerKeyId} placeholder={i18n.t('settings.plugins.center.keyIdPlaceholder')} disabled={busy} />
            <input class="form-input" bind:value={centerPublicKey} placeholder={i18n.t('settings.plugins.center.publicKeyPlaceholder')} disabled={busy} />
          </div>
        {/if}
      </section>
      <section>
        <h4>{i18n.t('settings.plugins.section.other')}</h4>
        <div class="settings-row-desc">{i18n.t('settings.plugins.section.otherDesc')}</div>
        <div class="settings-inline-form">
          <input class="form-input" data-testid="plugin-address-input" bind:value={address} placeholder={i18n.t('settings.plugins.addressPlaceholder')} disabled={busy} onkeydown={(event) => { if (event.key === 'Enter') void submitAddress(); }} />
          <button class="btn btn--sm btn--primary" data-testid="plugin-address-install" onclick={() => void submitAddress()} disabled={busy || !address.trim()}>{i18n.t('settings.plugins.installAddress')}</button>
        </div>
        <div class="sources-zip">
          <span class="settings-row-desc">{i18n.t('settings.plugins.installLocalDesc')}</span>
          <label class="btn btn--sm btn--secondary" class:plugin-disabled={busy}>
            {i18n.t('settings.plugins.chooseZip')}
            <input type="file" accept=".zip,application/zip" hidden onchange={submitFile} disabled={busy} />
          </label>
        </div>
      </section>
    </div>
  </SettingsDetailPage>
{/if}

{#if guideOpen}
  <PluginGuidePage onClose={() => { guideOpen = false; }} />
{/if}

<style>
  .store-section { display: flex; flex: 1 0 auto; flex-direction: column; min-height: 0; }
  .store-toolbar { display: flex; align-items: center; justify-content: space-between; gap: var(--space-3); flex-wrap: wrap; margin: var(--space-3) 0 var(--space-2); }
  .store-search { width: 240px; max-width: 100%; }
  .store-filters { margin-bottom: 0; border-bottom: 0; }
  .store-center-error { margin-top: var(--space-3); }
  .store-count { margin-left: 2px; color: var(--foreground-muted); font-size: var(--text-xs); font-variant-numeric: tabular-nums; }

  /* 左右分栏：整体是一块带边框的面板，左侧列表独立滚动，右侧显示所选插件。 */
  .store { display: grid; grid-template-columns: 300px minmax(0, 1fr); grid-template-rows: minmax(0, 1fr); flex: 1 1 0; min-height: 180px; border: var(--magi-hairline) solid var(--border); border-radius: 10px; overflow: hidden; }
  .store-list { display: flex; flex-direction: column; min-height: 0; border-right: var(--magi-hairline) solid var(--border); }
  .store-scroll { flex: 1; min-height: 0; overflow-y: auto; overscroll-behavior: contain; padding: 4px 6px 8px; }
  .store-group { display: flex; align-items: center; justify-content: space-between; padding: 12px 8px 4px; font-size: var(--text-xs); font-weight: var(--font-medium); color: var(--foreground-muted); letter-spacing: 0.02em; }
  .store-group[data-group='updatable'] { color: var(--primary); }
  .store-item { display: flex; align-items: center; gap: var(--space-2); width: 100%; padding: 8px; border: 0; border-radius: 8px; background: transparent; color: inherit; text-align: left; cursor: pointer; }
  .store-item:hover { background: var(--surface-hover); }
  .store-item.selected { background: var(--primary-muted); }
  .store-item:focus-visible { outline: 2px solid var(--primary); outline-offset: -2px; }
  .store-item-copy { min-width: 0; flex: 1; display: grid; gap: 2px; }
  .store-item-name { font-size: var(--text-sm); font-weight: var(--font-medium); color: var(--foreground); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .store-item-desc { font-size: var(--text-xs); color: var(--foreground-muted); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  /* 状态一眼可辨：有更新用主题色向下箭头，已启用为绿色对勾，已安装未启用为灰色对勾，未安装为加号。 */
  .store-item-state { display: inline-flex; flex-direction: column; align-items: center; gap: 2px; flex: 0 0 auto; min-width: 40px; color: var(--foreground-muted); }
  .store-item-state[data-state='updatable'] { color: var(--primary); }
  .store-item-state[data-state='active'] { color: var(--success); }
  .store-item-state[data-state='working'] { color: var(--primary); }
  .store-item-version { font-size: var(--text-xs); font-variant-numeric: tabular-nums; }
  .store-pager { display: grid; gap: var(--space-2); padding: var(--space-2) var(--space-3); border-top: var(--magi-hairline) solid var(--border); }
  .store-pager-actions { display: flex; align-items: center; justify-content: space-between; gap: var(--space-2); }
  .store-detail { min-width: 0; min-height: 0; overflow-y: auto; }
  @container settings-content (max-width: 720px) {
    .store { grid-template-columns: minmax(0, 1fr); grid-template-rows: auto auto; flex: 1 0 auto; }
    .store-list { border-right: 0; border-bottom: var(--magi-hairline) solid var(--border); max-height: 40dvh; min-height: 180px; }
  }
  .plugin-perm { display: grid; gap: var(--space-2); }
  .plugin-link { justify-self: start; align-self: flex-start; padding: 0; border: 0; background: transparent; color: var(--foreground-muted); font-size: var(--text-xs); cursor: pointer; }
  .plugin-link:hover { color: var(--primary); }
  .plugin-disabled { opacity: 0.45; pointer-events: none; }
  .plugins-status { margin-top: var(--space-3); }
  .sources-header { display: flex; align-items: center; justify-content: space-between; width: 100%; }
  .sources-header h3 { margin: 0; }
  .sources-body { display: grid; gap: var(--space-5); }
  .sources-body section { display: grid; gap: var(--space-2); }
  .sources-body h4 { margin: 0; font-size: var(--text-sm); font-weight: var(--font-bold); }
  .sources-zip { display: flex; align-items: center; justify-content: space-between; gap: var(--space-3); }
</style>
