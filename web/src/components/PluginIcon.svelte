<script lang="ts">
  /**
   * 插件图标：使用清单声明的图片（包括 SVG），没有声明或加载失败时退回通用插件图标。
   * 图标只作为图片展示，来源是 daemon 的 `/api/plugins/{id}/icon`，与插件是否激活无关。
   */
  import { agentUrl } from '../web/agent-api';
  import Icon from './Icon.svelte';
  import type { PluginUpstreamEntry } from '../shared/app-server-protocol.generated';

  interface Props {
    pluginId: string;
    /** 清单里的 `icon`；为空表示插件没有声明图标。 */
    icon?: string | null;
    size?: number;
    /** 摘要变化（升级）后让浏览器重新取图。 */
    revision?: string;
    upstream?: Pick<PluginUpstreamEntry, 'ecosystem' | 'package' | 'version'> | null;
  }

  let { pluginId, icon = null, upstream = null, size = 16, revision = '' }: Props = $props();
  let failed = $state(false);
  const src = $derived(
    icon ? agentUrl(`/api/plugins/${encodeURIComponent(pluginId)}/icon${revision ? `?v=${encodeURIComponent(revision.slice(0, 16))}` : ''}`)
      : upstream ? agentUrl(`/api/plugins/upstream/icon?${new URLSearchParams({ ecosystem: upstream.ecosystem, package: upstream.package, version: upstream.version })}`) : '',
  );
  $effect(() => { void src; failed = false; });
</script>

{#if src && !failed}
  <img class="plugin-icon" {src} alt="" loading="lazy" width={size} height={size} style:width="{size}px" style:height="{size}px" onerror={() => { failed = true; }} />
{:else}
  <Icon name="plug" {size} />
{/if}

<style>
  .plugin-icon {
    display: block;
    flex: 0 0 auto;
    object-fit: contain;
    border-radius: var(--radius-md);
  }
</style>
