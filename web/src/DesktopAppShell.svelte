<script lang="ts">
  import WebWorkbenchShell from './web/WebWorkbenchShell.svelte';
  import { messagesState } from './stores/messages.svelte';
  import { resolveCurrentSessionTitle } from './lib/session-title';

  let publishedContextKey = '';

  const currentSessionTitle = $derived(resolveCurrentSessionTitle({
    sessionId: messagesState.currentSessionId,
    workspaceId: messagesState.currentWorkspaceId,
    workspacePath: messagesState.currentWorkspacePath,
    workspaceSessions: messagesState.workspaceSessionProjection,
    workspaceSessionProjections: messagesState.workspaceSessionProjections,
    personalSessions: messagesState.personalSessionProjection.sessions,
  }));

  $effect(() => {
    const desktop = window.magiDesktop;
    if (!desktop) return;
    const context = {
      workspaceId: messagesState.currentWorkspaceId?.trim() || '',
      workspacePath: messagesState.currentWorkspacePath?.trim() || '',
      sessionId: messagesState.currentSessionId?.trim() || '',
      sessionTitle: currentSessionTitle,
    };
    const key = `${context.workspaceId}\u0000${context.workspacePath}\u0000${context.sessionId}\u0000${context.sessionTitle}`;
    if (key === publishedContextKey) return;
    publishedContextKey = key;
    void desktop.setContext(context).catch((error) => {
      if (key === publishedContextKey) publishedContextKey = '';
      console.error('[DesktopAppShell] 发布桌面上下文失败:', error);
    });
  });
</script>

<div class="desktop-app-frame">
  <div class="desktop-titlebar" data-testid="desktop-titlebar" aria-hidden="true">
    <span>{currentSessionTitle || 'Magi'}</span>
  </div>
  <div class="desktop-workbench">
    <WebWorkbenchShell desktopAppSurface={true} />
  </div>
</div>

<style>
  .desktop-app-frame {
    position: relative;
    height: 100%;
    display: flex;
    flex-direction: column;
  }

  .desktop-titlebar {
    flex: 0 0 max(32px, env(titlebar-area-height, 32px));
    background: var(--magi-surface-main);
    color: var(--foreground-muted);
    -webkit-app-region: drag;
    user-select: none;
    display: flex;
    align-items: center;
    font-size: 12px;
  }

  .desktop-titlebar span {
    margin-left: env(titlebar-area-x, 80px);
    width: env(titlebar-area-width, calc(100% - 160px));
    padding: 0 12px;
    text-align: center;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .desktop-workbench {
    flex: 1;
    min-height: 0;
  }
</style>
