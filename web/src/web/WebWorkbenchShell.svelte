<script lang="ts">
  import { onMount, tick, untrack, type Component } from 'svelte';
  import App from '../App.svelte';
  import { setWebSidebarContext } from './sidebar-context';
  import Icon from '../components/Icon.svelte';
  import MagiWordmark from '../components/MagiWordmark.svelte';
  import Modal from '../components/Modal.svelte';
  import { runActionWithFeedback } from '../lib/action-feedback';
  import {
    DESKTOP_CONTEXT_DROP_EVENT,
    normalizeDesktopDropPaths,
    registerDesktopFileDropListener,
    resolveDesktopDroppedPath,
    resolveDesktopDropZone,
    type DesktopDragDropEvent,
    type DesktopDropRect,
    type DesktopDropZone,
  } from '../lib/desktop-file-drop';
  import type { IconName } from '../lib/icons';
  import { desktopContextMenu } from '../lib/desktop-context-menu-contract';
  import {
    addToast,
    advanceWorkspaceSessionProjectionCursor,
    canApplyWorkspaceSessionProjectionCursor,
    messagesState,
    replaceWorkspaceSessionProjection,
    replacePersonalSessionProjection,
    updateWorkspaceSessionProjectionSessions,
    type WorkspaceSessionProjectionCursor,
  } from '../stores/messages.svelte';
  import {
    directIncidentError,
    incidentErrorDiagnostics,
    reportIncident,
  } from '../lib/notifications';
  import {
    resolveSessionActivityIndicator,
    resolveSessionRunningState,
    shouldMarkSessionCompletionViewed,
  } from '../lib/session-activity-indicator';
  import { getClientBridge } from '../shared/bridges/bridge-runtime';
  import { normalizeRustBootstrapPayload } from '../shared/bridges/rust-daemon-contract';
  import { i18n } from '../stores/i18n.svelte';
  import type { EditContentKind, Session } from '../types/message';
  import { resolveFilePreviewScope } from '../lib/file-preview-scope';
  import { isHtmlFile } from '../lib/file-preview-utils';
  import {
    cycleBuiltinAppearance,
    subscribeAppearanceRuntime,
    type AppearanceRuntimeSnapshot,
  } from '../appearance/runtime';
  import {
    OPEN_HTML_FILE_IN_BROWSER_EVENT,
    type OpenHtmlFileInBrowserRequest,
  } from '../lib/browser-navigation';
  import {
    RUNTIME_CONNECTION_EVENT,
    resolveAgentPath,
    getWorkspaceSessions,
    getPersonalSessions,
    listAgentWorkspaces,
    markAgentSessionViewed,
    registerAgentWorkspace,
    removeAgentWorkspace,
    renameAgentSession,
    resolveAgentBaseUrl,
    BROWSER_AUTHORITY_CHANGED_EVENT,
    type AgentConnectionEventDetail,
    type AgentWorkspaceSummary,
  } from './agent-api';
  import {
    loadBrowserAuthorityTab,
    loadBrowserAuthoritySession,
    prepareBrowserAuthorityForDesktop,
  } from './browser-authority-coordinator';
  import {
    agentBindingWorkspaceId,
    agentBindingWorkspacePath,
    resolveAgentBindingContext,
    type AgentBindingOverride,
  } from './agent-binding-context';
