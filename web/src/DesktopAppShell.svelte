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

<WebWorkbenchShell desktopAppSurface={true} />
