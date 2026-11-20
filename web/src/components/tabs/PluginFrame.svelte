<script lang="ts">
  import { onMount } from 'svelte';
  import Icon from '../Icon.svelte';
  import { agentUrl } from '../../web/agent-api';
  import { i18n } from '../../stores/i18n.svelte';
  import { applyAppearanceSnapshot, getAppearanceRuntimeSnapshot, subscribeAppearanceRuntime } from '../../appearance/runtime';
  import { activateAppearanceTheme } from '../../appearance/client';
  import { openExternalUrl } from '../../lib/external-link';
  import { pluginProjection } from '../../stores/plugin-registry.svelte';
  import { rightPaneState, getRightPaneState, openPluginTab, setActiveRightPaneTab, closeTab, type PluginTabPayload } from '../../stores/right-pane.svelte';
  import type { PluginPaneTab } from '../../lib/plugin-client-mount';
  import { closeSettings, openSettings } from '../../stores/shell-ui.svelte';
  import {
    callPluginRoute,
    installUpstreamPlugin,
    previewUpstreamPlugin,
    reportPluginClientSlots,
  } from '../../web/plugin-api';
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
    placement?: 'settings' | 'page' | 'rightPane' | 'background';
    viewEntry?: string | null;
    sessionId?: string;
    paneTab?: PluginPaneTab;
    registerClose?: (close: () => Promise<void>) => () => void;
  }

  let { pluginId, entry, label, frameTestId = 'plugin-view', placement = 'page', viewEntry = entry, sessionId, paneTab, registerClose }: Props = $props();
  /** 页面上报的内容高度（CSS 像素）；只在 settings 位置生效，重新加载后清空。 */
  let reportedHeight = $state<number | null>(null);
  let settingsInsets = $state({ left: 0, right: 0 });
  /** 与页面可上报的合理上限一致，防止异常值撑爆设置页。 */
  const MAX_VIEW_HEIGHT = 20_000;
  let iframe = $state<HTMLIFrameElement | null>(null);
  let instanceId = crypto.randomUUID();
  let destroyed = false;
  /** 插件界面缓存的大小上限。 */
  const MAX_WRITE_BYTES = 4 * 1024 * 1024;
  const installed = $derived(pluginProjection().installed.find((item) => item.manifest.id === pluginId));
  const runtime = $derived(installed?.runtime);
  const ready = $derived(Boolean(installed?.enabled && (!installed.manifest.extension || runtime?.state === 'running')));
  const waiting = $derived((!installed && !pluginProjection().loaded) || Boolean(installed?.enabled && (!runtime || runtime.state === 'starting' || runtime.state === 'restarting')));
  const runtimeState = $derived(installed?.enabled ? runtime?.state ?? 'starting' : 'notRunning');
  const resourceDigest = $derived(installed?.digest ?? '');
  const resourceRoot = $derived(resourceDigest
    ? `ui-v/${resourceDigest}${runtime?.instanceId ? `/instance/${encodeURIComponent(runtime.instanceId)}` : ''}`
    : 'ui');
  // 监管可能合并中间状态；即使只看到 running → running，新运行代次也必须创建新文档。
  const documentIdentity = $derived(JSON.stringify([pluginId, entry, resourceDigest, ready, runtime?.instanceId]));
  const resourceUrl = $derived(
    agentUrl(`/api/plugins/${encodeURIComponent(pluginId)}/${resourceRoot}/${entry.replace(/^ui\//, '').split('/').map(encodeURIComponent).join('/')}`),
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
    if (paneTab) return paneTab.viewId;
    return pluginProjection().manifests.find(item => item.id === pluginId)?.contributions.views?.find(view => view.entry === viewEntry)?.id ?? null;
  }

  let viewportObserver: ResizeObserver | undefined;
  let observedParent: Element | null = null;
  let observedFrame: Element | null = null;

  function disconnectViewportObserver(): void {
    viewportObserver?.disconnect();
    viewportObserver = undefined;
    observedParent = null;
    observedFrame = null;
  }

  function ensureSettingsViewportObserver(): void {
    if (placement !== 'settings' || !iframe?.parentElement) {
      disconnectViewportObserver();
      return;
    }
    const parent = iframe.parentElement;
    const frame = iframe.closest('.settings-frame');
    if (viewportObserver && observedParent === parent && observedFrame === frame) return;
    disconnectViewportObserver();
    observedParent = parent;
    observedFrame = frame;
    viewportObserver = new ResizeObserver(sendHostContext);
    viewportObserver.observe(parent);
    if (frame && frame !== parent) viewportObserver.observe(frame);
  }

  function sendHostContext(): void {
    if (destroyed || placement === 'settings' && !iframe?.getClientRects().length) return;
    if (placement === 'settings') ensureSettingsViewportObserver();
    else disconnectViewportObserver();
    send({
      type: 'context',
      placement,
      sessionId,
      paneTab,
      viewId: currentViewId(),
      locale: i18n.locale,
      theme: readTheme(),
      ...hostViewport(),
    });
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
    return {};
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

  let documentDigest = '';
  let documentRuntimeId: string | undefined;
  let navigationIdentity = '';
  let routedSessionId: string | undefined;
  const routeControllers = new Map<string, AbortController>();
  const muxSockets = new Map<string, WebSocket>();
  const MAX_MUX_SOCKETS = 8;
  let closeReady: (() => void) | undefined;
  let closing: Promise<void> | undefined;

  function prepareClose(): Promise<void> {
    if (!iframe?.contentWindow || !documentRuntimeId) return Promise.resolve();
    if (!closing) {
      closing = new Promise<void>(resolve => {
        const timeout = window.setTimeout(() => closeReady?.(), 500);
        closeReady = () => {
          window.clearTimeout(timeout);
          closeReady = undefined;
          resolve();
        };
        send({ type: 'prepareClose' });
      }).finally(() => { closing = undefined; });
    }
    return closing;
  }

  function abortRouteCalls(): void {
    for (const controller of routeControllers.values()) controller.abort();
    routeControllers.clear();
  }

  function releaseRoutes(): void {
    abortRouteCalls();
    for (const socket of muxSockets.values()) {
      try { socket.close(1000, 'frame released'); } catch { /* ignore */ }
    }
    muxSockets.clear();
    instanceId = crypto.randomUUID();
  }

  async function handleMessage(event: MessageEvent): Promise<void> {
    if (destroyed || (event.origin !== resourceOrigin && event.origin !== 'null')
      || event.source !== iframe?.contentWindow || !event.data || event.data.channel !== 'magi-plugin-v1'
      || event.data.instanceId !== instanceId) return;
    const expectedInstance = instanceId;
    if (event.data.type === 'ws.open') {
      const socketId = typeof event.data.socketId === 'string' ? event.data.socketId : '';
      const path = typeof event.data.path === 'string' ? event.data.path : '';
      if (!socketId || muxSockets.has(socketId) || muxSockets.size >= MAX_MUX_SOCKETS || path !== '/api/remote.mux') {
        if (socketId) {
          send({ type: 'ws.error', socketId }, expectedInstance);
          send({ type: 'ws.close', socketId, code: 1008, reason: 'invalid mux request' }, expectedInstance);
        }
        return;
      }
      const params = new URLSearchParams();
      if (documentDigest) params.set('expectedDigest', documentDigest);
      if (documentRuntimeId) params.set('expectedInstanceId', documentRuntimeId);
      const wsUrl = new URL(
        agentUrl(`/api/plugins/${encodeURIComponent(pluginId)}/mux${params.size ? `?${params.toString()}` : ''}`),
        window.location.href,
      );
      wsUrl.protocol = wsUrl.protocol === 'https:' ? 'wss:' : 'ws:';
      let ws: WebSocket;
      try {
        ws = new WebSocket(wsUrl.href);
      } catch {
        send({ type: 'ws.error', socketId }, expectedInstance);
        send({ type: 'ws.close', socketId, code: 1006, reason: 'websocket unavailable' }, expectedInstance);
        return;
      }
      muxSockets.set(socketId, ws);
      ws.addEventListener('open', () => {
        if (muxSockets.get(socketId) !== ws) return;
        send({ type: 'ws.open', socketId }, expectedInstance);
      });
      ws.addEventListener('message', ev => {
        if (muxSockets.get(socketId) !== ws) return;
        if (typeof ev.data === 'string') {
          send({ type: 'ws.message', socketId, data: ev.data }, expectedInstance);
        }
      });
      ws.addEventListener('error', () => {
        if (muxSockets.get(socketId) !== ws) return;
        send({ type: 'ws.error', socketId }, expectedInstance);
      });
      ws.addEventListener('close', ev => {
        if (muxSockets.get(socketId) === ws) muxSockets.delete(socketId);
        send({ type: 'ws.close', socketId, code: ev.code, reason: ev.reason }, expectedInstance);
      });
      return;
    }
    if (event.data.type === 'ws.send') {
      const socketId = typeof event.data.socketId === 'string' ? event.data.socketId : '';
      const ws = muxSockets.get(socketId);
      if (ws && ws.readyState === WebSocket.OPEN && typeof event.data.data === 'string'
        && event.data.data.length <= MAX_WRITE_BYTES) {
        ws.send(event.data.data);
      }
      return;
    }
    if (event.data.type === 'ws.close') {
      const socketId = typeof event.data.socketId === 'string' ? event.data.socketId : '';
      const ws = muxSockets.get(socketId);
      if (ws) {
        muxSockets.delete(socketId);
        const code = typeof event.data.code === 'number' ? event.data.code : undefined;
        const reason = typeof event.data.reason === 'string' ? event.data.reason : undefined;
        try { ws.close(code, reason); } catch { /* ignore */ }
      }
      return;
    }
    const requestId = typeof event.data.requestId === 'string' ? event.data.requestId : '';
    if (event.data.type === 'cancel') {
      routeControllers.get(requestId)?.abort();
      return;
    }
    if (event.data.type === 'closeReady') {
      closeReady?.();
      return;
    }
    if (event.data.type !== 'request') return;
    const callingPluginId = pluginId;
    const callingDigest = documentDigest;
    const callingRuntimeId = documentRuntimeId;
    const operation = event.data.operation;
    if (!requestId || ![
      'route.call', 'host.openExternal', 'host.theme.set', 'clipboard.write',
      'view.resize', 'view.open', 'view.close', 'view.focus', 'view.storage.read', 'view.storage.write',
      'slots.report', 'settings.close', 'plugins.install', 'plugins.update', 'plugins.openPackage',
    ].includes(operation)) return;
    try {
      if (operation === 'slots.report') {
        if (paneTab) throw new Error('右栏页面不负责发布插件贡献');
        const toStrArray = (val: unknown): string[] =>
          Array.isArray(val) ? val.filter((v): v is string => typeof v === 'string') : [];
        await reportPluginClientSlots(callingPluginId, {
          ...(callingRuntimeId ? { instanceId: callingRuntimeId } : {}),
          mountedSlots: toStrArray(event.data.mountedSlots),
          unmountedSlots: toStrArray(event.data.unmountedSlots),
          hasSettingsView: Boolean(event.data.hasSettingsView),
          mainKeys: toStrArray(event.data.mainKeys),
          ...(Array.isArray(event.data.rightPaneTabs) ? { rightPaneTabs: event.data.rightPaneTabs } : {}),
        });
        send({ type: 'response', requestId, ok: true, result: null }, expectedInstance);
        return;
      }
      if (operation === 'settings.close') {
        closeSettings();
        send({ type: 'response', requestId, ok: true, result: null }, expectedInstance);
        return;
      }
      if (operation === 'plugins.openPackage') {
        if (typeof event.data.packageName === 'string' && event.data.packageName.trim()) {
          openSettings('plugins', event.data.packageName.trim());
        }
        send({ type: 'response', requestId, ok: true, result: null }, expectedInstance);
        return;
      }
      if (operation === 'plugins.install' || operation === 'plugins.update') {
        let pkg = '';
        let ver: string | undefined;
        if (operation === 'plugins.update') {
          pkg = typeof event.data.packageName === 'string' ? event.data.packageName.trim() : '';
          ver = typeof event.data.version === 'string' && event.data.version.trim() ? event.data.version.trim() : undefined;
        } else if (typeof event.data.spec === 'string') {
          const raw = event.data.spec.trim();
          const atIdx = raw.lastIndexOf('@');
          if (atIdx > 0) {
            pkg = raw.slice(0, atIdx);
            ver = raw.slice(atIdx + 1) || undefined;
          } else {
            pkg = raw;
          }
        } else if (event.data.spec && typeof event.data.spec === 'object') {
          const specObj = event.data.spec as { package?: unknown; name?: unknown; version?: unknown };
          pkg = typeof specObj.package === 'string' ? specObj.package.trim() : typeof specObj.name === 'string' ? specObj.name.trim() : '';
          ver = typeof specObj.version === 'string' && specObj.version.trim() ? specObj.version.trim() : undefined;
        }
        if (!pkg) throw new Error('缺少插件包名');
        const preview = await previewUpstreamPlugin({ ecosystem: 'dsh', package: pkg, ...(ver ? { version: ver } : {}) });
        await installUpstreamPlugin(preview, 'dsh');
        send({ type: 'response', requestId, ok: true, result: { ok: true, package: preview.package, version: preview.version } }, expectedInstance);
        return;
      }
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
        if (!paneTab?.visible || paneTab.sessionId !== rightPaneState.activeSessionId
          || documentRuntimeId !== runtime?.instanceId || !ready) throw new Error('右栏视图已失效');
        const manifest = pluginProjection().manifests.find((item) => item.id === pluginId);
        const view = manifest?.contributions.views?.find((item) => item.id === event.data.viewId && item.placements.includes('rightPane'));
        if (!view) throw new Error('插件未提供此右栏视图或已停用');
        const address: unknown = event.data.address;
        if (typeof address !== 'string' || address.length > 8192 || !(
          address === ''
          || address.startsWith(`dsh-resource://file/session/${encodeURIComponent(paneTab.sessionId)}/`)
        )) throw new Error('此资源不属于当前右栏会话');
        const params: unknown = event.data.params;
        if (params !== undefined && (JSON.stringify(params)?.length ?? Infinity) > 16 * 1024) throw new Error('插件导航参数无效');
        const title = typeof event.data.title === 'string' ? event.data.title.trim().slice(0, 128) : '';
        const id = openPluginTab({ pluginId, contributionId: view.id, entry: view.entry, label: title || view.title,
          workspaceId: rightPaneState.activeWorkspaceId, sessionId: paneTab.sessionId, address, params });
        if (!id) throw new Error('当前没有可打开右栏的会话');
        send({ type: 'response', requestId, ok: true, result: id }, expectedInstance);
        return;
      }
      if (operation === 'view.close' || operation === 'view.focus') {
        if (!paneTab?.visible || paneTab.sessionId !== rightPaneState.activeSessionId
          || documentRuntimeId !== runtime?.instanceId || !ready) throw new Error('右栏视图已失效');
        const scope = rightPaneState.activeScopeKey;
        const tab = getRightPaneState(scope).openTabs.find(item => item.id === event.data.tabId
          && item.kind === 'plugin' && (item.payload as PluginTabPayload).pluginId === pluginId);
        if (!tab) throw new Error('此标签页不属于当前插件');
        if (operation === 'view.close') {
          if (tab.id !== paneTab.id) throw new Error('请从右栏关闭该标签页');
          await prepareClose();
          if (!destroyed && expectedInstance === instanceId) closeTab(scope, tab.id);
        } else {
          setActiveRightPaneTab(scope, tab.id);
        }
        send({ type: 'response', requestId, ok: true, result: null }, expectedInstance);
        return;
      }
      if (operation === 'route.call') {
        // 路径形态由 daemon 的协议校验把关；这里只限制体积，避免页面把大块内容塞进消息桥。
        if (new TextEncoder().encode(JSON.stringify(event.data)).length > 3 * 1024 * 1024 + 64 * 1024) {
          throw new Error('插件写入内容超过大小上限');
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
          ...(sessionId ? { sessionId } : {}),
          ...(typeof event.data.query === 'string' ? { query: event.data.query } : {}),
          ...(event.data.headers === undefined ? {} : { headers: event.data.headers }),
          ...(event.data.body === undefined ? {} : { body: event.data.body }),
          ...(event.data.bodyBase64 === undefined ? {} : { bodyBase64: event.data.bodyBase64 }),
        }, controller.signal); }
        finally { if (routeControllers.get(requestId) === controller) routeControllers.delete(requestId); }
        send({ type: 'response', requestId, ok: true, result: routed }, expectedInstance);
        return;
      }
      if (operation === 'view.resize') {
        // 页面报告自己的内容高度；只有设置页用于整页滚动，其他位置由宿主给定固定尺寸。
        const height = event.data.height;
        if (typeof height !== 'number' || !Number.isFinite(height) || height < 0 || height > MAX_VIEW_HEIGHT) {
          throw new Error('视图高度无效');
        }
        // 保活设置页隐藏时没有可测量视口，零尺寸不得覆盖最后一次有效高度，
        // 否则恢复布局会压缩滚动范围并永久夹断用户的阅读位置。
        if (placement === 'settings' && iframe?.getClientRects().length) reportedHeight = Math.ceil(height);
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
    } catch (error) {
      send({ type: 'response', requestId, ok: false, error: error instanceof Error ? error.message : String(error) }, expectedInstance);
    }
  }

  onMount(() => {
    const unregisterClose = registerClose?.(prepareClose);
    window.addEventListener('message', handleMessage);
    // 主题或语言变化时重新下发；订阅在挂载时立即回调一次，页面尚未初始化时 send 会被忽略，init 后再补一次。
    const unsubscribeAppearance = subscribeAppearanceRuntime(() => {
      // 变量应用到根元素之后再读取。
      queueMicrotask(sendHostContext);
    });
    return () => {
      destroyed = true;
      closeReady?.();
      unregisterClose?.();
      disconnectViewportObserver();
      window.removeEventListener('message', handleMessage);
      unsubscribeAppearance();
      releaseRoutes();
    };
  });

  $effect(() => {
    // 语言与外壳挂载位变化时同步下发；会话事实走 daemon → 共享宿主 session 网关的唯一事实源。
    void i18n.locale; void viewEntry; void placement; void sessionId; void paneTab;
    sendHostContext();
  });

  $effect.pre(() => {
    const next = sessionId;
    if (next === routedSessionId) return;
    routedSessionId = next;
    abortRouteCalls();
  });

  $effect.pre(() => {
    // 注册表投影刷新也会重新下发 props；只有文档身份真正变化才撤销消息桥。
    // 否则更新其他插件会改变 nonce，而当前 iframe 没有导航，之后的请求全部失配。
    const identity = documentIdentity;
    if (identity === navigationIdentity) return;
    navigationIdentity = identity;
    releaseRoutes();
    documentDigest = '';
    documentRuntimeId = undefined;
  });
