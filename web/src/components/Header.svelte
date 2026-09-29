<script lang="ts">
  import { onMount } from 'svelte';
  import { getUnreadNotificationCount } from '../stores/messages.svelte';
  import Icon from './Icon.svelte';
  import LanAccessPanel from './LanAccessPanel.svelte';
  import { i18n } from '../stores/i18n.svelte';
  import { getWebSidebarContext } from '../web/sidebar-context';
  import { closePopover, shellUi, togglePopover } from '../stores/shell-ui.svelte';
  import {
    rightPaneState,
    getRightPaneState,
    setRightPaneCollapsed,
  } from '../stores/right-pane.svelte';

  import type { Snippet } from 'svelte';
  interface Props {
    children?: Snippet;
  }

  let { children }: Props = $props();
  // Web 外壳通过 context 统一处理布局；无 context 时仍可直接切换 store 中的面板状态。
  const webSidebar = getWebSidebarContext();
  const currentRightPane = $derived(getRightPaneState(rightPaneState.activeScopeKey));
  let desktopRightPaneVisible = $state<boolean | null>(null);
  const rightPaneCollapsed = $derived(
    typeof window !== 'undefined'
      && window.magiDesktop?.surface === 'app'
      && desktopRightPaneVisible !== null
      ? !desktopRightPaneVisible
      : currentRightPane.collapsed,
  );

  const sidebarUnavailable = $derived(
    Boolean(webSidebar && (webSidebar.hidden || (webSidebar.isDrawer && !webSidebar.drawerOpen))),
  );
  // 通知入口在侧栏内；侧栏不可见时，未读提示必须仍能在侧栏开关上被看到。
  const unreadNotificationCount = $derived.by(() => getUnreadNotificationCount());
  const sidebarToggleTitle = $derived(
    i18n.t(sidebarUnavailable ? 'web.expandSidebar' : 'web.collapseSidebar'),
  );

  function toggleRightPane() {
    closePopover();
    if (webSidebar) {
      webSidebar.toggleRightPane();
      return;
    }
    setRightPaneCollapsed(rightPaneState.activeScopeKey, !currentRightPane.collapsed);
  }

  onMount(() => {
    const desktop = window.magiDesktop?.surface === 'app' ? window.magiDesktop : undefined;
    const applyDesktopSnapshot = (snapshot: MagiDesktopWindowSnapshot) => {
      desktopRightPaneVisible = snapshot.layout.rightPaneVisible;
    };
    void desktop?.getSnapshot().then(applyDesktopSnapshot).catch(() => undefined);
    const stopDesktopSnapshot = desktop?.onSnapshot(applyDesktopSnapshot);
    return () => stopDesktopSnapshot?.();
  });
</script>

<header class="header-bar">
  {#if webSidebar}
    <button
      type="button"
      class="btn-icon btn-icon--lg btn-icon--badged header-btn header-sidebar-toggle"
      aria-label={sidebarToggleTitle}
      title={sidebarToggleTitle}
      onclick={() => webSidebar.toggle()}
    >
      <Icon name="sidebar-toggle" size={14} />
      {#if sidebarUnavailable && unreadNotificationCount > 0}
        <span class="btn-icon__dot" aria-hidden="true"></span>
      {/if}
    </button>
  {/if}
  <div class="header-center">
    {@render children?.()}
  </div>

  <div class="header-actions">
    <div class="header-popover-anchor" data-shell-popover="lan">
      <button
        type="button"
        class="btn-icon btn-icon--lg header-btn"
        class:btn-icon--active={shellUi.popover === 'lan'}
        onclick={() => togglePopover('lan')}
        title={i18n.t('lanAccess.title')}
        aria-label={i18n.t('lanAccess.title')}
        aria-expanded={shellUi.popover === 'lan'}
      >
        <Icon name="qrcode" size={14} />
      </button>
      <LanAccessPanel
        visible={shellUi.popover === 'lan'}
        onClose={() => closePopover('lan')}
      />
    </div>
    <button
      type="button"
      class="btn-icon btn-icon--lg header-btn header-right-pane-btn"
      onclick={toggleRightPane}
      title={i18n.t(rightPaneCollapsed ? 'rightPane.expand' : 'rightPane.collapse')}
      aria-label={i18n.t(rightPaneCollapsed ? 'rightPane.expand' : 'rightPane.collapse')}
      aria-expanded={!rightPaneCollapsed}
    >
      <Icon name="sidebar-toggle" size={14} class="right-pane-toggle-icon" />
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

  /* Desktop 中栏的 app-container 提供结构性磨砂层。Header 只保留结构
     边界和控件状态，不能再使用更不透明的 glass-bg 覆盖中栏材质。 */
  :global([data-magi-desktop-surface='app']) .header-bar {
    background: transparent;
    backdrop-filter: none;
    -webkit-backdrop-filter: none;
  }

  .header-center {
    display: flex;
    flex: 1;
    min-width: 0;
    justify-content: center;
    overflow-x: auto;
    scrollbar-width: none;
  }

  .header-center::-webkit-scrollbar {
    display: none;
  }

  .header-sidebar-toggle {
    margin-right: 6px;
  }

  .header-actions {
    display: flex;
    align-items: center;
    gap: 3px;
    flex-shrink: 0;
    justify-content: flex-end;
    position: relative;
  }

  .header-popover-anchor {
    position: relative;
    display: inline-flex;
  }

  :global(.right-pane-toggle-icon) {
    transform: scaleX(-1);
  }

  /* 移动端：顶部保持单行三段式，触控目标放大。 */
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

    .header-btn {
      width: 38px;
      height: 38px;
    }

    .header-center {
      grid-column: 2;
      min-width: 0;
      justify-content: center;
      overflow-x: auto;
      -webkit-overflow-scrolling: touch;
      scrollbar-width: none;
    }

    .header-popover-anchor {
      position: static;
    }
  }
</style>
