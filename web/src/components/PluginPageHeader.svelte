<script lang="ts">
  /**
   * 插件设置页顶部：图标、名称、启用开关与卸载。
   * 生命周期事实在 daemon；这里只发起与插件详情面板相同的操作，并在完成后刷新投影。
   */
  import { i18n } from '../stores/i18n.svelte';
  import Icon from './Icon.svelte';
  import { confirmMessage } from '../stores/confirm-dialog.svelte';
  import {
    authorizeAndActivatePlugin,
    deactivatePlugin,
    uninstallPlugin,
  } from '../web/plugin-api';
  import { pluginProjection } from '../stores/plugin-registry.svelte';
  import type { PluginManifest } from '../shared/app-server-protocol.generated';
  import { openSettings } from '../stores/shell-ui.svelte';
  import Toggle from './Toggle.svelte';

  interface Props {
    manifest: PluginManifest;
  }

  let { manifest }: Props = $props();
  let busy = $state(false);
  let message = $state('');

  // 状态只读全局投影：daemon 的注册表是唯一事实，操作完成后由注册表变化事件刷新它，这里不另存一份。
  const installed = $derived(pluginProjection().installed.find((plugin) => plugin.manifest.id === manifest.id) ?? null);
  const loaded = $derived(pluginProjection().loaded);

  const active = $derived(Boolean(installed?.active));

  async function run(operation: () => Promise<void>): Promise<void> {
    if (busy) return;
    busy = true;
    message = '';
    try {
      await operation();
    } catch (error) {
      message = error instanceof Error ? error.message : String(error);
    } finally {
      busy = false;
    }
  }

  function toggle(next: boolean): void {
    const plugin = installed;
    if (plugin) void run(() => next ? authorizeAndActivatePlugin(plugin) : deactivatePlugin(plugin));
  }

  async function uninstall(): Promise<void> {
    if (!(await confirmMessage(i18n.t('overlay.uninstallBody', { name: manifest.name }), {
      title: i18n.t('settings.plugins.uninstall'), confirmLabel: i18n.t('settings.plugins.uninstall'), tone: 'danger',
    }))) return;
    await run(async () => {
      await uninstallPlugin(manifest.id);
      // 插件已不存在，它的设置页随之失效：回到插件管理页。
      openSettings('plugins');
    });
  }
</script>

<div class="plugin-page-actions" data-testid="plugin-page-header" data-plugin-id={manifest.id}>
  <span class="plugin-page-version">v{manifest.version}</span>
  {#if busy}
    <span class="plugin-page-busy" data-testid="plugin-page-progress" role="status" aria-label={i18n.t('common.loading')}><Icon name="loader" size={14} class="spinning" /></span>
  {/if}
  <label class="plugin-page-switch">
    <span>{i18n.t(active ? 'settings.plugins.pageHeader.on' : 'settings.plugins.pageHeader.off')}</span>
    <Toggle checked={active} disabled={busy || !loaded || !installed} ariaLabel={i18n.t('settings.plugins.pageHeader.toggle', { name: manifest.name })} onchange={toggle} />
  </label>
  <button class="btn btn--sm btn--secondary" type="button" data-testid="plugin-page-uninstall" onclick={() => void uninstall()} disabled={busy || !loaded || !installed}>
    {i18n.t('settings.plugins.uninstall')}
  </button>
  {#if message}
    <span class="plugin-page-error" role="status" title={message}>{message}</span>
  {/if}
</div>

<style>
  .plugin-page-actions { display: flex; flex-wrap: wrap; align-items: center; gap: var(--space-4); min-width: 0; flex: 0 1 auto; }
  .plugin-page-busy { display: inline-flex; align-items: center; color: var(--foreground-muted); }
  .plugin-page-version { font-size: var(--text-xs); color: var(--foreground-muted); }
  .plugin-page-switch { display: inline-flex; align-items: center; gap: var(--space-3); font-size: var(--text-sm); color: var(--foreground); }
  .plugin-page-error { max-width: 240px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: var(--text-xs); color: var(--error); }
</style>
