<script lang="ts">
  import PluginFrame from './PluginFrame.svelte';
  import { pluginProjection } from '../../stores/plugin-registry.svelte';
  import { registerPluginClientOutlet, type PluginClientOutlet, type PluginPaneTab } from '../../lib/plugin-client-mount';
  interface Props {
    pluginId: string;
    entry: string;
    label: string;
    frameTestId?: string;
    placement?: PluginClientOutlet['placement'] | 'rightPane';
    sessionId?: string;
    active?: boolean;
    paneTab?: PluginPaneTab;
    registerClose?: (close: () => Promise<void>) => () => void;
  }
  let { pluginId, entry, label, frameTestId = 'plugin-view', placement = 'page', sessionId, active = true, paneTab, registerClose }: Props = $props();
  function isPersistent(): boolean {
    return Boolean(pluginProjection().installed.find(item => item.manifest.id === pluginId)?.manifest.contributions.client);
  }
  let releaseOutlet = () => {};
  function parkBeforeRemoval(_node: HTMLElement) {
    // 全局 outro 在任一祖先分支移除 DOM 前运行；destroy 已晚于 iframe 被断开。
    releaseOutlet();
    return { duration: 0 };
  }
  type OutletValue = Omit<PluginClientOutlet, 'node' | 'placement'> & {
    active: boolean;
    pluginId: string;
    placement: PluginClientOutlet['placement'] | 'rightPane';
  };
  function outlet(node: HTMLElement, value: OutletValue) {
    let release = () => {};
    const update = (next: typeof value) => {
      release();
      const { active, pluginId, ...outlet } = next;
      if (!active || outlet.placement === 'rightPane') {
        release = () => {};
      } else {
        release = registerPluginClientOutlet(pluginId, {
          node,
          entry: outlet.entry,
          frameTestId: outlet.frameTestId,
          sessionId: outlet.sessionId,
          placement: outlet.placement,
        });
      }
    };
    update(value);
    releaseOutlet = () => release();
    return { update, destroy() { release(); } };
  }
</script>

{#if isPersistent() && placement !== 'rightPane'}
  <div class="plugin-client-outlet" use:outlet={{ active, pluginId, entry, placement, sessionId, frameTestId }} out:parkBeforeRemoval|global></div>
{:else}
  <PluginFrame {pluginId} {entry} {label} {frameTestId} {placement} {sessionId} {paneTab} {registerClose} />
{/if}

<style>
  .plugin-client-outlet { display: flex; flex-direction: column; flex: 1; min-height: 0; min-width: 0; width: 100%; }
</style>
