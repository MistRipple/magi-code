<script lang="ts">
  /**
   * 插件详情面板（商店右侧）：来源、版本、权限、全局启用状态与全部生命周期操作。
   * 左侧列表只放概要；所有“改变状态”的操作都在这里，由父组件执行并刷新 daemon 事实。
   */
  import { i18n } from '../stores/i18n.svelte';
  import type { InstalledPluginProjection } from '../web/plugin-api';
  import type { PluginManifest, PluginRuntimeStatus } from '../shared/app-server-protocol.generated';
  import { installedUpstreamRequest, pluginSettingsTab, type StoreItem } from '../lib/plugin-store';
  import { cachedUpstreamBadge } from '../lib/plugin-upstream-cache';
  import Icon from './Icon.svelte';
  import PluginIcon from './PluginIcon.svelte';
  import PluginImpactNotices from './PluginImpactNotices.svelte';

  interface Props {
    item: StoreItem;
    busy: boolean;
    busyLabel?: string;
    previewing?: boolean;
    onCancelPreview?: () => void;
    canCancelInstall: boolean;
    cancellingInstall: boolean;
    onCancelInstall: () => void;
    onInstall: () => void;
    onUpdateFromUpstream: () => void;
    onReinstall?: () => void;
    onRestartHost?: () => void;
    onUninstall: () => void;
    onEnable: () => void;
    onDisable: () => void;
    onOpenSettings: (tab: string) => void;
  }
  let {
    item, busy, busyLabel = '', previewing = false, onCancelPreview, onInstall, onUpdateFromUpstream,
    onReinstall, onRestartHost, onUninstall, onEnable, onDisable, onOpenSettings,
    canCancelInstall, cancellingInstall, onCancelInstall,
  }: Props = $props();

  const plugin = $derived(item.installed);
  const manifest = $derived<PluginManifest | null>(plugin?.manifest ?? null);
  const reinstallRequired = $derived(Boolean(plugin?.reinstallRequired));
  const canReinstallFromUpstream = $derived(Boolean(plugin && reinstallRequired && installedUpstreamRequest(plugin)));
  const settingsTab = $derived(plugin && plugin.enabled && !reinstallRequired ? pluginSettingsTab(plugin.manifest) : null);
  const versionUpdate = $derived(item.upstream?.state === 'updatable' ? item.upstream.version : null);

  /**
   * 上游徽标：生态来自已安装记录，立即可用；发布者与“官方/第三方”来自上游搜索结果，
   * 已见过的包用缓存补上，避免进入页面后徽标晚一步出现、页面跳动。
   */
  const upstreamBadges = $derived.by(() => {
    if (item.upstream) {
      return { ecosystem: item.upstream.ecosystem, official: item.upstream.official as boolean | null, publisher: item.upstream.publisher };
    }
    const request = plugin ? installedUpstreamRequest(plugin) : null;
    if (!request) return null;
    const cached = request.package ? cachedUpstreamBadge(request.ecosystem, request.package) : null;
    return { ecosystem: request.ecosystem, official: cached ? cached.official : null, publisher: cached?.publisher ?? '' };
  });

  function sourceLabel(installed: InstalledPluginProjection): string {
    return i18n.t(`settings.plugins.source.${installed.source.kind}`);
  }

  /** 受信扩展的宿主运行状态；只有声明了 extension 的插件才有这一行。 */
  const runtime = $derived(plugin?.runtime ?? null);
  const isExtension = $derived(Boolean(manifest?.extension));
  const hostStoppedByCrash = $derived(Boolean(plugin?.enabled && runtime?.state === 'stopped' && runtime.crashes > 0));
  const runtimeProblem = $derived(runtime !== null && (runtime.state === 'failed' || runtime.state === 'restarting' || hostStoppedByCrash));
  const unmountedSlots = $derived(runtime?.unmountedSlots ?? []);
  const hasNoSettingsPage = $derived(Boolean(plugin && plugin.enabled && !reinstallRequired && runtime?.hasSettingsView === false));

  function runtimeText(status: PluginRuntimeStatus | null): string {
    if (!status) return i18n.t('settings.plugins.runtime.state.notRunning');
    const suffix = status.crashes > 0 ? i18n.t('settings.plugins.runtime.crashes', { count: status.crashes }) : '';
    return `${i18n.t(`settings.plugins.runtime.state.${status.state}`)}${suffix}`;
  }
</script>

