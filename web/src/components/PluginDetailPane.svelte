<script lang="ts">
  /**
   * 插件详情面板（商店右侧）：来源、版本、权限、各作用域的启用状态与全部生命周期操作。
   * 左侧列表只放概要；所有“改变状态”的操作都在这里，由父组件执行并刷新 daemon 事实。
   */
  import { i18n } from '../stores/i18n.svelte';
  import type { InstalledPluginProjection } from '../web/plugin-api';
  import type { PluginManifest, PluginPermission } from '../shared/app-server-protocol.generated';
  import { pluginSettingsTab, type StoreItem } from '../lib/plugin-store';
  import { APPLICATION_PLUGIN_SCOPE } from '../stores/plugin-registry.svelte';
  import Icon from './Icon.svelte';

  interface Props {
    item: StoreItem;
    now: number;
    busy: boolean;
    onInstall: () => void;
    onUpdateFromCenter: () => void;
    onUpgradeFile: (event: Event) => void;
    onUninstall: () => void;
    onActivate: () => void;
    onDeactivate: () => void;
    onDisable: () => void;
    onOpenSettings: (tab: string) => void;
  }
  let {
    item, now, busy, onInstall, onUpdateFromCenter, onUpgradeFile,
    onUninstall, onActivate, onDeactivate, onDisable, onOpenSettings,
  }: Props = $props();

  interface ActivationRow {
    state: 'active' | 'enabled' | 'draining' | 'cancelling' | 'upgrading' | 'off';
    seconds: number;
  }

  /** 排空键 `*` 表示整个插件正在升级。 */
  const UPGRADE_DRAIN_SCOPE = '*';

  const plugin = $derived(item.installed);
  const manifest = $derived<PluginManifest | null>(plugin?.manifest ?? item.bundled);
  const settingsTab = $derived(plugin && plugin.activeScopes.length > 0 ? pluginSettingsTab(plugin.manifest) : null);
  const activation = $derived(plugin ? activationRow(plugin) : null);
  const removable = $derived(Boolean(plugin) && plugin!.activeScopes.length === 0 && plugin!.enabledScopes.length === 0);

  function permissionText(permission: PluginPermission): string {
    return `${permission.kind}: ${permission.scope}${permission.targets.length ? `, ${permission.targets.join(', ')}` : ''}`;
  }

  function sourceLabel(installed: InstalledPluginProjection): string {
    return i18n.t(`settings.plugins.source.${installed.source.kind}`);
  }

  /** 插件全局生效，只有一个启用状态；排空中的状态来自 daemon 的投影。 */
  function activationRow(installed: InstalledPluginProjection): ActivationRow {
    const draining = installed.draining.find((entry) => entry.scope === APPLICATION_PLUGIN_SCOPE || entry.scope === UPGRADE_DRAIN_SCOPE);
    const seconds = draining ? Math.max(0, Math.ceil((draining.deadlineMs - now) / 1000)) : 0;
    if (draining?.scope === UPGRADE_DRAIN_SCOPE) return { state: 'upgrading', seconds };
    if (draining) return { state: draining.phase, seconds };
    if (installed.activeScopes.includes(APPLICATION_PLUGIN_SCOPE)) return { state: 'active', seconds };
    if (installed.enabledScopes.includes(APPLICATION_PLUGIN_SCOPE)) return { state: 'enabled', seconds };
    return { state: 'off', seconds };
  }

  function stateText(row: ActivationRow): string {
    if (row.state === 'draining') return i18n.t('settings.plugins.scopeState.draining', { seconds: row.seconds });
    if (row.state === 'cancelling') return i18n.t('settings.plugins.scopeState.cancelling', { seconds: row.seconds });
    return i18n.t(`settings.plugins.scopeState.${row.state}`);
  }
</script>

