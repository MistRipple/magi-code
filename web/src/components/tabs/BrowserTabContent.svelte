<script lang="ts">
  import { onMount, tick } from 'svelte';
  import Icon from '../Icon.svelte';
  import { i18n } from '../../stores/i18n.svelte';
  import { normalizeExternalWebUrl, openExternalWebUrl } from '../../lib/external-link';
  import {
    browserAnnotationArtifactUrl,
    browserChannelUrl,
    browserScreenshotUrl,
    createBrowserElementAnnotation,
    createBrowserRegionAnnotation,
    getBrowserSession,
    navigateBrowserTab,
    readBrowserClipboardText,
    setBrowserTabViewport,
    writeBrowserClipboardText,
    type BrowserSessionSnapshot,
    type BrowserTabSnapshot,
    type BrowserDeviceType,
    type BrowserNormalizedRect,
  } from '../../web/agent-api';

  interface Props {
    browserSessionId: string;
    tabId: string;
    onTitleChange?: (label: string) => void;
  }

  const VIEWPORT_DEVICE_MODES = [
    { id: 'wide', width: 1280, height: 800, deviceType: 'desktop' },
    { id: 'narrow', width: 390, height: 844, deviceType: 'mobile' },
  ] as const;
  const CUSTOM_VIEWPORT_DEBOUNCE_MILLIS = 180;

  const viewportControllerId = `viewport-${crypto.randomUUID()}`;

  let { browserSessionId, tabId, onTitleChange }: Props = $props();
  let snapshot = $state<BrowserSessionSnapshot | null>(null);
  let address = $state('');
  let lastObservedUrl = '';
  let loading = $state(true);
  let error = $state('');
  let sessionError = $state('');
  let busy = $state(false);
  let marking = $state(false);
  let channelConnected = $state(false);
  let channelDisconnected = $state(false);
  let socket: WebSocket | null = null;
  let channelTabId = '';
  let channelGeneration = 0;
  let reconnectAttempt = 0;
  let reconnectTimer: number | null = null;
  let refreshGeneration = 0;
  let refreshInFlightKey = '';
  let canvas: HTMLCanvasElement | undefined;
  let viewportElement: HTMLDivElement | undefined;
  interface FrameMetadata {
    frameSequence: number;
    navigationRevision: number;
    width: number;
    height: number;
  }
  let pendingFrameMetadata: FrameMetadata | null = null;
  interface PendingFrame {
    bytes: ArrayBuffer;
    metadata: FrameMetadata;
  }
  let queuedFrame: PendingFrame | null = null;
  let frameDecoderActive = false;
  let frameDecoderGeneration = 0;
  let renderedFrameMetadata = $state<FrameMetadata | null>(null);
  let viewportSize = $state({ width: 0, height: 0 });
  let viewportResizeTimer: number | null = null;
  let viewportSyncInFlight = false;
  let pendingViewportSize = { width: 0, height: 0 };
  let pendingViewportClaim = false;
  let lastRequestedViewportSize = { width: 0, height: 0 };
  let lastReadyViewportTabId = '';
  let viewportMenuOpen = $state(false);
  let viewportMenuElement = $state<HTMLDivElement | undefined>();
  let annotationMenuOpen = $state(false);
  let annotationMenuElement = $state<HTMLDivElement | undefined>();
  let annotationPreview = $state<{
    annotationId: string;
    sequence: number;
    comment: string;
    url: string;
  } | null>(null);
  let annotationPreviewFailed = $state(false);
  let customViewportWidth = $state(390);
  let customViewportHeight = $state(844);
  let customViewportDeviceType = $state<BrowserDeviceType>('mobile');
  let customViewportResizeTimer: number | null = null;
  let customViewportSyncInFlight = false;
  let pendingCustomViewport: {
    width: number;
    height: number;
    deviceType: BrowserDeviceType;
  } | null = null;
  const suppressedKeyUps = new Set<string>();
  let annotationDrag = $state<{
    pointerId: number;
    start: { x: number; y: number };
    current: { x: number; y: number };
  } | null>(null);
  type PendingAnnotation =
    | {
        kind: 'element';
        navigationRevision: number;
        x: number;
        y: number;
        rect: BrowserNormalizedRect;
      }
    | {
        kind: 'region';
        navigationRevision: number;
        rect: BrowserNormalizedRect;
      };
  let pendingAnnotation = $state<PendingAnnotation | null>(null);
  let pendingAnnotationComment = $state('');
  let annotationInput = $state<HTMLTextAreaElement | undefined>();

  const activeTab = $derived.by<BrowserTabSnapshot | null>(() => {
    return snapshot?.tabs.find((tab) => tab.tabId === tabId && tab.lifecycle !== 'closed') ?? null;
  });
  const savedAnnotations = $derived.by(() => (
    (activeTab?.annotations ?? []).filter((annotation) => annotation.status !== 'deleted')
  ));
  const nextAnnotationSequence = $derived(
    Math.max(0, ...(activeTab?.annotations ?? []).map((annotation) => annotation.sequence)) + 1,
  );
  $effect(() => {
    if (
      annotationPreview
      && !savedAnnotations.some((annotation) => annotation.annotationId === annotationPreview?.annotationId)
    ) {
      closeAnnotationPreview();
    }
  });
  const runtimeReady = $derived(snapshot?.lifecycle === 'ready');
  const externalUrl = $derived(normalizeExternalWebUrl(activeTab?.url ?? ''));
  const connectionState = $derived.by<'ready' | 'connecting' | 'recovering' | 'error'>(() => {
    if (
      error
      || sessionError
      || channelDisconnected
      || activeTab?.lifecycle === 'crashed'
      || snapshot?.lifecycle === 'failed'
    ) {
      return 'error';
    }
    if (snapshot?.lifecycle === 'recovering') return 'recovering';
    if (
      loading
      || !runtimeReady
      || activeTab?.lifecycle !== 'ready'
      || !channelConnected
    ) {
      return 'connecting';
    }
    return 'ready';
  });
  const connectionStatusText = $derived.by(() => {
    if (error) return error;
    if (sessionError) return sessionError;
    if (activeTab?.lifecycle === 'crashed' || snapshot?.lifecycle === 'failed') {
      return i18n.t('browser.status.unavailable');
    }
    if (snapshot?.lifecycle === 'recovering') return i18n.t('browser.status.recovering');
    if (channelDisconnected) return i18n.t('browser.error.channelDisconnected');
    if (connectionState === 'ready') return i18n.t('browser.status.connected');
    return i18n.t('browser.status.connecting');
  });
  const connectionNotice = $derived(
    connectionState === 'error' || connectionState === 'recovering'
      ? connectionStatusText
      : '',
  );

  $effect(() => {
    const tab = activeTab;
    if (!tab) return;
    const label = tab.title.trim() || (tab.url === 'about:blank' ? i18n.t('browser.tab.new') : tab.url);
    onTitleChange?.(label);
  });

  function errorMessage(value: unknown): string {
    return value instanceof Error ? value.message : String(value);
  }

  async function refreshSession(initialLoad = false): Promise<void> {
    const requestedBrowserSessionId = browserSessionId;
    const requestedTabId = tabId;
    const requestKey = `${requestedBrowserSessionId}\u0000${requestedTabId}`;
    if (!initialLoad && refreshInFlightKey === requestKey) return;
    const generation = ++refreshGeneration;
    refreshInFlightKey = requestKey;
    if (initialLoad) loading = true;
    try {
      const nextSnapshot = await getBrowserSession(requestedBrowserSessionId);
      if (
        generation !== refreshGeneration
        || requestedBrowserSessionId !== browserSessionId
        || requestedTabId !== tabId
      ) {
        return;
      }
      snapshot = nextSnapshot;
      const nextUrl = nextSnapshot.tabs.find((tab) => (
        tab.tabId === requestedTabId && tab.lifecycle !== 'closed'
      ))?.url ?? '';
      if (initialLoad || address === lastObservedUrl) address = nextUrl;
      lastObservedUrl = nextUrl;
      sessionError = '';
      syncChannel(nextSnapshot, requestedTabId);
    } catch (cause) {
      if (
        generation !== refreshGeneration
        || requestedBrowserSessionId !== browserSessionId
        || requestedTabId !== tabId
      ) {
        return;
      }
      sessionError = errorMessage(cause);
    } finally {
      if (
        generation === refreshGeneration
        && requestedBrowserSessionId === browserSessionId
        && requestedTabId === tabId
      ) {
        refreshInFlightKey = '';
        loading = false;
      }
    }
  }

  function clearReconnectTimer(): void {
    if (reconnectTimer === null) return;
    window.clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }

  function disconnectChannel(): void {
    channelGeneration += 1;
    channelTabId = '';
    reconnectAttempt = 0;
    clearReconnectTimer();
    const current = socket;
    socket = null;
    channelConnected = false;
    channelDisconnected = false;
    current?.close();
    pendingFrameMetadata = null;
    queuedFrame = null;
    frameDecoderGeneration += 1;
    renderedFrameMetadata = null;
    annotationDrag = null;
    pendingAnnotation = null;
    pendingAnnotationComment = '';
  }

  function clearViewportResizeTimer(): void {
    if (viewportResizeTimer === null) return;
    window.clearTimeout(viewportResizeTimer);
    viewportResizeTimer = null;
  }

  function normalizedViewportSize(width: number, height: number): { width: number; height: number } {
    return {
      width: Math.min(7_680, Math.max(320, Math.round(width))),
      height: Math.min(4_320, Math.max(240, Math.round(height))),
    };
  }

  function scheduleViewportSync(): void {
    if (viewportResizeTimer !== null) return;
    viewportResizeTimer = window.setTimeout(() => {
      viewportResizeTimer = null;
      void syncViewportToPanel();
    }, 150);
  }

  function sameViewportSize(
    left: { width: number; height: number },
    right: { width: number; height: number },
  ): boolean {
    return left.width === right.width && left.height === right.height;
  }

  function deviceTypeForWidth(width: number): BrowserDeviceType {
    return width <= 600 ? 'mobile' : 'desktop';
  }

  function viewportControllerActive(): boolean {
    // 右侧区域仅挂载当前打开的面板；不能再以 window focus 作为首次布局同步的
    // 前置条件。应用刚显示、从后台恢复或嵌入容器交接焦点时，hasFocus() 可能仍为
    // false，导致新浏览器永远保留 1280x800 初始视口而在宽面板中出现内容截断。
    return document.visibilityState === 'visible';
  }

  function scheduleCurrentPanelViewport(force = false, claim = false): void {
    if (!viewportControllerActive()) return;
    const tab = activeTab;
    if (
      !tab
      || tab.viewportMode !== 'auto'
      || snapshot?.lifecycle !== 'ready'
      || tab.lifecycle !== 'ready'
    ) return;
    if (!viewportSize.width || !viewportSize.height) return;
    const nextViewportSize = normalizedViewportSize(viewportSize.width, viewportSize.height);
    if (!force && !claim && sameViewportSize(lastRequestedViewportSize, nextViewportSize)) return;
    lastRequestedViewportSize = nextViewportSize;
    pendingViewportSize = nextViewportSize;
    pendingViewportClaim ||= claim;
    scheduleViewportSync();
  }

  function reclaimViewportControl(): void {
    scheduleCurrentPanelViewport(true, true);
  }

  async function syncViewportToPanel(): Promise<void> {
    if (viewportSyncInFlight) return;
    const tab = activeTab;
    const requested = pendingViewportSize;
    if (
      !tab
      || tab.lifecycle !== 'ready'
      || tab.viewportMode !== 'auto'
      || snapshot?.lifecycle !== 'ready'
      || requested.width <= 0
      || requested.height <= 0
    ) {
      return;
    }
    const claim = pendingViewportClaim;
    pendingViewportClaim = false;
    if (
      !claim
      && tab.viewport.width === requested.width
      && tab.viewport.height === requested.height
    ) return;
    viewportSyncInFlight = true;
    let applied = false;
    try {
      const updated = await setBrowserTabViewport(tab.tabId, {
        action: 'sync',
        ...requested,
        controllerId: viewportControllerId,
        claim,
      });
      applyViewportUpdate(updated);
      annotationDrag = null;
      pendingAnnotation = null;
      pendingAnnotationComment = '';
      error = '';
      applied = true;
    } catch (cause) {
      error = errorMessage(cause);
    } finally {
      viewportSyncInFlight = false;
      if (
        applied
        && !sameViewportSize(requested, pendingViewportSize)
      ) {
        scheduleViewportSync();
      }
    }
  }

  function applyViewportUpdate(updated: BrowserTabSnapshot): void {
    if (!snapshot || tabId !== updated.tabId) return;
    snapshot = {
      ...snapshot,
      tabs: snapshot.tabs.map((candidate) => (
        candidate.tabId === updated.tabId
          ? {
              ...candidate,
              viewport: updated.viewport,
              viewportMode: updated.viewportMode,
              snapshotRevision: updated.snapshotRevision,
              updatedAt: updated.updatedAt,
            }
          : candidate
      )),
    };
  }

  function clearCustomViewportResizeTimer(): void {
    if (customViewportResizeTimer === null) return;
    window.clearTimeout(customViewportResizeTimer);
    customViewportResizeTimer = null;
  }

  function cancelPendingCustomViewport(): void {
    clearCustomViewportResizeTimer();
    pendingCustomViewport = null;
  }

  function queueCustomViewport(
    width: number,
    height: number,
    immediate = false,
  ): void {
    if (!Number.isFinite(width) || !Number.isFinite(height)) return;
    if (width < 320 || width > 7_680 || height < 240 || height > 4_320) return;
    const requested = normalizedViewportSize(width, height);
    const deviceType = deviceTypeForWidth(requested.width);
    customViewportDeviceType = deviceType;
    pendingCustomViewport = { ...requested, deviceType };
    clearCustomViewportResizeTimer();
    if (immediate) {
      void syncCustomViewport();
      return;
    }
    customViewportResizeTimer = window.setTimeout(() => {
      customViewportResizeTimer = null;
      void syncCustomViewport();
    }, CUSTOM_VIEWPORT_DEBOUNCE_MILLIS);
  }

  async function syncCustomViewport(): Promise<void> {
    if (customViewportSyncInFlight) return;
    const requested = pendingCustomViewport;
    const tab = activeTab;
    if (!requested || !tab || tab.lifecycle !== 'ready' || snapshot?.lifecycle !== 'ready') return;
    pendingCustomViewport = null;
    if (
      tab.viewportMode === 'fixed'
      && tab.viewport.width === requested.width
      && tab.viewport.height === requested.height
      && tab.viewport.deviceType === requested.deviceType
    ) return;
    customViewportSyncInFlight = true;
    try {
      const updated = await setBrowserTabViewport(tab.tabId, {
        action: 'set',
        mode: 'fixed',
        width: requested.width,
        height: requested.height,
        deviceType: requested.deviceType,
      });
      applyViewportUpdate(updated);
      lastRequestedViewportSize = { width: requested.width, height: requested.height };
      pendingViewportSize = { width: requested.width, height: requested.height };
      annotationDrag = null;
      pendingAnnotation = null;
      pendingAnnotationComment = '';
      error = '';
    } catch (cause) {
      error = errorMessage(cause);
    } finally {
      customViewportSyncInFlight = false;
      if (pendingCustomViewport) {
        clearCustomViewportResizeTimer();
        customViewportResizeTimer = window.setTimeout(() => {
          customViewportResizeTimer = null;
          void syncCustomViewport();
        }, 0);
      }
    }
  }

  function useAutomaticViewport(): void {
    if (!viewportSize.width || !viewportSize.height) return;
    const requested = normalizedViewportSize(viewportSize.width, viewportSize.height);
    const tab = activeTab;
    if (!tab || busy) return;
    cancelPendingCustomViewport();
    viewportMenuOpen = false;
    void run(async () => {
      const updated = await setBrowserTabViewport(tab.tabId, {
        action: 'set',
        mode: 'auto',
        ...requested,
        deviceType: deviceTypeForWidth(requested.width),
        controllerId: viewportControllerId,
      });
      applyViewportUpdate(updated);
      lastRequestedViewportSize = requested;
      pendingViewportSize = requested;
      annotationDrag = null;
      pendingAnnotation = null;
      pendingAnnotationComment = '';
    });
  }

  function useFixedViewport(
    width: number,
    height: number,
  ): void {
    customViewportWidth = width;
    customViewportHeight = height;
    queueCustomViewport(width, height, true);
  }

  function scheduleCustomViewportUpdate(): void {
    queueCustomViewport(
      customViewportWidth,
      customViewportHeight,
    );
  }

  function toggleViewportMenu(): void {
    if (viewportMenuOpen) {
      viewportMenuOpen = false;
      return;
    }
    const tab = activeTab;
    if (tab) {
      customViewportWidth = tab.viewport.width;
      customViewportHeight = tab.viewport.height;
      customViewportDeviceType = tab.viewport.deviceType;
    }
    viewportMenuOpen = true;
  }

  $effect(() => {
    const tab = activeTab;
    if (!tab || snapshot?.lifecycle !== 'ready' || tab.lifecycle !== 'ready') return;
    if (lastReadyViewportTabId === tab.tabId) return;
    lastReadyViewportTabId = tab.tabId;
    scheduleCurrentPanelViewport();
  });

  $effect(() => {
    const tab = activeTab;
    if (!tab || viewportMenuOpen) return;
    customViewportWidth = tab.viewport.width;
    customViewportHeight = tab.viewport.height;
    customViewportDeviceType = tab.viewport.deviceType;
  });

  function channelEligible(targetTabId: string): boolean {
    const tab = snapshot?.tabs.find((candidate) => (
      candidate.tabId === targetTabId && candidate.lifecycle !== 'closed'
    ));
    return snapshot?.lifecycle === 'ready' && tab?.lifecycle === 'ready';
  }

  function syncChannel(nextSnapshot: BrowserSessionSnapshot, targetTabId: string): void {
    const nextTab = nextSnapshot.tabs.find((tab) => (
      tab.tabId === targetTabId && tab.lifecycle !== 'closed'
    ));
    if (nextSnapshot.lifecycle !== 'ready' || nextTab?.lifecycle !== 'ready') {
      if (channelTabId) disconnectChannel();
      return;
    }
    if (channelTabId === targetTabId && (socket !== null || reconnectTimer !== null)) return;
    disconnectChannel();
    channelTabId = targetTabId;
    openChannel(targetTabId, channelGeneration);
  }

  function scheduleReconnect(tabId: string, generation: number): void {
    if (
      channelTabId !== tabId
      || channelGeneration !== generation
      || reconnectTimer !== null
      || !channelEligible(tabId)
    ) {
      return;
    }
    const delay = Math.min(10_000, 500 * 2 ** Math.min(reconnectAttempt, 5));
    reconnectAttempt += 1;
    reconnectTimer = window.setTimeout(() => {
      reconnectTimer = null;
      openChannel(tabId, generation);
    }, delay);
  }

  function openChannel(tabId: string, generation: number): void {
    if (
      channelTabId !== tabId
      || channelGeneration !== generation
      || !channelEligible(tabId)
    ) return;
    const next = new WebSocket(browserChannelUrl(tabId));
    next.binaryType = 'arraybuffer';
    socket = next;
    next.onopen = () => {
      if (socket !== next || channelGeneration !== generation) return;
      reconnectAttempt = 0;
      channelConnected = true;
      channelDisconnected = false;
    };
    next.onmessage = (event) => {
      if (socket !== next || channelGeneration !== generation) return;
      if (typeof event.data === 'string') {
        try {
          const message = JSON.parse(event.data) as {
            type?: string;
            message?: string;
            frameSequence?: number;
            navigationRevision?: number;
            width?: number;
            height?: number;
            operation?: 'copy' | 'cut';
            text?: string;
          };
          if (message.type === 'frame') {
            pendingFrameMetadata = {
              frameSequence: Number(message.frameSequence) || 0,
              navigationRevision: Number(message.navigationRevision) || 0,
              width: Math.max(1, Number(message.width) || 1),
              height: Math.max(1, Number(message.height) || 1),
            };
          } else if (message.type === 'error') {
            error = message.message?.trim() || i18n.t('browser.error.channelDisconnected');
          } else if (message.type === 'clipboard_text' && typeof message.text === 'string') {
            void writeBrowserClipboardText(message.text).catch(() => {
              error = i18n.t('browser.error.clipboardWrite');
            });
          }
        } catch {
          pendingFrameMetadata = null;
        }
        return;
      }
      const metadata = pendingFrameMetadata;
      pendingFrameMetadata = null;
      if (!metadata || !(event.data instanceof ArrayBuffer)) return;
      queueFrame(event.data, metadata);
    };
    next.onerror = () => {
      if (socket !== next || channelGeneration !== generation) return;
      channelConnected = false;
      channelDisconnected = true;
    };
    next.onclose = () => {
      if (socket === next) socket = null;
      if (channelTabId !== tabId || channelGeneration !== generation) return;
      channelConnected = false;
      channelDisconnected = true;
      scheduleReconnect(tabId, generation);
    };
  }

  function queueFrame(bytes: ArrayBuffer, metadata: FrameMetadata): void {
    if (
      !queuedFrame
      || metadata.frameSequence >= queuedFrame.metadata.frameSequence
    ) {
      queuedFrame = { bytes, metadata };
    }
    if (!frameDecoderActive) void drainFrameQueue(frameDecoderGeneration);
  }

  async function drainFrameQueue(generation: number): Promise<void> {
    if (frameDecoderActive) return;
    frameDecoderActive = true;
    try {
      while (generation === frameDecoderGeneration) {
        const frame = queuedFrame;
        queuedFrame = null;
        if (!frame) break;
        await drawFrame(frame.bytes, frame.metadata, generation);
      }
    } finally {
      frameDecoderActive = false;
      if (queuedFrame) void drainFrameQueue(frameDecoderGeneration);
    }
  }

  async function drawFrame(
    bytes: ArrayBuffer,
    metadata: FrameMetadata,
    generation: number,
  ): Promise<void> {
    let image: ImageBitmap;
    try {
      image = await createImageBitmap(new Blob([bytes], { type: 'image/jpeg' }));
    } catch {
      return;
    }
    if (
      generation !== frameDecoderGeneration
      || !canvas
      || (renderedFrameMetadata?.frameSequence ?? -1) >= metadata.frameSequence
    ) {
      image.close();
      return;
    }
    canvas.width = image.width;
    canvas.height = image.height;
    canvas.getContext('2d')?.drawImage(image, 0, 0);
    image.close();
    renderedFrameMetadata = metadata;
    if (pendingAnnotation && pendingAnnotation.navigationRevision !== metadata.navigationRevision) {
      pendingAnnotation = null;
      pendingAnnotationComment = '';
      error = i18n.t('browser.annotation.pageChanged');
    }
  }

  $effect(() => {
    const targetBrowserSessionId = browserSessionId.trim();
    const targetTabId = tabId.trim();
    disconnectChannel();
    snapshot = null;
    address = '';
    lastObservedUrl = '';
    lastReadyViewportTabId = '';
    pendingViewportSize = { width: 0, height: 0 };
    pendingViewportClaim = false;
    lastRequestedViewportSize = { width: 0, height: 0 };
    clearViewportResizeTimer();
    error = '';
    sessionError = '';
    if (!targetBrowserSessionId || !targetTabId) {
      loading = false;
      return;
    }
    loading = true;
    void refreshSession(true);
    return disconnectChannel;
  });

  onMount(() => {
    const resizeObserver = new ResizeObserver(([entry]) => {
      const box = entry?.contentRect;
      viewportSize = {
        width: Math.max(0, box?.width ?? 0),
        height: Math.max(0, box?.height ?? 0),
      };
      scheduleCurrentPanelViewport();
    });
    if (viewportElement) resizeObserver.observe(viewportElement);
    const refreshTimer = window.setInterval(() => void refreshSession(), 2_000);
    window.addEventListener('focus', reclaimViewportControl);
    window.addEventListener('pageshow', reclaimViewportControl);
    window.addEventListener('magi:browserViewportControllerClaim', reclaimViewportControl);
    document.addEventListener('visibilitychange', reclaimViewportControl);
    const handleToolbarMenuPointer = (event: PointerEvent) => {
      const target = event.target;
      if (
        viewportMenuOpen
        && viewportMenuElement
        && target instanceof Node
        && !viewportMenuElement.contains(target)
      ) {
        viewportMenuOpen = false;
      }
      if (
        annotationMenuOpen
        && annotationMenuElement
        && target instanceof Node
        && !annotationMenuElement.contains(target)
      ) {
        annotationMenuOpen = false;
      }
    };
    const handleToolbarMenuKey = (event: KeyboardEvent) => {
      if (event.key !== 'Escape') return;
      viewportMenuOpen = false;
      annotationMenuOpen = false;
      closeAnnotationPreview();
    };
    window.addEventListener('pointerdown', handleToolbarMenuPointer);
    window.addEventListener('keydown', handleToolbarMenuKey);
    return () => {
      window.clearInterval(refreshTimer);
      window.removeEventListener('focus', reclaimViewportControl);
      window.removeEventListener('pageshow', reclaimViewportControl);
      window.removeEventListener('magi:browserViewportControllerClaim', reclaimViewportControl);
      document.removeEventListener('visibilitychange', reclaimViewportControl);
      window.removeEventListener('pointerdown', handleToolbarMenuPointer);
      window.removeEventListener('keydown', handleToolbarMenuKey);
      resizeObserver.disconnect();
      clearViewportResizeTimer();
      clearCustomViewportResizeTimer();
      disconnectChannel();
    };
  });

  async function run(action: () => Promise<void>): Promise<void> {
    if (busy) return;
    busy = true;
    try {
      await action();
      error = '';
    } catch (cause) {
      error = errorMessage(cause);
    } finally {
      busy = false;
    }
  }

  function navigate(action: 'url' | 'back' | 'forward' | 'reload'): void {
    const tab = activeTab;
    if (!tab) return;
    void run(async () => {
      const updated = await navigateBrowserTab(tab.tabId, action, action === 'url' ? address : undefined);
      address = updated.url;
      lastObservedUrl = updated.url;
      await refreshSession();
    });
  }

  function toggleMarking(): void {
    const tab = activeTab;
    if (!tab) return;
    marking = !marking;
    annotationDrag = null;
    pendingAnnotation = null;
    pendingAnnotationComment = '';
    error = '';
  }

  function displayedFrameStyle(): string {
    const frame = renderedFrameMetadata;
    if (!frame || !viewportSize.width || !viewportSize.height) {
      return 'width: 0; height: 0;';
    }
    const scale = Math.min(
      1,
      viewportSize.width / frame.width,
      viewportSize.height / frame.height,
    );
    return `width: ${Math.max(1, Math.floor(frame.width * scale))}px; height: ${Math.max(1, Math.floor(frame.height * scale))}px;`;
  }

  async function focusAnnotationInput(): Promise<void> {
    await tick();
    annotationInput?.focus();
  }

  function selectElementAnnotation(point: { x: number; y: number }): void {
    const frame = renderedFrameMetadata;
    if (!frame) return;
    const markerWidth = Math.min(1, 12 / frame.width);
    const markerHeight = Math.min(1, 12 / frame.height);
    const x = Math.max(0, Math.min(1, point.x / frame.width));
    const y = Math.max(0, Math.min(1, point.y / frame.height));
    pendingAnnotation = {
      kind: 'element',
      navigationRevision: frame.navigationRevision,
      x,
      y,
      rect: {
        x: Math.max(0, Math.min(1 - markerWidth, x - markerWidth / 2)),
        y: Math.max(0, Math.min(1 - markerHeight, y - markerHeight / 2)),
        width: markerWidth,
        height: markerHeight,
      },
    };
    pendingAnnotationComment = '';
    void focusAnnotationInput();
  }

  function selectRegionAnnotation(
    start: { x: number; y: number },
    end: { x: number; y: number },
  ): void {
    const frame = renderedFrameMetadata;
    if (!frame) return;
    const left = Math.min(start.x, end.x);
    const top = Math.min(start.y, end.y);
    const right = Math.max(start.x, end.x);
    const bottom = Math.max(start.y, end.y);
    pendingAnnotation = {
      kind: 'region',
      navigationRevision: frame.navigationRevision,
      rect: {
        x: left / frame.width,
        y: top / frame.height,
        width: (right - left) / frame.width,
        height: (bottom - top) / frame.height,
      },
    };
    pendingAnnotationComment = '';
    void focusAnnotationInput();
  }

  function cancelPendingAnnotation(): void {
    pendingAnnotation = null;
    pendingAnnotationComment = '';
    annotationDrag = null;
  }

  function selectSavedAnnotation(annotation: BrowserTabSnapshot['annotations'][number]): void {
    window.dispatchEvent(new CustomEvent('magi:browserAnnotationCreated', {
      detail: annotation,
    }));
  }

  function toggleAnnotationMenu(): void {
    annotationMenuOpen = !annotationMenuOpen;
    if (annotationMenuOpen) viewportMenuOpen = false;
  }

  function openAnnotationPreview(
    annotation: BrowserTabSnapshot['annotations'][number],
    sequence: number,
  ): void {
    if (!annotation.screenshotArtifactId) return;
    annotationPreviewFailed = false;
    annotationPreview = {
      annotationId: annotation.annotationId,
      sequence,
      comment: annotation.comment,
      url: browserAnnotationArtifactUrl(annotation.annotationId),
    };
    annotationMenuOpen = false;
  }

  function closeAnnotationPreview(): void {
    annotationPreview = null;
    annotationPreviewFailed = false;
  }

  function savePendingAnnotation(): void {
    const tab = activeTab;
    const pending = pendingAnnotation;
    const comment = pendingAnnotationComment.trim();
    if (!tab || !pending || !comment) return;
    void run(async () => {
      if (renderedFrameMetadata?.navigationRevision !== pending.navigationRevision) {
        throw new Error(i18n.t('browser.annotation.pageChanged'));
      }
      const annotation = pending.kind === 'element'
        ? await createBrowserElementAnnotation(tab.tabId, {
            navigationRevision: pending.navigationRevision,
            x: pending.x,
            y: pending.y,
          }, comment)
        : await createBrowserRegionAnnotation(tab.tabId, {
            navigationRevision: pending.navigationRevision,
            rect: pending.rect,
          }, comment);
      window.dispatchEvent(new CustomEvent('magi:browserAnnotationCreated', {
        detail: annotation,
      }));
      pendingAnnotation = null;
      pendingAnnotationComment = '';
      await refreshSession();
    });
  }

  function handleAnnotationEditorKey(event: KeyboardEvent): void {
    if (event.key === 'Escape') {
      event.preventDefault();
      cancelPendingAnnotation();
      return;
    }
    if (event.key === 'Enter' && (event.metaKey || event.ctrlKey)) {
      event.preventDefault();
      savePendingAnnotation();
    }
  }

  function blobDataUrl(blob: Blob): Promise<string> {
    return new Promise((resolve, reject) => {
      const reader = new FileReader();
      reader.onload = () => {
        if (typeof reader.result === 'string') resolve(reader.result);
        else reject(new Error(i18n.t('browser.error.screenshot')));
      };
      reader.onerror = () => reject(reader.error ?? new Error(i18n.t('browser.error.screenshot')));
      reader.readAsDataURL(blob);
    });
  }

  function captureScreenshotForMessage(): void {
    const tab = activeTab;
    if (!tab) return;
    void run(async () => {
      const response = await fetch(browserScreenshotUrl(tab.tabId), {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ fullPage: false }),
      });
      if (!response.ok) {
        throw new Error(i18n.t('browser.error.screenshot').replace('{status}', String(response.status)));
      }
      const blob = await response.blob();
      const name = `magi-browser-${Date.now()}.png`;
      const dataUrl = await blobDataUrl(blob);
      window.dispatchEvent(new CustomEvent('magi:browserScreenshotCaptured', {
        detail: { name, dataUrl, size: blob.size, type: blob.type || 'image/png' },
      }));
    });
  }

  function openCurrentPageExternally(): void {
    const url = externalUrl;
    if (!url) return;
    void run(async () => {
      await openExternalWebUrl(url);
    });
  }

  function framePoint(event: PointerEvent | WheelEvent): { x: number; y: number } | null {
    const frame = renderedFrameMetadata;
    if (!canvas || !frame) return null;
    const rect = canvas.getBoundingClientRect();
    if (!rect.width || !rect.height) return null;
    return {
      x: Math.max(0, Math.min(frame.width, (event.clientX - rect.left) * frame.width / rect.width)),
      y: Math.max(0, Math.min(frame.height, (event.clientY - rect.top) * frame.height / rect.height)),
    };
  }

  function sendInput(event: Record<string, unknown>): void {
    if (socket?.readyState !== WebSocket.OPEN) return;
    socket.send(JSON.stringify({ type: 'user_input', event }));
  }

  function handlePointer(event: PointerEvent, type: 'mouse_move' | 'mouse_down' | 'mouse_up'): void {
    if (type === 'mouse_down') reclaimViewportControl();
    const point = framePoint(event);
    if (marking) {
      event.preventDefault();
      if (!point) return;
      if (type === 'mouse_down') {
        canvas?.setPointerCapture(event.pointerId);
        annotationDrag = { pointerId: event.pointerId, start: point, current: point };
      } else if (type === 'mouse_move' && annotationDrag?.pointerId === event.pointerId) {
        annotationDrag = { ...annotationDrag, current: point };
      } else if (type === 'mouse_up' && annotationDrag?.pointerId === event.pointerId) {
        const drag = annotationDrag;
        annotationDrag = null;
        if (canvas?.hasPointerCapture(event.pointerId)) canvas.releasePointerCapture(event.pointerId);
        const frameWidth = renderedFrameMetadata?.width ?? 0;
        const displayRect = canvas?.getBoundingClientRect();
        const threshold = displayRect?.width
          ? 6 * frameWidth / displayRect.width
          : 6;
        if (Math.hypot(point.x - drag.start.x, point.y - drag.start.y) <= threshold) {
          selectElementAnnotation(point);
        } else {
          selectRegionAnnotation(drag.start, point);
        }
      }
      return;
    }
    if (!point) return;
    if (type === 'mouse_down') canvas?.focus();
    sendInput({
      type,
      ...point,
      ...(type === 'mouse_move' ? {} : { button: 'left', click_count: 1 }),
    });
  }

  function handleWheel(event: WheelEvent): void {
    event.preventDefault();
    const point = framePoint(event);
    if (!point) return;
    sendInput({
      type: 'mouse_wheel',
      ...point,
      delta_x: event.deltaX,
      delta_y: event.deltaY,
    });
  }

  function keyboardModifiers(event: KeyboardEvent): number {
    return (event.altKey ? 1 : 0)
      | (event.ctrlKey ? 2 : 0)
      | (event.metaKey ? 4 : 0)
      | (event.shiftKey ? 8 : 0);
  }

  async function pasteClipboard(): Promise<void> {
    try {
      const text = await readBrowserClipboardText();
      if (text) sendInput({ type: 'insert_text', text });
    } catch {
      error = i18n.t('browser.error.clipboardRead');
    }
  }

  function handleKey(event: KeyboardEvent, type: 'key_down' | 'key_up'): void {
    event.preventDefault();
    const primaryModifier = event.metaKey || event.ctrlKey;
    if (type === 'key_down' && primaryModifier && event.key.toLowerCase() === 'v') {
      suppressedKeyUps.add(event.code);
      void pasteClipboard();
      return;
    }
    if (type === 'key_up' && suppressedKeyUps.delete(event.code)) return;
    sendInput({
      type,
      key: event.key,
      code: event.code,
      key_code: event.keyCode,
      modifiers: keyboardModifiers(event),
    });
    if (
      type === 'key_down'
      && event.key.length === 1
      && !event.metaKey
      && !event.ctrlKey
      && !event.altKey
    ) {
      sendInput({ type: 'insert_text', text: event.key });
    }
  }

  function cancelAnnotationDrag(event: PointerEvent): void {
    if (annotationDrag?.pointerId !== event.pointerId) return;
    annotationDrag = null;
    if (canvas?.hasPointerCapture(event.pointerId)) canvas.releasePointerCapture(event.pointerId);
  }

  function annotationRect(annotation: BrowserTabSnapshot['annotations'][number]): {
    x: number;
    y: number;
    width: number;
    height: number;
  } | null {
    return annotation.anchor.kind === 'element'
      ? annotation.anchor.boundingBox
      : annotation.anchor.rect;
  }

  function annotationDragStyle(): string {
    const frame = renderedFrameMetadata;
    const drag = annotationDrag;
    if (!frame || !drag) return '';
    const left = Math.min(drag.start.x, drag.current.x) / frame.width * 100;
    const top = Math.min(drag.start.y, drag.current.y) / frame.height * 100;
    const width = Math.abs(drag.current.x - drag.start.x) / frame.width * 100;
    const height = Math.abs(drag.current.y - drag.start.y) / frame.height * 100;
    return `left: ${left}%; top: ${top}%; width: ${width}%; height: ${height}%;`;
  }

  function pendingAnnotationStyle(): string {
    const rect = pendingAnnotation?.rect;
    if (!rect) return '';
    return `left: ${rect.x * 100}%; top: ${rect.y * 100}%; width: ${rect.width * 100}%; height: ${rect.height * 100}%;`;
  }
