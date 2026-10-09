<script lang="ts">
  import { agentUrl } from '../../web/agent-api';

  interface Props {
    pluginId: string;
    entry: string;
    label: string;
    scope: string;
  }

  let { pluginId, entry, label, scope }: Props = $props();
  const resourceUrl = $derived(
    agentUrl(`/api/plugins/${encodeURIComponent(pluginId)}/ui/${entry.replace(/^ui\//, '').split('/').map(encodeURIComponent).join('/')}?scope=${encodeURIComponent(scope)}`),
  );
</script>

<iframe
  class="plugin-view"
  src={resourceUrl}
  title={label}
  sandbox="allow-scripts"
  referrerpolicy="no-referrer"
></iframe>

<style>
  .plugin-view {
    display: block;
    width: 100%;
    min-height: 0;
    height: 100%;
    flex: 1;
    border: 0;
    background: var(--surface-1);
  }
</style>
