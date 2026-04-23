<script lang="ts">
  import type { OrchestratorRuntimeState } from '../types/message';
  import {
    messagesState,
    setCurrentBottomTab,
  } from '../stores/messages.svelte';
  import {
    buildTimelineRenderItems,
  } from '../lib/timeline-render-items';
  import MessageList from './MessageList.svelte';
  import InputArea from './InputArea.svelte';
  import BottomTabs from './BottomTabs.svelte';
  import AgentTab from './AgentTab.svelte';
  import RuntimeStatePanel from './RuntimeStatePanel.svelte';

  interface Props {
    isTopActive?: boolean;
  }
  let { isTopActive = true }: Props = $props();

  // 底部 Tab: 使用 store 中的状态，支持从其他组件跳转
  const activeBottomTab = $derived(messagesState.currentBottomTab as string);
  function handleBottomTabChange(tab: string) {
    setCurrentBottomTab(tab);
  }

  const activeWorkerTab = $derived.by(() => (
    isTopActive && activeBottomTab !== 'thread' ? activeBottomTab : null
  ));
  const threadRenderItems = $derived.by(() => (
    !isTopActive || activeBottomTab !== 'thread'
      ? []
      : messagesState.timelineProjection
        ? buildTimelineRenderItems(
            messagesState.timelineProjection,
            'thread',
          )
        : []
  ));
  const activeWorkerRenderItems = $derived.by(() => (
    !activeWorkerTab
      ? []
      : messagesState.timelineProjection
        ? buildTimelineRenderItems(
            messagesState.timelineProjection,
            'worker',
            activeWorkerTab,
          )
        : []
  ));
  const runtimeState = $derived.by<OrchestratorRuntimeState | null>(() => messagesState.orchestratorRuntimeState);
</script>

<div class="thread-panel">
  <RuntimeStatePanel {runtimeState} />
  <div class="main-content">
    {#if activeBottomTab === 'thread'}
      <MessageList renderItems={threadRenderItems} isActive={isTopActive && activeBottomTab === 'thread'} />
    {:else}
      <AgentTab workerName={activeBottomTab} renderItems={activeWorkerRenderItems} isActive={isTopActive} />
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
