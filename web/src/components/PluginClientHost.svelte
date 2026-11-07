<script lang="ts">
  import { onMount } from 'svelte';
  import PluginFrame from './tabs/PluginFrame.svelte';
  import { movePluginClient, registerPluginClientHost, type PluginClientOutlet } from '../lib/plugin-client-mount';
  let { pluginId, entry, label }: { pluginId: string; entry: string; label: string } = $props();
  let parking: HTMLDivElement;
  let host: HTMLDivElement;
  let outlet = $state<PluginClientOutlet | null>(null);
  onMount(() => registerPluginClientHost(pluginId, {
    select(next) {
      movePluginClient(host, next?.node ?? parking);
      outlet = next;
    },
    dispose() { host.remove(); },
  }));
</script>

<div class="plugin-client-parking" bind:this={parking}>
  <div class="plugin-client-host" bind:this={host}>
    <PluginFrame {pluginId} {entry} {label}
      viewEntry={outlet?.entry ?? null}
      placement={outlet?.placement ?? 'background'}
      frameTestId={outlet?.frameTestId ?? 'plugin-client-background'} />
  </div>
</div>

<style>
  /* 客户端继续执行，停放区不占布局、不进入焦点与辅助技术导航。 */
  .plugin-client-parking { display: none; }
  .plugin-client-host { display: flex; flex-direction: column; width: 100%; height: 100%; min-width: 0; min-height: 0; }
</style>
