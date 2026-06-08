<script lang="ts">
  import { addToast, getState, messagesState } from '../stores/messages.svelte';
  import { ensureArray } from '../lib/utils';
  import { vscode } from '../lib/vscode-bridge';
  import Icon from './Icon.svelte';
  import NotificationCenter from './NotificationCenter.svelte';
  import LanAccessPanel from './LanAccessPanel.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { buildSessionMarkdownExport, downloadMarkdown } from '../lib/session-export';
  import { getWebSidebarContext } from '../web/sidebar-context';
  import {
    rightPaneState,
    getRightPaneState,
    toggleRightPane,
  } from '../stores/right-pane.svelte';

  // 右侧面板：折叠/展开切换按钮作为常驻入口；作用域由 store 的 activeScopeKey 决定。
  const currentRightPane = $derived(getRightPaneState(rightPaneState.activeScopeKey));
  const showRightPaneToggle = $derived(Boolean(rightPaneState.activeScopeKey));

  import type { Snippet } from 'svelte';
  interface Props {
    onOpenSettings?: () => void;
    children?: Snippet;
  }

  let { onOpenSettings, children }: Props = $props();
  const appState = getState();
  // Web 外壳通过 context 注入 sidebar 切换能力，桌面/Tauri 路径无 context 时按钮不渲染。
  const webSidebar = getWebSidebarContext();

  // 局域网/隧道访问 popover（直接挂在 Header，操作更直观）
  let showLanPanel = $state(false);

  // 当前 workspace 的目录名（仅展示，便于在多 workspace 切换时确认上下文）
  const currentWorkspaceFolder = $derived.by(() => {
    const path = messagesState.currentWorkspacePath?.trim() ?? '';
    if (!path) return '';
    const cleaned = path.replace(/[\\/]+$/, '');
    const segments = cleaned.split(/[\\/]/).filter(Boolean);
    return segments.length > 0 ? segments[segments.length - 1] : cleaned;
  });

  // 只有“已有当前会话且该会话为空”时才禁止重复新建；草稿态不要求先绑定工作区。
  const hasCurrentSession = $derived.by(() => Boolean(messagesState.currentSessionId?.trim()));
  const isCurrentSessionEmpty = $derived(
    ensureArray(appState.threadMessages).length === 0
  );
  const newSessionDisabled = $derived(
    messagesState.sessionHydrating || (hasCurrentSession && isCurrentSessionEmpty)
  );
  const newSessionTitle = $derived(
    messagesState.sessionHydrating
      ? '正在打开新会话面板'
      : (hasCurrentSession && isCurrentSessionEmpty ? i18n.t('header.currentSessionEmpty') : i18n.t('header.newSession'))
  );

  // 导出当前会话为 Markdown 文件
  function exportSession() {
    const payload = buildSessionMarkdownExport();
    if (!payload) {
      addToast('warning', i18n.t('header.exportEmpty'));
      return;
    }
    try {
      downloadMarkdown(payload);
      addToast('success', i18n.t('header.exportSuccess', { count: payload.messageCount }));
    } catch (error) {
      console.warn('[Header] export session failed:', error);
      addToast('error', i18n.t('header.exportFailed'));
    }
  }
  const exportDisabled = $derived(
    !messagesState.currentSessionId || ensureArray(appState.threadMessages).length === 0,
  );

  // 新建会话
  function newSession() {
    addToast('info', '正在打开新会话面板...', undefined, {
      category: 'feedback',
      source: 'session-management',
      persistToCenter: false,
      countUnread: false,
      displayMode: 'notification_center',
      duration: 1800,
    });
    vscode.postMessage({
      type: 'newSession',
    });
  }

  // 打开设置
  function openSettings() {
    onOpenSettings?.();
  }

</script>

