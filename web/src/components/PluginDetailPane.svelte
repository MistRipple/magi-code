<script lang="ts">
  /**
   * 插件详情面板（商店右侧）：来源、版本、权限、全局启用状态与全部生命周期操作。
   * 左侧列表只放概要；所有“改变状态”的操作都在这里，由父组件执行并刷新 daemon 事实。
   */
  import { i18n } from '../stores/i18n.svelte';
  import type { InstalledPluginProjection } from '../web/plugin-api';
  import type { PluginManifest, PluginPermission, PluginRuntimeStatus } from '../shared/app-server-protocol.generated';
  import { pluginSettingsTab, type StoreItem } from '../lib/plugin-store';
  import Icon from './Icon.svelte';
  import PluginIcon from './PluginIcon.svelte';

  interface Props {
    item: StoreItem;
    busy: boolean;
    busyLabel?: string;
    previewing?: boolean;
    onCancelPreview?: () => void;
    onInstall: () => void;
    onUpdateFromCenter: () => void;
    onUninstall: () => void;
    onActivate: () => void;
    onDeactivate: () => void;
    onDisable: () => void;
    onOpenSettings: (tab: string) => void;
  }
  let {
    item, busy, busyLabel = '', previewing = false, onCancelPreview, onInstall, onUpdateFromCenter,
    onUninstall, onActivate, onDeactivate, onDisable, onOpenSettings,
  }: Props = $props();

  interface ActivationRow {
    state: 'active' | 'enabled' | 'off';
  }

  const plugin = $derived(item.installed);
  const manifest = $derived<PluginManifest | null>(plugin?.manifest ?? null);
  const settingsTab = $derived(plugin && plugin.active ? pluginSettingsTab(plugin.manifest) : null);
  const activation = $derived(plugin ? activationRow(plugin) : null);
  const removable = $derived(Boolean(plugin) && !plugin!.active && !plugin!.enabled);

  function permissionText(permission: PluginPermission): string {
    if (permission.kind === 'client') return i18n.t('settings.plugins.permission.client');
    if (permission.kind === 'attachments') return i18n.t('settings.plugins.permission.attachments');
    if (permission.kind === 'sessionRead') return i18n.t('settings.plugins.permission.sessionRead');
    if (permission.kind === 'composer') return i18n.t('settings.plugins.permission.composer');
    if (permission.kind === 'extension') return i18n.t('settings.plugins.permission.extensionFullTrust');
    if (permission.kind === 'pluginManagement') return [
      i18n.t(permission.targets.includes('authorize') ? 'settings.plugins.permission.pluginAuthorization' : 'settings.plugins.permission.pluginManagement'),
      ...(permission.targets.includes('compatibility') ? [i18n.t('settings.plugins.permission.pluginCompatibility')] : []),
      ...(permission.targets.includes('background') ? [i18n.t('settings.plugins.permission.pluginBackground')] : []),
    ].join('；');
    if (permission.kind === 'contextRewrite') return i18n.t('settings.plugins.permission.contextRewrite');
    if (permission.kind === 'modelSources') return i18n.t('settings.plugins.permission.modelSources', { sources: permission.targets.join(', ') });
    return `${permission.kind}${permission.targets.length ? `: ${permission.targets.join(', ')}` : ''}`;
  }

  function sourceLabel(installed: InstalledPluginProjection): string {
    return i18n.t(`settings.plugins.source.${installed.source.kind}`);
  }

  /** 插件全局生效，只有一个启用状态。 */
  function activationRow(installed: InstalledPluginProjection): ActivationRow {
    if (installed.active) return { state: 'active' };
    if (installed.enabled) return { state: 'enabled' };
    return { state: 'off' };
  }

  /** 受信扩展的宿主运行状态；只有声明了 extension 的插件才有这一行。 */
  const runtime = $derived(plugin?.runtime ?? null);
  const isExtension = $derived(Boolean(manifest?.extension));
  const runtimeProblem = $derived(runtime !== null && (runtime.state === 'failed' || runtime.state === 'restarting'));

  function runtimeText(status: PluginRuntimeStatus | null): string {
    if (!status) return i18n.t('settings.plugins.runtime.state.notRunning');
    const suffix = status.crashes > 0 ? i18n.t('settings.plugins.runtime.crashes', { count: status.crashes }) : '';
    return `${i18n.t(`settings.plugins.runtime.state.${status.state}`)}${suffix}`;
  }

  function stateText(row: ActivationRow): string {
    return i18n.t(`settings.plugins.activationState.${row.state}`);
  }