import {
  navigateSession,
  sessionNavigationState,
  waitForSessionNavigation,
} from '../shared/session-navigation.svelte';
  import {
    rightPaneState,
    getRightPaneState,
    openCodeTab,
    setActiveRightPaneTabFromDesktop,
    pendingDesktopPanelIntentFor,
    clearPendingDesktopPanelIntent,
    setRightPaneCollapsed,
    synchronizeBrowserSessionSnapshot,
    type BrowserTabPayload,
    type CodeTabPayload,
  } from '../stores/right-pane.svelte';
  import {
    syncComposerWorkspaces,
  } from '../stores/composer-workspace.svelte';
  import {
    closeWorkspaceFolderPicker,
    openWorkspaceFolderPicker,
    workspaceOnboardingState,
  } from '../stores/workspace-onboarding.svelte';
  import {
    PANEL_LAYOUT,
    resolvePanelLayout,
    resolvePreviewPanelWidthBounds,
    resolvePanelVisibility,
  } from './panel-layout';
  import {
    decideDesktopPanelActivation,
    desktopPanelTargetAcknowledged,
    desktopPanelTargetKey,
    sameDesktopPanelTarget,
  } from './desktop-panel-activation';

  interface Props {
    desktopAppSurface?: boolean;
  }

  let { desktopAppSurface = false }: Props = $props();

  // 这些 storage key 必须先于下方 `$state` 初始化器声明——它们被
  // readInitialExpandedWorkspaces / readInitialSidebarMode / readInitialWorkspacesCollapsed /
  // readInitialRecentSessionsCollapsed
  // 在 $state 初始化时读取，
  // 普通的 const 受 TDZ 约束，定义在文件下方会触发 ReferenceError。
  const SIDEBAR_EXPANDED_WORKSPACES_KEY = 'magi-sidebar-expanded-workspaces';
  const SIDEBAR_MODE_KEY = 'magi-sidebar-mode';
  const SIDEBAR_WORKSPACES_COLLAPSED_KEY = 'magi-sidebar-workspaces-collapsed';
  const SIDEBAR_RECENT_SESSIONS_COLLAPSED_KEY = 'magi-sidebar-recent-sessions-collapsed';

  let loading = $state(true);
  let loadError = $state('');
  let agentRecovering = $state(false);
  let agentBaseUrl = $state('');
  let workspaces = $state<AgentWorkspaceSummary[]>([]);
  let selectedWorkspaceId = $state('');
  let currentSessionId = $state<string | null>(null);
  let sessionsByWorkspace = $state<Record<string, Session[]>>({});
  let recentSessions = $state<Session[]>([]);
  let loadingWorkspaceIds = $state<Record<string, boolean>>({});
  let expandedWorkspaceIds = $state<Record<string, boolean>>(readInitialExpandedWorkspaces());
  let workspacesCollapsed = $state(readInitialWorkspacesCollapsed());
  let recentSessionsCollapsed = $state(readInitialRecentSessionsCollapsed());
  let workspaceSelectionPending = $state(false);
  let viewportWidth = $state(typeof window !== 'undefined' ? window.innerWidth : 1440);
  let sidebarOpen = $state(false);
  let workspaceActionPending = $state(false);
  let showRemoveWorkspaceDialog = $state(false);
  let pendingRemoveWorkspace = $state<AgentWorkspaceSummary | null>(null);
  let workspaceDialogError = $state('');
  let showDeleteSessionDialog = $state(false);
  let pendingDeleteSession = $state<{ workspace: AgentWorkspaceSummary | null; session: Session } | null>(null);
  let editingSession = $state<{ workspaceId: string | null; sessionId: string } | null>(null);
  let sessionRenameDraft = $state('');
  let sessionRenameError = $state('');
  let renamingSessionId = $state<string | null>(null);
  let sessionRenameInput = $state<HTMLInputElement | null>(null);
  let appearanceRuntime = $state<AppearanceRuntimeSnapshot>({
    library: null,
    activeTheme: null,
    mode: 'dark',
    previewing: false,
  });
  let sidebarMode = $state<'projects' | 'files'>(readInitialSidebarMode());
  let sidebarWidth = $state<number | null>(null);
  let isSidebarResizing = $state(false);
  let sidebarCollapsed = $state(false);
  let previewPanelWidth = $state<number | null>(null);
  let isPreviewPanelResizing = $state(false);
  type SidebarTooltipState = {
    text: string;
    left: number;
    top: number;
    placement: 'above' | 'below';
  };
  let sidebarTooltip = $state<SidebarTooltipState | null>(null);
  let sidebarTooltipTarget = $state<HTMLElement | null>(null);
  let sidebarTooltipFrame: number | null = null;
  let desktopRightPaneVisible = $state(false);
  let desktopSnapshot = $state<MagiDesktopWindowSnapshot | null>(null);
  let workbenchElement = $state<HTMLElement | null>(null);
  let pendingDesktopRightPaneWidth: number | null = null;
  let desktopRightPaneResizeFrame: number | null = null;
  let desktopRightPaneResizeCancel: (() => void) | null = null;
  let desktopSnapshotEpoch = '';
  let desktopSnapshotRevision = -1;
  type DesktopVisibilityTarget = {
    scopeKey: string;
    visible: boolean;
  };
  type DesktopVisibilitySyncRequest = DesktopVisibilityTarget & {
    requestId: number;
  };
  type DesktopVisibilitySyncFailure = DesktopVisibilityTarget & {
    desktopEpoch: string;
    snapshotRevision: number;
  };
  type DesktopPanelTarget = {
    scopeKey: string;
    kind: 'agent' | 'browser' | 'code' | 'terminal' | null;
    tabId: string | null;
    browserSessionId: string | null;
    browser: BrowserTabPayload | null;
  };
  type DesktopPanelActivationRequest = DesktopPanelTarget & {
    key: string;
    requestId: number;
    recoveryRevision: number;
  };
  // 右栏可见性只允许一条 IPC 请求在途。这个状态不是 UI 状态：
  // - target 来自当前 Renderer 的 rightPaneState；
  // - acknowledged 来自 Main 返回/推送的 desktopSnapshot；
  // - 下一次请求必须等待上一条请求完成，避免旧响应覆盖新意图。
  let desktopVisibilitySyncRequest: DesktopVisibilitySyncRequest | null = null;
  let desktopVisibilitySyncFailure: DesktopVisibilitySyncFailure | null = null;
  let desktopVisibilityRequestId = 0;
  let desktopVisibilitySyncEpoch = $state(0);
  // 非浏览器右栏面板的 Main 激活必须由这里唯一串行化。RightPane 只更新
  // 用户意图；只有 Main 快照确认后，才能认为原生 Browser Surface 已撤下。
  // 这避免 Renderer 已渲染终端/代码、而旧 Chromium guest 仍保留在命中树。
  let desktopPanelActivationRequest: DesktopPanelActivationRequest | null = null;
  let desktopPanelActivationRequestId = 0;
  let desktopPanelActivationFailureKey = '';
  let desktopPanelActivationEpoch = $state(0);
  let desktopRuntimeRecoveryRevision = $state(0);
  let desktopBrowserRuntimeReady = $state(false);
  let desktopPanelActivationCompletedRecoveryRevision = $state(0);
  let sidebarElement = $state<HTMLElement | null>(null);
  let desktopDropIndicator = $state<{
    zone: DesktopDropZone;
    rect: DesktopDropRect;
  } | null>(null);
  let workspaceSessionRequestSeq = 0;
  let workspaceListRequestSeq = 0;
  let personalSessionRequestSeq = 0;
  const workspaceSessionRequestSeqByWorkspace = new Map<string, number>();
  const workspaceSessionCursorByWorkspace = new Map<string, WorkspaceSessionProjectionCursor>();
  const sessionViewedRequests = new Set<string>();

  type WorkspaceFileSelection = { pathRef: string; displayPath: string; name: string };
  type ProjectFileTreeProps = {
    rootPath: string;
    workspaceId: string;
    title?: string;
    titlePath?: string;
    selectedFilePath?: string | null;
    onFileSelect?: (selection: WorkspaceFileSelection) => void;
  };
  type RightPaneProps = {
    workspaceRoot: string;
    overlay?: boolean;
    desktopSurface?: boolean;
    htmlBrowserOpenRequest?: HtmlBrowserOpenRequest | null;
    onHtmlBrowserOpenHandled?: (requestId: number) => void;
  };
  type HtmlBrowserOpenRequest = {
    requestId: number;
    filepath: string;
    workspaceId: string;
    workspacePath: string;
    sessionId: string;
  };
  type WebFolderPickerProps = {
    title?: string;
    onSelect: (selection: WorkspaceFileSelection) => void;
    onCancel: () => void;
    disabled?: boolean;
  };

  let ProjectFileTreeComponent = $state<Component<ProjectFileTreeProps> | null>(null);
  let RightPaneComponent = $state<Component<RightPaneProps> | null>(null);
  let WebFolderPickerComponent = $state<Component<WebFolderPickerProps> | null>(null);
  let htmlBrowserOpenRequest = $state<HtmlBrowserOpenRequest | null>(null);
  let htmlBrowserOpenRequestId = 0;
  let projectFileTreeLoad: Promise<void> | null = null;
  let rightPaneLoad: Promise<void> | null = null;
  let webFolderPickerLoad: Promise<void> | null = null;

  function loadProjectFileTree(): Promise<void> {
    if (ProjectFileTreeComponent) return Promise.resolve();
    projectFileTreeLoad ??= import('./ProjectFileTree.svelte')
      .then((module) => {
        ProjectFileTreeComponent = module.default;
      })
      .finally(() => {
        projectFileTreeLoad = null;
      });
    return projectFileTreeLoad;
  }

  function loadRightPane(): Promise<void> {
    if (RightPaneComponent) return Promise.resolve();
    rightPaneLoad ??= import('./RightPane.svelte')
      .then((module) => {
        RightPaneComponent = module.default;
      })
      .finally(() => {
        rightPaneLoad = null;
      });
    return rightPaneLoad;
  }

  function loadWebFolderPicker(): Promise<void> {
    if (WebFolderPickerComponent) return Promise.resolve();
    webFolderPickerLoad ??= import('./WebFolderPicker.svelte')
      .then((module) => {
        WebFolderPickerComponent = module.default;
      })
      .finally(() => {
        webFolderPickerLoad = null;
      });
    return webFolderPickerLoad;
  }

  const INTERNAL_SESSION_NAME_PATTERNS = [
    /^auto-deep-followup-\d+$/i,
    /^auto-governance-resume-\d+$/i,
    /^real-dispatch-regression-\d+$/i,
  ];
  const SIDEBAR_WIDTH_STORAGE_KEY = 'magi-sidebar-width';
  const SIDEBAR_COLLAPSED_STORAGE_KEY = 'magi-sidebar-collapsed';
  const PREVIEW_PANEL_WIDTH_STORAGE_KEY = 'magi-preview-panel-width';
  const DEFAULT_SIDEBAR_WIDTH = 320;
  const COMPACT_SIDEBAR_WIDTH = 240;
  const MIN_SIDEBAR_WIDTH = 220;
  const MAX_SIDEBAR_WIDTH = 520;
  // Desktop Main 的默认右栏宽度是 480px。Renderer 在快照到达前也使用同一
  // 个值，避免首帧先按 360px 排版、再跳到 Main 的宽度而造成窗口闪动。
  const DEFAULT_PREVIEW_PANEL_WIDTH = 480;
  const SESSION_NAME_MAX_CHARS = 40;

  const selectedWorkspace = $derived(
    workspaces.find((workspace) => workspace.workspaceId === selectedWorkspaceId) ?? null
  );
  const pendingNavigation = $derived(sessionNavigationState.pending);
  const pendingSessionSwitchId = $derived(
    pendingNavigation?.target.kind === 'session' ? pendingNavigation.target.sessionId : null
  );
  const pendingSessionSwitchWorkspaceId = $derived(
    pendingNavigation?.target.kind === 'session' && pendingNavigation.target.scope === 'workspace'
      ? pendingNavigation.target.workspaceId
      : null
  );

  $effect(() => {
    const nextWorkspaces = workspaces;
    const nextSelectedWorkspaceId = selectedWorkspaceId;
    untrack(() => {
      syncComposerWorkspaces(nextWorkspaces, nextSelectedWorkspaceId);
    });
  });

  $effect(() => {
    const sessions = messagesState.personalSessionProjection.sessions;
    if (sessions.length !== recentSessions.length || sessions.some((session, index) => recentSessions[index]?.id !== session.id || recentSessions[index]?.updatedAt !== session.updatedAt)) {
      recentSessions = sessions;
    }
  });

  const shellLayoutStyle = $derived([
    sidebarWidth ? `--sidebar-width: ${sidebarWidth}px` : '',
    previewPanelWidth ? `--preview-panel-width: ${previewPanelWidth}px` : '',
    desktopSnapshot?.layout.rightPaneWidth
      ? `--desktop-right-pane-width: ${desktopSnapshot.layout.rightPaneWidth}px`
      : '',
    `--workbench-min-content-width: ${PANEL_LAYOUT.minContentWidth}px`,
    `--preview-min-width: ${PANEL_LAYOUT.minPreviewWidth}px`,
    `--preview-handle-width: ${PANEL_LAYOUT.previewHandleWidth}px`,
    `--shell-padding: ${PANEL_LAYOUT.shellPadding}px`,
    `--shell-gap: ${PANEL_LAYOUT.shellGap}px`,
    `--desktop-right-pane-divider-width: ${PANEL_LAYOUT.previewHandleWidth}px`,
  ].filter(Boolean).join('; '));

  const effectiveSidebarWidth = $derived(
    sidebarWidth ?? (viewportWidth <= 1120 ? COMPACT_SIDEBAR_WIDTH : DEFAULT_SIDEBAR_WIDTH)
  );
  const effectivePreviewPanelWidth = $derived(
    desktopAppSurface
      ? (desktopSnapshot?.layout.rightPaneWidth ?? DEFAULT_PREVIEW_PANEL_WIDTH)
      : (previewPanelWidth ?? DEFAULT_PREVIEW_PANEL_WIDTH),
  );
  const rightPaneOpenForLayout = $derived(
    desktopAppSurface
      ? desktopRightPaneVisible
      : !getRightPaneState(rightPaneState.activeScopeKey).collapsed,
  );
  const panelLayout = $derived(resolvePanelLayout({
    viewportWidth,
    sidebarWidth: effectiveSidebarWidth,
    previewPanelWidth: effectivePreviewPanelWidth,
    sidebarVisible: desktopAppSurface
      ? !sidebarCollapsed
      : viewportWidth > PANEL_LAYOUT.mobileBreakpoint && !sidebarCollapsed,
    rightPaneOpen: rightPaneOpenForLayout,
    desktopSurface: desktopAppSurface,
  }));
  const sidebarIsDrawer = $derived(panelLayout.sidebarDrawer);

  /** 当前 session 的右栏多 tab 状态；由 right-pane store 派生 */
  const activeRightPaneState = $derived(getRightPaneState(rightPaneState.activeScopeKey));
  $effect(() => {
    if (!desktopAppSurface || !desktopSnapshot) return;
    const pendingIntent = pendingDesktopPanelIntentFor(rightPaneState.activeScopeKey);
    if (pendingIntent) {
      const committed = desktopSnapshot.layout.activePanelKind === pendingIntent.kind
        && desktopSnapshot.layout.activeTabId === pendingIntent.tabId;
      if (committed) {
        clearPendingDesktopPanelIntent(
          rightPaneState.activeScopeKey,
          pendingIntent.kind,
          pendingIntent.tabId,
        );
      } else {
        // 用户已经选择了另一个右栏 Tab，等待 Main 完成同一激活事务。
        // 这里不能用旧 desktopSnapshot 把选择项写回去，否则文件 Tab 会
        // 在浏览器激活期间瞬间跳回旧的 Browser Tab。
        return;
      }
    }
    if (desktopSnapshot.layout.activePanelKind !== 'browser') return;
    const logicalTabId = desktopSnapshot.layout.activeTabId?.trim() || '';
    if (!logicalTabId) return;
    const pane = activeRightPaneState;
    const target = pane.openTabs.find((tab) => (
      tab.kind === 'browser'
      && (tab.payload as BrowserTabPayload).tabId === logicalTabId
    ));
    if (target && pane.activeTabId !== target.id) {
      setActiveRightPaneTabFromDesktop(rightPaneState.activeScopeKey, target.id);
    }
  });
  /** Desktop 的窗口布局以 Main snapshot 为准；Web 客户端仍使用本地面板状态。 */
  const rightPaneVisible = $derived(
    rightPaneOpenForLayout,
  );
  const inlineRightPaneVisible = $derived(!desktopAppSurface && rightPaneVisible);
  const panelVisibility = $derived(resolvePanelVisibility({
    sidebarDrawer: sidebarIsDrawer,
    sidebarPreferredOpen: !sidebarCollapsed,
    sidebarDrawerOpen: sidebarOpen,
    rightPaneOpen: rightPaneVisible,
  }));
  const sidebarHidden = $derived(!sidebarIsDrawer && !panelVisibility.sidebarVisible);

  function applyDesktopSnapshot(snapshot: MagiDesktopWindowSnapshot): boolean {
    if (
      snapshot.desktopEpoch === desktopSnapshotEpoch
      && snapshot.snapshotRevision < desktopSnapshotRevision
    ) {
      return false;
    }
    desktopSnapshotEpoch = snapshot.desktopEpoch;
    desktopSnapshotRevision = snapshot.snapshotRevision;
    desktopSnapshot = snapshot;
    desktopRightPaneVisible = snapshot.layout.rightPaneVisible;
    // Main 已经产生了新的确认快照，之前针对旧快照的失败状态不再阻塞
    // 当前目标；是否需要再次提交由下面的单向状态机重新判断。
    desktopVisibilitySyncFailure = null;
    return true;
  }

  function sameDesktopVisibilityTarget(
    left: DesktopVisibilityTarget | null,
    right: DesktopVisibilityTarget,
  ): boolean {
    return Boolean(left)
      && left?.scopeKey === right.scopeKey
      && left.visible === right.visible;
  }

  function currentDesktopPanelTarget(): DesktopPanelTarget {
    const scopeKey = rightPaneState.activeScopeKey;
    const pane = getRightPaneState(scopeKey);
    const activeTab = pane.activeTabId
      ? pane.openTabs.find((tab) => tab.id === pane.activeTabId) ?? null
      : null;
    if (pane.collapsed || !activeTab) {
      return { scopeKey, kind: null, tabId: null, browserSessionId: null, browser: null };
    }
    if (activeTab.kind === 'browser') {
      const browser = activeTab.payload as BrowserTabPayload;
      return {
        scopeKey,
        kind: 'browser',
        tabId: browser.tabId,
        browserSessionId: browser.browserSessionId,
        browser,
      };
    }
    return {
      scopeKey,
      kind: activeTab.kind,
      tabId: activeTab.id,
      browserSessionId: null,
      browser: null,
    };
  }

  function isClosedBrowserTabError(error: unknown): boolean {
    const message = error instanceof Error ? error.message : String(error);
    return /browser tab is not ready:[\s\S]*\(Closed\)/u.test(message);
  }

  function resyncAfterClosedBrowserTab(target: DesktopPanelTarget): void {
    const browser = target.browser;
    if (!browser) return;
    // “已关闭”是权威状态，不是可重试的 Surface 激活失败。必须立即拉取
    // BrowserAuthority 快照收敛本地 Tab，避免旧 Tab 一直占据右栏内容槽。
    void loadBrowserAuthoritySession(browser.browserSessionId, (snapshot) => {
      synchronizeBrowserSessionSnapshot(snapshot, browser.workspacePath, {
          workspaceId: browser.workspaceId,
          sessionId: browser.sessionId,
        });
      })
      .catch((error) => {
        console.warn('[WebWorkbenchShell] 收敛已关闭浏览器 Tab 失败:', error);
      });
  }

  /**
   * 激活请求跨越 HTTP、IPC 和 Surface 就绪边界后，必须重新核对当前用户
   * 意图。旧请求只负责结束自己的单飞槽，不能继续改写 RightPane、
   * BrowserAuthority 或 Main 的活动 Surface。
   */
  function abandonSupersededDesktopPanelActivation(
    request: DesktopPanelActivationRequest,
  ): boolean {
    if (sameDesktopPanelTarget(currentDesktopPanelTarget(), request)) return false;
    if (desktopPanelActivationRequest?.requestId === request.requestId) {
      desktopPanelActivationRequest = null;
      desktopPanelActivationEpoch += 1;
    }
    return true;
  }

  async function activateDesktopPanelTarget(request: DesktopPanelActivationRequest): Promise<void> {
    const desktop = window.magiDesktop;
    if (!desktop) return;
    try {
      if (request.kind === 'browser' && request.browser) {
        const browser = request.browser;
        if (browser.lifecycle === 'closed') {
          resyncAfterClosedBrowserTab(request);
          return;
        }
        // Renderer 里的 BrowserTabPayload 只是右栏投影，重启或 Renderer
        // 重建期间可能仍保留旧的 about:blank。激活真实 Chromium guest 前，
        // 必须读取 Authority 的当前 URL 与 navigation revision，不能让投影
        // 覆盖已持久化的页面事实。
        const authorityResolution = await loadBrowserAuthorityTab(
          browser.browserSessionId,
          browser.tabId,
        );
        if (abandonSupersededDesktopPanelActivation(request)) return;
        const authoritativeTab = authorityResolution.tab;
        if (!authoritativeTab || authoritativeTab.lifecycle === 'closed') {
          synchronizeBrowserSessionSnapshot(
            authorityResolution.snapshot,
            browser.workspacePath,
            {
              workspaceId: authorityResolution.snapshot.workspaceId,
              sessionId: authorityResolution.snapshot.sessionId,
            },
          );
          return;
        }
        synchronizeBrowserSessionSnapshot(
          authorityResolution.snapshot,
          browser.workspacePath,
          {
            workspaceId: authorityResolution.snapshot.workspaceId,
            sessionId: authorityResolution.snapshot.sessionId,
          },
        );
        const activatedSnapshot = await desktop.activateBrowser({
          tabId: browser.tabId,
          browserSessionId: browser.browserSessionId,
          url: authoritativeTab.url,
          navigationRevision: authoritativeTab.navigationRevision,
          viewport: { mode: 'auto' },
        });
        applyDesktopSnapshot(activatedSnapshot);
        if (abandonSupersededDesktopPanelActivation(request)) return;
        // activateBrowser 只建立逻辑 Surface 并把当前内容槽身份提交给
        // Renderer。必须等对应 <webview> 完成注册后，才能恢复 Authority
        // 页面；否则 RestorePage 会在没有真实 WebContents 的 Surface 上
        // 等待，形成启动死锁。
        const readySnapshot = await desktop.waitForBrowserSurface({ tabId: browser.tabId });
        applyDesktopSnapshot(readySnapshot);
        if (abandonSupersededDesktopPanelActivation(request)) return;
        await prepareBrowserAuthorityForDesktop(
          {
            browserSessionId: browser.browserSessionId,
            tabId: browser.tabId,
            lifecycle: authoritativeTab.lifecycle,
          },
          (authoritySnapshot) => {
            // setActiveBrowserTab 已经进入 Authority lane，但用户可能在请求
            // 返回前选择了新 Tab。过期结果不得用 revealTabId 复活旧意图。
            if (!sameDesktopPanelTarget(currentDesktopPanelTarget(), request)) return;
            synchronizeBrowserSessionSnapshot(authoritySnapshot, browser.workspacePath, {
              workspaceId: browser.workspaceId,
              sessionId: browser.sessionId,
              revealTabId: browser.tabId,
              newTabLabel: i18n.t('browser.tab.new'),
            });
          },
          {
            forceRestore: request.recoveryRevision > desktopPanelActivationCompletedRecoveryRevision,
          },
        );
        if (abandonSupersededDesktopPanelActivation(request)) return;
      }
      const snapshot = request.kind === 'browser' && request.browser
        ? desktopSnapshot ?? await desktop.getSnapshot()
        : await desktop.activatePanel({ kind: request.kind, tabId: request.tabId });
      applyDesktopSnapshot(snapshot);
      if (desktopPanelActivationRequest?.requestId !== request.requestId) return;
      const recoveryIsObsolete = request.recoveryRevision < desktopRuntimeRecoveryRevision;
      desktopPanelActivationRequest = null;
      if (recoveryIsObsolete) {
        // 这条请求属于 daemon 重启前的旧运行时代次。它的结果只能收口
        // 自身的 Promise，不能把旧 Surface 当作恢复完成；下一轮 effect
        // 会用新的代次重新物化当前逻辑 Browser Tab。
        desktopPanelActivationFailureKey = '';
        desktopPanelActivationEpoch += 1;
        return;
      }
      const target = currentDesktopPanelTarget();
      if (sameDesktopPanelTarget(target, request)) {
        if (desktopPanelTargetAcknowledged(snapshot, target)) {
          desktopPanelActivationFailureKey = '';
          if (target.kind) {
            clearPendingDesktopPanelIntent(target.scopeKey, target.kind, target.tabId ?? '');
          }
          desktopPanelActivationCompletedRecoveryRevision = request.recoveryRevision;
        } else if (target.kind === 'browser') {
        } else {
          desktopPanelActivationFailureKey = request.key;
        }
      }
      desktopPanelActivationEpoch += 1;
    } catch (error) {
      if (desktopPanelActivationRequest?.requestId !== request.requestId) return;
      const recoveryIsObsolete = request.recoveryRevision < desktopRuntimeRecoveryRevision;
      desktopPanelActivationRequest = null;
      if (recoveryIsObsolete) {
        desktopPanelActivationFailureKey = '';
        desktopPanelActivationEpoch += 1;
        return;
      }
      const target = currentDesktopPanelTarget();
      if (sameDesktopPanelTarget(target, request)) {
        if (isClosedBrowserTabError(error)) {
          resyncAfterClosedBrowserTab(target);
        } else {
          desktopPanelActivationFailureKey = request.key;
        }
      }
      desktopPanelActivationEpoch += 1;
      console.warn('[WebWorkbenchShell] 激活桌面右栏面板失败:', error);
    }
  }
  /** 项目文件树高亮：active code tab 的 filepath */
  const activeCodeTabFilePath = $derived.by<string>(() => {
    if (!activeRightPaneState.activeTabId) return '';
    const tab = activeRightPaneState.openTabs.find((t) => t.id === activeRightPaneState.activeTabId);
    if (!tab || tab.kind !== 'code') return '';
    return (tab.payload as CodeTabPayload).filepath;
  });

  $effect(() => {
    if (sidebarMode === 'files') {
      void loadProjectFileTree().catch((error) => {
        console.error('[WebWorkbenchShell] 文件树加载失败:', error);
        addToast('error', i18n.t('app.featureLoadFailed'));
        sidebarMode = 'projects';
      });
    }
    if (inlineRightPaneVisible || (desktopAppSurface && desktopRightPaneVisible)) {
      void loadRightPane().catch((error) => {
        console.error('[WebWorkbenchShell] 右侧面板加载失败:', error);
        addToast('error', i18n.t('app.featureLoadFailed'));
        if (!desktopAppSurface) setRightPaneCollapsed(rightPaneState.activeScopeKey, true);
      });
    }
    if (workspaceOnboardingState.open) {
      void loadWebFolderPicker().catch((error) => {
        console.error('[WebWorkbenchShell] 工作区选择器加载失败:', error);
        addToast('error', i18n.t('app.featureLoadFailed'));
        closeWorkspaceFolderPicker();
      });
    }
  });
  const previewIsOverlay = $derived(inlineRightPaneVisible && panelLayout.previewOverlay);

  function currentBootstrapWorkspaceId(): string {
    return typeof messagesState.currentWorkspaceId === 'string'
      ? messagesState.currentWorkspaceId.trim()
      : '';
  }

  function currentWorkspaceBinding(): { scope: 'personal' | 'workspace'; workspaceId: string; workspacePath: string; sessionId: string } {
    const binding = resolveAgentBindingContext();
    return {
      scope: binding.scope,
      workspaceId: agentBindingWorkspaceId(binding),
      workspacePath: agentBindingWorkspacePath(binding),
      sessionId: binding.sessionId ?? '',
    };
  }

  function currentBootstrapSessionIdForWorkspace(workspaceId: string): string {
    const authoritativeWorkspaceId = currentBootstrapWorkspaceId();
    if (!authoritativeWorkspaceId || authoritativeWorkspaceId !== workspaceId) {
      return '';
    }
    return typeof messagesState.currentSessionId === 'string'
      ? messagesState.currentSessionId.trim()
      : '';
  }

  function workspacePathForId(workspaceId: string): string {
    const workspace = workspaces.find((candidate) => candidate.workspaceId === workspaceId);
    return workspace ? workspaceNavigationPath(workspace) : '';
  }

  function workspaceNavigationPath(workspace: AgentWorkspaceSummary): string {
    // 会话协议使用规范化后的真实工作区路径；rootPathRef 只用于文件系统引用。
    return workspace.rootPath.trim();
  }

  function workspacePathRef(workspace: AgentWorkspaceSummary): string {
    return workspace.rootPathRef?.trim() || workspace.rootPath.trim();
  }

  type BrowserAuthorityEventDetail = {
    eventType?: string;
    workspaceId?: string | null;
    sessionId?: string | null;
    payload?: Record<string, unknown>;
  };

  function browserSessionIdsForAuthorityEvent(detail: BrowserAuthorityEventDetail): string[] {
    const payload = detail.payload;
    const ids = new Set<string>();
    const directSessionId = payload?.browser_session_id ?? payload?.browserSessionId;
    if (typeof directSessionId === 'string' && directSessionId.trim()) {
      ids.add(directSessionId.trim());
    }

    // 页面更新事件来自 Chromium Surface，事件 payload 只携带 tab/binding，
    // 不重复带 browser_session_id。此时从已经投影到右栏的 Browser Tab 反查
    // 会话，保证非当前 Tab 的标题、生命周期和关闭状态也能收敛。
    const binding = payload?.binding;
    const bindingTabId = binding && typeof binding === 'object' && !Array.isArray(binding)
      ? (binding as Record<string, unknown>).tab_id
      : undefined;
    const changedTabId = payload?.tab_id ?? bindingTabId;
    if (typeof changedTabId === 'string' && changedTabId.trim()) {
      for (const pane of Object.values(rightPaneState.perSession)) {
        for (const tab of pane.openTabs) {
          if (tab.kind !== 'browser') continue;
          const browser = tab.payload as BrowserTabPayload;
          if (browser.tabId === changedTabId && browser.browserSessionId.trim()) {
            ids.add(browser.browserSessionId.trim());
          }
        }
      }
    }
    return [...ids];
  }

  function synchronizeBrowserProjectionFromAuthorityEvent(event: Event): void {
    const detail = (event as CustomEvent<BrowserAuthorityEventDetail>).detail;
    if (!detail || typeof detail !== 'object') return;
    const browserSessionIds = browserSessionIdsForAuthorityEvent(detail);
    for (const browserSessionId of browserSessionIds) {
      void loadBrowserAuthoritySession(browserSessionId, (snapshot) => {
        const workspaceId = snapshot.workspaceId?.trim() || detail.workspaceId?.trim() || '';
        const workspacePath = workspaceId ? workspacePathForId(workspaceId) : '';
        synchronizeBrowserSessionSnapshot(snapshot, workspacePath, {
          workspaceId,
          sessionId: snapshot.sessionId,
        });
      }).catch((error) => {
        // 浏览器事件可能在 Host 收口期间早于 HTTP 快照可读；事件流后续
        // 仍会再次触发同步，单次快照失败不能让右栏投影永久停在旧 Tab。
        console.warn('[WebWorkbenchShell] 同步浏览器权威投影失败:', error);
      });
    }
  }

  function resolveBackendWorkspaceSelection(nextWorkspaces: AgentWorkspaceSummary[]): string {
    const authoritativeWorkspaceId = currentBootstrapWorkspaceId();
    if (authoritativeWorkspaceId && nextWorkspaces.some((workspace) => workspace.workspaceId === authoritativeWorkspaceId)) {
      return authoritativeWorkspaceId;
    }
    const requestedWorkspaceId = currentWorkspaceBinding().workspaceId;
    if (requestedWorkspaceId && nextWorkspaces.some((workspace) => workspace.workspaceId === requestedWorkspaceId)) {
      return requestedWorkspaceId;
    }
    const requestedWorkspacePath = currentWorkspaceBinding().workspacePath;
    const requestedWorkspace = requestedWorkspacePath
      ? nextWorkspaces.find((workspace) => workspace.rootPath?.trim() === requestedWorkspacePath)
      : null;
    if (requestedWorkspace) {
      return requestedWorkspace.workspaceId;
    }
    return nextWorkspaces.find((workspace) => workspace.isActive)?.workspaceId
      || nextWorkspaces[0]?.workspaceId
      || '';
  }

  // 列表同步 effect：按 workspaceId 分片接收目录投影，不能用当前草稿工作区
  // 推断归属。这样后台工作区的运行态变化不会覆盖当前主投影，也不会被丢弃。
  $effect(() => {
    const projections = messagesState.workspaceSessionProjections;
    const nextSessionsByWorkspace = { ...sessionsByWorkspace };
    let changed = false;
    for (const [workspaceId, projection] of Object.entries(projections)) {
      const normalizedWorkspaceId = workspaceId.trim();
      if (!normalizedWorkspaceId) {
        continue;
      }
      const projectionRuntimeEpoch = projection.runtimeEpoch?.trim() || '';
      const projectionNextSequence = projection.eventStreamNextSequence;
      if (projectionRuntimeEpoch && projectionNextSequence >= 1) {
        workspaceSessionCursorByWorkspace.set(normalizedWorkspaceId, {
          runtimeEpoch: projectionRuntimeEpoch,
          eventStreamNextSequence: projectionNextSequence,
        });
      }
      const currentSessions = projection.sessions;
      const existingSessions = nextSessionsByWorkspace[normalizedWorkspaceId] ?? [];
      const sessionsChanged = existingSessions.length !== currentSessions.length
        || existingSessions.some((session, index) => {
          const next = currentSessions[index];
          return !next
            || session.id !== next.id
            || session.name !== next.name
            || session.updatedAt !== next.updatedAt
            || session.messageCount !== next.messageCount
            || session.isRunning !== next.isRunning
            || session.runningTaskCount !== next.runningTaskCount
            || session.hasUnreadCompletion !== next.hasUnreadCompletion;
        });
      if (sessionsChanged) {
        nextSessionsByWorkspace[normalizedWorkspaceId] = currentSessions;
        changed = true;
      }
    }
    if (changed) {
      sessionsByWorkspace = nextSessionsByWorkspace;
    }
  });

  $effect(() => {
    const workspaceId = currentBootstrapWorkspaceId();
    const sessionId = typeof messagesState.currentSessionId === 'string'
      ? messagesState.currentSessionId.trim()
      : '';
    if (!sessionId) {
      return;
    }
    const session = workspaceId
      ? (sessionsByWorkspace[workspaceId] ?? []).find((candidate) => candidate.id === sessionId)
      : recentSessions.find((candidate) => candidate.id === sessionId);
    if (!session) {
      return;
    }
    const isRunning = isSessionRunning(workspaceId, session);
    if (!shouldMarkSessionCompletionViewed({
      bootstrapped: messagesState.bootstrapped === true,
      sessionHydrating: messagesState.sessionHydrating === true,
      isCurrentSession: sessionId === currentSessionId
        && (!workspaceId || workspaceId === selectedWorkspaceId),
      isRunning,
      hasUnreadCompletion: session.hasUnreadCompletion === true,
    })) {
      return;
    }
    const requestKey = `${workspaceId || 'personal'}:${sessionId}`;
    if (sessionViewedRequests.has(requestKey)) {
      return;
    }
    sessionViewedRequests.add(requestKey);
    const workspacePath = workspaceId ? workspacePathForId(workspaceId) : '';
    const binding: AgentBindingOverride = workspaceId
      ? { scope: 'workspace', workspaceId, workspacePath, sessionId }
      : { scope: 'personal', sessionId };
    void markAgentSessionViewed(sessionId, binding).then((result) => {
      const cursor = {
        runtimeEpoch: result.runtimeEpoch,
        eventStreamNextSequence: result.eventStreamNextSequence,
      };
      if (workspaceId) {
        advanceWorkspaceSessionProjectionCursor(workspaceId, cursor);
        workspaceSessionCursorByWorkspace.set(workspaceId, cursor);
      }
      const currentSessions = workspaceId ? (sessionsByWorkspace[workspaceId] ?? []) : recentSessions;
      const nextSessions = currentSessions.map((candidate) => (
        candidate.id === sessionId
          ? { ...candidate, hasUnreadCompletion: false }
          : candidate
      ));
      if (workspaceId) {
        sessionsByWorkspace = {
          ...sessionsByWorkspace,
          [workspaceId]: nextSessions,
        };
      } else {
        recentSessions = nextSessions;
      }
      if (workspaceId && currentBootstrapWorkspaceId() === workspaceId) {
        updateWorkspaceSessionProjectionSessions(workspaceId, nextSessions);
      }
    }).catch((error) => {
      console.warn('[WebWorkbenchShell] 标记会话已查看失败:', error);
    }).finally(() => {
      sessionViewedRequests.delete(requestKey);
    });
  });

  // 工作区指针同步 effect：消息状态是已提交导航的唯一真值。导航事务完成前，
  // 侧栏保持当前已提交选择，不制造第二套乐观指针。
  $effect(() => {
    const authoritativeWorkspaceId = currentBootstrapWorkspaceId();
    if (!authoritativeWorkspaceId || loading || workspaceActionPending) {
      return;
    }
    const bootstrapSessionId = currentBootstrapSessionIdForWorkspace(authoritativeWorkspaceId);
    if (selectedWorkspaceId === authoritativeWorkspaceId) {
      return;
    }
    const workspace = workspaces.find((item) => item.workspaceId === authoritativeWorkspaceId) ?? null;
    if (!workspace) {
      return;
    }
    selectedWorkspaceId = authoritativeWorkspaceId;
    expandedWorkspaceIds = {
      ...expandedWorkspaceIds,
      [authoritativeWorkspaceId]: true,
    };
    currentSessionId = bootstrapSessionId || null;
    const currentSessions = sessionsByWorkspace[authoritativeWorkspaceId] ?? [];
    if (currentSessions.length === 0 || (bootstrapSessionId && !currentSessions.some((session) => session.id === bootstrapSessionId))) {
      void refreshWorkspaceSessions(
        authoritativeWorkspaceId,
        workspace.rootPath,
      );
    }
  });

  // 个人会话没有 workspace 指针，不能沿用上一次工作区的侧栏选择。
  // bootstrap 完成后同步本地侧栏指针，确保最近会话的 active 状态与主对话一致。
  $effect(() => {
    if (!messagesState.bootstrapped || loading || workspaceActionPending) {
      return;
    }
    if (currentBootstrapWorkspaceId()) {
      return;
    }
    const bootstrapSessionId = typeof messagesState.currentSessionId === 'string'
      ? messagesState.currentSessionId.trim()
      : '';
    if (selectedWorkspaceId) {
      selectedWorkspaceId = '';
    }
    const nextSessionId = bootstrapSessionId || null;
    if (currentSessionId !== nextSessionId) {
      currentSessionId = nextSessionId;
    }
  });

  // 激活会话指针同步 effect：把 bootstrap 的 currentSessionId 镜像到本地 currentSessionId。
  // bootstrap 是真值——非空就切过去；空也要镜像为空（删除/关闭/新建当前会话都会让它清空），
  // 否则本地 currentSessionId 和 URL 残留指向已删除的会话。
  $effect(() => {
    const authoritativeWorkspaceId = currentBootstrapWorkspaceId();
    if (!authoritativeWorkspaceId) {
      return;
    }
    if (selectedWorkspaceId !== authoritativeWorkspaceId) {
      return;
    }
    const bootstrapSessionId = typeof messagesState.currentSessionId === 'string'
      ? messagesState.currentSessionId.trim()
      : '';
    const workspace = workspaces.find((item) => item.workspaceId === selectedWorkspaceId) ?? null;
    if (bootstrapSessionId === currentSessionId) {
      const hasLoadedSessionList = Object.prototype.hasOwnProperty.call(
        sessionsByWorkspace,
        authoritativeWorkspaceId,
      );
      if (
        !bootstrapSessionId
        && workspace
        && !hasLoadedSessionList
        && !loadingWorkspaceIds[authoritativeWorkspaceId]
      ) {
        void loadWorkspaceSessionsForSidebar(workspace);
      }
      return;
    }

    if (!bootstrapSessionId) {
      // 当前会话指针与工作区会话目录是两个独立投影。进入草稿只同步指针和 URL，
      // 目录只能由显式加载或增删改结果更新，不能因指针清空而重新请求。
      invalidateWorkspaceSessionRequests(authoritativeWorkspaceId);
      currentSessionId = '';
      return;
    }

    // 非空：必须存在于当前工作区列表里，避免把别工作区的会话错激活到本地视图
    const belongsToSelectedWorkspace = (sessionsByWorkspace[selectedWorkspaceId] ?? [])
      .some((session) => session.id === bootstrapSessionId);
    if (!belongsToSelectedWorkspace) {
      // 新会话首条消息会先建立本地 session 身份，再等待服务端 accepted。
      // 这时任何更早发出的目录请求都不再具备清理当前指针的因果资格；先使其
      // 失效，accepted 后再由带事件游标的新快照接管目录。
      invalidateWorkspaceSessionRequests(authoritativeWorkspaceId);
      return;
    }
    currentSessionId = bootstrapSessionId;
  });

  function getWorkspaceSessionList(workspaceId: string): Session[] {
    return (sessionsByWorkspace[workspaceId] ?? []).filter((session) => !isInternalSession(session));
  }

  function getPersonalSessionList(): Session[] {
    return recentSessions.filter((session) => !isInternalSession(session));
  }

  async function refreshPersonalSessions(): Promise<void> {
    const requestSeq = ++personalSessionRequestSeq;
    try {
      const snapshot = await getPersonalSessions();
      if (requestSeq !== personalSessionRequestSeq) {
        return;
      }
      const applied = replacePersonalSessionProjection(snapshot.sessions, {
        runtimeEpoch: snapshot.runtimeEpoch,
        eventStreamNextSequence: snapshot.eventStreamNextSequence,
      });
      if (applied) {
        recentSessions = snapshot.sessions;
      }
    } catch (error) {
      console.warn('[WebWorkbenchShell] 刷新个人会话失败:', error);
    }
  }

  function openPersonalDraft(): void {
    if (workspaceActionPending || messagesState.sessionHydrating) return;
    navigateSession({ kind: 'draft', scope: 'personal' });
    if (sidebarIsDrawer) sidebarOpen = false;
  }

  function switchPersonalSession(sessionId: string): void {
    if (!sessionId) return;
    navigateSession({ kind: 'session', scope: 'personal', sessionId });
    if (sidebarIsDrawer) sidebarOpen = false;
  }

  function isSessionRunning(workspaceId: string, session: Session): boolean {
    return resolveSessionRunningState({
      isRunning: session.isRunning,
      runningTaskCount: session.runningTaskCount,
      isCurrentSession: session.id === currentSessionId,
      isCurrentWorkspace: workspaceId === selectedWorkspaceId,
      isPersonalScope: !workspaceId,
      hasActiveWorkspace: Boolean(currentBootstrapWorkspaceId()),
      isProcessing: messagesState.isProcessing === true,
    });
  }

  function isInternalSession(session: Session): boolean {
    const name = (session.name || '').trim();
    const preview = (session.preview || '').trim();
    return INTERNAL_SESSION_NAME_PATTERNS.some((pattern) => pattern.test(name))
      && (session.messageCount ?? 0) === 0
      && (!preview || preview === '新对话');
  }

  function formatRelativeTime(timestamp: string | number | Date | null | undefined): string {
    if (!timestamp) return '';
    const date = new Date(timestamp);
    const ms = Date.now() - date.getTime();
    if (Number.isNaN(ms) || ms < 0) {
      return date.toLocaleDateString(i18n.locale, { month: 'short', day: 'numeric' });
    }
    const isZh = (i18n.locale || '').toLowerCase().startsWith('zh');
    const minutes = Math.floor(ms / 60000);
    if (minutes < 1) return isZh ? '刚刚' : 'just now';
    if (minutes < 60) return isZh ? `${minutes} 分钟` : `${minutes}m`;
    const hours = Math.floor(ms / 3600000);
    if (hours < 24) return isZh ? `${hours} 小时` : `${hours}h`;
    const days = Math.floor(ms / 86400000);
    if (days < 30) return isZh ? `${days} 天` : `${days}d`;
    return date.toLocaleDateString(i18n.locale, { month: 'short', day: 'numeric' });
  }


  const themeIconName = $derived.by<IconName>(() => {
    if (appearanceRuntime.activeTheme?.id === 'builtin.system') return 'monitor';
    return appearanceRuntime.mode === 'light' ? 'sun' : 'moon';
  });
  const themeToggleTitle = $derived.by(() => {
    return appearanceRuntime.activeTheme?.name || i18n.t('web.themeSystem');
  });

  function toggleWebTheme(): void {
    void cycleBuiltinAppearance().catch((error) => {
      const message = directIncidentError(error, i18n.t('appearance.applyFailed'));
      reportIncident(message, {
        scope: 'workspace',
        title: i18n.t('appearance.applyFailed'),
        ...incidentErrorDiagnostics(error, message),
        failureStage: 'appearance_activation',
        source: 'appearance-runtime',
      });
    });
  }

  function setWorkspaceSessionLoading(workspaceId: string, isLoading: boolean): void {
    if (isLoading) {
      if (loadingWorkspaceIds[workspaceId]) {
        return;
      }
      loadingWorkspaceIds = { ...loadingWorkspaceIds, [workspaceId]: true };
      return;
    }
    if (!Object.prototype.hasOwnProperty.call(loadingWorkspaceIds, workspaceId)) {
      return;
    }
    const nextLoadingWorkspaceIds = { ...loadingWorkspaceIds };
    delete nextLoadingWorkspaceIds[workspaceId];
    loadingWorkspaceIds = nextLoadingWorkspaceIds;
  }

  function workspaceSessionCursor(
    snapshot: Awaited<ReturnType<typeof getWorkspaceSessions>>,
  ): WorkspaceSessionProjectionCursor {
    return {
      runtimeEpoch: snapshot.runtimeEpoch,
      eventStreamNextSequence: snapshot.eventStreamNextSequence,
    };
  }

  function commitSidebarWorkspaceSessionsSnapshot(
    workspaceId: string,
    snapshot: Awaited<ReturnType<typeof getWorkspaceSessions>>,
  ): boolean {
    const normalizedWorkspaceId = workspaceId.trim();
    const incomingCursor = workspaceSessionCursor(snapshot);
    if (!canApplyWorkspaceSessionProjectionCursor(normalizedWorkspaceId, incomingCursor)) {
      return false;
    }
    const currentCursor = workspaceSessionCursorByWorkspace.get(normalizedWorkspaceId);
    if (
      currentCursor
      && currentCursor.runtimeEpoch === incomingCursor.runtimeEpoch
      && currentCursor.eventStreamNextSequence > incomingCursor.eventStreamNextSequence
    ) {
      return false;
    }
    workspaceSessionCursorByWorkspace.set(normalizedWorkspaceId, incomingCursor);
    sessionsByWorkspace = {
      ...sessionsByWorkspace,
      [normalizedWorkspaceId]: snapshot.sessions,
    };
    return true;
  }

  function beginWorkspaceSessionRequest(workspaceId: string): number {
    const requestSeq = ++workspaceSessionRequestSeq;
    workspaceSessionRequestSeqByWorkspace.set(workspaceId, requestSeq);
    setWorkspaceSessionLoading(workspaceId, true);
    return requestSeq;
  }

  function finishWorkspaceSessionRequest(workspaceId: string, requestSeq: number): void {
    if (workspaceSessionRequestSeqByWorkspace.get(workspaceId) !== requestSeq) {
      return;
    }
    workspaceSessionRequestSeqByWorkspace.delete(workspaceId);
    setWorkspaceSessionLoading(workspaceId, false);
  }

  function invalidateWorkspaceSessionRequests(workspaceId?: string): void {
    const normalizedWorkspaceId = workspaceId?.trim() || '';
    if (normalizedWorkspaceId) {
      workspaceSessionRequestSeqByWorkspace.delete(normalizedWorkspaceId);
      setWorkspaceSessionLoading(normalizedWorkspaceId, false);
      return;
    }
    workspaceSessionRequestSeqByWorkspace.clear();
    loadingWorkspaceIds = {};
  }

  function applyWorkspaceSessionsSnapshot(
    workspaceId: string,
    snapshot: Awaited<ReturnType<typeof getWorkspaceSessions>>,
  ): void {
    const requestedWorkspaceId = workspaceId.trim();
    const authoritativeWorkspaceId = snapshot.workspace.workspaceId?.trim() || requestedWorkspaceId;
    if (!authoritativeWorkspaceId) {
      return;
    }
    if (!commitSidebarWorkspaceSessionsSnapshot(authoritativeWorkspaceId, snapshot)) {
      return;
    }

    const requestStillTargetsSelection = selectedWorkspaceId === requestedWorkspaceId
      || selectedWorkspaceId === authoritativeWorkspaceId;
    if (
      selectedWorkspaceId === requestedWorkspaceId
      && selectedWorkspaceId !== authoritativeWorkspaceId
      && workspaces.some((workspace) => workspace.workspaceId === authoritativeWorkspaceId)
    ) {
      selectedWorkspaceId = authoritativeWorkspaceId;
      expandedWorkspaceIds = {
        ...expandedWorkspaceIds,
        [authoritativeWorkspaceId]: true,
      };
    }
    const isStillSelectedWorkspace = requestStillTargetsSelection
      && selectedWorkspaceId === authoritativeWorkspaceId;
    if (!isStillSelectedWorkspace) {
      return;
    }
    replaceWorkspaceSessionProjection(
      authoritativeWorkspaceId,
      snapshot.sessions,
      workspaceSessionCursor(snapshot),
    );
  }

  function notifyWorkbenchError(actionLabel: string, error: unknown): void {
    console.warn(`[WebWorkbenchShell] ${actionLabel} failed:`, error);
    const title = i18n.t('web.workbenchActionFailed', { action: actionLabel });
    const directError = directIncidentError(error, title);
    reportIncident(directError, {
      scope: 'workspace',
      title,
      ...incidentErrorDiagnostics(error, directError),
      failureStage: 'web_workbench',
      source: 'web-workbench',
    });
  }

  function clampSidebarWidth(width: number): number {
    return Math.max(MIN_SIDEBAR_WIDTH, Math.min(MAX_SIDEBAR_WIDTH, Math.round(width)));
  }

  function clampPreviewPanelWidth(width: number): number {
    if (typeof window === 'undefined') {
      return Math.max(PANEL_LAYOUT.minPreviewWidth, Math.round(width));
    }
    const vw = viewportWidth || window.innerWidth;
    const bounds = resolvePreviewPanelWidthBounds({
      viewportWidth: vw,
      sidebarWidth: effectiveSidebarWidth,
      sidebarVisible: panelVisibility.sidebarVisible,
      rightPaneOpen: inlineRightPaneVisible,
      previewOverlay: panelLayout.previewOverlay,
    });
    return Math.max(
      bounds.minWidth,
      Math.min(bounds.maxWidth, Math.round(width)),
    );
  }

  function loadStoredSidebarWidth(): void {
    if (typeof window === 'undefined') {
      return;
    }
    const stored = Number.parseInt(window.localStorage.getItem(SIDEBAR_WIDTH_STORAGE_KEY) || '', 10);
    if (Number.isFinite(stored)) {
      sidebarWidth = clampSidebarWidth(stored);
    }
  }

  function loadStoredPreviewPanelWidth(): void {
    if (typeof window === 'undefined' || desktopAppSurface) {
      return;
    }
    const stored = Number.parseInt(window.localStorage.getItem(PREVIEW_PANEL_WIDTH_STORAGE_KEY) || '', 10);
    if (Number.isFinite(stored)) {
      previewPanelWidth = clampPreviewPanelWidth(stored);
    }
  }

  function persistSidebarWidth(width: number): void {
    if (typeof window === 'undefined') {
      return;
    }
    window.localStorage.setItem(SIDEBAR_WIDTH_STORAGE_KEY, String(clampSidebarWidth(width)));
  }

  function persistPreviewPanelWidth(width: number): void {
    if (typeof window === 'undefined' || desktopAppSurface) {
      return;
    }
    window.localStorage.setItem(PREVIEW_PANEL_WIDTH_STORAGE_KEY, String(clampPreviewPanelWidth(width)));
  }

  function loadStoredSidebarCollapsed(): void {
    if (typeof window === 'undefined') {
      return;
    }
    sidebarCollapsed = window.localStorage.getItem(SIDEBAR_COLLAPSED_STORAGE_KEY) === '1';
  }

  function persistSidebarCollapsed(collapsed: boolean): void {
    if (typeof window === 'undefined') {
      return;
    }
    if (collapsed) {
      window.localStorage.setItem(SIDEBAR_COLLAPSED_STORAGE_KEY, '1');
    } else {
      window.localStorage.removeItem(SIDEBAR_COLLAPSED_STORAGE_KEY);
    }
  }

  // ============================================================================
  // 左侧 sidebar 展开列表 / 模式 持久化
  // - 用同步 reader 函数作为 $state 初始值；函数声明在 JS 里是 hoist 的，可以放在引用点之后。
  // - 用 $effect 自动持久化：deep reactive proxy 任何字段变化都会触发；避免在每个 mutation
  //   末尾手写 persist 调用，新增 mutation 也不会漏。
  function readInitialExpandedWorkspaces(): Record<string, boolean> {
    if (typeof window === 'undefined') return {};
    try {
      const raw = window.localStorage.getItem(SIDEBAR_EXPANDED_WORKSPACES_KEY);
      if (!raw) return {};
      const parsed = JSON.parse(raw);
      if (!parsed || typeof parsed !== 'object') return {};
      // 防御性 sanitize：保证只保留 boolean 值，过滤掉非法/老格式
      const result: Record<string, boolean> = {};
      for (const [key, value] of Object.entries(parsed)) {
        if (typeof key === 'string' && typeof value === 'boolean') {
          result[key] = value;
        }
      }
      return result;
    } catch {
      return {};
    }
  }

  function persistExpandedWorkspaces(): void {
    if (typeof window === 'undefined') return;
    try {
      window.localStorage.setItem(
        SIDEBAR_EXPANDED_WORKSPACES_KEY,
        JSON.stringify(expandedWorkspaceIds),
      );
    } catch {
      // QuotaExceededError 等 → 静默忽略
    }
  }

  function readInitialSidebarMode(): 'projects' | 'files' {
    if (typeof window === 'undefined') return 'projects';
    const stored = window.localStorage.getItem(SIDEBAR_MODE_KEY);
    return stored === 'files' ? 'files' : 'projects';
  }

  function readInitialRecentSessionsCollapsed(): boolean {
    if (typeof window === 'undefined') return false;
    return window.localStorage.getItem(SIDEBAR_RECENT_SESSIONS_COLLAPSED_KEY) === '1';
  }

  function readInitialWorkspacesCollapsed(): boolean {
    if (typeof window === 'undefined') return false;
    return window.localStorage.getItem(SIDEBAR_WORKSPACES_COLLAPSED_KEY) === '1';
  }

  function persistWorkspacesCollapsed(): void {
    if (typeof window === 'undefined') return;
    try {
      if (workspacesCollapsed) {
        window.localStorage.setItem(SIDEBAR_WORKSPACES_COLLAPSED_KEY, '1');
      } else {
        window.localStorage.removeItem(SIDEBAR_WORKSPACES_COLLAPSED_KEY);
      }
    } catch {
      // 持久化失败不影响当前导航状态。
    }
  }

  function persistRecentSessionsCollapsed(): void {
    if (typeof window === 'undefined') return;
    try {
      if (recentSessionsCollapsed) {
        window.localStorage.setItem(SIDEBAR_RECENT_SESSIONS_COLLAPSED_KEY, '1');
      } else {
        window.localStorage.removeItem(SIDEBAR_RECENT_SESSIONS_COLLAPSED_KEY);
      }
    } catch {
      // 持久化失败不影响当前导航状态。
    }
  }

  function persistSidebarMode(): void {
    if (typeof window === 'undefined') return;
    try {
      window.localStorage.setItem(SIDEBAR_MODE_KEY, sidebarMode);
    } catch {
      // 静默忽略
    }
  }

  // 自动持久化挂载点；$state proxy 是深度 reactive 的，任何变化都会重新触发 persist。
  $effect(() => {
    persistExpandedWorkspaces();
  });
  $effect(() => {
    persistSidebarMode();
  });
  $effect(() => {
    persistRecentSessionsCollapsed();
  });
  $effect(() => {
    persistWorkspacesCollapsed();
  });

  function toggleRecentSessions(): void {
    recentSessionsCollapsed = !recentSessionsCollapsed;
  }

  function toggleWorkspaces(): void {
    workspacesCollapsed = !workspacesCollapsed;
  }

  function resetSidebarWidth(): void {
    const width = sidebarIsDrawer ? DEFAULT_SIDEBAR_WIDTH : window.innerWidth <= 1120 ? COMPACT_SIDEBAR_WIDTH : DEFAULT_SIDEBAR_WIDTH;
    sidebarWidth = width;
    persistSidebarWidth(width);
  }

  function startSidebarResize(event: PointerEvent): void {
    if (sidebarIsDrawer) {
      return;
    }
    const sidebarRect = sidebarElement?.getBoundingClientRect();
    if (!sidebarRect || sidebarRect.width <= 0) return;
    event.preventDefault();
    isSidebarResizing = true;
    document.body.style.cursor = 'col-resize';
    document.body.style.userSelect = 'none';

    const handlePointerMove = (moveEvent: PointerEvent) => {
      sidebarWidth = clampSidebarWidth(moveEvent.clientX - sidebarRect.left);
    };
    const handlePointerUp = () => {
      isSidebarResizing = false;
      document.body.style.cursor = '';
      document.body.style.userSelect = '';
      if (sidebarWidth) {
        persistSidebarWidth(sidebarWidth);
      }
      window.removeEventListener('pointermove', handlePointerMove);
      window.removeEventListener('pointerup', handlePointerUp);
    };

    window.addEventListener('pointermove', handlePointerMove);
    window.addEventListener('pointerup', handlePointerUp);
  }

  function resetPreviewPanelWidth(): void {
    previewPanelWidth = clampPreviewPanelWidth(DEFAULT_PREVIEW_PANEL_WIDTH);
    persistPreviewPanelWidth(previewPanelWidth);
  }

  function startPreviewPanelResize(event: PointerEvent): void {
    if (previewIsOverlay) {
      return;
    }
    const shellRect = workbenchElement?.getBoundingClientRect();
    if (!shellRect || shellRect.width <= 0) return;
    event.preventDefault();
    isPreviewPanelResizing = true;
    document.body.style.cursor = 'col-resize';
    document.body.style.userSelect = 'none';

    const handlePointerMove = (moveEvent: PointerEvent) => {
      previewPanelWidth = clampPreviewPanelWidth(
        shellRect.right - moveEvent.clientX,
      );
    };
    const handlePointerUp = () => {
      isPreviewPanelResizing = false;
      document.body.style.cursor = '';
      document.body.style.userSelect = '';
      if (previewPanelWidth) {
        persistPreviewPanelWidth(previewPanelWidth);
      }
      window.removeEventListener('pointermove', handlePointerMove);
      window.removeEventListener('pointerup', handlePointerUp);
    };

    window.addEventListener('pointermove', handlePointerMove);
    window.addEventListener('pointerup', handlePointerUp);
  }

  function submitPendingDesktopRightPaneWidth(): void {
    desktopRightPaneResizeFrame = null;
    const width = pendingDesktopRightPaneWidth;
    pendingDesktopRightPaneWidth = null;
    if (width === null || !window.magiDesktop) return;
    void window.magiDesktop.submitLayoutIntent({ type: 'right_pane_width', width })
      .catch((error) => console.warn('[WebWorkbenchShell] 调整桌面右栏宽度失败:', error));
  }

  function resetDesktopRightPaneWidth(): void {
    if (!desktopAppSurface) return;
    void window.magiDesktop?.submitLayoutIntent({ type: 'right_pane_reset_width' })
      .catch((error) => console.warn('[WebWorkbenchShell] 重置桌面右栏宽度失败:', error));
  }

  function startDesktopRightPaneResize(event: PointerEvent): void {
    if (!desktopAppSurface) return;
    if (!window.magiDesktop) return;
    const handle = event.currentTarget instanceof HTMLElement ? event.currentTarget : null;
    if (!handle) return;
    const rightPaneElement = workbenchElement?.querySelector('.desktop-right-pane-column');
    const rightPaneRect = rightPaneElement?.getBoundingClientRect();
    if (!rightPaneRect || rightPaneRect.width <= 0) return;
    // Main 与 Renderer 使用同一个定义：这里的宽度只代表右栏内容轨道，
    // 分隔条和 shell 外边距不再混入拖动值。
    const widthBounds = resolvePreviewPanelWidthBounds({
      viewportWidth: workbenchElement?.getBoundingClientRect().width ?? viewportWidth,
      sidebarWidth: effectiveSidebarWidth,
      sidebarVisible: panelVisibility.sidebarVisible,
      rightPaneOpen: true,
      previewOverlay: false,
      desktopSurface: true,
    });
    const minWidth = widthBounds.minWidth;
    const maxWidth = widthBounds.maxWidth;
    const initialWidth = Math.round(
      rightPaneRect.width,
    );
    if (!initialWidth) return;
    event.preventDefault();
    desktopRightPaneResizeCancel?.();
    isPreviewPanelResizing = true;
    const initialRight = rightPaneRect.right;
    const pointerId = event.pointerId;
    let stopped = false;
    try {
      handle.setPointerCapture(pointerId);
    } catch {
      // 指针捕获不是所有桌面 WebView 都支持，窗口级监听仍是唯一拖拽事件源。
    }
    document.body.style.cursor = 'col-resize';
    document.body.style.userSelect = 'none';
    const move = (moveEvent: PointerEvent) => {
      if (moveEvent.pointerId !== pointerId) return;
      pendingDesktopRightPaneWidth = Math.min(
        maxWidth,
        Math.max(minWidth, initialRight - moveEvent.clientX),
      );
      if (desktopRightPaneResizeFrame === null) {
        desktopRightPaneResizeFrame = requestAnimationFrame(submitPendingDesktopRightPaneWidth);
      }
    };
    const stop = (stopEvent?: PointerEvent) => {
      if (stopEvent && stopEvent.pointerId !== pointerId) return;
      if (stopped) return;
      stopped = true;
      if (desktopRightPaneResizeCancel) desktopRightPaneResizeCancel = null;
      isPreviewPanelResizing = false;
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', stop);
      window.removeEventListener('pointercancel', stop);
      document.body.style.cursor = '';
      document.body.style.userSelect = '';
      if (desktopRightPaneResizeFrame !== null) cancelAnimationFrame(desktopRightPaneResizeFrame);
      submitPendingDesktopRightPaneWidth();
      try {
        if (handle.hasPointerCapture(pointerId)) handle.releasePointerCapture(pointerId);
      } catch {
        // 捕获未建立时无需释放。
      }
    };
    desktopRightPaneResizeCancel = () => stop();
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', stop);
    window.addEventListener('pointercancel', stop);
  }

  /**
   * 把文件推到右栏的 code tab。
   * - 文件元信息（contentKind / size / mime / symlinkTarget / head|tailSummary）通过 store 透传给 RightPane
   * - 内容拉取在 RightPane 内部按 filepath 触发，shell 不再持有单文件状态
   */
  function handleFileSelect(
    filePath: string,
    metadata: {
      workspaceId?: string;
      workspacePath?: string;
      sessionId?: string;
      contentKind?: EditContentKind;
      size?: number;
      mime?: string;
      symlinkTarget?: string;
      headSummary?: string;
      tailSummary?: string;
      imageDataUrl?: string;
      displayPath?: string;
      label?: string;
    } = {},
  ): boolean {
    const currentBinding = currentWorkspaceBinding();
    const { workspaceId, workspacePath, sessionId, imageDataUrl } = resolveFilePreviewScope({
      metadataWorkspaceId: metadata.workspaceId,
      metadataWorkspacePath: metadata.workspacePath,
      metadataSessionId: metadata.sessionId,
      imageDataUrl: metadata.imageDataUrl,
      currentBinding,
      selectedWorkspaceId: selectedWorkspace?.workspaceId || selectedWorkspaceId,
      selectedWorkspacePath: selectedWorkspace ? workspaceNavigationPath(selectedWorkspace) : '',
      workspacePathForId,
      activeWorkspaceId: rightPaneState.activeWorkspaceId,
      activeSessionId: rightPaneState.activeSessionId,
    });
    // 文件树事件不携带 session 元数据。桌面端的右栏是独立 Renderer，不能假设
    // 它已经先于文件点击完成上下文同步；优先使用当前工作区的权威会话，再由
    // right-pane store 处理无会话的 workspace 草稿，避免代码/图片 Tab 被投影到
    // personal 或旧浏览器 scope 中。
    // 浏览器 view_image 的结果已经携带内存中的 PNG，不属于工作区文件。
    // 个人会话也必须能在右栏直接打开它，不能继续落到 bridge 的文件预览
    // 请求，否则后端会把绝对 artifact 路径误送进 workspace 变更投影。
    if ((!workspaceId || !workspacePath) && !imageDataUrl) {
      return false;
    }
    const normalizedFilePath = filePath.trim();
    if (!normalizedFilePath) {
      return false;
    }
    if (desktopAppSurface && isHtmlFile(metadata.displayPath || normalizedFilePath)) {
      // HTML 文件在桌面端代表可运行的网页入口。请求放在 Shell 状态中，
      // 由右栏组件消费，避免右栏懒加载或主 Renderer 切换布局时丢事件。
      htmlBrowserOpenRequest = {
        requestId: ++htmlBrowserOpenRequestId,
        filepath: normalizedFilePath,
        workspaceId,
        workspacePath,
        sessionId,
      };
      requestRightPaneVisibility(true);
    } else {
      htmlBrowserOpenRequest = null;
      openCodeTab(sessionId, normalizedFilePath, {
        displayPath: metadata.displayPath,
        label: metadata.label,
        workspaceId,
        workspacePath,
        sessionId,
        contentKind: metadata.contentKind,
        size: metadata.size,
        mime: metadata.mime,
        symlinkTarget: metadata.symlinkTarget,
        headSummary: metadata.headSummary,
        tailSummary: metadata.tailSummary,
        imageDataUrl: imageDataUrl || undefined,
      });
    }
    if (sidebarIsDrawer) {
      sidebarOpen = false;
    }
    return true;
  }

  async function refreshWorkspaceSessions(
    workspaceId: string,
    workspacePath = '',
  ): Promise<void> {
    const requestedWorkspaceId = workspaceId.trim();
    if (!requestedWorkspaceId) {
      return;
    }
    const requestSeq = beginWorkspaceSessionRequest(requestedWorkspaceId);
    try {
      const snapshot = await getWorkspaceSessions(requestedWorkspaceId, workspacePath);
      if (workspaceSessionRequestSeqByWorkspace.get(requestedWorkspaceId) !== requestSeq) {
        return;
      }
      applyWorkspaceSessionsSnapshot(requestedWorkspaceId, snapshot);
    } catch (error) {
      if (workspaceSessionRequestSeqByWorkspace.get(requestedWorkspaceId) === requestSeq) {
        notifyWorkbenchError(i18n.t('web.action.loadWorkspaceSessions'), error);
      }
    } finally {
      finishWorkspaceSessionRequest(requestedWorkspaceId, requestSeq);
    }
  }

  async function loadWorkspaceSessionsForSidebar(workspace: AgentWorkspaceSummary): Promise<boolean> {
    const requestedWorkspaceId = workspace.workspaceId.trim();
    if (!requestedWorkspaceId) {
      return false;
    }
    const requestSeq = beginWorkspaceSessionRequest(requestedWorkspaceId);
    try {
      const snapshot = await getWorkspaceSessions(requestedWorkspaceId, workspaceNavigationPath(workspace));
      if (workspaceSessionRequestSeqByWorkspace.get(requestedWorkspaceId) !== requestSeq) {
        return false;
      }
      commitSidebarWorkspaceSessionsSnapshot(requestedWorkspaceId, snapshot);
      return true;
    } catch (error) {
      if (workspaceSessionRequestSeqByWorkspace.get(requestedWorkspaceId) === requestSeq) {
        notifyWorkbenchError(i18n.t('web.action.loadWorkspaceSessions'), error);
      }
      return false;
    } finally {
      finishWorkspaceSessionRequest(requestedWorkspaceId, requestSeq);
    }
  }

  async function refreshWorkspaces(): Promise<void> {
    const requestSeq = ++workspaceListRequestSeq;
    loading = true;
    loadError = '';
    agentBaseUrl = resolveAgentBaseUrl();
    try {
      const next = await listAgentWorkspaces();
      if (requestSeq !== workspaceListRequestSeq) {
        return;
      }
      workspaces = next;
      invalidateWorkspaceSessionRequests();
      const nextWorkspaceIds = new Set(next.map((workspace) => workspace.workspaceId));
      sessionsByWorkspace = Object.fromEntries(
        Object.entries(sessionsByWorkspace).filter(([workspaceId]) => nextWorkspaceIds.has(workspaceId)),
      );
      for (const workspaceId of workspaceSessionCursorByWorkspace.keys()) {
        if (!nextWorkspaceIds.has(workspaceId)) {
          workspaceSessionCursorByWorkspace.delete(workspaceId);
        }
      }
      expandedWorkspaceIds = {};
      // 首次启动时 bootstrap 尚未返回，工作区列表的 isActive 只是工作区管理状态，
      // 不能抢先作为会话导航真值。否则会发起显式 workspace bootstrap，覆盖 daemon
      // 已持久化的最后会话选择。首次会话加载统一等待 bootstrap 权威状态。
      if (!messagesState.bootstrapped) {
        return;
      }
      selectedWorkspaceId = resolveBackendWorkspaceSelection(next);
      if (selectedWorkspaceId) {
        expandedWorkspaceIds = { [selectedWorkspaceId]: true };
        const selectedWorkspace = next.find((workspace) => workspace.workspaceId === selectedWorkspaceId);
        const preserveDraftSession = messagesState.bootstrapped
          && !currentBootstrapSessionIdForWorkspace(selectedWorkspaceId);
        if (preserveDraftSession && selectedWorkspace) {
          void loadWorkspaceSessionsForSidebar(selectedWorkspace);
        } else {
          void refreshWorkspaceSessions(
            selectedWorkspaceId,
            workspacePathForId(selectedWorkspaceId),
          );
        }
      }
    } catch (error) {
      if (requestSeq !== workspaceListRequestSeq) {
        return;
      }
      loadError = i18n.t('web.workspaceUnavailable');
      notifyWorkbenchError(i18n.t('web.action.loadWorkspaceList'), error);
    } finally {
      if (requestSeq === workspaceListRequestSeq) {
        loading = false;
      }
    }
  }

  async function registerWorkspaceRoot(rootPath: string): Promise<void> {
    const registration = await registerAgentWorkspace(rootPath);
    const addedWorkspace = registration.workspaces.find(
      (workspace) => workspace.workspaceId === registration.workspaceId,
    ) ?? null;
    if (!addedWorkspace) {
      throw new Error(`注册后未找到工作区: ${registration.workspaceId}`);
    }

    workspaces = registration.workspaces;
    selectedWorkspaceId = addedWorkspace.workspaceId;
    expandedWorkspaceIds = {
      ...expandedWorkspaceIds,
      [addedWorkspace.workspaceId]: true,
    };

    const navigation = navigateSession({
      kind: 'draft',
      scope: 'workspace',
      workspaceId: addedWorkspace.workspaceId,
      workspacePath: workspaceNavigationPath(addedWorkspace),
    });
    if (!navigation) {
      throw new Error(`工作区导航目标无效: ${addedWorkspace.workspaceId}`);
    }
    await waitForSessionNavigation(navigation);
    if (sidebarIsDrawer) sidebarOpen = false;
  }

  async function handleFolderSelected(
    selection: { pathRef: string; displayPath: string; name: string },
  ): Promise<void> {
    if (workspaceActionPending) {
      return;
    }
    workspaceDialogError = '';
    const normalizedRootPath = selection.pathRef.trim();
    if (!normalizedRootPath) {
      return;
    }
    closeAddWorkspaceDialog({ force: true });
    workspaceActionPending = true;
    try {
      await runActionWithFeedback(
        () => registerWorkspaceRoot(normalizedRootPath),
        {
          actionLabel: i18n.t('web.action.addWorkspace'),
          successMessage: i18n.t('web.workspaceAdded'),
        },
      );
    } finally {
      workspaceActionPending = false;
    }
  }

  async function handleDesktopWorkspaceDrop(paths: string[]): Promise<void> {
    if (workspaceActionPending) return;
    const droppedPaths = normalizeDesktopDropPaths(paths);
    if (droppedPaths.length === 0) return;
    workspaceActionPending = true;
    try {
      for (const path of droppedPaths) {
        const result = await resolveAgentPath(path);
        const dropped = resolveDesktopDroppedPath(path, result);
        if (!dropped || dropped.kind !== 'directory') continue;
        await registerWorkspaceRoot(dropped.path);
        return;
      }
      addToast('warning', i18n.t('web.desktopDropDirectoryOnly'));
    } catch (error) {
      console.warn('[WebWorkbenchShell] 拖入工作区失败:', error);
      addToast('error', i18n.t('web.desktopDropWorkspaceFailed'));
    } finally {
      workspaceActionPending = false;
    }
  }

  function openAddWorkspaceDialog(): void {
    if (workspaceActionPending || loadError) {
      return;
    }
    workspaceDialogError = '';
    openWorkspaceFolderPicker('sidebar');
  }

  function closeAddWorkspaceDialog(options: { force?: boolean } = {}): void {
    if (workspaceActionPending && options.force !== true) {
      return;
    }
    workspaceDialogError = '';
    
    closeWorkspaceFolderPicker();
  }

  function openRemoveWorkspaceDialog(workspace: AgentWorkspaceSummary): void {
    if (workspaceActionPending || workspaceSelectionPending || pendingNavigation) {
      return;
    }
    workspaceDialogError = '';
    pendingRemoveWorkspace = workspace;
    
    showRemoveWorkspaceDialog = true;
  }

  function closeRemoveWorkspaceDialog(options: { force?: boolean } = {}): void {
    if (workspaceActionPending && options.force !== true) {
      return;
    }
    workspaceDialogError = '';
    pendingRemoveWorkspace = null;
    
    showRemoveWorkspaceDialog = false;
  }

  async function removeWorkspace(): Promise<void> {
    if (workspaceActionPending || !pendingRemoveWorkspace) {
      return;
    }
    const removedId = pendingRemoveWorkspace.workspaceId;
    const removedName = pendingRemoveWorkspace.name;

    // 立即关闭弹窗，不等 API 返回
    closeRemoveWorkspaceDialog({ force: true });
    workspaceActionPending = true;

    try {
      const next = await runActionWithFeedback(
        () => removeAgentWorkspace(removedId),
        {
          actionLabel: i18n.t('web.action.removeWorkspace'),
          successMessage: i18n.t('web.workspaceRemoved', { name: removedName }),
        },
      );
      if (!next) {
        return;
      }
      invalidateWorkspaceSessionRequests(removedId);
      workspaces = next;
      sessionsByWorkspace = Object.fromEntries(
        Object.entries(sessionsByWorkspace).filter(([workspaceId]) => workspaceId !== removedId)
      );
      workspaceSessionCursorByWorkspace.delete(removedId);
      expandedWorkspaceIds = Object.fromEntries(
        Object.entries(expandedWorkspaceIds).filter(([workspaceId]) => workspaceId !== removedId)
      );

      if (selectedWorkspaceId === removedId) {
        selectedWorkspaceId = resolveBackendWorkspaceSelection(next);
        currentSessionId = null;
        if (selectedWorkspaceId) {
          expandedWorkspaceIds = {
            ...expandedWorkspaceIds,
            [selectedWorkspaceId]: true,
          };
          const nextWorkspace = next.find((workspace) => workspace.workspaceId === selectedWorkspaceId);
          if (nextWorkspace) {
            await loadWorkspaceSessionsForSidebar(nextWorkspace);
            const nextSession = getWorkspaceSessionList(selectedWorkspaceId)[0];
            navigateSession(nextSession
              ? {
                  kind: 'session',
                  scope: 'workspace',
                  workspaceId: selectedWorkspaceId,
                  workspacePath: workspaceNavigationPath(nextWorkspace),
                  sessionId: nextSession.id,
                }
              : {
                  kind: 'draft',
                  scope: 'workspace',
                  workspaceId: selectedWorkspaceId,
                  workspacePath: workspaceNavigationPath(nextWorkspace),
                });
          }
        } else {
          navigateSession({ kind: 'draft', scope: 'personal' });
        }
      }
    } finally {
      workspaceActionPending = false;
    }
  }

  async function selectWorkspace(workspace: AgentWorkspaceSummary): Promise<void> {
    if (workspaceActionPending || messagesState.sessionHydrating || workspaceSelectionPending) {
      return;
    }
    const workspaceId = workspace.workspaceId.trim();
    const workspacePath = workspaceNavigationPath(workspace);
    if (!workspaceId || !workspacePath || workspaceId === selectedWorkspaceId) {
      return;
    }

    workspaceSelectionPending = true;
    try {
      const hasLoadedSessions = Object.prototype.hasOwnProperty.call(sessionsByWorkspace, workspaceId);
      if (!hasLoadedSessions) {
        const loaded = await loadWorkspaceSessionsForSidebar(workspace);
        if (!loaded) {
          return;
        }
      }

      const nextSession = getWorkspaceSessionList(workspaceId)[0];
      navigateSession(nextSession
          ? {
            kind: 'session',
            scope: 'workspace',
            workspaceId,
            workspacePath,
            sessionId: nextSession.id,
          }
          : {
            kind: 'draft',
            scope: 'workspace',
            workspaceId,
            workspacePath,
          });
      if (sidebarIsDrawer) {
        sidebarOpen = false;
      }
    } finally {
      workspaceSelectionPending = false;
    }
  }

  function handleWorkspaceClick(workspace: AgentWorkspaceSummary): void {
    if (workspaceSelectionPending || workspaceActionPending || messagesState.sessionHydrating) {
      return;
    }
    if (workspace.workspaceId === selectedWorkspaceId) {
      toggleWorkspaceExpansion(workspace);
      return;
    }
    expandedWorkspaceIds = {
      ...expandedWorkspaceIds,
      [workspace.workspaceId]: true,
    };
    void selectWorkspace(workspace);
  }

  function toggleWorkspaceExpansion(workspace: AgentWorkspaceSummary): void {
    const isExpanded = !!expandedWorkspaceIds[workspace.workspaceId];
    expandedWorkspaceIds = {
      ...expandedWorkspaceIds,
      [workspace.workspaceId]: !isExpanded,
    };
    if (!isExpanded && getWorkspaceSessionList(workspace.workspaceId).length === 0) {
      void loadWorkspaceSessionsForSidebar(workspace);
    }
  }

  async function openWorkspaceDraft(workspace: AgentWorkspaceSummary): Promise<void> {
    if (workspaceActionPending || messagesState.sessionHydrating) {
      return;
    }
    const workspaceId = workspace.workspaceId.trim();
    const workspacePath = workspaceNavigationPath(workspace);
    if (!workspaceId || !workspacePath) {
      return;
    }

    const alreadyCurrentDraft = !messagesState.currentSessionId?.trim()
      && currentBootstrapWorkspaceId() === workspaceId;
    const sessionsAlreadyLoaded = Object.prototype.hasOwnProperty.call(
      sessionsByWorkspace,
      workspaceId,
    );

    expandedWorkspaceIds = {
      ...expandedWorkspaceIds,
      [workspaceId]: true,
    };
    if (!alreadyCurrentDraft) {
      navigateSession({ kind: 'draft', scope: 'workspace', workspaceId, workspacePath });
    }
    if (sidebarIsDrawer) {
      sidebarOpen = false;
    }

    if (!sessionsAlreadyLoaded) {
      await loadWorkspaceSessionsForSidebar(workspace);
    }
  }

  function switchSession(workspace: AgentWorkspaceSummary, sessionId: string): void {
    const isCurrentSelection = workspace.workspaceId === selectedWorkspaceId && sessionId === currentSessionId;
    if (!sessionId || isCurrentSelection) {
      return;
    }
    const nextSession = (sessionsByWorkspace[workspace.workspaceId] ?? []).find((session) => session.id === sessionId);
    const nextSessionName = nextSession?.name || i18n.t('header.unnamedSession');
    addToast('info', i18n.t('web.sessionSwitching', { name: nextSessionName }), undefined, {
      source: 'session-management',
      duration: 1800,
    });
    navigateSession({
      kind: 'session',
      scope: 'workspace',
      workspaceId: workspace.workspaceId,
      workspacePath: workspaceNavigationPath(workspace),
      sessionId,
    });
    if (sidebarIsDrawer) {
      sidebarOpen = false;
    }
  }

  function isEditingSession(workspaceId: string, sessionId: string): boolean {
    return editingSession?.workspaceId === workspaceId
      && editingSession.sessionId === sessionId;
  }

  function isEditingPersonalSession(sessionId: string): boolean {
    return editingSession?.workspaceId === null && editingSession.sessionId === sessionId;
  }

  async function beginSessionRename(
    workspace: AgentWorkspaceSummary,
    session: Session,
  ): Promise<void> {
    if (renamingSessionId || pendingNavigation || messagesState.sessionHydrating) {
      return;
    }
    editingSession = {
      workspaceId: workspace.workspaceId,
      sessionId: session.id,
    };
    sessionRenameDraft = session.name || '';
    sessionRenameError = '';
    await tick();
    sessionRenameInput?.focus();
    sessionRenameInput?.select();
  }

  function cancelSessionRename(): void {
    if (renamingSessionId) {
      return;
    }
    editingSession = null;
    sessionRenameDraft = '';
    sessionRenameError = '';
    sessionRenameInput = null;
  }

  function validateSessionName(name: string): string {
    if (!name) {
      return i18n.t('web.sessionNameRequired');
    }
    if (/\p{Cc}/u.test(name)) {
      return i18n.t('web.sessionNameInvalidCharacters');
    }
    if (Array.from(name).length > SESSION_NAME_MAX_CHARS) {
      return i18n.t('web.sessionNameTooLong', { max: SESSION_NAME_MAX_CHARS });
    }
    return '';
  }

  async function saveSessionRename(
    workspace: AgentWorkspaceSummary,
    session: Session,
  ): Promise<void> {
    if (!isEditingSession(workspace.workspaceId, session.id) || renamingSessionId) {
      return;
    }
    const normalizedName = sessionRenameDraft.trim();
    const validationError = validateSessionName(normalizedName);
    if (validationError) {
      sessionRenameError = validationError;
      sessionRenameInput?.focus();
      return;
    }
    if (normalizedName === (session.name || '').trim()) {
      cancelSessionRename();
      return;
    }

    sessionRenameError = '';
    renamingSessionId = session.id;
    let renameCommitted = false;
    try {
      const snapshot = await runActionWithFeedback(
        () => renameAgentSession(session.id, normalizedName, {
          scope: 'workspace',
          workspaceId: workspace.workspaceId,
          workspacePath: workspaceNavigationPath(workspace),
        }),
        {
          actionLabel: i18n.t('web.action.renameSession'),
          successMessage: i18n.t('web.sessionRenamed', { name: normalizedName }),
        },
      );
      if (!snapshot) {
        return;
      }
      const normalizedSnapshot = normalizeRustBootstrapPayload(snapshot, {
        workspaceId: workspace.workspaceId,
        workspacePath: workspaceNavigationPath(workspace),
      });
      const authoritativeWorkspaceId = normalizedSnapshot.workspace.workspaceId?.trim()
        || workspace.workspaceId;
      const cursor = {
        runtimeEpoch: normalizedSnapshot.agent?.runtimeEpoch || '',
        eventStreamNextSequence: normalizedSnapshot.eventStreamNextSequence || 0,
      };
      if (!canApplyWorkspaceSessionProjectionCursor(authoritativeWorkspaceId, cursor)) {
        return;
      }
      workspaceSessionCursorByWorkspace.set(authoritativeWorkspaceId, cursor);
      sessionsByWorkspace = {
        ...sessionsByWorkspace,
        [authoritativeWorkspaceId]: normalizedSnapshot.sessions,
      };
        if (currentBootstrapWorkspaceId() === authoritativeWorkspaceId) {
          replaceWorkspaceSessionProjection(authoritativeWorkspaceId, normalizedSnapshot.sessions, cursor);
        }
      renameCommitted = true;
    } finally {
      renamingSessionId = null;
    }
    if (renameCommitted) {
      cancelSessionRename();
    }
  }

  function handleSessionRenameBlur(
    workspace: AgentWorkspaceSummary,
    session: Session,
  ): void {
    setTimeout(() => {
      if (isEditingSession(workspace.workspaceId, session.id)) {
        void saveSessionRename(workspace, session);
      }
    }, 0);
  }

  function openDeleteSessionDialog(workspace: AgentWorkspaceSummary, session: Session): void {
    if (pendingNavigation || messagesState.sessionHydrating) {
      return;
    }
    pendingDeleteSession = { workspace, session };
    showDeleteSessionDialog = true;
  }

  async function beginPersonalSessionRename(session: Session): Promise<void> {
    if (renamingSessionId || pendingNavigation || messagesState.sessionHydrating) return;
    editingSession = { workspaceId: null, sessionId: session.id };
    sessionRenameDraft = session.name || '';
    sessionRenameError = '';
    await tick();
    sessionRenameInput?.focus();
    sessionRenameInput?.select();
  }

  async function savePersonalSessionRename(session: Session): Promise<void> {
    if (!isEditingPersonalSession(session.id) || renamingSessionId) return;
    const name = sessionRenameDraft.trim();
    const validationError = validateSessionName(name);
    if (validationError) {
      sessionRenameError = validationError;
      return;
    }
    renamingSessionId = session.id;
    let renameCommitted = false;
    try {
      await renameAgentSession(session.id, name, {});
      await refreshPersonalSessions();
      renameCommitted = true;
    } finally {
      renamingSessionId = null;
    }
    if (renameCommitted) {
      cancelSessionRename();
    }
  }

  function openPersonalDeleteSessionDialog(session: Session): void {
    if (pendingNavigation || messagesState.sessionHydrating) {
      return;
    }
    pendingDeleteSession = { workspace: null, session };
    showDeleteSessionDialog = true;
  }

  function closeDeleteSessionDialog(): void {
    showDeleteSessionDialog = false;
    pendingDeleteSession = null;
  }

  function confirmDeleteSession(): void {
    if (!pendingDeleteSession) {
      closeDeleteSessionDialog();
      return;
    }
    const { workspace, session } = pendingDeleteSession;
    const displayName = session.name || i18n.t('header.unnamedSession');
    addToast('info', i18n.t('web.sessionDeleting', { name: displayName }), undefined, {
      source: 'session-management',
      duration: 1800,
    });
    getClientBridge().postMessage({
      type: 'deleteSession',
      sessionId: session.id,
      ...(workspace ? {
        workspaceId: workspace.workspaceId,
        workspacePath: workspaceNavigationPath(workspace),
      } : {}),
      requireConfirm: false,
    });
    closeDeleteSessionDialog();
  }

  function applyViewportMode(): void {
    if (typeof window === 'undefined') {
      return;
    }
    viewportWidth = window.innerWidth;
  }

  function requestRightPaneVisibility(visible: boolean): void {
    if (desktopAppSurface) {
      // Renderer 只提交本地意图；下面唯一的 desktop visibility effect 负责
      // IPC，避免一次点击产生两次 layoutRevision 和两次 Surface 重排。
      desktopVisibilitySyncFailure = null;
      desktopVisibilitySyncEpoch += 1;
      setRightPaneCollapsed(rightPaneState.activeScopeKey, !visible);
      return;
    }
    setRightPaneCollapsed(rightPaneState.activeScopeKey, !visible);
  }

  function toggleSidebar(): void {
    const nextOpen = !sidebarOpen;
    sidebarOpen = nextOpen;
    // 窄屏 drawer 模式下打开 sidebar 抽屉时，自动折叠右侧 overlay（z=900）
    // 避免抽屉（z=800）被 overlay 遮住，造成用户操作无入口
    if (nextOpen && sidebarIsDrawer && rightPaneVisible) {
      requestRightPaneVisibility(false);
    }
  }

  // 顶部 Header 的 sidebar 切换按钮：drawer 模式下控制抽屉开合，桌面模式下控制折叠/展开。
  function toggleSidebarFromHeader(): void {
    if (sidebarIsDrawer) {
      toggleSidebar();
      return;
    }
    if (panelVisibility.sidebarVisible) {
      sidebarCollapsed = true;
      persistSidebarCollapsed(true);
      return;
    }
    sidebarCollapsed = false;
    persistSidebarCollapsed(false);
  }

  function toggleRightPaneFromHeader(): void {
    if (!rightPaneVisible && sidebarIsDrawer) {
      sidebarOpen = false;
    }
    requestRightPaneVisibility(!rightPaneVisible);
  }

  setWebSidebarContext({
    get hidden() { return sidebarHidden; },
    get isDrawer() { return sidebarIsDrawer; },
    get drawerOpen() { return sidebarOpen; },
    toggle: toggleSidebarFromHeader,
    toggleRightPane: toggleRightPaneFromHeader,
  });

  function applySidebarModeFromEvent(event: Event): void {
    const target = event.target instanceof Element ? event.target : null;
    const modeButton = target?.closest('[data-sidebar-mode]');
    const nextMode = modeButton instanceof HTMLElement ? modeButton.dataset.sidebarMode : '';
    if (nextMode === 'projects' || nextMode === 'files') {
      sidebarMode = nextMode;
    }
  }

  function sidebarTooltipTargetFromEvent(event: Event): HTMLElement | null {
    const eventTarget = event.target instanceof Element
      ? event.target.closest<HTMLElement>('[data-tooltip]')
      : null;
    if (!eventTarget || !sidebarElement?.contains(eventTarget)) {
      return null;
    }
    return eventTarget;
  }

  function clearSidebarTooltip(): void {
    if (sidebarTooltipFrame !== null && typeof window !== 'undefined') {
      window.cancelAnimationFrame(sidebarTooltipFrame);
      sidebarTooltipFrame = null;
    }
    sidebarTooltipTarget = null;
    sidebarTooltip = null;
  }

  function positionSidebarTooltip(target: HTMLElement | null = sidebarTooltipTarget): void {
    if (!target || !target.isConnected || !sidebarElement?.contains(target)) {
      clearSidebarTooltip();
      return;
    }
    const text = target.dataset.tooltip?.trim() || '';
    const rect = target.getBoundingClientRect();
    if (!text || rect.width <= 0 || rect.height <= 0) {
      clearSidebarTooltip();
      return;
    }

    const viewportPadding = 8;
    const tooltipGap = 6;
    const placement = rect.bottom + 40 <= window.innerHeight || rect.top < 40 ? 'below' : 'above';
    const top = placement === 'below' ? rect.bottom + tooltipGap : rect.top - tooltipGap;
    const left = Math.min(
      Math.max(rect.right, viewportPadding),
      Math.max(viewportPadding, window.innerWidth - viewportPadding),
    );
    sidebarTooltip = { text, left, top, placement };
  }

  function scheduleSidebarTooltipPosition(): void {
    if (!sidebarTooltipTarget || sidebarTooltipFrame !== null || typeof window === 'undefined') {
      return;
    }
    sidebarTooltipFrame = window.requestAnimationFrame(() => {
      sidebarTooltipFrame = null;
      positionSidebarTooltip();
    });
  }

  function showSidebarTooltip(event: Event): void {
    const target = sidebarTooltipTargetFromEvent(event);
    if (!target) return;
    const text = target.dataset.tooltip?.trim() || '';
    if (!text) return;
    sidebarTooltipTarget = target;
    positionSidebarTooltip(target);
  }

  function handleSidebarTooltipPointerOver(event: PointerEvent): void {
    const target = sidebarTooltipTargetFromEvent(event);
    const relatedTarget = event.relatedTarget;
    if (!target || (relatedTarget instanceof Node && target.contains(relatedTarget))) {
      return;
    }
    showSidebarTooltip(event);
  }

  function handleSidebarTooltipPointerOut(event: PointerEvent): void {
    const target = sidebarTooltipTargetFromEvent(event);
    const relatedTarget = event.relatedTarget;
    if (target && relatedTarget instanceof Node && target.contains(relatedTarget)) {
      return;
    }
    if (!relatedTarget || target === sidebarTooltipTarget) {
      clearSidebarTooltip();
    }
  }

  function handleSidebarTooltipFocusIn(event: FocusEvent): void {
    showSidebarTooltip(event);
  }

  function handleSidebarTooltipFocusOut(event: FocusEvent): void {
    const target = sidebarTooltipTargetFromEvent(event);
    const relatedTarget = event.relatedTarget;
    if (target && relatedTarget instanceof Node && target.contains(relatedTarget)) {
      return;
    }
    clearSidebarTooltip();
  }

  function handleSidebarTooltipViewportChange(): void {
    scheduleSidebarTooltipPosition();
  }

  $effect(() => {
    if (typeof document === 'undefined') {
      return;
    }

    const shouldLockViewport = sidebarIsDrawer && sidebarOpen;
    document.documentElement.classList.toggle('magi-web-drawer-open', shouldLockViewport);
    document.body.classList.toggle('magi-web-drawer-open', shouldLockViewport);

    return () => {
      document.documentElement.classList.remove('magi-web-drawer-open');
      document.body.classList.remove('magi-web-drawer-open');
    };
  });

  $effect(() => {
    if (!sidebarIsDrawer && sidebarOpen) {
      sidebarOpen = false;
    }
  });

  $effect(() => {
    if (sidebarIsDrawer && sidebarOpen && rightPaneVisible) {
      sidebarOpen = false;
    }
  });

  $effect(() => {
    if (desktopAppSurface) return;
    if (previewPanelWidth === null) {
      return;
    }
    void viewportWidth;
    void sidebarIsDrawer;
    void sidebarWidth;
    const clamped = clampPreviewPanelWidth(previewPanelWidth);
    if (clamped !== previewPanelWidth) {
      previewPanelWidth = clamped;
    }
  });

  function readDesktopDropRect(element: Element | null): DesktopDropRect | null {
    if (!element) return null;
    const rect = element.getBoundingClientRect();
    if (rect.width <= 0 || rect.height <= 0) return null;
    return {
      left: rect.left,
      top: rect.top,
      right: rect.right,
      bottom: rect.bottom,
      width: rect.width,
      height: rect.height,
    };
  }

  function handleDesktopDragDropEvent(event: DesktopDragDropEvent): void {
    if (event.type === 'leave') {
      desktopDropIndicator = null;
      return;
    }
    const point = event.position;
    const zones = {
      sidebar: sidebarHidden ? null : readDesktopDropRect(sidebarElement),
      conversation: readDesktopDropRect(
        document.querySelector('[data-desktop-drop-zone="conversation"]'),
      ),
    };
    let zone = resolveDesktopDropZone(point, zones);
    const hitTarget = document.elementFromPoint(point.x, point.y);
    if (hitTarget && zone === 'sidebar' && !sidebarElement?.contains(hitTarget)) {
      zone = null;
    } else if (
      hitTarget
      && zone === 'conversation'
      && !hitTarget.closest('[data-desktop-drop-zone="conversation"]')
    ) {
      zone = null;
    }
    const rect = zone ? zones[zone] : null;
    desktopDropIndicator = zone && rect ? { zone, rect } : null;
    if (event.type !== 'drop') return;

    desktopDropIndicator = null;
    const paths = normalizeDesktopDropPaths(event.paths);
    if (paths.length === 0) return;
    if (zone === 'sidebar') {
      void handleDesktopWorkspaceDrop(paths);
      return;
    }
    if (zone === 'conversation') {
      window.dispatchEvent(new CustomEvent(DESKTOP_CONTEXT_DROP_EVENT, {
        detail: { paths },
      }));
    }
  }

  $effect(() => {
    if (!desktopAppSurface) return;
    const scopeKey = rightPaneState.activeScopeKey;
    const pane = getRightPaneState(scopeKey);
    const desiredVisible = !pane.collapsed;
    const target: DesktopVisibilityTarget = { scopeKey, visible: desiredVisible };
    void desktopVisibilitySyncEpoch;
    const snapshot = desktopSnapshot;
    if (!snapshot) return;

    // 快照已经确认当前目标时进入 idle。即使快照先于 IPC Promise 到达，
    // 也不能提前清理在途请求，否则下一次用户操作会与旧请求并发。
    if (desktopRightPaneVisible === desiredVisible) {
      return;
    }

    const failure = desktopVisibilitySyncFailure;
    if (
      failure
      && sameDesktopVisibilityTarget(failure, target)
      && failure.desktopEpoch === desktopSnapshotEpoch
      && failure.snapshotRevision === desktopSnapshotRevision
    ) {
      // 当前 Main 快照和目标都没有变化时，失败请求保持 failed，避免
      // IPC 异常触发反馈式重试；用户下一次显式点击会清除该状态。
      return;
    }

    // 在途请求必须先完成。目标若已变化，只保留 rightPaneState 中的最新
    // 目标，完成回调通过 syncEpoch 触发下一轮收敛，避免响应乱序。
    if (desktopVisibilitySyncRequest) return;

    const desktop = window.magiDesktop;
    if (!desktop) return;
    const request: DesktopVisibilitySyncRequest = {
      ...target,
      requestId: ++desktopVisibilityRequestId,
    };
    desktopVisibilitySyncRequest = request;
    void desktop.submitLayoutIntent({
      type: 'right_pane_visibility',
      visible: desiredVisible,
    }).then((nextSnapshot) => {
      if (desktopVisibilitySyncRequest?.requestId !== request.requestId) return;
      applyDesktopSnapshot(nextSnapshot);
      desktopVisibilitySyncRequest = null;
      desktopVisibilitySyncEpoch += 1;
    }).catch((error) => {
      if (desktopVisibilitySyncRequest?.requestId !== request.requestId) return;
      desktopVisibilitySyncFailure = {
        ...request,
        desktopEpoch: desktopSnapshotEpoch,
        snapshotRevision: desktopSnapshotRevision,
      };
      desktopVisibilitySyncRequest = null;
      desktopVisibilitySyncEpoch += 1;
      console.warn('[WebWorkbenchShell] 恢复桌面右栏可见性失败:', error);
    });
  });

  $effect(() => {
    if (!desktopAppSurface || !desktopSnapshot || !window.magiDesktop) return;
    void desktopPanelActivationEpoch;
    const recoveryRevision = desktopRuntimeRecoveryRevision;
    const target = currentDesktopPanelTarget();
    const targetKey = desktopPanelTargetKey(target);
    const snapshot = desktopSnapshot;

    // 新打开的右栏必须先完成 Renderer 的可见性事务，随后才物化浏览器。
    // 不把 BrowserSurface 放到不可见右栏中，避免 Main 先挂载再撤下造成黑屏。
    if (target.kind === 'browser' && (!desktopRightPaneVisible || !desktopBrowserRuntimeReady)) return;

    const recoveryPending = desktopPanelActivationCompletedRecoveryRevision < recoveryRevision;
    const inFlightRequest = desktopPanelActivationRequest;
    if (inFlightRequest && inFlightRequest.recoveryRevision < recoveryRevision) {
      // 运行时恢复事件不能打断旧 IPC。旧请求结束后会主动清空自身结果，
      // 再由当前代次派发唯一一次新的激活。
      return;
    }
    if (!recoveryPending) {
      const activationDecision = decideDesktopPanelActivation(
        snapshot,
        target,
        inFlightRequest !== null,
        false,
      );
      if (activationDecision === 'acknowledged') {
        desktopPanelActivationFailureKey = '';
        if (target.kind) {
          clearPendingDesktopPanelIntent(target.scopeKey, target.kind, target.tabId ?? '');
        }
        return;
      }

      if (activationDecision === 'wait_for_in_flight') {
        // Main 侧操作严格串行。当前 Renderer 意图只保留在 rightPaneState，
        // 在途请求结束后由 epoch 再读取最新目标，绝不并发覆盖。
        return;
      }
    } else if (inFlightRequest) {
      return;
    }

    if (desktopPanelActivationFailureKey && desktopPanelActivationFailureKey !== targetKey) {
      desktopPanelActivationFailureKey = '';
    }
    if (desktopPanelActivationFailureKey === targetKey) return;

    const request: DesktopPanelActivationRequest = {
      ...target,
      key: targetKey,
      requestId: ++desktopPanelActivationRequestId,
      recoveryRevision,
    };
    desktopPanelActivationRequest = request;
    activateDesktopPanelTarget(request);
  });

  onMount(() => {
    if (!desktopAppSurface) return;
    const desktop = window.magiDesktop;
    if (!desktop) {
      throw new Error('desktop_preload_bridge_unavailable');
    }
    let disposed = false;
    void desktop.getSnapshot().then((snapshot) => {
      if (!disposed) applyDesktopSnapshot(snapshot);
    }).catch((error) => {
      if (!disposed) console.error('[WebWorkbenchShell] 获取桌面窗口快照失败:', error);
    });
    const stopSnapshot = desktop.onSnapshot(applyDesktopSnapshot);
    const applyBrowserComponent = (snapshot: MagiDesktopBrowserComponentSnapshot) => {
      // 组件状态只能撤销当前运行时资格，不能授予资格。授予资格的唯一
      // 来源是 Main 在同一代 Control WebSocket 完成 daemon ready 握手后
      // 发布的 browser-runtime-ready 事件，避免旧 daemon 状态抢先放行激活。
      if (snapshot.runtime.ready !== true) desktopBrowserRuntimeReady = false;
    };
    const stopBrowserComponent = desktop.onBrowserComponent(applyBrowserComponent);
    const stopBrowserRuntimeReady = desktop.onBrowserRuntimeReady((event) => {
      const revision = event && typeof event === 'object' && 'revision' in event
        && typeof event.revision === 'number' && Number.isFinite(event.revision)
        ? Math.max(0, Math.floor(event.revision))
        : 0;
      if (revision <= desktopRuntimeRecoveryRevision) return;
      desktopBrowserRuntimeReady = true;
      desktopRuntimeRecoveryRevision = revision;
      desktopPanelActivationFailureKey = '';
      desktopPanelActivationEpoch += 1;
      // 先读取 Main 的最新确认快照，再让唯一激活 effect 重新物化当前逻辑
      // Tab。事件本身只表示运行时代次已切换，不携带旧的布局事实。
      void desktop.getSnapshot().then((snapshot) => {
        if (!disposed) {
          applyDesktopSnapshot(snapshot);
          desktopPanelActivationEpoch += 1;
        }
      }).catch((error) => {
        if (!disposed) console.warn('[WebWorkbenchShell] 读取浏览器运行时恢复快照失败:', error);
      });
    });
    // 这只是 App Renderer 的首次绘制/窗口显示握手，不携带任何右栏 Tab
    // 或面板意图；右栏状态仍由本地 rightPaneState 唯一管理。
    void desktop.readyRightPane().catch((error) => {
      if (!disposed) console.error('[WebWorkbenchShell] 桌面 Renderer 就绪握手失败:', error);
    });
    return () => {
      disposed = true;
      stopSnapshot();
      stopBrowserComponent();
      stopBrowserRuntimeReady();
      if (desktopRightPaneResizeFrame !== null) {
        cancelAnimationFrame(desktopRightPaneResizeFrame);
        desktopRightPaneResizeFrame = null;
      }
      desktopRightPaneResizeCancel?.();
    };
  });

  onMount(() => {
    applyViewportMode();
    loadStoredSidebarWidth();
    loadStoredSidebarCollapsed();
    loadStoredPreviewPanelWidth();
    // 节流 resize：手机虚拟键盘弹出/收起会短时间内触发大量 resize 事件
    let resizeRaf: number | null = null;
    let desktopDropDisposed = false;
    let stopDesktopFileDrop: (() => void) | null = null;
    const handleResize = () => {
      if (resizeRaf !== null) return;
      resizeRaf = requestAnimationFrame(() => {
        resizeRaf = null;
        applyViewportMode();
      });
    };
    const handlePreviewFile = (event: Event) => {
      const detail = (event as CustomEvent<{
        filepath?: string;
        workspaceId?: string;
        workspacePath?: string;
        sessionId?: string;
        contentKind?: EditContentKind;
        size?: number;
        mime?: string;
        symlinkTarget?: string;
        headSummary?: string;
        tailSummary?: string;
        imageDataUrl?: string;
      }>).detail;
      const filepath = detail?.filepath;
      if (typeof filepath === 'string') {
        const handled = handleFileSelect(filepath, {
          workspaceId: detail?.workspaceId,
          workspacePath: detail?.workspacePath,
          sessionId: detail?.sessionId,
          contentKind: detail?.contentKind,
          size: detail?.size,
          mime: detail?.mime,
          symlinkTarget: detail?.symlinkTarget,
          headSummary: detail?.headSummary,
          tailSummary: detail?.tailSummary,
          imageDataUrl: detail?.imageDataUrl,
        });
        if (handled) {
          event.preventDefault();
        }
      }
    };
    const handleOpenHtmlFileInBrowser = (event: Event) => {
      if (!desktopAppSurface) return;
      const detail = (event as CustomEvent<OpenHtmlFileInBrowserRequest>).detail;
      const filepath = detail?.filepath?.trim() || '';
      if (!filepath || !isHtmlFile(filepath)) return;
      const binding = currentWorkspaceBinding();
      if (binding.scope !== 'workspace' || !binding.workspaceId || !binding.workspacePath || !binding.sessionId) return;
      htmlBrowserOpenRequest = {
        requestId: ++htmlBrowserOpenRequestId,
        filepath,
        workspaceId: binding.workspaceId,
        workspacePath: binding.workspacePath,
        sessionId: binding.sessionId,
      };
      requestRightPaneVisibility(true);
    };
    const handleAgentConnection = (event: Event) => {
      const detail = (event as CustomEvent<AgentConnectionEventDetail>).detail;
      const previousAgentBaseUrl = agentBaseUrl;
      agentBaseUrl = resolveAgentBaseUrl();
      if (detail?.status === 'recovering') {
        agentRecovering = true;
        if (!workspaces.length && !loading) {
          loadError = i18n.t('web.agentRecovering');
        }
        return;
      }
      agentRecovering = false;
      const shouldRefreshWorkspaces = !loading && (
        Boolean(loadError)
        || workspaces.length === 0
        || Boolean(detail?.recovered && previousAgentBaseUrl !== agentBaseUrl)
      );
      if (shouldRefreshWorkspaces) {
        void refreshWorkspaces();
      }
    };
    const handlePanelEscape = (event: KeyboardEvent) => {
      if (event.key !== 'Escape' || event.defaultPrevented) {
        return;
      }
      if (sidebarIsDrawer && sidebarOpen) {
        sidebarOpen = false;
        return;
      }
      if (previewIsOverlay && rightPaneVisible) {
        requestRightPaneVisibility(false);
      }
    };
    window.addEventListener('resize', handleResize);
    window.addEventListener('scroll', handleSidebarTooltipViewportChange, true);
    window.addEventListener('magi:previewFile', handlePreviewFile as EventListener);
    window.addEventListener(OPEN_HTML_FILE_IN_BROWSER_EVENT, handleOpenHtmlFileInBrowser as EventListener);
    window.addEventListener(RUNTIME_CONNECTION_EVENT, handleAgentConnection as EventListener);
    window.addEventListener(BROWSER_AUTHORITY_CHANGED_EVENT, synchronizeBrowserProjectionFromAuthorityEvent);
    window.addEventListener('keydown', handlePanelEscape);
    void registerDesktopFileDropListener(handleDesktopDragDropEvent)
      .then((stop) => {
        if (desktopDropDisposed) {
          stop();
          return;
        }
        stopDesktopFileDrop = stop;
      })
      .catch((error) => {
        console.warn('[WebWorkbenchShell] 注册 Desktop 文件拖放监听失败:', error);
      });
    void refreshWorkspaces();
    void refreshPersonalSessions();
    return () => {
      desktopDropDisposed = true;
      stopDesktopFileDrop?.();
      desktopDropIndicator = null;
      window.removeEventListener('resize', handleResize);
      window.removeEventListener('scroll', handleSidebarTooltipViewportChange, true);
      window.removeEventListener('magi:previewFile', handlePreviewFile as EventListener);
      window.removeEventListener(OPEN_HTML_FILE_IN_BROWSER_EVENT, handleOpenHtmlFileInBrowser as EventListener);
      window.removeEventListener(RUNTIME_CONNECTION_EVENT, handleAgentConnection as EventListener);
      window.removeEventListener(BROWSER_AUTHORITY_CHANGED_EVENT, synchronizeBrowserProjectionFromAuthorityEvent);
      window.removeEventListener('keydown', handlePanelEscape);
      if (resizeRaf !== null) {
        cancelAnimationFrame(resizeRaf);
      }
      clearSidebarTooltip();
    };
  });

  onMount(() => {
    return subscribeAppearanceRuntime((snapshot) => {
      appearanceRuntime = snapshot;
    });
  });
