<script lang="ts">
  import PluginFrame from './PluginFrame.svelte';
  import { pluginProjection } from '../../stores/plugin-registry.svelte';
  import { registerPluginClientOutlet } from '../../lib/plugin-client-mount';
  interface Props { pluginId: string; entry: string; label: string; frameTestId?: string; placement?: 'settings' | 'page'; active?: boolean }
  let { pluginId, entry, label, frameTestId = 'plugin-view', placement = 'page', active = true }: Props = $props();
  function isPersistent(): boolean {
    return Boolean(pluginProjection().installed.find(item => item.manifest.id === pluginId)?.manifest.contributions.client);
  }
  let releaseOutlet = () => {};
  function parkBeforeRemoval(_node: HTMLElement) {
    // 全局 outro 在任一祖先分支移除 DOM 前运行；destroy 已晚于 iframe 被断开。
    releaseOutlet();
    return { duration: 0 };
  }
  function outlet(node: HTMLElement, value: { active: boolean; pluginId: string; entry: string; placement: 'settings' | 'page'; frameTestId: string }) {
    let release = () => {};
    const update = (next: typeof value) => {
      release();
      release = next.active ? registerPluginClientOutlet(next.pluginId, { node, entry: next.entry, placement: next.placement, frameTestId: next.frameTestId }) : () => {};
    };
    update(value);
    releaseOutlet = () => release();
    return { update, destroy() { release(); } };
  }
</script>

{#if isPersistent()}
  <div class="plugin-client-outlet" use:outlet={{ active, pluginId, entry, placement, frameTestId }} out:parkBeforeRemoval|global></div>
{:else}
  <PluginFrame {pluginId} {entry} {label} {frameTestId} {placement} />
{/if}

<style>
  .plugin-client-outlet { display: flex; flex-direction: column; flex: 1; min-height: 0; min-width: 0; width: 100%; }
</style>