</script>

<article class="pane" data-testid="plugin-card" data-plugin-id={item.id}>
  <div class="pane-scroll" data-testid="plugin-detail-scroll">
  <header class="pane-head">
    <div class="pane-title">
      <span class="pane-icon" aria-hidden="true"><PluginIcon pluginId={item.id} icon={item.installed?.manifest.icon} upstream={item.installed ? null : item.upstream} revision={item.installed?.digest ?? ''} size={28} /></span>
      <h3>{item.name}</h3>
      <span class="settings-chip">v{item.version}</span>
      {#if item.status === 'active'}<span class="settings-chip settings-chip--ok">{i18n.t('settings.plugins.status.active')}</span>
            {:else if item.status === 'installed'}<span class="settings-chip">{i18n.t('settings.plugins.status.installed')}</span>
      {:else}<span class="settings-chip">{i18n.t('settings.plugins.status.notInstalled')}</span>{/if}
      {#if item.updatable}<span class="settings-chip settings-chip--accent">{i18n.t('settings.plugins.status.updatable')}</span>{/if}
    </div>
    <div class="pane-id settings-mono">{item.upstream && !plugin ? item.upstream.package : item.id}</div>
    {#if item.upstream}
      <div class="settings-chips pane-upstream" data-testid="plugin-upstream-badges">
        <span class="settings-chip">{i18n.t(`settings.plugins.upstream.ecosystem.${item.upstream.ecosystem}`)}</span>
        <span class="settings-chip" class:settings-chip--ok={item.upstream.official}>
          {item.upstream.official ? i18n.t('settings.plugins.upstream.official') : i18n.t('settings.plugins.upstream.thirdParty')}
        </span>
        {#if item.upstream.publisher}<span class="settings-chip">{i18n.t('settings.plugins.upstream.publisher')}：{item.upstream.publisher}</span>{/if}
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
    <div><dt>{i18n.t('settings.plugins.detail.version')}</dt><dd>v{item.version}{#if item.updatable} → v{item.catalog?.version ?? item.upstream?.version}{/if}</dd></div>
      <div data-testid="plugin-source" class:pane-warn={plugin?.source.kind === 'address' || plugin?.source.kind === 'local'}>
        <dt>{i18n.t('settings.plugins.detail.source')}</dt>
        <dd>{#if plugin}{sourceLabel(plugin)} <span class="settings-mono" title={plugin.source.location}>{plugin.source.location}</span>{:else}{i18n.t(item.catalog ? 'settings.plugins.source.center' : item.upstream ? 'settings.plugins.source.upstream' : 'common.notProvided')}{/if}</dd>
      </div>
      <div><dt>{i18n.t('settings.plugins.detail.digest')}</dt><dd class="settings-mono" title={plugin?.digest}>{plugin?.digest.slice(0, 16) || i18n.t('common.notProvided')}</dd></div>
      <div><dt>{i18n.t('settings.plugins.detail.size')}</dt><dd>{!plugin && item.catalog ? `${Math.round(item.catalog.size / 1024)} KB` : i18n.t('common.notProvided')}</dd></div>
  </dl>

  {#if runtime?.reason}
    <p class="settings-notice settings-notice--{runtimeProblem ? 'error' : 'warn'}" role="status" data-testid="plugin-runtime-reason">
      <Icon name="warning" size={13} /><span>{runtime.reason}</span>
    </p>
  {/if}
  {#if runtime && runtime.blocking.length > 0}
    <p class="settings-notice settings-notice--warn" role="status" data-testid="plugin-runtime-blocking">
      <Icon name="shield" size={13} />
      <span>{i18n.t('settings.plugins.runtime.blocking', { hooks: runtime.blocking.join(', ') })}</span>
    </p>
  {/if}

  <section class="pane-section">
    <h4>{i18n.t('settings.plugins.permissions')}</h4>
    {#if manifest}
      <div class="settings-chips">
        {#each manifest.permissions as permission}
          <span class="settings-chip settings-mono" class:settings-chip--wrap={permission.kind === 'extension' || permission.kind === 'contextRewrite' || permission.kind === 'modelSources' || permission.kind === 'pluginManagement' || permission.kind === 'composer' || permission.kind === 'sessionRead' || permission.kind === 'client' || permission.kind === 'attachments'}>{permissionText(permission)}</span>
        {:else}
          <span class="settings-row-desc">{i18n.t('settings.plugins.noPermissions')}</span>
        {/each}
      </div>
    {:else}
      <p class="settings-row-desc">{i18n.t('settings.plugins.detail.permissionsAfterInstall')}</p>
    {/if}
  </section>

  {#if plugin && activation}
    <section class="pane-section">
      <h4>{i18n.t('settings.plugins.activation')}</h4>
      <div class="pane-activation">
        <div class="pane-activation-row" data-testid="plugin-activation-row" data-state={activation.state}>
          <span class="settings-dot" class:settings-dot--ok={activation.state === 'active'} class:settings-dot--warn={activation.state === 'enabled'} aria-hidden="true"></span>
          <div class="settings-row-copy">
            <span class="settings-row-title">{i18n.t('settings.plugins.globalActivation')}</span>
            <span class="settings-row-desc">{stateText(activation)}</span>
          </div>
          <div class="settings-row-actions">
            {#if activation.state === 'active'}
              <button class="btn btn--sm btn--secondary" data-action="deactivate" onclick={onDeactivate} disabled={busy}>{i18n.t('settings.plugins.deactivate')}</button>
            {:else if activation.state === 'enabled' || activation.state === 'off'}
              {#if activation.state === 'enabled'}
                <button class="btn btn--sm btn--secondary" onclick={onDisable} disabled={busy}>{i18n.t('settings.plugins.disable')}</button>
              {/if}
              <button class="btn btn--sm btn--primary" data-action="activate" onclick={onActivate} disabled={busy}>{i18n.t('settings.plugins.activate')}</button>
            {/if}
          </div>
        </div>
      </div>
    </section>
  {/if}
  </div>
  <div class="settings-card-foot pane-actions" data-testid="plugin-detail-actions">
    {#if previewing}<button class="btn btn--sm btn--secondary" data-testid="plugin-cancel-preview" onclick={onCancelPreview}>{i18n.t('common.cancel')}</button>{/if}
    {#if plugin}
      {#if item.updatable && (item.catalog || item.upstream)}
        <button class="btn btn--lg btn--primary" data-testid="plugin-detail-update" onclick={onUpdateFromCenter} disabled={busy}>
          <Icon name="download" size={13} />
          <span>{i18n.t('settings.plugins.center.update', { from: plugin.manifest.version })}</span>
        </button>
      {/if}
      {#if settingsTab}
        <button class="btn btn--sm {item.updatable ? 'btn--secondary' : 'btn--primary'}" data-testid="plugin-open-settings" onclick={() => onOpenSettings(settingsTab)}>
          <Icon name="settings" size={13} />
          <span>{i18n.t('settings.plugins.openSettings')}</span>
        </button>
      {/if}
      <button
        class="btn btn--sm btn--secondary"
        onclick={onUninstall}
        disabled={busy || !removable}
        title={removable ? undefined : i18n.t('settings.plugins.uninstallBlocked')}
      >{i18n.t('settings.plugins.uninstall')}</button>
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
  .pane-facts dd > *, .pane-activation .settings-row-copy { min-width: 0; max-width: 100%; overflow-wrap: anywhere; }
  .pane-activation-row { flex-wrap: wrap; }
  .pane-warn dd { color: var(--warning); }
  .pane-section h4 { margin: 0 0 var(--space-2); font-size: var(--text-sm); font-weight: var(--font-bold); color: var(--foreground); }
  .pane-activation { border: var(--magi-hairline) solid var(--border); border-radius: 10px; padding: 0 12px; }
  .pane-activation-row { display: flex; align-items: center; gap: var(--space-3); min-height: 46px; padding: var(--space-2) 0; }
</style>