<header class="header-bar">
  {#if webSidebar}
    <button
      type="button"
      class="header-sidebar-toggle"
      aria-label={i18n.t(webSidebar.hidden || (webSidebar.isDrawer && !webSidebar.drawerOpen) ? 'web.expandSidebar' : 'web.collapseSidebar')}
      title={i18n.t(webSidebar.hidden || (webSidebar.isDrawer && !webSidebar.drawerOpen) ? 'web.expandSidebar' : 'web.collapseSidebar')}
      onclick={() => webSidebar.toggle()}
    >
      <Icon name="sidebar-toggle" size={14} />
    </button>
  {/if}
  {#if currentWorkspaceFolder}
    <div
      class="workspace-breadcrumb"
      title={messagesState.currentWorkspacePath || currentWorkspaceFolder}
      aria-label={i18n.t('header.workspaceBreadcrumbTitle', { path: messagesState.currentWorkspacePath || currentWorkspaceFolder })}
    >
      <Icon name="folder" size={12} />
      <span class="workspace-breadcrumb-name">{currentWorkspaceFolder}</span>
    </div>
  {/if}
  <div class="header-center">
    {@render children?.()}
  </div>

  <!-- 右侧操作按钮 -->
  <div class="header-actions">
    <button class="btn-icon btn-icon--sm" onclick={newSession} title={newSessionTitle} disabled={newSessionDisabled}>
      <Icon name="plus" size={14} />
    </button>
    <button
      class="btn-icon btn-icon--sm"
      onclick={exportSession}
      disabled={exportDisabled}
      title={i18n.t('header.exportSession')}
      aria-label={i18n.t('header.exportSession')}
    >
      <Icon name="download" size={14} />
    </button>
    <div class="lan-access-wrapper">
      <button
        class="btn-icon btn-icon--sm lan-access-trigger"
        class:active={showLanPanel}
        onclick={() => { showLanPanel = !showLanPanel; }}
        title={i18n.t('lanAccess.title')}
        aria-label={i18n.t('lanAccess.title')}
      >
        <Icon name="qrcode" size={14} />
      </button>
      <LanAccessPanel visible={showLanPanel} onClose={() => { showLanPanel = false; }} />
    </div>
    <NotificationCenter />
    {#if showRightPaneToggle}
      <button
        class="btn-icon btn-icon--sm"
        class:active={!currentRightPane.collapsed}
        onclick={() => toggleRightPane(rightPaneState.activeScopeKey)}
        title={currentRightPane.collapsed ? i18n.t('rightPane.expand') : i18n.t('rightPane.collapse')}
        aria-label={currentRightPane.collapsed ? i18n.t('rightPane.expand') : i18n.t('rightPane.collapse')}
        aria-expanded={!currentRightPane.collapsed}
      >
        <Icon name="sidebar-toggle" size={14} />
      </button>
    {/if}
    <button class="btn-icon btn-icon--sm" onclick={openSettings} title={i18n.t('header.settings')}>
      <Icon name="settings" size={14} />
    </button>
  </div>
</header>

<style>
  .header-bar {
    display: flex;
    justify-content: space-between;
    align-items: center;
    height: 48px;
    padding: 0 var(--space-4);
    background: var(--glass-bg);
    backdrop-filter: blur(14px);
    -webkit-backdrop-filter: blur(14px);
    flex-shrink: 0;
    position: sticky;
    top: 0;
    z-index: var(--z-sticky);
    border-bottom: 1px solid color-mix(in srgb, var(--border) 78%, transparent);
  }

  .header-center {
    display: flex;
    flex: 1;
    justify-content: center;
  }

  .header-sidebar-toggle {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
    margin-right: 6px;
    padding: 0;
    border: 1px solid transparent;
    border-radius: var(--radius-md);
    background: transparent;
    color: var(--foreground-muted);
    cursor: pointer;
    flex-shrink: 0;
    transition: background var(--transition-fast), color var(--transition-fast), border-color var(--transition-fast);
  }
  .header-sidebar-toggle:hover {
    background: var(--surface-hover);
    color: var(--foreground);
    border-color: var(--border);
  }

  .workspace-breadcrumb {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    height: 24px;
    padding: 0 8px;
    margin-right: 4px;
    color: var(--foreground-muted);
    font-size: 11px;
    border-radius: var(--radius-full);
    background: color-mix(in srgb, var(--surface-2) 50%, transparent);
    flex-shrink: 0;
    cursor: default;
  }
  .workspace-breadcrumb-name {
    max-width: 160px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  @media (max-width: 720px) {
    .workspace-breadcrumb-name { max-width: 80px; }
  }

  .header-actions {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    flex-shrink: 0;
    justify-content: flex-end;
  }

  .lan-access-wrapper {
    position: relative;
    display: inline-flex;
  }

  /* 移动端：Header 变两行 */
  @media (max-width: 768px) {
    .header-bar {
      flex-wrap: wrap;
      height: auto;
      padding: var(--space-2) var(--space-3);
      gap: var(--space-1);
    }

    .header-actions {
      flex: 0;
    }

    /* 第二行：Tab 栏占满整行 */
    .header-center {
      flex-basis: 100%;
      order: 3;
      justify-content: flex-start;
      overflow-x: auto;
      -webkit-overflow-scrolling: touch;
      scrollbar-width: none;
    }

    .header-center::-webkit-scrollbar {
      display: none;
    }
  }

  /* 使用全局 .btn-icon 样式，这里只覆盖必要的 */
</style>
