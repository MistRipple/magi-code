<script lang="ts">
  import { onMount } from 'svelte';
  import App from '../App.svelte';
  import Icon from '../components/Icon.svelte';
  import Modal from '../components/Modal.svelte';
  import { runActionWithFeedback } from '../lib/action-feedback';
  import type { IconName } from '../lib/icons';
  import { addToast, messagesState, setCurrentSessionId } from '../stores/messages.svelte';
  import { getClientBridge } from '../shared/bridges/bridge-runtime';
  import { i18n } from '../stores/i18n.svelte';
  import type { Session } from '../types/message';
  import WebFolderPicker from './WebFolderPicker.svelte';
  import {
    cycleWebThemePreference,
    subscribeWebTheme,
    type WebThemeMode,
    type WebThemePreference,
  } from './theme';
  import {
    AGENT_CONNECTION_EVENT,
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
  let sidebarOpen = $state(false);
  let sidebarSearchQuery = $state('');
  let workspaceActionPending = $state(false);
  let showAddWorkspaceDialog = $state(false);
  let showRemoveWorkspaceDialog = $state(false);
  let pendingRemoveWorkspace = $state<AgentWorkspaceSummary | null>(null);
  let workspaceDialogError = $state('');
  let webThemePreference = $state<WebThemePreference>('system');
  let webThemeMode = $state<WebThemeMode>('dark');
  let pendingSessionSwitchTimer: ReturnType<typeof setTimeout> | null = null;

  const INTERNAL_SESSION_NAME_PATTERNS = [
    /^auto-deep-followup-\d+$/i,
    /^replan-gate-ask-\d+$/i,
    /^auto-repair-\d+$/i,
    /^auto-governance-resume-\d+$/i,
    /^followup-blocked-\d+$/i,
    /^real-dispatch-regression-\d+$/i,
  ];

  const selectedWorkspace = $derived(
    workspaces.find((workspace) => workspace.workspaceId === selectedWorkspaceId) ?? null
  );

  const currentSession = $derived(
    selectedWorkspaceId
      ? (sessionsByWorkspace[selectedWorkspaceId] ?? []).find((session) => session.id === currentSessionId) ?? null
      : null
  );

  const normalizedSidebarSearch = $derived(sidebarSearchQuery.trim().toLowerCase());

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

  function formatSessionMeta(session: Session): string {
    const timestamp = session.updatedAt || session.createdAt;
    const date = new Date(timestamp);
    const messageCount = session.messageCount ?? 0;
    const formattedDate = date.toLocaleDateString(i18n.locale, {
      month: 'short',
      day: 'numeric',
      hour: '2-digit',
      minute: '2-digit',
    });
    return `${messageCount} 条消息  ${formattedDate}`;
  }

  function getVisibleWorkspaceSessions(workspaceId: string): Session[] {
    const sessions = getWorkspaceSessionList(workspaceId);
    if (!normalizedSidebarSearch) {
      return sessions;
    }
    return sessions.filter((session) => {
      const name = (session.name || '').toLowerCase();
      const preview = (session.preview || '').toLowerCase();
      return name.includes(normalizedSidebarSearch) || preview.includes(normalizedSidebarSearch);
    });
  }

  function getVisibleWorkspaces(): AgentWorkspaceSummary[] {
    if (!normalizedSidebarSearch) {
      return workspaces;
    }
    return workspaces.filter((workspace) => {
      const workspaceMatch = workspace.name.toLowerCase().includes(normalizedSidebarSearch)
        || workspace.rootPath.toLowerCase().includes(normalizedSidebarSearch);
      if (workspaceMatch) {
        return true;
      }
      return getWorkspaceSessionList(workspace.workspaceId).some((session) => {
        const name = (session.name || '').toLowerCase();
        const preview = (session.preview || '').toLowerCase();
        return name.includes(normalizedSidebarSearch) || preview.includes(normalizedSidebarSearch);
      });
    });
  }

  // 缓存的可见工作区列表（避免模板中每次渲染都重新过滤）
  const visibleWorkspaces = $derived(getVisibleWorkspaces());

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

    const requestedSessionId = preferredSessionId.trim();
    const currentAnchoredSessionId = selectedWorkspaceId === workspaceId
      ? (typeof currentSessionId === 'string' ? currentSessionId.trim() : '')
      : '';
    const candidateSessionIds = [requestedSessionId, currentAnchoredSessionId].filter((value, index, arr) => (
      value.length > 0 && arr.indexOf(value) === index
    ));
    const preservedSessionId = candidateSessionIds.find((sessionId) => (
      snapshot.sessions.some((session) => session.id === sessionId)
    )) || '';
    const resolvedSessionId = preservedSessionId;

    currentSessionId = resolvedSessionId || null;
    if (selectedWorkspaceId === workspaceId) {
      setCurrentSessionId(resolvedSessionId || null);
    }
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

  function toggleWorkspace(workspace: AgentWorkspaceSummary): void {
    void (async () => {
      try {
        const wasSelected = workspace.workspaceId === selectedWorkspaceId;
        const isExpanded = !!expandedWorkspaceIds[workspace.workspaceId];
        selectedWorkspaceId = workspace.workspaceId;
        expandedWorkspaceIds = {
          ...expandedWorkspaceIds,
          [workspace.workspaceId]: wasSelected ? !isExpanded : true,
        };
        if (!wasSelected) {
          currentSessionId = null;
          setCurrentSessionId(null);
          syncBrowserSessionBinding(workspace.workspaceId, workspace.rootPath, null);
          requestWorkspaceBindingSync(workspace, null);
        }
        const shouldLoad = !wasSelected || !isExpanded || getWorkspaceSessionList(workspace.workspaceId).length === 0;
        if (shouldLoad) {
          const resolvedSessionId = await refreshWorkspaceSessions(
            workspace.workspaceId,
            resolveWorkspacePreferredSessionId(workspace.workspaceId, workspace.rootPath, {
              preserveCurrentSession: wasSelected,
            }),
          );
          if (resolvedSessionId) {
            requestCurrentSessionState();
          }
        } else {
          const anchoredSessionId = (sessionsByWorkspace[workspace.workspaceId] ?? [])
            .some((session) => session.id === currentSessionId)
            ? currentSessionId
            : null;
          currentSessionId = anchoredSessionId;
          setCurrentSessionId(anchoredSessionId);
          syncBrowserSessionBinding(workspace.workspaceId, workspace.rootPath, anchoredSessionId);
          requestWorkspaceBindingSync(workspace, anchoredSessionId);
        }
      } catch (error) {
        loadError = error instanceof Error ? error.message : String(error);
      }
    })();
  }

  function switchSession(sessionId: string): void {
    if (!sessionId || sessionId === currentSessionId || pendingSessionSwitchId) {
      return;
    }
    const workspace = workspaces.find((item) => item.workspaceId === selectedWorkspaceId) ?? null;
    if (!workspace) {
      return;
    }
    const nextSession = (sessionsByWorkspace[selectedWorkspaceId] ?? []).find((session) => session.id === sessionId);
    const nextSessionName = nextSession?.name || '未命名会话';
    const fallbackSessionId = typeof currentSessionId === 'string' ? currentSessionId : null;
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
    if (isMobileViewport) {
      sidebarOpen = false;
    }
  }

  function applyViewportMode(): void {
    if (typeof window === 'undefined') {
      return;
    }
    isMobileViewport = window.innerWidth <= 900;
    if (!isMobileViewport) {
      sidebarOpen = false;
    }
  }

  function toggleSidebar(): void {
    sidebarOpen = !sidebarOpen;
  }

  $effect(() => {
    if (typeof document === 'undefined') {
      return;
    }

    const shouldLockViewport = isMobileViewport && sidebarOpen;
    document.documentElement.classList.toggle('magi-web-drawer-open', shouldLockViewport);
    document.body.classList.toggle('magi-web-drawer-open', shouldLockViewport);

    return () => {
      document.documentElement.classList.remove('magi-web-drawer-open');
      document.body.classList.remove('magi-web-drawer-open');
    };
  });

  onMount(() => {
    applyViewportMode();
    // 节流 resize：手机虚拟键盘弹出/收起会短时间内触发大量 resize 事件
    let resizeRaf: number | null = null;
    const handleResize = () => {
      if (resizeRaf !== null) return;
      resizeRaf = requestAnimationFrame(() => {
        resizeRaf = null;
        applyViewportMode();
      });
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
    window.addEventListener(AGENT_CONNECTION_EVENT, handleAgentConnection as EventListener);
    void refreshWorkspaces();
    return () => {
      window.removeEventListener('resize', handleResize);
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
  class:web-workbench-shell--mobile-sidebar-open={isMobileViewport && sidebarOpen}
>
  {#if isMobileViewport && sidebarOpen}
    <button
      type="button"
      class="mobile-overlay"
      aria-label="关闭工作区导航"
      onclick={() => {
        sidebarOpen = false;
      }}
    ></button>
  {/if}

  <aside class="sidebar" class:sidebar--mobile-open={isMobileViewport && sidebarOpen}>
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

    <section class="sidebar-section sidebar-section--workspaces">
      <div class="section-title">{i18n.t('common.workspace')}</div>
      <input
        class="sidebar-search"
        type="search"
        bind:value={sidebarSearchQuery}
        placeholder={i18n.t('web.searchPlaceholder')}
        aria-label={i18n.t('web.searchPlaceholder')}
      />
      {#if loading}
        <div class="sidebar-empty">{i18n.t('common.loading')}</div>
      {:else if loadError}
        <div class="sidebar-error">
          <div class="sidebar-error-title">{i18n.t('web.workspaceUnavailable')}</div>
          <div>{loadError}</div>
        </div>
      {:else if workspaces.length === 0}
        <div class="sidebar-empty">{i18n.t('web.noWorkspaces')}</div>
      {:else if visibleWorkspaces.length === 0}
        <div class="sidebar-empty">{i18n.t('web.workspaceNotFound')}</div>
      {:else}
        <div class="workspace-tree">
          {#each visibleWorkspaces as workspace (workspace.workspaceId)}
            <div class="workspace-node">
              <button
                type="button"
                class="workspace-item"
                class:active={workspace.workspaceId === selectedWorkspaceId}
                data-workspace-id={workspace.workspaceId}
                onclick={() => toggleWorkspace(workspace)}
              >
                <span class="workspace-header">
                  <span
                    class="workspace-chevron"
                    class:workspace-chevron--expanded={!!expandedWorkspaceIds[workspace.workspaceId]}
                  >
                    ▾
                  </span>
                  <span class="workspace-name">{workspace.name}</span>
                </span>
                <span class="workspace-path">{workspace.rootPath}</span>
              </button>
              <div class="workspace-actions">
                <button
                  type="button"
                  class="workspace-action-btn workspace-action-btn--danger"
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
                  {:else if getVisibleWorkspaceSessions(workspace.workspaceId).length === 0}
                    <div class="sidebar-empty sidebar-empty--nested">当前工作区暂无会话</div>
                  {:else}
                    <div class="session-list session-list--nested">
                      {#each getVisibleWorkspaceSessions(workspace.workspaceId) as session (session.id)}
                        <button
                          type="button"
                          class="session-item"
                          class:active={session.id === currentSessionId}
                          class:pending={session.id === pendingSessionSwitchId}
                          data-session-id={session.id}
                          disabled={pendingSessionSwitchId !== null}
                          onclick={() => switchSession(session.id)}
                        >
                          <span class="session-name">{session.name || i18n.t('header.unnamedSession')}</span>
                          <span class="session-meta">{formatSessionMeta(session)}</span>
                        </button>
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
  </aside>

  <main
    class="workbench-content"
    class:workbench-content--mobile-dimmed={isMobileViewport && sidebarOpen}
    aria-hidden={isMobileViewport && sidebarOpen ? 'true' : 'false'}
  >
    {#if isMobileViewport}
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
    <App />
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

<style>
  .web-workbench-shell {
    display: grid;
    grid-template-columns: 280px minmax(0, 1fr);
    height: 100vh;
    width: 100vw;
    background: var(--background);
    color: var(--foreground);
    isolation: isolate;
    overflow: hidden;
  }

  .sidebar {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    padding: var(--space-4);
    border-right: 1px solid var(--border);
    background: var(--surface-1);
    overflow-y: auto;
  }

  .mobile-overlay {
    display: none;
  }

  .sidebar-header {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
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

  .workspace-path,
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
    overflow: hidden;
  }

  .sidebar-error-title {
    font-size: var(--text-sm);
    font-weight: var(--font-semibold);
    color: var(--foreground);
  }

  .section-title {
    font-size: var(--text-sm);
    font-weight: var(--font-semibold);
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--foreground-muted);
  }

  .workspace-tree,
  .session-list {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
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

  .sidebar::-webkit-scrollbar,
  .workspace-tree::-webkit-scrollbar {
    width: 10px;
  }

  .sidebar::-webkit-scrollbar-track,
  .workspace-tree::-webkit-scrollbar-track {
    background: color-mix(in srgb, var(--surface-2) 58%, transparent);
    border-radius: 999px;
  }

  .sidebar::-webkit-scrollbar-thumb,
  .workspace-tree::-webkit-scrollbar-thumb {
    background: var(--scrollbar-thumb);
    border-radius: 999px;
    border: 2px solid color-mix(in srgb, var(--surface-1) 88%, transparent);
    background-clip: content-box;
  }

  .sidebar::-webkit-scrollbar-thumb:hover,
  .workspace-tree::-webkit-scrollbar-thumb:hover {
    background: var(--scrollbar-thumb-hover);
    background-clip: content-box;
  }

  .sidebar-search {
    width: 100%;
    min-width: 0;
    height: 42px;
    padding: 0 var(--space-4);
    border-radius: var(--radius-md);
    background: var(--vscode-input-background, var(--surface-2));
    border: 1px solid var(--border);
    color: var(--foreground);
    font-size: var(--text-base);
    line-height: 1.2;
  }

  .sidebar-search:focus {
    outline: none;
    border-color: var(--info);
    box-shadow: 0 0 0 1px color-mix(in srgb, var(--info) 55%, transparent);
  }

  .workspace-node {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    position: relative;
  }

  .workspace-actions {
    position: absolute;
    top: var(--space-2);
    right: var(--space-2);
    display: flex;
    align-items: center;
    gap: var(--space-1);
  }

  .workspace-item,
  .session-item {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: var(--space-1);
    width: 100%;
    padding: var(--space-3);
    border-radius: var(--radius-lg);
    border: 1px solid var(--border-subtle);
    background: var(--surface-1);
    color: var(--foreground);
    cursor: pointer;
    text-align: left;
    transition: background var(--transition-fast), border-color var(--transition-fast);
    touch-action: manipulation;
    min-width: 0;
    overflow: hidden;
  }

  .workspace-item {
    padding-right: calc(var(--space-3) + 42px);
  }

  .workspace-item:hover,
  .session-item:hover {
    background: color-mix(in srgb, var(--surface-hover) 78%, transparent);
  }

  .workspace-item.active,
  .session-item.active {
    border-color: var(--info);
    background: color-mix(in srgb, var(--surface-selected) 80%, transparent);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--info) 18%, transparent);
  }

  .workspace-header {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }

  .workspace-chevron {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 12px;
    color: var(--foreground-muted);
    transform: rotate(-90deg);
    transition: transform var(--transition-fast);
  }

  .workspace-chevron--expanded {
    transform: rotate(0deg);
  }

  .workspace-name,
  .session-name {
    font-size: var(--text-md);
    font-weight: var(--font-medium);
    line-height: 1.3;
    display: -webkit-box;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 100%;
    word-break: break-word;
  }

  .workspace-path,
  .session-meta {
    display: block;
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .workspace-children {
    padding-left: var(--space-4);
    border-left: 1px solid var(--border-subtle);
  }

  .workspace-action-btn {
    width: 34px;
    height: 34px;
    border: 1px solid transparent;
    border-radius: var(--radius-md);
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    font-size: 18px;
    line-height: 1;
    display: inline-flex;
    align-items: center;
    justify-content: center;
  }

  .workspace-action-btn:hover {
    color: var(--foreground);
    border-color: color-mix(in srgb, var(--foreground) 20%, transparent);
    background: color-mix(in srgb, var(--foreground) 6%, transparent);
  }

  .workspace-action-btn--danger:hover {
    color: var(--error);
    border-color: color-mix(in srgb, var(--error) 22%, transparent);
    background: color-mix(in srgb, var(--error) 6%, transparent);
  }

  .session-list--nested {
    gap: var(--space-2);
  }

  .session-item {
    position: relative;
    width: calc(100% - 10px);
    margin-left: var(--space-2);
    min-height: 58px;
    justify-content: center;
  }

  .session-item::before {
    content: '';
    position: absolute;
    top: 50%;
    left: calc(var(--space-4) * -1);
    width: calc(var(--space-4) - var(--space-2));
    border-top: 1px solid var(--border-subtle);
    transform: translateY(-50%);
  }

  .session-item:disabled {
    cursor: default;
  }

  .session-item.pending {
    opacity: 0.78;
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
    min-width: 0;
    min-height: 0;
    overflow: hidden;
  }

  .workbench-content--mobile-dimmed {
    pointer-events: none;
    user-select: none;
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
    .web-workbench-shell {
      grid-template-columns: 240px minmax(0, 1fr);
    }
  }

  @media (max-width: 900px) {
    .web-workbench-shell {
      grid-template-columns: minmax(0, 1fr);
      position: relative;
    }

    .sidebar {
      position: fixed;
      top: 0;
      left: 0;
      bottom: 0;
      width: min(86vw, 320px);
      max-width: 320px;
      z-index: 12000;
      transform: translateX(-100%);
      transition: transform var(--transition-normal);
      box-shadow: var(--shadow-lg);
      border-right: 1px solid var(--border);
      overflow: hidden;
      padding:
        calc(var(--space-4) + env(safe-area-inset-top))
        var(--space-4)
        calc(var(--space-4) + env(safe-area-inset-bottom));
      background: var(--vscode-sideBar-secondaryBackground, var(--background));
      opacity: 1;
      isolation: isolate;
      backdrop-filter: none;
      -webkit-backdrop-filter: none;
      contain: layout paint style;
    }

    .sidebar-section {
      gap: var(--space-2);
    }

    .workspace-actions {
      top: var(--space-3);
      right: var(--space-3);
    }

    .workspace-action-btn {
      width: 38px;
      height: 38px;
    }

    .sidebar--mobile-open {
      transform: translateX(0);
    }

    .mobile-overlay {
      display: block;
      position: fixed;
      inset: 0;
      background: color-mix(in srgb, var(--overlay-heavy) 88%, transparent);
      z-index: 11990;
    }

    .workbench-content {
      display: flex;
      flex-direction: column;
      min-height: 0;
      position: relative;
      z-index: 0;
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

    .sidebar-search {
      height: 48px;
      font-size: var(--text-base);
    }

    .workspace-tree {
      padding-right: 0;
      gap: var(--space-3);
    }

    .sidebar-brand {
      flex-wrap: wrap;
    }

    .sidebar-header,
    .sidebar-section,
    .workspace-item,
    .session-item,
    .sidebar-search {
      background: color-mix(in srgb, var(--foreground) 3%, var(--vscode-sideBar-secondaryBackground, var(--background)));
    }

    .workspace-item.active,
    .session-item.active {
      background: color-mix(in srgb, var(--info) 10%, var(--vscode-sideBar-secondaryBackground, var(--background)));
    }

    .workspace-item,
    .session-item {
      padding: var(--space-3);
      border-radius: var(--radius-xl);
    }

    .workspace-item {
      padding-right: calc(var(--space-3) + 48px);
    }

    .workspace-name,
    .session-name {
      font-size: var(--text-base);
      line-height: 1.3;
    }

    .workspace-path,
    .session-meta {
      font-size: var(--text-sm);
      line-height: 1.35;
    }

    .workspace-children {
      margin-top: 2px;
      padding-left: var(--space-3);
    }

    .session-list--nested {
      gap: var(--space-2);
    }

    .session-item {
      min-height: 64px;
      margin-left: 0;
      padding-left: calc(var(--space-3) + 10px);
    }

    .session-item::before {
      left: calc(var(--space-3) * -1 + 2px);
      width: calc(var(--space-3) - 2px);
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

    .workspace-item,
    .session-item {
      padding: var(--space-3) var(--space-3);
    }

    .workspace-item {
      padding-right: calc(var(--space-3) + 48px);
    }
  }

  :global(html.magi-web-drawer-open),
  :global(body.magi-web-drawer-open) {
    overflow: hidden;
    overscroll-behavior: none;
  }
</style>
