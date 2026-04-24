<script lang="ts">
  import Header from './components/Header.svelte';
  import TopTabs from './components/TopTabs.svelte';
  import ThreadPanel from './components/ThreadPanel.svelte';
  import TasksPanel from './components/TasksPanel.svelte';
  import EditsPanel from './components/EditsPanel.svelte';
  import KnowledgePanel from './components/KnowledgePanel.svelte';
  import SettingsPanel from './components/SettingsPanel.svelte';
  import ToastContainer from './components/ToastContainer.svelte';
  import Icon from './components/Icon.svelte';
  import { setCurrentTopTab, messagesState } from './stores/messages.svelte';
  import { i18n } from './stores/i18n.svelte';

  type TopTabType = 'thread' | 'tasks' | 'edits' | 'knowledge';

  // 安全获取顶部 Tab（映射非顶部 Tab 到默认值）
  const currentTopTab = $derived<TopTabType>(
    ['thread', 'tasks', 'edits', 'knowledge'].includes(messagesState.currentTopTab as string)
      ? (messagesState.currentTopTab as TopTabType)
      : 'thread'
  );

  // 设置面板是否打开
  let settingsOpen = $state(false);

  // 启动连接状态：后端 bootstrap 数据尚未就绪时显示等待提示
  const isBootstrapping = $derived(!messagesState.bootstrapped);

  function handleTabChange(tab: TopTabType) {
    setCurrentTopTab(tab);
  }

  function openSettings() {
    settingsOpen = true;
  }

  function closeSettings() {
    settingsOpen = false;
  }

</script>

<div class="app-container">
  <!-- 顶部标题栏 + 导航栏 -->
  <Header onOpenSettings={openSettings}>
    <TopTabs activeTopTab={currentTopTab} onTabChange={handleTabChange} />
  </Header>

  <!-- Tab 内容区域：主对话面板常驻以保留输入草稿，其余非主线面板仅在激活时挂载 -->
  <div class="tab-content-wrapper">
    {#if isBootstrapping}
      <!-- 启动连接等待层：后端 bootstrap 数据尚未就绪 -->
      <div class="bootstrap-overlay">
        <div class="bootstrap-content">
          <div class="bootstrap-spinner">
            <Icon name="loader" size={32} />
          </div>
          <p class="bootstrap-title">{i18n.t('app.bootstrapConnecting')}</p>
          <p class="bootstrap-hint">{i18n.t('app.bootstrapConnectingHint')}</p>
        </div>
      </div>
    {/if}
    <div class="top-tab-pane" class:active={currentTopTab === 'thread'}>
      <ThreadPanel isTopActive={currentTopTab === 'thread'} />
    </div>
    <div class="top-tab-pane" class:active={currentTopTab === 'tasks'}>
      {#if currentTopTab === 'tasks'}
        <TasksPanel />
      {/if}
    </div>
    <div class="top-tab-pane" class:active={currentTopTab === 'edits'}>
      {#if currentTopTab === 'edits'}
        <EditsPanel />
      {/if}
    </div>
    <div class="top-tab-pane" class:active={currentTopTab === 'knowledge'}>
      {#if currentTopTab === 'knowledge'}
        <KnowledgePanel />
      {/if}
    </div>
  </div>

  <!-- 设置面板（覆盖层） -->
  {#if settingsOpen}
    <SettingsPanel onClose={closeSettings} />
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

  /* 启动连接等待覆盖层 */
  .bootstrap-overlay {
    position: absolute;
    inset: 0;
    z-index: 10;
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
  }

  .bootstrap-spinner {
    color: var(--foreground-muted, #888);
    animation: bootstrap-spin 1.2s linear infinite;
  }

  .bootstrap-title {
    font-size: 15px;
    font-weight: 500;
    color: var(--foreground, #ccc);
    margin: 0;
  }

  .bootstrap-hint {
    font-size: 12px;
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
