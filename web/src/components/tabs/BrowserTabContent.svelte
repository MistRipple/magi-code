<script lang="ts">
  import { onMount, tick, untrack } from 'svelte';
  import Icon from '../Icon.svelte';
  import { i18n } from '../../stores/i18n.svelte';
  import { normalizeExternalWebUrl, openExternalWebUrl } from '../../lib/external-link';
  import {
    browserScreenshotUrl,
    browserClientPlatform,
    getBrowserSession,
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
  import { onDesktopBlockingOverlayChange } from '../../shared/desktop-overlay-contract';

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

  interface DesktopBrowserEvent {
    type?: string;
    binding?: { tab_id?: string };
    page?: { url?: string; title?: string };
    loading?: boolean;
    reason?: string;
  }

  const VIEWPORT_DEVICE_MODES = [
    { id: 'wide', width: 1280, height: 800, deviceType: 'desktop' },
    { id: 'narrow', width: 390, height: 844, deviceType: 'mobile' },
  ] as const;
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
  // Browser Tab 的运行通道由右栏宿主显式决定。Desktop 使用 Main 进程创建的
  // WebContentsView；Renderer 只负责提供当前 Browser Tab 的内容槽几何。
  const desktopRuntime = $derived(desktopSurface);
  let browserSurfaceSlot = $state<HTMLDivElement | null>(null);
  let desktopSnapshot = $state<MagiDesktopWindowSnapshot | null>(null);
  let browserSlotPublished = $state(false);
  let snapshot = $state<BrowserSessionSnapshot | null>(null);
  let address = $state('');
  let addressEditing = $state(false);
  let loading = $state(true);
  let browserLoading = $state(false);
  let sessionError = $state('');
  let actionError = $state('');
  let busy = $state(false);
  let viewportMenuElement = $state<HTMLDivElement | undefined>();
  let annotationMenuElement = $state<HTMLDivElement | undefined>();
  let viewportMenuButton = $state<HTMLButtonElement | undefined>();
  let annotationHistoryButton = $state<HTMLButtonElement | undefined>();
  let viewportMenuOpen = $state(false);
  let annotationMenuOpen = $state(false);
  let localViewportMode = $state<BrowserViewportMode>('auto');
  let localViewport = $state({ width: 1280, height: 800, deviceType: 'desktop' as BrowserDeviceType });
  let customViewportWidth = $state(390);
  let customViewportHeight = $state(844);
  let customViewportTimer: number | null = null;
  let pendingViewport: { width: number; height: number; deviceType: BrowserDeviceType } | null = null;
  let desktopOverlayId = $state<string | null>(null);
  let annotationSelection = $state<BrowserAnnotationSelection | null>(null);
  let annotationComment = $state('');
  let browserSurfaceHiddenByOverlay = $state(false);
  let refreshGeneration = 0;
  let desktopSurfaceSyncGeneration = 0;
  let activeBrowserIdentityKey = '';
  let publishedSlotTabId = '';

  const activeTab = $derived.by<BrowserTabSnapshot | null>(() => (
    snapshot?.tabs.find((tab) => tab.tabId === tabId && tab.lifecycle !== 'closed') ?? null
  ));
  const savedAnnotations = $derived((activeTab?.annotations ?? []).filter((annotation) => annotation.status !== 'deleted'));
  const externalUrl = $derived(normalizeExternalWebUrl(activeTab?.url || address));
  const browserSlotHostAvailable = $derived(
    // Main snapshot 是右栏宿主布局的唯一几何权威。内容槽必须先发布
    // 几何，Main 才能在异步物化 Chromium Surface 后重新绑定它；不能用
    // activeSurfaceId 反向门控首个槽位发布，否则会形成初始化死锁。
    desktopRuntime
      && desktopSnapshot?.layout.rightPaneVisible === true
      && desktopSnapshot?.layout.activePanelKind === 'browser'
      && desktopSnapshot.layout.activeTabId === tabId
      && !browserSurfaceHiddenByOverlay,
  );
  const browserSurfaceAvailable = $derived(
    browserSlotHostAvailable && Boolean(desktopSnapshot?.layout.activeSurfaceId),
  );
  const browserReady = $derived(browserSurfaceAvailable && browserSlotPublished);
  const error = $derived(actionError || sessionError);
  const lifecycleFailure = $derived(lifecycle === 'crashed' || activeTab?.lifecycle === 'crashed');
  const connectionState = $derived.by<'ready' | 'connecting' | 'error'>(() => {
    if (error || lifecycleFailure || !desktopRuntime) return 'error';
    // 元数据刷新属于页面信息同步，不代表 Chromium Surface 的连接状态。
    // 真实 Surface 已经由 Main 激活后，地址栏和页面操作必须立即可用。
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
      // WebContentsView 已经就绪时，即使页面正在请求资源，也不能把连接状态
      // 显示成“正在连接”，否则用户会误以为内置浏览器尚未启动。
      return i18n.t('browser.status.connected');
    }
    return i18n.t('browser.status.connecting');
  });

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

  function fixedPresetSelected(mode: (typeof VIEWPORT_DEVICE_MODES)[number]): boolean {
    return localViewportMode === 'fixed'
      && localViewport.width === mode.width
      && localViewport.height === mode.height;
  }

  async function updateLogicalViewport(
    mode: BrowserViewportMode,
    viewport?: { width: number; height: number; deviceType: BrowserDeviceType },
  ): Promise<void> {
    const tab = activeTab;
    const desktop = window.magiDesktop;
    if (!tab || !desktop) return;
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
    applyDesktopViewport(next);
    actionError = '';
  }

  function applyDesktopViewport(next: MagiDesktopWindowSnapshot): void {
    desktopSnapshot = next;
    latestLayoutRevision = next.layout.layoutRevision;
    const slotHostMatchesTab = next.layout.rightPaneVisible
      && next.layout.activePanelKind === 'browser'
      && next.layout.activeTabId === tabId;
    if (!slotHostMatchesTab) {
      invalidateBrowserSlotPublication();
    } else {
      // Surface 物化会让 activeSurfaceId 先为空、再切换为有效值；布局快照
      // 是这一状态变化的唯一可靠通知。每次收到当前宿主快照都重新提交
      // 当前 DOM 槽位，确保异步物化完成后原生 Surface 一定能接上。
      scheduleBrowserSlotBounds();
    }
    if (next.layout.activeTabId !== tabId || !next.activeBrowserViewport) return;
    const viewport = next.activeBrowserViewport;
    localViewportMode = viewport.mode;
    if (viewport.mode === 'auto') return;
    localViewport = {
      width: viewport.width,
      height: viewport.height,
      deviceType: viewport.device_type,
    };
    customViewportWidth = viewport.width;
    customViewportHeight = viewport.height;
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
      if (
        next.layout.activePanelKind === 'browser'
        && next.layout.rightPaneVisible
        && next.layout.activeTabId === expectedTabId
      ) {
        scheduleBrowserSlotBounds();
      }
    } catch (cause) {
      if (generation === desktopSurfaceSyncGeneration) {
        actionError = errorMessage(cause);
      }
    }
  }

  function useAutomaticViewport(): void {
    void run(async () => updateLogicalViewport('auto'));
  }

  function useFixedViewport(width: number, height: number): void {
    const viewport = { width, height, deviceType: width <= 600 ? 'mobile' as const : 'desktop' as const };
    customViewportWidth = width;
    customViewportHeight = height;
    void run(async () => updateLogicalViewport('fixed', viewport));
  }

  function scheduleCustomViewportUpdate(): void {
    const width = Math.round(Number(customViewportWidth));
    const height = Math.round(Number(customViewportHeight));
    if (width < 320 || width > 7680 || height < 240 || height > 4320) return;
    pendingViewport = { width, height, deviceType: width <= 600 ? 'mobile' : 'desktop' };
    if (customViewportTimer !== null) window.clearTimeout(customViewportTimer);
    customViewportTimer = window.setTimeout(() => {
      customViewportTimer = null;
      const next = pendingViewport;
      pendingViewport = null;
      if (next) void run(async () => updateLogicalViewport('fixed', next));
    }, CUSTOM_VIEWPORT_DEBOUNCE_MILLIS);
  }

  function openDesktopViewportMenu(): void {
    const desktop = window.magiDesktop;
    const anchor = viewportMenuButton?.getBoundingClientRect();
    if (!desktop || !anchor || anchor.width <= 0 || anchor.height <= 0) return;
    const overlayId = `browser-viewport-${tabId}`;
    desktopOverlayId = overlayId;
    void desktop.openOverlay({
      overlayId,
      kind: 'menu',
      phase: 'menu',
      ownerId: `browser:${tabId}`,
      placement: 'browser-viewport',
      anchorBounds: {
        x: anchor.left,
        y: anchor.top,
        width: anchor.width,
        height: anchor.height,
      },
      title: i18n.t('browser.viewport.control'),
      items: [
        {
          id: 'auto',
          label: i18n.t('browser.viewport.auto'),
          icon: 'monitor',
          selected: localViewportMode === 'auto',
          disabled: false,
        },
        {
          id: 'wide',
          label: i18n.t('browser.viewport.mode.wide'),
          icon: 'monitor',
          selected: fixedPresetSelected(VIEWPORT_DEVICE_MODES[0]),
          disabled: false,
        },
        {
          id: 'narrow',
          label: i18n.t('browser.viewport.mode.narrow'),
          icon: 'monitor',
          selected: fixedPresetSelected(VIEWPORT_DEVICE_MODES[1]),
          disabled: false,
        },
      ],
      fields: [
        {
          id: 'width',
          label: i18n.t('browser.viewport.width'),
          type: 'number',
          value: String(customViewportWidth),
          min: 320,
          max: 7680,
        },
        {
          id: 'height',
          label: i18n.t('browser.viewport.height'),
          type: 'number',
          value: String(customViewportHeight),
          min: 240,
          max: 4320,
        },
      ],
    }).catch((cause) => {
      if (desktopOverlayId === overlayId) desktopOverlayId = null;
      actionError = errorMessage(cause);
    });
  }

  function toggleViewportMenu(): void {
    if (desktopRuntime) {
      if (desktopOverlayId) {
        desktopOverlayId = null;
        void window.magiDesktop?.closeOverlay();
      } else {
        openDesktopViewportMenu();
      }
      return;
    }
    viewportMenuOpen = !viewportMenuOpen;
    annotationMenuOpen = false;
  }

  function openDesktopAnnotationMenu(): void {
    const desktop = window.magiDesktop;
    const anchor = annotationHistoryButton?.getBoundingClientRect();
    if (!desktop || !anchor || anchor.width <= 0 || anchor.height <= 0) return;
    const overlayId = `browser-annotations-${tabId}`;
    desktopOverlayId = overlayId;
    void desktop.openOverlay({
      overlayId,
      kind: 'menu',
      phase: 'menu',
      ownerId: `browser:${tabId}`,
      placement: 'browser-annotations',
      anchorBounds: {
        x: anchor.left,
        y: anchor.top,
        width: anchor.width,
        height: anchor.height,
      },
      title: i18n.t('browser.annotation.history'),
      items: savedAnnotations.map((annotation) => ({
        id: annotation.annotationId,
        label: `${annotation.sequence}. ${annotation.comment}`,
        icon: 'target',
        selected: false,
        disabled: false,
      })),
      fields: [],
    }).catch((cause) => {
      if (desktopOverlayId === overlayId) desktopOverlayId = null;
      actionError = errorMessage(cause);
    });
  }

  function openDesktopAnnotationCreation(): void {
    const desktop = window.magiDesktop;
    if (!desktop || !activeTab || !browserReady || desktopOverlayId) return;
    const overlayId = `browser-annotation-create-${tabId}`;
    annotationSelection = null;
    annotationComment = '';
    desktopOverlayId = overlayId;
    void desktop.openOverlay({
      overlayId,
      kind: 'annotation',
      phase: 'select',
      ownerId: `browser:${tabId}`,
      placement: 'browser-annotations',
      anchorBounds: null,
      title: i18n.t('browser.annotation.title'),
      items: [],
      fields: [],
    }).catch((cause) => {
      if (desktopOverlayId === overlayId) desktopOverlayId = null;
      actionError = errorMessage(cause);
    });
  }

  function clampUnit(value: number): number {
    return Math.max(0, Math.min(1, value));
  }

  function parseAnnotationSelection(value: string | null): BrowserAnnotationSelection | null {
    if (!value || !activeTab) return null;
    try {
      const parsed = JSON.parse(value) as {
        kind?: string;
        x?: number;
        y?: number;
        rect?: Partial<BrowserNormalizedRect>;
      };
      const navigationRevision = activeTab.navigationRevision;
      if (parsed.kind === 'element' && Number.isFinite(parsed.x) && Number.isFinite(parsed.y)) {
        return {
          kind: 'element',
          navigationRevision,
          x: clampUnit(Number(parsed.x)),
          y: clampUnit(Number(parsed.y)),
        };
      }
      if (parsed.kind === 'region' && parsed.rect) {
        const rect = parsed.rect;
        if (![rect.x, rect.y, rect.width, rect.height].every((item) => Number.isFinite(item))) return null;
        return {
          kind: 'region',
          navigationRevision,
          rect: {
            x: clampUnit(Number(rect.x)),
            y: clampUnit(Number(rect.y)),
            width: clampUnit(Number(rect.width)),
            height: clampUnit(Number(rect.height)),
          },
        };
      }
    } catch {
      return null;
    }
    return null;
  }

  function openAnnotationCommentOverlay(): void {
    const desktop = window.magiDesktop;
    if (!desktop || !desktopOverlayId) return;
    void desktop.openOverlay({
      overlayId: desktopOverlayId,
      kind: 'annotation',
      phase: 'comment',
      ownerId: `browser:${tabId}`,
      placement: 'browser-annotations',
      anchorBounds: null,
      title: i18n.t('browser.annotation.title'),
      items: [],
      fields: [{
        id: 'comment',
        label: i18n.t('browser.annotation.placeholder'),
        type: 'text',
        value: annotationComment,
        min: null,
        max: 4000,
      }],
    }).catch((cause) => {
      desktopOverlayId = null;
      actionError = errorMessage(cause);
    });
  }

  function submitCreatedAnnotation(): void {
    const tab = activeTab;
    const selection = annotationSelection;
    const comment = annotationComment.trim();
    const overlayId = desktopOverlayId;
    if (!tab || !selection || !comment || !overlayId) return;
    void run(async () => {
      const created = await createBrowserAnnotation(tab.tabId, selection, comment);
      await refreshSession(true);
      window.dispatchEvent(new CustomEvent('magi:browserAnnotationCreated', { detail: created }));
      annotationSelection = null;
      annotationComment = '';
    });
  }

  function toggleAnnotationMenu(): void {
    if (desktopRuntime) {
      if (desktopOverlayId) {
        desktopOverlayId = null;
        void window.magiDesktop?.closeOverlay();
      } else if (savedAnnotations.length > 0) {
        openDesktopAnnotationMenu();
      }
      return;
    }
    annotationMenuOpen = !annotationMenuOpen;
    viewportMenuOpen = false;
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
    void run(async () => {
      const updated = await navigateBrowserTab(tab.tabId, action, action === 'url' ? address : undefined);
      address = updated.url;
      await refreshSession();
    });
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
    window.dispatchEvent(new CustomEvent('magi:browserAnnotationCreated', { detail: annotation }));
  }

  function handleDesktopBrowserEvent(value: unknown): void {
    if (!value || typeof value !== 'object') return;
    const event = value as DesktopBrowserEvent;
    if (event.binding?.tab_id !== tabId) return;
    if (event.type === 'loading_changed') {
      browserLoading = event.loading === true;
      return;
    }
    if (event.type === 'page_failed') {
      actionError = event.reason?.trim() || i18n.t('browser.error.pageLoadFailed');
      browserLoading = false;
      return;
    }
    if (event.type !== 'page_updated' || !event.page) return;
    actionError = '';
    if (event.page.url?.trim()) {
      if (!addressEditing) address = event.page.url;
    }
    if (event.page.title?.trim()) onTitleChange?.(event.page.title);
  }

  let slotPublishTimer: number | null = null;
  let lastPublishedSlot = '';
  let pendingPublishedSlot = '';
  let slotRevision = 0;
  let slotLifecycleGeneration = 0;
  let slotOwnerTabId = '';
  let componentDisposed = false;
  let latestLayoutRevision = 0;

  function invalidateBrowserSlotPublication(): number {
    slotLifecycleGeneration += 1;
    slotRevision += 1;
    browserSlotPublished = false;
    publishedSlotTabId = '';
    slotOwnerTabId = '';
    lastPublishedSlot = '';
    pendingPublishedSlot = '';
    return slotRevision;
  }

  function clearPublishedBrowserSlot(tabId: string): void {
    const desktop = window.magiDesktop;
    const revision = invalidateBrowserSlotPublication();
    if (!desktop || !tabId) return;
    void desktop.updateBrowserSlot({
      tabId,
      slotRevision: revision,
      layoutRevision: latestLayoutRevision,
      bounds: null,
    }).catch(() => undefined);
  }

  function publishBrowserSlotBounds(): void {
    slotPublishTimer = null;
    const desktop = window.magiDesktop;
    const slot = browserSurfaceSlot;
    const currentTab = activeTab;
    if (!browserSlotHostAvailable || !desktop || !slot || !currentTab || currentTab.tabId !== tabId) return;
    const rect = slot.getBoundingClientRect();
    if (rect.width <= 0 || rect.height <= 0) return;
    const bounds = {
      x: Math.round(rect.left),
      y: Math.round(rect.top),
      width: Math.round(rect.width),
      height: Math.round(rect.height),
    };
    const publicationGeneration = slotLifecycleGeneration;
    const publicationTabId = tabId;
    const boundsKey = `${publicationTabId}:${bounds.x}:${bounds.y}:${bounds.width}:${bounds.height}`;
    const publicationKey = `${publicationGeneration}:${boundsKey}`;
    if (boundsKey === lastPublishedSlot || publicationKey === pendingPublishedSlot) return;
    slotOwnerTabId = publicationTabId;
    pendingPublishedSlot = publicationKey;
    const nextSlotRevision = ++slotRevision;
    void desktop.updateBrowserSlot({
      tabId: publicationTabId,
      slotRevision: nextSlotRevision,
      layoutRevision: latestLayoutRevision,
      bounds,
    })
      .then((next) => {
        if (
          componentDisposed
          || publicationGeneration !== slotLifecycleGeneration
          || nextSlotRevision !== slotRevision
          || publicationTabId !== tabId
          || publicationTabId !== currentTab.tabId
        ) return;
        const accepted = next.layout.activePanelKind === 'browser'
          && next.layout.rightPaneVisible
          && next.layout.activeTabId === publicationTabId
          && Boolean(next.layout.activeSurfaceId);
        browserSlotPublished = accepted;
        if (accepted) {
          publishedSlotTabId = publicationTabId;
          lastPublishedSlot = boundsKey;
        }
        applyDesktopViewport(next);
      })
      .catch((cause) => {
        if (
          !componentDisposed
          && publicationGeneration === slotLifecycleGeneration
          && nextSlotRevision === slotRevision
          && publicationTabId === tabId
        ) {
          actionError = errorMessage(cause);
        }
      })
      .finally(() => {
        if (pendingPublishedSlot === publicationKey) pendingPublishedSlot = '';
      });
  }

  function scheduleBrowserSlotBounds(): void {
    if (slotPublishTimer !== null) return;
    // requestAnimationFrame 在隐藏的 Electron WebContents 中会暂停，导致
    // 窗口首次显示前或系统切换窗口时永远无法提交内容槽。用短定时器合并
    // 同一事件循环内的布局变化，既不触发页面刷新，也不依赖窗口可见性。
    slotPublishTimer = window.setTimeout(publishBrowserSlotBounds, 0);
  }

  // 内容槽是在 Browser Tab 激活后才挂载的，不能只在 onMount 时读取一次
  // browserSurfaceSlot。懒绑定 ResizeObserver，确保右栏拖动、布局切换和
  // 首次物化都沿同一条内容槽发布链路更新原生 WebContentsView。
  $effect(() => {
    const slot = browserSurfaceSlot;
    if (!slot || typeof ResizeObserver === 'undefined') return;
    const observer = new ResizeObserver(scheduleBrowserSlotBounds);
    observer.observe(slot);
    return () => observer.disconnect();
  });

  $effect(() => {
    const slot = browserSurfaceSlot;
    const currentTab = activeTab;
    const available = browserSlotHostAvailable;
    if (!desktopRuntime || !slot || !currentTab || !available) return;
    void tick().then(scheduleBrowserSlotBounds);
  });

  $effect(() => {
    if (!desktopRuntime) return;
    void synchronizeDesktopSurface();
  });

  $effect(() => {
    const expectedSessionId = browserSessionId.trim();
    const expectedTabId = tabId.trim();
    const identityKey = `${expectedSessionId}\u0000${expectedTabId}`;
    if (identityKey === activeBrowserIdentityKey) return;
    activeBrowserIdentityKey = identityKey;
    untrack(() => {
      snapshot = null;
      address = '';
      addressEditing = false;
      sessionError = '';
      actionError = '';
      browserLoading = false;
    browserSlotPublished = false;
      const previousSlotTabId = slotOwnerTabId || publishedSlotTabId;
      if (previousSlotTabId && previousSlotTabId !== expectedTabId) {
        clearPublishedBrowserSlot(previousSlotTabId);
      } else {
        invalidateBrowserSlotPublication();
      }
      loading = Boolean(expectedSessionId && expectedTabId);
      if (!expectedSessionId || !expectedTabId) return;
      void refreshSession(true);
    });
  });

  onMount(() => {
    const desktop = window.magiDesktop;
    if (desktop) void synchronizeDesktopSurface();
    window.addEventListener('resize', scheduleBrowserSlotBounds);
    window.visualViewport?.addEventListener('resize', scheduleBrowserSlotBounds);
    const pointerDown = (event: PointerEvent) => {
      const target = event.target;
      if (viewportMenuElement && target instanceof Node && !viewportMenuElement.contains(target)) viewportMenuOpen = false;
      if (annotationMenuElement && target instanceof Node && !annotationMenuElement.contains(target)) annotationMenuOpen = false;
    };
    const keyboard = (event: KeyboardEvent) => {
      if (event.key !== 'Escape') return;
      viewportMenuOpen = false;
      annotationMenuOpen = false;
      if (desktopOverlayId) {
        desktopOverlayId = null;
        void desktop?.closeOverlay();
      }
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
    const unsubscribeOverlayAction = desktop?.onOverlayAction((action) => {
      if (
        !desktopOverlayId
        || action.overlayId !== desktopOverlayId
        || action.ownerId !== `browser:${tabId}`
      ) return;
      if (action.interaction === 'input') {
        if (action.kind === 'annotation' && action.id === 'comment') {
          annotationComment = action.value ?? '';
          return;
        }
        if (action.id === 'width') customViewportWidth = Number(action.value ?? customViewportWidth);
        if (action.id === 'height') customViewportHeight = Number(action.value ?? customViewportHeight);
        scheduleCustomViewportUpdate();
        return;
      }
      if (action.kind === 'annotation') {
        if (action.id === 'selection') {
          const selection = parseAnnotationSelection(action.value);
          if (!selection) {
            actionError = i18n.t('browser.annotation.pageChanged');
            desktopOverlayId = null;
            return;
          }
          annotationSelection = selection;
          annotationComment = '';
          openAnnotationCommentOverlay();
          return;
        }
        if (action.id === 'save') {
          submitCreatedAnnotation();
          return;
        }
        if (action.id === 'cancel') {
          annotationSelection = null;
          annotationComment = '';
          desktopOverlayId = null;
          return;
        }
      }
      desktopOverlayId = null;
      if (action.id === 'auto') {
        useAutomaticViewport();
        return;
      }
      const preset = VIEWPORT_DEVICE_MODES.find((mode) => mode.id === action.id);
      if (preset) {
        useFixedViewport(preset.width, preset.height);
        return;
      }
      const annotation = savedAnnotations.find((item) => item.annotationId === action.id);
      if (annotation) selectSavedAnnotation(annotation);
    });
    const unsubscribeOverlayState = desktop?.onOverlayState((state) => {
      if (state.ownerId !== `browser:${tabId}`) desktopOverlayId = null;
    });
    const unsubscribeOverlayClosed = desktop?.onOverlayClosed(() => {
      desktopOverlayId = null;
      annotationSelection = null;
      annotationComment = '';
    });
    const unsubscribeDesktopSnapshot = desktop?.onSnapshot((next) => {
      applyDesktopViewport(next);
      if (
        next.layout.activePanelKind === 'browser'
        && next.layout.rightPaneVisible
        && next.layout.activeTabId === tabId
      ) {
        scheduleBrowserSlotBounds();
      }
    });
    const unsubscribeBrowserEvent = window.magiDesktop?.onBrowserEvent(handleDesktopBrowserEvent);
    window.addEventListener('pointerdown', pointerDown);
    window.addEventListener('keydown', keyboard);
    window.addEventListener(BROWSER_AUTHORITY_CHANGED_EVENT, browserAuthorityChanged);
    return () => {
      componentDisposed = true;
      unsubscribeBrowserEvent?.();
      unsubscribeOverlayAction?.();
      unsubscribeOverlayState?.();
      unsubscribeOverlayClosed?.();
      unsubscribeDesktopSnapshot?.();
      window.removeEventListener('pointerdown', pointerDown);
      window.removeEventListener('keydown', keyboard);
      window.removeEventListener('resize', scheduleBrowserSlotBounds);
      window.visualViewport?.removeEventListener('resize', scheduleBrowserSlotBounds);
      if (slotPublishTimer !== null) window.clearTimeout(slotPublishTimer);
      slotPublishTimer = null;
      if (desktop) {
        const ownedSlotTabId = slotOwnerTabId || publishedSlotTabId;
        if (ownedSlotTabId) clearPublishedBrowserSlot(ownedSlotTabId);
        else invalidateBrowserSlotPublication();
        if (desktopOverlayId) void desktop.closeOverlay().catch(() => undefined);
      }
      window.removeEventListener(BROWSER_AUTHORITY_CHANGED_EVENT, browserAuthorityChanged);
      if (customViewportTimer !== null) window.clearTimeout(customViewportTimer);
      customViewportTimer = null;
      pendingViewport = null;
    };
  });

  onMount(() => onDesktopBlockingOverlayChange((visible) => {
    if (!desktopRuntime) return;
    browserSurfaceHiddenByOverlay = visible;
  }));
</script>

<section
  class="browser-pane"
  aria-label={i18n.t('browser.pane.label')}
>
  <div class="browser-toolbar">
    {#if desktopRuntime}
      <button type="button" class="icon-button flip" onclick={() => navigate('back')} disabled={!browserReady || busy} title={i18n.t('browser.navigation.back')} aria-label={i18n.t('browser.navigation.back')}><Icon name="chevron-right" size={13} /></button>
      <button type="button" class="icon-button" onclick={() => navigate('forward')} disabled={!browserReady || busy} title={i18n.t('browser.navigation.forward')} aria-label={i18n.t('browser.navigation.forward')}><Icon name="chevron-right" size={13} /></button>
      <button type="button" class="icon-button" onclick={() => navigate('reload')} disabled={!browserReady || busy} title={i18n.t('browser.navigation.reload')} aria-label={i18n.t('browser.navigation.reload')}><Icon name="refresh" size={13} /></button>
      <form class="address-form" onsubmit={(event) => { event.preventDefault(); navigate('url'); }}>
        <input
          bind:value={address}
          aria-label={i18n.t('browser.navigation.address')}
          spellcheck="false"
          disabled={!browserReady || busy}
          onfocus={() => { addressEditing = true; }}
          onblur={() => {
            addressEditing = false;
            if (activeTab?.url) address = activeTab.url;
          }}
          onkeydown={(event) => { if (event.key !== 'Enter' || event.isComposing) return; event.preventDefault(); navigate('url'); }}
        />
        <button type="submit" class="address-submit" disabled={!browserReady || busy} title={i18n.t('browser.navigation.go')} aria-label={i18n.t('browser.navigation.go')}><Icon name="chevron-right" size={12} /></button>
      </form>
      <div class="menu-wrap" bind:this={viewportMenuElement}>
        <button bind:this={viewportMenuButton} type="button" class="icon-button" class:active={localViewportMode === 'fixed'} onclick={toggleViewportMenu} disabled={!browserReady || busy} title={i18n.t('browser.viewport.control')} aria-label={i18n.t('browser.viewport.control')}><Icon name="monitor" size={13} /></button>
        {#if viewportMenuOpen && !desktopRuntime}
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
                <input type="number" min="320" max="7680" value={customViewportWidth} oninput={(event) => { customViewportWidth = Number((event.currentTarget as HTMLInputElement).value); scheduleCustomViewportUpdate(); }} />
              </label>
              <label>
                <span>{i18n.t('browser.viewport.height')}</span>
                <input type="number" min="240" max="4320" value={customViewportHeight} oninput={(event) => { customViewportHeight = Number((event.currentTarget as HTMLInputElement).value); scheduleCustomViewportUpdate(); }} />
              </label>
            </div>
          </div>
        {/if}
      </div>
    {:else}
      <div class="record-address" title={activeTab?.url || i18n.t('browser.status.noTab')}>
        <Icon name="globe" size={13} />
        <span>{activeTab?.title || activeTab?.url || i18n.t('browser.status.noTab')}</span>
      </div>
    {/if}
    <button type="button" class="icon-button" onclick={openCurrentPageExternally} disabled={!externalUrl || busy} title={i18n.t('browser.action.openExternal')} aria-label={i18n.t('browser.action.openExternal')}><Icon name="external-link" size={13} /></button>
    {#if desktopRuntime}
      <button type="button" class="icon-button" onclick={captureScreenshotForMessage} disabled={!browserReady || busy} title={i18n.t('browser.action.screenshot')} aria-label={i18n.t('browser.action.screenshot')}><Icon name="file-plus" size={13} /></button>
    {/if}
    <div class="menu-wrap" bind:this={annotationMenuElement}>
      {#if desktopRuntime}
        <button type="button" class="icon-button" onclick={openDesktopAnnotationCreation} disabled={!browserReady || busy || Boolean(desktopOverlayId)} title={i18n.t('browser.action.annotate')} aria-label={i18n.t('browser.action.annotate')}><Icon name="target" size={13} /></button>
      {/if}
      <button bind:this={annotationHistoryButton} type="button" class="icon-button annotation-history-button" class:active={annotationMenuOpen} onclick={toggleAnnotationMenu} disabled={savedAnnotations.length === 0} title={i18n.t('browser.annotation.history')} aria-label={i18n.t('browser.annotation.history')}><Icon name="target" size={13} />{#if savedAnnotations.length}<span class="annotation-count">{savedAnnotations.length}</span>{/if}</button>
      {#if annotationMenuOpen && !desktopRuntime}
        <div class="annotation-menu" role="menu">
          {#each savedAnnotations as annotation (annotation.annotationId)}
            <button type="button" onclick={() => selectSavedAnnotation(annotation)} title={annotation.comment}><span class="annotation-menu-number">{annotation.sequence}</span><span>{annotation.comment}</span></button>
          {/each}
        </div>
      {/if}
    </div>
    {#if desktopRuntime}
      <span class="status-light" class:ready={connectionState === 'ready' && !browserLoading} class:loading={connectionState === 'ready' && browserLoading} class:error={connectionState === 'error'} title={connectionStatusText} role="status"></span>
    {:else}
      <span class="record-status" role="status">{activeTab ? i18n.t('browser.status.recordOnly') : i18n.t('browser.status.noTab')}</span>
    {/if}
  </div>

  <div bind:this={browserSurfaceSlot} class="browser-surface-slot" aria-label={i18n.t('browser.viewport.label')}>
    {#if desktopRuntime && !browserReady}
      <div class="browser-placeholder" class:error={connectionState === 'error'} aria-live="polite">{connectionStatusText}</div>
    {:else if desktopRuntime && Boolean(error)}
      <div class="browser-placeholder error" role="alert">{connectionStatusText}</div>
    {:else if desktopRuntime && browserReady}
      <div class="browser-native-surface" aria-hidden="true"></div>
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
  .browser-toolbar { z-index: 2; box-sizing: border-box; display: flex; align-items: center; width: 100%; min-width: 0; height: 36px; min-height: 36px; gap: 3px; padding: 4px 6px; border-bottom: 1px solid var(--border); flex-shrink: 0; overflow: hidden; }
  .icon-button { width: 27px; height: 27px; display: inline-flex; align-items: center; justify-content: center; flex-shrink: 0; padding: 0; border: 0; border-radius: var(--radius-sm); background: transparent; color: var(--foreground-muted); cursor: pointer; }
  .icon-button:hover:not(:disabled) { background: var(--surface-2); color: var(--foreground); }
  .icon-button.active { color: var(--primary); background: var(--surface-2); }
  .icon-button:disabled { opacity: .45; cursor: default; }
  .flip { transform: scaleX(-1); }
  .address-form { display: flex; flex: 1 1 0; min-width: 0; }
  .address-form input { box-sizing: border-box; width: 100%; min-width: 0; height: 27px; padding: 0 8px; border: 1px solid var(--border); border-right: 0; border-radius: var(--radius-sm) 0 0 var(--radius-sm); background: var(--surface-1); color: var(--foreground); font: inherit; }
  .address-submit { display: grid; place-items: center; width: 27px; height: 27px; flex: 0 0 27px; padding: 0; border: 1px solid var(--border); border-radius: 0 var(--radius-sm) var(--radius-sm) 0; background: var(--surface-1); color: var(--foreground-muted); cursor: pointer; }
  .record-address { display: flex; align-items: center; gap: 7px; flex: 1; min-width: 0; height: 27px; padding: 0 8px; border: 1px solid var(--border); border-radius: var(--radius-sm); background: var(--surface-1); color: var(--foreground-muted); font-size: var(--text-xs); }
  .record-address span { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .menu-wrap { position: relative; display: flex; flex: 0 0 auto; }
  .viewport-menu, .annotation-menu { position: absolute; z-index: 10; top: calc(100% + 5px); right: 0; width: min(300px, calc(100vw - 24px)); max-height: 300px; overflow: auto; padding: 5px; border: 1px solid var(--border); border-radius: 6px; background: var(--dropdown-bg); box-shadow: var(--shadow-lg); }
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
  .browser-surface-slot { position: relative; display: flex; flex: 1; min-width: 0; min-height: 0; overflow: hidden; background: var(--surface-1); }
  .browser-native-surface { flex: 1 1 auto; width: 100%; min-width: 0; min-height: 0; background: transparent; }
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
