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
    shouldMarkSessionCompletionViewed,
  } from '../lib/session-activity-indicator';
  import { getClientBridge } from '../shared/bridges/bridge-runtime';
  import { normalizeRustBootstrapPayload } from '../shared/bridges/rust-daemon-contract';
  import { i18n } from '../stores/i18n.svelte';
  import type { EditContentKind, Session } from '../types/message';
  import {
    cycleBuiltinAppearance,
    subscribeAppearanceRuntime,
    type AppearanceRuntimeSnapshot,
  } from '../appearance/runtime';
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
    type AgentConnectionEventDetail,
    type AgentWorkspaceSummary,
  } from './agent-api';
  import {
    agentBindingWorkspaceId,
    agentBindingWorkspacePath,
    resolveAgentBindingContext,
    type AgentBindingOverride,
  } from './agent-binding-context';
  import { navigateSession, sessionNavigationState } from '../shared/session-navigation.svelte';
  import {
    rightPaneState,
    getRightPaneState,
    openCodeTab,
    setRightPaneCollapsed,
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

  interface Props {
    desktopAppSurface?: boolean;
  }

  let { desktopAppSurface = false }: Props = $props();

  // 这些 storage key 必须先于下方 `$state` 初始化器声明——它们被
  // readInitialExpandedWorkspaces / readInitialSidebarMode / readInitialRecentSessionsCollapsed
  // 在 $state 初始化时读取，
  // 普通的 const 受 TDZ 约束，定义在文件下方会触发 ReferenceError。
  const SIDEBAR_EXPANDED_WORKSPACES_KEY = 'magi-sidebar-expanded-workspaces';
  const SIDEBAR_MODE_KEY = 'magi-sidebar-mode';
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
  let recentSessionsCollapsed = $state(readInitialRecentSessionsCollapsed());
  let workspaceSelectionPending = $state(false);
  let viewportWidth = $state(typeof window !== 'undefined' ? window.innerWidth : 1440);
  let sidebarOpen = $state(false);
  let workspaceActionPending = $state(false);
  let pendingWorkspaceRegistrationDisplayPath = '';
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
  let desktopRightPaneVisible = $state(false);
  let desktopSnapshotEpoch = '';
  let desktopSnapshotRevision = -1;
  let sidebarElement = $state<HTMLElement | null>(null);
  let desktopDropIndicator = $state<{
    zone: DesktopDropZone;
    rect: DesktopDropRect;
  } | null>(null);
  let workspaceSessionRequestSeq = 0;
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
  type RightPaneProps = { workspaceRoot: string; overlay?: boolean; desktopSurface?: boolean };
  type WebFolderPickerProps = {
    title?: string;
    onSelect: (selection: WorkspaceFileSelection) => void;
    onCancel: () => void;
    disabled?: boolean;
  };

  let ProjectFileTreeComponent = $state<Component<ProjectFileTreeProps> | null>(null);
  let RightPaneComponent = $state<Component<RightPaneProps> | null>(null);
  let WebFolderPickerComponent = $state<Component<WebFolderPickerProps> | null>(null);
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
  const DEFAULT_PREVIEW_PANEL_WIDTH = 360;
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
    `--workbench-min-content-width: ${PANEL_LAYOUT.minContentWidth}px`,
    `--preview-min-width: ${PANEL_LAYOUT.minPreviewWidth}px`,
    `--preview-handle-width: ${PANEL_LAYOUT.previewHandleWidth}px`,
  ].filter(Boolean).join('; '));

  const effectiveSidebarWidth = $derived(
    sidebarWidth ?? (viewportWidth <= 1120 ? COMPACT_SIDEBAR_WIDTH : DEFAULT_SIDEBAR_WIDTH)
  );
  const effectivePreviewPanelWidth = $derived(previewPanelWidth ?? DEFAULT_PREVIEW_PANEL_WIDTH);
  const panelLayout = $derived(resolvePanelLayout({
    viewportWidth,
    sidebarWidth: effectiveSidebarWidth,
    previewPanelWidth: effectivePreviewPanelWidth,
  }));
  const sidebarIsDrawer = $derived(panelLayout.sidebarDrawer);

  /** 当前 session 的右栏多 tab 状态；由 right-pane store 派生 */
  const activeRightPaneState = $derived(getRightPaneState(rightPaneState.activeScopeKey));
  /** Desktop 的窗口布局以 Main snapshot 为准；Web 客户端仍使用本地面板状态。 */
  const rightPaneVisible = $derived(
    desktopAppSurface ? desktopRightPaneVisible : !activeRightPaneState.collapsed,
  );
  const inlineRightPaneVisible = $derived(!desktopAppSurface && rightPaneVisible);
  const panelVisibility = $derived(resolvePanelVisibility({
    sidebarDrawer: sidebarIsDrawer,
    panelsCanCoexist: panelLayout.panelsCanCoexist,
    sidebarPreferredOpen: !sidebarCollapsed,
    sidebarDrawerOpen: sidebarOpen,
    rightPaneOpen: inlineRightPaneVisible,
  }));
  const sidebarHidden = $derived(!sidebarIsDrawer && !panelVisibility.sidebarVisible);
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
    if (inlineRightPaneVisible) {
      void loadRightPane().catch((error) => {
        console.error('[WebWorkbenchShell] 右侧面板加载失败:', error);
        addToast('error', i18n.t('app.featureLoadFailed'));
        setRightPaneCollapsed(rightPaneState.activeScopeKey, true);
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
    return workspace ? workspaceBindingPath(workspace) : '';
  }

  function workspaceBindingPath(workspace: AgentWorkspaceSummary): string {
    return workspace.rootPathRef?.trim() || workspace.rootPath.trim();
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

  // 列表同步 effect：只按列表自身的工作区作用域投影，不能用当前草稿工作区推断归属。
  // 与"激活指针同步"正交——删除当前会话时 currentSessionId 会被后端清空，但列表本身的
  // 增删（删除/新建/改名）必须独立地落到 sessionsByWorkspace 上，否则左侧列表不刷新。
  $effect(() => {
    const sessionsWorkspaceId = messagesState.workspaceSessionProjection.workspaceId?.trim() || '';
    if (!sessionsWorkspaceId) {
      return;
    }
    const currentSessions = messagesState.workspaceSessionProjection.sessions;
    const projectionRuntimeEpoch = messagesState.workspaceSessionProjection.runtimeEpoch?.trim() || '';
    const projectionNextSequence = messagesState.workspaceSessionProjection.eventStreamNextSequence;
    if (projectionRuntimeEpoch && projectionNextSequence >= 1) {
      workspaceSessionCursorByWorkspace.set(sessionsWorkspaceId, {
        runtimeEpoch: projectionRuntimeEpoch,
        eventStreamNextSequence: projectionNextSequence,
      });
    }
    const existingSessions = sessionsByWorkspace[sessionsWorkspaceId] ?? [];
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
      sessionsByWorkspace = {
        ...sessionsByWorkspace,
        [sessionsWorkspaceId]: currentSessions,
      };
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
    try {
      const snapshot = await getPersonalSessions();
      recentSessions = snapshot.sessions;
      replacePersonalSessionProjection(snapshot.sessions, {
        runtimeEpoch: snapshot.runtimeEpoch,
        eventStreamNextSequence: snapshot.eventStreamNextSequence,
      });
    } catch (error) {
      console.warn('[WebWorkbenchShell] 刷新个人会话失败:', error);
    }
  }

  function openPersonalDraft(): void {
    if (workspaceActionPending || messagesState.sessionHydrating || pendingNavigation) return;
    navigateSession({ kind: 'draft', scope: 'personal' });
    if (sidebarIsDrawer) sidebarOpen = false;
  }

  function switchPersonalSession(sessionId: string): void {
    if (!sessionId || pendingNavigation) return;
    navigateSession({ kind: 'session', scope: 'personal', sessionId });
    if (sidebarIsDrawer) sidebarOpen = false;
  }

  function isSessionRunning(workspaceId: string, session: Session): boolean {
    const runningTaskCount = typeof session.runningTaskCount === 'number'
      ? session.runningTaskCount
      : 0;
    if (session.isRunning === true || runningTaskCount > 0) {
      return true;
    }
    if (!workspaceId) {
      return !currentBootstrapWorkspaceId()
        && session.id === currentSessionId
        && messagesState.isProcessing === true;
    }
    return workspaceId === selectedWorkspaceId
      && session.id === currentSessionId
      && messagesState.isProcessing === true;
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

  function toggleRecentSessions(): void {
    recentSessionsCollapsed = !recentSessionsCollapsed;
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
    event.preventDefault();
    isSidebarResizing = true;
    document.body.style.cursor = 'col-resize';
    document.body.style.userSelect = 'none';

    const handlePointerMove = (moveEvent: PointerEvent) => {
      sidebarWidth = clampSidebarWidth(moveEvent.clientX - PANEL_LAYOUT.shellPadding);
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
    event.preventDefault();
    isPreviewPanelResizing = true;
    document.body.style.cursor = 'col-resize';
    document.body.style.userSelect = 'none';

    const handlePointerMove = (moveEvent: PointerEvent) => {
      previewPanelWidth = clampPreviewPanelWidth(
        window.innerWidth - moveEvent.clientX - PANEL_LAYOUT.shellPadding,
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
    const workspaceId = metadata.workspaceId?.trim() || selectedWorkspace?.workspaceId?.trim() || selectedWorkspaceId.trim();
    const currentBinding = currentWorkspaceBinding();
    // 文件树事件不携带 session 元数据。桌面端的右栏是独立 Renderer，不能假设
    // 它已经先于文件点击完成上下文同步；优先使用当前工作区的权威会话，再由
    // right-pane store 处理无会话的 workspace 草稿，避免代码/图片 Tab 被投影到
    // personal 或旧浏览器 scope 中。
    const sessionId = metadata.sessionId?.trim()
      || (workspaceId === currentBinding.workspaceId ? currentBinding.sessionId : '')
      || (workspaceId === rightPaneState.activeWorkspaceId ? rightPaneState.activeSessionId : '');
    const workspacePath = metadata.workspacePath?.trim()
      || (selectedWorkspace ? workspaceBindingPath(selectedWorkspace) : '')
      || workspacePathForId(workspaceId)
      || (workspaceId === currentBinding.workspaceId ? currentBinding.workspacePath : '');
    if (!workspaceId || !workspacePath) {
      return false;
    }
    const normalizedFilePath = filePath.trim();
    if (!normalizedFilePath) {
      return false;
    }
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
      imageDataUrl: metadata.imageDataUrl,
    });
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
      notifyWorkbenchError(i18n.t('web.action.loadWorkspaceSessions'), error);
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
      const snapshot = await getWorkspaceSessions(requestedWorkspaceId, workspaceBindingPath(workspace));
      if (workspaceSessionRequestSeqByWorkspace.get(requestedWorkspaceId) !== requestSeq) {
        return false;
      }
      commitSidebarWorkspaceSessionsSnapshot(requestedWorkspaceId, snapshot);
      return true;
    } catch (error) {
      notifyWorkbenchError(i18n.t('web.action.loadWorkspaceSessions'), error);
      return false;
    } finally {
      finishWorkspaceSessionRequest(requestedWorkspaceId, requestSeq);
    }
  }

  async function refreshWorkspaces(): Promise<void> {
    loading = true;
    loadError = '';
    agentBaseUrl = resolveAgentBaseUrl();
    try {
      const next = await listAgentWorkspaces();
      workspaces = next;
      invalidateWorkspaceSessionRequests();
      workspaceSessionCursorByWorkspace.clear();
      sessionsByWorkspace = {};
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
      loadError = i18n.t('web.workspaceUnavailable');
      notifyWorkbenchError(i18n.t('web.action.loadWorkspaceList'), error);
    } finally {
      loading = false;
    }
  }

  async function registerWorkspaceRoot(rootPath: string, openDraft: boolean): Promise<void> {
    const expectedDisplayPath = pendingWorkspaceRegistrationDisplayPath || rootPath;
    const next = await registerAgentWorkspace(rootPath);
    const addedWorkspace = next.find((workspace) => workspace.rootPath === expectedDisplayPath) ?? null;
    if (!addedWorkspace) {
      throw new Error(`注册后未找到工作区: ${expectedDisplayPath}`);
    }

    workspaces = next;
    selectedWorkspaceId = addedWorkspace.workspaceId;
    expandedWorkspaceIds = {
      ...expandedWorkspaceIds,
      [addedWorkspace.workspaceId]: true,
    };

    if (openDraft) {
      navigateSession({
        kind: 'draft',
        scope: 'workspace',
        workspaceId: addedWorkspace.workspaceId,
        workspacePath: workspaceBindingPath(addedWorkspace),
      });
      if (sidebarIsDrawer) sidebarOpen = false;
      return;
    }

    await refreshWorkspaceSessions(
      addedWorkspace.workspaceId,
      addedWorkspace.rootPath,
    );
  }

  async function handleFolderSelected(
    selection: { pathRef: string; displayPath: string; name: string },
  ): Promise<void> {
    if (workspaceActionPending) {
      return;
    }
    workspaceDialogError = '';
    const normalizedRootPath = selection.pathRef.trim();
    const displayPath = selection.displayPath.trim();
    if (!normalizedRootPath || !displayPath) {
      return;
    }
    const onboardingOrigin = workspaceOnboardingState.origin;
    closeAddWorkspaceDialog({ force: true });
    workspaceActionPending = true;
    pendingWorkspaceRegistrationDisplayPath = displayPath;
    try {
      await runActionWithFeedback(
        () => registerWorkspaceRoot(normalizedRootPath, onboardingOrigin === 'composer'),
        {
          actionLabel: i18n.t('web.action.addWorkspace'),
          successMessage: i18n.t('web.workspaceAdded'),
        },
      );
    } finally {
      pendingWorkspaceRegistrationDisplayPath = '';
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
        await registerWorkspaceRoot(dropped.path, true);
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
    if (workspaceActionPending) {
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
    const removedPath = pendingRemoveWorkspace.rootPath;
    const removedName = pendingRemoveWorkspace.name;

    // 立即关闭弹窗，不等 API 返回
    closeRemoveWorkspaceDialog({ force: true });
    workspaceActionPending = true;

    try {
      const next = await runActionWithFeedback(
        () => removeAgentWorkspace(removedId, removedPath),
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
          await refreshWorkspaceSessions(
            selectedWorkspaceId,
            workspacePathForId(selectedWorkspaceId),
          );
        }
      }
    } finally {
      workspaceActionPending = false;
    }
  }

  async function selectWorkspace(workspace: AgentWorkspaceSummary): Promise<void> {
    if (workspaceActionPending || messagesState.sessionHydrating || pendingNavigation || workspaceSelectionPending) {
      return;
    }
    const workspaceId = workspace.workspaceId.trim();
    const workspacePath = workspaceBindingPath(workspace);
    if (!workspaceId || !workspacePath || workspaceId === selectedWorkspaceId) {
      return;
    }

    workspaceSelectionPending = true;
    try {
      const hasLoadedSessions = Object.prototype.hasOwnProperty.call(sessionsByWorkspace, workspaceId);
      if (!hasLoadedSessions) {
        const loaded = await loadWorkspaceSessionsForSidebar(workspace);
        if (!loaded || pendingNavigation) {
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
    if (workspaceSelectionPending || workspaceActionPending || messagesState.sessionHydrating || pendingNavigation) {
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
    if (workspaceActionPending || messagesState.sessionHydrating || pendingNavigation) {
      return;
    }
    const workspaceId = workspace.workspaceId.trim();
    const workspacePath = workspaceBindingPath(workspace);
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
    if (!sessionId || isCurrentSelection || pendingNavigation) {
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
      workspacePath: workspaceBindingPath(workspace),
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
    if (renamingSessionId || pendingNavigation) {
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
    try {
      const snapshot = await runActionWithFeedback(
        () => renameAgentSession(session.id, normalizedName, {
          scope: 'workspace',
          workspaceId: workspace.workspaceId,
          workspacePath: workspaceBindingPath(workspace),
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
        workspacePath: workspaceBindingPath(workspace),
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
      editingSession = null;
      sessionRenameDraft = '';
      sessionRenameInput = null;
    } finally {
      renamingSessionId = null;
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
    pendingDeleteSession = { workspace, session };
    showDeleteSessionDialog = true;
  }

  async function beginPersonalSessionRename(session: Session): Promise<void> {
    if (renamingSessionId || pendingNavigation) return;
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
    try {
      await renameAgentSession(session.id, name, {});
      await refreshPersonalSessions();
      cancelSessionRename();
    } finally {
      renamingSessionId = null;
    }
  }

  function openPersonalDeleteSessionDialog(session: Session): void {
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
        workspacePath: workspaceBindingPath(workspace),
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
      const desktop = window.magiDesktop;
      if (!desktop) {
        console.error('[WebWorkbenchShell] Desktop preload bridge 不可用');
        return;
      }
      void desktop.submitLayoutIntent({ type: 'right_pane_visibility', visible })
        .catch((error) => console.warn('[WebWorkbenchShell] 更新桌面右栏布局失败:', error));
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
    if (rightPaneVisible && !panelLayout.panelsCanCoexist) {
      requestRightPaneVisibility(false);
    }
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
    const collapsed = !desktopRightPaneVisible;
    if (pane.collapsed !== collapsed) {
      setRightPaneCollapsed(scopeKey, collapsed);
    }
  });

  onMount(() => {
    if (!desktopAppSurface) return;
    const desktop = window.magiDesktop;
    if (!desktop) {
      throw new Error('desktop_preload_bridge_unavailable');
    }
    let disposed = false;
    const applySnapshot = (snapshot: MagiDesktopWindowSnapshot) => {
      if (
        snapshot.desktopEpoch === desktopSnapshotEpoch
        && snapshot.snapshotRevision < desktopSnapshotRevision
      ) {
        return;
      }
      desktopSnapshotEpoch = snapshot.desktopEpoch;
      desktopSnapshotRevision = snapshot.snapshotRevision;
      desktopRightPaneVisible = snapshot.layout.rightPaneVisible;
    };
    void desktop.getSnapshot().then((snapshot) => {
      if (!disposed) applySnapshot(snapshot);
    }).catch((error) => {
      if (!disposed) console.error('[WebWorkbenchShell] 获取桌面窗口快照失败:', error);
    });
    const stopSnapshot = desktop.onSnapshot(applySnapshot);
    return () => {
      disposed = true;
      stopSnapshot();
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
    window.addEventListener('magi:previewFile', handlePreviewFile as EventListener);
    window.addEventListener(RUNTIME_CONNECTION_EVENT, handleAgentConnection as EventListener);
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
      window.removeEventListener('magi:previewFile', handlePreviewFile as EventListener);
      window.removeEventListener(RUNTIME_CONNECTION_EVENT, handleAgentConnection as EventListener);
      window.removeEventListener('keydown', handlePanelEscape);
      if (resizeRaf !== null) {
        cancelAnimationFrame(resizeRaf);
      }
    };
  });

  onMount(() => {
    return subscribeAppearanceRuntime((snapshot) => {
      appearanceRuntime = snapshot;
    });
  });
</script>

<div
  class="web-workbench-shell"
  class:web-workbench-shell--sidebar-drawer={sidebarIsDrawer}
  class:web-workbench-shell--sidebar-open={sidebarIsDrawer && sidebarOpen}
  class:web-workbench-shell--sidebar-hidden={sidebarHidden}
  class:web-workbench-shell--preview-overlay={previewIsOverlay}
  class:web-workbench-shell--has-preview={inlineRightPaneVisible}
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
  <aside bind:this={sidebarElement} class="sidebar" class:sidebar--open={sidebarIsDrawer && sidebarOpen}>
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

    <div class="sidebar-navigation-scroll">
      {#if sidebarMode === 'projects'}
        <section class="sidebar-section sidebar-section--workspaces">
        <div class="section-title-row">
          <div class="section-title">{i18n.t('common.workspace')}</div>
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
                      workspacePathRef: workspaceBindingPath(workspace),
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
                    title={i18n.t('web.newWorkspaceSessionTitle')}
                    aria-label={i18n.t('web.newWorkspaceSessionAria', { name: workspace.name })}
                    disabled={workspaceActionPending || messagesState.sessionHydrating || Boolean(pendingNavigation)}
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
                                disabled={pendingNavigation !== null}
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
        </section>
      {:else}
        <section class="sidebar-section sidebar-section--file-tree-mode">
        <div class="file-tree-mode-header">
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

      <div class="recent-sessions-section">
        <div class="section-title-row recent-sessions-header">
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
            disabled={workspaceActionPending || messagesState.sessionHydrating || Boolean(pendingNavigation)}
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
                    <button type="button" class="session-item" class:active={session.id === currentSessionId && !currentBootstrapWorkspaceId()} class:pending={session.id === pendingSessionSwitchId && pendingSessionSwitchWorkspaceId === null} data-session-id={session.id} disabled={pendingNavigation !== null} title={session.name || i18n.t('header.unnamedSession')} onclick={() => switchPersonalSession(session.id)}>
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
      </div>
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
        <App {desktopAppSurface} />
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
        />
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
    gap: 8px;
    height: 100vh;
    width: 100vw;
    padding: 8px;
    box-sizing: border-box;
    background: transparent;
    color: var(--foreground);
    isolation: isolate;
    overflow: hidden;
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

  /* 自定义 tooltip（图标按钮通用） */
  .sidebar-icon-btn::after,
  .theme-toggle-btn::after {
    content: attr(data-tooltip);
    position: absolute;
    top: calc(100% + 6px);
    left: 50%;
    transform: translateX(-50%);
    padding: 4px 8px;
    font-size: var(--text-xs);
    font-weight: var(--font-medium);
    color: var(--foreground);
    background: var(--glass-bg);
    backdrop-filter: blur(12px);
    -webkit-backdrop-filter: blur(12px);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    white-space: nowrap;
    pointer-events: none;
    opacity: 0;
    transition: opacity var(--transition-fast);
    z-index: var(--z-tooltip);
  }

  .sidebar-icon-btn:hover::after,
  .theme-toggle-btn:hover::after {
    opacity: 1;
  }

  .sidebar-icon-btn:disabled::after {
    display: none;
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

  .recent-sessions-header {
    position: sticky;
    top: 0;
    z-index: 3;
    min-height: 28px;
    padding: 2px 0;
    /* 侧栏自身已经承担皮肤背景；这里不能再次铺半透明背景，否则在
       壁纸/透明主题下会产生明显的矩形叠色。 */
    background: transparent;
  }

  .recent-session-new-btn {
    opacity: 0;
    pointer-events: none;
    transition: opacity var(--transition-fast), background var(--transition-fast), color var(--transition-fast);
  }

  .recent-sessions-header:hover .recent-session-new-btn,
  .recent-session-new-btn:focus-visible {
    opacity: 1;
    pointer-events: auto;
  }

  .recent-sessions-header:hover .recent-session-new-btn:disabled {
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
    color: var(--foreground-subtle);
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
    /* 不要再创建独立 stacking context，否则内部的 .settings-overlay 等全局 modal
       会被困在 pane 子树（auto=0）内，被相邻的 file-preview-panel 等覆盖。
       外层 .web-workbench-shell 已用 isolation: isolate 做了一层隔离。 */
    min-width: 0;
    min-height: 0;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--magi-surface-main);
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
    top: 8px;
    left: 8px;
    bottom: 8px;
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
