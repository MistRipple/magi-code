<script lang="ts">
  import { onMount } from 'svelte';
  import { agentUrl } from '../../web/agent-api';
  import {
    readPluginResource,
    readPluginSettings,
    subscribePluginResource,
    writePluginResource,
    writePluginSettings,
  } from '../../web/plugin-api';

  interface Props {
    pluginId: string;
    entry: string;
    label: string;
    scope: string;
  }

  let { pluginId, entry, label, scope }: Props = $props();
  let iframe = $state<HTMLIFrameElement | null>(null);
  const instanceId = crypto.randomUUID();
  const resourceUrl = $derived(
    agentUrl(`/api/plugins/${encodeURIComponent(pluginId)}/ui/${entry.replace(/^ui\//, '').split('/').map(encodeURIComponent).join('/')}?scope=${encodeURIComponent(scope)}`),
  );
  const resourceOrigin = $derived(new URL(resourceUrl, window.location.href).origin);

  function send(message: Record<string, unknown>): void {
    iframe?.contentWindow?.postMessage({
      channel: 'magi-plugin-v1',
      instanceId,
      ...message,
    // The sandbox intentionally keeps the plugin document on an opaque
    // origin (`null`).  The receiving side is still bound to this exact
    // contentWindow and the per-instance nonce below; using a concrete
    // origin here would silently drop every host message.
    }, '*');
  }

  const activeResourceSubscriptions = new Map<string, () => void>();

  function releaseResourceSubscriptions(): void {
    for (const unsubscribe of activeResourceSubscriptions.values()) unsubscribe();
    activeResourceSubscriptions.clear();
  }

  function handleResourceSubscriptionRequest(requestId: string, resourceId: string): void {
    if (activeResourceSubscriptions.has(resourceId)) {
      send({ type: 'response', requestId, ok: true });
      return;
    }
    const unsubscribe = subscribePluginResource(
      pluginId,
      resourceId,
      scope,
      (resource) => {
        send({ type: 'event', resourceId, resource });
      },
      () => {
        send({ type: 'event', resourceId, error: 'subscription_failed' });
      },
    );
    activeResourceSubscriptions.set(resourceId, unsubscribe);
    send({ type: 'response', requestId, ok: true });
  }

  async function handleMessage(event: MessageEvent): Promise<void> {
    if ((event.origin !== resourceOrigin && event.origin !== 'null')
      || event.source !== iframe?.contentWindow || !event.data || event.data.channel !== 'magi-plugin-v1'
      || event.data.instanceId !== instanceId || event.data.type !== 'request') return;
    const requestId = typeof event.data.requestId === 'string' ? event.data.requestId : '';
    const operation = event.data.operation;
    if (!requestId || !['resource.read', 'resource.write', 'resource.subscribe', 'settings.read', 'settings.write'].includes(operation)) return;
    try {
      const resourceId = typeof event.data.resourceId === 'string' ? event.data.resourceId : '';
      if (operation.startsWith('resource.') && (!resourceId || resourceId.length > 256)) {
        throw new Error('插件资源标识无效');
      }
      if (operation === 'resource.subscribe') {
        handleResourceSubscriptionRequest(requestId, resourceId);
        return;
      }
      const result = operation === 'resource.read'
        ? await readPluginResource(pluginId, resourceId, scope)
        : operation === 'resource.write'
          ? await writePluginResource(
            pluginId,
            resourceId,
            scope,
            typeof event.data.expectedVersion === 'number' ? event.data.expectedVersion : -1,
            event.data.value,
          )
          : operation === 'settings.read'
            ? await readPluginSettings(pluginId, scope)
            : await writePluginSettings(
              pluginId,
              scope,
              typeof event.data.expectedVersion === 'number' ? event.data.expectedVersion : -1,
              event.data.value,
            );
      send({ type: 'response', requestId, ok: true, result });
    } catch (error) {
      send({ type: 'response', requestId, ok: false, error: error instanceof Error ? error.message : String(error) });
    }
  }

  onMount(() => {
    window.addEventListener('message', handleMessage);
    return () => {
      window.removeEventListener('message', handleMessage);
      releaseResourceSubscriptions();
    };
  });
</script>

<iframe
  class="plugin-view"
  src={resourceUrl}
  title={label}
  bind:this={iframe}
  onload={() => send({ type: 'init', pluginId, scope })}
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
