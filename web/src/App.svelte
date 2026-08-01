<script lang="ts">
  import { onMount, type Component } from 'svelte';
  import Header from './components/Header.svelte';
  import TopTabs from './components/TopTabs.svelte';
  import ThreadPanel from './components/ThreadPanel.svelte';
  import ToastContainer from './components/ToastContainer.svelte';
  import Icon from './components/Icon.svelte';
  import { isDesktopRuntime } from './lib/desktop-updater';
  import {
    addToast,
    isPersistedSessionId,
    setCurrentTopTab,
    messagesState,
  } from './stores/messages.svelte';
  import { refreshPendingChangesProjection } from './lib/pending-changes-refresh';
  import { activateRightPaneSession } from './stores/right-pane.svelte';
  import { i18n } from './stores/i18n.svelte';
  import {
    RUNTIME_CONNECTION_EVENT,
    isWebAgentMode,
    type AgentConnectionEventDetail,
  } from './web/agent-api';

  type TopTabType = 'thread' | 'edits' | 'knowledge';

  // 安全获取顶部 Tab（映射非顶部 Tab 到默认值）
  const currentTopTab = $derived<TopTabType>(
    ['thread', 'edits', 'knowledge'].includes(messagesState.currentTopTab as string)
      ? (messagesState.currentTopTab as TopTabType)
      : 'thread'
  );
  const isWebMode = isWebAgentMode();
  const changeRefreshIntervalMs = 1000;

  // 设置面板是否打开
  let settingsOpen = $state(false);
  let EditsPanelComponent = $state<Component | null>(null);
  let KnowledgePanelComponent = $state<Component | null>(null);
  let SettingsPanelComponent = $state<Component<{ onClose: () => void }> | null>(null);
  let DesktopRuntimeRecoveryComponent = $state<Component | null>(null);
  let editsPanelLoad: Promise<void> | null = null;
  let knowledgePanelLoad: Promise<void> | null = null;
  let settingsPanelLoad: Promise<void> | null = null;
  let desktopRecoveryLoad: Promise<void> | null = null;

  function loadEditsPanel(): Promise<void> {
    if (EditsPanelComponent) return Promise.resolve();
    editsPanelLoad ??= import('./components/EditsPanel.svelte')
      .then((module) => {
        EditsPanelComponent = module.default;
      })
      .finally(() => {
        editsPanelLoad = null;
      });
    return editsPanelLoad;
  }

  function loadKnowledgePanel(): Promise<void> {
    if (KnowledgePanelComponent) return Promise.resolve();
    knowledgePanelLoad ??= import('./components/KnowledgePanel.svelte')
      .then((module) => {
        KnowledgePanelComponent = module.default;
      })
      .finally(() => {
        knowledgePanelLoad = null;
      });
    return knowledgePanelLoad;
  }

  function loadSettingsPanel(): Promise<void> {
    if (SettingsPanelComponent) return Promise.resolve();
    settingsPanelLoad ??= import('./components/SettingsPanel.svelte')
      .then((module) => {
        SettingsPanelComponent = module.default;
      })
      .finally(() => {
        settingsPanelLoad = null;
      });
    return settingsPanelLoad;
  }

  function loadDesktopRuntimeRecovery(): Promise<void> {
    if (DesktopRuntimeRecoveryComponent) return Promise.resolve();
    desktopRecoveryLoad ??= import('./components/DesktopRuntimeRecovery.svelte')
      .then((module) => {
        DesktopRuntimeRecoveryComponent = module.default;
      })
      .finally(() => {
        desktopRecoveryLoad = null;
      });
    return desktopRecoveryLoad;
  }

  // 启动连接状态：启动数据尚未就绪时显示等待提示
  const isBootstrapping = $derived(!messagesState.bootstrapped);
  let bootstrapConnectionFailed = $state(false);
  let runtimeRecoveryVisible = $state(false);
  const desktopRuntime = isDesktopRuntime();
  const showDesktopRecovery = $derived(
    desktopRuntime && (bootstrapConnectionFailed || runtimeRecoveryVisible)
  );

  function handleTabChange(tab: TopTabType) {
    setCurrentTopTab(tab);
  }

  function openSettings() {
    settingsOpen = true;
    void loadSettingsPanel().catch((error) => {
      console.error('[App] 设置面板加载失败:', error);
      settingsOpen = false;
      addToast('error', i18n.t('app.featureLoadFailed'));
    });
  }

  function closeSettings() {
    settingsOpen = false;
  }

  onMount(() => {
    let recoveryTimer: ReturnType<typeof setTimeout> | null = null;
    const clearRecoveryTimer = () => {
      if (recoveryTimer !== null) {
        clearTimeout(recoveryTimer);
        recoveryTimer = null;
      }
    };
    const handleAgentConnection = (event: Event) => {
      const detail = (event as CustomEvent<AgentConnectionEventDetail>).detail;
      if (detail?.status === 'connected') {
        clearRecoveryTimer();
        bootstrapConnectionFailed = false;
        runtimeRecoveryVisible = false;
        return;
      }
      if (!messagesState.bootstrapped) {
        bootstrapConnectionFailed = true;
        return;
      }
      if (desktopRuntime && recoveryTimer === null && !runtimeRecoveryVisible) {
        recoveryTimer = setTimeout(() => {
          recoveryTimer = null;
          runtimeRecoveryVisible = true;
        }, 3_000);
      }
    };
    window.addEventListener(RUNTIME_CONNECTION_EVENT, handleAgentConnection as EventListener);
    return () => {
      clearRecoveryTimer();
      window.removeEventListener(RUNTIME_CONNECTION_EVENT, handleAgentConnection as EventListener);
    };
  });

  $effect(() => {
    if (messagesState.bootstrapped) {
      bootstrapConnectionFailed = false;
    }
  });

  $effect(() => {
    if (currentTopTab === 'edits') {
      void loadEditsPanel().catch((error) => {
        console.error('[App] 变更面板加载失败:', error);
        addToast('error', i18n.t('app.featureLoadFailed'));
        setCurrentTopTab('thread');
      });
    } else if (currentTopTab === 'knowledge') {
      void loadKnowledgePanel().catch((error) => {
        console.error('[App] 知识面板加载失败:', error);
        addToast('error', i18n.t('app.featureLoadFailed'));
        setCurrentTopTab('thread');
      });
    }
  });

  $effect(() => {
    if (showDesktopRecovery) {
      void loadDesktopRuntimeRecovery().catch((error) => {
        console.error('[App] 桌面恢复面板加载失败:', error);
      });
    }
  });

  // 切换会话时同步 RightPane 上下文；空 sessionId 也要清掉，避免显示别的会话残留
  $effect(() => {
    activateRightPaneSession(messagesState.currentWorkspaceId, messagesState.currentSessionId);
  });

  $effect(() => {
    const sessionId = messagesState.currentSessionId?.trim() || '';
    const workspaceId = messagesState.currentWorkspaceId?.trim() || '';
    const workspacePath = messagesState.currentWorkspacePath?.trim() || '';
    const changesVisible = currentTopTab === 'edits';
    if (
      !isWebMode
      || messagesState.sessionHydrating
      || !isPersistedSessionId(sessionId)
      || (!workspaceId && !workspacePath)
    ) {
      return;
    }

    let disposed = false;
    let inFlight = false;
    let forceRefreshQueued = false;
    let errorReported = false;
    let mutationRetryTimer: number | null = null;
    const refresh = async (forceRefresh = false) => {
      if (disposed) {
        return;
      }
      if (inFlight) {
        forceRefreshQueued ||= forceRefresh;
        return;
      }
      if (messagesState.changeMutationStatus?.isMutating) {
        forceRefreshQueued ||= forceRefresh;
        if (mutationRetryTimer === null) {
          mutationRetryTimer = window.setTimeout(() => {
            mutationRetryTimer = null;
            void refresh(forceRefreshQueued);
          }, 100);
        }
        return;
      }
      inFlight = true;
      try {
        await refreshPendingChangesProjection({
          sessionId,
          workspaceId,
          workspacePath,
          forceRefresh,
        });
        errorReported = false;
      } catch (error) {
        console.warn('[App] 刷新变更列表失败:', error);
        if (!errorReported) {
          errorReported = true;
          addToast('error', '变更列表刷新失败，请稍后重试');
        }
      } finally {
        inFlight = false;
        if (!disposed && forceRefreshQueued) {
          forceRefreshQueued = false;
          void refresh(true);
        }
      }
    };
    const handleWindowFocus = () => void refresh(true);
    const handleVisibilityChange = () => {
      if (document.visibilityState === 'visible') {
        void refresh(true);
      }
    };
    const handleWorkspaceContentChanged = () => void refresh(true);

    void refresh(true);
    const timer = changesVisible
      ? window.setInterval(() => void refresh(), changeRefreshIntervalMs)
      : null;
    window.addEventListener('focus', handleWindowFocus);
    window.addEventListener('magi:workspaceContentChanged', handleWorkspaceContentChanged);
    document.addEventListener('visibilitychange', handleVisibilityChange);
    return () => {
      disposed = true;
      if (timer !== null) {
        window.clearInterval(timer);
      }
      if (mutationRetryTimer !== null) {
        window.clearTimeout(mutationRetryTimer);
      }
      window.removeEventListener('focus', handleWindowFocus);
      window.removeEventListener('magi:workspaceContentChanged', handleWorkspaceContentChanged);
      document.removeEventListener('visibilitychange', handleVisibilityChange);
    };
  });

