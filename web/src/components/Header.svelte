<script lang="ts">
  import { onMount } from 'svelte';
  import { getState, getUnreadNotificationCount, messagesState } from '../stores/messages.svelte';
  import { showFeedback } from '../lib/notifications';
  import { ensureArray } from '../lib/utils';
  import { vscode } from '../lib/vscode-bridge';
  import Icon from './Icon.svelte';
  import NotificationCenter from './NotificationCenter.svelte';
  import LanAccessPanel from './LanAccessPanel.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { getWebSidebarContext } from '../web/sidebar-context';
  import {
    rightPaneState,
    getRightPaneState,
    setRightPaneCollapsed,
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

  type HeaderPanel = 'notifications' | 'more' | 'lan';
  let activeHeaderPanel = $state<HeaderPanel | null>(null);

  // 只有“已有当前会话且该会话为空”时才禁止重复新建；草稿态不要求先绑定工作区。
  const hasCurrentSession = $derived.by(() => Boolean(messagesState.currentSessionId?.trim()));
  const unreadNotificationCount = $derived.by(() => getUnreadNotificationCount());
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

  // 新建会话
  function newSession() {
    showFeedback('info', '正在打开新会话面板...', {
      source: 'session-management',
      duration: 1800,
    });
    vscode.postMessage({
      type: 'newSession',
    });
  }

  // 打开设置
  function openSettings() {
    activeHeaderPanel = null;
    onOpenSettings?.();
  }

  function openRemoteAccess() {
    activeHeaderPanel = 'lan';
  }

  function setNotificationOpen(open: boolean) {
    activeHeaderPanel = open ? 'notifications' : null;
  }

  function openRightPane() {
    activeHeaderPanel = null;
    if (webSidebar) {
      webSidebar.openRightPane();
      return;
    }
    setRightPaneCollapsed(rightPaneState.activeScopeKey, false);
  }

  onMount(() => {
    const closeHeaderPanel = (event: PointerEvent) => {
      const target = event.target instanceof Element ? event.target : null;
      if (!target?.closest('.header-actions')) {
        activeHeaderPanel = null;
      }
    };
    const closeHeaderPanelOnEscape = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        activeHeaderPanel = null;
      }
    };
    window.addEventListener('pointerdown', closeHeaderPanel);
    window.addEventListener('keydown', closeHeaderPanelOnEscape);
    return () => {
      window.removeEventListener('pointerdown', closeHeaderPanel);
      window.removeEventListener('keydown', closeHeaderPanelOnEscape);
    };
  });

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
  <div class="header-center">
    {@render children?.()}
  </div>

  <!-- 右侧操作按钮 -->
  <div class="header-actions">
    <button class="btn-icon header-action-btn" onclick={newSession} title={newSessionTitle} disabled={newSessionDisabled}>
      <Icon name="plus" size={14} />
    </button>
    <button
      class="btn-icon header-action-btn header-notification-btn"
      class:active={activeHeaderPanel === 'notifications'}
      onclick={() => setNotificationOpen(activeHeaderPanel !== 'notifications')}
      title={i18n.t('notification.buttonTitle')}
      aria-label={i18n.t('notification.buttonTitle')}
      aria-expanded={activeHeaderPanel === 'notifications'}
    >
      <Icon name="bell" size={14} />
      {#if unreadNotificationCount > 0}
        <span class="header-action-badge">{unreadNotificationCount > 99 ? '99+' : unreadNotificationCount}</span>
      {/if}
    </button>
    {#if showRightPaneToggle && currentRightPane.collapsed}
      <button
        class="btn-icon header-action-btn header-right-pane-btn"
        onclick={openRightPane}
        title={i18n.t('rightPane.expand')}
        aria-label={i18n.t('rightPane.expand')}
      >
        <Icon name="sidebar-toggle" size={14} class="right-pane-toggle-icon" />
      </button>
    {/if}
    <div class="header-more-wrapper">
      <button
        class="btn-icon header-action-btn"
        class:active={activeHeaderPanel === 'more' || activeHeaderPanel === 'lan'}
        class:header-mobile-active={activeHeaderPanel === 'notifications'}
        onclick={(event) => {
          event.stopPropagation();
          activeHeaderPanel = activeHeaderPanel === 'more' ? null : 'more';
        }}
        title={i18n.t('header.more')}
        aria-label={i18n.t('header.more')}
        aria-expanded={activeHeaderPanel === 'more'}
      >
        <Icon name="more-horizontal" size={14} />
        {#if unreadNotificationCount > 0}
          <span class="header-more-unread-dot" aria-hidden="true"></span>
        {/if}
      </button>
      {#if activeHeaderPanel === 'more'}
        <div class="header-more-menu">
          <button class="header-mobile-menu-item" type="button" onclick={() => setNotificationOpen(true)}>
            <Icon name="bell" size={14} />
            <span>{i18n.t('notification.title')}</span>
            {#if unreadNotificationCount > 0}
              <span class="header-menu-badge">{unreadNotificationCount > 99 ? '99+' : unreadNotificationCount}</span>
            {/if}
          </button>
          <button type="button" onclick={openRemoteAccess}>
            <Icon name="qrcode" size={14} />
            <span>{i18n.t('lanAccess.title')}</span>
          </button>
          <button type="button" onclick={openSettings}>
            <Icon name="settings" size={14} />
            <span>{i18n.t('header.settings')}</span>
          </button>
        </div>
      {/if}
      <LanAccessPanel
        visible={activeHeaderPanel === 'lan'}
        onClose={() => { activeHeaderPanel = null; }}
      />
      <NotificationCenter
        open={activeHeaderPanel === 'notifications'}
        onOpenChange={setNotificationOpen}
      />
    </div>
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
    width: 32px;
    height: 32px;
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

  .header-actions {
    display: flex;
    align-items: center;
    gap: 3px;
    flex-shrink: 0;
    justify-content: flex-end;
    position: relative;
  }

  .header-more-wrapper {
    position: relative;
    display: inline-flex;
  }

  .header-action-btn {
    position: relative;
    width: 32px;
    height: 32px;
    flex: 0 0 32px;
  }

  .header-action-btn.active {
    background: var(--surface-active);
    color: var(--foreground);
  }

  .header-action-badge {
    position: absolute;
    top: -2px;
    right: -2px;
    min-width: 14px;
    height: 14px;
    padding: 0 3px;
    border-radius: var(--radius-full);
    background: var(--error);
    color: var(--primary-foreground);
    font-size: 9px;
    font-weight: var(--font-bold);
    line-height: 14px;
    text-align: center;
    pointer-events: none;
  }

  .header-more-unread-dot {
    display: none;
    position: absolute;
    top: 5px;
    right: 5px;
    width: 6px;
    height: 6px;
    border-radius: var(--radius-full);
    background: var(--error);
    box-shadow: 0 0 0 2px var(--glass-bg);
  }

  :global(.right-pane-toggle-icon) {
    transform: scaleX(-1);
  }

  .header-more-menu {
    position: absolute;
    top: calc(100% + 6px);
    right: 0;
    z-index: var(--z-popover);
    width: 168px;
    padding: 5px;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--dropdown-bg);
    box-shadow: var(--shadow-lg);
  }

  .header-more-menu button {
    width: 100%;
    height: 34px;
    padding: 0 9px;
    display: flex;
    align-items: center;
    gap: 9px;
    border: 0;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--foreground);
    font-size: var(--text-sm);
    cursor: pointer;
  }

  .header-more-menu button:hover {
    background: var(--surface-hover);
  }

  .header-more-menu .header-mobile-menu-item {
    display: none;
  }

  .header-menu-badge {
    min-width: 18px;
    height: 18px;
    margin-left: auto;
    padding: 0 5px;
    border-radius: var(--radius-full);
    background: var(--error);
    color: var(--primary-foreground);
    font-size: 10px;
    line-height: 18px;
    text-align: center;
  }

  /* 移动端：低频入口收进更多菜单，顶部保持单行三段式。 */
  @media (max-width: 768px) {
    .header-bar {
      display: grid;
      grid-template-columns: 1fr auto 1fr;
      height: 48px;
      padding: var(--space-2) var(--space-3);
      gap: var(--space-2);
    }

    .header-sidebar-toggle {
      grid-column: 1;
      justify-self: start;
      margin-right: 0;
    }

    .header-actions {
      grid-column: 3;
      justify-self: end;
    }

    .header-sidebar-toggle,
    .header-action-btn {
      width: 38px;
      height: 38px;
      flex-basis: 38px;
    }

    .header-center {
      grid-column: 2;
      min-width: 0;
      justify-content: center;
      overflow-x: auto;
      -webkit-overflow-scrolling: touch;
      scrollbar-width: none;
    }

    .header-center::-webkit-scrollbar {
      display: none;
    }

    .header-notification-btn {
      display: none;
    }

    .header-more-unread-dot {
      display: block;
    }

    .header-action-btn.header-mobile-active {
      background: var(--surface-active);
      color: var(--foreground);
    }

    .header-more-menu .header-mobile-menu-item {
      display: flex;
    }
  }

  /* 使用全局 .btn-icon 样式，这里只覆盖必要的 */
</style>
