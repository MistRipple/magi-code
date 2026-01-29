<script lang="ts">
  import { onMount } from 'svelte';
  import { initializeState } from './stores/messages.svelte';
  import Header from './components/Header.svelte';
  import TopTabs from './components/TopTabs.svelte';
  import ThreadPanel from './components/ThreadPanel.svelte';
  import TasksPanel from './components/TasksPanel.svelte';
  import EditsPanel from './components/EditsPanel.svelte';
  import KnowledgePanel from './components/KnowledgePanel.svelte';
  import SettingsPanel from './components/SettingsPanel.svelte';
  import SkillPopup from './components/SkillPopup.svelte';
  import ToastContainer from './components/ToastContainer.svelte';

  // 当前激活的顶部 Tab
  let activeTopTab = $state<'thread' | 'tasks' | 'edits' | 'knowledge'>('thread');

  // 设置面板是否打开
  let settingsOpen = $state(false);

  // 技能弹窗是否打开
  let skillPopupOpen = $state(false);

  function handleTabChange(tab: 'thread' | 'tasks' | 'edits' | 'knowledge') {
    activeTopTab = tab;
  }

  function openSettings() {
    settingsOpen = true;
  }

  function closeSettings() {
    settingsOpen = false;
  }

  function openSkillPopup() {
    skillPopupOpen = true;
  }

  function closeSkillPopup() {
    skillPopupOpen = false;
  }

  // 初始化状态
  onMount(() => {
    initializeState();
    console.log('[App] Svelte webview 已初始化');

    // 监听打开技能弹窗的消息
    const handler = (event: MessageEvent) => {
      const msg = event.data;
      if (msg.type === 'openSkillPopup') {
        skillPopupOpen = true;
      }
    };
    window.addEventListener('message', handler);
    return () => window.removeEventListener('message', handler);
  });
</script>

<div class="app-container">
  <!-- 顶部标题栏 -->
  <Header onOpenSettings={openSettings} />

  <!-- 顶部 Tab 栏：对话/任务/变更/知识 -->
  <TopTabs {activeTopTab} onTabChange={handleTabChange} />

  <!-- Tab 内容区域 -->
  <div class="tab-content-wrapper">
    {#if activeTopTab === 'thread'}
      <ThreadPanel />
    {:else if activeTopTab === 'tasks'}
      <TasksPanel />
    {:else if activeTopTab === 'edits'}
      <EditsPanel />
    {:else if activeTopTab === 'knowledge'}
      <KnowledgePanel />
    {/if}
  </div>

  <!-- 设置面板（覆盖层） -->
  {#if settingsOpen}
    <SettingsPanel onClose={closeSettings} />
  {/if}

  <!-- 技能弹窗 -->
  <SkillPopup visible={skillPopupOpen} onClose={closeSkillPopup} />

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
    overflow: hidden;
    display: flex;
    flex-direction: column;
  }
</style>

