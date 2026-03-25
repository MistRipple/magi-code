<script lang="ts">
  import { onMount } from 'svelte';
  import App from '../App.svelte';
  import { messagesState } from '../stores/messages.svelte';
  import { getClientBridge } from '../../../shared/bridges/bridge-runtime';
  import { i18n } from '../stores/i18n.svelte';
  import type { Session } from '../types/message';
  import WebFolderPicker from './WebFolderPicker.svelte';
  import {
    AGENT_CONNECTION_EVENT,
    getWorkspaceSessions,
    listAgentWorkspaces,
    registerAgentWorkspace,
    renameAgentWorkspace,
    removeAgentWorkspace,
    resolveAgentBaseUrl,
    type AgentConnectionEventDetail,
    type AgentWorkspaceSummary,
  } from './agent-api';

  let loading = $state(true);
  let loadError = $state('');
  let agentBaseUrl = $state('');
  let workspaces = $state<AgentWorkspaceSummary[]>([]);
  let selectedWorkspaceId = $state('');
  let currentSessionId = $state<string | null>(null);
  let sessionsByWorkspace = $state<Record<string, Session[]>>({});
  let loadingWorkspaceIds = $state<Record<string, boolean>>({});
  let expandedWorkspaceIds = $state<Record<string, boolean>>({});
  let isMobileViewport = $state(false);
  let sidebarOpen = $state(false);
  let sidebarSearchQuery = $state('');
  let workspaceActionPending = $state(false);
  let showAddWorkspaceDialog = $state(false);
  let showRemoveWorkspaceDialog = $state(false);
  let showRenameWorkspaceDialog = $state(false);
  let pendingRemoveWorkspace = $state<AgentWorkspaceSummary | null>(null);
  let pendingRenameWorkspace = $state<AgentWorkspaceSummary | null>(null);
  let renameWorkspaceValue = $state('');
  let workspaceDialogError = $state('');

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
    if (!selectedWorkspaceId) {
      return;
    }

    const currentSessions = messagesState.sessions;
    if (!Array.isArray(currentSessions) || currentSessions.length === 0) {
      return;
    }

    const existingSessions = sessionsByWorkspace[selectedWorkspaceId] ?? [];
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
        [selectedWorkspaceId]: currentSessions,
      };
    }

    const bootstrapSessionId = typeof messagesState.currentSessionId === 'string'
      ? messagesState.currentSessionId.trim()
      : '';
    if (!bootstrapSessionId || bootstrapSessionId === currentSessionId) {
      return;
    }

    currentSessionId = bootstrapSessionId;
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
      localStorage.removeItem('magi-workspace-id');
      localStorage.removeItem('magi-workspace-path');
      localStorage.removeItem('magi-session-id');
      nextUrl.searchParams.delete('workspaceId');
      nextUrl.searchParams.delete('workspacePath');
      nextUrl.searchParams.delete('sessionId');
      if (nextUrl.toString() !== currentUrl.toString()) {
        window.history.replaceState(window.history.state, '', nextUrl);
      }
      return;
    }

    localStorage.setItem('magi-workspace-id', normalizedWorkspaceId);
    localStorage.setItem('magi-workspace-path', normalizedWorkspacePath);
    nextUrl.searchParams.set('workspaceId', normalizedWorkspaceId);
    nextUrl.searchParams.set('workspacePath', normalizedWorkspacePath);

    if (normalizedSessionId) {
      localStorage.setItem('magi-session-id', normalizedSessionId);
      nextUrl.searchParams.set('sessionId', normalizedSessionId);
    } else {
      localStorage.removeItem('magi-session-id');
      nextUrl.searchParams.delete('sessionId');
    }

    if (nextUrl.toString() !== currentUrl.toString()) {
      window.history.replaceState(window.history.state, '', nextUrl);
    }
  }

  function resolveWorkspacePreferredSessionId(workspaceId: string, workspacePath: string): string {
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

    const storedWorkspaceId = localStorage.getItem('magi-workspace-id') || '';
    const storedWorkspacePath = localStorage.getItem('magi-workspace-path') || '';
    const storedSessionId = localStorage.getItem('magi-session-id') || '';
    if (storedSessionId && (storedWorkspaceId === workspaceId || storedWorkspacePath === workspacePath)) {
      return storedSessionId;
    }

    if (selectedWorkspaceId === workspaceId && typeof currentSessionId === 'string' && currentSessionId.trim()) {
      return currentSessionId.trim();
    }

    return '';
  }

  function applyWorkspaceSessionsSnapshot(
    workspaceId: string,
    snapshot: Awaited<ReturnType<typeof getWorkspaceSessions>>,
    preferredSessionId = '',
  ): void {
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
    const resolvedSessionId = preservedSessionId || snapshot.sessionId;

    currentSessionId = resolvedSessionId || null;
    syncBrowserSessionBinding(snapshot.workspace.workspaceId, snapshot.workspace.rootPath, resolvedSessionId || null);
  }

  async function refreshWorkspaceSessions(workspaceId: string, preferredSessionId = ''): Promise<void> {
    if (!workspaceId) {
      currentSessionId = null;
      return;
    }
    loadingWorkspaceIds = { ...loadingWorkspaceIds, [workspaceId]: true };
    try {
      const snapshot = await getWorkspaceSessions(workspaceId, preferredSessionId);
      applyWorkspaceSessionsSnapshot(workspaceId, snapshot, preferredSessionId);
    } finally {
      loadingWorkspaceIds = { ...loadingWorkspaceIds, [workspaceId]: false };
    }
  }

  async function ensureWorkspaceSessions(workspaceId: string): Promise<void> {
    if (!workspaceId || getWorkspaceSessionList(workspaceId).length > 0 || loadingWorkspaceIds[workspaceId]) {
      return;
    }
    await refreshWorkspaceSessions(workspaceId);
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
      const storedWorkspaceId = localStorage.getItem('magi-workspace-id') || '';
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
        getClientBridge().postMessage({ type: 'requestState' });
      }
    } catch (error) {
      loadError = error instanceof Error ? error.message : String(error);
    } finally {
      loading = false;
    }
  }

  async function handleFolderSelected(rootPath: string, name: string): Promise<void> {
    if (workspaceActionPending) {
      return;
    }
    workspaceDialogError = '';
    workspaceActionPending = true;
    try {
      const normalizedRootPath = rootPath.trim();
      if (!normalizedRootPath) {
        return;
      }
      const next = await registerAgentWorkspace(normalizedRootPath, name?.trim() || undefined);
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
        getClientBridge().postMessage({ type: 'requestState' });
      }
      showAddWorkspaceDialog = false;
    } catch (error) {
      workspaceDialogError = error instanceof Error ? error.message : String(error);
    } finally {
      workspaceActionPending = false;
    }
  }

  function openAddWorkspaceDialog(): void {
    workspaceDialogError = '';
    showAddWorkspaceDialog = true;
  }

  function closeAddWorkspaceDialog(): void {
    if (workspaceActionPending) {
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

  function openRenameWorkspaceDialog(workspace: AgentWorkspaceSummary): void {
    if (workspaceActionPending) {
      return;
    }
    workspaceDialogError = '';
    pendingRenameWorkspace = workspace;
    renameWorkspaceValue = workspace.name;
    showRenameWorkspaceDialog = true;
  }

  function closeRenameWorkspaceDialog(): void {
    if (workspaceActionPending) {
      return;
    }
    workspaceDialogError = '';
    pendingRenameWorkspace = null;
    renameWorkspaceValue = '';
    showRenameWorkspaceDialog = false;
  }

  function closeRemoveWorkspaceDialog(): void {
    if (workspaceActionPending) {
      return;
    }
    workspaceDialogError = '';
    pendingRemoveWorkspace = null;
    showRemoveWorkspaceDialog = false;
  }

  async function renameWorkspace(): Promise<void> {
    if (workspaceActionPending || !pendingRenameWorkspace) {
      return;
    }
    const nextName = renameWorkspaceValue.trim();
    if (!nextName) {
      workspaceDialogError = '工作区名称不能为空。';
      return;
    }
    workspaceActionPending = true;
    workspaceDialogError = '';
    try {
      const next = await renameAgentWorkspace(
        pendingRenameWorkspace.workspaceId,
        pendingRenameWorkspace.rootPath,
        nextName,
      );
      workspaces = next;
      closeRenameWorkspaceDialog();
    } catch (error) {
      workspaceDialogError = error instanceof Error ? error.message : String(error);
    } finally {
      workspaceActionPending = false;
    }
  }

  async function removeWorkspace(): Promise<void> {
    if (workspaceActionPending || !pendingRemoveWorkspace) {
      return;
    }
    workspaceActionPending = true;
    workspaceDialogError = '';
    try {
      const next = await removeAgentWorkspace(pendingRemoveWorkspace.workspaceId, pendingRemoveWorkspace.rootPath);
      workspaces = next;
      sessionsByWorkspace = Object.fromEntries(
        Object.entries(sessionsByWorkspace).filter(([workspaceId]) => workspaceId !== pendingRemoveWorkspace.workspaceId)
      );
      loadingWorkspaceIds = Object.fromEntries(
        Object.entries(loadingWorkspaceIds).filter(([workspaceId]) => workspaceId !== pendingRemoveWorkspace.workspaceId)
      );
      expandedWorkspaceIds = Object.fromEntries(
        Object.entries(expandedWorkspaceIds).filter(([workspaceId]) => workspaceId !== pendingRemoveWorkspace.workspaceId)
      );

      if (selectedWorkspaceId === pendingRemoveWorkspace.workspaceId) {
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
          getClientBridge().postMessage({ type: 'requestState' });
        }
      }
      closeRemoveWorkspaceDialog();
    } catch (error) {
      workspaceDialogError = error instanceof Error ? error.message : String(error);
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
        const shouldLoad = !wasSelected || !isExpanded || getWorkspaceSessionList(workspace.workspaceId).length === 0;
        if (shouldLoad) {
          await refreshWorkspaceSessions(
            workspace.workspaceId,
            resolveWorkspacePreferredSessionId(workspace.workspaceId, workspace.rootPath),
          );
          getClientBridge().postMessage({ type: 'requestState' });
        }
      } catch (error) {
        loadError = error instanceof Error ? error.message : String(error);
      }
    })();
  }

  function switchSession(sessionId: string): void {
    if (!sessionId || sessionId === currentSessionId) {
      return;
    }
    getClientBridge().postMessage({ type: 'switchSession', sessionId });
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
    const handleResize = () => applyViewportMode();
    const handleAgentConnection = (event: Event) => {
      const detail = (event as CustomEvent<AgentConnectionEventDetail>).detail;
      const previousAgentBaseUrl = agentBaseUrl;
      agentBaseUrl = resolveAgentBaseUrl();
      if (detail?.status === 'recovering') {
        if (!workspaces.length && !loading) {
          loadError = detail.error || '正在等待 Local Agent 恢复连接...';
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
    };
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
      <div>
        <div class="sidebar-title">Magi Workbench</div>
        <div class="sidebar-subtitle">{i18n.t('header.sessionHistory')}</div>
      </div>
      <div class="sidebar-header-actions">
        <button class="sidebar-refresh" type="button" data-testid="sidebar-refresh" onclick={() => void refreshWorkspaces()}>
          {i18n.t('common.refresh')}
        </button>
        <button class="sidebar-refresh" type="button" onclick={openAddWorkspaceDialog} disabled={workspaceActionPending}>
          选择文件夹
        </button>
      </div>
    </div>

    <section class="sidebar-section">
      <div class="section-title">Agent</div>
      <div class="agent-card" class:agent-card--error={!!loadError}>
        <div class="agent-card-row">
          <span class="agent-status-dot" class:agent-status-dot--error={!!loadError}></span>
          <span class="agent-status-text">{loadError ? '未连接' : '已配置'}</span>
        </div>
        <div class="agent-base-url">{agentBaseUrl || '未设置 Agent 地址'}</div>
        {#if loadError}
          <div class="agent-help">
            请先启动 Local Agent，或通过当前 Agent 地址访问 `/web.html`
          </div>
        {/if}
      </div>
    </section>

    <section class="sidebar-section sidebar-section--workspaces">
      <div class="section-title">{i18n.t('common.workspace')}</div>
      <input
        class="sidebar-search"
        type="search"
        bind:value={sidebarSearchQuery}
        placeholder="搜索工作区或会话"
        aria-label="搜索工作区或会话"
      />
      {#if loading}
        <div class="sidebar-empty">{i18n.t('common.loading')}</div>
      {:else if loadError}
        <div class="sidebar-error">
          <div class="sidebar-error-title">工作区列表不可用</div>
          <div>{loadError}</div>
        </div>
      {:else if getVisibleWorkspaces().length === 0}
        <div class="sidebar-empty">未找到匹配的工作区或会话</div>
      {:else if workspaces.length === 0}
        <div class="sidebar-empty">暂无已注册工作区</div>
      {:else}
        <div class="workspace-tree">
          {#each getVisibleWorkspaces() as workspace (workspace.workspaceId)}
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
                  class="workspace-action-btn"
                  title="重命名工作区"
                  aria-label={`重命名工作区 ${workspace.name}`}
                  onclick={(event) => {
                    event.stopPropagation();
                    openRenameWorkspaceDialog(workspace);
                  }}
                >
                  ✎
                </button>
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
                          data-session-id={session.id}
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
          {sidebarOpen ? '关闭导航' : '工作区 / 会话'}
        </button>
        <div class="mobile-toolbar-meta">
          <div class="mobile-toolbar-title">{selectedWorkspace?.name || '未选择工作区'}</div>
          <div class="mobile-toolbar-subtitle">{currentSession?.name || i18n.t('header.unnamedSession')}</div>
        </div>
      </div>
    {/if}
    <App />
  </main>
</div>

{#if showAddWorkspaceDialog}
  <div class="modal-overlay" role="presentation" onclick={closeAddWorkspaceDialog}>
    <div
      class="modal-dialog modal-dialog--md"
      role="dialog"
      aria-modal="true"
      aria-labelledby="workspace-picker-title"
      tabindex="-1"
      onclick={(event) => event.stopPropagation()}
      onkeydown={(event) => {
        if (event.key === 'Escape') {
          closeAddWorkspaceDialog();
        }
      }}
    >
      <div class="modal-header">
        <div class="modal-title" id="workspace-picker-title">选择工作区目录</div>
        <button class="modal-close" type="button" onclick={closeAddWorkspaceDialog}>×</button>
      </div>
      <div class="modal-body" style="padding: 0;">
        {#if workspaceDialogError}
          <div class="workspace-dialog-error" style="margin: var(--space-3, 12px);">{workspaceDialogError}</div>
        {/if}
        <WebFolderPicker
          onSelect={(path, name) => void handleFolderSelected(path, name)}
          onCancel={closeAddWorkspaceDialog}
          disabled={workspaceActionPending}
        />
      </div>
    </div>
  </div>
{/if}

{#if showRemoveWorkspaceDialog && pendingRemoveWorkspace}
  <div class="modal-overlay" role="presentation" onclick={closeRemoveWorkspaceDialog}>
    <div
      class="modal-dialog modal-dialog--sm"
      role="dialog"
      aria-modal="true"
      aria-labelledby="workspace-remove-title"
      tabindex="-1"
      onclick={(event) => event.stopPropagation()}
      onkeydown={(event) => {
        if (event.key === 'Escape') {
          closeRemoveWorkspaceDialog();
        }
      }}
    >
      <div class="modal-header">
        <div class="modal-title" id="workspace-remove-title">从 Magi 中移除工作区</div>
        <button class="modal-close" type="button" onclick={closeRemoveWorkspaceDialog}>×</button>
      </div>
      <div class="modal-body">
        <p class="workspace-dialog-text">将从 Magi 的工作区注册表中移除 <strong>{pendingRemoveWorkspace.name}</strong>。</p>
        <p class="workspace-dialog-text workspace-dialog-text--muted">不会删除本地项目目录，也不会删除该工作区下已有的历史会话。</p>
        {#if workspaceDialogError}
          <div class="workspace-dialog-error">{workspaceDialogError}</div>
        {/if}
      </div>
      <div class="modal-footer">
        <button class="modal-btn secondary" type="button" onclick={closeRemoveWorkspaceDialog} disabled={workspaceActionPending}>取消</button>
        <button class="modal-btn danger" type="button" onclick={() => void removeWorkspace()} disabled={workspaceActionPending}>
          {workspaceActionPending ? '正在移除...' : '确认移除'}
        </button>
      </div>
    </div>
  </div>
{/if}

{#if showRenameWorkspaceDialog && pendingRenameWorkspace}
  <div class="modal-overlay" role="presentation" onclick={closeRenameWorkspaceDialog}>
    <div
      class="modal-dialog modal-dialog--sm"
      role="dialog"
      aria-modal="true"
      aria-labelledby="workspace-rename-title"
      tabindex="-1"
      onclick={(event) => event.stopPropagation()}
      onkeydown={(event) => {
        if (event.key === 'Escape') {
          closeRenameWorkspaceDialog();
        }
      }}
    >
      <div class="modal-header">
        <div class="modal-title" id="workspace-rename-title">重命名工作区</div>
        <button class="modal-close" type="button" onclick={closeRenameWorkspaceDialog}>×</button>
      </div>
      <div class="modal-body">
        <label class="workspace-dialog-label" for="workspace-rename-input">工作区名称</label>
        <input
          id="workspace-rename-input"
          class="workspace-dialog-input"
          type="text"
          bind:value={renameWorkspaceValue}
          placeholder="请输入新的工作区名称"
        />
        {#if workspaceDialogError}
          <div class="workspace-dialog-error">{workspaceDialogError}</div>
        {/if}
      </div>
      <div class="modal-footer">
        <button class="modal-btn secondary" type="button" onclick={closeRenameWorkspaceDialog} disabled={workspaceActionPending}>取消</button>
        <button class="modal-btn primary" type="button" onclick={() => void renameWorkspace()} disabled={workspaceActionPending}>
          {workspaceActionPending ? '正在保存...' : '保存名称'}
        </button>
      </div>
    </div>
  </div>
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
    gap: var(--space-4);
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
    align-items: flex-start;
    justify-content: space-between;
    gap: var(--space-3);
  }

  .sidebar-header-actions {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }

  .sidebar-title {
    font-size: var(--text-lg);
    font-weight: var(--font-semibold);
    line-height: 1.15;
    letter-spacing: -0.01em;
  }

  .sidebar-subtitle,
  .workspace-path,
  .session-meta,
  .sidebar-empty,
  .agent-base-url,
  .agent-help {
    color: var(--foreground-muted);
    font-size: var(--text-base);
  }

  .sidebar-refresh {
    height: var(--btn-height-sm);
    padding: 0 var(--space-3);
    border-radius: var(--radius-md);
    border: 1px solid var(--border);
    background: var(--surface-2);
    color: var(--foreground);
    cursor: pointer;
    white-space: nowrap;
    word-break: keep-all;
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

  .agent-card {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    padding: var(--space-3);
    border-radius: var(--radius-lg);
    border: 1px solid var(--border);
    background: var(--surface-1);
  }

  .agent-card--error {
    border-color: color-mix(in srgb, var(--error) 40%, var(--border));
    background: color-mix(in srgb, var(--error) 7%, var(--surface-1));
  }

  .agent-card-row {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }

  .agent-status-dot {
    width: 8px;
    height: 8px;
    border-radius: var(--radius-full);
    background: var(--success);
    flex-shrink: 0;
  }

  .agent-status-dot--error {
    background: var(--error);
  }

  .agent-status-text,
  .sidebar-error-title {
    font-size: var(--text-base);
    font-weight: var(--font-semibold);
    color: var(--foreground);
  }

  .section-title {
    font-size: var(--text-base);
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
    height: 44px;
    padding: 0 var(--space-4);
    border-radius: var(--radius-md);
    background: var(--vscode-input-background, var(--surface-2));
    border: 1px solid var(--border);
    color: var(--foreground);
    font-size: var(--text-base);
    line-height: 1.2;
  }

  .sidebar-search:focus,
  .workspace-dialog-input:focus {
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
    min-width: 0;
    overflow: hidden;
  }

  .workspace-item {
    padding-right: calc(var(--space-3) + 76px);
  }

  .workspace-item:hover,
  .session-item:hover {
    background: var(--surface-hover);
  }

  .workspace-item.active,
  .session-item.active {
    border-color: var(--info);
    background: var(--surface-selected);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--info) 28%, transparent);
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
    line-height: var(--leading-tight);
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
    border-color: color-mix(in srgb, var(--foreground) 25%, transparent);
    background: color-mix(in srgb, var(--foreground) 8%, transparent);
  }

  .workspace-action-btn--danger:hover {
    color: var(--error);
    border-color: color-mix(in srgb, var(--error) 25%, transparent);
    background: color-mix(in srgb, var(--error) 8%, transparent);
  }

  .session-list--nested {
    gap: var(--space-2);
  }

  .session-item {
    position: relative;
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

  .workspace-dialog-text {
    margin: 0;
    color: var(--foreground);
    line-height: 1.6;
  }

  .workspace-dialog-text--muted {
    color: var(--foreground-muted);
    font-size: var(--text-sm);
  }

  .workspace-dialog-label {
    display: block;
    margin-bottom: var(--space-2);
    color: var(--foreground);
    font-size: var(--text-sm);
    font-weight: var(--font-medium);
  }

  .workspace-dialog-input {
    width: 100%;
    min-width: 0;
    height: var(--btn-height-md);
    padding: 0 var(--space-3);
    border-radius: var(--radius-md);
    border: 1px solid var(--border);
    background: var(--surface-2);
    color: var(--foreground);
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

    .sidebar-header {
      align-items: stretch;
      flex-direction: column;
      gap: var(--space-3);
    }

    .sidebar-header-actions {
      width: 100%;
      display: grid;
      grid-template-columns: repeat(2, minmax(0, 1fr));
      gap: var(--space-2);
    }

    .sidebar-refresh {
      width: 100%;
      min-width: 0;
      min-height: 42px;
      padding: 0 var(--space-2);
      font-size: var(--text-base);
      text-align: center;
      background: var(--surface-2);
    }

    .sidebar-header,
    .sidebar-section,
    .agent-card,
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
      padding-right: calc(var(--space-3) + 84px);
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

    .sidebar-subtitle {
      font-size: var(--text-base);
    }

    .sidebar-header {
      gap: var(--space-2);
    }

    .sidebar-refresh {
      min-height: 44px;
    }

    .workspace-item,
    .session-item {
      padding: var(--space-3) var(--space-3);
    }

    .workspace-item {
      padding-right: calc(var(--space-3) + 84px);
    }
  }

  :global(html.magi-web-drawer-open),
  :global(body.magi-web-drawer-open) {
    overflow: hidden;
    overscroll-behavior: none;
  }
</style>