<article class="pane" data-testid="plugin-card" data-plugin-id={item.id}>
  <div class="pane-scroll" data-testid="plugin-detail-scroll">
  <header class="pane-head">
    <div class="pane-title">
      <span class="pane-icon" aria-hidden="true"><PluginIcon pluginId={item.id} icon={item.installed?.manifest.icon} upstream={item.installed ? null : item.upstream} revision={item.installed?.digest ?? ''} size={28} /></span>
      <h3>{item.name}</h3>
      <span class="settings-chip">v{item.version}</span>
      {#if reinstallRequired}<span class="settings-chip settings-chip--warn" data-testid="plugin-status-reinstall">{i18n.t('settings.plugins.status.reinstallRequired')}</span>
      {:else if item.status === 'enabled'}<span class="settings-chip settings-chip--ok">{i18n.t('settings.plugins.status.enabled')}</span>
{:else if item.status === 'disabled'}<span class="settings-chip">{i18n.t('settings.plugins.status.disabled')}</span>
      {:else}<span class="settings-chip">{i18n.t('settings.plugins.status.notInstalled')}</span>{/if}
      {#if item.updatable && !reinstallRequired}<span class="settings-chip settings-chip--accent">{i18n.t('settings.plugins.status.updatable')}</span>{/if}
    </div>
    <div class="pane-id settings-mono">{item.upstream && !plugin ? item.upstream.package : item.id}</div>
    {#if upstreamBadges}
      <div class="settings-chips pane-upstream" data-testid="plugin-upstream-badges">
        <span class="settings-chip">{i18n.t(`settings.plugins.upstream.ecosystem.${upstreamBadges.ecosystem}`)}</span>
        {#if upstreamBadges.official !== null}
          <span class="settings-chip" class:settings-chip--ok={upstreamBadges.official}>
            {upstreamBadges.official ? i18n.t('settings.plugins.upstream.official') : i18n.t('settings.plugins.upstream.thirdParty')}
          </span>
        {/if}
        {#if upstreamBadges.publisher}<span class="settings-chip">{i18n.t('settings.plugins.upstream.publisher')}：{upstreamBadges.publisher}</span>{/if}
      </div>
    {/if}
  </header>

  <p class="pane-desc">{item.description || item.id}</p>

  {#if item.upstream && !plugin}
    <p class="settings-notice settings-notice--warn" data-testid="plugin-upstream-notice" role="note"><Icon name="info" size={13} /><span>{i18n.t('settings.plugins.upstream.notice')}</span></p>
    {#if item.upstream.blockedReason}
      <p class="settings-notice settings-notice--warn" data-testid="plugin-upstream-blocked" role="status"><Icon name="warning" size={13} /><span>{item.upstream.blockedReason}</span></p>
    {/if}
  {/if}


  {#if busy}
    <div class="settings-hint" data-testid="plugin-operation-progress" role="status"><Icon name="loader" size={13} class="spinning" /> {busyLabel || i18n.t('common.loading')}
    </div>
  {/if}

  <dl class="pane-facts">
    <div><dt>{i18n.t('settings.plugins.detail.type')}</dt><dd>{i18n.t('settings.plugins.detail.kindExtension')}</dd></div>
    {#if isExtension}
      <div data-testid="plugin-runtime" class:pane-warn={runtimeProblem}>
        <dt>{i18n.t('settings.plugins.runtime.label')}</dt>
        <dd>
          {#if runtimeProblem}<Icon name="warning" size={13} />{/if}
          <span>{runtimeText(runtime)}</span>
        </dd>
      </div>
    {/if}
    <div><dt>{i18n.t('settings.plugins.detail.version')}</dt><dd>v{item.version}{#if versionUpdate} → v{versionUpdate}{/if}</dd></div>
      <div data-testid="plugin-source" class:pane-warn={plugin?.source.kind === 'address' || plugin?.source.kind === 'local'}>
        <dt>{i18n.t('settings.plugins.detail.source')}</dt>
        <dd>{#if plugin}{@const location = plugin.source.github ? `github:${plugin.source.github.repository}#${plugin.source.github.commit}` : plugin.source.location}{sourceLabel(plugin)} <span class="settings-mono" title={location} dir="ltr">{location}</span>{:else}{i18n.t(item.upstream ? 'settings.plugins.source.upstream' : 'common.notProvided')}{/if}</dd>
      </div>
      <div><dt>{i18n.t('settings.plugins.detail.digest')}</dt><dd class="settings-mono" title={plugin?.digest}>{plugin?.digest.slice(0, 16) || i18n.t('common.notProvided')}</dd></div>

  </dl>

  {#if plugin?.disabledBy}
    <p class="settings-notice settings-notice--warn" role="status" data-testid="plugin-disabled-by">
      <Icon name="info" size={13} /><span>{i18n.t('settings.plugins.disabledBy', { name: plugin.disabledBy.name, reason: plugin.disabledBy.reason })}</span>
    </p>
  {/if}

  {#if reinstallRequired}
    <p class="settings-notice settings-notice--warn" role="status" data-testid="plugin-reinstall-notice">
      <Icon name="warning" size={13} /><span>{i18n.t('settings.plugins.reinstallNotice')}</span>
    </p>
  {:else if runtime?.reason}
    <div class="settings-notice settings-notice--{runtimeProblem ? 'error' : 'warn'} pane-runtime-notice" role="status" data-testid="plugin-runtime-reason">
      <Icon name="warning" size={13} /><span>{runtime.reason}</span>
      {#if hostStoppedByCrash && onRestartHost}
        <button class="btn btn--sm btn--secondary" data-testid="plugin-restart-host" onclick={onRestartHost} disabled={busy}>
          <Icon name="refresh" size={13} />
          <span>{i18n.t('settings.plugins.runtime.restartHost')}</span>
        </button>
      {/if}
    </div>
  {/if}
  {#if unmountedSlots.length > 0}
    <p class="settings-notice settings-notice--warn" role="status" data-testid="plugin-unmounted-slots">
      <Icon name="info" size={13} />
      <span>{i18n.t('settings.plugins.unmountedSlotsNotice', { count: unmountedSlots.length, slots: unmountedSlots.join('、') })}</span>
    </p>
  {/if}
  {#if hasNoSettingsPage}
    <p class="settings-notice" role="status" data-testid="plugin-no-settings">
      <Icon name="info" size={13} />
      <span>{i18n.t('settings.plugins.noSettingsView')}</span>
    </p>
  {/if}
  {#if runtime && runtime.blocking.length > 0}
    <p class="settings-notice settings-notice--warn" role="status" data-testid="plugin-runtime-blocking">
      <Icon name="shield" size={13} />
      <span>{i18n.t('settings.plugins.runtime.blocking', { hooks: runtime.blocking.join(', ') })}</span>
    </p>
  {/if}

  {#if manifest && !reinstallRequired}
    <PluginImpactNotices {manifest} />
  {/if}

  {#if plugin && !reinstallRequired}
    <section class="pane-section">
      <h4>{i18n.t('settings.plugins.stateHeading')}</h4>
      <div class="pane-state">
        <div class="pane-state-row" data-testid="plugin-state-row" data-state={plugin.enabled ? 'enabled' : 'disabled'}>
          <span class="settings-dot" class:settings-dot--ok={plugin.enabled} aria-hidden="true"></span>
          <div class="settings-row-copy">
            <span class="settings-row-title">{i18n.t('settings.plugins.globalEnabled')}</span>
            <span class="settings-row-desc">{i18n.t(plugin.enabled ? 'settings.plugins.enabledState.on' : 'settings.plugins.enabledState.off')}</span>
          </div>
          <div class="settings-row-actions">
            {#if plugin.enabled}
              <button class="btn btn--sm btn--secondary" data-action="disable" onclick={onDisable} disabled={busy}>{i18n.t('settings.plugins.disable')}</button>
            {:else}
              <button class="btn btn--sm btn--primary" data-action="enable" onclick={onEnable} disabled={busy}>{i18n.t('settings.plugins.enable')}</button>
            {/if}
          </div>
        </div>
      </div>
    </section>
  {/if}
  </div>
  <div class="settings-card-foot pane-actions" data-testid="plugin-detail-actions">
    {#if previewing}<button class="btn btn--sm btn--secondary" data-testid="plugin-cancel-preview" onclick={onCancelPreview}>{i18n.t('common.cancel')}</button>{/if}
    {#if canCancelInstall}
      <button class="btn btn--sm btn--secondary" data-testid="plugin-cancel-install" disabled={cancellingInstall} onclick={onCancelInstall}>
        {cancellingInstall ? i18n.t('settings.plugins.candidate.cancelling') : i18n.t('settings.plugins.candidate.cancelOperation')}
      </button>
    {/if}
    {#if plugin}
      {#if reinstallRequired}
        {#if canReinstallFromUpstream && onReinstall}
          <button class="btn btn--lg btn--primary" data-testid="plugin-reinstall-action" onclick={onReinstall} disabled={busy}>
            <Icon name={busy ? 'loader' : 'download'} class={busy ? 'spinning' : ''} size={13} />
            <span>{busy ? busyLabel || i18n.t('common.loading') : i18n.t('settings.plugins.reinstallAction')}</span>
          </button>
        {/if}
      {:else if item.updatable && item.upstream}
        <button class="btn btn--lg btn--primary" data-testid="plugin-detail-update" onclick={onUpdateFromUpstream} disabled={busy}>
          <Icon name={busy ? 'loader' : 'download'} class={busy ? 'spinning' : ''} size={13} />
          <span>{busy ? busyLabel || i18n.t('common.loading') : i18n.t('settings.plugins.center.update', { from: plugin.manifest.version })}</span>
        </button>
      {/if}
      {#if settingsTab}
        <button class="btn btn--sm {item.updatable ? 'btn--secondary' : 'btn--primary'}" data-testid="plugin-open-settings" onclick={() => onOpenSettings(settingsTab)}>
          <Icon name="settings" size={13} />
          <span>{i18n.t('settings.plugins.openSettings')}</span>
        </button>
      {/if}
      <button class="btn btn--sm btn--secondary" onclick={onUninstall} disabled={busy}>{i18n.t('settings.plugins.uninstall')}</button>
    {:else}
      <button class="btn btn--lg btn--primary" data-testid="plugin-detail-install" onclick={onInstall} disabled={busy || (item.upstream !== null && !item.upstream.installable)} title={item.upstream?.blockedReason ?? undefined}>
        <Icon name={busy ? 'loader' : 'download'} class={busy ? 'spinning' : ''} size={13} />
        <span>{busy ? busyLabel || i18n.t('common.loading') : i18n.t('settings.plugins.installAddress')}</span>
      </button>
    {/if}
  </div>

</article>

<style>
  .pane { display: flex; flex-direction: column; flex: 1; min-height: 0; min-width: 0; user-select: text; }
  .pane-scroll { display: grid; align-content: start; gap: var(--space-4); padding: var(--space-5); flex: 1; min-height: 0; overflow-y: auto; overscroll-behavior: contain; }
  .pane-title { display: flex; align-items: center; gap: var(--space-2); flex-wrap: wrap; }
  .pane-title h3 { overflow-wrap: anywhere; min-width: 0; margin: 0; font-size: 20px; font-weight: calc(var(--font-semibold) + 50); color: var(--foreground); }
  .pane-icon { display: grid; place-items: center; width: 36px; height: 36px; flex: 0 0 auto; border-radius: 10px; color: var(--primary); background: var(--primary-muted); }
  .pane-upstream { margin-top: var(--space-2); }
  .pane-id { margin-top: 6px; color: var(--foreground-muted); overflow-wrap: anywhere; }
  .pane-desc { margin: 0; color: var(--foreground); font-size: var(--text-sm); line-height: 1.6; }
  .pane-actions { display: flex; flex: 0 0 auto; justify-content: flex-end; gap: var(--space-2); flex-wrap: wrap; padding: var(--space-3) var(--space-5); border-top: var(--magi-hairline) solid var(--border); }
  .pane-facts { margin: 0; display: grid; gap: 8px; }
  .pane-facts > div { display: grid; grid-template-columns: fit-content(30%) minmax(0, 1fr); gap: var(--space-3); align-items: baseline; }
  .pane-facts dt { overflow-wrap: anywhere; color: var(--foreground-muted); font-size: var(--text-xs); }
  .pane-facts dd { margin: 0; font-size: var(--text-xs); color: var(--foreground); overflow-wrap: anywhere; display: flex; gap: 8px; flex-wrap: wrap; }
  .pane-facts dd > *, .pane-state .settings-row-copy { min-width: 0; max-width: 100%; overflow-wrap: anywhere; }
  .pane-state-row { flex-wrap: wrap; }
  .pane-warn dd { color: var(--warning); }
  .pane-section h4 { margin: 0 0 var(--space-2); font-size: var(--text-sm); font-weight: var(--font-bold); color: var(--foreground); }
  .pane-state { border: var(--magi-hairline) solid var(--border); border-radius: 10px; padding: 0 12px; }
  .pane-state-row { display: flex; align-items: center; gap: var(--space-3); min-height: 46px; padding: var(--space-2) 0; }
  .pane-runtime-notice { align-items: center; flex-wrap: wrap; }
  .pane-runtime-notice > span { flex: 1 1 200px; min-width: 0; }
</style>
