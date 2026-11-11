<script lang="ts">
  import { onMount } from 'svelte';
  import Icon from '../Icon.svelte';
  import { agentUrl } from '../../web/agent-api';
  import { i18n } from '../../stores/i18n.svelte';
  import { applyAppearanceSnapshot, getAppearanceRuntimeSnapshot, subscribeAppearanceRuntime } from '../../appearance/runtime';
  import { activateAppearanceTheme } from '../../appearance/client';
  import { openExternalUrl } from '../../lib/external-link';
  import { pluginProjection } from '../../stores/plugin-registry.svelte';
  import { rightPaneState, openPluginTab } from '../../stores/right-pane.svelte';
  import {
    callPluginRoute,
    describePluginSettings,
    readPluginResource,
    readPluginSettings,
    subscribePluginResource,
    writePluginResource,
    writePluginSettings,
  } from '../../web/plugin-api';
  import { createPluginComposerBridge } from '../../lib/plugin-composer-bridge';
  import { subscribePluginComposer } from '../../lib/plugin-composer';
  import { admitPluginComposer, admitPluginView } from '../../web/plugin-api';
  import type { PluginConversationViewContext } from '../../shared/app-server-protocol.generated';

  interface Props {
    pluginId: string;
    entry: string;
    label: string;
    /** 同一页面可能同时存在主视图和主页卡片，卡片用不同的测试标识避免混淆。 */
    frameTestId?: string;
    /**
     * 视图所处的位置。`settings` 嵌入设置页的整页滚动：高度由页面经 `view.resize` 上报，
     * 底色取设置页主区域的实际底色；其他位置由宿主给定固定尺寸，页面自己滚动。
     */
    placement?: 'settings' | 'page' | 'background' | 'composerLeft' | 'chatAssistant';
    viewEntry?: string | null;
    conversation?: PluginConversationViewContext;
  }

  let { pluginId, entry, label, frameTestId = 'plugin-view', placement = 'page', viewEntry = entry, conversation }: Props = $props();
  const inline = $derived(placement === 'composerLeft' || placement === 'chatAssistant');
  /** 页面上报的内容高度（CSS 像素）；只在 settings 位置生效，重新加载后清空。 */
  let reportedHeight = $state<number | null>(null);
  let overflowTop = $state(0);
  let overflowLeft = $state(0);
  let reportedWidth = $state<number | null>(null);
  let settingsInsets = $state({ left: 0, right: 0 });
  /** 与页面可上报的合理上限一致，防止异常值撑爆设置页。 */
  const MAX_VIEW_HEIGHT = 20_000;
  let iframe = $state<HTMLIFrameElement | null>(null);
  let instanceId = crypto.randomUUID();
  let destroyed = false;
  /** 与 daemon 资源快照上限一致：更大的写入在到达消息桥之前就拒绝。 */
  const MAX_WRITE_BYTES = 4 * 1024 * 1024;
  const installed = $derived(pluginProjection().installed.find((item) => item.manifest.id === pluginId));
  const runtime = $derived(installed?.runtime);
  const ready = $derived(Boolean(installed?.active && (!installed.manifest.extension || runtime?.state === 'running')));
  const waiting = $derived((!installed && !pluginProjection().loaded) || Boolean(installed?.active && (!runtime || runtime.state === 'starting' || runtime.state === 'restarting')));
  const runtimeState = $derived(installed?.active ? runtime?.state ?? 'starting' : 'notRunning');
  const resourceDigest = $derived(installed?.digest ?? '');
  // 监管可能合并中间状态；即使只看到 running → running，新运行代次也必须创建新文档。
  const documentIdentity = $derived(JSON.stringify([pluginId, entry, resourceDigest, ready, runtime?.instanceId, inline ? conversation?.sessionId : null]));
  const resourceUrl = $derived(
    agentUrl(`/api/plugins/${encodeURIComponent(pluginId)}/${resourceDigest ? `ui-v/${resourceDigest}` : 'ui'}/${entry.replace(/^ui\//, '').split('/').map(encodeURIComponent).join('/')}`),
  );
  const resourceOrigin = $derived(new URL(resourceUrl, window.location.href).origin);

  /**
   * 下发给插件页面的主题令牌：取宿主根元素当前生效的计算值，不依赖具体主题包。
   * 插件页面在自己的文档里把它们写成 CSS 变量，因此明暗切换和自定义主题都自然生效。
   */
  const THEME_TOKENS = [
    '--background', '--foreground', '--foreground-secondary', '--foreground-muted', '--border', '--border-subtle',
    '--surface-1', '--surface-2', '--surface-3', '--surface-hover', '--surface-active', '--surface-selected',
    '--primary', '--primary-hover', '--primary-foreground', '--primary-muted',
    '--secondary', '--secondary-hover',
    '--success', '--success-muted', '--warning', '--warning-muted', '--error', '--error-muted', '--info', '--info-muted',
    '--success-bg', '--warning-bg', '--error-bg', '--info-bg',
    '--success-text', '--warning-text', '--error-text', '--info-text',
    '--code-bg', '--dropdown-bg', '--overlay',
    '--text-xs', '--text-sm', '--text-base', '--text-md', '--text-lg', '--text-xl',
    '--space-1', '--space-2', '--space-3', '--space-4', '--space-5', '--space-6', '--space-8',
    '--radius-xs', '--radius-sm', '--radius-md', '--radius-lg', '--radius-xl', '--radius-2xl', '--font-semibold',
    '--shadow-sm', '--shadow-md', '--shadow-lg', '--duration-normal', '--ease-in-out',
    '--scrollbar-thumb', '--scrollbar-thumb-hover',
    // 主按钮的底色与文字色是一对实心反色；叠层色（--surface-*）是半透明的，不能当文字色用。
    '--btn-primary-bg', '--btn-primary-fg', '--btn-primary-hover-bg', '--magi-canvas',
    '--vscode-font-family', '--vscode-editor-font-family',
  ];

  function hostSurfaceColor(): string | null {
    for (let node: Element | null = iframe?.parentElement ?? null; node; node = node.parentElement) {
      const color = getComputedStyle(node).backgroundColor;
      if (color && color !== 'transparent' && !/^rgba\(.*,\s*0\)$/.test(color)) return color;
    }
    return null;
  }

  function readTheme() {
    const root = document.documentElement;
    const computed = getComputedStyle(root);
    const tokens: Record<string, string> = {};
    for (const name of THEME_TOKENS) {
      const value = computed.getPropertyValue(name).trim();
      if (value) tokens[name] = value;
    }
    // 设置页里页面应与周围同底色：背景令牌取容器链上第一个不透明的实际底色。
    if (placement === 'settings') {
      const surface = hostSurfaceColor();
      if (surface) tokens['--background'] = surface;
      // 设置页内的卡片圆角，与原生设置分类一致。
      const settingsScope = getComputedStyle(iframe?.closest('.settings-tab-content') ?? root);
      const radius = settingsScope.getPropertyValue('--ind-radius-card').trim();
      if (radius) tokens['--ind-radius-card'] = radius;
    }
    const { library, activeTheme } = getAppearanceRuntimeSnapshot();
    return { mode: root.classList.contains('theme-light') ? 'light' : 'dark', tokens,
      ...(library && activeTheme ? { appearance: {
        revision: library.revision, preference: library.activeThemeId, activeTheme,
        themes: library.themes.map(record => record.pack),
      } } : {}),
    };
  }

  function currentViewId(): string | null {
    return pluginProjection().manifests.find(item => item.id === pluginId)?.contributions.views?.find(view => view.entry === viewEntry)?.id ?? null;
  }

  function sendHostContext(): void {
    if (destroyed || placement === 'settings' && !iframe?.getClientRects().length) return;
    send({ type: 'context', placement, viewId: currentViewId(), locale: i18n.locale, theme: readTheme(), ...hostViewport() });
    void sendConversationContext();
  }
  function hostViewport() {
    if (placement === 'settings' && iframe) {
      const frame = iframe.closest('.settings-frame');
      const style = frame ? getComputedStyle(frame) : null;
      const left = Number.parseFloat(style?.paddingLeft ?? '') || 0;
      const right = Number.parseFloat(style?.paddingRight ?? '') || 0;
      if (settingsInsets.left !== left || settingsInsets.right !== right) settingsInsets = { left, right };
      return { settingsInsets: { left, right } };
    }
    if (settingsInsets.left || settingsInsets.right) settingsInsets = { left: 0, right: 0 };
    return inline && iframe ? { layoutWidth: iframe.parentElement?.clientWidth, availableWidth: window.innerWidth - iframe.getBoundingClientRect().left - 8 } : {};
  }
  let viewportObserver: ResizeObserver | undefined;

  let contextRevision = 0;
  async function sendConversationContext(): Promise<void> {
    if (!inline || !conversation || !documentDigest) return;
    const revision = ++contextRevision, expectedInstance = instanceId, context = conversation;
    try {
      await admitPluginView(pluginId, { expectedDigest: documentDigest, entry: documentEntry, placement: placement as 'composerLeft' | 'chatAssistant' });
      if (revision === contextRevision) send({ type: 'conversation', context }, expectedInstance);
    } catch {
      if (revision === contextRevision) send({ type: 'conversation', context: null }, expectedInstance);
    }
  }

  function send(message: Record<string, unknown>, expectedInstance = instanceId): void {
    if (destroyed || expectedInstance !== instanceId) return;
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
  const STREAM_ROUTE = '/__magi_compat/streams';
  const activeStreams = new Map<string, string>();
  let documentDigest = '';
  let documentRuntimeId: string | undefined;
  let documentEntry = '';
  let navigationIdentity = '';
  let composerSubscribed = false;
  let composerTarget: 'images' | 'text' = 'images';
  let composerBridge = makeComposerBridge();
  function makeComposerBridge() {
    const expectedInstance = instanceId;
    return createPluginComposerBridge(async (target) => {
      if (!documentDigest) throw new Error('插件页面缺少安装摘要');
      await admitPluginComposer(pluginId, { expectedDigest: documentDigest, entry: documentEntry, target });
    }, () => instanceId === expectedInstance, pluginId);
  }
  function sendComposerSnapshot(): void {
    if (destroyed || !composerSubscribed) return;
    const expectedInstance = instanceId;
    void composerBridge.call('composer.snapshot', { target: composerTarget }).then((snapshot) => {
      send({ type: 'composer', snapshot }, expectedInstance);
    }).catch(() => {
      if (expectedInstance !== instanceId) return;
      composerSubscribed = false;
      send({ type: 'composer', snapshot: { sessionId: null, attachmentIds: [], writable: false } }, expectedInstance);
    });
  }
  const routeControllers = new Map<string, AbortController>();

  function releaseRoutes(): void {
    for (const controller of routeControllers.values()) controller.abort();
    routeControllers.clear();
    for (const [id, owner] of activeStreams) {
      void callPluginRoute(owner, { method: 'POST', path: STREAM_ROUTE, body: { action: 'close', id }, expectedDigest: documentDigest, expectedInstanceId: documentRuntimeId }).catch(() => {});
    }
    activeStreams.clear();
    // 迟到 open 回执也不属于下一份文档；收到后会走相同 close 入口。
    instanceId = crypto.randomUUID();
    composerSubscribed = false;
    composerBridge = makeComposerBridge();
  }


  function releaseResourceSubscriptions(): void {
    for (const unsubscribe of activeResourceSubscriptions.values()) unsubscribe();
    activeResourceSubscriptions.clear();
  }

  function handleResourceSubscriptionRequest(requestId: string, resourceId: string): void {
    const expectedInstance = instanceId;
    if (activeResourceSubscriptions.has(resourceId)) {
      send({ type: 'response', requestId, ok: true });
      return;
    }
    const unsubscribe = subscribePluginResource(
      pluginId,
      resourceId,
      (resource) => {
        send({ type: 'event', resourceId, resource }, expectedInstance);
      },
      () => {
        // 订阅失败或被 daemon 结束（停用、撤权、升级）：释放句柄，视图可在恢复后重新订阅。
        if (expectedInstance !== instanceId) return;
        activeResourceSubscriptions.get(resourceId)?.();
        activeResourceSubscriptions.delete(resourceId);
        send({ type: 'event', resourceId, error: 'subscription_failed' });
      },
    );
    activeResourceSubscriptions.set(resourceId, unsubscribe);
    send({ type: 'response', requestId, ok: true });
  }

  async function handleMessage(event: MessageEvent): Promise<void> {
    if (destroyed || (event.origin !== resourceOrigin && event.origin !== 'null')
      || event.source !== iframe?.contentWindow || !event.data || event.data.channel !== 'magi-plugin-v1'
      || event.data.instanceId !== instanceId) return;
    const requestId = typeof event.data.requestId === 'string' ? event.data.requestId : '';
    if (event.data.type === 'cancel') {
      routeControllers.get(requestId)?.abort();
      return;
    }
    if (event.data.type !== 'request') return;
    const callingPluginId = pluginId;
    const expectedInstance = instanceId;
    const callingDigest = documentDigest;
    const callingRuntimeId = documentRuntimeId;
    const operation = event.data.operation;
    if (!requestId || !['resource.read', 'resource.write', 'resource.subscribe', 'settings.read', 'settings.write', 'settings.describe', 'route.call', 'host.openExternal', 'host.theme.set', 'clipboard.write', 'view.resize', 'view.open', 'view.storage.read', 'view.storage.write', 'composer.snapshot', 'composer.addImages', 'composer.removeImage', 'composer.notify', 'composer.focus', 'composer.submitText'].includes(operation)) return;
    try {
      if (operation === 'host.theme.set') {
        const { library } = getAppearanceRuntimeSnapshot();
        if (typeof event.data.themeId !== 'string' || !library?.themes.some(record => record.pack.id === event.data.themeId)
          || !Number.isSafeInteger(event.data.expectedRevision)) throw new Error('主题选择无效');
        const snapshot = await activateAppearanceTheme(event.data.themeId, event.data.expectedRevision);
        await applyAppearanceSnapshot(snapshot);
        send({ type: 'response', requestId, ok: true, result: null }, expectedInstance);
        return;
      }
      if (operation === 'view.storage.read' || operation === 'view.storage.write') {
        // 这是界面缓存，归浏览器 profile 所有；业务配置仍只走 daemon 的 settings CAS。
        const storageKey = `magi:plugin-ui:${pluginId}`;
        const read = (storage: Storage): Record<string, string> => {
          const value: unknown = JSON.parse(storage.getItem(storageKey) ?? '{}');
          if (!value || typeof value !== 'object' || Array.isArray(value) || Object.values(value).some((item) => typeof item !== 'string')) throw new Error('插件界面缓存无效');
          return value as Record<string, string>;
        };
        if (operation === 'view.storage.read') {
          send({ type: 'response', requestId, ok: true, result: { local: read(localStorage), session: read(sessionStorage) } }, expectedInstance);
          return;
        }
        if (!['local', 'session'].includes(event.data.area)) throw new Error('插件存储区域无效');
        const storage = event.data.area === 'local' ? localStorage : sessionStorage;
        const mutation = event.data.mutation;
        if (!mutation || !['set', 'remove', 'clear'].includes(mutation.kind) || (mutation.kind !== 'clear' && typeof mutation.key !== 'string') || (mutation.kind === 'set' && typeof mutation.value !== 'string')) throw new Error('插件存储操作无效');
        const values = new Map(Object.entries(read(storage)));
        if (mutation.kind === 'clear') values.clear();
        else if (mutation.kind === 'remove') values.delete(mutation.key);
        else values.set(mutation.key, mutation.value);
        const encoded = JSON.stringify(Object.fromEntries(values));
        if (encoded.length > MAX_WRITE_BYTES) throw new Error('插件界面缓存超过大小上限');
        storage.setItem(storageKey, encoded);
        send({ type: 'response', requestId, ok: true, result: null }, expectedInstance);
        return;
      }
      if (operation === 'view.open') {
        const manifest = pluginProjection().manifests.find((item) => item.id === pluginId);
        const view = manifest?.contributions.views?.find((item) => item.id === event.data.viewId && item.placements.includes('rightPane'));
        if (!view) throw new Error('插件未提供此右栏视图或已停用');
        const id = openPluginTab({ pluginId, contributionId: view.id, entry: view.entry, label: view.title,
          workspaceId: rightPaneState.activeWorkspaceId, sessionId: rightPaneState.activeSessionId });
        if (!id) throw new Error('当前没有可打开右栏的会话');
        send({ type: 'response', requestId, ok: true, result: id }, expectedInstance);
        return;
      }
      if (operation.startsWith('composer.')) {
        const result = await composerBridge.call(operation, event.data);
        if (expectedInstance !== instanceId) return;
        if (operation === 'composer.snapshot') {
          composerSubscribed = true;
          composerTarget = event.data.target === 'text' ? 'text' : 'images';
        }
        send({ type: 'response', requestId, ok: true, result }, expectedInstance);
        sendComposerSnapshot();
        return;
      }
      const resourceId = typeof event.data.resourceId === 'string' ? event.data.resourceId : '';
      if (operation.startsWith('resource.') && (!resourceId || resourceId.length > 256)) {
        throw new Error('插件资源标识无效');
      }
      if (operation.endsWith('.write') && JSON.stringify(event.data.value ?? null).length > MAX_WRITE_BYTES) {
        throw new Error('插件写入内容超过大小上限');
      }
      if (operation === 'resource.subscribe') {
        handleResourceSubscriptionRequest(requestId, resourceId);
        return;
      }
      if (operation === 'route.call') {
        // 路径形态由 daemon 的协议校验把关；这里只限制体积，避免页面把大块内容塞进消息桥。
        if (new TextEncoder().encode(JSON.stringify(event.data)).length > 3 * 1024 * 1024 + 64 * 1024) {
          throw new Error('插件写入内容超过大小上限');
        }
        const stream = event.data.path === STREAM_ROUTE ? event.data.body : undefined;
        if (event.data.path === STREAM_ROUTE) {
          if (!stream || !['open', 'read', 'close'].includes(stream.action) || event.data.method !== 'POST') throw new Error('插件事件请求无效');
          if (stream.action !== 'open' && !activeStreams.has(stream.id)) throw new Error('事件连接不属于当前视图');
          if (stream.action === 'open' && activeStreams.size >= 8) throw new Error('插件事件连接超过上限');
        }
        const controller = new AbortController();
        if (routeControllers.has(requestId)) return;
        routeControllers.set(requestId, controller);
        let routed;
        try { routed = await callPluginRoute(callingPluginId, {
          method: event.data.method,
          path: event.data.path,
          expectedDigest: callingDigest,
          expectedInstanceId: callingRuntimeId,
          ...(typeof event.data.query === 'string' ? { query: event.data.query } : {}),
          ...(event.data.headers === undefined ? {} : { headers: event.data.headers }),
          ...(event.data.body === undefined ? {} : { body: event.data.body }),
          ...(event.data.bodyBase64 === undefined ? {} : { bodyBase64: event.data.bodyBase64 }),
        }, controller.signal); }
        finally { if (routeControllers.get(requestId) === controller) routeControllers.delete(requestId); }
        if (stream?.action === 'open' && routed.status === 200 && typeof (routed.body as { id?: unknown })?.id === 'string') {
          const id = (routed.body as { id: string }).id;
          if (expectedInstance !== instanceId || controller.signal.aborted) {
            void callPluginRoute(callingPluginId, { method: 'POST', path: STREAM_ROUTE, body: { action: 'close', id }, expectedDigest: callingDigest, expectedInstanceId: callingRuntimeId }).catch(() => {});
            return;
          }
          activeStreams.set(id, callingPluginId);
        }
        if (stream?.action === 'close') activeStreams.delete(stream.id);
        send({ type: 'response', requestId, ok: true, result: routed }, expectedInstance);
        return;
      }
      if (operation === 'view.resize') {
        // 页面报告自己的内容高度；只有嵌入设置页整页滚动的位置采用，其余位置确认收到但不改变尺寸。
        const height = event.data.height;
        if (typeof height !== 'number' || !Number.isFinite(height) || height < 0 || height > MAX_VIEW_HEIGHT) {
          throw new Error('视图高度无效');
        }
        if (inline) {
          const top = event.data.overflowTop ?? 0, left = event.data.overflowLeft ?? 0, width = event.data.width;
          if (typeof top !== 'number' || !Number.isFinite(top) || top < 0 || top > 1000
            || typeof left !== 'number' || !Number.isFinite(left) || left < 0 || left > 1000
            || typeof width !== 'number' || !Number.isFinite(width) || width < 0 || width > 20000) throw new Error('内嵌视图尺寸无效');
          overflowTop = Math.ceil(top); overflowLeft = Math.ceil(left); reportedWidth = Math.ceil(width);
        }
        // 先校验整份尺寸消息；拒绝的请求不能留下部分布局更新。
        // 保活设置页隐藏时没有可测量视口，零尺寸不得覆盖最后一次有效高度，
        // 否则恢复布局会压缩滚动范围并永久夹断用户的阅读位置。
        if (inline || placement === 'settings' && iframe?.getClientRects().length) reportedHeight = Math.ceil(height);
        send({ type: 'response', requestId, ok: true, result: null }, expectedInstance);
        return;
      }
      if (operation === 'host.openExternal') {
        // 沙箱页面不能自己开窗口；链接交给宿主按产品规则打开（桌面端走系统浏览器，仅 http/https/mailto）。
        if (typeof event.data.url !== 'string') throw new Error('链接无效');
        await openExternalUrl(event.data.url);
        send({ type: 'response', requestId, ok: true, result: null }, expectedInstance);
        return;
      }
      if (operation === 'clipboard.write') {
        // 沙箱页面没有剪贴板权限；复制由用户点击触发，经桥写入，文本大小受限。
        if (typeof event.data.text !== 'string' || event.data.text.length > 65_536) throw new Error('剪贴板内容无效');
        await navigator.clipboard.writeText(event.data.text);
        send({ type: 'response', requestId, ok: true, result: null }, expectedInstance);
        return;
      }
      const result = operation === 'resource.read'
        ? await readPluginResource(pluginId, resourceId)
        : operation === 'resource.write'
          ? await writePluginResource(
            pluginId,
            resourceId,
                  typeof event.data.expectedVersion === 'number' ? event.data.expectedVersion : -1,
            event.data.value,
          )
          : operation === 'settings.describe'
            ? await describePluginSettings(pluginId)
          : operation === 'settings.read'
            ? await readPluginSettings(pluginId)
            : await writePluginSettings(
              pluginId,
                      typeof event.data.expectedVersion === 'number' ? event.data.expectedVersion : -1,
              event.data.value,
            );
      send({ type: 'response', requestId, ok: true, result }, expectedInstance);
    } catch (error) {
      send({ type: 'response', requestId, ok: false, error: error instanceof Error ? error.message : String(error) }, expectedInstance);
    }
  }

  onMount(() => {
    window.addEventListener('message', handleMessage);
    const unsubscribeComposer = subscribePluginComposer(sendComposerSnapshot);
    // 主题或语言变化时重新下发；订阅在挂载时立即回调一次，页面尚未初始化时 send 会被忽略，init 后再补一次。
    const unsubscribeAppearance = subscribeAppearanceRuntime(() => {
      // 变量应用到根元素之后再读取。
      queueMicrotask(sendHostContext);
    });
    return () => {
      destroyed = true;
      viewportObserver?.disconnect();
      window.removeEventListener('message', handleMessage);
      unsubscribeAppearance();
      unsubscribeComposer();
      releaseResourceSubscriptions();
      releaseRoutes();
    };
  });

  $effect(() => {
    // 语言切换：读取响应式的 locale 即建立依赖。
    void i18n.locale; void viewEntry; void placement; void conversation;
    sendHostContext();
  });

  $effect.pre(() => {
    // 注册表投影刷新也会重新下发 props；只有文档身份真正变化才撤销消息桥。
    // 否则更新其他插件会改变 nonce，而当前 iframe 没有导航，之后的请求全部失配。
    const identity = documentIdentity;
    if (identity === navigationIdentity) return;
    navigationIdentity = identity;
    releaseResourceSubscriptions();
    releaseRoutes();
    documentDigest = '';
    documentRuntimeId = undefined;
    documentEntry = '';
  });
</script>

{#if ready}
{#key documentIdentity}
<iframe
  class="plugin-view"
  class:plugin-view--settings={placement === 'settings'}
  class:plugin-view--sized={placement === 'settings' && reportedHeight !== null}
  class:plugin-view--inline={inline}
  style:height={(placement === 'settings' || inline) && reportedHeight !== null ? `${reportedHeight}px` : inline ? '40px' : undefined}
  style:margin-top={inline && overflowTop ? `${-overflowTop}px` : undefined}
  style:margin-left={placement === 'settings' ? `${-settingsInsets.left}px` : inline && overflowLeft ? `${-overflowLeft}px` : undefined}
  style:margin-right={placement === 'settings' ? `${-settingsInsets.right}px` : undefined}
  style:width={placement === 'settings' ? `calc(100% + ${settingsInsets.left + settingsInsets.right}px)` : inline && reportedWidth ? `${reportedWidth}px` : undefined}
  data-testid={frameTestId}
  data-plugin-id={pluginId}
  src={resourceUrl}
  title={label}
  bind:this={iframe}
  onload={() => {
    releaseResourceSubscriptions();
    releaseRoutes();
    reportedHeight = null;
    overflowTop = 0; overflowLeft = 0; reportedWidth = null;
    documentDigest = resourceDigest;
    documentRuntimeId = runtime?.instanceId;
    documentEntry = entry;
    viewportObserver?.disconnect();
    if ((inline || placement === 'settings') && iframe?.parentElement) {
      viewportObserver = new ResizeObserver(sendHostContext);
      viewportObserver.observe(iframe.parentElement);
      const frame = iframe.closest('.settings-frame');
      if (frame && frame !== iframe.parentElement) viewportObserver.observe(frame);
    }
    send({ type: 'init', pluginId, placement, viewId: currentViewId(), locale: i18n.locale, theme: readTheme(),
      ...hostViewport(),
      resourceBase: documentRuntimeId ? agentUrl(`/api/plugins/${encodeURIComponent(pluginId)}/assets/${encodeURIComponent(documentDigest)}/${encodeURIComponent(documentRuntimeId)}/`) : undefined });
    void sendConversationContext();
  }}
  sandbox="allow-scripts allow-downloads"
  referrerpolicy="no-referrer"
></iframe>
{/key}
{:else}
  <div class="settings-notice plugin-view-status" class:settings-notice--error={runtime?.state === 'failed'} role="status" aria-busy={waiting} data-testid="plugin-view-runtime" data-plugin-id={pluginId}>
    <Icon name={waiting ? 'loader' : 'warning'} class={waiting ? 'spinning' : ''} size={14} />
    <span>{waiting && !installed ? i18n.t('common.loading') : i18n.t(`settings.plugins.runtime.state.${runtimeState}`)}{#if installed?.active && runtime?.reason}<br />{runtime.reason}{/if}</span>
  </div>
{/if}

<style>
  .plugin-view-status { min-width: 0; overflow-wrap: anywhere; }
  .plugin-view-status :global(svg) { flex: none; }
  .plugin-view {
    display: block;
    width: 100%;
    min-height: 0;
    height: 100%;
    flex: 1;
    border: 0;
    background: var(--surface-1);
  }
  /* 设置页：与其他分类同一圆角，背景透明，高度随页面内容，整页由设置容器滚动。 */
  .plugin-view--settings {
    border-radius: var(--ind-radius-card, 12px);
    background: transparent;
  }
  .plugin-view--sized {
    flex: none;
    /* 内容高度决定宿主滚动范围，可用高度托住原包的最小视口。
       短页面也要保留完整视口，不能让 fixed 浮层和 vh 限高跟着正文缩小。 */
    min-height: 100%;
  }
  .plugin-view--inline { flex: none; background: transparent; }
</style>
