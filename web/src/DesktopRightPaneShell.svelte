<script lang="ts">
  import { onMount } from 'svelte';
  import RightPane from './web/RightPane.svelte';
  import { i18n } from './stores/i18n.svelte';
  import {
    activateRightPaneSession,
    openAgentTab,
    openCodeTab,
    openTerminalTab,
    synchronizeBrowserTabs,
  } from './stores/right-pane.svelte';
  import type { DesktopRightPaneTabIntent } from '@magi/desktop-browser-contracts';
  import {
    BROWSER_AUTHORITY_CHANGED_EVENT,
    getCurrentBrowserSession,
  } from './web/agent-api';

  const expectedWindowId = window.magiDesktop?.windowId
    ?? new URLSearchParams(window.location.search).get('desktopWindowId')
    ?? '';
  let snapshot = $state<MagiDesktopWindowSnapshot | null>(null);
  let context = $state<MagiDesktopContextSnapshot>({
    contextRevision: 0,
    workspaceId: '',
    workspacePath: '',
    sessionId: '',
  });
  let browserAuthorityRequest = 0;
  let resizeFrame: number | null = null;
  let pendingRightPaneWidth: number | null = null;

  const overlay = $derived(snapshot?.layout.rightPaneMode === 'overlay');
  const workspaceRoot = $derived(context.workspacePath);

  function applySnapshot(next: MagiDesktopWindowSnapshot): void {
    if (expectedWindowId && next.windowId !== expectedWindowId) return;
    if (snapshot && next.desktopEpoch === snapshot.desktopEpoch && next.snapshotRevision < snapshot.snapshotRevision) {
      return;
    }
    snapshot = next;
  }

  function applyContext(next: MagiDesktopContextSnapshot): void {
    if (expectedWindowId && next.windowId && next.windowId !== expectedWindowId) return;
    if (next.contextRevision < context.contextRevision) return;
    context = {
      contextRevision: next.contextRevision,
      windowId: next.windowId,
      workspaceId: next.workspaceId?.trim() ?? '',
      workspacePath: next.workspacePath?.trim() ?? '',
      sessionId: next.sessionId?.trim() ?? '',
    };
    activateRightPaneSession(context.workspaceId, context.sessionId);
    void synchronizeBrowserAuthority();
  }

  async function synchronizeBrowserAuthority(revealActiveTab = false): Promise<void> {
    const workspaceId = context.workspaceId;
    const workspacePath = context.workspacePath;
    const sessionId = context.sessionId;
    if (!sessionId) return;
    const request = ++browserAuthorityRequest;
    try {
      const authority = await getCurrentBrowserSession(workspaceId, sessionId, workspacePath);
      if (
        request !== browserAuthorityRequest
        || workspaceId !== context.workspaceId
        || sessionId !== context.sessionId
      ) {
        return;
      }
      synchronizeBrowserTabs(workspaceId, workspacePath, sessionId, authority, {
        revealActiveTab,
        newTabLabel: i18n.t('browser.tab.new'),
      });
    } catch (error) {
      if (request === browserAuthorityRequest) {
        console.warn('[DesktopRightPaneShell] 同步浏览器权威状态失败:', error);
      }
    }
  }

  function submitPendingRightPaneWidth(): void {
    resizeFrame = null;
    const width = pendingRightPaneWidth;
    pendingRightPaneWidth = null;
    if (width === null) return;
    void window.magiDesktop?.submitLayoutIntent({ type: 'right_pane_width', width })
      .catch((error) => console.warn('[DesktopRightPaneShell] 调整右栏宽度失败:', error));
  }

  function startRightPaneResize(event: PointerEvent): void {
    const initialWidth = snapshot?.layout.rightPaneWidth;
    if (!initialWidth || !window.magiDesktop) return;
    event.preventDefault();
    const startScreenX = event.screenX;
    const handle = event.currentTarget as HTMLElement;
    handle.setPointerCapture(event.pointerId);
    document.body.style.cursor = 'col-resize';
    document.body.style.userSelect = 'none';
    const move = (moveEvent: PointerEvent) => {
      pendingRightPaneWidth = initialWidth - (moveEvent.screenX - startScreenX);
      if (resizeFrame === null) {
        resizeFrame = requestAnimationFrame(submitPendingRightPaneWidth);
      }
    };
    const stop = () => {
      handle.removeEventListener('pointermove', move);
      handle.removeEventListener('pointerup', stop);
      handle.removeEventListener('pointercancel', stop);
      document.body.style.cursor = '';
      document.body.style.userSelect = '';
      if (resizeFrame !== null) cancelAnimationFrame(resizeFrame);
      submitPendingRightPaneWidth();
    };
    handle.addEventListener('pointermove', move);
    handle.addEventListener('pointerup', stop);
    handle.addEventListener('pointercancel', stop);
  }

  function applyRightPaneIntent(intent: DesktopRightPaneTabIntent): void {
    switch (intent.kind) {
      case 'agent':
        openAgentTab(intent.sessionId, intent.agentRunId, {
          workspaceId: intent.workspaceId,
          workspacePath: intent.workspacePath,
          label: intent.label,
          accentToken: intent.accentToken,
        });
        return;
      case 'code':
        openCodeTab(intent.sessionId, intent.filepath, {
          workspaceId: intent.workspaceId,
          workspacePath: intent.workspacePath,
          sessionId: intent.sessionId,
          label: intent.label,
          displayPath: intent.displayPath,
          diff: intent.diff,
          originalContent: intent.originalContent,
          currentContent: intent.currentContent,
          isChangeDiff: intent.isChangeDiff,
          changeRevision: intent.changeRevision ?? undefined,
          content: intent.content,
          language: intent.language,
          contentKind: intent.contentKind,
          size: intent.size ?? undefined,
          mime: intent.mime ?? undefined,
          symlinkTarget: intent.symlinkTarget ?? undefined,
          headSummary: intent.headSummary ?? undefined,
          tailSummary: intent.tailSummary ?? undefined,
          imageDataUrl: intent.imageDataUrl ?? undefined,
        });
        return;
      case 'terminal':
        openTerminalTab({
          terminalTabId: intent.terminalTabId,
          workspaceId: intent.workspaceId,
          workspacePath: intent.workspacePath,
          sessionId: intent.sessionId,
        });
        return;
    }
  }

  onMount(() => {
    const desktop = window.magiDesktop;
    if (!desktop) {
      throw new Error('desktop_preload_bridge_unavailable');
    }
    let disposed = false;
    void desktop.getSnapshot().then((next) => {
      if (!disposed) applySnapshot(next);
    }).catch((error) => {
      if (!disposed) console.error('[DesktopRightPaneShell] 获取桌面窗口快照失败:', error);
    });
    const stopSnapshot = desktop.onSnapshot(applySnapshot);
    const stopContext = desktop.onContext(applyContext);
    const stopRightPaneIntent = desktop.onRightPaneIntent((envelope) => {
      if (envelope.version !== 1) return;
      applyRightPaneIntent(envelope.intent);
    });
    void desktop.readyRightPane().catch((error) => {
      if (!disposed) console.error('[DesktopRightPaneShell] 右栏 Renderer 就绪通知失败:', error);
    });
    const handleBrowserAuthorityChanged = (event: Event) => {
      const detail = (event as CustomEvent<{
        eventType?: string;
        workspaceId?: string;
        sessionId?: string;
      }>).detail;
      if (
        (detail?.workspaceId?.trim() && detail.workspaceId.trim() !== context.workspaceId)
        || (detail?.sessionId?.trim() && detail.sessionId.trim() !== context.sessionId)
      ) {
        return;
      }
      const eventType = detail?.eventType?.trim() ?? '';
      void synchronizeBrowserAuthority(
        eventType === 'browser.tab.created' || eventType === 'browser.tab.activated',
      );
    };
    window.addEventListener(BROWSER_AUTHORITY_CHANGED_EVENT, handleBrowserAuthorityChanged);
    return () => {
      disposed = true;
      stopSnapshot();
      stopContext();
      stopRightPaneIntent();
      window.removeEventListener(BROWSER_AUTHORITY_CHANGED_EVENT, handleBrowserAuthorityChanged);
    };
  });
</script>

<div class="desktop-right-pane-shell">
  <div
    class="desktop-right-pane-resize-handle"
    role="separator"
    aria-orientation="vertical"
    aria-label={i18n.t('web.filePreviewResizeReset')}
    onpointerdown={startRightPaneResize}
    ondblclick={() => void window.magiDesktop?.submitLayoutIntent({ type: 'right_pane_reset_width' })}
  ></div>
  <RightPane workspaceRoot={workspaceRoot} {overlay} desktopSurface={true} />
</div>

<style>
  .desktop-right-pane-shell {
    width: 100%;
    height: 100%;
    min-width: 0;
    min-height: 0;
    overflow: hidden;
    position: relative;
  }

  .desktop-right-pane-resize-handle {
    position: absolute;
    z-index: 2;
    inset: 0 auto auto 0;
    width: 8px;
    height: 100%;
    cursor: col-resize;
    touch-action: none;
  }
</style>
