<script lang="ts">
  import { onMount, untrack } from 'svelte';
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
  import type { MessageBrowserNodeSelection } from '../../types/message';
  import { normalizeOptionalDomNodeId } from '@magi/desktop-browser-contracts';
  import { measureDesktopOverlayMenuBounds } from '../../lib/desktop-overlay-geometry';

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

  interface BrowserOverlayIdentity {
    overlayId: string;
    ownerId: string;
  }

  interface BrowserOverlayUiSnapshot {
    identity: BrowserOverlayIdentity | null;
    kind: MagiDesktopOverlayState['kind'] | null;
    annotationSelection: BrowserAnnotationSelection | null;
    annotationComment: string;
  }

  interface PendingBrowserOverlayClose {
    identity: BrowserOverlayIdentity;
    confirmation: Promise<void>;
    resolve: () => void;
    reject: (cause: unknown) => void;
    operation: Promise<void> | null;
  }

  interface DesktopMenuLayout {
    state: MagiDesktopOverlayState;
    anchor: HTMLElement;
    itemCount: number;
    fieldCount: number;
  }

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
    diagnostic?: string;
    node?: DesktopInspectedNode;
    payload?: Record<string, unknown>;
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
  // Browser Tab 的运行通道由右栏宿主显式决定。Desktop 使用 Main 进程创建的
  // WebContentsView；右栏 DOM 只负责保留内容槽，物理 Surface 的几何由
  // WindowManager 在同一个布局事务中计算。
  const desktopRuntime = $derived(desktopSurface);
  let desktopSnapshot = $state<MagiDesktopWindowSnapshot | null>(null);
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
  // 桌面端一次只允许一个原生弹层；标记编辑、标记记录和视口菜单都走同一
  // Overlay 身份事务。ownerId 不能从当前 tab 推导替代，因为组件切换 Tab
  // 后，旧关闭请求仍必须携带旧 owner 才能被主进程精确校验。
  let desktopOverlayIdentity = $state<BrowserOverlayIdentity | null>(null);
  let desktopOverlayKind = $state<MagiDesktopOverlayState['kind'] | null>(null);
  let pendingDesktopOverlayClose: PendingBrowserOverlayClose | null = null;
  let desktopOverlayClosing = $state(false);
  let desktopOverlayOperations = Promise.resolve();
  let overlayInstanceSequence = 0;
  let desktopMenuLayout = $state<DesktopMenuLayout | null>(null);
  let desktopMenuReflowFrame: number | null = null;
  let annotationSelection = $state<BrowserAnnotationSelection | null>(null);
  let annotationComment = $state('');
  let pageError = $state('');
  let nodeInspectActive = $state(false);
  let nodeInspectBusy = $state(false);
  let nodeSelection = $state<BrowserNodeSelectionContext | null>(null);
  let nodeInspectIdentity = $state<BrowserInspectIdentity | null>(null);
  let nodeInspectGeneration = 0;
  let refreshGeneration = 0;
  let desktopSurfaceSyncGeneration = 0;
  let activeBrowserIdentityKey = '';

  const activeTab = $derived.by<BrowserTabSnapshot | null>(() => (
    snapshot?.tabs.find((tab) => tab.tabId === tabId && tab.lifecycle !== 'closed') ?? null
  ));
  const savedAnnotations = $derived((activeTab?.annotations ?? []).filter((annotation) => annotation.status !== 'deleted'));
  const externalUrl = $derived(normalizeExternalWebUrl(activeTab?.url || address));
  const browserSurfaceAvailable = $derived(
    desktopRuntime
      && desktopSnapshot?.layout.rightPaneVisible === true
      && desktopSnapshot?.layout.activePanelKind === 'browser'
      && desktopSnapshot.layout.activeTabId === tabId
      && Boolean(desktopSnapshot?.layout.activeSurfaceId)
      // activeSurfaceId 只代表 Main 已物化 WebContents。只有同一 Tab 的
      // renderer_geometry 已确认真实内容槽后，原生 View 才进入可见状态；
      // 在这之前必须保留稳定占位，不能渲染透明占位元素造成黑屏窗口。
      && desktopSnapshot.layout.rendererGeometry?.layoutRevision === desktopSnapshot.layout.layoutRevision
      && desktopSnapshot.layout.rendererGeometry?.browserContentSlot?.tabId === tabId,
  );
  // activeSurfaceId 由 Main 在完成物理挂载和 bounds 更新后发布。它不是
  // Authority/Worker 握手状态，也不会因页面刷新或右栏拖动而重置。
  const browserReady = $derived(browserSurfaceAvailable);
  const activeBrowserIdentity = $derived.by<BrowserInspectIdentity | null>(() => {
    const tab = activeTab;
    const surfaceId = desktopSnapshot?.layout.activeSurfaceId;
    if (
      !desktopRuntime
      || !tab
      || !surfaceId
      || desktopSnapshot?.layout.rightPaneVisible !== true
      || desktopSnapshot.layout.activePanelKind !== 'browser'
      || desktopSnapshot.layout.activeTabId !== tabId
    ) return null;
    return {
      tabId,
      surfaceId,
      navigationRevision: tab.navigationRevision,
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

  function browserOverlayOwner(tab = tabId): string {
    return `browser:${tab}`;
  }

  function nextBrowserOverlayId(kind: MagiDesktopOverlayState['kind']): string {
    overlayInstanceSequence += 1;
    const token = typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function'
      ? crypto.randomUUID()
      : `${Date.now()}-${overlayInstanceSequence}`;
    return `browser-${kind}-${token}`;
  }

  function sameOverlayIdentity(
    left: BrowserOverlayIdentity | null,
    right: BrowserOverlayIdentity | null,
  ): boolean {
    return Boolean(
      left
      && right
      && left.overlayId === right.overlayId
      && left.ownerId === right.ownerId,
    );
  }

  function captureOverlayUi(): BrowserOverlayUiSnapshot {
    return {
      identity: desktopOverlayIdentity
        ? { ...desktopOverlayIdentity }
        : null,
      kind: desktopOverlayKind,
      annotationSelection,
      annotationComment,
    };
  }

  function clearDesktopOverlayUi(expected: BrowserOverlayIdentity | null = null): void {
    if (expected && !sameOverlayIdentity(desktopOverlayIdentity, expected)) return;
    desktopOverlayIdentity = null;
    desktopOverlayKind = null;
    desktopMenuLayout = null;
    annotationSelection = null;
    annotationComment = '';
  }

  function restoreOverlayUi(previous: BrowserOverlayUiSnapshot): void {
    desktopOverlayIdentity = previous.identity;
    desktopOverlayKind = previous.kind;
    annotationSelection = previous.annotationSelection;
    annotationComment = previous.annotationComment;
  }

  function enqueueDesktopOverlayOperation(operation: () => Promise<void>): Promise<void> {
    const next = desktopOverlayOperations.then(operation, operation);
    // 每个操作自己负责把失败反映到 actionError；队列不能因一次 IPC
    // 失败而拒绝后续的打开/关闭事务。
    desktopOverlayOperations = next.catch(() => undefined);
    return next;
  }

  function confirmDesktopOverlayClosed(event: MagiDesktopOverlayClosedEvent): void {
    const current = desktopOverlayIdentity;
    const pending = pendingDesktopOverlayClose;
    const currentMatches = sameOverlayIdentity(current, event);
    const pendingMatches = Boolean(pending && sameOverlayIdentity(pending.identity, event));
    if (!currentMatches && !pendingMatches) return;

    // replacement 事件确认的是旧身份的终结；即使 replacement 已经是
    // 当前本地期望，也不能用旧事件清空新 Overlay。
    if (currentMatches) clearDesktopOverlayUi(event);
    if (pendingMatches && pending) {
      pendingDesktopOverlayClose = null;
      desktopOverlayClosing = false;
      pending.resolve();
    }
  }

  function openDesktopOverlay(
    state: MagiDesktopOverlayState,
    configure: () => void,
  ): void {
    const desktop = window.magiDesktop;
    if (!desktop) return;
    const previousUi = captureOverlayUi();
    const nextIdentity: BrowserOverlayIdentity = {
      overlayId: state.overlayId,
      ownerId: state.ownerId,
    };
    const pendingClose = pendingDesktopOverlayClose;
    const waitsForClose = Boolean(pendingClose && !sameOverlayIdentity(pendingClose.identity, nextIdentity));
    configure();
    desktopOverlayIdentity = nextIdentity;
    desktopOverlayKind = state.kind;

    void enqueueDesktopOverlayOperation(async () => {
      let closeConfirmed = !waitsForClose;
      try {
        if (pendingClose && waitsForClose) {
          await pendingClose.confirmation;
          closeConfirmed = true;
        }
        await desktop.openOverlay(state);
      } catch (cause) {
        if (sameOverlayIdentity(desktopOverlayIdentity, nextIdentity)) {
          // 若新打开失败且旧 Overlay 尚未关闭，主进程仍保留旧状态，必须
          // 恢复其完整本地上下文；旧状态已确认关闭时只能清空，不能复活。
          if (waitsForClose && closeConfirmed) clearDesktopOverlayUi(nextIdentity);
          else restoreOverlayUi(previousUi);
          actionError = errorMessage(cause);
        }
        throw cause;
      }
    }).catch(() => undefined);
  }

  function scheduleDesktopMenuReflow(): void {
    if (desktopMenuReflowFrame !== null) return;
    desktopMenuReflowFrame = requestAnimationFrame(() => {
      desktopMenuReflowFrame = null;
      const desktop = window.magiDesktop;
      const layout = desktopMenuLayout;
      const identity = desktopOverlayIdentity;
      if (!desktop || !layout || !identity || desktopOverlayClosing || !sameOverlayIdentity(identity, layout.state)) return;
      const popupBounds = measureDesktopOverlayMenuBounds(layout.anchor, layout.itemCount, layout.fieldCount);
      if (!popupBounds) return;
      const previous = layout.state.popupBounds;
      if (
        previous
        && previous.x === popupBounds.x
        && previous.y === popupBounds.y
        && previous.width === popupBounds.width
        && previous.height === popupBounds.height
      ) return;
      const state = { ...layout.state, popupBounds };
      desktopMenuLayout = { ...layout, state };
      void enqueueDesktopOverlayOperation(async () => {
        await desktop.openOverlay(state);
      }).catch((cause) => {
        if (sameOverlayIdentity(desktopOverlayIdentity, state)) actionError = errorMessage(cause);
      });
    });
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
    // 节点选中后由 Main 的一次性 Inspect 事务负责清理 Chromium Overlay。
    // 这里仅清除 Renderer 的本地状态，不能再等待一个重复 stop 请求，
    // 否则节点采集完成后的慢 CDP 清理会把工具按钮锁成 disabled。
    if (preserveSelection || !identity || !desktop) return;
    // Main/Suface 侧按同一 Surface 的 inspect generation 做幂等清理。
    // 这里 fire-and-forget 是有意的：清理是旧生命周期的副作用，不能成为
    // 新生命周期的 UI 前置条件。
    void desktop.stopBrowserInspect(identity).catch(() => undefined);
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
    desktopSnapshot = next;
    if (next.layout.activeTabId !== tabId || !next.activeBrowserViewport) return;
    const viewport = next.activeBrowserViewport;
    localViewportMode = viewport.mode;
    if (viewport.mode === 'auto') return;
    localViewport = {
      width: viewport.width,
      height: viewport.height,
      deviceType: viewport.device_type,
    };
    if (!customViewportWidthEditing) customViewportWidthInput = String(viewport.width);
    if (!customViewportHeightEditing) customViewportHeightInput = String(viewport.height);
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

  function desktopViewportMenuState(popupBounds: MagiDesktopRectangle): MagiDesktopOverlayState {
    return {
      overlayId: nextBrowserOverlayId('menu'),
      kind: 'menu',
      phase: 'menu',
      ownerId: browserOverlayOwner(),
      placement: 'browser-viewport',
      popupBounds,
      title: i18n.t('browser.viewport.control'),
      items: [
        ...VIEWPORT_DEVICE_MODES.map((mode) => ({
          id: `viewport:${mode.id}`,
          label: `${i18n.t(`browser.viewport.mode.${mode.id}`)} ${mode.width} x ${mode.height}`,
          icon: mode.id === 'narrow' ? 'smartphone' : 'monitor',
          selected: fixedPresetSelected(mode),
          disabled: false,
        })),
        {
          id: 'viewport:auto',
          label: i18n.t('browser.viewport.auto'),
          icon: 'maximize',
          selected: localViewportMode === 'auto',
          disabled: false,
        },
      ],
      fields: [
        {
          id: 'viewport-width',
          label: i18n.t('browser.viewport.width'),
          type: 'number',
          value: customViewportWidthInput,
          min: VIEWPORT_DIMENSION_LIMITS.width.min,
          max: VIEWPORT_DIMENSION_LIMITS.width.max,
        },
        {
          id: 'viewport-height',
          label: i18n.t('browser.viewport.height'),
          type: 'number',
          value: customViewportHeightInput,
          min: VIEWPORT_DIMENSION_LIMITS.height.min,
          max: VIEWPORT_DIMENSION_LIMITS.height.max,
        },
      ],
    };
  }

  function openDesktopViewportMenu(): void {
    const desktop = window.magiDesktop;
    if (!desktop || !activeTab || !browserReady || desktopOverlayIdentity || desktopOverlayClosing) return;
    const anchor = viewportMenuButton;
    if (!anchor) return;
    const popupBounds = measureDesktopOverlayMenuBounds(anchor, 3, 2);
    if (!popupBounds) return;
    const state = desktopViewportMenuState(popupBounds);
    openDesktopOverlay(state, () => {
      desktopMenuLayout = {
        state,
        anchor,
        itemCount: 3,
        fieldCount: 2,
      };
      viewportMenuOpen = false;
      annotationMenuOpen = false;
    });
  }

  function openDesktopAnnotationHistory(): void {
    const desktop = window.magiDesktop;
    if (!desktop || !activeTab || !browserReady || desktopOverlayIdentity || desktopOverlayClosing) return;
    const anchor = annotationHistoryButton;
    if (!anchor) return;
    const popupBounds = measureDesktopOverlayMenuBounds(anchor, savedAnnotations.length, 0);
    if (!popupBounds) return;
    const state: MagiDesktopOverlayState = {
      overlayId: nextBrowserOverlayId('menu'),
      kind: 'menu',
      phase: 'menu',
      ownerId: browserOverlayOwner(),
      placement: 'browser-annotations',
      popupBounds,
      title: i18n.t('browser.annotation.history'),
      items: savedAnnotations.map((annotation) => ({
        id: `annotation:${annotation.annotationId}`,
        label: annotation.comment,
        icon: null,
        selected: false,
        disabled: false,
      })),
      fields: [],
    };
    openDesktopOverlay(state, () => {
      desktopMenuLayout = {
        state,
        anchor,
        itemCount: savedAnnotations.length,
        fieldCount: 0,
      };
      viewportMenuOpen = false;
      annotationMenuOpen = false;
    });
  }

  function toggleViewportMenu(): void {
    if (!activeTab || busy || desktopOverlayClosing) return;
    if (desktopRuntime) {
      if (desktopOverlayIdentity) {
        void closeDesktopOverlay().catch(() => undefined);
        return;
      }
      annotationMenuOpen = false;
      openDesktopViewportMenu();
      return;
    }
    annotationMenuOpen = false;
    viewportMenuOpen = !viewportMenuOpen;
  }

  function handleDesktopViewportInput(fieldId: string, value: string): void {
    if (fieldId === 'viewport-width') {
      customViewportWidthEditing = true;
      customViewportWidthInput = value;
    } else if (fieldId === 'viewport-height') {
      customViewportHeightEditing = true;
      customViewportHeightInput = value;
    } else return;
    customViewportInputDirty = true;
    ++viewportMutationGeneration;
    scheduleCustomViewportUpdate();
  }

  function handleDesktopMenuAction(action: MagiDesktopOverlayAction): void {
    if (action.kind !== 'menu') return;
    if (action.interaction === 'input') {
      handleDesktopViewportInput(action.id, action.value ?? '');
      return;
    }
    if (action.id === 'viewport:auto') {
      void closeDesktopOverlay().then(() => useAutomaticViewport()).catch(() => undefined);
      return;
    }
    if (action.id.startsWith('viewport:')) {
      const mode = VIEWPORT_DEVICE_MODES.find((item) => `viewport:${item.id}` === action.id);
      if (!mode) return;
      void closeDesktopOverlay().then(() => useFixedViewport(mode.width, mode.height)).catch(() => undefined);
      return;
    }
    if (action.id.startsWith('annotation:')) {
      const annotation = savedAnnotations.find((item) => `annotation:${item.annotationId}` === action.id);
      if (!annotation) return;
      annotationMenuOpen = false;
      void closeDesktopOverlay().then(() => {
        window.dispatchEvent(new CustomEvent('magi:browserAnnotationCreated', { detail: annotation }));
      }).catch(() => undefined);
    }
  }

  function openDesktopAnnotationCreation(): void {
    const desktop = window.magiDesktop;
    if (!desktop || !activeTab || !browserReady || desktopOverlayIdentity || desktopOverlayClosing) return;
    const overlayId = nextBrowserOverlayId('annotation');
    openDesktopOverlay({
      overlayId,
      kind: 'annotation',
      phase: 'select',
      ownerId: browserOverlayOwner(),
      placement: 'browser-annotations',
      popupBounds: null,
      title: i18n.t('browser.annotation.title'),
      items: [],
      fields: [],
    }, () => {
      desktopMenuLayout = null;
      annotationSelection = null;
      annotationComment = '';
      viewportMenuOpen = false;
      annotationMenuOpen = false;
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
    const identity = desktopOverlayIdentity;
    if (!desktop || !identity) return;
    openDesktopOverlay({
      overlayId: identity.overlayId,
      kind: 'annotation',
      phase: 'comment',
      ownerId: identity.ownerId,
      placement: 'browser-annotations',
      popupBounds: null,
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
    }, () => {
      desktopMenuLayout = null;
      // 同一身份的 select -> comment 更新失败时保留主进程的旧状态，
      // 不伪造已关闭事件；错误由 openDesktopOverlay 统一展示。
    });
  }

  function closeDesktopOverlay(
    expected: BrowserOverlayIdentity | null = desktopOverlayIdentity,
    options: { silent?: boolean } = {},
  ): Promise<void> {
    const desktop = window.magiDesktop;
    if (!expected || !desktop) return Promise.resolve();
    const existing = pendingDesktopOverlayClose;
    if (existing && sameOverlayIdentity(existing.identity, expected)) {
      return existing.operation ?? existing.confirmation;
    }

    let resolveConfirmation!: () => void;
    let rejectConfirmation!: (cause: unknown) => void;
    const confirmation = new Promise<void>((resolve, reject) => {
      resolveConfirmation = resolve;
      rejectConfirmation = reject;
    });
    const pending: PendingBrowserOverlayClose = {
      identity: { ...expected },
      confirmation,
      resolve: resolveConfirmation,
      reject: rejectConfirmation,
      operation: null,
    };
    pendingDesktopOverlayClose = pending;
    desktopOverlayClosing = true;

    const operation = enqueueDesktopOverlayOperation(async () => {
      try {
        const response = await desktop.closeOverlay(pending.identity);
        // 主进程以 overlay-closed 事件作为跨 Renderer 的提交确认；若桥接
        // 层直接返回同一事件，也可作为同一确认，不重复等待广播。
        if (response && sameOverlayIdentity(response, pending.identity)) {
          confirmDesktopOverlayClosed(response);
        } else if (response === null) {
          throw new Error('desktop_overlay_close_not_confirmed');
        }
        await pending.confirmation;
      } catch (cause) {
        if (pendingDesktopOverlayClose === pending) {
          pendingDesktopOverlayClose = null;
          desktopOverlayClosing = false;
          pending.reject(cause);
        }
        if (!options.silent && sameOverlayIdentity(desktopOverlayIdentity, pending.identity)) {
          actionError = errorMessage(cause);
        }
        throw cause;
      } finally {
        if (pendingDesktopOverlayClose === pending && !desktopOverlayClosing) {
          pendingDesktopOverlayClose = null;
        }
      }
    });
    pending.operation = operation;
    // 调用方可以选择等待确认；组件生命周期清理等场景不应产生未处理
    // rejection，但也不能把失败伪装成已关闭。
    operation.catch(() => undefined);
    return operation;
  }

  function submitCreatedAnnotation(): void {
    const tab = activeTab;
    const selection = annotationSelection;
    const comment = annotationComment.trim();
    const identity = desktopOverlayIdentity;
    if (!tab || !selection || !comment || !identity) return;
    void run(async () => {
      const created = await createBrowserAnnotation(tab.tabId, selection, comment);
      await refreshSession(true);
      window.dispatchEvent(new CustomEvent('magi:browserAnnotationCreated', { detail: created }));
      annotationSelection = null;
      annotationComment = '';
      await closeDesktopOverlay(identity);
    });
  }

  function toggleAnnotationMenu(): void {
    if (!activeTab || busy || desktopOverlayClosing) return;
    if (desktopRuntime) {
      if (desktopOverlayIdentity) {
        void closeDesktopOverlay().catch(() => undefined);
        return;
      }
      viewportMenuOpen = false;
      openDesktopAnnotationHistory();
      return;
    }
    if (desktopOverlayIdentity) {
      void closeDesktopOverlay().then(() => {
        annotationSelection = null;
        annotationComment = '';
        viewportMenuOpen = false;
        annotationMenuOpen = true;
      }).catch(() => undefined);
      return;
    }
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

  function handleDesktopBrowserEvent(value: unknown): void {
    if (!value || typeof value !== 'object' || Array.isArray(value)) return;
    const event = value as DesktopBrowserEvent;
    const binding = browserEventBinding(event);
    const currentIdentity = activeBrowserIdentity;
    const tab = activeTab;
    if (!binding || binding.tabId !== tabId) return;

    if (
      event.type === 'primary_changed'
      || event.type === 'primary_surface_changed'
      || event.type === 'control_revoked'
      || event.type === 'page_crashed'
    ) {
      clearNodeInspection();
      if (event.type === 'page_crashed') {
        pageError = event.reason?.trim() || event.diagnostic?.trim() || i18n.t('browser.error.pageLoadFailed');
        browserLoading = false;
      }
      return;
    }

    if (!currentIdentity || !tab || binding.surfaceId !== currentIdentity.surfaceId) return;

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
      // 后续普通网页点击继续被 Overlay 拦截；已选上下文仍保留给消息层。
      clearNodeInspection(true);
      return;
    }

    if (binding.navigationRevision !== tab.navigationRevision) {
      if (binding.navigationRevision > tab.navigationRevision) clearNodeInspection();
      return;
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
        pageError = '';
        clearNodeInspection();
      }
      return;
    }
    if (event.type === 'page_failed') {
      clearNodeInspection();
      pageError = event.reason?.trim() || i18n.t('browser.error.pageLoadFailed');
      browserLoading = false;
      return;
    }
    if (event.type !== 'page_updated' || !event.page) return;
    pageError = '';
    if (event.page.url?.trim()) {
      if (!addressEditing) address = event.page.url;
    }
    if (event.page.title?.trim()) onTitleChange?.(event.page.title);
  }

  $effect(() => {
    if (!desktopRuntime) return;
    void synchronizeDesktopSurface();
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
    const expectedSessionId = browserSessionId.trim();
    const expectedTabId = tabId.trim();
    const identityKey = `${expectedSessionId}\u0000${expectedTabId}`;
    if (identityKey === activeBrowserIdentityKey) return;
    const previousOverlay = desktopOverlayIdentity;
    activeBrowserIdentityKey = identityKey;
    untrack(() => {
      // BrowserTabContent 可能因 RightPane 切换而复用实例。先以旧身份
      // 提交关闭，再清理本地可见状态；新 Tab 的任何迟到 state/close
      // 事件都必须经过新的 overlayId + ownerId 校验，不能复用旧状态。
      if (previousOverlay) {
        void closeDesktopOverlay(previousOverlay, { silent: true }).catch(() => undefined);
        clearDesktopOverlayUi(previousOverlay);
      }
      clearNodeInspection();
      cancelPendingCustomViewport();
      ++viewportMutationGeneration;
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
      loading = Boolean(expectedSessionId && expectedTabId);
      if (!expectedSessionId || !expectedTabId) return;
      void refreshSession(true);
    });
  });

  onMount(() => {
    const desktop = window.magiDesktop;
    if (desktop) void synchronizeDesktopSurface();
    const menuPane = viewportMenuButton?.closest<HTMLElement>('.right-pane') ?? null;
    const menuGeometryObserver = typeof ResizeObserver === 'undefined' || !menuPane
      ? null
      : new ResizeObserver(() => scheduleDesktopMenuReflow());
    if (menuPane) menuGeometryObserver?.observe(menuPane);
    const windowResize = () => scheduleDesktopMenuReflow();
    const pointerDown = (event: PointerEvent) => {
      const target = event.target;
      if (!(target instanceof Node)) return;
      if (
        viewportMenuElement
        && !viewportMenuElement.contains(target)
        && !viewportMenuButton?.contains(target)
      ) viewportMenuOpen = false;
      if (
        annotationMenuElement
        && !annotationMenuElement.contains(target)
        && !annotationHistoryButton?.contains(target)
      ) annotationMenuOpen = false;
    };
    const keyboard = (event: KeyboardEvent) => {
      if (event.key !== 'Escape') return;
      const hasOpenOverlay = Boolean(desktopOverlayIdentity || desktopOverlayClosing || viewportMenuOpen || annotationMenuOpen);
      if (!hasOpenOverlay) return;
      event.preventDefault();
      event.stopPropagation();
      viewportMenuOpen = false;
      annotationMenuOpen = false;
      if (desktopOverlayIdentity) {
        void closeDesktopOverlay();
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
        desktopOverlayClosing
        || !desktopOverlayIdentity
        || action.overlayId !== desktopOverlayIdentity.overlayId
        || action.ownerId !== desktopOverlayIdentity.ownerId
      ) return;
      if (action.kind === 'menu') {
        handleDesktopMenuAction(action);
        return;
      }
      if (action.interaction === 'input') {
        if (action.kind === 'annotation' && action.id === 'comment') {
          annotationComment = action.value ?? '';
          return;
        }
        return;
      }
      if (action.kind === 'annotation') {
        if (action.id === 'selection') {
          const selection = parseAnnotationSelection(action.value);
          if (!selection) {
            actionError = i18n.t('browser.annotation.pageChanged');
            void closeDesktopOverlay();
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
          void closeDesktopOverlay();
          return;
        }
      }
    });
    const unsubscribeOverlayState = desktop?.onOverlayState((state) => {
      // 只接受当前组件明确发起的身份。不能因为 ownerId 相同就接收
      // 旧 session/旧实例的迟到 state，否则会把本地状态重新复活。
      if (
        !desktopOverlayIdentity
        || state.overlayId !== desktopOverlayIdentity.overlayId
        || state.ownerId !== desktopOverlayIdentity.ownerId
      ) return;
      desktopOverlayIdentity = {
        overlayId: state.overlayId,
        ownerId: state.ownerId,
      };
      desktopOverlayKind = state.kind;
    });
    const unsubscribeOverlayClosed = desktop?.onOverlayClosed((event) => {
      // replacement/closed 都必须先匹配完整身份。旧 owner 的迟到关闭
      // 不能清除已替换的新 Overlay；匹配 pending close 时才算提交确认。
      confirmDesktopOverlayClosed(event);
    });
    const unsubscribeDesktopSnapshot = desktop?.onSnapshot((next) => {
      applyDesktopViewport(next);
    });
    const unsubscribeBrowserEvent = window.magiDesktop?.onBrowserEvent(handleDesktopBrowserEvent);
    window.addEventListener('pointerdown', pointerDown);
    window.addEventListener('keydown', keyboard, true);
    window.addEventListener('resize', windowResize);
    window.addEventListener(BROWSER_AUTHORITY_CHANGED_EVENT, browserAuthorityChanged);
    return () => {
      const overlayAtUnmount = desktopOverlayIdentity;
      if (desktop && overlayAtUnmount) {
        void closeDesktopOverlay(overlayAtUnmount, { silent: true }).catch(() => undefined);
      }
      unsubscribeBrowserEvent?.();
      unsubscribeOverlayAction?.();
      unsubscribeOverlayState?.();
      unsubscribeOverlayClosed?.();
      unsubscribeDesktopSnapshot?.();
      window.removeEventListener('pointerdown', pointerDown);
      window.removeEventListener('keydown', keyboard, true);
      window.removeEventListener('resize', windowResize);
      menuGeometryObserver?.disconnect();
      if (desktopMenuReflowFrame !== null) {
        cancelAnimationFrame(desktopMenuReflowFrame);
        desktopMenuReflowFrame = null;
      }
      clearNodeInspection();
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
  <div class="browser-toolbar">
    {#if desktopRuntime}
      <button type="button" class="icon-button flip" onclick={() => navigate('back')} disabled={!browserReady || busy} data-tooltip={i18n.t('browser.navigation.back')} aria-label={i18n.t('browser.navigation.back')}><Icon name="chevron-right" size={13} /></button>
      <button type="button" class="icon-button" onclick={() => navigate('forward')} disabled={!browserReady || busy} data-tooltip={i18n.t('browser.navigation.forward')} aria-label={i18n.t('browser.navigation.forward')}><Icon name="chevron-right" size={13} /></button>
      <button type="button" class="icon-button" onclick={() => navigate('reload')} disabled={!browserReady || busy} data-tooltip={i18n.t('browser.navigation.reload')} aria-label={i18n.t('browser.navigation.reload')}><Icon name="refresh" size={13} /></button>
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
      <button bind:this={viewportMenuButton} type="button" class="icon-button" class:active={localViewportMode === 'fixed'} onclick={toggleViewportMenu} disabled={!browserReady || busy || desktopOverlayClosing} data-tooltip={i18n.t('browser.viewport.control')} aria-label={i18n.t('browser.viewport.control')}><Icon name="monitor" size={13} /></button>
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
        <button type="button" class="icon-button toolbar-edge-button" onclick={openDesktopAnnotationCreation} disabled={!browserReady || busy || Boolean(desktopOverlayIdentity) || desktopOverlayClosing} data-tooltip={i18n.t('browser.action.annotate')} aria-label={i18n.t('browser.action.annotate')}><Icon name="target" size={13} /></button>
      {/if}
      {#if savedAnnotations.length > 0}
        <button bind:this={annotationHistoryButton} type="button" class="icon-button annotation-history-button toolbar-edge-button" class:active={annotationMenuOpen} onclick={toggleAnnotationMenu} data-tooltip={i18n.t('browser.annotation.history')} aria-label={i18n.t('browser.annotation.history')} aria-expanded={annotationMenuOpen}><Icon name="list" size={13} /><span class="annotation-count">{savedAnnotations.length}</span></button>
      {/if}
    </div>
    {#if desktopRuntime}
      <span class="status-light" class:ready={connectionState === 'ready' && !browserLoading} class:loading={connectionState === 'ready' && browserLoading} class:error={connectionState === 'error'} title={connectionStatusText} role="status"></span>
    {:else}
      <span class="record-status" role="status">{activeTab ? i18n.t('browser.status.recordOnly') : i18n.t('browser.status.noTab')}</span>
    {/if}
  </div>

  {#if viewportMenuOpen && !desktopRuntime}
    <div bind:this={viewportMenuElement} class="viewport-popover" data-menu="viewport">
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
              type="number"
              min={VIEWPORT_DIMENSION_LIMITS.width.min}
              max={VIEWPORT_DIMENSION_LIMITS.width.max}
              value={customViewportWidthInput}
              onfocus={() => { customViewportWidthEditing = true; }}
              oninput={(event) => handleCustomViewportInput('width', event)}
              onblur={handleCustomViewportBlur}
            />
          </label>
          <label>
            <span>{i18n.t('browser.viewport.height')}</span>
            <input
              type="number"
              min={VIEWPORT_DIMENSION_LIMITS.height.min}
              max={VIEWPORT_DIMENSION_LIMITS.height.max}
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

  {#if annotationMenuOpen && !desktopRuntime}
    <div bind:this={annotationMenuElement} class="annotation-history-popover" data-menu="annotations">
      <div class="annotation-menu" role="menu" aria-label={i18n.t('browser.annotation.history')}>
        {#each savedAnnotations as annotation (annotation.annotationId)}
          <button type="button" onclick={() => selectSavedAnnotation(annotation)} title={annotation.comment}><span class="annotation-menu-number">{annotation.sequence}</span><span>{annotation.comment}</span></button>
        {/each}
      </div>
    </div>
  {/if}

  <div
    class="browser-surface-slot"
    data-browser-tab-id={tabId}
    aria-label={i18n.t('browser.viewport.label')}
  >
    {#if desktopRuntime && !browserReady}
      <div class="browser-placeholder" class:error={connectionState === 'error'} aria-live="polite">{connectionStatusText}</div>
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
  /* Main 的 browserContentBounds 使用的是内容槽外框高度。显式采用
     border-box，确保 padding 和底边框不会把工具栏撑高后侵入 Chromium
     Surface，避免拖动/刷新时出现工具栏被页面覆盖。 */
  .browser-toolbar { position: relative; z-index: 2; box-sizing: border-box; display: flex; align-items: center; width: 100%; min-width: 0; height: 36px; min-height: 36px; gap: 3px; padding: 4px 6px; border-bottom: 1px solid var(--border); flex-shrink: 0; overflow: visible; }
  .icon-button, .address-submit { position: relative; }
  .icon-button { width: 27px; height: 27px; display: inline-flex; align-items: center; justify-content: center; flex-shrink: 0; padding: 0; border: 0; border-radius: var(--radius-sm); background: transparent; color: var(--foreground-muted); cursor: pointer; }
  .icon-button:hover:not(:disabled) { background: var(--surface-2); color: var(--foreground); }
  .icon-button.active { color: var(--primary); background: var(--surface-2); }
  .icon-button:disabled { opacity: .45; cursor: default; }
  .flip :global(svg) { transform: scaleX(-1); }
  /* 原生 title 提示在 Electron 的分层视图中不稳定：提示层可能落到
     Browser Surface 后面。把说明绘制在工具栏上方，始终留在 App Renderer
     的可见区域内，同时保留 aria-label 供键盘和辅助技术使用。 */
  .icon-button[data-tooltip]::after,
  .address-submit[data-tooltip]::after {
    content: attr(data-tooltip);
    position: absolute;
    z-index: var(--z-tooltip, 1200);
    right: auto;
    bottom: calc(100% - 1px);
    left: 50%;
    max-width: min(240px, calc(100vw - 16px));
    padding: 4px 7px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--glass-bg, var(--dropdown-bg));
    box-shadow: var(--shadow-sm);
    color: var(--foreground);
    font-size: var(--text-xs);
    font-weight: var(--font-medium, 500);
    line-height: 1.25;
    white-space: nowrap;
    pointer-events: none;
    opacity: 0;
    visibility: hidden;
    transform: translate(-50%, 3px);
    transition: opacity var(--transition-fast), visibility var(--transition-fast), transform var(--transition-fast);
  }
  .toolbar-edge-button[data-tooltip]::after {
    right: 0;
    left: auto;
    transform: translate(0, 3px);
  }
  .icon-button[data-tooltip]:hover:not(:disabled)::after,
  .address-submit[data-tooltip]:hover:not(:disabled)::after,
  .icon-button[data-tooltip]:focus-visible::after,
  .address-submit[data-tooltip]:focus-visible::after {
    opacity: 1;
    visibility: visible;
    transform: translate(-50%, 0);
  }
  .toolbar-edge-button[data-tooltip]:hover:not(:disabled)::after,
  .toolbar-edge-button[data-tooltip]:focus-visible::after {
    transform: translate(0, 0);
  }
  .icon-button[data-tooltip]:disabled::after,
  .address-submit[data-tooltip]:disabled::after { display: none; }
  .address-form { display: flex; flex: 1 1 0; min-width: 0; }
  .address-form input { box-sizing: border-box; width: 100%; min-width: 0; height: 27px; padding: 0 8px; border: 1px solid var(--border); border-right: 0; border-radius: var(--radius-sm) 0 0 var(--radius-sm); background: var(--surface-1); color: var(--foreground); font: inherit; }
  .address-submit { display: grid; place-items: center; width: 27px; height: 27px; flex: 0 0 27px; padding: 0; border: 1px solid var(--border); border-radius: 0 var(--radius-sm) var(--radius-sm) 0; background: var(--surface-1); color: var(--foreground-muted); cursor: pointer; }
  .record-address { display: flex; align-items: center; gap: 7px; flex: 1; min-width: 0; height: 27px; padding: 0 8px; border: 1px solid var(--border); border-radius: var(--radius-sm); background: var(--surface-1); color: var(--foreground-muted); font-size: var(--text-xs); }
  .record-address span { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .menu-wrap { position: relative; display: flex; flex: 0 0 auto; }
  /* 顶部工具菜单是浏览器框架上的浮层，不属于页面内容的排版轨道。
     桌面端由同一位置的原生 Overlay 承载；非桌面端使用此绝对定位版本，
     两条路径都不会打开菜单时改变内容槽高度。 */
  .viewport-popover,
  .annotation-history-popover { position: absolute; top: 40px; right: 6px; z-index: 4; box-sizing: border-box; width: min(300px, calc(100% - 12px)); pointer-events: auto; }
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