<article class="pane" data-testid="plugin-card" data-plugin-id={item.id}>
  <header class="pane-head">
    <div class="pane-title">
      <h3>{item.name}</h3>
      <span class="settings-chip">v{item.version}</span>
      {#if item.status === 'active'}<span class="settings-chip settings-chip--ok">{i18n.t('settings.plugins.status.active')}</span>
      {:else if item.status === 'working'}<span class="settings-chip settings-chip--accent">{i18n.t('settings.plugins.status.working')}</span>
      {:else if item.status === 'installed'}<span class="settings-chip">{i18n.t('settings.plugins.status.installed')}</span>
      {:else}<span class="settings-chip">{i18n.t('settings.plugins.status.notInstalled')}</span>{/if}
      {#if item.updatable}<span class="settings-chip settings-chip--accent">{i18n.t('settings.plugins.status.updatable')}</span>{/if}
    </div>
    <div class="pane-id settings-mono">{item.id}</div>
  </header>

  <p class="pane-desc">{item.description || item.id}</p>

  <div class="pane-actions">
    {#if plugin}
      {#if item.updatable && (item.bundled || item.catalog)}
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
      <label class="btn btn--sm btn--secondary" class:pane-disabled={busy}>
        {i18n.t('settings.plugins.upgradeFromFile')}
        <input type="file" accept=".zip,application/zip" hidden onchange={onUpgradeFile} disabled={busy} />
      </label>
      <button
        class="btn btn--sm btn--secondary"
        onclick={onUninstall}
        disabled={busy || !removable}
        title={removable ? undefined : i18n.t('settings.plugins.uninstallBlocked')}
      >{i18n.t('settings.plugins.uninstall')}</button>
    {:else}
      <button class="btn btn--lg btn--primary" data-testid="plugin-detail-install" onclick={onInstall} disabled={busy}>
        <Icon name="download" size={13} />
        <span>{i18n.t('settings.plugins.installBundled')}</span>
      </button>
    {/if}
  </div>

  {#if busy}
    <div class="settings-hint" role="status"><Icon name="loader" size={13} class="spinning" /> {i18n.t('common.loading')}</div>
  {/if}

  <dl class="pane-facts">
    <div><dt>{i18n.t('settings.plugins.detail.type')}</dt><dd>{item.bundled ? i18n.t('settings.plugins.detail.kindBuiltin') : i18n.t('settings.plugins.detail.kindExtension')}</dd></div>
    <div><dt>{i18n.t('settings.plugins.detail.version')}</dt><dd>v{item.version}{#if item.updatable} → v{item.bundled?.version ?? item.catalog?.version}{/if}</dd></div>
    {#if plugin}
      <div data-testid="plugin-source" class:pane-warn={plugin.source.kind === 'address' || plugin.source.kind === 'local'}>
        <dt>{i18n.t('settings.plugins.detail.source')}</dt>
        <dd>{sourceLabel(plugin)} <span class="settings-mono" title={plugin.source.location}>{plugin.source.location}</span></dd>
      </div>
      <div><dt>{i18n.t('settings.plugins.detail.digest')}</dt><dd class="settings-mono" title={plugin.digest}>{plugin.digest.slice(0, 16)}</dd></div>
    {:else if item.catalog}
      <div><dt>{i18n.t('settings.plugins.detail.size')}</dt><dd>{Math.max(1, Math.round(item.catalog.size / 1024))} KB</dd></div>
    {:else if item.bundled}
      <div><dt>{i18n.t('settings.plugins.detail.source')}</dt><dd>{i18n.t('settings.plugins.source.builtin')}</dd></div>
    {/if}
  </dl>

  <section class="pane-section">
    <h4>{i18n.t('settings.plugins.permissions')}</h4>
    {#if manifest}
      <div class="settings-chips">
        {#each manifest.permissions as permission}
          <span class="settings-chip settings-mono">{permissionText(permission)}</span>
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
      <h4>{i18n.t('settings.plugins.scopes')}</h4>
      <div class="pane-scopes">
        <div class="pane-scope" data-testid="plugin-scope-row" data-scope={APPLICATION_PLUGIN_SCOPE} data-state={activation.state}>
          <span class="settings-dot" class:settings-dot--ok={activation.state === 'active'} class:settings-dot--warn={activation.state === 'enabled'} class:settings-dot--busy={activation.state === 'draining' || activation.state === 'cancelling' || activation.state === 'upgrading'} aria-hidden="true"></span>
          <div class="settings-row-copy">
            <span class="settings-row-title">{i18n.t('settings.plugins.scope.application')}</span>
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
</article>

<style>
  .pane { display: grid; align-content: start; gap: var(--space-4); padding: var(--space-5); min-width: 0; user-select: text; }
  .pane-title { display: flex; align-items: center; gap: var(--space-2); flex-wrap: wrap; }
  .pane-title h3 { overflow-wrap: anywhere; min-width: 0; margin: 0; font-size: 20px; font-weight: calc(var(--font-semibold) + 50); color: var(--foreground); }
  .pane-id { margin-top: 6px; color: var(--foreground-muted); overflow-wrap: anywhere; }
  .pane-desc { margin: 0; color: var(--foreground); font-size: var(--text-sm); line-height: 1.6; }
  .pane-actions { display: flex; gap: var(--space-2); flex-wrap: wrap; }
  .pane-facts { margin: 0; display: grid; gap: 8px; }
  .pane-facts > div { display: grid; grid-template-columns: fit-content(30%) minmax(0, 1fr); gap: var(--space-3); align-items: baseline; }
  .pane-facts dt { overflow-wrap: anywhere; color: var(--foreground-muted); font-size: var(--text-xs); }
  .pane-facts dd { margin: 0; font-size: var(--text-xs); color: var(--foreground); overflow-wrap: anywhere; display: flex; gap: 8px; flex-wrap: wrap; }
  .pane-facts dd > *, .pane-scopes .settings-row-copy { min-width: 0; max-width: 100%; overflow-wrap: anywhere; }
  .pane-scope { flex-wrap: wrap; }
  .pane-warn dd { color: var(--warning); }
  .pane-section h4 { margin: 0 0 var(--space-2); font-size: var(--text-sm); font-weight: var(--font-bold); color: var(--foreground); }
  .pane-scopes { border: var(--magi-hairline) solid var(--border); border-radius: 10px; padding: 0 12px; }
  .pane-scope { display: flex; align-items: center; gap: var(--space-3); min-height: 46px; padding: var(--space-2) 0; }
  .pane-disabled { opacity: 0.45; pointer-events: none; }
</style>
