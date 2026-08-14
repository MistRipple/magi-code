<script lang="ts">
  import WebWorkbenchShell from './web/WebWorkbenchShell.svelte';
  import { messagesState } from './stores/messages.svelte';

  let publishedContextKey = '';

  $effect(() => {
    const desktop = window.magiDesktop;
    if (!desktop) return;
    const context = {
      workspaceId: messagesState.currentWorkspaceId?.trim() || '',
      workspacePath: messagesState.currentWorkspacePath?.trim() || '',
      sessionId: messagesState.currentSessionId?.trim() || '',
    };
    const key = `${context.workspaceId}\u0000${context.workspacePath}\u0000${context.sessionId}`;
    if (key === publishedContextKey) return;
    publishedContextKey = key;
    void desktop.setContext(context).catch((error) => {
      if (key === publishedContextKey) publishedContextKey = '';
      console.error('[DesktopAppShell] 发布桌面上下文失败:', error);
    });
  });
</script>

<WebWorkbenchShell desktopAppSurface={true} />