</script>

<section class="browser-pane" aria-label={i18n.t('browser.pane.label')}>
  <div class="browser-toolbar">
    <button type="button" class="icon-button flip" onclick={() => navigate('back')} disabled={!activeTab || busy} title={i18n.t('browser.navigation.back')} aria-label={i18n.t('browser.navigation.back')}>
      <Icon name="chevron-right" size={13} />
    </button>
    <button type="button" class="icon-button" onclick={() => navigate('forward')} disabled={!activeTab || busy} title={i18n.t('browser.navigation.forward')} aria-label={i18n.t('browser.navigation.forward')}>
      <Icon name="chevron-right" size={13} />
    </button>
    <button type="button" class="icon-button" onclick={() => navigate('reload')} disabled={!activeTab || busy} title={i18n.t('browser.navigation.reload')} aria-label={i18n.t('browser.navigation.reload')}>
      <Icon name="refresh" size={13} />
    </button>
    <form class="address-form" onsubmit={(event) => { event.preventDefault(); navigate('url'); }}>
      <input
        bind:value={address}
        aria-label={i18n.t('browser.navigation.address')}
        spellcheck="false"
        disabled={!activeTab || busy}
        onkeydown={(event) => {
          if (event.key !== 'Enter') return;
          event.preventDefault();
          navigate('url');
        }}
      />
      <button type="submit" class="address-submit" disabled={!activeTab || busy} title={i18n.t('browser.navigation.go')} aria-label={i18n.t('browser.navigation.go')}>
        <Icon name="chevron-right" size={12} />
      </button>
    </form>
    <div class="viewport-menu-wrap" bind:this={viewportMenuElement}>
      <button
        type="button"
        class="icon-button"
        class:active={activeTab?.viewportMode === 'fixed'}
        onclick={toggleViewportMenu}
        disabled={!activeTab || busy}
        title={i18n.t('browser.viewport.control')}
        aria-label={i18n.t('browser.viewport.control')}
        aria-expanded={viewportMenuOpen}
      >
        <Icon name="monitor" size={13} />
      </button>
      {#if viewportMenuOpen && activeTab}
        <div class="viewport-menu" role="menu" aria-label={i18n.t('browser.viewport.control')}>
          <button
            type="button"
            class:selected={activeTab.viewportMode === 'auto'}
            onclick={useAutomaticViewport}
            role="menuitem"
          >
            <span>{i18n.t('browser.viewport.auto')}</span>
            <span class="viewport-size">{Math.round(viewportSize.width)} x {Math.round(viewportSize.height)}</span>
          </button>
          <div class="viewport-menu-divider"></div>
          <div class="viewport-device-modes" role="group" aria-label={i18n.t('browser.viewport.deviceMode')}>
            {#each VIEWPORT_DEVICE_MODES as mode (mode.id)}
              <button
                type="button"
                class:selected={activeTab.viewportMode === 'fixed' && customViewportDeviceType === mode.deviceType}
                onclick={() => useFixedViewport(mode.width, mode.height)}
              >
                {i18n.t(`browser.viewport.mode.${mode.id}`)}
              </button>
            {/each}
          </div>
          <div class="viewport-menu-divider"></div>
          <div class="viewport-custom">
            <label>
              <span>{i18n.t('browser.viewport.width')}</span>
              <input type="number" min="320" max="7680" bind:value={customViewportWidth} oninput={scheduleCustomViewportUpdate} />
            </label>
            <label>
              <span>{i18n.t('browser.viewport.height')}</span>
              <input type="number" min="240" max="4320" bind:value={customViewportHeight} oninput={scheduleCustomViewportUpdate} />
            </label>
          </div>
        </div>
      {/if}
    </div>
    <button type="button" class="icon-button" onclick={openCurrentPageExternally} disabled={!externalUrl || busy} title={i18n.t('browser.action.openExternal')} aria-label={i18n.t('browser.action.openExternal')}>
      <Icon name="external-link" size={13} />
    </button>
    <button type="button" class="icon-button" onclick={captureScreenshotForMessage} disabled={!activeTab || busy} title={i18n.t('browser.action.screenshot')} aria-label={i18n.t('browser.action.screenshot')}>
      <Icon name="file-plus" size={13} />
    </button>
    <button type="button" class="icon-button" class:active={marking} onclick={toggleMarking} disabled={!activeTab || busy} title={i18n.t('browser.action.annotate')} aria-label={i18n.t('browser.action.annotate')}>
      <Icon name="edit" size={13} />
    </button>
    <div class="annotation-menu-wrap" bind:this={annotationMenuElement}>
      <button
        type="button"
        class="icon-button annotation-history-button"
        class:active={annotationMenuOpen}
        onclick={toggleAnnotationMenu}
        disabled={savedAnnotations.length === 0}
        title={i18n.t('browser.annotation.history')}
        aria-label={i18n.t('browser.annotation.history')}
        aria-expanded={annotationMenuOpen}
      >
        <Icon name="target" size={13} />
        {#if savedAnnotations.length > 0}<span class="annotation-count">{savedAnnotations.length}</span>{/if}
      </button>
      {#if annotationMenuOpen}
        <div class="annotation-menu" role="menu" aria-label={i18n.t('browser.annotation.history')}>
          {#each savedAnnotations as annotation (annotation.annotationId)}
            <button
              type="button"
              class:stale={annotation.status === 'stale'}
              disabled={!annotation.screenshotArtifactId}
              onclick={() => openAnnotationPreview(annotation, annotation.sequence)}
              role="menuitem"
              title={annotation.comment}
            >
              <span class="annotation-menu-number">{annotation.sequence}</span>
              <span class="annotation-menu-comment">{annotation.comment}</span>
            </button>
          {/each}
        </div>
      {/if}
    </div>
    <span
      class="status-light"
      class:ready={connectionState === 'ready'}
      class:error={connectionState === 'error'}
      title={connectionStatusText}
      aria-label={connectionStatusText}
      role="status"
    ></span>
  </div>

  <div
    bind:this={viewportElement}
    class="browser-viewport interactive"
    class:marking
  >
    {#if connectionNotice}
      <div class="browser-notice" class:error={connectionState === 'error'} role="status">
        {connectionNotice}
      </div>
    {/if}
    {#if loading}
      <div class="browser-placeholder">{i18n.t('browser.status.connecting')}</div>
    {:else if !activeTab}
      <div class="browser-placeholder">{i18n.t('browser.status.noTab')}</div>
    {/if}
    <div class="browser-frame" style={displayedFrameStyle()}>
      <canvas
        bind:this={canvas}
        tabindex="0"
        aria-label={i18n.t('browser.viewport.label')}
        onpointermove={(event) => handlePointer(event, 'mouse_move')}
        onpointerdown={(event) => handlePointer(event, 'mouse_down')}
        onpointerup={(event) => handlePointer(event, 'mouse_up')}
        onpointercancel={cancelAnnotationDrag}
        onwheel={handleWheel}
        onkeydown={(event) => handleKey(event, 'key_down')}
        onkeyup={(event) => handleKey(event, 'key_up')}
      ></canvas>
      {#each (activeTab?.annotations ?? []).filter((annotation) => annotation.status === 'active') as annotation (annotation.annotationId)}
        {@const rect = annotationRect(annotation)}
        {#if rect}
          <button
            type="button"
            class="annotation-marker"
            class:element={annotation.anchor.kind === 'element'}
            style={`left: ${rect.x * 100}%; top: ${rect.y * 100}%; width: ${rect.width * 100}%; height: ${rect.height * 100}%;`}
            title={annotation.comment}
            aria-label={annotation.comment}
            onclick={() => selectSavedAnnotation(annotation)}
          ><span class="annotation-number">{annotation.sequence}</span></button>
        {/if}
      {/each}
      {#if annotationDrag}
        <div class="annotation-draft" style={annotationDragStyle()}></div>
      {/if}
      {#if pendingAnnotation}
        <div
          class="annotation-selection"
          class:element={pendingAnnotation.kind === 'element'}
          style={pendingAnnotationStyle()}
        ><span class="annotation-number">{nextAnnotationSequence}</span></div>
      {/if}
    </div>
    {#if pendingAnnotation}
      <div class="annotation-editor" role="dialog" aria-label={i18n.t('browser.annotation.title')}>
        <div class="annotation-editor-title">{i18n.t('browser.annotation.title')}</div>
        <textarea
          bind:this={annotationInput}
          bind:value={pendingAnnotationComment}
          maxlength="4000"
          rows="3"
          placeholder={i18n.t('browser.annotation.placeholder')}
          aria-label={i18n.t('browser.annotation.placeholder')}
          onkeydown={handleAnnotationEditorKey}
        ></textarea>
        <div class="annotation-editor-actions">
          <button type="button" onclick={cancelPendingAnnotation} disabled={busy}>{i18n.t('browser.annotation.cancel')}</button>
          <button type="button" class="primary" onclick={savePendingAnnotation} disabled={busy || !pendingAnnotationComment.trim()}>{i18n.t('browser.annotation.save')}</button>
        </div>
      </div>
    {/if}
  </div>
  {#if annotationPreview}
    <div class="annotation-preview" role="dialog" aria-label={i18n.t('browser.annotation.preview')}>
      <div class="annotation-preview-header">
        <span class="annotation-menu-number">{annotationPreview.sequence}</span>
        <span class="annotation-preview-comment">{annotationPreview.comment}</span>
        <button type="button" class="icon-button" onclick={closeAnnotationPreview} title={i18n.t('browser.annotation.closePreview')} aria-label={i18n.t('browser.annotation.closePreview')}>
          <Icon name="close" size={13} />
        </button>
      </div>
      <div class="annotation-preview-body">
        {#if annotationPreviewFailed}
          <span>{i18n.t('browser.annotation.previewFailed')}</span>
        {:else}
          <img
            src={annotationPreview.url}
            alt={i18n.t('browser.annotation.previewAlt', { index: annotationPreview.sequence })}
            onerror={() => { annotationPreviewFailed = true; }}
          />
        {/if}
      </div>
    </div>
  {/if}
</section>

<style>
  .browser-pane { position: relative; display: flex; flex-direction: column; height: 100%; min-height: 0; background: var(--background); }
  .browser-toolbar { display: flex; align-items: center; min-height: 34px; gap: 3px; padding: 4px 6px; border-bottom: 1px solid var(--border); flex-shrink: 0; }
  .icon-button { width: 27px; height: 27px; display: inline-flex; align-items: center; justify-content: center; border: 0; border-radius: var(--radius-sm); background: transparent; color: var(--foreground-muted); cursor: pointer; flex-shrink: 0; }
  .icon-button:hover:not(:disabled) { background: var(--surface-2); color: var(--foreground); }
  .icon-button.active { color: var(--primary); background: var(--surface-2); }
  .icon-button:disabled { opacity: .45; cursor: default; }
  .flip { transform: scaleX(-1); }
  .address-form { display: flex; flex: 1; min-width: 80px; }
  .address-form input { box-sizing: border-box; width: 100%; min-width: 0; height: 27px; border: 1px solid var(--border); border-right: 0; border-radius: var(--radius-sm) 0 0 var(--radius-sm); background: var(--surface-1); color: var(--foreground); padding: 0 8px; font: inherit; }
  .address-submit { display: grid; place-items: center; width: 27px; height: 27px; flex: 0 0 27px; padding: 0; border: 1px solid var(--border); border-radius: 0 var(--radius-sm) var(--radius-sm) 0; background: var(--surface-1); color: var(--foreground-muted); cursor: pointer; }
  .address-submit:hover:not(:disabled) { background: var(--surface-hover); color: var(--foreground); }
  .address-submit:disabled { opacity: .45; cursor: default; }
  .viewport-menu-wrap { position: relative; display: flex; flex: 0 0 auto; }
  .annotation-menu-wrap { position: relative; display: flex; flex: 0 0 auto; }
  .annotation-history-button { position: relative; }
  .annotation-count { position: absolute; top: 1px; right: 1px; min-width: 12px; height: 12px; padding: 0 2px; border-radius: 6px; background: var(--info); color: white; font-size: 8px; font-weight: 700; line-height: 12px; text-align: center; }
  .annotation-menu { position: absolute; z-index: 20; top: calc(100% + 5px); right: 0; width: min(300px, calc(100vw - 24px)); max-height: 240px; overflow: auto; padding: 5px; border: 1px solid var(--border); border-radius: 6px; background: var(--dropdown-bg); box-shadow: var(--shadow-lg); }
  .annotation-menu > button { box-sizing: border-box; display: flex; align-items: center; gap: 7px; width: 100%; min-height: 32px; padding: 4px 7px; border: 0; border-radius: 4px; background: transparent; color: var(--foreground); font: inherit; font-size: var(--text-xs); cursor: pointer; text-align: left; }
  .annotation-menu > button:hover:not(:disabled) { background: var(--surface-hover); }
  .annotation-menu > button.stale { color: var(--foreground-muted); }
  .annotation-menu > button:disabled { opacity: .5; cursor: default; }
  .annotation-menu-number { display: grid; place-items: center; flex: 0 0 19px; width: 19px; height: 19px; border-radius: 50%; background: var(--info); color: white; font-size: 10px; font-weight: 700; line-height: 1; }
  .annotation-menu-comment { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .viewport-menu { position: absolute; z-index: 20; top: calc(100% + 5px); right: 0; width: 242px; padding: 5px; border: 1px solid var(--border); border-radius: 6px; background: var(--dropdown-bg); box-shadow: var(--shadow-lg); }
  .viewport-menu > button { box-sizing: border-box; display: flex; align-items: center; justify-content: space-between; width: 100%; min-height: 30px; padding: 0 8px; border: 0; border-radius: 4px; background: transparent; color: var(--foreground); font: inherit; font-size: var(--text-xs); cursor: pointer; }
  .viewport-menu > button:hover,
  .viewport-menu > button.selected { background: var(--surface-hover); }
  .viewport-menu > button.selected { color: var(--primary); }
  .viewport-size { color: var(--foreground-muted); font-variant-numeric: tabular-nums; }
  .viewport-menu-divider { height: 1px; margin: 4px 3px; background: var(--border); }
  .viewport-device-modes { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 3px; padding: 3px; }
  .viewport-device-modes button { min-width: 0; height: 28px; border: 1px solid var(--border); border-radius: 4px; background: var(--surface-1); color: var(--foreground-muted); font: inherit; font-size: var(--text-xs); cursor: pointer; }
  .viewport-device-modes button:hover { background: var(--surface-hover); color: var(--foreground); }
  .viewport-device-modes button.selected { border-color: var(--primary); background: var(--surface-hover); color: var(--primary); }
  .viewport-custom { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1fr); gap: 5px; align-items: end; padding: 3px; }
  .viewport-custom label { display: grid; gap: 3px; min-width: 0; color: var(--foreground-muted); font-size: 10px; }
  .viewport-custom input { box-sizing: border-box; width: 100%; height: 27px; min-width: 0; padding: 0 5px; border: 1px solid var(--border); border-radius: 4px; background: var(--surface-1); color: var(--foreground); font: inherit; font-size: var(--text-xs); }
  .status-light { width: 7px; height: 7px; margin: 0 4px; border-radius: 50%; background: var(--warning); flex-shrink: 0; }
  .status-light.ready { background: var(--success); }
  .status-light.error { background: var(--error); }
  .browser-viewport { position: relative; flex: 1; min-height: 0; overflow: hidden; background: #151719; display: grid; place-items: center; }
  .browser-frame { position: relative; flex: 0 0 auto; overflow: hidden; }
  .browser-frame canvas { display: block; width: 100%; height: 100%; outline: none; }
  .browser-viewport.interactive canvas { cursor: default; }
  .browser-viewport.marking canvas { cursor: crosshair; }
  .browser-placeholder { position: absolute; color: #b6bbc2; font-size: var(--text-sm); }
  .browser-notice { position: absolute; z-index: 7; top: 10px; left: 50%; max-width: calc(100% - 24px); transform: translateX(-50%); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; padding: 4px 8px; border: 1px solid color-mix(in srgb, var(--warning) 45%, var(--border)); border-radius: 4px; background: color-mix(in srgb, var(--background) 92%, transparent); color: var(--foreground-muted); box-shadow: 0 2px 8px rgb(0 0 0 / 18%); font-size: var(--text-xs); }
  .browser-notice.error { border-color: color-mix(in srgb, var(--error) 45%, var(--border)); color: var(--error); }
  .annotation-marker { position: absolute; z-index: 4; min-width: 8px; min-height: 8px; padding: 0; border: 2px solid var(--info); border-radius: 3px; background: color-mix(in srgb, var(--info) 9%, transparent); cursor: pointer; }
  .browser-viewport.marking .annotation-marker { pointer-events: none; }
  .annotation-marker.element { background: color-mix(in srgb, var(--info) 13%, transparent); }
  .annotation-draft { position: absolute; min-width: 2px; min-height: 2px; border: 1px dashed var(--warning); background: color-mix(in srgb, var(--warning) 12%, transparent); pointer-events: none; }
  .annotation-selection { position: absolute; z-index: 3; min-width: 3px; min-height: 3px; border: 2px solid var(--info); border-radius: 3px; background: color-mix(in srgb, var(--info) 9%, transparent); pointer-events: none; }
  .annotation-selection.element { background: color-mix(in srgb, var(--info) 13%, transparent); }
  .annotation-number { position: absolute; top: -11px; right: -11px; display: grid; place-items: center; box-sizing: border-box; width: 21px; height: 21px; border: 2px solid white; border-radius: 50%; background: var(--info); color: white; font-size: 11px; font-weight: 700; line-height: 1; }
  .annotation-editor { position: absolute; z-index: 8; top: 12px; right: 12px; box-sizing: border-box; width: min(300px, calc(100% - 24px)); padding: 10px; border: 1px solid var(--border); border-radius: 6px; background: var(--background); box-shadow: 0 10px 30px rgb(0 0 0 / 28%); }
  .annotation-editor-title { margin-bottom: 7px; color: var(--foreground); font-size: var(--text-sm); font-weight: 600; }
  .annotation-editor textarea { box-sizing: border-box; display: block; width: 100%; min-height: 70px; resize: vertical; border: 1px solid var(--border); border-radius: 4px; background: var(--surface-1); color: var(--foreground); padding: 7px 8px; font: inherit; font-size: var(--text-sm); line-height: 1.45; }
  .annotation-editor textarea:focus { outline: 1px solid var(--primary); border-color: var(--primary); }
  .annotation-editor-actions { display: flex; justify-content: flex-end; gap: 6px; margin-top: 8px; }
  .annotation-editor-actions button { min-height: 27px; padding: 0 10px; border: 1px solid var(--border); border-radius: 4px; background: var(--surface-1); color: var(--foreground); font: inherit; font-size: var(--text-xs); cursor: pointer; }
  .annotation-editor-actions button.primary { border-color: var(--primary); background: var(--primary); color: var(--primary-foreground); }
  .annotation-editor-actions button:disabled { opacity: .5; cursor: default; }
  .annotation-preview { position: absolute; z-index: 30; inset: 38px 8px 8px; display: flex; flex-direction: column; min-width: 0; min-height: 0; border: 1px solid var(--border); border-radius: 6px; background: var(--background); box-shadow: var(--shadow-lg); }
  .annotation-preview-header { display: flex; align-items: center; gap: 7px; min-height: 34px; padding: 3px 4px 3px 8px; border-bottom: 1px solid var(--border); }
  .annotation-preview-comment { min-width: 0; flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--foreground); font-size: var(--text-xs); }
  .annotation-preview-body { display: grid; place-items: center; flex: 1; min-height: 0; overflow: auto; padding: 8px; background: #151719; color: #b6bbc2; font-size: var(--text-xs); }
  .annotation-preview-body img { display: block; max-width: 100%; max-height: 100%; object-fit: contain; image-rendering: auto; }
</style>