</script>

{#if ready}
{#key documentIdentity}
<iframe
  class="plugin-view"
  class:plugin-view--settings={placement === 'settings'}
  class:plugin-view--sized={placement === 'settings' && reportedHeight !== null}
  style:height={placement === 'settings' && reportedHeight !== null ? `${reportedHeight}px` : undefined}
  style:margin-left={placement === 'settings' ? `${-settingsInsets.left}px` : undefined}
  style:margin-right={placement === 'settings' ? `${-settingsInsets.right}px` : undefined}
  style:width={placement === 'settings' ? `calc(100% + ${settingsInsets.left + settingsInsets.right}px)` : undefined}
  data-testid={frameTestId}
  data-plugin-id={pluginId}
  src={resourceUrl}
  title={label}
  bind:this={iframe}
  onload={() => {
    releaseRoutes();
    reportedHeight = null;
    documentDigest = resourceDigest;
    documentRuntimeId = runtime?.instanceId;
    disconnectViewportObserver();
    ensureSettingsViewportObserver();
    send({
      type: 'init',
      pluginId,
      placement,
      sessionId,
      paneTab,
      viewId: currentViewId(),
      locale: i18n.locale,
      theme: readTheme(),
      ...hostViewport(),
      resourceBase: documentRuntimeId ? agentUrl(`/api/plugins/${encodeURIComponent(pluginId)}/assets/${encodeURIComponent(documentDigest)}/${encodeURIComponent(documentRuntimeId)}/`) : undefined,
    });
  }}
  sandbox="allow-scripts allow-downloads"
  referrerpolicy="no-referrer"
></iframe>
{/key}
{:else}
  <div class="settings-notice plugin-view-status" class:settings-notice--error={runtime?.state === 'failed'} role="status" aria-busy={waiting} data-testid="plugin-view-runtime" data-plugin-id={pluginId}>
    <Icon name={waiting ? 'loader' : 'warning'} class={waiting ? 'spinning' : ''} size={14} />
    <span>{waiting && !installed ? i18n.t('common.loading') : i18n.t(`settings.plugins.runtime.state.${runtimeState}`)}{#if installed?.enabled && runtime?.reason}<br />{runtime.reason}{/if}</span>
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
</style>
