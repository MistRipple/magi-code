<script lang="ts">
  import type { TimelineRenderItem } from '../types/message';
  import {
    getState,
    messagesState,
    setCurrentBottomTab,
  } from '../stores/messages.svelte';
  import MessageList from './MessageList.svelte';
  import InputArea from './InputArea.svelte';
  import BottomTabs from './BottomTabs.svelte';
  import AgentTab from './AgentTab.svelte';
  import RuntimeStatePanel from './RuntimeStatePanel.svelte';

  interface Props {
    isTopActive?: boolean;
  }
  let { isTopActive = true }: Props = $props();
  const appState = getState();

  // 直接使用 messagesState 对象，确保 Svelte 5 响应式追踪正常

  // 底部 Tab: 使用 store 中的状态，支持从其他组件跳转
  const activeBottomTab = $derived(messagesState.currentBottomTab as 'thread' | 'claude' | 'codex' | 'gemini');

  function handleBottomTabChange(tab: 'thread' | 'claude' | 'codex' | 'gemini') {
    setCurrentBottomTab(tab);
  }

  const activeWorkerTab = $derived.by(() => (
    isTopActive && activeBottomTab !== 'thread' ? activeBottomTab : null
  ));
  const threadRenderItems = $derived.by(() => (
    isTopActive && activeBottomTab === 'thread'
      ? (appState.getThreadRenderItems() as TimelineRenderItem[])
      : []
  ));
  const activeWorkerRenderItems = $derived.by(() => (
    activeWorkerTab
      ? (appState.getWorkerRenderItems(activeWorkerTab) as TimelineRenderItem[])
      : []
  ));
  const runtimeState = $derived(appState.orchestratorRuntimeState);
</script>

<div class="thread-panel">
  <RuntimeStatePanel {runtimeState} />
  <div class="main-content">
    {#if activeBottomTab === 'thread'}
      <MessageList renderItems={threadRenderItems} isActive={isTopActive && activeBottomTab === 'thread'} />
    {:else if activeBottomTab === 'claude'}
      <AgentTab workerName="claude" renderItems={activeWorkerRenderItems} isActive={isTopActive} />
    {:else if activeBottomTab === 'codex'}
      <AgentTab workerName="codex" renderItems={activeWorkerRenderItems} isActive={isTopActive} />
    {:else if activeBottomTab === 'gemini'}
      <AgentTab workerName="gemini" renderItems={activeWorkerRenderItems} isActive={isTopActive} />
    {/if}
  </div>

  <!-- 底部 Agent Tab 栏 - 在输入框上方 -->
  <BottomTabs activeTab={activeBottomTab} onTabChange={handleBottomTabChange} />

  <!-- 输入区域 -->
  <InputArea />
</div>

<style>
  .thread-panel {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0; /* flex 布局防溢出 */
    overflow: hidden;
  }

  .main-content {
    flex: 1;
    min-height: 0; /* flex 布局防溢出 */
    overflow: hidden;
    display: flex;
    flex-direction: column;
  }
</style>
