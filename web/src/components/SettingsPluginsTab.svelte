<script lang="ts">
  import { onDestroy, tick, untrack } from 'svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { confirmMessage } from '../stores/confirm-dialog.svelte';
  import {
    applyPluginCandidate,
    cancelPluginInstall,
    disablePlugin,
    enablePlugin,
    listInstalledPlugins,
    installUpstreamPlugin,
    previewPluginCandidate,
    previewUpstreamPlugin,
    restartPluginHost,
    searchUpstreamPlugins,
    uninstallPlugin,
    type PluginCandidate,
  } from '../web/plugin-api';
  import PluginDetailPane from './PluginDetailPane.svelte';
  import PluginIcon from './PluginIcon.svelte';
  import PluginImpactNotices from './PluginImpactNotices.svelte';
  import PluginGuidePage from './PluginGuidePage.svelte';
  import Icon from './Icon.svelte';
  import SettingsDetailPage from './SettingsDetailPage.svelte';
  import { loadViewDraft, saveViewDraft } from '../lib/view-drafts';
  import { openSettings, shellUi } from '../stores/shell-ui.svelte';
  import { buildStoreItems, filterStoreItems, installedUpstreamRequest, paginate, storeGroup, type StoreFilter, type StoreGroup, type StoreItem } from '../lib/plugin-store';
  import { pluginRegistryRevision, pluginProjection } from '../stores/plugin-registry.svelte';
  import { cachedUpstreamProjection, rememberUpstreamProjection } from '../lib/plugin-upstream-cache';
  import type { PluginCandidatePreview, PluginEcosystem, PluginUpstreamRequest, PluginList, PluginManifest, PluginUpstreamPreview, PluginUpstreamProjection } from '../shared/app-server-protocol.generated';

  // 先用全局投影里已有的已安装插件起步，进入页面时列表不是空的，也不需要等一次请求。
  let list = $state<PluginList | null>(pluginProjection().loaded ? { plugins: pluginProjection().installed } : null);
  let address = $state(loadViewDraft('plugin-sources', 'archive', { address: '' }, 'memory').address);
  let busy = $state(false);
  let busyLabel = $state('');
  let previewController = $state<AbortController | null>(null);
  let installRequestId = $state<string | null>(null);
  let cancellingInstall = $state(false);
  let installApplying = $state(false);
  const canCancelInstall = $derived(installRequestId !== null && !installApplying);
  let detailPane: HTMLElement;
  let message = $state('');
  let listRequestSeq = 0;
  let guideOpen = $state(false);
  /** 已通过准备阶段、等待用户确认的候选包；确认前不写入任何安装状态。 */
  type Staged =
    | { kind: 'file'; candidate: PluginCandidate; preview: PluginCandidatePreview }
    | { kind: 'upstream'; ecosystem: PluginEcosystem; preview: PluginUpstreamPreview };
  let staged = $state<Staged | null>(null);
  let upstream = $state<PluginUpstreamProjection | null>(null);
  let upstreamSeq = 0;
  /** 当前 `upstream` 对应的搜索词；搜索词不变时的刷新沿用已有结果。 */
  let upstreamQuery: string | null = null;
  let upstreamLoading = $state(false);
  let searchRetry = $state(0);
  const plugins = $derived(list?.plugins ?? []);

  async function refresh(): Promise<void> {
    const requestSeq = ++listRequestSeq;
    const installed = await listInstalledPlugins();
    if (requestSeq !== listRequestSeq) return;
    list = installed;
  }

  /** 搜索上游生态的官方 npm 发布。失败时仍可管理已安装插件。 */
  async function refreshUpstream(text: string, requestSeq: number, signal: AbortSignal): Promise<void> {
    try {
      const next = await searchUpstreamPlugins(text, signal);
      if (requestSeq !== upstreamSeq) return;
      upstream = next;
      rememberUpstreamProjection(text, next);
    } catch (error) {
      if (signal.aborted || requestSeq !== upstreamSeq) return;
      // 静默刷新失败时保留已有内容，只附上错误；不能让列表被清空。
      upstream = { registry: upstream?.registry ?? '', entries: upstream?.entries ?? [], error: describe(error) };
    } finally {
      if (requestSeq === upstreamSeq) upstreamLoading = false;
    }
  }

  function describe(error: unknown): string {
    return error instanceof Error ? error.message : String(error);
  }

  // 首次进入以及注册表在别处变化（其他客户端、窗口重新获得焦点）后重新读取 daemon 事实。
  $effect(() => {
    void pluginRegistryRevision();
    void refresh().catch((error) => { message = describe(error); });
  });

  /** 执行一次改变状态的操作并刷新 daemon 事实；成功由界面状态本身体现，只有失败才提示。 */
  async function revealFeedback(): Promise<void> {
    await tick();
    if (!detailPane) return;
    for (const content of detailPane.querySelectorAll('.pane-scroll, .candidate-scroll')) content.scrollTop = 0;
    detailPane.scrollIntoView({ block: 'nearest' });
  }

  async function run(operation: () => Promise<void>, label = i18n.t('common.loading')): Promise<void> {
    if (busy) return;
    busy = true;
    busyLabel = label;
    message = '';
    try {
      await operation();
      await refresh();
    } catch (error) {
      message = describe(error);
      await refresh().catch(() => undefined);
      await revealFeedback();
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

  function cancelPreview(): void {
    if (previewController) {
      previewController.abort();
      previewController = null;
      busy = false;
    }
    staged = null;
  }

  async function prepareCandidate(load: (signal: AbortSignal) => Promise<Staged>): Promise<void> {
    if (busy) return;
    const controller = new AbortController();
    previewController = controller;
    busy = true;
    busyLabel = i18n.t('settings.plugins.candidate.preparing');
    message = '';
    staged = null;
    try {
      const next = await load(controller.signal);
      if (controller.signal.aborted) return;
      const preview = next.kind === 'upstream' ? next.preview.candidate : next.preview;
      if (preview.operation === 'unchanged') message = i18n.t('settings.plugins.candidate.unchanged');
      else staged = next;
      await revealFeedback();
    } catch (error) {
      if (controller.signal.aborted) return;
      message = describe(error);
      await revealFeedback();
    } finally {
      if (previewController === controller) { previewController = null; busy = false; }
    }
  }

  async function stageUpstream(entry: PluginUpstreamRequest): Promise<void> {
    await prepareCandidate(async (signal) => ({ kind: 'upstream', ecosystem: entry.ecosystem, preview: await previewUpstreamPlugin(entry, signal) }));
  }

  async function stage(candidate: PluginCandidate): Promise<void> {
    await prepareCandidate(async (signal) => ({ kind: 'file', candidate, preview: await previewPluginCandidate(candidate, signal) }));
  }

  async function stageFile(event: Event): Promise<void> {
    const archive = await readArchive(event);
    if (archive) await stage({ source: 'local:browser-upload', archive });
  }

  async function stageAddress(): Promise<void> {
    const source = address.trim();
    if (!source) return;
    await stage({ source: source.startsWith('local:') || source.startsWith('address:') ? source : `address:${source}` });
  }

  async function confirmStaged(): Promise<void> {
    const pending = staged;
    if (!pending) return;
    await run(async () => {
      if (pending.kind === 'upstream') {
        if (!await submitUpstream(pending.preview, pending.ecosystem)) return;
      }
      else await applyPluginCandidate(pending.candidate, pending.preview);
      staged = null;
      address = '';
      const preview = pending.kind === 'upstream' ? pending.preview.candidate : pending.preview;
      selectedId = preview.manifest.id;
      filter = 'all';
      page = 1;
    }, i18n.t('settings.plugins.candidate.installing'));
  }

  async function submitUpstream(preview: PluginUpstreamPreview, ecosystem: PluginEcosystem): Promise<boolean> {
    const id = Array.from(crypto.getRandomValues(new Uint8Array(16)), byte => byte.toString(16).padStart(2, '0')).join('');
    installRequestId = id;
    try {
      const applied = await installUpstreamPlugin(preview, ecosystem, id);
      if (!applied) {
        message = i18n.t('settings.plugins.candidate.cancelled');
        await revealFeedback();
      }
      return applied;
    } finally {
      installRequestId = null;
      cancellingInstall = false;
      installApplying = false;
    }
  }

  async function cancelActiveInstall(): Promise<void> {
    const id = installRequestId;
    if (!id || cancellingInstall || installApplying) return;
    cancellingInstall = true;
    message = '';
    try {
      const result = await cancelPluginInstall(id);
      if (id !== installRequestId) return;
      if (result === 'too-late') {
        installApplying = true;
        busyLabel = i18n.t('settings.plugins.candidate.applying');
      } else if (result === 'not-running') {
        message = i18n.t('settings.plugins.candidate.cancelPending');
        await revealFeedback();
      }
    } catch (error) {
      if (id !== installRequestId) return;
      message = describe(error);
      await revealFeedback();
    } finally {
      if (id === installRequestId) cancellingInstall = false;
    }
  }

  /** 插件的名称与说明显示清单原文。 */
  function manifestText(manifest: PluginManifest, field: 'name' | 'description'): string {
    return manifest[field];
  }

  // ---- 商店：左侧列表（合并已安装、内置与插件中心，搜索 + 过滤 + 分页），右侧是所选插件的详情 ----
  const PAGE_SIZE = 20;
  const GROUPS: StoreGroup[] = ['updatable', 'installed', 'available'];
  let filter = $state<StoreFilter>('all');
  let query = $state('');
  let page = $state(1);
  let selectedId = $state<string | null>(null);
  let sourcesOpen = $state(false);

  $effect(() => {
    const id = shellUi.settingsRequestedPlugin;
    const plugin = plugins.find(plugin => plugin.manifest.id === id);
    if (!id || !plugin) return;
    cancelPreview();
    query = plugin.manifest.name;
    filter = 'installed';
    selectedId = id;
    shellUi.settingsRequestedPlugin = null;
  });

  const items = $derived(buildStoreItems(plugins, upstream?.entries ?? [], manifestText));
  const filtered = $derived(filterStoreItems(items, filter, query));
  const view = $derived(paginate(filtered, page, PAGE_SIZE));
  /** 默认选中当前页第一项；所选插件不在当前页（翻页、过滤后）时回到第一项。 */
  const selected = $derived<StoreItem | null>(view.rows.find((entry) => entry.id === selectedId) ?? view.rows[0] ?? null);
  const groups = $derived(
    query.trim() ? (view.rows.length ? [{ group: 'results', total: filtered.length, rows: view.rows }] : []) : GROUPS.map((group) => ({
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
    if (item.upstream) await stageUpstream(item.upstream);
  }

  /**
   * 上游条目升级：点击一次即完成取包及提交，不再停在确认页。
   * 只有会清空插件数据（dataReset）的升级才保留确认页，避免无提示丢数据。
   */
  async function upgradeUpstream(entry: PluginUpstreamRequest): Promise<void> {
    await run(async () => {
      const preview = await previewUpstreamPlugin(entry);
      const candidate = preview.candidate;
      if (candidate.operation === 'unchanged') { message = i18n.t('settings.plugins.candidate.unchanged'); return; }
      if (candidate.dataReset) { staged = { kind: 'upstream', ecosystem: entry.ecosystem, preview }; return; }
      await submitUpstream(preview, entry.ecosystem);
    }, i18n.t('settings.plugins.candidate.installing'));
  }

  async function updateFromUpstream(item: StoreItem): Promise<void> {
    if (item.upstream) await upgradeUpstream(item.upstream);
  }

  // 搜索词变化后稍作等待再请求 registry；注册表变化（安装、卸载）后立即刷新，使安装状态保持准确。
  $effect(() => {
    const text = query.trim();
    void pluginRegistryRevision();
    void searchRetry;
    const requestSeq = ++upstreamSeq;
    const controller = new AbortController();
    // 只有用户输入了搜索词、且还没有这个搜索词的结果时才显示“搜索中”。
    // 进入页面、窗口重新获得焦点、安装/卸载后的刷新都是后台刷新：沿用已有结果，不出现搜索状态。
    const cached = cachedUpstreamProjection(text);
    untrack(() => {
      upstream = cached ?? (upstreamQuery === text ? upstream : null);
      upstreamQuery = text;
      upstreamLoading = Boolean(text) && !cached;
    });
    const timer = setTimeout(() => void refreshUpstream(text, requestSeq, controller.signal), text ? 350 : 0);
    return () => { clearTimeout(timer); controller.abort(); };
  });

  function selectItem(item: StoreItem): void {
    cancelPreview();
    message = '';
    selectedId = item.id;
    void revealFeedback();
  }

  function changeQuery(): void { cancelPreview(); message = ''; selectedId = null; }
  onDestroy(() => { previewController?.abort(); });

  function changePage(next: number): void { cancelPreview(); message = ''; selectedId = null; page = next; }

  $effect(() => {
    saveViewDraft('plugin-sources', 'archive', { address }, 'memory');
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

    {#if list?.configurationError}
      <div class="settings-notice settings-notice--warn" data-testid="plugin-configuration-error" role="status">
        <Icon name="alert-triangle" size={14} />
        <span>{i18n.t('settings.plugins.configurationError', { reason: list.configurationError })}</span>
      </div>
    {/if}
    {#if upstream?.error}
      <div class="settings-notice settings-notice--warn store-center-error" data-testid="plugin-upstream-error" role="status">
        <Icon name="alert-triangle" size={14} />
        <span>{i18n.t('settings.plugins.upstream.searchFailed', { reason: upstream.error })}</span>
        <button class="btn btn--sm btn--secondary" onclick={() => { upstream = null; searchRetry += 1; }}>{i18n.t('settings.plugins.center.retry')}</button>
      </div>
    {/if}
    {#if !query.trim()}
      <div class="settings-row-desc" data-testid="plugin-upstream-hint">{i18n.t('settings.plugins.upstream.searchHint')}</div>
    {/if}

    <div class="store-toolbar">
      <div class="store-search">
        <input class="form-input" data-testid="plugin-search" type="search" bind:value={query} oninput={changeQuery} disabled={busy && !previewController} placeholder={i18n.t('settings.plugins.store.search')} aria-label={i18n.t('settings.plugins.store.search')} aria-controls="plugin-search-results" />
        {#if upstreamLoading}
          <span class="store-search-status" data-testid="plugin-search-loading" role="status"><Icon name="loader" class="spinning" size={16} /><span>{i18n.t('settings.plugins.store.searching')}</span></span>
        {/if}
      </div>
      <div class="settings-subtabs store-filters" role="tablist">
        {#each [['all', 'filter.all'], ['installed', 'filter.installed'], ['available', 'filter.available'], ['updatable', 'filter.updatable']] as [id, key] (id)}
          {#if id !== 'updatable' || counts.updatable > 0}
            <button type="button" role="tab" class="settings-subtab" class:active={filter === id} aria-selected={filter === id} data-testid={`plugin-filter-${id}`} disabled={busy && !previewController} onclick={() => { changeQuery(); filter = id as StoreFilter; }}>
              {i18n.t(`settings.plugins.store.${key}`)} <span class="store-count">{counts[id as StoreFilter]}</span>
            </button>
          {/if}
        {/each}
      </div>
    </div>

    <div class="store" data-testid="plugin-store">
      <aside class="store-list">
        <div class="store-scroll" data-testid="plugin-list" id="plugin-search-results" aria-busy={upstreamLoading}>
          {#each groups as entry (entry.group)}
            <div class="store-group" data-group={entry.group}>
              <span>{entry.group === 'results' ? i18n.t('settings.plugins.store.results') : i18n.t(`settings.plugins.store.group.${entry.group}`)}</span>
              <span class="store-count">{entry.total}</span>
            </div>
            {#each entry.rows as item (item.id)}
              {@const runtime = item.installed?.runtime}
              {@const runtimeState = runtime?.state}
              {@const statusAlert = item.installed?.reinstallRequired
                ? i18n.t('settings.plugins.status.reinstallRequired')
                : runtime && (runtimeState === 'waiting' || runtimeState === 'failed' || runtimeState === 'restarting'
                  || (item.installed?.enabled && runtimeState === 'stopped' && runtime.crashes > 0))
                  ? i18n.t(`settings.plugins.runtime.state.${runtimeState}`)
                  : null}
              <button
                type="button"
                class="store-item"
                class:selected={selected?.id === item.id}
                aria-current={selected?.id === item.id ? 'true' : undefined}
                data-testid="plugin-row"
                data-plugin-id={item.id}
                data-status={item.status}
                data-group={entry.group}
                onclick={() => selectItem(item)}
                disabled={busy && !previewController}
              >
                <span class="settings-icon-tile" aria-hidden="true"><PluginIcon pluginId={item.id} icon={item.installed?.manifest.icon} upstream={item.installed ? null : item.upstream} revision={item.installed?.digest ?? ''} size={18} /></span>
                <span class="store-item-copy">
                  <span class="store-item-name">{item.name}</span>
                  <span class="store-item-desc">{item.description}</span>
                </span>
                <span class="store-item-state" data-state={item.updatable ? 'updatable' : item.status}>
                  {#if statusAlert}
                    <span
                      class="store-item-alert"
                      role="img"
                      aria-label={statusAlert}
                      title={statusAlert}
                    ><Icon name="warning" size={14} /></span>
                  {:else if item.updatable}
                    <Icon name="download" size={14} />
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
            <div class="settings-empty" role={upstreamLoading || !list ? 'status' : undefined}>
              {#if upstreamLoading || !list}<Icon name="loader" class="spinning" size={14} />{/if}
              <span>{upstreamLoading ? i18n.t('settings.plugins.store.searching') : !list ? i18n.t('common.loading') : items.length === 0 ? i18n.t('settings.plugins.empty') : i18n.t('settings.plugins.store.noMatch')}</span>
            </div>
          {/each}
        </div>

        {#if view.pages > 1}
          <div class="store-pager" data-testid="plugin-pager">
            <span class="settings-row-desc">{i18n.t('settings.plugins.store.pageSummary', { from: (view.page - 1) * PAGE_SIZE + 1, to: Math.min(view.page * PAGE_SIZE, filtered.length), total: filtered.length })}</span>
            <div class="store-pager-actions">
              <button class="btn btn--sm btn--secondary" data-testid="plugin-page-prev" onclick={() => changePage(view.page - 1)} disabled={view.page <= 1 || (busy && !previewController)}>{i18n.t('settings.plugins.store.prev')}</button>
              <span class="settings-row-desc">{view.page} / {view.pages}</span>
              <button class="btn btn--sm btn--secondary" data-testid="plugin-page-next" onclick={() => changePage(view.page + 1)} disabled={view.page >= view.pages || (busy && !previewController)}>{i18n.t('settings.plugins.store.next')}</button>
            </div>
          </div>
        {/if}
      </aside>

      <section class="store-detail" data-testid="plugin-detail" bind:this={detailPane} aria-busy={busy}>
  {#if message}
    <div class="settings-notice settings-notice--warn plugins-status" data-testid="plugin-message" role="status"><Icon name="info" size={14} /><span class="plugin-message-text">{message}</span></div>
  {/if}

  {#if staged}
    {@const preview = staged.kind === 'upstream' ? staged.preview.candidate : staged.preview}
    {@const original = staged.kind === 'upstream' ? staged.preview : null}
    <div class="settings-section plugin-candidate" data-testid="plugin-candidate" data-operation={preview.operation} data-origin={staged.kind}>
      <div class="settings-card plugin-candidate-card">
        <div class="candidate-scroll" data-testid="plugin-candidate-scroll">
        <div class="settings-card-head">
          <div class="settings-icon-tile" aria-hidden="true"><Icon name="download" size={15} /></div>
          <div class="settings-card-title">
            <div class="settings-card-title-line">
              <span class="settings-card-name">{manifestText(preview.manifest, 'name')}</span>
              <span class="settings-chip">
                {preview.installedVersion && preview.installedVersion !== preview.manifest.version
                  ? i18n.t('settings.plugins.candidate.versionChange', { from: preview.installedVersion, to: preview.manifest.version })
                  : `v${preview.manifest.version}`}
              </span>
              <span class="settings-chip settings-chip--accent">{preview.operation === 'upgrade' ? i18n.t('settings.plugins.upgrade') : i18n.t('settings.plugins.installAddress')}</span>
            </div>
            <span class="settings-row-desc">{manifestText(preview.manifest, 'description') || preview.manifest.id}</span>
          </div>
        </div>
        <div class="settings-card-body">
          <PluginImpactNotices manifest={preview.manifest} />
          {#if preview.dataSchemaFrom !== undefined}
            <div class="settings-notice" class:settings-notice--error={preview.dataReset} role={preview.dataReset ? 'alert' : undefined}>
              {preview.dataReset
                ? i18n.t('settings.plugins.candidate.dataReset', { from: preview.dataSchemaFrom, to: preview.manifest.dataSchemaVersion })
                : i18n.t('settings.plugins.candidate.dataChanged', { from: preview.dataSchemaFrom, to: preview.manifest.dataSchemaVersion })}
            </div>
          {/if}
          {#if original}
            <div class="plugin-perm" data-testid="candidate-origin">
              <span class="settings-row-title">{i18n.t('settings.plugins.upstream.origin')}</span>
              <div class="settings-row-desc">
                {i18n.t('settings.plugins.upstream.publisher')}：{original.publisher || i18n.t('common.notProvided')}
                {original.official ? i18n.t('settings.plugins.upstream.official') : i18n.t('settings.plugins.upstream.thirdParty')}
              </div>
              <div class="settings-row-desc">{i18n.t('settings.plugins.upstream.notice')}</div>
              <div class="settings-row-desc">{i18n.t('settings.plugins.upstream.runtimePending')}</div>
              <div class="settings-row-desc">{i18n.t('settings.plugins.upstream.integrity')} <span class="settings-mono" title={original.integrity}>{original.integrity.slice(0, 24)}</span></div>
            </div>
          {/if}
          <div class="settings-row-desc">
            {preview.operation === 'upgrade' ? i18n.t('settings.plugins.candidate.upgradeHint') : i18n.t('settings.plugins.candidate.installHint')}
            <span class="settings-mono" title={preview.digest}>{i18n.t('settings.plugins.digest', { digest: preview.digest.slice(0, 12) })}</span>
          </div>
        </div>
        </div>
        <div class="settings-card-foot" data-testid="plugin-candidate-actions">
          {#if canCancelInstall}
            <button class="btn btn--sm btn--secondary" data-testid="plugin-cancel-install" disabled={cancellingInstall} onclick={() => void cancelActiveInstall()}>
              {cancellingInstall ? i18n.t('settings.plugins.candidate.cancelling') : i18n.t('settings.plugins.candidate.cancelOperation')}
            </button>
          {:else}
            <button class="btn btn--sm btn--secondary" data-action="cancel-candidate" onclick={() => { staged = null; }} disabled={busy}>{i18n.t('settings.plugins.candidate.cancel')}</button>
          {/if}
          <button class="btn btn--lg btn--primary" data-action="confirm-candidate" onclick={() => void confirmStaged()} disabled={busy}>
            {#if busy}<Icon name="loader" class="spinning" size={14} /> {busyLabel}
            {:else}{preview.operation === 'upgrade' ? i18n.t('settings.plugins.candidate.confirmUpgrade') : i18n.t('settings.plugins.candidate.confirmInstall')}{/if}
          </button>
        </div>
      </div>
    </div>
  {/if}

        {#if !staged}
        {#if selected}
          {#key selected.id}
            <PluginDetailPane
              item={selected}
              {busy}
              {busyLabel}
              previewing={previewController !== null}
              onCancelPreview={cancelPreview}
              {canCancelInstall}
              {cancellingInstall}
              onCancelInstall={() => void cancelActiveInstall()}
              onInstall={() => void installItem(selected)}
              onUpdateFromUpstream={() => void updateFromUpstream(selected)}
              onReinstall={() => {
                const req = selected.installed ? installedUpstreamRequest(selected.installed) : null;
                if (req) void upgradeUpstream(req);
              }}
              onRestartHost={() => void run(() => restartPluginHost())}
              onUninstall={() => void uninstallItem(selected)}
              onEnable={() => void run(() => enablePlugin(selected.installed!))}
              onDisable={() => void run(() => disablePlugin(selected.installed!))}
              onOpenSettings={openSettings}
            />
          {/key}
        {:else}
          <div class="settings-empty"><span>{i18n.t('settings.plugins.detail.empty')}</span></div>
        {/if}
        {/if}
      </section>
    </div>
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
    {#if busy}
      <div class="settings-hint" data-testid="plugin-sources-progress" role="status"><Icon name="loader" size={13} class="spinning" /> {busyLabel || i18n.t('common.loading')}</div>
    {/if}
    <div class="sources-body">
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
            <input type="file" accept=".tgz,.tar.gz,application/gzip,application/x-gzip" hidden onchange={submitFile} disabled={busy} />
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
  .store-search { display: flex; align-items: center; gap: var(--space-2); min-width: 0; flex: 1 1 260px; }
  .store-search input { min-width: 0; flex: 1; }
  .store-search-status { display: inline-flex; align-items: center; gap: var(--space-1); color: var(--foreground-muted); font-size: var(--text-xs); }
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
  .store-item:disabled { cursor: wait; opacity: .65; }
  .store-item.selected { background: var(--primary-muted); }
  .store-item:focus-visible { outline: 2px solid var(--primary); outline-offset: -2px; }
  .store-item-copy { min-width: 0; flex: 1; display: grid; gap: 2px; }
  .store-item-name { font-size: var(--text-sm); font-weight: var(--font-medium); color: var(--foreground); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .store-item-desc { font-size: var(--text-xs); color: var(--foreground-muted); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  /* 状态一眼可辨：有更新用主题色向下箭头，已启用为绿色对勾，已安装未启用为灰色对勾，未安装为加号。 */
  .store-item-state { display: inline-flex; flex-direction: column; align-items: center; gap: 2px; flex: 0 0 auto; min-width: 40px; color: var(--foreground-muted); }
  .store-item-state[data-state='updatable'] { color: var(--primary); }
  .store-item-alert { display: inline-flex; color: var(--warning); }
  .store-item-state[data-state='active'] { color: var(--success); }
  .store-item-state[data-state='working'] { color: var(--primary); }
  .store-item-version { font-size: var(--text-xs); font-variant-numeric: tabular-nums; }
  .store-pager { display: grid; gap: var(--space-2); padding: var(--space-2) var(--space-3); border-top: var(--magi-hairline) solid var(--border); }
  .store-pager-actions { display: flex; align-items: center; justify-content: space-between; gap: var(--space-2); }
  .store-detail { display: flex; flex-direction: column; min-width: 0; min-height: 0; overflow: hidden; }
  @container settings-content (max-width: 720px) {
    .store-section { flex: 1 1 0; }
    .store { grid-template-columns: minmax(0, 1fr); grid-template-rows: minmax(88px, .4fr) minmax(0, 1fr); }
    .store-list { border-right: 0; border-bottom: var(--magi-hairline) solid var(--border); }
  }
  .plugin-perm { display: grid; gap: var(--space-2); }
  .plugin-disabled { opacity: 0.45; pointer-events: none; }
  .plugins-status { margin: var(--space-3); flex: 0 1 auto; max-height: 35%; overflow-y: auto; }
  .plugin-message-text { white-space: pre-wrap; overflow-wrap: anywhere; }
  .plugin-candidate { display: flex; flex: 1; min-height: 0; margin: 0; }
  .plugin-candidate-card { display: flex; flex-direction: column; flex: 1; min-height: 0; border: 0; border-radius: 0; }
  .candidate-scroll { flex: 1; min-height: 0; overflow-y: auto; overscroll-behavior: contain; }
  .plugin-candidate-card > .settings-card-foot { flex: 0 0 auto; flex-wrap: wrap; }
  .sources-header { display: flex; align-items: center; justify-content: space-between; width: 100%; }
  .sources-header h3 { margin: 0; }
  .sources-body { display: grid; gap: var(--space-5); }
  .sources-body section { display: grid; gap: var(--space-2); }
  .sources-body h4 { margin: 0; font-size: var(--text-sm); font-weight: var(--font-bold); }
  .sources-zip { display: flex; align-items: center; justify-content: space-between; gap: var(--space-3); }
</style>
