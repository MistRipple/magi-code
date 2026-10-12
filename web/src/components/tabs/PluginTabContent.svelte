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
    /** 同一页面可能同时存在主视图和主页卡片，卡片用不同的测试标识避免混淆。 */
    frameTestId?: string;
  }

  let { pluginId, entry, label, scope, frameTestId = 'plugin-view' }: Props = $props();
  let iframe = $state<HTMLIFrameElement | null>(null);
  const instanceId = crypto.randomUUID();
  /** 与 daemon 资源快照上限一致：更大的写入在到达消息桥之前就拒绝。 */
  const MAX_WRITE_BYTES = 4 * 1024 * 1024;
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
        // 订阅失败或被 daemon 结束（停用、撤权、升级）：释放句柄，视图可在恢复后重新订阅。
        activeResourceSubscriptions.get(resourceId)?.();
        activeResourceSubscriptions.delete(resourceId);
        send({ type: 'event', resourceId, error: 'subscription_failed' });
      },
    );
    activeResourceSubscriptions.set(resourceId, unsubscribe);
    send({ type: 'response', requestId, ok: true });
  }

  /**
   * 视图把用户选中的对象交给宿主：只携带资源、版本、名称和简短摘要。插件身份来自本视图实例，
   * 不采信消息里自报的插件；是否合法由 daemon 在提交时按作用域和授权再次校验。
   */
  function selectContext(data: Record<string, unknown>): void {
    const resourceId = typeof data.resourceId === 'string' ? data.resourceId.trim() : '';
    const label = typeof data.label === 'string' ? data.label.trim() : '';
    if (!resourceId || resourceId.length > 64 || !label || label.length > 128 || typeof data.version !== 'number' || !Number.isSafeInteger(data.version) || data.version < 0) {
      throw new Error('插件选择需要有效的资源、版本和名称');
    }
    const summary = typeof data.summary === 'string' ? data.summary.trim().slice(0, 1024) : '';
    const selection = data.selection === undefined ? null : data.selection;
    if (JSON.stringify(selection).length > 4096) throw new Error('插件选中对象超过大小上限');
    window.dispatchEvent(new CustomEvent('magi:pluginSelectionAdded', {
      detail: { pluginId, resourceId, version: data.version, label, summary, selection },
    }));
  }

  async function handleMessage(event: MessageEvent): Promise<void> {
    if ((event.origin !== resourceOrigin && event.origin !== 'null')
      || event.source !== iframe?.contentWindow || !event.data || event.data.channel !== 'magi-plugin-v1'
      || event.data.instanceId !== instanceId || event.data.type !== 'request') return;
    const requestId = typeof event.data.requestId === 'string' ? event.data.requestId : '';
    const operation = event.data.operation;
    if (!requestId || !['resource.read', 'resource.write', 'resource.subscribe', 'settings.read', 'settings.write', 'context.select'].includes(operation)) return;
    try {
      const resourceId = typeof event.data.resourceId === 'string' ? event.data.resourceId : '';
      if (operation.startsWith('resource.') && (!resourceId || resourceId.length > 256)) {
        throw new Error('插件资源标识无效');
      }
      if (operation.endsWith('.write') && JSON.stringify(event.data.value ?? null).length > MAX_WRITE_BYTES) {
        throw new Error('插件写入内容超过大小上限');
      }
      if (operation === 'context.select') {
        selectContext(event.data);
        send({ type: 'response', requestId, ok: true });
        return;
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
  data-testid={frameTestId}
  data-plugin-id={pluginId}
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
