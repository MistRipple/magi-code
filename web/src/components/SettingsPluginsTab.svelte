<script lang="ts">
  import { onMount } from 'svelte';
  import { i18n } from '../stores/i18n.svelte';
  import {
    authorizeAndActivatePlugin,
    deactivatePlugin,
    disablePlugin,
    installPluginAddress,
    installPluginArchive,
    listInstalledPlugins,
    uninstallPlugin,
    type InstalledPluginProjection,
  } from '../web/plugin-api';

  interface Props { workspaceId: string | null; }
  let { workspaceId }: Props = $props();
  let plugins = $state<InstalledPluginProjection[]>([]);
  let address = $state('');
  let busy = $state(false);
  let message = $state('');
  const scope = $derived(workspaceId?.trim() ? `workspace:${workspaceId.trim()}` : 'application');

  async function refresh(): Promise<void> {
    plugins = await listInstalledPlugins();
  }

  onMount(() => { void refresh().catch((error) => { message = String(error); }); });

  async function installFile(event: Event): Promise<void> {
    const input = event.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    if (!file) return;
    busy = true;
    message = '';
    try {
      await installPluginArchive(await file.arrayBuffer());
      await refresh();
      message = i18n.t('settings.plugins.installed');
    } catch (error) { message = String(error); }
    finally { busy = false; input.value = ''; }
  }

  async function installAddress(): Promise<void> {
    const source = address.trim();
    if (!source) return;
    busy = true;
    message = '';
    try {
      await installPluginAddress(source.includes(':') ? source : `address:${source}`);
      address = '';
      await refresh();
      message = i18n.t('settings.plugins.installed');
    } catch (error) { message = String(error); }
    finally { busy = false; }
  }

  async function activate(plugin: InstalledPluginProjection): Promise<void> {
    busy = true;
    message = '';
    try {
      await authorizeAndActivatePlugin(plugin.manifest.id, plugin.manifest, scope);
      await refresh();
      message = i18n.t('settings.plugins.activated');
    } catch (error) { message = String(error); }
    finally { busy = false; }
  }

  async function deactivate(plugin: InstalledPluginProjection): Promise<void> {
    busy = true;
    message = '';
    try {
      await deactivatePlugin(plugin.manifest.id, scope);
      await refresh();
      message = i18n.t('settings.plugins.deactivated');
    } catch (error) { message = String(error); }
    finally { busy = false; }
  }

  async function disable(plugin: InstalledPluginProjection): Promise<void> {
    busy = true;
    message = '';
    try {
      await disablePlugin(plugin.manifest.id, scope);
      await refresh();
      message = i18n.t('settings.plugins.disabled');
    } catch (error) { message = String(error); }
    finally { busy = false; }
  }

  async function remove(plugin: InstalledPluginProjection): Promise<void> {
    busy = true;
    message = '';
    try {
      await uninstallPlugin(plugin.manifest.id);
      await refresh();
      message = i18n.t('settings.plugins.uninstalled');
    } catch (error) { message = String(error); }
    finally { busy = false; }
  }
</script>

<section class="plugins-settings">
  <div class="settings-section-title">{i18n.t('settings.plugins.title')}</div>
  <p class="settings-hint">{i18n.t('settings.plugins.hint')}</p>
  <div class="plugin-install-row">
    <label class="btn btn--secondary btn--sm">
      {i18n.t('settings.plugins.installLocal')}
      <input type="file" accept=".zip,application/zip" hidden onchange={installFile} disabled={busy} />
    </label>
    <input class="form-input" bind:value={address} placeholder={i18n.t('settings.plugins.addressPlaceholder')} disabled={busy} />
    <button class="btn btn--primary btn--sm" onclick={() => void installAddress()} disabled={busy || !address.trim()}>{i18n.t('settings.plugins.installAddress')}</button>
  </div>
  {#if message}<p class="settings-hint">{message}</p>{/if}
  <div class="plugin-list">
    {#if plugins.length === 0}<p class="settings-hint">{i18n.t('settings.plugins.empty')}</p>{/if}
    {#each plugins as plugin (plugin.manifest.id)}
      <article class="plugin-card">
        <div>
          <strong>{plugin.manifest.name}</strong>
          <span class="plugin-version">{plugin.manifest.id} · v{plugin.manifest.version}</span>
          <p class="settings-hint">{plugin.manifest.description}</p>
          <p class="settings-hint">{i18n.t('settings.plugins.scopeStatus', { status: plugin.activeScopes.includes(scope) ? i18n.t('settings.plugins.active') : i18n.t('settings.plugins.inactive') })}</p>
          {#if plugin.manifest.permissions.length > 0}
            <div class="plugin-permissions">
              <span class="permission-title">{i18n.t('settings.plugins.permissions')}</span>
              {#each plugin.manifest.permissions as permission}
                <span class="permission-chip">
                  {permission.kind} · {permission.scope}{permission.targets.length ? ` · ${permission.targets.join(', ')}` : ''}
                </span>
              {/each}
            </div>
          {:else}
            <p class="settings-hint">{i18n.t('settings.plugins.noPermissions')}</p>
          {/if}
        </div>
        <div class="plugin-actions">
          {#if plugin.activeScopes.includes(scope)}
            <button class="btn btn--secondary btn--sm" onclick={() => void deactivate(plugin)} disabled={busy}>{i18n.t('settings.plugins.deactivate')}</button>
          {:else if plugin.enabledScopes.includes(scope)}
            <button class="btn btn--primary btn--sm" onclick={() => void activate(plugin)} disabled={busy}>{i18n.t('settings.plugins.activate')}</button>
            <button class="btn btn--secondary btn--sm" onclick={() => void disable(plugin)} disabled={busy}>{i18n.t('settings.plugins.disable')}</button>
          {:else}
            <button class="btn btn--primary btn--sm" onclick={() => void activate(plugin)} disabled={busy}>{i18n.t('settings.plugins.activate')}</button>
          {/if}
          <button class="btn btn--secondary btn--sm" onclick={() => void remove(plugin)} disabled={busy || plugin.activeScopes.length > 0 || plugin.enabledScopes.length > 0}>{i18n.t('settings.plugins.uninstall')}</button>
        </div>
      </article>
    {/each}
  </div>
</section>

<style>
  .plugins-settings { display: grid; gap: 14px; }
  .plugin-install-row { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
  .plugin-install-row .form-input { flex: 1 1 260px; min-width: 180px; }
  .plugin-list { display: grid; gap: 10px; }
  .plugin-card { display: flex; justify-content: space-between; gap: 12px; align-items: center; padding: 12px; border: 1px solid var(--border-subtle); border-radius: 8px; background: var(--surface-1); }
  .plugin-version { margin-left: 8px; color: var(--text-muted); font-size: 12px; }
  .plugin-actions { display: flex; gap: 8px; flex-wrap: wrap; }
  .plugin-permissions { display: flex; flex-wrap: wrap; gap: 6px; align-items: center; margin-top: 8px; }
  .permission-title { color: var(--text-muted); font-size: 12px; }
  .permission-chip { border: 1px solid var(--border-subtle); border-radius: 999px; padding: 2px 7px; color: var(--text-secondary); font-size: 11px; }
</style>
