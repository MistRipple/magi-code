<script lang="ts">
  import type { PluginManifest } from '../shared/app-server-protocol.generated';
  import { pluginImpactNotices } from '../lib/plugin-store';
  import { i18n } from '../stores/i18n.svelte';
  import Icon from './Icon.svelte';
  let { manifest }: { manifest: PluginManifest } = $props();
  const notices = $derived(pluginImpactNotices(manifest, i18n.t.bind(i18n)));
</script>

{#each notices as notice (notice.id)}
  <p class="settings-notice" role="note" data-testid={notice.id === 'global-prompt' ? 'plugin-global-prompt-notice' : `plugin-impact-${notice.id}`}>
    <Icon name="info" size={13} /><span>{notice.text}</span>
  </p>
{/each}
