<script lang="ts">
  import { onMount, tick } from 'svelte';
  import Icon from '../components/Icon.svelte';
  import MarkdownContent from '../components/MarkdownContent.svelte';
  import DiffCodeBlock from '../components/blocks/DiffCodeBlock.svelte';
  import AgentTabContent from '../components/tabs/AgentTabContent.svelte';
  import BrowserTabContent from '../components/tabs/BrowserTabContent.svelte';
  import TerminalTabContent from '../components/tabs/TerminalTabContent.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { highlightCode } from '../lib/code-highlighter';
  import {
    OPEN_URL_IN_BROWSER_EVENT,
    type OpenUrlInBrowserRequest,
  } from '../lib/browser-navigation';
  import { normalizeExternalWebUrl, openExternalWebUrl } from '../lib/external-link';
  import { addToast } from '../stores/messages.svelte';
  import { navigateSession, waitForSessionNavigation } from '../shared/session-navigation.svelte';
  import {
    isHtmlFile,
    isKnownBinaryFile,
    isMarkdownFile,
    isWordFile,
    isImageFile,
  } from '../lib/file-preview-utils';
  import {
    rightPaneState,
    getRightPaneState,
    closeTab,
    setActiveRightPaneTab,
    updateRightPaneTabLabel,
    setRightPaneCollapsed,
    clearPendingBrowserTabIntent,
    type RightPaneTab,
    type CodeTabPayload,
    type AgentTabPayload,
    type BrowserTabPayload,
    type TerminalTabPayload,
    openBrowserTab,
    synchronizeBrowserSessionSnapshot,
    openTerminalTab,
  } from '../stores/right-pane.svelte';
  import {
    activateBrowserTab,
    closeBrowserTab,
    closeTerminalSession,
    createBrowserSession,
    createBrowserTab,
    waitForBrowserTabReady,
    getBrowserCapabilities,
    materializeSession,
    getAgentChangeDiff,
    getAgentFilePreview,
    agentUrl,
    buildFilePreviewQuery,
    isPublicTunnelAccess,
    type BrowserCapabilitiesSnapshot,
  } from './agent-api';

  interface Props {
    workspaceRoot: string;
    overlay?: boolean;
    desktopSurface?: boolean;
  }

  let {
    workspaceRoot,
    overlay = false,
    desktopSurface = false,
  }: Props = $props();

  // ============ Tab 状态 ============
  const paneScopeKey = $derived(rightPaneState.activeScopeKey);
  const paneState = $derived(getRightPaneState(paneScopeKey));
  const openTabs = $derived(paneState.openTabs);
  let tabStripElement: HTMLDivElement | undefined;

  $effect(() => {
    const activeTabId = paneState.activeTabId;
    void openTabs.length;
    if (!activeTabId || !tabStripElement) return;
    void tick().then(() => {
      const strip = tabStripElement;
      const activeElement = Array.from(strip?.children ?? []).find((element) => (
        element instanceof HTMLElement && element.dataset.tabId === activeTabId
      ));
      if (!strip || !(activeElement instanceof HTMLElement)) return;
      const left = activeElement.offsetLeft;
      const right = left + activeElement.offsetWidth;
      if (left < strip.scrollLeft) {
        strip.scrollLeft = left;
      } else if (right > strip.scrollLeft + strip.clientWidth) {
        strip.scrollLeft = right - strip.clientWidth;
      }
    });
  });

  function closePane(): void {
    setRightPaneCollapsed(paneScopeKey, true);
  }
  let creatingBrowserPane = $state(false);
  let browserCapabilities = $state<BrowserCapabilitiesSnapshot | null>(null);
  let addPaneMenuOpen = $state(false);
  let addPaneMenuElement = $state<HTMLDivElement | undefined>(undefined);
  let addPaneButtonElement = $state<HTMLButtonElement | undefined>(undefined);
  const canCreateBrowserPane = $derived(Boolean(
    desktopSurface
      && window.magiDesktop
      && browserCapabilities?.platformCapabilities.desktopBrowserSurface === true
      && browserCapabilities?.inAppBrowserEnabled
  ));
  const canCreateTerminalPane = $derived(Boolean(
    !isPublicTunnelAccess()
      && rightPaneState.activeSessionId.trim(),
  ));

  function applyBrowserCapabilities(snapshot: BrowserCapabilitiesSnapshot): void {
    browserCapabilities = snapshot;
  }

  onMount(() => {
    void getBrowserCapabilities()
      .then(applyBrowserCapabilities)
      .catch((error) => console.warn('[RightPane] 获取浏览器能力失败:', error));
    const handleCapabilitiesChanged = (event: Event) => {
      applyBrowserCapabilities((event as CustomEvent<BrowserCapabilitiesSnapshot>).detail);
    };
    window.addEventListener('magi:browserCapabilitiesChanged', handleCapabilitiesChanged);
    const handleOutsidePointer = (event: PointerEvent) => {
      const target = event.target;
      if (addPaneMenuElement && target instanceof Node && !addPaneMenuElement.contains(target)) {
        addPaneMenuOpen = false;
        if (desktopSurface) void desktop?.closeOverlay();
      }
    };
    const handleEscape = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        addPaneMenuOpen = false;
        if (desktopSurface) void desktop?.closeOverlay();
      }
    };
    const desktop = desktopSurface ? window.magiDesktop : undefined;
    const handleOverlayState = (state: MagiDesktopOverlayState) => {
      if (!desktopSurface) return;
      addPaneMenuOpen = state.ownerId === 'right-pane'
        && state.placement === 'right-pane-add';
    };
    const handleOverlayAction = (action: MagiDesktopOverlayAction) => {
      if (
        action.kind !== 'menu'
        || action.ownerId !== 'right-pane'
        || action.interaction !== 'select'
      ) return;
      addPaneMenuOpen = false;
      if (action.id === 'browser' || action.id === 'terminal') {
        chooseAddPane(action.id);
      }
    };
    const unsubscribeOverlayState = desktop?.onOverlayState(handleOverlayState);
    const unsubscribeOverlayAction = desktop?.onOverlayAction(handleOverlayAction);
    const unsubscribeOverlayClosed = desktop?.onOverlayClosed(() => {
      addPaneMenuOpen = false;
    });
    const handleOpenUrlInBrowser = (event: Event) => {
      const request = (event as CustomEvent<OpenUrlInBrowserRequest>).detail;
      if (!request?.url) return;
      if (desktopSurface) {
        void createBrowserPane(request.url);
        return;
      }
      // Web 和手机 Web 没有物理 BrowserSurface。链接仍需保持可用，
      // 直接交给系统浏览器，避免点击后只得到一条无法继续操作的提示。
      const externalUrl = normalizeExternalWebUrl(request.url);
      if (!externalUrl) return;
      void openExternalWebUrl(externalUrl).catch(() => {
        addToast('error', i18n.t('browser.error.openExternal'), undefined, { forceVisible: true });
      });
    };
    window.addEventListener('pointerdown', handleOutsidePointer);
    window.addEventListener('keydown', handleEscape);
    window.addEventListener(OPEN_URL_IN_BROWSER_EVENT, handleOpenUrlInBrowser);
    return () => {
      window.removeEventListener('magi:browserCapabilitiesChanged', handleCapabilitiesChanged);
      window.removeEventListener('pointerdown', handleOutsidePointer);
      window.removeEventListener('keydown', handleEscape);
      window.removeEventListener(OPEN_URL_IN_BROWSER_EVENT, handleOpenUrlInBrowser);
      unsubscribeOverlayState?.();
      unsubscribeOverlayAction?.();
      unsubscribeOverlayClosed?.();
    };
  });

  async function closeBrowserPanelResource(tabId: string): Promise<void> {
    try {
      await closeBrowserTab(tabId);
    } catch (error) {
      console.warn('[RightPane] 关闭浏览器面板资源失败:', error);
    }
  }

  async function closeTerminalPanelResources(payload: TerminalTabPayload): Promise<void> {
    try {
      await closeTerminalSession({
        terminalTabId: payload.terminalTabId,
        workspaceId: payload.workspaceId,
        workspacePath: payload.workspacePath,
        sessionId: payload.sessionId,
      });
    } catch (error) {
      console.warn('[RightPane] 关闭终端面板资源失败:', error);
    }
  }

  function registerBrowserPane(
    browserSessionId: string,
    tabId: string,
    workspaceId: string,
    sessionId: string,
    workspacePath?: string,
    lifecycle?: BrowserTabPayload['lifecycle'],
  ): void {
    openBrowserTab(browserSessionId, tabId, {
      workspaceId,
      workspacePath,
      sessionId,
      label: i18n.t('browser.tab.new'),
      lifecycle,
    });
    // 创建响应和 Authority 事件可能以任意顺序到达。无论响应已经是 ready
    // 还是仍处于 creating，都必须走一次同一条权威收敛链路并显式 reveal
    // 新 Tab，否则后台事件只会把 Tab 插入列表，当前选中项仍停留在旧 Tab。
    // waitForBrowserTabReady 对 ready 结果会立即返回，因此这里不会额外阻塞
    // 已经完成的创建。
    reconcileCreatedBrowserPane(
      browserSessionId,
      tabId,
      workspaceId,
      sessionId,
      workspacePath,
    );
  }

  function reconcileCreatedBrowserPane(
    browserSessionId: string,
    tabId: string,
    workspaceId: string,
    sessionId: string,
    workspacePath?: string,
  ): void {
    void waitForBrowserTabReady(browserSessionId, tabId)
      .then((snapshot) => {
        synchronizeBrowserSessionSnapshot(snapshot, workspacePath, {
          workspaceId,
          sessionId,
          revealTabId: tabId,
          newTabLabel: i18n.t('browser.tab.new'),
        });
      })
      .catch((error) => {
        // 超时或权威请求失败不能继续保留 creating，否则 UI 会永久显示“正在连接”。
        // crashed 是 Authority 已定义的明确失败态；后续用户再次激活时仍可通过
        // activateBrowserTab 走恢复流程，不会丢失逻辑 Tab。
        openBrowserTab(browserSessionId, tabId, {
          workspaceId,
          workspacePath,
          sessionId,
          label: i18n.t('browser.tab.new'),
          lifecycle: 'crashed',
        });
        console.warn('[RightPane] 浏览器 Tab 权威状态收敛失败，已进入失败态:', {
          browserSessionId,
          tabId,
          workspaceId,
          sessionId,
          error,
        });
      });
  }

  async function createBrowserPane(initialUrl = 'about:blank'): Promise<void> {
    if (creatingBrowserPane) return;
    if (!canCreateBrowserPane) {
      const message = desktopSurface
        ? i18n.t('browser.error.internalUnavailable')
        : i18n.t('browser.error.desktopRequired');
      addToast('warning', message, undefined, { forceVisible: true });
      return;
    }
    const targetUrl = initialUrl === 'about:blank'
      ? initialUrl
      : normalizeExternalWebUrl(initialUrl);
    if (!targetUrl) return;
    const workspaceId = rightPaneState.activeWorkspaceId.trim();
    creatingBrowserPane = true;
    try {
      let sessionId = rightPaneState.activeSessionId.trim();
      if (!sessionId) {
        const materialized = await materializeSession(
          workspaceId || null,
          workspaceId ? workspaceRoot : undefined,
        );
        sessionId = materialized.sessionId;
        const navigation = navigateSession(workspaceId
          ? {
              kind: 'session',
              scope: 'workspace',
              workspaceId,
              workspacePath: workspaceRoot,
              sessionId,
            }
          : { kind: 'session', scope: 'personal', sessionId });
        if (!navigation) {
          throw new Error('当前已有会话导航操作正在进行');
        }
        // 主视图导航与浏览器资源创建并行推进。后端会话已经由
        // materializeSession 建立，右栏不应等待另一个 Renderer 的 URL
        // 收敛，否则浏览器新增操作会被无期限阻塞。
        void waitForSessionNavigation(navigation).catch((error) => {
          console.warn('[RightPane] 浏览器会话导航收敛失败:', error);
        });
      }
      const browserSession = await createBrowserSession(workspaceId, sessionId, workspaceRoot);
      const tab = await createBrowserTab(
        browserSession.browserSessionId,
        targetUrl,
      );
      registerBrowserPane(
        browserSession.browserSessionId,
        tab.tabId,
        workspaceId,
        sessionId,
        workspaceRoot,
        tab.lifecycle,
      );
    } catch (error) {
      console.warn('[RightPane] 新建浏览器面板失败:', error);
      addToast('error', i18n.t('browser.error.openInternal'), undefined, { forceVisible: true });
    } finally {
      creatingBrowserPane = false;
    }
  }

  function createTerminalPane(): void {
    if (!canCreateTerminalPane) return;
    openTerminalTab({
      workspaceId: rightPaneState.activeWorkspaceId,
      workspacePath: workspaceRoot,
      sessionId: rightPaneState.activeSessionId,
    });
  }

  type RightPaneCreationKind = 'browser' | 'terminal';
  const addablePaneKinds = $derived([
    {
      kind: 'terminal' as const,
      label: i18n.t('rightPane.addPanelTerminal'),
      icon: 'terminal' as const,
      enabled: canCreateTerminalPane,
    },
    ...(canCreateBrowserPane ? [{
      kind: 'browser' as const,
      label: i18n.t('rightPane.addPanelBrowser'),
      icon: 'globe' as const,
      enabled: true,
    }] : []),
  ]);
  const canOpenAddPaneMenu = $derived(addablePaneKinds.some((item) => item.enabled));

  function toggleAddPaneMenu(): void {
    // 浏览器 Surface 创建是异步的，但新增面板选择器属于右栏通用能力。
    // 浏览器正在连接时，代码、图片、终端和其他面板仍必须可以打开。
    if (!canOpenAddPaneMenu) return;
    if (!desktopSurface || !window.magiDesktop) {
      addPaneMenuOpen = !addPaneMenuOpen;
      return;
    }
    if (addPaneMenuOpen) {
      addPaneMenuOpen = false;
      void window.magiDesktop.closeOverlay();
      return;
    }
    const anchor = addPaneButtonElement?.getBoundingClientRect();
    if (!anchor || anchor.width <= 0 || anchor.height <= 0) return;
    addPaneMenuOpen = true;
    void window.magiDesktop.openOverlay({
      overlayId: 'right-pane-add',
      kind: 'menu',
      phase: 'menu',
      ownerId: 'right-pane',
      placement: 'right-pane-add',
      anchorBounds: {
        x: anchor.left,
        y: anchor.top,
        width: anchor.width,
        height: anchor.height,
      },
      title: i18n.t('rightPane.addPanel'),
      items: addablePaneKinds.map((item) => ({
        id: item.kind,
        label: item.label,
        icon: item.icon,
        selected: false,
        disabled: !item.enabled,
      })),
      fields: [],
    }).catch((error) => {
      addPaneMenuOpen = false;
      console.warn('[RightPane] 打开新增面板菜单失败:', error);
    });
  }

  function chooseAddPane(kind: RightPaneCreationKind): void {
    addPaneMenuOpen = false;
    if (kind === 'browser') {
      void createBrowserPane();
      return;
    }
    createTerminalPane();
  }
  const activeTab = $derived.by<RightPaneTab | null>(() => {
    // Tab 条与内容区必须从同一份根状态解析当前 Tab。不要透过 paneState/openTabs
    // 的派生引用再做二次缓存，否则同一轮内新增 Tab 并激活时可能出现 Tab 条已更新、
    // 内容区仍读取旧 activeTab 的撕裂状态。
    const scopeKey = rightPaneState.activeScopeKey;
    const state = scopeKey ? rightPaneState.perSession[scopeKey] : undefined;
    const activeTabId = state?.activeTabId;
    return activeTabId
      ? state?.openTabs.find((tab) => tab.id === activeTabId) ?? null
      : null;
  });

  // 右侧一级 Tab 是用户当前查看页面的唯一选择源。Desktop Renderer 只向 Main
  // 提交逻辑激活意图；原生 WebContentsView 由 Main 进程持有，Renderer 只
  // 提供当前浏览器 Tab 的内容槽位。
  let activeBrowserActivationKey = '';
  let activeBrowserActivationRequest = 0;
  $effect(() => {
    const current = activeTab;
    if (!current || current.kind !== 'browser') {
      if (activeBrowserActivationKey) activeBrowserActivationRequest += 1;
      activeBrowserActivationKey = '';
      return;
    }
    const payload = current.payload as BrowserTabPayload;
    const activationIdentity = `${payload.browserSessionId}\u0000${payload.tabId}`;
    const activationKey = `${activationIdentity}\u0000${payload.lifecycle}`;
    if (activationKey === activeBrowserActivationKey) return;
    if (payload.lifecycle === 'creating') {
      // 创建请求只注册逻辑 Tab，必须等待 waitForBrowserTabReady 的权威
      // 快照把状态推进到 ready 后再激活。清空 key 也会取消同一 Tab
      // 上一次尚未完成的激活，避免旧请求重新抢占当前内容槽。
      activeBrowserActivationRequest += 1;
      activeBrowserActivationKey = '';
      return;
    }
    activeBrowserActivationKey = activationKey;
    const request = ++activeBrowserActivationRequest;
    void activateBrowserTab(payload.tabId)
      .then(async (authority) => {
        if (request !== activeBrowserActivationRequest || !desktopSurface) return;
        const desktop = window.magiDesktop;
        if (!desktop) throw new Error('desktop_preload_bridge_unavailable');
        const tab = authority.tabs.find((candidate) => candidate.tabId === payload.tabId);
        if (!tab) throw new Error(`browser_tab_not_found:${payload.tabId}`);
        // activateBrowserTab 已经完成 Host RestorePage 并将恢复的 suspended/
        // crashed Tab 收敛为 ready。接口返回的快照是这次激活的权威结果，
        // 必须在等待 Main 原生 Surface 布局前先回写右栏，否则
        // BrowserTabContent 仍会以旧 lifecycle 阻止内容槽发布，表现为
        // “正在连接”，直到用户新建另一个 Tab 才被全量同步掩盖。
        activeBrowserActivationKey = `${activationIdentity}\u0000${tab.lifecycle}`;
        synchronizeBrowserSessionSnapshot(authority, payload.workspacePath, {
          workspaceId: payload.workspaceId,
          sessionId: payload.sessionId,
        });
        await desktop.activateBrowser({
          tabId: tab.tabId,
          browserSessionId: authority.browserSessionId,
          url: tab.url,
          navigationRevision: tab.navigationRevision,
          viewport: { mode: 'auto' },
        });
      })
      .catch((error) => {
        if (request !== activeBrowserActivationRequest) return;
        clearPendingBrowserTabIntent(paneScopeKey, payload.tabId);
        activeBrowserActivationKey = '';
        console.warn('[RightPane] 激活浏览器面板失败:', error);
        addToast('error', i18n.t('browser.error.openInternal'), undefined, { forceVisible: true });
      });
  });

  let activeDesktopPanelKey = '';
  $effect(() => {
    if (!desktopSurface) return;
    const current = activeTab;
    if (current?.kind === 'browser') {
      activeDesktopPanelKey = '';
      return;
    }
    const key = current ? `${current.kind}:${current.id}` : 'empty';
    if (key === activeDesktopPanelKey) return;
    activeDesktopPanelKey = key;
    void window.magiDesktop?.activatePanel({
      kind: current?.kind ?? null,
      tabId: current?.id ?? null,
    }).catch((error) => {
      if (key !== activeDesktopPanelKey) return;
      activeDesktopPanelKey = '';
      console.warn('[RightPane] 激活桌面右栏面板失败:', error);
    });
  });

  // ============ Code tab：内容拉取 ============
  /** filepath → 异步加载的源码内容（仅用于补全 store 中没有 content 时） */
  let fetchedContents = $state<Record<string, string>>({});
  /** filepath → loading 标记 */
  let fetchingFlags = $state<Record<string, boolean>>({});
  /** filepath → 拉取出错时的错误信息 */
  let fetchErrors = $state<Record<string, string>>({});
  interface FetchedDiffDetail {
    diff: string;
    originalContent: string | null;
    currentContent: string | null;
  }
  /** filepath → 异步加载的完整变更详情（diff + 两侧全文）。 */
  let fetchedDiffDetails = $state<Record<string, FetchedDiffDetail>>({});
  /** filepath → diff loading 标记 */
  let fetchingDiffFlags = $state<Record<string, boolean>>({});
  /** filepath → diff 拉取出错时的错误信息 */
  let fetchDiffErrors = $state<Record<string, string>>({});
  /** 每个文档 Tab 独立保存预览/源码模式，避免切换 Tab 时相互污染。 */
  let documentModes = $state<Record<string, 'rendered' | 'raw'>>({});
  let previewRequestSeq = 0;
  const contentRequestSeqByKey = new Map<string, number>();
  const diffRequestSeqByKey = new Map<string, number>();

  function invalidatePreviewRequests(): void {
    contentRequestSeqByKey.clear();
    diffRequestSeqByKey.clear();
  }

  // 工作区内容变更（如切分支）后，清空已拉取的文件内容缓存，触发 $effect 按新分支重新拉取。
  onMount(() => {
    const handleWorkspaceContentChanged = () => {
      invalidatePreviewRequests();
      fetchedContents = {};
      fetchErrors = {};
      fetchingFlags = {};
      fetchedDiffDetails = {};
      fetchDiffErrors = {};
      fetchingDiffFlags = {};
    };
    window.addEventListener('magi:workspaceContentChanged', handleWorkspaceContentChanged);
    return () => window.removeEventListener('magi:workspaceContentChanged', handleWorkspaceContentChanged);
  });

  const activeCodePayload = $derived.by<CodeTabPayload | null>(() => {
    if (!activeTab || activeTab.kind !== 'code') return null;
    return activeTab.payload as CodeTabPayload;
  });

  const activeFilePath = $derived(activeCodePayload?.filepath ?? '');
  const activeDisplayFilePath = $derived(activeCodePayload?.displayPath ?? activeFilePath);
  function codePayloadCacheKey(payload: CodeTabPayload | null | undefined): string {
    if (!payload?.filepath) return '';
    return [
      payload.workspaceId ?? '',
      payload.workspacePath ?? '',
      payload.sessionId ?? '',
      payload.filepath,
      payload.isChangeDiff ? (payload.changeRevision ?? '') : '',
    ].join('::');
  }

  function pruneRecord<T>(record: Record<string, T>, retainedKeys: Set<string>): Record<string, T> {
    const entries = Object.entries(record).filter(([key]) => retainedKeys.has(key));
    return entries.length === Object.keys(record).length ? record : Object.fromEntries(entries);
  }

  const activeContentCacheKey = $derived.by(() => {
    return codePayloadCacheKey(activeCodePayload);
  });
  const activeContentKind = $derived(activeCodePayload?.contentKind ?? 'text');
  const explicitContent = $derived(activeCodePayload?.content ?? null);
  const explicitDiff = $derived(activeCodePayload?.diff ?? null);
  const hasExplicitDiffContents = $derived(Boolean(
    activeCodePayload
      && Object.prototype.hasOwnProperty.call(activeCodePayload, 'originalContent')
      && Object.prototype.hasOwnProperty.call(activeCodePayload, 'currentContent')
  ));
  const activeWantsDiff = $derived(Boolean(
    activeCodePayload?.isChangeDiff
      && (activeContentKind === 'text' || activeContentKind === 'large_text')
  ));
  const activeChangeWorkspaceBinding = $derived.by(() => {
    const workspaceId = activeCodePayload?.workspaceId?.trim() || '';
    const workspacePath = activeCodePayload?.workspacePath?.trim() || '';
    if (!workspaceId && !workspacePath) return null;
    const sessionId = activeCodePayload?.sessionId?.trim() || '';
    return {
      scope: 'workspace' as const,
      workspaceId,
      workspacePath,
      ...(sessionId ? { sessionId } : {}),
    };
  });
  const activeFilePreviewQuery = $derived.by(() => {
    if (!activeFilePath) return '';
    return buildFilePreviewQuery(activeFilePath, {
      sessionId: activeCodePayload?.sessionId,
      workspaceId: activeCodePayload?.workspaceId,
      workspacePath: activeCodePayload?.workspacePath,
    });
  });

  // 异步内容缓存跟随当前 Tab 集合裁剪，关闭预览后立即释放对应内容。
  $effect(() => {
    const retainedKeys = new Set<string>();
    for (const tab of openTabs) {
      if (tab.kind !== 'code') continue;
      const key = codePayloadCacheKey(tab.payload as CodeTabPayload);
      if (key) retainedKeys.add(key);
    }
    for (const key of contentRequestSeqByKey.keys()) {
      if (!retainedKeys.has(key)) contentRequestSeqByKey.delete(key);
    }
    for (const key of diffRequestSeqByKey.keys()) {
      if (!retainedKeys.has(key)) diffRequestSeqByKey.delete(key);
    }
    fetchedContents = pruneRecord(fetchedContents, retainedKeys);
    fetchErrors = pruneRecord(fetchErrors, retainedKeys);
    fetchingFlags = pruneRecord(fetchingFlags, retainedKeys);
    fetchedDiffDetails = pruneRecord(fetchedDiffDetails, retainedKeys);
    fetchDiffErrors = pruneRecord(fetchDiffErrors, retainedKeys);
    fetchingDiffFlags = pruneRecord(fetchingDiffFlags, retainedKeys);
    documentModes = pruneRecord(documentModes, retainedKeys);
  });

  /**
   * 是否需要异步拉取内容：text 类型、未带 content、未带 diff、且非二进制/word 文件。
   * 触发条件统一在 $effect 里检查，避免重复请求。
   */
  $effect(() => {
    const filepath = activeFilePath;
    const cacheKey = activeContentCacheKey;
    if (!filepath) return;
    if (!cacheKey) return;
    if (typeof explicitContent === 'string') return; // 已经有内容
    if (typeof explicitDiff === 'string' && explicitDiff.trim().length > 0) return; // diff 模式
    if (activeWantsDiff) return; // 变更 tab 缺 diff 时由 changes/diff 恢复，不退化成源码预览
    if (activeContentKind !== 'text') return; // 非文本类不拉取
    if (isWordFile(activeDisplayFilePath) || isKnownBinaryFile(activeDisplayFilePath)) return;
    if (typeof fetchedContents[cacheKey] === 'string') return; // 已成功拉过
    if (typeof fetchErrors[cacheKey] === 'string' && fetchErrors[cacheKey].length > 0) return; // 已失败过，停止重试避免死循环
    if (fetchingFlags[cacheKey]) return; // 拉取中

    const requestSeq = ++previewRequestSeq;
    contentRequestSeqByKey.set(cacheKey, requestSeq);
    fetchingFlags = { ...fetchingFlags, [cacheKey]: true };
    fetchErrors = { ...fetchErrors, [cacheKey]: '' };
    (async () => {
      try {
        const payload = await getAgentFilePreview(filepath, {
          sessionId: activeCodePayload?.sessionId,
          workspaceId: activeCodePayload?.workspaceId || '',
          workspacePath: activeCodePayload?.workspacePath || '',
        });
        if (contentRequestSeqByKey.get(cacheKey) !== requestSeq) return;
        fetchedContents = { ...fetchedContents, [cacheKey]: payload.content || '' };
      } catch (error) {
        if (contentRequestSeqByKey.get(cacheKey) !== requestSeq) return;
        console.warn('[RightPane] file preview load failed:', error);
        fetchErrors = { ...fetchErrors, [cacheKey]: i18n.t('web.filePreviewError') };
      } finally {
        if (contentRequestSeqByKey.get(cacheKey) === requestSeq) {
          contentRequestSeqByKey.delete(cacheKey);
          fetchingFlags = { ...fetchingFlags, [cacheKey]: false };
        }
      }
    })();
  });

  // 变更全文不持久化；缺少两侧全文时按权威接口补齐，以支持展开未变更区段。
  $effect(() => {
    const filepath = activeFilePath;
    const cacheKey = activeContentCacheKey;
    if (!filepath) return;
    if (!cacheKey) return;
    if (!activeWantsDiff) return;
    if (!activeChangeWorkspaceBinding) return;
    if (hasExplicitDiffContents) return;
    if (fetchedDiffDetails[cacheKey]) return;
    if (typeof fetchDiffErrors[cacheKey] === 'string' && fetchDiffErrors[cacheKey].length > 0) return;
    if (fetchingDiffFlags[cacheKey]) return;

    const requestSeq = ++previewRequestSeq;
    diffRequestSeqByKey.set(cacheKey, requestSeq);
    fetchingDiffFlags = { ...fetchingDiffFlags, [cacheKey]: true };
    fetchDiffErrors = { ...fetchDiffErrors, [cacheKey]: '' };
    (async () => {
      try {
        const payload = await getAgentChangeDiff(filepath, {
          ...activeChangeWorkspaceBinding,
        });
        if (diffRequestSeqByKey.get(cacheKey) !== requestSeq) return;
        fetchedDiffDetails = {
          ...fetchedDiffDetails,
          [cacheKey]: {
            diff: payload.diff || '',
            originalContent: typeof payload.originalContent === 'string' ? payload.originalContent : null,
            currentContent: typeof payload.currentContent === 'string' ? payload.currentContent : null,
          },
        };
      } catch (error) {
        if (diffRequestSeqByKey.get(cacheKey) !== requestSeq) return;
        console.warn('[RightPane] change diff load failed:', error);
        fetchDiffErrors = { ...fetchDiffErrors, [cacheKey]: i18n.t('web.filePreviewError') };
      } finally {
        if (diffRequestSeqByKey.get(cacheKey) === requestSeq) {
          diffRequestSeqByKey.delete(cacheKey);
          fetchingDiffFlags = { ...fetchingDiffFlags, [cacheKey]: false };
        }
      }
    })();
  });

  const previewLoading = $derived.by(() => {
    if (!activeContentCacheKey) return false;
    return Boolean(activeWantsDiff
      ? fetchingDiffFlags[activeContentCacheKey]
      : fetchingFlags[activeContentCacheKey]);
  });
  const previewError = $derived.by(() => {
    if (!activeContentCacheKey) return '';
    return activeWantsDiff
      ? (fetchDiffErrors[activeContentCacheKey] || '')
      : (fetchErrors[activeContentCacheKey] || '');
  });
  /** 最终用于渲染的内容：优先 store 显式 content，其次异步拉取结果 */
  const previewContent = $derived.by<string | null>(() => {
    if (typeof explicitContent === 'string') return explicitContent;
    if (!activeContentCacheKey) return null;
    return fetchedContents[activeContentCacheKey] ?? null;
  });

  // ============ 代码高亮 ============
  const EXT_LANG_MAP: Record<string, string> = {
    ts: 'typescript', tsx: 'typescript', js: 'javascript', jsx: 'javascript',
    py: 'python', rb: 'ruby', go: 'go', rs: 'rust', java: 'java',
    cpp: 'cpp', c: 'c', cs: 'csharp', kt: 'kotlin', swift: 'swift',
    html: 'xml', vue: 'xml', svelte: 'xml', xml: 'xml', svg: 'xml',
    css: 'css', scss: 'scss', less: 'less',
    json: 'json', yaml: 'yaml', yml: 'yaml', toml: 'ini',
    md: 'markdown', sh: 'bash', bash: 'bash', zsh: 'bash',
    sql: 'sql', graphql: 'graphql', dockerfile: 'dockerfile',
  };

  const fileLanguage = $derived.by(() => {
    if (!activeDisplayFilePath) return '';
    const ext = activeDisplayFilePath.split('.').pop()?.toLowerCase() ?? '';
    return EXT_LANG_MAP[ext] ?? '';
  });

  function escapeHtml(str: string): string {
    return str
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;')
      .replace(/'/g, '&#039;');
  }

  const diffCode = $derived.by(() => {
    if (typeof explicitDiff === 'string' && explicitDiff.trim().length > 0) {
      return explicitDiff.trimEnd();
    }
    if (activeWantsDiff && activeContentCacheKey && fetchedDiffDetails[activeContentCacheKey]) {
      return fetchedDiffDetails[activeContentCacheKey].diff.trimEnd();
    }
    return '';
  });
  const diffOriginalContent = $derived.by<string | null>(() => {
    if (hasExplicitDiffContents) return activeCodePayload?.originalContent ?? null;
    if (!activeContentCacheKey) return null;
    return fetchedDiffDetails[activeContentCacheKey]?.originalContent ?? null;
  });
  const diffCurrentContent = $derived.by<string | null>(() => {
    if (hasExplicitDiffContents) return activeCodePayload?.currentContent ?? null;
    if (!activeContentCacheKey) return null;
    return fetchedDiffDetails[activeContentCacheKey]?.currentContent ?? null;
  });
  const hasDiff = $derived(diffCode.trim().length > 0);

  // ============ 文件类型派生 ============
  const displayPath = $derived(getDisplayPath(activeDisplayFilePath, workspaceRoot));
  const markdownFile = $derived(isMarkdownFile(activeDisplayFilePath));
  const htmlFile = $derived(isHtmlFile(activeDisplayFilePath));
  const documentFile = $derived(markdownFile || htmlFile);
  const wordFile = $derived(isWordFile(activeDisplayFilePath));
  const imageFile = $derived(activeDisplayFilePath ? isImageFile(activeDisplayFilePath) : false);
  const imageSource = $derived(
    activeCodePayload?.imageDataUrl?.trim()
      || agentUrl('/api/files/raw', activeFilePreviewQuery),
  );
  // 图片虽属二进制，但走专门的 <img> 预览分支，故从 binaryFile（元信息兜底）排除。
  const binaryFile = $derived(
    !imageFile
      && (activeContentKind === 'binary' || (activeDisplayFilePath ? isKnownBinaryFile(activeDisplayFilePath) : false)),
  );
  const largeTextFile = $derived(activeContentKind === 'large_text');
  const symlinkFile = $derived(activeContentKind === 'symlink');
  const specialFile = $derived(activeContentKind === 'special');

  // ============ 图片缩放 / 平移 ============
  const IMAGE_ZOOM_MIN = 0.1;
  const IMAGE_ZOOM_MAX = 8;
  const IMAGE_ZOOM_STEP = 0.2;
  let imageZoom = $state(1);
  let imagePanX = $state(0);
  let imagePanY = $state(0);
  let imageDragging = $state(false);
  let imageDragStartX = 0;
  let imageDragStartY = 0;
  let imagePanStartX = 0;
  let imagePanStartY = 0;

  // 切换文件时重置缩放/平移，避免沿用上一张图的视图状态。
  $effect(() => {
    void activeFilePath;
    imageZoom = 1;
    imagePanX = 0;
    imagePanY = 0;
  });

  function clampZoom(value: number): number {
    return Math.min(IMAGE_ZOOM_MAX, Math.max(IMAGE_ZOOM_MIN, value));
  }

  function setImageZoom(next: number) {
    const clamped = clampZoom(next);
    if (clamped === 1) {
      imagePanX = 0;
      imagePanY = 0;
    }
    imageZoom = clamped;
  }

  function zoomImageIn() {
    setImageZoom(imageZoom + IMAGE_ZOOM_STEP);
  }

  function zoomImageOut() {
    setImageZoom(imageZoom - IMAGE_ZOOM_STEP);
  }

  function resetImageZoom() {
    imageZoom = 1;
    imagePanX = 0;
    imagePanY = 0;
  }

  function handleImageWheel(event: WheelEvent) {
    event.preventDefault();
    const factor = event.deltaY < 0 ? 1 + IMAGE_ZOOM_STEP : 1 / (1 + IMAGE_ZOOM_STEP);
    setImageZoom(imageZoom * factor);
  }

  function handleImagePointerDown(event: PointerEvent) {
    if (imageZoom <= 1) return;
    imageDragging = true;
    imageDragStartX = event.clientX;
    imageDragStartY = event.clientY;
    imagePanStartX = imagePanX;
    imagePanStartY = imagePanY;
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
  }

  function handleImagePointerMove(event: PointerEvent) {
    if (!imageDragging) return;
    imagePanX = imagePanStartX + (event.clientX - imageDragStartX);
    imagePanY = imagePanStartY + (event.clientY - imageDragStartY);
  }

  function handleImagePointerUp(event: PointerEvent) {
    if (!imageDragging) return;
    imageDragging = false;
    (event.currentTarget as HTMLElement).releasePointerCapture(event.pointerId);
  }

  // ============ Markdown 预览与源码切换 ============
  const documentMode = $derived.by<'rendered' | 'raw'>(() => {
    if (!activeContentCacheKey) return 'rendered';
    if (htmlFile) return 'raw';
    return documentModes[activeContentCacheKey] ?? 'rendered';
  });
  function setDocumentMode(mode: 'rendered' | 'raw'): void {
    if (!activeContentCacheKey) return;
    documentModes = { ...documentModes, [activeContentCacheKey]: mode };
  }

  let openingHtmlInBrowser = $state(false);
  async function openHtmlInMagiBrowser(): Promise<void> {
    if (!canCreateBrowserPane) {
      const message = desktopSurface
        ? i18n.t('browser.error.internalUnavailable')
        : i18n.t('browser.error.desktopRequired');
      addToast('warning', message, undefined, { forceVisible: true });
      return;
    }
    const workspaceId = activeCodePayload?.workspaceId?.trim() || '';
    const sessionId = activeCodePayload?.sessionId?.trim() || '';
    if (!htmlFile || !activeFilePreviewQuery || !workspaceId || !sessionId || openingHtmlInBrowser) return;
    openingHtmlInBrowser = true;
    try {
      const browserSession = await createBrowserSession(workspaceId, sessionId, workspaceRoot);
      const tab = await createBrowserTab(
        browserSession.browserSessionId,
        agentUrl('/api/files/site-open', activeFilePreviewQuery),
      );
      registerBrowserPane(
        browserSession.browserSessionId,
        tab.tabId,
        workspaceId,
        sessionId,
        activeCodePayload?.workspacePath,
      );
    } finally {
      openingHtmlInBrowser = false;
    }
  }
  const rawPreviewContent = $derived(previewContent ?? '');
  const truncatedContent = $derived(
    rawPreviewContent.length > 500_000 ? rawPreviewContent.slice(0, 100_000) : rawPreviewContent,
  );
  const isLargeFile = $derived(rawPreviewContent.length > 500_000);
  /**
   * source 视图行高亮：对整段内容做一次高亮（保持跨行 token），
   * 然后按 '\n' 切片，避免逐行高亮丢失多行 token 上下文。
   */
  let sourceLines = $state<string[]>([]);
  $effect(() => {
    const source = truncatedContent;
    const lang = fileLanguage;
    const lines = source.split('\n');
    sourceLines = lines.map(escapeHtml);
    if (!source || !lang) return;

    let cancelled = false;
    void highlightCode(source, lang).then((result) => {
      if (!cancelled && result !== null) sourceLines = result.split('\n');
    }).catch((error) => {
      console.warn('[RightPane] 源码高亮失败:', error);
    });
    return () => {
      cancelled = true;
    };
  });
  const hasContent = $derived(rawPreviewContent.length > 0);
  const codeMode = $derived(
    !previewLoading && !previewError && !wordFile && !binaryFile
      && !largeTextFile && !symlinkFile && !specialFile
      && (hasDiff || (hasContent && (!documentFile || documentMode === 'raw'))),
  );

  // ============ Tab 视觉 ============
  // 代理 tab 的 label / accentToken 由 ToolCall 触发 openAgentTab 时一次性写入；
  // RightPane 不再二次按 roleId 反查 registry —— tab 本身即为视觉真源。

  function tabAccent(tab: RightPaneTab): string {
    if (tab.kind === 'agent') {
      const accent = tab.accentToken?.trim();
      if (!accent) return 'var(--accent)';
      if (
        accent.startsWith('var(')
        || accent.startsWith('#')
        || accent.startsWith('rgb(')
        || accent.startsWith('rgba(')
        || accent.startsWith('hsl(')
        || accent.startsWith('hsla(')
      ) {
        return accent;
      }
      return `var(--${accent})`;
    }
    return 'var(--info)';
  }

  function tabLabel(tab: RightPaneTab): string {
    if (tab.kind === 'terminal') return i18n.t('terminalPanel.title');
    return tab.label;
  }

  function tabIcon(tab: RightPaneTab): 'file-text' | 'chevron-right' | 'globe' | 'terminal' {
    if (tab.kind === 'code') return 'file-text';
    if (tab.kind === 'browser') return 'globe';
    return tab.kind === 'terminal' ? 'terminal' : 'chevron-right';
  }

  function tabTooltip(tab: RightPaneTab): string {
    if (tab.kind === 'code') {
      const payload = tab.payload as CodeTabPayload;
      return payload.displayPath || payload.filepath;
    }
    if (tab.kind === 'browser') {
      const occupied = (tab.payload as BrowserTabPayload).agentOccupied;
      return `${tabLabel(tab)} · ${i18n.t(occupied ? 'browser.control.agentOccupied' : 'browser.control.released')}`;
    }
    if (tab.kind === 'terminal') {
      const payload = tab.payload as TerminalTabPayload;
      return payload.workspacePath || payload.workspaceId || (i18n.locale.startsWith('zh') ? 'Magi 空间' : 'Magi Space');
    }
    return tabLabel(tab);
  }

  // ============ 交互 ============
  /**
   * Tab 条 drag-to-pan 状态：
   * - 滚轮鼠标横向需求由 onwheel（deltaY → scrollLeft）解决；
   * - 触控板横滑由原生 deltaX 路径解决；
   * - 这里补的是「按住鼠标在 tab 条上拖动来平移」——VSCode / Chrome tab strip 的标准交互。
   */
  let dragState: { startX: number; startScrollLeft: number; moved: boolean } | null = null;
  let isDraggingTabs = $state(false);
  /** 真实拖拽刚结束的时间戳；用于吞掉紧随 pointerup 的 click 事件，避免拖拽结束误切换 tab */
  let dragEndedAt = 0;
  const DRAG_THRESHOLD = 4;
  const DRAG_CLICK_SUPPRESS_MS = 50;

  function recentlyDragged(): boolean {
    return performance.now() - dragEndedAt < DRAG_CLICK_SUPPRESS_MS;
  }

  function handleTabClick(tabId: string) {
    if (recentlyDragged()) return;
    setActiveRightPaneTab(paneScopeKey, tabId);
  }

  function handleTabClose(event: MouseEvent, tabId: string) {
    event.stopPropagation();
    if (recentlyDragged()) return;
    const tab = openTabs.find((item) => item.id === tabId);
    closeTab(paneScopeKey, tabId);
    if (tab?.kind === 'browser') {
      const browserTabId = (tab.payload as BrowserTabPayload).tabId;
      void closeBrowserPanelResource(browserTabId);
    } else if (tab?.kind === 'terminal') {
      void closeTerminalPanelResources(tab.payload as TerminalTabPayload);
    }
  }

  /**
   * 单行 tab 条只在水平方向溢出（overflow-x: auto），但标准鼠标滚轮只发出
   * 垂直方向的 deltaY，浏览器不会自动把它翻译成 scrollLeft——结果就是
   * 滚轮鼠标用户完全无法浏览溢出的 tab。这里把 deltaY 转成 scrollLeft，
   * 保留触控板原生 deltaX 走原路径（不重复消费）。
   */
  function handleTabsWheel(event: WheelEvent) {
    if (event.deltaX !== 0) return; // 触控板已经在水平方向输入 delta，不干预
    if (event.deltaY === 0) return;
    const target = event.currentTarget as HTMLDivElement;
    if (target.scrollWidth <= target.clientWidth) return; // 没有溢出就别拦
    target.scrollLeft += event.deltaY;
    event.preventDefault();
  }

  function handleTabsPointerDown(event: PointerEvent) {
    // 只对鼠标主键启用 drag-to-pan；触摸 / 笔 / 触控板交给原生路径
    if (event.pointerType !== 'mouse' || event.button !== 0) return;
    // 关闭按钮 (×) 不接管——保证用户点 × 关闭 tab 时不会进入拖拽
    const targetEl = event.target as HTMLElement | null;
    if (targetEl?.closest('.right-pane-tab-close')) return;
    const strip = event.currentTarget as HTMLDivElement;
    dragState = {
      startX: event.clientX,
      startScrollLeft: strip.scrollLeft,
      moved: false,
    };
  }

  function handleTabsPointerMove(event: PointerEvent) {
    if (!dragState) return;
    const dx = event.clientX - dragState.startX;
    if (!dragState.moved) {
      if (Math.abs(dx) < DRAG_THRESHOLD) return; // 未越过阈值仍按普通点击处理
      dragState.moved = true;
      isDraggingTabs = true;
      const strip = event.currentTarget as HTMLDivElement;
      strip.setPointerCapture(event.pointerId);
    }
    const strip = event.currentTarget as HTMLDivElement;
    strip.scrollLeft = dragState.startScrollLeft - dx;
    event.preventDefault();
  }

  function handleTabsPointerEnd(event: PointerEvent) {
    if (!dragState) return;
    const moved = dragState.moved;
    dragState = null;
    if (moved) {
      dragEndedAt = performance.now();
      isDraggingTabs = false;
      const strip = event.currentTarget as HTMLDivElement;
      if (strip.hasPointerCapture(event.pointerId)) {
        strip.releasePointerCapture(event.pointerId);
      }
    }
  }

  function formatSize(value?: number): string {
    if (typeof value !== 'number' || !Number.isFinite(value) || value < 0) return '-';
    if (value < 1024) return `${value} B`;
    if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KB`;
    if (value < 1024 * 1024 * 1024) return `${(value / (1024 * 1024)).toFixed(1)} MB`;
    return `${(value / (1024 * 1024 * 1024)).toFixed(1)} GB`;
  }

  function getDisplayPath(path: string, root: string): string {
    if (!path) return '';
    const normalizedPath = path.replace(/\\/g, '/');
    const normalizedRoot = root.replace(/\\/g, '/').replace(/\/+$/, '');
    if (normalizedRoot && normalizedPath.startsWith(`${normalizedRoot}/`)) {
      return normalizedPath.slice(normalizedRoot.length + 1);
    }
    return path;
  }
</script>

<aside class="right-pane" aria-label={i18n.t('rightPane.title')}>
  <!-- 顶部 Tab 条；右栏折叠入口由工作台外壳固定在窗口右上角。 -->
  <header class="right-pane-tabbar" class:right-pane-tabbar--overlay={overlay}>
    {#if overlay}
      <button
        type="button"
        class="right-pane-overlay-action"
        onclick={closePane}
        title={i18n.t('rightPane.backToConversation')}
        aria-label={i18n.t('rightPane.backToConversation')}
      >
        <Icon name="chevron-right" size={14} class="right-pane-back-icon" />
      </button>
    {/if}
    <div
      bind:this={tabStripElement}
      class="right-pane-tabs"
      class:dragging={isDraggingTabs}
      role="tablist"
      tabindex="-1"
      aria-label={i18n.t('rightPane.title')}
      onwheel={handleTabsWheel}
      onpointerdown={handleTabsPointerDown}
      onpointermove={handleTabsPointerMove}
      onpointerup={handleTabsPointerEnd}
      onpointercancel={handleTabsPointerEnd}
    >
      {#each openTabs as tab (tab.id)}
        {@const isActive = tab.id === paneState.activeTabId}
        {@const accent = tabAccent(tab)}
        <!-- svelte-ignore a11y_click_events_have_key_events -->
        <div
          class="right-pane-tab"
          class:active={isActive}
          role="tab"
          data-tab-id={tab.id}
          tabindex="0"
          aria-selected={isActive}
          style={`--tab-accent: ${accent};`}
          title={tabTooltip(tab)}
          onclick={() => handleTabClick(tab.id)}
          onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); handleTabClick(tab.id); } }}
        >
          <span class="right-pane-tab-icon" aria-hidden="true">
            <Icon name={tabIcon(tab)} size={12} />
          </span>
          {#if tab.kind === 'browser'}
            {@const agentOccupied = (tab.payload as BrowserTabPayload).agentOccupied}
            <span
              class="browser-control-status"
              class:occupied={agentOccupied}
              title={i18n.t(agentOccupied ? 'browser.control.agentOccupied' : 'browser.control.released')}
              aria-label={i18n.t(agentOccupied ? 'browser.control.agentOccupied' : 'browser.control.released')}
            ></span>
          {/if}
          <span class="right-pane-tab-label" class:mono={tab.kind === 'code'}>{tabLabel(tab)}</span>
          <button
            type="button"
            class="right-pane-tab-close"
            aria-label={i18n.t('rightPane.closeTab')}
            onclick={(event) => handleTabClose(event, tab.id)}
          >
            <Icon name="x" size={10} />
          </button>
        </div>
      {/each}
    </div>
    {#if canOpenAddPaneMenu}
      <div bind:this={addPaneMenuElement} class="right-pane-add-wrap">
        <button
          bind:this={addPaneButtonElement}
          type="button"
          class="right-pane-add-tab"
          data-open-tab-count={openTabs.length}
          onclick={toggleAddPaneMenu}
          disabled={!canOpenAddPaneMenu}
          title={i18n.t('rightPane.addPanel')}
          aria-label={i18n.t('rightPane.addPanel')}
          aria-haspopup="menu"
          aria-expanded={addPaneMenuOpen}
          aria-busy={creatingBrowserPane}
        >
          <Icon name={creatingBrowserPane ? 'loader' : 'plus'} size={14} />
        </button>
        {#if addPaneMenuOpen && !desktopSurface}
          <div class="right-pane-add-menu" role="menu" aria-label={i18n.t('rightPane.addPanel')}>
            {#each addablePaneKinds as item (item.kind)}
              <button
                type="button"
                class="right-pane-add-menu-item"
                role="menuitem"
                disabled={!item.enabled}
                onclick={() => chooseAddPane(item.kind)}
              >
                <Icon name={item.icon} size={14} />
                <span>{item.label}</span>
              </button>
            {/each}
          </div>
        {/if}
      </div>
    {/if}
  </header>

  <!-- 当前 code tab 的副标题：路径 + 文档预览操作 -->
  {#if activeTab && activeTab.kind === 'code'}
    <div class="right-pane-subbar">
      <div class="right-pane-path" title={activeDisplayFilePath}>{displayPath}</div>
      {#if documentFile && !hasDiff && !previewLoading && !previewError && !wordFile && !binaryFile && !largeTextFile && !symlinkFile && !specialFile && (htmlFile || hasContent)}
        <div class="right-pane-document-actions">
          {#if markdownFile}
            <div class="right-pane-document-modes" role="tablist" aria-label={i18n.t('web.filePreviewTitle')}>
              <button
                type="button"
                class="right-pane-document-mode"
                class:active={documentMode === 'rendered'}
                onclick={() => setDocumentMode('rendered')}
              >{i18n.t('web.filePreviewRendered')}</button>
              <button
                type="button"
                class="right-pane-document-mode"
                class:active={documentMode === 'raw'}
                onclick={() => setDocumentMode('raw')}
              >{i18n.t('web.filePreviewRaw')}</button>
            </div>
          {:else if htmlFile && canCreateBrowserPane}
            <button
              type="button"
              class="right-pane-document-icon-action"
              onclick={() => void openHtmlInMagiBrowser()}
              disabled={openingHtmlInBrowser}
              title={i18n.t('web.filePreviewOpenBrowser')}
              aria-label={i18n.t('web.filePreviewOpenBrowser')}
            ><Icon name="globe" size={13} /></button>
          {/if}
        </div>
      {/if}
    </div>
  {/if}

  <!-- Body：按 activeTab 路由 -->
  <div
    class="right-pane-body"
    class:right-pane-body--code={codeMode}
    class:right-pane-body--browser={activeTab?.kind === 'browser'}
    class:right-pane-body--terminal={activeTab?.kind === 'terminal'}
  >
    {#if !activeTab}
      <div class="right-pane-state">
        <Icon name="sidebar-toggle" size={22} />
        <span>{i18n.t('rightPane.empty.title')}</span>
        <span class="right-pane-meta-line">{i18n.t('rightPane.empty.hint')}</span>
      </div>
    {:else if activeTab.kind === 'agent'}
      {@const agentPayload = activeTab.payload as AgentTabPayload}
      <AgentTabContent
        agentRunId={agentPayload.agentRunId}
        workspaceId={agentPayload.workspaceId}
        workspacePath={agentPayload.workspacePath}
        sessionId={agentPayload.sessionId}
      />
    {:else if activeTab.kind === 'browser'}
      {@const browserPayload = activeTab.payload as BrowserTabPayload}
      <BrowserTabContent
        browserSessionId={browserPayload.browserSessionId}
        tabId={browserPayload.tabId}
        lifecycle={browserPayload.lifecycle}
        workspaceId={browserPayload.workspaceId}
        workspacePath={browserPayload.workspacePath}
        sessionId={browserPayload.sessionId}
        desktopSurface={desktopSurface}
        onTitleChange={(label) => updateRightPaneTabLabel(paneScopeKey, activeTab.id, label)}
      />
    {:else if activeTab.kind === 'terminal'}
      {@const terminalPayload = activeTab.payload as TerminalTabPayload}
      <TerminalTabContent
        terminalTabId={terminalPayload.terminalTabId}
        workspaceId={terminalPayload.workspaceId}
        workspacePath={terminalPayload.workspacePath}
        sessionId={terminalPayload.sessionId}
      />
    {:else if previewLoading}
      <div class="right-pane-state">{i18n.t('web.filePreviewLoading')}</div>
    {:else if previewError}
      <div class="right-pane-state right-pane-state--error">
        {previewError}
      </div>
    {:else if wordFile}
      <div class="right-pane-state">
        <Icon name="document" size={22} />
        <span>{i18n.t('web.filePreviewUnsupportedWord')}</span>
      </div>
    {:else if imageFile}
      <div class="right-pane-image-wrap">
        <div
          class="right-pane-image"
          class:dragging={imageDragging}
          class:zoomed={imageZoom > 1}
          role="img"
          aria-label={displayPath}
          onwheel={handleImageWheel}
          onpointerdown={handleImagePointerDown}
          onpointermove={handleImagePointerMove}
          onpointerup={handleImagePointerUp}
          onpointercancel={handleImagePointerUp}
        >
          <img
            class="right-pane-image-el"
            src={imageSource}
            alt={displayPath}
            draggable="false"
            style={`transform: translate(${imagePanX}px, ${imagePanY}px) scale(${imageZoom});`}
          />
        </div>
        <div class="right-pane-image-controls">
          <button class="image-zoom-btn" onclick={zoomImageOut} disabled={imageZoom <= IMAGE_ZOOM_MIN} title={i18n.t('web.imageZoomOut')} aria-label={i18n.t('web.imageZoomOut')}>
            <Icon name="minus" size={14} />
          </button>
          <button class="image-zoom-level" onclick={resetImageZoom} title={i18n.t('web.imageZoomReset')}>{Math.round(imageZoom * 100)}%</button>
          <button class="image-zoom-btn" onclick={zoomImageIn} disabled={imageZoom >= IMAGE_ZOOM_MAX} title={i18n.t('web.imageZoomIn')} aria-label={i18n.t('web.imageZoomIn')}>
            <Icon name="plus" size={14} />
          </button>
        </div>
      </div>
    {:else if binaryFile}
      <div class="right-pane-state right-pane-state--metadata">
        <Icon name="file" size={22} />
        <span>{i18n.t('web.filePreviewUnsupportedBinary')}</span>
        <span class="right-pane-meta-line">{i18n.t('edits.nonText.size')}: {formatSize(activeCodePayload?.size)}</span>
        {#if activeCodePayload?.mime}
          <span class="right-pane-meta-line">{i18n.t('edits.nonText.mime')}: {activeCodePayload.mime}</span>
        {/if}
      </div>
    {:else if largeTextFile}
      <div class="right-pane-large-text">
        <div class="right-pane-notice">{i18n.t('edits.nonText.largeTextTitle')} · {i18n.t('edits.nonText.size')}: {formatSize(activeCodePayload?.size)}</div>
        {#if activeCodePayload?.headSummary}
          <div class="right-pane-summary-section">
            <div class="right-pane-summary-title">{i18n.t('edits.nonText.head')}</div>
            <pre class="right-pane-summary-content">{activeCodePayload.headSummary}</pre>
          </div>
        {/if}
        {#if activeCodePayload?.tailSummary}
          <div class="right-pane-summary-section">
            <div class="right-pane-summary-title">{i18n.t('edits.nonText.tail')}</div>
            <pre class="right-pane-summary-content">{activeCodePayload.tailSummary}</pre>
          </div>
        {/if}
      </div>
    {:else if symlinkFile}
      <div class="right-pane-state right-pane-state--metadata">
        <Icon name="file" size={22} />
        <span>{i18n.t('edits.nonText.symlinkTitle')}</span>
        <span class="right-pane-meta-line">{i18n.t('edits.nonText.target')}: {activeCodePayload?.symlinkTarget ?? '-'}</span>
      </div>
    {:else if specialFile}
      <div class="right-pane-state right-pane-state--metadata">
        <Icon name="file" size={22} />
        <span>{i18n.t('edits.nonText.specialTitle')}</span>
        <span class="right-pane-meta-line">{i18n.t('edits.nonText.specialHint')}</span>
      </div>
    {:else if hasDiff}
      <div class="right-pane-diff" aria-label={displayPath}>
        <DiffCodeBlock
          diff={diffCode}
          originalContent={diffOriginalContent}
          currentContent={diffCurrentContent}
          ariaLabel={displayPath}
          language={fileLanguage}
          fill={true}
        />
      </div>
    {:else if !hasContent}
      <div class="right-pane-state">{i18n.t('edits.preview.empty')}</div>
    {:else}
      {#if isLargeFile}
        <div class="right-pane-notice">{i18n.t('web.filePreviewLargeFile')}</div>
      {/if}
      {#if markdownFile && documentMode === 'rendered'}
        <div class="right-pane-markdown">
          <MarkdownContent
            content={truncatedContent}
            baseFilePath={activeFilePath}
            filePreviewScope={{
              workspaceId: activeCodePayload?.workspaceId,
              workspacePath: activeCodePayload?.workspacePath || workspaceRoot,
              sessionId: activeCodePayload?.sessionId,
            }}
          />
        </div>
      {:else}
        <div class="right-pane-source" aria-label={displayPath}>
          {#each sourceLines as line, index (index)}
            <div class="right-pane-source-row">
              <span class="right-pane-source-line-number" aria-hidden="true">{index + 1}</span>
              <code class="right-pane-source-line">{@html line || '&nbsp;'}</code>
            </div>
          {/each}
        </div>
      {/if}
    {/if}
  </div>
</aside>

<style>
  .right-pane {
    /* 与左侧 sidebar 同款卡片样式：1px border + radius-lg + surface-1 底，
       overflow:hidden 用于让顶部 tabbar 的高亮条/底色被卡片圆角裁切，避免溢出 */
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
    height: 100%;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--magi-surface-right-pane);
    overflow: hidden;
  }

  /* ============ Tab 条 ============ */
  .right-pane-tabbar {
    position: relative;
    display: flex;
    align-items: stretch;
    height: 38px;
    flex-shrink: 0;
    border-bottom: 1px solid var(--border);
    background: transparent;
    padding-right: var(--space-2);
  }

  .right-pane-tabs {
    display: flex;
    flex: 1;
    min-width: 0;
    overflow-x: auto;
    scrollbar-width: none;
    /* drag-to-pan：默认抓握光标，提示用户「这一条可以按住拖」；
       拖拽进行中切到 grabbing 并禁用文字选择，避免选中 tab 文字 */
    cursor: grab;
    user-select: none;
  }
  .right-pane-tabs::-webkit-scrollbar { display: none; }
  .right-pane-tabs.dragging { cursor: grabbing; }

  .right-pane-tabbar--overlay {
    gap: 4px;
    padding: 0 6px;
  }

  .right-pane-add-tab {
    flex: 0 0 auto;
    align-self: center;
    width: 28px;
    height: 28px;
    margin: 0 4px;
    padding: 0;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border: none;
    border-radius: 6px;
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    transition: background var(--transition-fast), color var(--transition-fast);
  }

  .right-pane-add-wrap {
    position: relative;
    flex: 0 0 auto;
    display: flex;
    align-items: center;
  }

  .right-pane-add-menu {
    position: absolute;
    z-index: 20;
    top: calc(100% + 4px);
    right: 4px;
    min-width: 168px;
    padding: 4px;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--dropdown-bg);
    box-shadow: var(--shadow-lg);
  }

  .right-pane-add-menu-item {
    width: 100%;
    min-height: 32px;
    padding: 0 8px;
    display: flex;
    align-items: center;
    gap: 8px;
    border: 0;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--foreground);
    font: inherit;
    cursor: pointer;
  }

  .right-pane-add-menu-item:hover:not(:disabled),
  .right-pane-add-menu-item:focus-visible {
    background: var(--surface-hover);
  }

  .right-pane-add-menu-item:disabled {
    opacity: 0.45;
    cursor: default;
  }

  .right-pane-add-tab:hover:not(:disabled),
  .right-pane-add-tab:focus-visible {
    background: var(--surface-hover);
    color: var(--foreground);
  }

  .right-pane-add-tab:disabled {
    cursor: default;
    opacity: 0.4;
  }

  .right-pane-add-tab[aria-busy='true'] :global(svg) {
    animation: right-pane-spin 0.8s linear infinite;
  }

  @keyframes right-pane-spin {
    to { transform: rotate(360deg); }
  }

  .right-pane-overlay-action {
    flex: 0 0 auto;
    align-self: center;
    width: 28px;
    height: 28px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border: none;
    border-radius: 6px;
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    transition: background var(--transition-fast), color var(--transition-fast);
  }

  .right-pane-overlay-action:hover,
  .right-pane-overlay-action:focus-visible {
    background: var(--surface-hover);
    color: var(--foreground);
  }

  :global(.right-pane-back-icon) {
    transform: rotate(180deg);
  }

  .right-pane-tab {
    flex: 0 0 auto;
    max-width: 180px;
    min-width: 90px;
    padding: 0 var(--space-3) 0 var(--space-4);
    height: 100%;
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    border: none;
    background: transparent;
    color: var(--foreground-muted);
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    cursor: pointer;
    position: relative;
    border-right: 1px solid var(--border-subtle);
    transition: background var(--transition-fast), color var(--transition-fast);
  }

  .right-pane-tab:hover {
    background: var(--surface-hover);
    color: var(--foreground);
  }

  .right-pane-tab.active {
    background: var(--surface-1);
    color: var(--foreground);
    font-weight: var(--font-semibold);
  }

  .right-pane-tab.active::before {
    content: '';
    position: absolute;
    left: 0; right: 0; top: 0;
    height: 2px;
    background: var(--tab-accent, var(--primary));
  }

  .right-pane-tab-icon {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    color: var(--tab-accent, var(--foreground-muted));
    flex-shrink: 0;
  }

  .right-pane-tab-label {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex: 1;
    min-width: 0;
  }
  .right-pane-tab-label.mono { font-family: var(--font-mono); font-size: var(--text-xs); }

  .browser-control-status {
    width: 7px;
    height: 7px;
    flex: 0 0 auto;
    border-radius: 50%;
    background: var(--foreground-muted);
  }

  .browser-control-status.occupied {
    background: var(--success);
    box-shadow: 0 0 0 2px color-mix(in srgb, var(--success) 16%, transparent);
  }

  .right-pane-tab-close {
    width: 16px;
    height: 16px;
    border-radius: var(--radius-xs);
    background: transparent;
    color: var(--foreground-muted);
    border: none;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    cursor: pointer;
    opacity: 0;
    flex-shrink: 0;
    padding: 0;
    transition: opacity var(--transition-fast), background var(--transition-fast);
  }

  .right-pane-tab:hover .right-pane-tab-close,
  .right-pane-tab.active .right-pane-tab-close {
    opacity: 0.85;
  }

  .right-pane-tab-close:hover {
    background: color-mix(in srgb, var(--foreground-muted) 18%, transparent);
    opacity: 1;
  }

  /* ============ 副标题（路径 + 文档预览操作） ============ */
  .right-pane-subbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
    padding: 6px var(--space-4);
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
  }

  .right-pane-path {
    min-width: 0;
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    font-family: var(--font-mono);
  }

  .right-pane-document-actions,
  .right-pane-document-modes {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    flex-shrink: 0;
  }

  .right-pane-document-mode {
    padding: 3px 10px;
    border: none;
    border-radius: var(--radius-full);
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    font-size: var(--text-xs);
    transition: background var(--transition-fast), color var(--transition-fast);
  }

  .right-pane-document-mode:hover,
  .right-pane-document-mode.active {
    background: color-mix(in srgb, var(--surface-selected) 72%, transparent);
    color: var(--foreground);
  }

  .right-pane-document-icon-action {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 26px;
    height: 26px;
    padding: 0;
    border: none;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    transition: background var(--transition-fast), color var(--transition-fast);
  }

  .right-pane-document-icon-action:hover {
    background: color-mix(in srgb, var(--surface-selected) 72%, transparent);
    color: var(--foreground);
  }

  /* ============ Body ============ */
  .right-pane-body {
    min-width: 0;
    min-height: 0;
    flex: 1;
    overflow: auto;
    padding: var(--space-4);
  }

  .right-pane-body--code {
    display: flex;
    flex-direction: column;
    overflow: hidden;
    padding: 0;
  }

  .right-pane-body--browser {
    display: flex;
    flex-direction: column;
    width: 100%;
    overflow: hidden;
    padding: 0;
  }

  .right-pane-body--terminal {
    display: flex;
    overflow: hidden;
    padding: 0;
  }

  .right-pane-source {
    min-height: 0;
    flex: 1;
    overflow: auto;
    padding: var(--space-4) 0;
    background: transparent;
    color: var(--foreground);
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    line-height: 1.6;
  }

  .right-pane-source-row {
    display: grid;
    grid-template-columns: 46px minmax(0, 1fr);
    align-items: start;
    min-width: 0;
  }

  .right-pane-source-line-number {
    position: sticky;
    left: 0;
    z-index: 1;
    padding: 0 10px 0 var(--space-2);
    background: transparent;
    color: var(--foreground-muted);
    font-variant-numeric: tabular-nums;
    opacity: 0.46;
    text-align: right;
    user-select: none;
  }

  .right-pane-source-line {
    min-width: 0;
    padding: 0 var(--space-4) 0 var(--space-3);
    background: transparent !important;
    border: none !important;
    box-shadow: none !important;
    color: inherit;
    font: inherit;
    overflow-wrap: anywhere;
    tab-size: 2;
    white-space: pre-wrap;
  }

  /* ============ Diff 视图（与对话区共享 DiffCodeBlock） ============ */
  .right-pane-diff {
    display: flex;
    min-height: 0;
    flex: 1;
    overflow: hidden;
    padding: var(--space-4);
    background: transparent;
  }

  .right-pane-markdown {
    max-width: 880px;
    color: var(--foreground);
    line-height: 1.65;
  }

  .right-pane-state {
    min-height: 180px;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: var(--space-3);
    color: var(--foreground-muted);
    text-align: center;
    font-size: var(--text-sm);
    line-height: 1.5;
  }

  .right-pane-state--error { color: var(--error); }
  .right-pane-state--metadata { padding: var(--space-4); }

  .right-pane-meta-line {
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }

  .right-pane-image-wrap {
    position: relative;
    width: 100%;
    height: 100%;
    box-sizing: border-box;
  }

  .right-pane-image {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 100%;
    height: 100%;
    padding: var(--space-4);
    overflow: hidden;
    box-sizing: border-box;
    touch-action: none;
  }

  .right-pane-image.zoomed {
    cursor: grab;
  }

  .right-pane-image.dragging {
    cursor: grabbing;
  }

  .right-pane-image-el {
    max-width: 100%;
    max-height: 100%;
    object-fit: contain;
    transform-origin: center center;
    will-change: transform;
    user-select: none;
    -webkit-user-drag: none;
    /* 透明图片用棋盘格底衬出边界，避免与面板同色看不清 */
    background-image:
      linear-gradient(45deg, var(--surface-subtle, #e5e7eb) 25%, transparent 25%),
      linear-gradient(-45deg, var(--surface-subtle, #e5e7eb) 25%, transparent 25%),
      linear-gradient(45deg, transparent 75%, var(--surface-subtle, #e5e7eb) 75%),
      linear-gradient(-45deg, transparent 75%, var(--surface-subtle, #e5e7eb) 75%);
    background-size: 16px 16px;
    background-position: 0 0, 0 8px, 8px -8px, -8px 0;
    border-radius: var(--radius-sm, 4px);
  }

  .right-pane-image-controls {
    position: absolute;
    bottom: var(--space-3);
    left: 50%;
    transform: translateX(-50%);
    display: flex;
    align-items: center;
    gap: var(--space-1);
    padding: 4px 6px;
    background: var(--surface-overlay, rgba(20, 20, 22, 0.82));
    border: 1px solid var(--border-subtle, rgba(255, 255, 255, 0.12));
    border-radius: var(--radius-md, 8px);
    box-shadow: 0 2px 8px rgba(0, 0, 0, 0.25);
    backdrop-filter: blur(6px);
  }

  .image-zoom-btn,
  .image-zoom-level {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border: none;
    background: transparent;
    color: var(--foreground-on-overlay, #f5f5f5);
    cursor: pointer;
    border-radius: var(--radius-sm, 4px);
  }

  .image-zoom-btn {
    width: 26px;
    height: 26px;
  }

  .image-zoom-level {
    min-width: 48px;
    height: 26px;
    padding: 0 6px;
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }

  .image-zoom-btn:hover:not(:disabled),
  .image-zoom-level:hover {
    background: rgba(255, 255, 255, 0.14);
  }

  .image-zoom-btn:disabled {
    opacity: 0.4;
    cursor: default;
  }

  .right-pane-large-text {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }

  .right-pane-summary-section {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .right-pane-summary-title {
    color: var(--foreground-muted);
    font-size: var(--text-xs);
    font-weight: var(--font-semibold);
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }

  .right-pane-summary-content {
    margin: 0;
    padding: var(--space-3);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: color-mix(in srgb, var(--surface-1) 82%, var(--background));
    color: var(--foreground);
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    line-height: 1.6;
    max-height: 260px;
    overflow: auto;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  .right-pane-notice {
    margin-bottom: var(--space-3);
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius-md);
    border: 1px solid color-mix(in srgb, var(--warning, #f59e0b) 30%, var(--border));
    background: color-mix(in srgb, var(--warning, #f59e0b) 10%, transparent);
    color: var(--foreground);
    font-size: var(--text-xs);
  }
</style>