</script>

<div
  bind:this={workbenchElement}
  class="web-workbench-shell"
  class:web-workbench-shell--sidebar-drawer={sidebarIsDrawer}
  class:web-workbench-shell--sidebar-open={sidebarIsDrawer && sidebarOpen}
  class:web-workbench-shell--sidebar-hidden={sidebarHidden}
  class:web-workbench-shell--desktop={desktopAppSurface}
  class:web-workbench-shell--preview-overlay={previewIsOverlay}
  class:web-workbench-shell--has-preview={inlineRightPaneVisible}
  class:web-workbench-shell--desktop-right-pane-visible={desktopAppSurface && desktopRightPaneVisible}
  class:web-workbench-shell--resizing={isSidebarResizing || isPreviewPanelResizing}
  class:web-workbench-shell--sidebar-resizing={isSidebarResizing}
  class:web-workbench-shell--preview-resizing={isPreviewPanelResizing}
  style={shellLayoutStyle}
>
  {#if desktopDropIndicator}
    <div
      class="desktop-drop-overlay"
      class:desktop-drop-overlay--sidebar={desktopDropIndicator.zone === 'sidebar'}
      style={`left:${desktopDropIndicator.rect.left}px;top:${desktopDropIndicator.rect.top}px;width:${desktopDropIndicator.rect.width}px;height:${desktopDropIndicator.rect.height}px;`}
      aria-hidden="true"
    >
      <div class="desktop-drop-overlay__label">
        <Icon name={desktopDropIndicator.zone === 'sidebar' ? 'folder' : 'document'} size={18} />
        <span>
          {desktopDropIndicator.zone === 'sidebar'
            ? i18n.t('web.desktopDropWorkspaceHint')
            : i18n.t('input.add.contextDropHint')}
        </span>
      </div>
    </div>
  {/if}

  {#if sidebarIsDrawer && sidebarOpen}
    <button
      type="button"
      class="drawer-overlay"
      aria-label={i18n.t('web.closeNav')}
      onclick={() => {
        sidebarOpen = false;
      }}
    ></button>
  {/if}

  {#if !sidebarHidden}
  <aside
    bind:this={sidebarElement}
    class="sidebar"
    class:sidebar--open={sidebarIsDrawer && sidebarOpen}
    onpointerover={handleSidebarTooltipPointerOver}
    onpointerout={handleSidebarTooltipPointerOut}
    onfocusin={handleSidebarTooltipFocusIn}
    onfocusout={handleSidebarTooltipFocusOut}
  >
    <div class="sidebar-header">
      <div class="sidebar-toolbar">
        <MagiWordmark />
        <div class="sidebar-header-tools">
          <button
            class="theme-toggle-btn"
            type="button"
            data-tooltip={themeToggleTitle}
            aria-label={themeToggleTitle}
            data-theme-id={appearanceRuntime.activeTheme?.id || ''}
            data-theme-mode={appearanceRuntime.mode}
            onclick={toggleWebTheme}
          >
            <Icon name={themeIconName} size={14} />
          </button>
          <button class="sidebar-icon-btn" type="button" data-testid="sidebar-refresh" onclick={() => void refreshWorkspaces()} data-tooltip={i18n.t('common.refresh')}>
            <Icon name="refresh" size={14} />
          </button>
          <button class="sidebar-icon-btn" type="button" onclick={openAddWorkspaceDialog} disabled={workspaceActionPending || !!loadError} data-tooltip={i18n.t('web.selectFolder')}>
            <Icon name="folder" size={14} />
          </button>
          {#if sidebarIsDrawer}
            <button
              class="sidebar-icon-btn sidebar-drawer-close"
              type="button"
              onclick={() => { sidebarOpen = false; }}
              data-tooltip={i18n.t('web.closeSidebar')}
              aria-label={i18n.t('web.closeSidebar')}
            >
              <Icon name="x" size={14} />
            </button>
          {/if}
        </div>
      </div>
    </div>

    <div
      class="sidebar-navigation-scroll"
      class:sidebar-navigation-scroll--projects={sidebarMode === 'projects'}
      class:sidebar-navigation-scroll--files={sidebarMode === 'files'}
    >
      {#if sidebarMode === 'projects'}
        <section class="sidebar-section sidebar-section--workspaces">
        <div class="section-title-row section-title-row--sticky">
          <button
            type="button"
            class="section-title-toggle"
            aria-expanded={!workspacesCollapsed}
            aria-controls="workspace-section-content"
            aria-label={workspacesCollapsed ? i18n.t('web.expandWorkspaces') : i18n.t('web.collapseWorkspaces')}
            title={workspacesCollapsed ? i18n.t('web.expandWorkspaces') : i18n.t('web.collapseWorkspaces')}
            onclick={toggleWorkspaces}
          >
            <span class="section-title">{i18n.t('common.workspace')}</span>
            <span class="section-title-chevron" class:section-title-chevron--collapsed={workspacesCollapsed} aria-hidden="true">
              <Icon name="chevronDown" size={11} />
            </span>
          </button>
          <button
            type="button"
            class="sidebar-icon-btn sidebar-icon-btn--compact"
            data-tooltip={i18n.t('web.projectFiles')}
            data-sidebar-mode="files"
            aria-label={i18n.t('web.projectFiles')}
            onpointerdown={applySidebarModeFromEvent}
            onclick={applySidebarModeFromEvent}
          >
            <Icon name="list" size={13} />
          </button>
        </div>
        <div id="workspace-section-content">
        {#if !workspacesCollapsed}
        {#if loading}
          <div class="sidebar-empty">{i18n.t('common.loading')}</div>
        {:else if agentRecovering}
          <div class="sidebar-empty sidebar-empty--recovering">
            <Icon name="loader" size={13} />
            <span>{i18n.t('web.agentRecovering')}</span>
          </div>
        {:else if loadError}
          <div class="sidebar-error">
            <div class="sidebar-error-title">{i18n.t('web.workspaceUnavailable')}</div>
            <div>{loadError}</div>
          </div>
        {:else if workspaces.length === 0}
          <div class="sidebar-empty">{i18n.t('web.noWorkspaces')}</div>
        {:else}
          <div class="workspace-tree">
            {#each workspaces as workspace (workspace.workspaceId)}
              <div class="workspace-node">
                <div class="workspace-row">
                  <button
                    type="button"
                    class="workspace-header-btn"
                    use:desktopContextMenu={{
                      kind: 'workspace',
                      workspacePathRef: workspacePathRef(workspace),
                    }}
                    class:active={workspace.workspaceId === selectedWorkspaceId}
                    aria-expanded={!!expandedWorkspaceIds[workspace.workspaceId]}
                    data-workspace-id={workspace.workspaceId}
                    title={workspace.rootPath}
                    onclick={() => handleWorkspaceClick(workspace)}
                  >
                    <span
                      class="workspace-chevron"
                      class:workspace-chevron--expanded={!!expandedWorkspaceIds[workspace.workspaceId]}
                      aria-hidden="true"
                    >
                      <Icon name="chevronDown" size={10} />
                    </span>
                    <Icon name="folder" size={12} class="workspace-folder-icon" />
                    <span class="workspace-name">{workspace.name}</span>
                  </button>
                  <button
                    type="button"
                    class="workspace-new-session-btn"
                    data-tooltip={i18n.t('web.newWorkspaceSessionTitle')}
                    title={i18n.t('web.newWorkspaceSessionTitle')}
                    aria-label={i18n.t('web.newWorkspaceSessionAria', { name: workspace.name })}
                    disabled={workspaceActionPending || messagesState.sessionHydrating}
                    onclick={(event) => {
                      event.stopPropagation();
                      void openWorkspaceDraft(workspace);
                    }}
                  >
                    <Icon name="plus" size={12} />
                  </button>
                  <button
                    type="button"
                    class="workspace-remove-btn"
                    title={i18n.t('web.removeWorkspaceTitle')}
                    aria-label={i18n.t('web.removeWorkspaceAria', { name: workspace.name })}
                    onclick={(event) => {
                      event.stopPropagation();
                      openRemoveWorkspaceDialog(workspace);
                    }}
                  >
                    ×
                  </button>
                </div>
                {#if expandedWorkspaceIds[workspace.workspaceId]}
                  <div class="workspace-children">
                    {#if loadingWorkspaceIds[workspace.workspaceId]}
                      <div class="sidebar-empty sidebar-empty--nested">{i18n.t('common.loading')}</div>
                    {:else if getWorkspaceSessionList(workspace.workspaceId).length === 0}
                      <div class="sidebar-empty sidebar-empty--nested">{i18n.t('web.noWorkspaceSessions')}</div>
                    {:else}
                      <div class="session-list session-list--nested">
                        {#each getWorkspaceSessionList(workspace.workspaceId) as session (session.id)}
                          {@const sessionRunning = isSessionRunning(workspace.workspaceId, session)}
                          {@const sessionIndicator = resolveSessionActivityIndicator({
                            isRunning: sessionRunning,
                            hasUnreadCompletion: session.hasUnreadCompletion === true,
                          })}
                          <div class="session-row" class:active={session.id === currentSessionId && workspace.workspaceId === selectedWorkspaceId} class:editing={isEditingSession(workspace.workspaceId, session.id)}>
                            {#if isEditingSession(workspace.workspaceId, session.id)}
                              <div class="session-rename-editor">
                                <div class="session-rename-controls">
                                  <input
                                    bind:this={sessionRenameInput}
                                    bind:value={sessionRenameDraft}
                                    class:invalid={Boolean(sessionRenameError)}
                                    class="session-rename-input"
                                    maxlength={SESSION_NAME_MAX_CHARS}
                                    aria-label={i18n.t('header.renameSession')}
                                    aria-invalid={Boolean(sessionRenameError)}
                                    disabled={renamingSessionId === session.id}
                                    oninput={() => { sessionRenameError = ''; }}
                                    onkeydown={(event) => {
                                      if (event.key === 'Enter') {
                                        event.preventDefault();
                                        void saveSessionRename(workspace, session);
                                      } else if (event.key === 'Escape') {
                                        event.preventDefault();
                                        cancelSessionRename();
                                      }
                                    }}
                                    onblur={() => handleSessionRenameBlur(workspace, session)}
                                  />
                                  <button
                                    type="button"
                                    class="session-rename-action session-rename-save"
                                    title={i18n.t('header.saveSessionName')}
                                    aria-label={i18n.t('header.saveSessionName')}
                                    disabled={renamingSessionId === session.id}
                                    onclick={() => void saveSessionRename(workspace, session)}
                                  >
                                    <Icon name="check" size={12} />
                                  </button>
                                  <button
                                    type="button"
                                    class="session-rename-action"
                                    title={i18n.t('header.cancelSessionRename')}
                                    aria-label={i18n.t('header.cancelSessionRename')}
                                    disabled={renamingSessionId === session.id}
                                    onclick={cancelSessionRename}
                                  >
                                    <Icon name="x" size={12} />
                                  </button>
                                </div>
                                {#if sessionRenameError}
                                  <span class="session-rename-error">{sessionRenameError}</span>
                                {/if}
                              </div>
                            {:else}
                              <button
                                type="button"
                                class="session-item"
                                class:active={session.id === currentSessionId && workspace.workspaceId === selectedWorkspaceId}
                                class:pending={session.id === pendingSessionSwitchId && workspace.workspaceId === pendingSessionSwitchWorkspaceId}
                                data-session-id={session.id}
                                title={session.name || i18n.t('header.unnamedSession')}
                                onclick={() => switchSession(workspace, session.id)}
                              >
                                <span
                                  class="session-running-dot"
                                  class:running={sessionIndicator === 'running'}
                                  class:unread={sessionIndicator === 'unread'}
                                  aria-hidden="true"
                                ></span>
                                <span class="session-name">{session.name || i18n.t('header.unnamedSession')}</span>
                                <span class="session-meta">
                                  <span class="session-msg-count" title={i18n.t('header.messageCount', { count: session.messageCount ?? 0 })}>{session.messageCount ?? 0}</span>
                                  <span class="session-time">{formatRelativeTime(session.updatedAt || session.createdAt)}</span>
                                </span>
                              </button>
                              <div class="session-actions">
                                <button
                                  type="button"
                                  class="session-action-btn session-rename-btn"
                                  title={i18n.t('header.renameSession')}
                                  aria-label={i18n.t('header.renameSession')}
                                  onclick={(event) => {
                                    event.stopPropagation();
                                    void beginSessionRename(workspace, session);
                                  }}
                                >
                                  <Icon name="pencil" size={12} />
                                </button>
                                <button
                                  type="button"
                                  class="session-action-btn session-delete-btn"
                                  title={i18n.t('header.deleteSession')}
                                  aria-label={i18n.t('header.deleteSession')}
                                  onclick={(event) => {
                                    event.stopPropagation();
                                    openDeleteSessionDialog(workspace, session);
                                  }}
                                >
                                  <Icon name="delete" size={12} />
                                </button>
                              </div>
                            {/if}
                          </div>
                        {/each}
                      </div>
                    {/if}
                  </div>
                {/if}
              </div>
            {/each}
          </div>
        {/if}
        {/if}
        </div>
        </section>
      {:else}
        <section class="sidebar-section sidebar-section--file-tree-mode">
        <div class="file-tree-mode-header section-title-row--sticky">
          <button
            type="button"
            class="file-tree-back-btn"
            title={i18n.t('web.projectFilesBack')}
            aria-label={i18n.t('web.projectFilesBack')}
            data-sidebar-mode="projects"
            onpointerdown={applySidebarModeFromEvent}
            onclick={applySidebarModeFromEvent}
          >
            <Icon name="chevron-right" size={12} />
            <span>{i18n.t('web.projectFilesBack')}</span>
          </button>
        </div>
        {#if ProjectFileTreeComponent}
          <ProjectFileTreeComponent
            rootPath={selectedWorkspace?.rootPath || ''}
            workspaceId={selectedWorkspaceId}
            title={selectedWorkspace?.name || i18n.t('web.projectFiles')}
            titlePath={selectedWorkspace?.rootPath || ''}
            selectedFilePath={activeCodeTabFilePath}
            onFileSelect={(selection) => handleFileSelect(selection.pathRef, {
              displayPath: selection.displayPath,
              label: selection.name,
            })}
          />
        {:else}
          <div class="sidebar-empty">{i18n.t('common.loading')}</div>
        {/if}
        </section>
      {/if}

      {#if sidebarMode === 'projects'}
      <section class="recent-sessions-section">
        <div class="section-title-row recent-sessions-header section-title-row--sticky">
          <button
            type="button"
            class="section-title-toggle"
            aria-expanded={!recentSessionsCollapsed}
            aria-controls="recent-session-content"
            aria-label={recentSessionsCollapsed ? i18n.t('web.expandRecentSessions') : i18n.t('web.collapseRecentSessions')}
            title={recentSessionsCollapsed ? i18n.t('web.expandRecentSessions') : i18n.t('web.collapseRecentSessions')}
            onclick={toggleRecentSessions}
          >
            <span class="section-title">{i18n.t('web.recentSessions')}</span>
            <span class="section-title-chevron" class:section-title-chevron--collapsed={recentSessionsCollapsed} aria-hidden="true">
              <Icon name="chevronDown" size={11} />
            </span>
          </button>
          <button
            type="button"
            class="sidebar-icon-btn sidebar-icon-btn--compact recent-session-new-btn"
            data-tooltip={i18n.t('web.newPersonalSessionTitle')}
            title={i18n.t('web.newPersonalSessionTitle')}
            aria-label={i18n.t('web.newPersonalSessionTitle')}
            disabled={workspaceActionPending || messagesState.sessionHydrating}
            onclick={openPersonalDraft}
          >
            <Icon name="plus" size={13} />
          </button>
        </div>
        {#if !recentSessionsCollapsed}
          <div id="recent-session-content">
            {#if getPersonalSessionList().length === 0}
              <div class="sidebar-empty sidebar-empty--nested">{i18n.t('web.noRecentSessions')}</div>
            {:else}
              <div class="session-list session-list--nested">
                {#each getPersonalSessionList() as session (session.id)}
                {@const sessionRunning = isSessionRunning('', session)}
                {@const sessionIndicator = resolveSessionActivityIndicator({ isRunning: sessionRunning, hasUnreadCompletion: session.hasUnreadCompletion === true })}
                <div class="session-row" class:active={session.id === currentSessionId && !currentBootstrapWorkspaceId()} class:editing={isEditingPersonalSession(session.id)}>
                  {#if isEditingPersonalSession(session.id)}
                    <div class="session-rename-editor">
                      <div class="session-rename-controls">
                        <input bind:this={sessionRenameInput} bind:value={sessionRenameDraft} class:invalid={Boolean(sessionRenameError)} class="session-rename-input" maxlength={SESSION_NAME_MAX_CHARS} aria-label={i18n.t('header.renameSession')} oninput={() => { sessionRenameError = ''; }} onkeydown={(event) => { if (event.key === 'Enter') { event.preventDefault(); void savePersonalSessionRename(session); } else if (event.key === 'Escape') { event.preventDefault(); cancelSessionRename(); } }} />
                        <button type="button" class="session-rename-action session-rename-save" title={i18n.t('header.saveSessionName')} onclick={() => void savePersonalSessionRename(session)}><Icon name="check" size={12} /></button>
                        <button type="button" class="session-rename-action" title={i18n.t('header.cancelSessionRename')} onclick={cancelSessionRename}><Icon name="x" size={12} /></button>
                      </div>
                      {#if sessionRenameError}<span class="session-rename-error">{sessionRenameError}</span>{/if}
                    </div>
                  {:else}
                    <button type="button" class="session-item" class:active={session.id === currentSessionId && !currentBootstrapWorkspaceId()} class:pending={session.id === pendingSessionSwitchId && pendingSessionSwitchWorkspaceId === null} data-session-id={session.id} title={session.name || i18n.t('header.unnamedSession')} onclick={() => switchPersonalSession(session.id)}>
                      <span class="session-running-dot" class:running={sessionIndicator === 'running'} class:unread={sessionIndicator === 'unread'} aria-hidden="true"></span>
                      <span class="session-name">{session.name || i18n.t('header.unnamedSession')}</span>
                      <span class="session-meta"><span class="session-msg-count">{session.messageCount ?? 0}</span><span class="session-time">{formatRelativeTime(session.updatedAt || session.createdAt)}</span></span>
                    </button>
                    <div class="session-actions">
                      <button type="button" class="session-action-btn session-rename-btn" title={i18n.t('header.renameSession')} onclick={() => void beginPersonalSessionRename(session)}><Icon name="pencil" size={12} /></button>
                      <button type="button" class="session-action-btn session-delete-btn" title={i18n.t('header.deleteSession')} onclick={() => openPersonalDeleteSessionDialog(session)}><Icon name="delete" size={12} /></button>
                    </div>
                  {/if}
                </div>
                {/each}
              </div>
            {/if}
          </div>
        {/if}
      </section>
      {/if}
    </div>

    <div
      class="sidebar-resize-handle"
      role="separator"
      aria-orientation="vertical"
      title={i18n.t('web.sidebarResizeReset')}
      onpointerdown={startSidebarResize}
      ondblclick={resetSidebarWidth}
    ></div>
  </aside>
  {/if}

  {#if sidebarTooltip}
    <div
      class="sidebar-tooltip"
      class:sidebar-tooltip--above={sidebarTooltip.placement === 'above'}
      style={`left:${sidebarTooltip.left}px;top:${sidebarTooltip.top}px;`}
      role="tooltip"
    >{sidebarTooltip.text}</div>
  {/if}

  <main
    class="workbench-content"
    class:workbench-content--drawer-dimmed={sidebarIsDrawer && sidebarOpen}
    aria-hidden={sidebarIsDrawer && sidebarOpen ? 'true' : 'false'}
  >
    <div
      class="workbench-body"
      class:workbench-body--with-preview={inlineRightPaneVisible && !previewIsOverlay}
      class:workbench-body--overlay-preview={inlineRightPaneVisible && previewIsOverlay}
    >
      <div class="workbench-app-pane" data-testid="workbench-app-pane">
        <App />
      </div>
      {#if inlineRightPaneVisible && RightPaneComponent}
        {#if !previewIsOverlay}
          <div
            class="preview-resize-handle"
            role="separator"
            aria-orientation="vertical"
            title={i18n.t('web.filePreviewResizeReset')}
            onpointerdown={startPreviewPanelResize}
            ondblclick={resetPreviewPanelWidth}
          ></div>
        {/if}
        <RightPaneComponent
          workspaceRoot={selectedWorkspace?.rootPath || ''}
          overlay={previewIsOverlay}
          htmlBrowserOpenRequest={htmlBrowserOpenRequest}
          onHtmlBrowserOpenHandled={(requestId) => {
            if (htmlBrowserOpenRequest?.requestId === requestId) {
              htmlBrowserOpenRequest = null;
            }
          }}
        />
      {:else if desktopAppSurface && desktopRightPaneVisible && RightPaneComponent}
        <div
          class="desktop-right-pane-resize-handle"
          role="separator"
          aria-orientation="vertical"
          title={i18n.t('web.filePreviewResizeReset')}
          onpointerdown={startDesktopRightPaneResize}
          ondblclick={resetDesktopRightPaneWidth}
        ></div>
        <div
          class="desktop-right-pane-column"
        >
          <RightPaneComponent
            workspaceRoot={selectedWorkspace?.rootPath || ''}
            desktopSurface={true}
            htmlBrowserOpenRequest={htmlBrowserOpenRequest}
            onHtmlBrowserOpenHandled={(requestId) => {
              if (htmlBrowserOpenRequest?.requestId === requestId) {
                htmlBrowserOpenRequest = null;
              }
            }}
          />
        </div>
      {/if}
    </div>
  </main>
</div>

{#if workspaceOnboardingState.open}
  <Modal
    onClose={closeAddWorkspaceDialog}
    closeOnBackdrop={true}
    size="md"
    modalClass="workspace-picker-modal-body"
    showHeader={false}
  >
    {#if workspaceDialogError}
      <div class="workspace-dialog-error workspace-dialog-error--banner">{workspaceDialogError}</div>
    {/if}
    {#if WebFolderPickerComponent}
      <WebFolderPickerComponent
        title={i18n.t('web.selectWorkspaceFolder')}
        onSelect={(selection) => void handleFolderSelected(selection)}
        onCancel={closeAddWorkspaceDialog}
        disabled={workspaceActionPending}
      />
    {:else}
      <div class="sidebar-empty">{i18n.t('common.loading')}</div>
    {/if}
  </Modal>
{/if}

{#if showRemoveWorkspaceDialog && pendingRemoveWorkspace}
  <Modal
    title={i18n.t('web.removeWorkspaceTitle')}
    onClose={closeRemoveWorkspaceDialog}
    closeOnBackdrop={true}
    size="sm"
  >
    <p class="workspace-dialog-text">{i18n.t('web.removeWorkspaceDescPrefix')}<strong>{pendingRemoveWorkspace.name}</strong>{i18n.t('web.removeWorkspaceDescSuffix')}</p>
    <p class="workspace-dialog-text workspace-dialog-text--muted">{i18n.t('web.removeWorkspaceKeepData')}</p>
    {#if workspaceDialogError}
      <div class="workspace-dialog-error">{workspaceDialogError}</div>
    {/if}

    {#snippet footer()}
      <button class="btn btn--secondary" type="button" onclick={() => closeRemoveWorkspaceDialog()} disabled={workspaceActionPending}>{i18n.t('web.folderPickerCancel')}</button>
      <button class="btn btn--danger" type="button" onclick={() => void removeWorkspace()} disabled={workspaceActionPending}>
        {workspaceActionPending ? i18n.t('web.removingWorkspace') : i18n.t('web.confirmRemoveWorkspace')}
      </button>
    {/snippet}
  </Modal>
{/if}

{#if showDeleteSessionDialog && pendingDeleteSession}
  <Modal
    title={i18n.t('header.deleteSessionTitle')}
    onClose={closeDeleteSessionDialog}
    size="sm"
    closeOnBackdrop={true}
  >
    <p>{i18n.t('header.deleteSessionConfirm', { name: pendingDeleteSession.session.name || i18n.t('header.unnamedSession') })}</p>

    {#snippet footer()}
      <button class="btn btn--secondary" type="button" onclick={closeDeleteSessionDialog}>{i18n.t('header.cancel')}</button>
      <button class="btn btn--danger" type="button" onclick={confirmDeleteSession}>{i18n.t('header.confirmDelete')}</button>
    {/snippet}
  </Modal>
{/if}

<style>
  .web-workbench-shell {
    display: grid;
    grid-template-columns: var(--sidebar-width, 320px) minmax(0, 1fr);
    gap: var(--shell-gap, 8px);
    height: 100%;
    width: 100%;
    padding: var(--shell-padding, 8px);
    box-sizing: border-box;
    background: transparent;
    color: var(--foreground);
    isolation: isolate;
    overflow: hidden;
  }

  .web-workbench-shell--desktop-right-pane-visible .workbench-body {
    grid-template-columns:
      minmax(0, 1fr)
      var(--desktop-right-pane-divider-width, 8px)
      minmax(var(--preview-min-width, 320px), var(--desktop-right-pane-width, 480px));
  }

  .web-workbench-shell--desktop-right-pane-visible .workbench-app-pane {
    grid-column: 1;
  }

  .desktop-right-pane-resize-handle {
    grid-column: 2;
    position: relative;
    z-index: 2;
    min-width: 0;
    min-height: 0;
    cursor: col-resize;
    touch-action: none;
  }

  .desktop-right-pane-resize-handle::before {
    content: '';
    position: absolute;
    inset: 0;
    width: 1px;
    margin: 0 auto;
    background: var(--border);
    opacity: 0.72;
  }

  .desktop-right-pane-resize-handle:hover {
    background: color-mix(in srgb, var(--primary) 8%, transparent);
  }

  .desktop-right-pane-column {
    grid-column: 3;
    display: flex;
    flex-direction: column;
    box-sizing: border-box;
    width: 100%;
    min-width: 0;
    min-height: 0;
  }


  .desktop-right-pane-column :global(.right-pane) {
    box-sizing: border-box;
    width: 100%;
    height: 100%;
  }

  .web-workbench-shell--desktop .desktop-right-pane-column :global(.right-pane) {
    border: 0;
    border-radius: 0;
    background: var(--magi-surface-right-pane);
    box-shadow: none;
  }

  .desktop-drop-overlay {
    position: fixed;
    z-index: var(--z-modal, 1000);
    display: flex;
    align-items: center;
    justify-content: center;
    box-sizing: border-box;
    padding: 16px;
    border: 2px dashed color-mix(in srgb, var(--primary) 70%, var(--border));
    border-radius: var(--radius-lg);
    background: color-mix(in srgb, var(--primary) 10%, var(--background));
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--primary) 12%, transparent);
    pointer-events: none;
  }

  .desktop-drop-overlay--sidebar {
    border-color: color-mix(in srgb, var(--success) 70%, var(--border));
    background: color-mix(in srgb, var(--success) 9%, var(--background));
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--success) 12%, transparent);
  }

  .desktop-drop-overlay__label {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    max-width: min(320px, 90%);
    padding: 10px 14px;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--dropdown-bg);
    color: var(--foreground);
    box-shadow: var(--shadow-md);
    font-size: var(--text-base);
    font-weight: var(--font-medium);
    text-align: center;
  }

  .sidebar {
    /* position:relative 仅用于 resize handle / tooltip 等绝对定位子元素；
       不显式 z-index，避免创建独立 stacking context 把设置面板等 fixed overlay 困在主区 pane 之下。
       drawer 模式下另有 --z-overlay-sidebar 显式控制层级。 */
    position: relative;
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    min-height: 0;
    padding: var(--space-4);
    border-radius: var(--radius-lg);
    border: 1px solid var(--border);
    background: var(--magi-surface-sidebar);
    overflow: visible;
  }

  .drawer-overlay {
    display: none;
  }

  .sidebar-header {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    flex-shrink: 0;
  }

  .sidebar-toolbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
  }

  .sidebar-header-tools {
    display: flex;
    align-items: center;
    gap: var(--space-1);
  }

  .sidebar-icon-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
    border-radius: var(--radius-md);
    border: none;
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    transition: background var(--transition-fast), color var(--transition-fast);
    flex-shrink: 0;
    position: relative;
  }

  .sidebar-icon-btn:hover {
    background: var(--surface-hover);
    color: var(--foreground);
  }

  .sidebar-icon-btn:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .sidebar-icon-btn :global(svg) {
    pointer-events: none;
  }

  .sidebar-icon-btn--compact {
    width: 24px;
    height: 24px;
    border-radius: var(--radius-sm);
  }

  /* Tooltip 挂载在 Shell 顶层，不能再由 sidebar-navigation-scroll 的
     overflow 或中间面板的绘制顺序裁剪。 */
  .sidebar-tooltip {
    position: fixed;
    z-index: var(--z-tooltip, 1200);
    max-width: min(260px, calc(100vw - 16px));
    padding: 4px 8px;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--magi-surface-popover, var(--dropdown-bg));
    box-shadow: var(--shadow-md);
    color: var(--foreground);
    font-size: var(--text-xs);
    font-weight: var(--font-medium);
    line-height: 1.3;
    white-space: nowrap;
    pointer-events: none;
    transform: translateX(-100%);
  }

  .sidebar-tooltip--above {
    transform: translate(-100%, -100%);
  }

  .session-meta,
  .sidebar-empty {
    color: var(--foreground-muted);
    font-size: var(--text-sm);
  }

  .sidebar-empty--recovering {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }

  .sidebar-empty--recovering :global(svg) {
    animation: sidebar-recovery-spin 1s linear infinite;
  }

  @keyframes sidebar-recovery-spin {
    to { transform: rotate(360deg); }
  }

  .theme-toggle-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
    border-radius: var(--radius-md);
    border: none;
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    transition: background var(--transition-fast), color var(--transition-fast);
    flex-shrink: 0;
  }

  .theme-toggle-btn:hover {
    background: var(--surface-hover);
    color: var(--foreground);
  }

  .theme-toggle-btn[data-theme-id='builtin.light'],
  .theme-toggle-btn[data-theme-id='builtin.dark'] {
    color: var(--primary);
  }

  .theme-toggle-btn:focus-visible {
    outline: 2px solid var(--primary);
    outline-offset: 2px;
  }

  .sidebar-section {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
  }

  .sidebar-navigation-scroll {
    display: flex;
    flex: 1;
    min-height: 0;
    flex-direction: column;
    gap: var(--space-3);
    overflow-y: auto;
    overflow-x: hidden;
    overscroll-behavior: contain;
    -webkit-overflow-scrolling: touch;
    scrollbar-gutter: stable;
    scrollbar-width: thin;
    scrollbar-color: var(--scrollbar-thumb) transparent;
  }

  .sidebar-navigation-scroll--files {
    overflow: hidden;
  }

  .sidebar-section--workspaces {
    flex: 0 0 auto;
    overflow: visible;
  }

  .sidebar-error-title {
    font-size: var(--text-sm);
    font-weight: var(--font-semibold);
    color: var(--foreground);
  }

  .section-title-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
  }

  .section-title-row--sticky {
    position: sticky;
    top: 0;
    z-index: 3;
    min-height: 28px;
    padding: 2px 0;
    /* 标题是滚动内容的遮挡层，使用主题基色而不是半透明面板材质，
       避免下方工作区/会话行透出来造成文字串层。 */
    background: var(--magi-canvas, var(--background));
    box-shadow: 0 1px 0 var(--border-subtle);
  }

  .section-title {
    font-size: var(--text-sm);
    font-weight: var(--font-semibold);
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--foreground-muted);
  }

  .sidebar-section--file-tree-mode {
    flex: 1;
    min-height: 0;
    overflow: visible;
  }

  .file-tree-mode-header {
    display: flex;
    align-items: center;
    padding-bottom: 2px;
  }

  .file-tree-back-btn {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    align-self: flex-start;
    max-width: 100%;
    height: 28px;
    padding: 0 8px 0 6px;
    border: none;
    border-radius: var(--radius-md);
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    transition: background var(--transition-fast), color var(--transition-fast);
  }

  .file-tree-back-btn:hover {
    background: var(--surface-hover);
    color: var(--foreground);
  }

  .file-tree-back-btn :global(svg) {
    transform: rotate(180deg);
    flex-shrink: 0;
    pointer-events: none;
  }

  .sidebar-section--file-tree-mode :global(.project-file-tree) {
    flex: 1;
    min-height: 0;
    overflow: hidden;
  }

  .sidebar-section--file-tree-mode :global(.file-tree-list) {
    flex: 1;
    overflow-y: auto;
    overflow-x: hidden;
    padding-right: var(--space-1);
    overscroll-behavior: contain;
    scrollbar-gutter: stable;
    scrollbar-width: thin;
    scrollbar-color: var(--scrollbar-thumb) transparent;
  }

  .sidebar-resize-handle {
    position: absolute;
    top: 0;
    right: -9px;
    bottom: 0;
    width: 10px;
    cursor: col-resize;
    z-index: 40;
    display: flex;
    align-items: center;
    justify-content: center;
    transition: background var(--transition-fast);
  }

  .sidebar-resize-handle::before {
    content: '';
    position: absolute;
    top: 0;
    bottom: 0;
    left: 50%;
    width: 1px;
    transform: translateX(-50%);
    background: transparent;
    transition: background var(--transition-fast);
  }

  .sidebar-resize-handle::after {
    content: '';
    width: 2px;
    height: 28px;
    border-radius: 999px;
    background: var(--border);
    opacity: 0;
    transition: opacity var(--transition-fast), background var(--transition-fast);
  }

  .sidebar-resize-handle:hover {
    background: color-mix(in srgb, var(--primary) 8%, transparent);
  }

  .sidebar-resize-handle:hover::before,
  .web-workbench-shell--sidebar-resizing .sidebar-resize-handle::before {
    background: color-mix(in srgb, var(--primary) 45%, transparent);
  }

  .sidebar-resize-handle:hover::after,
  .web-workbench-shell--sidebar-resizing .sidebar-resize-handle::after {
    background: var(--primary);
    opacity: 0.8;
  }

  .workspace-tree {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .recent-sessions-section {
    flex: 0 0 auto;
    padding-top: var(--space-2);
    border-top: 1px solid color-mix(in srgb, var(--border-subtle) 70%, transparent);
  }

  .recent-session-new-btn {
    opacity: 0.72;
    pointer-events: auto;
    transition: opacity var(--transition-fast), background var(--transition-fast), color var(--transition-fast);
  }

  .recent-session-new-btn:focus-visible {
    opacity: 1;
  }

  .recent-session-new-btn:disabled {
    opacity: 0.35;
    pointer-events: none;
  }

  .section-title-toggle {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    min-width: 0;
    padding: 2px 4px;
    border: none;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    text-align: left;
    transition: background var(--transition-fast), color var(--transition-fast);
  }

  .section-title-toggle:hover {
    background: var(--surface-hover);
    color: var(--foreground);
  }

  .section-title-toggle:focus-visible {
    outline: 2px solid var(--primary);
    outline-offset: 1px;
  }

  .section-title-chevron {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    transition: transform var(--transition-fast);
  }

  .section-title-chevron--collapsed {
    transform: rotate(-90deg);
  }

  .session-list {
    display: flex;
    flex-direction: column;
    gap: 1px;
  }

  .workspace-tree {
    flex: 0 0 auto;
    overflow: visible;
  }

  .sidebar-navigation-scroll::-webkit-scrollbar,
  .sidebar-section--file-tree-mode :global(.file-tree-list::-webkit-scrollbar) {
    width: 10px;
  }

  .sidebar-navigation-scroll::-webkit-scrollbar-track,
  .sidebar-section--file-tree-mode :global(.file-tree-list::-webkit-scrollbar-track) {
    background: color-mix(in srgb, var(--surface-2) 58%, transparent);
    border-radius: 999px;
  }

  .sidebar-navigation-scroll::-webkit-scrollbar-thumb,
  .sidebar-section--file-tree-mode :global(.file-tree-list::-webkit-scrollbar-thumb) {
    background: var(--scrollbar-thumb);
    border-radius: 999px;
    border: 2px solid color-mix(in srgb, var(--surface-1) 88%, transparent);
    background-clip: content-box;
  }

  .sidebar-navigation-scroll::-webkit-scrollbar-thumb:hover,
  .sidebar-section--file-tree-mode :global(.file-tree-list::-webkit-scrollbar-thumb:hover) {
    background: var(--scrollbar-thumb-hover);
    background-clip: content-box;
  }

  .workspace-node {
    display: flex;
    flex-direction: column;
    position: relative;
  }

  .workspace-row {
    display: flex;
    align-items: center;
    gap: 2px;
    border-radius: var(--radius-md);
    transition: background var(--transition-fast);
  }

  .workspace-row:hover {
    background: color-mix(in srgb, var(--surface-hover) 60%, transparent);
  }

  .workspace-row:hover .workspace-new-session-btn,
  .workspace-row:hover .workspace-remove-btn,
  .workspace-new-session-btn:focus-visible,
  .workspace-remove-btn:focus-visible {
    opacity: 1;
    pointer-events: auto;
  }

  .workspace-header-btn {
    display: flex;
    align-items: center;
    gap: 6px;
    flex: 1;
    min-width: 0;
    padding: 4px 6px;
    border: none;
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    text-align: left;
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
    line-height: 1.4;
    transition: color var(--transition-fast);
    touch-action: manipulation;
  }

  .workspace-header-btn:hover {
    color: var(--foreground);
  }

  .workspace-header-btn.active .workspace-name {
    color: var(--foreground);
  }

  .workspace-chevron {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 12px;
    height: 12px;
    flex-shrink: 0;
    color: var(--foreground-muted);
    transform: rotate(-90deg);
    transition: transform var(--transition-fast);
  }

  .workspace-chevron--expanded {
    transform: rotate(0deg);
  }

  :global(.workspace-folder-icon) {
    flex-shrink: 0;
    color: var(--foreground-muted);
  }

  .workspace-name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .workspace-children {
    padding-left: 18px;
    margin-top: 2px;
  }

  .workspace-new-session-btn,
  .workspace-remove-btn {
    width: 22px;
    height: 22px;
    border: none;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    font-size: 14px;
    line-height: 1;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    opacity: 0;
    pointer-events: none;
    transition: opacity var(--transition-fast), background var(--transition-fast), color var(--transition-fast);
    flex-shrink: 0;
  }

  .workspace-new-session-btn {
    color: var(--foreground-muted);
  }

  .workspace-new-session-btn:hover {
    color: var(--foreground);
    background: var(--surface-hover);
  }

  .workspace-new-session-btn:disabled {
    cursor: default;
    color: var(--foreground-muted);
  }

  .workspace-row:hover .workspace-new-session-btn:disabled,
  .workspace-new-session-btn:focus-visible:disabled {
    opacity: 0.35;
  }

  .workspace-remove-btn {
    margin-right: 4px;
  }

  .workspace-remove-btn:hover {
    color: var(--error);
    background: color-mix(in srgb, var(--error) 10%, transparent);
  }

  .session-list--nested {
    gap: 1px;
  }

  .session-row {
    position: relative;
    display: flex;
    align-items: stretch;
    border-radius: var(--radius-md);
    transition: background var(--transition-fast);
  }

  .session-row:hover {
    background: color-mix(in srgb, var(--surface-hover) 70%, transparent);
  }

  .session-row.active {
    background: color-mix(in srgb, var(--surface-selected) 78%, transparent);
  }

  .session-row:hover .session-actions,
  .session-row:focus-within .session-actions {
    opacity: 1;
    pointer-events: auto;
  }

  .session-item {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    flex: 1;
    min-width: 0;
    padding: 5px 10px;
    border: none;
    border-radius: var(--radius-md);
    background: transparent;
    color: var(--foreground);
    cursor: pointer;
    text-align: left;
    font-size: var(--text-sm);
    line-height: 1.35;
    transition: color var(--transition-fast);
    touch-action: manipulation;
  }

  .session-item.active {
    color: var(--foreground);
    font-weight: var(--font-medium);
  }

  .session-item.pending {
    opacity: 0.78;
  }

  .session-item:disabled {
    cursor: default;
  }

  .session-running-dot {
    position: relative;
    width: 14px;
    height: 14px;
    border-radius: var(--radius-full);
    background: transparent;
    opacity: 0;
    flex-shrink: 0;
  }

  .session-running-dot.running,
  .session-running-dot.unread {
    opacity: 1;
  }

  .session-running-dot.running::before,
  .session-running-dot.running::after,
  .session-running-dot.unread::before {
    content: '';
    position: absolute;
    inset: 50% auto auto 50%;
    border-radius: var(--radius-full);
    transform: translate(-50%, -50%);
    pointer-events: none;
  }

  .session-running-dot.running::before {
    width: 6px;
    height: 6px;
    background: var(--info);
    box-shadow: 0 0 8px color-mix(in srgb, var(--info) 58%, transparent);
    z-index: 1;
    animation: session-running-core-breath 1.8s ease-in-out infinite;
  }

  .session-running-dot.running::after {
    width: 6px;
    height: 6px;
    background: color-mix(in srgb, var(--info) 32%, transparent);
    box-shadow:
      0 0 0 1px color-mix(in srgb, var(--info) 52%, transparent),
      0 0 8px color-mix(in srgb, var(--info) 46%, transparent);
    animation: session-running-breath 1.8s cubic-bezier(0.2, 0.55, 0.35, 1) infinite;
  }

  .session-running-dot.unread::before {
    width: 6px;
    height: 6px;
    background: var(--success);
    box-shadow: 0 0 7px color-mix(in srgb, var(--success) 42%, transparent);
  }

  @keyframes session-running-breath {
    0% {
      opacity: 0.78;
      transform: translate(-50%, -50%) scale(0.7);
    }
    48% {
      opacity: 0.34;
      transform: translate(-50%, -50%) scale(1.8);
    }
    82% {
      opacity: 0;
      transform: translate(-50%, -50%) scale(2.8);
    }
    100% {
      opacity: 0;
      transform: translate(-50%, -50%) scale(2.8);
    }
  }

  @keyframes session-running-core-breath {
    0%, 100% {
      opacity: 0.72;
      box-shadow: 0 0 4px color-mix(in srgb, var(--info) 38%, transparent);
    }
    50% {
      opacity: 1;
      box-shadow: 0 0 10px color-mix(in srgb, var(--info) 78%, transparent);
    }
  }

  .session-name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .session-meta {
    display: inline-flex;
    align-items: center;
    justify-content: flex-end;
    gap: 6px;
    min-width: 58px;
    flex-shrink: 0;
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    white-space: nowrap;
    transition: opacity var(--transition-fast);
  }

  .session-msg-count {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: 18px;
    height: 16px;
    padding: 0 5px;
    border-radius: 8px;
    background: color-mix(in srgb, var(--foreground) 10%, transparent);
    color: var(--foreground-muted);
    font-size: 10px;
    font-weight: var(--font-medium);
    line-height: 1;
  }

  .session-row:hover .session-meta,
  .session-row:focus-within .session-meta {
    opacity: 0;
    pointer-events: none;
  }

  .session-actions {
    position: absolute;
    top: 50%;
    right: 6px;
    transform: translateY(-50%);
    display: inline-flex;
    align-items: center;
    justify-content: flex-end;
    width: 58px;
    gap: 1px;
    padding-left: 8px;
    background: linear-gradient(90deg, transparent, var(--surface-hover) 28%);
    opacity: 0;
    pointer-events: none;
    transition: opacity var(--transition-fast);
  }

  .session-action-btn,
  .session-rename-action {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 22px;
    border: none;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    transition: background var(--transition-fast), color var(--transition-fast);
    flex-shrink: 0;
  }

  .session-rename-btn:hover,
  .session-rename-save:hover {
    color: var(--info);
    background: color-mix(in srgb, var(--info) 12%, transparent);
  }

  .session-delete-btn:hover {
    color: var(--error);
    background: color-mix(in srgb, var(--error) 12%, transparent);
  }

  .session-rename-editor {
    display: flex;
    flex: 1;
    min-width: 0;
    flex-direction: column;
    gap: 3px;
    padding: 4px 6px;
  }

  .session-rename-controls {
    display: flex;
    align-items: center;
    min-width: 0;
    gap: 2px;
  }

  .session-rename-input {
    flex: 1;
    min-width: 0;
    height: 24px;
    padding: 0 6px;
    border: 1px solid var(--border-focus);
    border-radius: var(--radius-sm);
    outline: none;
    background: var(--surface);
    color: var(--foreground);
    font: inherit;
  }

  .session-rename-input:focus {
    box-shadow: 0 0 0 2px color-mix(in srgb, var(--border-focus) 24%, transparent);
  }

  .session-rename-input.invalid {
    border-color: var(--error);
  }

  .session-rename-error {
    padding-left: 1px;
    color: var(--error);
    font-size: 10px;
    line-height: 1.25;
  }

  .session-rename-action:disabled,
  .session-rename-input:disabled {
    cursor: wait;
    opacity: 0.55;
  }

  .sidebar-empty--nested {
    padding: var(--space-2) 0 var(--space-2) var(--space-2);
  }

  .sidebar-error {
    padding: var(--space-3);
    border-radius: var(--radius-md);
    border: 1px solid rgba(239, 68, 68, 0.3);
    background: var(--error-muted);
    color: var(--foreground);
    font-size: var(--text-base);
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }

  .workbench-content {
    position: relative;
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
    overflow: hidden;
  }

  .workbench-body {
    position: relative;
    flex: 1;
    min-width: 0;
    min-height: 0;
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    overflow: hidden;
  }

  .workbench-body--with-preview {
    grid-template-columns:
      minmax(var(--workbench-min-content-width, 448px), 1fr)
      var(--preview-handle-width, 8px)
      minmax(var(--preview-min-width, 320px), var(--preview-panel-width, 360px));
  }

  .workbench-app-pane {
    /* 外层只负责中栏的边界、圆角和裁切；真实主题材质由内部 App 的
       .app-container 唯一提供，避免普通 Web 模式出现双层中栏背景。
       不要再创建独立 stacking context，否则内部的 .settings-overlay 等全局
       modal 会被困在 pane 子树（auto=0）内，被相邻的 file-preview-panel 等覆盖。
       外层 .web-workbench-shell 已用 isolation: isolate 做了一层隔离。 */
    min-width: 0;
    min-height: 0;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    overflow: hidden;
  }

  .preview-resize-handle {
    position: relative;
    min-width: 0;
    min-height: 0;
    cursor: col-resize;
    display: flex;
    align-items: center;
    justify-content: center;
    transition: background var(--transition-fast);
    z-index: 2;
  }

  .web-workbench-shell--desktop {
    gap: 0;
    padding: 0;
    background: transparent;
  }

  .web-workbench-shell--desktop .sidebar {
    border: 0;
    border-right: 1px solid var(--border);
    border-radius: 0;
    background: var(--magi-surface-sidebar);
  }

  .web-workbench-shell--desktop .workbench-content,
  .web-workbench-shell--desktop .workbench-body {
    background: transparent;
  }

  .web-workbench-shell--desktop .workbench-app-pane {
    border: 0;
    border-radius: 0;
    background: transparent;
  }

  .preview-resize-handle::before {
    content: '';
    position: absolute;
    top: 0;
    bottom: 0;
    width: 1px;
    background: transparent;
    transition: background var(--transition-fast);
  }

  .preview-resize-handle::after {
    content: '';
    width: 2px;
    height: 32px;
    border-radius: 999px;
    background: var(--border);
    opacity: 0;
    transition: opacity var(--transition-fast), background var(--transition-fast);
  }

  .preview-resize-handle:hover {
    background: color-mix(in srgb, var(--primary) 8%, transparent);
  }

  .preview-resize-handle:hover::before,
  .web-workbench-shell--preview-resizing .preview-resize-handle::before {
    background: color-mix(in srgb, var(--primary) 45%, transparent);
  }

  .preview-resize-handle:hover::after,
  .web-workbench-shell--preview-resizing .preview-resize-handle::after {
    background: var(--primary);
    opacity: 0.8;
  }

  .workbench-content--drawer-dimmed {
    pointer-events: none;
    user-select: none;
  }

  /* 抽屉模式：sidebar 离开网格，悬浮覆盖 */
  .web-workbench-shell--sidebar-drawer {
    grid-template-columns: minmax(0, 1fr);
  }

  /* 折叠模式：sidebar 不渲染，shell 收为单列 */
  .web-workbench-shell--sidebar-hidden {
    grid-template-columns: minmax(0, 1fr);
  }

  .web-workbench-shell--sidebar-drawer .sidebar {
    position: fixed;
    top: 0;
    left: 0;
    bottom: 0;
    width: min(86vw, 320px);
    max-width: 320px;
    z-index: var(--z-overlay-sidebar);
    transform: translateX(calc(-100% - 16px));
    transition: transform var(--transition-normal);
    box-shadow: var(--shadow-lg);
    overflow: hidden;
  }

  .web-workbench-shell--sidebar-drawer .sidebar--open {
    transform: translateX(0);
  }

  .web-workbench-shell--sidebar-drawer .drawer-overlay {
    display: block;
    position: fixed;
    inset: 0;
    background: color-mix(in srgb, var(--overlay-heavy) 88%, transparent);
    z-index: calc(var(--z-overlay-sidebar) - 1);
    border: none;
    cursor: pointer;
  }

  .web-workbench-shell--sidebar-drawer .sidebar-resize-handle {
    display: none;
  }

  /* 主对话不足最小宽度时，右栏切换为覆盖层；窄平板和手机共用同一覆盖逻辑。 */
  .web-workbench-shell--preview-overlay :global(.right-pane) {
    position: absolute;
    inset: 0;
    z-index: var(--z-overlay-preview);
    border-radius: var(--radius-lg);
    border: 1px solid var(--border);
    background: var(--background);
    box-shadow: var(--shadow-lg);
  }

  .web-workbench-shell--preview-overlay :global(.header-bar) {
    display: none;
  }

  .workspace-dialog-text {
    margin: 0;
    color: var(--foreground);
    line-height: 1.6;
  }

  .workspace-dialog-text--muted {
    color: var(--foreground-muted);
    font-size: var(--text-sm);
  }

  .workspace-dialog-error {
    margin-top: var(--space-3);
    padding: var(--space-3);
    border-radius: var(--radius-md);
    border: 1px solid color-mix(in srgb, var(--error) 40%, var(--border));
    background: color-mix(in srgb, var(--error) 8%, var(--surface-1));
    color: var(--foreground);
    font-size: var(--text-sm);
  }

  .workspace-dialog-error--banner {
    margin: 12px 16px 0;
  }

  :global(.workspace-picker-modal-body) {
    padding: 0;
  }

  @media (max-width: 1120px) {
    .web-workbench-shell:not(.web-workbench-shell--sidebar-drawer):not(.web-workbench-shell--sidebar-hidden) {
      grid-template-columns: var(--sidebar-width, 240px) minmax(0, 1fr);
    }
  }

  @media (max-width: 900px) {
    .web-workbench-shell {
      padding: 0;
      gap: 0;
    }

    .workbench-app-pane,
    .web-workbench-shell--preview-overlay :global(.right-pane) {
      border: 0;
      border-radius: 0;
    }

    .web-workbench-shell--sidebar-drawer .sidebar {
      top: 0;
      left: 0;
      bottom: 0;
      transform: translateX(-100%);
      border-radius: 0;
      border: none;
      padding:
        calc(var(--space-4) + env(safe-area-inset-top))
        var(--space-4)
        calc(var(--space-4) + env(safe-area-inset-bottom));
      background: var(--vscode-sideBar-secondaryBackground, var(--background));
      contain: layout paint style;
    }

    .web-workbench-shell--sidebar-drawer .sidebar--open {
      transform: translateX(0);
    }

    .sidebar-section {
      gap: var(--space-2);
    }

    .file-tree-back-btn {
      height: 34px;
      font-size: var(--text-base);
    }

    .workspace-new-session-btn,
    .workspace-remove-btn {
      width: 28px;
      height: 28px;
      opacity: 1;
      pointer-events: auto;
    }

    .workspace-new-session-btn:disabled {
      opacity: 0.35;
    }

    .sidebar-drawer-close {
      width: 36px;
      height: 36px;
    }

    .workspace-tree {
      padding-right: 0;
      gap: var(--space-3);
    }

    .sidebar-header,
    .sidebar-section {
      background: color-mix(in srgb, var(--foreground) 3%, var(--vscode-sideBar-secondaryBackground, var(--background)));
    }

    .session-item.active {
      background: color-mix(in srgb, var(--info) 10%, var(--vscode-sideBar-secondaryBackground, var(--background)));
    }

    .workspace-header-btn,
    .session-item {
      padding: 8px 10px;
      font-size: var(--text-base);
      line-height: 1.35;
    }

    .session-meta {
      font-size: var(--text-sm);
    }

    .workspace-children {
      padding-left: 22px;
      margin-top: 4px;
    }

    .session-list--nested {
      gap: 2px;
    }
  }

  @media (max-width: 480px) {
    .sidebar {
      width: min(92vw, 360px);
      max-width: 360px;
    }

    .sidebar-header {
      gap: var(--space-2);
    }

    .workspace-header-btn,
    .session-item {
      padding: 8px 10px;
    }
  }

  :global(html.magi-web-drawer-open),
  :global(body.magi-web-drawer-open) {
    overflow: hidden;
    overscroll-behavior: none;
  }
</style>