</script>

<div class="app-container">
  <!-- 顶部标题栏 + 导航栏 -->
  <Header onOpenSettings={openSettings}>
    <TopTabs activeTopTab={currentTopTab} onTabChange={handleTabChange} />
  </Header>

  <!-- Tab 内容区域：常驻 ThreadPanel + 按需挂载的其他 top-tab -->
  <div class="tab-content-wrapper">
    {#if isBootstrapping || runtimeRecoveryVisible}
      <!-- 启动连接等待层：启动数据尚未就绪 -->
      <div class="bootstrap-overlay">
        {#if showDesktopRecovery && DesktopRuntimeRecoveryComponent}
          <DesktopRuntimeRecoveryComponent />
        {:else}
          <div class="bootstrap-content" class:error={bootstrapConnectionFailed}>
            <div class="bootstrap-spinner" class:static={bootstrapConnectionFailed}>
              <Icon name={bootstrapConnectionFailed ? 'warning' : 'loader'} size={32} />
            </div>
            <p class="bootstrap-title">
              {bootstrapConnectionFailed ? i18n.t('app.bootstrapConnectionFailed') : i18n.t('app.bootstrapConnecting')}
            </p>
            <p class="bootstrap-hint">
              {bootstrapConnectionFailed ? i18n.t('app.bootstrapConnectionHint') : i18n.t('app.bootstrapConnectingHint')}
            </p>
          </div>
        {/if}
      </div>
    {/if}
    <div class="top-tab-pane" class:active={currentTopTab === 'thread'}>
      <ThreadPanel isTopActive={currentTopTab === 'thread'} />
    </div>
    <div class="top-tab-pane" class:active={currentTopTab === 'edits'}>
      {#if currentTopTab === 'edits'}
        {#if EditsPanelComponent}
          <EditsPanelComponent />
        {:else}
          <div class="lazy-panel-loading" role="status"><Icon name="loader" size={18} /><span>{i18n.t('common.loading')}</span></div>
        {/if}
      {/if}
    </div>
    <div class="top-tab-pane" class:active={currentTopTab === 'knowledge'}>
      {#if currentTopTab === 'knowledge'}
        {#if KnowledgePanelComponent}
          <KnowledgePanelComponent />
        {:else}
          <div class="lazy-panel-loading" role="status"><Icon name="loader" size={18} /><span>{i18n.t('common.loading')}</span></div>
        {/if}
      {/if}
    </div>
  </div>

  <!-- 设置面板（覆盖层） -->
  {#if settingsOpen}
    {#if SettingsPanelComponent}
      <SettingsPanelComponent onClose={closeSettings} />
    {:else}
      <div class="lazy-settings-loading" role="status"><Icon name="loader" size={22} /><span>{i18n.t('common.loading')}</span></div>
    {/if}
  {/if}
  <!-- Toast 通知容器 -->
  <ToastContainer />
</div>

<style>
  .app-container {
    display: flex;
    flex-direction: column;
    height: 100%;
    width: 100%;
    overflow: hidden;
    background: var(--background);
  }

  .tab-content-wrapper {
    flex: 1;
    min-height: 0; /* flex 布局防溢出：防止子元素撑破容器产生页面级滚动条 */
    overflow: hidden;
    display: flex;
    flex-direction: column;
    position: relative;
  }

  .lazy-panel-loading,
  .lazy-settings-loading {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: var(--space-2);
    color: var(--foreground-muted);
    font-size: var(--text-sm);
  }

  .lazy-panel-loading {
    width: 100%;
    height: 100%;
  }

  .lazy-settings-loading {
    position: fixed;
    inset: 0;
    z-index: 1000;
    background: var(--background);
  }

  .lazy-panel-loading :global(svg),
  .lazy-settings-loading :global(svg) {
    animation: bootstrap-spin 1s linear infinite;
  }

  /* 启动连接等待覆盖层 */
  .bootstrap-overlay {
    position: absolute;
    inset: 0;
    z-index: 250;
    display: flex;
    align-items: center;
    justify-content: center;
    background: var(--background);
    animation: bootstrap-fade-in 0.3s ease-out;
  }

  .bootstrap-content {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 12px;
    text-align: center;
    padding: 0 24px;
    max-width: min(460px, calc(100% - 48px));
  }

  .bootstrap-content.error {
    gap: 10px;
  }

  .bootstrap-spinner {
    color: var(--foreground-muted, #888);
    animation: bootstrap-spin 1.2s linear infinite;
  }

  .bootstrap-spinner.static {
    color: var(--warning, #fbbf24);
    animation: none;
  }

  .bootstrap-title {
    font-size: 15px;
    font-weight: 500;
    color: var(--foreground, #ccc);
    margin: 0;
  }

  .bootstrap-hint {
    font-size: 12px;
    line-height: 1.6;
    color: var(--foreground-muted, #888);
    margin: 0;
  }

  @keyframes bootstrap-spin {
    from { transform: rotate(0deg); }
    to { transform: rotate(360deg); }
  }

  @keyframes bootstrap-fade-in {
    from { opacity: 0; }
    to { opacity: 1; }
  }

  /* 顶部 Tab 面板：默认隐藏，激活时显示（与 ThreadPanel 底部 Tab 同一模式） */
  .top-tab-pane {
    display: none;
    flex: 1;
    min-height: 0;
  }

  .top-tab-pane.active {
    display: flex;
    flex-direction: column;
  }
</style>
