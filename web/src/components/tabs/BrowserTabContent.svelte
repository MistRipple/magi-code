<script lang="ts">
  import { onMount, tick, untrack } from 'svelte';
  import Icon from '../Icon.svelte';
  import { i18n } from '../../stores/i18n.svelte';
  import { normalizeExternalWebUrl, openExternalWebUrl } from '../../lib/external-link';
  import {
    browserScreenshotUrl,
    browserClientPlatform,
    getBrowserSession,
    isReferenceableBrowserAnnotation,
    navigateBrowserTab,
    type BrowserAnnotationSnapshot,
    type BrowserAnnotationSelection,
    type BrowserDeviceType,
    createBrowserAnnotation,
    type BrowserNormalizedRect,
    type BrowserSessionSnapshot,
    type BrowserTabLifecycle,
    type BrowserTabSnapshot,
    type BrowserViewportMode,
    BROWSER_AUTHORITY_CHANGED_EVENT,
  } from '../../web/agent-api';
  import { synchronizeBrowserSessionSnapshot } from '../../stores/right-pane.svelte';
  import type { MessageBrowserNodeSelection } from '../../types/message';
  import { normalizeOptionalDomNodeId } from '@magi/desktop-browser-contracts';

  interface Props {
    browserSessionId: string;
    tabId: string;
    lifecycle: BrowserTabLifecycle;
    workspaceId?: string;
    workspacePath?: string;
    sessionId?: string;
    onTitleChange?: (label: string) => void;
    desktopSurface?: boolean;
  }

  // 消息链路的节点上下文是唯一契约；Renderer 只补充 Chromium 返回的
  // 截断标记，避免在 Browser Tab 内维护第二套消息类型。
  export type BrowserNodeSelectionContext = MessageBrowserNodeSelection;

  interface BrowserInspectIdentity {
    tabId: string;
    surfaceId: string;
    navigationRevision: number;
  }

  interface EmbeddedBrowserWebviewElement extends HTMLElement {
    getWebContentsId(): number;
  }

  type TopLayerPopoverElement = HTMLDivElement;

  interface DesktopBrowserEvent {
    type?: string;
    binding?: {
      tab_id?: string;
      surface_id?: string;
      navigation_revision?: number;
    };
    page?: { url?: string; title?: string };
    loading?: boolean;
    reason?: string;
    url?: string;
    diagnostic?: string;
    node?: DesktopInspectedNode;
    payload?: Record<string, unknown>;
    downloadId?: string;
    suggestedFilename?: string;
    state?: string;
    receivedBytes?: number;
    totalBytes?: number | null;
    error?: string;
    selection?: {
      kind?: string;
      navigation_revision?: number;
      normalized_x?: number;
      normalized_y?: number;
      rect?: { x?: number; y?: number; width?: number; height?: number };
    };
  }

  interface DesktopInspectedNode {
    backend_node_id?: number;
    node_id?: number | null;
    frame_id?: string | null;
    node_name?: string;
    attributes?: Record<string, string>;
    node_value?: string;
    outer_html?: string;
    outer_html_truncated?: boolean;
    bounds?: { x: number; y: number; width: number; height: number } | null;
    page_url?: string;
    page_title?: string;
  }

  const VIEWPORT_DEVICE_MODES = [
    { id: 'wide', width: 1280, height: 800, deviceType: 'desktop' },
    { id: 'narrow', width: 390, height: 844, deviceType: 'mobile' },
  ] as const;
  const VIEWPORT_DIMENSION_LIMITS = {
    width: { min: 320, max: 7680 },
    height: { min: 240, max: 4320 },
  } as const;
  const CUSTOM_VIEWPORT_DEBOUNCE_MILLIS = 180;

  let {
    browserSessionId,
    tabId,
    lifecycle,
    workspaceId,
    workspacePath,
    sessionId,
    onTitleChange,
    desktopSurface = false,
  }: Props = $props();
  // Browser Tab 的运行通道由右栏宿主显式决定。Desktop 使用主 Renderer
  // 直接承载 Electron <webview>；Main 只负责控制 guest 的生命周期和 CDP，
  // 不参与右栏几何计算。
  const desktopRuntime = $derived(desktopSurface);
  let desktopSnapshot = $state<MagiDesktopWindowSnapshot | null>(null);
  let browserWebview = $state<EmbeddedBrowserWebviewElement | undefined>();
  let browserSurfaceSlot = $state<HTMLDivElement | undefined>();
  let browserToolbar = $state<HTMLDivElement | undefined>();
  let registeredWebviewKey = '';
  let registeredWebviewRelease: MagiDesktopReleasedBrowserWebviewRequest | null = null;
  let webviewRegistered = $state(false);
  let webviewRegistrationTimer: number | null = null;
  let webviewRegistrationAttempts = 0;
  let displaySizeObserver: ResizeObserver | null = null;
  let displaySizeAnimationFrame: number | null = null;
  let displaySizeSyncGeneration = 0;
  let lastDisplaySizeKey = '';
  let snapshot = $state<BrowserSessionSnapshot | null>(null);
  let address = $state('');
  let addressEditing = $state(false);
  let loading = $state(true);
  let browserLoading = $state(false);
  let sessionError = $state('');
  let actionError = $state('');
  let busy = $state(false);
  let viewportMenuElement = $state<TopLayerPopoverElement | undefined>();
  let annotationMenuElement = $state<TopLayerPopoverElement | undefined>();
  let viewportMenuButton = $state<HTMLButtonElement | undefined>();
  let annotationHistoryButton = $state<HTMLButtonElement | undefined>();
  let annotationCreateButton = $state<HTMLButtonElement | undefined>();
  let annotationEditorElement = $state<TopLayerPopoverElement | undefined>();
  let pageErrorElement = $state<TopLayerPopoverElement | undefined>();
  let actionErrorElement = $state<TopLayerPopoverElement | undefined>();
  let tooltipElement = $state<TopLayerPopoverElement | undefined>();
  let tooltipAnchorElement = $state<HTMLElement | undefined>();
  let tooltipAnchorPreviousValue = '';
  let tooltipAnchorPreviousPriority = '';
  let tooltipText = $state('');
  let viewportMenuOpen = $state(false);
  let annotationMenuOpen = $state(false);
  let localViewportMode = $state<BrowserViewportMode>('auto');
  let localViewport = $state({ width: 1280, height: 800, deviceType: 'desktop' as BrowserDeviceType });
  // 保留输入框的原始字符串，避免无效的空值被 Number('') 转成 0，
  // 进而在用户逐字符输入时被受控渲染抢回焦点内容。
  let customViewportWidthInput = $state('390');
  let customViewportHeightInput = $state('844');
  let customViewportWidthEditing = $state(false);
  let customViewportHeightEditing = $state(false);
  let customViewportInputDirty = $state(false);
  let customViewportTimer: number | null = null;
  let pendingViewport: { width: number; height: number; deviceType: BrowserDeviceType } | null = null;
  let viewportMutationGeneration = 0;
  let latestDesktopSnapshotRevision = 0;
  let annotationSelection = $state<BrowserAnnotationSelection | null>(null);
  let annotationComment = $state('');
  let annotationPhase = $state<'select' | 'comment' | null>(null);
  let annotationCaptureIdentity = $state<BrowserInspectIdentity | null>(null);
  let annotationCaptureGeneration = 0;
  let annotationEditor = $state<HTMLTextAreaElement | undefined>();
  let pageError = $state('');
  let nodeInspectActive = $state(false);
  let nodeInspectBusy = $state(false);
  let nodeSelection = $state<BrowserNodeSelectionContext | null>(null);
  let nodeInspectIdentity = $state<BrowserInspectIdentity | null>(null);
  let nodeInspectGeneration = 0;
  let refreshGeneration = 0;
  let desktopSurfaceSyncGeneration = 0;
  let activeBrowserIdentityKey = '';
  let activeDownloads = $state<MagiDesktopBrowserDownloadSnapshot[]>([]);
  const anchorToken = $derived(tabId.replace(/[^A-Za-z0-9_-]/gu, '_'));
  const toolbarAnchorName = $derived(`--magi-browser-toolbar-${anchorToken}`);
  const surfaceAnchorName = $derived(`--magi-browser-surface-${anchorToken}`);
  const viewportAnchorName = $derived(`--magi-browser-viewport-${anchorToken}`);
  const annotationHistoryAnchorName = $derived(`--magi-browser-annotations-${anchorToken}`);
  const tooltipAnchorName = $derived(`--magi-browser-tooltip-${anchorToken}`);

  $effect(() => {
    if (annotationPhase !== 'comment') return;
    void tick().then(() => {
      if (annotationPhase === 'comment') annotationEditor?.focus();
    });
  });

  $effect(() => {
    if (browserSurfaceAvailable) return;
    untrack(() => {
      viewportMenuOpen = false;
      annotationMenuOpen = false;
      annotationPhase = null;
      annotationSelection = null;
      annotationComment = '';
      stopDesktopAnnotationCapture(annotationCaptureIdentity);
      clearToolbarTooltip();
    });
  });

  // Electron 的 <webview> guest 是独立的原生合成层，普通 z-index 无法
  // 覆盖它。Popover Top Layer 仍由当前 Renderer 管理，但会被 Chromium
  // 放到 guest 之上，因此菜单、标记选择层和工具提示不会再被网页盖住。
  $effect(() => {
    const popovers: Array<[TopLayerPopoverElement | undefined, boolean]> = [
      [viewportMenuElement, viewportMenuOpen],
      [annotationMenuElement, annotationMenuOpen],
      [annotationEditorElement, annotationPhase === 'comment' && Boolean(annotationSelection)],
      [pageErrorElement, Boolean(pageError && browserReady && !browserLoading)],
      [actionErrorElement, Boolean(actionError && browserReady)],
      [tooltipElement, Boolean(tooltipText)],
    ];
    for (const [element, open] of popovers) {
      if (!element) continue;
      if (open) element.showPopover?.();
      else element.hidePopover?.();
    }
  });

  const activeTab = $derived.by<BrowserTabSnapshot | null>(() => (
    snapshot?.tabs.find((tab) => tab.tabId === tabId && tab.lifecycle !== 'closed') ?? null
  ));
  const savedAnnotations = $derived(
    (activeTab?.annotations ?? []).filter((annotation) => (
      isReferenceableBrowserAnnotation(annotation.status)
    )),
  );
  const activeDownload = $derived.by<MagiDesktopBrowserDownloadSnapshot | null>(() => {
    if (!desktopRuntime) return null;
    const downloads = activeDownloads.filter((download) => download.tabId === tabId);
    return downloads[downloads.length - 1] ?? null;
  });
  const externalUrl = $derived(normalizeExternalWebUrl(activeTab?.url || address));
  const browserSurfaceAvailable = $derived(
    desktopRuntime
      && desktopSnapshot?.layout.rightPaneVisible === true
      && desktopSnapshot?.layout.activePanelKind === 'browser'
      && desktopSnapshot.layout.activeTabId === tabId
      && Boolean(desktopSnapshot?.layout.activeSurfaceId)
      && webviewRegistered,
  );
  // activeSurfaceId 是 Main 为当前 Browser Tab 分配的逻辑页面身份。它不是
  // Authority/Worker 握手状态，也不会因页面刷新或右栏拖动而重置。
  const browserReady = $derived(browserSurfaceAvailable);
  const activeBrowserIdentity = $derived.by<BrowserInspectIdentity | null>(() => {
    const tab = activeTab;
    const surfaceId = desktopSnapshot?.layout.activeSurfaceId;
    const navigationRevision = desktopSnapshot?.activeBrowserNavigationRevision;
    if (
      !desktopRuntime
      || !tab
      || !surfaceId
      || typeof navigationRevision !== 'number'
      || !Number.isSafeInteger(navigationRevision)
      || navigationRevision < 0
      || desktopSnapshot?.layout.rightPaneVisible !== true
      || desktopSnapshot.layout.activePanelKind !== 'browser'
      || desktopSnapshot.layout.activeTabId !== tabId
    ) return null;
    return {
      tabId,
      surfaceId,
      navigationRevision,
    };
  });
  const error = $derived(actionError || pageError || sessionError);
  const lifecycleFailure = $derived(lifecycle === 'crashed' || activeTab?.lifecycle === 'crashed');
  const connectionState = $derived.by<'ready' | 'connecting' | 'error'>(() => {
    if (error || lifecycleFailure || !desktopRuntime) return 'error';
    // 这里只表示当前 Chromium Surface 是否已绑定到右栏内容槽。
    // Authority、daemon、Worker 的握手和页面导航都不参与这个状态。
    if (!browserReady) return 'connecting';
    return 'ready';
  });
  const connectionStatusText = $derived.by(() => {
    if (error) return error;
    if (lifecycleFailure) return i18n.t('browser.status.unavailable');
    if (!desktopRuntime) {
      return i18n.t('browser.error.internalUnavailable');
    }
    if (connectionState === 'ready') {
      // 页面导航的 loading 与 BrowserSurface 连接状态是两个独立状态。
      // Chromium guest 已经就绪时，即使页面正在请求资源，也不能把连接状态
      // 显示成“正在连接”，否则用户会误以为内置浏览器尚未启动。
      return i18n.t('browser.status.connected');
    }
    return i18n.t('browser.status.connecting');
  });

  function browserPartitionForSession(id: string): string {
    return `magi-browser-${id.replace(/[^A-Za-z0-9._-]/gu, '_')}`;
  }

  function currentBrowserDisplaySize(): MagiDesktopBrowserDisplaySize | null {
    const slot = browserSurfaceSlot;
    if (!slot) return null;
    const rect = slot.getBoundingClientRect();
    const width = Math.round(rect.width);
    const height = Math.round(rect.height);
    if (width < 1 || height < 1) return null;
    return { width, height };
  }

  function browserDisplaySizeKey(
    webContentsId: number,
    displaySize: MagiDesktopBrowserDisplaySize,
  ): string {
    return `${webContentsId}\u0000${displaySize.width}\u0000${displaySize.height}`;
  }

  function scheduleBrowserDisplaySizeSync(): void {
    if (displaySizeAnimationFrame !== null) return;
    displaySizeAnimationFrame = window.requestAnimationFrame(() => {
      displaySizeAnimationFrame = null;
      synchronizeBrowserDisplaySize();
    });
  }

  function synchronizeBrowserDisplaySize(): void {
    const desktop = window.magiDesktop;
    const release = registeredWebviewRelease;
    const displaySize = currentBrowserDisplaySize();
    if (!desktop || !release || !webviewRegistered || !displaySize) return;
    const key = browserDisplaySizeKey(release.webContentsId, displaySize);
    if (key === lastDisplaySizeKey) return;
    const generation = ++displaySizeSyncGeneration;
    lastDisplaySizeKey = key;
    void desktop.updateBrowserDisplaySize({
      ...release,
      displaySize,
    }).then((next) => {
      if (
        generation !== displaySizeSyncGeneration
        || registeredWebviewRelease?.webContentsId !== release.webContentsId
      ) return;
      applyDesktopViewport(next);
    }).catch((cause) => {
      if (
        generation !== displaySizeSyncGeneration
        || registeredWebviewRelease?.webContentsId !== release.webContentsId
      ) return;
      lastDisplaySizeKey = '';
      actionError = errorMessage(cause);
    });
  }

  function scheduleWebviewRegistration(delay = 0): void {
    if (webviewRegistrationTimer !== null || webviewRegistrationAttempts >= 40) return;
    const run = () => {
      webviewRegistrationTimer = null;
      registerCurrentWebview();
    };
    if (delay === 0) queueMicrotask(run);
    else webviewRegistrationTimer = window.setTimeout(run, delay);
  }

  function registerCurrentWebview(): void {
    const desktop = window.magiDesktop;
    const identity = activeBrowserIdentity;
    const view = browserWebview;
    if (!desktop || !identity || !view) return;
    let webContentsId: number;
    try {
      webContentsId = view.getWebContentsId();
    } catch {
      webviewRegistrationAttempts += 1;
      scheduleWebviewRegistration(Math.min(250, 40 + webviewRegistrationAttempts * 5));
      return;
    }
    if (!Number.isSafeInteger(webContentsId) || webContentsId <= 0) {
      webviewRegistrationAttempts += 1;
      scheduleWebviewRegistration(Math.min(250, 40 + webviewRegistrationAttempts * 5));
      return;
    }
    const displaySize = currentBrowserDisplaySize();
    if (!displaySize) {
      webviewRegistrationAttempts += 1;
      scheduleWebviewRegistration(Math.min(250, 40 + webviewRegistrationAttempts * 5));
      return;
    }
    const key = [identity.tabId, identity.surfaceId, identity.navigationRevision, webContentsId].join('\u0000');
    if (registeredWebviewKey === key) return;
    if (registeredWebviewRelease && registeredWebviewRelease.webContentsId !== webContentsId) {
      const previousRelease = registeredWebviewRelease;
      registeredWebviewRelease = null;
      void desktop.releaseBrowserWebview(previousRelease).catch(() => undefined);
    }
    registeredWebviewKey = key;
    webviewRegistrationAttempts = 0;
    webviewRegistered = false;
    void desktop.registerBrowserWebview({
      tabId: identity.tabId,
      browserSessionId,
      navigationRevision: identity.navigationRevision,
      webContentsId,
      displaySize,
    }).then((next) => {
      if (registeredWebviewKey !== key) return;
      applyDesktopViewport(next);
      webviewRegistered = true;
      registeredWebviewRelease = {
        tabId: identity.tabId,
        browserSessionId,
        navigationRevision: identity.navigationRevision,
        webContentsId,
      };
      lastDisplaySizeKey = browserDisplaySizeKey(webContentsId, displaySize);
      scheduleBrowserDisplaySizeSync();
    }).catch((cause) => {
      if (registeredWebviewKey !== key) return;
      registeredWebviewKey = '';
      lastDisplaySizeKey = '';
      webviewRegistered = false;
      actionError = errorMessage(cause);
      scheduleWebviewRegistration(250);
    });
  }

  $effect(() => {
    const tab = activeTab;
    if (!tab) return;
    onTitleChange?.(tab.title.trim() || (tab.url === 'about:blank' ? i18n.t('browser.tab.new') : tab.url));
  });

  function errorMessage(value: unknown): string {
    return value instanceof Error ? value.message : String(value);
  }

  async function refreshSession(initialLoad = false): Promise<void> {
    const expectedSessionId = browserSessionId;
    const expectedTabId = tabId;
    const generation = ++refreshGeneration;
    if (initialLoad) loading = true;
    try {
      const next = await getBrowserSession(expectedSessionId);
      if (generation !== refreshGeneration || expectedSessionId !== browserSessionId || expectedTabId !== tabId) return;
      snapshot = next;
      // BrowserAuthority 事件可能发生在会话导航完成之前，导致 App 层错过该事件。
      // 当前 Browser Tab 自己拿到的完整权威快照必须回写同一个右栏投影入口，
      // 否则本地仍停留在 creating，尽管 authority 和 Main Surface 已经 ready。
      synchronizeBrowserSessionSnapshot(next, workspacePath, {
        workspaceId,
        sessionId,
      });
      const nextTab = next.tabs.find((candidate) => candidate.tabId === expectedTabId && candidate.lifecycle !== 'closed');
      const nextUrl = nextTab?.url ?? '';
      if (initialLoad || !addressEditing) address = nextUrl;
      sessionError = '';
    } catch (cause) {
      if (generation === refreshGeneration) sessionError = errorMessage(cause);
    } finally {
      if (generation === refreshGeneration) {
        loading = false;
        if (desktopRuntime) {
          void synchronizeDesktopSurface();
        }
      }
    }
  }

  function sameBrowserIdentity(
    left: BrowserInspectIdentity | null,
    right: BrowserInspectIdentity | null,
  ): boolean {
    return Boolean(
      left
      && right
      && left.tabId === right.tabId
      && left.surfaceId === right.surfaceId
      && left.navigationRevision === right.navigationRevision,
    );
  }

  function stopDesktopAnnotationCapture(
    identity: BrowserInspectIdentity | null,
  ): void {
    ++annotationCaptureGeneration;
    annotationCaptureIdentity = null;
    if (!identity) return;
    void window.magiDesktop?.stopBrowserAnnotationCapture(identity).catch(() => undefined);
  }

  function notifyNodeSelectionInvalidated(selection: BrowserNodeSelectionContext | null): void {
    if (!selection) return;
    window.dispatchEvent(new CustomEvent('magi:browserNodeSelectionInvalidated', {
      detail: {
        browserSessionId: selection.browserSessionId,
        tabId: selection.tabId,
        surfaceId: selection.surfaceId,
        navigationRevision: selection.navigationRevision,
      },
    }));
  }

  function clearNodeInspection(preserveSelection = false): void {
    const identity = nodeInspectIdentity;
    const selection = nodeSelection;
    ++nodeInspectGeneration;
    nodeInspectIdentity = null;
    nodeInspectActive = false;
    if (!preserveSelection) {
      nodeSelection = null;
      notifyNodeSelectionInvalidated(selection);
    }
    // 本地状态是当前 Renderer 的生命周期事实源。导航、切 Tab、切面板和
    // 组件卸载都必须立即释放按钮；旧身份的 Main 清理请求不能阻塞下一次
    // 检查，也不能因为 stale binding 拒绝而把当前 UI 永久留在 disabled。
    nodeInspectBusy = false;

    const desktop = window.magiDesktop;
    // 节点选中后由 Main 的一次性 Inspect 事务负责清理 Chromium 检查态。
    // 这里仅清除 Renderer 的本地状态，不能再等待一个重复 stop 请求，
    // 否则节点采集完成后的慢 CDP 清理会把工具按钮锁成 disabled。
    if (preserveSelection || !identity || !desktop) return;
    // Main/Suface 侧按同一 Surface 的 inspect generation 做幂等清理。
    // 这里 fire-and-forget 是有意的：清理是旧生命周期的副作用，不能成为
    // 新生命周期的 UI 前置条件。
    void desktop.stopBrowserInspect(identity).catch(() => undefined);
  }

  function stopNodeInspectionForAnnotation(): void {
    const identity = nodeInspectIdentity;
    const shouldStop = nodeInspectActive || nodeInspectBusy;
    clearNodeInspection(true);
    if (!shouldStop || !identity) return;
    void window.magiDesktop?.stopBrowserInspect(identity).catch(() => undefined);
  }

  function toggleNodeInspection(): void {
    if (nodeInspectActive || nodeInspectBusy) {
      if (!busy) clearNodeInspection();
      return;
    }
    if (nodeInspectBusy || busy || !browserReady) {
      return;
    }
    const desktop = window.magiDesktop;
    const identity = activeBrowserIdentity;
    if (!desktop || !identity) return;

    const generation = ++nodeInspectGeneration;
    nodeInspectIdentity = identity;
    nodeInspectActive = false;
    nodeSelection = null;
    nodeInspectBusy = true;
    actionError = '';
    void desktop.startBrowserInspect(identity)
      .then((next) => {
        if (
          generation !== nodeInspectGeneration
          || !sameBrowserIdentity(nodeInspectIdentity, identity)
          || !sameBrowserIdentity(activeBrowserIdentity, identity)
        ) {
          // 生命周期清理已经处理了旧身份；仅在本次启动仍未被清理时
          // 追加一次同身份停止，避免异步 IPC 返回后重新留下检查模式。
          if (generation === nodeInspectGeneration && sameBrowserIdentity(nodeInspectIdentity, identity)) {
            clearNodeInspection();
          }
          return;
        }
        applyDesktopViewport(next);
        nodeInspectActive = true;
      })
      .catch((cause) => {
        if (generation !== nodeInspectGeneration) return;
        nodeInspectIdentity = null;
        nodeInspectActive = false;
        nodeSelection = null;
        actionError = errorMessage(cause);
      })
      .finally(() => {
        if (generation === nodeInspectGeneration) nodeInspectBusy = false;
      });
  }

  function finiteRectangle(value: unknown): BrowserNormalizedRect | null {
    if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
    const rect = value as Record<string, unknown>;
    if (![rect.x, rect.y, rect.width, rect.height].every((item) => typeof item === 'number' && Number.isFinite(item))) {
      return null;
    }
    return {
      x: rect.x as number,
      y: rect.y as number,
      width: rect.width as number,
      height: rect.height as number,
    };
  }

  function stringAttributes(value: unknown): Record<string, string> | null {
    if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
    const attributes: Record<string, string> = {};
    for (const [key, attribute] of Object.entries(value as Record<string, unknown>)) {
      if (typeof attribute !== 'string') return null;
      attributes[key] = attribute;
    }
    return attributes;
  }

  function nodeSelectionFromEvent(
    event: DesktopBrowserEvent,
    binding: { tabId: string; surfaceId: string; navigationRevision: number },
  ): BrowserNodeSelectionContext | null {
    const source = event.type === 'node_inspected'
      ? event.node as unknown
      : event.payload;
    if (!source || typeof source !== 'object' || Array.isArray(source)) return null;
    const value = source as Record<string, unknown>;
    const attributes = stringAttributes(value.attributes);
    const selectedBrowserSessionId = value.browser_session_id ?? value.browserSessionId;
    const backendDomNodeId = value.backend_node_id ?? value.backend_dom_node_id;
    const domNodeId = value.node_id ?? value.dom_node_id;
    const frameId = value.frame_id ?? value.frameId;
    const nodeName = value.node_name ?? value.nodeName;
    const url = value.page_url ?? value.url;
    const title = value.page_title ?? value.title;
    const outerHtml = value.outer_html ?? value.outerHtml;
    const outerHtmlTruncated = value.outer_html_truncated ?? value.outerHtmlTruncated;
    const boundsValue = value.bounds;
    const normalizedBackendDomNodeId = normalizeOptionalDomNodeId(backendDomNodeId);
    const normalizedDomNodeId = normalizeOptionalDomNodeId(domNodeId);
    const domNodeIdIsValid = domNodeId === null
      || domNodeId === undefined
      || domNodeId === 0
      || normalizedDomNodeId !== null;
    if (
      typeof backendDomNodeId !== 'number'
      || normalizedBackendDomNodeId === null
      || typeof selectedBrowserSessionId !== 'string'
      || selectedBrowserSessionId.trim() !== browserSessionId.trim()
      || !domNodeIdIsValid
      || (frameId !== null && frameId !== undefined && typeof frameId !== 'string')
      || typeof nodeName !== 'string'
      || !nodeName.trim()
      || !attributes
      || typeof url !== 'string'
      || typeof title !== 'string'
      || typeof outerHtml !== 'string'
      || typeof outerHtmlTruncated !== 'boolean'
      || !('bounds' in value)
    ) return null;
    const bounds = boundsValue === null ? null : finiteRectangle(boundsValue);
    if (boundsValue !== null && !bounds) return null;
    const nodeValue = typeof value.node_value === 'string'
      ? value.node_value.trim()
      : typeof value.text_excerpt === 'string'
        ? value.text_excerpt.trim()
        : '';
    const ariaRole = typeof value.aria_role === 'string'
      ? value.aria_role.trim() || null
      : attributes.role?.trim() || null;
    const ariaName = typeof value.aria_name === 'string'
      ? value.aria_name.trim() || null
      : attributes['aria-label']?.trim() || null;
    const textExcerpt = nodeValue || ariaName || attributes.title?.trim() || '';
    return {
      browserSessionId: selectedBrowserSessionId.trim(),
      tabId: binding.tabId,
      surfaceId: binding.surfaceId,
      navigationRevision: binding.navigationRevision,
      url,
      title,
      frameId: frameId === undefined ? null : frameId,
      backendDomNodeId: normalizedBackendDomNodeId,
      domNodeId: normalizedDomNodeId,
      nodeName,
      attributes,
      textExcerpt,
      outerHtml,
      outerHtmlTruncated,
      ariaRole,
      ariaName,
      bounds,
    };
  }

  function fixedPresetSelected(mode: (typeof VIEWPORT_DEVICE_MODES)[number]): boolean {
    return localViewportMode === 'fixed'
      && localViewport.width === mode.width
      && localViewport.height === mode.height;
  }

  function parseViewportDimension(
    raw: string,
    limits: { min: number; max: number },
  ): number | null {
    const normalized = raw.trim();
    if (!/^\d+$/u.test(normalized)) return null;
    const value = Number(normalized);
    if (!Number.isSafeInteger(value) || value < limits.min || value > limits.max) return null;
    return value;
  }

  function parsedCustomViewport(): { width: number; height: number; deviceType: BrowserDeviceType } | null {
    const width = parseViewportDimension(customViewportWidthInput, VIEWPORT_DIMENSION_LIMITS.width);
    const height = parseViewportDimension(customViewportHeightInput, VIEWPORT_DIMENSION_LIMITS.height);
    if (width === null || height === null) return null;
    return {
      width,
      height,
      deviceType: width <= 600 ? 'mobile' : 'desktop',
    };
  }

  function cancelPendingCustomViewport(): void {
    if (customViewportTimer !== null) window.clearTimeout(customViewportTimer);
    customViewportTimer = null;
    pendingViewport = null;
  }

  function normalizeCustomViewportInputs(): { width: number; height: number; deviceType: BrowserDeviceType } {
    const width = parseViewportDimension(customViewportWidthInput, VIEWPORT_DIMENSION_LIMITS.width)
      ?? localViewport.width;
    const height = parseViewportDimension(customViewportHeightInput, VIEWPORT_DIMENSION_LIMITS.height)
      ?? localViewport.height;
    customViewportWidthInput = String(width);
    customViewportHeightInput = String(height);
    return {
      width,
      height,
      deviceType: width <= 600 ? 'mobile' : 'desktop',
    };
  }

  async function updateLogicalViewport(
    mode: BrowserViewportMode,
    viewport?: { width: number; height: number; deviceType: BrowserDeviceType },
  ): Promise<void> {
    const tab = activeTab;
    const desktop = window.magiDesktop;
    if (!tab || !desktop) return;
    const mutationGeneration = ++viewportMutationGeneration;
    const next = await desktop.setBrowserViewport({
      tabId: tab.tabId,
      viewport: mode === 'auto'
        ? { mode: 'auto' }
        : {
            mode: 'fixed',
            width: viewport!.width,
            height: viewport!.height,
            deviceScaleFactorMillis: 1_000,
            deviceType: viewport!.deviceType,
          },
    });
    if (mutationGeneration !== viewportMutationGeneration) return;
    applyDesktopViewport(next);
    customViewportInputDirty = false;
    actionError = '';
  }

  function applyDesktopViewport(next: MagiDesktopWindowSnapshot): void {
    if (next.snapshotRevision < latestDesktopSnapshotRevision) return;
    latestDesktopSnapshotRevision = next.snapshotRevision;
    desktopSnapshot = next;
    const activeDownloadIds = new Set(
      next.activeBrowserDownloads.map((download) => download.downloadId),
    );
    const terminalDownloads = activeDownloads
      .filter((download) => (
        !activeDownloadIds.has(download.downloadId)
        && download.state !== 'started'
        && download.state !== 'progressing'
      ))
      .slice(-8);
    activeDownloads = [...terminalDownloads, ...next.activeBrowserDownloads];
    if (next.layout.activeTabId !== tabId || !next.activeBrowserViewport) return;
    const viewport = next.activeBrowserViewport;
    localViewportMode = viewport.mode;
    if (viewport.mode !== 'auto') {
      localViewport = {
        width: viewport.width,
        height: viewport.height,
        deviceType: viewport.device_type,
      };
      if (!customViewportWidthEditing) customViewportWidthInput = String(viewport.width);
      if (!customViewportHeightEditing) customViewportHeightInput = String(viewport.height);
    }
  }

  async function synchronizeDesktopSurface(): Promise<void> {
    const desktop = window.magiDesktop;
    const expectedTabId = tabId.trim();
    if (!desktopRuntime || !desktop || !expectedTabId) return;
    const generation = ++desktopSurfaceSyncGeneration;
    try {
      const next = await desktop.getSnapshot();
      if (generation !== desktopSurfaceSyncGeneration || expectedTabId !== tabId) return;
      applyDesktopViewport(next);
    } catch (cause) {
      if (generation === desktopSurfaceSyncGeneration) {
        actionError = errorMessage(cause);
      }
    }
  }

  function useAutomaticViewport(): void {
    cancelPendingCustomViewport();
    customViewportWidthEditing = false;
    customViewportHeightEditing = false;
    customViewportInputDirty = false;
    normalizeCustomViewportInputs();
    void run(async () => updateLogicalViewport('auto'));
  }

  function useFixedViewport(width: number, height: number): void {
    const viewport = { width, height, deviceType: width <= 600 ? 'mobile' as const : 'desktop' as const };
    cancelPendingCustomViewport();
    customViewportWidthEditing = false;
    customViewportHeightEditing = false;
    customViewportInputDirty = false;
    customViewportWidthInput = String(width);
    customViewportHeightInput = String(height);
    void run(async () => updateLogicalViewport('fixed', viewport));
  }

  function flushPendingCustomViewport(): void {
    customViewportTimer = null;
    const next = pendingViewport;
    pendingViewport = null;
    if (!next) return;
    if (busy) {
      pendingViewport = next;
      customViewportTimer = window.setTimeout(flushPendingCustomViewport, CUSTOM_VIEWPORT_DEBOUNCE_MILLIS);
      return;
    }
    void run(async () => updateLogicalViewport('fixed', next));
  }

  function scheduleCustomViewportCommit(
    viewport: { width: number; height: number; deviceType: BrowserDeviceType },
    delay = CUSTOM_VIEWPORT_DEBOUNCE_MILLIS,
  ): void {
    cancelPendingCustomViewport();
    pendingViewport = viewport;
    customViewportTimer = window.setTimeout(flushPendingCustomViewport, delay);
  }

  function scheduleCustomViewportUpdate(): void {
    cancelPendingCustomViewport();
    const viewport = parsedCustomViewport();
    if (!viewport) return;
    scheduleCustomViewportCommit(viewport);
  }

  function handleCustomViewportInput(
    dimension: 'width' | 'height',
    event: Event,
  ): void {
    const input = event.currentTarget as HTMLInputElement;
    if (dimension === 'width') {
      customViewportWidthEditing = true;
      customViewportWidthInput = input.value;
    } else {
      customViewportHeightEditing = true;
      customViewportHeightInput = input.value;
    }
    customViewportInputDirty = true;
    // 任何新输入都会使正在返回的旧 IPC 结果失效，避免旧值覆盖当前编辑内容。
    ++viewportMutationGeneration;
    scheduleCustomViewportUpdate();
  }

  function handleCustomViewportBlur(event: FocusEvent): void {
    const relatedTarget = event.relatedTarget;
    if (relatedTarget instanceof Node && viewportMenuElement?.contains(relatedTarget)) return;
    const wasEditing = customViewportWidthEditing || customViewportHeightEditing;
    const wasDirty = customViewportInputDirty;
    customViewportWidthEditing = false;
    customViewportHeightEditing = false;
    if (!wasEditing) return;
    // 失焦提交前先淘汰旧请求，避免旧响应在归一化后短暂回写旧尺寸。
    ++viewportMutationGeneration;
    const viewport = normalizeCustomViewportInputs();
    if (!wasDirty) return;
    customViewportInputDirty = false;
    scheduleCustomViewportCommit(viewport, 0);
  }

  function toggleViewportMenu(): void {
    if (!activeTab || busy || !browserReady) return;
    clearToolbarTooltip();
    annotationMenuOpen = false;
    viewportMenuOpen = !viewportMenuOpen;
  }

  function tooltipTarget(event: Event): HTMLElement | null {
    const target = event.target;
    return target instanceof Element
      ? target.closest<HTMLElement>('[data-tooltip]')
      : null;
  }

  function clearToolbarTooltip(): void {
    if (tooltipAnchorElement) {
      if (tooltipAnchorPreviousValue) {
        tooltipAnchorElement.style.setProperty(
          'anchor-name',
          tooltipAnchorPreviousValue,
          tooltipAnchorPreviousPriority,
        );
      } else {
        tooltipAnchorElement.style.removeProperty('anchor-name');
      }
    }
    tooltipAnchorElement = undefined;
    tooltipAnchorPreviousValue = '';
    tooltipAnchorPreviousPriority = '';
    tooltipText = '';
  }

  function updateToolbarTooltip(event: Event): void {
    const target = tooltipTarget(event);
    const text = target?.dataset.tooltip?.trim() ?? '';
    if (!target || !text || target.matches(':disabled')) {
      clearToolbarTooltip();
      return;
    }
    if (tooltipAnchorElement !== target) {
      clearToolbarTooltip();
      tooltipAnchorPreviousValue = target.style.getPropertyValue('anchor-name').trim();
      tooltipAnchorPreviousPriority = target.style.getPropertyPriority('anchor-name');
      const anchorNames = tooltipAnchorPreviousValue
        ? `${tooltipAnchorPreviousValue}, ${tooltipAnchorName}`
        : tooltipAnchorName;
      target.style.setProperty('anchor-name', anchorNames, tooltipAnchorPreviousPriority);
      tooltipAnchorElement = target;
    }
    tooltipText = text;
  }

  function handleToolbarPointerOut(event: PointerEvent): void {
    const target = tooltipTarget(event);
    const related = event.relatedTarget;
    if (target && related instanceof Node && target.contains(related)) return;
    if (related instanceof Node && browserToolbar?.contains(related)) return;
    clearToolbarTooltip();
  }

  function restoreOverlayTriggerFocus(
    target: HTMLButtonElement | undefined,
  ): void {
    void tick().then(() => target?.focus());
  }

  function handleToolbarFocusOut(event: FocusEvent): void {
    const related = event.relatedTarget;
    if (related instanceof Node && browserToolbar?.contains(related)) return;
    clearToolbarTooltip();
  }

  async function openDesktopAnnotationCreation(): Promise<void> {
    const tab = activeTab;
    const desktop = window.magiDesktop;
    const identity = activeBrowserIdentity;
    if (!tab || !desktop || !identity || !browserReady || annotationPhase) return;
    clearToolbarTooltip();
    stopNodeInspectionForAnnotation();
    annotationSelection = null;
    annotationComment = '';
    annotationPhase = 'select';
    viewportMenuOpen = false;
    annotationMenuOpen = false;
    const generation = ++annotationCaptureGeneration;
    annotationCaptureIdentity = identity;
    actionError = '';
    try {
      await desktop.startBrowserAnnotationCapture(identity);
      if (
        generation !== annotationCaptureGeneration
        || !sameBrowserIdentity(annotationCaptureIdentity, identity)
        || !sameBrowserIdentity(activeBrowserIdentity, identity)
        || annotationPhase !== 'select'
      ) {
        void desktop.stopBrowserAnnotationCapture(identity).catch(() => undefined);
      }
    } catch (cause) {
      if (generation !== annotationCaptureGeneration) return;
      annotationCaptureIdentity = null;
      annotationPhase = null;
      annotationSelection = null;
      annotationComment = '';
      actionError = errorMessage(cause);
    }
  }

  function cancelAnnotationCreation(): void {
    ++annotationCaptureGeneration;
    const identity = annotationCaptureIdentity;
    annotationCaptureIdentity = null;
    annotationPhase = null;
    annotationSelection = null;
    annotationComment = '';
    if (identity) {
      void window.magiDesktop?.stopBrowserAnnotationCapture(identity).catch(() => undefined);
    }
  }

  function submitCreatedAnnotation(): void {
    const tab = activeTab;
    const selection = annotationSelection;
    const comment = annotationComment.trim();
    if (!tab || !selection || !comment || annotationPhase !== 'comment') return;
    void run(async () => {
      const created = await createBrowserAnnotation(tab.tabId, selection, comment);
      await refreshSession(true);
      window.dispatchEvent(new CustomEvent('magi:browserAnnotationCreated', { detail: created }));
      annotationSelection = null;
      annotationComment = '';
      annotationPhase = null;
    });
  }

  function toggleAnnotationMenu(): void {
    if (!activeTab || busy || !browserReady || annotationPhase) return;
    clearToolbarTooltip();
    viewportMenuOpen = false;
    annotationMenuOpen = !annotationMenuOpen;
  }

  async function run(action: () => Promise<void>): Promise<void> {
    if (busy) return;
    busy = true;
    actionError = '';
    try {
      await action();
    } catch (cause) {
      actionError = errorMessage(cause);
    } finally {
      busy = false;
    }
  }

  function navigate(action: 'url' | 'back' | 'forward' | 'reload'): void {
    const tab = activeTab;
    if (!tab || !browserReady) return;
    addressEditing = false;
    clearNodeInspection();
    void run(async () => {
      const updated = await navigateBrowserTab(tab.tabId, action, action === 'url' ? address : undefined);
      address = updated.url;
      await refreshSession();
    });
  }

  function stopNavigation(): void {
    const tab = activeTab;
    if (!tab || !browserReady || !browserLoading) return;
    void (async () => {
      actionError = '';
      try {
        const updated = await navigateBrowserTab(tab.tabId, 'stop');
        address = updated.url;
        await refreshSession();
      } catch (cause) {
        actionError = errorMessage(cause);
      }
    })();
  }

  function cancelBrowserDownload(): void {
    const download = activeDownload;
    if (!download || download.state !== 'started' && download.state !== 'progressing') return;
    void (async () => {
      actionError = '';
      try {
        const next = await window.magiDesktop?.cancelBrowserDownload({
          tabId,
          downloadId: download.downloadId,
        });
        if (!next) throw new Error(i18n.t('browser.error.internalUnavailable'));
        applyDesktopViewport(next);
      } catch (cause) {
        actionError = errorMessage(cause);
      }
    })();
  }

  function formatDownloadBytes(value: number): string {
    if (value < 1024) return `${value} B`;
    const units = ['KB', 'MB', 'GB'];
    let size = value;
    let unitIndex = -1;
    do {
      size /= 1024;
      unitIndex += 1;
    } while (size >= 1024 && unitIndex < units.length - 1);
    return `${size.toFixed(size >= 10 ? 0 : 1)} ${units[unitIndex]}`;
  }

  function openCurrentPageExternally(): void {
    if (!externalUrl) return;
    void run(async () => {
      if (desktopRuntime && window.magiDesktop) {
        await window.magiDesktop.openExternal(externalUrl);
        return;
      }
      await openExternalWebUrl(externalUrl);
    });
  }

  function blobDataUrl(blob: Blob): Promise<string> {
    return new Promise((resolve, reject) => {
      const reader = new FileReader();
      reader.onload = () => typeof reader.result === 'string'
        ? resolve(reader.result)
        : reject(new Error(i18n.t('browser.error.screenshot')));
      reader.onerror = () => reject(reader.error ?? new Error(i18n.t('browser.error.screenshot')));
      reader.readAsDataURL(blob);
    });
  }

  function captureScreenshotForMessage(): void {
    const tab = activeTab;
    if (!tab || !browserReady) return;
    void run(async () => {
      const response = await fetch(browserScreenshotUrl(tab.tabId), {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ fullPage: false, clientPlatform: browserClientPlatform() }),
      });
      if (!response.ok) throw new Error(i18n.t('browser.error.screenshot').replace('{status}', String(response.status)));
      const blob = await response.blob();
      window.dispatchEvent(new CustomEvent('magi:browserScreenshotCaptured', {
        detail: {
          name: `magi-browser-${Date.now()}.png`,
          dataUrl: await blobDataUrl(blob),
          size: blob.size,
          type: blob.type || 'image/png',
        },
      }));
    });
  }

  function selectSavedAnnotation(annotation: BrowserAnnotationSnapshot): void {
    annotationMenuOpen = false;
    restoreOverlayTriggerFocus(annotationHistoryButton);
    window.dispatchEvent(new CustomEvent('magi:browserAnnotationCreated', { detail: annotation }));
  }

  function browserEventBinding(event: DesktopBrowserEvent): BrowserInspectIdentity | null {
    // Main Renderer 的 node_selection 事件使用与 Host 相同的结构化 payload，
    // 其身份不再放在旁路 binding 中。其它页面生命周期事件仍使用顶层
    // binding；明确按事件类型分流，避免接收不完整或混合形状的数据。
    const candidate = event.type === 'node_selection'
      ? event.payload
      : event.binding;
    if (!candidate) return null;
    if (
      typeof candidate.tab_id !== 'string'
      || !candidate.tab_id
      || typeof candidate.surface_id !== 'string'
      || !candidate.surface_id
      || typeof candidate.navigation_revision !== 'number'
      || !Number.isSafeInteger(candidate.navigation_revision)
      || candidate.navigation_revision < 0
    ) return null;
    return {
      tabId: candidate.tab_id,
      surfaceId: candidate.surface_id,
      navigationRevision: candidate.navigation_revision,
    };
  }

  function annotationSelectionFromEvent(
    event: DesktopBrowserEvent,
    binding: BrowserInspectIdentity,
  ): BrowserAnnotationSelection | null {
    const raw = event.selection;
    if (!raw || raw.navigation_revision !== binding.navigationRevision) return null;
    const isNormalized = (value: unknown): value is number => (
      typeof value === 'number' && Number.isFinite(value) && value >= 0 && value <= 1
    );
    if (raw.kind === 'element' && isNormalized(raw.normalized_x) && isNormalized(raw.normalized_y)) {
      return {
        kind: 'element',
        navigationRevision: binding.navigationRevision,
        normalizedX: raw.normalized_x,
        normalizedY: raw.normalized_y,
      };
    }
    const rect = raw.rect;
    if (
      raw.kind !== 'region'
      || !rect
      || !isNormalized(rect.x)
      || !isNormalized(rect.y)
      || !isNormalized(rect.width)
      || !isNormalized(rect.height)
      || rect.width <= 0
      || rect.height <= 0
      || rect.x + rect.width > 1
      || rect.y + rect.height > 1
    ) return null;
    return {
      kind: 'region',
      navigationRevision: binding.navigationRevision,
      rect: {
        x: rect.x,
        y: rect.y,
        width: rect.width,
        height: rect.height,
      },
    };
  }

  function handleDesktopBrowserEvent(value: unknown): void {
    if (!value || typeof value !== 'object' || Array.isArray(value)) return;
    const event = value as DesktopBrowserEvent;
    const binding = browserEventBinding(event);
    const currentIdentity = activeBrowserIdentity;
    const tab = activeTab;
    if (!binding || binding.tabId !== tabId) return;

    if (event.type === 'download') {
      if (event.downloadId !== undefined) {
        applyDownloadEvent(event);
      }
      return;
    }

    if (
      event.type === 'primary_changed'
      || event.type === 'primary_surface_changed'
      || event.type === 'control_revoked'
      || event.type === 'page_crashed'
    ) {
      clearNodeInspection();
      cancelAnnotationCreation();
      if (event.type === 'page_crashed') {
        pageError = event.reason?.trim() || event.diagnostic?.trim() || i18n.t('browser.error.pageLoadFailed');
        browserLoading = false;
      }
      return;
    }

    if (!currentIdentity || !tab || binding.surfaceId !== currentIdentity.surfaceId) return;

    if (event.type === 'annotation_selection') {
      if (
        !sameBrowserIdentity(annotationCaptureIdentity, binding)
        || !sameBrowserIdentity(currentIdentity, binding)
      ) return;
      const selection = annotationSelectionFromEvent(event, binding);
      if (!selection) {
        actionError = i18n.t('browser.annotation.failed');
        stopDesktopAnnotationCapture(binding);
        annotationPhase = null;
        annotationSelection = null;
        annotationComment = '';
        return;
      }
      stopDesktopAnnotationCapture(binding);
      annotationSelection = selection;
      annotationComment = '';
      annotationPhase = 'comment';
      actionError = '';
      return;
    }

    if (event.type === 'popup_blocked') {
      actionError = popupBlockedMessage(event.reason);
      return;
    }

    if (event.type === 'node_inspected' || event.type === 'node_selection') {
      // 节点上下文必须精确匹配启动检查时的三元身份。页面事件允许
      // 使用更高 revision 穿过导航竞态，但节点绝不能跨代次复用。
      if (binding.navigationRevision > currentIdentity.navigationRevision) {
        clearNodeInspection();
        return;
      }
      if (
        !nodeInspectActive
        || !sameBrowserIdentity(nodeInspectIdentity, currentIdentity)
        || binding.navigationRevision !== currentIdentity.navigationRevision
      ) return;
      const selection = nodeSelectionFromEvent(event, binding);
      if (!selection) {
        actionError = i18n.t('browser.nodeSelection.failed');
        clearNodeInspection();
        return;
      }
      nodeSelection = selection;
      actionError = '';
      window.dispatchEvent(new CustomEvent<BrowserNodeSelectionContext>(
        'magi:browserNodeSelected',
        { detail: selection },
      ));
      // Chromium 的 inspect mode 是一次选择动作。选中后立即退出，避免
      // 后续普通网页点击继续被检查态拦截；已选上下文仍保留给消息层。
      clearNodeInspection(true);
      return;
    }

    if (binding.navigationRevision < tab.navigationRevision) return;
    const pageEventIsNewerThanSnapshot = binding.navigationRevision > tab.navigationRevision;
    if (pageEventIsNewerThanSnapshot) {
      // Desktop Surface 事件与 Authority 快照是两条异步链。页面已经由
      // Chromium 提交时，右栏组件可能仍持有上一代快照；新页面事件本身
      // 是当前 Surface 的权威事实，不能因为快照尚未到达而丢弃地址、标题
      // 和加载状态。
      clearNodeInspection();
    }
    if (event.type === 'user_takeover') {
      // 普通鼠标移动/点击只代表用户重新接管浏览器，不代表已经选中的
      // DOM 上下文失效。节点选择已经复制到 InputArea 的消息草稿中，
      // 这里仅结束当前 Inspect 状态，保留对话框中的节点引用直到用户
      // 主动移除，或页面/Tab 生命周期明确使其失效。
      clearNodeInspection(true);
      return;
    }
    if (event.type === 'loading_changed') {
      browserLoading = event.loading === true;
      if (browserLoading) {
        actionError = '';
        pageError = '';
        clearNodeInspection();
        // 页面导航会使未提交选择失去语义；已保存的标记仍由 Authority
        // 持久化并在新文档就绪后重放。
        cancelAnnotationCreation();
        viewportMenuOpen = false;
        annotationMenuOpen = false;
      }
      if (pageEventIsNewerThanSnapshot) void refreshSession();
      return;
    }
    if (event.type === 'page_failed') {
      clearNodeInspection();
      cancelAnnotationCreation();
      pageError = event.reason?.trim() || i18n.t('browser.error.pageLoadFailed');
      browserLoading = false;
      if (pageEventIsNewerThanSnapshot) void refreshSession();
      return;
    }
    if (event.type !== 'page_updated' || !event.page) return;
    if (event.page.url?.trim()) {
      if (!addressEditing) address = event.page.url;
    }
    if (event.page.title?.trim()) onTitleChange?.(event.page.title);
    if (pageEventIsNewerThanSnapshot) void refreshSession();
  }

  function popupBlockedMessage(reason: string | undefined): string {
    switch (reason) {
      case 'unsupported_protocol':
        return i18n.t('browser.popup.unsupportedProtocol');
      case 'script_blank_window':
        return i18n.t('browser.popup.scriptBlankWindow');
      case 'named_window':
        return i18n.t('browser.popup.namedWindow');
      case 'opener_required':
        return i18n.t('browser.popup.openerRequired');
      case 'separate_window_features':
        return i18n.t('browser.popup.separateWindowFeatures');
      default:
        return i18n.t('browser.popup.invalidUrl');
    }
  }

  function applyDownloadEvent(event: DesktopBrowserEvent): void {
    const downloadId = event.downloadId;
    const suggestedFilename = event.suggestedFilename?.trim();
    const state = event.state;
    if (
      typeof downloadId !== 'string'
      || !downloadId
      || typeof suggestedFilename !== 'string'
      || !suggestedFilename
      || state !== 'started'
      && state !== 'progressing'
      && state !== 'completed'
      && state !== 'cancelled'
      && state !== 'interrupted'
    ) return;
    const receivedBytes = event.receivedBytes;
    const totalBytes = event.totalBytes;
    if (
      typeof receivedBytes !== 'number'
      || !Number.isFinite(receivedBytes)
      || receivedBytes < 0
      || totalBytes !== null && (
        typeof totalBytes !== 'number'
        || !Number.isFinite(totalBytes)
        || totalBytes < 0
      )
    ) return;
    const next: MagiDesktopBrowserDownloadSnapshot = {
      downloadId,
      tabId,
      suggestedFilename,
      state,
      receivedBytes: Math.floor(receivedBytes),
      totalBytes: totalBytes === null ? null : Math.floor(totalBytes),
    };
    activeDownloads = [
      ...activeDownloads.filter((download) => download.downloadId !== downloadId),
      next,
    ];
  }

  $effect(() => {
    if (!desktopRuntime) return;
    void synchronizeDesktopSurface();
  });

  $effect(() => {
    const identity = activeBrowserIdentity;
    if (!identity) {
      ++displaySizeSyncGeneration;
      lastDisplaySizeKey = '';
      registeredWebviewKey = '';
      webviewRegistered = false;
      webviewRegistrationAttempts = 0;
      if (webviewRegistrationTimer !== null) {
        window.clearTimeout(webviewRegistrationTimer);
        webviewRegistrationTimer = null;
      }
      return;
    }
    // <webview> 的 guest 在元素 did-attach 后才有 WebContents id；把注册
    // 放到当前渲染提交之后，避免 activation IPC 与 DOM guest 建立竞争。
    scheduleWebviewRegistration();
  });

  $effect(() => {
    const currentIdentity = activeBrowserIdentity;
    const inspectedIdentity = nodeInspectIdentity;
    const selectedIdentity = nodeSelection;
    if (
      (inspectedIdentity && !sameBrowserIdentity(inspectedIdentity, currentIdentity))
      || (selectedIdentity && !sameBrowserIdentity(selectedIdentity, currentIdentity))
    ) {
      // Tab、Surface、导航代次或右栏面板任一项变化，都必须让旧检查请求
      // 和旧节点选择失效。停止调用使用旧身份，绝不能把清理动作发给
      // 新页面；选择上下文也不能跨导航继续进入消息输入层。
      untrack(() => clearNodeInspection());
    }
  });

  $effect(() => {
    const currentIdentity = activeBrowserIdentity;
    const captureIdentity = annotationCaptureIdentity;
    if (captureIdentity && !sameBrowserIdentity(captureIdentity, currentIdentity)) {
      untrack(cancelAnnotationCreation);
    }
  });

  $effect(() => {
    const expectedSessionId = browserSessionId.trim();
    const expectedTabId = tabId.trim();
    const identityKey = `${expectedSessionId}\u0000${expectedTabId}`;
    if (identityKey === activeBrowserIdentityKey) return;
    activeBrowserIdentityKey = identityKey;
    untrack(() => {
      clearNodeInspection();
      cancelPendingCustomViewport();
      ++viewportMutationGeneration;
      ++displaySizeSyncGeneration;
      lastDisplaySizeKey = '';
      customViewportWidthEditing = false;
      customViewportHeightEditing = false;
      customViewportInputDirty = false;
      snapshot = null;
      address = '';
      addressEditing = false;
      sessionError = '';
      actionError = '';
      pageError = '';
      browserLoading = false;
      cancelAnnotationCreation();
      viewportMenuOpen = false;
      annotationMenuOpen = false;
      activeDownloads = [];
      loading = Boolean(expectedSessionId && expectedTabId);
      if (!expectedSessionId || !expectedTabId) return;
      void refreshSession(true);
    });
  });

  onMount(() => {
    const desktop = window.magiDesktop;
    if (desktop) void synchronizeDesktopSurface();
    const view = browserWebview;
    const handleWebviewAttached = () => scheduleWebviewRegistration();
    const handleWebviewReady = () => scheduleWebviewRegistration();
    view?.addEventListener('did-attach', handleWebviewAttached);
    view?.addEventListener('dom-ready', handleWebviewReady);
    scheduleWebviewRegistration();
    displaySizeObserver = browserSurfaceSlot
      ? new ResizeObserver(scheduleBrowserDisplaySizeSync)
      : null;
    if (browserSurfaceSlot && displaySizeObserver) {
      displaySizeObserver.observe(browserSurfaceSlot);
    }
    const pointerDown = (event: PointerEvent) => {
      const target = event.target;
      if (!(target instanceof Node)) return;
      if (viewportMenuElement && !viewportMenuElement.contains(target) && !viewportMenuButton?.contains(target)) viewportMenuOpen = false;
      if (annotationMenuElement && !annotationMenuElement.contains(target) && !annotationHistoryButton?.contains(target)) annotationMenuOpen = false;
    };
    const keyboard = (event: KeyboardEvent) => {
      if (event.key !== 'Escape') return;
      const hasOpenOverlay = Boolean(annotationPhase || viewportMenuOpen || annotationMenuOpen);
      if (!hasOpenOverlay) return;
      const focusTarget = annotationPhase
        ? annotationCreateButton
        : annotationMenuOpen
          ? annotationHistoryButton
          : viewportMenuButton;
      event.preventDefault();
      event.stopPropagation();
      viewportMenuOpen = false;
      annotationMenuOpen = false;
      if (annotationPhase) cancelAnnotationCreation();
      restoreOverlayTriggerFocus(focusTarget);
    };
    const browserAuthorityChanged = (event: Event) => {
      const detail = (event as CustomEvent<{
        eventType?: string;
        payload?: Record<string, unknown>;
      }>).detail;
      const changedTabId = detail?.payload?.tab_id;
      if (typeof changedTabId === 'string' && changedTabId !== tabId) return;
      void refreshSession();
    };
    const unsubscribeDesktopSnapshot = desktop?.onSnapshot((next) => {
      applyDesktopViewport(next);
    });
    const unsubscribeBrowserEvent = window.magiDesktop?.onBrowserEvent(handleDesktopBrowserEvent);
    window.addEventListener('pointerdown', pointerDown);
    window.addEventListener('keydown', keyboard, true);
    window.addEventListener(BROWSER_AUTHORITY_CHANGED_EVENT, browserAuthorityChanged);
    return () => {
      const release = registeredWebviewRelease;
      registeredWebviewRelease = null;
      ++displaySizeSyncGeneration;
      lastDisplaySizeKey = '';
      if (desktop && release) {
        void desktop.releaseBrowserWebview(release).catch(() => undefined);
      }
      unsubscribeBrowserEvent?.();
      unsubscribeDesktopSnapshot?.();
      window.removeEventListener('pointerdown', pointerDown);
      window.removeEventListener('keydown', keyboard, true);
      clearToolbarTooltip();
      clearNodeInspection();
      stopDesktopAnnotationCapture(annotationCaptureIdentity);
      view?.removeEventListener('did-attach', handleWebviewAttached);
      view?.removeEventListener('dom-ready', handleWebviewReady);
      displaySizeObserver?.disconnect();
      displaySizeObserver = null;
      if (displaySizeAnimationFrame !== null) {
        window.cancelAnimationFrame(displaySizeAnimationFrame);
        displaySizeAnimationFrame = null;
      }
      if (webviewRegistrationTimer !== null) {
        window.clearTimeout(webviewRegistrationTimer);
        webviewRegistrationTimer = null;
      }
      window.removeEventListener(BROWSER_AUTHORITY_CHANGED_EVENT, browserAuthorityChanged);
      if (customViewportTimer !== null) window.clearTimeout(customViewportTimer);
      customViewportTimer = null;
      pendingViewport = null;
    };
  });

