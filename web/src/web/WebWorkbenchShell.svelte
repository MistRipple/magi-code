<script lang="ts">
  import { onMount } from 'svelte';
  import App from '../App.svelte';
  import { setWebSidebarContext } from './sidebar-context';
  import Icon from '../components/Icon.svelte';
  import Modal from '../components/Modal.svelte';
  import { runActionWithFeedback } from '../lib/action-feedback';
  import type { IconName } from '../lib/icons';
  import { addToast, messagesState, setCurrentSessionId } from '../stores/messages.svelte';
  import { getClientBridge } from '../shared/bridges/bridge-runtime';
  import { i18n } from '../stores/i18n.svelte';
  import type { EditContentKind, Session } from '../types/message';
  import FilePreviewPanel from './FilePreviewPanel.svelte';
  import ProjectFileTree from './ProjectFileTree.svelte';
  import WebFolderPicker from './WebFolderPicker.svelte';
  import {
    cycleWebThemePreference,
    subscribeWebTheme,
    type WebThemeMode,
    type WebThemePreference,
  } from './theme';
  import {
    AGENT_CONNECTION_EVENT,
    getAgentFilePreview,
    getWorkspaceSessions,
    listAgentWorkspaces,
    registerAgentWorkspace,
    removeAgentWorkspace,
    resolveAgentBaseUrl,
    type AgentConnectionEventDetail,
    type AgentWorkspaceSummary,
  } from './agent-api';
  import {
    clearStoredBrowserWorkspaceBinding,
    persistStoredBrowserWorkspaceBinding,
    readStoredBrowserWorkspaceBinding,
  } from '../shared/bridges/browser-workspace-binding';
  import {
    isKnownBinaryFile,
    isWordFile,
  } from '../lib/file-preview-utils';

  let loading = $state(true);
  let loadError = $state('');
  let agentBaseUrl = $state('');
  let workspaces = $state<AgentWorkspaceSummary[]>([]);
  let selectedWorkspaceId = $state('');
  let currentSessionId = $state<string | null>(null);
  let pendingSessionSwitchId = $state<string | null>(null);
  let sessionsByWorkspace = $state<Record<string, Session[]>>({});
  let loadingWorkspaceIds = $state<Record<string, boolean>>({});
  let expandedWorkspaceIds = $state<Record<string, boolean>>({});
  let isMobileViewport = $state(false);
  let viewportWidth = $state(typeof window !== 'undefined' ? window.innerWidth : 1440);
  let sidebarOpen = $state(false);
  let workspaceActionPending = $state(false);
  let showAddWorkspaceDialog = $state(false);
  let showRemoveWorkspaceDialog = $state(false);
  let pendingRemoveWorkspace = $state<AgentWorkspaceSummary | null>(null);
  let workspaceDialogError = $state('');
  let showDeleteSessionDialog = $state(false);
  let pendingDeleteSession = $state<{ workspace: AgentWorkspaceSummary; session: Session } | null>(null);
  let webThemePreference = $state<WebThemePreference>('system');
  let webThemeMode = $state<WebThemeMode>('dark');
  let sidebarMode = $state<'projects' | 'files'>('projects');
  let previewFilePath = $state<string | null>(null);
  let previewContent = $state<string | null>(null);
  let previewContentKind = $state<EditContentKind>('text');
  let previewFileSize = $state<number | undefined>(undefined);
  let previewMime = $state<string | undefined>(undefined);
  let previewSymlinkTarget = $state<string | undefined>(undefined);
  let previewHeadSummary = $state<string | undefined>(undefined);
  let previewTailSummary = $state<string | undefined>(undefined);
  let previewLoading = $state(false);
  let previewError = $state('');
  let sidebarWidth = $state<number | null>(null);
  let isSidebarResizing = $state(false);
  let sidebarCollapsed = $state(false);
  let previewPanelWidth = $state<number | null>(null);
  let isPreviewPanelResizing = $state(false);
  let pendingSessionSwitchTimer: ReturnType<typeof setTimeout> | null = null;

  const INTERNAL_SESSION_NAME_PATTERNS = [
    /^auto-deep-followup-\d+$/i,
    /^auto-governance-resume-\d+$/i,
    /^real-dispatch-regression-\d+$/i,
  ];
  const SIDEBAR_WIDTH_STORAGE_KEY = 'magi-sidebar-width';
  const SIDEBAR_COLLAPSED_STORAGE_KEY = 'magi-sidebar-collapsed';
  const PREVIEW_PANEL_WIDTH_STORAGE_KEY = 'magi-preview-panel-width';
  const DEFAULT_SIDEBAR_WIDTH = 280;
  const COMPACT_SIDEBAR_WIDTH = 240;
  const MIN_SIDEBAR_WIDTH = 220;
  const MAX_SIDEBAR_WIDTH = 520;
  const MIN_PREVIEW_PANEL_WIDTH = 320;
  const DEFAULT_PREVIEW_PANEL_WIDTH = 480;
  const MAX_PREVIEW_PANEL_WIDTH = 900;
  const SHELL_PADDING = 8;
  const MIN_CONTENT_WIDTH = 620;
  const VIEWPORT_MOBILE_BREAKPOINT = 900;
  const VIEWPORT_PREVIEW_OVERLAY_BREAKPOINT = 1340;

  const selectedWorkspace = $derived(
    workspaces.find((workspace) => workspace.workspaceId === selectedWorkspaceId) ?? null
  );

  const currentSession = $derived(
    selectedWorkspaceId
      ? (sessionsByWorkspace[selectedWorkspaceId] ?? []).find((session) => session.id === currentSessionId) ?? null
      : null
  );

  const shellLayoutStyle = $derived([
    sidebarWidth ? `--sidebar-width: ${sidebarWidth}px` : '',
    previewPanelWidth ? `--preview-panel-width: ${previewPanelWidth}px` : '',
  ].filter(Boolean).join('; '));

  const sidebarIsDrawer = $derived(isMobileViewport);
  const sidebarHidden = $derived(!sidebarIsDrawer && sidebarCollapsed);
  const previewIsOverlay = $derived(
    !!previewFilePath && viewportWidth > 0 && viewportWidth <= VIEWPORT_PREVIEW_OVERLAY_BREAKPOINT
  );

  $effect(() => {
    const authoritativeWorkspaceId = typeof messagesState.currentWorkspaceId === 'string'
      ? messagesState.currentWorkspaceId.trim()
      : '';
    if (!authoritativeWorkspaceId) {
      return;
    }

    const bootstrapSessionId = typeof messagesState.currentSessionId === 'string'
      ? messagesState.currentSessionId.trim()
      : '';
    if (!bootstrapSessionId) {
      return;
    }

    const currentSessions = Array.isArray(messagesState.sessions) ? messagesState.sessions : [];

    const existingSessions = sessionsByWorkspace[authoritativeWorkspaceId] ?? [];
    const sessionsChanged = existingSessions.length !== currentSessions.length
      || existingSessions.some((session, index) => {
        const next = currentSessions[index];
        return !next
          || session.id !== next.id
          || session.name !== next.name
          || session.updatedAt !== next.updatedAt
          || session.messageCount !== next.messageCount;
      });

    if (sessionsChanged) {
      sessionsByWorkspace = {
        ...sessionsByWorkspace,
        [authoritativeWorkspaceId]: currentSessions,
      };
    }

    if (selectedWorkspaceId !== authoritativeWorkspaceId) {
      return;
    }

    if (bootstrapSessionId === currentSessionId) {
      return;
    }
    const belongsToSelectedWorkspace = (sessionsByWorkspace[selectedWorkspaceId] ?? [])
      .some((session) => session.id === bootstrapSessionId);
    if (!belongsToSelectedWorkspace) {
      return;
    }
    if (urlExplicitlyClearsWorkspaceSession(selectedWorkspaceId, selectedWorkspace?.rootPath || '')) {
      return;
    }

    currentSessionId = bootstrapSessionId;
    if (pendingSessionSwitchId === bootstrapSessionId) {
      if (pendingSessionSwitchTimer) {
        clearTimeout(pendingSessionSwitchTimer);
        pendingSessionSwitchTimer = null;
      }
      pendingSessionSwitchId = null;
    }
    const workspace = workspaces.find((item) => item.workspaceId === selectedWorkspaceId) ?? null;
    if (workspace) {
      syncBrowserSessionBinding(workspace.workspaceId, workspace.rootPath, bootstrapSessionId);
    }
  });

  $effect(() => {
    if (loading) {
      return;
    }
    const workspaceId = selectedWorkspaceId.trim();
    const workspacePath = selectedWorkspace?.rootPath?.trim() || '';
    const sessionId = typeof currentSessionId === 'string' ? currentSessionId.trim() : '';
    syncBrowserSessionBinding(workspaceId, workspacePath, sessionId || null);
  });

  function getWorkspaceSessionList(workspaceId: string): Session[] {
    return (sessionsByWorkspace[workspaceId] ?? []).filter((session) => !isInternalSession(session));
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


  function getThemePreferenceLabel(preference: WebThemePreference): string {
    switch (preference) {
      case 'light':
        return i18n.t('web.themeLight');
      case 'dark':
        return i18n.t('web.themeDark');
      default:
        return i18n.t('web.themeSystem');
    }
  }

  function getThemeIconName(preference: WebThemePreference): IconName {
    switch (preference) {
      case 'light':
        return 'sun';
      case 'dark':
        return 'moon';
      default:
        return 'monitor';
    }
  }

  function getNextThemePreference(preference: WebThemePreference): WebThemePreference {
    switch (preference) {
      case 'system':
        return 'light';
      case 'light':
        return 'dark';
      default:
        return 'system';
    }
  }

  const themeIconName = $derived.by(() => getThemeIconName(webThemePreference));
  const themeToggleTitle = $derived.by(() => {
    const currentLabel = getThemePreferenceLabel(webThemePreference);
    const nextLabel = getThemePreferenceLabel(getNextThemePreference(webThemePreference));
    return i18n.t('web.themeToggleTitle', { current: currentLabel, next: nextLabel });
  });

  function toggleWebTheme(): void {
    cycleWebThemePreference();
  }

  function syncBrowserSessionBinding(workspaceId: string, workspacePath: string, sessionId: string | null): void {
    if (typeof window === 'undefined') {
      return;
    }

    const normalizedWorkspaceId = workspaceId.trim();
    const normalizedWorkspacePath = workspacePath.trim();
    const normalizedSessionId = typeof sessionId === 'string' ? sessionId.trim() : '';
    const currentUrl = new URL(window.location.href);
    const nextUrl = new URL(window.location.href);

    if (!normalizedWorkspaceId || !normalizedWorkspacePath) {
      clearStoredBrowserWorkspaceBinding();
      nextUrl.searchParams.delete('workspaceId');
      nextUrl.searchParams.delete('workspacePath');
      nextUrl.searchParams.delete('sessionId');
      if (nextUrl.toString() !== currentUrl.toString()) {
        window.history.replaceState(window.history.state, '', nextUrl);
      }
      return;
    }

    persistStoredBrowserWorkspaceBinding({
      workspaceId: normalizedWorkspaceId,
      workspacePath: normalizedWorkspacePath,
    });
    nextUrl.searchParams.set('workspaceId', normalizedWorkspaceId);
    nextUrl.searchParams.set('workspacePath', normalizedWorkspacePath);

    if (normalizedSessionId) {
      nextUrl.searchParams.set('sessionId', normalizedSessionId);
    } else {
      nextUrl.searchParams.delete('sessionId');
    }

    if (nextUrl.toString() !== currentUrl.toString()) {
      window.history.replaceState(window.history.state, '', nextUrl);
    }
  }

  function urlExplicitlyClearsWorkspaceSession(workspaceId: string, workspacePath: string): boolean {
    if (typeof window === 'undefined') {
      return false;
    }
    const currentUrl = new URL(window.location.href);
    const queryWorkspaceId = currentUrl.searchParams.get('workspaceId')?.trim() || '';
    const queryWorkspacePath = currentUrl.searchParams.get('workspacePath')?.trim() || '';
    const querySessionId = currentUrl.searchParams.get('sessionId')?.trim() || '';
    return !querySessionId && (queryWorkspaceId === workspaceId || queryWorkspacePath === workspacePath);
  }

  function resolveWorkspacePreferredSessionId(
    workspaceId: string,
    workspacePath: string,
    options: {
      preserveCurrentSession?: boolean;
    } = {},
  ): string {
    if (typeof window === 'undefined') {
      return '';
    }

    const currentUrl = new URL(window.location.href);
    const queryWorkspaceId = currentUrl.searchParams.get('workspaceId')?.trim() || '';
    const queryWorkspacePath = currentUrl.searchParams.get('workspacePath')?.trim() || '';
    const querySessionId = currentUrl.searchParams.get('sessionId')?.trim() || '';
    if (querySessionId && (queryWorkspaceId === workspaceId || queryWorkspacePath === workspacePath)) {
      return querySessionId;
    }
    if (!querySessionId && (queryWorkspaceId === workspaceId || queryWorkspacePath === workspacePath)) {
      return '';
    }

    if (
      options.preserveCurrentSession !== false
      && selectedWorkspaceId === workspaceId
      && typeof currentSessionId === 'string'
      && currentSessionId.trim()
    ) {
      return currentSessionId.trim();
    }

    return '';
  }

  function requestWorkspaceBindingSync(workspace: AgentWorkspaceSummary, sessionId: string | null): void {
    getClientBridge().postMessage({
      type: 'workspaceBindingChanged',
      workspaceId: workspace.workspaceId,
      workspacePath: workspace.rootPath,
      sessionId: sessionId || '',
    });
  }

  function requestCurrentSessionState(): void {
    if (!currentSessionId) {
      return;
    }
    getClientBridge().postMessage({ type: 'requestState' });
  }

  function applyWorkspaceSessionsSnapshot(
    workspaceId: string,
    snapshot: Awaited<ReturnType<typeof getWorkspaceSessions>>,
    preferredSessionId = '',
  ): string {
    sessionsByWorkspace = {
      ...sessionsByWorkspace,
      [workspaceId]: snapshot.sessions,
    };

    const isStillSelectedWorkspace = selectedWorkspaceId === workspaceId;
    if (!isStillSelectedWorkspace) {
      return '';
    }

    const requestedSessionId = preferredSessionId.trim();
    const currentAnchoredSessionId = typeof currentSessionId === 'string' ? currentSessionId.trim() : '';
    const candidateSessionIds = [requestedSessionId, currentAnchoredSessionId].filter((value, index, arr) => (
      value.length > 0 && arr.indexOf(value) === index
    ));
    const preservedSessionId = candidateSessionIds.find((sessionId) => (
      snapshot.sessions.some((session) => session.id === sessionId)
    )) || '';
    const resolvedSessionId = preservedSessionId;

    currentSessionId = resolvedSessionId || null;
    setCurrentSessionId(resolvedSessionId || null);
    syncBrowserSessionBinding(snapshot.workspace.workspaceId, snapshot.workspace.rootPath, resolvedSessionId || null);
    requestWorkspaceBindingSync(snapshot.workspace, resolvedSessionId || null);
    return resolvedSessionId;
  }

  function notifyWorkbenchError(actionLabel: string, error: unknown): void {
    const detail = error instanceof Error ? error.message : String(error);
    addToast('error', detail ? `${actionLabel}失败：${detail}` : `${actionLabel}失败`, undefined, {
      category: 'incident',
      source: 'web-workbench',
      actionRequired: true,
      persistToCenter: true,
      countUnread: true,
      displayMode: 'toast',
    });
  }

  function clampSidebarWidth(width: number): number {
    return Math.max(MIN_SIDEBAR_WIDTH, Math.min(MAX_SIDEBAR_WIDTH, Math.round(width)));
  }

  function clampPreviewPanelWidth(width: number): number {
    if (typeof window === 'undefined') {
      return Math.max(MIN_PREVIEW_PANEL_WIDTH, Math.min(MAX_PREVIEW_PANEL_WIDTH, Math.round(width)));
    }
    const vw = viewportWidth || window.innerWidth;
    const sidebarTakenWidth = sidebarIsDrawer ? 0 : (sidebarWidth ?? DEFAULT_SIDEBAR_WIDTH) + SHELL_PADDING;
    const availableWidth = Math.max(
      MIN_PREVIEW_PANEL_WIDTH,
      vw - sidebarTakenWidth - MIN_CONTENT_WIDTH - SHELL_PADDING * 2 - SHELL_PADDING,
    );
    return Math.max(
      MIN_PREVIEW_PANEL_WIDTH,
      Math.min(Math.min(MAX_PREVIEW_PANEL_WIDTH, availableWidth), Math.round(width)),
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
    if (typeof window === 'undefined') {
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
    if (typeof window === 'undefined') {
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

  function toggleSidebarCollapsed(): void {
    sidebarCollapsed = !sidebarCollapsed;
    persistSidebarCollapsed(sidebarCollapsed);
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
      sidebarWidth = clampSidebarWidth(moveEvent.clientX - SHELL_PADDING);
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
      previewPanelWidth = clampPreviewPanelWidth(window.innerWidth - moveEvent.clientX - SHELL_PADDING);
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

  function closeFilePreview(): void {
    previewFilePath = null;
    previewContent = null;
    previewContentKind = 'text';
    previewFileSize = undefined;
    previewMime = undefined;
    previewSymlinkTarget = undefined;
    previewHeadSummary = undefined;
    previewTailSummary = undefined;
    previewLoading = false;
    previewError = '';
  }

  function resolvePreviewFilePath(filePath: string): string {
    const trimmedPath = filePath.trim();
    if (!trimmedPath) {
      return '';
    }
    if (/^(?:[a-zA-Z]:[\\/]|\/|\\\\)/.test(trimmedPath)) {
      return trimmedPath;
    }
    const workspaceRoot = selectedWorkspace?.rootPath?.trim() || '';
    return workspaceRoot ? `${workspaceRoot.replace(/[\\/]+$/, '')}/${trimmedPath.replace(/^[\\/]+/, '')}` : trimmedPath;
  }

  async function handleFileSelect(
    filePath: string,
    metadata: {
      contentKind?: EditContentKind;
      size?: number;
      mime?: string;
      symlinkTarget?: string;
      headSummary?: string;
      tailSummary?: string;
    } = {},
  ): Promise<void> {
    const resolvedFilePath = resolvePreviewFilePath(filePath);
    if (!resolvedFilePath) {
      return;
    }
    previewFilePath = resolvedFilePath;
    previewContent = null;
    previewContentKind = metadata.contentKind ?? 'text';
    previewFileSize = metadata.size;
    previewMime = metadata.mime;
    previewSymlinkTarget = metadata.symlinkTarget;
    previewHeadSummary = metadata.headSummary;
    previewTailSummary = metadata.tailSummary;
    previewError = '';

    if (
      isWordFile(resolvedFilePath)
      || isKnownBinaryFile(resolvedFilePath)
      || previewContentKind === 'binary'
      || previewContentKind === 'large_text'
      || previewContentKind === 'symlink'
      || previewContentKind === 'special'
    ) {
      previewLoading = false;
      return;
    }

    previewLoading = true;
    try {
      const payload = await getAgentFilePreview(resolvedFilePath, { includeSession: false });
      previewContent = payload.content || '';
    } catch (error) {
      previewError = error instanceof Error ? error.message : String(error);
    } finally {
      previewLoading = false;
    }
  }

  async function refreshWorkspaceSessions(workspaceId: string, preferredSessionId = ''): Promise<string> {
    if (!workspaceId) {
      currentSessionId = null;
      setCurrentSessionId(null);
      return '';
    }
    loadingWorkspaceIds = { ...loadingWorkspaceIds, [workspaceId]: true };
    try {
      const snapshot = await getWorkspaceSessions(workspaceId, preferredSessionId);
      return applyWorkspaceSessionsSnapshot(workspaceId, snapshot, preferredSessionId);
    } catch (error) {
      notifyWorkbenchError('加载工作区会话', error);
      return '';
    } finally {
      loadingWorkspaceIds = { ...loadingWorkspaceIds, [workspaceId]: false };
    }
  }

  async function refreshWorkspaces(): Promise<void> {
    loading = true;
    loadError = '';
    agentBaseUrl = resolveAgentBaseUrl();
    try {
      const next = await listAgentWorkspaces();
      workspaces = next;
      sessionsByWorkspace = {};
      loadingWorkspaceIds = {};
      expandedWorkspaceIds = {};
      const currentUrl = typeof window !== 'undefined' ? new URL(window.location.href) : null;
      const queryWorkspaceId = currentUrl?.searchParams.get('workspaceId')?.trim() || '';
      const queryWorkspacePath = currentUrl?.searchParams.get('workspacePath')?.trim() || '';
      const queryMatchedWorkspace = next.find((workspace) => {
        if (queryWorkspaceId && workspace.workspaceId === queryWorkspaceId) {
          return true;
        }
        if (queryWorkspacePath && workspace.rootPath === queryWorkspacePath) {
          return true;
        }
        return false;
      }) ?? null;
      const storedWorkspaceId = readStoredBrowserWorkspaceBinding().workspaceId;
      if (queryMatchedWorkspace) {
        selectedWorkspaceId = queryMatchedWorkspace.workspaceId;
      } else if (storedWorkspaceId && next.some((workspace) => workspace.workspaceId === storedWorkspaceId)) {
        selectedWorkspaceId = storedWorkspaceId;
      } else {
        selectedWorkspaceId = next[0]?.workspaceId || '';
      }
      if (selectedWorkspaceId) {
        expandedWorkspaceIds = { [selectedWorkspaceId]: true };
      }
      const selectedWorkspacePath = next.find((workspace) => workspace.workspaceId === selectedWorkspaceId)?.rootPath || '';
      await refreshWorkspaceSessions(
        selectedWorkspaceId,
        resolveWorkspacePreferredSessionId(selectedWorkspaceId, selectedWorkspacePath),
      );
      if (selectedWorkspaceId) {
        requestCurrentSessionState();
      }
    } catch (error) {
      loadError = error instanceof Error ? error.message : String(error);
      notifyWorkbenchError('加载工作区列表', error);
    } finally {
      loading = false;
    }
  }

  async function handleFolderSelected(rootPath: string, _name: string): Promise<void> {
    if (workspaceActionPending) {
      return;
    }
    workspaceDialogError = '';
    const normalizedRootPath = rootPath.trim();
    if (!normalizedRootPath) {
      return;
    }
    closeAddWorkspaceDialog({ force: true });
    workspaceActionPending = true;
    try {
      const next = await runActionWithFeedback(
        () => registerAgentWorkspace(normalizedRootPath),
        {
          actionLabel: '添加工作区',
          successMessage: '工作区已添加',
        },
      );
      if (!next) {
        return;
      }
      workspaces = next;
      const addedWorkspace = next.find((workspace) => workspace.rootPath === normalizedRootPath);
      selectedWorkspaceId = addedWorkspace?.workspaceId || next[0]?.workspaceId || '';
      if (selectedWorkspaceId) {
        expandedWorkspaceIds = {
          ...expandedWorkspaceIds,
          [selectedWorkspaceId]: true,
        };
        await refreshWorkspaceSessions(
          selectedWorkspaceId,
          resolveWorkspacePreferredSessionId(
            selectedWorkspaceId,
            addedWorkspace?.rootPath || '',
          ),
        );
        requestCurrentSessionState();
      }
    } finally {
      workspaceActionPending = false;
    }
  }

  function openAddWorkspaceDialog(): void {
    if (workspaceActionPending || loadError) {
      return;
    }
    workspaceDialogError = '';
    showAddWorkspaceDialog = true;
  }

  function closeAddWorkspaceDialog(options: { force?: boolean } = {}): void {
    if (workspaceActionPending && options.force !== true) {
      return;
    }
    workspaceDialogError = '';
    
    showAddWorkspaceDialog = false;
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
          actionLabel: '移除工作区',
          successMessage: `工作区“${removedName}”已移除`,
        },
      );
      if (!next) {
        return;
      }
      workspaces = next;
      sessionsByWorkspace = Object.fromEntries(
        Object.entries(sessionsByWorkspace).filter(([workspaceId]) => workspaceId !== removedId)
      );
      loadingWorkspaceIds = Object.fromEntries(
        Object.entries(loadingWorkspaceIds).filter(([workspaceId]) => workspaceId !== removedId)
      );
      expandedWorkspaceIds = Object.fromEntries(
        Object.entries(expandedWorkspaceIds).filter(([workspaceId]) => workspaceId !== removedId)
      );

      if (selectedWorkspaceId === removedId) {
        selectedWorkspaceId = next[0]?.workspaceId || '';
        currentSessionId = null;
        if (selectedWorkspaceId) {
          expandedWorkspaceIds = {
            ...expandedWorkspaceIds,
            [selectedWorkspaceId]: true,
          };
          const nextWorkspacePath = next.find((workspace) => workspace.workspaceId === selectedWorkspaceId)?.rootPath || '';
          await refreshWorkspaceSessions(
            selectedWorkspaceId,
            resolveWorkspacePreferredSessionId(selectedWorkspaceId, nextWorkspacePath),
          );
          requestCurrentSessionState();
        }
      }
    } finally {
      workspaceActionPending = false;
    }
  }

  function toggleWorkspaceExpansion(workspace: AgentWorkspaceSummary): void {
    const isExpanded = !!expandedWorkspaceIds[workspace.workspaceId];
    expandedWorkspaceIds = {
      ...expandedWorkspaceIds,
      [workspace.workspaceId]: !isExpanded,
    };
    if (!isExpanded && getWorkspaceSessionList(workspace.workspaceId).length === 0) {
      void (async () => {
        try {
          await refreshWorkspaceSessions(workspace.workspaceId, '');
        } catch (error) {
          loadError = error instanceof Error ? error.message : String(error);
        }
      })();
    }
  }

  function switchSession(workspace: AgentWorkspaceSummary, sessionId: string): void {
    if (!sessionId || sessionId === currentSessionId || pendingSessionSwitchId) {
      return;
    }
    const nextSession = (sessionsByWorkspace[workspace.workspaceId] ?? []).find((session) => session.id === sessionId);
    const nextSessionName = nextSession?.name || '未命名会话';
    const fallbackSessionId = typeof currentSessionId === 'string' ? currentSessionId : null;
    selectedWorkspaceId = workspace.workspaceId;
    currentSessionId = sessionId;
    pendingSessionSwitchId = sessionId;
    syncBrowserSessionBinding(workspace.workspaceId, workspace.rootPath, sessionId);
    if (pendingSessionSwitchTimer) {
      clearTimeout(pendingSessionSwitchTimer);
    }
    pendingSessionSwitchTimer = setTimeout(() => {
      if (pendingSessionSwitchId !== sessionId) {
        return;
      }
      pendingSessionSwitchId = null;
      pendingSessionSwitchTimer = null;
      const confirmedSessionId = typeof messagesState.currentSessionId === 'string'
        ? messagesState.currentSessionId.trim()
        : '';
      currentSessionId = confirmedSessionId || fallbackSessionId;
      syncBrowserSessionBinding(
        workspace.workspaceId,
        workspace.rootPath,
        currentSessionId,
      );
    }, 6000);
    addToast('info', `正在切换到会话“${nextSessionName}”...`, undefined, {
      category: 'feedback',
      source: 'session-management',
      persistToCenter: false,
      countUnread: false,
      displayMode: 'toast',
      duration: 1800,
    });
    getClientBridge().postMessage({
      type: 'switchSession',
      sessionId,
      workspaceId: workspace.workspaceId,
      workspacePath: workspace.rootPath,
    });
    if (sidebarIsDrawer) {
      sidebarOpen = false;
    }
  }

  function openDeleteSessionDialog(workspace: AgentWorkspaceSummary, session: Session): void {
    pendingDeleteSession = { workspace, session };
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
    addToast('info', `正在删除会话“${displayName}”...`, undefined, {
      category: 'feedback',
      source: 'session-management',
      persistToCenter: false,
      countUnread: false,
      displayMode: 'toast',
      duration: 1800,
    });
    getClientBridge().postMessage({
      type: 'deleteSession',
      sessionId: session.id,
      workspaceId: workspace.workspaceId,
      workspacePath: workspace.rootPath,
      requireConfirm: false,
    });
    closeDeleteSessionDialog();
  }

  function applyViewportMode(): void {
    if (typeof window === 'undefined') {
      return;
    }
    viewportWidth = window.innerWidth;
    isMobileViewport = window.innerWidth <= VIEWPORT_MOBILE_BREAKPOINT;
  }

  function toggleSidebar(): void {
    sidebarOpen = !sidebarOpen;
  }

  // 顶部 Header 的 sidebar 切换按钮：drawer 模式下控制抽屉开合，桌面模式下控制折叠/展开。
  function toggleSidebarFromHeader(): void {
    if (sidebarIsDrawer) {
      toggleSidebar();
    } else {
      toggleSidebarCollapsed();
    }
  }

  setWebSidebarContext({
    get collapsed() { return sidebarCollapsed; },
    get hidden() { return sidebarHidden; },
    get isDrawer() { return sidebarIsDrawer; },
    get drawerOpen() { return sidebarOpen; },
    toggle: toggleSidebarFromHeader,
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

  onMount(() => {
    applyViewportMode();
    loadStoredSidebarWidth();
    loadStoredSidebarCollapsed();
    loadStoredPreviewPanelWidth();
    // 节流 resize：手机虚拟键盘弹出/收起会短时间内触发大量 resize 事件
    let resizeRaf: number | null = null;
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
        contentKind?: EditContentKind;
        size?: number;
        mime?: string;
        symlinkTarget?: string;
        headSummary?: string;
        tailSummary?: string;
      }>).detail;
      const filepath = detail?.filepath;
      if (typeof filepath === 'string') {
        event.preventDefault();
        void handleFileSelect(filepath, {
          contentKind: detail?.contentKind,
          size: detail?.size,
          mime: detail?.mime,
          symlinkTarget: detail?.symlinkTarget,
          headSummary: detail?.headSummary,
          tailSummary: detail?.tailSummary,
        });
      }
    };
    const handleAgentConnection = (event: Event) => {
      const detail = (event as CustomEvent<AgentConnectionEventDetail>).detail;
      const previousAgentBaseUrl = agentBaseUrl;
      agentBaseUrl = resolveAgentBaseUrl();
      if (detail?.status === 'recovering') {
        if (!workspaces.length && !loading) {
          loadError = detail.error || i18n.t('web.agentRecovering');
        }
        return;
      }
      const shouldRefreshWorkspaces = !loading && (
        Boolean(loadError)
        || workspaces.length === 0
        || Boolean(detail?.recovered && previousAgentBaseUrl !== agentBaseUrl)
      );
      if (shouldRefreshWorkspaces) {
        void refreshWorkspaces();
      }
    };
    window.addEventListener('resize', handleResize);
    window.addEventListener('magi:previewFile', handlePreviewFile as EventListener);
    window.addEventListener(AGENT_CONNECTION_EVENT, handleAgentConnection as EventListener);
    void refreshWorkspaces();
    return () => {
      window.removeEventListener('resize', handleResize);
      window.removeEventListener('magi:previewFile', handlePreviewFile as EventListener);
      window.removeEventListener(AGENT_CONNECTION_EVENT, handleAgentConnection as EventListener);
      if (resizeRaf !== null) {
        cancelAnimationFrame(resizeRaf);
      }
    };
  });

  onMount(() => {
    return subscribeWebTheme((snapshot) => {
      webThemePreference = snapshot.preference;
      webThemeMode = snapshot.mode;
    });
  });
</script>

<div
  class="web-workbench-shell"
  class:web-workbench-shell--sidebar-drawer={sidebarIsDrawer}
  class:web-workbench-shell--sidebar-open={sidebarIsDrawer && sidebarOpen}
  class:web-workbench-shell--sidebar-hidden={sidebarHidden}
  class:web-workbench-shell--preview-overlay={previewIsOverlay}
  class:web-workbench-shell--has-preview={!!previewFilePath}
  class:web-workbench-shell--resizing={isSidebarResizing || isPreviewPanelResizing}
  class:web-workbench-shell--sidebar-resizing={isSidebarResizing}
  class:web-workbench-shell--preview-resizing={isPreviewPanelResizing}
  style={shellLayoutStyle}
>
  {#if sidebarIsDrawer && sidebarOpen}
    <button
      type="button"
      class="drawer-overlay"
      aria-label="关闭工作区导航"
      onclick={() => {
        sidebarOpen = false;
      }}
    ></button>
  {/if}

  {#if !sidebarHidden}
  <aside class="sidebar" class:sidebar--open={sidebarIsDrawer && sidebarOpen}>
    <div class="sidebar-header">
      <div class="sidebar-brand">
        <div class="sidebar-title">Magi</div>
        <div class="sidebar-header-tools">
          <button
            class="theme-toggle-btn"
            type="button"
            data-tooltip={themeToggleTitle}
            aria-label={themeToggleTitle}
            data-theme-preference={webThemePreference}
            data-theme-mode={webThemeMode}
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
        </div>
      </div>
    </div>

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
                    class:active={workspace.workspaceId === selectedWorkspaceId}
                    aria-expanded={!!expandedWorkspaceIds[workspace.workspaceId]}
                    data-workspace-id={workspace.workspaceId}
                    title={workspace.rootPath}
                    onclick={() => toggleWorkspaceExpansion(workspace)}
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
                    class="workspace-remove-btn"
                    title="从 Magi 中移除工作区"
                    aria-label={`移除工作区 ${workspace.name}`}
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
                      <div class="sidebar-empty sidebar-empty--nested">当前工作区暂无会话</div>
                    {:else}
                      <div class="session-list session-list--nested">
                        {#each getWorkspaceSessionList(workspace.workspaceId) as session (session.id)}
                          <div class="session-row" class:active={session.id === currentSessionId && workspace.workspaceId === selectedWorkspaceId}>
                            <button
                              type="button"
                              class="session-item"
                              class:active={session.id === currentSessionId && workspace.workspaceId === selectedWorkspaceId}
                              class:pending={session.id === pendingSessionSwitchId}
                              data-session-id={session.id}
                              disabled={pendingSessionSwitchId !== null}
                              title={session.name || i18n.t('header.unnamedSession')}
                              onclick={() => switchSession(workspace, session.id)}
                            >
                              <span class="session-name">{session.name || i18n.t('header.unnamedSession')}</span>
                              <span class="session-meta">
                                <span class="session-msg-count" title={i18n.t('header.messageCount', { count: session.messageCount ?? 0 })}>{session.messageCount ?? 0}</span>
                                <span class="session-time">{formatRelativeTime(session.updatedAt || session.createdAt)}</span>
                              </span>
                            </button>
                            <button
                              type="button"
                              class="session-delete-btn"
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
        <ProjectFileTree
          rootPath={selectedWorkspace?.rootPath || ''}
          workspaceId={selectedWorkspaceId}
          title={selectedWorkspace?.name || i18n.t('web.projectFiles')}
          titlePath={selectedWorkspace?.rootPath || ''}
          selectedFilePath={previewFilePath}
          onFileSelect={(path) => void handleFileSelect(path)}
        />
      </section>
    {/if}

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
    {#if sidebarIsDrawer}
      <div class="mobile-toolbar">
        <button type="button" class="mobile-toolbar-btn" onclick={toggleSidebar}>
          {sidebarOpen ? i18n.t('web.closeNav') : i18n.t('web.workspaceOrSession')}
        </button>
        <div class="mobile-toolbar-meta">
          <div class="mobile-toolbar-title">{selectedWorkspace?.name || i18n.t('web.unselectedWorkspace')}</div>
          <div class="mobile-toolbar-subtitle">{currentSession?.name || i18n.t('header.unnamedSession')}</div>
        </div>
        <div class="mobile-toolbar-actions">
          <button
            type="button"
            class="theme-toggle-btn theme-toggle-btn--mobile"
            title={themeToggleTitle}
            aria-label={themeToggleTitle}
            data-theme-preference={webThemePreference}
            data-theme-mode={webThemeMode}
            onclick={toggleWebTheme}
          >
            <Icon name={themeIconName} size={16} />
          </button>
        </div>
      </div>
    {/if}
    <div
      class="workbench-body"
      class:workbench-body--with-preview={!!previewFilePath && !previewIsOverlay}
      class:workbench-body--overlay-preview={!!previewFilePath && previewIsOverlay}
    >
      <div class="workbench-app-pane">
        <App />
      </div>
      {#if previewFilePath}
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
        <FilePreviewPanel
          filePath={previewFilePath}
          workspaceRoot={selectedWorkspace?.rootPath || ''}
          content={previewContent}
          loading={previewLoading}
          error={previewError}
          contentKind={previewContentKind}
          size={previewFileSize}
          mime={previewMime}
          symlinkTarget={previewSymlinkTarget}
          headSummary={previewHeadSummary}
          tailSummary={previewTailSummary}
          onClose={closeFilePreview}
        />
      {/if}
    </div>
  </main>
</div>

{#if showAddWorkspaceDialog}
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
    <WebFolderPicker
      title={i18n.t('web.selectWorkspaceFolder')}
      onSelect={(path, name) => void handleFolderSelected(path, name)}
      onCancel={closeAddWorkspaceDialog}
      disabled={workspaceActionPending}
    />
  </Modal>
{/if}

{#if showRemoveWorkspaceDialog && pendingRemoveWorkspace}
  <Modal
    title="从 Magi 中移除工作区"
    onClose={closeRemoveWorkspaceDialog}
    closeOnBackdrop={true}
    size="sm"
  >
    <p class="workspace-dialog-text">将从 Magi 的工作区注册表中移除 <strong>{pendingRemoveWorkspace.name}</strong>。</p>
    <p class="workspace-dialog-text workspace-dialog-text--muted">不会删除本地项目目录，也不会删除该工作区下已有的历史会话。</p>
    {#if workspaceDialogError}
      <div class="workspace-dialog-error">{workspaceDialogError}</div>
    {/if}

    {#snippet footer()}
      <button class="modal-btn secondary" type="button" onclick={() => closeRemoveWorkspaceDialog()} disabled={workspaceActionPending}>取消</button>
      <button class="modal-btn danger" type="button" onclick={() => void removeWorkspace()} disabled={workspaceActionPending}>
        {workspaceActionPending ? '正在移除...' : '确认移除'}
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
      <button class="modal-btn secondary" type="button" onclick={closeDeleteSessionDialog}>{i18n.t('header.cancel')}</button>
      <button class="modal-btn danger" type="button" onclick={confirmDeleteSession}>{i18n.t('header.confirmDelete')}</button>
    {/snippet}
  </Modal>
{/if}

<style>
  .web-workbench-shell {
    display: grid;
    grid-template-columns: var(--sidebar-width, 280px) minmax(0, 1fr);
    gap: 8px;
    height: 100vh;
    width: 100vw;
    padding: 8px;
    box-sizing: border-box;
    background: var(--background);
    color: var(--foreground);
    isolation: isolate;
    overflow: hidden;
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
    background: var(--surface-1);
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

  .sidebar-brand {
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

  .sidebar-title {
    font-size: var(--text-lg);
    font-weight: var(--font-bold);
    line-height: 1.15;
    letter-spacing: -0.02em;
  }

  .session-meta,
  .sidebar-empty {
    color: var(--foreground-muted);
    font-size: var(--text-sm);
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

  .theme-toggle-btn[data-theme-preference='light'],
  .theme-toggle-btn[data-theme-preference='dark'] {
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

  .sidebar-section--workspaces {
    flex: 1;
    min-height: 0;
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

  .session-list {
    display: flex;
    flex-direction: column;
    gap: 1px;
  }

  .workspace-tree {
    min-height: 0;
    flex: 1;
    overflow-y: auto;
    overflow-x: hidden;
    padding-right: var(--space-1);
    overscroll-behavior: contain;
    -webkit-overflow-scrolling: touch;
    scrollbar-gutter: stable;
    scrollbar-width: thin;
    scrollbar-color: var(--scrollbar-thumb) transparent;
  }

  .workspace-tree::-webkit-scrollbar,
  .sidebar-section--file-tree-mode :global(.file-tree-list::-webkit-scrollbar) {
    width: 10px;
  }

  .workspace-tree::-webkit-scrollbar-track,
  .sidebar-section--file-tree-mode :global(.file-tree-list::-webkit-scrollbar-track) {
    background: color-mix(in srgb, var(--surface-2) 58%, transparent);
    border-radius: 999px;
  }

  .workspace-tree::-webkit-scrollbar-thumb,
  .sidebar-section--file-tree-mode :global(.file-tree-list::-webkit-scrollbar-thumb) {
    background: var(--scrollbar-thumb);
    border-radius: 999px;
    border: 2px solid color-mix(in srgb, var(--surface-1) 88%, transparent);
    background-clip: content-box;
  }

  .workspace-tree::-webkit-scrollbar-thumb:hover,
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

  .workspace-row:hover .workspace-remove-btn {
    opacity: 1;
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

  .workspace-remove-btn {
    width: 22px;
    height: 22px;
    margin-right: 4px;
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
    transition: opacity var(--transition-fast), background var(--transition-fast), color var(--transition-fast);
    flex-shrink: 0;
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

  .session-row:hover .session-delete-btn,
  .session-row:focus-within .session-delete-btn {
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
    gap: 6px;
    flex-shrink: 0;
    font-size: var(--text-xs);
    color: var(--foreground-muted);
    white-space: nowrap;
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

  .session-row:hover .session-time {
    opacity: 0;
    pointer-events: none;
  }

  .session-time {
    transition: opacity var(--transition-fast);
  }

  .session-delete-btn {
    position: absolute;
    top: 50%;
    right: 6px;
    transform: translateY(-50%);
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
    opacity: 0;
    pointer-events: none;
    transition: opacity var(--transition-fast), background var(--transition-fast), color var(--transition-fast);
    flex-shrink: 0;
  }

  .session-delete-btn:hover {
    color: var(--error);
    background: color-mix(in srgb, var(--error) 12%, transparent);
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
    grid-template-columns: minmax(620px, 1fr) 8px minmax(320px, var(--preview-panel-width, 400px));
  }

  .workbench-app-pane {
    /* 不要再创建独立 stacking context，否则内部的 .settings-overlay 等全局 modal
       会被困在 pane 子树（auto=0）内，被相邻的 file-preview-panel 等覆盖。
       外层 .web-workbench-shell 已用 isolation: isolate 做了一层隔离。 */
    min-width: 0;
    min-height: 0;
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

  .workbench-body :global(.file-preview-panel) {
    border-radius: var(--radius-lg);
    border: 1px solid var(--border);
    overflow: hidden;
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

  /* 预览覆盖模式：preview 浮在主区上方 */
  .web-workbench-shell--preview-overlay :global(.file-preview-panel) {
    position: absolute;
    inset: 0;
    z-index: var(--z-overlay-preview);
    border-radius: 0;
    border: none;
    border-left: 1px solid var(--border);
    background: var(--background);
  }

  .mobile-toolbar {
    display: none;
  }

  .mobile-toolbar-actions {
    margin-left: auto;
    display: flex;
    align-items: center;
    flex-shrink: 0;
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

    .workspace-remove-btn {
      width: 28px;
      height: 28px;
      opacity: 1;
    }

    .mobile-toolbar {
      display: flex;
      align-items: center;
      gap: var(--space-3);
      padding: var(--space-2) var(--space-4);
      border-bottom: 1px solid var(--border);
      background: var(--vscode-sideBar-secondaryBackground, var(--background));
      flex-shrink: 0;
      position: relative;
      z-index: 1;
    }

    .mobile-toolbar-btn {
      display: inline-flex;
      align-items: center;
      justify-content: center;
      min-width: 112px;
      height: var(--btn-height-md);
      padding: 0 var(--space-3);
      border-radius: var(--radius-md);
      border: 1px solid var(--border);
      background: color-mix(in srgb, var(--foreground) 3%, var(--vscode-sideBar-secondaryBackground, var(--background)));
      color: var(--foreground);
      font-size: var(--text-base);
      font-weight: var(--font-medium);
      flex-shrink: 0;
    }

    .theme-toggle-btn--mobile {
      width: 36px;
      height: 36px;
    }

    .mobile-toolbar-meta {
      min-width: 0;
      display: flex;
      flex-direction: column;
      gap: 2px;
    }

    .mobile-toolbar-title {
      font-size: var(--text-base);
      font-weight: var(--font-semibold);
      color: var(--foreground);
      white-space: nowrap;
      overflow: hidden;
      text-overflow: ellipsis;
    }

    .mobile-toolbar-subtitle {
      font-size: var(--text-sm);
      color: var(--foreground-muted);
      white-space: nowrap;
      overflow: hidden;
      text-overflow: ellipsis;
    }

    .workspace-tree {
      padding-right: 0;
      gap: var(--space-3);
    }

    .sidebar-brand {
      flex-wrap: wrap;
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

    .sidebar-title {
      font-size: var(--text-lg);
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