</script>

<section
  class="browser-pane"
  aria-label={i18n.t('browser.pane.label')}
>
  <div
    bind:this={browserToolbar}
    class="browser-toolbar"
    style:anchor-name={toolbarAnchorName}
    role="toolbar"
    tabindex="-1"
    onpointerover={updateToolbarTooltip}
    onpointerout={handleToolbarPointerOut}
    onfocusin={updateToolbarTooltip}
    onfocusout={handleToolbarFocusOut}
  >
    {#if desktopRuntime}
      <button type="button" class="icon-button flip" onclick={() => navigate('back')} disabled={!browserReady || busy} data-tooltip={i18n.t('browser.navigation.back')} aria-label={i18n.t('browser.navigation.back')}><Icon name="chevron-right" size={13} /></button>
      <button type="button" class="icon-button" onclick={() => navigate('forward')} disabled={!browserReady || busy} data-tooltip={i18n.t('browser.navigation.forward')} aria-label={i18n.t('browser.navigation.forward')}><Icon name="chevron-right" size={13} /></button>
      {#if browserLoading}
        <button type="button" class="icon-button" onclick={stopNavigation} disabled={!browserReady} data-tooltip={i18n.t('browser.navigation.stop')} aria-label={i18n.t('browser.navigation.stop')}><Icon name="stop" size={13} /></button>
      {:else}
        <button type="button" class="icon-button" onclick={() => navigate('reload')} disabled={!browserReady || busy} data-tooltip={i18n.t('browser.navigation.reload')} aria-label={i18n.t('browser.navigation.reload')}><Icon name="refresh" size={13} /></button>
      {/if}
      <form class="address-form" onsubmit={(event) => { event.preventDefault(); navigate('url'); }}>
        <input
          bind:value={address}
          aria-label={i18n.t('browser.navigation.address')}
          spellcheck="false"
          disabled={!browserReady || busy}
          onfocus={() => { addressEditing = true; }}
          onblur={() => { addressEditing = false; }}
          onkeydown={(event) => { if (event.key !== 'Enter' || event.isComposing) return; event.preventDefault(); navigate('url'); }}
        />
        <button type="submit" class="address-submit" disabled={!browserReady || busy} data-tooltip={i18n.t('browser.navigation.go')} aria-label={i18n.t('browser.navigation.go')}><Icon name="chevron-right" size={12} /></button>
      </form>
      <div class="menu-wrap">
      <button bind:this={viewportMenuButton} type="button" class="icon-button" class:active={localViewportMode === 'fixed'} style:anchor-name={viewportAnchorName} onclick={toggleViewportMenu} disabled={!browserReady || busy} data-tooltip={i18n.t('browser.viewport.control')} aria-label={i18n.t('browser.viewport.control')} aria-expanded={viewportMenuOpen}><Icon name="monitor" size={13} /></button>
      </div>
      <button
        type="button"
        class="icon-button"
        class:active={nodeInspectActive || Boolean(nodeSelection)}
        onclick={toggleNodeInspection}
        disabled={!browserReady || busy || (!nodeInspectActive && !nodeInspectBusy && !activeBrowserIdentity)}
        data-tooltip={i18n.t(nodeInspectActive ? 'browser.action.stopInspectNode' : 'browser.action.inspectNode')}
        aria-label={i18n.t(nodeInspectActive ? 'browser.action.stopInspectNode' : 'browser.action.inspectNode')}
        aria-pressed={nodeInspectActive}
      ><Icon name="code" size={13} /></button>
    {:else}
      <div class="record-address" title={activeTab?.url || i18n.t('browser.status.noTab')}>
        <Icon name="globe" size={13} />
        <span>{activeTab?.title || activeTab?.url || i18n.t('browser.status.noTab')}</span>
      </div>
    {/if}
    <button type="button" class="icon-button toolbar-edge-button" onclick={openCurrentPageExternally} disabled={!externalUrl || busy} data-tooltip={i18n.t('browser.action.openExternal')} aria-label={i18n.t('browser.action.openExternal')}><Icon name="external-link" size={13} /></button>
    {#if desktopRuntime}
      <button type="button" class="icon-button toolbar-edge-button" onclick={captureScreenshotForMessage} disabled={!browserReady || busy} data-tooltip={i18n.t('browser.action.screenshot')} aria-label={i18n.t('browser.action.screenshot')}><Icon name="file-plus" size={13} /></button>
    {/if}
    <div class="menu-wrap">
      {#if desktopRuntime}
        <button bind:this={annotationCreateButton} type="button" class="icon-button toolbar-edge-button" onclick={openDesktopAnnotationCreation} disabled={!browserReady || busy || Boolean(annotationPhase)} data-tooltip={i18n.t('browser.action.annotate')} aria-label={i18n.t('browser.action.annotate')}><Icon name="target" size={13} /></button>
      {/if}
      {#if savedAnnotations.length > 0}
        <button bind:this={annotationHistoryButton} type="button" class="icon-button annotation-history-button toolbar-edge-button" class:active={annotationMenuOpen} style:anchor-name={annotationHistoryAnchorName} onclick={toggleAnnotationMenu} data-tooltip={i18n.t('browser.annotation.history')} aria-label={i18n.t('browser.annotation.history')} aria-expanded={annotationMenuOpen}><Icon name="list" size={13} /><span class="annotation-count">{savedAnnotations.length}</span></button>
      {/if}
    </div>
    {#if desktopRuntime}
      <span class="status-light" class:ready={connectionState === 'ready' && !browserLoading} class:loading={connectionState === 'ready' && browserLoading} class:error={connectionState === 'error'} title={connectionStatusText} role="status"></span>
    {:else}
      <span class="record-status" role="status">{activeTab ? i18n.t('browser.status.recordOnly') : i18n.t('browser.status.noTab')}</span>
    {/if}
  </div>

  {#if tooltipText}
    <div
      bind:this={tooltipElement}
      class="browser-toolbar-tooltip"
      style:position-anchor={tooltipAnchorName}
      popover="manual"
      role="tooltip"
    >{tooltipText}</div>
  {/if}

  {#if activeDownload && (activeDownload.state === 'started' || activeDownload.state === 'progressing' || activeDownload.state === 'completed' || activeDownload.state === 'cancelled' || activeDownload.state === 'interrupted')}
    <div
      class="browser-download-panel"
      style:position-anchor={surfaceAnchorName}
      role="status"
      aria-label={i18n.t('browser.download.status')}
    >
      <span class="browser-download-name" title={activeDownload.suggestedFilename}>{activeDownload.suggestedFilename}</span>
      {#if activeDownload.state === 'started' || activeDownload.state === 'progressing'}
        <span class="browser-download-state">{i18n.t('browser.download.progressing')}</span>
        {#if activeDownload.totalBytes}
          <span class="browser-download-bytes">{formatDownloadBytes(activeDownload.receivedBytes)} / {formatDownloadBytes(activeDownload.totalBytes)}</span>
        {:else}
          <span class="browser-download-bytes">{formatDownloadBytes(activeDownload.receivedBytes)}</span>
        {/if}
        <button type="button" onclick={cancelBrowserDownload} aria-label={i18n.t('browser.download.cancel')}>{i18n.t('browser.download.cancel')}</button>
      {:else}
        <span class="browser-download-state">{i18n.t(`browser.download.${activeDownload.state}`)}</span>
        {#if activeDownload.state === 'interrupted'}
          <span class="browser-download-bytes">{formatDownloadBytes(activeDownload.receivedBytes)}</span>
        {/if}
      {/if}
    </div>
  {/if}

  {#if viewportMenuOpen}
    <div bind:this={viewportMenuElement} class="viewport-popover" style:position-anchor={viewportAnchorName} data-menu="viewport" popover="manual">
      <div class="viewport-menu" role="menu" aria-label={i18n.t('browser.viewport.control')}>
        <button type="button" class:selected={localViewportMode === 'auto'} role="menuitem" onclick={() => { viewportMenuOpen = false; useAutomaticViewport(); }}>
          <Icon name="monitor" size={14} />
          <span>{i18n.t('browser.viewport.auto')}</span>
        </button>
        <div class="menu-divider" aria-hidden="true"></div>
        <div class="viewport-device-modes" role="group" aria-label={i18n.t('browser.viewport.deviceMode')}>
          {#each VIEWPORT_DEVICE_MODES as mode (mode.id)}
            <button
              type="button"
              class:selected={fixedPresetSelected(mode)}
              onclick={() => { viewportMenuOpen = false; useFixedViewport(mode.width, mode.height); }}
              title={`${mode.width} x ${mode.height}`}
            >{i18n.t(`browser.viewport.mode.${mode.id}`)}</button>
          {/each}
        </div>
        <div class="viewport-custom">
          <label>
            <span>{i18n.t('browser.viewport.width')}</span>
            <input
              type="text"
              inputmode="numeric"
              autocomplete="off"
              spellcheck="false"
              aria-label={i18n.t('browser.viewport.width')}
              value={customViewportWidthInput}
              onfocus={() => { customViewportWidthEditing = true; }}
              oninput={(event) => handleCustomViewportInput('width', event)}
              onblur={handleCustomViewportBlur}
            />
          </label>
          <label>
            <span>{i18n.t('browser.viewport.height')}</span>
            <input
              type="text"
              inputmode="numeric"
              autocomplete="off"
              spellcheck="false"
              aria-label={i18n.t('browser.viewport.height')}
              value={customViewportHeightInput}
              onfocus={() => { customViewportHeightEditing = true; }}
              oninput={(event) => handleCustomViewportInput('height', event)}
              onblur={handleCustomViewportBlur}
            />
          </label>
        </div>
      </div>
    </div>
  {/if}

  {#if annotationMenuOpen}
    <div bind:this={annotationMenuElement} class="annotation-history-popover" style:position-anchor={annotationHistoryAnchorName} data-menu="annotations" popover="manual">
      <div class="annotation-menu" role="menu" aria-label={i18n.t('browser.annotation.history')}>
        {#each savedAnnotations as annotation (annotation.annotationId)}
          <button type="button" onclick={() => selectSavedAnnotation(annotation)} title={annotation.comment}><span class="annotation-menu-number">{annotation.sequence}</span><span>{annotation.comment}</span></button>
        {/each}
      </div>
    </div>
  {/if}

  <div
    bind:this={browserSurfaceSlot}
    class="browser-surface-slot"
    style:anchor-name={surfaceAnchorName}
    data-browser-tab-id={tabId}
    aria-label={i18n.t('browser.viewport.label')}
  >
    {#if desktopRuntime}
      <webview
        bind:this={browserWebview}
        class="browser-webview"
        src="about:blank"
        partition={browserPartitionForSession(browserSessionId)}
        allowpopups={false}
        webpreferences="focusOnNavigation=no"
        aria-label={i18n.t('browser.viewport.label')}
      ></webview>
      {#if !browserReady}
        <div class="browser-placeholder browser-placeholder-overlay" class:error={connectionState === 'error'} aria-live="polite">{connectionStatusText}</div>
      {/if}
      {#if actionError && browserReady}
        <div bind:this={actionErrorElement} class="browser-action-error" style:position-anchor={surfaceAnchorName} popover="manual" role="alert">
          <Icon name="alert-circle" size={14} />
          <span>{actionError}</span>
          <button type="button" onclick={() => { actionError = ''; }} aria-label={i18n.t('common.close')}><Icon name="close" size={12} /></button>
        </div>
      {/if}
      {#if pageError && browserReady && !browserLoading}
        <div bind:this={pageErrorElement} class="browser-page-error" style:position-anchor={surfaceAnchorName} popover="manual" role="alert" aria-live="assertive">
          <Icon name="alert-circle" size={28} />
          <strong>{i18n.t('browser.error.pageLoadFailed')}</strong>
          <span>{pageError}</span>
          <button type="button" onclick={() => navigate('reload')} disabled={busy}>
            <Icon name="refresh" size={13} />
            <span>{i18n.t('app.recoveryRetry')}</span>
          </button>
        </div>
      {/if}
      {#if annotationPhase === 'comment' && annotationSelection}
        <div bind:this={annotationEditorElement} class="annotation-editor" style:position-anchor={surfaceAnchorName} popover="manual" role="dialog" aria-label={i18n.t('browser.annotation.title')}>
          <textarea
            bind:this={annotationEditor}
            bind:value={annotationComment}
            placeholder={i18n.t('browser.annotation.placeholder')}
            maxlength="4000"
          ></textarea>
          <div class="annotation-editor-actions">
            <button type="button" onclick={cancelAnnotationCreation}>{i18n.t('common.cancel')}</button>
            <button type="button" class="primary" disabled={!annotationComment.trim()} onclick={submitCreatedAnnotation}>{i18n.t('common.save')}</button>
          </div>
        </div>
      {/if}
    {:else}
      {#if loading}
        <div class="browser-placeholder">{i18n.t('browser.status.loading')}</div>
      {:else if sessionError}
        <div class="browser-placeholder error">{sessionError}</div>
      {:else if activeTab}
        <div class="browser-record" aria-label={i18n.t('browser.status.recordOnly')}>
          <Icon name="globe" size={28} />
          <strong>{activeTab.title || i18n.t('browser.tab.new')}</strong>
          <span class="browser-record-url">{activeTab.url || 'about:blank'}</span>
          <span class="browser-record-description">{i18n.t('browser.status.recordOnly')}</span>
          {#if savedAnnotations.length > 0}
            <div class="browser-record-annotations" aria-label={i18n.t('browser.annotation.history')}>
              {#each savedAnnotations as annotation (annotation.annotationId)}
                <button type="button" class="browser-record-annotation" onclick={() => selectSavedAnnotation(annotation)}>
                  <span class="annotation-menu-number">{annotation.sequence}</span>
                  <span>{annotation.comment}</span>
                </button>
              {/each}
            </div>
          {/if}
          {#if externalUrl}
            <button type="button" class="browser-record-external" onclick={openCurrentPageExternally} disabled={busy}>
              <Icon name="external-link" size={13} />
              <span>{i18n.t('browser.action.openExternal')}</span>
            </button>
          {/if}
        </div>
      {:else}
        <div class="browser-placeholder">{i18n.t('browser.status.noTab')}</div>
      {/if}
    {/if}
  </div>
</section>

<style>
  .browser-pane { position: relative; display: flex; flex: 1 1 auto; flex-direction: column; width: 100%; min-width: 0; min-height: 0; height: 100%; background: var(--background); }
  /* 工具栏固定在右栏内容区内，边框不改变内容槽的有效尺寸。 */
  .browser-toolbar { position: relative; z-index: 10; isolation: isolate; box-sizing: border-box; display: flex; align-items: center; width: 100%; min-width: 0; height: 36px; min-height: 36px; gap: 3px; padding: 4px 6px; border-bottom: 1px solid var(--border); flex-shrink: 0; overflow: visible; }
  .icon-button, .address-submit { position: relative; }
  .icon-button { width: 27px; height: 27px; display: inline-flex; align-items: center; justify-content: center; flex-shrink: 0; padding: 0; border: 0; border-radius: var(--radius-sm); background: transparent; color: var(--foreground-muted); cursor: pointer; }
  .icon-button:hover:not(:disabled) { background: var(--surface-2); color: var(--foreground); }
  .icon-button.active { color: var(--primary); background: var(--surface-2); }
  .icon-button:disabled { opacity: .45; cursor: default; }
  .flip :global(svg) { transform: scaleX(-1); }
  /* 原生 title 和普通伪元素都会落在 <webview> 后面；工具提示通过
     Popover Top Layer 从当前工具栏向下展示，仍由 Renderer DOM 管理。 */
  .browser-toolbar-tooltip { position: fixed; top: calc(anchor(bottom) + 5px); left: anchor(right); z-index: var(--z-tooltip, 1200); max-width: min(240px, calc(100vw - 16px)); padding: 4px 7px; border: 1px solid var(--border); border-radius: var(--radius-sm); background: var(--glass-bg, var(--dropdown-bg)); box-shadow: var(--shadow-sm); color: var(--foreground); font-size: var(--text-xs); font-weight: var(--font-medium, 500); line-height: 1.25; white-space: nowrap; pointer-events: none; transform: translateX(-100%); }
  .address-form { display: flex; flex: 1 1 0; min-width: 0; }
  .address-form input { box-sizing: border-box; width: 100%; min-width: 0; height: 27px; padding: 0 8px; border: 1px solid var(--border); border-right: 0; border-radius: var(--radius-sm) 0 0 var(--radius-sm); background: var(--surface-1); color: var(--foreground); font: inherit; }
  .address-submit { display: grid; place-items: center; width: 27px; height: 27px; flex: 0 0 27px; padding: 0; border: 1px solid var(--border); border-radius: 0 var(--radius-sm) var(--radius-sm) 0; background: var(--surface-1); color: var(--foreground-muted); cursor: pointer; }
  .record-address { display: flex; align-items: center; gap: 7px; flex: 1; min-width: 0; height: 27px; padding: 0 8px; border: 1px solid var(--border); border-radius: var(--radius-sm); background: var(--surface-1); color: var(--foreground-muted); font-size: var(--text-xs); }
  .record-address span { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .menu-wrap { position: relative; display: flex; flex: 0 0 auto; }
  /* 顶部工具菜单是浏览器框架上的 Renderer 浮层，不属于页面内容的排版轨道。
     CSS Anchor Positioning 让它跟随真实工具栏位置，Popover Top Layer 解决
     Electron webview 原生合成层的遮挡，不需要任何浏览器内容坐标映射。 */
  .viewport-popover,
  .annotation-history-popover { position: fixed; top: calc(anchor(bottom) + 4px); left: anchor(right); z-index: 20; box-sizing: border-box; width: min(300px, calc(100vw - 12px)); pointer-events: auto; transform: translateX(-100%); }
  .viewport-menu, .annotation-menu { box-sizing: border-box; width: min(300px, 100%); overflow: hidden; padding: 5px; border: 1px solid var(--border); border-radius: 6px; background: var(--dropdown-bg); box-shadow: var(--shadow-lg); }
  .annotation-history-popover .annotation-menu { width: 100%; max-height: min(420px, calc(100vh - 48px)); overflow-y: auto; scrollbar-width: none; }
  .annotation-history-popover .annotation-menu::-webkit-scrollbar { display: none; }
  .viewport-menu > button, .annotation-menu > button { box-sizing: border-box; display: flex; align-items: center; gap: 7px; width: 100%; min-height: 30px; padding: 0 8px; border: 0; border-radius: 4px; background: transparent; color: var(--foreground); font: inherit; font-size: var(--text-xs); cursor: pointer; text-align: left; }
  .viewport-menu > button:hover, .viewport-menu > button.selected, .annotation-menu > button:hover { background: var(--surface-hover); }
  .viewport-menu > button.selected { color: var(--primary); }
  .menu-divider { height: 1px; margin: 4px 3px; background: var(--border); }
  .viewport-device-modes { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 3px; padding: 3px; }
  .viewport-device-modes button { height: 28px; min-width: 0; border: 1px solid var(--border); border-radius: 4px; background: var(--surface-1); color: var(--foreground-muted); font: inherit; font-size: var(--text-xs); cursor: pointer; }
  .viewport-device-modes button.selected { border-color: var(--primary); color: var(--primary); }
  .viewport-custom { display: grid; grid-template-columns: 1fr 1fr; gap: 5px; padding: 3px; }
  .viewport-custom label { display: grid; gap: 3px; min-width: 0; color: var(--foreground-muted); font-size: 10px; }
  .viewport-custom input { box-sizing: border-box; min-width: 0; width: 100%; height: 27px; padding: 0 5px; border: 1px solid var(--border); border-radius: 4px; background: var(--surface-1); color: var(--foreground); font: inherit; }
  .annotation-history-button { position: relative; }
  .annotation-count { position: absolute; top: 1px; right: 1px; min-width: 12px; height: 12px; padding: 0 2px; border-radius: 6px; background: var(--info); color: white; font-size: 8px; font-weight: 700; line-height: 12px; text-align: center; }
  .annotation-menu-number { display: grid; place-items: center; flex: 0 0 19px; width: 19px; height: 19px; border-radius: 50%; background: var(--info); color: white; font-size: 10px; font-weight: 700; }
  .status-light { width: 7px; height: 7px; flex: 0 0 auto; border-radius: 50%; background: var(--foreground-muted); }
  .status-light.ready { background: var(--success); }
  .status-light.loading { background: var(--warning); }
  .status-light.error { background: var(--error); }
  .record-status { min-width: 0; max-width: 180px; overflow: hidden; color: var(--foreground-muted); font-size: 10px; text-overflow: ellipsis; white-space: nowrap; }
  .browser-download-panel { position: fixed; top: calc(anchor(bottom) - 10px); left: calc(anchor(left) + 10px); z-index: 5; box-sizing: border-box; display: flex; align-items: center; gap: 7px; width: min(420px, calc(anchor-size(width) - 20px)); min-height: 30px; padding: 4px 6px; border: 1px solid var(--border); border-radius: 6px; background: var(--dropdown-bg); box-shadow: var(--shadow-lg); pointer-events: auto; transform: translateY(-100%); }
  .browser-download-name { flex: 1 1 auto; min-width: 0; overflow: hidden; color: var(--foreground); font-size: var(--text-xs); text-overflow: ellipsis; white-space: nowrap; }
  .browser-download-state { flex: 0 0 auto; color: var(--foreground-muted); font-size: 10px; white-space: nowrap; }
  .browser-download-bytes { flex: 0 0 auto; color: var(--foreground-muted); font-size: 10px; font-variant-numeric: tabular-nums; white-space: nowrap; }
  .browser-download-panel button { flex: 0 0 auto; height: 22px; padding: 0 7px; border: 1px solid var(--border); border-radius: 4px; background: var(--surface-1); color: var(--foreground); font: inherit; font-size: 10px; cursor: pointer; }
  .browser-download-panel button:hover { background: var(--surface-hover); }
  .browser-surface-slot { position: relative; z-index: 0; display: flex; flex: 1; min-width: 0; min-height: 0; overflow: hidden; background: var(--surface-1); }
  /* Electron 的 webview 自定义元素依赖自身的 flex 内部布局把 guest
     视口同步到内容槽。覆盖为 block 会让 guest 退回默认 150px 高度，
     从而出现页面被截断、滚动条异常和截图范围错误。 */
  .browser-webview { display: flex; flex: 1 1 auto; width: 100%; height: 100%; min-width: 0; min-height: 0; border: 0; background: #fff; }
  .browser-placeholder-overlay { position: absolute; inset: 0; pointer-events: none; background: var(--surface-1); }
  .browser-action-error { position: fixed; top: calc(anchor(top) + 10px); left: calc(anchor(left) + 10px); z-index: 6; box-sizing: border-box; display: grid; grid-template-columns: auto minmax(0, 1fr) auto; align-items: center; gap: 7px; width: min(420px, calc(anchor-size(width) - 20px)); min-height: 34px; margin: 0; padding: 5px 6px 5px 8px; border: 1px solid color-mix(in srgb, var(--error) 45%, var(--border)); border-radius: 6px; background: var(--dropdown-bg); box-shadow: var(--shadow-lg); color: var(--foreground); pointer-events: auto; }
  .browser-action-error > :global(svg) { color: var(--error); }
  .browser-action-error span { min-width: 0; overflow-wrap: anywhere; font-size: var(--text-xs); line-height: 1.35; }
  .browser-action-error button { display: grid; place-items: center; width: 24px; height: 24px; padding: 0; border: 0; border-radius: 4px; background: transparent; color: var(--foreground-muted); cursor: pointer; }
  .browser-action-error button:hover { background: var(--surface-hover); color: var(--foreground); }
  /* 失败页仍由 Chromium guest 负责导航，但 guest 的 chrome-error 页面在
     Electron webview 中不保证有可见错误文案。使用当前内容槽的 Top Layer
     呈现同一失败事实，避免普通 DOM 被原生 guest 合成层覆盖。 */
  .browser-page-error { position: fixed; top: anchor(top); left: anchor(left); width: anchor-size(width); height: anchor-size(height); box-sizing: border-box; display: grid; place-content: center; justify-items: center; gap: 9px; margin: 0; padding: 24px; border: 0; background: var(--surface-1); color: var(--foreground-muted); text-align: center; }
  .browser-page-error :global(svg) { color: var(--error); }
  .browser-page-error strong { color: var(--foreground); font-size: var(--text-md); font-weight: 600; }
  .browser-page-error span { max-width: min(520px, 100%); overflow-wrap: anywhere; font-family: var(--font-mono); font-size: var(--text-xs); }
  .browser-page-error button { display: inline-flex; align-items: center; gap: 6px; min-height: 30px; margin-top: 4px; padding: 0 10px; border: 1px solid var(--border); border-radius: 5px; background: var(--surface-1); color: var(--foreground); font: inherit; font-size: var(--text-xs); cursor: pointer; }
  .browser-page-error button:hover:not(:disabled) { background: var(--surface-hover); }
  .browser-page-error button:disabled { cursor: default; opacity: .5; }
  .annotation-editor { position: fixed; top: calc(anchor(bottom) - 12px); left: anchor(left); width: min(360px, calc(anchor-size(width) - 24px)); z-index: 4; box-sizing: border-box; padding: 10px; border: 1px solid var(--border); border-radius: 6px; background: var(--dropdown-bg); box-shadow: var(--shadow-lg); transform: translateY(-100%); }
  .annotation-editor textarea { box-sizing: border-box; width: 100%; min-height: 74px; resize: vertical; padding: 7px; border: 1px solid var(--border); border-radius: 4px; background: var(--surface-1); color: var(--foreground); font: inherit; font-size: var(--text-xs); }
  .annotation-editor-actions { display: flex; justify-content: flex-end; gap: 6px; margin-top: 8px; }
  .annotation-editor-actions button { min-width: 58px; height: 28px; border: 1px solid var(--border); border-radius: 4px; background: var(--surface-1); color: var(--foreground); cursor: pointer; }
  .annotation-editor-actions button.primary { border-color: var(--primary); background: var(--primary); color: var(--primary-foreground); }
  .annotation-editor-actions button:disabled { opacity: .5; cursor: default; }
  .browser-placeholder { display: grid; flex: 1; place-items: center; padding: 20px; color: var(--foreground-muted); font-size: var(--text-sm); text-align: center; }
  .browser-placeholder.error { color: var(--error); }
  .browser-record { display: flex; flex: 1; flex-direction: column; align-items: center; gap: 9px; min-width: 0; padding: 44px 24px; color: var(--foreground-muted); text-align: center; }
  .browser-record strong { max-width: 100%; color: var(--foreground); font-size: var(--text-md); font-weight: 600; overflow-wrap: anywhere; }
  .browser-record-url { max-width: 100%; color: var(--foreground-muted); font-family: var(--font-mono); font-size: 11px; overflow-wrap: anywhere; }
  .browser-record-description { max-width: 420px; font-size: var(--text-sm); line-height: 1.5; }
  .browser-record-annotations { display: grid; gap: 5px; width: min(420px, 100%); margin-top: 8px; }
  .browser-record-annotation { display: flex; align-items: center; gap: 8px; min-width: 0; padding: 7px 9px; border: 1px solid var(--border); border-radius: 5px; background: var(--surface-1); color: var(--foreground); font: inherit; font-size: var(--text-xs); text-align: left; cursor: pointer; }
  .browser-record-annotation:hover { background: var(--surface-hover); }
  .browser-record-annotation > span:last-child { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .browser-record-external { display: inline-flex; align-items: center; gap: 6px; min-height: 30px; margin-top: 8px; padding: 0 10px; border: 1px solid var(--border); border-radius: 5px; background: var(--surface-1); color: var(--foreground); font: inherit; font-size: var(--text-xs); cursor: pointer; }
  .browser-record-external:hover:not(:disabled) { background: var(--surface-hover); }
  .browser-record-external:disabled { cursor: default; opacity: .45; }
</style>
